//! A video whose sound track no decoder here opens (WavPack) imports with its
//! picture, its sound silent for as long as the file's header says it plays, and
//! a returned warning that names the codec in the words a person knows it by.
//! The file is made with ffmpeg at test time; nothing is committed.
//!
//! ```text
//! cargo test -p engine --release --test silent_audio_import -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::{AudioSession, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Two seconds of test picture at 25 fps, with a 48 kHz sine encoded as WavPack.
fn wavpack_video(dir: &Path) -> PathBuf {
    let out = dir.join("wavpack_sound.mkv");
    let run = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=25"])
        .args(["-f", "lavfi", "-i", "sine=duration=2:sample_rate=48000"])
        .args(["-map", "0:v", "-map", "1:a", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "wavpack"])
        .arg(&out)
        .output()
        .expect("ffmpeg runs");
    assert!(run.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&run.stderr));
    out
}

#[test]
fn an_undecodable_sound_track_imports_silent_with_a_warning_naming_wavpack() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("silent-audio");
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let file = wavpack_video(&dir);

    // The sound opens to silence, and its length is the file's own: 2 s.
    assert!(
        matches!(AudioSession::open(&file), Ok(None)),
        "no decoder opens this WavPack track, so it is silent"
    );
    let secs = AudioSession::stream_duration_secs(&file, 0)
        .expect("the length reads")
        .expect("the header states a length for the silent sound");
    assert!((secs - 2.0).abs() < 0.1, "the sound is held for the picture's 2 s: {secs}");

    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the picture file");
    session.set_gain(0.0);
    let (source, warnings) = session
        .import_with_warnings(&file)
        .expect("a picture whose sound nothing decodes still imports");
    assert_eq!(session.sources()[source].path, file);

    // The picture is all there: two seconds of the timeline's frames. A file at its own rate
    // covers the same wall time, whatever rate it was shot at.
    let timeline_fps = session.native_frame_rate();
    let frames = f64::from(session.file_frames(&file));
    assert!(
        (frames - 2.0 * timeline_fps).abs() <= 2.0,
        "the video frames are present: {frames} frames at a {timeline_fps} fps timeline"
    );

    // One warning, naming the codec as WavPack and saying the sound is silent.
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = &warnings[0];
    assert!(warning.contains("WavPack"), "{warning}");
    assert!(!warning.contains("WAVPACK4"), "the raw tag is not shown: {warning}");
    assert!(warning.ends_with("its sound will be silent"), "{warning}");
}
