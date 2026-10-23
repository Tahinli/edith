//! Audio output checks. These need a PulseAudio-protocol server (pipewire-pulse
//! or PulseAudio), so they are `#[ignore]`d by default. They only ever talk to
//! the server named by `PULSE_SERVER`, and refuse to run against the default
//! one under `$XDG_RUNTIME_DIR`: point them at a private instance. Run them with:
//!
//! ```text
//! PULSE_SERVER=unix:/path/to/private/pulse/native \
//!   cargo test -p engine --release --test ao_output -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `no_daemon_opens_nothing` re-runs itself as a child process with
//! `PULSE_SERVER` pointed at a missing socket, since the client reads the real
//! environment.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use engine::ao::AoSession;

const RATE: u32 = 44100;
const CHANNELS: u32 = 2;

/// Half a second of a quiet 1 kHz tone, interleaved stereo.
fn tone() -> Vec<f32> {
    let frames = RATE as usize / 2;
    let mut samples = Vec::with_capacity(frames * CHANNELS as usize);
    for i in 0..frames {
        let value = (i as f32 * TAU * 1000.0 / RATE as f32).sin() * 0.2;
        samples.extend([value; CHANNELS as usize]);
    }
    samples
}

/// Opens the device the way this suite wants it: connected for real, and
/// silent. Everything these tests assert -- that the clock runs, at the right
/// rate, and stops when the stream is paused -- is measured from the device's
/// own position, which the gain does not touch: the frames are still consumed,
/// only the last multiply before the speaker is zero.
///
/// corner-cut: it does mean the suite is deaf, so a gain stuck at zero would pass
/// it. Nothing here could ever hear that -- the checks that can are the gain unit
/// test in `pulse` and a person with the app open. The upgrade path is recording
/// a null sink and asserting on the samples that come back.
fn open_silent() -> AoSession {
    want_real_device();
    let ao = AoSession::open(RATE, CHANNELS).expect("no PulseAudio-protocol server available");
    assert!(ao.set_volume(0.0), "mute rejected");
    ao
}

/// Routes the opens in this suite to the real server, and only to a server the
/// caller named: a suite that fell through to the default socket would play
/// through the person's own sound. A session's own server lives under
/// `/run/user/<uid>`, so that prefix is refused.
fn want_real_device() {
    let server = engine::envcfg::var("PULSE_SERVER").expect("set PULSE_SERVER to a private server");
    assert!(
        !server.contains("/run/user/"),
        "PULSE_SERVER names a session's own server; use a private one"
    );
    engine::envcfg::set("VE_AO", "1");
}

/// Waits up to `limit` for the clock to report a played position past zero.
fn wait_for_playback(ao: &AoSession, limit: Duration) -> i64 {
    let start = Instant::now();
    loop {
        if let Some(position) = ao.position()
            && position > 0
        {
            return position;
        }
        assert!(start.elapsed() < limit, "playback position never advanced");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "needs a private pulse-protocol server (PULSE_SERVER)"]
fn plays_and_tracks_position() {
    assert!(AoSession::probe(), "no server socket");
    let mut ao = open_silent();

    // The ring holds a second, so two halves go in whole -- enough real audio
    // to cover the measurement below without starving.
    let samples = tone();
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");

    // Over 400 ms the clock must run at the rate we asked for, not at the
    // device's: the plugin converts the server's ticks out of its graph rate.
    let first = wait_for_playback(&ao, Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(400));
    let second = ao.position().expect("position went unknown");
    let advanced = second - first;
    let expected = RATE as i64 * 400 / 1000;
    eprintln!("clock advanced {advanced} samples in 400 ms (expected ~{expected})");
    assert!(
        (advanced - expected).abs() < expected / 10,
        "clock runs at the wrong rate: {advanced} samples in 400 ms"
    );

    // Paused means the device plays nothing, so the master clock must not move.
    assert!(ao.set_active(false), "pause rejected");
    std::thread::sleep(Duration::from_millis(150));
    let paused = ao.position().expect("position went unknown");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        ao.position(),
        Some(paused),
        "position moved while paused: {paused}"
    );

    ao.write(&samples).expect("write after pause");
    assert!(ao.set_active(true), "resume rejected");
    std::thread::sleep(Duration::from_millis(200));
    let resumed = ao.position().expect("position went unknown");
    assert!(
        resumed > paused,
        "clock did not resume: {paused} -> {resumed}"
    );
}

/// Muting is not pausing: the device keeps consuming and the master clock keeps
/// running, which is the whole reason the picture can play on through a mute.
/// That the samples really go quiet is the gain unit test in `pulse` -- nothing
/// out here can hear the device.
#[test]
#[ignore = "needs a private pulse-protocol server (PULSE_SERVER)"]
fn muting_silences_without_stopping_the_clock() {
    assert!(AoSession::probe(), "no server socket");
    let mut ao = open_silent();

    let samples = tone();
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");
    let first = wait_for_playback(&ao, Duration::from_secs(2));

    // Muted and fed: the position must move exactly as it would have.
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");
    std::thread::sleep(Duration::from_millis(300));
    let muted = ao.position().expect("position went unknown");
    let advanced = muted - first;
    let expected = RATE as i64 * 300 / 1000;
    eprintln!("clock advanced {advanced} samples in 300 ms while muted (expected ~{expected})");
    assert!(
        (advanced - expected).abs() < expected / 5,
        "the clock stalled while muted: {advanced} samples in 300 ms"
    );

    // Every level in range is taken, and the two ends are in range. Done on an
    // emptied ring and left back at zero, so that the audible gains this walks
    // through have nothing but the underrun fill to be applied to: whoever runs
    // the suite should not have to hear it prove that 1.0 is a legal number.
    assert!(ao.flush(), "flush rejected");
    // A flush lands on the next callback and the ring refuses writes until it
    // does, so give it a couple of quanta before asking for anything else.
    std::thread::sleep(Duration::from_millis(50));
    for gain in [0.5, 1.0, 0.0] {
        assert!(ao.set_volume(gain), "gain {gain} rejected");
    }
    // Out of range is refused at the ABI rather than multiplied into the
    // speakers -- a NaN would otherwise reach the RT thread and stay there.
    for gain in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
        assert!(!ao.set_volume(gain), "gain {gain} accepted");
    }
    // ...and a refusal changes nothing: the last accepted gain still stands.
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        ao.position().expect("position went unknown") > muted,
        "clock stopped after a refused gain"
    );
}

/// No reachable daemon must fail the open cleanly and quickly, never hang.
#[test]
#[ignore = "needs a private pulse-protocol server (PULSE_SERVER)"]
fn no_daemon_opens_nothing() {
    // The client reads `PULSE_SERVER` from the real environment, which this
    // process may not edit safely, so the test runs itself again as a child with
    // the variable set and does the open there.
    const CHILD: &str = "EDITH_AO_NO_DAEMON_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("no_daemon_opens_nothing")
            .args(["--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("PULSE_SERVER", "unix:/nonexistent/pulse/native")
            .status()
            .expect("re-run the test binary");
        assert!(status.success(), "the no-daemon child failed: {status}");
        return;
    }
    want_real_device();
    let start = Instant::now();
    let session = AoSession::open(RATE, CHANNELS);

    let elapsed = start.elapsed();

    assert!(session.is_none(), "opened a stream with no daemon");
    assert!(elapsed < Duration::from_secs(2), "open took {elapsed:?}");
    eprintln!("open with no daemon failed in {elapsed:?}");
}
