//! Every picture path of a 10-bit export keeps 16-bit samples end to end: a smooth
//! 10-bit ramp goes through each path (plain, grade, LUT, effects, placement,
//! dissolve, layers, adjustment layer, title, speed, ramp, stabilise) into a
//! 10-bit HEVC, and the decoded output must hold more than 256 distinct luma
//! codes (an 8-bit staircase has at most 256 at 10-bit codes, here fewer). Where the path
//! should leave frame 0 as the source, that frame is also held to the source by PSNR.
//!
//! ```text
//! cargo test -p engine --release --test export_deep_ten_bit -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::color::ColorParams;
use engine::export::{ExportSettings, Format};
use engine::project::{Lane, LaneKind, Title};
use engine::text::TextStyle;
use engine::transform::TransformParams;

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 25;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-deep10-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A 1-second 10-bit diagonal ramp over the limited span (64..940), neutral chroma,
/// as lossless Main 10 HEVC.
fn make_source(out: &Path) -> bool {
    let (w, h) = (W as usize, H as usize);
    let mut raw = Vec::new();
    for _ in 0..FPS {
        for y in 0..h {
            for x in 0..w {
                let code = 64 + (x + y) * 876 / (w + h - 2);
                raw.extend_from_slice(&(code as u16).to_le_bytes());
            }
        }
        for _ in 0..2 * (w / 2) * (h / 2) {
            raw.extend_from_slice(&512u16.to_le_bytes());
        }
    }
    let input = out.with_extension("yuv");
    if std::fs::write(&input, &raw).is_err() {
        return false;
    }
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le"])
        .args(["-s", &format!("{W}x{H}"), "-r", &FPS.to_string()])
        .arg("-i")
        .arg(&input)
        .args(["-pix_fmt", "yuv420p10le", "-c:v", "libx265", "-preset", "ultrafast"])
        .args(["-x265-params", "log-level=error:lossless=1"])
        .arg(out)
        .status();
    let _ = std::fs::remove_file(&input);
    matches!(status, Ok(s) if s.success())
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

fn pix_fmt(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn psnr_10(a: &[u8], b: &[u8]) -> f64 {
    let mut squared = 0f64;
    let mut count = 0usize;
    for (x, y) in a.as_chunks::<2>().0.iter().zip(b.as_chunks::<2>().0) {
        let d = f64::from(u16::from_le_bytes(*x)) - f64::from(u16::from_le_bytes(*y));
        squared += d * d;
        count += 1;
    }
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (1023.0f64 * 1023.0 * count as f64 / squared).log10()
}

const FRAME_LUMA: usize = (W * H * 2) as usize;
const FRAME_BYTES: usize = (W * H * 3 / 2 * 2) as usize;

/// Distinct luma codes over the output's picture, counted on every frame together.
fn levels(frames: &[u8]) -> usize {
    let mut seen = [false; 1024];
    for frame in frames.as_chunks::<FRAME_BYTES>().0 {
        for s in frame[..FRAME_LUMA].as_chunks::<2>().0 {
            seen[usize::from(u16::from_le_bytes(*s)).min(1023)] = true;
        }
    }
    seen.iter().filter(|&&s| s).count()
}

/// The share of a frame's luma samples that sit exactly on the 8-bit lattice (`v << 2 | v >> 6`):
/// an 8-bit picture shifted up lands on it entirely, a true 10-bit one on about a quarter.
fn lattice_share(frame: &[u8]) -> f64 {
    let on: std::collections::HashSet<u16> = (0..256u16).map(|v| v << 2 | v >> 6).collect();
    let luma = frame[..FRAME_LUMA].as_chunks::<2>().0;
    luma.iter().filter(|s| on.contains(&u16::from_le_bytes(**s))).count() as f64 / luma.len() as f64
}

/// The most distinct luma codes any single frame holds.
fn best_frame_levels(frames: &[u8]) -> usize {
    frames.as_chunks::<FRAME_BYTES>().0.iter().map(|f| levels(f)).max().unwrap_or(0)
}

/// Runs `setup` on a fresh session over the ramp, exports a 10-bit AV1 (near lossless), and returns the
/// decoded output and the source decoded. Asserts the export is 10-bit.
fn run(name: &str, setup: impl FnOnce(&mut PlaybackSession, &Path)) -> Option<(Vec<u8>, Vec<u8>)> {
    if !(have("ffmpeg") && have("ffprobe")) {
        eprintln!("no ffmpeg: skipping {name}");
        return None;
    }
    let dir = scratch(name);
    let source = dir.join("ramp.mkv");
    if !make_source(&source) {
        eprintln!("no libx265: skipping {name}");
        return None;
    }
    let mut session = PlaybackSession::open(&source).expect("open the ramp");
    setup(&mut session, &source);
    let out = dir.join("out.mkv");
    let settings = ExportSettings { format: Format::Av1, bitrate: Some(200_000_000), ..Default::default() };
    let handle = session.export_to_with(&out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "{name}: the export did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
    assert_eq!(pix_fmt(&out), "yuv420p10le", "{name}: the export is 10-bit");
    let (got, src) = (decoded(&out), decoded(&source));
    let _ = std::fs::remove_dir_all(&dir);
    Some((got, src))
}

/// The path must keep more than 256 luma codes on a picture; with `same_first`, frame 0
/// must also be the source's within `psnr_floor`.
fn check(name: &str, setup: impl FnOnce(&mut PlaybackSession, &Path), psnr_floor: Option<f64>) {
    let Some((got, src)) = run(name, setup) else { return };
    let (all, best) = (levels(&got), best_frame_levels(&got));
    let psnr = psnr_10(&got[..FRAME_BYTES], &src[..FRAME_BYTES]);
    eprintln!("DEEP10 {name}: {all} distinct luma codes over the clip, {best} on the best frame, frame-0 PSNR {psnr:.1} dB");
    // The codec is near lossless but not exact, so the distinct count alone cannot tell a staircase
    // from a ramp; the lattice share can: an 8-bit picture sits on it, a 10-bit one on about a quarter.
    let lattice = got.as_chunks::<FRAME_BYTES>().0.iter().map(|f| lattice_share(f)).fold(0.0, f64::max);
    eprintln!("DEEP10 {name}: worst frame {:.0}% on the 8-bit lattice", 100.0 * lattice);
    assert!(lattice < 0.6, "{name}: {:.0}% of a frame's luma sits on the 8-bit lattice: narrowed to 8 bits somewhere", 100.0 * lattice);
    assert!(best > 256, "{name}: {best} luma codes on a frame: narrowed to 8 bits somewhere");
    if let Some(floor) = psnr_floor {
        assert!(psnr > floor, "{name}: frame 0 is {psnr:.1} dB from the source, under {floor}");
    }
}

#[test]
fn plain_clip() {
    check("plain", |_, _| {}, Some(60.0));
}

#[test]
fn grade() {
    check(
        "grade",
        |s, _| {
            assert!(s.set_color(Lane::V1, 0, Some(ColorParams { brightness: 0.03, contrast: 1.05, ..Default::default() })));
        },
        None,
    );
}

#[test]
fn lut() {
    check(
        "lut",
        |s, src| {
            let cube = src.with_extension("cube");
            let size = 17;
            let mut t = format!("LUT_3D_SIZE {size}\n");
            let e = (size - 1) as f32;
            for b in 0..size {
                for g in 0..size {
                    for r in 0..size {
                        let f = |v: f32| (v * 0.9 + 0.03).clamp(0.0, 1.0);
                        t.push_str(&format!("{:.6} {:.6} {:.6}\n", f(r as f32 / e), f(g as f32 / e), f(b as f32 / e)));
                    }
                }
            }
            std::fs::write(&cube, t).expect("cube");
            let lut = engine::cube::LutRef::reload(&cube);
            let look = engine::color::Look { lut: Some(lut), ..Default::default() };
            assert!(s.set_color(Lane::V1, 0, Some(ColorParams { look, ..Default::default() })));
        },
        None,
    );
}

#[test]
fn effects() {
    use engine::effects::{Effect, EffectKind};
    check(
        "effects",
        |s, _| {
            let mut v = Effect::new(EffectKind::Vignette);
            v.set("amount", 0.5).unwrap();
            assert!(s.set_effects(Lane::V1, 0, vec![v]));
        },
        None,
    );
}

#[test]
fn transform_and_crop() {
    check(
        "transform",
        |s, _| {
            let t = TransformParams { scale: 0.9, crop_l: 0.05, crop_t: 0.05, ..Default::default() };
            assert!(s.set_transform(Lane::V1, 0, Some(t)));
        },
        None,
    );
}

#[test]
fn dissolve() {
    check(
        "dissolve",
        |s, _| {
            assert!(s.cut_at(0.5));
            assert!(s.set_transition_out(Lane::V1, 0, 8));
        },
        Some(60.0),
    );
}

#[test]
fn layers() {
    check(
        "layers",
        |s, src| {
            let v2 = s.add_lane(LaneKind::Video);
            assert!(s.place_stream_at(0.0, src, 0, Some(v2)).expect("placed"));
            let t = TransformParams { opacity: 0.6, scale: 0.8, ..Default::default() };
            assert!(s.set_transform(v2, 0, Some(t)));
        },
        None,
    );
}

#[test]
fn layers_opaque_over_base_is_the_source() {
    check(
        "layers-opaque",
        |s, src| {
            let v2 = s.add_lane(LaneKind::Video);
            assert!(s.place_stream_at(0.0, src, 0, Some(v2)).expect("placed"));
        },
        Some(60.0),
    );
}

#[test]
fn adjustment_layer() {
    check(
        "adjust",
        |s, src| {
            let v2 = s.add_lane(LaneKind::Video);
            let _ = src;
            let adjust = s.generator_named("adjust").expect("adjust");
            assert!(s.place_generated(adjust, 0.0, Some(1.0), Some(v2)).expect("placed"));
            assert!(s.set_color(v2, 0, Some(ColorParams { brightness: 0.03, ..Default::default() })));
        },
        None,
    );
}

#[test]
fn title() {
    check(
        "title",
        |s, _| {
            s.add_title(Title { start: 0, frames: 25, text: "T".into(), style: TextStyle::default(), fade_in: 0, fade_out: 0, in_anim: Default::default(), out_anim: Default::default(), anim_frames: 0 })
                .expect("title");
        },
        None,
    );
}

#[test]
fn slowed_clip() {
    check(
        "slow",
        |s, _| {
            s.set_speed(Lane::V1, 0, engine::project::Speed::from_permille(500)).expect("slow it");
        },
        Some(60.0),
    );
}

#[test]
fn speed_ramp() {
    use engine::project::{Speed, SpeedKey};
    check(
        "ramp",
        |s, _| {
            let keys = [SpeedKey { at: 0, speed: Speed::NORMAL }, SpeedKey { at: 12, speed: Speed::from_permille(2000) }];
            s.set_speed_ramp(Lane::V1, 0, &keys).expect("the ramp sets");
        },
        Some(60.0),
    );
}

#[test]
fn layered_dissolve_and_effects() {
    use engine::effects::{Effect, EffectKind};
    check(
        "layers-fx",
        |s, src| {
            let v2 = s.add_lane(LaneKind::Video);
            assert!(s.place_stream_at(0.0, src, 0, Some(v2)).expect("placed"));
            assert!(s.cut_at_lanes(0.5, &[v2]));
            assert!(s.set_transition_out(v2, 0, 6));
            let mut v = Effect::new(EffectKind::Vignette);
            v.set("amount", 0.3).unwrap();
            assert!(s.set_effects(v2, 0, vec![v]));
        },
        None,
    );
}

#[test]
fn layered_stabilised_clip() {
    check(
        "layers-stab",
        |s, src| {
            let v2 = s.add_lane(LaneKind::Video);
            assert!(s.place_stream_at(0.0, src, 0, Some(v2)).expect("placed"));
            let st = engine::stabilise::Stabilise::new(0.5, engine::stabilise::Crop::Zoom);
            let _ = s.set_stabilise(v2, 0, Some(st));
        },
        None,
    );
}
