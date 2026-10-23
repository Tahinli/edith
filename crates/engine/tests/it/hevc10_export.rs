//! HEVC Main 10 with HDR signalling (`ExportSettings::hdr`): a PQ or HLG export
//! of a PQ or HLG source is Main 10, `yuv420p10le`, tagged BT.2020 with the
//! right transfer in the stream and in the container (mp4 `colr`, Matroska
//! `Colour`), carries the mastering display and content light SEIs, and decodes
//! back within a PSNR floor of the source with *no* tone map in between. The
//! refusals (HDR in 8 bits, HDR in H.264, one HDR curve into the other) are by
//! name. And an export that asks for none of it is the file it always was: the
//! default exports' bytes are pinned by hash.
//!
//! Sources are synthetic (ffmpeg `testsrc2`). Without ffmpeg and a 10-bit source
//! encoder the tests say so and pass.
//!
//! ```text
//! cargo test -p engine --release --test hevc10_export -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::export::{ExportSettings, Format, HdrOutput, HdrTransfer, MasteringDisplay};

use crate::seat;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 25;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

fn source_encoder() -> Option<&'static str> {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().ok()?;
    let list = String::from_utf8_lossy(&out.stdout);
    if list.contains("libx265") {
        Some("libx265")
    } else if list.contains("libaom-av1") {
        Some("libaom-av1")
    } else {
        None
    }
}

fn codec_args(encoder: &str) -> Vec<&str> {
    match encoder {
        "libx265" => vec!["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error"],
        _ => vec!["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "30", "-b:v", "0"],
    }
}

/// A 1-second `testsrc2` at `pix_fmt` in Matroska, tagged BT.2020 with `trc`
/// (`smpte2084`, `arib-std-b67`) where one is given.
fn make_source(out: &Path, pix_fmt: &str, trc: Option<&str>, codec: &[&str]) -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={WIDTH}x{HEIGHT}:rate={FPS}:duration=1"))
        .args(["-pix_fmt", pix_fmt])
        .args(codec);
    if let Some(trc) = trc {
        // The frame properties as well as the encoder's flags: a filter graph's
        // unspecified frames would otherwise override the flags.
        cmd.args(["-vf", &format!("setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc={trc}:range=tv")]);
        cmd.args(["-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", trc, "-color_range", "tv"]);
    }
    matches!(cmd.arg(out).status(), Ok(s) if s.success())
}

fn probe(path: &Path, field: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(format!("stream={field}"))
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    // The container's static metadata is side data, which csv prints after the field.
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().next().unwrap_or("").trim_end_matches(',').to_owned()
}

/// ffprobe's first-frame side data, as text.
fn first_frame_side_data(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-read_intervals", "%+#1", "-show_frames"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn decoded(path: &Path) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decodes {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn psnr_10(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let mut squared = 0f64;
    for (x, y) in a.as_chunks::<2>().0.iter().zip(b.as_chunks::<2>().0) {
        let d = f64::from(u16::from_le_bytes(*x)) - f64::from(u16::from_le_bytes(*y));
        squared += d * d;
    }
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (1023.0f64 * 1023.0 * (a.len() / 2) as f64 / squared).log10()
}

fn run(source: &Path, out: &Path, settings: &ExportSettings) -> Result<(), String> {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let handle = session.export_to_with(out, settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").map_err(|e| e.to_string())
}

fn hdr(transfer: HdrTransfer) -> HdrOutput {
    HdrOutput {
        transfer,
        mastering_display: Some(MasteringDisplay::p3_d65(1000.0, 0.005)),
        max_cll: Some(900),
        max_fall: Some(300),
    }
}

#[test]
fn pq_and_hlg_exports_are_main_10_bt2020_and_decode_back() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a 10-bit source encoder: skipping the HDR export test");
        return;
    };
    let _seat = seat::software();
    let dir = scratch("hevc10-hdr");
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2 * 2) as usize;
    for (transfer, trc, tag) in [(HdrTransfer::Pq, "smpte2084", "smpte2084"), (HdrTransfer::Hlg, "arib-std-b67", "arib-std-b67")] {
        let source = dir.join(format!("source-{tag}.mkv"));
        assert!(make_source(&source, "yuv420p10le", Some(trc), &codec_args(encoder)), "ffmpeg makes the HDR source");
        assert_eq!(probe(&source, "color_transfer"), tag, "the source is tagged {tag}");
        let reference = decoded(&source);
        for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
            let out = dir.join(format!("{tag}.{ext}"));
            let settings = ExportSettings { format, hdr: hdr(transfer), ..Default::default() };
            run(&source, &out, &settings).unwrap_or_else(|e| panic!("{tag} {ext}: {e}"));

            assert_eq!(probe(&out, "profile"), "Main 10", "{tag} {ext}");
            assert_eq!(probe(&out, "pix_fmt"), "yuv420p10le", "{tag} {ext}");
            assert_eq!(probe(&out, "color_transfer"), tag, "{tag} {ext}: transfer");
            assert_eq!(probe(&out, "color_primaries"), "bt2020", "{tag} {ext}: primaries");
            assert_eq!(probe(&out, "color_space"), "bt2020nc", "{tag} {ext}: matrix");
            assert_eq!(probe(&out, "color_range"), "tv", "{tag} {ext}: range");
            // The bitstream says top-left (type 2); an mp4 reads it from there. A
            // Matroska reader answers for the container, which states no siting.
            if ext == "mp4" {
                assert_eq!(probe(&out, "chroma_location"), "topleft", "{tag} {ext}: chroma location");
            }
            let side = first_frame_side_data(&out);
            assert!(side.contains("Mastering display metadata"), "{tag} {ext}: mastering display SEI\n{side}");
            assert!(side.contains("Content light level metadata"), "{tag} {ext}: content light SEI");
            assert!(side.contains("max_content=900") && side.contains("max_average=300"), "{tag} {ext}: MaxCLL/MaxFALL");

            let back = decoded(&out);
            assert_eq!(back.len(), reference.len(), "{tag} {ext}: every picture reads back");
            assert!(back.len() / frame_bytes >= FPS as usize - 1);
            let psnr = psnr_10(&reference, &back);
            eprintln!("{tag} {ext}: {psnr:.2} dB against the HDR source");
            // No tone map and no matrix remap: the 10-bit pictures pass through untouched.
            assert!(psnr > 30.0, "{tag} {ext}: {psnr:.2} dB against the source");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_sdr_source_can_be_tagged_hdr_and_hdr_without_the_extras_has_no_sei() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a 10-bit source encoder: skipping");
        return;
    };
    let _seat = seat::software();
    let dir = scratch("hevc10-sdr-as-hdr");
    let source = dir.join("sdr.mkv");
    assert!(make_source(&source, "yuv420p", None, &codec_args(encoder)));
    let out = dir.join("sdr-as-pq.mp4");
    let settings = ExportSettings {
        format: Format::HevcMp4,
        hdr: HdrOutput { transfer: HdrTransfer::Pq, ..Default::default() },
        ..Default::default()
    };
    run(&source, &out, &settings).expect("an SDR source exports tagged PQ: a user choice");
    assert_eq!(probe(&out, "profile"), "Main 10", "HDR is 10-bit even from 8-bit pictures");
    assert_eq!(probe(&out, "color_transfer"), "smpte2084");
    assert_eq!(probe(&out, "color_primaries"), "bt2020");
    let side = first_frame_side_data(&out);
    assert!(!side.contains("Mastering display metadata"), "no mastering display SEI unless asked");
    assert!(!side.contains("Content light level metadata"), "no content light SEI unless asked");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hdr_that_cannot_be_written_is_refused_by_name() {
    let (Some(encoder), true) = (source_encoder(), have("ffmpeg")) else {
        eprintln!("no ffmpeg with a 10-bit source encoder: skipping");
        return;
    };
    let _seat = seat::software();
    let dir = scratch("hevc10-refusals");
    let source = dir.join("pq.mkv");
    assert!(make_source(&source, "yuv420p10le", Some("smpte2084"), &codec_args(encoder)));
    let out = dir.join("refused.mp4");

    let eight = ExportSettings { format: Format::HevcMp4, hdr: hdr(HdrTransfer::Pq), video_bit_depth: Some(8), ..Default::default() };
    let why = run(&source, &out, &eight).expect_err("HDR in 8 bits");
    assert!(why.contains("10-bit"), "{why}");

    let h264 = ExportSettings { format: Format::Mp4, hdr: hdr(HdrTransfer::Pq), ..Default::default() };
    let why = run(&source, &out, &h264).expect_err("HDR in H.264");
    assert!(why.contains("only HEVC"), "{why}");

    let curve = ExportSettings { format: Format::HevcMp4, hdr: hdr(HdrTransfer::Hlg), ..Default::default() };
    let why = run(&source, &out, &curve).expect_err("PQ source into an HLG export");
    assert!(why.contains("PQ") && why.contains("HLG"), "{why}");

    let stray = ExportSettings {
        format: Format::HevcMp4,
        hdr: HdrOutput { max_cll: Some(1000), ..Default::default() },
        ..Default::default()
    };
    let why = run(&source, &out, &stray).expect_err("light levels without an HDR transfer");
    assert!(why.contains("HDR transfer"), "{why}");
    assert!(!out.exists(), "a refused export leaves no file");
    let _ = std::fs::remove_dir_all(&dir);
}

/// ffprobe's `-show_streams` text of the video stream (container-level side data).
fn stream_side_data(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_streams"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn hdr_static_metadata_is_in_the_container_as_well_as_the_sei() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a 10-bit source encoder: skipping the container HDR test");
        return;
    };
    let _seat = seat::software();
    let dir = scratch("hevc10-container-hdr");
    let source = dir.join("source.mkv");
    assert!(make_source(&source, "yuv420p10le", Some("smpte2084"), &codec_args(encoder)), "ffmpeg makes the HDR source");
    for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
        let out = dir.join(format!("hdr.{ext}"));
        run(&source, &out, &ExportSettings { format, hdr: hdr(HdrTransfer::Pq), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ext}: {e}"));
        let streams = stream_side_data(&out);
                let frames = first_frame_side_data(&out);
        for (what, text) in [("show_streams", &streams), ("show_frames", &frames)] {
            assert!(text.contains("Mastering display metadata"), "{ext} {what}: mastering display");
            assert!(text.contains("Content light level metadata"), "{ext} {what}: content light");
            assert!(text.contains("max_content=900") && text.contains("max_average=300"), "{ext} {what}: MaxCLL/FALL");
            // P3 D65 at 1000 / 0.005 nits (red 0.68,0.32 green 0.265,0.69 blue 0.15,0.06
            // white 0.3127,0.329). ffprobe prints a Matroska's floats as reduced rationals, an mp4's boxes and
            // the SEI's as the bitstream codes them (0.00002 and 0.0001 steps).
            let want: [&str; 10] = if what == "show_streams" && ext == "mkv" {
                [
                    "red_x=17/25", "red_y=8/25", "green_x=53/200", "green_y=69/100", "blue_x=3/20", "blue_y=3/50",
                    "white_point_x=3127/10000", "white_point_y=329/1000", "min_luminance=1/200", "max_luminance=1000/1",
                ]
            } else {
                [
                    "red_x=34000/50000", "red_y=16000/50000", "green_x=13250/50000", "green_y=34500/50000",
                    "blue_x=7500/50000", "blue_y=3000/50000", "white_point_x=15635/50000", "white_point_y=16450/50000",
                    "min_luminance=50/10000", "max_luminance=10000000/10000",
                ]
            };
            for v in want {
                assert!(text.contains(v), "{ext} {what}: {v}\n{text}");
            }
        }
    }
    // Not HDR: nothing new in the container.
    let sdr = dir.join("sdr.mp4");
    let sdr_source = dir.join("sdr-source.mkv");
    assert!(make_source(&sdr_source, "yuv420p10le", None, &codec_args(encoder)));
    run(&sdr_source, &sdr, &ExportSettings { format: Format::HevcMp4, video_bit_depth: Some(10), ..Default::default() }).expect("sdr");
    let text = stream_side_data(&sdr);
    assert!(!text.contains("Mastering display") && !text.contains("Content light"), "sdr stream:\n{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Planar 4:2:0 PSNR of two byte streams of `bytes_per_sample`-wide samples.
fn psnr_planar(a: &[u8], b: &[u8], ten: bool) -> f64 {
    if ten {
        return psnr_10(a, b);
    }
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let squared: f64 = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum();
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 * a.len() as f64 / squared).log10()
}

fn frame_count(path: &Path) -> usize {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().trim_end_matches(',').parse().expect("a packet count")
}

fn decoded_as(path: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decodes {}", path.display());
    out.stdout
}

/// The speed presets (`ExportSettings::hevc_speed`) write streams that decode: the same
/// number of pictures as the default export, at a PSNR close to its, in 8 and 10 bits.
#[test]
fn hevc_speed_presets_decode_close_to_the_default() {
    use engine::export::HevcSpeed;
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the HEVC speed test");
        return;
    };
    let _seat = seat::software();
    let dir = scratch("hevc-speed");
    for (pix_fmt, ten) in [("yuv420p", false), ("yuv420p10le", true)] {
        let source = dir.join(format!("source-{pix_fmt}.mkv"));
        assert!(make_source(&source, pix_fmt, None, &codec_args(encoder)), "ffmpeg makes the source");
        let reference = decoded_as(&source, pix_fmt);
        let mut base: Option<(f64, usize)> = None;
        for (name, speed) in [("default", None), ("fast", Some(HevcSpeed::Fast)), ("faster", Some(HevcSpeed::Faster))] {
            let out = dir.join(format!("{pix_fmt}-{name}.mkv"));
            let settings = ExportSettings {
                format: Format::Hevc,
                video_bit_depth: Some(if ten { 10 } else { 8 }),
                hevc_speed: speed,
                ..Default::default()
            };
            run(&source, &out, &settings).unwrap_or_else(|e| panic!("{pix_fmt} {name}: {e}"));
            assert_eq!(probe(&out, "pix_fmt"), pix_fmt, "{pix_fmt} {name}");
            let back = decoded_as(&out, pix_fmt);
            assert_eq!(back.len(), reference.len(), "{pix_fmt} {name}: every picture reads back");
            let frames = frame_count(&out);
            let psnr = psnr_planar(&reference, &back, ten);
            eprintln!("{pix_fmt} {name}: {frames} frames, {psnr:.2} dB");
            assert!(psnr > 25.0, "{pix_fmt} {name}: {psnr:.2} dB");
            match base {
                None => base = Some((psnr, frames)),
                Some((p0, f0)) => {
                    assert_eq!(frames, f0, "{pix_fmt} {name}: the frame count of the default");
                    let slack = if speed == Some(HevcSpeed::Fast) { 1.0 } else { 3.0 };
                    assert!(p0 - psnr < slack, "{pix_fmt} {name}: {psnr:.2} dB against the default's {p0:.2}");
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
