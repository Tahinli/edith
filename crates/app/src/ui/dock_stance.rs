//! The dock's own content: the Sources/Clip tab pair and what each shows
//! (DESIGN.md §5, §12 step 4). `stance.rs::dock()` owns the panel's frame
//! (width, surfaces, border); this module owns what fills it.
//!
//! The Sources tab used to be `Player::library` verbatim -- the legacy
//! panel's pill tabs, Media/Audio/Text row, filled Import button and amber
//! accent, wearing the darkroom surfaces only because its tokens happened to
//! alias into them. The user named that: "this place is not fitting design
//! language. it's same as our original." This module now builds the tab
//! fresh, off MOCK-SPEC.md's "Dock" section -- `library.rs`/`library_meta.rs`
//! still supply the row facts ([`library_rows`], [`source_tint`],
//! [`clip_middle`]...), only the anatomy around them is new.

use crate::ui::hitmap;
use crate::ui::type_scale::{self, Typeset, head, label, mono};
use crate::*;
use gpui::FontWeight;

/// Where the dock tab pick lives: one word beside the theme and the
/// keybindings (`ui::theme::config_path`/`save`/`load` is the exact pattern
/// this follows -- a small, silent, config-file round trip is the mechanism
/// this editor already uses for a preference that outlives the window, and
/// the playhead's own continuity lives in the *project* file, which a dock
/// tab pick is not: it is not part of the timeline, so it does not belong in
/// a `.edith`).
pub(crate) fn config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("dock-tab")
}

/// The pick from the last session, if there was one. Anything unreadable or
/// unknown leaves the default (`Src`) in force, exactly as a bad theme file
/// does -- neither is the user's work, so neither is worth a startup notice.
pub(crate) fn load() -> bool {
    std::fs::read_to_string(config_path())
        .map(|text| text.trim() != "Clip")
        .unwrap_or(true)
}

/// Writes the pick. One word, written whole.
fn save(src_active: bool) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if src_active { "Src\n" } else { "Clip\n" });
}

/// Whether the room's one open param card is maximized -- the same
/// small-file round trip as the dock tab pick above, its own word beside it
/// ("a room reopens exactly as left", DESIGN.md:135), so a maximized EQ stays
/// maximized across a close and reopen of the whole room, not just the card.
pub(crate) fn maximized_config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("card-maximized")
}

pub(crate) fn load_maximized() -> bool {
    std::fs::read_to_string(maximized_config_path())
        .map(|text| text.trim() == "1")
        .unwrap_or(false)
}

pub(crate) fn save_maximized(maximized: bool) {
    let path = maximized_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if maximized { "1\n" } else { "0\n" });
}

/// The Sources tab's four sort chips (MOCK-SPEC "Dock" §3). `Recent` is the
/// library's own arrival order -- the only order this editor tracks without
/// a fourth field to keep it in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DockSort {
    #[default]
    Recent,
    Name,
    Usage,
    Unused,
}

impl DockSort {
    const ALL: [DockSort; 4] = [
        DockSort::Recent,
        DockSort::Name,
        DockSort::Usage,
        DockSort::Unused,
    ];
    fn label(self) -> &'static str {
        match self {
            DockSort::Recent => "Recent",
            DockSort::Name => "Name",
            DockSort::Usage => "Usage",
            DockSort::Unused => "Unused",
        }
    }
}

/// A ghost verb (DESIGN §4): borderless glyph/label in `ink2`, its chord in
/// `ink3` beside it, read live off the keymap so it can never drift from the
/// key that does the same thing. Hover is one fill step and an ink brighten;
/// held open (`active`) keeps both. A refused verb dims and says why on
/// hover instead of disappearing (§8).
fn ghost_verb(
    id: &'static str,
    verb_label: &'static str,
    action: ActionId,
    active: bool,
    hint: &str,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let enabled = player.enable(action, None);
    let key = player.keymap.chord(action);
    let say: SharedString = match enabled.why() {
        Some(why) => format!("{key} — {why}"),
        None => format!("{key} — {hint}"),
    }
    .into();
    let on = enabled.yes();
    let label_style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    let chord_style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
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
        .tooltip(move |_, cx| cx.new(|_| Tip(say.clone())).into())
        .when(!on, |d| d.opacity(0.4).cursor_not_allowed())
        .when(on, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                .on_click(on_click)
        })
        .children(hitmap::action(action, on))
        .child(
            div()
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(if active { INK1() } else { INK2() }))
                .child(verb_label),
        )
        .child(
            div()
                .flex_none()
                .font(chord_style.font)
                .text_size(chord_style.size)
                .text_color(rgb(if active { INK1() } else { INK3() }))
                .child(key),
        )
}

/// One tab of the Src/Clip pair (MOCK-SPEC "Dock": no pill, no fill, no
/// rounded button -- a 1px `ink1` top rule and `ink1` text mark the showing
/// tab; the resting one is `ink3`). The mock's own trailing letters (`L`,
/// `I`) are dropped rather than bound: both bare keys are already this
/// editor's own busiest edit strokes (`l` lifts a clip, `i` marks in), and
/// DESIGN §11's frequency check puts a lift and a mark far ahead of a tab
/// switch -- wearing a chord this room does not answer to would be exactly
/// the lie DESIGN §4 forbids, so the letters go instead.
fn dock_tab(
    id: &'static str,
    label_text: &'static str,
    active: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .pt(px(6.))
        .pb(px(6.))
        .when(active, |d| d.border_t_1().border_color(rgb(INK1())))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.dock_src_active = label_text == "SOURCES";
            save(this.dock_src_active);
            cx.notify();
        }))
        .children(hitmap::control(id, label_text, true))
        .child(
            div()
                .font(style.font)
                .text_size(style.size)
                .text_color(rgb(if active { INK1() } else { INK3() }))
                .child(label_text),
        )
}

/// A 12px uppercase Archivo section head, `ink3` (DESIGN §3).
fn section_head(text: impl Into<SharedString>) -> impl IntoElement {
    let style = head();
    div()
        .flex_none()
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(INK3()))
        .child(text.into())
}

/// Every lane a source plays on right now, as its own `V1`/`A1` labels
/// (MOCK-SPEC "Dock" §4's usage line), plus how many clips altogether --
/// [`Player::row_ctx`]'s `placed` count, the same number the library card's
/// Remove refusal reads.
fn usage_line(player: &Player, source_idx: usize, placed: usize) -> String {
    if placed == 0 {
        return "unused".to_string();
    }
    let lanes: Vec<String> = player.session.as_ref().map_or_else(Vec::new, |session| {
        session
            .lanes()
            .into_iter()
            .filter(|lane| {
                session
                    .lane_clips(*lane)
                    .iter()
                    .any(|c| c.source == source_idx)
            })
            .map(Lane::label)
            .collect()
    });
    format!(
        "{} · {placed} use{}",
        lanes.join(" "),
        if placed == 1 { "" } else { "s" }
    )
}

/// One source row, two lines (MOCK-SPEC "Dock" §4): an ink dot, the name in
/// mono `ink1` -- readable, complete, ellipsized at the end rather than
/// clipped mid-glyph or faded -- and right-aligned usage; under it, the
/// codec/length/decoder line in mono `ink3`. Drag (gesture 1), `↵` (gesture
/// 2, wired in `render.rs`'s key handler since a row takes no keyboard focus
/// of its own), double-click (gesture 3) and right-click the dot (gesture 4)
/// all live here.
fn source_row(
    player: &Player,
    i: usize,
    row: &Row,
    placed: usize,
    picked: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let usable = row.unusable.is_none();
    let name: SharedString = row.name.clone().into();
    let under: String = match &row.unusable {
        Some(why) => why.clone(),
        None => join_detail(
            &row.detail,
            &timecode(f64::from(row.frames) / player.fps, player.fps),
        ),
    };
    let usage = usage_line(player, row.tint, placed);
    let (path, stream) = (row.path.clone(), row.stream);
    let dragged = (path.clone(), stream);
    let dot_path = path.clone();
    let menu_path = path.clone();
    let add_path = path.clone();
    let proxy_path = path.clone();
    let ghost = name.clone();
    let can_insert = usable && player.session.is_some() && player.exporting().is_none();
    let (proxy_glyph, proxy_on, proxy_progress, proxy_waiting, proxy_tip) =
        match player.proxies.get(&row.path) {
            Some(Proxy::Ready) => (
                "●",
                true,
                None,
                false,
                format!("Stand-in ON for {} — click to delete it", row.name),
            ),
            Some(Proxy::Making(job)) => (
                "■",
                true,
                Some(job.progress()),
                false,
                format!("Stop making the stand-in for {}", row.name),
            ),
            Some(Proxy::Cancelling(_)) => (
                "■",
                true,
                None,
                true,
                format!("Stopping the stand-in for {}…", row.name),
            ),
            Some(Proxy::Asked) => (
                "○",
                false,
                None,
                true,
                format!("Preparing the stand-in for {}…", row.name),
            ),
            _ => (
                "○",
                false,
                None,
                false,
                format!("Stand-in OFF for {} — click to make one", row.name),
            ),
        };
    let proxy_stops = proxy_glyph == "■";
    let proxy_tip: SharedString = proxy_tip.into();
    div()
        .id(("dock-source", i))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(3.))
        .when(!usable, |d| d.opacity(0.5))
        // FAULT 3: this used to be `bg(DARK_RAISED())`, pixel-identical to
        // the row's own `hover` fill below -- a picked row and a merely
        // hovered one painted the same, so the editor could not see what was
        // selected. DESIGN §4's ring (1px `ink1`) is the one that fits here,
        // not §2's complement-leaning ink: §2's rule is about a mark drawn
        // *over a source's own extracted film ink* (the bench clip's ring,
        // already `ink1` in `bench_stance.rs`, sits on a source-tinted trace
        // it must stay legible against regardless of hue) -- a dock row
        // carries no film ink of its own to complement, it is a plain list
        // line, so the general focus/selection ring applies, the same one
        // the bench already uses for the same job.
        .when(picked, |d| d.border_1().border_color(rgb(INK1())))
        // Right-click ANYWHERE on the row opens the row's menu -- Add,
        // Remove, Reveal, Properties (DESIGN §9: "verbs of the thing under
        // the cursor"). It used to hang off the 8px ink dot alone, and only
        // for a usable row, which is how the room shipped with no reachable
        // way to take a source out of the library at all (user 2026-08-21:
        // "can not remove a media from library") -- and the one row that
        // most wants removing, a file the editor cannot use, was the one
        // row whose menu was gated off. Unconditional now, dot included.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.selected_asset = Some((menu_path.clone(), stream));
                this.library_menu = Some(LibraryMenu {
                    path: menu_path.clone(),
                    stream,
                    at: event.position,
                    details: false,
                });
                cx.notify();
            }),
        )
        .when(usable, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.selected_asset = Some((path.clone(), stream));
                    // Gesture 3: a second click in the same spot plays the
                    // source in the screen, the way `library.rs`'s preview
                    // triangle already does -- `open_preview` is the one
                    // door either takes.
                    if event.click_count() >= 2 {
                        this.open_preview(&path, stream, cx);
                    }
                    cx.notify();
                }))
                // Gesture 1: the row is a drag source exactly as
                // `library.rs`'s row is -- same payload, same ghost tip --
                // and the bench's `AssetDrag` drop target already accepts it
                // (`ui/bench_stance.rs`'s bed). The bug the user hit ("can't
                // drag media in timeline") was the dock showing the *legacy*
                // panel, which the darkroom bench was never wired against;
                // this row is the darkroom's own half of that pairing.
                .on_drag(AssetDrag(dragged.0, dragged.1), move |_, _, _, cx| {
                    cx.new(|_| Tip(ghost.clone()))
                })
        })
        .children(hitmap::dynamic(
            move || (format!("source.{i}.row"), "Source row".into()),
            usable,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id(("dock-source-dot", i))
                        .flex_none()
                        .w(px(8.))
                        .h(px(8.))
                        .rounded(px(4.))
                        .bg(rgb(source_tint(row.tint)))
                        .when(usable, |d| {
                            d.on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    // Gesture 4, and DESIGN §2's demotion rule at
                                    // once: re-inking lives nowhere but here.
                                    // The library's own right-click menu is the
                                    // affordance this opens -- DESIGN §12 step 5
                                    // allows a stub where extraction-by-hue isn't
                                    // built yet, as long as the menu appears; it
                                    // does, over the same file this dot names.
                                    cx.stop_propagation();
                                    this.selected_asset = Some((dot_path.clone(), stream));
                                    this.library_menu = Some(LibraryMenu {
                                        path: dot_path.clone(),
                                        stream,
                                        at: event.position,
                                        details: false,
                                    });
                                    cx.notify();
                                }),
                            )
                        }),
                )
                .child({
                    let style = mono(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .font(style.font)
                        .text_size(style.size)
                        .text_color(rgb(INK1()))
                        .child(name)
                })
                .child({
                    let style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
                    div()
                        .flex_none()
                        .font(style.font)
                        .text_size(style.size)
                        .text_color(rgb(INK3()))
                        .child(usage)
                })
                .when(usable, |d| {
                    d.child({
                        let label_style = label(type_scale::FLOOR_PX, FontWeight::MEDIUM);
                        let chord_style =
                            mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
                        div()
                            .id(("dock-add-at-playhead", i))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .px(px(4.))
                            .py(px(2.))
                            .rounded(px(3.))
                            .when(!can_insert, |d| d.opacity(0.4).cursor_not_allowed())
                            .when(can_insert, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.insert_source(
                                                &add_path,
                                                stream,
                                                None,
                                                None,
                                                cx,
                                            );
                                            cx.notify();
                                        },
                                    ))
                            })
                            .children(hitmap::dynamic(
                                move || (format!("source.{i}.add"), "Add at playhead".into()),
                                can_insert,
                            ))
                            .tooltip(move |_, cx| {
                                cx.new(|_| {
                                    Tip(
                                        "Add at playhead — ↵ does the same after selecting this source"
                                            .into(),
                                    )
                                })
                                .into()
                            })
                            .child(
                                div()
                                    .font(label_style.font)
                                    .text_size(label_style.size)
                                    .text_color(rgb(INK2()))
                                    .child("Add"),
                            )
                            .child(
                                div()
                                    .font(chord_style.font)
                                    .text_size(chord_style.size)
                                    .text_color(rgb(INK3()))
                                    .child("↵"),
                            )
                    })
                    .child({
                        let style = mono(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
                        div()
                            .id(("dock-proxy-toggle", i))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(2.))
                            .px(px(4.))
                            .py(px(2.))
                            .rounded(px(3.))
                            .when(proxy_on, |d| d.bg(rgb(DARK_RAISED())))
                            .when(proxy_waiting, |d| d.opacity(0.55))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(proxy_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_proxy(&proxy_path, proxy_stops, cx);
                            }))
                            .children(hitmap::dynamic(
                                move || (format!("source.{i}.proxy"), "Toggle proxy".into()),
                                !proxy_waiting,
                            ))
                            .child(
                                div()
                                    .font(style.font)
                                    .text_size(style.size)
                                    .text_color(rgb(if proxy_on { INK1() } else { INK3() }))
                                    .child(proxy_glyph),
                            )
                            .children(proxy_progress.map(|progress| {
                                div()
                                    .flex_none()
                                    .w(px(20.))
                                    .h(px(2.))
                                    .rounded(px(1.))
                                    .bg(rgb(DARK_HAIRLINE()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(px(20. * progress.clamp(0., 1.)))
                                            .rounded(px(1.))
                                            .bg(rgb(INK2())),
                                    )
                            }))
                    })
                }),
        )
        .child({
            let style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
            div()
                .pl(px(14.))
                .truncate()
                .font(style.font)
                .text_size(style.size)
                .text_color(rgb(INK3()))
                .child(under)
        })
}

/// Imported subtitle tracks, grouped by their source. A row selects and drags
/// its track; its header folds that source's rows without becoming a cycle.
fn subtitle_palette(player: &Player, cx: &mut Context<Player>) -> Option<AnyElement> {
    let groups = subtitle_rows(player.session.as_ref()?.subtitles());
    if groups.is_empty() {
        return None;
    }
    let count: usize = groups.iter().map(|group| group.rows.len()).sum();
    let rows: Vec<_> = groups
        .into_iter()
        .enumerate()
        .map(|(group_ord, group)| {
            let folded = player.sub_folded.contains(&group.path);
            let fold_path = group.path.clone();
            let tint = file_tint(player.sources(), &group.path);
            let track_count = group.rows.len();
            let tracks: Vec<_> = group
                .rows
                .into_iter()
                .map(|row| {
                    let track = row.track;
                    let picked = track == player.sub_track;
                    let usable = row.refused.is_none();
                    let title: SharedString = match track_count {
                        1 => row.label,
                        _ => format!("{} {}", row.label, row.number),
                    }
                    .into();
                    let detail: SharedString = row
                        .refused
                        .unwrap_or_else(|| "drag onto an S lane to place".to_string())
                        .into();
                    let ghost = title.clone();
                    div()
                        .id(("dock-subtitle-track", track))
                        .flex_none()
                        .h(px(CONTROL_H))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .rounded(px(3.))
                        .when(picked, |d| d.border_1().border_color(rgb(INK1())))
                        .when(!usable, |d| d.opacity(0.5))
                        .when(usable, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(DARK_RAISED())))
                                .on_drag(SubPick(track), move |_, _, _, cx| {
                                    cx.new(|_| Tip(ghost.clone()))
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.sub_track = track;
                                    cx.notify();
                                }))
                        })
                        .child(
                            div()
                                .flex_none()
                                .w(px(4.))
                                .h(px(16.))
                                .rounded(px(2.))
                                .when_some(tint, |d, tint| d.bg(rgb(tint))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK2()))
                                .child(title),
                        )
                        .child(
                            div()
                                .flex_none()
                                .truncate()
                                .type_style(mono(type_scale::FLOOR_PX, FontWeight::MEDIUM))
                                .text_color(rgb(INK3()))
                                .child(detail),
                        )
                        .child(
                            div()
                                .id(("dock-subtitle-remove", track))
                                .flex_none()
                                .w(px(HIT_MIN))
                                .h_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_subtitle_track(track, cx);
                                }))
                                .children(hitmap::dynamic(
                                    move || {
                                        (
                                            format!("subtitle.{track}.remove"),
                                            "Remove subtitle".into(),
                                        )
                                    },
                                    true,
                                ))
                                .child("×"),
                        )
                })
                .collect();
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .id(("dock-subtitle-group", group_ord))
                        .flex_none()
                        .h(px(CONTROL_H))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(DARK_RAISED())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if !this.sub_folded.remove(&fold_path) {
                                this.sub_folded.insert(fold_path.clone());
                            }
                            cx.notify();
                        }))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("subtitle-group.{group_ord}.fold"),
                                    "Toggle subtitle group".into(),
                                )
                            },
                            true,
                        ))
                        .child(if folded { "▸" } else { "▾" })
                        .when_some(tint, |d, tint| {
                            d.child(
                                div()
                                    .flex_none()
                                    .w(px(4.))
                                    .h(px(16.))
                                    .rounded(px(2.))
                                    .bg(rgb(tint)),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM))
                                .text_color(rgb(INK2()))
                                .child(group.name),
                        )
                        .child(
                            div()
                                .flex_none()
                                .type_style(mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK3()))
                                .child(format!(
                                    "{track_count} track{}",
                                    if track_count == 1 { "" } else { "s" }
                                )),
                        ),
                )
                .when(!folded, |d| d.children(tracks))
        })
        .collect();
    Some(
        div()
            .id("dock-subtitle-palette")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(section_head(format!("SUBTITLES · {count}")))
            .children(rows)
            .into_any_element(),
    )
}

/// The Sources tab, built off MOCK-SPEC.md's "Dock" section: count line,
/// filter, sort chips, rows, IMPORT, hint -- none of it the legacy
/// Media/Audio/Text panel `library.rs` draws. That panel's row facts
/// ([`library_rows`]) are still what every row here is built from.
fn sources_tab(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let sources = player
        .session
        .as_ref()
        .map_or(&[][..], PlaybackSession::sources);
    let all_rows: Vec<Row> = library_rows(
        sources,
        &player.streams,
        &player.decoders,
        player.timeline_audio(),
        |path| {
            player
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(path))
        },
    );
    let filter = player.dock_filter.to_lowercase();
    let mut rows: Vec<(Row, usize)> = all_rows
        .into_iter()
        .filter(|row| player.library_tab.holds(&row.path))
        .map(|row| {
            let placed = player.row_ctx(&row.path, row.stream).placed;
            (row, placed)
        })
        .filter(|(row, placed)| {
            filter.is_empty()
                || row.name.to_lowercase().contains(&filter)
                || row.detail.to_lowercase().contains(&filter)
                || ("unused".contains(&filter) && *placed == 0)
        })
        .collect();
    let unused_count = rows.iter().filter(|(_, placed)| *placed == 0).count();
    match player.dock_sort {
        DockSort::Recent => {}
        DockSort::Name => {
            rows.sort_by(|(a, _), (b, _)| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }
        DockSort::Usage => rows.sort_by(|(_, a), (_, b)| b.cmp(a)),
        DockSort::Unused => rows.sort_by_key(|(_, placed)| *placed > 0),
    }
    let total = rows.len();
    let filter_text: SharedString = player.dock_filter.clone().into();
    div()
        .id("dock-sources")
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(8.))
        .overflow_y_scroll()
        .child(section_head(format!(
            "{} · {total} · {unused_count} UNUSED",
            player.library_tab.label().to_uppercase()
        )))
        .child({
            let style = mono(type_scale::CHORD_METADATA_MAX_PX, FontWeight::MEDIUM);
            div()
                .id("dock-filter")
                .flex_none()
                .cursor_text()
                .font(style.font)
                .text_size(style.size)
                .text_color(rgb(if player.dock_filter_edit {
                    INK1()
                } else {
                    INK3()
                }))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.dock_filter_edit = true;
                    cx.notify();
                }))
                .children(hitmap::control("dock.filter", "Filter sources", true))
                .child(match player.dock_filter.is_empty() {
                    true => "⌕ type to filter — name, codec, unused…".to_string(),
                    false => format!("⌕ {filter_text}"),
                })
        })
        .child(
            div()
                .flex_none()
                .flex()
                .gap(px(4.))
                .children(LIBRARY_TABS.map(|tab| {
                    let on = tab == player.library_tab;
                    let style = label(type_scale::FLOOR_PX, FontWeight::MEDIUM);
                    div()
                        .id(("dock-source-filter", tab as usize))
                        .flex_none()
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(3.))
                        .font(style.font)
                        .text_size(style.size)
                        .when(on, |d| d.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                        .when(!on, |d| d.text_color(rgb(INK3())))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(DARK_RAISED())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.library_tab = tab;
                            cx.notify();
                        }))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("dock.filter.{}", tab.label()),
                                    format!("Filter {}", tab.label()),
                                )
                            },
                            true,
                        ))
                        .child(tab.label())
                })),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .gap(px(4.))
                .children(DockSort::ALL.map(|sort| {
                    let on = sort == player.dock_sort;
                    let style = label(type_scale::FLOOR_PX, FontWeight::MEDIUM);
                    div()
                        .id(("dock-sort", sort as usize))
                        .flex_none()
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(3.))
                        .font(style.font)
                        .text_size(style.size)
                        .when(on, |d| d.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                        .when(!on, |d| d.text_color(rgb(INK3())))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(DARK_RAISED())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.dock_sort = sort;
                            cx.notify();
                        }))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("dock.sort.{}", sort.label()),
                                    format!("Sort {}", sort.label()),
                                )
                            },
                            true,
                        ))
                        .child(sort.label())
                })),
        )
        .child(
            div()
                .id("dock-source-rows")
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_y_scroll()
                .when(rows.is_empty(), |d| {
                    let style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
                    d.child(
                        div()
                            .font(style.font)
                            .text_size(style.size)
                            .text_color(rgb(INK3()))
                            .child(match player.dock_filter.is_empty() {
                                true if player.library_tab == LibraryTab::Text => {
                                    "No subtitle sources yet — Import subtitles".to_string()
                                }
                                true => player.library_tab.empty().to_string(),
                                false => "nothing matches the filter".to_string(),
                            }),
                    )
                })
                .children({
                    let elements: Vec<_> = rows
                        .iter()
                        .enumerate()
                        .map(|(i, (row, placed))| {
                            let picked = player
                                .selected_asset
                                .as_ref()
                                .is_some_and(|p| *p == (row.path.clone(), row.stream));
                            source_row(player, i, row, *placed, picked, cx).into_any_element()
                        })
                        .collect();
                    elements
                }),
        )
        .children(subtitle_palette(player, cx))
        .child(section_head("IMPORT"))
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .child(ghost_verb(
                    "dock-import-files",
                    "Add files",
                    ActionId::AddFiles,
                    false,
                    "adds a file to this list — or drop one on the window",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)),
                ))
                .child(ghost_verb(
                    "dock-paste-path",
                    "Paste path",
                    ActionId::PasteFilePath,
                    false,
                    "imports the file named on the clipboard",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.paste_file_path(cx)),
                ))
                // Subtitles come in through the same door every other file
                // does. This row is `↓S`'s new geographic home: the spine's
                // TRACK group carried it as a two-glyph badge on a 56px rail
                // that no longer had the height for it (user 2026-08-21:
                // "the spine menu is too crowded"), and an import belongs
                // beside the other two imports, not beside the lane verbs.
                .child(ghost_verb(
                    "dock-import-subtitles",
                    "Import subtitles",
                    ActionId::ImportSubtitles,
                    false,
                    "reads an .srt/.ass file onto a subtitle lane",
                    player,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.act(ActionId::ImportSubtitles, window, cx)
                    }),
                )),
        )
        .child({
            // Metadata, not a row: DESIGN §3's metadata role, and the quietest
            // text in the dock: ink3, tight leading so it never competes with
            // the source rows above it.
            let style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
            div()
                .flex_none()
                .line_height(relative(1.25))
                .font(style.font)
                .text_size(style.size)
                .text_color(rgb(INK3()))
                .child("drag · ↵ add · double-click plays")
        })
}

/// The Clip tab: the four verbs DESIGN §5 names, as ghosts, over whichever
/// param-row card they open -- [`Player::eq_card`], [`Player::color_card`],
/// [`Player::transform_card`], [`Player::speed_card`] verbatim, the same
/// param-row rendering `inspector.rs`'s selection section already opens
/// these onto. Drag-while-playing and every other gesture on a row is
/// whatever that card already does; nothing about the gesture is reimplemented
/// here.
fn clip_tab(
    player: &Player,
    width: f32,
    window_size: Size<Pixels>,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // The room actually given here, not the window's: `eq_card_w`/
    // `card_max_w` are asked "how wide may I draw" and answered with the
    // *whole viewport's* width when handed `window_size` verbatim -- but this
    // tab is a ~280-390px strip inside the dock, not the window, so a card
    // capped at up to 720px overflowed past the window's own right edge
    // (`20k` rendered as `20`, the `Spectrum on s` button cut mid-glyph).
    // Height stays the window's: none of the un-maximized cards' own
    // arithmetic uses it (`below_picture_floor` only fires once maximized,
    // and a maximized card is mounted at `ui::stance::render` instead, never
    // through this `room`).
    let room = Size::new(px(width), window_size.height);
    let none_open = player.eq_open.is_none()
        && player.color_open.is_none()
        && player.transform_open.is_none()
        && player.speed_open.is_none()
        && player.silence_open.is_none()
        && !player.mix_open
        && !player.subtitle_style_open;
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
                ))
                // Silence and Mix: two more verbs over param rows, GAP 2's
                // fix for "the card had a key but no way in" -- the same
                // ghost-verb-over-inline-card anatomy the four above already
                // use, not a new pattern. Subtitle style is *not* here: its
                // natural home is beside the subtitle lane it edits
                // (`bench_stance.rs`, another builder's file this session);
                // it stays reachable by its chord until that lane grows a
                // header to hang a verb on.
                .child(ghost_verb(
                    "dock-verb-silence",
                    "Silence",
                    ActionId::Silence,
                    player.silence_open.is_some(),
                    "scans for quiet stretches to cut or speed up",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_silence(cx)),
                ))
                .child(ghost_verb(
                    "dock-verb-mix",
                    "Mix",
                    ActionId::Mix,
                    player.mix_open,
                    "track volumes and the limiter",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_mix(None, cx)),
                ))
                // Fit: a cycle, not a card, so `active` is always false --
                // there is no open state to hold it down for, only the click
                // itself (same anatomy as `cycle_fit`'s own inspector.rs row,
                // legacy's `crates/app/src/ui/inspector.rs:244-253`). It was
                // reachable only by its chord or buried in the clip's
                // right-click menu (`overlays.rs`'s `Pick::Fit`); this is its
                // direct button, beside the other per-clip verbs it keeps
                // company with.
                .child(ghost_verb(
                    "dock-verb-fit",
                    "Fit",
                    ActionId::Fit,
                    false,
                    "how this picture is placed on the project canvas",
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.cycle_fit(cx)),
                )),
        )
        .child(
            div()
                .id("dock-clip-rows")
                .flex_1()
                .min_h(px(0.))
                .px(px(8.))
                .pb(px(8.))
                // Maximized is mounted at the window root instead
                // (`ui::stance::render`'s `stance-centre`), not here: this
                // column is a ~280-390px strip, and an absolutely-positioned
                // child is sized against its *immediate* parent regardless of
                // any `.relative()` marker -- so a maximized card asking for
                // window-sized room while still parented here cannot get it,
                // it only gets pushed around inside this narrow, short box
                // (the "maximize shrinks the card" defect). Un-maximized, the
                // seven cards below stay docked here exactly as before.
                .when(!player.card_maximized, |d| {
                    d.children(player.eq_card(room, cx))
                        .children(player.color_card(room, cx))
                        .children(player.transform_card(room, cx))
                        .children(player.speed_card(room, cx))
                        .children(player.silence_card(room, cx))
                        .children(player.mix_card(room, cx))
                        // Subtitle style has no verb row of its own here (see
                        // the comment above the ghost verbs) but its card
                        // still has to be mounted somewhere in the darkroom
                        // or its chord (`y`) opens an invisible modal (GAP
                        // 2) -- this is that mount, painted in the Clip tab
                        // like the six cards beside it until the subtitle
                        // lane grows the header that is its real home.
                        .children(player.subtitle_style_card(room, cx))
                })
                // A plate, not bare space: DESIGN §11's "states" checklist --
                // nothing picked reads as a hint, not as a hole in the panel.
                .when(none_open, |d| {
                    let style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
                    d.child(
                        div()
                            .rounded(px(2.))
                            .bg(rgb(DARK_CANVAS()))
                            .p(px(8.))
                            .font(style.font)
                            .text_size(style.size)
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
/// Sources scrolls its own row list; Clip's param rows are whatever their
/// card already draws at this width.
pub(crate) fn render(
    player: &Player,
    width: f32,
    window_size: Size<Pixels>,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let src_active = player.dock_src_active;
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
                .px(px(8.))
                .border_b_1()
                .border_color(rgb(DARK_HAIRLINE()))
                .child(dock_tab("dock-tab-src", "SOURCES", src_active, cx))
                .child(dock_tab("dock-tab-clip", "CLIP", !src_active, cx)),
        )
        .child(match src_active {
            true => sources_tab(player, cx).into_any_element(),
            false => clip_tab(player, width, window_size, cx).into_any_element(),
        })
}
