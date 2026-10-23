//! Video stabilisation against a shake whose every frame is known: a static noise
//! texture turned and shifted per frame by ffmpeg with a seeded jitter, then the
//! motion the analysis measured, the picture the session composes and the trims
//! and speeds it must survive. Run with `cargo test --release -p engine --test stabilise`.

use std::path::Path;
use std::process::Command;

use engine::PlaybackSession;
use engine::decode::DecodeSession;
use engine::project::{Edge, Lane, Rate, Speed};
use engine::scratch::Scratch;
use engine::stabilise::{self, Crop, Sim, Stabilise};

const W: u32 = 640;
const H: u32 = 360;
const FRAMES: u32 = 90;
/// The jitter: up to +-6 px each way and +-0.5 degrees.
const SHIFT: f64 = 6.0;
const TURN_DEG: f64 = 0.5;

/// The cache dir this process points `XDG_CACHE_HOME` at.
fn cache_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("edith-stabilise-test-cache-{}", std::process::id()))
}

/// Tests running at once share the cache dir; the last one to finish removes it.
static LIVE: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

struct CacheGuard;

impl Drop for CacheGuard {
    fn drop(&mut self) {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        *live -= 1;
        if *live == 0 {
            let _ = std::fs::remove_dir_all(cache_dir());
        }
    }
}

fn setup() -> CacheGuard {
    *LIVE.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    engine::ao::set_silent_output();
    engine::envcfg::set("XDG_CACHE_HOME", cache_dir());
    CacheGuard
}

/// The seeded noise both ffmpeg and this test evaluate: `fract(sin(n * 12.9898 + k) * 43758.5453)`.
fn hash(n: u32, k: f64) -> f64 {
    let v = (f64::from(n) * 12.9898 + k).sin() * 43758.5453;
    v - v.floor()
}
fn hash_expr(k: f64) -> String {
    format!("(sin(n*12.9898+{k})*43758.5453-floor(sin(n*12.9898+{k})*43758.5453))")
}

/// Frame `n`'s known shake: the crop offset (whole pixels, the pan included) and
/// the turn in radians.
fn shake(n: u32, pan: f64) -> (f64, f64, f64) {
    let dx = (hash(n, 1.0) * (2.0 * SHIFT + 1.0)).floor() - SHIFT + (pan * f64::from(n)).floor();
    let dy = (hash(n, 2.0) * (2.0 * SHIFT + 1.0)).floor() - SHIFT;
    let turn = (hash(n, 3.0) * 2.0 - 1.0) * TURN_DEG.to_radians();
    (dx, dy, turn)
}

/// A 640x360 clip of a static texture shaken by [`shake`], panning `pan` px a
/// frame to the right.
fn shaky(name: &str, pan: f64) -> Scratch {
    let out = Scratch::file(name, "mp4");
    let span = (pan * f64::from(FRAMES)).ceil() as u32;
    let (tw, th) = (W + 320 + span, H + 180);
    let x = format!("{}+floor({}*{})-{SHIFT}+floor({pan}*n)", 160, hash_expr(1.0), 2.0 * SHIFT + 1.0);
    let y = format!("{}+floor({}*{})-{SHIFT}", 90, hash_expr(2.0), 2.0 * SHIFT + 1.0);
    let a = format!("({}*2-1)*{}", hash_expr(3.0), TURN_DEG.to_radians());
    // The texture: blurred noise, the same every frame. Turned about the texture's
    // centre, then the window is cut at the frame's offset from it.
    let graph = format!(
        "nullsrc=s={tw}x{th}:r=30,format=gray,geq=lum='random(1)*255',gblur=sigma=1.5,trim=end_frame=1,loop=loop=-1:size=1,setpts=N/30/TB,\
         rotate=a='{a}':c=black,crop={W}:{H}:'{x}':'{y}':exact=1,format=yuv420p"
    );
    let _ = (tw, th);
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &graph, "-frames:v", &FRAMES.to_string()])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "8", "-pix_fmt", "yuv420p"])
        .arg(&*out)
        .status()
        .expect("ffmpeg runs")
        .success();
    assert!(ok, "ffmpeg made the shaky clip");
    out
}

/// The one still shaky clip the read-only tests share, analysed over its whole
/// length before anyone is handed it, so none of them starts a measuring of its
/// own. A static is never dropped, so this is held weakly: the clip lives while a
/// test holds it, and its file goes when the last one lets go. The lock is kept
/// while it is made and measured, so two tests asking at once wait for the one
/// analysis rather than racing to start it.
fn shared_shake() -> std::sync::Arc<Scratch> {
    static SHARED: std::sync::Mutex<std::sync::Weak<Scratch>> = std::sync::Mutex::new(std::sync::Weak::new());
    let mut slot = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(clip) = slot.upgrade() {
        return clip;
    }
    let clip = std::sync::Arc::new(shaky("stab_shared", 0.0));
    assert_eq!(stabilise::wait(&clip, 0, FRAMES), stabilise::Status::Ready);
    *slot = std::sync::Arc::downgrade(&clip);
    clip
}

/// The known world-to-frame map of frame `n`: the texture turned by `turn` about
/// its centre, then the window `(dx, dy)` off it. `sign` is the turn's direction.
fn known(n: u32, pan: f64, sign: f64) -> Sim {
    let (dx, dy, turn) = shake(n, pan);
    let t = sign * turn;
    Sim { a: t.cos(), b: t.sin(), tx: -dx, ty: -dy }
}

/// RMS over a 3x3 grid spanning the frame of where `a` and `b` put its points.
fn gap(a: &Sim, b: &Sim) -> f64 {
    let (hw, hh) = (f64::from(W - 1) / 2.0, f64::from(H - 1) / 2.0);
    let mut s = 0.0;
    for gy in [-hh, 0.0, hh] {
        for gx in [-hw, 0.0, hw] {
            let (p, q) = (a.apply(gx, gy), b.apply(gx, gy));
            s += (p.0 - q.0).powi(2) + (p.1 - q.1).powi(2);
        }
    }
    (s / 9.0).sqrt()
}

fn rms(xs: &[f64]) -> f64 {
    (xs.iter().map(|x| x * x).sum::<f64>() / xs.len() as f64).sqrt()
}

/// The luma of every picture `s` composes, frames `0..n`.
fn composed(s: &PlaybackSession, n: u32) -> Vec<Vec<u8>> {
    (0..n)
        .map(|f| {
            let fr = s.compose_at(f, W, H).expect("compose");
            stabilise::luma_from_bgra(&fr.bgra, W as usize, H as usize)
        })
        .collect()
}

fn steps_of(frames: &[Vec<u8>]) -> Vec<Sim> {
    frames
        .windows(2)
        .map(|p| stabilise::estimate(&p[0], &p[1], W as usize, H as usize).expect("tracked"))
        .collect()
}

fn analysed(path: &Path) -> stabilise::Track {
    let (p, c) = (Default::default(), Default::default());
    stabilise::analyse(path, 0, FRAMES, &p, &c).expect("analysed")
}

#[test]
fn the_measured_motion_is_the_known_shake() {
    let _cache = setup();
    let clip = shared_shake();
    let track = analysed(&clip);
    assert_eq!(track.steps.len(), FRAMES as usize);
    // Which way ffmpeg's turn goes is its convention; the measurement must match
    // one of the two to the bar, and the bar is the test.
    let errs = |sign: f64| -> (Vec<f64>, Vec<f64>) {
        (1..FRAMES)
            .map(|n| {
                let truth = known(n - 1, 0.0, sign).inverse().then(known(n, 0.0, sign));
                let est = track.steps[n as usize];
                (gap(&est, &truth), (est.angle() - truth.angle()).to_degrees())
            })
            .unzip()
    };
    let (px, deg) = [errs(1.0), errs(-1.0)].into_iter().min_by(|a, b| rms(&a.0).total_cmp(&rms(&b.0))).expect("two");
    eprintln!("estimated vs known: {:.3} px RMS, {:.4} deg RMS", rms(&px), rms(&deg));
    assert!(rms(&px) < 0.3, "translation error {:.3} px RMS", rms(&px));
    assert!(rms(&deg) < 0.05, "rotation error {:.4} deg RMS", rms(&deg));
}

#[test]
fn a_stabilised_clip_holds_still_and_shows_no_edge() {
    let _cache = setup();
    let clip = shared_shake();
    let mut s = PlaybackSession::open(&*clip).expect("open");
    let before = steps_of(&composed(&s, FRAMES));
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::new(0.5, Crop::Zoom))));
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    let frames = composed(&s, FRAMES);
    let after = steps_of(&frames);
    let still = |steps: &[Sim]| rms(&steps.iter().map(|m| gap(m, &Sim::IDENTITY)).collect::<Vec<_>>());
    eprintln!("frame-to-frame motion: {:.3} px RMS before, {:.3} px RMS after", still(&before), still(&after));
    assert!(still(&before) > 3.0, "the clip is shaky to begin with: {:.3}", still(&before));
    assert!(still(&after) < 0.5, "residual jitter {:.3} px RMS", still(&after));
    // Zoomed to hide the edges: no frame has a black border pixel.
    for (n, f) in frames.iter().enumerate() {
        let (w, h) = (W as usize, H as usize);
        let border = (0..w).flat_map(|x| [f[x], f[(h - 1) * w + x]]).chain((0..h).flat_map(|y| [f[y * w], f[y * w + w - 1]]));
        let darkest = border.min().expect("pixels");
        assert!(darkest > 20, "frame {n} shows an edge: luma {darkest}");
    }

    // Off is the picture it was, byte for byte.
    let plain = PlaybackSession::open(&*clip).expect("open");
    assert!(s.set_stabilise(Lane::V1, 0, None));
    for f in [0, 31, 89] {
        assert_eq!(s.compose_at(f, W, H).unwrap().bgra, plain.compose_at(f, W, H).unwrap().bgra, "frame {f}");
    }
}

#[test]
fn a_slow_pan_is_kept_while_the_shake_goes() {
    let _cache = setup();
    let pan = 1.5;
    let clip = shaky("stab_pan", pan);
    let mut s = PlaybackSession::open(&clip).expect("open");
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    let steps = steps_of(&composed(&s, FRAMES));
    let mean = steps.iter().map(|m| m.tx).sum::<f64>() / steps.len() as f64;
    let zoom = stabilise::correction(&path, lo, hi, Stabilise::default(), 30.0).expect("ready").zoom;
    // The window moves right, so the picture moves left, by the pan at the zoom.
    let pan_px = -pan * zoom;
    let off: Vec<f64> = steps.iter().map(|m| gap(m, &Sim { tx: pan_px, ..Sim::IDENTITY })).collect();
    eprintln!("pan kept at {mean:.3} px/frame (want {pan_px:.3}), residual {:.3} px RMS", rms(&off));
    assert!((mean - pan_px).abs() < 0.15, "the pan is {mean:.3} px a frame, not {pan_px:.3}");
    assert!(rms(&off) < 0.5, "residual around the pan {:.3} px RMS", rms(&off));
}

#[test]
fn the_correction_follows_the_source_frame_through_trims_and_speed() {
    let _cache = setup();
    let clip = shared_shake();
    let mut s = PlaybackSession::open(&*clip).expect("open");
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    let whole = composed(&s, FRAMES);
    let at = |s: &PlaybackSession, f: u32| stabilise::luma_from_bgra(&s.compose_at(f, W, H).unwrap().bgra, W as usize, H as usize);

    // A head trim of 10: timeline frame k now shows source frame k + 10, from the
    // analysis it was already given (the wider range covers the trimmed one).
    let mut trimmed = PlaybackSession::open(&*clip).expect("open");
    assert!(trimmed.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    assert!(trimmed.trim_clip(Lane::V1, 0, Edge::Start, 10));
    let start = trimmed.lane_clips(Lane::V1)[0].start;
    assert_eq!(trimmed.stabilise_status(Lane::V1, 0), Some(stabilise::Status::Ready), "no new analysis");
    for k in [0, 20, 50] {
        assert_eq!(at(&trimmed, start + k), whole[(k + 10) as usize], "trimmed frame {k}");
    }

    // Double speed: timeline frame k is source frame 2k.
    let mut fast = PlaybackSession::open(&*clip).expect("open");
    assert!(fast.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    fast.set_speed(Lane::V1, 0, Speed::from_permille(2000)).expect("speed");
    for k in [0, 10, 40] {
        assert_eq!(at(&fast, k), whole[(2 * k) as usize], "2x frame {k}");
    }
}

#[test]
fn the_analysis_reports_progress_and_stops_on_cancel() {
    let _cache = setup();
    let clip = shared_shake();
    let (_, rx, stop) = DecodeSession::open_range(&*clip, 0, 1).expect("decodes");
    drop(rx);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let progress = std::sync::atomic::AtomicU32::new(0);
    let cancel = std::sync::atomic::AtomicBool::new(true);
    assert!(stabilise::analyse(&clip, 0, FRAMES, &progress, &cancel).is_err(), "a set cancel stops it");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    assert!(stabilise::analyse(&clip, 0, FRAMES, &progress, &cancel).is_ok());
    assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 1000);
}

/// RMS of the frame-to-frame motion over `frames`: 0 for a picture held still.
fn jitter(frames: &[Vec<u8>]) -> f64 {
    rms(&steps_of(frames).iter().map(|m| gap(m, &Sim::IDENTITY)).collect::<Vec<_>>())
}

/// The luma of every picture a software mp4 export of `s` holds.
fn exported(s: &PlaybackSession, name: &str) -> Vec<Vec<u8>> {
    exported_watched(s, name).0
}

/// [`exported`], and what its status said while it ran: the stage with its
/// fraction, and the encode's progress, polled every few milliseconds.
type Poll = (Option<(&'static str, f32)>, f32);
fn exported_watched(s: &PlaybackSession, name: &str) -> (Vec<Vec<u8>>, Vec<Poll>) {
    let mut polls = Vec::new();
    let out = Scratch::file(name, "mp4");
    let settings = engine::export::ExportSettings {
        format: engine::export::Format::Mp4,
        seat: engine::export::EncoderSeat::Software,
        ..Default::default()
    };
    let handle = s.export_to_with(&out, &settings);
    let started = std::time::Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < std::time::Duration::from_secs(300), "the export hangs");
        polls.push((handle.stage(), handle.progress()));
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    handle.result().expect("an outcome").expect("the export");
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&*out)
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .expect("ffmpeg runs")
        .stdout;
    let size = (W * H) as usize;
    ((0..raw.len() / size).map(|i| raw[i * size..(i + 1) * size].to_vec()).collect(), polls)
}

#[test]
fn an_export_measures_a_clip_whose_analysis_was_cleared() {
    let _cache = setup();
    let clip = shaky("stab_cold", 0.0);
    let mut s = PlaybackSession::open(&clip).expect("open");
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    // The cache is cleared under the editor: nothing covers the clip any more.
    std::fs::remove_file(stabilise::cache_path(&path, lo, hi).expect("a cache")).expect("removed");
    assert_eq!(s.stabilise_status(Lane::V1, 0), Some(stabilise::Status::Missing));
    let (frames, polls) = exported_watched(&s, "stab_cold_out");
    assert_eq!(frames.len(), FRAMES as usize);
    // The measuring is a stage of its own, said before the encode moves: its
    // fraction climbs, and the bar returns to the encode once it is done.
    let staged: Vec<f32> = polls.iter().filter_map(|(st, _)| st.map(|(_, f)| f)).collect();
    assert!(polls.iter().filter_map(|(st, _)| *st).all(|(n, _)| n == engine::export::MEASURING_SHAKE));
    assert!(staged.len() >= 2, "the measuring stage was reported: {polls:?}");
    assert!(staged.windows(2).all(|w| w[0] <= w[1]), "the stage never goes back: {staged:?}");
    assert!(staged.last() > staged.first(), "the stage climbs: {staged:?}");
    let last_staged = polls.iter().rposition(|(st, _)| st.is_some()).expect("staged");
    // The picture's share of the bar waits for it: the bar holds (the sound's band
    // may land meanwhile) and moves on only once the encode starts.
    let mut held: Vec<f32> = polls[..=last_staged].iter().map(|p| p.1).collect();
    held.dedup();
    assert!(held.len() <= 2, "the encode's bar moved while measuring: {held:?}");
    let after = polls[last_staged + 1..].iter().map(|p| p.1).fold(0.0, f32::max);
    assert!(after > held[held.len() - 1], "the encode moved the bar after measuring: {held:?} then {after}");
    assert!(polls[last_staged + 1..].iter().all(|(st, _)| st.is_none()), "one measuring stage, then the encode");
    eprintln!("measuring stage: {} polls, {:.2} -> {:.2}", staged.len(), staged[0], staged[staged.len() - 1]);
    let left = jitter(&frames);
    eprintln!("cold export: {left:.3} px RMS");
    assert!(left < 0.5, "the export wrote the clip unstabilised: {left:.3} px RMS");
    assert_eq!(s.stabilise_status(Lane::V1, 0), Some(stabilise::Status::Ready), "measured and cached");
}

#[test]
fn a_trim_widened_past_the_analysis_is_stabilised_end_to_end() {
    let _cache = setup();
    let clip = shaky("stab_widen", 0.0);
    let mut s = PlaybackSession::open(&clip).expect("open");
    assert!(s.trim_clip(Lane::V1, 0, Edge::Start, 10));
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    assert_eq!((lo, hi), (10, FRAMES));
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    // Back out to the whole file: frames 0..10 were never measured.
    let start = s.lane_clips(Lane::V1)[0].start;
    assert!(s.trim_clip(Lane::V1, 0, Edge::Start, start - 10));
    assert_eq!(s.stabilise_range(Lane::V1, 0).map(|r| (r.1, r.2)), Some((0, FRAMES)));
    assert_eq!(s.stabilise_status(Lane::V1, 0), Some(stabilise::Status::Missing));
    let frames = exported(&s, "stab_widen_out");
    assert_eq!(frames.len(), FRAMES as usize);
    let head = jitter(&frames[..12]);
    let whole = jitter(&frames);
    eprintln!("widened export: {head:.3} px RMS over the new head, {whole:.3} over the clip");
    assert!(head < 0.5 && whole < 0.5, "part of the clip is unstabilised: head {head:.3}, whole {whole:.3}");
}

#[test]
fn a_timeline_at_another_rate_measures_and_smooths_as_the_viewer_does() {
    let _cache = setup();
    let clip = shared_shake();
    let mut s = PlaybackSession::open(&*clip).expect("open");
    assert!(s.set_frame_rate(25.0));
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    // The viewer's range is the one an export's layer reads: the file's own frames
    // through the 30-to-25 rate.
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    let rate = Rate::from_fps(30.0, 25.0).expect("a rate");
    assert_eq!((lo, hi), stabilise::file_range(&s.lane_clips(Lane::V1)[0], rate));
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    // One smoothing, at the timeline's rate, for the card, the viewer and the export.
    let fps = s.meta().frame_rate;
    assert_eq!(fps, 25.0);
    let viewer = stabilise::correction(&path, lo, hi, Stabilise::default(), fps).expect("ready");
    assert!(std::sync::Arc::ptr_eq(&viewer, &stabilise::correction(&path, lo, hi, Stabilise::default(), fps).expect("ready")));
    let n = s.lane_clips(Lane::V1)[0].end().min(70);
    let frames = composed(&s, n);
    let left = jitter(&frames);
    eprintln!("25 fps timeline over a 30 fps file: {left:.3} px RMS");
    assert!(left < 0.5, "residual {left:.3} px RMS");
}

#[test]
fn a_still_of_an_unmeasured_clip_comes_at_once_and_starts_the_measuring() {
    let _cache = setup();
    let clip = shaky("stab_still_now", 0.0);
    let plain = PlaybackSession::open(&clip).expect("open");
    let mut s = PlaybackSession::open(&clip).expect("open");
    assert!(s.set_stabilise(Lane::V1, 0, Some(Stabilise::default())));
    let (path, lo, hi) = s.stabilise_range(Lane::V1, 0).expect("footage");
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    std::fs::remove_file(stabilise::cache_path(&path, lo, hi).expect("a cache")).expect("removed");
    assert_eq!(s.stabilise_status(Lane::V1, 0), Some(stabilise::Status::Missing));
    // The still does not wait for the analysis: it is the clip as it is, and the
    // analysis it needs is under way when it returns.
    let t = std::time::Instant::now();
    let still = s.compose_at(31, W, H).expect("compose").bgra;
    let took = t.elapsed();
    assert_eq!(still, plain.compose_at(31, W, H).unwrap().bgra, "drawn unstabilised, not waited for");
    assert!(took < std::time::Duration::from_secs(2), "the still took {took:?}");
    assert_ne!(s.stabilise_status(Lane::V1, 0), Some(stabilise::Status::Missing), "the measuring started");
    assert_eq!(stabilise::wait(&path, lo, hi), stabilise::Status::Ready);
    assert_ne!(s.compose_at(31, W, H).unwrap().bgra, still, "stabilised once measured");
}
