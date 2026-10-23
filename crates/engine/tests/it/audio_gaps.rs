//! The containers and codecs this build refuses by name: ASF / WMV (Windows Media) and Speex in
//! Ogg. A refusal is an answer, not a silent import: the probe, the warning and the import each
//! name the file's format, and never the mp4 "ftyp" error an ASF used to get.
//!
//! The fixtures are made here, at test time, by ffmpeg. Skipped, loudly, where ffmpeg (or the
//! encoder a case needs) is not installed.

use std::path::PathBuf;
use std::process::Command;

use engine::{AudioSession, PlaybackSession};

const ASF_REFUSAL: &str = "ASF/WMV (Windows Media) files are not supported: convert to MP4 or MKV";
const SPEEX_REFUSAL: &str = "Speex audio is not supported";

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

/// Whether this ffmpeg lists `name` among its encoders.
fn ffmpeg_has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(name))
        })
}

/// Makes `name.ext` with ffmpeg from the given output arguments; `None` (and a note) when ffmpeg
/// cannot make it here.
fn make(name: &str, ext: &str, args: &[&str]) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("audio-gaps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(format!("{name}.{ext}"));
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1:sample_rate=16000"])
        .args(["-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=10"])
        .args(args)
        .arg(&out)
        .status()
        .ok()?;
    assert!(status.success(), "ffmpeg makes {name}");
    Some(out)
}

#[test]
fn asf_and_wmv_are_refused_by_name_not_by_the_mp4_error() {
    let Some(pic) = asset("test_av.mp4").exists().then(|| asset("test_av.mp4")) else {
        return;
    };
    let mut session = PlaybackSession::open(&pic).expect("open the picture file");
    session.set_gain(0.0);
    for (name, ext) in [("wma_asf", "asf"), ("wma_wmv", "wmv")] {
        let Some(file) = make(name, ext, &["-c:v", "wmv2", "-c:a", "wmav2", "-b:a", "64k"]) else {
            continue;
        };
        let probe = AudioSession::probe_streams(&file).expect_err("an ASF is refused");
        assert_eq!(probe.to_string(), ASF_REFUSAL, "{name}: probe");
        let import = session.import_with_warnings(&file).expect_err("an ASF is refused");
        assert_eq!(import.to_string(), ASF_REFUSAL, "{name}: import");
        assert!(!import.to_string().contains("ftyp"), "{name}: the mp4 error leaked");
        let why = AudioSession::unsupported(&file).expect_err("an ASF is refused");
        assert_eq!(why.to_string(), ASF_REFUSAL, "{name}: warning path");
    }
}

#[test]
fn audio_only_speex_in_ogg_is_refused_by_name() {
    if !ffmpeg_has_encoder("libspeex") {
        eprintln!("skipped: this ffmpeg has no libspeex encoder");
        return;
    }
    for (name, ext) in [("speex_ogg", "ogg"), ("speex_spx", "spx")] {
        let Some(file) = make(name, ext, &["-vn", "-c:a", "libspeex", "-ar", "16000", "-ac", "1", "-f", "ogg"])
        else {
            continue;
        };
        assert_eq!(
            AudioSession::probe_streams(&file).expect_err("Speex is refused").to_string(),
            SPEEX_REFUSAL,
            "{name}: probe"
        );
        assert_eq!(
            AudioSession::unsupported(&file).expect("asks").as_deref(),
            Some(SPEEX_REFUSAL),
            "{name}: the reason the import warns with"
        );
    }
}

#[test]
fn an_ogg_vorbis_file_is_not_taken_for_speex() {
    // A file that is not Speex must not be refused as one: the Ogg check reads the first packet.
    if !ffmpeg_has_encoder("libvorbis") {
        eprintln!("skipped: this ffmpeg has no libvorbis encoder");
        return;
    }
    let Some(file) = make("vorbis_ogg", "ogg", &["-vn", "-c:a", "libvorbis", "-ac", "1", "-f", "ogg"]) else {
        return;
    };
    // Whatever the reader says of a Vorbis Ogg, it is never the Speex refusal.
    let reason = AudioSession::unsupported(&file).ok().flatten();
    assert_ne!(reason.as_deref(), Some(SPEEX_REFUSAL));
}

#[test]
fn speex_with_video_in_matroska_imports_silent_with_a_warning() {
    // A Speex track beside a picture is the silent path: the picture imports, the sound is silent,
    // and the one warning names the codec.
    if !ffmpeg_has_encoder("libspeex") {
        eprintln!("skipped: this ffmpeg has no libspeex encoder");
        return;
    }
    let Some(file) = make(
        "speex_video_mkv",
        "mkv",
        &["-c:v", "mpeg4", "-c:a", "libspeex", "-ar", "16000", "-ac", "1"],
    ) else {
        return;
    };
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the picture file");
    session.set_gain(0.0);
    let (_, warnings) = session.import_with_warnings(&file).expect("the picture imports");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("Speex"), "the warning names the codec: {warnings:?}");
    assert!(warnings[0].contains("silent"), "the warning says the sound is silent: {warnings:?}");
}
