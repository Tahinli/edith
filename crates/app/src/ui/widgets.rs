//! The small elements every region is built out of.

use crate::*;

/// `ClickEvent::modifiers()` answers the mouse-*up* modifiers (gpui-0.2.2
/// `interactive.rs`), so releasing ctrl before the button lands undoes a
/// ctrl-held toggle decided on mouse-down. This reads the *press* modifiers
/// instead -- the ones a ctrl-click decision (multi-pick, no collapse) was
/// actually made under.
pub(crate) fn press_modifiers(event: &ClickEvent) -> gpui::Modifiers {
    match event {
        ClickEvent::Mouse(m) => m.down.modifiers,
        ClickEvent::Keyboard(_) => gpui::Modifiers::default(),
    }
}

/// As [`waveform`], but in a caller-picked ink rather than the fixed
/// secondary foreground -- the darkroom bench's own audio clips draw their
/// envelope in the source's ink (DESIGN §5), which the legacy timeline never
/// needed a colour for.
pub(crate) fn waveform_ink(
    peaks: Arc<Vec<(f32, f32)>>,
    from: f64,
    to: f64,
    ink: u32,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let cols = envelope(&peaks, from, to, f32::from(s.width), f32::from(s.height));
            if cols.len() < 2 {
                return;
            }
            // Down the tops and back along the bottoms: one closed outline of
            // the whole envelope, which is one path rather than a path a column.
            let mut points: Vec<Point<Pixels>> = cols
                .iter()
                .map(|&(x, top, _)| point(o.x + px(x), o.y + px(top)))
                .collect();
            points.extend(
                cols.iter()
                    .rev()
                    .map(|&(x, _, bottom)| point(o.x + px(x), o.y + px(bottom))),
            );
            let mut path = PathBuilder::fill();
            path.add_polygon(&points, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(ink));
            }
        },
    )
    .size_full()
}

/// An audio clip's fade, drawn as a translucent wedge over its box: silent
/// (full shade) at the clip's own edge, clear by the fade's far side --
/// [`is_in`] mirrors it for the tail's ramp-down. The same `canvas` +
/// `PathBuilder` idiom [`waveform`] draws its envelope with, filled with the
/// same wash the in/out range already paints the bed with
/// (`ACCENT_WASH`, timeline.rs), so a fade reads as one more overlay on this
/// bed rather than a shape borrowed from somewhere else.
pub(crate) fn fade_wedge(is_in: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let (w, h) = (f32::from(s.width), f32::from(s.height));
            if w <= 0. || h <= 0. {
                return;
            }
            // A right triangle: the clip's silent corner (top of the clip's
            // own edge) down to the bed and across to the fade's far side --
            // the wedge shape every editor draws for a ramp.
            let points = match is_in {
                true => vec![
                    point(o.x, o.y),
                    point(o.x, o.y + px(h)),
                    point(o.x + px(w), o.y + px(h)),
                ],
                false => vec![
                    point(o.x + px(w), o.y),
                    point(o.x + px(w), o.y + px(h)),
                    point(o.x, o.y + px(h)),
                ],
            };
            let mut path = PathBuilder::fill();
            path.add_polygon(&points, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, rgba(ACCENT_WASH()));
            }
        },
    )
    .size_full()
}

/// A video clip's dissolve into its neighbour, drawn as a small X astride the
/// join -- the box it is given is the last [`Clip::transition_out`] frames of
/// the clip's own width, so the mark sits right at the edge it dissolves
/// through. Two strokes rather than [`fade_wedge`]'s fill: a dissolve is not
/// a ramp to or from silence, it is two clips overlapping, and an X reads as
/// a splice the way a wedge reads as a fade.
pub(crate) fn dissolve_glyph() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let (w, h) = (f32::from(s.width), f32::from(s.height));
            if w <= 0. || h <= 0. {
                return;
            }
            let mut path = PathBuilder::stroke(px(1.5));
            path.move_to(point(o.x, o.y));
            path.line_to(point(o.x + px(w), o.y + px(h)));
            path.move_to(point(o.x + px(w), o.y));
            path.line_to(point(o.x, o.y + px(h)));
            if let Ok(path) = path.build() {
                window.paint_path(path, rgba(ACCENT_WASH()));
            }
        },
    )
    .size_full()
}

/// A toolbar button: its glyph, its name, and its key on hover. `id` only buys
/// `on_click` and the tooltip -- it is still not focusable, so the root's own
/// key listener keeps working after a press, and the click lands on mouse-up
/// inside the button (a press that slides off does nothing).
///
/// A button that would do nothing says so: dimmed, no pointer, no listener.
pub(crate) fn control(
    id: &'static str,
    // The rect the label is allowed to change inside: 0 hugs the text, anything
    // else is reserved once and never moves again, which is what keeps a button
    // that relabels itself ("Export"/"Cancel") from shoving its neighbours
    // along the row every time its state changes.
    w: f32,
    // The plane the button stands on. One button in the window is the accent
    // (Export, the primary action); everything else is `BG_RAISED`.
    bg: u32,
    glyph: Option<AnyElement>,
    // Not `&'static str`: the volume button's label is its state.
    label: impl Into<SharedString>,
    shortcut: String,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let tip: SharedString = format!("{label} — {shortcut}").into();
    div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .px(px(8.))
        .when(w > 0., |d| d.w(px(w)).overflow_hidden())
        .rounded(px(4.))
        .bg(rgb(bg))
        .when(bg == ACCENT_PRIMARY(), |d| d.text_color(rgb(BG_CANVAS())))
        // A glyph sits in a box of its own width, never in the width it happens
        // to draw: the pause bars are 12 px wide and the play triangle is 11, so
        // pressing Play used to slide every button in the row one pixel left. A
        // slot is the only fix that holds for the next glyph as well as this one.
        .children(glyph.map(|glyph| {
            div()
                .flex_none()
                .w(px(GLYPH_SLOT))
                .flex()
                .items_center()
                .justify_center()
                .child(glyph)
        }))
        .child(label)
        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
        .when(enabled, |d| {
            // An accented button keeps its accent under the pointer: hovering it
            // to the chrome's own hover colour reads as the primary action
            // turning itself off.
            d.cursor_pointer()
                .hover(move |s| {
                    s.bg(rgb(match bg == ACCENT_PRIMARY() {
                        true => ACCENT_HOVER(),
                        false => BG_HOVER(),
                    }))
                })
                .on_click(on_click)
        })
}

/// The monitoring level as something to drag: 4 px of bar to look at and the
/// whole control's height to hit (WCAG 2.5.8), the split the speed bar and the
/// colour sliders both make. Only the level -- mute is the button beside it, so
/// a muted slider still shows what unmuting comes back to, drawn dim.
///
/// Whether a card or a menu is drawn over the window, as the hover labels see
/// it: written once a frame by [`Player::render`], read by every [`Tip`] before
/// it paints.
///
/// A tooltip already on screen when an overlay opens *stays* on screen in gpui:
/// occluding the surface under it does not take it back, because the check that
/// keeps it visible works off the element's absolute bounds and knows nothing
/// about what was painted over it (`div.rs::handle_tooltip_mouse_move`, its own
/// TODO). So the tip is what has to stand aside -- here, once, for every hover
/// label in this window, rather than at fifteen call sites of which the
/// sixteenth would be forgotten.
pub(crate) static OVERLAID: AtomicBool = AtomicBool::new(false);

/// Whether a tip is cleared to paint: an ordinary tip stands aside whenever
/// `OVERLAID` is set (a card/menu is up over the underlying UI it belongs
/// to), but a tip *anchored on the overlay itself* -- the `?` glyph on a
/// card's own head, say -- was never the case `OVERLAID` meant to catch, so
/// it paints regardless. One predicate, so [`Tip`] and [`OverlayTip`] can't
/// drift apart on what "overlaid" is supposed to mean.
pub(crate) fn tip_may_paint(anchored_on_overlay: bool) -> bool {
    anchored_on_overlay || !OVERLAID.load(Ordering::Relaxed)
}

/// DESIGN §4: "Notices, menus, chips, cues, and tooltips are all the same
/// plate" -- canvas-on-panel, 2px radius, the room's own type
/// (`ui::type_scale`), not a bordered box in an ad hoc size.
fn tip_plate(text: &SharedString) -> Div {
    let style = crate::ui::type_scale::label(
        crate::ui::type_scale::LABEL_ROW_PX,
        gpui::FontWeight::MEDIUM,
    );
    div()
        .px(px(8.))
        .py(px(4.))
        .rounded(px(2.))
        .bg(rgb(DARK_CANVAS()))
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(INK1()))
        .child(text.clone())
}

/// A tooltip is a view in gpui and nothing smaller, so this is the smallest one
/// that carries a line of text. It paints outside the window's element tree and
/// therefore owns its colours.
pub(crate) struct Tip(pub(crate) SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A card or a menu is up: nothing. A line of text over the items of the
        // menu that just opened under the pointer is the card being painted
        // over by the window it covers. (This struct is every tooltip on the
        // *underlying* UI -- grepped: `timeline.rs`, `library.rs`,
        // `overlays.rs`, `spine_stance.rs`, `dock_stance.rs`,
        // `timeband_stance.rs`, `preview.rs`, here -- so the fix belongs here
        // once, not per call site.)
        if !tip_may_paint(false) {
            return div();
        }
        tip_plate(&self.0)
    }
}

/// A tip anchored on the overlay itself -- a card's own `?` glyph, drawn on
/// its own card head. `OVERLAID` exists to hide a [`Tip`] on the UI *under*
/// a card while the card sits over it; a tip painted on the card is not
/// that case, so it is exempt (`tip_may_paint(true)` is always `true`).
/// Everything else about it is [`Tip`]'s own plate.
pub(crate) struct OverlayTip(pub(crate) SharedString);

impl Render for OverlayTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tip_plate(&self.0)
    }
}

/// A number being typed into a row that has no text field of its own: the
/// digits so far, the range and digit cap that row enforces, and once a
/// commit has been refused, why. Text-field semantics on a card that has no
/// text field -- typing, backspace, arrows that step, enter that commits and
/// escape that gives up -- held as state and driven by the root's key
/// handler, since nothing in these rows takes gpui focus. Shared by the
/// export card's custom bitrate field and the transition duration row
/// (DEBT #111) -- one implementation, each caller its own range and unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NumberEdit {
    pub(crate) text: String,
    pub(crate) refusal: Option<String>,
    min: u32,
    max: u32,
    digits: usize,
    unit: &'static str,
}

impl NumberEdit {
    /// Starts on the number the row already carries, so backspace edits it
    /// rather than the field opening empty over a value that is still in
    /// force. Zero is no number at all -- it is what the row opens at, before
    /// anyone has typed one.
    pub(crate) fn new(value: u32, min: u32, max: u32, digits: usize, unit: &'static str) -> Self {
        NumberEdit {
            text: match value {
                0 => String::new(),
                v => v.to_string(),
            },
            refusal: None,
            min,
            max,
            digits,
            unit,
        }
    }

    /// A digit against what is there. The one past the row's digit cap is
    /// refused *out loud*: a keystroke dropped in silence is how the old
    /// digit capture left a row showing a number the user had already typed
    /// past.
    pub(crate) fn digit(&mut self, digit: u32) {
        if self.text.chars().count() >= self.digits {
            self.refusal = Some(format!(
                "{} digits is already past the ceiling",
                self.digits
            ));
            return;
        }
        match char::from_digit(digit, 10) {
            Some(c) => {
                self.text.push(c);
                self.refusal = None;
            }
            None => self.refusal = Some("digits only".into()),
        }
    }

    /// Erases the last digit, and the refusal with it: the number on screen has
    /// changed, so the reason the old one was refused no longer describes it.
    pub(crate) fn backspace(&mut self) {
        self.text.pop();
        self.refusal = None;
    }

    /// The arrows, which is how a number gets picked rather than typed. Steps
    /// from what is in the field -- an empty one starts at the floor, so the
    /// first press up is the row's minimum and not a jump to some remembered
    /// value -- and stays inside the range, because a step is a walk through
    /// the legal numbers rather than a way out of them.
    pub(crate) fn step(&mut self, by: i32) {
        let at = self
            .text
            .parse::<i32>()
            .unwrap_or(self.min as i32 - by.signum());
        self.text = (at + by)
            .clamp(self.min as i32, self.max as i32)
            .to_string();
        self.refusal = None;
    }

    /// The number, or `None` with the reason recorded where the row will read
    /// it. Never clamped: a row that clamps a typed number to its ceiling
    /// without saying so is writing a number the user never typed. For a row
    /// whose own stepper already clamps silently, use [`Self::commit_clamped`]
    /// instead -- refusing there would be a stricter rule than the mouse door
    /// enforces.
    pub(crate) fn commit(&mut self) -> Option<u32> {
        match self.text.parse::<u32>() {
            Ok(v) if (self.min..=self.max).contains(&v) => {
                self.refusal = None;
                Some(v)
            }
            Ok(0) => {
                self.refusal = Some(format!(
                    "0 is not a value — {}–{} {}",
                    self.min, self.max, self.unit
                ));
                None
            }
            Ok(v) => {
                self.refusal = Some(format!(
                    "{v} is past the {} {} ceiling",
                    self.max, self.unit
                ));
                None
            }
            Err(_) => {
                self.refusal = Some(format!(
                    "type a number — {}–{} {}",
                    self.min, self.max, self.unit
                ));
                None
            }
        }
    }

    /// The number, clamped into range instead of refused -- for a row whose
    /// own stepper already clamps silently (the transition duration's setter
    /// clamps to the clips' shared length), so a typed number lands exactly
    /// where that same stepper would have walked it to. Always succeeds.
    pub(crate) fn commit_clamped(&mut self) -> u32 {
        let v = self.text.parse::<u32>().unwrap_or(self.min);
        self.refusal = None;
        v.clamp(self.min, self.max)
    }

    /// What the row shows while it is being typed into: the digits, the caret
    /// that says they are landing *here*, and either the refusal or the two
    /// keys that end the edit.
    pub(crate) fn detail(&self) -> String {
        format!(
            "{}▏ {} — {}",
            self.text,
            self.unit,
            match &self.refusal {
                Some(why) => why.as_str(),
                None => "enter commits · esc cancels",
            }
        )
    }
}

/// What a window with no file open shows: DESIGN §8 wants a noun, not a
/// sentence -- both ways in (window drop target, Import in the media list)
/// are taught by their own geography, not by this label.
pub(crate) fn empty_hint() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .text_color(rgb(FG_SECONDARY()))
        .child("No project")
}
