//! The standalone `.opus` export ([`Format::Opus`], [`engine::opus_write`]), read back with
//! ffprobe and ffmpeg: codec, 48 kHz, channels, a duration exact to the millisecond, a clean
//! decode, and the decode measured against the source (SNR); the 5.1 mapping-family-1 layout,
//! a ranged export, and chapters as `CHAPTERxxx` comments. The -16 LUFS `podcast_opus` preset
//! is measured in `export_presets.rs`. Without ffmpeg the tests say so and pass.
//!
//! ```text
//! cargo test -p engine --release --test opus_file_export -- --nocapture
//! ```

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::PlaybackSession;

const RATE: u32 = 48_000;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
        && Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Speech-like stereo: a 140 Hz voice with harmonics under a 4 Hz syllable envelope, a
/// different pitch on the right, at about -12 dBFS. `secs` long at `rate`.
fn speechy(rate: u32, secs: f64) -> Vec<f32> {
    let frames = (secs * f64::from(rate)) as usize;
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let t = i as f64 / f64::from(rate);
        let env = 0.55 + 0.45 * (t * 4.0 * std::f64::consts::TAU).sin();
        let voice = |f0: f64| {
            (1..=6).map(|h| (t * f0 * h as f64 * std::f64::consts::TAU).sin() / h as f64).sum::<f64>()
        };
        out.push((0.15 * env * voice(140.0)) as f32);
        out.push((0.15 * env * voice(190.0)) as f32);
    }
    out
}

fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
    let data = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&((s * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, wav).expect("write the source");
}

fn probe(path: &Path, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// ffmpeg decodes the whole file with nothing to say on stderr.
fn assert_clean_decode(path: &Path) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decode failed");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.trim().is_empty(), "ffmpeg reported: {err}");
}

/// The file decoded by ffmpeg as interleaved f32 at 48 kHz.
fn decode(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success());
    out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// SNR in dB of `got` against `want`, one channel of `channels`, over the middle (the codec's
/// first and last frames excluded), at lag zero.
fn snr(want: &[f32], got: &[f32], channels: usize, ch: usize) -> f64 {
    let frames = want.len().min(got.len()) / channels;
    let (mut sig, mut err) = (0f64, 0f64);
    for i in 960..frames.saturating_sub(960) {
        let (a, b) = (f64::from(want[i * channels + ch]), f64::from(got[i * channels + ch]));
        sig += a * a;
        err += (a - b) * (a - b);
    }
    10.0 * (sig / err.max(1e-20)).log10()
}

fn export(src: &Path, name: &str, settings: &ExportSettings) -> Scratch {
    let session = PlaybackSession::open(src).expect("open the source");
    let out = Scratch::file(&format!("ve_opusfile_{name}"), "opus");
    let handle = session.export_to_with(&out, settings);
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(result) = handle.result() {
            result.expect("the export finishes");
            break;
        }
        assert!(Instant::now() < deadline, "export never settled");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(handle.progress(), 1.0);
    out
}

/// The file's playing length in seconds, as ffprobe states it, less the pre-skip.
///
/// ffprobe's Ogg duration is the last granule over 48 kHz, pre-skip included -- a libopus file
/// ffmpeg writes of a 3 s sine probes at 3.0065 s with its 312-sample pre-skip, and decodes to
/// exactly 144000 samples. So the stated length is the granule, and the length a player plays
/// is that less the pre-skip in `OpusHead`; both are checked, and the decode's sample count
/// beside them in each test.
fn duration(path: &Path) -> f64 {
    let stated: f64 = probe(path, &["-show_entries", "format=duration"]).parse().expect("a duration");
    let bytes = std::fs::read(path).unwrap();
    let head = bytes.windows(8).position(|w| w == b"OpusHead").expect("an OpusHead");
    let pre_skip = u16::from_le_bytes([bytes[head + 10], bytes[head + 11]]);
    println!("ffprobe duration {stated:.6} s, pre-skip {pre_skip}");
    stated - f64::from(pre_skip) / f64::from(RATE)
}

#[test]
fn a_stereo_opus_export_probes_decodes_and_matches_its_source() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let src = Scratch::file("ve_opusfile_src", "wav");
    let pcm = speechy(RATE, 3.0);
    write_wav(&src, RATE, 2, &pcm);
    let out = export(
        &src,
        "stereo",
        &ExportSettings { format: Format::Opus, audio_kbps: Some(128), ..ExportSettings::default() },
    );
    assert_eq!(
        probe(&out, &["-select_streams", "a:0", "-show_entries", "stream=codec_name,sample_rate,channels"]),
        "opus,48000,2"
    );
    let d = duration(&out);
    println!("stereo: duration {d:.6} s (timeline 3.000000)");
    assert!((d - 3.0).abs() <= 0.001, "duration {d}");
    assert_clean_decode(&out);
    let got = decode(&out);
    assert_eq!(got.len(), pcm.len(), "decoded to the sample");
    let (l, r) = (snr(&pcm, &got, 2, 0), snr(&pcm, &got, 2, 1));
    println!("stereo 128k: SNR L {l:.1} dB, R {r:.1} dB");
    assert!(l > 20.0 && r > 20.0, "SNR {l} / {r}");
    let tags = probe(&out, &["-show_entries", "stream_tags=encoder"]);
    assert!(tags.contains("edith"), "encoder tag: {tags}");
}

#[test]
fn a_44k_timeline_is_mixed_at_48k_for_opus() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let src = Scratch::file("ve_opusfile_src44", "wav");
    write_wav(&src, 44_100, 2, &speechy(44_100, 2.0));
    let out = export(&src, "44k", &ExportSettings { format: Format::Opus, ..ExportSettings::default() });
    assert_eq!(probe(&out, &["-select_streams", "a:0", "-show_entries", "stream=sample_rate"]), "48000");
    let d = duration(&out);
    assert!((d - 2.0).abs() <= 0.001, "duration {d}");
    assert_clean_decode(&out);
    assert_eq!(decode(&out).len(), 2 * RATE as usize * 2, "two seconds at 48 kHz, to the sample");
}

#[test]
fn a_ranged_opus_export_holds_exactly_the_range() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let src = Scratch::file("ve_opusfile_srcr", "wav");
    let pcm = speechy(RATE, 4.0);
    write_wav(&src, RATE, 2, &pcm);
    // 30 fps audio-only canvas: frames 31..92 are 1.0333 s to 3.0667 s, 97600 samples.
    let out = export(
        &src,
        "range",
        &ExportSettings { format: Format::Opus, range: Some((31, 92)), ..ExportSettings::default() },
    );
    let want = 61.0 / 30.0;
    let d = duration(&out);
    println!("range: duration {d:.6} s (range {want:.6})");
    assert!((d - want).abs() <= 0.001, "duration {d}");
    assert_clean_decode(&out);
    let got = decode(&out);
    let from = 31 * 1600 * 2;
    let slice = &pcm[from..from + 61 * 1600 * 2];
    assert_eq!(got.len(), slice.len());
    let s = snr(slice, &got, 2, 0);
    println!("range: SNR L {s:.1} dB");
    assert!(s > 20.0, "SNR {s}");
}

#[test]
fn chapters_travel_as_vorbis_comments() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let src = Scratch::file("ve_opusfile_srcc", "wav");
    write_wav(&src, RATE, 2, &speechy(RATE, 3.0));
    let out = export(
        &src,
        "chapters",
        &ExportSettings {
            format: Format::Opus,
            chapters: vec![(0, "Intro".into()), (1_500_000_000, "Middle".into())],
            ..ExportSettings::default()
        },
    );
    let listed = probe(&out, &["-show_chapters"]);
    println!("chapters: {listed}");
    assert!(listed.contains("Intro") && listed.contains("Middle"), "{listed}");
    assert_eq!(listed.lines().count(), 2, "{listed}");
    assert!(listed.contains("1.500000"), "{listed}");
}

/// 5.1, written straight through the writer (a timeline mix is at most stereo): family 1,
/// four streams of which two coupled, and every WAV-order channel decoded back by ffmpeg in
/// its own place.
#[test]
fn a_5_1_opus_file_is_mapping_family_1_with_every_channel_in_place() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let freqs = [300.0, 500.0, 700.0, 60.0, 1100.0, 1300.0];
    let frames = RATE as usize * 2;
    let mut pcm = Vec::with_capacity(frames * 6);
    for i in 0..frames {
        let t = i as f64 / f64::from(RATE);
        for f in freqs {
            pcm.push((0.2 * (t * f * std::f64::consts::TAU).sin()) as f32);
        }
    }
    let out = Scratch::file("ve_opusfile_51", "opus");
    let tags = engine::opus_write::Tags::default();
    engine::opus_write::write(&out, &pcm, 6, RATE, 256, &tags, &mut |_| Ok(())).expect("write 5.1");
    let bytes = std::fs::read(&out).unwrap();
    let head = bytes.windows(8).position(|w| w == b"OpusHead").expect("an OpusHead");
    assert_eq!(bytes[head + 9], 6, "channels");
    assert_eq!(&bytes[head + 18..head + 21], &[1, 4, 2], "family 1, 4 streams, 2 coupled");
    assert_eq!(&bytes[head + 21..head + 27], &[0, 4, 1, 2, 3, 5]);
    assert_eq!(
        probe(&out, &["-select_streams", "a:0", "-show_entries", "stream=codec_name,sample_rate,channels"]),
        "opus,48000,6"
    );
    let d = duration(&out);
    assert!((d - 2.0).abs() <= 0.001, "duration {d}");
    assert_clean_decode(&out);
    let got = decode(&out);
    assert_eq!(got.len(), pcm.len());
    for ch in 0..6 {
        let s = snr(&pcm, &got, 6, ch);
        println!("5.1 256k: channel {ch} SNR {s:.1} dB");
        // The LFE (channel 3, a 60 Hz tone) is the hardest thing a CELT frame codes; the mapping
        // claim is the next one, that each channel came back where it went in.
        assert!(s > if ch == 3 { 8.0 } else { 20.0 }, "channel {ch}: SNR {s}");
        for other in (0..6).filter(|&o| o != ch) {
            let (mut num, mut a2, mut b2) = (0f64, 0f64, 0f64);
            for i in 960..frames - 960 {
                let (a, b) = (f64::from(pcm[i * 6 + other]), f64::from(got[i * 6 + ch]));
                num += a * b;
                a2 += a * a;
                b2 += b * b;
            }
            let c = num / (a2 * b2).sqrt();
            assert!(c.abs() < 0.1, "decoded channel {ch} carries source channel {other}: {c}");
        }
    }
}
