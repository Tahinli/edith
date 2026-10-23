//! A placed caption's box on a subtitle lane (DESIGN §6).

use super::clip::trim_plate;
use super::*;
use crate::player::caption_cues::{CueGrab, CueMenu};
use gpui::{anchored, deferred};

/// A placed caption's box, on a subtitle lane: [`clip_box`]'s twin, minus the
/// thumbnail/waveform trace a caption has none of (DESIGN §6 -- "the subtitle
/// cue edge IS a cut", so the same selection ring, drag, edge-trim and
/// right-click menu a clip gets apply here too). Reused rather than
/// reimplemented: `Player::trimmed_sub`/`SubDrag`/`sub_pick_name`/`cue_box`,
/// the same plumbing `ui/timeline.rs`'s own caption box already drives.
pub(super) fn sub_box(
    player: &Player,
    lane: Lane,
    idx: usize,
    sub: &SubClip,
    scale: Scale,
    (picks, pick_links): (&[(Lane, usize)], &[Option<u32>]),
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let placed = *sub;
    let shown = player.trimmed_sub(lane, idx, placed);
    let (start, len) = (
        f64::from(shown.start) / player.fps,
        f64::from(shown.frames) / player.fps,
    );
    let span = scale.width_px(len);
    let width = clip_width(span);
    let left = scale.px_at(start);
    let on = marked((lane, idx), placed.link, picks, pick_links);
    // The caption a move is carrying is dimmed where it stands, as a clip's
    // is (`clip_box`), and the edge in the hand gets its plate from the
    // caption's own old and in-flight lengths (`trim_plate`).
    let dragging = player.drag_src == Some((lane, idx));
    let dragged_edge = player
        .trim
        .filter(|trim| (trim.lane, trim.idx) == (lane, idx))
        .map(|trim| (trim, placed.frames, shown.frames));
    // This placement's own cues, where they fall in its body
    // (`PlaybackSession::sub_lane_cue_spans`, the map the export and the picture's
    // own plate go through). The lane's cues are clipped to each window and
    // placements on a lane never overlap, so the ones that start inside this
    // span are this placement's. Each keeps the (track, cue) it is, which is
    // what a click, a drag and a right-click act on.
    let cues: Vec<CueBlock> = player.session.as_ref().map_or_else(Vec::new, |s| {
        s.sub_lane_cue_spans(lane)
            .into_iter()
            .filter_map(|span| {
                let cue = s.subtitles().get(span.track)?.cues.get(span.cue)?;
                let (mut cl, mut cw) = crate::subs::span_box(scale, span.start_us, span.end_us);
                // A cue in the hand is drawn where the drag has it: the delta of its own clock,
                // turned into the timeline's by the proportion the placement shows it at.
                if let Some(d) = player.dragged_cue(span.track, span.cue) {
                    let shown = (span.end_us - span.start_us) as f64;
                    let window = (span.window.1 - span.window.0).max(1) as f64;
                    let px_per_track_us = scale.width_px(1e-6) * (shown / window) as f32;
                    cl += (d.now.0 - d.orig.0) as f32 * px_per_track_us;
                    cw += ((d.now.1 - d.now.0) - (d.orig.1 - d.orig.0)) as f32 * px_per_track_us;
                }
                let rel = cl - left;
                (rel + cw > 0.0 && rel < width).then(|| CueBlock {
                    left: rel,
                    width: cw.max(1.),
                    track: span.track,
                    cue: span.cue,
                    // The words being typed show on the block as they are typed, not at the click away.
                    text: match player.cue_edit.as_ref().filter(|e| (e.lane, e.track, e.cue) == (lane, span.track, span.cue)) {
                        Some(edit) => edit.text.replace('\n', " "),
                        None => cue.text.replace('\n', " "),
                    },
                    picture: cue.image.is_some(),
                })
            })
            .collect()
    });
    // The cue being retyped, if it is one of this placement's: a small plate floating over its
    // own block (`cue_edit_plate`), so the lane's height never clips the words.
    let editing = player.cue_edit.as_ref().filter(|edit| edit.lane == lane && !edit.in_panel).map(|e| (e.track, e.cue));
    let selected_cue = player.sel_cue.filter(|&(l, _, _)| l == lane).map(|(_, t, c)| (t, c));
    let menu = player
        .cue_menu
        .filter(|m| m.lane == lane && cues.iter().any(|c| (c.track, c.cue) == (m.track, m.cue)))
        .map(|m| cue_menu_plate(player, m, cx));
    let label = player
        .session
        .as_ref()
        .and_then(|s| sub_pick_name(s.subtitles(), placed.track));
    let ghost: SharedString = label.unwrap_or_else(|| lane.label()).into();
    // The room a trim zone may spill into on each side: the middle of the gap
    // to the neighbouring cue there (`interact::trim_zone`), exactly as a
    // clip's is, so a cue edge is the same target a clip edge is.
    let lane_subs: &[SubClip] = player
        .session
        .as_ref()
        .map_or(&[][..], |s| s.sub_lane(lane));
    let room_before = idx
        .checked_sub(1)
        .and_then(|p| lane_subs.get(p))
        .map_or(f32::INFINITY, |p| {
            scale.width_px(f64::from(shown.start.saturating_sub(p.end())) / player.fps)
        });
    let room_after = lane_subs.get(idx + 1).map_or(f32::INFINITY, |n| {
        scale.width_px(f64::from(n.start.saturating_sub(shown.end())) / player.fps)
    });
    // Zones are siblings of the body, not children: the body clips its
    // contents to its own edges, and a zone must reach past them.
    let zones = [Edge::Start, Edge::End]
        .into_iter()
        .filter(|_| trims(span))
        .map(|edge| {
            let (inside, outside) = trim_zone(
                span,
                match edge {
                    Edge::Start => room_before,
                    Edge::End => room_after,
                },
            );
            let zone = div()
                .absolute()
                .top_0()
                .h_full()
                .w(px(inside + outside))
                .occlude()
                .cursor(CursorStyle::ResizeLeftRight)
                .hover(|s| s.bg(rgb(INK1())))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.start_trim(lane, idx, edge, event.modifiers.control, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.open_menu(lane, idx, event.position, cx);
                    }),
                );
            match edge {
                Edge::Start => zone.left(px(-outside)),
                Edge::End => zone.right(px(-outside)),
            }
        })
        .collect::<Vec<_>>();
    // The cues themselves: where in this window the track's words fall, each a block of its
    // own with its words inside -- a placement is a window of a track, not full of speech. They
    // are siblings of the body, which sits under them: a block's press stops there, so it is the
    // caption's and never starts the placement's own drag (and no occlude, which would starve the
    // root of the moves a drag inside the block needs).
    let cue_blocks = cues
        .into_iter()
        .map(|c| {
            let cx_app: &App = cx;
            let this_edit = editing == Some((c.track, c.cue));
            let on = selected_cue == Some((c.track, c.cue));
            let (track, cue) = (c.track, c.cue);
            let tone = crate::ui::theme::tokens().clip(crate::ui::theme::ClipKind::Caption);
            let zone_w = (c.width / 3.).clamp(2., CUE_EDGE_W);
            let zone = move |edge: Edge, cx: &mut Context<Player>| {
                let grab = match edge {
                    Edge::Start => CueGrab::Start,
                    Edge::End => CueGrab::End,
                };
                let z = div()
                    .id(("bench-sub-cue-edge", (lane.ord * 1000 + idx) * 8192 + cue * 2 + usize::from(edge == Edge::End)))
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(px(zone_w))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(|s| s.bg(rgba(crate::ui::theme::with_alpha(0xffffff, 0x40))))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.start_cue_drag(lane, track, cue, grab, event.position.x, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_cue_menu(lane, track, cue, event.position, cx);
                        }),
                    );
                match edge {
                    Edge::Start => z.left_0(),
                    Edge::End => z.right_0(),
                }
            };
            div()
                .id(("bench-sub-cue", (lane.ord * 1000 + idx) * 8192 + cue))
                .absolute()
                .top(px(2.))
                .bottom(px(2.))
                .left(px(c.left))
                .w(px((c.width - 1.).max(1.)))
                .rounded(px(crate::layout::redesign::R_CLIP))
                .bg(rgb(tone.strip))
                .border_1()
                .border_color(match on {
                    true => rgb(crate::ui::theme::tokens().accent),
                    false => rgba(crate::ui::theme::with_alpha(0x000000, 0x59)),
                })
                .when(on, |d| d.border_2())
                .overflow_hidden()
                .cursor_pointer()
                .flex()
                .items_center()
                .px(px(6.))
                // A click picks this one caption; a press on the body arms the move.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.start_cue_drag(lane, track, cue, CueGrab::Body, event.position.x, cx);
                    }),
                )
                // A double-click opens the words for typing in place.
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    if event.click_count() >= 2 {
                        this.open_cue_edit(lane, track, cue, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_cue_menu(lane, track, cue, event.position, cx);
                    }),
                )
                .when(!c.picture && !this_edit && player.cue_menu.is_none(), |d| {
                    d.tooltip(crate::ui::widgets::tip_hover(
                        &ellipsize(&c.text, 60),
                        "",
                        Some("Double-click to edit"),
                    ))
                })
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .type_style(cue_style())
                        .text_color(rgb(tone.label))
                        .child(match (c.picture, this_edit) {
                            (_, true) => SharedString::default(),
                            (true, false) => "Picture".into(),
                            (false, false) => fitted(cx_app, &c.text, (c.width - 15.).max(0.)).into(),
                        }),
                )
                .child(zone(Edge::Start, cx))
                .child(zone(Edge::End, cx))
                // The editor and the menu hang from this block's corner, in a zero-size holder so
                // the block's own centring does not move them.
                .children(player.cue_edit.as_ref().filter(|_| this_edit).map(|edit| {
                    div().absolute().top_0().left_0().size_0().child(cue_edit_plate(edit))
                }))
        })
        .collect::<Vec<_>>();
    let body = div()
        .id(("bench-sub", lane.ord * 1000 + idx))
        .absolute()
        .top(px(2.))
        .bottom(px(2.))
        .left_0()
        .w(px((width - 1.).max(1.)))
        .overflow_hidden()
        .rounded(px(crate::layout::redesign::R_CLIP))
        .bg(rgb(crate::ui::theme::tokens().clip(crate::ui::theme::ClipKind::Caption).body))
        .when(dragging, |d| d.opacity(0.85))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.pick((lane, idx), event.modifiers.control, cx);
            }),
        )
        // A plain click collapses a multi-pick to this one, exactly as a
        // clip's own box does -- see the comment there.
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if !press_modifiers(event).control {
                this.select((lane, idx), cx);
            }
        }))
        // Same door a clip's box opens its menu by (DESIGN §9).
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.open_menu(lane, idx, event.position, cx);
            }),
        )
        .on_drag(
            SubDrag {
                lane,
                idx,
                sub: placed,
            },
            {
                let ghost = ghost.clone();
                let me = cx.entity();
                move |_, _, _, cx| {
                    // A fresh gesture inherits nothing: `Player::drag_lane`
                    // and `Player::drag_x` are the *last live sample* of the
                    // drag being made, and a drag whose first sample lands
                    // outside every row would otherwise be let go onto the
                    // lane the previous one ended over.
                    me.update(cx, |this, cx| {
                        // Whatever the last gesture left up (a shadow, a
                        // snap line) goes before this one's source is set.
                        this.end_drag_preview(cx);
                        this.drag_lane = None;
                        this.drag_x = None;
                        this.drag_src = Some((lane, idx));
                    });
                    cx.new(|_| Tip(ghost.clone()))
                }
            },
        )
        .children(hitmap::dynamic(
            move || {
                (
                    format!("subtitle.{}.{}.{}", lane.label(), lane.ord, idx),
                    "Timeline subtitle".into(),
                )
            },
            true,
        ))
        // The 1px inner border, black 45%.
        .child(
            div()
                .absolute()
                .size_full()
                .rounded(px(crate::layout::redesign::R_CLIP))
                .border_1()
                .border_color(rgba(crate::ui::theme::with_alpha(0x000000, 0x73))),
        )
        // The plate of the edge in the hand, as a clip's is (`clip_box`).
        .when_some(dragged_edge, |d, (trim, _, new)| {
            d.child(trim_plate(trim, new, player.fps))
        });
    div()
        .absolute()
        .top_0()
        .h_full()
        .left(px(left))
        .w(px(width))
        .child(body)
        .when(on, |d| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(-2.))
                    .w(px((width - 1.).max(1.) + 4.))
                    .rounded(px(crate::layout::redesign::R_CLIP + 2.))
                    .border_2()
                    .border_color(rgb(crate::ui::theme::tokens().accent)),
            )
        })
        .children(cue_blocks)
        .children(zones)
        .children(menu)
}

/// One caption's block on the lane: where it sits in the placement's body, and what it says.
struct CueBlock {
    left: f32,
    width: f32,
    track: usize,
    cue: usize,
    text: String,
    picture: bool,
}

/// How wide the strip at each end of a cue block is that trims it, in pixels.
const CUE_EDGE_W: f32 = 6.;

/// The widest the floating cue editor is drawn.
const CUE_EDIT_W: f32 = 300.;

/// The face a caption's words are set in on its block.
fn cue_style() -> type_scale::TypeStyle {
    type_scale::label(type_scale::CHORD_METADATA_MIN_PX, gpui::FontWeight::MEDIUM)
}

/// `text` cut with an ellipsis so it is no wider than `max_w` pixels in the block's face. Measured
/// with the font's own advances: a truncating text element keeps the width it was first measured at.
fn fitted(cx: &App, text: &str, max_w: f32) -> String {
    let style = cue_style();
    let system = cx.text_system();
    let id = system.resolve_font(&style.font);
    let adv = |ch: char| system.advance(id, style.size, ch).map_or(7., |s| f32::from(s.width));
    let ell = adv('\u{2026}');
    let mut w = 0.;
    let mut keep = 0;
    for (i, ch) in text.char_indices() {
        w += adv(ch);
        if w + ell <= max_w {
            keep = i + ch.len_utf8();
        }
    }
    match w <= max_w {
        true => text.to_string(),
        false if keep == 0 => String::new(),
        false => format!("{}\u{2026}", text[..keep].trim_end()),
    }
}

/// `text` cut to `max` characters with an ellipsis, for a tooltip's title.
fn ellipsize(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}\u{2026}", text[..cut].trim_end()),
        None => text.to_string(),
    }
}

/// The in-place cue editor (DESIGN §7): a small plate floating over the cue's block, as wide as
/// the words need up to [`CUE_EDIT_W`], with the words wrapped on as many lines as they take and
/// the caret on them. A quiet line under them says which keys end the edit.
fn cue_edit_plate(edit: &CueEdit) -> impl IntoElement + use<> {
    let t = crate::ui::theme::tokens();
    let words = match edit.caret_at_end {
        true => format!("{}\u{258f}", edit.text),
        false => format!("\u{258f}{}", edit.text),
    };
    deferred(
        anchored().snap_to_window_with_margin(px(8.)).child(
            crate::ui::kit::plate(6.)
                .id("cue-edit-plate")
                .w(px(CUE_EDIT_W))
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .occlude()
                // A press on the plate is the editor's own: it must not reach the
                // root's click-away, which would commit the words mid-typing.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .w_full()
                        .min_h(px(type_scale::BODY_PX * 1.4))
                        .type_style(type_scale::body())
                        .text_color(rgb(t.ink1))
                        .child(words),
                )
                .child(
                    div()
                        .type_style(type_scale::caption())
                        .text_color(rgb(match &edit.refusal {
                            Some(_) => t.warn,
                            None => t.ink3,
                        }))
                        .child(match &edit.refusal {
                            Some(why) => why.clone(),
                            None => "Enter saves, Shift+Enter starts a new line, Esc cancels".to_string(),
                        }),
                ),
        ),
    )
    .with_priority(2)
}

/// The caption menu: Edit text, Split at playhead, Merge with next, Delete and Style, on the one
/// popup plate. A row that cannot run says why on hover and ignores clicks.
fn cue_menu_plate(player: &Player, m: CueMenu, cx: &mut Context<Player>) -> impl IntoElement + use<> {
    use crate::ui::popup::{popup_row, popup_rule, popup_w};
    let split_why = player.cue_split_why(m);
    let merge_why = player.cue_merge_why(m);
    let picture = player
        .session
        .as_ref()
        .and_then(|s| s.subtitles().get(m.track))
        .and_then(|t| t.cues.get(m.cue))
        .is_some_and(|c| c.image.is_some());
    let edit_why = picture.then_some("A picture caption has no words to edit");
    let rows = [
        ("Edit text", edit_why),
        ("Split at playhead", split_why),
        ("Merge with next", merge_why),
        ("Delete", None),
        ("Style\u{2026}", None),
    ];
    let w = popup_w(rows.iter().map(|(l, _)| (*l, "")), false);
    let row = |id: usize, (label, why): (&'static str, Option<&'static str>), cx: &mut Context<Player>| {
        let r = popup_row(("cue-menu-row", id), label, "", None, why);
        match why {
            Some(_) => r,
            None => r.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                match id {
                    0 => {
                        this.cue_menu = None;
                        this.open_cue_edit(m.lane, m.track, m.cue, cx);
                    }
                    1 => this.split_cue_at_playhead(m, cx),
                    2 => this.merge_cue_with_next(m, cx),
                    3 => this.delete_cue_at(m, cx),
                    _ => {
                        this.cue_menu = None;
                        this.open_subtitle_style(cx);
                    }
                }
            })),
        }
    };
    deferred(
        anchored().position(m.at).snap_to_window_with_margin(px(8.)).child(
            crate::ui::kit::plate(6.)
                .id("cue-menu")
                .w(px(w))
                .p(px(crate::layout::MENU_PAD))
                .flex()
                .flex_col()
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.cue_menu = None;
                    cx.notify();
                }))
                .child(row(0, rows[0], cx))
                .child(row(1, rows[1], cx))
                .child(row(2, rows[2], cx))
                .child(row(3, rows[3], cx))
                .child(popup_rule())
                .child(row(4, rows[4], cx)),
        ),
    )
    .with_priority(2)
}
