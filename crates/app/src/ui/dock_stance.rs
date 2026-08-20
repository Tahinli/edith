//! The dock's own content: the Sources/Clip tab pair and what each shows
//! (DESIGN.md §5, §12 step 4). `stance.rs::dock()` owns the panel's frame
//! (width, surfaces, border); this module owns what fills it.

use crate::*;

/// corner-cut: which dock tab is showing lives in a module-level flag rather
/// than a `Player` field, because the stance's other regions (spine, screen,
/// time band) are being threaded onto `Player` by a second builder in this
/// same pass and a shared struct is not a safe place for two agents to land
/// fields at once. Ceiling: fold into a `Player::dock_tab` field once that
/// pass lands, so DESIGN §5's "a room reopens exactly as left ... dock tab"
/// continuity (session save/load) can reach it -- today the flag resets with
/// the process.
static DOCK_SRC_ACTIVE: AtomicBool = AtomicBool::new(true);

/// A ghost verb (DESIGN §4): borderless glyph/label in `ink2`, its chord in
/// `ink3` beside it, read live off the keymap so it can never drift from the
/// key that does the same thing. Hover is one fill step and an ink brighten;
/// held open (`active`) keeps both. A refused verb dims and says why on
/// hover instead of disappearing (§8).
fn ghost_verb(
    id: &'static str,
    label: &'static str,
    action: ActionId,
    active: bool,
    hint: &str,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let enabled = player.enable(action, None);
    let key = player.keymap.display(action);
    let say: SharedString = match enabled.why() {
        Some(why) => format!("{key} — {why}"),
        None => format!("{key} — {hint}"),
    }
    .into();
    let on = enabled.yes();
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .h(px(CONTROL_H))
        .px(px(8.))
        .rounded(px(3.))
        .when(active, |d| d.bg(rgb(DARK_RAISED())))
        .text_color(rgb(if active { INK1() } else { INK2() }))
        .tooltip(move |_, cx| cx.new(|_| Tip(say.clone())).into())
        .when(!on, |d| d.opacity(0.4).cursor_not_allowed())
        .when(on, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                .on_click(on_click)
        })
        .child(label)
        .child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(rgb(if active { INK1() } else { INK3() }))
                .child(key),
        )
}

/// One tab glyph of the Src/Clip pair: a plate (2px radius per §4), `ink1` +
/// `raised` fill when it is the showing tab, `ink2` at rest.
fn dock_tab(id: &'static str, label: &'static str, active: bool, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(2.))
        .py(px(6.))
        .text_size(px(10.5))
        .text_color(rgb(if active { INK1() } else { INK2() }))
        .when(active, |d| d.bg(rgb(DARK_RAISED())))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_RAISED())))
        .on_click(cx.listener(move |_this, _: &ClickEvent, _, cx| {
            DOCK_SRC_ACTIVE.store(label == "Src", Ordering::Relaxed);
            cx.notify();
        }))
        .child(label)
}

/// The Sources tab: [`Player::library`] verbatim -- filter tabs, usage
/// (proxy/preview) chips, Import, drag, Add at playhead -- restyled for free,
/// because its rows already paint through the theme tokens `library.rs`
/// shares with this stance (`BG_PANEL` etc. alias to the Darkroom substrate
/// under `PaletteId::Darkroom`). No ink/re-inking control lives in it (DESIGN
/// §2's demotion rule): `library.rs` never put one on a row to begin with, so
/// there is nothing here to strip.
fn sources_tab(player: &Player, width: f32, cx: &mut Context<Player>) -> impl IntoElement {
    player.library(width, cx)
}

/// The Clip tab: the four verbs DESIGN §5 names, as ghosts, over whichever
/// param-row card they open -- [`Player::eq_card`], [`Player::color_card`],
/// [`Player::transform_card`], [`Player::speed_card`] verbatim, the same
/// param-row rendering `inspector.rs`'s selection section already opens
/// these onto. Drag-while-playing and every other gesture on a row is
/// whatever that card already does; nothing about the gesture is reimplemented
/// here.
fn clip_tab(player: &Player, width: f32, window_h: Pixels, cx: &mut Context<Player>) -> impl IntoElement {
    let room = size(px(width), window_h);
    let none_open = player.eq_open.is_none()
        && player.color_open.is_none()
        && player.transform_open.is_none()
        && player.speed_open.is_none();
    div()
        .id("dock-clip")
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .child(
            div()
                .id("dock-clip-verbs")
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .p(px(8.))
                .child(ghost_verb(
                    "dock-verb-speed",
                    "Speed",
                    ActionId::Speed,
                    player.speed_open.is_some(),
                    "how fast this clip and its group play",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_speed(cx)),
                ))
                .child(ghost_verb(
                    "dock-verb-color",
                    "Colour",
                    ActionId::Color,
                    player.color_open.is_some(),
                    "exposure, contrast, saturation and temperature",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_color(cx)),
                ))
                .child(ghost_verb(
                    "dock-verb-transform",
                    "Transform",
                    ActionId::Transform,
                    player.transform_open.is_some(),
                    "position, scale, rotation and crop for this clip",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_transform(cx)),
                ))
                .child(ghost_verb(
                    "dock-verb-eq",
                    "EQ",
                    ActionId::Equalizer,
                    player.eq_open.is_some(),
                    "the bands this clip's sound is filtered through",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_eq(cx)),
                )),
        )
        .child(
            div()
                .id("dock-clip-rows")
                .flex_1()
                .min_h(px(0.))
                .px(px(8.))
                .pb(px(8.))
                .children(player.eq_card(room, cx))
                .children(player.color_card(cx))
                .children(player.transform_card(cx))
                .children(player.speed_card(cx))
                // A plate, not bare space: DESIGN §11's "states" checklist --
                // nothing picked reads as a hint, not as a hole in the panel.
                .when(none_open, |d| {
                    d.child(
                        div()
                            .rounded(px(2.))
                            .bg(rgb(DARK_CANVAS()))
                            .p(px(8.))
                            .text_size(px(10.5))
                            .text_color(rgb(INK3()))
                            .child("pick a verb above"),
                    )
                }),
        )
}

/// The dock's content, under `stance.rs::dock()`'s tab-bar-and-body frame:
/// the tab row, then whichever tab is showing.
///
/// Degradation (DESIGN §7): the dock is a fixed-width side panel, not a lane
/// bed, so it has no width ladder of its own to walk -- the panel is either
/// on screen at its one width or, at the narrowest floors this editor draws
/// to, is the first region asked to give up its width entirely (a step
/// `layout.rs`'s split budget already owns for the legacy inspector/library
/// pair). What degrades *inside* fixed width is the two tabs' own content:
/// Sources hands off to `library.rs`'s own row anatomy and ladder; Clip's
/// param rows are whatever their card already draws at this width.
pub(crate) fn render(player: &Player, width: f32, window_h: Pixels, cx: &mut Context<Player>) -> impl IntoElement {
    let src_active = DOCK_SRC_ACTIVE.load(Ordering::Relaxed);
    div()
        .id("stance-dock-body")
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .child(
            div()
                .id("stance-dock-tabs")
                .flex_none()
                .flex()
                .gap(px(4.))
                .p(px(8.))
                .border_b_1()
                .border_color(rgb(DARK_HAIRLINE()))
                .child(dock_tab("dock-tab-src", "Src", src_active, cx))
                .child(dock_tab("dock-tab-clip", "Clip", !src_active, cx)),
        )
        .child(match src_active {
            true => sources_tab(player, width, cx).into_any_element(),
            false => clip_tab(player, width, window_h, cx).into_any_element(),
        })
}
