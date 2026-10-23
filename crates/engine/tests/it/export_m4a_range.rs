//! A ranged audio-only AAC export states, in its `elst`, exactly the samples the
//! range holds, and not the last AAC frame's padding rounded up to a whole
//! packet. The timeline is a 48 kHz tone on the 30 fps canvas an audio-only
//! timeline gets, so frame `f` is sample `f * 1600`.
//!
//! ```text
//! cargo test -p engine --release --test export_m4a_range
//! ```

use std::path::Path;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::{ExportHandle, PlaybackSession};

const RATE: u32 = 48_000;
const FPS: u32 = 30;
/// Long enough that the last range below is inside the timeline.
const SECONDS: u32 = 14;

/// A 16-bit stereo sine at [`RATE`], written by hand so no tool is needed.
fn write_tone(path: &Path) {
    let frames = RATE * SECONDS;
    let data_len = frames * 2 * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..frames {
        let t = f64::from(n) / f64::from(RATE);
        let left = (t * 440.0 * std::f64::consts::TAU).sin() * 8000.0;
        let right = (t * 880.0 * std::f64::consts::TAU).sin() * 8000.0;
        wav.extend_from_slice(&(left as i16).to_le_bytes());
        wav.extend_from_slice(&(right as i16).to_le_bytes());
    }
    std::fs::write(path, wav).expect("write the source tone");
}

fn wait(handle: &ExportHandle) -> engine::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(result) = handle.result() {
            return result;
        }
        assert!(Instant::now() < deadline, "export never settled");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The payload of the first box called `name` in `data`, which is a sequence of
/// boxes.
fn child<'a>(data: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut at = 0;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let kind = &data[at + 4..at + 8];
        let (header, len) = match size {
            0 => (8, data.len() - at),
            1 => (
                16,
                u64::from_be_bytes(data[at + 8..at + 16].try_into().ok()?) as usize,
            ),
            n => (8, n),
        };
        if len < header || at + len > data.len() {
            return None;
        }
        if kind == name {
            return Some(&data[at + header..at + len]);
        }
        at += len;
    }
    None
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

/// The movie timescale, and the first track's single `elst` entry as
/// `(segment_duration, media_time)`, read straight from the boxes.
fn edit_of(path: &Path) -> (u32, u32, i64) {
    let file = std::fs::read(path).expect("read the export back");
    let moov = child(&file, b"moov").expect("a moov box");
    let mvhd = child(moov, b"mvhd").expect("an mvhd box");
    // mvhd version 0, as the writer declares it: the timescale is at byte 12.
    let timescale = be32(mvhd, 12);
    let trak = child(moov, b"trak").expect("a trak box");
    let elst = child(child(trak, b"edts").expect("an edts box"), b"elst").expect("an elst box");
    assert_eq!(be32(elst, 4), 1, "one edit: {}", path.display());
    let segment = be32(elst, 8);
    let media_time = be32(elst, 12) as i32 as i64;
    let rate = be32(elst, 16);
    assert_eq!(rate, 0x0001_0000, "media rate 1.0");
    (timescale, segment, media_time)
}

/// Exports the timeline's frames `start..end` as `format`, and checks the edit
/// list states exactly those frames' samples after the encoder's priming.
fn ranged_edit(format: Format, name: &str, start: u32, end: u32) {
    let tone = Scratch::file(&format!("ve_m4a_range_src_{name}"), "wav");
    write_tone(&tone);
    let session = PlaybackSession::open(&*tone).expect("open the tone");
    let ext = match format {
        Format::M4a => "m4a",
        _ => "mp4",
    };
    let out = Scratch::file(&format!("ve_m4a_range_{name}"), ext);
    wait(&session.export_to_with(
        &out,
        &ExportSettings {
            format,
            range: Some((start, end)),
            ..ExportSettings::default()
        },
    ))
    .expect("the ranged audio-only export finishes");

    let samples = u64::from(end - start) * u64::from(RATE) / u64::from(FPS);
    let (timescale, segment, media_time) = edit_of(&out);
    // The movie clock rounds up to whole ticks, as the writer's durations do.
    let want = (samples * u64::from(timescale)).div_ceil(u64::from(RATE));
    assert_eq!(
        u64::from(segment),
        want,
        "{}: segment_duration in {timescale}/s ticks for {samples} samples",
        out.display()
    );
    assert_eq!(media_time, 1024, "{}: the AAC priming", out.display());
}

/// 5 s to 7 s: 336000 samples, a whole number of seconds on the movie clock.
#[test]
fn an_m4a_range_states_exactly_its_samples() {
    ranged_edit(Format::M4a, "whole", 150, 360);
}

/// 5 s to 7.0333 s: 337600 samples, not a multiple of the 1024-sample frame and
/// not a whole number of milliseconds, so the movie clock rounds it up.
#[test]
fn a_non_integral_frame_range_states_exactly_its_samples() {
    ranged_edit(Format::M4a, "fraction", 150, 361);
}

/// The sound-only `.mp4` is the same AAC track and takes the same edit.
#[test]
fn an_audio_only_mp4_range_states_exactly_its_samples() {
    ranged_edit(Format::Mp4, "mp4", 150, 361);
}
