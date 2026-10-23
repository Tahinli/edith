//! The effects card's side of the window: opening it on a picture clip and the writes
//! behind its rows. Every discrete act (add, switch, move, remove, a choice) is one
//! [`engine::PlaybackSession::set_effects`] call and so one undo step; a slider drag is
//! held as a draft the card shows and written once when the hand lets go.

use crate::*;
use crate::player::cards::{bar_fraction, value_at};
use engine::effects::{BlendMode, Effect, EffectKind, MAX_EFFECTS, MAX_PARAMS, MaskKey, ParamSpec};
use engine::transform::KeyEase;

/// How a parameter is drawn: a bar, an on/off switch (a step of 1 over 0..1), or the
/// blend mode's choice of words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamShape {
    Bar,
    Switch,
    Mode,
}

pub(crate) fn param_shape(kind: EffectKind, spec: &ParamSpec) -> ParamShape {
    if kind == EffectKind::Blend && spec.name == "mode" {
        ParamShape::Mode
    } else if spec.min == 0. && spec.max == 1. && spec.step == 1. {
        ParamShape::Switch
    } else {
        ParamShape::Bar
    }
}

/// A value on its slider's grid: the step the spec names, clamped to its range.
pub(crate) fn snap_param(spec: &ParamSpec, value: f32) -> f32 {
    let steps = ((value - spec.min) / spec.step).round();
    (spec.min + steps * spec.step).clamp(spec.min, spec.max)
}


/// The unit a bar's number reads in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamUnit {
    /// Whole pixels: blur radius, camera shake distance.
    Px,
    /// Whole percent of the stored fraction: amounts, softness, mask edges, glow.
    Percent,
    /// A multiplier, `1.00×` at the default: sharpen amount.
    Times,
    /// A rate: camera shake speed, a number of times a second.
    PerSecond,
    /// A bare whole number.
    Plain,
}

impl ParamUnit {
    /// The factor from the stored value to the number the card reads and types (a percent reads
    /// 100 for 1).
    pub(crate) fn scale(self) -> f32 {
        match self {
            Self::Percent => 100.,
            _ => 1.,
        }
    }
}

/// The unit a parameter's number is shown in.
pub(crate) fn param_unit(kind: EffectKind, spec: &ParamSpec) -> ParamUnit {
    match (kind, spec.name) {
        (EffectKind::Blur, "radius") | (EffectKind::Shake, "amount") | (EffectKind::Pixelate, "size") => ParamUnit::Px,
        (EffectKind::Shake, "speed") => ParamUnit::PerSecond,
        (EffectKind::Sharpen, "amount") => ParamUnit::Times,
        _ if spec.max <= 3. => ParamUnit::Percent,
        _ => ParamUnit::Plain,
    }
}

/// A value as the card shows it, with its unit (see [`ParamUnit`]).
pub(crate) fn param_display(kind: EffectKind, spec: &ParamSpec, value: f32) -> String {
    match param_unit(kind, spec) {
        ParamUnit::Px => format!("{} px", value.round()),
        ParamUnit::Percent => format!("{}%", (value * 100.).round()),
        ParamUnit::Times => format!("{value:.2}×"),
        ParamUnit::PerSecond => format!("{value:.1} per second"),
        ParamUnit::Plain => format!("{}", value.round()),
    }
}

/// The plain word for a blend mode.
/// The mask key a mask effect holds at source frame `at`: the shape it shows there, as a key of its own.
fn mask_key_at(e: &Effect, at: u32) -> MaskKey {
    let base = e.posed(f64::from(at)).into_owned();
    let v = base.values;
    MaskKey { at, x: v[0], y: v[1], width: v[2], height: v[3], feather: v[4], ease: KeyEase::Linear }
}

/// Writes parameter `p` of `e`. A keyed mask takes the change on its key at source frame
/// `at`, made there from what it shows when none is, so an edit at the playhead animates; any
/// other effect, or an unkeyed mask, takes it as its static value.
fn write_param(e: &mut Effect, p: usize, v: f32, at: Option<u32>) {
    let mask = matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask) && p < 5;
    match at {
        Some(at) if mask && !e.keys.is_empty() => {
            let mut key = match e.keys.iter().position(|k| k.at == at) {
                Some(i) => e.keys[i],
                None => mask_key_at(e, at),
            };
            match p {
                0 => key.x = v,
                1 => key.y = v,
                2 => key.width = v,
                3 => key.height = v,
                _ => key.feather = v,
            }
            match e.keys.binary_search_by_key(&at, |k| k.at) {
                Ok(i) => e.keys[i] = key,
                Err(i) => e.keys.insert(i, key),
            }
        }
        _ => e.values[p] = v,
    }
}

pub(crate) fn mode_text(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Add => "Add",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
    }
}

impl Player {
    /// Opens the effects card on the clip whose stack is to change: the selected
    /// picture clip, else the one under the playhead -- the transform card's rule.
    pub(crate) fn open_effects(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("Nothing to add effects to — no file open".into());
            cx.notify();
            return;
        };
        let target = match self.selected.anchor() {
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                match caption_media_half(session, (lane, idx), LaneKind::Video) {
                    Some(half) => Some(half),
                    None => {
                        self.notify_user("Nothing to add effects to — a caption has no picture".into());
                        cx.notify();
                        return;
                    }
                }
            }
            Some((lane, _)) if lane.kind == LaneKind::Audio => session.video_clip_at(session.now()),
            other => other.or_else(|| session.video_clip_at(session.now())),
        };
        match target {
            Some(clip) => {
                // One card at a time -- `close_card`'s own rule.
                self.close_other_cards();
                self.effects_open = Some(clip);
                self.effects_mode_open = None;
                self.context_menu = None;
            }
            None => self.notify_user("Nothing to add effects to — no clip under the playhead".into()),
        }
        cx.notify();
    }

    /// The stack the card shows: the draft while a bar is held, else the clip's own.
    pub(crate) fn effects_shown(&self) -> Vec<Effect> {
        if let Some(draft) = &self.effects_draft {
            return draft.clone();
        }
        self.effects_open
            .zip(self.session.as_ref())
            .map_or_else(Vec::new, |((lane, idx), s)| s.effects_of(lane, idx).to_vec())
    }

    /// The source frame the playhead shows on the effects card's clip, where a mask key goes;
    /// `None` when the playhead is not on the clip.
    pub(crate) fn effects_src(&self) -> Option<u32> {
        let (lane, idx) = self.effects_open?;
        let session = self.session.as_ref()?;
        session.key_source_frame(lane, idx, self.key_frame()).or_else(|| {
            // The playhead can stand on the clip's end frame, which is not one of its frames:
            // the picture holds the last frame there, so the card shows the values of that one,
            // as the renderer draws it.
            let end = session.lane_clips(lane).get(idx)?.end();
            session.key_source_frame(lane, idx, end.checked_sub(1)?)
        })
    }

    /// The keyframe diamond on a mask row: a key for the mask's shape at the playhead, or that
    /// key taken off. Taking off the last key leaves the shape it had there as the static one.
    pub(crate) fn toggle_mask_key(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(at) = self.effects_src() else {
            self.notify_user("The playhead is not on this clip".into());
            cx.notify();
            return;
        };
        let mut stack = self.effects_shown();
        if let Some(e) = stack.get_mut(i)
            && matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask)
        {
            match e.keys.binary_search_by_key(&at, |k| k.at) {
                Ok(pos) => {
                    let key = e.keys.remove(pos);
                    if e.keys.is_empty() {
                        e.values[..5].copy_from_slice(&[key.x, key.y, key.width, key.height, key.feather]);
                    }
                }
                Err(pos) => {
                    let key = mask_key_at(e, at);
                    e.keys.insert(pos, key);
                }
            }
            self.write_effects(stack, cx);
        }
    }

    /// Follows the mask at stack position `slot` of the open clip from the playhead to the
    /// clip's end, or back to its start ([`engine::PlaybackSession::track_mask_start`]). The
    /// keys land when the track ends ([`Self::poll_tracks`]).
    pub(crate) fn start_track(&mut self, slot: usize, backward: bool, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.effects_open else {
            return;
        };
        match self.effects_src() {
            Some(from) => self.begin_track(lane, idx, slot, from, backward, cx),
            None => {
                self.notify_user("The playhead is not on this clip".into());
                cx.notify();
            }
        }
    }

    /// Track mask ([`ActionId::TrackMask`]): the selected clip's first rect or ellipse mask,
    /// followed from the playhead to the clip's end.
    pub(crate) fn track_selected_mask(&mut self, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            self.notify_user("Track mask: select a video clip".into());
            cx.notify();
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let slot = session
            .lane_clips(lane)
            .get(idx)
            .and_then(|c| c.effects.iter().position(|e| matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask)));
        let from = session.key_source_frame(lane, idx, self.key_frame());
        match (slot, from) {
            (Some(slot), Some(from)) => self.begin_track(lane, idx, slot, from, false, cx),
            (None, _) => {
                self.notify_user("Track mask: the clip has no rect or ellipse mask".into());
                cx.notify();
            }
            (_, None) => {
                self.notify_user("Track mask: the playhead is not on the clip".into());
                cx.notify();
            }
        }
    }

    /// Starts the track of the mask at `slot` of `lane`/`idx` from source frame `from`.
    fn begin_track(&mut self, lane: Lane, idx: usize, slot: usize, from: u32, backward: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let Some((_, lo, hi)) = session.stabilise_range(lane, idx) else {
            return;
        };
        let to = if backward { lo } else { hi - 1 };
        match session.track_mask_start(lane, idx, slot, from, to) {
            Ok(job) => self.track_jobs.push(job),
            Err(e) => self.notify_user(format!("Track: {e}").into()),
        }
        cx.notify();
    }

    /// Stops the running track of the mask at `slot` on the open clip.
    pub(crate) fn cancel_track(&mut self, slot: usize, cx: &mut Context<Self>) {
        if let Some((lane, idx)) = self.effects_open {
            for job in self.track_jobs.iter().filter(|j| j.lane == lane && j.idx == idx && j.slot == slot) {
                engine::track::cancel(job.id);
            }
        }
        cx.notify();
    }

    /// Writes every finished mask track onto its mask, one undo step each, and says how each
    /// one ended.
    pub(crate) fn poll_tracks(&mut self) {
        if self.track_jobs.is_empty() {
            return;
        }
        let done = match &mut self.session {
            Some(session) => session.track_settle(&mut self.track_jobs),
            None => return,
        };
        if done.is_empty() {
            return;
        }
        self.mark_dirty();
        self.reset_after_reseek();
        for d in done {
            let text = match d.result {
                Ok(o) => match o.lost_at {
                    Some(f) => format!("Lost {} · {} keys", lost_at_text(f, self.fps), o.keys),
                    None => format!("Tracked · {} keys", o.keys),
                },
                Err(e) if e.contains("cancelled") => "Track cancelled".to_owned(),
                Err(_) => "Track failed".to_owned(),
            };
            self.notify_user(text.into());
        }
    }

    /// Writes a whole stack at the card's clip: one undo step, or none when it is the
    /// stack already there.
    pub(crate) fn write_effects(&mut self, stack: Vec<Effect>, cx: &mut Context<Self>) {
        self.effects_draft = None;
        let Some((lane, idx)) = self.effects_open else {
            return;
        };
        if let Some(session) = &mut self.session
            && session.set_effects(lane, idx, stack)
        {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Puts a new effect at the end of the stack.
    pub(crate) fn add_effect(&mut self, kind: EffectKind, cx: &mut Context<Self>) {
        let mut stack = self.effects_shown();
        if stack.len() >= MAX_EFFECTS {
            self.notify_user(format!("A clip holds at most {MAX_EFFECTS} effects").into());
            cx.notify();
            return;
        }
        stack.push(Effect::new(kind));
        self.effects_adding = false;
        self.write_effects(stack, cx);
    }

    /// Switches effect `i` on or off, keeping its settings.
    pub(crate) fn toggle_effect(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut stack = self.effects_shown();
        if let Some(e) = stack.get_mut(i) {
            e.on = !e.on;
            self.write_effects(stack, cx);
        }
    }

    /// Moves effect `i` one place toward the end (`down`) or the start of the stack.
    pub(crate) fn move_effect(&mut self, i: usize, down: bool, cx: &mut Context<Self>) {
        // The stack's rows change under an open list, so it closes.
        self.effects_mode_open = None;
        let mut stack = self.effects_shown();
        let to = if down { i + 1 } else { i.wrapping_sub(1) };
        if i < stack.len() && to < stack.len() {
            stack.swap(i, to);
            self.write_effects(stack, cx);
        }
    }

    /// Takes effect `i` out of the stack.
    pub(crate) fn remove_effect(&mut self, i: usize, cx: &mut Context<Self>) {
        // The stack's rows change under an open list, so it closes.
        self.effects_mode_open = None;
        let mut stack = self.effects_shown();
        if i < stack.len() {
            stack.remove(i);
            self.write_effects(stack, cx);
        }
    }

    /// Sets parameter `p` of effect `i` in one step: a switch, a mode, a reset.
    pub(crate) fn set_effect_param(&mut self, i: usize, p: usize, value: f32, cx: &mut Context<Self>) {
        let at = self.effects_src();
        let mut stack = self.effects_shown();
        if let Some(e) = stack.get_mut(i)
            && let Some(spec) = e.kind.params().get(p)
        {
            write_param(e, p, snap_param(spec, value), at);
            self.write_effects(stack, cx);
        }
    }

    /// Sets the key colour of effect `i` (the green screen's three channels) in one step.
    pub(crate) fn set_effect_colour(&mut self, i: usize, rgb: u32, cx: &mut Context<Self>) {
        let mut stack = self.effects_shown();
        if let Some(e) = stack.get_mut(i) {
            e.values[0] = ((rgb >> 16) & 0xff) as f32;
            e.values[1] = ((rgb >> 8) & 0xff) as f32;
            e.values[2] = (rgb & 0xff) as f32;
            self.write_effects(stack, cx);
        }
    }

    /// The bar parameter `p` of effect `i` is read against, as the card last drew it.
    pub(crate) fn effect_bar(&self, i: usize, p: usize) -> Rc<Cell<Bounds<Pixels>>> {
        let at = i * MAX_PARAMS + p;
        let mut bars = self.effects_bars.borrow_mut();
        if bars.len() <= at {
            bars.resize_with(at + 1, Default::default);
        }
        bars[at].clone()
    }

    /// A bar was pressed: the drag it starts, and the value under the press.
    pub(crate) fn press_effect(&mut self, i: usize, p: usize, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.effects_drag = Some((i, p));
        self.drag_effect(x, cx);
    }

    /// The held bar follows the pointer into the draft; nothing is written yet.
    pub(crate) fn drag_effect(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((i, p)) = self.effects_drag else {
            return;
        };
        let at = self.effects_src();
        let mut stack = self.effects_shown();
        if let Some(e) = stack.get_mut(i)
            && let Some(spec) = e.kind.params().get(p)
            && let Some(fraction) = bar_fraction(self.effect_bar(i, p).get(), x)
        {
            write_param(e, p, snap_param(spec, value_at(fraction, spec.min, spec.max)), at);
            self.effects_draft = Some(stack);
        }
        cx.notify();
    }

    /// The hand lets go: the draft is written as the one undo step of the whole drag.
    pub(crate) fn release_effect(&mut self, cx: &mut Context<Self>) {
        if self.effects_drag.take().is_some()
            && let Some(draft) = self.effects_draft.take()
        {
            self.write_effects(draft, cx);
        }
        cx.notify();
    }
}

/// Where a lost feature stands, as `m:ss.t` (`0:01.2`), for the notice.
fn lost_at_text(frame: u32, fps: f64) -> String {
    let s = f64::from(frame) / fps.max(1.0);
    format!("{}:{:04.1}", (s / 60.0) as u64, s % 60.0)
}
