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
            Scope::Take => format!("this take ({named})"),
            Scope::Track => format!("this track ({named})"),
            Scope::Everything => "every track".to_string(),
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

/// The curve box. Tall enough that 1 dB is a visible move at the ±12 dB axis,
/// and short enough that the card still fits a 360 px window.
pub(crate) const EQ_GRAPH_H: f32 = 132.;

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
