//! A clip's effects stack ([`engine::effects`]) in the picture paths: preview's
//! composer, the 8-bit export and the 10-bit one. A stack that does nothing --
//! none, or every effect switched off -- leaves every byte where it was; one that
//! does something shows in all three, and a key or a mask on an upper lane reveals
//! the lane beneath. Software encoders only.
//!
//! ```text
//! cargo test --release -p engine --test effects
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::effects::{Effect, EffectKind};
use engine::export::{EncoderSeat, ExportSettings, Format};
use engine::project::{Lane, LaneKind};
use engine::scratch::Scratch;

const TOP: Lane = Lane::new(LaneKind::Video, 1);

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn open(path: impl AsRef<Path>) -> PlaybackSession {
    engine::ao::set_silent_output();
    PlaybackSession::open(path).expect("open")
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

fn export(session: &PlaybackSession, out: &Path, settings: &ExportSettings) {
    let handle = session.export_to_with(out, settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

fn software(range: (u32, u32)) -> ExportSettings {
    ExportSettings {
        format: Format::Mp4,
        seat: EncoderSeat::Software,
        range: Some(range),
        ..Default::default()
    }
}

/// The mean of a BGRA frame's first channel over a `size`-square box at `(x, y)`.
fn mean(frame: &engine::decode::Frame, x: u32, y: u32, size: u32) -> f64 {
    let mut sum = 0.0;
    for row in y..y + size {
        for col in x..x + size {
            let i = ((row * frame.width + col) * 4) as usize;
            sum += f64::from(frame.bgra[i]) + f64::from(frame.bgra[i + 1]) + f64::from(frame.bgra[i + 2]);
        }
    }
    sum / f64::from(size * size * 3)
}

fn vignette_and_blur() -> Vec<Effect> {
    let mut vignette = Effect::new(EffectKind::Vignette);
    vignette.set("amount", 0.9).unwrap();
    vignette.set("softness", 0.6).unwrap();
    let mut blur = Effect::new(EffectKind::Blur);
    blur.set("radius", 8.0).unwrap();
    vec![vignette, blur]
}

#[test]
fn a_stack_that_does_nothing_leaves_every_byte_alone() {
    let dir = Scratch::dir("ve_effects_identity");
    let plain = open(asset("test_av.mp4"));
    let mut off = open(asset("test_av.mp4"));
    let stack: Vec<Effect> = vignette_and_blur().into_iter().map(|e| Effect { on: false, ..e }).collect();
    assert!(off.set_effects(Lane::V1, 0, stack));
    assert_eq!(off.effects_of(Lane::V1, 0).len(), 2, "the stack is kept, switched off");
    // The composer: the same picture.
    assert_eq!(plain.compose_at(20, 640, 360).unwrap().bgra, off.compose_at(20, 640, 360).unwrap().bgra);
    // The export: the same file, byte for byte.
    let (a, b) = (dir.join("plain.mp4"), dir.join("off.mp4"));
    export(&plain, &a, &software((0, 12)));
    export(&off, &b, &software((0, 12)));
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "a switched-off stack is no stack");
}

#[test]
fn vignette_and_blur_show_in_the_composer_and_one_undo_takes_them_off() {
    let mut s = open(asset("test_av.mp4"));
    let before = s.compose_at(20, 640, 360).unwrap();
    assert!(s.set_effects(Lane::V1, 0, vignette_and_blur()));
    let after = s.compose_at(20, 640, 360).unwrap();
    assert!(mean(&after, 0, 0, 24) < mean(&before, 0, 0, 24) * 0.6, "the corner is darker");
    assert_ne!(before.bgra, after.bgra);
    assert!(s.undo(), "one undo step");
    assert_eq!(s.compose_at(20, 640, 360).unwrap().bgra, before.bgra, "undone: the picture it was");
}

#[test]
fn a_mask_on_the_upper_lane_reveals_the_lane_beneath() {
    let dir = Scratch::dir("ve_effects_mask");
    let project = dir.join("layers.edith");
    let text = format!(
        "edith 4\nplayhead 0\nsource 0 {}\nsource 0 {}\n\
         video 1 0 0 150 0 -\naudio 1 0 0 150 0 -\n\
         video 2 20 0 60 1 -\n",
        asset("test_av.mp4").display(),
        asset("test_av2.mp4").display(),
    );
    std::fs::write(&project, text).unwrap();
    let mut s = PlaybackSession::open_project(&project).expect("open the layers");
    let (w, h) = (640, 360);
    let top = s.compose_at(30, w, h).unwrap();
    assert!(s.set_lane_hidden(TOP, true));
    let bottom = s.compose_at(30, w, h).unwrap();
    assert!(s.set_lane_hidden(TOP, false));
    // A box over the left half, hard-edged: the top clip there, the bottom one elsewhere.
    let mut mask = Effect::new(EffectKind::RectMask);
    for (k, v) in [("x", 0.25), ("y", 0.5), ("width", 0.5), ("height", 1.0), ("feather", 0.0)] {
        mask.set(k, v).unwrap();
    }
    assert!(s.set_effects(TOP, 0, vec![mask.clone()]));
    let masked = s.compose_at(30, w, h).unwrap();
    let near = |a: &engine::decode::Frame, b: &engine::decode::Frame, x: u32, y: u32| (mean(a, x, y, 16) - mean(b, x, y, 16)).abs() < 3.0;
    assert!(near(&masked, &top, 60, 160), "inside the mask: the top clip");
    assert!(near(&masked, &bottom, 520, 160), "outside: the lane beneath");
    // Inverted, the other way round.
    mask.set("invert", 1.0).unwrap();
    assert!(s.set_effects(TOP, 0, vec![mask]));
    let inverted = s.compose_at(30, w, h).unwrap();
    assert!(near(&inverted, &bottom, 60, 160) && near(&inverted, &top, 520, 160));
    // Screen over the bottom is never darker than the bottom.
    let mut blend = Effect::new(EffectKind::Blend);
    blend.set("mode", 2.0).unwrap();
    assert!(s.set_effects(TOP, 0, vec![blend]));
    let screened = s.compose_at(30, w, h).unwrap();
    assert!(mean(&screened, 300, 160, 32) >= mean(&bottom, 300, 160, 32) - 1.0);
}

#[test]
fn an_eight_bit_export_carries_the_effects() {
    if !have("ffmpeg") {
        eprintln!("no ffmpeg: skipping");
        return;
    }
    let dir = Scratch::dir("ve_effects_export8");
    let plain = open(asset("test_av.mp4"));
    let mut fx = open(asset("test_av.mp4"));
    assert!(fx.set_effects(Lane::V1, 0, vignette_and_blur()));
    let (a, b) = (dir.join("plain.mp4"), dir.join("fx.mp4"));
    export(&plain, &a, &software((0, 10)));
    export(&fx, &b, &software((0, 10)));
    let frame = |p: &Path| {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(p)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .unwrap();
        out.stdout
    };
    let (pa, pb) = (frame(&a), frame(&b));
    assert_eq!(pa.len(), 1280 * 720);
    let corner = |f: &[u8]| (0..32).flat_map(|r| (0..32).map(move |c| r * 1280 + c)).map(|i| f64::from(f[i])).sum::<f64>();
    assert!(corner(&pb) < corner(&pa) * 0.7, "the exported corner is darker");
}

/// A 10-bit source with a lone blurred clip composes at 10 bits: its export keeps
/// more than 256 luma levels, which an 8-bit pass could not.
#[test]
fn a_ten_bit_export_draws_the_effects_at_ten_bits() {
    if !have("ffmpeg") {
        eprintln!("no ffmpeg: skipping");
        return;
    }
    let dir = Scratch::dir("ve_effects_export10");
    let source = dir.join("ramp.mkv");
    let (w, h) = (320usize, 240usize);
    let mut raw = Vec::new();
    for _ in 0..25 {
        for y in 0..h {
            for x in 0..w {
                raw.extend_from_slice(&((64 + (x + y) * 876 / (w + h - 2)) as u16).to_le_bytes());
            }
        }
        for _ in 0..2 * (w / 2) * (h / 2) {
            raw.extend_from_slice(&512u16.to_le_bytes());
        }
    }
    let input = dir.join("ramp.yuv");
    std::fs::write(&input, &raw).unwrap();
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-s", "320x240", "-r", "25", "-i"])
        .arg(&input)
        .args(["-pix_fmt", "yuv420p10le", "-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "20", "-b:v", "0"])
        .arg(&source)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("no 10-bit source encoder: skipping");
        return;
    }
    let mut s = PlaybackSession::open(&source).expect("open the ramp");
    let mut blur = Effect::new(EffectKind::Blur);
    blur.set("radius", 6.0).unwrap();
    assert!(s.set_effects(Lane::V1, 0, vec![blur, Effect::new(EffectKind::Vignette)]));
    let out = dir.join("fx.mkv");
    export(
        &s,
        &out,
        &ExportSettings { format: Format::Av1, video_bit_depth: Some(10), bitrate: Some(50_000_000), ..Default::default() },
    );
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .unwrap()
        .stdout;
    let luma = &raw[..320 * 240 * 2];
    let mut seen = [false; 1024];
    for s in luma.as_chunks::<2>().0 {
        seen[usize::from(u16::from_le_bytes(*s)).min(1023)] = true;
    }
    let levels = seen.iter().filter(|&&s| s).count();
    assert!(levels > 256, "{levels} luma levels: narrowed to 8 bits somewhere");
    let last = u16::from_le_bytes([luma[luma.len() - 2], luma[luma.len() - 1]]);
    assert!(last < 800, "the vignette darkened the white corner: {last}");
}

/// A camera shake follows timeline time on every path: the export's frame N is
/// the composer's frame N (the picture preview shows there), not a frame the
/// shake has moved on from or not reached. With a wide shake, frames ten apart
/// sit tens of pixels apart, which no encode error comes near.
#[test]
fn a_shake_is_at_the_same_offset_in_preview_and_export() {
    if !have("ffmpeg") {
        eprintln!("no ffmpeg: skipping");
        return;
    }
    let dir = Scratch::dir("ve_effects_shake");
    let mut s = open(asset("test_av.mp4"));
    let mut shake = Effect::new(EffectKind::Shake);
    shake.set("amount", 100.0).unwrap();
    shake.set("speed", 3.0).unwrap();
    assert!(s.set_effects(Lane::V1, 0, vec![shake]));
    // Start the range past frame 0, so a clip- or range-relative clock would show.
    let out = dir.join("shake.mp4");
    export(&s, &out, &software((10, 40)));
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap()
        .stdout;
    let size = 1280 * 720 * 3;
    let exported = |frame: u32| &raw[(frame - 10) as usize * size..][..size];
    let composed = |frame: u32| s.compose_at(frame, 1280, 720).unwrap().bgra;
    let diff = |rgb: &[u8], bgra: &[u8]| -> f64 {
        let total: u64 = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .zip(bgra.as_chunks::<4>().0)
            .map(|(a, b)| u64::from(a[0].abs_diff(b[2])) + u64::from(a[1].abs_diff(b[1])) + u64::from(a[2].abs_diff(b[0])))
            .sum();
        total as f64 / (1280.0 * 720.0 * 3.0)
    };
    for frame in [15, 30] {
        let same = diff(exported(frame), &composed(frame));
        let other = diff(exported(frame), &composed(frame + 8));
        eprintln!("frame {frame}: same {same:.2}, eight on {other:.2}");
        assert!(same < 4.0 && same * 3.0 < other, "frame {frame}: export {same:.2} from preview, {other:.2} from eight frames on");
    }
}
