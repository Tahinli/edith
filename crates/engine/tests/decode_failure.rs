//! A decode failure reaches the user, the way a lost sound device already does.
//!
//! A worker that ran to the end of a file and a worker that gave up on a
//! corrupt packet both drop the sender, and `try_frame` returns `None` for
//! either. The app read that as end-of-stream, so a file with a damaged middle
//! froze on its last good picture and the transport said ENDED: no notice, no
//! banner, and nothing on screen to say the film had stopped being the film.
//! The sound path has had a reason channel for a device that dies mid-session
//! all along; this is that channel's picture-side twin.
//!
//! The fixture is **built here**: `test_av.mp4`'s `moov` sits *after* its `mdat`,
//! so truncating the file would take the index with it. What is done instead is
//! what a download that dies mid-transfer leaves behind -- the tail of the
//! sample data overwritten, the container index intact and still claiming every
//! frame. The header and the first seconds stay valid, so frames really do
//! arrive before the failure, which is the whole shape of the bug.
//!
//! ```text
//! cargo test -p engine --test decode_failure -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Software everywhere, so the suite proves the path every machine has.
fn pin_software() {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| {
        engine::envcfg::set("VE_SW", "1");
        engine::envcfg::set("VE_SW_ENC", "1");
    });
}

/// `test_av.mp4` with the last eighth of its **sample data** overwritten, and
/// the file otherwise untouched: `moov` still claims every frame the header
/// ever did, and the demuxer hands the decoder exactly what it always handed
/// it.
///
/// The damage is bounded to the `mdat` payload on purpose. `moov` sits *after*
/// `mdat` in this fixture, so a damage that ran to the end of the file would
/// take the index with it and leave a file that will not open at all -- a
/// different defect, reached before a single frame is decoded and invisible to
/// the thing this test is about.
///
/// Returns the scratch guard too, so the directory goes when the test ends.
fn damaged() -> (Scratch, PathBuf) {
    let bytes = std::fs::read(asset("test_av.mp4")).expect("the fixture");
    let (start, end) = mdat_payload(&bytes).expect("an mdat payload");
    let mut damaged_bytes = bytes;
    let tail = start + (end - start) * 7 / 8;
    damaged_bytes[tail..end].fill(0);
    let scratch = Scratch::file("ve_decode_failure", "mp4");
    let path = PathBuf::from(&*scratch);
    std::fs::write(&path, &damaged_bytes).expect("write the damaged fixture");
    (scratch, path)
}

/// The `mdat` box's payload, start and end: the walk a container parser does,
/// four bytes at a time, and the reason the fixture is damaged rather than
/// truncated.
fn mdat_payload(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut at = 0usize;
    while at + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        if size < 8 {
            return None;
        }
        if kind == b"mdat" {
            return Some((at + 8, (at + size).min(bytes.len())));
        }
        at += size;
    }
    None
}

/// Plays `path` to its end and answers what the transport would show and what
/// the session says went wrong.
fn play_to_the_end(path: &std::path::Path) -> (u32, Option<String>) {
    let mut session = PlaybackSession::open(path).expect("open the file");
    // A seek rather than a play: this wants the whole span decoded whatever the
    // clock says, and a paused session still decodes -- which is what a scrub
    // does, and is the same worker either way.
    session.seek(0.0);
    let started = Instant::now();
    let mut frames = 0u32;
    // The loop runs on the session's own end-of-stream flag rather than on the
    // frame channel, because **that is the bug**: a closed channel and a clean
    // end of file are one event to a caller that only has the channel, and the
    // app reads that event as the film finishing. Draining on the flag is what
    // a fixed caller does, and the reason has to be found somewhere else.
    while !session.is_eos() {
        if let Some(frame) = session.try_frame() {
            frames = frame.index + 1;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the session never reported an end of stream"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // The worker sets the reason as it gives up, which can be a moment after
    // the flag it also sets; a fixed caller waits for the answer rather than
    // racing it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.video_failure_reason().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    (frames, session.video_failure_reason())
}

/// The defect: a file whose middle is damaged produces real frames, then stops,
/// and says why. Before this the second half was silent -- the reason was an
/// `eprintln!` on a decode worker's stderr, and the only thing the caller could
/// see was a closed channel, which it read as the film finishing.
#[test]
fn a_damaged_file_says_why_it_stopped() {
    pin_software();
    let (_scratch, path) = damaged();
    let (frames, reason) = play_to_the_end(&path);

    assert!(
        frames > 0,
        "the fixture must really decode before it fails, or this proves nothing \
         about a failure that happens mid-stream"
    );
    let reason = reason.expect(
        "a file that stopped short of its end has to say why: before this the \
         reason was an eprintln on a decode worker and the caller could only \
         see a closed channel",
    );
    assert!(
        !reason.is_empty(),
        "an empty reason is not a reason a notice can print"
    );
}

/// The other half, and the one that keeps the notice from becoming noise: a
/// file that plays all the way through says **nothing**. A reason channel that
/// fires on every film is a channel nobody reads, which is how the audio
/// device-loss notice earns its place on the strip.
#[test]
fn a_file_that_plays_to_its_end_says_nothing() {
    pin_software();
    let path = asset("test_av.mp4");
    let (frames, reason) = play_to_the_end(&path);
    assert!(frames > 0, "the control must actually play");
    assert_eq!(
        reason, None,
        "a film that reached its own end is not a failure"
    );
}
