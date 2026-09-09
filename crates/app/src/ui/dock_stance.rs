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
use crate::ui::stance::{Surface, is_focus_cycle_key, is_focus_exit_key, next_surface};
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
pub(crate) fn save(src_active: bool) {
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

/// A ghost verb (DESIGN §4): borderless glyph/label in `ink2`, its chord in
/// `ink3` beside it, read live off the keymap so it can never drift from the
/// key that does the same thing. Hover is one fill step and an ink brighten;
/// held open (`active`) keeps both. A verb the current *state* refuses dims
/// and says why on hover; a verb the clip's *media kind* can never use
/// ([`Enable::Hidden`]) is left out of the row entirely instead (§8) -- the
/// same `listed()`/`Hidden` split the clip menu already reads.
fn ghost_verb(
    id: &'static str,
    verb_label: &'static str,
    action: ActionId,
    active: bool,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Option<impl IntoElement> {
    let enabled = player.enable(action, None);
    if !enabled.listed() {
        return None;
    }
    let key = player.keymap.chord(action);
    let on = enabled.yes();
    let label_style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    let chord_style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
    Some(
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
            // One hover line for the whole room (`widgets::tip_line`): the
            // verb and its stroke, not the sentence this row used to carry
            // (DESIGN §8 -- a tooltip is not instructional prose).
            .tooltip(crate::ui::widgets::action_hover(player, action))
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
            ),
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
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            // The keys list is body state, not a tab (user 2026-09-09:
            // "remove keys section from here since we also have it next to
            // ledger") -- picking either tab is also the way out of it, and
            // the pair itself (`dock_src_active`) is what the file keeps.
            this.keys_open = false;
            let surface = match label_text == "SOURCES" {
                true => Surface::Dock,
                false => Surface::Inspector,
            };
            // Same dead-handle class as `keys_tab`'s: the tab being left
            // takes its `FocusHandle`'s element out of the tree with it, so a
            // ring that was on that handle has to move with the tab or the
            // keyboard goes silent. Only when the ring is actually in the
            // dock -- a tab picked while the bench (or nothing) has focus
            // does not steal it.
            let in_ring = window
                .focused(cx)
                .is_some_and(|f| f == this.focus_dock || f == this.focus_inspector);
            match in_ring {
                true => this.focus_surface(surface, window, cx),
                false => {
                    this.dock_src_active = label_text == "SOURCES";
                    save(this.dock_src_active);
                }
            }
            cx.notify();
        }))
        .tooltip(crate::ui::widgets::tip_hover(&format!("Show {}", label_text.to_lowercase()), "", None))
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
/// (MOCK-SPEC "Dock" §4's usage line). The clip *count* that used to trail it
/// ("2 uses") is gone with cleanse round 2 (2026-09-10): the lanes are the
/// answer to "is it in the film?", the number is a statistic, and the
/// Properties side of the row menu still prints it whole.
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
    lanes.join(" ")
}

/// The file's name without its extension -- what the editor calls the film,
/// not what the filesystem calls the bytes (cleanse round 2). A row that
/// names several streams of one file keeps the stream half it was given
/// ([`library_meta::row_name`] appends it after the file name).
///
/// corner-cut: private to the dock while U2's shared `stem()` is unlanded;
/// the lead reconciles the two into `ui/widgets.rs` or `files.rs`.
pub(crate) fn stem(name: &str) -> String {
    let (file, tail) = match name.split_once(" [") {
        Some((file, tail)) => (file, format!(" [{tail}")),
        None => (name, String::new()),
    };
    match file.rsplit_once('.') {
        // A name that is *all* extension (".srt") keeps it: it is the name.
        Some(("", _)) | None => format!("{file}{tail}"),
        Some((base, _)) => format!("{base}{tail}"),
    }
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
    let name: SharedString = stem(&row.name).into();
    // The decoder seat (`HW`/`SW`) is off the row with cleanse round 2: the
    // codec is what the file *is*, the seat is how this machine happens to
    // read it today, and the transport line and the Properties side both
    // still say it. Read off `Backend::label` rather than a copy of those
    // words, so a renamed seat cannot leave a stale filter behind.
    let seats = [
        Backend::Opening,
        Backend::Hardware,
        Backend::Software,
        Backend::Still,
        Backend::Gap,
    ]
    .map(Backend::label);
    let detail = row
        .detail
        .split(" · ")
        .filter(|part| !seats.contains(part))
        .collect::<Vec<_>>()
        .join(" · ");
    let under: String = match &row.unusable {
        Some(why) => why.clone(),
        None => join_detail(
            &join_detail(
                &detail,
                &timecode(f64::from(row.frames) / player.fps, player.fps),
            ),
            &usage_line(player, row.tint, placed),
        ),
    };
    let (path, stream) = (row.path.clone(), row.stream);
    let dragged = (path.clone(), stream);
    let dot_path = path.clone();
    let menu_path = path.clone();
    let add_path = path.clone();
    let ghost = name.clone();
    let can_insert = usable && player.session.is_some() && player.exporting().is_none();
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
        .border_1()
        .border_color(match picked {
            // The ring is always in flow -- transparent at rest, ink1 when
            // picked -- because a `when(picked, border_1())` added a pixel of
            // box on selection and shoved every row's text sideways.
            true => rgb(INK1()).into(),
            false => gpui::transparent_black(),
        })
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
        .tooltip(crate::ui::widgets::tip_hover("Source", "drag to a lane", None))
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
                        // FAULT: this was `min_w(px(0.))` -- the row's five
                        // flex_none siblings (usage, Preview, Add ↵, proxy)
                        // took their full width first and the name, the one
                        // thing a row is *for*, collapsed to nothing at the
                        // default dock width: the library read `● V1 A1 · 2
                        // uses Preview Add ↵ ○` with no filename at all. The
                        // name is the first line and keeps at least 60% of
                        // the row; the verbs shrank to glyph ghosts and the
                        // usage moved down to the metadata line.
                        .min_w(relative(0.6))
                        .truncate()
                        .font(style.font)
                        .text_size(style.size)
                        .text_color(rgb(INK1()))
                        .child(name)
                })
                // Preview (`▷`) and the stand-in toggle (`○`) are off the
                // row with cleanse round 2: a double-click starts the source playing
                // and the row menu carries `Preview`, the stand-in lives in
                // that same menu's `Proxy` row and in Settings. What is left
                // on the right edge is the one verb a source row is *for*.
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
                                    .child("+"),
                            )
                            .child(
                                div()
                                    .font(chord_style.font)
                                    .text_size(chord_style.size)
                                    .text_color(rgb(INK3()))
                                    .child("↵"),
                            )
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

/// Imported subtitle tracks, grouped by their source -- the Text tab's own
/// rows (DESIGN.md's "text section ... not serving to anything", user
/// 2026-08-27: relocated off the unconditional spot under the source list
/// and into the tab that names them). A row selects and drags its track; its
/// header folds that source's rows without becoming a cycle. Styled off
/// [`source_row`] -- same dot, same name/detail ink and type scale, same
/// hover fill and selection ring -- so Text stops reading as a second
/// dialect from Media/Audio. Hands back the track count for the tab header's
/// own count line, same as `rows.len()` does for the other two tabs.
fn subtitle_tab_rows(player: &Player, cx: &mut Context<Player>) -> (usize, Vec<AnyElement>) {
    let mut groups = match player.session.as_ref() {
        Some(session) => subtitle_rows(session.subtitles()),
        None => Vec::new(),
    };
    // The same search box the other two tabs answer to -- a visible filter
    // that a whole tab ignored would be a dead control. It matches a track's
    // label or its file's name; a group with no surviving track leaves with
    // its rows.
    let filter = player.dock_filter.to_lowercase();
    if !filter.is_empty() {
        for group in &mut groups {
            if group.name.to_lowercase().contains(&filter) {
                continue;
            }
            group
                .rows
                .retain(|row| row.label.to_lowercase().contains(&filter));
        }
        groups.retain(|group| !group.rows.is_empty());
    }
    // Which lane(s) actually hold each track's captions right now -- the
    // row's second line answers "is it showing?", so it has to read the
    // timeline, not stay the literal "not placed" it was born saying (the
    // old palette's badge never updated after a drop).
    let placed_on = |track: usize| -> Option<String> {
        let session = player.session.as_ref()?;
        let lanes: Vec<String> = session
            .lanes()
            .into_iter()
            .filter(|lane| session.sub_lane(*lane).iter().any(|sub| sub.track == track))
            .map(|lane| lane.label())
            .collect();
        if lanes.is_empty() {
            None
        } else {
            Some(lanes.join(" "))
        }
    };
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
                        .or_else(|| placed_on(track))
                        .unwrap_or_else(|| "not placed".to_string())
                        .into();
                    let ghost = title.clone();
                    let hitmap_title = title.clone();
                    let select_title = title.clone();
                    div()
                        .id(("dock-subtitle-track", track))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        // A track is the header's subset, and the row says
                        // so the way an outline does: stepped in under the
                        // header's own name (user 2026-08-27, "indentation
                        // is needed for subset").
                        .pl(px(30.))
                        .pr(px(8.))
                        .py(px(4.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(match picked {
                            // The ring is always in flow -- transparent at rest, ink1 when
                            // picked -- because a `when(picked, border_1())` added a pixel of
                            // box on selection and shoved every row's text sideways.
                            true => rgb(INK1()).into(),
                            false => gpui::transparent_black(),
                        })
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
                        .tooltip(crate::ui::widgets::tip_hover("Subtitle track", "drag to a lane", None))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("subtitle.{group_ord}.{track}.row"),
                                    hitmap_title.to_string(),
                                )
                            },
                            usable,
                        ))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("subtitle.{group_ord}.{track}.select"),
                                    format!("Select {select_title}"),
                                )
                            },
                            usable,
                        ))
                        .child(
                            // The same round dot `source_row`'s own name line
                            // opens with, not the old vertical bar -- one dot
                            // shape for every row this dock draws.
                            div()
                                .flex_none()
                                .w(px(8.))
                                .h(px(8.))
                                .rounded(px(4.))
                                .when_some(tint, |d, tint| d.bg(rgb(tint))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                // A step below the header's LABEL_ROW_PX --
                                // subset smaller than superset (user
                                // 2026-08-27), same ink so it stays a name,
                                // not a footnote.
                                .type_style(mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK1()))
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
                                .h(px(HIT_MIN))
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
                                .tooltip(crate::ui::widgets::tip_hover("Remove subtitle track", "", None))
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
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(DARK_RAISED())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if !this.sub_folded.remove(&fold_path) {
                                this.sub_folded.insert(fold_path.clone());
                            }
                            cx.notify();
                        }))
                        .tooltip(crate::ui::widgets::tip_hover("Fold this source", "click", None))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("subtitle-group.{group_ord}.fold"),
                                    "Toggle subtitle group".into(),
                                )
                            },
                            true,
                        ))
                        // Bare, the glyph inherited the panel's dim ink and
                        // vanished ("the little arrow on left side is hard
                        // to see", user 2026-08-27) -- it reads at the same
                        // ink as the name it folds.
                        .child(
                            div()
                                .flex_none()
                                .w(px(10.))
                                .text_color(rgb(INK1()))
                                .child(if folded { "▸" } else { "▾" }),
                        )
                        .when_some(tint, |d, tint| {
                            d.child(
                                div()
                                    .flex_none()
                                    .w(px(8.))
                                    .h(px(8.))
                                    .rounded(px(4.))
                                    .bg(rgb(tint)),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(mono(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM))
                                .text_color(rgb(INK1()))
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
                .into_any_element()
        })
        .collect();
    (count, rows)
}

/// The dock's own menu ([`DOCK_ITEMS`]): the three ways a file gets in, on a
/// right-click anywhere in the Sources body a row does not claim first. It is
/// where `Paste path` and `Import subtitles` went when cleanse round 2 took
/// their footer rows, so both keep a pointer door -- including on an empty
/// library, which is when a pasted path is worth most.
fn opens_dock_menu(
    cx: &mut Context<Player>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(|this, event: &MouseDownEvent, _, cx| {
        if this.modal() || this.session.is_none() {
            return;
        }
        cx.stop_propagation();
        this.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::Dock,
            at: event.position,
            details: false,
        });
        cx.notify();
    })
}

/// The Sources tab, cleanse round 2 (2026-09-10, user "seems cool, let's
/// apply"): one filter and one list of everything this project holds --
/// picture, sound, and the subtitle tracks beside them -- with what a row
/// *is* read off its own metadata line instead of off a sub-tab the editor
/// had to pick first. The `MEDIA` head, the `Media / Audio / Text` strip, the
/// `↕ Recent` sort cycle and the `IMPORT` head are all gone with it; the row
/// facts are still [`library_rows`]' and [`subtitle_tab_rows`]'.
fn sources_tab(
    player: &Player,
    _window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
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
    let rows: Vec<(Row, usize)> = all_rows
        .into_iter()
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
    let mut row_elements: Vec<AnyElement> = rows
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
    // The subtitle tracks join the same list rather than hiding behind a tab
    // of their own: a standalone `.srt` is a source in the sense that matters
    // here -- something that came in and can go on a lane -- and the tracks
    // inside a container stay grouped under the file that carries them.
    let (_, subtitles) = subtitle_tab_rows(player, cx);
    row_elements.extend(subtitles);
    let filter_text: SharedString = player.dock_filter.clone().into();
    div()
        .id("dock-sources")
        .track_focus(&player.focus_dock)
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            cycle_on_key_down(Surface::Dock)(this, event, window, cx)
        }))
        // Right-click anywhere the rows are not: the ways *in* (Add files,
        // Paste path, Import subtitles), which is what the dock itself can be
        // told -- and the door those last two kept when their rows left the
        // footer. A row's own right-click stops here first (`source_row`).
        .on_mouse_down(MouseButton::Right, opens_dock_menu(cx))
        // The focus ring is not painted (user 2026-09-09: "clicking through
        // timeline draws a white overlay around the timeline, same happens
        // for library section too"). The border stays in flow, transparent in
        // every state, so keyboard focus still moves and nothing shifts.
        .border_1()
        .border_color(gpui::transparent_black())
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(8.))
        .overflow_y_scroll()
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
                .tooltip(crate::ui::widgets::tip_hover("Filter sources", "type to narrow", None))
                .children(hitmap::control("dock.filter", "Filter sources", true))
                // The glass alone at rest: the word "filter" beside it was
                // the box telling the editor what a filter box is (DESIGN §8).
                .child(match player.dock_filter.is_empty() {
                    true => "⌕".to_string(),
                    false => format!("⌕ {filter_text}"),
                })
        })
        .child(
            div()
                .id("dock-source-rows")
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_y_scroll()
                // The list fills the dock, so the empty stretch under the last
                // row is *this* element and not its parent: the menu opens off
                // both, or a right-click below the rows finds nothing.
                .on_mouse_down(MouseButton::Right, opens_dock_menu(cx))
                .when(row_elements.is_empty(), |d| {
                    let style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
                    d.child(
                        div()
                            .font(style.font)
                            .text_size(style.size)
                            .text_color(rgb(INK3()))
                            // One noun for both empties -- nothing imported and
                            // nothing matching are the same sight, and neither
                            // is a sentence (DESIGN §8).
                            .child("none"),
                    )
                })
                .children(row_elements),
        )
        // One door in, at the bottom. `Paste path` (`^l`) and `Import
        // subtitles` (`^i`) keep their strokes, their KEYS rows and the
        // dock's own right-click menu; `Add files` takes a `.srt`/`.vtt` as
        // happily as an `.mkv` (`Player::import` forks on `is_subtitle`), so
        // subtitles arrive through this row too.
        .children(ghost_verb(
            "dock-import-files",
            "Add",
            ActionId::AddFiles,
            false,
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)),
        ))
}

/// The transition a clip carries into its immediate successor, if any -- a
/// dissolve on a video lane ([`Clip::transition_out`]) or a crossfade on an
/// audio lane, told apart from a plain one-sided fade drag by
/// [`Project::crossfade`]'s own signature: both ends of the join set
/// together, not just this clip's tail. `cap` is what the pair actually
/// offers -- the shorter of the two clips' own lengths -- the same ceiling
/// [`Project::set_transition_out`]/[`Project::crossfade`] clamp to, so the
/// row this feeds never claims more room than a step could land in.
pub(crate) fn transition_of(
    lane_kind: LaneKind,
    clip: &Clip,
    next: Option<&Clip>,
) -> Option<(&'static str, u32, u32)> {
    let next = next.filter(|n| n.start == clip.end())?;
    let cap = clip.frames().min(next.frames());
    match lane_kind {
        LaneKind::Video if clip.transition_out > 0 => Some(("Dissolve", clip.transition_out, cap)),
        LaneKind::Audio if clip.fade_out > 0 && next.fade_in > 0 => {
            Some(("Crossfade", clip.fade_out, cap))
        }
        _ => None,
    }
}

/// The transition duration row: `transition_of` picks the anchor's
/// transition and its cap, [`Player::nudge_transition`] does the clamped
/// step, [`Player::edit_transition`]/[`Player::commit_transition`] the
/// type-in (DEBT #111) -- only the paint is this tab's own (`ghost_verb`'s
/// row height and ink tokens).
fn transition_row(player: &Player, cx: &mut Context<Player>) -> Option<impl IntoElement> {
    let (lane, idx) = player.selected.anchor()?;
    let session = player.session.as_ref()?;
    let clips = session.lane_clips(lane);
    let (label_text, frames, cap) = transition_of(lane.kind, clips.get(idx)?, clips.get(idx + 1))?;
    let label_style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    let field = player.transition_edit.as_ref();
    let step = |id: &'static str,
                label: &'static str,
                glyph: &'static str,
                by: i32,
                cx: &mut Context<Player>| {
        div()
            .id(id)
            .flex()
            .w(px(HIT_MIN))
            .h(px(HIT_MIN))
            .items_center()
            .justify_center()
            .rounded(px(3.))
            .bg(rgb(DARK_RAISED()))
            .cursor_pointer()
            .hover(|s| s.text_color(rgb(INK1())))
            .tooltip(crate::ui::widgets::tip_hover(label, "", None))
            .children(hitmap::control(id, label, true))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.nudge_transition(lane, idx, by, cx);
            }))
            .child(glyph)
    };
    Some(
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(CONTROL_H))
            .px(px(8.))
            .child(
                div()
                    .id("transition-duration-field")
                    .flex_1()
                    .font(label_style.font)
                    .text_size(label_style.size)
                    .text_color(rgb(INK2()))
                    .cursor_pointer()
                    .tooltip(crate::ui::widgets::tip_hover("Transition duration", "click to type", None))
                    .children(hitmap::control(
                        "transition-duration-field",
                        "Transition duration, type-in",
                        true,
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.edit_transition(frames, cap);
                        cx.notify();
                    }))
                    .child(match field {
                        Some(edit) => edit.detail(),
                        None => format!("{label_text} {frames}f (of {cap}f offered)"),
                    }),
            )
            .child(step("transition-minus", "Shorten transition", "−", -1, cx))
            .child(step("transition-plus", "Lengthen transition", "+", 1, cx)),
    )
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
    _window: &mut Window,
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
    div()
        .id("dock-clip")
        .track_focus(&player.focus_inspector)
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            cycle_on_key_down(Surface::Inspector)(this, event, window, cx)
        }))
        // The focus ring is not painted (user 2026-09-09: "clicking through
        // timeline draws a white overlay around the timeline, same happens
        // for library section too"). The border stays in flow, transparent in
        // every state, so keyboard focus still moves and nothing shifts.
        .border_1()
        .border_color(gpui::transparent_black())
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
                .children(ghost_verb(
                    "dock-verb-speed",
                    "Speed",
                    ActionId::Speed,
                    player.speed_open.is_some(),
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_speed(cx)),
                ))
                .children(ghost_verb(
                    "dock-verb-color",
                    "Colour",
                    ActionId::Color,
                    player.color_open.is_some(),
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_color(cx)),
                ))
                .children(ghost_verb(
                    "dock-verb-transform",
                    "Transform",
                    ActionId::Transform,
                    player.transform_open.is_some(),
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_transform(cx)),
                ))
                .children(ghost_verb(
                    "dock-verb-eq",
                    "EQ",
                    ActionId::Equalizer,
                    player.eq_open.is_some(),
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
                .children(ghost_verb(
                    "dock-verb-silence",
                    "Silence",
                    ActionId::Silence,
                    player.silence_open.is_some(),
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_silence(cx)),
                ))
                .children(ghost_verb(
                    "dock-verb-mix",
                    "Mix",
                    ActionId::Mix,
                    player.mix_open,
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
                .children(ghost_verb(
                    "dock-verb-fit",
                    "Fit",
                    ActionId::Fit,
                    false,
                    player,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.cycle_fit(cx)),
                )),
        )
        .children(transition_row(player, cx))
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
                }),
        )
}

/// The KEYS tab (DESIGN §9, amended 2026-09-09): every bound command,
/// sectioned by [`keymap::Category`] exactly as [`keys_rows`] files them --
/// reusing that registry-driven order is what makes an action added anywhere
/// land here without a second list to forget. It lives in the dock beside
/// SOURCES and CLIP rather than on a plate over the bench: stable geography,
/// occludes nothing, scrolls like its neighbours, and the tab strip itself is
/// the way back, so there is no heading prose and no `esc` hint to write.
///
/// An action the room would refuse right now greys to `ink4`, the same ink a
/// refused verb wears (DESIGN §8) -- never hidden: the list is the full truth
/// about the keyboard, including the strokes that would answer `No` today.
fn keys_tab(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    // The surface this body is standing in for. `keys_tab` replaces whichever
    // tab was mounted, and gpui dispatches a key only along the *rendered*
    // focus node's ancestors: a `FocusHandle` whose element left the tree
    // falls back to the window's own root node, which the room's handler
    // (`ui::stance::render`'s div) is not -- so every key dies until the next
    // click. Mounting the same handle here is what keeps `?` able to close
    // the list it just opened (measured 2026-09-09: second `?` and `space`
    // reached no listener at all).
    let surface = match player.dock_src_active {
        true => Surface::Dock,
        false => Surface::Inspector,
    };
    let focus = player.focus_handle(surface).clone();
    let row_label = label(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
    let row_chord = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
    let pair = move |text: String, chord: String, ink: u32| {
        div()
            .flex_none()
            .flex()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    // The label gives way, never the chord: a long parenthetical
                    // ("Previous sync point (a cut here is copied...)") pushed
                    // the chord column clean off the dock's right edge until
                    // this row took `min_w(0)` + ellipsis, the same shape a
                    // source row's name already uses.
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font(row_label.font.clone())
                    .text_size(row_label.size)
                    .text_color(rgb(ink))
                    .child(text),
            )
            .child(
                div()
                    .flex_none()
                    .font(row_chord.font.clone())
                    .text_size(row_chord.size)
                    .text_color(rgb(if ink == INK4() { INK4() } else { INK3() }))
                    .child(chord),
            )
            .into_any_element()
    };
    div()
        .id("dock-keys-rows")
        .track_focus(&focus)
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            cycle_on_key_down(surface)(this, event, window, cx)
        }))
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .gap(px(3.))
        .p(px(8.))
        .overflow_y_scroll()
        // The list no longer wears a tab, so it says its own name the way
        // every other dock body section does -- MEDIA / IMPORT's head.
        .child(section_head("KEYS"))
        .children(keys_rows().into_iter().map(|row| match row {
            KeyRow::Head(category) => div()
                .flex_none()
                .pt(px(4.))
                .child(section_head(category.label().to_uppercase()))
                .into_any_element(),
            // The full-truth surface: every chord the action answers to
            // (`display`), not the badge's primary-only compact form.
            KeyRow::Act(action) => pair(
                action.label().to_string(),
                player.keymap.display(action),
                match player.enable(action, None) {
                    Enable::Yes => INK2(),
                    _ => INK4(),
                },
            ),
            // A fixed stroke is the window's, not an action the room can
            // refuse, so it never greys.
            KeyRow::Fixed(i) => {
                let f = &keymap::FIXED[i];
                pair(f.label.to_string(), f.chord.to_string(), INK2())
            }
        }))
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
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let keys = player.keys_open;
    let src_active = player.dock_src_active && !keys;
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
                .child(dock_tab("dock-tab-clip", "CLIP", !src_active && !keys, cx)),
        )
        .child(match (keys, src_active) {
            (true, _) => keys_tab(player, cx).into_any_element(),
            (false, true) => sources_tab(player, window, cx).into_any_element(),
            (false, false) => clip_tab(player, width, window_size, window, cx).into_any_element(),
        })
}

/// The Tab/Shift-Tab handler shared by the dock's two tabs (`sources_tab`'s
/// "dock/library" and `clip_tab`'s "inspector") -- only [`is_focus_cycle_key`]
/// is ever answered here; every other key is left un-stopped so it bubbles
/// to the room's root handler (`ui::stance::render`'s `on_key_down`), which
/// is the one every other keybind, Tab included when nothing in the ring
/// has focus yet, still hangs off.
fn cycle_on_key_down(
    surface: Surface,
) -> impl Fn(&mut Player, &KeyDownEvent, &mut Window, &mut Context<Player>) + 'static {
    move |this, event, window, cx| {
        let key = event.keystroke.key.as_str();
        if is_focus_cycle_key(key) {
            let next = next_surface(surface, event.keystroke.modifiers.shift);
            this.focus_surface(next, window, cx);
            cx.stop_propagation();
        } else if is_focus_exit_key(key) {
            // A dismissible overlay (context menu, library menu, picker, any
            // card, a live preview) opened by a click into the dock/inspector
            // takes this escape first -- the same door the root handler
            // itself closes it through (`Player::escape_closes_overlay`) --
            // so it does not go on sitting open just because focus happened
            // to be inside this ring when escape was pressed (the "esc is
            // not closing menus opened by clicking in the clip section"
            // report).
            if this.escape_closes_overlay(cx) {
                cx.stop_propagation();
                return;
            }
            // Same exit as `stance.rs`'s bench handler: leaves the ring for
            // the root handle rather than letting escape bubble to whatever
            // the root does with it.
            window.focus(&this.focus);
            cx.stop_propagation();
            cx.notify();
        }
    }
}
