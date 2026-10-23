//! The undo history card: every named step the project can undo or redo, newest at the top,
//! with the step the project stands on marked. A click on a row moves the project to that point,
//! through the same door Undo and Redo use, so each jump is one step of the same history.

use super::common::*;
use crate::*;

impl Player {
    /// Opens the history card in the Clip tab, closing the others as the markers card does.
    pub(crate) fn open_history(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.close_other_cards();
        self.history_open = true;
        self.context_menu = None;
        cx.notify();
    }

    /// Moves the project `steps` from where it stands: negative back, positive forward, through
    /// [`PlaybackSession::jump_to`]. Clears the selection the way Undo and Redo do.
    pub(crate) fn jump_history(&mut self, steps: isize, cx: &mut Context<Self>) {
        let moved = self.step_history(|s| s.jump_to(steps) > 0);
        if moved {
            self.selected.clear();
            self.drop_sel_cue();
            self.cue_menu = None;
            self.mark_dirty();
            self.settle_saved_point();
        }
        cx.notify();
    }

    /// The history card. Rows run from the newest step the project can redo down to the state it
    /// was opened in. The row for the current state carries the mark; rows above it are steps
    /// that were undone, and a click on any row goes to that point.
    pub(crate) fn history_card(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.history_open {
            return None;
        }
        let (undo, redo) = match self.session.as_ref() {
            Some(s) => (s.undo_labels(), s.redo_labels()),
            None => (Vec::new(), Vec::new()),
        };
        let (n, r) = (undo.len() as isize, redo.len() as isize);
        let rows: Vec<_> = (-n..=r)
            .rev()
            .map(|k| {
                // The step that brought the project to offset `k` from where it stands: a redo
                // step above the mark, the undo step under it below, and none for the state opened.
                let step = match k {
                    k if k > 0 => redo.get((r - k) as usize),
                    k => undo.get((n - 1 + k) as usize).filter(|_| n - 1 + k >= 0),
                };
                let text: SharedString = match step {
                    Some(Some(name)) => name.clone().into(),
                    Some(None) => "Edit".into(),
                    None => "Opened".into(),
                };
                let current = k == 0;
                let undone = k > 0;
                div()
                    .id(SharedString::from(format!("history-row-{k}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(KEYS_ROW_H))
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(current, |d| d.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.jump_history(k, cx)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(10.))
                            .h(px(10.))
                            .rounded(px(5.))
                            .when(current, |d| d.bg(rgb(tokens().accent)))
                            .when(!current, |d| d.border_1().border_color(rgb(tokens().ink4))),
                    )
                    .child(dark_row_label(text, current).text_color(rgb(match (current, undone) {
                        (true, _) => tokens().ink1,
                        (false, true) => tokens().ink4,
                        _ => tokens().ink2,
                    })))
            })
            .collect();
        let help_text: SharedString = "Newest at the top. The marked step is where the project stands. Click a step to go to it: steps above the mark are undone, and a click on one brings them back.".into();
        let tail_text: SharedString = format!(
            "{} step{} to undo, {} to redo",
            n,
            if n == 1 { "" } else { "s" },
            r
        )
        .into();
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
                        .child(dark_card_head("History", None, Some(help_text), Some(self.card_maximized), cx))
                        .child(
                            div()
                                .id("history-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(dark_help(tail_text)),
                ),
        )
    }
}
