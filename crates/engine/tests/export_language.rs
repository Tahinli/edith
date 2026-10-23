//! A Matroska export states its picture's and its sound's language on their
//! tracks: the source's own when it names one, `und` when it names none. The
//! Matroska default for a track with no `Language` is `eng`, so leaving the
//! element out would tell every player an undetermined source is English.
//!
//! The sources are made here with ffmpeg, in this test's own scratch directory
//! (never `assets/`), and the file that comes out is read back with ffprobe --
//! the one reader that is not this project's own. Where there is no ffmpeg on
//! the box the test says so and passes.
//!
//! ```text
//! cargo test -p engine --release --test export_language -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::subtitle::{Cue, SubtitleTrack};
use engine::{ExportHandle, Project};

/// Thirty frames of picture and a second of sound: enough to be a real export
/// and short enough that a software encoder is not the test's cost.
const FRAMES: u32 = 30;

fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| {
        engine::envcfg::set("VE_SW", "1");
        engine::envcfg::set("VE_SW_ENC", "1");
    });
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// A one-second H.264 and AAC Matroska source. `language` is written onto both
/// of its tracks, or onto neither when it is `None`. `None` when ffmpeg cannot
/// make one here, which the caller takes as "no witness on this box".
fn source(name: &str, language: Option<&str>) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}.mkv", std::process::id()));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=320x240:rate={FRAMES}"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args([
            "-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
        ]);
    if let Some(language) = language {
        cmd.args(["-metadata:s:v:0", &format!("language={language}")])
            .args(["-metadata:s:a:0", &format!("language={language}")]);
    }
    let ok = cmd.arg(&path).status().ok()?.success();
    ok.then_some(path)
}

/// A subtitle track of `source` in `language` (`""` where it names none), with
/// one cue. It is picked from the project the way an embedded track is.
fn subtitle(source: &Path, language: &str) -> SubtitleTrack {
    SubtitleTrack {
        path: source.to_path_buf(),
        track: Some(3),
        language: language.into(),
        name: String::new(),
        label: if language.is_empty() {
            "und".into()
        } else {
            language.into()
        },
        bitmap: false,
        cues: vec![Cue {
            start_us: 100_000,
            end_us: 900_000,
            text: "a line".into(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        }],
        refused: None,
    }
}

/// The export of `source` in `format`, with `sub` picked as its one subtitle
/// track when it is given, and the language each track states as ffprobe reads
/// them: `(codec_type, language)` in file order.
fn exported_languages(
    name: &str,
    source: &Path,
    format: Format,
    sub: Option<&str>,
) -> Vec<(String, String)> {
    pin_software();
    let (meta, _) = engine::demux::Demuxer::open(source).expect("probe the source");
    let ext = match format {
        Format::Mp4 => "mp4",
        _ => "mkv",
    };
    let out = Scratch::file(&format!("ve_export_language_{name}"), ext);
    let project = match sub {
        Some(language) => {
            Project::single(source, FRAMES).with_subtitles(vec![subtitle(source, language)])
        }
        None => Project::single(source, FRAMES),
    };
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format,
            subtitles: if sub.is_some() { vec![0] } else { Vec::new() },
            ..Default::default()
        },
        None,
    );
    wait(&handle, Duration::from_secs(900)).expect("the export finishes");
    let printed = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:stream_tags=language",
        ])
        .args(["-of", "csv=p=0"])
        .arg(&out)
        .output()
        .expect("ffprobe runs");
    let text = String::from_utf8_lossy(&printed.stdout).into_owned();
    println!("ffprobe reads the export as:\n{text}");
    let kinds: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let kind = line.trim_end_matches(',').split(',').next()?.to_string();
            matches!(kind.as_str(), "video" | "audio" | "subtitle").then_some(kind)
        })
        .collect();
    // ffprobe prints no language for a Matroska track that says `und` (it reads
    // the element and drops `und`), so a Matroska file's languages are read off
    // its `Language` elements directly, in the same track order.
    let langs: Vec<String> = match format {
        Format::Mp4 => text
            .lines()
            .filter_map(|line| {
                let mut fields = line.trim_end_matches(',').split(',');
                let kind = fields.next()?;
                matches!(kind, "video" | "audio" | "subtitle")
                    .then(|| fields.next().unwrap_or_default().to_string())
            })
            .collect(),
        _ => matroska_languages(&out),
    };
    assert_eq!(
        kinds.len(),
        langs.len(),
        "one language per track: {kinds:?} {langs:?}"
    );
    kinds.into_iter().zip(langs).collect()
}

/// The `Language` of every track entry in the Matroska `path`, in file order, read
/// off the header's `TrackLanguage` elements (ID `0x22B59C`) before the first
/// cluster. Every track of an export carries one, so none is skipped.
fn matroska_languages(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).expect("read the export");
    let header = bytes
        .windows(4)
        .position(|w| w == [0x1F, 0x43, 0xB6, 0x75])
        .unwrap_or(bytes.len());
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= header {
        if bytes[at..at + 3] != [0x22, 0xB5, 0x9C] {
            at += 1;
            continue;
        }
        // An EBML size: the count of leading zero bits names its width.
        let first = bytes[at + 3];
        let width = first.leading_zeros() as usize + 1;
        let mut size = usize::from(first) & ((1 << (8 - width)) - 1);
        for &b in &bytes[at + 4..at + 3 + width] {
            size = (size << 8) | usize::from(b);
        }
        let start = at + 3 + width;
        out.push(String::from_utf8_lossy(&bytes[start..start + size]).into_owned());
        at = start + size;
    }
    out
}

fn pair(kind: &str, lang: &str) -> (String, String) {
    (kind.to_string(), lang.to_string())
}

#[test]
fn a_source_with_no_language_exports_und_on_both_tracks() {
    let Some(src) = source("lang_none", None) else {
        println!("no ffmpeg on this box: the source cannot be made here");
        return;
    };
    let tracks = exported_languages("und", &src, Format::Hevc, None);
    assert_eq!(
        tracks,
        vec![pair("video", "und"), pair("audio", "und")],
        "neither track names a language, so both say und"
    );
}

#[test]
fn a_source_language_is_the_language_of_both_exported_tracks() {
    let Some(src) = source("lang_ger", Some("ger")) else {
        println!("no ffmpeg on this box: the source cannot be made here");
        return;
    };
    let tracks = exported_languages("ger", &src, Format::Hevc, None);
    assert_eq!(
        tracks,
        vec![pair("video", "ger"), pair("audio", "ger")],
        "both tracks carry the source's language"
    );
}

#[test]
fn a_subtitle_with_no_language_is_und_in_matroska() {
    let Some(src) = source("lang_none_sub", None) else {
        println!("no ffmpeg on this box: the source cannot be made here");
        return;
    };
    let tracks = exported_languages("sub_und", &src, Format::Hevc, Some(""));
    assert_eq!(
        tracks.last(),
        Some(&pair("subtitle", "und")),
        "a track the source names no language for is und, not the default eng"
    );
}

#[test]
fn a_known_subtitle_language_is_stated_in_matroska_and_mp4() {
    let Some(src) = source("lang_none_fre", None) else {
        println!("no ffmpeg on this box: the source cannot be made here");
        return;
    };
    let mkv = exported_languages("sub_fre_mkv", &src, Format::Hevc, Some("fre"));
    assert_eq!(
        mkv.last(),
        Some(&pair("subtitle", "fre")),
        "Matroska states fre"
    );
    let mp4 = exported_languages("sub_fre_mp4", &src, Format::Mp4, Some("fre"));
    assert_eq!(mp4.last(), Some(&pair("subtitle", "fre")), "mp4 states fre");
}
