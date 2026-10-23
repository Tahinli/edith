//! The markers card: the project's comment markers as rows -- colour, time, name, note --
//! each part a door to the thing it shows (recolour, go there, rename), and a button that
//! drops a new one at the playhead.

use super::common::*;
use crate::ui::icons::Icon;
use crate::ui::theme::marker_rgb;
use crate::ui::widgets::MarkerField;
use crate::*;

/// The time a marker row shows: where it starts, and for a range how long it runs.
pub(crate) fn marker_time_text(frame: u32, length: u32, fps: f64) -> String {
    match length {
        0 => frames_timecode(frame, fps),
        n => format!("{} +{}", frames_timecode(frame, fps), frames_timecode(n, fps)),
    }
}

impl Player {
    /// The markers card. Rows are the session's markers in frame order; the selected row
    /// (`marker_sel`) is the one the arrow keys moved to and the one enter renames. A
    /// name or note being typed shows its text field in place of the old words.
    pub(crate) fn markers_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.markers_open {
            return None;
        }
        let markers = self.session.as_ref().map(|s| s.markers().to_vec()).unwrap_or_default();
        let fps = self.fps;
        let editing = self.marker_edit.clone();
        let rows: Vec<_> = markers
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let picked = self.marker_sel == i;
                let words = |field: MarkerField, shown: &str, placeholder: &str| -> SharedString {
                    match &editing {
                        Some(e) if e.idx == i && e.field == field => format!("{}▏", e.text).into(),
                        _ if shown.is_empty() => placeholder.to_string().into(),
                        _ => shown.to_string().into(),
                    }
                };
                let typing = |field: MarkerField| editing.as_ref().is_some_and(|e| e.idx == i && e.field == field);
                let tip = match (m.name.is_empty(), m.note.is_empty()) {
                    (_, true) => m.name.clone(),
                    (true, false) => m.note.replace('\n', " "),
                    (false, false) => format!("{} \u{2014} {}", m.name, m.note.replace('\n', " ")),
                };
                // The row is one tidy line, dot, time, name: a click anywhere on it takes the
                // playhead there. The note shows on hover, and in full under the picked row.
                let line = div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(KEYS_ROW_H))
                    .child(
                        div()
                            .id(("markers-color", i))
                            .flex_none()
                            .w(px(HIT_MIN / 2.))
                            .h(px(HIT_MIN / 2.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, swallow)
                            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                                this.marker_sel = i;
                                this.picker_under(Pick::MarkerColor(i), ev.position(), cx);
                            }))
                            .tooltip(crate::ui::widgets::overlay_tip_hover("Marker colour", "", None))
                            .child(div().w(px(10.)).h(px(10.)).rounded(px(5.)).bg(rgb(marker_rgb(m.color)))),
                    )
                    .child(div().flex_none().child(dark_row_value(marker_time_text(m.frame, m.length, fps), picked)))
                    .child(
                        div()
                            .id(("markers-name", i))
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .whitespace_nowrap()
                            .tooltip(crate::ui::widgets::overlay_tip_hover(&tip, "", None))
                            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                                if ev.click_count() >= 2 {
                                    this.begin_marker_edit(i, MarkerField::Name, cx);
                                }
                            }))
                            // A name is retyped on its flag on the ruler, the one field for it, so
                            // the row keeps the name's words while that happens.
                            .child(dark_row_label(gpui::SharedString::from(if m.name.is_empty() { "Marker".to_string() } else { m.name.clone() }), picked)),
                    )
                    .child(
                        div().on_mouse_down(MouseButton::Left, swallow).child(crate::ui::kit::icon_ghost(
                            "markers-remove",
                            Icon::X,
                            "Remove marker",
                            None,
                            {
                                let me = cx.entity();
                                move |_, app| me.update(app, |this, cx| this.remove_marker_at(i, cx))
                            },
                        )),
                    );
                // The note, whole and wrapped, under the picked row (or while it is typed).
                let note = (picked || typing(MarkerField::Note)).then(|| {
                    div()
                        .id(("markers-note", i))
                        .pl(px(HIT_MIN / 2. + 8.))
                        .pb(px(4.))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, swallow)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.begin_marker_edit(i, MarkerField::Note, cx);
                        }))
                        .child(
                            dark_row_label(words(MarkerField::Note, &m.note.replace('\n', " "), "Add a note"), typing(MarkerField::Note))
                                .text_color(rgb(match (typing(MarkerField::Note), m.note.is_empty()) {
                                    (true, _) => tokens().ink1,
                                    (false, true) => tokens().ink4,
                                    _ => tokens().ink3,
                                })),
                        )
                });
                div()
                    .id(("markers-row", i))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(picked, |d| d.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.seek_marker(i, cx)))
                    .child(line)
                    .children(note)
                    .into_any_element()
            })
            .collect();
        let add_chord = self.keymap.display(ActionId::AddMarker);
        let help_text: SharedString = "Click a row to go there, the dot to recolour it, double-click the name or click the note to edit it. Up and down select, Enter renames, Shift+Enter edits the note.".into();
        let tail_text: SharedString = match markers.len() {
            0 => "No markers yet".into(),
            n => format!("{n} marker{}", if n == 1 { "" } else { "s" }).into(),
        };
        Some(
            self.card_outer(viewport, cx)
                .child(
                    div()
                        .w_full()
                        .max_w(px(card_max_w(COLOR_W, self.card_maximized, f32::from(viewport.width))))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .max_h(px(360. - 24.))
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
.map(|d| self.card_surface(d))
                        .child(dark_card_head(
                            "Markers in this sequence",
                            Some(format!("{}", markers.len()).into()),
                            Some(help_text),
                            Some(self.card_maximized),
                            cx,
                        ))
                        .child(
                            div()
                                .id("markers-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(div().flex().child(dark_ghost_button(
                            "markers-add",
                            "Add at playhead",
                            &add_chord,
                            false,
                            cx.listener(|this, _: &ClickEvent, _, cx| this.add_marker_here(cx)),
                        )))
                        .child(dark_help(tail_text)),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_shows_its_length_after_its_start() {
        assert_eq!(marker_time_text(0, 0, 25.), frames_timecode(0, 25.));
        let range = marker_time_text(50, 25, 25.);
        assert!(range.starts_with(&frames_timecode(50, 25.)) && range.contains('+'), "{range}");
    }
}
