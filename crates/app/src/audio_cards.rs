//! The equalizer, speed, silence and mixer cards' numbers.

use crate::*;

/// How far a band may be pushed either way, in dB. The engine clamps nothing
/// here -- it will filter whatever it is given -- so this is a UI decision:
/// past about this a peaking band stops sounding like tone and starts sounding
/// like a fault.
pub(crate) const EQ_GAIN_LIMIT: f32 = 12.;

/// One dB per keystroke, which is roughly the smallest step anyone hears on a
/// single band.
pub(crate) const EQ_STEP: f32 = 1.;

/// A twentieth of real time per keystroke, in the thousandths a [`Speed`] is
/// held in: fine enough to creep up on a rate, coarse enough that the whole
/// range is eighty presses and not eight hundred -- and it divides 1000, so
/// stepping from anywhere lands on exactly 1.00x on the way past.
pub(crate) const SPEED_STEP: i32 = 50;

/// The rates the card's buttons offer, so the ones people actually name are one
/// click and not a drag. Real time is among them: it is the reset.
pub(crate) const SPEED_PRESETS: [u16; 6] = [250, 500, 1000, 1500, 2000, 4000];

/// A rate from a number of thousandths that may have run off either end -- what
/// a keystroke and a drag both produce. Clamped, not refused: a hand pushing
/// past the limit means "as far as it goes", exactly as a trim does.
pub(crate) fn speed_at(permille: i32) -> Speed {
    Speed::from_permille(permille.clamp(0, i32::from(u16::MAX)) as u16)
}

/// How far a ramp key's speed moves per press of its stepper: a quarter of real
/// time, the same quarter the bar's lowest rate is.
pub(crate) const RAMP_KEY_STEP: i32 = 250;

/// The ramp shapes the speed card and the palette offer, each laid across the clip's
/// own length on the timeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RampPreset {
    /// Real time easing up to double.
    Up,
    /// Double easing down to real time.
    Down,
    /// Slow, then fast through the middle, then slow: the classic speed ramp.
    SlowFastSlow,
    /// Fast, then slow through the middle, then fast: the hero shot.
    FastSlowFast,
}

impl RampPreset {
    pub(crate) const ALL: [RampPreset; 4] = [
        RampPreset::Up,
        RampPreset::Down,
        RampPreset::SlowFastSlow,
        RampPreset::FastSlowFast,
    ];

    /// The chip's face on the card.
    pub(crate) fn label(self) -> &'static str {
        match self {
            RampPreset::Up => "Speed up",
            RampPreset::Down => "Slow down",
            RampPreset::SlowFastSlow => "Fast in the middle",
            RampPreset::FastSlowFast => "Slow in the middle",
        }
    }

    /// The keys of the shape over a clip `frames` timeline frames long. `None` when the
    /// clip is too short to hold the shape's keys one frame apart.
    pub(crate) fn keys(self, frames: u32) -> Option<Vec<engine::project::SpeedKey>> {
        let key = |at: u32, permille: u16| engine::project::SpeedKey {
            at,
            speed: Speed::from_permille(permille),
        };
        match self {
            RampPreset::Up => (frames >= 1).then(|| vec![key(0, 1000), key(frames, 2000)]),
            RampPreset::Down => (frames >= 1).then(|| vec![key(0, 2000), key(frames, 1000)]),
            RampPreset::SlowFastSlow => (frames >= 2)
                .then(|| vec![key(0, 500), key(frames / 2, 2000), key(frames, 500)]),
            RampPreset::FastSlowFast => (frames >= 2)
                .then(|| vec![key(0, 2000), key(frames / 2, 500), key(frames, 2000)]),
        }
    }
}

/// `keys` with a key of `speed` at clip frame `at`: one already there is replaced,
/// else the new one is slotted in order. `None` when the ramp is full
/// ([`engine::project::MAX_SPEED_KEYS`]) and `at` is a new frame.
pub(crate) fn ramp_with_key(
    keys: &[engine::project::SpeedKey],
    at: u32,
    speed: Speed,
) -> Option<Vec<engine::project::SpeedKey>> {
    let mut out = keys.to_vec();
    match out.iter().position(|k| k.at >= at) {
        Some(i) if out[i].at == at => out[i].speed = speed,
        _ if out.len() >= engine::project::MAX_SPEED_KEYS => return None,
        Some(i) => out.insert(i, engine::project::SpeedKey { at, speed }),
        None => out.push(engine::project::SpeedKey { at, speed }),
    }
    Some(out)
}

/// `keys` without the `i`th; the last key out leaves no ramp at all.
pub(crate) fn ramp_without_key(
    keys: &[engine::project::SpeedKey],
    i: usize,
) -> Vec<engine::project::SpeedKey> {
    let mut out = keys.to_vec();
    if i < out.len() {
        out.remove(i);
    }
    out
}

/// `keys` with the `i`th key's speed moved by `steps` of [`RAMP_KEY_STEP`], clamped to
/// what a [`Speed`] holds.
pub(crate) fn ramp_key_stepped(
    keys: &[engine::project::SpeedKey],
    i: usize,
    steps: i32,
) -> Vec<engine::project::SpeedKey> {
    let mut out = keys.to_vec();
    if let Some(k) = out.get_mut(i) {
        k.speed = speed_at(i32::from(k.speed.permille()) + steps * RAMP_KEY_STEP);
    }
    out
}

/// The speed curve of a ramp as the card draws it: `n + 1` points across the clip's
/// `frames`, x the fraction of the way along and y the speed's place on the speed bar
/// (log scale, real time mid-height), so the card's curve and its bar read alike. A
/// clip without a ramp draws its constant speed as a level line.
pub(crate) fn ramp_curve(
    keys: &[engine::project::SpeedKey],
    constant: Speed,
    frames: u32,
    n: usize,
) -> Vec<(f32, f32)> {
    let n = n.max(1);
    (0..=n)
        .map(|i| {
            let x = i as f32 / n as f32;
            let at = (x * frames as f32).round() as u32;
            let speed = match keys.is_empty() {
                true => constant,
                false => engine::project::ramp_speed_at(keys, at),
            };
            (x, crate::player::cards::speed_fraction(f32::from(speed.permille())))
        })
        .collect()
}

/// A clip's speed as its box wears it: `2×` for a constant one, the ramp's keys joined
/// by arrows (`1×→2×→1×`) for a ramp, and only its two ends and a count once it has more
/// than three keys. `None` for a clip at real time with no ramp.
pub(crate) fn speed_chip_text(clip: &Clip) -> Option<String> {
    if clip.speed_keys.is_empty() {
        return (!clip.speed.is_normal()).then(|| crate::ui::cards::speed_text(clip.speed));
    }
    let at = |k: &engine::project::SpeedKey| crate::ui::cards::speed_text(k.speed);
    let keys = &clip.speed_keys;
    Some(match keys.as_slice() {
        [only] => at(only),
        [a, b] => format!("{}→{}", at(a), at(b)),
        [a, b, c] => format!("{}→{}→{}", at(a), at(b), at(c)),
        [first, .., last] => format!("{}→{} ({} keys)", at(first), at(last), keys.len()),
        [] => return None,
    })
}

/// The silence card's rows, in the order it lists them: how wide the apply
/// reaches, the threshold and the unit it is read in, the three durations a
/// scan is told, and the rate the speed-up plays at. What `silence_field`
/// indexes and what [`Player::nudge_silence`] moves.
pub(crate) const SILENCE_ROWS: usize = 7;

/// How wide a jumpcut reaches. A ripple used to be the whole timeline's
/// business and nothing else; it is a *choice* now, because a podcast track's
/// silences are not the music track's business -- and the choice has to be on
/// screen, because "everything after this moved" is not a thing to discover
/// afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// The lanes of the take the scanned clip belongs to: its picture and its
    /// sound stay one take, and nothing else moves. The default, because a clip
    /// picked on screen is what a person means by "this".
    Take,
    /// That clip's lane, alone. Refused by the engine, by name, while the take
    /// has a half elsewhere -- detaching is how a person says they mean it.
    Track,
    /// Every lane, which is what a ripple always was and what the timeline-wide
    /// jumpcut still is.
    Everything,
}

/// The order the card cycles them in.
pub(crate) const SCOPES: [Scope; 3] = [Scope::Take, Scope::Track, Scope::Everything];

impl Scope {
    /// What the row says, given the lanes it works out to: the *names* of the
    /// tracks, because "this take" means nothing until it says which two.
    pub(crate) fn label(self, lanes: &[Lane]) -> String {
        let named = lanes
            .iter()
            .map(|l| l.label())
            .collect::<Vec<_>>()
            .join("+");
        match self {
            Scope::Take => format!("this clip ({named})"),
            Scope::Track => format!("this track ({named})"),
            Scope::Everything => "the whole timeline".to_string(),
        }
    }
}

/// One press of a nudge key on each kind of row: a dB on the threshold, a
/// twentieth of a second on the three durations, and the speed card's own step
/// on the rate.
pub(crate) const SILENCE_DB_STEP: f32 = 1.;
pub(crate) const SILENCE_SECS_STEP: f64 = 0.05;

/// How far each of them may be pushed. UI decisions, all of them: the engine
/// takes any finite number, but a forgiveness of ten seconds finds nothing in a
/// talking head. The threshold reaches full scale, which calls a whole take
/// silent -- that is a thing someone may want to ask for (the preview on the
/// lane says what it would cost before anything is cut), so the top is 0 rather
/// than a number this card picked for them.
pub(crate) const SILENCE_DB_RANGE: (f32, f32) = (-80., 0.);
pub(crate) const SILENCE_SECS_RANGE: (f64, f64) = (0., 5.);

/// One press of a nudge key on the mix card, in dB: the step every fader and
/// the limiter's ceiling moves by. A whole decibel, the smallest move anyone
/// hears as a move -- and it lands on round numbers, so a track set by ear
/// still reads as a number a person would say.
pub(crate) const MIX_DB_STEP: f32 = 1.;

/// The mix card's rows below the faders: the limiter's ceiling and its switch.
/// One fader per audio track comes first, however many there are.
pub(crate) const MIX_MASTER_ROWS: usize = 2;

/// The speed-up rate is bounded *below* by real time: a "speed-up" that slows
/// the silence down would make the timeline longer, which is the one thing
/// neither button may do. The top is [`Speed::MAX`].
pub(crate) fn silence_rate(permille: i32) -> Speed {
    speed_at(permille.clamp(
        i32::from(Speed::NORMAL.permille()) + SPEED_STEP,
        i32::from(Speed::MAX.permille()),
    ))
}

/// The graph's frequency axis: the range an ear works in, and the range every
/// band a file can carry sits inside. Log-spaced, so an octave is an octave
/// wherever it falls.
pub(crate) const EQ_FREQ_LOW: f32 = 20.;
pub(crate) const EQ_FREQ_HIGH: f32 = 20_000.;

/// The curve box, docked: a 140 px plot with the knob room above and below it
/// (the strip for "Play to see sound" over the plot, the frequency labels under).
/// Tall enough that 1 dB is a visible move at the ±12 dB axis, and the card scrolls
/// in a 360 px window.
pub(crate) const EQ_GRAPH_H: f32 = 182.;

/// The curve box maximized: 1 dB and a semitone-ish band of frequency both
/// read as more pixels than the docked floor spends on them -- the same
/// curve, the same ±12 dB axis and the same 20 Hz-20 kHz sweep, just spread
/// over more of the vertical resolution a maximized card borrows from the
/// bench/ledger rows below the picture ([`crate::ui::stance::below_picture_floor`]).
pub(crate) const EQ_GRAPH_MAX_H: f32 = 320.;

/// The graph's own height for the card's current size: the docked floor, or
/// the maximized card's own taller box -- never a CSS scale of the same
/// pixels, an actually taller curve with more room per decibel and per
/// decade.
pub(crate) fn eq_graph_h(maximized: bool) -> f32 {
    if maximized {
        EQ_GRAPH_MAX_H
    } else {
        EQ_GRAPH_H
    }
}

/// How wide the equalizer card is allowed to get. It is the one card that is a
/// *graph*: every pixel across is frequency resolution, and at the 320 px the
/// other cards use, a third of an octave was a couple of pixels. Past this the
/// curve stops gaining anything and the card starts reading as a wall.
pub(crate) const EQ_W_MAX: f32 = 720.;

/// The gap the card leaves either side of it, so it reads as a card on a scrim
/// rather than as a second window: it takes the width it can get inside that.
pub(crate) const EQ_W_MARGIN: f32 = 32.;

/// How many bands one clip's equalizer may carry from this card. Ten because
/// the keyboard picks a band with a digit and a keyboard has ten of them --
/// past that a band would be reachable by pointer only. The engine itself caps
/// nothing (`EqParams::bands` is a plain `Vec`), so a file may still carry more
/// and this card will draw and edit every one it finds.
pub(crate) const EQ_BANDS_MAX: usize = 10;

/// One press of the frequency keys, as a factor: a sixth of an octave, so a
/// band walks the whole axis in about sixty presses and still lands close
/// enough to a named frequency to aim at one.
pub(crate) const EQ_FREQ_STEP: f32 = 1.122_462;

/// One press of the Q keys, as a factor, and the range they move in. Below the
/// bottom a peak is barely a peak any more; above the top it is a whistle on
/// one frequency. 0.707 -- the flat-shelf value, and the default -- sits inside
/// them, so nothing a file carries has to be dragged into range first.
pub(crate) const EQ_Q_STEP: f32 = 1.25;
pub(crate) const EQ_Q_LOW: f32 = 0.3;
pub(crate) const EQ_Q_HIGH: f32 = 12.;

/// How many points the curve is drawn from. One per ~3 px across the card:
/// past that the line is smooth and the extra biquad evaluations are wasted.
pub(crate) const EQ_CURVE_STEPS: usize = 96;

/// A band's handle on the curve. Only the dot -- what is *grabbed* is the whole
/// graph (the nearest band along the frequency axis), so the target is the box.
pub(crate) const EQ_HANDLE: f32 = 10.;

/// A band's hit area on the curve: 24 px ([`crate::layout::HIT_MIN`], WCAG
/// 2.5.8) centred on the dot, so the point is found by aim alone. The dot is
/// drawn [`EQ_HANDLE`] wide inside it; the rest is a target, not a drawing.
pub(crate) const EQ_HIT: f32 = crate::layout::HIT_MIN;

/// The frequencies the graph names, so the curve can be read as a curve *of
/// something*. The two ends label themselves at the edges.
pub(crate) const EQ_TICKS: [(f32, &str); 5] = [
    (20., "20 Hz"),
    (100., "100"),
    (1000., "1k"),
    (10000., "10k"),
    (20000., "20k"),
];

/// The gains the graph rules a line across, besides the 0 dB one it already
/// had: half way to each limit, so a boost can be read as "about six" without
/// counting pixels. The limits themselves are the box's own edges and are
/// named at the corners instead.
pub(crate) const EQ_DB_GRID: [f32; 2] = [6., -6.];

/// How wide an end tick's own label box has to be so "20 Hz" reads as one
/// line rather than wrapping onto two ("20" over "Hz") -- driven live and
/// found doing exactly that at a fixed 24 px box once the end ticks were
/// anchored to the graph's edge instead of centred on it (the anchor alone
/// was not the whole fix). Sized off the label's own character count at the
/// size it is actually drawn, using the list-truncation ladder's already
/// -calibrated characters-to-pixels ratio ([`crate::layout::LIST_CHAR_W`] at
/// [`crate::ui::type_scale::CHORD_METADATA_MIN_PX`]) rather than a second
/// magic pixel count -- so a future type-scale bump (this one already moved
/// once this session) widens the box along with the text instead of
/// re-wrapping it.
pub(crate) fn eq_tick_end_w(label: &str, text_px: f32) -> f32 {
    label.chars().count() as f32 * crate::layout::LIST_CHAR_W
        / crate::ui::type_scale::CHORD_METADATA_MIN_PX
        * text_px
}

#[cfg(test)]
mod eq_graph_maximize_tests {
    use super::*;

    /// D2's regression class: the maximized card revealed LESS than docked
    /// at the same bench, because the graph claimed a fixed slice (a flat
    /// `EQ_GRAPH_MAX_H`) that left nothing for the rows below it, forcing a
    /// scroll docked never needed at the same bench. This binary has no
    /// `TestAppContext`, so it cannot read the flex layout gpui actually
    /// paints back -- this is a value-level pin on the two numbers the fix
    /// (`crates/app/src/ui/cards.rs`'s `graph_maximized` branch, `flex_1()`
    /// between a floor and a ceiling rather than a fixed height) depends on,
    /// not a substitute for driving a maximize at a small bench: the
    /// maximized graph's floor must be the *same* constant docked draws its
    /// fixed height at (so maximized can never show less graph than docked,
    /// whatever room is left over for the graph to grow into), and that
    /// floor must not exceed its own ceiling (an inverted `min_h`/`max_h`
    /// clamp is a layout-time panic, not a compile-time one).
    #[test]
    fn maximized_graph_floor_matches_docked_and_never_exceeds_its_own_ceiling() {
        assert_eq!(
            eq_graph_h(false),
            EQ_GRAPH_H,
            "docked height must be the floor maximized also uses"
        );
        const { assert!(EQ_GRAPH_H <= EQ_GRAPH_MAX_H) };
    }
}

#[cfg(test)]
mod eq_tick_end_w_tests {
    use super::*;

    /// The defect measured live: a fixed 24 px box wrapped "20 Hz" (5 chars)
    /// at the dark theme's 13 px end-tick size. The derived width must clear
    /// that box, and must grow, not shrink, if a future scale bump raises
    /// the size fed in.
    #[test]
    fn wide_enough_for_the_widest_end_label_and_grows_with_the_type_scale() {
        let dark_w = eq_tick_end_w("20 Hz", crate::ui::type_scale::CHORD_METADATA_MIN_PX);
        assert!(dark_w > 24., "{dark_w} must clear the old fixed box");
        let bumped_w = eq_tick_end_w("20 Hz", crate::ui::type_scale::CHORD_METADATA_MIN_PX + 4.);
        assert!(bumped_w > dark_w);
    }
}

/// How many played samples one spectrum frame is transformed from. A power of
/// two ([`fft`] is radix-2) and the whole of the engine's tap: 1024 at 48 kHz
/// is a 47 Hz bin, fine enough that the bass end is a shape and short enough
/// (21 ms) that the analyser moves with the music.
pub(crate) const EQ_FFT: usize = 1024;

/// The level range the analyser is drawn across, floor to ceiling in dBFS: the
/// bottom of the box is silence and the top is a bin at -12 dBFS, which is
/// about where a mixed track's loudest band sits. A look, not a measurement --
/// the numbers on the axis are the curve's dB, never the analyser's.
pub(crate) const EQ_SPECTRUM_DB: (f32, f32) = (-96., -12.);
