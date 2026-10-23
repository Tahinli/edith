//! Voice-over recording against the private audio stack. Ignored by default: it
//! needs a PulseAudio-protocol server and a tone playing into the `edtest` sink
//! while it runs. It only ever talks to the server named by `PULSE_SERVER`, so
//! point it at the private stack (`scratchpad/pulse/private-audio.sh`):
//!
//! ```text
//! PULSE_SERVER=unix:/tmp/edpa-run/pulse/native \
//!   cargo test -p engine --release --test voiceover_record -- --ignored --nocapture --test-threads=1
//! ```

use std::time::Duration;

use engine::pulse;
use engine::recorder::Recorder;

/// The private stack's null sink monitor, the input the tone is played into.
const SOURCE: &str = "edtest.monitor";
/// The tone the test expects to hear.
const TONE_HZ: f64 = 1000.0;

#[test]
#[ignore = "needs a private pulse-protocol server with a 1 kHz tone playing into edtest"]
fn a_second_of_tone_from_the_monitor_records_at_its_frequency() {
    let sources = pulse::list_sources().expect("the server lists its sources");
    assert!(
        sources.iter().any(|s| s.name == SOURCE && s.monitor),
        "{SOURCE} is listed as a monitor: {sources:?}"
    );

    let dir = std::env::temp_dir().join(format!("edith-voiceover-{}", std::process::id()));
    let recorder = Recorder::start(&dir, Some(SOURCE)).expect("the recording starts");
    std::thread::sleep(Duration::from_secs(1));
    let take = recorder.stop().expect("the recording stops");
    let bytes = std::fs::read(&take.path).expect("the take is on disk");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(take.duration_secs > 0.9, "about a second written: {take:?}");
    // Left channel, 16-bit. A sine's rising zero crossings count its cycles.
    let (frames, _) = bytes[44..].as_chunks::<4>();
    let samples: Vec<i16> = frames.iter().map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let mut rising = 0usize;
    for pair in samples.windows(2) {
        if pair[0] <= 0 && pair[1] > 0 {
            rising += 1;
        }
    }
    let hz = rising as f64 / take.duration_secs;
    assert!((hz - TONE_HZ).abs() < 0.05 * TONE_HZ, "heard {hz:.1} Hz, expected {TONE_HZ} Hz");
}
