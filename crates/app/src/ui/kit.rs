//! REDESIGN §1.5 / §6 -- the component kit every redesigned region is built
//! from: one set of hover / pressed / focus / disabled rules (§1.5), three
//! button styles, toggle, segmented control, switch, scrub-number field,
//! section head and keycap.
//!
//! These sit beside the legacy helpers in `ui::widgets` (`control`, `tip_*`,
//! `focus_ring`), which stay for the existing call sites; later phases move
//! regions over to this module and then retire the legacy ones.
//!
//! Rules every component here keeps:
//! * **No layout shift.** Sizes are fixed by the style; hover, press, focus
//!   and "on" change colour only (the one border a component has is always
//!   there, only its colour changes).
//! * **Colours come from [`theme::tokens`]**, never a literal.
//! * **State is overridable.** Each builder takes an optional [`Look`] that
//!   forces a visual state, which is how the component gallery paints every
//!   state at once and how a keyboard-focused control (whose handle the
//!   caller owns) is drawn.
//! * Builders return a plain `Stateful<Div>`: chain `.on_click(..)`,
//!   `.tooltip(..)`, drag handlers on the result. A disabled component draws
//!   dimmed with a not-allowed cursor and should be given no handler.

use crate::layout::redesign as r;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::theme::{self, tokens};
use crate::ui::type_scale::{self, Typeset};
use crate::*;
use gpui::{BoxShadow, Hsla, ScrollHandle, SharedString, Stateful, hsla};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

fn c(x: u32) -> Hsla {
    rgb(x).into()
}

fn ca(x: u32) -> Hsla {
    rgba(x).into()
}

/// A forced visual state (see the module doc); `None` means live (pointer-driven).
/// `Focus` keeps the rest fill and adds the accent ring; the others pin the
/// fill hover or press would give.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Hover,
    Pressed,
    Focus,
}

/// A control's three fills: at rest, under the pointer, pressed.
#[derive(Clone, Copy, Debug)]
pub struct Fills {
    pub rest: Hsla,
    pub hover: Hsla,
    pub press: Hsla,
}

impl Fills {
    /// Transparent at rest, a white wash (6% / 10%) on hover / press: ghost
    /// buttons, toolbar buttons, menu rows, dock tabs.
    pub fn ghost() -> Self {
        Self {
            rest: hsla(0., 0., 0., 0.),
            hover: ca(theme::WASH_HOVER),
            press: ca(theme::WASH_PRESS),
        }
    }

    /// `e2` at rest, `e3` hover, `e4` pressed: secondary buttons, fields.
    pub fn solid() -> Self {
        let t = tokens();
        Self { rest: c(t.e2), hover: c(t.e3), press: c(t.e4) }
    }

    /// A toggle that is on: `e3`, one step up under the pointer.
    pub fn on() -> Self {
        let t = tokens();
        Self { rest: c(t.e3), hover: c(t.e4), press: c(theme::mix(t.e4, t.ink1, 0.06)) }
    }

    /// The primary button: `accent_fill`, darker on press.
    pub fn primary() -> Self {
        let t = tokens();
        Self {
            rest: c(t.accent_fill),
            hover: c(t.accent_press),
            press: c(theme::mix(t.accent_fill, 0x000000, 0.15)),
        }
    }
}

/// The keyboard-focus ring (§1.5): the control's own 1px border turns `accent`.
/// Every component here carries a 1px border that is transparent at rest, so
/// focus never moves a neighbour. (A spread shadow was tried first: gpui
/// paints it *under* the element, so a transparent ghost button filled solid
/// accent; and the spec's 2px/1px-offset needs the parent surface colour.)
/// Needs the element to hold a gpui focus handle to show live; `Look::Focus`
/// forces it.
pub fn focus_ring_accent<E: InteractiveElement + Styled>(el: E) -> E {
    // The ring is for the keyboard: a click also focuses a control, and must not light it.
    el.focus(|s| if KBD_RING.load(Ordering::Relaxed) { s.border_color(c(tokens().accent)) } else { s })
}

/// Whether the last input was the keyboard (Tab in a dialog). Set on Tab, cleared by any
/// mouse press, read when a focused control paints its ring.
static KBD_RING: AtomicBool = AtomicBool::new(false);

pub fn set_kbd_ring(on: bool) {
    KBD_RING.store(on, Ordering::Relaxed);
}

thread_local! {
    /// The focus handles a dialog's controls take while it is being built, and how many are used.
    static TAB_POOL: RefCell<(Option<Vec<FocusHandle>>, usize)> = const { RefCell::new((None, 0)) };
}

/// The open [`choice_list`] of the last frame, for the arrow keys: which one, how many rows,
/// the highlighted row, and the pick handler.
type PickFn = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(Default)]
struct OpenList {
    id: &'static str,
    len: usize,
    hi: usize,
    pick: Option<PickFn>,
}

thread_local! {
    static OPEN_LIST: RefCell<OpenList> = RefCell::new(OpenList::default());
    /// The scroll position of each choice list by its id, so the arrow keys can bring the
    /// highlighted row into view.
    static LIST_SCROLL: RefCell<std::collections::HashMap<&'static str, ScrollHandle>> = RefCell::new(Default::default());
}

/// The tallest a [`choice_list`] gets before it scrolls: a long list (a font list, say) stays on
/// the window and its rows are reached with the wheel or the arrow keys.
pub const LIST_MAX_H: f32 = 320.;

/// How tall a [`choice_list`] of `rows` rows is: its rows and padding, capped at [`LIST_MAX_H`].
pub fn list_height(rows: usize) -> f32 {
    (rows as f32 * crate::layout::MENU_ROW_H + 2. * crate::layout::MENU_PAD).min(LIST_MAX_H)
}

/// Whether the list of `rows` rows for the dropdown field `field` opens above the field, because
/// there is no room for it below on the window. Only the `top` or `bottom` offset of the list
/// changes: the list stays 2px clear of the field, below it or above it.
pub fn list_opens_above(field: &'static str, rows: usize) -> bool {
    crate::ui::anchor::opens_above(field, list_height(rows))
}

/// The scroll handle of the list `id`.
fn list_scroll(id: &'static str) -> ScrollHandle {
    LIST_SCROLL.with(|m| m.borrow_mut().entry(id).or_default().clone())
}

/// Start a frame: no handle is used yet, and no list is known to be open until one draws.
/// Call once before any [`tab_scope`].
pub fn tab_frame_reset() {
    TAB_POOL.with(|p| p.borrow_mut().1 = 0);
    OPEN_LIST.with(|l| {
        let mut l = l.borrow_mut();
        if l.pick.take().is_none() {
            l.id = "";
        }
    });
}

/// Up, Down and Enter for the [`choice_list`] that is open: Up and Down move its highlighted
/// row (wrapping), Enter picks it. `true` when the key was the list's (the caller repaints).
pub fn list_nav(key: &str, window: &mut Window, cx: &mut App) -> bool {
    let (id, len, hi, pick) = OPEN_LIST.with(|l| {
        let l = l.borrow();
        (l.id, l.len, l.hi, l.pick.clone())
    });
    let Some(pick) = pick else { return false };
    if len == 0 {
        return false;
    }
    match key {
        "up" | "down" => {
            let to = if key == "down" { (hi + 1) % len } else { (hi + len - 1) % len };
            OPEN_LIST.with(|l| l.borrow_mut().hi = to);
            list_scroll(id).scroll_to_item(to);
            true
        }
        "enter" => {
            pick(hi, window, cx);
            true
        }
        _ => false,
    }
}

/// Runs `build`, giving every kit control made inside it (`states`) a tab stop from `pool`, in
/// build order. Controls built outside a scope stay out of the Tab order, so a dialog's Tab
/// never walks the window behind it.
pub fn tab_scope<R>(pool: &[FocusHandle], build: impl FnOnce() -> R) -> R {
    TAB_POOL.with(|p| p.borrow_mut().0 = Some(pool.to_vec()));
    let out = build();
    TAB_POOL.with(|p| p.borrow_mut().0 = None);
    out
}

/// A row-like control that is not a kit button (a settings row, a chip, a swatch): a tab stop
/// inside a [`tab_scope`], lit with an accent wash while the keyboard is on it.
pub fn kbd_stop<E: InteractiveElement + Styled>(el: E) -> E {
    tab_stop(el).focus(|s| if KBD_RING.load(Ordering::Relaxed) { s.bg(c(tokens().accent).opacity(0.22)) } else { s })
}

fn tab_stop<E: InteractiveElement>(el: E) -> E {
    let h = TAB_POOL.with(|p| {
        let mut p = p.borrow_mut();
        let at = p.1;
        let h = p.0.as_ref().and_then(|pool| pool.get(at).cloned());
        if h.is_some() {
            p.1 += 1;
        }
        h
    });
    match h {
        Some(h) => el.track_focus(&h),
        None => el,
    }
}

/// Applies the §1.5 state table to an element (which must carry a 1px border,
/// see [`focus_ring_accent`]): the fill for each state, the focus border, the
/// pointer, and the disabled dimming. `forced` pins a state instead of
/// waiting for the pointer.
pub fn states<E>(el: E, fills: Fills, enabled: bool, forced: Option<Look>) -> E
where
    E: StatefulInteractiveElement + Styled,
{
    if !enabled {
        // Dimmed but still legible: a disabled label must stay readable (it often carries the reason).
        return el.bg(fills.rest).opacity(0.6).cursor_not_allowed();
    }
    match forced {
        Some(Look::Hover) => el.bg(fills.hover).cursor_pointer(),
        Some(Look::Pressed) => el.bg(fills.press).cursor_pointer(),
        Some(Look::Focus) => el.bg(fills.rest).border_color(c(tokens().accent)).cursor_pointer(),
        None => focus_ring_accent(
            tab_stop(el).bg(fills.rest)
                .cursor_pointer()
                .hover(move |s| s.bg(fills.hover))
                .active(move |s| s.bg(fills.press)),
        ),
    }
}

/// The three button styles (§1.5). Exactly these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonStyle {
    /// `accent_fill`, dark `on_accent` text; one per surface (Export, Apply, Import).
    Primary,
    /// `e2` fill, `line` border, `ink1` text.
    Secondary,
    /// Transparent: toolbars, inspector header actions.
    Ghost,
}

/// Control heights (§1.2): 24 small, 28 default, 32 toolbar / primary, 36 hero.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonSize {
    Small,
    Default,
    Large,
    Hero,
}

impl ButtonSize {
    pub const fn height(self) -> f32 {
        match self {
            ButtonSize::Small => r::CONTROL_S,
            ButtonSize::Default => r::CONTROL_M,
            ButtonSize::Large => r::CONTROL_L,
            ButtonSize::Hero => r::CONTROL_XL,
        }
    }

    fn pad(self) -> f32 {
        match self {
            ButtonSize::Small => 8.,
            ButtonSize::Default => 12.,
            ButtonSize::Large => 14.,
            ButtonSize::Hero => 16.,
        }
    }

    fn icon(self) -> IconSize {
        match self {
            ButtonSize::Small | ButtonSize::Default => IconSize::Inline,
            ButtonSize::Large | ButtonSize::Hero => IconSize::Toolbar,
        }
    }

    fn text(self) -> type_scale::TypeStyle {
        match self {
            ButtonSize::Small => type_scale::caption_strong(),
            ButtonSize::Default | ButtonSize::Large => type_scale::body_strong(),
            ButtonSize::Hero => type_scale::emph(),
        }
    }
}

/// A button, icon button or toggle, depending on what it is given:
/// a label, an icon, both, and (for the ghost style) `.on(true)`.
///
/// ```ignore
/// Button::new("export").label("Export").style(ButtonStyle::Primary).build()
///     .on_click(|_, _, cx| ..)
/// Button::new("snap").icon(Icon::Magnet).label("Snap").size(ButtonSize::Large).on(snap).build()
/// Button::new("play").icon(Icon::Play).size(ButtonSize::Large).build()   // icon-only: square
/// ```
pub struct Button {
    id: &'static str,
    label: Option<SharedString>,
    icon: Option<Icon>,
    style: ButtonStyle,
    size: ButtonSize,
    on: bool,
    enabled: bool,
    look: Option<Look>,
    min_w: f32,
}

impl Button {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            label: None,
            icon: None,
            style: ButtonStyle::Secondary,
            size: ButtonSize::Default,
            on: false,
            enabled: true,
            look: None,
            min_w: 0.,
        }
    }
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }
    /// A toggle that is on: `e3` fill, `accent` icon and label. Ghost style only.
    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    /// Force a visual state (gallery, or a keyboard-focused control).
    pub fn look(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
    /// A width floor, for a label that changes ("Export" / "Cancel") so its
    /// neighbours do not move.
    pub fn min_w(mut self, w: f32) -> Self {
        self.min_w = w;
        self
    }

    pub fn build(self) -> Stateful<Div> {
        let t = tokens();
        let h = self.size.height();
        let on = self.on && self.style == ButtonStyle::Ghost;
        let fills = match (self.style, on) {
            (ButtonStyle::Primary, _) => Fills::primary(),
            (ButtonStyle::Secondary, _) => Fills::solid(),
            (ButtonStyle::Ghost, true) => Fills::on(),
            (ButtonStyle::Ghost, false) => Fills::ghost(),
        };
        // Text and icon colour: rest -> hover/press -> on -> disabled.
        let (rest_ink, hover_ink) = match (self.style, on) {
            (ButtonStyle::Primary, _) => (t.on_accent, t.on_accent),
            (ButtonStyle::Secondary, _) => (t.ink1, t.ink1),
            (_, true) => (t.accent, t.accent),
            (ButtonStyle::Ghost, false) => (t.ink2, t.ink1),
        };
        let forced_ink = match self.look {
            Some(Look::Hover | Look::Pressed) => hover_ink,
            _ => rest_ink,
        };
        let ink = if self.enabled { forced_ink } else { t.ink3 };
        let group: SharedString = format!("btn-{}", self.id).into();
        let square = self.label.is_none();
        let mut el = div()
            .id(self.id)
            .group(group.clone())
            .flex_none()
            .h(px(h))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .rounded(px(r::R_CONTROL))
            .border_1()
            .border_color(match self.style {
                ButtonStyle::Secondary => ca(t.line),
                _ => hsla(0., 0., 0., 0.),
            })
            .text_color(c(ink))
            .type_style(self.size.text());
        el = if square { el.w(px(h)) } else { el.px(px(self.size.pad())) };
        if self.min_w > 0. {
            el = el.min_w(px(self.min_w));
        }
        if let Some(icon) = self.icon {
            let mut glyph = icon.el(self.size.icon(), ink);
            if self.enabled && self.look.is_none() && hover_ink != rest_ink {
                glyph = glyph.group_hover(group.clone(), move |s| s.text_color(c(hover_ink)));
            }
            el = el.child(glyph);
        }
        if let Some(label) = self.label {
            el = el.child(label);
        }
        let mut el = states(el, fills, self.enabled, self.look);
        if self.style == ButtonStyle::Primary && self.enabled && self.look.is_none() {
            // An accent ring on an accent fill is invisible: the primary's ring is the ink.
            el = el.focus(|s| if KBD_RING.load(Ordering::Relaxed) { s.border_color(c(t.ink1)) } else { s });
        }
        if self.enabled && self.look.is_none() && hover_ink != rest_ink {
            el = el.hover(move |s| s.text_color(c(hover_ink)));
        }
        el
    }
}

/// One entry of a [`segmented`] control.
pub struct Segment {
    pub label: SharedString,
    pub icon: Option<Icon>,
    /// This pill alone cannot be chosen: drawn in `ink3` (still readable), no hover, no click.
    pub disabled: bool,
}

impl Segment {
    /// Marks this pill as unavailable while the others stay live.
    #[allow(dead_code)] // no pill is disabled today; the look and the guard in `segmented` stay ready
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn text(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), icon: None, disabled: false }
    }
}

/// The segmented control (§1.5): an `e2` track, the selected pill `e3`.
/// 28px tall, every pill the same weight so selecting never reflows the row.
/// `on_select` gets the index of the pill clicked. `forced` pins one pill's
/// state (gallery). This replaces every "Off On" text row.
pub fn segmented(
    id: &'static str,
    items: &[Segment],
    selected: usize,
    enabled: bool,
    forced: Option<(usize, Look)>,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    segmented_in(SegLayout::Hug, id, items, selected, enabled, forced, on_select)
}

/// [`segmented`] that fills the width it is given with every pill the same
/// width (`flex_1`), for rows of a few short options ("Left Centre Right")
/// where a ragged track looks unfinished. Put it in a parent with a width.
pub fn segmented_equal(
    id: &'static str,
    items: &[Segment],
    selected: usize,
    enabled: bool,
    forced: Option<(usize, Look)>,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    segmented_in(SegLayout::Equal, id, items, selected, enabled, forced, on_select)
}

/// [`segmented`] for more options than fit one row of a narrow panel: the
/// track takes the parent's width and the pills wrap onto further lines, so a
/// label is never clipped. The track grows by a pill row per wrap (so give it
/// room, or reserve the height of the wrapped case).
pub fn segmented_wrap(
    id: &'static str,
    items: &[Segment],
    selected: usize,
    enabled: bool,
    forced: Option<(usize, Look)>,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    segmented_in(SegLayout::Wrap, id, items, selected, enabled, forced, on_select)
}

/// How a segmented track sizes itself.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SegLayout {
    /// As wide as its pills.
    Hug,
    /// The parent's width, pills of equal width.
    Equal,
    /// The parent's width, pills wrap onto more lines.
    Wrap,
}

fn segmented_in(
    layout: SegLayout,
    id: &'static str,
    items: &[Segment],
    selected: usize,
    enabled: bool,
    forced: Option<(usize, Look)>,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let t = tokens();
    let mut track = div()
        .flex()
        .items_center()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(r::R_CONTROL))
        .bg(c(t.e2))
        .when(!enabled, |d| d.opacity(0.4));
    track = match layout {
        SegLayout::Hug => track.flex_none().h(px(r::CONTROL_M)),
        SegLayout::Equal => track.w_full().h(px(r::CONTROL_M)),
        SegLayout::Wrap => track.w_full().min_h(px(r::CONTROL_M)).flex_wrap(),
    };
    for (i, item) in items.iter().enumerate() {
        let is_sel = i == selected;
        let look = forced.filter(|(f, _)| *f == i).map(|(_, l)| l);
        let fills = if is_sel {
            Fills { rest: c(t.e3), hover: c(t.e3), press: c(t.e4) }
        } else {
            Fills::ghost()
        };
        let live = enabled && !item.disabled;
        let ink = match (is_sel, look) {
            _ if item.disabled => t.ink3,
            (true, _) => t.ink1,
            (false, Some(Look::Hover | Look::Pressed)) => t.ink1,
            (false, _) => t.ink2,
        };
        let mut pill = div()
            .id((id, i))
            .h(px(r::CONTROL_S))
            .px(px(10.))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .gap(px(6.))
            .rounded(px(r::R_CLIP))
            .border_1()
            .border_color(hsla(0., 0., 0., 0.))
            .type_style(type_scale::body_strong())
            .text_color(c(if enabled { ink } else { t.ink4 }));
        pill = match layout {
            SegLayout::Equal => pill.flex_1().min_w(px(0.)).justify_center(),
            _ => pill.flex_none(),
        };
        if let Some(icon) = item.icon {
            pill = pill.child(icon.el(IconSize::Inline, if enabled { ink } else { t.ink4 }));
        }
        if !item.label.is_empty() {
            pill = pill.child(item.label.clone());
        }
        let mut pill = if item.disabled { pill.cursor_not_allowed() } else { states(pill, fills, enabled, look) };
        if live && look.is_none() && !is_sel {
            pill = pill.hover(move |s| s.bg(fills.hover).text_color(c(tokens().ink1)));
        }
        if live {
            let on_select = on_select.clone();
            pill = pill.on_click(move |_, window, cx| on_select(i, window, cx));
        }
        track = track.child(pill);
    }
    track
}

/// The switch (§1.5): a 32x18 track and a 12px knob. On is `accent_fill` with
/// a dark knob; off is `e3` with an `ink2` knob. The knob is absolutely
/// placed, so the control's box is the same either way.
pub fn switch(id: impl Into<gpui::ElementId>, on: bool, enabled: bool, look: Option<Look>) -> Stateful<Div> {
    let t = tokens();
    let fills = if on {
        Fills::primary()
    } else {
        Fills { rest: c(t.e3), hover: c(t.e4), press: c(theme::mix(t.e4, t.ink1, 0.06)) }
    };
    let knob = div()
        .absolute()
        .top(px(2.))
        .left(px(if on { 16. } else { 2. }))
        .size(px(12.))
        .rounded_full()
        .bg(c(if on { t.on_accent } else { t.ink2 }));
    states(
        div()
            .id(id)
            .flex_none()
            .relative()
            .w(px(32.))
            .h(px(18.))
            .rounded_full()
            .border_1()
            .border_color(if on { hsla(0., 0., 0., 0.) } else { ca(t.line_hi) })
            .child(knob),
        fills,
        enabled,
        look,
    )
}

/// Which state a [`scrub_field`] is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrubLook {
    Hover,
    /// The user is typing a value: 1px `accent` border and a caret.
    Typing,
}

/// The scrub-number field (§6): 24px, `e2`; hover `e3` with a `col-resize`
/// cursor; typing shows a 1px `accent` border and a caret. The value is
/// `caption` with tabular figures, the unit `ink3`. The 1px border is always
/// there (transparent unless typing), so focusing never shifts the box.
/// Dragging and typing are the caller's: attach `on_drag` / `on_click` to the
/// result.
pub fn scrub_field(
    id: &'static str,
    w: f32,
    value: impl Into<SharedString>,
    unit: Option<&'static str>,
    enabled: bool,
    look: Option<ScrubLook>,
) -> Stateful<Div> {
    let t = tokens();
    let typing = look == Some(ScrubLook::Typing);
    let fills = Fills::solid();
    let mut el = div()
        .id(id)
        .flex_none()
        .w(px(w))
        .h(px(r::CONTROL_S))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(2.))
        .rounded(px(r::R_CONTROL))
        .border_1()
        .border_color(if typing { c(t.accent) } else { hsla(0., 0., 0., 0.) })
        .type_style(type_scale::caption())
        .text_color(c(if enabled { t.ink1 } else { t.ink4 }))
        .child(value.into());
    if typing {
        el = el.child(div().w(px(1.)).h(px(14.)).bg(c(t.accent)));
    }
    if let Some(unit) = unit {
        el = el.child(div().text_color(c(t.ink3)).child(unit));
    }
    if !enabled {
        // Dimmed but still legible: a disabled label must stay readable (it often carries the reason).
        return el.bg(fills.rest).opacity(0.6).cursor_not_allowed();
    }
    match look {
        Some(ScrubLook::Hover) => el.bg(fills.hover).cursor(CursorStyle::ResizeLeftRight),
        Some(ScrubLook::Typing) => el.bg(fills.rest),
        _ => el.bg(fills.rest).hover(move |s| s.bg(fills.hover)).cursor(CursorStyle::ResizeLeftRight),
    }
}

/// An inspector section head (§1.1 `section`): 12px Inter 600, sentence case,
/// `ink2`, in a 24px row; `trailing` (a reset button, a chevron) sits at the right.
pub fn section_head(text: impl Into<SharedString>, trailing: Option<AnyElement>) -> Div {
    div()
        .flex_none()
        .h(px(r::CONTROL_S))
        .flex()
        .items_center()
        .justify_between()
        .type_style(type_scale::section())
        .text_color(c(tokens().ink2))
        .child(text.into())
        .children(trailing)
}

/// A key shown in a tooltip, menu or sheet (§5 rule 1: nowhere else): `e4`
/// plate, `line_hi` border, `caption` text.
pub fn keycap(text: impl Into<SharedString>) -> Div {
    let t = tokens();
    div()
        .flex_none()
        .h(px(18.))
        .min_w(px(18.))
        .px(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r::R_CONTROL))
        .border_1()
        .border_color(ca(t.line_hi))
        .bg(c(t.e4))
        .type_style(type_scale::caption())
        .text_color(c(t.ink2))
        .child(text.into())
}

/// The one popup plate (§1.6): `e3`, 1px `line_hi` border, radius 6 (8 for
/// sheets and the palette), soft shadow. Menus, dropdowns, tooltips and toasts
/// all start from this.
pub fn plate(radius: f32) -> Div {
    let t = tokens();
    div()
        .bg(c(t.e3))
        .border_1()
        .border_color(ca(t.line_hi))
        .rounded(px(radius))
        .shadow(vec![BoxShadow {
            color: ca(theme::PLATE_SHADOW),
            offset: point(px(0.), px(8.)),
            blur_radius: px(24.),
            spread_radius: px(0.),
        }])
}

// ---------------------------------------------------------------------------
// Card components: what the property cards, the sheets and the dialogs are
// built from. Each takes its handler as an argument and attaches it only when
// the control is live, so a disabled one cannot be driven by a stray click.
// ---------------------------------------------------------------------------

/// A switch row: the label (and, under it, an optional one-line description in
/// `ink3`) on the left, the [`switch`] on the right, the whole row one click
/// target. `off_why` disables it: the row dims, the cursor says not-allowed, a
/// hover tooltip gives the reason, and `on_toggle` is never attached, so the
/// row ignores input.
///
/// ```ignore
/// switch_row("loop", "Loop playback", Some("Start again at the end".into()), on, None,
///     move |_, cx| entity.update(cx, |p, cx| p.toggle_loop(cx)))
/// ```
pub fn switch_row(
    id: &'static str,
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    on: bool,
    off_why: Option<&str>,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let t = tokens();
    let label: SharedString = label.into();
    let enabled = off_why.is_none();
    let row = div()
        .id(id)
        .w_full()
        .flex_none()
        .min_h(px(r::CONTROL_M))
        .py(px(2.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .type_style(type_scale::body())
                        .text_color(c(if enabled { t.ink1 } else { t.ink4 }))
                        .child(label.clone()),
                )
                .children(description.map(|d| {
                    div().type_style(type_scale::caption()).text_color(c(if enabled { t.ink3 } else { t.ink4 })).child(d)
                })),
        )
        .child(switch((id, 1usize), on, enabled, None));
    match off_why {
        Some(why) => row
            .cursor_not_allowed()
            .tooltip(crate::ui::widgets::overlay_tip_hover(&label, "", Some(why))),
        None => row.cursor_pointer().on_click(move |_, window, cx| on_toggle(window, cx)),
    }
}

/// The reset slot: a 24px square that is *always* in the layout. It holds the
/// ghost reset icon (tooltip "Reset to default") while `show`, and is empty
/// otherwise, so a value that changes never nudges its neighbours. The click is
/// attached only while shown.
pub fn reset_slot(id: &'static str, show: bool, on_reset: impl Fn(&mut Window, &mut App) + 'static) -> Div {
    let slot = div().flex_none().size(px(r::CONTROL_S)).flex().items_center().justify_center();
    if !show {
        return slot;
    }
    slot.child(
        Button::new(id)
            .icon(Icon::RotateCcw)
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Small)
            .build()
            .tooltip(crate::ui::widgets::overlay_tip_hover("Reset to default", "", None))
            .on_click(move |_, window, cx| on_reset(window, cx)),
    )
}

/// A value pill: a fixed-width `e2` box with the value right-aligned in tabular
/// `caption` (`ink1` once changed from the default, `ink2` at the default) and
/// the [`reset_slot`] beside it. Width and slot never change, so editing a
/// value moves nothing.
pub fn value_pill(
    id: &'static str,
    w: f32,
    value: impl Into<SharedString>,
    unit: Option<&'static str>,
    changed: bool,
    on_reset: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    let t = tokens();
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.))
        .child(
            div()
                .flex_none()
                .w(px(w))
                .h(px(r::CONTROL_S))
                .px(px(8.))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(2.))
                .rounded(px(r::R_CONTROL))
                .bg(c(t.e2))
                .whitespace_nowrap()
                .type_style(type_scale::caption())
                .text_color(c(if changed { t.ink1 } else { t.ink2 }))
                .child(value.into())
                .children(unit.map(|u| div().text_color(c(t.ink3)).child(u))),
        )
        .child(reset_slot(id, changed, on_reset))
}

/// A number being typed into a [`number_field`]: the buffer, with the first
/// keystroke replacing the old text (it opens fully selected). Wire it like
/// this: a click (or double-click) on the field makes
/// `NumberEdit::begin(&shown)`; each key goes to [`NumberEdit::key`]; on
/// `Commit` call [`NumberEdit::commit`] and write the value, on `Cancel` just
/// drop the edit. Dragging the field is [`scrub_value`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberEdit {
    field: crate::player::text_field::TextField,
}

impl NumberEdit {
    /// Opens on `shown` (the text the field displayed), all selected.
    pub fn begin(shown: &str) -> Self {
        Self { field: crate::player::text_field::TextField::selected(shown) }
    }

    /// The buffer, for [`number_field`]'s `editing`.
    pub fn text(&self) -> &str {
        &self.field.text
    }

    /// One key: digits, `.`, `,` and `-` are typed (anything else is ignored),
    /// backspace deletes, Enter commits, Esc cancels.
    #[allow(dead_code, reason = "the cards adopt it next")]
    pub fn key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>) -> crate::player::text_field::FieldKey {
        let typed: Option<String> = key_char
            .map(|k| k.chars().filter(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-')).collect::<String>())
            .filter(|k| !k.is_empty());
        // A letter or symbol that is not part of a number changes nothing.
        if typed.is_none() && key_char.is_some() && !matches!(key, "enter" | "escape" | "backspace") {
            return crate::player::text_field::FieldKey::Edited;
        }
        self.field.key(key, ctrl, alt, typed.as_deref(), 12)
    }

    /// The typed number clamped to `min..=max`, or `None` when the buffer is
    /// not a number (the caller then keeps the old value). `,` reads as `.`.
    #[allow(dead_code, reason = "the cards adopt it next")]
    pub fn commit(&self, min: f64, max: f64) -> Option<f64> {
        let v: f64 = self.field.text.trim().replace(',', ".").parse().ok()?;
        v.is_finite().then(|| v.clamp(min, max))
    }
}

/// The value after dragging a [`number_field`] `dx` pixels from where the drag
/// began at `start`: `per_px` units a pixel (a tenth of that with `fine`, which
/// callers wire to Shift), clamped to `min..=max`.
#[allow(dead_code, reason = "the cards adopt it next")]
pub fn scrub_value(start: f64, dx: f32, per_px: f64, fine: bool, min: f64, max: f64) -> f64 {
    let per = if fine { per_px / 10. } else { per_px };
    (start + f64::from(dx) * per).clamp(min, max)
}

/// The number field (§6): a [`scrub_field`] that also shows what is being
/// typed. `editing` is the [`NumberEdit`] buffer while the user types (1px
/// `accent` border, caret), `None` at rest, where the field drags to scrub
/// (`col-resize` cursor). Compose with a [`reset_slot`] to its right for the
/// reset-to-default affordance. Drag, click and key wiring are the caller's.
pub fn number_field(
    id: &'static str,
    w: f32,
    value: impl Into<SharedString>,
    unit: Option<&'static str>,
    editing: Option<&NumberEdit>,
    enabled: bool,
) -> Stateful<Div> {
    match editing {
        Some(edit) => scrub_field(id, w, edit.text().to_string(), unit, enabled, Some(ScrubLook::Typing)),
        None => scrub_field(id, w, value, unit, enabled, None),
    }
}

/// `#RRGGBB` for an `0xRRGGBB` colour.
pub fn hex_text(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0xff_ffff)
}

/// The colour a typed `#RRGGBB` (or `RRGGBB`, or `#RGB`) names.
#[allow(dead_code, reason = "the cards adopt it next")]
pub fn parse_hex(text: &str) -> Option<u32> {
    let h = text.trim().trim_start_matches('#');
    match h.len() {
        6 => u32::from_str_radix(h, 16).ok(),
        3 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            let (r_, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some(((r_ * 0x11) << 16) | ((g * 0x11) << 8) | (b * 0x11))
        }
        _ => None,
    }
}

/// The swatch row: round swatches, the current colour wearing a ring. A
/// current colour that is not in `colours` (a custom one) is shown as an extra
/// swatch at the end, ringed, so the row always says what is in force. After
/// the swatches, the `#RRGGBB` field (`hex_editing` is the typed text while the
/// field is being edited); a click on it calls `on_hex`. The user's own colour
/// is used exactly: nothing here rounds it to a swatch.
pub fn swatch_row(
    id: &'static str,
    colours: &[u32],
    current: u32,
    hex_editing: Option<&str>,
    on_pick: impl Fn(u32, &mut Window, &mut App) + Clone + 'static,
    on_hex: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    let t = tokens();
    let current = current & 0xff_ffff;
    let custom = !colours.iter().any(|x| x & 0xff_ffff == current);
    let all: Vec<u32> = colours.iter().copied().chain(custom.then_some(current)).collect();
    let mut row = div().flex().flex_wrap().items_center().gap(px(6.));
    for (i, colour) in all.into_iter().enumerate() {
        let on = colour & 0xff_ffff == current;
        let on_pick = on_pick.clone();
        // The ring is the box's own border (always there, transparent when off), so
        // choosing never moves a swatch.
        row = row.child(
            div()
                .id((id, i))
                .flex_none()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if on { c(t.ink1) } else { hsla(0., 0., 0., 0.) })
                .cursor_pointer()
                .when(!on, |d| d.hover(|s| s.border_color(ca(t.line_hi))))
                .tooltip(crate::ui::widgets::overlay_tip_hover(
                    &if custom && i == colours.len() { format!("Custom {}", hex_text(colour)) } else { hex_text(colour) },
                    "",
                    None,
                ))
                .child(div().size(px(14.)).rounded_full().bg(c(colour & 0xff_ffff)).border_1().border_color(ca(t.line)))
                .on_click(move |_, window, cx| on_pick(colour & 0xff_ffff, window, cx)),
        );
    }
    let typing = hex_editing.is_some();
    row.child(
        div()
            .id((id, 99usize))
            .flex_none()
            .w(px(76.))
            .h(px(r::CONTROL_S))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(r::R_CONTROL))
            .border_1()
            .border_color(if typing { c(t.accent) } else { hsla(0., 0., 0., 0.) })
            .bg(c(t.e2))
            .cursor(CursorStyle::IBeam)
            .hover(move |s| s.bg(c(tokens().e3)))
            .type_style(type_scale::caption())
            .text_color(c(t.ink1))
            .child(match hex_editing {
                Some(text) => format!("{text}\u{258f}"),
                None => hex_text(current),
            })
            .on_click(move |_, window, cx| on_hex(window, cx)),
    )
}

/// The closed half of a card dropdown: an `e2` row control, the current value
/// on the left, a chevron on the right, 28px tall. It records its bounds under
/// `id` ([`crate::ui::anchor`]) so the list it opens hangs from it: on click
/// call `Player::picker_at_control(Pick::..)` for an enumerated setting, or
/// show a [`choice_list`] at [`crate::ui::anchor::hang`]. A blank value reads
/// `Custom` (the setting matches none of the listed choices), never an empty box.
pub fn dropdown_field(id: &'static str, value: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    let t = tokens();
    let value: SharedString = value.into();
    let value = if value.is_empty() { SharedString::from("Custom") } else { value };
    states(
        div()
            .id(id)
            .relative()
            .w_full()
            .flex_none()
            .h(px(r::CONTROL_M))
            .px(px(10.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .rounded(px(r::R_CONTROL))
            .border_1()
            .border_color(ca(t.line))
            .type_style(type_scale::body_strong())
            .text_color(c(if enabled { t.ink1 } else { t.ink4 }))
            .child(crate::ui::anchor::probe(id))
            .child(div().min_w(px(0.)).truncate().child(value))
            .child(Icon::ChevronDown.el(IconSize::Inline, if enabled { t.ink2 } else { t.ink4 })),
        Fills::solid(),
        enabled,
        None,
    )
}

/// The open half of a card dropdown: a [`plate`] of rows, the one in force
/// marked with a check (no focus ring: that appears only on keyboard
/// navigation). Position it absolutely at [`crate::ui::anchor::hang`] of the
/// field. `on_pick` gets the row's index.
pub fn choice_list(
    id: &'static str,
    w: f32,
    items: &[SharedString],
    current: usize,
    on_pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Stateful<Div> {
    choice_list_faced(id, w, items, &[], current, on_pick)
}

/// Moves the highlight of the open list `id` to row `ix` and brings that row into view: for a list
/// whose rows just changed (a search narrowed them).
pub fn set_list_highlight(id: &'static str, ix: usize) {
    OPEN_LIST.with(|l| {
        let mut l = l.borrow_mut();
        if l.id == id {
            l.hi = ix;
        }
    });
    list_scroll(id).scroll_to_item(ix);
}

/// [`choice_list`] whose row `i` is drawn in the font family `faces[i]` when it is `Some`, and in
/// the UI font when it is `None` or `faces` is short. Up, Down and Enter work it as they do any
/// list ([`list_nav`]). A list taller than [`LIST_MAX_H`] scrolls.
pub fn choice_list_faced(
    id: &'static str,
    w: f32,
    items: &[SharedString],
    faces: &[Option<SharedString>],
    current: usize,
    on_pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Stateful<Div> {
    let t = tokens();
    let scroll = list_scroll(id);
    let hi = OPEN_LIST.with(|l| {
        let mut l = l.borrow_mut();
        if l.id != id {
            l.id = id;
            l.hi = current;
            scroll.scroll_to_item(current);
        }
        l.len = items.len();
        l.pick = Some(Rc::new(on_pick.clone()));
        l.hi
    });
    let mut list = plate(6.)
        .id(id)
        .w(px(w))
        .p(px(crate::layout::MENU_PAD))
        .flex()
        .flex_col()
        .max_h(px(LIST_MAX_H))
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .occlude();
    for (i, label) in items.iter().enumerate() {
        let on_pick = on_pick.clone();
        let picked = i == current;
        list = list.child(
            div()
                .id((id, i))
                .flex_none()
                .h(px(crate::layout::MENU_ROW_H))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(ca(theme::WASH_HOVER)))
                .when(i == hi, |d| d.bg(ca(theme::WASH_HOVER)))
                .type_style(type_scale::body_strong())
                .text_color(c(t.ink1))
                .child(div().flex_none().size(px(16.)).children(picked.then(|| Icon::Check.el(IconSize::Inline, t.accent))))
                .child(
                    div()
                        .min_w(px(0.))
                        .whitespace_nowrap()
                        .when_some(faces.get(i).cloned().flatten(), |d, face| d.font_family(face))
                        .child(label.clone()),
                )
                .on_click(move |_, window, cx| on_pick(i, window, cx)),
        );
    }
    list
}

/// One row of a [`plate`] list: the [`choice_list`] row on its own, for a plate that holds
/// more than one list. A check marks the one in force.
pub fn choice_row(
    id: (&'static str, usize),
    label: impl Into<SharedString>,
    picked: bool,
    on_pick: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let t = tokens();
    div()
        .id(id)
        .flex_none()
        .h(px(crate::layout::MENU_ROW_H))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|s| s.bg(ca(theme::WASH_HOVER)))
        .type_style(type_scale::body_strong())
        .text_color(c(t.ink1))
        .child(div().flex_none().size(px(16.)).children(picked.then(|| Icon::Check.el(IconSize::Inline, t.accent))))
        .child(div().min_w(px(0.)).whitespace_nowrap().child(label.into()))
        .on_click(move |_, window, cx| on_pick(window, cx))
}

/// A ghost icon button with a tooltip, for row actions (move up, move down,
/// remove). While `why` is `Some` it is disabled (dimmed, not-allowed, no
/// handler) and the tooltip gives the reason instead of the name.
pub fn icon_ghost(
    id: &'static str,
    icon: Icon,
    tip: &str,
    why: Option<&str>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let b = Button::new(id)
        .icon(icon)
        .style(ButtonStyle::Ghost)
        .size(ButtonSize::Small)
        .enabled(why.is_none())
        .build()
        .tooltip(crate::ui::widgets::overlay_tip_hover(tip, "", why));
    match why {
        Some(_) => b,
        None => b.on_click(move |_, window, cx| on_click(window, cx)),
    }
}

/// A chord drawn as keycaps: `"Ctrl+Shift+R"` is three [`keycap`]s with a
/// small gap. A trailing `+` is the plus key (`"Ctrl++"`), and a bare `+` or
/// `-` is its own key. `" or "` separates alternatives (`"Ctrl+Y or Ctrl+Shift+Z"`),
/// each drawn as its own run with a plain "or" between. For sheets, the
/// palette's hint column and tooltips only (§5 rule 1).
pub fn keycaps(chord: &str) -> Div {
    let mut parts: Vec<AnyElement> = Vec::new();
    for (i, alt) in chord.split(" or ").enumerate() {
        if i > 0 {
            parts.push(div().type_style(type_scale::caption()).text_color(c(tokens().ink3)).child("or").into_any_element());
        }
        parts.extend(chord_keys(alt).into_iter().map(|k| keycap(k.to_string()).into_any_element()));
    }
    div().flex_none().flex().items_center().gap(px(3.)).children(parts)
}

/// The width [`keycaps`] takes for `chord`, from the same split: each cap's
/// text plus its padding and border (never narrower than the cap's minimum), a
/// 3px gap between caps, and the "or" word between alternatives. A tooltip sizes
/// its plate from this, so a long chord is not clipped.
pub fn keycaps_w(chord: &str) -> f32 {
    let cap = |k: &str| (crate::ui::popup::text_w(k, 12., false) + 12.).max(18.);
    let mut w = 0.;
    for (i, alt) in chord.split(" or ").enumerate() {
        if i > 0 {
            w += crate::ui::popup::text_w("or", 12., false) + 3.;
        }
        for (j, k) in chord_keys(alt).iter().enumerate() {
            w += cap(k) + if j > 0 { 3. } else { 0. };
        }
    }
    w
}

/// One alternative's keys: `"Ctrl+Shift+R"` is three, `"Ctrl++"` is "Ctrl" and
/// "+", a bare `"+"` or `"-"` is itself.
fn chord_keys(chord: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = chord;
    while !rest.is_empty() {
        if rest == "+" {
            parts.push("+");
            break;
        }
        match rest.find('+') {
            Some(0) => {
                parts.push("+");
                rest = &rest[1..];
            }
            Some(at) => {
                parts.push(&rest[..at]);
                rest = &rest[at + 1..];
            }
            None => {
                parts.push(rest);
                break;
            }
        }
    }
    parts
}

// ---------------------------------------------------------------------------
// The dialog shell: every modal sheet (New project, Save version, Versions,
// Collect media, About, unsaved changes, Relink, Shortcuts) is built from this.
// ---------------------------------------------------------------------------

/// The dialog layer: a subtle scrim over the window *below the menu bar* (the bar stays live
/// and unshaded) that centres its child in the picture region (`bottom_inset` is the height of
/// the timeline under it, which the dialog never covers). A press on the scrim is the caller's
/// to handle (`.on_mouse_down(..)`); the dialog itself stops it.
pub fn dialog_layer(bottom_inset: f32) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .top(px(crate::layout::MENU_BAR_H))
        .occlude()
        .pt(px(8.))
        .pb(px(8. + bottom_inset * 0.))
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(0., 0., 0., 0.55))
}

/// The dialog plate: [`plate`]`(8)`, `w` wide, 20px padding, children stacked
/// 12px apart, height hugging its content, and the close X at its top right
/// (it asks the window to cancel whatever dialog is up, the same as Esc).
/// Put `dialog_title`, a body and [`dialog_actions`] in it.
pub fn dialog(id: &'static str, w: f32) -> Stateful<Div> {
    let close = TAB_POOL.with(|p| {
        // The X is a pointer door; Esc is its key, so it takes no place in the Tab order.
        let held = p.borrow_mut().0.take();
        let b = Button::new("dialog-x")
            .icon(Icon::X)
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Small)
            .build()
            .tooltip(crate::ui::widgets::overlay_tip_hover("Close", "Esc", None))
            .on_click(|_, window, cx| {
                if let Some(Some(player)) = window.root::<Player>() {
                    player.update(cx, |player, cx| player.dialog_cancel(cx));
                }
            });
        p.borrow_mut().0 = held;
        b
    });
    plate(8.)
        .id(id)
        .relative()
        .flex_none()
        .w(px(w))
        .max_w(px(900.))
        .occlude()
        .p(px(20.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
        .child(div().absolute().top(px(10.)).right(px(10.)).child(close))
}

/// A dialog's title: one size for every dialog (`title`, `ink1`), clear of the close X.
pub fn dialog_title(text: impl Into<SharedString>) -> Div {
    div().pr(px(28.)).type_style(type_scale::title()).text_color(c(tokens().ink1)).child(text.into())
}

/// A dialog's body line: `body`, `ink2`, wrapping.
pub fn dialog_text(text: impl Into<SharedString>) -> Div {
    div().type_style(type_scale::body()).text_color(c(tokens().ink2)).child(text.into())
}

/// The button row at a dialog's foot: right-aligned, 8px between buttons.
pub fn dialog_actions() -> Div {
    div().flex_none().flex().items_center().justify_end().gap(px(8.)).pt(px(4.))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::text_field::FieldKey;

    #[test]
    fn typing_a_number_replaces_the_old_text_and_commits_clamped() {
        let mut e = NumberEdit::begin("1.00");
        assert_eq!(e.key("2", false, false, Some("2")), FieldKey::Edited);
        e.key(",", false, false, Some(","));
        e.key("5", false, false, Some("5"));
        // A letter is ignored.
        e.key("x", false, false, Some("x"));
        assert_eq!(e.text(), "2,5");
        assert_eq!(e.commit(0., 2.), Some(2.));
        assert_eq!(e.key("enter", false, false, None), FieldKey::Commit);
        assert_eq!(e.key("escape", false, false, None), FieldKey::Cancel);
        assert_eq!(NumberEdit::begin("").commit(0., 1.), None);
    }

    #[test]
    fn scrubbing_moves_per_pixel_fine_with_shift_and_stays_in_range() {
        assert_eq!(scrub_value(1., 10., 0.01, false, 0., 2.), 1.1);
        assert!((scrub_value(1., 10., 0.01, true, 0., 2.) - 1.01).abs() < 1e-9);
        assert_eq!(scrub_value(1., 1000., 0.01, false, 0., 2.), 2.);
    }

    #[test]
    fn hex_round_trips_and_short_form_expands() {
        assert_eq!(hex_text(0x00ff7a), "#00FF7A");
        assert_eq!(parse_hex("#00ff7a"), Some(0x00ff7a));
        assert_eq!(parse_hex("f80"), Some(0xff8800));
        assert_eq!(parse_hex("#12"), None);
    }
}
