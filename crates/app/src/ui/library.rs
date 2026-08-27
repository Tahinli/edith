//! The media library: the left panel's rows and the menu over them.

use crate::ui::type_scale::Typeset;
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
        // DESIGN §9: the same plate row treatment `Player::context_card`
        // draws with -- one function, so a copy-paste sibling menu cannot
        // drift from it.
        let row = move |n: usize| {
            div()
                .id(("library-menu", n))
                .flex()
                .min_h(px(MENU_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
                .map(|d| {
                    d.type_style(crate::ui::type_scale::label(
                        crate::ui::type_scale::LABEL_ROW_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(INK2()))
                })
        };
        let chord_style = |d: Div| -> Div {
            d.map(|d| {
                d.type_style(crate::ui::type_scale::mono(
                    crate::ui::type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
            })
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        // What every item of this menu is answered from, read once.
        let ctx = self.row_ctx(&path, menu.stream);
        if menu.details {
            // What the library knows about this row and nothing probed for the
            // card: the streams table is filled once per file at import.
            let info = self
                .streams
                .get(&path)
                .and_then(|of_file| of_file.iter().find(|s| s.index == menu.stream));
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
            for (label, value) in [
                ("File", file_name(&path)),
                ("Path", path.display().to_string()),
                match image {
                    true => ("Picture", kind),
                    false => (
                        "Audio",
                        info.map_or_else(|| "no track of its own".to_string(), stream_detail),
                    ),
                },
                (
                    "Bitrate",
                    bitrate_detail(
                        self.bitrates.get(&path).copied().flatten(),
                        self.streams.get(&path).map_or(0, Vec::len),
                    ),
                ),
                match image {
                    true => (
                        "Longest hold",
                        timecode(f64::from(frames) / self.fps, self.fps),
                    ),
                    false => ("Length", timecode(f64::from(frames) / self.fps, self.fps)),
                },
                ("On the timeline", format!("{placed} clips")),
            ] {
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(
                            chord_style(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(rgb(FG_SECONDARY())),
                            )
                            .child(value),
                        )
                        .into_any_element(),
                );
            }
        } else {
            // The oracle's list, exactly as the clip menu takes its rows from
            // `menu_items`: an item that means nothing for the file that was
            // right-clicked is not a row, and one this moment refuses is drawn
            // dimmed and says why in place of its hint.
            for item in row_items(ctx) {
                // DESIGN §9: destructive verbs sit below a rule line. One
                // hairline, drawn once, above whichever of the two removes
                // this row context offers.
                if matches!(item, RowItem::Remove | RowItem::RemoveWithClips) {
                    rows.push(
                        div()
                            .flex_none()
                            .my(px(3.))
                            .h(px(1.))
                            .bg(rgb(DARK_HAIRLINE()))
                            .into_any_element(),
                    );
                }
                let refusal = row_enable(item, ctx);
                let enabled = refusal.yes();
                rows.push(
                    row(rows.len())
                        .child(item.label())
                        .child(
                            chord_style(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(rgb(FG_SECONDARY())),
                            )
                            .child(refusal.why().unwrap_or_else(|| item.hint())),
                        )
                        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.act_on_row(item, cx);
                                }))
                        })
                        .into_any_element(),
                );
            }
        }
        // Placed at the pointer, clamped only to the window's own bounds --
        // standard context-menu behavior. Unlike the clip menu, this menu
        // mounts on the root over the dock (no picture behind it to protect),
        // so it does not go through `menu_floor`'s picture-floor clamp; doing
        // so pulled a right-click high in the dock down to the picture floor,
        // teleporting the menu away from the pointer that opened it.
        let list_h = menu_rows_h(rows.len(), viewport);
        let (x, y) = menu_at(menu.at, viewport, MENU_PAD * 2. + list_h);
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
                    menu_plate("library-menu-card", x, y)
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
