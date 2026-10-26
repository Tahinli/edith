//! The small elements every region is built out of.

use crate::player::text_field::FieldKey;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::theme;
use crate::ui::type_scale::{self, Typeset};
use crate::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// How tall the "more below" fade is at the foot of a scrolled body.
const MORE_BELOW_H: f32 = 32.;

/// The soft fade (no words) over the foot of a scrolled body while more of it is below --
/// the export sheet's (the third row of preset tiles cut mid-text read as
/// the end of the list) and the Settings rows' (a short window cut them off
/// mid-row). The body tracks `scroll`; this sits after it in a `relative`
/// parent, over its last [`MORE_BELOW_H`] pixels. Painted, not laid out: the scroll handle's extent is only known
/// after the body's own layout, and a canvas painted after it reads that
/// frame's numbers rather than the last one's. No hitbox, so the tiles
/// under it stay pressable.
pub(crate) fn more_below(scroll: ScrollHandle) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if !more_below_shows(f32::from(scroll.offset().y), f32::from(scroll.max_offset().height)) {
                return;
            }
            let panel = rgb(DARK_PANEL());
            let clear = gpui::Rgba { a: 0., ..panel };
            window.paint_quad(gpui::fill(
                bounds,
                gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(clear, 0.),
                    gpui::linear_color_stop(panel, 0.7),
                ),
            ));
        },
    )
    .absolute()
    .left_0()
    .right_0()
    .bottom_0()
    .h(px(MORE_BELOW_H))
}

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
                window.paint_path(path, rgba(crate::ui::theme::with_alpha(0x000000, 0x33)));
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
                window.paint_path(path, rgba(crate::ui::theme::with_alpha(0xffffff, 0xd9)));
            }
        },
    )
    .size_full()
}

/// The hover line every control in the room wears (the user, 2026-09-09:
/// "each and every button gets hover over information" -- nobody could name
/// the band's own glyphs). One shape, built from the same pair the hitmap
/// already names the control by: `<name> · <chord>`, DESIGN §4's "every
/// command wears its chord" and §8's no-prose rule -- never a sentence. A
/// control the current state refuses says *why* in the chord's place: naming
/// a stroke that will not fire is the lie §4 forbids.
///
/// The one-line form, kept for the test that pins it. The plates draw
/// [`TipParts`] instead (DESIGN §4.5: the key sits beside the name, not after a
/// dot, and a reason takes a line of its own).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn tip_line(name: &str, chord: &str, why: Option<&str>) -> SharedString {
    match why {
        Some(why) => format!("{name} · {why}"),
        // `Keymap::chord`'s own "unbound" badge: a glyph with no stroke says
        // its name and stops, rather than wearing `--` as if that were one.
        None if chord.is_empty() || chord == "--" => name.to_string(),
        None => format!("{name} · {chord}"),
    }
    .into()
}

/// What a hover plate says, in DESIGN §4.5's order: the name, the key beside
/// it (mono, ink3, from `Keymap::display` for an action), and -- when the
/// state refuses the control -- the reason on a line of its own in ink2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TipParts {
    pub(crate) name: SharedString,
    pub(crate) key: Option<SharedString>,
    pub(crate) why: Option<SharedString>,
}

impl TipParts {
    /// `""` and `"--"` are no key at all (`Keymap::chord`'s unbound badge). A
    /// key that is words rather than a chord -- a mouse gesture such as "drag"
    /// or "double-click", or any phrase -- is not a key: it is body text, under
    /// the name, in the same ink as a reason. A keycap is one token ("Esc",
    /// "Ctrl+K", "r").
    pub(crate) fn new(name: &str, key: &str, why: Option<&str>) -> Self {
        let (key, prose) = match key {
            "" | "--" => (None, None),
            key if is_keycap(key) => (Some(key.to_string().into()), None),
            prose => (None, Some(prose)),
        };
        let why = match (prose, why) {
            (Some(prose), Some(why)) => Some(format!("{prose}. {why}")),
            (Some(prose), None) => Some(prose.to_string()),
            (None, why) => why.map(str::to_string),
        };
        Self {
            name: name.to_string().into(),
            key,
            why: why.map(|why| why.into()),
        }
    }
}

impl TipParts {
    /// A keymap's own reading of an action's key (`↑ or ,`, `Ctrl+K`): always a keycap, never
    /// reclassified as a phrase the way [`TipParts::new`] reads free text.
    pub(crate) fn chord(name: &str, key: &str, why: Option<&str>) -> Self {
        let mut parts = Self::new(name, "", why);
        parts.key = match key {
            "" | "--" => None,
            key => Some(key.to_string().into()),
        };
        parts
    }
}

/// Whether a tooltip's key text is a keycap (`Esc`, `Ctrl+K`, `↓ or .`). A key that
/// names a mouse gesture or reads as a phrase ("drag", "click to type", "Video, audio or
/// images") is a sentence the plate says as plain body text instead.
fn is_keycap(key: &str) -> bool {
    const PHRASE_WORDS: [&str; 6] = ["drag", "click", "double-click", "right-click", "type", "to"];
    let lower = key.to_ascii_lowercase();
    !key.contains(',') && !lower.split_whitespace().any(|word| PHRASE_WORDS.contains(&word))
}

/// The parts of an action's plate, read off the keymap and the enable answer
/// the caller already holds -- so the key is whatever the keymap says now, and
/// a refusal's reason is the oracle's own. Pulled out of [`action_hover`] so a
/// test can check it without a window.
pub(crate) fn action_parts(keymap: &Keymap, action: ActionId, enabled: Enable) -> TipParts {
    TipParts::chord(action.label(), &keymap.display(action), enabled.why())
}

/// [`TipParts`] in the shape gpui's `.tooltip` takes. Every shared control
/// constructor attaches this rather than building its own plate, which is
/// what keeps the next control from shipping without a hover line.
pub(crate) fn tip_hover(
    name: &str,
    key: &str,
    why: Option<&str>,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + use<> {
    let parts = TipParts::new(name, key, why);
    move |_, cx| {
        cx.new(|_| KeyTip {
            parts: parts.clone(),
            on_overlay: false,
        })
        .into()
    }
}

/// [`tip_hover`] for a control drawn *on* a card or a menu, where an
/// ordinary [`KeyTip`] stands aside (`OVERLAID`) and would show nothing.
pub(crate) fn overlay_tip_hover(
    name: &str,
    key: &str,
    why: Option<&str>,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + use<> {
    let parts = TipParts::new(name, key, why);
    move |_, cx| {
        cx.new(|_| KeyTip {
            parts: parts.clone(),
            on_overlay: true,
        })
        .into()
    }
}

/// [`tip_hover`] for a control that dispatches an action: the name and the
/// key are read live off the keymap and the enable state, so a rebind or a
/// refusal can never leave a stale plate under the pointer.
pub(crate) fn action_hover(
    player: &Player,
    action: ActionId,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + use<> {
    let parts = action_parts(&player.keymap, action, player.enable(action, None));
    move |_, cx| {
        cx.new(|_| KeyTip {
            parts: parts.clone(),
            on_overlay: false,
        })
        .into()
    }
}

/// The keyboard focus ring (DESIGN §4.1): 2px of accent drawn *outside* the
/// control as a zero-blur shadow, so a focused control never moves its
/// neighbours. Applied through [`focus_ring`] on every control that can hold
/// gpui focus; the ring shows exactly while its handle is focused.
pub(crate) const FOCUS_RING_PX: f32 = 2.;

/// The ring as a style: what `.focus(..)` paints.
pub(crate) fn focus_ring_shadow() -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: rgb(crate::ui::theme::tokens().accent).into(),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(FOCUS_RING_PX),
    }]
}

/// Gives a control the focus ring. One door so toolbar, menu bar and widgets
/// cannot drift apart on what a focused control looks like.
pub(crate) fn focus_ring<E: InteractiveElement + Styled>(el: E) -> E {
    el.focus(|s| s.shadow(focus_ring_shadow()))
}

/// A toolbar button: its glyph, its name, and its key on hover. `id` only buys
/// `on_click` and the tooltip -- it is still not focusable, so the root's own
/// key listener keeps working after a press, and the click lands on mouse-up
/// inside the button (a press that slides off does nothing).
///
/// A button that would do nothing says so: dimmed, no pointer, no listener.
#[allow(
    clippy::too_many_arguments,
    reason = "every button in the window takes this same set; a params struct would be built at all 20+ call sites for no gain"
)]
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
    let tip = TipParts::new(&label, &shortcut, None);
    focus_ring(div().id(id))
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
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                parts: tip.clone(),
                on_overlay: false,
            })
            .into()
        })
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

/// The name on a hover plate, and the reason beneath it (DESIGN §4.5: names
/// are 14px, the key is the 13px mono the room gives every chord).
const TIP_NAME_PX: f32 = 13.;
/// The widest a plate grows before its text wraps (DESIGN §4.5).
const TIP_MAX_W: f32 = 300.;
/// gpui puts a tooltip's box one pixel below-right of the pointer, so a plate
/// there covers the lower half of the control it describes. The box is padded
/// by these clear margins (transparent), which pushes the visible plate off the
/// control: down and to the right of where the pointer is.
const TIP_CLEAR_Y: f32 = 26.;
const TIP_CLEAR_X: f32 = 10.;

/// The plate's definite width for these lines: wide enough for the longest on
/// one line, never past [`TIP_MAX_W`], so the text wraps inside it. A definite
/// width is what keeps gpui's measuring pass and its paint pass agreeing on
/// the height; with a max width alone the text overflowed its own box and ran
/// off the window's edge.
fn tip_w(lines: &[&str]) -> f32 {
    let widest = lines
        .iter()
        .map(|l| crate::ui::popup::text_w(l, TIP_NAME_PX, false))
        .fold(0f32, f32::max);
    (widest + 22.).clamp(60., TIP_MAX_W)
}

/// `text` broken into lines no wider than `max` px (by [`popup::text_w`]'s
/// upper bound). Broken here, by hand, because gpui's own wrapping did not take
/// the plate's width inside a tooltip (the text ran on past the plate's edge).
fn tip_lines(text: &str, max: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(last)
                if crate::ui::popup::text_w(&format!("{last} {word}"), TIP_NAME_PX, false)
                    <= max =>
            {
                last.push(' ');
                last.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

/// Where gpui hangs a tooltip box of `size` for a pointer at `mouse`: one pixel
/// below-right, flipped to the left of / above the pointer where that would
/// leave the window, then pinned inside. gpui's own rule (window.rs
/// `prepaint_tooltip`), kept here so a test can hold it to "always inside".
#[cfg(test)]
pub(crate) fn tip_origin(mouse: (f32, f32), size: (f32, f32), window: (f32, f32)) -> (f32, f32) {
    let mut x = mouse.0 + 1.;
    let mut y = mouse.1 + 1.;
    if x + size.0 > window.0 {
        x = mouse.0 - size.0 - 1.;
    }
    if y + size.1 > window.1 {
        y = mouse.1 - size.1 - 1.;
    }
    (x.max(0.), y.max(0.))
}

/// The dock's width, set every frame (`render.rs`): what a media row fits its meta lines to.
pub(crate) static DOCK_W_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The window y where the time band (transport, toolbar, ruler, lanes) begins, set every frame
/// (`render.rs`); `0` while unknown.
pub(crate) static TIMELINE_TOP_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The depth of the timeline's own chrome band (toolbar, sequence tabs, overview, ruler) in
/// which a plate is lifted above the region. Deeper (the lanes) a plate stays by the pointer:
/// a tooltip is always next to the control it names, never a long way off.
pub(crate) const TIP_LIFT_BAND: f32 = 100.;

/// How far above a pointer at `mouse_y` a plate's bottom edge sits when the pointer is in the
/// timeline's chrome band (`top` to `top + TIP_LIFT_BAND`): the plate hangs just above the region,
/// `TIP_CLEAR_Y / 2` clear, so it never covers the ruler or the toolbar. `None` elsewhere,
/// where the ordinary below-right place applies.
pub(crate) fn tip_lift(mouse_y: f32, top: f32) -> Option<f32> {
    (top > 0. && mouse_y >= top && mouse_y < top + TIP_LIFT_BAND).then(|| mouse_y + 1. - top + TIP_CLEAR_Y / 2.)
}

/// The plate inside its transparent clear margin ([`TIP_CLEAR_Y`]), below-right of the pointer
/// (gpui flips it at the window's edge); with the pointer in the timeline's chrome band, above
/// that region ([`tip_lift`]). The one placement rule for every tooltip.
fn tip_boxed(plate: Div, w: f32, window: Option<&Window>) -> Div {
    let top = f32::from_bits(TIMELINE_TOP_BITS.load(Ordering::Relaxed));
    let lift = window.and_then(|window| {
        let (mx, my) = (f32::from(window.mouse_position().x), f32::from(window.mouse_position().y));
        tip_lift(my, top).map(|l| (l, mx + 1. + w - f32::from(window.viewport_size().width)))
    });
    match lift {
        // A root of no size, so gpui's own flip at the window's edge has nothing
        // to flip; the plate hangs off it, absolute.
        Some((lift, over)) => div().w(px(0.)).h(px(0.)).child(plate.absolute().left(px(-over.max(0.))).bottom(px(lift))),
        None => div().pt(px(TIP_CLEAR_Y)).pl(px(TIP_CLEAR_X)).child(plate),
    }
}

/// One line of a hover plate: the keycap's height, so a label and its chips share a line box
/// and the text centres on the chips.
const TIP_LINE_PX: f32 = 18.;

/// DESIGN §2.5 and §4.5: a tooltip is a floating plate, so it is a panel with a
/// hairline, a 6px radius and the one shadow the room uses for floating plates
/// (menus, the palette, sheets, toasts). Padded 10px on every side.
fn tip_plate() -> Div {
    crate::ui::kit::plate(6.)
        .flex()
        .flex_none()
        .flex_col()
        .gap(px(4.))
        .p(px(10.))
}

/// A plain prose tooltip ([`Tip`], [`OverlayTip`]): one line in ink1 on the
/// same plate, so a sentence-shaped hover still wears the room's one plate.
fn tip_text(text: &SharedString, window: Option<&Window>) -> Div {
    let name = crate::ui::type_scale::label(TIP_NAME_PX, gpui::FontWeight::MEDIUM);
    let lines = tip_lines(text, TIP_MAX_W - 22.);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let w = tip_w(&refs);
    tip_boxed(
        tip_plate()
            .w(px(w))
            .font(name.font)
            .text_size(name.size)
            .text_color(rgb(INK1()))
            .children(lines.into_iter().map(|l| div().h(px(TIP_LINE_PX)).line_height(px(TIP_LINE_PX)).whitespace_nowrap().child(l))),
        w,
        window,
    )
}

/// One tooltip's text: the name in ink1, the key beside it in mono ink3, and
/// the reason (when the state refuses) on the line below in ink2.
fn tip_card(parts: &TipParts, window: Option<&Window>) -> Div {
    let name = crate::ui::type_scale::label(TIP_NAME_PX, gpui::FontWeight::MEDIUM);
    // The name wraps by hand like every tip line ([`tip_lines`]); the key sits
    // beside its last line.
    let mut name_lines = tip_lines(&parts.name, TIP_MAX_W - 22.);
    let last = name_lines.pop().unwrap_or_default();
    let ink = |l: String, c: u32| {
        div()
            .h(px(TIP_LINE_PX))
            .line_height(px(TIP_LINE_PX))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .font(name.font.clone())
            .text_size(name.size)
            .text_color(rgb(c))
            .child(l)
    };
    let row = div()
        .flex()
        .flex_col()
        .children(name_lines.iter().cloned().map(|l| ink(l, INK1())))
        .child(
            div()
                .h(px(TIP_LINE_PX))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(ink(last.clone(), INK1()))
                .when_some(parts.key.clone(), |row, text| {
                    row.child(crate::ui::kit::keycaps(&text).whitespace_nowrap())
                }),
        );
    let why_lines = parts
        .why
        .as_ref()
        .map(|w| tip_lines(w, TIP_MAX_W - 22.))
        .unwrap_or_default();
    let head_w = crate::ui::popup::text_w(&last, TIP_NAME_PX, false)
        + parts.key.as_ref().map_or(0., |k| crate::ui::kit::keycaps_w(k.as_ref()) + 8.)
        + 22.;
    let w = tip_w(
        &name_lines
            .iter()
            .map(String::as_str)
            .chain(why_lines.iter().map(String::as_str))
            .collect::<Vec<_>>(),
    )
    .max(head_w.min(TIP_MAX_W + 40.));
    tip_boxed(
        tip_plate()
            .w(px(w))
            .child(row)
            .children(
                why_lines
                    .iter()
                    .map(|l| {
                        let l = l.clone();
                        div()
                            .h(px(TIP_LINE_PX))
                            .line_height(px(TIP_LINE_PX))
                            .flex()
                            .items_center()
                            .whitespace_nowrap()
                            .font(name.font.clone())
                            .text_size(name.size)
                            .text_color(rgb(INK2()))
                            .child(l)
                    }),
            ),
        w,
        window,
    )
}

/// A tooltip's entity, where the pointer was when it first painted, and whether it is dead.
type TipBorn = (gpui::EntityId, (f32, f32), bool);

thread_local! {
    /// The pointer position of the last press (set once a frame by [`Player::render`]'s listener).
    static CLICK_AT: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// The tooltip last painted: its entity, where the pointer was when it first painted (which is
    /// where gpui shows it), and whether it is already dead.
    static TIP_BORN: Cell<Option<TipBorn>> = const { Cell::new(None) };
}

/// Notes a press at `at`: a tooltip that has not shown yet must not show for it afterwards.
pub(crate) fn note_press(at: gpui::Point<Pixels>) {
    CLICK_AT.with(|c| c.set(Some((f32::from(at.x), f32::from(at.y)))));
}

/// How far the pointer may drift from where a tooltip appeared before it is gone.
const TIP_DRIFT: f32 = 12.;

/// Whether the tooltip view `id` must paint nothing: the pointer has left the spot it was
/// shown for (a stale plate, whatever kept gpui from hiding it), or it was born right under a
/// press (a tip whose delay ran out after the click). gpui's own rule hides a *shown* tip on a
/// press but lets a *waiting* one show afterwards; this is the one place that closes both.
fn tip_gone(id: gpui::EntityId, window: &Window) -> bool {
    let m = (f32::from(window.mouse_position().x), f32::from(window.mouse_position().y));
    let (born, dead) = match TIP_BORN.with(Cell::get) {
        Some((i, p, dead)) if i == id => (p, dead),
        _ => {
            let dead = CLICK_AT.with(Cell::get).is_some_and(|c| (c.0 - m.0).abs() < 1. && (c.1 - m.1).abs() < 1.);
            (m, dead)
        }
    };
    let dead = dead || (born.0 - m.0).abs() > TIP_DRIFT || (born.1 - m.1).abs() > TIP_DRIFT;
    TIP_BORN.with(|c| c.set(Some((id, born, dead))));
    dead
}

/// A tooltip is a view in gpui and nothing smaller, so this is the smallest one
/// that carries a line of text. It paints outside the window's element tree and
/// therefore owns its colours.
pub(crate) struct Tip(pub(crate) SharedString);

impl Render for Tip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A card or a menu is up: nothing. A line of text over the items of the
        // menu that just opened under the pointer is the card being painted
        // over by the window it covers. (This struct is every tooltip on the
        // *underlying* UI -- grepped: `timeline.rs`, `library.rs`,
        // `overlays.rs`, `spine_stance.rs`, `dock_stance.rs`,
        // `timeband_stance.rs`, `preview.rs`, here -- so the fix belongs here
        // once, not per call site.)
        if !tip_may_paint(false) || (!cx.has_active_drag() && tip_gone(cx.entity_id(), window)) {
            return div();
        }
        // A drag ghost follows the pointer; only a hover plate keeps off the dock.
        tip_text(&self.0, (!cx.has_active_drag()).then_some(&*window))
    }
}

/// Whether any `(start, len)` span holds `frame`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn spans_cover(spans: impl IntoIterator<Item = (u32, u32)>, frame: u32) -> bool {
    spans.into_iter().any(|(start, len)| frame >= start && frame < start + len)
}

/// A tip anchored on the overlay itself -- a card's own `?` glyph, drawn on
/// its own card head. `OVERLAID` exists to hide a [`Tip`] on the UI *under*
/// a card while the card sits over it; a tip painted on the card is not
/// that case, so it is exempt (`tip_may_paint(true)` is always `true`).
/// Everything else about it is [`Tip`]'s own plate.
pub(crate) struct OverlayTip(pub(crate) SharedString);

impl Render for OverlayTip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if tip_gone(cx.entity_id(), window) {
            return div();
        }
        tip_text(&self.0, Some(window))
    }
}

/// The hover plate every shared control and action wears (DESIGN §4.5): the
/// name, its key and, when refused, its reason. [`Tip`]'s rule applies: it
/// stands aside while a card is over the UI it belongs to, unless it is
/// anchored on the overlay (`on_overlay`, see [`tip_may_paint`]).
pub(crate) struct KeyTip {
    pub(crate) parts: TipParts,
    pub(crate) on_overlay: bool,
}

impl Render for KeyTip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !tip_may_paint(self.on_overlay) || tip_gone(cx.entity_id(), window) {
            return div();
        }
        tip_card(&self.parts, Some(window))
    }
}

/// How much text the colour field will hold: `rgba(255, 255, 255, 255)` with
/// room to spare. Every legal colour is far inside it; the cap is there so a
/// held key cannot grow the buffer without bound.
const COLOR_EDIT_CHARS: usize = 32;

/// A colour being typed into a row that has no text field of its own: the
/// free-text sibling of the kit number field, for the same reason it exists -- these
/// rows take no gpui focus, so the text is held as state and driven by the
/// root's key handler. One caller, the Clip tab's colour field
/// ([`Player::edit_viz_hex`]): typing, backspace, enter that commits and
/// escape that gives up, and a refusal in words when what is there is not a
/// colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColorEdit {
    pub(crate) text: String,
    pub(crate) refusal: Option<String>,
}

impl ColorEdit {
    /// Starts on the colour the row already shows, so backspace edits it
    /// rather than the field opening empty over a colour still in force
    /// (the number field's own rule).
    pub(crate) fn new(seed: &str) -> Self {
        ColorEdit {
            text: seed.to_string(),
            refusal: None,
        }
    }

    /// One printable stroke. The one past the cap is refused *out loud*: a
    /// keystroke dropped in silence is what leaves a field showing something
    /// the user did not type (the number field's own reason).
    pub(crate) fn typed(&mut self, c: char) {
        if self.text.chars().count() >= COLOR_EDIT_CHARS {
            self.refusal = Some(format!("{COLOR_EDIT_CHARS} characters is already past the field"));
            return;
        }
        self.text.push(c);
        self.refusal = None;
    }

    /// Erases the last character, and the refusal with it: the text on screen
    /// has changed, so the reason the old one was refused no longer describes
    /// it.
    pub(crate) fn backspace(&mut self) {
        self.text.pop();
        self.refusal = None;
    }

    /// The colour, or `None` with the reason recorded where the row will read
    /// it. Only the shapes the paint field can carry are read at all
    /// (`engine::decode::viz_color_from_text`); anything else is refused
    /// rather than guessed at, and the refusal says what a colour looks like
    /// so the next try is a backspace away.
    pub(crate) fn commit(&mut self) -> Option<(u8, u8)> {
        match engine::decode::viz_color_from_text(&self.text) {
            Some(hue_sat) => {
                self.refusal = None;
                Some(hue_sat)
            }
            None => {
                self.refusal = Some("type a colour — #RRGGBB or rgb(r,g,b)".to_string());
                None
            }
        }
    }

    /// What the row shows while it is being typed into: the text, the caret
    /// that says it is landing *here*, and either the refusal or the two keys
    /// that end the edit.
    pub(crate) fn detail(&self) -> String {
        format!(
            "{}▏ {}",
            self.text,
            match &self.refusal {
                Some(why) => why.as_str(),
                None => "enter commits · esc cancels",
            }
        )
    }
}

/// How many characters a cue's words may run to in the in-place editor. Well
/// past any caption a viewer reads in the time it is up, so the cap only stops
/// a held key from growing the buffer without bound.
const CUE_EDIT_CHARS: usize = 500;

/// A subtitle cue being retyped in place on its block: the free-text sibling of
/// [`ColorEdit`], for the same reason -- the block takes no gpui focus, so the
/// text is held as state and driven by the root's key handler
/// ([`Player::open_cue_edit`], [`Player::commit_cue_edit`]). Unlike a colour it
/// takes any printable character, not just ASCII, because a cue is words in
/// whatever language the file is in.
///
/// `lane`, `track` and `cue` name the block being edited: `track` and `cue` are
/// the palette row and its cue ([`engine::PlaybackSession::set_cue_text`]), so
/// every placement of that track shows what is typed here on commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CueEdit {
    pub(crate) lane: Lane,
    pub(crate) track: usize,
    pub(crate) cue: usize,
    pub(crate) text: String,
    /// Where the caret is drawn: at the end, where every stroke lands. Kept as a
    /// field so the draw and the buffer agree on where the caret is.
    pub(crate) caret_at_end: bool,
    pub(crate) refusal: Option<String>,
    /// Typed in the Clip tab's caption panel rather than in the floating box over the block.
    pub(crate) in_panel: bool,
    /// Every word is selected (Ctrl+A): the next character or line break replaces them all.
    pub(crate) all: bool,
}

impl CueEdit {
    /// Starts on the words the cue already has, so backspace edits them rather
    /// than the block opening empty over words still on the screen.
    pub(crate) fn new(lane: Lane, track: usize, cue: usize, seed: &str) -> Self {
        CueEdit {
            lane,
            track,
            cue,
            text: seed.to_string(),
            caret_at_end: true,
            refusal: None,
            in_panel: false,
            all: false,
        }
    }

    /// Ctrl+A: every word is selected, and the next stroke that writes replaces it.
    pub(crate) fn select_all(&mut self) {
        self.all = true;
    }

    /// Drops the selected words before a stroke that writes (a selection is replaced, not added to).
    fn replace_selection(&mut self) {
        if self.all {
            self.text.clear();
            self.all = false;
        }
    }

    /// One printable character. Control characters are ignored outright (a
    /// stray tab or a bell is not a word); the one past the cap is refused out
    /// loud, for the reason [`ColorEdit::typed`] gives.
    pub(crate) fn typed(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.replace_selection();
        self.push(c);
    }

    /// Shift+Enter: a line break inside the cue, which a multi-line cue
    /// carries as `\n` ([`engine::subtitle::Cue::text`]).
    pub(crate) fn newline(&mut self) {
        self.replace_selection();
        self.push('\n');
    }

    fn push(&mut self, c: char) {
        if self.text.chars().count() >= CUE_EDIT_CHARS {
            self.refusal = Some(format!(
                "{CUE_EDIT_CHARS} characters is the most a cue holds"
            ));
            return;
        }
        self.text.push(c);
        self.refusal = None;
    }

    /// Erases the last character, and the refusal with it, for the reason
    /// [`ColorEdit::backspace`] gives.
    pub(crate) fn backspace(&mut self) {
        if self.all {
            self.text.clear();
            self.all = false;
        } else {
            self.text.pop();
        }
        self.refusal = None;
    }

    /// The words to write, or `None` when they are the words the cue already
    /// has -- an edit that changed nothing takes no undo step and writes
    /// nothing, so opening a block and leaving it with enter is no edit.
    pub(crate) fn commit(&self, current: &str) -> Option<String> {
        (self.text != current).then(|| self.text.clone())
    }
}

/// Which text of a marker the markers card is retyping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerField {
    Name,
    Note,
}

/// A marker's name or note being retyped on the markers card: the free-text sibling of
/// [`CueEdit`], driven by the root's key handler for the same reason (the card takes no
/// gpui focus). `idx` is the marker's place in `PlaybackSession::markers`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkerEdit {
    pub(crate) idx: usize,
    pub(crate) field: MarkerField,
    pub(crate) text: String,
    /// Every word is selected (Ctrl+A): the next character replaces them all.
    pub(crate) all: bool,
}

impl MarkerEdit {
    /// Starts on the words the field already has. A name opens fully selected, so the first
    /// letter replaces it (a rename); a note opens with the caret at its end.
    pub(crate) fn new(idx: usize, field: MarkerField, seed: &str) -> Self {
        MarkerEdit { idx, field, text: seed.to_string(), all: field == MarkerField::Name }
    }

    /// One printable character; control characters are ignored, and a held key stops at
    /// the most the field holds.
    pub(crate) fn typed(&mut self, c: char) {
        let cap = match self.field {
            MarkerField::Name => engine::project::MAX_MARKER_NAME_CHARS,
            MarkerField::Note => engine::project::MAX_MARKER_NOTE_CHARS,
        };
        if self.all {
            self.text.clear();
            self.all = false;
        }
        if !c.is_control() && self.text.chars().count() < cap {
            self.text.push(c);
        }
    }

    /// Erases the last character.
    pub(crate) fn backspace(&mut self) {
        if self.all {
            self.text.clear();
            self.all = false;
        } else {
            self.text.pop();
        }
    }
}

/// Where a value sits on its slider, 0..1: the thumb's share of the track.
/// A value past either end pins to that end, and a range with no width (or a
/// value that is not a number) reads as the start rather than dividing by
/// nothing.
#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn slider_fraction(value: f32, lo: f32, hi: f32) -> f32 {
    let span = hi - lo;
    if span.is_nan() || span <= 0. || value.is_nan() {
        return 0.;
    }
    ((value - lo) / span).clamp(0., 1.)
}

/// A slider press: window x of the sample, the card's own write.
pub(crate) type SliderPress<V> = Box<dyn Fn(&mut V, Pixels, &mut Context<V>)>;
/// A slider reset: back to the default.
pub(crate) type SliderReset<V> = Rc<dyn Fn(&mut V, &mut Context<V>)>;

/// One card slider's row (DESIGN §9.1). The card builds it and [`slider_row`]
/// draws it, so every panel's slider reads the same and hits the same.
#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) struct SliderRow<V> {
    pub(crate) id: &'static str,
    pub(crate) label: SharedString,
    pub(crate) value: SharedString,
    /// Where the value sits on its range, 0..1 ([`slider_fraction`] maps it).
    pub(crate) fraction: f32,
    /// Off the default: the value reads ink1 and a Reset button appears.
    pub(crate) changed: bool,
    /// The track's box, recorded by a [`bounds_probe`] so a press can be turned
    /// into a fraction with [`frac_along`].
    pub(crate) bar: Rc<Cell<Bounds<Pixels>>>,
    /// A press or drag sample at window x. The card owns its own drag flag and
    /// its write, the way the other card sliders already do.
    pub(crate) on_press: SliderPress<V>,
    /// Back to the default. Double-clicking the value and the Reset button both
    /// call it; `None` leaves neither on the row.
    pub(crate) on_reset: Option<SliderReset<V>>,
    /// Typing the value in: a click on the value opens it as a field. `None` keeps
    /// the value drag-only.
    pub(crate) typed: Option<SliderTyped<V>>,
}

/// Writes a typed slider value, already clamped, in the units the value reads.
pub(crate) type SliderSet<V> = Rc<dyn Fn(&mut V, f64, &mut Context<V>)>;
/// A typed value's write once it is due: it reaches the card's entity from outside the card.
type TypedCommit = Rc<dyn Fn(f64, &mut App)>;

/// A slider's typed entry: the range a typed number clamps to and the write it makes,
/// both in the units the value reads ("12 px" is 12, "40%" is 40).
pub(crate) struct SliderTyped<V> {
    pub(crate) lo: f64,
    pub(crate) hi: f64,
    pub(crate) set: SliderSet<V>,
}

/// A slider value being typed: which row (the identity of its track), the buffer, the unit the
/// value wears (" px", "%"), the range and the write a commit makes.
struct SliderEdit {
    row: usize,
    edit: crate::ui::kit::NumberEdit,
    unit: SharedString,
    lo: f64,
    hi: f64,
    commit: TypedCommit,
    /// The value takes the keyboard while it is typed (its keys stop there, before a card's own
    /// escape sees them), and hands it back to `back`, what had it before, when typing ends.
    focus: FocusHandle,
    back: Option<FocusHandle>,
}

thread_local! {
    static SLIDER_EDIT: RefCell<Option<SliderEdit>> = const { RefCell::new(None) };
}

/// Whether a slider value is being typed: the keyboard is its until Enter or Escape.
pub(crate) fn slider_typing() -> bool {
    SLIDER_EDIT.with(|s| s.borrow().is_some())
}

/// One key while a slider value is typed: Enter writes it, Escape drops it, the rest is text.
pub(crate) fn slider_key(key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut App) {
    let Some(mut e) = SLIDER_EDIT.with(|s| s.borrow_mut().take()) else {
        return;
    };
    match e.edit.key(key, ctrl, alt, key_char) {
        FieldKey::Edited => SLIDER_EDIT.with(|s| *s.borrow_mut() = Some(e)),
        FieldKey::Commit => write_typed(&e, cx),
        FieldKey::Cancel => {}
    }
    cx.refresh_windows();
}

/// Writes what was typed, once the update that delivered the keystroke or press is over: the
/// card's own entity is still being updated when a key reaches it, so the write waits its turn.
fn write_typed(e: &SliderEdit, cx: &mut App) {
    if let Some(v) = e.edit.commit(e.lo, e.hi) {
        let commit = e.commit.clone();
        cx.defer(move |app| commit(v, app));
    }
}

/// The typed value is written when the press lands outside the value, the way a click away
/// from a field keeps what was typed.
fn slider_leave(row: usize, cx: &mut App) {
    let Some(e) = SLIDER_EDIT.with(|s| s.borrow_mut().take_if(|e| e.row == row)) else {
        return;
    };
    write_typed(&e, cx);
    cx.refresh_windows();
}

/// The focus a typed value gives back when it ends.
fn slider_back(row: usize) -> Option<FocusHandle> {
    SLIDER_EDIT.with(|s| s.borrow().as_ref().filter(|e| e.row == row).and_then(|e| e.back.clone()))
}

/// Drops a row's typed value without writing it (a double-click resets the value instead).
fn slider_forget(row: usize) {
    SLIDER_EDIT.with(|s| {
        s.borrow_mut().take_if(|e| e.row == row);
    });
}

/// Splits a shown value into the number it opens on and the unit it wears: "12 px" is ("12",
/// " px"), "−90°" is ("-90", "°").
fn split_value(shown: &str) -> (String, String) {
    let cut = shown.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | '\u{2212}'))).unwrap_or(shown.len());
    let (num, unit) = shown.split_at(cut);
    (num.replace('\u{2212}', "-"), unit.to_owned())
}

/// The key a readout's typing is kept under: its id, hashed. Every typed readout has an id of
/// its own, so one row's typing never shows in another's.
pub(crate) fn typed_key(id: &str) -> usize {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    h.finish() as usize
}

/// A value's box, which a click types in: the value pill of a slider and of the rows that draw
/// their own track. With `typed` set, a click opens the box as a field: Enter writes it, Escape
/// drops it and a press outside keeps it. With `reset` set, a double-click resets the value.
/// `open` is the number and unit the field opens on; `None` reads them off `shown`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn typed_readout<V: 'static>(
    id: &'static str,
    shown: &str,
    open: Option<(String, String)>,
    min_w: f32,
    ink: u32,
    typed: Option<SliderTyped<V>>,
    reset: Option<SliderReset<V>>,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let t = theme::tokens();
    let row = typed_key(id);
    let typing_text = SLIDER_EDIT.with(|s| {
        s.borrow().as_ref().filter(|e| e.row == row).map(|e| (SharedString::from(e.edit.text().to_string()), e.unit.clone()))
    });
    let (num, unit) = open.unwrap_or_else(|| split_value(shown));
    let unit = SharedString::from(unit);
    // What a click on the value opens: the number it shows, and the write a commit makes.
    let entry = typed.map(|typed| {
        let me = cx.entity();
        let set = typed.set;
        let commit: TypedCommit = Rc::new(move |v, app: &mut App| {
            me.update(app, |this, cx| set(this, v, cx));
        });
        (typed.lo, typed.hi, commit)
    });
    // A click on the value types it in (the box keeps its size: the border is always there,
    // clear until it is typed in); a double-click resets it, as before.
    let editing = typing_text.is_some();
    let focus = SLIDER_EDIT.with(|s| s.borrow().as_ref().filter(|e| e.row == row).map(|e| e.focus.clone()));
    let on_down = (entry.is_some() || reset.is_some()).then(|| {
        let open = entry.clone();
        let (num, unit) = (num.clone(), unit.clone());
        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
            if event.click_count >= 2 {
                if let Some(back) = slider_back(row) {
                    window.focus(&back);
                }
                slider_forget(row);
                if let Some(reset) = &reset {
                    reset(this, cx);
                    cx.notify();
                }
            } else if let Some((lo, hi, commit)) = &open {
                let focus = cx.focus_handle();
                let back = window.focused(cx);
                window.focus(&focus);
                let edit = SliderEdit {
                    row,
                    edit: crate::ui::kit::NumberEdit::begin(&num),
                    unit: unit.clone(),
                    lo: *lo,
                    hi: *hi,
                    commit: commit.clone(),
                    focus,
                    back,
                };
                SLIDER_EDIT.with(|s| *s.borrow_mut() = Some(edit));
                cx.notify();
            }
        })
    });
    let leave = entry.is_some().then(|| {
        cx.listener(move |_, _: &MouseDownEvent, window, cx| {
            let back = slider_back(row);
            slider_leave(row, cx);
            if let Some(back) = back {
                window.focus(&back);
            }
        })
    });
    // While typed, the value holds the keyboard: a key it does not take still ends at the value.
    let key = focus.is_some().then(|| {
        cx.listener(move |_, event: &KeyDownEvent, window, cx| {
            let k = &event.keystroke;
            let back = slider_back(row);
            slider_key(&k.key, k.modifiers.control, k.modifiers.alt, k.key_char.as_deref(), cx);
            if !slider_typing()
                && let Some(back) = back
            {
                window.focus(&back);
            }
            cx.stop_propagation();
        })
    });
    div()
        .id((id, row))
        .h(px(crate::layout::redesign::CONTROL_S))
        .px(px(8.))
        .min_w(px(min_w))
        .justify_end()
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(if editing { gpui::Hsla::from(rgb(t.accent)) } else { gpui::transparent_black() })
        .bg(rgb(t.e2))
        .type_style(type_scale::caption())
        .text_color(rgb(ink))
        .when(entry.is_some(), |d| d.cursor_pointer())
        .when_some(on_down, |d, on_down| d.on_mouse_down(MouseButton::Left, on_down))
        .when_some(leave, |d, leave| d.on_mouse_down_out(leave))
        .when_some(focus, |d, focus| d.track_focus(&focus))
        .when_some(key, |d, key| d.on_key_down(key))
        .when_some(typing_text, |d, (buf, suffix)| {
            d.child(buf).child(div().w(px(1.)).h(px(14.)).bg(rgb(t.accent))).child(suffix)
        })
        .when(!editing, |d| d.child(SharedString::from(shown.to_string())))
}

/// A value pill with its Reset slot beside it, laid out the way [`crate::ui::kit::value_pill`]
/// lays one out, with the box typed by [`typed_readout`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn typed_value_pill<V: 'static>(
    id: &'static str,
    shown: &str,
    open: Option<(String, String)>,
    min_w: f32,
    changed: bool,
    typed: Option<SliderTyped<V>>,
    on_reset: impl Fn(&mut Window, &mut App) + 'static,
    cx: &mut Context<V>,
) -> Div {
    let t = theme::tokens();
    let ink = if changed { t.ink1 } else { t.ink2 };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.))
        .child(typed_readout(id, shown, open, min_w, ink, typed, None, cx))
        .child(crate::ui::kit::reset_slot(id, changed, on_reset))
}

/// The slider row (DESIGN §9.1): label left in ink3, value right in mono, a
/// 4px track under them and a 14px thumb on it, the whole track one `HIT_MIN`
/// tall to hit. Arrow keys are the card's own nudge, not this row's.
#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn slider_row<V: 'static>(row: SliderRow<V>, cx: &mut Context<V>) -> impl IntoElement {
    slider_row_with(row, false, None, cx)
}

/// [`slider_row`] whose label carries a tooltip saying what the setting is for.
pub(crate) fn slider_row_tip<V: 'static>(
    row: SliderRow<V>,
    tip: &'static str,
    cx: &mut Context<V>,
) -> impl IntoElement {
    slider_row_with(row, false, Some(tip), cx)
}

/// [`slider_row`] on one line: label at the left, the track filling the middle, the value (and
/// reset slot) at the right, so a settings list can keep every row `CONTROL_H` tall.
pub(crate) fn slider_row_inline<V: 'static>(row: SliderRow<V>, cx: &mut Context<V>) -> impl IntoElement {
    slider_row_with(row, true, None, cx)
}

fn slider_row_with<V: 'static>(
    row: SliderRow<V>,
    inline: bool,
    tip: Option<&'static str>,
    cx: &mut Context<V>,
) -> gpui::Stateful<Div> {
    const THUMB: f32 = 14.;
    let SliderRow {
        id,
        label,
        value,
        fraction,
        changed,
        bar,
        on_press,
        on_reset,
        typed,
    } = row;
    let fraction = slider_fraction(fraction, 0., 1.);
    let t = theme::tokens();
    let value_ink = if changed { t.ink1 } else { t.ink2 };
    let value_reset = on_reset.clone();
    // Always on the row, so a value change never moves anything: live when the
    // value is off its default, dimmed and inert while it sits there.
    let reset_button = on_reset.map(|reset| {
        // REDESIGN §3.9: a 16px `rotate-ccw` at the row's right edge, drawn
        // only while the value is off its default (the slot stays, so
        // nothing moves when it appears).
        div()
            .id(SharedString::from(format!("{id}-reset")))
            .flex_none()
            .size(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .when(changed, |d| {
                d.cursor_pointer()
                    .tooltip(overlay_tip_hover("Reset", "", None))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        reset(this, cx);
                        cx.notify();
                    }))
                    .child(Icon::RotateCcw.el(IconSize::Inline, t.ink2))
            })
    });
    let label_el = div()
        .id(SharedString::from(format!("{id}-label")))
        .flex_none()
        .when(inline, |d| d.w(px(96.)))
        .when_some(tip, |d, tip| d.tooltip(overlay_tip_hover(&label, "", Some(tip))))
        .type_style(type_scale::body())
        .text_color(rgb(t.ink2))
        .child(label);
    let pill = typed_readout(id, &value, None, 56., value_ink, typed, value_reset, cx);
    // Inline, the reset slot sits before the value so the value ends at the row's edge like
    // every other row's control; stacked, the value is already clear of the track's end.
    let value_el = div().flex_none().flex().items_center().gap(px(8.));
    let value_el = match inline {
        true => value_el.children(reset_button).child(pill),
        false => value_el.child(pill).children(reset_button),
    };
    let track = div()
                .relative()
                .w_full()
                .h(px(HIT_MIN))
                .flex()
                .items_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        on_press(this, event.position.x, cx);
                    }),
                )
                .child(bounds_probe(bar))
                .child(
                    div()
                        .w_full()
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(rgb(t.e3))
                        .child(
                            div()
                                .h_full()
                                .w(relative(fraction))
                                .rounded(px(2.))
                                .bg(rgb(t.accent)),
                        ),
                )
                .child(
                    // Centred on its fraction: the thumb's own half width is the
                    // margin, so the middle of the thumb, not its left edge, is
                    // where the value reads.
                    div()
                        .absolute()
                        .top(px((HIT_MIN - THUMB) / 2.))
                        .left(relative(fraction))
                        .ml(px(-THUMB / 2.))
                        .size(px(THUMB))
                        .rounded_full()
                        .bg(rgb(t.ink1)),
                )
        .when(inline, |d| d.flex_1().w_auto().min_w(px(0.)));
    let root = div().id(id).w_full().flex();
    match inline {
        true => root.items_center().gap(px(12.)).child(label_el).child(track).child(value_el),
        false => root
            .flex_col()
            .gap(px(4.))
            .child(div().flex().justify_between().items_center().child(label_el).child(value_el))
            .child(track),
    }
}

/// A bipolar slider's bare track, for a pan: the same track as [`slider_row`] with its
/// neutral point (the centre, 0) marked by two notches above and below, clear of the thumb's
/// footprint, and the fill running out from that centre to the value.
pub(crate) fn centred_track<V: 'static>(
    id: &'static str,
    fraction: f32,
    bar: Rc<Cell<Bounds<Pixels>>>,
    on_press: impl Fn(&mut V, Pixels, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> gpui::Stateful<Div> {
    const THUMB: f32 = 14.;
    const NOTCH: f32 = 4.;
    let t = theme::tokens();
    let fraction = slider_fraction(fraction, 0., 1.);
    let (from, to) = (fraction.min(0.5), fraction.max(0.5));
    div()
        .id(id)
        .relative()
        .w_full()
        .h(px(HIT_MIN))
        .flex()
        .items_center()
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| on_press(this, event.position.x, cx)),
        )
        .child(bounds_probe(bar))
        .child(
            div().relative().w_full().h(px(4.)).rounded(px(2.)).bg(rgb(t.e3)).child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(from))
                    .w(relative(to - from))
                    .rounded(px(2.))
                    .bg(rgb(t.accent)),
            ),
        )
        .child(div().absolute().top_0().left(relative(0.5)).w(px(1.)).h(px(NOTCH)).bg(rgb(t.ink3)))
        .child(div().absolute().bottom_0().left(relative(0.5)).w(px(1.)).h(px(NOTCH)).bg(rgb(t.ink3)))
        .child(
            div()
                .absolute()
                .top(px((HIT_MIN - THUMB) / 2.))
                .left(relative(fraction))
                .ml(px(-THUMB / 2.))
                .size(px(THUMB))
                .rounded_full()
                .bg(rgb(t.ink1)),
        )
}

/// The `More` fold (DESIGN §9.1): a row that opens and closes a group the
/// card keeps hidden by default. The open state is the card's; keep it with
/// [`load_folds`] / [`save_folds`] so it is remembered per panel.
#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn more_fold<V: 'static>(
    id: &'static str,
    open: bool,
    on_toggle: impl Fn(&mut V, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    div()
        .id(id)
        .w_full()
        .h(px(HIT_MIN))
        .flex()
        .items_center()
        .gap(px(4.))
        .cursor_pointer()
        .type_style(type_scale::body_strong())
        .text_color(rgb(t.ink2))
        .hover(|s| s.text_color(rgb(crate::ui::theme::tokens().ink1)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            on_toggle(this, cx);
            cx.notify();
        }))
        .child(
            if open { crate::ui::icons::Icon::ChevronDown } else { crate::ui::icons::Icon::ChevronRight }
                .el(crate::ui::icons::IconSize::Inline, t.ink3),
        )
        .child("More")
}

/// Where the open `More …` folds live: next to the other small preference
/// files (`dock-tab`, `stance-splits`), one panel id per line. A fold the file
/// does not name is closed, which is the default.
#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn folds_config_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("panel-folds")
}

#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn load_folds() -> BTreeSet<String> {
    load_folds_from(&folds_config_path())
}

#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn load_folds_from(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn save_folds(open: &BTreeSet<String>) {
    save_folds_to(open, &folds_config_path());
}

#[allow(dead_code, reason = "wave 2 panels mount these")]
pub(crate) fn save_folds_to(open: &BTreeSet<String>, path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = open.iter().map(|id| format!("{id}\n")).collect();
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod slider_fold_tests {
    use super::*;

    #[test]
    fn a_marker_edit_types_printable_text_to_its_cap_and_backspaces() {
        let mut edit = MarkerEdit::new(0, MarkerField::Name, "Fix");
        edit.all = false; // the caret at the end, so the letters add to the words
        edit.typed('!');
        edit.typed('\n');
        edit.typed('é');
        assert_eq!(edit.text, "Fix!é", "a control character is not a letter");
        edit.backspace();
        assert_eq!(edit.text, "Fix!");
        let mut name = MarkerEdit::new(1, MarkerField::Name, "");
        name.all = false;
        for _ in 0..(engine::project::MAX_MARKER_NAME_CHARS + 10) {
            name.typed('x');
        }
        assert_eq!(name.text.chars().count(), engine::project::MAX_MARKER_NAME_CHARS);
    }

    #[test]
    fn a_slider_fraction_is_the_share_of_its_range_and_pins_at_the_ends() {
        assert_eq!(slider_fraction(5., 0., 10.), 0.5);
        assert_eq!(slider_fraction(0., 0., 10.), 0.);
        assert_eq!(slider_fraction(10., 0., 10.), 1.);
        // A value past either end pins to it; a drag that slid off the bar
        // names the bar's end, never a number outside it.
        assert_eq!(slider_fraction(-3., 0., 10.), 0.);
        assert_eq!(slider_fraction(42., 0., 10.), 1.);
        // Speed runs 0.25x..4x, so 1x sits a little under the middle.
        let at_one = slider_fraction(1., 0.25, 4.);
        assert!((at_one - 0.2).abs() < 1e-6, "got {at_one}");
    }

    #[test]
    fn a_range_with_no_width_or_a_value_that_is_not_a_number_reads_as_the_start() {
        assert_eq!(slider_fraction(3., 2., 2.), 0.);
        assert_eq!(slider_fraction(f32::NAN, 0., 10.), 0.);
        assert_eq!(slider_fraction(f32::INFINITY, 0., 10.), 1.);
    }

    #[test]
    fn open_folds_survive_a_round_trip_and_a_missing_file_leaves_them_closed() {
        let dir = engine::scratch::Scratch::dir("edith-panel-folds");
        let path = dir.join("panel-folds");
        assert!(load_folds_from(&dir.join("nothing-here")).is_empty());

        let open: BTreeSet<String> = ["eq", "speed"].into_iter().map(str::to_owned).collect();
        save_folds_to(&open, &path);
        assert_eq!(load_folds_from(&path), open);

        // Blank lines and padding are not folds.
        std::fs::write(&path, "  color \n\n\ntransform\n").expect("write");
        let loaded = load_folds_from(&path);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("color") && loaded.contains("transform"));
    }
}

#[cfg(test)]
mod cue_edit_tests {
    use super::*;

    fn edit(seed: &str) -> CueEdit {
        CueEdit::new(Lane::S1, 0, 0, seed)
    }

    #[test]
    fn select_all_is_replaced_by_the_next_stroke_and_backspace_clears_it() {
        let mut e = edit("Intro");
        e.select_all();
        e.typed('O');
        assert_eq!(e.text, "O");
        assert!(!e.all);
        e.select_all();
        e.backspace();
        assert_eq!(e.text, "");
        let mut e = edit("ab");
        e.select_all();
        e.newline();
        assert_eq!(e.text, "\n");
    }

    #[test]
    fn a_marker_name_opens_selected_whole_and_the_next_letter_replaces_it() {
        let mut m = MarkerEdit::new(0, MarkerField::Name, "Summer");
        assert!(m.all, "a rename opens with the whole name selected");
        assert!(!MarkerEdit::new(0, MarkerField::Note, "Summer").all);
        m.typed('I');
        assert_eq!(m.text, "I");
        assert!(!m.all);
    }

    #[test]
    fn typing_takes_any_printable_character_not_just_ascii() {
        let mut e = edit("");
        for c in "Ça va — 界".chars() {
            e.typed(c);
        }
        assert_eq!(e.text, "Ça va — 界");
        e.typed('\u{7}');
        e.typed('\t');
        assert_eq!(e.text, "Ça va — 界", "control characters are not words");
    }

    #[test]
    fn backspace_takes_back_one_whole_character_and_the_refusal_with_it() {
        let mut e = edit("é界");
        e.backspace();
        assert_eq!(e.text, "é", "one character, not one byte");
        e.refusal = Some("old".into());
        e.backspace();
        assert_eq!(e.text, "");
        assert_eq!(e.refusal, None);
        e.backspace();
        assert_eq!(e.text, "", "backspace on nothing is nothing");
    }

    #[test]
    fn shift_enter_is_a_line_break_inside_the_cue() {
        let mut e = edit("first");
        e.newline();
        e.typed('x');
        assert_eq!(e.text, "first\nx");
    }

    #[test]
    fn commit_writes_only_what_changed() {
        let mut e = edit("same");
        assert_eq!(e.commit("same"), None, "unchanged is no edit");
        e.typed('!');
        assert_eq!(e.commit("same"), Some("same!".to_string()));
    }

    #[test]
    fn the_cap_refuses_out_loud_and_is_not_below_500() {
        let mut e = edit(&"a".repeat(CUE_EDIT_CHARS - 1));
        e.typed('b');
        assert_eq!(e.text.chars().count(), CUE_EDIT_CHARS);
        assert_eq!(e.refusal, None, "the last one still fits");
        e.typed('c');
        assert_eq!(e.text.chars().count(), CUE_EDIT_CHARS);
        assert!(e.refusal.is_some(), "the one past it is refused");
    }
}

#[cfg(test)]
mod tip_place_tests {
    use super::*;

    /// A tooltip box (the plate plus its clear margin) at the right and bottom
    /// edges of a 1280x720 window stays whole inside it, and a long sentence
    /// is capped to the plate's max width so it wraps rather than running on.
    #[test]
    fn a_tooltip_stays_inside_the_window_at_every_edge() {
        let window = (1280., 720.);
        let long = "Quieter than this counts as silence. Lower it to keep more of the room tone and quiet words, raise it to cut harder.";
        let w = tip_w(&[long]);
        assert!(w <= TIP_MAX_W);
        let size = (w + TIP_CLEAR_X, 120. + TIP_CLEAR_Y);
        for mouse in [(1270., 300.), (300., 710.), (1275., 715.), (5., 5.), (640., 360.)] {
            let (x, y) = tip_origin(mouse, size, window);
            assert!(x >= 0. && y >= 0., "{mouse:?}");
            assert!(x + size.0 <= window.0 && y + size.1 <= window.1, "{mouse:?} -> {x},{y}");
        }
    }
}

/// A word that may change between two spellings (`Mute`/`Muted`, `Snap on`/
/// `Snap off`) without moving anything beside it: the slot is as wide as
/// `widest` (laid out but unseen), and the word is painted over it, left
/// aligned. Text metrics decide the width, so no pixel guess goes stale.
#[allow(dead_code, reason = "no worded toggle swaps spellings after REDESIGN §3.3; kept for the next one")]
pub(crate) fn steady_word(word: impl Into<SharedString>, widest: &'static str) -> Div {
    div()
        .relative()
        .flex_none()
        .whitespace_nowrap()
        .child(div().invisible().child(widest))
        .child(div().absolute().top_0().left_0().child(word.into()))
}
