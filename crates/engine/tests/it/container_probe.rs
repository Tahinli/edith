//! What the per-file probes say about containers that are not mp4.
//!
//! The media panel asks `decode::probe` and `probe_media` of every file it shows. A wav, flac or
//! ogg is not an mp4, so the mp4 reader must never be asked of it: its refusal ("ftyp not found:
//! it is not an mp4 or QuickTime container") would be shown beside a file that plays. A real mp4
//! keeps its picture answer.

use std::path::Path;
use std::process::Command;

use engine::decode::{Backend, probe};
use engine::demux::{Codec, probe_media};
use engine::scratch::Scratch;

fn ffmpeg(out: &Path, args: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
        .args(args)
        .arg(out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {}", out.display());
}

/// Every word a panel could show for `result`, and whether any of them is the mp4 reader's.
fn mp4_words(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("ftyp") || text.contains("mp4") || text.contains("quicktime")
}

#[test]
fn sound_files_and_mkv_are_never_answered_in_mp4_words() {
    let cases: [(&str, &[&str]); 4] = [
        ("wavftyp_a", &["-ac", "2"]),
        ("wavftyp_b", &["-c:a", "flac", "-ac", "2"]),
        ("wavftyp_c", &["-c:a", "libvorbis", "-ac", "2"]),
        ("wavftyp_d", &["-c:a", "libopus", "-ac", "2"]),
    ];
    let exts = ["wav", "flac", "ogg", "mkv"];
    for ((name, args), ext) in cases.iter().zip(exts) {
        let file = Scratch::file(name, ext);
        ffmpeg(&file, args);
        let path: &Path = &file;

        // The decoder answer the panel shows on the row's second line.
        match probe(path) {
            Ok((codec, backend)) => {
                if ext != "mkv" {
                    assert_eq!(codec, None, "{ext}: a sound file has no coded stream");
                    assert_eq!(backend, Backend::Still, "{ext}");
                }
            }
            Err(e) => {
                let text = e.to_string();
                assert!(!mp4_words(&text), "{ext}: the probe answered in mp4 words: {text}");
            }
        }

        // The facts the panel draws: the container and the length, with no mp4 words in them.
        let facts = probe_media(path);
        if let Some(container) = &facts.container {
            assert!(!mp4_words(container), "{ext}: container {container}");
        }
        assert!(facts.duration_secs.is_some_and(|d| d > 0.5), "{ext}: {:?}", facts.duration_secs);
        assert!(facts.video.is_none(), "{ext}: a sound file has no picture");
    }
}

#[test]
fn a_real_mp4_still_gets_its_picture_answer() {
    let file = Scratch::file("wavftyp_mp4", "mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
        .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=1"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", "-shortest"])
        .arg(&*file)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for the mp4 fixture");
    let path: &Path = &file;
    let (codec, backend) = probe(path).expect("an mp4 with a picture probes");
    assert_eq!(codec, Some(Codec::H264));
    assert_ne!(backend, Backend::Still);
    let facts = probe_media(path);
    let video = facts.video.expect("the mp4 has its picture");
    assert_eq!(video.codec, Codec::H264);
    assert_eq!((video.width, video.height), (320, 240));
    assert!(facts.streams.iter().any(|s| s.codec_id.as_deref() == Some("mp4a")), "{:?}", facts.streams);
}
