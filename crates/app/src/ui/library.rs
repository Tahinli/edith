//! The media library: the left panel's rows and the menu over them.

use crate::ui::popup::{popup_plate, popup_row, popup_row_icon, popup_rule, popup_w};
use crate::ui::widgets::*;
use crate::*;

impl Player {
    /// The menu a right-click on a library row opens: what can be done with the
    /// *file* rather than with a clip of it, and a turn-over side saying what
    /// that file is. Built like [`Player::context_card`] down to the scrim, the
    /// row height and the clamp, because it is the same menu on the other panel
    /// -- a click away or any stroke closes it.
    pub(crate) fn library_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.library_menu.clone()?;
        let path = menu.path.clone();
        let mut measure: Vec<(String, String)> = Vec::new();
        let mut rows: Vec<AnyElement> = Vec::new();
        // What every item of this menu is answered from, read once.
        let ctx = self.row_ctx(&path, menu.stream);
        if menu.details {
            // What the library knows about this file: its header, read once at
            // import, and the frames for the length line. Every track of the
            // file is listed, not only this row's -- a plate describes the file.
            let frames = self
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(&path));
            // How many clips play from this exact row -- the number that
            // decides whether Remove is refused, so the card answers the
            // question the refusal would otherwise raise.
            let placed = ctx.placed;
            // A still is described by what it has -- a picture, a size, and a
            // longest it may be held for -- where a media file is described by
            // its streams and its length. Same card, the rows that mean
            // something for this kind of source.
            let image = engine::is_image(&path);
            let kind = match self.sizes.get(&path).copied().flatten() {
                Some((w, h)) => format!("still image · {w}x{h}"),
                None => "still image".to_string(),
            };
            let streams = self.streams.get(&path).map_or(&[][..], Vec::as_slice);
            let mut lines = file_properties(
                &path,
                self.facts.get(&path).and_then(Option::as_ref),
                streams,
                self.bitrates.get(&path).copied().flatten(),
            );
            if image {
                lines.push(("Picture".to_string(), kind));
            }
            // The path is cut in the middle (both ends say where it is and what it is);
            // the whole of it is the plate's tooltip and the Copy button.
            for (label, value) in &mut lines {
                if label == "Path" {
                    *value = crate::recent::middle(value, 38);
                }
                if label == "Container" {
                    *value = container_name(value);
                }
            }
            let secs = f64::from(frames) / self.fps;
            lines.push(match image {
                // A still has no length of its own: how long it may be held, in words.
                true => ("Longest hold".to_string(), hold_words(secs)),
                false => ("Length".to_string(), timecode(secs, self.fps)),
            });
            lines.push((
                "On the timeline".to_string(),
                format!("{placed} clip{}", if placed == 1 { "" } else { "s" }),
            ));
            for (label, value) in lines {
                measure.push((label.clone(), value.clone()));
                rows.push(
                    popup_row(("library-menu", rows.len()), label, value, None, None)
                        .cursor_default()
                        .into_any_element(),
                );
            }
            let text = path.display().to_string();
            rows.push(
                div()
                    .flex_none()
                    .p(px(4.))
                    .child(
                        crate::ui::kit::Button::new("properties-copy-path")
                            .label("Copy path")
                            .icon(crate::ui::icons::Icon::Copy)
                            .style(crate::ui::kit::ButtonStyle::Secondary)
                            .size(crate::ui::kit::ButtonSize::Small)
                            .build()
                            .w_full()
                            .tooltip(crate::ui::widgets::tip_hover("Copy the full path of this file", "", None))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                                this.notify_user("Path copied".into());
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        } else {
            // The oracle's list, exactly as the clip menu takes its rows from
            // `menu_items`: an item that means nothing for the file that was
            // right-clicked is not a row, and one this moment refuses is drawn
            // dimmed and says why in place of its hint.
            // Whether the rule line above the destructive rows has been drawn.
            let mut ruled = false;
            for item in row_items(ctx) {
                // DESIGN §9: destructive verbs sit below a rule line, drawn once
                // before the first of them -- the same seam the clip menu draws
                // (`DARK_SEAM`), so the two menus cannot disagree about it.
                if item.destructive() && !rows.is_empty() && !ruled {
                    ruled = true;
                    rows.push(popup_rule());
                }
                let refusal = row_enable(item, ctx);
                let enabled = refusal.yes();
                let hint = match refusal.why() {
                    Some(_) => String::new(),
                    None => item.hint().to_string(),
                };
                measure.push((item.label_in(ctx).to_string(), hint.clone()));
                rows.push(
                    popup_row_icon(
                        ("library-menu", rows.len()),
                        item.label_in(ctx),
                        hint,
                        Some(false),
                        Some(item.icon()),
                        refusal.why(),
                    )
                        // A destructive row answers a hover in the error token.
                        .when(!enabled && item.destructive(), |d| {
                            d.hover(|s| s.text_color(rgb(STATUS_ERROR())))
                        })
                        .when(enabled, |d| {
                            let destructive = item.destructive();
                            d.cursor_pointer()
                                .hover(move |s| {
                                    let s = s.bg(rgb(BG_HOVER()));
                                    if destructive {
                                        s.text_color(rgb(STATUS_ERROR()))
                                    } else {
                                        s
                                    }
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.act_on_row(item, cx);
                                }))
                        })
                        .into_any_element(),
                );
            }
        }
        // Opens at the pointer, top-left on it, like every context menu. When
        // the plate would not fit whole below the pointer it flips up so its
        // bottom edge sits on the pointer; `menu_at_w` then keeps it inside
        // all four window edges.
        let list_h = menu_rows_h(rows.len(), viewport);
        let h = MENU_PAD * 2. + list_h;
        // The item rows reserve the icon column, so the plate is measured with it.
        let pw = popup_w(measure.iter().map(|(l, h)| (l.as_str(), h.as_str())), !menu.details);
        // Properties sits beside the row it describes, top-aligned with it: right of the row,
        // or left of it when the window has no room on the right.
        let beside = menu.details.then(|| crate::ui::anchor::row_of(&path, menu.stream)).flatten().map(|row| {
            let right = f32::from(row.origin.x + row.size.width) + 4.;
            let x = if right + pw + MENU_EDGE > f32::from(viewport.width) {
                (f32::from(row.origin.x) - pw - 4.).max(0.)
            } else {
                right
            };
            point(px(x), row.origin.y)
        });
        let at = if let Some(beside) = beside {
            beside
        } else if f32::from(menu.at.y) + h + MENU_EDGE > f32::from(viewport.height) {
            point(menu.at.x, px((f32::from(menu.at.y) - h).max(0.)))
        } else {
            menu.at
        };
        let (x, y) = menu_at_w(at, viewport, pw, h);
        let full: SharedString = path.display().to_string().into();
        Some(
            scrim()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.library_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.library_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    popup_plate("library-menu-card", x, y, pw)
                        // Painted after the scrim, so this listener runs first
                        // and a press meant for an item never closes the menu
                        // out from under its own click (`context_card`).
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .when(menu.details, |d| {
                            d.tooltip(move |_, cx| cx.new(|_| Tip(full.clone())).into())
                        })
                        // Scrolls where the window has no room for the list,
                        // like the clip menu's -- an item hanging off the bottom
                        // edge is an item nobody can click.
                        .child(
                            div()
                                .id("library-menu-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        )
    }
}

/// A container as its name reads: `mp4` is "MP4", `mov` is "QuickTime". A name this
/// table does not know is shown with its first letter capitalised.
fn container_name(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "mp4" => "MP4".to_string(),
        "mov" | "quicktime" => "QuickTime".to_string(),
        "m4a" => "M4A".to_string(),
        "m4v" => "M4V".to_string(),
        "3gp" => "3GP".to_string(),
        "mkv" | "matroska" => "Matroska".to_string(),
        "webm" => "WebM".to_string(),
        "ogg" | "ogv" => "Ogg".to_string(),
        "wav" => "WAV".to_string(),
        "avi" => "AVI".to_string(),
        "flac" => "FLAC".to_string(),
        "mp3" => "MP3".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// A duration in plain words: "10 s", "10 min", "1 min 30 s".
fn hold_words(secs: f64) -> String {
    let secs = secs.max(0.).round() as u64;
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}
