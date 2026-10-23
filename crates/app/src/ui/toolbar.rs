//! The edit toolbar (REDESIGN §3.4, DESIGN.md §6.4): the row of editing commands
//! that sits at the top of the bench, directly above the lanes. `stance.rs::bench()`
//! mounts it and spends its height out of the bench's own box, so nothing is
//! reserved twice: the seam drag, the floor and the lanes all read the one
//! [`TOOLBAR_H`].
//!
//! Groups, left to right, separated by 1px rules: split, delete and lift; undo
//! and redo; snap and the two marks. Then, pushed right, zoom out, a zoom
//! slider, zoom in and Fit. Export lives on the menu bar now.
//!
//! Every button is an icon and a word (Split, Delete, Lift, Undo, Redo, Snap,
//! In, Out, Fit) -- never a key: the key is in the tooltip, read from the
//! keymap. Below [`COMPACT_BELOW`] the second and third groups drop their words
//! and keep their icons, so nothing is ever clipped. A button the oracle
//! refuses is drawn dim with the reason as its tooltip; a click still goes
//! through [`Player::act`], which says the same reason as a notice.

use crate::layout::{SEQ_BAR_H, SEQ_TAB_H, TOOLBAR_BTN_H, TOOLBAR_H, TOOLBAR_ROW_H};
use crate::ui::hitmap;
use crate::ui::icons::Icon;
use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
use crate::ui::timeband_stance;
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::{action_hover, tip_hover};
use crate::*;

/// The zoom slider's track, end to end, in px.
const ZOOM_TRACK_W: f32 = 120.;
/// The slider's thumb, square.
const ZOOM_THUMB: f32 = 12.;

/// Below this column width the second and third groups (undo, redo, snap, the
/// marks) keep only their icons (the word stays in the tooltip), so the zoom
/// group always fits. Measured at 1280x720 (column 958 px): all words need
/// about 1010 px, the compact row about 730.
const COMPACT_BELOW: f32 = 900.;

/// Below this the zoom slider's track is [`ZOOM_TRACK_TIGHT_W`] wide.
const TIGHT_BELOW: f32 = 1100.;
const ZOOM_TRACK_TIGHT_W: f32 = 60.;

/// Whether the column is narrow enough that buttons shed their key.
pub(crate) fn compact() -> bool {
    let w = timeband_stance::column_w();
    w > 0. && w < COMPACT_BELOW
}

/// The zoom track's width at the column's current width.
/// One recorded box per tab (the anchor registry is keyed by static names), so a tab's menu can
/// hang from the tab that was pressed.
const SEQ_TAB_ANCHORS: [&str; 8] = ["seq-tab-0", "seq-tab-1", "seq-tab-2", "seq-tab-3", "seq-tab-4", "seq-tab-5", "seq-tab-6", "seq-tab-7"];

fn zoom_track_w() -> f32 {
    let w = timeband_stance::column_w();
    if w > 0. && w < TIGHT_BELOW { ZOOM_TRACK_TIGHT_W } else { ZOOM_TRACK_W }
}

/// The drag payload that carries a zoom-slider gesture. Only its type matters:
/// `on_drag_move::<ZoomDrag>` is how the slider hears the pointer after the
/// press, with no state kept on [`Player`] for it.
#[derive(Clone)]
struct ZoomDrag;

/// The toolbar region: the button row over the sequence strip, full width of the centre column.
pub(crate) fn render(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-toolbar-region")
        .flex_none()
        .w_full()
        .h(px(TOOLBAR_H))
        .flex()
        .flex_col()
        .child(row(player, cx))
        .child(sequence_strip(player, cx))
}

/// The sequence strip (REDESIGN §3.5): one tab per sequence, the open one marked
/// with a 2px accent underline, a plus for a new one, and the way back out of a
/// compound clip with the path in. A fixed height whatever it holds, drawn with
/// one sequence too, so nothing moves when a second appears.
fn sequence_strip(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let tabs = player.sequence_tabs();
    let name_of = |id: u32| tabs.iter().find(|q| q.id == id).map(|q| q.name.clone());
    let back = player.enable(ActionId::CloseCompound, None).yes();
    let current = tabs.iter().find(|q| q.active).map(|q| q.name.clone()).unwrap_or_default();
    let path: Vec<String> = player
        .seq_trail
        .iter()
        .filter_map(|&id| name_of(id))
        .chain(std::iter::once(current))
        .collect();
    let typing = player.seq_edit.clone();
    div()
        .id("stance-sequences")
        .flex_none()
        .w_full()
        .h(px(SEQ_BAR_H))
        .flex()
        .items_center()
        .gap(px(2.))
        .px(px(8.))
        .bg(rgb(t.e1))
        .border_t_1()
        .border_color(rgba(t.line))
        // The way back out of a compound: a labelled "Back" control, drawn only inside one, with
        // the sequence it returns to named in its tooltip. Its slot is always there, so the strip
        // never shifts when one opens.
        .child(
            div()
                .flex_none()
                .w(px(78.))
                .h(px(24.))
                .mr(px(4.))
                .when(back, |d| {
                    d.child(
                        Button::new("seq-back")
                            .style(ButtonStyle::Ghost)
                            .size(ButtonSize::Small)
                            .icon(Icon::ChevronLeft)
                            .label("Back")
                            .build()
                            .tooltip(action_hover(player, ActionId::CloseCompound))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.act(ActionId::CloseCompound, window, cx);
                            }))
                            .children(hitmap::action(ActionId::CloseCompound, true)),
                    )
                }),
        )
        .child(
            div()
                .id("seq-tabs")
                .flex_shrink()
                .min_w(px(0.))
                .h_full()
                .flex()
                .items_center()
                .gap(px(2.))
                .overflow_hidden()
                .children(tabs.iter().enumerate().map(|(ti, q)| {
                    let id = q.id;
                    let active = q.active;
                    // The tab being renamed is a text field in the tab's own place: a plate with
                    // the accent edge, the name selected (all of it) or with the caret after it.
                    let editing = typing.clone().filter(|_| active);
                    let name = q.name.clone();
                    // The 2px underline is always there (transparent when inactive), so
                    // a tab switch moves nothing.
                    div()
                        .id(("seq-tab", id as usize))
                        .relative()
                        .flex_none()
                        .flex()
                        .items_center()
                        .max_w(px(180.))
                        .h(px(SEQ_TAB_H.max(28.)))
                        .px(px(10.))
                        .border_b_2()
                        .border_color(if active { rgb(t.accent) } else { rgba(0) })
                        .cursor_pointer()
                        .when(!active, |d| d.hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER))))
                        .type_style(type_scale::body_strong())
                        .text_color(rgb(if active { t.ink1 } else { t.ink2 }))
                        .tooltip(tip_hover(
                            "Sequence",
                            "",
                            Some(if active { "Double-click to rename" } else { "Click to open" }),
                        ))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.open_seq_tab_menu(id, event.position, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            if event.click_count() >= 2 && this.seq_edit.is_none() {
                                this.switch_sequence_tab(id, cx);
                                this.begin_seq_edit(cx);
                            } else {
                                this.switch_sequence_tab(id, cx);
                            }
                        }))
                        .children(hitmap::dynamic(
                            move || (format!("seq-tab.{id}"), format!("Sequence {name}")),
                            true,
                        ))
                        .child(crate::ui::anchor::probe(SEQ_TAB_ANCHORS[ti.min(SEQ_TAB_ANCHORS.len() - 1)]))
                        .child(match editing {
                            Some(field) => seq_name_field(&field).into_any_element(),
                            None => div().min_w(px(0.)).truncate().child(q.name.clone()).into_any_element(),
                        })
                })),
        )
        .child(
            Button::new("seq-new")
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Small)
                .icon(Icon::Plus)
                .build()
                .tooltip(action_hover(player, ActionId::NewSequence))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.act(ActionId::NewSequence, window, cx);
                }))
                .children(hitmap::action(ActionId::NewSequence, true)),
        )
        .child(div().flex_1().min_w(px(0.)))
        .when(path.len() > 1, |d| {
            d.child(
                div()
                    .flex_none()
                    .pl(px(10.))
                    .type_style(type_scale::caption())
                    .text_color(rgb(t.ink3))
                    .child(path.join(" › ")),
            )
        })
}

/// A name while it is being typed: a text field in the name's own place, with the accent edge the
/// other fields wear, the name selected when it is all selected (a rename that has not been typed
/// into yet) and the caret after it otherwise. Shared by the sequence tabs and the markers card.
pub(crate) fn seq_name_field(field: &crate::player::text_field::TextField) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let text = match field.all {
        true => div()
            .rounded(px(2.))
            .bg(rgba(crate::ui::theme::with_alpha(t.accent, 0x66)))
            .child(field.text.clone())
            .into_any_element(),
        false => format!("{}\u{258f}", field.text).into_any_element(),
    };
    div()
        .flex_none()
        .min_w(px(96.))
        .max_w(px(168.))
        .h(px(24.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(t.accent))
        .bg(rgb(t.e2))
        .type_style(type_scale::body())
        .text_color(rgb(t.ink1))
        .child(div().min_w(px(0.)).truncate().child(text))
}

/// The toolbar's button row.
fn row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let words = !compact();
    div()
        .id("stance-toolbar")
        .flex_none()
        .w_full()
        .h(px(TOOLBAR_ROW_H))
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .bg(rgb(t.e1))
        // Group one: split, delete, lift. Their words stay at every width.
        .child(button("tb-split", player, ActionId::Cut, Icon::Scissors, Some("Split"), false, cx))
        .child(button("tb-delete", player, ActionId::Delete, Icon::Trash2, Some("Delete"), false, cx))
        .child(button("tb-lift", player, ActionId::Lift, Icon::Eraser, Some("Lift"), false, cx))
        .child(rule())
        // Group two: undo, redo.
        .child(button("tb-undo", player, ActionId::Undo, Icon::Undo2, words.then_some("Undo"), false, cx))
        .child(button("tb-redo", player, ActionId::Redo, Icon::Redo2, words.then_some("Redo"), false, cx))
        .child(rule())
        // Group three: snap, whose state is the tint and fill (the word never changes), and the marks.
        .child(button("tb-snap", player, ActionId::ToggleSnap, Icon::Magnet, words.then_some("Snap"), player.snap, cx))
        .child(button("tb-mark-in", player, ActionId::SetIn, Icon::SquareArrowDownRight, words.then_some("In"), false, cx))
        .child(button("tb-mark-out", player, ActionId::SetOut, Icon::SquareArrowDownLeft, words.then_some("Out"), false, cx))
        // Pushed right: the zoom group and Fit.
        .child(div().flex_1().min_w(px(0.)))
        .child(button("tb-zoom-out", player, ActionId::ZoomOut, Icon::ZoomOut, None, false, cx))
        .child(zoom_slider(player, cx))
        .child(button("tb-zoom-in", player, ActionId::ZoomIn, Icon::ZoomIn, None, false, cx))
        .child(button("tb-fit", player, ActionId::ZoomFit, Icon::Scan, Some("Fit"), false, cx))
}

/// A 1px rule between two groups, 16px tall with 8px of margin.
fn rule() -> impl IntoElement {
    div()
        .flex_none()
        .w(px(1.))
        .h(px(16.))
        .mx(px(4.))
        .bg(rgba(crate::ui::theme::tokens().line))
}

/// One command: a ghost button with its icon and (at wide widths) its word, never
/// its key. `on` is the toggle state (Snap): accent icon on an `e3` fill. Always
/// dispatches through [`Player::act`] so a refused press still says why; draws
/// dim when the oracle refuses it.
fn button(
    id: &'static str,
    player: &Player,
    action: ActionId,
    icon: Icon,
    word: Option<&'static str>,
    on: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let yes = player.enable(action, None).yes();
    let icon_only = word.is_none();
    let mut b = Button::new(id)
        .style(ButtonStyle::Ghost)
        .size(if icon_only && matches!(action, ActionId::ZoomIn | ActionId::ZoomOut) {
            ButtonSize::Small
        } else {
            ButtonSize::Large
        })
        .icon(icon)
        .on(on)
        .enabled(yes);
    if let Some(word) = word {
        b = b.label(word);
    }
    b.build()
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.act(action, window, cx);
        }))
        .tooltip(toolbar_tip(player, action))
        .children(hitmap::action(action, yes))
}

/// The hover line a toolbar button wears. Delete and Lift say what they leave behind in the
/// toolbar's own words; every other button has its action's name, as ever.
fn toolbar_tip(player: &Player, action: ActionId) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + use<> {
    let name = match action {
        ActionId::Delete => "Remove and close the gap".to_string(),
        ActionId::Lift => "Remove and leave a gap".to_string(),
        _ => player.action_name(action),
    };
    tip_hover(&name, &player.keymap.display(action), player.enable(action, None).why())
}

/// Where a share of the slider, 0 at the far-out stop and 1 at the far-in one,
/// sits in pixels per second. The scale is logarithmic because a zoom is a
/// multiplier: each equal step of the thumb is the same factor of magnification.
fn zoom_at(min: f64, max: f64, share: f64) -> f64 {
    min * (max / min).powf(share.clamp(0., 1.))
}

/// The inverse of [`zoom_at`]: which share of the slider a given zoom sits at.
fn share_of(min: f64, max: f64, pps: f64) -> f64 {
    if max <= min {
        return 0.;
    }
    ((pps / min).ln() / (max / min).ln()).clamp(0., 1.)
}

/// Zooms the timeline so that the slider reads `share`. Goes through
/// [`Player::zoom`], the one zoom door, so the stops clamp it exactly as they do
/// a key press, and the playhead stays where it was.
fn zoom_to_share(player: &mut Player, share: f32, cx: &mut Context<Player>) {
    let view = player.view();
    let Some((min, max)) = view.stops() else {
        return;
    };
    let target = zoom_at(min, max, f64::from(share));
    let factor = target / view.scale.pps;
    player.zoom(factor as f32, None, cx);
}

/// The zoom slider: a thumb on a hairline track, showing the timeline's current
/// zoom on every render, and dragged to set it. A bed that has never been
/// painted has no stops yet, so the slider is drawn dim and takes no drag.
fn zoom_slider(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let view = player.view();
    let stops = view.stops();
    let share = match stops {
        Some((min, max)) => share_of(min, max, view.settled().pps) as f32,
        None => 0.,
    };
    let thumb_x = share * (zoom_track_w() - ZOOM_THUMB);
    let mut track = div()
        .id("tb-zoom-slider")
        .relative()
        .flex_none()
        .w(px(zoom_track_w()))
        .h(px(TOOLBAR_BTN_H))
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .top(px((TOOLBAR_BTN_H - 3.) / 2.))
                .h(px(3.))
                .rounded(px(2.))
                .bg(rgb(crate::ui::theme::tokens().e3)),
        )
        .child(
            div()
                .absolute()
                .left(px(thumb_x))
                .top(px((TOOLBAR_BTN_H - ZOOM_THUMB) / 2.))
                .size(px(ZOOM_THUMB))
                .rounded(px(ZOOM_THUMB / 2.))
                .bg(rgb(if stops.is_some() { crate::ui::theme::tokens().ink1 } else { crate::ui::theme::tokens().ink4 })),
        );
    if stops.is_some() {
        track = track
            .cursor_pointer()
            .tooltip(tip_hover("Zoom", "", Some("Drag to zoom the timeline")))
            .on_drag(ZoomDrag, |_, _, _, cx| cx.new(|_| gpui::Empty))
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<ZoomDrag>, _, cx| {
                    // The thumb's centre follows the pointer, so the half
                    // thumb at each end is taken off before the share is
                    // worked out.
                    let at = f32::from(event.event.position.x)
                        - f32::from(event.bounds.left())
                        - ZOOM_THUMB / 2.;
                    zoom_to_share(this, at / (zoom_track_w() - ZOOM_THUMB), cx);
                }),
            );
    }
    track
}
