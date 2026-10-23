//! Undo and redo under heavy random use. Every successful edit from the public
//! edit API is followed by a canonical serialisation of the project, and an
//! independent model of the undo and redo stacks predicts what every undo, redo
//! and failed edit must leave behind. Each step is checked against that model:
//!
//! * a successful edit pushes one step, and clears the redo stack;
//! * a refused edit changes nothing and pushes nothing;
//! * undo and redo restore exactly the state the model recorded;
//! * the undo history is capped at [`CAP`] steps, the oldest dropped first.
//!
//! The seeds are fixed, so a failure names a seed and a step that reproduce it.
//!
//! ```text
//! cargo test --release -p engine --test undo_stress -- --nocapture
//! ```

use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::time::Instant;

use engine::color::ColorParams;
use engine::eq::EqParams;
use engine::project::{Clip, Edge, GainKey, Lane, LaneKind, Speed, Title};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::text::TextStyle;
use engine::transform::TransformParams;
use engine::PlaybackSession;

/// The undo cap the model assumes; the session's own cap is `HISTORY_CAP` in
/// `project.rs`, and the checks below would fail if the two disagreed.
const CAP: usize = 100;
const OPS: usize = 10_000;
/// Well past `CAP`, so a held history outgrows the cap; more ops add only run time.
const HELD_OPS: usize = 1_500;
const SEEDS: [u64; 4] = [0x9E37_79B9_7F4A_7C15, 0x0BAD_C0DE_1234_5678, 0xDEAD_BEEF_F00D_0001, 42];

/// xorshift64: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }

    fn signed(&mut self, lo: f32, hi: f32) -> f32 {
        lo + ((self.next() >> 40) as f32 / (1u64 << 24) as f32) * (hi - lo)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// The project as a byte string. The playhead is never moved by the stress
/// edits, so it stays at frame 0 and the file is a function of the project alone.
fn state(s: &PlaybackSession, file: &Path) -> Vec<u8> {
    s.save_autosave(file).expect("the project saves");
    fs::read(file).expect("the saved project reads back")
}

/// What the stacks must hold, predicted from the successes and refusals alone.
struct Model {
    undo: VecDeque<Vec<u8>>,
    redo: Vec<Vec<u8>>,
    cur: Vec<u8>,
    pushes: u64,
    cap: usize,
}

impl Model {
    fn new(cur: Vec<u8>, cap: usize) -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new(), cur, pushes: 0, cap }
    }

    /// A successful edit: one step, and the redo branch is gone.
    fn edited(&mut self) {
        if self.undo.len() == self.cap {
            self.undo.pop_front();
        }
        self.undo.push_back(self.cur.clone());
        self.redo.clear();
        self.pushes += 1;
    }
}

fn lanes_with_clips(s: &PlaybackSession) -> Vec<Lane> {
    s.lanes().into_iter().filter(|l| l.kind != LaneKind::Subtitle).collect()
}

fn any_lane(s: &PlaybackSession, rng: &mut Rng) -> Option<Lane> {
    let lanes = lanes_with_clips(s);
    if lanes.is_empty() {
        None
    } else {
        Some(rng.pick(&lanes))
    }
}

fn any_clip(s: &PlaybackSession, rng: &mut Rng) -> Option<(Lane, usize)> {
    let lanes = lanes_with_clips(s);
    let with: Vec<(Lane, usize)> = lanes
        .iter()
        .flat_map(|&l| (0..s.lane_clips(l).len()).map(move |i| (l, i)))
        .collect();
    if with.is_empty() {
        None
    } else {
        Some(rng.pick(&with))
    }
}

fn timeline_frames(s: &PlaybackSession) -> u32 {
    (s.timeline_duration() * s.meta().frame_rate).ceil() as u32
}

/// A fresh clip of the one source the session opens with, `test_av.mp4`
/// (150 frames of picture and sound).
fn random_clip(rng: &mut Rng) -> Clip {
    let in_frame = rng.range(0, 120);
    let out_frame = in_frame + rng.range(1, 30);
    Clip {
        start: 0,
        in_frame,
        out_frame,
        source: 0,
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::Fit,
        speed: Speed::default(),
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
    }
}

const TITLES: [&str; 4] = ["Opening", "Ελληνικά", "Ответ", "Chapter one"];

fn random_title(rng: &mut Rng) -> Title {
    Title {
        start: rng.range(0, 200),
        frames: rng.range(1, 60),
        text: rng.pick(&TITLES).to_string(),
        style: TextStyle::default(),
        fade_in: 0,
        fade_out: 0,
        in_anim: Default::default(),
        out_anim: Default::default(),
        anim_frames: 0,
    }
}

/// One random edit from the public API. `true` when it succeeded, which is the
/// only thing the model needs: every success is one undo step.
fn random_edit(s: &mut PlaybackSession, rng: &mut Rng, last: &mut usize) -> bool {
    let frames = timeline_frames(s);
    *last = rng.below(31);
    match *last {
        0 => s.cut_at(rng.signed(0.0, s.timeline_duration() as f32 + 0.2) as f64),
        1 => match any_clip(s, rng) {
            Some((l, i)) => {
                let edge = if rng.chance(50) { Edge::Start } else { Edge::End };
                s.trim_clip(l, i, edge, rng.range(0, frames + 10))
            }
            None => false,
        },
        2 => match any_clip(s, rng) {
            Some((from, i)) => {
                let to = rng.pick(&lanes_with_clips(s).into_iter().filter(|l| l.kind == from.kind).collect::<Vec<_>>());
                s.move_clip_to(from, i, to, rng.range(0, frames + 10))
            }
            None => false,
        },
        3 => match any_clip(s, rng) {
            Some((l, i)) => s.delete_clip(l, i),
            None => false,
        },
        4 => match any_clip(s, rng) {
            Some((l, i)) => s.lift_clip(l, i),
            None => false,
        },
        5 => match any_lane(s, rng) {
            // An Ok sweep with nothing to close takes no step, so only a closed gap counts.
            Some(l) => s.close_all_gaps_on_lane(l).is_ok_and(|r| r.closed > 0),
            None => false,
        },
        6 => match any_clip(s, rng) {
            Some((l, i)) => s.set_speed(l, i, Speed::from_permille(rng.range(250, 4000) as u16)).is_ok(),
            None => false,
        },
        7 => match any_clip(s, rng) {
            Some((l, i)) => {
                if rng.chance(50) {
                    s.set_fade_in(l, i, rng.range(0, 30))
                } else {
                    s.set_fade_out(l, i, rng.range(0, 30))
                }
            }
            None => false,
        },
        8 => match any_clip(s, rng) {
            Some((l, i)) => s.crossfade(l, i, rng.range(0, 20)),
            None => false,
        },
        9 => match any_clip(s, rng) {
            Some((l, i)) => s.set_transition_out(l, i, rng.range(0, 20)),
            None => false,
        },
        10 => match any_clip(s, rng) {
            Some((l, i)) => {
                let params = rng.chance(80).then(|| ColorParams {
                    brightness: rng.signed(-1.0, 1.0),
                    contrast: rng.signed(0.0, 2.0),
                    saturation: rng.signed(0.0, 2.0),
                    tint: rng.signed(-1.0, 1.0),
                    ..Default::default()
                });
                s.set_color(l, i, params)
            }
            None => false,
        },
        11 => match any_clip(s, rng) {
            Some((l, i)) => {
                let params = rng.chance(80).then(|| TransformParams {
                    pos_x: rng.signed(-0.5, 0.5),
                    scale: rng.signed(0.5, 1.5),
                    rotate: rng.signed(-30.0, 30.0),
                    ..TransformParams::default()
                });
                s.set_transform(l, i, params)
            }
            None => false,
        },
        12 => match any_clip(s, rng) {
            Some((l, i)) => {
                let params = rng.chance(80).then(|| {
                    let mut eq = EqParams::default_layout();
                    for band in &mut eq.bands {
                        band.gain_db = rng.signed(-6.0, 6.0);
                    }
                    eq
                });
                s.set_eq(l, i, params)
            }
            None => false,
        },
        13 => match any_lane(s, rng) {
            Some(l) => s.set_lane_gain_db(l, rng.signed(-20.0, 6.0)),
            None => false,
        },
        14 => {
            let kind = rng.pick(&[LaneKind::Video, LaneKind::Audio]);
            s.add_lane(kind);
            true
        }
        15 => match any_lane(s, rng) {
            Some(l) => s.remove_lane(l).is_ok(),
            None => false,
        },
        16 => match any_lane(s, rng) {
            Some(l) => {
                let to = rng.below(s.lanes().len());
                s.move_lane(l, to).is_some()
            }
            None => false,
        },
        17 => match any_lane(s, rng) {
            Some(l) => match rng.below(3) {
                0 => s.set_lane_hidden(l, rng.chance(50)),
                1 => s.set_lane_muted(l, rng.chance(50)),
                _ => s.set_lane_locked(l, rng.chance(50)),
            },
            None => false,
        },
        18 => match (any_clip(s, rng), any_clip(s, rng)) {
            (Some((a, ai)), Some((b, bi))) => s.group(a, ai, b, bi).is_ok(),
            _ => false,
        },
        19 => match any_clip(s, rng) {
            Some((l, i)) => s.ungroup(l, i),
            None => false,
        },
        20 => s.add_title(random_title(rng)).is_ok(),
        21 => {
            let n = s.titles().len();
            if n == 0 {
                false
            } else {
                s.set_title(rng.below(n), random_title(rng))
            }
        }
        22 => {
            let n = s.titles().len();
            n > 0 && s.remove_title(rng.below(n))
        }
        23 => s.add_chapter(rng.range(0, frames.max(1)), rng.pick(&TITLES)).is_ok(),
        24 => {
            let n = s.chapters().len();
            if n == 0 {
                false
            } else {
                let i = rng.below(n);
                match rng.below(3) {
                    0 => s.remove_chapter(i),
                    1 => s.set_chapter_title(i, rng.pick(&TITLES)),
                    _ => {
                        // Moving to the frame it already holds is an Ok no-op with no step.
                        let to = rng.range(0, frames.max(1));
                        let from = s.chapters()[i].frame;
                        s.move_chapter(i, to).is_ok() && from != to
                    }
                }
            }
        }
        25 => match any_clip(s, rng) {
            Some((l, i)) => {
                let at = rng.range(0, 150);
                if rng.chance(70) {
                    s.add_gain_key(l, i, GainKey::new(at, rng.signed(-12.0, 12.0)))
                } else {
                    s.remove_gain_key(l, i, at)
                }
            }
            None => false,
        },
        26 => match any_clip(s, rng) {
            Some((l, i)) => s.set_clip_gain(l, i, rng.signed(-12.0, 12.0)),
            None => false,
        },
        27 => match any_clip(s, rng) {
            Some((l, i)) => s.set_fit(l, i, rng.pick(&[FitPolicy::Fit, FitPolicy::Fill])),
            None => false,
        },
        28 => match (any_lane(s, rng), any_clip(s, rng)) {
            (Some(l), Some((_, _))) => {
                let a = rng.range(0, 40);
                let b = rng.range(0, 40);
                s.cut_regions(&[(a, a + rng.range(1, 10)), (b + 60, b + 70)], &[l]).is_ok()
            }
            _ => false,
        },
        29 => match any_lane(s, rng) {
            Some(l) => {
                let n = s.lane_clips(l).len();
                n >= 2 && s.swap_clips(l, rng.below(n), rng.below(n))
            }
            None => false,
        },
        _ => {
            let lanes = lanes_with_clips(s);
            let lane = rng.pick(&lanes);
            let secs = rng.signed(0.0, s.timeline_duration() as f32 + 1.0) as f64;
            s.place_at(lane, secs, random_clip(rng))
        }
    }
}

/// Resident set in KiB, from `/proc`; 0 where that is not there.
fn rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
}

fn open_session() -> PlaybackSession {
    let asset = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
    PlaybackSession::open(asset).expect("open the fixture")
}

/// Walks the undo stack back to the bottom, then the redo stack forward to the
/// top, checking every state against the model on the way.
fn drain_and_refill(s: &mut PlaybackSession, model: &mut Model, file: &Path, tag: &str) {
    let mut step = 0usize;
    while let Some(prev) = model.undo.pop_back() {
        assert!(s.undo(), "{tag}: undo {step} refused with {} steps left", model.undo.len() + 1);
        let back = state(s, file);
        assert!(back == prev, "{tag}: undo {step} does not restore the recorded state");
        model.redo.push(std::mem::replace(&mut model.cur, prev));
        step += 1;
    }
    assert!(!s.undo(), "{tag}: undo past the bottom must be refused");
    assert!(state(s, file) == model.cur, "{tag}: a refused undo changed the project");
    assert_eq!(s.undo_depth(), 0, "{tag}: the bottom of the history is empty");

    let mut step = 0usize;
    while let Some(next) = model.redo.pop() {
        assert!(s.redo(), "{tag}: redo {step} refused");
        let fwd = state(s, file);
        assert!(fwd == next, "{tag}: redo {step} does not restore the recorded state");
        model.undo.push_back(std::mem::replace(&mut model.cur, next));
        step += 1;
    }
    assert!(!s.redo(), "{tag}: redo past the top must be refused");
    assert!(state(s, file) == model.cur, "{tag}: a refused redo changed the project");
    assert_eq!(s.redo_depth(), 0, "{tag}: the top of the redo stack is empty");
}

/// The random run: edits with undo and redo interleaved, checked after every
/// step against the model. `cap` is `Some(CAP)` for the session's own cap, or
/// `None` when the history is held and nothing is evicted.
fn random_run(seed: u64, ops: usize, cap: Option<usize>) -> (Model, PlaybackSession) {
    let file = Scratch::file(&format!("undo_stress_{seed:x}"), "edith");
    let mut s = open_session();
    if cap.is_none() {
        s.hold_history(true);
    }
    let mut model = Model::new(state(&s, &file), cap.unwrap_or(usize::MAX));
    let mut rng = Rng::new(seed);
    let mut undos = 0usize;
    let mut redos = 0usize;
    let mut refused = 0usize;
    let mut noops = 0usize;

    for step in 0..ops {
        let tag = format!("seed {seed:#x} step {step}");
        let mut edited = false;
        let mut last_op = usize::MAX;
        match rng.below(100) {
            0..=7 => {
                let expect = model.undo.pop_back();
                let ok = s.undo();
                match expect {
                    None => assert!(!ok, "{tag}: undo with an empty model stack succeeded"),
                    Some(prev) => {
                        assert!(ok, "{tag}: undo refused with {} steps modelled", model.undo.len() + 1);
                        model.redo.push(std::mem::replace(&mut model.cur, prev));
                        undos += 1;
                    }
                }
            }
            8..=15 => {
                let expect = model.redo.pop();
                let ok = s.redo();
                match expect {
                    None => assert!(!ok, "{tag}: redo with an empty model stack succeeded"),
                    Some(next) => {
                        assert!(ok, "{tag}: redo refused");
                        model.undo.push_back(std::mem::replace(&mut model.cur, next));
                        redos += 1;
                    }
                }
            }
            _ => {
                let had_redo = model.redo.len();
                let mut op = 0usize;
                let ok = random_edit(&mut s, &mut rng, &mut op);
                last_op = op;
                if ok && state(&s, &file) == model.cur {
                    // An edit that changes nothing costs no undo step and keeps
                    // the redo branch: it is settled before anyone sees it.
                    assert_eq!(s.redo_depth(), had_redo, "{tag}: a no-op edit (op {op}) touched the redo stack");
                    noops += 1;
                } else if ok {
                    model.edited();
                    edited = true;
                    assert_eq!(s.redo_depth(), 0, "{tag}: a new edit left redo steps behind");
                } else {
                    // A refusal changes nothing: same state, same stacks.
                    assert_eq!(s.redo_depth(), had_redo, "{tag}: a refused edit (op {op}) touched the redo stack");
                    refused += 1;
                }
            }
        }
        // A success's new state is not known in advance, so it is read here and
        // becomes the current state; every undo and redo must land on a recorded one.
        let now = state(&s, &file);
        if edited {
            model.cur = now;
        } else {
            assert!(now == model.cur, "{tag}: state drifted from the model");
        }
        assert_eq!(s.undo_depth(), model.undo.len(), "{tag}: undo depth (last edit kind {last_op})");
        assert_eq!(s.redo_depth(), model.redo.len(), "{tag}: redo depth");
        assert_eq!(s.history_pushes(), model.pushes, "{tag}: pushes (last edit kind {last_op})");
    }
    eprintln!(
        "seed {seed:#x}: {ops} steps, {} edits, {noops} no-ops, {refused} refused, {undos} undos, {redos} redos",
        model.pushes
    );
    (model, s)
}

#[test]
fn random_edits_undo_and_redo_exactly_for_every_seed() {
    for seed in SEEDS {
        let started = Instant::now();
        let (mut model, mut s) = random_run(seed, OPS, Some(CAP));
        assert!(model.undo.len() <= CAP, "the modelled history never exceeds the cap");
        let file = Scratch::file(&format!("undo_stress_drain_{seed:x}"), "edith");
        drain_and_refill(&mut s, &mut model, &file, &format!("seed {seed:#x} drain"));
        eprintln!(
            "seed {seed:#x}: {:.2?}, rss {} MiB",
            started.elapsed(),
            rss_kib() / 1024
        );
    }
}

#[test]
fn undo_past_the_cap_is_a_noop_and_the_oldest_steps_are_dropped() {
    let file = Scratch::file("undo_stress_cap", "edith");
    let mut s = open_session();
    let bottom_at = |n: usize| -> Vec<u8> {
        // The state after `n` gain edits, from a fresh session that makes just those.
        let mut t = open_session();
        for i in 1..=n {
            assert!(t.set_lane_gain_db(Lane::new(LaneKind::Video, 0), -0.1 * i as f32));
        }
        state(&t, &file)
    };
    let lane = Lane::new(LaneKind::Video, 0);
    for i in 1..=CAP + 50 {
        assert!(s.set_lane_gain_db(lane, -0.1 * i as f32), "gain edit {i} is a success");
    }
    assert_eq!(s.undo_depth(), CAP, "the history is capped");
    assert_eq!(s.history_pushes(), (CAP + 50) as u64, "every edit still counted");

    for _ in 0..CAP {
        assert!(s.undo());
    }
    assert_eq!(s.undo_depth(), 0);
    assert!(!s.undo(), "undo past the cap is a refused no-op");
    assert_eq!(s.undo_depth(), 0);
    // Oldest dropped: the 50 earliest steps are gone, so the bottom is the state after edit 50.
    assert!(state(&s, &file) == bottom_at(50), "undo stops at the oldest kept step");
    assert_eq!(s.history_pushes(), (CAP + 50) as u64, "an undo does not roll the count back");
    assert!(s.redo(), "the oldest kept step is still a redo");
}

#[test]
fn held_history_keeps_every_step_and_trims_back_to_the_cap_on_release() {
    let seed = 7;
    let (mut model, mut s) = random_run(seed, HELD_OPS, None);
    let file = Scratch::file("undo_stress_held", "edith");
    // Every step survived the run, so the whole chain is walkable back.
    assert!(model.undo.len() > CAP, "held history runs past the cap ({})", model.undo.len());
    assert_eq!(s.undo_depth(), model.undo.len());
    s.hold_history(false);
    model.undo.drain(..model.undo.len().saturating_sub(CAP));
    assert_eq!(s.undo_depth(), CAP, "releasing trims back to the cap");
    drain_and_refill(&mut s, &mut model, &file, "held");
}
