//! The row atoms and helpers every property card shares: what a row's label,
//! value and step glyph paint in the darkroom (DESIGN §3/§4), and the card head.

use crate::layout::redesign as r;
use crate::ui::hitmap;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
use crate::ui::theme::tokens;
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::*;

/// DESIGN §3/§4 atoms shared by every param/setting card below (colour,
/// transform, speed, EQ, silence, mix, subtitle style): what a row's label
/// and value paint in the darkroom, so the seven card bodies each pick
/// tokens rather than reimplement the same font/colour swap seven times.
///
/// A row label is what the room says about the control -- Archivo, §3's
/// label-row size, `ink2` at rest / `ink1` picked (no permanent pill: the
/// picked state is a 1px `ink1` rule, §4's focus ring, not a fill).
pub(super) fn dark_row_label(text: impl Into<SharedString>, picked: bool) -> Div {
    div()
        .type_style(type_scale::body())
        .text_color(rgb(if picked { tokens().ink1 } else { tokens().ink2 }))
        .child(text.into())
}

/// A transform row's value with its unit: degrees for rotation, `×` for
/// scale (a multiplier, not a fraction of the frame), percent for
/// position/crop (both are a fraction of the frame's own size) -- the bare
/// `{value:.2}` this used to print could not tell a percent from a
/// multiplier apart.
pub(super) fn transform_row_value(band: usize, value: f32) -> String {
    if band == ROTATE_BAND {
        format!("{value:.0}°")
    } else if band == SCALE_BAND {
        format!("{value:.2}×")
    } else {
        format!("{:.0}%", value * 100.)
    }
}

/// A row's value -- what the film/the setting *says*, mono per §3. Ink1 once
/// it is changed from its default, ink3 while it sits there (DESIGN §9.1).
pub(super) fn dark_row_value(text: impl Into<SharedString>, changed: bool) -> Div {
    div()
        .type_style(type_scale::caption())
        .text_color(rgb(if changed { tokens().ink1 } else { tokens().ink2 }))
        .child(text.into())
}


/// A card's own head line: the verb in Archivo (§3 section-head casing) and,
/// where there is one, the clip it names in mono beside it -- "which clip"
/// is metadata about the footage, not the room's own voice. `help`, when
/// given, is the card's own how-to sentence -- it used to sit under the
/// head as a permanent line (a terminal-screen row of prose the user named
/// directly); now it rides a `?` glyph beside the head and only shows on
/// hover, the same hover-only convention [`dock_stance::ghost_verb`]
/// already uses for a verb's own description.
/// `maximize` is `None` for the one card this head builds that is not one of
/// the seven param cards (the export progress sheet): everything else passes
/// `Some(self.card_maximized)` and gets the affordance this session's
/// complaint asked for -- worn on the head (DESIGN.md:91's "chord is worn"),
/// so it never was a keyboard-only option (the repeated "some options are
/// only reachable via keyboard shortcut" complaint). A double-click anywhere
/// on the head does the same thing the glyph's click and the `m` chord do
/// ([`Player::toggle_maximize`]) -- mouse and key reach the same switch.
// `+ use<>` (edition 2024 precise capturing): without it, the elided
// lifetime on `cx` would be captured into the returned opaque type by
// default, so a hoisted `let head = true.then(|| dark_card_head(..., cx));`
// would keep `cx` borrowed until `head` is finally consumed -- fighting
// every other `cx.listener(...)` call built in between, which is exactly
// the E0500/E0501 chain this card's own hoist is here to avoid.
pub(super) fn dark_card_head(
    verb: &str,
    meta: Option<SharedString>,
    help: Option<SharedString>,
    maximize: Option<bool>,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    dark_card_head_reset(verb, meta, help, maximize, None, cx)
}

/// [`dark_card_head`] with the `Reset all` slot (DESIGN §9.1): the button shows
/// only when the card hands it a `reset`, and presses it through the card.
pub(super) fn dark_card_head_reset(
    _verb: &str,
    _meta: Option<SharedString>,
    help: Option<SharedString>,
    _maximize: Option<bool>,
    reset: Option<fn(&mut Player, &mut Context<Player>)>,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    // REDESIGN §3.9: the card lives under its own section head in the Clip
    // inspector, so the verb, the clip name, the back arrow and the close
    // cross are gone (the section head names it, and clicking the head folds
    // it). What stays is what only the card knows: its how-to sentence behind
    // a `circle-help` icon and the `Reset all` ghost button.
    let t = tokens();
    let any = help.is_some() || reset.is_some();
    div()
        .id("card-head")
        .flex_none()
        .when(any, |d| d.h(px(r::CONTROL_S)))
        .flex()
        .items_center()
        .gap(px(CARD_GAP))
        .child(div().flex_1())
        .children(help.map(|h| {
            div()
                .id("card-head-help")
                .flex_none()
                .size(px(r::CONTROL_S))
                .flex()
                .items_center()
                .justify_center()
                .tooltip(move |_, cx| cx.new(|_| OverlayTip(h.clone())).into())
                .child(Icon::CircleHelp.el(IconSize::Inline, t.ink3))
        }))
        .children(reset.map(|reset| {
            Button::new("card-head-reset-all")
                .label("Reset all")
                .icon(Icon::RotateCcw)
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Small)
                .build()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    reset(this, cx);
                    cx.notify();
                }))
        }))
}

/// A card's section header row: the section's `title` on the left (`section`
/// type, `ink2`), and on the right the `?` help (its sentence on hover) and
/// the `Reset all` ghost button. Each of the two is drawn only when given, and
/// the row is drawn only when there is something to put in it, so a card never
/// shows an empty bar with a lone `?`. `reset` is pressed through the card.
#[allow(dead_code, reason = "the cards adopt it next")]
pub(super) fn section_header_row(
    title: &str,
    help: Option<SharedString>,
    reset: Option<fn(&mut Player, &mut Context<Player>)>,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let t = tokens();
    let title: SharedString = title.to_string().into();
    div()
        .id("card-section-header")
        .flex_none()
        .w_full()
        .h(px(r::CONTROL_S))
        .flex()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .type_style(type_scale::section())
                .text_color(rgb(t.ink2))
                .child(title),
        )
        .children(help.map(|h| {
            div()
                .id("card-section-help")
                .flex_none()
                .size(px(r::CONTROL_S))
                .flex()
                .items_center()
                .justify_center()
                .tooltip(move |_, cx| cx.new(|_| OverlayTip(h.clone())).into())
                .child(Icon::CircleHelp.el(IconSize::Inline, t.ink3))
        }))
        .children(reset.map(|reset| {
            Button::new("card-section-reset-all")
                .label("Reset all")
                .icon(Icon::RotateCcw)
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Small)
                .build()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    reset(this, cx);
                    cx.notify();
                }))
        }))
}

/// A disclosure row ("More"): a chevron (right when closed, down when open)
/// and the label, one 28px click target; `ink2` at rest, `ink1` under the
/// pointer. `on_toggle` flips the card's own open flag. The chevron is the
/// Lucide icon, never a text glyph.
#[allow(dead_code, reason = "the cards adopt it next")]
pub(super) fn disclosure_row(
    id: &'static str,
    label: impl Into<SharedString>,
    open: bool,
    on_toggle: fn(&mut Player, &mut Context<Player>),
    cx: &mut Context<Player>,
) -> Stateful<Div> {
    let t = tokens();
    div()
        .id(id)
        .flex_none()
        .w_full()
        .h(px(r::CONTROL_M))
        .flex()
        .items_center()
        .gap(px(4.))
        .cursor_pointer()
        .type_style(type_scale::body_strong())
        .text_color(rgb(t.ink2))
        .hover(|s| s.text_color(rgb(tokens().ink1)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            on_toggle(this, cx);
            cx.notify();
        }))
        .child(if open { Icon::ChevronDown } else { Icon::ChevronRight }.el(IconSize::Inline, t.ink3))
        .child(label.into())
}

/// Gap between a card's head, rows and sections: one for every card.
pub(crate) const CARD_GAP: f32 = 6.;

impl Player {
    /// The outer element of every clip card. Docked (the normal case) it is a
    /// plain column in the Clip tab, in flow, so every card opens in the same
    /// place; only a maximized card still takes the window-space scrim.
    pub(super) fn card_outer(&self, viewport: Size<Pixels>, cx: &Context<Self>) -> Div {
        match self.card_maximized {
            false => div().w_full().flex_none().flex().flex_col(),
            true => drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .top(px(crate::ui::stance::maximized_card_top(
                    f32::from(viewport.height),
                    self.split_px(Split::Bench, viewport),
                )))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                ),
        }
    }

    /// The panel inside [`Self::card_outer`]: bare in the dock, the bordered
    /// plate when maximized.
    pub(super) fn card_surface<S: Styled>(&self, d: S) -> S {
        match self.card_maximized {
            false => d,
            true => d
                .p(px(12.))
                .rounded(px(6.))
                .bg(rgb(DARK_PANEL()))
                .border_1()
                .border_color(rgba(DARK_SEAM())),
        }
    }
}

/// The help/status line under a card's head: what the keys do, which is
/// metadata about the room's controls -- mono, §3.
pub(super) fn dark_help(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .min_w(px(0.))
        .type_style(type_scale::caption())
        .text_color(rgb(tokens().ink3))
        .child(text.into())
}

/// A ghost action row (DESIGN §4): borderless label + its chord, never a
/// filled box -- the shape every card's Reset/Flatten/Add/Remove/toggle
/// button takes now that "boxes are commitments" and Export is the one box
/// in the room. `active` is a toggle's own held-on state (EQ's spectrum
/// switch), not hover -- it keeps the fill and brightens the ink the same
/// way a picked param row's ring does.
pub(super) fn dark_ghost_button(
    id: impl Into<gpui::ElementId>,
    text: impl Into<SharedString>,
    chord: &str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let text: SharedString = text.into();
    let t = tokens();
    let hitmap = hitmap::enabled().then(|| {
        let id = id.to_string();
        let label = text.clone();
        hitmap::dynamic(move || (format!("card.{id}"), label.to_string()), true)
    });
    // REDESIGN §1.5: a secondary button (`e2`, `line` border), `e3` and `ink1`
    // while it is the one that is on. Keys live in the tooltip, never on it.
    div()
        .id(id)
        .flex_1()
        .flex()
        .h(px(r::CONTROL_M))
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(r::R_CONTROL))
        .border_1()
        .border_color(rgba(t.line))
        .bg(rgb(if active { t.e3 } else { t.e2 }))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(t.e3)))
        .active(move |s| s.bg(rgb(t.e4)))
        .on_click(on_click)
        // A tooltip that only repeats the label is noise: only a chord earns one.
        .when(!chord.is_empty(), |d| {
            d.tooltip(crate::ui::widgets::overlay_tip_hover(&text, chord, None))
        })
        .children(hitmap.flatten())
        .child(
            div()
                .type_style(type_scale::body_strong())
                .text_color(rgb(if active { t.ink1 } else { t.ink2 }))
                .child(text),
        )
}

/// A two-state toggle button (Mute, Solo): when `on` it takes a fill and a border of `tint` and
/// its label in `tint`, so the held state reads at a glance; off, it is the plain ghost button.
/// The border is drawn either way, so the button is the same size on and off. `tip` says what the
/// switch does, in plain words, on hover.
pub(super) fn dark_toggle_button(
    id: impl Into<gpui::ElementId>,
    text: impl Into<SharedString>,
    on: bool,
    tint: u32,
    tip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let text: SharedString = text.into();
    let t = tokens();
    let hitmap = hitmap::enabled().then(|| {
        let id = id.to_string();
        let label = text.clone();
        hitmap::dynamic(move || (format!("card.{id}"), label.to_string()), true)
    });
    let fill = match on {
        true => crate::ui::theme::mix(t.e2, tint, 0.22),
        false => t.e2,
    };
    div()
        .id(id)
        .flex_1()
        .flex()
        .h(px(r::CONTROL_M))
        .items_center()
        .justify_center()
        .rounded(px(r::R_CONTROL))
        .border_1()
        .border_color(match on {
            true => rgb(tint),
            false => rgba(t.line),
        })
        .bg(rgb(fill))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(match on {
            true => crate::ui::theme::mix(t.e3, tint, 0.22),
            false => t.e3,
        })))
        .active(move |s| s.bg(rgb(t.e4)))
        .tooltip(crate::ui::widgets::overlay_tip_hover(&text, "", Some(tip)))
        .on_click(on_click)
        .children(hitmap.flatten())
        .child(
            div()
                .type_style(type_scale::body_strong())
                .text_color(rgb(if on { tint } else { t.ink2 }))
                .child(text),
        )
}
