//! Generated clips, colour labels and the clip on/off switch on the window's side: the
//! File ▸ New rows that put a solid, a gradient, a countdown, colour bars or an adjustment layer on
//! the timeline, the Generator card's writes, Label and Enabled. The model is the engine's
//! ([`engine::generate`], [`engine::project::ClipMark`]); every change here is one undo step.

use crate::*;
use crate::player::text_field::{FieldKey, TextField};
use engine::generate::{Gen, Kind};

/// Which field of the cards the shared typing buffer ([`Player::gen_hex`]) is in: a paint's hex,
/// the green screen's key colour, or one of the numbers typed in the Generator and Title cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenPaint {
    Fill,
    Stroke,
    Solid,
    From,
    To,
    /// The key colour of the green-screen effect at this place in the stack.
    Key(usize),
    /// A shape's stroke width, in pixels.
    Width,
    /// A rectangle's corner radius, in pixels.
    Radius,
    /// A gradient's angle, in degrees.
    Angle,
    /// A countdown's start, in seconds.
    Count,
    /// One of the selected title's numbers, as the Title card shows it.
    Title(crate::player::titling::ScrubField),
    /// The caption style's highlight colour, typed on the Subtitle style card.
    SubHighlight,
}

/// A Generator card number being scrubbed: where the press was and the value it had. A press
/// that never moves opens the field for typing instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GenScrub {
    pub(crate) paint: GenPaint,
    pub(crate) x0: f32,
    pub(crate) v0: f64,
    pub(crate) cur: f64,
    pub(crate) moved: bool,
}

impl GenPaint {
    /// The range and the units a pixel of drag moves a Generator number; `None` for the rest.
    pub(crate) fn num_range(self) -> Option<(f64, f64, f64)> {
        match self {
            GenPaint::Width => Some((0., 400., 1.)),
            GenPaint::Radius => Some((0., 2000., 4.)),
            GenPaint::Angle => Some((0., 359., 1.)),
            GenPaint::Count => Some((1., 99., 0.25)),
            _ => None,
        }
    }
}

/// The kind a New action makes, by the word [`engine::generate::KINDS`] uses.
/// The folder a tone of a project at `project` is kept in: beside the project file once it is
/// saved (it exists on disk), else the data folder.
pub(crate) fn tone_dir(project: &std::path::Path) -> Option<std::path::PathBuf> {
    match project.parent() {
        Some(parent) if project.is_file() && !parent.as_os_str().is_empty() => Some(parent.to_path_buf()),
        _ => engine::generate::Tone::dir(),
    }
}

pub(crate) fn new_kind(action: ActionId) -> Option<&'static str> {
    match action {
        ActionId::NewSolid => Some("solid"),
        ActionId::NewGradient => Some("gradient"),
        ActionId::NewCountdown => Some("countdown"),
        ActionId::NewBars => Some("bars"),
        ActionId::NewAdjustment => Some("adjust"),
        ActionId::NewRectangle => Some("rectangle"),
        ActionId::NewEllipse => Some("ellipse"),
        ActionId::NewLine => Some("line"),
        ActionId::NewArrow => Some("arrow"),
        _ => None,
    }
}

impl Player {
    /// The clip a Label or Enabled stroke acts on: the selection's anchor, else the picture
    /// under the playhead, as Reverse reads it.
    pub(crate) fn label_target(&self) -> Option<(Lane, usize)> {
        let session = self.session.as_ref()?;
        self.selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()))
            .filter(|(lane, idx)| lane.kind != LaneKind::Subtitle && session.lane_clips(*lane).get(*idx).is_some())
    }

    /// Every clip the switch acts on: the whole selection, else the one the stroke reads.
    fn switch_targets(&self) -> Vec<(Lane, usize)> {
        let (picks, _) = self.marks();
        let picks: Vec<_> = picks.into_iter().filter(|(lane, _)| lane.kind != LaneKind::Subtitle).collect();
        match picks.is_empty() {
            true => self.label_target().into_iter().collect(),
            false => picks,
        }
    }

    /// Enabled: switches the selected clips off, or back on when they are all off. One undo step
    /// for the lot; the toast says which way it went.
    pub(crate) fn toggle_clip_enabled(&mut self, cx: &mut Context<Self>) {
        let targets = self.switch_targets();
        let Some(session) = &mut self.session else {
            self.notify_user("Nothing to switch — no file open".into());
            cx.notify();
            return;
        };
        if targets.is_empty() {
            self.notify_user("Nothing to switch — select a clip".into());
            cx.notify();
            return;
        }
        let enable = targets.iter().all(|&(lane, idx)| session.mark_of(lane, idx).off);
        session.begin_batch();
        let done = targets.iter().filter(|&&(lane, idx)| session.set_enabled(lane, idx, enable)).count();
        session.end_batch();
        if done > 0 {
            self.mark_dirty();
            self.reset_after_reseek();
            let what = if enable { "enabled" } else { "disabled" };
            let undo = self.keymap.display(ActionId::Undo);
            self.notify_user(format!("{done} clip{} {what} · undo {undo}", if done == 1 { "" } else { "s" }).into());
        }
        cx.notify();
    }

    /// Labels the clip (0 clears it). One undo step.
    pub(crate) fn apply_label(&mut self, lane: Lane, idx: usize, label: u8, cx: &mut Context<Self>) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.set_label(lane, idx, label) {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The Label list for the clip a stroke or the palette names, hung from the clip readout.
    pub(crate) fn open_label_list(&mut self, cx: &mut Context<Self>) {
        match self.label_target() {
            Some((lane, idx)) => {
                let at = self.ledger_anchor(LedgerItem::Clip);
                self.open_picker(Pick::Label(lane, idx), at, cx);
            }
            None => {
                self.notify_user("Nothing to label — select a clip".into());
                cx.notify();
            }
        }
    }

    /// File ▸ New: puts a generated clip on the timeline at the playhead. A solid, gradient,
    /// countdown or bars lands on the lowest video lane that is free for its stretch, an
    /// adjustment layer on the highest one (the layers below are what it acts on); when no lane
    /// is free a lane is added above. Nothing is overwritten. One undo step, and the clip is
    /// selected.
    pub(crate) fn new_generator(&mut self, action: ActionId, cx: &mut Context<Self>) {
        let Some(name) = new_kind(action) else {
            return;
        };
        let Some(session) = &mut self.session else {
            self.notify_user("Nothing to add to — no file open".into());
            cx.notify();
            return;
        };
        let Some(spec) = session.generator_named(name) else {
            return;
        };
        let fps = session.meta().frame_rate;
        let at = session.now();
        let start = frame_at(at, fps);
        let secs = match spec.kind {
            Kind::Countdown { from, .. } => f64::from(from),
            _ => 5.0,
        };
        let end = start + (secs * fps).round() as u32;
        let free = |session: &engine::PlaybackSession, lane: Lane| {
            !session.lane_clips(lane).iter().any(|c| c.start < end && c.end() > start)
        };
        let mut video: Vec<Lane> = session.lanes().into_iter().filter(|l| l.kind == LaneKind::Video).collect();
        let adjust = spec.is_adjust();
        if adjust {
            video.reverse();
        }
        // A shape goes above whatever the playhead is on, so it lands over the picture.
        if spec.has_alpha() {
            let top = video.iter().rposition(|&l| session.lane_clips(l).iter().any(|c| c.start <= start && c.end() > start));
            video.drain(..top.map_or(0, |t| t + 1));
        }
        session.begin_batch();
        let lane = match video.into_iter().find(|&l| free(session, l)) {
            Some(lane) => lane,
            None => session.add_lane(LaneKind::Video),
        };
        let placed = session.place_generated(spec, at, None, Some(lane));
        if let (Ok(true), Some(scale)) = (&placed, spec.default_scale())
            && let Some(i) = session.lane_clips(lane).iter().position(|c| c.start == start)
        {
            session.set_transform(lane, i, Some(engine::transform::TransformParams { scale, ..Default::default() }));
        }
        session.end_batch();
        match placed {
            Ok(true) => {
                let idx = session
                    .lane_clips(lane)
                    .iter()
                    .position(|c| c.start <= start && c.end() > start)
                    .unwrap_or(0);
                self.selected.set_one((lane, idx));
                self.mark_dirty();
                self.reset_after_reseek();
                let undo = self.keymap.display(ActionId::Undo);
                self.notify_user(format!("{} added on {} · undo {undo}", action.label(), lane.label()).into());
            }
            Ok(false) => self.notify_user("Nothing added — the place is refused".into()),
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// File > New > Tone, and Bars and tone: a 1 kHz test tone at -20 dBFS (RMS), ten seconds, on the
    /// first free audio lane at the playhead (a new lane when none is free). Bars and tone adds
    /// colour bars of the same length on a free video lane and links the two. One undo step.
    pub(crate) fn new_tone(&mut self, action: ActionId, cx: &mut Context<Self>) {
        // A saved project keeps its tone files beside it; an untitled one keeps them in the data folder.
        let tones = tone_dir(&self.project_path);
        let Some(session) = &mut self.session else {
            self.notify_user("Nothing to add to — no file open".into());
            cx.notify();
            return;
        };
        let both = action == ActionId::NewBarsAndTone;
        let fps = session.meta().frame_rate;
        let at = session.now();
        let start = frame_at(at, fps);
        let secs = engine::generate::TONE_SECS;
        let end = start + (f64::from(secs) * fps).round() as u32;
        let free = |session: &engine::PlaybackSession, lane: Lane| !session.lane_clips(lane).iter().any(|c| c.start < end && c.end() > start);
        session.begin_batch();
        let audio = match session.lanes().into_iter().filter(|l| l.kind == LaneKind::Audio).find(|&l| free(session, l)) {
            Some(lane) => lane,
            None => session.add_lane(LaneKind::Audio),
        };
        let video = both.then(|| match session.lanes().into_iter().filter(|l| l.kind == LaneKind::Video).find(|&l| free(session, l)) {
            Some(lane) => lane,
            None => session.add_lane(LaneKind::Video),
        });
        let bars = match (video, session.generator_named("bars")) {
            (Some(lane), Some(spec)) => session.place_generated(spec, at, Some(f64::from(secs)), Some(lane)),
            _ => Ok(true),
        };
        let tone = match &tones {
            Some(dir) => session.place_tone(engine::generate::Tone::DEFAULT, at, secs, Some(audio), dir),
            None => Err("no data folder to keep the tone in".into()),
        };
        if let (Some(v), Ok(true), Ok(true)) = (video, &bars, &tone)
            && let (Some(vi), Some(ai)) = (
                session.lane_clips(v).iter().position(|c| c.start == start),
                session.lane_clips(audio).iter().position(|c| c.start == start),
            )
        {
            let _ = session.group(v, vi, audio, ai);
        }
        session.end_batch();
        let picked = match (&bars, &tone) {
            (Ok(true), Ok(true)) => session.lane_clips(audio).iter().position(|c| c.start == start).map(|i| (audio, i)),
            _ => None,
        };
        match (bars, tone) {
            (Ok(true), Ok(true)) => {
                if let Some(pick) = picked {
                    self.selected.set_one(pick);
                }
                self.mark_dirty();
                self.reset_after_reseek();
                let undo = self.keymap.display(ActionId::Undo);
                let what = match video {
                    Some(v) => format!("{} and tone added on {} and {}", "Colour bars", v.label(), audio.label()),
                    None => format!("Tone added on {}", audio.label()),
                };
                self.notify_user(format!("{what} · undo {undo}").into());
            }
            (Err(e), _) | (_, Err(e)) => self.notify_user(e.to_string().into()),
            _ => self.notify_user("Nothing added — the place is refused".into()),
        }
        cx.notify();
    }

    /// A press on a shape's hex field: it opens for typing with all of it selected.
    pub(crate) fn begin_gen_hex(&mut self, paint: GenPaint, cx: &mut Context<Self>) {
        self.commit_gen_hex(cx);
        let Some(session) = &self.session else { return };
        let Some((lane, idx)) = self.selected.anchor() else { return };
        let Some(kind) = session.generator_of(lane, idx).map(|g| g.kind) else { return };
        let now = match (paint, kind) {
            (GenPaint::Fill, Kind::Shape(sh)) => sh.fill,
            (GenPaint::Stroke, Kind::Shape(sh)) => sh.stroke,
            (GenPaint::Solid, Kind::Solid { rgb }) => Some(rgb),
            (GenPaint::From, Kind::Gradient { a, .. }) => Some(a),
            (GenPaint::To, Kind::Gradient { b, .. }) => Some(b),
            _ => return,
        };
        self.gen_hex = Some((paint, TextField::selected(now.map_or("none".to_string(), |c| format!("{c:06x}")))));
        cx.notify();
    }

    /// Writes the typed colour (six hex digits, or none) as the shape's paint, one undo step.
    /// Anything else keeps the old colour and says so.
    pub(crate) fn commit_gen_hex(&mut self, cx: &mut Context<Self>) {
        let Some((paint, field)) = self.gen_hex.take() else {
            return;
        };
        match paint {
            GenPaint::Key(i) => return self.commit_key_hex(i, &field.text, cx),
            GenPaint::Title(f) => return self.commit_title_num(f, &field.text, cx),
            GenPaint::SubHighlight => return self.commit_sub_highlight_hex(&field.text, cx),
            GenPaint::Width | GenPaint::Radius | GenPaint::Angle | GenPaint::Count => {
                return match typed_number(&field.text) {
                    Some(v) => self.write_gen_num(paint, v, cx),
                    None => {
                        self.notify_user("That is not a number — kept the old one".into());
                        cx.notify();
                    }
                };
            }
            _ => {}
        }
        let text = field.text.trim().trim_start_matches('#').to_ascii_lowercase();
        let colour = match text.as_str() {
            "none" | "" => Some(None),
            t if t.len() == 6 => u32::from_str_radix(t, 16).ok().map(Some),
            _ => None,
        };
        let Some(colour) = colour else {
            self.notify_user("A colour is six hex digits, such as ffcc00, or none — kept the old one".into());
            cx.notify();
            return;
        };
        let Some(session) = &self.session else { return };
        let Some((lane, idx)) = self.selected.anchor() else { return };
        let Some(spec) = session.generator_of(lane, idx) else { return };
        let new_kind = match (paint, spec.kind) {
            (GenPaint::Solid, Kind::Solid { rgb }) => Kind::Solid { rgb: colour.unwrap_or(rgb) },
            (GenPaint::From, Kind::Gradient { a, b, angle }) => Kind::Gradient { a: colour.unwrap_or(a), b, angle },
            (GenPaint::To, Kind::Gradient { a, b, angle }) => Kind::Gradient { a, b: colour.unwrap_or(b), angle },
            (_, Kind::Shape(mut sh)) => {
                match paint {
                    GenPaint::Fill => sh.fill = colour,
                    _ => {
                        sh.stroke = colour;
                        if colour.is_some() && sh.width == 0 {
                            sh.width = 6;
                        }
                    }
                }
                // A shape with nothing to paint is no shape.
                let visible = sh.fill.is_some() && sh.form.fills() || sh.stroke.is_some() && sh.width > 0;
                if !visible {
                    self.notify_user("A shape needs a fill or a stroke — kept the old colour".into());
                    cx.notify();
                    return;
                }
                Kind::Shape(sh)
            }
            _ => return,
        };
        self.write_generator(lane, idx, Gen { kind: new_kind, ..spec }, cx);
    }

    /// The keyboard while a shape's hex is being typed: enter applies, escape leaves.
    pub(crate) fn gen_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        let Some((paint, field)) = self.gen_hex.as_mut() else { return };
        let number = matches!(paint, GenPaint::Width | GenPaint::Radius | GenPaint::Angle | GenPaint::Count | GenPaint::Title(_));
        let result = field.key(key, ctrl, alt, key_char, if number { 8 } else { 7 });
        match paint {
            GenPaint::Key(_) => field.text.retain(|c| c.is_ascii_hexdigit() || c == '#'),
            _ if number => field.text.retain(|c| c.is_ascii_digit() || c == '.' || c == ','),
            _ => field.text.retain(|c| c.is_ascii_alphanumeric() || c == '#'),
        }
        match result {
            FieldKey::Commit => self.commit_gen_hex(cx),
            FieldKey::Cancel => self.gen_hex = None,
            FieldKey::Edited => {}
        }
        cx.notify();
    }

    /// A press on the green screen's key colour field: its hex opens for typing, all selected.
    pub(crate) fn begin_key_hex(&mut self, i: usize, cx: &mut Context<Self>) {
        self.commit_gen_hex(cx);
        let Some(e) = self.effects_shown().get(i).cloned() else { return };
        let rgb = ((e.values[0] as u32) << 16) | ((e.values[1] as u32) << 8) | e.values[2] as u32;
        self.gen_hex = Some((GenPaint::Key(i), TextField::selected(crate::ui::kit::hex_text(rgb))));
        cx.notify();
    }

    /// Writes the typed key colour as one undo step; anything that is not a colour keeps the old one.
    fn commit_key_hex(&mut self, i: usize, text: &str, cx: &mut Context<Self>) {
        match crate::ui::kit::parse_hex(text) {
            Some(rgb) => self.set_effect_colour(i, rgb, cx),
            None => {
                self.notify_user("A colour is six hex digits, such as 00ff00 — kept the old one".into());
                cx.notify();
            }
        }
    }

    /// The value of a Generator number on the selected clip.
    pub(crate) fn gen_num_value(&self, paint: GenPaint) -> Option<f64> {
        let (lane, idx) = self.selected.anchor()?;
        match (paint, self.session.as_ref()?.generator_of(lane, idx)?.kind) {
            (GenPaint::Width, Kind::Shape(s)) => Some(f64::from(s.width)),
            (GenPaint::Radius, Kind::Shape(s)) => Some(f64::from(s.radius)),
            (GenPaint::Angle, Kind::Gradient { angle, .. }) => Some(f64::from(angle)),
            (GenPaint::Count, Kind::Countdown { from, .. }) => Some(f64::from(from)),
            _ => None,
        }
    }

    /// Writes a Generator number (clamped and rounded to whole units) as one undo step.
    fn write_gen_num(&mut self, paint: GenPaint, v: f64, cx: &mut Context<Self>) {
        let Some((lo, hi, _)) = paint.num_range() else { return };
        let Some((lane, idx)) = self.selected.anchor() else { return };
        let Some(spec) = self.session.as_ref().and_then(|s| s.generator_of(lane, idx)) else { return };
        let v = v.round().clamp(lo, hi) as u16;
        let kind = match (paint, spec.kind) {
            (GenPaint::Width, Kind::Shape(s)) => Kind::Shape(engine::generate::Shape { width: v, ..s }),
            (GenPaint::Radius, Kind::Shape(s)) => Kind::Shape(engine::generate::Shape { radius: v, ..s }),
            (GenPaint::Angle, Kind::Gradient { a, b, .. }) => Kind::Gradient { a, b, angle: v },
            (GenPaint::Count, Kind::Countdown { milli_fps, .. }) => Kind::Countdown { from: v, milli_fps },
            _ => return,
        };
        if kind != spec.kind {
            self.write_generator(lane, idx, Gen { kind, ..spec }, cx);
        }
    }

    /// A press on a Generator number: a drag that follows scrubs it, a click that does not
    /// move types into it.
    pub(crate) fn begin_gen_scrub(&mut self, paint: GenPaint, x: f32, cx: &mut Context<Self>) {
        self.commit_gen_hex(cx);
        if let Some(v0) = self.gen_num_value(paint) {
            self.gen_scrub = Some(GenScrub { paint, x0: x, v0, cur: v0, moved: false });
        }
    }

    /// The pointer moved with a Generator number pressed: the number follows, shown live.
    pub(crate) fn gen_scrub_move(&mut self, x: f32, fine: bool, cx: &mut Context<Self>) {
        let Some(s) = self.gen_scrub else { return };
        let Some((lo, hi, per_px)) = s.paint.num_range() else { return };
        let dx = x - s.x0;
        if !s.moved && dx.abs() < 3. {
            return;
        }
        let cur = crate::ui::kit::scrub_value(s.v0, dx, per_px, fine, lo, hi).round();
        self.gen_scrub = Some(GenScrub { cur, moved: true, ..s });
        cx.notify();
    }

    /// The button came up: a scrubbed number is written (one undo step); a press that never
    /// moved opens the field for typing.
    pub(crate) fn gen_scrub_end(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.gen_scrub.take() else { return };
        if s.moved {
            self.write_gen_num(s.paint, s.cur, cx);
        } else {
            self.gen_hex = Some((s.paint, TextField::selected(format!("{}", s.v0))));
        }
        cx.notify();
    }

    /// Writes a typed Title number as one undo step. Seconds become whole frames; a number out
    /// of range is held to it.
    fn commit_title_num(&mut self, field: crate::player::titling::ScrubField, text: &str, cx: &mut Context<Self>) {
        use crate::player::titling::ScrubField as F;
        let Some(v) = typed_number(text) else {
            self.notify_user("That is not a number — kept the old one".into());
            cx.notify();
            return;
        };
        let fps = self.session.as_ref().map_or(25., |s| s.meta().frame_rate).max(1.);
        let frames = (v.max(0.) * fps).round() as u32;
        self.write_title(
            |t| match field {
                // The card shows the share of the picture height in percent; the engine keeps per mille.
                F::Size => t.style.size = ((v * 10.).round().max(0.) as u32).clamp(engine::text::SIZE_RANGE.0, engine::text::SIZE_RANGE.1),
                F::Start | F::Move => t.start = frames,
                F::Length => {
                    t.frames = frames.max(1);
                    t.fade_in = t.fade_in.min(t.frames);
                    t.fade_out = t.fade_out.min(t.frames);
                    t.anim_frames = t.anim_frames.min(t.frames);
                }
                F::AnimLength => t.anim_frames = frames.clamp(1, t.frames.max(1)),
                F::BoxOpacity => t.style.box_color[3] = ((v.round().clamp(0., 100.) as u32 * 255 + 50) / 100) as u8,
            },
            cx,
        );
    }

    /// Writes new parameters for the generator clip at `(lane, idx)`. One undo step.
    pub(crate) fn write_generator(&mut self, lane: Lane, idx: usize, spec: Gen, cx: &mut Context<Self>) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.set_generator(lane, idx, spec) {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }
}

/// The number a typed buffer holds (`,` reads as `.`), or `None` when it is not one.
fn typed_number(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite())
}
