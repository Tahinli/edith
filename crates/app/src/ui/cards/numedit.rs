//! The EQ band's number fields: drag to scrub, click to type.
//!
//! The edit state is one pair of cells per window thread, since only one number is ever being
//! pressed or typed at a time; `typing` is what the key handler asks before the keymap sees a
//! stroke.

use super::tools::drag_catcher;
use crate::player::text_field::FieldKey;
use crate::ui::kit::{self, NumberEdit};
use crate::ui::widgets::{slider_key, slider_typing};
use crate::*;
use std::cell::RefCell;

/// The numbers this module edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumTarget {
    Freq,
    Gain,
    Width,
}

impl NumTarget {
    fn range(self) -> (f64, f64) {
        match self {
            Self::Freq => (f64::from(EQ_FREQ_LOW), f64::from(EQ_FREQ_HIGH)),
            Self::Gain => (-f64::from(EQ_GAIN_LIMIT), f64::from(EQ_GAIN_LIMIT)),
            Self::Width => (f64::from(EQ_Q_LOW), f64::from(EQ_Q_HIGH)),
        }
    }

    fn per_px(self) -> f64 {
        match self {
            Self::Gain => 0.1,
            Self::Width => 0.01,
            // Unused: a frequency scrubs by ratio ([`scrub`]).
            Self::Freq => 0.,
        }
    }

    /// The number as the field shows and the buffer opens on it.
    pub(crate) fn shown(self, v: f64) -> String {
        match self {
            Self::Freq if v.round() < 1000. => format!("{v:.0}"),
            // kHz from 1 kHz up, always one decimal: "1.0", "12.5".
            Self::Freq => format!("{:.1}", v / 1000.),
            // A real minus sign, the same one the axis labels carry.
            Self::Gain => format!("{v:+.1}").replace('-', "\u{2212}"),
            Self::Width => format!("{v:.2}"),
        }
    }

    /// The unit the field wears beside [`Self::shown`]: a frequency reads in kHz from 1 kHz up.
    pub(crate) fn unit(self, v: f64) -> &'static str {
        match self {
            Self::Freq if v.round() >= 1000. => "kHz",
            Self::Freq => "Hz",
            Self::Gain => "dB",
            Self::Width => "Q",
        }
    }

    /// What the buffer opens on: a frequency as whole Hz (a bare number types as Hz).
    fn typed(self, v: f64) -> String {
        match self {
            Self::Freq => format!("{v:.0}"),
            _ => self.shown(v).replace('\u{2212}', "-"),
        }
    }

    /// The value a drag of `dx` pixels from `v0` lands on: a frequency moves by octaves (an
    /// octave per 50 px, a tenth of that fine), the rest by a fixed amount a pixel.
    pub(crate) fn scrub(self, v0: f64, dx: f32, fine: bool) -> f64 {
        let (lo, hi) = self.range();
        match self {
            Self::Freq => {
                let per = if fine { 0.002 } else { 0.02 };
                (v0 * (f64::from(dx) * per).exp2()).clamp(lo, hi)
            }
            _ => kit::scrub_value(v0, dx, self.per_px(), fine, lo, hi),
        }
    }

    fn get(self, p: &Player) -> f64 {
        let band = p.eq_params.bands.get(p.eq_band);
        match self {
            Self::Freq => band.map_or(1000., |b| f64::from(b.freq_hz)),
            Self::Gain => band.map_or(0., |b| f64::from(b.gain_db)),
            Self::Width => band.map_or(1., |b| f64::from(b.q)),
        }
    }

    fn set(self, p: &mut Player, v: f64) -> bool {
        match self {
            Self::Freq => p.set_band(|b| b.freq_hz = v as f32),
            Self::Gain => p.set_band(|b| b.gain_db = v as f32),
            Self::Width => p.set_band(|b| b.q = v as f32),
        }
    }
}

#[derive(Clone, Copy)]
struct Drag {
    target: NumTarget,
    x0: f32,
    v0: f64,
    moved: bool,
}

#[derive(Default)]
struct State {
    drag: Option<Drag>,
    edit: Option<(NumTarget, NumberEdit)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Whether a number is being typed: the keyboard is the field's until Enter or Escape. A slider's
/// value typed in on a card counts too, since the key handler asks here for both.
pub(crate) fn typing() -> bool {
    STATE.with(|s| s.borrow().edit.is_some()) || slider_typing()
}

/// Drops any press or edit (the card closed under it).
pub(crate) fn cancel() {
    STATE.with(|s| *s.borrow_mut() = State::default());
}

/// A number field for `target` showing `value`; a press scrubs on drag and types on click.
pub(crate) fn field(
    id: &'static str,
    label: &'static str,
    target: NumTarget,
    value: f64,
    cx: &mut Context<Player>,
) -> Stateful<Div> {
    let editing = STATE.with(|s| s.borrow().edit.clone().filter(|(t, _)| *t == target).map(|(_, e)| e));
    let unit = if editing.is_some() && target == NumTarget::Freq { "Hz" } else { target.unit(value) };
    kit::number_field(id, 96., target.shown(value), Some(unit), editing.as_ref(), true)
        .tooltip(crate::ui::widgets::overlay_tip_hover(label, "", Some("Drag to change, click to type")))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let v0 = target.get(this);
                STATE.with(|s| {
                    s.borrow_mut().drag = Some(Drag { target, x0: f32::from(event.position.x), v0, moved: false })
                });
                cx.notify();
            }),
        )
}

/// The window-wide catcher that carries a press on one of the fields through its drag.
pub(crate) fn catcher(cx: &mut Context<Player>) -> impl IntoElement {
    let live = STATE.with(|s| s.borrow().drag.is_some());
    drag_catcher(cx.entity(), live, move_to, release)
}

fn move_to(this: &mut Player, x: f32, fine: bool, cx: &mut Context<Player>) {
    let Some(d) = STATE.with(|s| s.borrow().drag) else {
        return;
    };
    let dx = x - d.x0;
    if !d.moved && dx.abs() < 3. {
        return;
    }
    STATE.with(|s| s.borrow_mut().drag = Some(Drag { moved: true, ..d }));
    if d.target.set(this, d.target.scrub(d.v0, dx, fine)) {
        cx.notify();
    }
}

fn release(this: &mut Player, cx: &mut Context<Player>) {
    let Some(d) = STATE.with(|s| s.borrow_mut().drag.take()) else {
        return;
    };
    if d.moved {
        this.commit_eq(cx);
    } else {
        let shown = d.target.typed(d.target.get(this));
        STATE.with(|s| s.borrow_mut().edit = Some((d.target, NumberEdit::begin(&shown))));
    }
    cx.notify();
}

/// One key while a number is typed: Enter writes it, Escape drops it.
pub(crate) fn key(this: &mut Player, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Player>) {
    if slider_typing() {
        slider_key(key, ctrl, alt, key_char, cx);
        cx.notify();
        return;
    }
    let Some((target, mut edit)) = STATE.with(|s| s.borrow().edit.clone()) else {
        return;
    };
    if this.eq_open.is_none() {
        cancel();
        return;
    }
    // The buffer takes digits only, so a "k" reads the number so far as kHz and writes it out in Hz.
    if target == NumTarget::Freq && !ctrl && !alt && key_char.is_some_and(|c| c.eq_ignore_ascii_case("k")) {
        if let Some(hz) = parse_freq(&format!("{}k", edit.text())) {
            edit = NumberEdit::begin(&format!("{hz:.0}"));
        }
        STATE.with(|s| s.borrow_mut().edit = Some((target, edit)));
        cx.notify();
        return;
    }
    let done = edit.key(key, ctrl, alt, key_char);
    if done == FieldKey::Commit {
        let (lo, hi) = target.range();
        let typed = if target == NumTarget::Freq { parse_freq(edit.text()).map(|v| v.clamp(lo, hi)) } else { edit.commit(lo, hi) };
        if let Some(v) = typed {
            target.set(this, v);
            this.commit_eq(cx);
        }
    }
    STATE.with(|s| {
        s.borrow_mut().edit = match done {
            FieldKey::Edited => Some((target, edit)),
            FieldKey::Commit | FieldKey::Cancel => None,
        }
    });
    cx.notify();
}

/// A typed frequency in Hz: "80", "11100", "11.1k", "11.1 kHz" (a bare number is Hz).
fn parse_freq(text: &str) -> Option<f64> {
    let t = text.trim().to_lowercase().replace(',', ".");
    let t = t.strip_suffix("hz").unwrap_or(&t).trim();
    let (num, mult) = t.strip_suffix('k').map_or((t, 1.), |n| (n.trim(), 1000.));
    let v: f64 = num.parse().ok()?;
    (v.is_finite() && v > 0.).then_some(v * mult)
}
