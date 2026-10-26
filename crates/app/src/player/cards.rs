//! The cards the window opens over itself: the pickers, colour, speed,
//! silence, the mixer and the equalizer.

use crate::player::project_dialogs::PathFor;
use crate::ui::settings_stance::SettingsTab;
use crate::*;
use engine::project::KeyKind;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// An auto reframe being measured on a thread ([`Player::start_reframe`]).
pub(crate) struct ReframeRun {
    lane: Lane,
    idx: usize,
    job: engine::reframe::ReframeJob,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<engine::reframe::Subject, String>>>>,
}

/// The transform card's More fold: the crop rows live under it, closed by
/// default and remembered per panel like every other fold.
pub(crate) const TRANSFORM_CROP_FOLD: &str = "transform-crop";

/// How many transform rows sit above the crop fold: Position X and Y, Scale
/// and Rotation. The crop rows are the bands after them.
pub(crate) const TRANSFORM_MAIN_BANDS: usize = 4;

/// A keyboard nudge on a position row, in canvas pixels.
pub(crate) const POSITION_NUDGE_PX: f32 = 10.;
/// The scale a picture in picture takes: the Place row's corner pick, and the
/// Picture in picture button, both put a full-size clip at this size.
pub(crate) const PIP_SCALE: f32 = 0.3;

/// Position X and Y, the first two transform rows. The engine keeps them as a
/// fraction of the canvas, so the card reads and drags them in pixels.
pub(crate) fn is_position_band(band: usize) -> bool {
    band < 2
}

/// The canvas span a position band's fraction is taken over: the width for X,
/// the height for Y.
fn position_span(band: usize, canvas: (u32, u32)) -> f32 {
    if band == 0 { canvas.0 } else { canvas.1 }.max(1) as f32
}

/// A position on its whole-pixel grid, clamped to the band's range. A drag and
/// a readout agree on the pixel the picture lands on, not a 5% step.
pub(crate) fn snap_position_px(band: usize, value: f32, canvas: (u32, u32)) -> f32 {
    let (_, low, high) = TRANSFORM_BANDS[band];
    let span = position_span(band, canvas);
    ((value * span).round() / span).clamp(low, high)
}

/// What an apply acts on: the previewed regions and the lanes they reach.
type Previewed = (Vec<(u32, u32)>, Vec<Lane>);
/// Where a slider bar was last laid out, read back by its drag handler.
type BarCell = Rc<Cell<Bounds<Pixels>>>;

/// The quietest a meter shows, in dBFS: the bottom of its bar.
pub(crate) const METER_FLOOR_DB: f32 = -60.;
/// How fast a meter falls back once the sound drops, in dB a second.
const METER_FALL_DB_PER_S: f32 = 20.;
/// How long a peak marker holds before it starts to fall.
const METER_HOLD_S: f32 = 1.;
/// A pause between two reads longer than this (a stalled window, a sleep) is
/// counted as this long, so the meters do not fall through the floor at once.
const METER_MAX_DT_S: f32 = 0.25;

/// dBFS of a linear peak, floored at [`METER_FLOOR_DB`].
pub(crate) fn meter_db(peak: f32) -> f32 {
    match peak > 0. {
        true => (20. * peak.log10()).max(METER_FLOOR_DB),
        false => METER_FLOOR_DB,
    }
}

/// One level meter: how loud the sound is now, and the marker that holds the
/// loudest it has been for [`METER_HOLD_S`]. Attacks at once and falls at a
/// fixed rate, so a short peak reads, and a steady one reads as steady.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MixMeter {
    /// dBFS, at or above [`METER_FLOOR_DB`].
    pub(crate) level_db: f32,
    /// dBFS of the held marker.
    pub(crate) hold_db: f32,
    /// When the marker last rose; `None` before its first reading.
    held_at: Option<Instant>,
    /// When this meter was last read, for the fall between two reads.
    read_at: Option<Instant>,
}

impl Default for MixMeter {
    /// Silent, with nothing held.
    fn default() -> Self {
        Self {
            level_db: METER_FLOOR_DB,
            hold_db: METER_FLOOR_DB,
            held_at: None,
            read_at: None,
        }
    }
}

impl MixMeter {
    /// Takes one reading: `peak` is the linear level the sound reached since
    /// the last read ([`engine::Levels`]), `now` when it was read.
    pub(crate) fn feed(&mut self, peak: f32, now: Instant) {
        let peak_db = meter_db(peak);
        let dt = self
            .read_at
            .map_or(0., |at| now.saturating_duration_since(at).as_secs_f32())
            .min(METER_MAX_DT_S);
        self.read_at = Some(now);
        self.level_db = fall(self.level_db, peak_db, dt);
        if peak_db >= self.hold_db {
            self.hold_db = peak_db;
            self.held_at = Some(now);
        } else if self
            .held_at
            .is_none_or(|at| now.saturating_duration_since(at).as_secs_f32() >= METER_HOLD_S)
        {
            self.hold_db = fall(self.hold_db, peak_db, dt);
        }
    }

    /// Back to silence: what a paused or stopped meter shows.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// `shown` after `dt` seconds with `peak` read: it rises at once to `peak` and
/// otherwise falls at [`METER_FALL_DB_PER_S`], never below `peak`.
fn fall(shown: f32, peak: f32, dt: f32) -> f32 {
    match peak >= shown {
        true => peak,
        false => (shown - METER_FALL_DB_PER_S * dt).max(peak),
    }
}

/// The shortest time between two reads of the transport's master meter: 30 Hz is plenty for a
/// bar and keeps the engine's peaks (and the repaint they cost) off the per-frame path.
pub(crate) const MASTER_METER_EVERY_S: f32 = 1. / 30.;
/// How long the clip lamp stays lit after a sample reached full scale.
pub(crate) const CLIP_LAMP_S: f32 = 3.;
/// A linear peak this high is a clipped sample (0 dBFS, with a hair of float slack).
pub(crate) const CLIP_PEAK: f32 = 0.999;

/// The transport's stereo master meter: a [`MixMeter`] per side, read at most
/// [`MASTER_METER_EVERY_S`] apart, and the clip lamp's latch.
#[derive(Default)]
pub(crate) struct MasterMeter {
    pub(crate) lr: [MixMeter; 2],
    read_at: Option<Instant>,
    clip_at: Option<Instant>,
}

impl MasterMeter {
    /// One frame's step. Playing, it reads `peaks` (left, right) if the last read was long enough
    /// ago -- otherwise it leaves them in the engine to pile up for the next read -- and lights
    /// the lamp for a clipped peak. Not playing, everything falls to silence at once.
    pub(crate) fn step(&mut self, playing: bool, peaks: impl FnOnce() -> [f32; 2], now: Instant) {
        if !playing {
            *self = Self::default();
            return;
        }
        if self
            .read_at
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() < MASTER_METER_EVERY_S)
        {
            return;
        }
        self.read_at = Some(now);
        let peaks = peaks();
        for (meter, peak) in self.lr.iter_mut().zip(peaks) {
            meter.feed(peak, now);
        }
        if peaks.iter().any(|&p| p >= CLIP_PEAK) {
            self.clip_at = Some(now);
        }
    }

    /// Whether the clip lamp is lit at `now`.
    pub(crate) fn clipped(&self, now: Instant) -> bool {
        self.clip_at
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() < CLIP_LAMP_S)
    }
}

/// The mix card's own state between frames: the meters it reads and the bars
/// its sliders are read against. Behind a `RefCell` on [`Player`], because the
/// card draws from `&self` and a meter has to be read at the moment it is drawn.
#[derive(Default)]
pub(crate) struct MixPanel {
    meters: Vec<(Lane, MixMeter)>,
    master: MixMeter,
    master_meter: MasterMeter,
    gain_bars: Vec<(Lane, Rc<Cell<Bounds<Pixels>>>)>,
    /// The selected clip's volume bar.
    pub(crate) clip_bar: Rc<Cell<Bounds<Pixels>>>,
    /// The selected clip's pitch track.
    pub(crate) pitch_bar: Rc<Cell<Bounds<Pixels>>>,
    ceiling_bar: Rc<Cell<Bounds<Pixels>>>,
    /// The bars of the voice-dynamics sliders, one per lane and setting.
    voice_bars: Vec<((Lane, VoiceParam), BarCell)>,
    /// The lanes whose dynamics section is unfolded. Folded is the default: a
    /// strip is a fader first.
    voice_open: Vec<Lane>,
}

/// One of a lane's voice processors ([`engine::voice`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoiceProc {
    Gate,
    Nr,
    Comp,
    Deess,
}

impl VoiceProc {
    pub(crate) const ALL: [VoiceProc; 4] = [Self::Gate, Self::Nr, Self::Comp, Self::Deess];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Gate => "Gate",
            Self::Nr => "Noise reduction",
            Self::Comp => "Compressor",
            Self::Deess => "De-esser",
        }
    }

    pub(crate) fn is_on(self, fx: &engine::voice::VoiceFx) -> bool {
        match self {
            Self::Gate => fx.gate.on,
            Self::Nr => fx.nr.on,
            Self::Comp => fx.comp.on,
            Self::Deess => fx.deess.on,
        }
    }

    /// The sliders that belong to this processor.
    pub(crate) fn params(self) -> &'static [VoiceParam] {
        match self {
            Self::Gate => &[VoiceParam::GateThreshold],
            Self::Nr => &[VoiceParam::NrStrength],
            Self::Comp => &[
                VoiceParam::CompThreshold,
                VoiceParam::CompRatio,
                VoiceParam::CompMakeup,
            ],
            Self::Deess => &[VoiceParam::DeessThreshold, VoiceParam::DeessAmount],
        }
    }
}

/// One slider of the dynamics section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoiceParam {
    GateThreshold,
    NrStrength,
    CompThreshold,
    CompRatio,
    CompMakeup,
    DeessThreshold,
    DeessAmount,
}

impl VoiceParam {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GateThreshold | Self::CompThreshold | Self::DeessThreshold => "Threshold",
            Self::NrStrength => "Strength",
            Self::CompRatio => "Ratio",
            Self::CompMakeup => "Makeup",
            Self::DeessAmount => "Amount",
        }
    }

    /// The range the slider spans and the step a value snaps to.
    pub(crate) fn range(self) -> (f32, f32, f32) {
        match self {
            Self::GateThreshold => (-80., -10., 1.),
            Self::NrStrength | Self::DeessAmount => (0., 1., 0.05),
            Self::CompThreshold | Self::DeessThreshold => (-60., 0., 1.),
            Self::CompRatio => (1., 10., 0.5),
            Self::CompMakeup => (0., 18., 0.5),
        }
    }

    pub(crate) fn get(self, fx: &engine::voice::VoiceFx) -> f32 {
        match self {
            Self::GateThreshold => fx.gate.threshold_db,
            Self::NrStrength => fx.nr.strength,
            Self::CompThreshold => fx.comp.threshold_db,
            Self::CompRatio => fx.comp.ratio,
            Self::CompMakeup => fx.comp.makeup(),
            Self::DeessThreshold => fx.deess.threshold_db,
            Self::DeessAmount => fx.deess.amount,
        }
    }

    /// Writes `v` snapped to the step. A makeup typed by hand turns the
    /// automatic makeup off, as the MCP op does.
    pub(crate) fn set(self, fx: &mut engine::voice::VoiceFx, v: f32) {
        let (lo, hi, step) = self.range();
        let v = ((v.clamp(lo, hi) / step).round() * step).clamp(lo, hi);
        match self {
            Self::GateThreshold => fx.gate.threshold_db = v,
            Self::NrStrength => fx.nr.strength = v,
            Self::CompThreshold => fx.comp.threshold_db = v,
            Self::CompRatio => fx.comp.ratio = v,
            Self::CompMakeup => {
                fx.comp.makeup_db = v;
                fx.comp.auto_makeup = false;
            }
            Self::DeessThreshold => fx.deess.threshold_db = v,
            Self::DeessAmount => fx.deess.amount = v,
        }
    }

    pub(crate) fn text(self, v: f32) -> String {
        match self {
            Self::GateThreshold | Self::CompThreshold | Self::DeessThreshold => {
                format!("{v:.0} dB")
            }
            Self::NrStrength | Self::DeessAmount => format!("{:.0}%", v * 100.),
            Self::CompRatio => format!("{v:.1}:1"),
            Self::CompMakeup => format!("{v:+.1} dB"),
        }
    }
}

/// One audio lane as the card draws it this frame.
pub(crate) struct MixStrip {
    pub(crate) lane: Lane,
    pub(crate) meter: MixMeter,
    pub(crate) bar: Rc<Cell<Bounds<Pixels>>>,
}

/// The mix card's readings for one frame.
pub(crate) struct MixReading {
    pub(crate) strips: Vec<MixStrip>,
    pub(crate) master: MixMeter,
    pub(crate) ceiling_bar: Rc<Cell<Bounds<Pixels>>>,
}

/// The slider a mix drag is moving: a track's fader or the limiter's ceiling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MixDrag {
    Gain(Lane),
    /// The selected clip's own volume ([`PlaybackSession::set_clip_gain`]).
    Clip(Lane, usize),
    /// The selected clip's pitch in semitones ([`PlaybackSession::set_clip_pitch`]).
    Pitch(Lane, usize),
    Ceiling,
    Voice(Lane, VoiceParam),
}

/// The pointer's place along a slider's bar, 0..1, or `None` when the bar has
/// not been drawn yet.
pub(crate) fn bar_fraction(bar: Bounds<Pixels>, x: Pixels) -> Option<f32> {
    (bar.size.width > px(0.)).then(|| frac_along(x, bar))
}

/// A value on a range, from its place along the slider.
pub(crate) fn value_at(fraction: f32, lo: f32, hi: f32) -> f32 {
    lo + fraction.clamp(0., 1.) * (hi - lo)
}

/// Where a track's gain sits on its fader, 0..1. The working range, -24..+12 dB, takes the
/// most of the travel (80%); the quiet tail down to -60 dB takes the rest, so a drag near a
/// working level moves the gain in fine steps.
const GAIN_KNEE_DB: f32 = -24.0;
const GAIN_KNEE_AT: f32 = 0.2;

pub(crate) fn gain_fraction(db: f32, hi: f32) -> f32 {
    let lo = engine::project::MIN_GAIN_DB;
    let at = match db < GAIN_KNEE_DB {
        true => (db - lo) / (GAIN_KNEE_DB - lo) * GAIN_KNEE_AT,
        false => GAIN_KNEE_AT + (db - GAIN_KNEE_DB) / (hi - GAIN_KNEE_DB) * (1. - GAIN_KNEE_AT),
    };
    at.clamp(0., 1.)
}

/// The gain a place along the fader reads: the inverse of [`gain_fraction`].
pub(crate) fn gain_at(fraction: f32, hi: f32) -> f32 {
    let lo = engine::project::MIN_GAIN_DB;
    let f = fraction.clamp(0., 1.);
    match f < GAIN_KNEE_AT {
        true => lo + f / GAIN_KNEE_AT * (GAIN_KNEE_DB - lo),
        false => GAIN_KNEE_DB + (f - GAIN_KNEE_AT) / (1. - GAIN_KNEE_AT) * (hi - GAIN_KNEE_DB),
    }
}

/// The toggled solo set: `lane` leaves it when it is in it, and joins it when
/// it is not.
pub(crate) fn solo_toggled(solo: &[Lane], lane: Lane) -> Vec<Lane> {
    match solo.contains(&lane) {
        true => solo.iter().copied().filter(|&l| l != lane).collect(),
        false => solo.iter().copied().chain([lane]).collect(),
    }
}

/// Which meter reads `lane`, given the audio lanes that hold something, in the
/// order the engine's levels are kept. An empty track has none; a timeline
/// with no sound at all reads its `A1` as the one slot.
pub(crate) fn meter_slot_in(sounding: &[Lane], lane: Lane) -> Option<usize> {
    match sounding.is_empty() {
        true => (lane == Lane::A1).then_some(0),
        false => sounding.iter().position(|&l| l == lane),
    }
}

/// One frame's reading into a meter: the peak while the sound plays, silence
/// otherwise.
fn feed_meter(meter: &mut MixMeter, playing: bool, peak: f32, now: Instant) {
    match playing {
        true => meter.feed(peak, now),
        false => meter.reset(),
    }
}

/// Gets the entry for `lane`, adding a default one the first time it is asked for.
fn lane_entry<T: Default>(items: &mut Vec<(Lane, T)>, lane: Lane) -> &mut T {
    let at = match items.iter().position(|(l, _)| *l == lane) {
        Some(at) => at,
        None => {
            items.push((lane, T::default()));
            items.len() - 1
        }
    };
    &mut items[at].1
}

/// Where a rate (in thousandths) sits on the speed bar, 0..1. The bar is
/// logarithmic, so every doubling takes the same travel: a quarter to a half is
/// as far along as two to four.
pub(crate) fn speed_fraction(permille: f32) -> f32 {
    let lo = f32::from(Speed::MIN.permille());
    let hi = f32::from(Speed::MAX.permille());
    ((permille / lo).ln() / (hi / lo).ln()).clamp(0., 1.)
}

/// The rate under a fraction of the speed bar, on the [`SPEED_STEP`] grid.
/// Real time is on the grid (1000 divides by 50), so a pointer can land on it.
pub(crate) fn speed_on_bar(along: f32) -> Speed {
    let lo = f32::from(Speed::MIN.permille());
    let hi = f32::from(Speed::MAX.permille());
    let raw = lo * (hi / lo).powf(along.clamp(0., 1.));
    speed_at((raw / SPEED_STEP as f32).round() as i32 * SPEED_STEP)
}


/// The next row the arrow keys land on from `from`, skipping the platform's internal fonts (a
/// name with a leading dot), which the card does not show. Row 0 and the default row never hide,
/// so the walk always ends.
fn sub_row_step(fonts: &[String], rows: usize, from: usize, down: bool) -> usize {
    let mut r = from;
    loop {
        r = if down { (r + 1) % rows } else { (r + rows - 1) % rows };
        let hidden = r >= SUB_ROW_FIRST_FAMILY && fonts[r - SUB_ROW_FIRST_FAMILY].starts_with('.');
        if !hidden || r == from {
            return r;
        }
    }
}

impl Player {
    /// The project resized, whichever asked: the list that names a size
    /// outright comes through here. With no
    /// timeline yet the size is held as [`Player::pending_settings`] instead of
    /// being written anywhere -- there is no session to write it onto -- and
    /// applied the moment the first one exists.
    pub(crate) fn apply_resolution(&mut self, width: u32, height: u32, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session {
            if session.set_resolution(width, height) {
                // A project field written from a click, with no keymap action
                // behind it to arm it ([`Player::act`]'s list is the action
                // doors only).
                self.mark_dirty();
                self.notify_user(format!("Project: {width}x{height}").into());
                self.reset_after_reseek();
            }
        } else {
            self.pending_settings.0 = Some((width, height));
            self.notify_user(
                format!("Project: {width}x{height} — takes effect on the next file opened").into(),
            );
        }
        cx.notify();
    }

    /// The project cut at another rate: the list names one and this is where it
    /// happens, the way [`apply_resolution`](Self::apply_resolution) is for a
    /// size. The whole timeline is conformed to it by the engine
    /// ([`PlaybackSession::set_frame_rate`]) -- same seconds, same footage --
    /// and the rate the app itself counts frames in follows, since every
    /// timecode, ruler mark and step key here is measured in it.
    pub(crate) fn apply_frame_rate(&mut self, fps: f64, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session {
            if session.set_frame_rate(fps) {
                self.fps = session.meta().frame_rate;
                // [`Self::apply_resolution`]'s own reason. Armed after the
                // session's own reads, since the borrow outlives nothing.
                self.mark_dirty();
                self.notify_user(format!("Project: {} fps", fps_label(fps)).into());
                self.reset_after_reseek();
            }
        } else if fps.is_finite() && fps > 0.0 {
            // Held the same way a resolution picked before any file is
            // ([`apply_resolution`](Self::apply_resolution)): there is no
            // timeline yet to cut at it.
            self.pending_settings.1 = Some(fps);
            self.notify_user(
                format!(
                    "Project: {} fps — takes effect on the next file opened",
                    fps_label(fps)
                )
                .into(),
            );
        }
        cx.notify();
    }

    /// The project's mix cut at another rate: the list names one and this is
    /// where it happens, the way [`apply_resolution`](Self::apply_resolution)
    /// is for a size. Nothing to reseek here -- unlike a resolution or a rate,
    /// this touches no picture and re-times nothing -- so the notice says the
    /// truth about when it is heard: the next time the sound is rebuilt (a
    /// seek, an edit, a reopen), not the stream already playing
    /// ([`engine::PlaybackSession::set_sample_rate`]).
    pub(crate) fn apply_sample_rate(&mut self, rate: Option<u32>, cx: &mut Context<Self>) {
        let label = |rate: Option<u32>| {
            rate.map_or("the source's own rate".to_string(), |r| format!("{r} Hz"))
        };
        if let Some(session) = &mut self.session {
            let was = session.sample_rate();
            session.set_sample_rate(rate);
            // The engine's setter takes no answer back, so the arm is gated
            // on the setting having moved: picking the rate already in force
            // is not an edit.
            let changed = session.sample_rate() != was;
            if changed {
                self.mark_dirty();
            }
            self.notify_user(
                format!(
                    "Project sound: {} — takes effect on the next seek, edit or reopen",
                    label(rate)
                )
                .into(),
            );
        } else {
            self.pending_settings.2 = rate;
            self.notify_user(
                format!(
                    "Project sound: {} — takes effect on the next file opened",
                    label(rate)
                )
                .into(),
            );
        }
        cx.notify();
    }

    /// The settings page's still image length, opened for typing in seconds.
    pub(crate) fn begin_still_num(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        self.still_num = Some(crate::ui::kit::NumberEdit::begin(&format!("{:.1}", session.still_secs())));
        cx.notify();
    }

    /// One key while the still length is typed: Enter writes it (clamped to the range a still may
    /// be), Escape drops it.
    pub(crate) fn still_num_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        use crate::player::text_field::FieldKey;
        let Some(mut edit) = self.still_num.take() else {
            return;
        };
        match edit.key(key, ctrl, alt, key_char) {
            FieldKey::Edited => self.still_num = Some(edit),
            FieldKey::Cancel => {}
            FieldKey::Commit => {
                if let Some(secs) = edit.commit(engine::project::STILL_MIN_SECS, engine::project::STILL_MAX_SECS) {
                    self.apply_still_secs(secs, cx);
                }
            }
        }
        cx.notify();
    }

    /// The project's still length set: stills placed from now on take it, and the ones already
    /// on the timeline keep theirs. One undo step, and the project reads unsaved.
    pub(crate) fn apply_still_secs(&mut self, secs: f64, cx: &mut Context<Self>) {
        let changed = match &mut self.session {
            Some(session) => session.set_still_secs(secs).then(|| session.still_secs()),
            None => None,
        };
        if let Some(now) = changed {
            self.mark_dirty();
            self.notify_user(format!("Still image length: {now:.1} s").into());
        }
        cx.notify();
    }

    /// The project's HDR media shown another way: the list names a rendition and
    /// this is where it happens, the way [`apply_resolution`](Self::apply_resolution)
    /// is for a size. The engine remaps the frame under the playhead at once
    /// ([`PlaybackSession::set_tone`]), so the picture on screen is the picked
    /// one before the notice has faded -- and an SDR project is unmoved, which
    /// is what the notice says rather than pretending something happened.
    pub(crate) fn apply_tone(&mut self, preset: Preset, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_tone(preset)
        {
            // [`Self::apply_resolution`]'s own reason.
            self.mark_dirty();
            self.notify_user(format!("HDR: {} — affects HDR media", tone_label(preset)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Opens a choice list on a setting, where it was asked for. One floating
    /// thing at a time: the click that opens it is the click that closes
    /// whatever menu it was opened from.
    pub(crate) fn open_picker(&mut self, of: Pick, at: Point<Pixels>, cx: &mut Context<Self>) {
        // On the row that is in force, so the first ↑ or ↓ steps off the
        // current value rather than off the top of the list.
        let sel = self
            .choices(of)
            .iter()
            .position(|(.., picked)| *picked)
            .unwrap_or(0);
        self.context_menu = None;
        self.library_menu = None;
        self.picker = Some(Picker { of, at, sel, floored: true, above: None });
        cx.notify();
    }

    /// [`Self::open_picker`] for a list that hangs from the foot of a control in the dock, where
    /// the picture and timeline floor has nothing to protect.
    pub(crate) fn picker_under(&mut self, of: Pick, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.open_picker(of, at, cx);
        if let Some(p) = &mut self.picker {
            p.floored = false;
        }
    }

    /// [`Self::open_picker`] for a dropdown box: the list hangs from the foot of the control that
    /// holds `click` (or its top, when there is no room below), never from the pointer.
    pub(crate) fn picker_at_control(&mut self, of: Pick, click: Point<Pixels>, cx: &mut Context<Self>) {
        match crate::ui::anchor::control_at(click) {
            Some(control) => {
                let (at, above) = crate::ui::anchor::hang(control);
                self.open_picker(of, at, cx);
                if let Some(p) = &mut self.picker {
                    p.floored = false;
                    p.above = Some(above);
                }
            }
            None => self.picker_under(of, click, cx),
        }
    }

    /// The list a menu-bar row opens beside itself, `None` while its setting has nothing to offer.
    pub(crate) fn menu_pick(&self, action: ActionId) -> Option<Pick> {
        match action {
            ActionId::Resolution => Some(Pick::Resolution),
            ActionId::ViewZoom => Some(Pick::ViewZoom),
            ActionId::OutputDisplay => Some(Pick::OutputDisplay),
            ActionId::PlaybackQuality => Some(Pick::ViewQuality),
            ActionId::GuideMask => Some(Pick::ViewMask),
            ActionId::GuideStyle => Some(Pick::ViewGuides),
            ActionId::Fit => self.fit_target().ok().map(|(lane, idx)| Pick::Fit(lane, idx)),
            ActionId::ClipLabel => self.label_target().map(|(lane, idx)| Pick::Label(lane, idx)),
            ActionId::KeyEasing => {
                let kind = self.ease_kind();
                self.key_here(kind).map(|_| Pick::KeyEase(kind))
            }
            _ => None,
        }
    }

    /// Where a list opened from the keyboard hangs: the status-bar item it is about, so it never
    /// opens at wherever the pointer was left. A list about the selected clip hangs from the
    /// clip readout, which names that clip.
    pub(crate) fn ledger_anchor(&self, of: LedgerItem) -> Point<Pixels> {
        let cells = match of {
            LedgerItem::Size | LedgerItem::Clip => &self.ledger_readout,
        };
        Some(cells.get())
            .filter(|b| b.size.width > px(0.))
            .map_or(point(px(600.), px(700.)), |b| point(b.origin.x, b.origin.y))
    }

    /// A row of the open list was picked. Closes the list first -- the rule
    /// every menu item here follows -- then does exactly what the stroke for
    /// that setting does, through the same door.
    pub(crate) fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.picker = None;
        match choice {
            Choice::Size(w, h) => self.apply_resolution(w, h, cx),
            Choice::Fps(fps) => self.apply_frame_rate(fps, cx),
            Choice::SampleRate(rate) => self.apply_sample_rate(rate, cx),
            Choice::Fit(lane, idx, fit) => self.apply_fit(lane, idx, fit, cx),
            Choice::Label(lane, idx, label) => self.apply_label(lane, idx, label, cx),
            Choice::MarkerColor(idx, color) => self.set_marker_color(idx, color, cx),
            Choice::KeyEase(kind, ease) => self.set_key_ease_here(kind, ease, cx),
            Choice::Tone(preset) => self.apply_tone(preset, cx),
            Choice::ViewZoom(zoom) => self.set_viewer_zoom(zoom, cx),
            Choice::OutputDisplay(index) => self.set_output_display(index, cx),
            Choice::ViewQuality(q) => {
                self.viewer.quality = q;
                self.viewer_changed(cx);
            }
            Choice::ViewGuides(g) => {
                self.viewer.guide = g;
                self.viewer_changed(cx);
            }
            Choice::ViewMask(m) => {
                self.viewer.mask = m;
                self.viewer_changed(cx);
            }
            // The project's, like the tone map above and unlike the palette
            // below: kept in the `.edith` and read straight back out of the
            // session by the card and by the export.
            Choice::Encoder(seat) => self.apply_encoder(seat, cx),
            // In force for the next paint -- every token is read through
            // [`ui::theme::palette`], so one store repaints the whole window --
            // and kept for the next launch. A file that could not be written is
            // said out loud: the difference between "picked" and "picked for
            // good" is the user's to know.
            Choice::Theme(id) => {
                ui::theme::set(id);
                if let Err(e) = ui::theme::save(id) {
                    let path = ui::theme::config_path();
                    self.notify_user(
                        format!("Theme could not be kept — {} — {e}", path.display()).into(),
                    );
                }
                cx.notify();
            }
            // Kept for the next launch; a file that cannot be written is said, as the theme's is.
            Choice::Autosave(secs) => {
                self.autosave_secs = secs;
                if let Err(e) = crate::player::library::save_autosave_pref(secs) {
                    self.notify_user(format!("Autosave setting could not be kept: {e}").into());
                }
                cx.notify();
            }
            Choice::Accent(a) => {
                ui::theme::set_accent(a);
                self.keep_accent();
                cx.notify();
            }
            // The same field the row's key steps, set outright: a list picks a
            // value, it does not step to one.
            Choice::AudioRate(kbps) => {
                self.audio_kbps = kbps;
                self.export_sheet.custom = true;
                cx.notify();
            }
            // The same door the Picture row's `c` steps through, so a refusal
            // is still said and the destination still follows the format.
            Choice::Picture(format) => {
                self.set_format(format);
                cx.notify();
            }
            Choice::Export(pick) => self.apply_export_pick(pick, cx),
            Choice::Bin(bin) => {
                let rows = self.picked_assets();
                self.move_assets_to_bin(rows, bin, cx)
            }
        }
    }

    /// Every value the open list offers, in the order it lists them. Empty
    /// without a timeline, which is the state where nothing here has a value to
    /// offer -- and where the surfaces that open the list are dimmed anyway.
    pub(crate) fn choices(&self, of: Pick) -> Vec<ChoiceRow> {
        // The palette is not the project's, so it is offered before the
        // timeline is asked about: an empty window is painted too, and its
        // Theme button is live there like the snap beside it.
        if let Pick::Export(row) = of {
            return self.export_choices(row);
        }
        match of {
            Pick::ViewZoom => {
                return crate::viewer_view::Zoom::ALL
                    .into_iter()
                    .map(|z| (Choice::ViewZoom(z), z.label().into(), "".into(), z == self.viewer.zoom))
                    .collect();
            }
            Pick::OutputDisplay => {
                let target = self.output_target();
                return self
                    .output_displays
                    .iter()
                    .enumerate()
                    .map(|(i, (id, name))| (Choice::OutputDisplay(i), name.clone(), "".into(), Some(*id) == target))
                    .collect();
            }
            Pick::ViewQuality => {
                return crate::viewer_view::Quality::ALL
                    .into_iter()
                    .map(|q| (Choice::ViewQuality(q), q.label().into(), q.detail().into(), q == self.viewer.quality))
                    .collect();
            }
            Pick::ViewGuides => {
                return crate::viewer_view::Guide::ALL
                    .into_iter()
                    .map(|g| (Choice::ViewGuides(g), g.label().into(), g.detail().into(), g == self.viewer.guide))
                    .collect();
            }
            Pick::ViewMask => {
                return crate::viewer_view::Mask::ALL
                    .into_iter()
                    .map(|m| (Choice::ViewMask(m), m.label().into(), m.detail().into(), m == self.viewer.mask))
                    .collect();
            }
            Pick::Bin => return self.bin_choices(),
            _ => {}
        }
        if of == Pick::Autosave {
            return crate::player::library::AUTOSAVE_CHOICES
                .into_iter()
                .map(|secs| (Choice::Autosave(secs), crate::player::library::autosave_label(secs).into(), crate::player::library::autosave_detail(secs).into(), secs == self.autosave_secs))
                .collect();
        }
        if of == Pick::Accent {
            let picked = ui::theme::accent();
            return ui::theme::AccentId::ALL
                .into_iter()
                .map(|a| (Choice::Accent(a), a.label().into(), "".into(), a == picked))
                .collect();
        }
        if of == Pick::Theme {
            return ui::theme::PaletteId::ALL
                .into_iter()
                .map(|id| {
                    (
                        Choice::Theme(id),
                        id.label().into(),
                        id.detail().into(),
                        id == ui::theme::active(),
                    )
                })
                .collect();
        }
        // The file an export is written as is this computer's pick, not the
        // project's, so the list is offered before a timeline is asked about
        // -- a refusal rides each row it applies to, and is empty without a
        // session to refuse anything.
        if of == Pick::Picture {
            return crate::ui::settings_stance::picture_choices(self.format, |f| {
                self.session
                    .as_ref()
                    .and_then(|session| format_refusal(session, f))
            });
        }
        let Some(session) = &self.session else {
            // Resolution and rate are reachable before any file is open --
            // they hold as `pending_settings` until one is ([`apply_resolution`],
            // [`apply_frame_rate`]) -- every other row still needs a session to
            // have anything to offer.
            return match of {
                Pick::Resolution => pending_resolution_choices(self.pending_settings.0),
                Pick::Fps => pending_fps_choices(self.pending_settings.1),
                Pick::SampleRate => sample_rate_choices(self.pending_settings.2),
                _ => Vec::new(),
            };
        };
        match of {
            Pick::Resolution => {
                resolution_choices(session.resolution(), session.native_resolution())
            }
            Pick::Fps => fps_choices(session.meta().frame_rate, session.native_frame_rate()),
            Pick::SampleRate => sample_rate_choices(session.sample_rate()),
            Pick::Fit(lane, idx) => {
                fit_choices(lane, idx, session.fit_of(lane, idx), session.resolution())
            }
            Pick::AudioRate => audio_rate_choices(self.audio_kbps),
            Pick::Tone => tone_choices(session.tone()),
            Pick::Encoder => encoder_choices(session.encoder_seat()),
            // Answered above, with or without a timeline.
            Pick::KeyEase(kind) => {
                let here = self.key_here(kind);
                engine::transform::KeyEase::ALL
                    .into_iter()
                    .map(|ease| (Choice::KeyEase(kind, ease), crate::export_ui::ease_label(ease).into(), "".into(), here == Some(ease)))
                    .collect()
            }
            Pick::Label(lane, idx) => {
                let here = session.mark_of(lane, idx).label;
                std::iter::once((Choice::Label(lane, idx, 0), "None".into(), "".into(), here == 0))
                    .chain(engine::project::LABELS.iter().enumerate().map(|(i, (name, _))| {
                        let n = i as u8 + 1;
                        (Choice::Label(lane, idx, n), (*name).into(), "".into(), here == n)
                    }))
                    .collect()
            }
            Pick::MarkerColor(idx) => {
                let here = session.markers().get(idx).map(|m| m.color);
                engine::project::MarkerColor::ALL
                    .into_iter()
                    .map(|c| {
                        let name = c.name();
                        let label = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
                        (Choice::MarkerColor(idx, c), label.into(), "".into(), here == Some(c))
                    })
                    .collect()
            }
            Pick::Theme | Pick::Accent | Pick::Picture | Pick::Export(_) | Pick::ViewZoom | Pick::OutputDisplay | Pick::ViewQuality | Pick::ViewMask | Pick::ViewGuides | Pick::Bin | Pick::Autosave => Vec::new(),
        }
    }

    /// Opens the colour card on the clip a grade would go on: the clip that was
    /// clicked when it is a video one, and otherwise the clip the picture is
    /// coming from -- the one the engine's own compositing rule picks, which is
    /// what a person means by "this shot". The fallback stands even now that a
    /// selection key exists: a grade asked for with nothing selected still means
    /// the shot on screen.
    pub(crate) fn open_color(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("Nothing to grade — no file open".into());
            cx.notify();
            return;
        };
        // A caption the hand is on names the clip it was pinned to, and the
        // card opens on that; one in no group has no picture to grade and it
        // says so, rather than falling to the playhead's clip behind the
        // hand's back.
        let target = match self.selected.anchor() {
            // A caption the hand is on names the clip it was pinned to, and the
            // grade is that clip's; one in no group has no picture to grade
            // and it says so, rather than grading the playhead's clip behind
            // the hand's back.
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                match caption_media_half(session, (lane, idx), LaneKind::Video) {
                    Some(half) => Some(half),
                    None => {
                        self.notify_user("Nothing to grade — a caption has no picture".into());
                        cx.notify();
                        return;
                    }
                }
            }
            // A sound half has no picture of its own: the playhead's clip, as
            // it always was.
            Some((lane, _)) if lane.kind == LaneKind::Audio => session.video_clip_at(session.now()),
            other => other.or_else(|| session.video_clip_at(session.now())),
        };
        match target {
            Some(clip) => {
                // One card at a time: `close_card` is the single list of
                // every flag that means "a card is up", so opening this one
                // starts by closing all of them.
                self.close_other_cards();
                self.color_open = Some(clip);
                self.color_band = section_bands(self.color_section).start;
                self.color_dragging = false;
                self.color_wheel_drag = None;
                // A sample the last card held back belongs to the clip it was
                // dragged on, and this may be another one.
                self.pending_color = None;
                self.context_menu = None;
            }
            None => self.notify_user("Nothing to grade — no clip under the playhead".into()),
        }
        cx.notify();
    }

    /// What the card's clip is graded by right now -- the identity for one
    /// nobody has graded, which is what the sliders start at. A sample a drag is
    /// still holding wins over the clip's own: it is what the hand has asked
    /// for, so it is what the sliders show and what the next sample builds on.
    pub(crate) fn color_params(&self) -> ColorParams {
        if let Some(params) = self.pending_color {
            return params;
        }
        let frame = self.key_frame();
        self.color_open
            .zip(self.session.as_ref())
            .and_then(|((lane, idx), session)| session.color_at(lane, idx, frame))
            .unwrap_or_default()
    }

    /// Puts `params` on the card's clip, or takes the grade off when they are
    /// the identity -- a slider walked back to the middle leaves the clip
    /// ungraded rather than carrying a do-nothing entry, which is what keeps an
    /// untouched project byte-identical. The engine reseeks on the edit, so the
    /// frame on screen repaints through the new grade; this only owes the flags
    /// that reseek clears.
    pub(crate) fn set_color(&mut self, params: ColorParams, cx: &mut Context<Self>) {
        self.write_color(params, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through
    /// (`PlaybackSession::set_color_live`). Either way the engine reseeks, so
    /// the picture -- and the histogram counted off it -- is regraded at once.
    pub(crate) fn write_color(&mut self, params: ColorParams, live: bool, cx: &mut Context<Self>) {
        self.named_step("Colour", |this| this.write_color_now(params, live, cx));
    }

    pub(crate) fn write_color_now(&mut self, params: ColorParams, live: bool, cx: &mut Context<Self>) {
        // Any write supersedes a held sample, whichever way it arrived -- a key,
        // a reset, or the flush that took this one out of the stash.
        self.pending_color = None;
        let frame = self.key_frame();
        let Some((lane, idx)) = self.color_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        // A LUT is kept even at intensity 0: the file is the hand's choice, and
        // the next pull of the bar is on it.
        let grade = Some(params).filter(|p| !p.is_identity() || p.look.lut.is_some());
        let keyed = session.is_keyed(lane, idx, KeyKind::Color);
        let base = session.color_of(lane, idx).copied().unwrap_or_default();
        let took = if keyed && base.look != params.look {
            // The look holds under the keys: it is the clip's own grade's,
            // written there whatever the keys say about the sliders.
            let base = Some(ColorParams { look: params.look, ..base })
                .filter(|p| !p.is_identity() || p.look.lut.is_some());
            match live {
                true => session.set_color_live(lane, idx, base),
                false => session.set_color(lane, idx, base),
            }
        } else if keyed {
            // A keyed grade: the slider writes the key on the playhead.
            session.put_color_key_at(lane, idx, frame, params, !live)
        } else {
            match live {
                true => session.set_color_live(lane, idx, grade),
                false => session.set_color(lane, idx, grade),
            }
        };
        if took {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Shows another section of the colour card; the arrows then walk its sliders.
    pub(crate) fn show_color_section(&mut self, section: ColorSection, cx: &mut Context<Self>) {
        self.color_section = section;
        self.color_band = section_bands(section).start;
        cx.notify();
    }

    /// What the graph over the colour controls shows. A scope is counted off the
    /// next frame; the one on screen is counted now, so the switch shows at once.
    pub(crate) fn show_scope(&mut self, view: ScopeView, cx: &mut Context<Self>) {
        self.scope_view = view;
        self.scopes = None;
        if view != ScopeView::Histogram
            && let Some(image) = &self.image
            && let Some(frame) = image.as_bytes(0)
        {
            let size = image.size(0);
            let (w, h) = (size.width.0 as usize, size.height.0 as usize);
            self.scopes = Some(engine::scopes::compute(frame, w, h, SCOPE_COLS));
        }
        cx.notify();
    }

    /// Asks for a `.cube` file and puts it on the card's clip at full intensity,
    /// or -- `relink` -- points every clip using the clip's current LUT at it.
    /// The chooser runs off the UI thread; a file that cannot be read is said so
    /// and changes nothing.
    pub(crate) fn pick_lut(&mut self, relink: bool, cx: &mut Context<Self>) {
        let picked = cx.background_executor().spawn(async { pick_file("edith — LUT (.cube)") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(path)) => this.set_lut(&path, relink, cx),
                Ok(None) => {}
                // No chooser here: the same question, typed.
                Err(_) => this.open_path_entry(PathFor::Lut { relink }, cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn set_lut(&mut self, path: &Path, relink: bool, cx: &mut Context<Self>) {
        let r = engine::cube::LutRef::reload(path);
        if let Some(why) = r.problem() {
            self.notify_user(format!("That LUT cannot be used: {why}").into());
            cx.notify();
            return;
        }
        let mut params = self.color_params();
        let old = params.look.lut;
        if relink && let (Some(old), Some(session)) = (old, &mut self.session) {
            let n = session.relink_lut(&old.path(), path);
            if n > 0 {
                self.mark_dirty();
                self.reset_after_reseek();
                self.notify_user(format!("Relinked the LUT on {}", count_noun(n, "clip", "clips")).into());
            }
            cx.notify();
            return;
        }
        params.look.lut = Some(r);
        params.look.lut_mix = 1.;
        self.set_color(params, cx);
    }

    /// Takes the LUT off the card's clip; the rest of its grade stays.
    pub(crate) fn clear_lut(&mut self, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        params.look.lut = None;
        params.look.lut_mix = 1.;
        self.set_color(params, cx);
    }

    /// Moves the picked slider by `steps` of [`COLOR_STEP`], clamped to that
    /// band's range. One edit, so one undo step per press.
    pub(crate) fn nudge_color(&mut self, steps: f32, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        let (band, wheel, curve) = (self.color_band, self.color_wheel, self.color_curve);
        let (_, low, high, step) = band_spec(band);
        let value = (band_value(&params, band, wheel, curve) + steps * step).clamp(low, high);
        set_band(&mut params, band, wheel, curve, value);
        self.set_color(params, cx);
    }

    /// Where the pointer sits along a slider, as that band's value: the left end
    /// of the bar is the bottom of its range and the right end the top. Called
    /// on every pointer sample, so the grade -- and the picture, and the
    /// histogram over it -- moves under the hand.
    ///
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live. That is why it writes even when
    /// the value did not change -- without that snapshot the rest of the drag
    /// would be unundoable.
    ///
    /// Values land on the [`COLOR_STEP`] grid the keys use, which also bounds
    /// one drag to forty-odd entries in the project's colour table.
    ///
    /// Samples crossed while the worker still owes a frame are held rather than
    /// written ([`stash_or_write`]): a reopen costs half a second on a big film,
    /// so a bar-wide sweep that wrote every step would queue forty opens, cancel
    /// thirty-nine of them and freeze the window for the sum. What is written is
    /// one grade per frame the worker actually delivers.
    pub(crate) fn drag_color(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let (band, wheel, curve) = (self.color_band, self.color_wheel, self.color_curve);
        let (_, low, high, step) = band_spec(band);
        let along = frac_along(x, self.color_bars[band].get());
        // The basic bands land on the keys' grid; the wheels and curves on a
        // finer one of their own, since a hundredth of a lift already shows.
        let raw = low + along * (high - low);
        let value = if band < WHEEL_BAND { color_snap(raw) } else { (raw / step).round() * step };
        let value = value.clamp(low, high);
        let mut params = self.color_params();
        if band_value(&params, band, wheel, curve) == value && !first {
            return;
        }
        set_band(&mut params, band, wheel, curve, value);
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_color, params, first, busy) {
            Some(params) => self.write_color(params, !first, cx),
            // The sliders draw off the held sample, so the handle goes on
            // following the hand while the picture catches up.
            None => cx.notify(),
        }
    }

    /// One pointer sample on a colour wheel: the puck (`master` false) sets the
    /// wheel's R, G and B offsets from its place on the pad, the bar under it sets
    /// the master. `first` is the press, which takes the undo step.
    pub(crate) fn drag_wheel(&mut self, wheel: usize, master: bool, at: gpui::Point<Pixels>, first: bool, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        self.color_wheel = wheel;
        if master {
            let along = frac_along(at.x, self.color_masters[wheel].get());
            let v = wheel_snap(along * 2. - 1.);
            set_band(&mut params, WHEEL_BAND, wheel, self.color_curve, v);
            self.color_band = WHEEL_BAND;
        } else {
            let b = self.color_pads[wheel].get();
            let (w, h) = (f32::from(b.size.width).max(1.), f32::from(b.size.height).max(1.));
            let x = (f32::from(at.x - b.origin.x) - w / 2.) / (w / 2.);
            let y = (h / 2. - f32::from(at.y - b.origin.y)) / (h / 2.);
            for (i, v) in wheel_offsets(x, y).into_iter().enumerate() {
                set_band(&mut params, WHEEL_BAND + 1 + i, wheel, self.color_curve, wheel_snap(v));
            }
            self.color_band = WHEEL_BAND + 1;
        }
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_color, params, first, busy) {
            Some(params) => self.write_color(params, !first, cx),
            None => cx.notify(),
        }
    }

    /// Double-click on a wheel: its puck back to the centre (`master` false) or
    /// its master back to zero.
    pub(crate) fn reset_wheel(&mut self, wheel: usize, master: bool, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        let bands = if master { WHEEL_BAND..WHEEL_BAND + 1 } else { WHEEL_BAND + 1..CURVE_BAND };
        for band in bands {
            set_band(&mut params, band, wheel, self.color_curve, 0.);
        }
        self.set_color(params, cx);
    }

    /// Opens the transform card on the clip a placement would go on --
    /// [`open_color`](Self::open_color)'s own rule, since it targets the same
    /// clip a grade would.
    pub(crate) fn open_transform(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("Nothing to place — no file open".into());
            cx.notify();
            return;
        };
        let target = match self.selected.anchor() {
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                match caption_media_half(session, (lane, idx), LaneKind::Video) {
                    Some(half) => Some(half),
                    None => {
                        self.notify_user("Nothing to place — a caption has no picture".into());
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
                self.transform_open = Some(clip);
                self.transform_band = 0;
                self.transform_dragging = false;
                self.pending_transform = None;
                self.place_open = false;
                self.title_in_open = false;
                self.title_out_open = false;
                self.context_menu = None;
            }
            None => self.notify_user("Nothing to place — no clip under the playhead".into()),
        }
        cx.notify();
    }

    /// What the card's clip is placed by right now -- [`color_params`](Self::color_params)'s
    /// own rule.
    pub(crate) fn transform_params(&self) -> TransformParams {
        if let Some(params) = self.pending_transform {
            return params;
        }
        let frame = self.key_frame();
        self.transform_open
            .zip(self.session.as_ref())
            .map(|((lane, idx), session)| {
                let mut params = session.transform_at(lane, idx, frame).unwrap_or_default();
                if session.is_keyed(lane, idx, KeyKind::Opacity) {
                    params.opacity = session.opacity_at(lane, idx, frame);
                }
                params
            })
            .unwrap_or_default()
    }

    /// Puts `params` on the card's clip, or takes the placement off when they
    /// are the identity -- [`set_color`](Self::set_color)'s own rule.
    pub(crate) fn set_transform(&mut self, params: TransformParams, cx: &mut Context<Self>) {
        self.write_transform(params, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through --
    /// [`write_color`](Self::write_color)'s own rule.
    pub(crate) fn write_transform(&mut self, params: TransformParams, live: bool, cx: &mut Context<Self>) {
        self.named_step("Transform", |this| this.write_transform_now(params, live, cx));
    }

    fn write_transform_now(&mut self, params: TransformParams, live: bool, cx: &mut Context<Self>) {
        self.pending_transform = None;
        let frame = self.key_frame();
        let Some((lane, idx)) = self.transform_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let mut params = params;
        let mut took = false;
        if session.is_keyed(lane, idx, KeyKind::Opacity) {
            // Keyed opacity: the slider writes its key, and the placement keeps
            // the opacity it had.
            if session.opacity_at(lane, idx, frame) != params.opacity {
                took = session.put_opacity_key_at(lane, idx, frame, params.opacity, !live);
            }
            params.opacity = session
                .transform_at(lane, idx, frame)
                .map_or(1.0, |t| t.opacity);
        }
        if session.is_keyed(lane, idx, KeyKind::Placement) {
            if session.transform_at(lane, idx, frame) != Some(params) {
                took |= session.put_transform_key_at(lane, idx, frame, params, !live && !took);
            }
        } else {
            let placement = Some(params).filter(|p| !p.is_identity());
            if session.transform_at(lane, idx, frame) != placement || !took {
                took |= match live {
                    true => session.set_transform_live(lane, idx, placement),
                    false => session.set_transform(lane, idx, placement),
                };
            }
        }
        if took {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The canvas the placement is measured on: position is a fraction of it,
    /// so the pixel readout and the pixel drag both need its size. The card
    /// only opens with a file, so the project default stands in for nothing.
    pub(crate) fn transform_canvas(&self) -> (u32, u32) {
        self.session
            .as_ref()
            .map_or((1920, 1080), |session| session.resolution())
    }

    /// The pixel size of the picture the card's clip shows, as the engine
    /// measures it: a video's from its probed facts, a still's from its header.
    /// `None` for sound, a generated clip, or a video whose facts are still
    /// being read.
    pub(crate) fn clip_picture_size(&self, lane: Lane, idx: usize) -> Option<(u32, u32)> {
        let session = self.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx)?;
        let path = &session.sources().get(clip.source)?.path;
        if engine::is_image(path) {
            return engine::decode::image_size(path).ok();
        }
        let video = self.facts.get(path)?.as_ref()?.video.as_ref()?;
        Some((video.width, video.height))
    }

    /// Puts the card's clip in `corner`, at `scale` when one is given. Without
    /// one, a corner pick on a clip still at full size makes it a picture in
    /// picture ([`PIP_SCALE`]), so the picture is smaller than the frame it sits
    /// in. Keeps the default corner margin. One undo step, as every placement is
    /// ([`set_transform`](Self::set_transform)).
    pub(crate) fn place_in_corner(&mut self, corner: engine::transform::Corner, scale: Option<f32>, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.transform_open else {
            return;
        };
        let Some(size) = self.clip_picture_size(lane, idx) else {
            return;
        };
        let mut params = self.transform_params();
        if let Some(scale) = scale {
            params.scale = scale;
        } else if corner != engine::transform::Corner::Centre && params.scale == TransformParams::default().scale {
            params.scale = PIP_SCALE;
        }
        let canvas = self.transform_canvas();
        let (x, y) = engine::transform::corner_placement(corner, &params, size, canvas, engine::transform::CORNER_MARGIN_PX);
        params.pos_x = x;
        params.pos_y = y;
        self.set_transform(params, cx);
    }

    /// Puts the picture at `scale` as a scale change: a picture in a corner stays
    /// in that corner ([`engine::transform::keep_corner`]), at the card's clip's
    /// own picture size. `old` is what the picture was before the change.
    pub(crate) fn scaled_in_place(&self, old: TransformParams, new: TransformParams) -> TransformParams {
        let Some((lane, idx)) = self.transform_open else {
            return new;
        };
        let (Some(size), canvas) = (self.clip_picture_size(lane, idx), self.transform_canvas()) else {
            return new;
        };
        engine::transform::keep_corner(&old, new, size, canvas, engine::transform::CORNER_MARGIN_PX)
    }

    /// Whether the crop rows are unfolded.
    pub(crate) fn transform_crop_open(&self) -> bool {
        self.folds.contains(TRANSFORM_CROP_FOLD)
    }

    /// Opens or closes the crop rows and remembers it. Closing them moves a
    /// picked crop row back to the first one, so the arrows never move a slider
    /// nobody can see.
    pub(crate) fn toggle_transform_crop(&mut self, cx: &mut Context<Self>) {
        if self.folds.contains(TRANSFORM_CROP_FOLD) {
            self.folds.remove(TRANSFORM_CROP_FOLD);
            if self.transform_band >= TRANSFORM_MAIN_BANDS {
                self.transform_band = 0;
            }
        } else {
            self.folds.insert(TRANSFORM_CROP_FOLD.to_owned());
        }
        save_folds(&self.folds);
        cx.notify();
    }

    /// Moves the picked row one down or up, skipping the crop rows while their
    /// fold is shut.
    pub(crate) fn move_transform_band(&mut self, down: bool) {
        let n = if self.transform_crop_open() {
            TRANSFORM_BANDS.len()
        } else {
            TRANSFORM_MAIN_BANDS
        };
        if self.transform_band >= n {
            self.transform_band = 0;
        }
        self.transform_band = if down {
            (self.transform_band + 1) % n
        } else {
            (self.transform_band + n - 1) % n
        };
    }

    /// Moves the picked slider by `steps` -- [`nudge_color`](Self::nudge_color)'s
    /// own rule, except rotation wraps into its range on [`transform_snap`]
    /// instead of clamping, and a position moves in whole canvas pixels.
    pub(crate) fn nudge_transform(&mut self, steps: f32, cx: &mut Context<Self>) {
        let mut params = self.transform_params();
        let band = self.transform_band;
        let (_, low, high) = TRANSFORM_BANDS[band];
        let canvas = self.transform_canvas();
        let old = params;
        let value = transform_band_mut(&mut params, band);
        *value = if band == ROTATE_BAND {
            transform_snap(band, *value + steps * ROTATE_STEP)
        } else if is_position_band(band) {
            let raw = *value + steps * POSITION_NUDGE_PX / position_span(band, canvas);
            snap_position_px(band, raw, canvas)
        } else {
            (*value + steps * TRANSFORM_STEP).clamp(low, high)
        };
        if band == SCALE_BAND {
            params = self.scaled_in_place(old, params);
        }
        self.set_transform(params, cx);
    }

    /// Where the pointer sits along a slider, as that band's value --
    /// [`drag_color`](Self::drag_color)'s own rule, on the pixel grid for a
    /// position.
    pub(crate) fn drag_transform(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        if self.opacity_drag {
            self.drag_opacity(x, first, cx);
            return;
        }
        let band = self.transform_band;
        let (low, high) = slider_range(band);
        let along = frac_along(x, self.transform_bars[band].get());
        let raw = low + along * (high - low);
        let value = if is_position_band(band) {
            snap_position_px(band, raw, self.transform_canvas())
        } else {
            transform_snap(band, raw)
        };
        let old = self.transform_params();
        let mut params = old;
        let at = transform_band_mut(&mut params, band);
        if *at == value && !first {
            return;
        }
        *at = value;
        if band == SCALE_BAND {
            params = self.scaled_in_place(old, params);
        }
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_transform, params, first, busy) {
            Some(params) => self.write_transform(params, !first, cx),
            None => cx.notify(),
        }
    }

    /// Where the pointer sits along the opacity bar, as the clip's opacity, on a
    /// whole percent. The row shares the transform card's drag flag, so the root
    /// hands its samples here while [`Self::opacity_drag`] is set.
    pub(crate) fn drag_opacity(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let value = (frac_along(x, self.opacity_bar.get()) * 100.).round() / 100.;
        let mut params = self.transform_params();
        if params.opacity == value && !first {
            return;
        }
        params.opacity = value;
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_transform, params, first, busy) {
            Some(params) => self.write_transform(params, !first, cx),
            None => cx.notify(),
        }
    }

    /// The clip a speed action acts on: the selected one, or the clip the picture
    /// is coming from. A caption names the clip it was pinned to, and a caption in
    /// no group has no rate -- said out loud, since a card that reads 1.00x and
    /// writes nothing is worse than a refusal. `None` when there is nothing to act on,
    /// and the refusal has been said.
    pub(crate) fn speed_anchor(&mut self, cx: &mut Context<Self>) -> Option<(Lane, usize)> {
        let Some(session) = self.session.as_ref() else {
            self.notify_user("Nothing to re-time — no file open".into());
            cx.notify();
            return None;
        };
        let anchor = match self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()))
        {
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                match caption_media_half(session, (lane, idx), LaneKind::Video) {
                    Some(half) => Some(half),
                    None => {
                        self.notify_user(
                            "Nothing to re-time — a caption has no speed of its own".into(),
                        );
                        cx.notify();
                        None
                    }
                }
            }
            other => other,
        };
        if anchor.is_none() {
            self.notify_user("Nothing to re-time — no clip under the playhead".into());
            cx.notify();
        }
        anchor
    }

    /// Opens the speed card on the clip whose rate is to change: the selected
    /// one, or -- with nothing selected -- the clip the picture is coming from,
    /// which is what a person means by "this shot". Either half of a take will
    /// do: a rate applies to the whole group, so opening it on the sound and
    /// opening it on the picture are the same card.
    pub(crate) fn open_speed(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let anchor = self.speed_anchor(cx);
        match anchor {
            Some(clip) => {
                // One card at a time -- `close_card`'s own rule.
                self.close_other_cards();
                self.speed_open = Some(clip);
                self.speed_dragging = false;
                // The colour card's rule: a held sample is the last clip's.
                self.pending_speed = None;
                self.ramp_key_speed = None;
                // A clip that already ramps opens on its keys.
                self.speed_ramp_mode = self
                    .session
                    .as_ref()
                    .and_then(|s| s.lane_clips(clip.0).get(clip.1))
                    .is_some_and(|c| !c.speed_keys.is_empty());
                self.context_menu = None;
            }
            None => return,
        }
        cx.notify();
    }

    /// What the card's clip plays at right now -- real time for one nobody has
    /// touched, which is where the bar starts.
    pub(crate) fn card_speed(&self) -> Speed {
        if let Some(speed) = self.pending_speed {
            return speed;
        }
        // The ramp mode's bar is the next key's speed, not the clip's.
        if self.speed_ramp_mode {
            return self.ramp_key_speed.unwrap_or_else(|| self.ramp_default_speed());
        }
        self.speed_open
            .zip(self.session.as_ref())
            .map_or(Speed::NORMAL, |((lane, idx), session)| {
                session.speed_of(lane, idx)
            })
    }

    /// Writes a rate at the card's clip and its whole group -- one undo step for
    /// the lot ([`engine::PlaybackSession::set_speed`]). The engine reseeks, so
    /// the picture runs at the new rate and the sound is time-stretched from the
    /// next chunk on, pitch kept; a refusal (a slower clip would run into its neighbour) comes
    /// back in the engine's own words and *names* the clip in the way, because
    /// "it did not fit" is not something a person can go and fix.
    pub(crate) fn set_speed(&mut self, speed: Speed, cx: &mut Context<Self>) {
        self.write_speed(speed, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through -- so a drag from 1.00x to
    /// 2.00x is one undo press and lands back where the hand picked it up, and
    /// the whole linked group comes back with it.
    pub(crate) fn write_speed(&mut self, speed: Speed, live: bool, cx: &mut Context<Self>) {
        self.named_step("Speed", |this| this.write_speed_now(speed, live, cx));
    }

    fn write_speed_now(&mut self, speed: Speed, live: bool, cx: &mut Context<Self>) {
        // The colour card's rule: a write supersedes whatever a drag was holding.
        self.pending_speed = None;
        if self.speed_ramp_mode {
            // Nothing is written until a key is added: the bar picks its speed.
            self.ramp_key_speed = Some(speed);
            cx.notify();
            return;
        }
        let Some((lane, idx)) = self.speed_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        if speed != session.speed_of(lane, idx) {
            let wrote = match live {
                true => session.set_speed_live(lane, idx, speed),
                false => session.set_speed(lane, idx, speed),
            };
            match wrote {
                Ok(()) => {
                    self.mark_dirty();
                    self.reset_after_reseek();
                }
                Err(e) => self.notify_user(e.to_string().into()),
            }
        }
        cx.notify();
    }

    /// Reverses the clip the speed card is open on, or the one the menu and the
    /// palette mean (the selection, else the picture under the playhead --
    /// `open_speed`'s rule), and its whole group: one undo step. Said once on
    /// the toast so the toggle has an answer when the card is closed.
    pub(crate) fn toggle_reverse(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.notify_user("Nothing to reverse — no file open".into());
            cx.notify();
            return;
        };
        let anchor = self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()))
            .filter(|(lane, _)| lane.kind != LaneKind::Subtitle);
        match anchor {
            Some((lane, idx)) => {
                let on = !session.reversed_of(lane, idx);
                self.write_reverse(lane, idx, on, cx);
            }
            None => {
                self.notify_user("Nothing to reverse — select a clip".into());
                cx.notify();
            }
        }
    }

    /// Writes the direction at `(lane, idx)` and its group
    /// ([`engine::PlaybackSession::set_reverse`]); the engine reseeks, so the
    /// picture and the sound run the new way from the next frame.
    pub(crate) fn write_reverse(&mut self, lane: Lane, idx: usize, on: bool, cx: &mut Context<Self>) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.set_reverse(lane, idx, on) {
            Ok(()) => {
                self.mark_dirty();
                self.reset_after_reseek();
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// The clip a ramp edit acts on: the open card's, else the speed actions' own pick.
    fn ramp_target(&mut self, cx: &mut Context<Self>) -> Option<(Lane, usize)> {
        match self.speed_open {
            Some(clip) => Some(clip),
            None => self.speed_anchor(cx),
        }
    }

    /// The playhead's frame counted from the start of the clip at `clip`, when the
    /// playhead is on it.
    fn ramp_playhead(&self, clip: (Lane, usize)) -> Option<u32> {
        let session = self.session.as_ref()?;
        let c = session.lane_clips(clip.0).get(clip.1)?;
        let at = crate::subs::frame_at(session.now(), self.fps);
        (at >= c.start && at < c.end()).then(|| at - c.start)
    }

    /// The speed the ramp runs at the playhead on the card's clip (or the clip's
    /// constant one without a ramp): what the bar holds for the next key until a
    /// hand moves it.
    pub(crate) fn ramp_default_speed(&self) -> Speed {
        let Some(clip) = self.speed_open else {
            return Speed::NORMAL;
        };
        let Some(c) = self
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(clip.0).get(clip.1))
        else {
            return Speed::NORMAL;
        };
        match self.ramp_playhead(clip) {
            Some(rel) if !c.speed_keys.is_empty() => {
                engine::project::ramp_speed_at(&c.speed_keys, rel)
            }
            _ => c.speed,
        }
    }

    /// Writes `keys` as the ramp of `clip` and its group -- one undo step -- and
    /// reseeks. Empty keys clear the ramp. A refusal (the longer clip would run into
    /// its neighbour) comes back in the engine's words, as the constant rate's does.
    fn write_ramp(&mut self, clip: (Lane, usize), keys: &[engine::project::SpeedKey], cx: &mut Context<Self>) {
        let Some(session) = &mut self.session else {
            return;
        };
        let before = session.lane_clips(clip.0).get(clip.1).map(|c| c.frames());
        match session.set_speed_ramp(clip.0, clip.1, keys) {
            Ok(()) => {
                let after = self.session.as_ref().and_then(|s| s.lane_clips(clip.0).get(clip.1)).map(|c| c.frames());
                self.mark_dirty();
                self.reset_after_reseek();
                if self.speed_open == Some(clip) {
                    self.speed_ramp_mode = !keys.is_empty() || self.speed_ramp_mode;
                    self.ramp_key_speed = None;
                }
                // A ramp that runs shorter leaves its space empty: nothing ripples, so say so.
                if let (Some(before), Some(after)) = (before, after)
                    && after < before
                {
                    let secs = f64::from(after) / self.fps.max(1.);
                    self.notify_user(
                        format!("Shortened to {secs:.1} s. The space after it is empty: Close gap removes it.").into(),
                    );
                }
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Lays a ramp shape across the target clip's length on the timeline.
    pub(crate) fn set_ramp_preset(&mut self, preset: RampPreset, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(clip) = self.ramp_target(cx) else {
            return;
        };
        let frames = self
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(clip.0).get(clip.1))
            .map_or(0, Clip::frames);
        match preset.keys(frames) {
            Some(keys) => {
                self.write_ramp(clip, &keys, cx);
                if self.speed_open == Some(clip) {
                    self.speed_ramp_mode = true;
                }
            }
            None => {
                self.notify_user("This clip is too short for that speed ramp".into());
                cx.notify();
            }
        }
    }

    /// Puts a key at the playhead on the target clip: the bar's speed when the card
    /// is up, else the speed the ramp already runs there. The first key on a clip with
    /// no ramp is joined by one at its start holding the constant speed, so the ramp
    /// eases into the key instead of being a constant at it.
    pub(crate) fn add_ramp_key(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(clip) = self.ramp_target(cx) else {
            return;
        };
        let Some(rel) = self.ramp_playhead(clip) else {
            self.notify_user("Put the playhead on the clip to key its speed there".into());
            cx.notify();
            return;
        };
        let Some(c) = self
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(clip.0).get(clip.1))
            .cloned()
        else {
            return;
        };
        let speed = match self.speed_open == Some(clip) && self.speed_ramp_mode {
            true => self.card_speed(),
            false => match c.speed_keys.is_empty() {
                true => c.speed,
                false => engine::project::ramp_speed_at(&c.speed_keys, rel),
            },
        };
        let base = match c.speed_keys.is_empty() && rel > 0 {
            true => vec![engine::project::SpeedKey { at: 0, speed: c.speed }],
            false => c.speed_keys.clone(),
        };
        match ramp_with_key(&base, rel, speed) {
            Some(keys) => self.write_ramp(clip, &keys, cx),
            None => {
                self.notify_user(
                    format!("A speed ramp holds at most {} keys", engine::project::MAX_SPEED_KEYS)
                        .into(),
                );
                cx.notify();
            }
        }
    }

    /// Takes the `i`th key off the card's ramp.
    pub(crate) fn remove_ramp_key(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(clip) = self.speed_open else {
            return;
        };
        let Some(c) = self
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(clip.0).get(clip.1))
        else {
            return;
        };
        let keys = ramp_without_key(&c.speed_keys, i);
        self.write_ramp(clip, &keys, cx);
    }

    /// Moves the `i`th key's speed by `steps` quarters.
    pub(crate) fn step_ramp_key(&mut self, i: usize, steps: i32, cx: &mut Context<Self>) {
        let Some(clip) = self.speed_open else {
            return;
        };
        let Some(c) = self
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(clip.0).get(clip.1))
        else {
            return;
        };
        let keys = ramp_key_stepped(&c.speed_keys, i, steps);
        self.write_ramp(clip, &keys, cx);
    }

    /// Takes the target clip's speed ramp off: its constant speed comes back.
    pub(crate) fn clear_ramp(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(clip) = self.ramp_target(cx) else {
            return;
        };
        let had = self
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(clip.0).get(clip.1))
            .is_some_and(|c| !c.speed_keys.is_empty());
        if had {
            self.write_ramp(clip, &[], cx);
        }
        if self.speed_open == Some(clip) {
            self.speed_ramp_mode = false;
            self.ramp_key_speed = None;
        }
        cx.notify();
    }

    /// Starts an auto reframe of the clip: its subject is found on a thread of its
    /// own ([`engine::reframe::analyse`]), shown as a percent in the status bar, and
    /// [`Self::poll_reframe`] writes the keys -- one undo step -- when it lands.
    pub(crate) fn start_reframe(&mut self, lane: Lane, idx: usize, cx: &mut Context<Self>) {
        if self.reframe_run.is_some() {
            self.notify_user("Auto reframe is already measuring a clip".into());
            cx.notify();
            return;
        }
        let Some(job) = self.session.as_ref().and_then(|s| s.reframe_job(lane, idx)) else {
            self.notify_user("Auto reframe: select a video clip".into());
            cx.notify();
            return;
        };
        let run = ReframeRun {
            lane,
            idx,
            job: job.clone(),
            progress: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
        };
        let (progress, cancel, result) = (run.progress.clone(), run.cancel.clone(), run.result.clone());
        std::thread::spawn(move || {
            let r = engine::reframe::analyse(&job.path, job.lo, job.hi, &progress, &cancel).map_err(|e| e.to_string());
            if let Ok(mut slot) = result.lock() {
                *slot = Some(r);
            }
        });
        self.reframe_run = Some(run);
        cx.notify();
    }

    /// Auto reframe of the selected clip ([`ActionId::Reframe`]).
    pub(crate) fn reframe_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected.anchor() {
            Some((lane, idx)) => self.start_reframe(lane, idx, cx),
            None => {
                self.notify_user("Auto reframe: select a video clip".into());
                cx.notify();
            }
        }
    }

    /// Stops the running reframe measurement.
    pub(crate) fn cancel_reframe(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.reframe_run {
            run.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// The status bar's words for a running reframe, `Reframing 42%`.
    pub(crate) fn reframe_words(&self) -> Option<String> {
        let run = self.reframe_run.as_ref()?;
        Some(format!("Reframing {}%", run.progress.load(Ordering::Relaxed) / 10))
    }

    /// Lands a finished reframe: the keys are written if the clip is still the one
    /// that was measured, and a failure or a stop is said once.
    pub(crate) fn poll_reframe(&mut self) {
        let Some(run) = &self.reframe_run else { return };
        let Some(outcome) = run.result.lock().ok().and_then(|mut r| r.take()) else { return };
        let run = self.reframe_run.take().expect("checked above");
        let said = match outcome {
            Err(e) if run.cancel.load(Ordering::Relaxed) => {
                let _ = e;
                "Auto reframe stopped".to_string()
            }
            Err(e) => format!("Auto reframe failed: {e}"),
            Ok(subject) => {
                let Some(session) = &mut self.session else { return };
                // The clip may have moved or changed while it was measured.
                let same = session
                    .reframe_job(run.lane, run.idx)
                    .is_some_and(|j| j.path == run.job.path && (j.lo, j.hi) == (run.job.lo, run.job.hi) && j.target.in_frame == run.job.target.in_frame);
                if !same {
                    "Auto reframe: the clip changed while it was measured; run it again".to_string()
                } else {
                    let keys = engine::reframe::keys(&subject, &run.job.target, engine::reframe::DEFAULT_SMOOTHNESS);
                    let n = keys.len();
                    if session.apply_reframe(run.lane, run.idx, keys) {
                        self.reframed = Some((run.lane, run.idx, n));
                        self.mark_dirty();
                        self.reset_after_reseek();
                        format!("Reframed to the project with {n} key{} (Ctrl+Z undoes it)", if n == 1 { "" } else { "s" })
                    } else {
                        "Auto reframe: no change — the clip is already framed this way".to_string()
                    }
                }
            }
        };
        self.notify_user(said.into());
    }

    /// Turns the clip's stabilisation on with `st`, or off with `None`, as one
    /// undo step. Turning it on starts its analysis in the background, and the
    /// clip is waited on ([`Self::poll_stabilise`]) until it lands.
    pub(crate) fn write_stabilise(
        &mut self,
        lane: Lane,
        idx: usize,
        st: Option<engine::stabilise::Stabilise>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.set_stabilise(lane, idx, st) {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        self.wait_stabilise(lane, idx);
        cx.notify();
    }

    /// Sets how the clip's interlaced source is deinterlaced (one undo step).
    pub(crate) fn write_deinterlace(
        &mut self,
        lane: Lane,
        idx: usize,
        mode: engine::project::Deinterlace,
        cx: &mut Context<Self>,
    ) {
        if self.lane_locked_refusal(lane) {
            cx.notify();
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        if session.set_deinterlace(lane, idx, mode) {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// Starts (again, after a failure) the clip's analysis and waits on it.
    pub(crate) fn wait_stabilise(&mut self, lane: Lane, idx: usize) {
        let Some(session) = &self.session else {
            return;
        };
        if session.stabilise_of(lane, idx).is_none() {
            return;
        }
        if let (Some(range), Some(engine::stabilise::Status::Running(_))) =
            (session.stabilise_range(lane, idx), session.stabilise_start(lane, idx))
            && !self.stab_waiting.contains(&range)
        {
            self.stab_waiting.push(range);
        }
    }

    /// Stops the clip's running analysis; the clip stays switched on and plays
    /// as it is until it is analysed.
    pub(crate) fn cancel_stabilise(&mut self, lane: Lane, idx: usize, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session.stabilise_cancel(lane, idx);
        }
        cx.notify();
    }

    /// Lands every finished analysis: the picture is redrawn with its
    /// correction, and a failure is said once.
    pub(crate) fn poll_stabilise(&mut self) {
        if self.stab_waiting.is_empty() {
            return;
        }
        let mut landed = false;
        let mut said = Vec::new();
        self.stab_waiting.retain(|(path, lo, hi)| match engine::stabilise::status(path, *lo, *hi) {
            engine::stabilise::Status::Running(_) => true,
            engine::stabilise::Status::Ready => {
                landed = true;
                false
            }
            engine::stabilise::Status::Failed(e) => {
                said.push(format!("Stabilise stopped for {}: {e}", file_name(path)));
                false
            }
            engine::stabilise::Status::Missing => false,
        });
        if landed && let Some(session) = &mut self.session {
            session.stabilise_refresh();
            self.reset_after_reseek();
        }
        for text in said {
            self.notify_user(text.into());
        }
    }

    /// One [`SPEED_STEP`] per keystroke, clamped to what a [`Speed`] can hold.
    pub(crate) fn nudge_speed(&mut self, steps: i32, cx: &mut Context<Self>) {
        let at = i32::from(self.card_speed().permille()) + steps * SPEED_STEP;
        self.set_speed(speed_at(at), cx);
    }

    /// Where the pointer sits along the bar, as a rate: the left end is
    /// [`Speed::MIN`] and the right end [`Speed::MAX`], on a log scale, and
    /// snapped to the same [`SPEED_STEP`] grid the keys move on -- so a drag can
    /// land on exactly 1.00x and the same drag twice is one entry, not forty.
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live -- the colour card's rule, for the
    /// colour card's reason.
    pub(crate) fn drag_speed(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let along = frac_along(x, self.speed_bar.get());
        // Held back while the worker is busy, the colour card's way and for a
        // sharper reason: a live rate also restarts the sound, so a sweep that
        // wrote every step would restart it forty times.
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_speed, speed_on_bar(along), first, busy) {
            Some(speed) => self.write_speed(speed, !first, cx),
            None => cx.notify(),
        }
    }

    /// Writes what a slider drag held back, now that the worker has delivered.
    /// The gate is the frame that landed and never a timer: a 100 ms tick
    /// ([`SCRUB_GAP`]) says nothing about a reopen that costs half a second, and
    /// a drag gated on one would still queue opens nobody sees.
    ///
    /// Called again by the release, where readiness is beside the point: the
    /// value the hand let go on is owed whatever the worker is doing, and a
    /// gesture may not end on a sample that was dropped.
    pub(crate) fn flush_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(params) = self.pending_color.take() {
            self.write_color(params, true, cx);
        }
        if let Some(speed) = self.pending_speed.take() {
            self.write_speed(speed, true, cx);
        }
        if let Some((_, hue, sat)) = self.pending_viz.take() {
            // Live, as the two above are: the press is the one step this
            // gesture rolls back to (`Player::write_viz_hs`).
            self.write_viz_hs(hue, sat, true, cx);
        }
    }

    /// Opens the silence card on the clip to be scanned: the selected one, or
    /// -- with nothing selected -- the clip the picture is coming from, which is
    /// the rule the speed card follows and what a person means by "this shot".
    /// Either half of a take will do: both halves of an A/V take name the same
    /// file and play the same source frames, which is the whole of what a scan
    /// is of ([`ScanKey`]).
    ///
    /// The card is up on the next frame whatever the file is: a still is
    /// refused by name here, where the answer costs a look at the path, and
    /// everything the decoder has to open the file to know -- a track that is
    /// not there, a read that fails -- is refused the same way when the scan
    /// lands, because a fifty-second decode is not a thing to open a card
    /// behind ([`Player::start_silence_scan`]).
    pub(crate) fn open_silence(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("Nothing to scan — no file open".into());
            cx.notify();
            return;
        };
        // A caption the hand is on names the sound its group plays: the card
        // scans that half. A caption with no sound behind it -- alone, or
        // grouped with clips that have none -- is a scan with nothing to hear,
        // and it says so instead of returning without a word.
        let anchor = match self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()))
        {
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                match caption_media_half(session, (lane, idx), LaneKind::Audio) {
                    Some(half) => Some(half),
                    None => {
                        self.notify_user(
                            "Nothing to scan — a caption has no sound of its own".into(),
                        );
                        cx.notify();
                        return;
                    }
                }
            }
            other => other,
        };
        match anchor.map(|clip| audio_half(session, clip)) {
            Some((lane, idx)) => {
                let found = self.session.as_ref().and_then(|session| {
                    let clip = session.lane_clips(lane).get(idx)?.clone();
                    Some((session.sources().get(clip.source)?.clone(), clip))
                });
                // A still is asked *before* the decoder is: handing a png to the
                // mp4 demuxer answers "a box with a larger size than it", which
                // is a true sentence about a container and nothing a person can
                // act on. A picture has no sound for the same reason a silent
                // video has none, so it is refused in the same words.
                let Some((source, clip)) = found else {
                    cx.notify();
                    return;
                };
                if engine::is_image(&source.path) {
                    self.notify_user(unscannable(lane, idx, &source.path).into());
                    cx.notify();
                    return;
                }
                // One card at a time -- `close_card`'s own rule. This clears
                // `silence_open` too when the same clip is reopened; the
                // marks it drops come back below from the levels cache
                // (`scan_plan`/`silence_cached`), which `close_card` never
                // touches.
                self.close_other_cards();
                self.silence_open = Some((lane, idx));
                // Nothing picked: the card works on the whole timeline, and says so.
                if self.selected.anchor().is_none() {
                    self.silence_scope = Scope::Everything;
                }
                self.silence_field = 0;
                self.context_menu = None;
                // The clip's own range, not the file's: the scan reads what this
                // clip plays and nothing else, so a take cut in half costs half
                // the decode and finds only what is still on the timeline.
                let key = (
                    source.path.clone(),
                    source.audio_stream,
                    clip.in_frame,
                    clip.out_frame,
                );
                match scan_plan(
                    // The whole-source background scan counts as "cached"
                    // too: a card opened after import finds the levels a
                    // repaint has already been decoding since the file
                    // arrived ([`Player::cache_media`]), and `scan_silences`
                    // is what slices this clip's stretch out of them.
                    silence_cached(&self.silence_levels, &key),
                    self.silence_scan.as_ref().map(|scan| &scan.key),
                    &key,
                ) {
                    ScanPlan::Marks => self.scan_silences(),
                    ScanPlan::Start => self.start_silence_scan(key, cx),
                    ScanPlan::Wait => {}
                }
            }
            None => self.notify_user("Nothing to scan — no clip under the playhead".into()),
        }
        cx.notify();
    }

    /// Opens the mix card. `lane` is the row it lands on -- the track whose
    /// header was clicked -- and `None` starts at the top, which is what the
    /// stroke means.
    ///
    /// Nothing here is a clip's, so nothing is refused for want of a selection:
    /// a timeline with no audio track at all still has a limiter to set, and a
    /// fader on an empty track is the level the next take lands at.
    pub(crate) fn open_mix(&mut self, lane: Option<Lane>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // One card at a time -- `close_card`'s own rule.
        self.close_other_cards();
        self.mix_open = true;
        self.reissue_mix_solo();
        self.mix_field = lane
            .and_then(|lane| self.mix_lanes().iter().position(|&l| l == lane))
            .unwrap_or(0);
        self.context_menu = None;
        // Param cards live in the Clip tab. Route every opening door here so a
        // lane header never opens a card behind Sources.
        self.dock_src_active = false;
        crate::ui::dock_stance::save(false);
        cx.notify();
    }

    /// The audio tracks the card shows a fader for, top to bottom: *every* one
    /// of them, empty ones included -- what the timeline lays out, not what the
    /// mixer happens to open (`Project::audio_lanes` leaves an empty track out,
    /// and a fader that disappeared when a track was cleared would be a setting
    /// nobody could reach).
    pub(crate) fn mix_lanes(&self) -> Vec<Lane> {
        self.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Audio)
                .collect()
        })
    }

    /// Moves the row the card has picked: a fader by [`MIX_DB_STEP`], the
    /// ceiling by the same, and the switch either way (a ring of two, like the
    /// silence card's unit row).
    ///
    /// Every one of them goes through the session, which hands it straight to
    /// the running mixer: what the ear hears while the arrow is held is the mix
    /// that is being set, and nothing is rebuilt to make that true -- no reseek,
    /// so no `reset_after_reseek` and no blink in the picture behind the card
    /// ([`engine::PlaybackSession::set_lane_gain_db`]).
    pub(crate) fn nudge_mix(&mut self, steps: i32, cx: &mut Context<Self>) {
        let lanes = self.mix_lanes();
        if self.session.is_none() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        // The arm rides the door's answer, not the press: a fader already
        // against its wall, a ceiling already at its clamp and a limiter
        // switch that did not move are *not* edits, and `set_lane_gain_db` /
        // `set_limiter` each answer `false` for exactly those (an unchanged
        // value). Arming before the ask lit the ledger's `unsaved` ghost,
        // wrote an autosave sidecar and put the close prompt up over a
        // project whose bytes had not moved.
        let took = match lanes.get(self.mix_field) {
            Some(&lane) => {
                let at = session.lane_gain_db(lane) + steps as f32 * MIX_DB_STEP;
                session.set_lane_gain_db(lane, at)
            }
            None => {
                let limiter = session.limiter();
                let at = match self.mix_field - lanes.len() {
                    0 => Limiter {
                        on: limiter.on,
                        ..limiter
                    }
                    .with_ceiling(limiter.ceiling_db + steps as f32 * MIX_DB_STEP),
                    _ => Limiter {
                        on: !limiter.on,
                        ..limiter
                    },
                };
                session.set_limiter(at)
            }
        };
        if took {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The transport's stereo master meter for this frame (left and right, and whether the clip
    /// lamp is lit). Reads the stereo peaks only, so it and the mix card's meters (which read the
    /// mono master and the lane peaks) never starve each other.
    pub(crate) fn master_reading(&self, now: Instant) -> ([MixMeter; 2], bool) {
        let session = self.session.as_ref();
        let playing = session.is_some_and(|s| s.is_playing());
        let mut panel = self.mix_panel.borrow_mut();
        let meter = &mut panel.master_meter;
        meter.step(
            playing,
            || session.map_or([0.; 2], |s| s.master_peaks_lr()),
            now,
        );
        (meter.lr, meter.clipped(now))
    }


    /// Reads the mix card's meters for one frame: the peaks the sound reached
    /// since the last read, fed into each lane's meter. Paused or stopped, every
    /// meter falls to silence at once, so a paused card never holds a level.
    pub(crate) fn mix_reading(&self, now: Instant) -> MixReading {
        let lanes = self.mix_lanes();
        let session = self.session.as_ref();
        let playing = session.is_some_and(|s| s.is_playing());
        let levels = session.map(|s| s.levels());
        let sounding: Vec<Lane> = session.map_or_else(Vec::new, |s| {
            s.lanes()
                .into_iter()
                .filter(|&l| l.kind == LaneKind::Audio && !s.lane_clips(l).is_empty())
                .collect()
        });
        let mut panel = self.mix_panel.borrow_mut();
        let panel = &mut *panel;
        let mut strips = Vec::with_capacity(lanes.len());
        for &lane in &lanes {
            let peak = match (levels.as_ref(), meter_slot_in(&sounding, lane)) {
                (Some(levels), Some(at)) => levels.lanes.get(at).copied().unwrap_or(0.),
                _ => 0.,
            };
            let meter = lane_entry(&mut panel.meters, lane);
            feed_meter(meter, playing, peak, now);
            let bar = lane_entry(&mut panel.gain_bars, lane).clone();
            strips.push(MixStrip {
                lane,
                meter: *meter,
                bar,
            });
        }
        feed_meter(
            &mut panel.master,
            playing,
            levels.as_ref().map_or(0., |l| l.master),
            now,
        );
        MixReading {
            strips,
            master: panel.master,
            ceiling_bar: panel.ceiling_bar.clone(),
        }
    }

    /// The bar a fader's slider is read against, as the card last drew it.
    fn mix_gain_bar(&self, lane: Lane) -> Rc<Cell<Bounds<Pixels>>> {
        self.mix_panel
            .borrow()
            .gain_bars
            .iter()
            .find(|(l, _)| *l == lane)
            .map(|(_, bar)| bar.clone())
            .unwrap_or_default()
    }

    /// The bar a dynamics slider is read against, as the card last drew it.
    pub(crate) fn mix_voice_bar(&self, lane: Lane, param: VoiceParam) -> Rc<Cell<Bounds<Pixels>>> {
        let mut panel = self.mix_panel.borrow_mut();
        match panel.voice_bars.iter().find(|(k, _)| *k == (lane, param)) {
            Some((_, bar)) => bar.clone(),
            None => {
                let bar: Rc<Cell<Bounds<Pixels>>> = Default::default();
                panel.voice_bars.push(((lane, param), bar.clone()));
                bar
            }
        }
    }

    /// Whether a lane's dynamics section is unfolded.
    pub(crate) fn mix_voice_open(&self, lane: Lane) -> bool {
        self.mix_panel.borrow().voice_open.contains(&lane)
    }

    /// Unfolds or folds a lane's dynamics section.
    pub(crate) fn toggle_voice_section(&mut self, lane: Lane, cx: &mut Context<Self>) {
        let mut panel = self.mix_panel.borrow_mut();
        match panel.voice_open.iter().position(|&l| l == lane) {
            Some(at) => {
                panel.voice_open.remove(at);
            }
            None => panel.voice_open.push(lane),
        }
        drop(panel);
        cx.notify();
    }

    /// Sets one dynamics slider; `true` when it moved. The processor keeps its
    /// on/off state: a slider is a setting, the switch beside it is the bypass.
    pub(crate) fn set_voice_param(&mut self, lane: Lane, param: VoiceParam, v: f32) -> bool {
        match self.session.as_mut() {
            Some(session) => {
                let mut fx = session.lane_voice(lane);
                param.set(&mut fx, v);
                session.set_lane_voice(lane, fx)
            }
            None => false,
        }
    }

    /// Switches one processor on or off, keeping its settings. The noise
    /// reduction learns its profile the first time it is switched on.
    pub(crate) fn toggle_voice_proc(&mut self, lane: Lane, which: VoiceProc, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let mut fx = session.lane_voice(lane);
        let on = !which.is_on(&fx);
        match which {
            VoiceProc::Gate => fx.gate.on = on,
            VoiceProc::Comp => fx.comp.on = on,
            VoiceProc::Deess => fx.deess.on = on,
            VoiceProc::Nr => {
                if on && fx.nr.profile.is_empty() {
                    match session.learn_noise(lane, None) {
                        Ok(learned) => fx.nr.profile = learned.profile,
                        Err(e) => {
                            self.notify_user(format!("Noise reduction stays off — {e}").into());
                            cx.notify();
                            return;
                        }
                    }
                }
                fx.nr.on = on;
            }
        }
        if session.set_lane_voice(lane, fx) {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The one-click **Clean voice**: gate, noise reduction (its profile learned
    /// from the lane's quietest stretch), compressor and de-esser at `strength`,
    /// as one undo step ([`engine::voice::VoiceFx::clean_voice`], the same preset
    /// the MCP tool sets).
    pub(crate) fn clean_mix_voice(
        &mut self,
        lane: Lane,
        strength: engine::voice::Strength,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if lane.kind != LaneKind::Audio || session.lane_clips(lane).is_empty() {
            self.notify_user(format!("Nothing to clean — {} holds no sound", lane.label()).into());
            cx.notify();
            return;
        }
        // No quiet stretch to learn from is not a failure: the rest still applies.
        let learned = session.learn_noise(lane, None).ok();
        let fx = engine::voice::VoiceFx::clean_voice(
            strength,
            learned.as_ref().map(|l| l.profile.clone()).unwrap_or_default(),
            learned.as_ref().map(|l| l.noise_db),
        );
        let took = session.set_lane_voice(lane, fx);
        let message = match (took, learned.is_some()) {
            (false, _) => format!("{} is already clean at {}", lane.label(), strength.name()),
            (true, true) => format!("Cleaned {} ({})", lane.label(), strength.name()),
            (true, false) => format!(
                "Cleaned {} ({}) — no quiet stretch found, noise reduction left off",
                lane.label(),
                strength.name()
            ),
        };
        if took {
            self.mark_dirty();
        }
        self.notify_user(message.into());
        cx.notify();
    }

    /// The audio lane the Clean voice command works on: the selected clip's if
    /// it is sound, else the mix card's picked strip, else the first track with
    /// something on it.
    pub(crate) fn voice_target_lane(&self) -> Option<Lane> {
        let session = self.session.as_ref()?;
        if let Some((lane, _)) = self.selected.anchor().filter(|(l, _)| l.kind == LaneKind::Audio) {
            return Some(lane);
        }
        let lanes = self.mix_lanes();
        if self.mix_open
            && let Some(&lane) = lanes.get(self.mix_field)
        {
            return Some(lane);
        }
        lanes.into_iter().find(|&l| !session.lane_clips(l).is_empty())
    }

    /// Mutes or unmutes a track. Saved with the project, one undo step.
    pub(crate) fn toggle_mix_mute(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let changed = match self.session.as_mut() {
            Some(session) => {
                let on = !session.lane_muted(lane);
                session.set_lane_muted(lane, on)
            }
            None => false,
        };
        if changed {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// Solos or un-solos a track for listening. Not saved and not an undo step:
    /// the session is handed the set, and nothing else is touched.
    pub(crate) fn toggle_mix_solo(&mut self, lane: Lane, cx: &mut Context<Self>) {
        self.mix_solo = solo_toggled(&self.mix_solo, lane);
        self.push_mix_solo();
        cx.notify();
    }

    /// Hands the session the solo set. A lane that is gone, or that is not an
    /// audio lane, is left out, so a stale handle cannot silence the wrong track.
    fn push_mix_solo(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.set_solo(&self.mix_solo);
        }
    }

    /// Re-issues the solo set after the lanes may have changed under it: a
    /// handle names a position, so one whose lane has gone is dropped first.
    pub(crate) fn reissue_mix_solo(&mut self) {
        if let Some(session) = self.session.as_ref() {
            let lanes = session.lanes();
            self.mix_solo.retain(|l| lanes.contains(l));
        }
        self.push_mix_solo();
    }

    /// A mix slider was pressed: the drag it starts, and the value under the press.
    pub(crate) fn press_mix(&mut self, drag: MixDrag, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mix_field = match drag {
            MixDrag::Gain(lane) | MixDrag::Clip(lane, _) | MixDrag::Pitch(lane, _) => self
                .mix_lanes()
                .iter()
                .position(|&l| l == lane)
                .unwrap_or(0),
            MixDrag::Ceiling => self.mix_lanes().len(),
            MixDrag::Voice(lane, _) => self
                .mix_lanes()
                .iter()
                .position(|&l| l == lane)
                .unwrap_or(0),
        };
        self.mix_drag = Some(drag);
        self.drag_mix(x, cx);
    }

    /// The held slider follows the pointer: a fader or the ceiling takes the
    /// value under `x` on its bar, and nothing is rebuilt to make it heard.
    pub(crate) fn drag_mix(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.mix_drag else {
            return;
        };
        let (bar, lo, hi) = match drag {
            MixDrag::Gain(lane) => (
                self.mix_gain_bar(lane),
                engine::project::MIN_GAIN_DB,
                engine::project::MAX_GAIN_DB,
            ),
            MixDrag::Clip(..) => (
                self.mix_panel.borrow().clip_bar.clone(),
                engine::project::MIN_CLIP_GAIN_DB,
                engine::project::MAX_CLIP_GAIN_DB,
            ),
            MixDrag::Pitch(..) => (
                self.mix_panel.borrow().pitch_bar.clone(),
                f32::from(engine::project::MIN_CLIP_PITCH),
                f32::from(engine::project::MAX_CLIP_PITCH),
            ),
            MixDrag::Ceiling => (
                self.mix_panel.borrow().ceiling_bar.clone(),
                Limiter::MIN_DB,
                Limiter::MAX_DB,
            ),
            MixDrag::Voice(lane, param) => {
                let (lo, hi, _) = param.range();
                (self.mix_voice_bar(lane, param), lo, hi)
            }
        };
        if let Some(fraction) = bar_fraction(bar.get(), x) {
            // The faders share the knee: the clip's volume reads and drags on the same curve as the track's.
            let at = match drag {
                MixDrag::Gain(_) | MixDrag::Clip(..) => gain_at(fraction, hi),
                _ => value_at(fraction, lo, hi),
            };
            let took = match drag {
                MixDrag::Gain(lane) => self.set_mix_gain(lane, at),
                MixDrag::Clip(lane, idx) => self.set_clip_volume(lane, idx, at),
                // Whole semitones, which is what is stored.
                MixDrag::Pitch(lane, idx) => self.set_clip_pitch_mix(lane, idx, at.round() as i32),
                MixDrag::Ceiling => self.set_mix_ceiling(at),
                MixDrag::Voice(lane, param) => self.set_voice_param(lane, param, at),
            };
            if took {
                self.mark_dirty();
            }
        }
        cx.notify();
    }

    /// Lets go of a mix slider.
    pub(crate) fn release_mix(&mut self, cx: &mut Context<Self>) {
        self.mix_drag = None;
        // The card's pan fields are scrubbed here too, and a release over the card never reaches the root.
        self.pan_scrub_end(cx);
        cx.notify();
    }

    /// Sets a track's fader; `true` when it moved.
    pub(crate) fn set_mix_gain(&mut self, lane: Lane, db: f32) -> bool {
        match self.session.as_mut() {
            Some(session) => session.set_lane_gain_db(lane, db),
            None => false,
        }
    }

    /// Sets the selected clip's volume; `true` when it moved.
    pub(crate) fn set_clip_volume(&mut self, lane: Lane, idx: usize, db: f32) -> bool {
        match self.session.as_mut() {
            Some(session) => session.set_clip_gain(lane, idx, db),
            None => false,
        }
    }

    /// Sets the selected clip's pitch in semitones; `true` when it moved.
    pub(crate) fn set_clip_pitch_mix(&mut self, lane: Lane, idx: usize, semitones: i32) -> bool {
        match self.session.as_mut() {
            Some(session) => session.set_clip_pitch(lane, idx, semitones),
            None => false,
        }
    }

    /// Sets the limiter's ceiling; `true` when it moved.
    pub(crate) fn set_mix_ceiling(&mut self, db: f32) -> bool {
        match self.session.as_mut() {
            Some(session) => {
                let limiter = session.limiter();
                session.set_limiter(limiter.with_ceiling(db))
            }
            None => false,
        }
    }

    /// A fader or the ceiling back to its default, from its own Reset.
    pub(crate) fn reset_mix(&mut self, drag: MixDrag, cx: &mut Context<Self>) {
        let took = match drag {
            MixDrag::Gain(lane) => self.set_mix_gain(lane, 0.),
            MixDrag::Clip(lane, idx) => self.set_clip_volume(lane, idx, 0.),
            MixDrag::Pitch(lane, idx) => self.set_clip_pitch_mix(lane, idx, 0),
            MixDrag::Ceiling => self.set_mix_ceiling(Limiter::default().ceiling_db),
            MixDrag::Voice(lane, param) => {
                let at = param.get(&engine::voice::VoiceFx::default());
                self.set_voice_param(lane, param, at)
            }
        };
        if took {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// Opens the subtitle style card: the platform's font list, asked for
    /// once and kept while the card is up, and the size stepper beside it.
    /// Nothing here is a clip's or even the project's -- it never reaches the
    /// export, which never burns a cue into the picture -- so like the mix
    /// card it opens with no timeline required and nothing to refuse.
    /// Opens the settings page (three tabs -- PROJECT/EDITOR/EXPORT -- over
    /// whichever section is showing, `ui::settings_stance`): the one door for
    /// "edit project and editor settings" -- refused while an export is
    /// running, same as every other card here, since the exporting card owns
    /// this same modal slot (`oracle.rs` agrees: `ActionId::Settings` is `No`
    /// there too, for the same reason).
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // One card at a time -- `close_card`'s own rule -- except the Export
        // tab, which is a tab: closing Settings lands back on it.
        let on_export = self.export_open;
        self.close_card();
        self.export_open = on_export;
        self.settings_open = true;
        self.context_menu = None;
        cx.notify();
    }

    /// Which of the settings page's three tabs is showing -- the strip's one
    /// door, and the page's only piece of state that [`Self::close_card`]
    /// leaves alone, so a close and a reopen come back to the section a hand
    /// left rather than snapping back to PROJECT. Display state and nothing
    /// else: no row reads it, it writes no project value, and it therefore
    /// never lights the ledger or arms autosave -- picking a tab is not an
    /// edit, which is why this is a setter with no [`Self::mark_dirty`] in
    /// it, unlike every row the page carries that writes a value.
    pub(crate) fn set_settings_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        self.settings_tab = tab;
        // The keyboard's row is the showing tab's; a new tab starts at rest.
        self.settings_focus = None;
        cx.notify();
    }

    pub(crate) fn open_subtitle_style(&mut self, cx: &mut Context<Self>) {
        // One card at a time -- `close_card`'s own rule.
        self.close_card();
        // Only the families a caption can be set in: a symbol, emoji, math or mono face is left out.
        let text = cx.text_system().clone();
        self.subtitle_fonts = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|name| crate::ui::cards::substyle::family_face_ok(&text, name))
            .collect();
        self.subtitle_style_open = true;
        self.subtitle_style_field = 0;
        // A drag cut off by a close never saw its button come up.
        self.sub_dragging = false;
        self.sub_font_open = false;
        self.sub_font_query.clear();
        self.context_menu = None;
        cx.notify();
    }

    /// Picks a family outright -- a click on a row, the card's rule for a
    /// list rather than a stepper. `None` is the top row, the platform's own
    /// choice, which is what a family nobody can spell as a fallback would
    /// leave silently wrong instead.
    pub(crate) fn set_sub_family(&mut self, family: Option<String>, cx: &mut Context<Self>) {
        self.sub_family = family;
        self.keep_subtitle_style(cx);
    }

    /// One [`SUB_SIZE_STEP`] per keystroke or stepper press, clamped to
    /// [`SUB_SIZE_RANGE`].
    pub(crate) fn nudge_sub_size(&mut self, steps: i32, cx: &mut Context<Self>) {
        self.set_sub_size(self.sub_text + steps as f32 * SUB_SIZE_STEP, cx);
    }

    /// Sets the size, clamped to [`SUB_SIZE_RANGE`], and keeps it.
    pub(crate) fn set_sub_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.sub_text = size.clamp(SUB_SIZE_RANGE.0, SUB_SIZE_RANGE.1);
        self.keep_subtitle_style(cx);
    }

    /// A slider drag: the size follows the pointer on screen but is not kept
    /// until the button comes up ([`Self::end_sub_drag`]), so a sweep is one
    /// write to the file and not one per pixel.
    pub(crate) fn drag_sub_size(&mut self, x: Pixels, bar: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.sub_text = sub_size_at(frac_along(x, bar));
        cx.notify();
    }

    pub(crate) fn end_sub_drag(&mut self, cx: &mut Context<Self>) {
        if self.sub_dragging {
            self.sub_dragging = false;
            self.keep_subtitle_style(cx);
        }
    }

    pub(crate) fn set_sub_outline(&mut self, outline: SubOutline, cx: &mut Context<Self>) {
        self.sub_outline = outline;
        self.keep_subtitle_style(cx);
    }

    pub(crate) fn set_sub_boxed(&mut self, boxed: bool, cx: &mut Context<Self>) {
        self.sub_boxed = boxed;
        self.keep_subtitle_style(cx);
    }

    pub(crate) fn set_sub_place(&mut self, place: SubPlace, cx: &mut Context<Self>) {
        self.sub_place = place;
        self.keep_subtitle_style(cx);
    }

    /// A press on the highlight colour's hex field: it opens for typing with all of it selected.
    pub(crate) fn begin_sub_highlight_hex(&mut self, cx: &mut Context<Self>) {
        self.commit_gen_hex(cx);
        let [r, g, b] = self.session.as_ref().map(|s| s.caption_style()).unwrap_or_default().highlight;
        let rgb = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        self.gen_hex = Some((
            crate::player::generators::GenPaint::SubHighlight,
            crate::player::text_field::TextField::selected(crate::ui::kit::hex_text(rgb)),
        ));
        cx.notify();
    }

    /// Writes the typed highlight colour (`#RRGGBB`, `RRGGBB` or `#RGB`) as the caption style's own,
    /// exactly as typed. Anything else keeps the old colour and says so.
    pub(crate) fn commit_sub_highlight_hex(&mut self, text: &str, cx: &mut Context<Self>) {
        match crate::ui::kit::parse_hex(text) {
            Some(c) => self.edit_caption_style(|cs| cs.highlight = [(c >> 16) as u8, (c >> 8) as u8, c as u8], cx),
            None => {
                self.notify_user("A colour is six hex digits, such as FFD600 — kept the old one".into());
                cx.notify();
            }
        }
    }

    /// Changes the project's caption style (the animation a burn draws) with `f`: one undo
    /// step, and the project is dirty. Nothing without a project open.
    pub(crate) fn edit_caption_style(
        &mut self,
        f: impl FnOnce(&mut engine::captionanim::CaptionStyle),
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = &mut self.session {
            let mut cs = session.caption_style();
            f(&mut cs);
            if session.set_caption_style(cs) {
                self.mark_dirty();
            }
        }
        cx.notify();
    }

    /// The arrow keys on the picked row: left and right move the size, walk
    /// the outline, or flip the box or the place. Families and the default
    /// have no step, only a pick.
    pub(crate) fn step_sub_field(&mut self, dir: i32, cx: &mut Context<Self>) {
        match self.subtitle_style_field {
            SUB_ROW_SIZE => self.nudge_sub_size(dir, cx),
            SUB_ROW_OUTLINE => self.set_sub_outline(self.sub_outline.step(dir), cx),
            SUB_ROW_BOX => self.set_sub_boxed(!self.sub_boxed, cx),
            SUB_ROW_PLACE => self.set_sub_place(
                match self.sub_place {
                    SubPlace::Bottom => SubPlace::Top,
                    SubPlace::Top => SubPlace::Bottom,
                },
                cx,
            ),
            SUB_ROW_ANIM => self.edit_caption_style(
                |cs| {
                    use engine::captionanim::CaptionAnim;
                    let all = CaptionAnim::ALL;
                    let at = all.iter().position(|a| *a == cs.anim).unwrap_or(0) as i32;
                    cs.anim = all[(at + dir).rem_euclid(all.len() as i32) as usize];
                },
                cx,
            ),
            SUB_ROW_HIGHLIGHT => self.edit_caption_style(
                |cs| {
                    let at = HIGHLIGHT_SWATCHES.iter().position(|(_, c)| *c == cs.highlight);
                    let n = HIGHLIGHT_SWATCHES.len() as i32;
                    let next = at.map_or(0, |a| (a as i32 + dir).rem_euclid(n) as usize);
                    cs.highlight = HIGHLIGHT_SWATCHES[next].1;
                },
                cx,
            ),
            SUB_ROW_HL_BOX => self.edit_caption_style(|cs| cs.hl_box = !cs.hl_box, cx),
            SUB_ROW_UPPERCASE => self.edit_caption_style(|cs| cs.uppercase = !cs.uppercase, cx),
            SUB_ROW_WORDS => self.edit_caption_style(
                |cs| {
                    let max = i32::from(engine::captionanim::MAX_WORDS_PER_LINE);
                    cs.words_per_line = (i32::from(cs.words_per_line) + dir).clamp(0, max) as u8;
                },
                cx,
            ),
            _ => {}
        }
    }

    /// Writes the style to disk and repaints: every change to any field
    /// goes through here, so the file on disk and the plate on screen never
    /// disagree about which session's edit is in force.
    pub(crate) fn keep_subtitle_style(&mut self, cx: &mut Context<Self>) {
        let style = SubStyle {
            family: self.sub_family.clone(),
            size: self.sub_text,
            outline: self.sub_outline,
            boxed: self.sub_boxed,
            place: self.sub_place,
        };
        if let Err(e) = save_subtitle_style(&style) {
            let path = subtitle_style_path();
            self.notify_user(
                format!(
                    "Subtitle style could not be kept — {} — {e}",
                    path.display()
                )
                .into(),
            );
        }
        cx.notify();
    }

    /// Closes it and drops the preview with it: marks left on the lane after
    /// the card is gone would name frames the next edit has already moved.
    pub(crate) fn close_silence(&mut self) {
        self.silence_open = None;
        self.silence_marks.clear();
        self.cancel_silence_scan();
    }

    /// Tells the worker nobody is waiting any more. It gives up at its next
    /// chunk and the levels it had are dropped: half a track is not an answer,
    /// and the flag stays set on the [`Arc`] the landing closure holds, which is
    /// how that closure knows to keep its hands off the card.
    pub(crate) fn cancel_silence_scan(&mut self) {
        if let Some(scan) = self.silence_scan.take() {
            scan.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Hands the decode to a worker and returns at once -- the card is drawn by
    /// the very next frame, saying it is scanning. Fifty-one seconds on a 25 GB
    /// film is what this used to cost on the render thread, with the card
    /// marked open and nothing on screen.
    ///
    /// Whatever was scanning is cancelled first: one card, one scan, and the
    /// clip that has just been asked about is the one worth the disk.
    ///
    /// Only the clip's own `[in, out)` is read -- source frames over the
    /// project's rate, the same seconds [`engine::Project`] hands the decoder
    /// for playback -- so half a take is half a wait.
    pub(crate) fn start_silence_scan(&mut self, key: ScanKey, cx: &mut Context<Self>) {
        self.cancel_silence_scan();
        self.silence_marks.clear();
        let progress = Arc::new(engine::silence::Progress::default());
        let range = source_secs(&key, self.fps);
        let scan = cx.background_executor().spawn({
            let (key, progress) = (key.clone(), Arc::clone(&progress));
            async move { engine::silence::levels_with_progress(&key.0, key.1, range, &progress) }
        });
        let now = Instant::now();
        self.silence_scan = Some(SilenceScan {
            key: key.clone(),
            started: now,
            progress: Arc::clone(&progress),
            seen: 0,
            since: now,
        });
        cx.spawn(async move |this, cx| {
            let landed = scan.await;
            this.update(cx, |this, cx| {
                // Cancelled means the card moved on or closed: the levels are a
                // prefix of a track nobody asked about any more.
                if progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.silence_scan = None;
                match landed {
                    Ok(Some(levels)) => {
                        this.silence_levels.insert(key.clone(), Arc::new(levels));
                        this.scan_silences();
                    }
                    // A source with no audio track is not one long silence: it
                    // is a clip this card has nothing to say about, named so the
                    // user knows which one it meant.
                    Ok(None) => {
                        if let Some((lane, idx)) = this.silence_open {
                            this.notify_user(unscannable(lane, idx, &key.0).into());
                        }
                        this.close_silence();
                    }
                    Err(e) => {
                        this.close_silence();
                        this.notify_user(format!("Scan failed: {e}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the scanning line's stall clock, for [`Player::poll_import`]'s
    /// reason: sampled once per frame rather than while drawing.
    pub(crate) fn poll_silence(&mut self) {
        if let Some(scan) = &mut self.silence_scan {
            scan.poll();
        }
    }

    /// Applies the settings to levels already in hand and replaces the preview
    /// -- never stacks on it. Arithmetic only: the decode is
    /// [`Player::start_silence_scan`]'s and happens once per source, so every
    /// run here is numbers already read, which is what makes moving a threshold
    /// feel like moving a slider. A source still being scanned has no marks yet
    /// and says so on the card.
    ///
    /// Changes nothing about the project: a preview is not an edit, and no undo
    /// step is spent until a button is pressed.
    pub(crate) fn scan_silences(&mut self) {
        let Some((lane, idx)) = self.silence_open else {
            return;
        };
        self.silence_marks.clear();
        // Copied out before anything is written back: the cache below lives on
        // the same struct the session does.
        let Some((clip, source)) = self.session.as_ref().and_then(|session| {
            let clip = session.lane_clips(lane).get(idx)?.clone();
            let source = session.sources().get(clip.source)?.clone();
            Some((clip, source))
        }) else {
            return;
        };
        // Nothing read yet: the worker is running and the card is drawing its
        // line. The marks arrive with the levels.
        //
        // The clip's own read first -- an on-demand scan already asked for
        // exactly this stretch -- and failing that, this clip's slice of the
        // whole-source background scan ([`Player::cache_media`]), which is
        // what makes the card answer at once for a file that arrived a
        // repaint or two ago.
        let key = (
            source.path.clone(),
            source.audio_stream,
            clip.in_frame,
            clip.out_frame,
        );
        let levels = match self.silence_levels.get(&key).cloned() {
            Some(levels) => Some(levels),
            None => self
                .silence_levels
                .get(&full_scan_key(&source.path, source.audio_stream))
                .map(|whole| {
                    Arc::new(slice_whole_levels(
                        whole,
                        self.fps,
                        clip.in_frame,
                        clip.out_frame,
                    ))
                }),
        };
        let Some(levels) = levels else {
            return;
        };
        // Keep-only flips what the applies act on: the *speech* between the
        // quiet stretches, so a cut removes the words and leaves the room
        // tone. Skipped when nothing was quiet -- inverting an empty scan
        // would mark the whole clip, the one press this card must never make
        // by accident.
        let quiet = engine::silence::regions(&levels, self.silence);
        let source = if self.silence_keep_only && !quiet.is_empty() {
            let duration = levels.len() as f64 / f64::from(engine::silence::WINDOWS_PER_SEC);
            engine::silence::invert(duration, &quiet)
        } else {
            quiet
        };
        self.silence_marks = engine::silence::timeline_regions(&clip, self.fps, &source);
    }

    /// Keep-only-silence off and on, the marks going with it: the same scan,
    /// the complement of the quiet when it is on. A preview is not an edit,
    /// so a flip costs nothing and changes nothing but what is marked.
    pub(crate) fn toggle_silence_keep_only(&mut self) {
        self.silence_keep_only = !self.silence_keep_only;
        self.scan_silences();
    }

    /// Moves the picked row by `steps` and re-runs the scan against it, so the
    /// marks on the lane are always what the numbers on the card say.
    pub(crate) fn nudge_silence(&mut self, steps: i32) {
        let secs = |at: f64| {
            (at + f64::from(steps) * SILENCE_SECS_STEP)
                .clamp(SILENCE_SECS_RANGE.0, SILENCE_SECS_RANGE.1)
        };
        match self.silence_field {
            // Round either way, like the fit policy's cycle: three choices are
            // a ring, not a range.
            0 => {
                let at = SCOPES.iter().position(|&s| s == self.silence_scope);
                let step = steps.rem_euclid(SCOPES.len() as i32) as usize;
                self.silence_scope = SCOPES[(at.unwrap_or(0) + step) % SCOPES.len()];
            }
            1 => {
                self.silence.threshold_db = (self.silence.threshold_db
                    + steps as f32 * SILENCE_DB_STEP)
                    .clamp(SILENCE_DB_RANGE.0, SILENCE_DB_RANGE.1)
            }
            // Two spellings of the same level, so either arrow flips it -- a
            // ring of two, like the scope row's.
            2 => self.silence_dbfs = !self.silence_dbfs,
            3 => self.silence.min_silence = secs(self.silence.min_silence),
            4 => self.silence.padding = secs(self.silence.padding),
            5 => self.silence.min_keep = secs(self.silence.min_keep),
            _ => {
                self.silence_factor =
                    silence_rate(i32::from(self.silence_factor.permille()) + steps * SPEED_STEP)
            }
        }
        // Neither the scope nor the rate is part of the scan, but re-running is
        // cheap (the levels are cached) and one path is one place for the marks
        // to come from.
        self.scan_silences();
    }

    /// The rows the silence card shows before its `More …` fold, in the order
    /// it lists them: the scope, the threshold, the forgiveness and the rate.
    pub(crate) const SILENCE_MAIN_ROWS: [usize; 4] = [0, 1, 3, 6];
    /// The rows behind the fold: the unit the level is read in, the padding and
    /// the sliver floor -- settings a first run does not need.
    pub(crate) const SILENCE_MORE_ROWS: [usize; 3] = [2, 4, 5];
    /// The fold's id in the panel-folds file ([`crate::ui::widgets::load_folds`]).
    pub(crate) const SILENCE_MORE_FOLD: &'static str = "silence-more";

    /// Whether the card's `More …` fold is open.
    pub(crate) fn silence_more_open(&self) -> bool {
        self.folds.contains(Self::SILENCE_MORE_FOLD)
    }

    /// The rows in the order the arrows walk them and the card draws them: the
    /// main rows, then the fold's when it is open.
    pub(crate) fn silence_fields(&self) -> Vec<usize> {
        const {
            assert!(
                Self::SILENCE_MAIN_ROWS.len() + Self::SILENCE_MORE_ROWS.len() == SILENCE_ROWS,
                "every silence row is on the card, main or behind the fold"
            )
        };
        let mut rows = Self::SILENCE_MAIN_ROWS.to_vec();
        if self.silence_more_open() {
            rows.extend(Self::SILENCE_MORE_ROWS);
        }
        rows
    }

    /// Moves the picked row one place along [`Self::silence_fields`], wrapping.
    pub(crate) fn step_silence_field(&mut self, by: i32) {
        let rows = self.silence_fields();
        let at = rows
            .iter()
            .position(|&n| n == self.silence_field)
            .unwrap_or(0);
        let next = (at as i32 + by).rem_euclid(rows.len() as i32) as usize;
        self.silence_field = rows[next];
    }

    /// Opens or closes the fold and saves it with the other panels' folds. A
    /// picked row behind the fold hands the arrows back to the first row.
    pub(crate) fn toggle_silence_more(&mut self, cx: &mut Context<Self>) {
        if !self.folds.remove(Self::SILENCE_MORE_FOLD) {
            self.folds.insert(Self::SILENCE_MORE_FOLD.to_string());
        }
        if !self.silence_more_open() && !Self::SILENCE_MAIN_ROWS.contains(&self.silence_field) {
            self.silence_field = 0;
        }
        crate::ui::widgets::save_folds(&self.folds);
        cx.notify();
    }

    /// The range a slider row sweeps and the grid its value snaps to: the
    /// threshold in dB, the three durations in seconds, the rate in thousandths
    /// (never below real time -- see [`silence_rate`]). `None` for the two rows
    /// that are choices, not numbers.
    pub(crate) fn silence_range(field: usize) -> Option<(f64, f64, f64)> {
        match field {
            1 => Some((
                f64::from(SILENCE_DB_RANGE.0),
                f64::from(SILENCE_DB_RANGE.1),
                f64::from(SILENCE_DB_STEP),
            )),
            3..=5 => Some((
                SILENCE_SECS_RANGE.0,
                SILENCE_SECS_RANGE.1,
                SILENCE_SECS_STEP,
            )),
            6 => Some((
                f64::from(Speed::NORMAL.permille()) + f64::from(SPEED_STEP),
                f64::from(Speed::MAX.permille()),
                f64::from(SPEED_STEP),
            )),
            _ => None,
        }
    }

    /// What a slider row says its value is, in the unit its range is in.
    pub(crate) fn silence_value(&self, field: usize) -> f64 {
        match field {
            1 => f64::from(self.silence.threshold_db),
            3 => self.silence.min_silence,
            4 => self.silence.padding,
            5 => self.silence.min_keep,
            6 => f64::from(self.silence_factor.permille()),
            _ => 0.,
        }
    }

    /// The value a fresh card opens with, for the same row.
    pub(crate) fn silence_default(field: usize) -> f64 {
        let fresh = engine::silence::Settings::default();
        match field {
            1 => f64::from(fresh.threshold_db),
            3 => fresh.min_silence,
            4 => fresh.padding,
            5 => fresh.min_keep,
            _ => f64::from(Speed::MAX.permille()),
        }
    }

    /// Whether a slider row has moved off its default -- the ink1 and the
    /// Reset button both say so.
    pub(crate) fn silence_changed(&self, field: usize) -> bool {
        (self.silence_value(field) - Self::silence_default(field)).abs() > 1e-9
    }

    /// Where a slider row's value sits on its track, 0..1.
    pub(crate) fn silence_fraction(&self, field: usize) -> f32 {
        match Self::silence_range(field) {
            Some((lo, hi, _)) => {
                slider_fraction(self.silence_value(field) as f32, lo as f32, hi as f32)
            }
            None => 0.,
        }
    }

    /// Writes one slider row's value, clamped by the caller. The rate goes
    /// through [`silence_rate`], whose floor is real time plus one step.
    fn write_silence(&mut self, field: usize, at: f64) {
        match field {
            1 => self.silence.threshold_db = at as f32,
            3 => self.silence.min_silence = at,
            4 => self.silence.padding = at,
            5 => self.silence.min_keep = at,
            6 => self.silence_factor = silence_rate(at as i32),
            _ => {}
        }
    }

    /// A press on a slider row: the value at `along` of its track, snapped to
    /// its grid. A preview like the arrows, so it re-runs the scan and touches
    /// no project value.
    pub(crate) fn press_silence(&mut self, field: usize, along: f32) {
        let Some(at) = Self::silence_snap(field, along) else {
            return;
        };
        self.silence_field = field;
        self.write_silence(field, at);
        self.scan_silences();
    }

    /// The value at `along` of a slider row's track, snapped to its grid and
    /// kept inside its range. `None` for a row that is not a slider.
    pub(crate) fn silence_snap(field: usize, along: f32) -> Option<f64> {
        let (lo, hi, step) = Self::silence_range(field)?;
        let raw = lo + f64::from(along.clamp(0., 1.)) * (hi - lo);
        Some(((raw / step).round() * step).clamp(lo, hi))
    }

    /// One slider row back to its default: its own Reset button.
    pub(crate) fn reset_silence_field(&mut self, field: usize, cx: &mut Context<Self>) {
        self.silence_field = field;
        self.write_silence(field, Self::silence_default(field));
        self.scan_silences();
        cx.notify();
    }

    /// Whether any row is off its default, which is when the head offers
    /// `Reset all`.
    pub(crate) fn silence_any_changed(&self) -> bool {
        !self.silence_dbfs
            || [1, 3, 4, 5, 6]
                .into_iter()
                .any(|field| self.silence_changed(field))
    }

    /// Every row back to what a fresh card opens with: the head's `Reset all`.
    /// The keep-only switch stays where the hand left it -- it is a mode, not
    /// a setting.
    pub(crate) fn reset_silence(&mut self, cx: &mut Context<Self>) {
        self.silence = engine::silence::Settings::default();
        self.silence_factor = Speed::MAX;
        self.silence_scope = Scope::Take;
        self.silence_dbfs = true;
        self.scan_silences();
        cx.notify();
    }

    /// Which lanes an apply reaches, as the card's scope row says it: the
    /// lanes of the take the scanned clip belongs to, that clip's lane alone,
    /// or every lane there is.
    ///
    /// The take's lanes are the ones carrying its group id -- a link is one
    /// span on however many lanes, so "the take" is exactly the set of lanes
    /// that would otherwise be pulled apart. Nothing widens behind the user's
    /// back: [`Project::cut_regions`] refuses a scope that would split a take,
    /// and this row is how the user says the take instead.
    pub(crate) fn silence_lanes(&self) -> Vec<Lane> {
        let (Some((lane, idx)), Some(session)) = (self.silence_open, self.session.as_ref()) else {
            return Vec::new();
        };
        match self.silence_scope {
            Scope::Track => vec![lane],
            Scope::Everything => session.lanes(),
            Scope::Take => match session.lane_clips(lane).get(idx).and_then(|c| c.link) {
                None => vec![lane],
                Some(id) => session
                    .lanes()
                    .into_iter()
                    .filter(|&l| {
                        l == lane
                            || session.lane_clips(l).iter().any(|c| c.link == Some(id))
                            // The caption pinned to the take travels with it:
                            // the ripple closes its lane too, or the words
                            // would stay put while the clip under them moved.
                            || session.sub_lane(l).iter().any(|s| s.link == Some(id))
                    })
                    .collect(),
            },
        }
    }

    /// What an apply acts on: the previewed set and the lanes it reaches, or
    /// nothing at all with a notice saying so in the numbers that found
    /// nothing.
    pub(crate) fn previewed(&mut self) -> Option<Previewed> {
        if self.silence_marks.is_empty() {
            self.notify_user(if self.silence_keep_only {
                "No speech to cut — keep-only-silence needs silence and speech".into()
            } else {
                format!(
                    "No silence — under {:.0} dBFS lasting {:.2} s",
                    self.silence.threshold_db, self.silence.min_silence
                )
                .into()
            });
            return None;
        }
        Some((self.silence_marks.clone(), self.silence_lanes()))
    }

    /// What an apply says afterwards: which tracks it reached, and -- when that
    /// was not all of them -- that the rest were left where they were. The
    /// scope is a choice, so the confirmation has to name the choice.
    pub(crate) fn silence_reach(&self, lanes: &[Lane]) -> String {
        let named = lanes
            .iter()
            .map(|l| l.label())
            .collect::<Vec<_>>()
            .join("+");
        match self.silence_scope {
            Scope::Everything => "on every track".to_string(),
            _ => format!("on {named} — other tracks untouched"),
        }
    }

    /// Cuts every previewed silence out of the lanes the scope names, rippling
    /// each hole closed -- one edit and **one** undo press however many there
    /// were ([`engine::PlaybackSession::cut_regions`]). Tracks outside the
    /// scope do not move; a scope that would take half a take with it comes
    /// back refused in the engine's own words, naming both halves.
    pub(crate) fn cut_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let saved = f64::from(regions.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        let (count, reach) = (regions.len(), self.silence_reach(&lanes));
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.cut_regions(&regions, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Every hole closed moves the clips after it up a place, so the
                // selection now names a different clip than the one that is
                // highlighted -- dropped here as after every other edit that
                // moves indexes (a delete, a paste, an undo).
                self.selected.clear();
                self.reset_after_reseek();
                let what = if self.silence_keep_only {
                    format!("{count} speech")
                } else {
                    count_noun(count, "silence", "silences")
                };
                self.notify_user(
                    format!(
                        "{what} cut {reach} — {} shorter · undo {}",
                        secs_label(saved),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Plays them fast instead of cutting them, closing the room each one no
    /// longer needs. One undo press like the cut, and the same scope; the
    /// refusals (a clip lapping over a silence, a scope that would split a
    /// take) come back in the engine's own words and name the lane and frame,
    /// and the card stays up so the numbers that produced it are still on
    /// screen.
    pub(crate) fn speed_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let (count, rate) = (regions.len(), self.silence_factor);
        let reach = self.silence_reach(&lanes);
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.speed_regions(&regions, rate, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Splitting each silence out and closing the room it no longer
                // needs moves indexes exactly as the cut does: the selection
                // goes with them.
                self.selected.clear();
                self.reset_after_reseek();
                let what = if self.silence_keep_only {
                    "speech"
                } else {
                    "silences"
                };
                self.notify_user(
                    format!(
                        "{count} {what} at {rate} {reach} · undo {}",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Whether a card owns the window. While one does the timeline under it is
    /// out of reach, so a right-click there opens no menu -- the same rule the
    /// key handler and the drop target already follow.
    /// Whether anything at all is drawn over the window -- a card, a menu or an
    /// open list. What the hover labels stand aside for ([`OVERLAID`]): a
    /// tooltip belongs to the surface the pointer is on, and while one of these
    /// is up that surface is behind it.
    pub(crate) fn overlaid(&self) -> bool {
        self.modal()
            || self.context_menu.is_some()
            || self.cue_menu.is_some()
            || self.library_menu.is_some()
            || self.picker.is_some()
    }

    /// Whether a card owns the screen. A running export is deliberately not
    /// in here: it is a *state* the oracle answers for action by action (the
    /// editor stays usable from the keyboard, and what must wait is spoken as
    /// a refusal), not a surface sitting on the keys. The darkroom's key guard
    /// ([`ui::stance`]'s `on_key_down`) reads [`Player::card_open`] for the same
    /// reason, so a keystroke during an export reaches `enable()`/`act()`.
    pub(crate) fn modal(&self) -> bool {
        self.card_open()
    }

    /// Whether an actual card owns the screen -- the card half of
    /// [`Player::modal`], which is this now. Kept as its own name because the
    /// key guard reads it, and `tests/cards.rs` pins that it never grows an
    /// `exporting` read.
    pub(crate) fn card_open(&self) -> bool {
        // Not `keys_open`: since 2026-09-09 that is the dock's KEYS *tab*
        // showing, a region like any other, not a surface sitting on the
        // keyboard -- every stroke still means what the spine says it means
        // while the list is up.
        self.settings_open
            || self.proj_dialog.is_some()
            || self.export_open
            || self.eq_open.is_some()
            || self.color_open.is_some()
            || self.transform_open.is_some()
            || self.speed_open.is_some()
            || self.effects_open.is_some()
            || self.silence_open.is_some()
            || self.beats_open.is_some()
            || self.mix_open
            || self.markers_open
            || self.history_open
            || self.tools.is_some()
            || self.subtitle_style_open
    }

    /// Whether the Export tab is the only card up: escape leaves it showing.
    pub(crate) fn only_export_tab(&self) -> bool {
        self.export_open
            && !(self.settings_open
                || self.proj_dialog.is_some()
                || self.eq_open.is_some()
                || self.color_open.is_some()
                || self.transform_open.is_some()
                || self.speed_open.is_some()
                || self.effects_open.is_some()
                || self.silence_open.is_some()
                || self.mix_open
                || self.markers_open
                || self.history_open
            || self.history_open
                || self.tools.is_some()
                || self.subtitle_style_open)
    }

    /// Whether something covers the room's pointer doors (the timeline's menus, drags and clicks).
    /// Only a real overlay does: Settings and the project dialogs. Every card docked in the right
    /// panel (Tools, Markers, Silence, EQ, Export, ...) shades nothing, so none of them may eat a
    /// right-click, a drag or a click on the timeline -- that was the regression, three times.
    /// Floating plates and menus answer for themselves ([`Player::overlaid`]).
    pub(crate) fn covers_room(&self) -> bool {
        self.settings_open || self.proj_dialog.is_some()
    }

    /// Whether the only thing up is a card docked in the right panel (grade, transform, speed,
    /// effects, EQ, silence, mix, markers, tools, subtitle style): it shares the dock with
    /// everything else and draws nothing over the timeline, so it must not behave as the invisible
    /// modal the sheets are -- the transport keys keep their meaning while it shows. The Export
    /// tab keeps its own key list ([`export_tab_action`]).
    pub(crate) fn docked_card_only(&self) -> bool {
        self.card_open() && !self.covers_room() && !self.export_open
    }

    /// The invariant behind every card-opening door (menu, context menu,
    /// palette, key, hook): a card that mounts in the Clip tab is never up
    /// while the dock shows Media, so the dock swings to the Clip tab. One
    /// place, read each frame, instead of a line in each `open_*` -- the
    /// five that forgot it left the card invisible behind Sources. Returns
    /// whether it moved the tab.
    pub(crate) fn sync_dock_tab(&mut self) -> bool {
        if self.dock_src_active && (self.clip_card_open() || self.export_open || self.transcript_open) {
            self.dock_src_active = false;
            crate::ui::dock_stance::save(false);
            return true;
        }
        false
    }

    /// What a right-click on the bench may do: refused only under a sheet that covers the room.
    /// A docked card stays as it is while the menu opens.
    pub(crate) fn menu_allowed(&mut self) -> bool {
        !self.covers_room()
    }

    /// Whether one of the dock's Clip-tab cards is up (every card but the
    /// export panel and Settings): the Clip tab then shows it in place of the
    /// verb list, and the dock switches to that tab on its own.
    pub(crate) fn clip_card_open(&self) -> bool {
        self.eq_open.is_some()
            || self.color_open.is_some()
            || self.transform_open.is_some()
            || self.speed_open.is_some()
            || self.effects_open.is_some()
            || self.silence_open.is_some()
            || self.beats_open.is_some()
            || self.mix_open
            || self.markers_open
            || self.history_open
            || self.tools.is_some()
            || self.subtitle_style_open
    }

    /// The pointer's way out of whatever card is up: what every scrim's press
    /// calls, so `esc` is *a* way out and never the only one. One list, and the
    /// same one [`Player::modal`] reads -- a card that can be counted there but
    /// not closed here is a card a hand alone cannot shut, which is what
    /// `every_card_closes_without_the_keyboard` fails on.
    ///
    /// Every card at once because only one is ever up (`export_open`): closing
    /// "the" card and closing all of them are the same act.
    pub(crate) fn close_card(&mut self) {
        // A clip card closes back to the Clip tab's list, never to Media.
        if self.clip_card_open() {
            self.dock_src_active = false;
        }
        self.keys_open = false;
        self.keys_filter.clear();
        self.settings_open = false;
        self.export_open = false;
        // A finished or failed face belongs to the export that made it; once
        // the card is closed the tab opens on fresh settings.
        if self.export.is_none() {
            self.export_sheet.done = None;
            self.export_sheet.failed = None;
        }
        self.transcript_open = false;
        self.transcript.search_edit = false;
        // The two things typed *into* the export moment go with it: a field
        // left open would take the next keystroke for a card that is gone.
        self.budget_edit = None;
        self.picker = None;
        self.eq_open = None;
        self.eq_dragging = false;
        self.color_open = None;
        self.transform_open = None;
        self.speed_open = None;
        self.speed_ramp_mode = false;
        self.ramp_key_speed = None;
        self.effects_open = None;
        self.effects_drag = None;
        self.effects_draft = None;
        self.effects_adding = false;
        // Marks and a running scan go with this one, which is why it is a call
        // and not an assignment ([`Player::close_silence`]).
        if self.silence_open.is_some() {
            self.close_silence();
        }
        self.close_beats();
        self.mix_open = false;
        self.markers_open = false;
        self.history_open = false;
        // A running listen or speak stops with the card that shows it.
        self.close_tools();
        // A name half-typed goes with the card it was typed into.
        self.marker_edit = None;
        self.subtitle_style_open = false;
        self.sub_anim_open = false;
    }

    /// Opening one of the Clip tab's sections: closes every card that is not itself a docked section
    /// (Settings, Export, markers, tools, caption style, silence, a menu) and leaves the sections
    /// that are open as they are, so several can be open at once (P7). [`Player::close_card`] stays
    /// the one way out of everything.
    pub(crate) fn close_other_cards(&mut self) {
        let kept = (
            self.eq_open.take(),
            self.color_open.take(),
            self.transform_open.take(),
            self.speed_open.take(),
            (self.speed_ramp_mode, self.ramp_key_speed),
            self.effects_open.take(),
            (self.effects_drag.take(), self.effects_draft.take(), self.effects_adding),
            self.mix_open,
        );
        self.close_card();
        self.eq_open = kept.0;
        self.color_open = kept.1;
        self.transform_open = kept.2;
        self.speed_open = kept.3;
        (self.speed_ramp_mode, self.ramp_key_speed) = kept.4;
        self.effects_open = kept.5;
        (self.effects_drag, self.effects_draft, self.effects_adding) = kept.6;
        self.mix_open = kept.7;
    }

    /// Escape's one door to every dismissible overlay the room can have up
    /// (`ui::stance::render`'s root `on_key_down`'s own escape branches,
    /// mirrored here exactly): a focus surface with its own escape branch
    /// (dock/inspector's `cycle_on_key_down`, the bench's handler) calls
    /// this FIRST, before its own ring-exit behavior, so an overlay opened
    /// by a click into that surface still closes on the first escape
    /// instead of the ring's `stop_propagation` swallowing the key before
    /// the root ever sees it (the "esc does not close menus opened by
    /// clicking in the clip section" report). Returns whether it closed
    /// something, so a caller only falls through to its own ring-exit
    /// behavior when nothing was open to close.
    pub(crate) fn escape_closes_overlay(&mut self, cx: &mut Context<Self>) -> bool {
        // Words being typed in a caption take escape first: they are given up and the card stays.
        if self.escape_cue_edit() {
            cx.notify();
            return true;
        }
        if self.card_open()
            || self.context_menu.is_some()
            || self.cue_menu.is_some()
            || self.library_menu.is_some()
            || self.picker.is_some()
            || self.about_open
        {
            self.about_open = false;
            // A popup over a tab goes alone, and so does Settings over the
            // Export tab: escape never takes the tab itself away.
            if self.context_menu.is_some() || self.cue_menu.is_some() || self.library_menu.is_some() || self.picker.is_some() {
                self.context_menu = None;
                self.cue_menu = None;
                self.library_menu = None;
                self.picker = None;
                cx.notify();
                return true;
            }
            if self.export_open && self.settings_open {
                self.settings_open = false;
                self.keys_filter.clear();
                cx.notify();
                return true;
            }
            if self.only_export_tab() {
                return true;
            }
            // The font list is a pick inside the style card: escape closes that list alone, and the
            // card (and its section) stays as it was.
            if self.subtitle_style_open && self.sub_font_open {
                self.sub_font_open = false;
                cx.notify();
                return true;
            }
            self.close_card();
            self.context_menu = None;
            self.library_menu = None;
            self.picker = None;
            cx.notify();
            return true;
        }
        if self.preview_session.is_some() {
            self.close_preview(cx);
            return true;
        }
        false
    }

    /// The one switch every param card's expand affordance and its `m`
    /// chord both flip (`dark_card_head`, [`Player::param_card_key`]):
    /// whichever card is open trades its usual capped, floating size for the
    /// bench/ledger rows' own room, in place -- layout only, no value on the
    /// card changes. Persisted like the dock tab pick so a maximized EQ is
    /// still maximized after the room closes and reopens.
    pub(crate) fn toggle_maximize(&mut self) {
        // A card that fills the room floats over the viewer, which the dock
        // design forbids (every card opens in the Clip tab): the switch stays
        // for the chord and the saved flag, but the card stays docked.
        self.card_maximized = false;
        ui::dock_stance::save_maximized(false);
    }

    /// Opens the equalizer on the selected clip. Audio only, and it says so
    /// rather than opening a card of bands that would reach nothing: a video
    /// clip carries no sound of its own here (the sound is the audio lane's),
    /// and the model would take the setting without anything ever playing it.
    pub(crate) fn open_eq(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // A caption the hand is on names the sound its group plays, and the
        // card opens on that; one in no group has no sound to equalize and
        // falls to the lane refusal below, in the words it always used.
        let anchor = match (self.selected.anchor(), self.session.as_ref()) {
            (Some((lane, idx)), Some(session)) if lane.kind == LaneKind::Subtitle => {
                caption_media_half(session, (lane, idx), LaneKind::Audio).or(self.selected.anchor())
            }
            _ => self.selected.anchor(),
        };
        // A video clip's sound is its linked audio half, so Alt+4 on a picked film opens on that.
        let anchor = match anchor {
            Some((lane, idx)) if lane.kind == LaneKind::Video => self
                .linked_partners((lane, idx))
                .into_iter()
                .find(|(l, _)| l.kind == LaneKind::Audio)
                .or(anchor),
            _ => anchor,
        };
        let refusal = match (anchor, &self.session) {
            (_, None) => Some("No timeline — no file open".to_string()),
            (None, _) => Some(format!(
                "Nothing selected · select all {}",
                self.keymap.display(ActionId::Select)
            )),
            (Some((lane, _)), _) if lane.kind != LaneKind::Audio => {
                Some("Not an audio clip — the equalizer works on sound".to_string())
            }
            _ => None,
        };
        if let Some(refusal) = refusal {
            self.notify_user(refusal.into());
            cx.notify();
            return;
        }
        let (lane, idx) = anchor.expect("checked above");
        let session = self.session.as_ref().expect("checked above");
        // What the clip already plays through, or the flat default -- so the
        // card opens on the curve that is in force and a reopen shows the last
        // drag rather than a fresh set of zeroes.
        self.eq_params = session
            .eq_of(lane, idx)
            .cloned()
            .unwrap_or_else(EqParams::default_layout);
        // One card at a time -- `close_card`'s own rule.
        self.close_other_cards();
        self.eq_band = 0;
        self.eq_dragging = false;
        self.eq_open = Some((lane, idx));
        self.context_menu = None;
        cx.notify();
    }

    /// Writes what the card is showing at its clip: one undo step, one entry in
    /// the append-only equalizer table, so this is called once per *gesture* --
    /// the end of a drag, a keystroke -- and never per pointer sample.
    ///
    /// A curve that moves nothing is stored as *no* equalizer at all, which is
    /// what keeps a clip nobody has touched on the identity path through
    /// playback and export (`engine::eq::EqParams::is_identity`).
    pub(crate) fn commit_eq(&mut self, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.eq_open else {
            return;
        };
        let params = (!self.eq_params.is_identity()).then(|| self.eq_params.clone());
        if let Some(session) = &mut self.session {
            // `set_eq` answers "the index was there and the curve is finite"
            // -- it snapshots and rewrites the very slot it already holds for
            // a curve that is already in force -- so on its own bool a
            // re-commit of the same curve armed autosave, the ledger's
            // `unsaved` ghost and the close prompt over a project whose bytes
            // had not moved. The arm rides the curve having moved, the
            // comparison `apply_fit` reads for `set_fit`
            // ([`Player::apply_fit`]), and then the door's own answer.
            let moved = session.eq_of(lane, idx).cloned() != params;
            if moved && session.set_eq(lane, idx, params) {
                self.mark_dirty();
                // `set_eq` reseeks inside the engine -- that is what makes the
                // change audible at once -- and a reseek is what these flags
                // are about. Nothing was written, so nothing was reseeked.
                self.reset_after_reseek();
            }
        }
        cx.notify();
    }

    /// Changes the picked band in place and says whether anything moved. Every
    /// edit of a band goes through here -- the drag, each key, each stepper
    /// button -- so the card has exactly one place that clamps a band into what
    /// the graph can draw, and no caller has to remember the limits.
    pub(crate) fn set_band(&mut self, change: impl FnOnce(&mut Band)) -> bool {
        let Some(band) = self.eq_params.bands.get_mut(self.eq_band) else {
            return false;
        };
        let was = *band;
        change(band);
        band.freq_hz = band.freq_hz.clamp(EQ_FREQ_LOW, EQ_FREQ_HIGH);
        band.gain_db = band.gain_db.clamp(-EQ_GAIN_LIMIT, EQ_GAIN_LIMIT);
        band.q = band.q.clamp(EQ_Q_LOW, EQ_Q_HIGH);
        *band != was
    }

    /// The keyboard's and the buttons' version of a drag: one step on the picked
    /// band, committed straight away -- neither has a release to wait for.
    pub(crate) fn nudge_band(&mut self, change: impl FnOnce(&mut Band), cx: &mut Context<Self>) {
        if self.set_band(change) {
            self.commit_eq(cx);
        }
    }

    /// Where the pointer sits in the graph, as the picked band's frequency and
    /// gain: across is the frequency axis and down is the gain one, so the
    /// handle follows the hand both ways rather than sliding up a rail. Called
    /// on every pointer sample, so the curve bends under it; the write is the
    /// release's ([`commit_eq`](Player::commit_eq)).
    pub(crate) fn drag_band(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = self.eq_graph.get();
        let gain = (0.5 - frac_down(at.y, bounds)) * 2. * EQ_GAIN_LIMIT;
        let freq = eq_freq(frac_along(at.x, bounds));
        if self.set_band(|b| {
            b.gain_db = gain;
            b.freq_hz = freq;
        }) {
            cx.notify();
        }
    }

    /// A band added beside the picked one, at the frequency with the most room
    /// around it ([`inserted_band`]), and picked so the next keystroke moves the
    /// band that was just made. Refused rather than silently ignored at the cap.
    pub(crate) fn add_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() >= EQ_BANDS_MAX {
            self.notify_user(format!("Equalizer full — {EQ_BANDS_MAX} bands").into());
            cx.notify();
            return;
        }
        let band = inserted_band(&self.eq_params.bands, self.eq_band);
        self.eq_band = (self.eq_band + 1).min(self.eq_params.bands.len());
        self.eq_params.bands.insert(self.eq_band, band);
        self.commit_eq(cx);
    }

    /// Takes the picked band out. The last one stays: an equalizer of no bands
    /// is a card with nothing to edit, and flattening is what "off" means here.
    pub(crate) fn remove_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() <= 1 {
            self.notify_user("Last band · flatten r".into());
            cx.notify();
            return;
        }
        self.eq_params.bands.remove(self.eq_band);
        self.eq_band = self.eq_band.min(self.eq_params.bands.len() - 1);
        self.commit_eq(cx);
    }

    /// Which band a press on the graph grabs: the nearest one along the
    /// frequency axis, so the whole box is the handle rather than a 10 px dot
    /// -- and a press that misses every dot still moves the band it is under.
    pub(crate) fn nearest_band(&self, x: Pixels) -> usize {
        let at = frac_along(x, self.eq_graph.get());
        self.eq_params
            .bands
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (eq_x(a.freq_hz) - at)
                    .abs()
                    .total_cmp(&(eq_x(b.freq_hz) - at).abs())
            })
            .map_or(0, |(i, _)| i)
    }

    /// Every param card's own key branch (EQ, Colour, Transform, Speed,
    /// Silence, Mix, Subtitle style), factored out of the legacy tree's
    /// `render.rs` handler so a second stance (`ui/stance.rs`) can drive the
    /// same cards without a second, divergent copy of this logic -- the
    /// defect class DESIGN §6's "drag while playing... `r` resets" was
    /// missing from the darkroom for. Returns `true` when a card was open
    /// and ate the key, the caller's cue to `cx.notify()` and stop exactly
    /// where each of these branches used to `return` inline.
    pub(crate) fn param_card_key(
        &mut self,
        key: &str,
        shift: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        // The one chord every param card answers to, wherever the pointer
        // toggle sits in `dark_card_head` -- mouse and key reach the same
        // switch (DESIGN's repeated "some options are only reachable via
        // keyboard shortcut" complaint). Checked once, ahead of the seven
        // card-local branches below, because it means the same thing in all
        // of them: none of the seven uses bare "m" for anything else.
        if key == "m"
            && (self.eq_open.is_some()
                || self.color_open.is_some()
                || self.transform_open.is_some()
                || self.speed_open.is_some()
                || self.effects_open.is_some()
                || self.silence_open.is_some()
                || self.mix_open
                || self.subtitle_style_open)
        {
            self.toggle_maximize();
            return true;
        }
        if self.eq_open.is_some() {
            if key == ESCAPE {
                // Maximized comes off before the card does -- "esc always
                // retreats" one step at a time (DESIGN.md:146).
                if self.card_maximized {
                    self.toggle_maximize();
                    return true;
                }
                // Nothing to undo: every change is already at the clip, and
                // undo is undo's own key.
                self.eq_open = None;
                self.eq_dragging = false;
            } else if key == "up" {
                self.nudge_band(|b| b.gain_db += EQ_STEP, cx);
            } else if key == "down" {
                self.nudge_band(|b| b.gain_db -= EQ_STEP, cx);
            } else if key == "left" && shift {
                self.nudge_band(|b| b.q /= EQ_Q_STEP, cx);
            } else if key == "right" && shift {
                self.nudge_band(|b| b.q *= EQ_Q_STEP, cx);
            } else if key == "left" {
                self.nudge_band(|b| b.freq_hz /= EQ_FREQ_STEP, cx);
            } else if key == "right" {
                self.nudge_band(|b| b.freq_hz *= EQ_FREQ_STEP, cx);
            } else if key == "r" {
                for band in &mut self.eq_params.bands {
                    band.gain_db = 0.;
                }
                self.commit_eq(cx);
            } else if key == "f" {
                // This one band back to flat, which is the undo of one hand
                // movement -- `r` is the undo of the whole card.
                self.nudge_band(|b| b.gain_db = 0., cx);
            } else if key == "a" {
                self.add_band(cx);
            } else if key == "x" {
                self.remove_band(cx);
            } else if key == "s" {
                // The analyser off and on. Nothing is committed: it is what
                // the card *shows*, so it survives no further than this
                // window.
                self.eq_spectrum = !self.eq_spectrum;
            } else if let Ok(digit) = key.parse::<usize>() {
                // As the keys are laid out: 1-9 then 0 for the tenth, which
                // is the cap ([`EQ_BANDS_MAX`]). A digit past the last band
                // picks nothing rather than panics.
                let band = match digit {
                    0 => EQ_BANDS_MAX - 1,
                    n => n - 1,
                };
                if band < self.eq_params.bands.len() {
                    self.eq_band = band;
                }
            } else {
                return false;
            }
            return true;
        }
        // The colour card owns the keyboard the same way the export card
        // does, and its keys mean nothing outside it: the arrows pick a
        // slider and move it, and `r` takes the grade off. Not keymap
        // bindings for exactly that reason -- see `FIXED`, where the keys
        // menu still lists them.
        if self.color_open.is_some() {
            match color_key(key) {
                Some(ColorKey::Close) if self.card_maximized => self.toggle_maximize(),
                Some(ColorKey::Close) => {
                    self.color_open = None;
                    self.color_dragging = false;
                    self.color_wheel_drag = None;
                }
                Some(ColorKey::Band(step)) => {
                    let bands = section_bands(self.color_section);
                    let n = bands.len();
                    let at = self.color_band.saturating_sub(bands.start).min(n - 1);
                    // `up` arrives as the last band of the four; walk this section's own.
                    let step = if step == COLOR_BANDS.len() - 1 { n - 1 } else { step };
                    self.color_band = bands.start + (at + step) % n;
                }
                Some(ColorKey::Nudge(steps)) => self.nudge_color(steps, cx),
                Some(ColorKey::Reset) => {
                    self.set_color(ColorParams::default(), cx);
                }
                None => return false,
            }
            return true;
        }
        // The transform card, the same way again: the arrows pick a slider
        // and move it, and `r` puts the placement back to identity.
        // Card-local, [`ActionId::Color`]'s own reason (see `FIXED`, where
        // the keys menu still lists them).
        if self.transform_open.is_some() {
            if key == ESCAPE && self.card_maximized {
                self.toggle_maximize();
            } else if key == ESCAPE {
                self.transform_open = None;
                self.transform_dragging = false;
            } else if key == "down" {
                self.move_transform_band(true);
            } else if key == "up" {
                self.move_transform_band(false);
            } else if key == "right" {
                self.nudge_transform(1., cx);
            } else if key == "left" {
                self.nudge_transform(-1., cx);
            } else if key == "r" {
                self.set_transform(TransformParams::default(), cx);
            } else {
                return false;
            }
            return true;
        }
        // The speed card, the same way again: its arrows move the rate and
        // `r` puts it back to real time, and neither means anything outside
        // the card -- so neither is a binding (see `FIXED`, where the keys
        // menu still lists them).
        if self.speed_open.is_some() {
            match color_key(key) {
                Some(ColorKey::Close) if self.card_maximized => self.toggle_maximize(),
                Some(ColorKey::Close) => {
                    self.speed_open = None;
                    self.speed_dragging = false;
                }
                // The card has one value, so the pair that picks a slider on
                // the colour card moves this one by a whole preset's worth
                // instead of a step.
                Some(ColorKey::Band(step)) => self.nudge_speed(if step == 1 { -2 } else { 2 }, cx),
                Some(ColorKey::Nudge(steps)) => self.nudge_speed(steps as i32, cx),
                Some(ColorKey::Reset) => self.set_speed(Speed::NORMAL, cx),
                None => return false,
            }
            return true;
        }
        // The silence card, the same way again: the arrows pick one of its
        // rows and move it, its two apply keys are the two things it can do
        // to the timeline, and `k` flips which half of the sound they act
        // on. Card-local, every one of them -- and listed in the keys menu
        // (keymap.rs `FIXED`), because a key that cuts forty places at once
        // is not a secret.
        if self.silence_open.is_some() {
            if key == ESCAPE && self.card_maximized {
                self.toggle_maximize();
            } else if key == ESCAPE {
                // Nothing to undo: a preview is not an edit.
                self.close_silence();
            } else if key == "down" {
                self.step_silence_field(1);
            } else if key == "up" {
                self.step_silence_field(-1);
            } else if key == "right" {
                self.nudge_silence(1);
            } else if key == "left" {
                self.nudge_silence(-1);
            } else if key == "enter" {
                self.cut_silences(cx);
            } else if key == "f" {
                self.speed_silences(cx);
            } else if key == "k" {
                self.toggle_silence_keep_only();
                cx.notify();
            } else {
                return false;
            }
            return true;
        }
        // The mix card, the same way again: up/down pick a row -- a track's
        // fader, the limiter's ceiling or its switch -- and left/right move
        // it, held or pressed. Card-local like the four above it.
        // The effects card: its bars and buttons are pointer-driven, so a key only
        // leaves it, maximized first.
        if self.effects_open.is_some() {
            if key == ESCAPE && self.card_maximized {
                self.toggle_maximize();
            } else if key == ESCAPE {
                self.close_card();
            } else {
                return false;
            }
            return true;
        }
        // The markers card: up/down pick a marker, enter retypes its name.
        if self.markers_open {
            return self.markers_card_key(key, shift, cx);
        }
        if self.tools.is_some() {
            return self.tools_card_key(key, cx);
        }
        if self.mix_open {
            let rows = self.mix_lanes().len() + MIX_MASTER_ROWS;
            if key == ESCAPE && self.card_maximized {
                self.toggle_maximize();
            } else if key == ESCAPE {
                self.mix_open = false;
            } else if key == "down" {
                self.mix_field = (self.mix_field + 1) % rows;
            } else if key == "up" {
                self.mix_field = (self.mix_field + rows - 1) % rows;
            } else if key == "right" {
                self.nudge_mix(1, cx);
            } else if key == "left" {
                self.nudge_mix(-1, cx);
            } else {
                return false;
            }
            return true;
        }
        // The subtitle style card, the same way again: up/down walk its rows
        // (the size, the three chip rows, the platform default, then every
        // family in `subtitle_fonts`), left/right moves the picked row, and
        // `enter` steps a chip row or picks a family -- since a hand with no
        // mouse still has to be able to leave the platform default.
        if self.subtitle_style_open {
            let rows = SUB_ROW_FIRST_FAMILY + self.subtitle_fonts.len();
            let field = self.subtitle_style_field;
            self.sub_keys_used = true;
            if key == ESCAPE && self.card_maximized {
                self.toggle_maximize();
            } else if key == ESCAPE {
                self.subtitle_style_open = false;
                self.sub_anim_open = false;
            } else if key == "down" {
                self.subtitle_style_field = sub_row_step(&self.subtitle_fonts, rows, field, true);
            } else if key == "up" {
                self.subtitle_style_field = sub_row_step(&self.subtitle_fonts, rows, field, false);
            } else if key == "right" {
                self.step_sub_field(1, cx);
            } else if key == "left" {
                self.step_sub_field(-1, cx);
            } else if key == "enter" && field == SUB_ROW_DEFAULT {
                self.set_sub_family(None, cx);
            } else if key == "enter" && (SUB_ROW_OUTLINE..SUB_ROW_DEFAULT).contains(&field) {
                self.step_sub_field(1, cx);
            } else if key == "enter" && field >= SUB_ROW_FIRST_FAMILY {
                let family = self.subtitle_fonts[field - SUB_ROW_FIRST_FAMILY].clone();
                self.set_sub_family(Some(family), cx);
            } else {
                return false;
            }
            return true;
        }
        // The Clip tab's colour field (`Player::edit_viz_hex`): the
        // transition row's own keyboard door, and its free-text half --
        // checked here, beside it, for its reason (the Clip tab is always on
        // screen, not modal). Every printable stroke lands in the buffer, so
        // a letter that is also a room chord types into the field while it is
        // open, exactly as the dock's source filter does.
        if self.viz_hex_edit.is_some() {
            match key {
                ESCAPE => self.viz_hex_edit = None,
                "enter" => self.commit_viz_hex(cx),
                "backspace" => {
                    if let Some(edit) = self.viz_hex_edit.as_mut() {
                        edit.backspace();
                    }
                }
                _ => match typed(key) {
                    Some(c) => {
                        if let Some(edit) = self.viz_hex_edit.as_mut() {
                            edit.typed(c);
                        }
                    }
                    None => return false,
                },
            }
            return true;
        }
        // The Clip tab's transition length field: typed digits, `.`, Enter and
        // Escape, checked first since it can be open while no card is (the Clip
        // tab is always on screen, not modal).
        if self.transition_edit.is_some() && self.transition_key(key, cx) {
            return true;
        }
        // The settings page: card-local like the colour and transform
        // branches above, so escape still closes the page and every other key
        // falls through to the room.
        if self.settings_open {
            // The arrows walk the choice rows and step the focused one
            // ([`crate::ui::settings_stance::settings_arrow`]); an open list
            // keeps them for itself.
            if self.picker.is_none() && crate::ui::settings_stance::settings_arrow(self, key, cx) {
                return true;
            }
            match key {
                // The Level row's own three, and they have to be *here*:
                // this page counts as a card in [`Self::card_open`], so a
                // stroke that falls through this branch is swallowed by the
                // darkroom's modal guard rather than reaching the keymap --
                // `m` did nothing at all with the page up (caught in the
                // harness, not by a guard). Same setter the keymap's
                // [`ActionId::ToggleMute`]/`VolumeUp`/`VolumeDown` call, so
                // the row cannot drift from the chord it wears.
                "m" => self.set_volume(|volume| volume.muted = !volume.muted, cx),
                "=" => self.set_volume(|volume| volume.step(true), cx),
                "-" => self.set_volume(|volume| volume.step(false), cx),
                _ => return false,
            }
            return true;
        }
        // The export moment: its own chords, and every other stroke
        // swallowed while it is up -- a chord this surface consumes must
        // never also reach the room's keymap underneath it
        // ([`Player::export_moment_key`]).
        if self.export_moment_key(key, shift, cx) {
            return true;
        }
        false
    }
}
