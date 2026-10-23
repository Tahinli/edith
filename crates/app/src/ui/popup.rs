//! The one popup look: the menu bar's dropdowns, the clip and media
//! right-click menus and every other hanging list draw their plate, rows and
//! rule lines through here, so surface, border, radius, padding, row height,
//! type, hover and the right-aligned shortcut column cannot drift apart.
//!
//! The plate fits its content: [`popup_w`] measures the widest label beside
//! the widest shortcut, so nothing is ever cut off.

use crate::layout::{MENU_PAD, MENU_ROW_H, MENU_W};
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::overlay_tip_hover;
use crate::*;

/// The widest a plate grows. A longer value (a file path in a details side)
/// truncates in its own column; the label never does.
pub(crate) const POPUP_MAX_W: f32 = 460.;
/// The gap between the label and the shortcut column.
const COL_GAP: f32 = 24.;
/// Each row's side padding.
const ROW_PX: f32 = 8.;
/// The column a check mark takes on a plate whose rows can be switches.
pub(crate) const CHECK_W: f32 = 16.;

/// An upper bound on the width of `text` set at `size_px`. Archivo averages
/// about 0.55 em a glyph and the mono face 0.6 em; the factors here sit a
/// little above both, so a plate sized by this never cuts a word.
pub(crate) fn text_w(text: &str, size_px: f32, mono: bool) -> f32 {
    let em = if mono { 0.62 } else { 0.58 };
    text.chars().count() as f32 * size_px * em
}

/// The plate width for rows of `(label, shortcut)`: the widest label (measured
/// at the row's `body_strong`), the gap, the widest shortcut, the paddings, plus
/// the icon / check column (16px and its gap) when `checks`. Pass `checks: true`
/// whenever any row is built with `checked: Some(..)` ([`popup_row_icon`]) -- an
/// icon row counts -- or its label would be squeezed into a wrap.
pub(crate) fn popup_w<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str)>, checks: bool) -> f32 {
    let (mut label, mut hint) = (0f32, 0f32);
    for (l, h) in rows {
        // Rows set the label in `body_strong` (medium weight), a touch wider than regular.
        label = label.max(text_w(l, type_scale::BODY_PX, false) * 1.06);
        hint = hint.max(text_w(h, type_scale::CAPTION_PX, false));
    }
    let gap = if hint > 0. { COL_GAP } else { 0. };
    let check = if checks { CHECK_W + 8. } else { 0. };
    // 2 for the plate's border.
    let w = MENU_PAD * 2. + 2. + ROW_PX * 2. + check + label + gap + hint;
    w.clamp(MENU_W * 0.75, POPUP_MAX_W)
}

/// The plate: absolute at `(x, y)`, `w` wide.
pub(crate) fn popup_plate(id: &'static str, x: f32, y: f32, w: f32) -> Stateful<Div> {
    // Occluding: without it the click-away backdrop under a dropdown counted as
    // hovered too and closed the menu on the press, before a row's click landed.
    crate::interact::menu_plate(id, x, y).w(px(w)).occlude()
}

/// The rule line between groups, the same everywhere.
pub(crate) fn popup_rule() -> AnyElement {
    div()
        .flex_none()
        .my(px(4.))
        .h(px(1.))
        .bg(rgba(crate::ui::theme::tokens().line))
        .into_any_element()
}

/// One row: the label left, the shortcut right-aligned in mono `ink3`, and
/// nothing at all where there is no shortcut. `checked` is `Some` on a plate
/// that has switches (the check column is then reserved on every row, so the
/// labels line up). A disabled row is dimmed and says why on hover, never as
/// text in the row. The caller adds the click and the hover colour of an
/// enabled row ([`popup_hover`]).
pub(crate) fn popup_row(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    checked: Option<bool>,
    why: Option<&str>,
) -> Stateful<Div> {
    popup_row_icon(id, label, hint, checked, None, why)
}

/// [`popup_row`] with the 16px icon column (REDESIGN §3.1): `checked` opens the
/// column (a tick when `Some(true)`), and `icon` fills it for rows that are not
/// switches. Rows with neither leave the column blank so labels line up.
pub(crate) fn popup_row_icon(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    checked: Option<bool>,
    icon: Option<crate::ui::icons::Icon>,
    why: Option<&str>,
) -> Stateful<Div> {
    use crate::ui::icons::{Icon, IconSize};
    let t = crate::ui::theme::tokens();
    let label: SharedString = label.into();
    let hint: SharedString = hint.into();
    let disabled = why.is_some();
    let ink = if disabled { t.ink4 } else { t.ink1 };
    let mut row = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(COL_GAP))
        .h(px(MENU_ROW_H))
        .px(px(ROW_PX))
        .rounded(px(4.))
        .type_style(type_scale::body_strong())
        .text_color(rgb(ink))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w(px(0.))
                .flex_none()
                .whitespace_nowrap()
                .when_some(checked, |d, on| {
                    d.child(
                        div().flex_none().w(px(CHECK_W)).h(px(CHECK_W)).children(
                            match (on, icon) {
                                (true, _) => Some(Icon::Check.el(IconSize::Inline, t.accent)),
                                (false, Some(i)) => {
                                    Some(i.el(IconSize::Inline, if disabled { t.ink4 } else { t.ink2 }))
                                }
                                (false, None) => None,
                            },
                        ),
                    )
                })
                .child(div().min_w(px(0.)).whitespace_nowrap().child(label.clone())),
        );
    if !hint.is_empty() {
        row = row.child(
            div()
                .flex_none()
                .max_w(px(300.))
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(if disabled { t.ink4 } else { t.ink3 }))
                .child(hint),
        );
    }
    match why {
        Some(why) => row
            .cursor_not_allowed()
            .tooltip(overlay_tip_hover(&label, "", Some(why))),
        None => row.cursor_pointer().when(!disabled, popup_hover),
    }
}

pub(crate) fn shortcut(keymap: &crate::keymap::Keymap, action: ActionId) -> String {
    match keymap.display(action) {
        s if s == "unbound" || s == "--" => String::new(),
        // The column is narrow; the long spelling is in the shortcuts list.
        s if s == "Delete or Backspace" => "Del".into(),
        // A menu column has room for one key; the sheet lists every stroke.
        s => s.split(" or ").next().unwrap_or(&s).to_string(),
    }
}

/// The hover colour of a live row.
pub(crate) fn popup_hover(row: Stateful<Div>) -> Stateful<Div> {
    row.hover(|s| s.bg(rgb(BG_HOVER())))
}

/// The About box's width. Its height hugs its content.
pub(crate) const ABOUT_W: f32 = 360.;

impl Player {
    /// The modal sheet that is up, if any, in the shared dialog shell: Help,
    /// About edith (the app mark, the name, the version and the licence line,
    /// centred), or the shortcuts sheet. Click away, Esc or Close shuts it.
    pub(crate) fn about_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let area_h = crate::ui::stance::below_picture_floor(
            f32::from(viewport.height),
            self.split_px(Split::Bench, viewport),
        );
        let bottom = (f32::from(viewport.height) - area_h).max(0.);
        if self.quit_ask {
            return Some(self.unsaved_dialog(bottom, cx));
        }
        if let Some(why) = self.recovery_failure.clone() {
            return Some(self.recovery_failed_dialog(&why, bottom, cx));
        }
        if self.save_ask {
            return Some(self.overwrite_dialog(bottom, cx));
        }
        if self.keys_open && !self.about_open {
            return Some(crate::ui::dock_stance::keys_sheet(self, viewport, 0., cx).into_any_element());
        }
        if !self.about_open {
            return None;
        }
        use crate::ui::kit;
        let t = crate::ui::theme::tokens();
        Some(
            kit::dialog_layer(bottom)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.about_open = false;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    kit::dialog("about-card", ABOUT_W)
                        .items_center()
                        .gap(px(6.))
                        .child(
                            // The app mark: the menu bar's "E" on the accent square.
                            div()
                                .flex_none()
                                .size(px(56.))
                                .mb(px(6.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(12.))
                                .bg(rgb(t.accent_fill))
                                .type_style(type_scale::label(28., gpui::FontWeight::SEMIBOLD))
                                .text_color(rgb(t.on_accent))
                                .child("E"),
                        )
                        .child(div().type_style(type_scale::title()).text_color(rgb(t.ink1)).child("edith"))
                        .child(
                            div()
                                .type_style(type_scale::caption())
                                .text_color(rgb(t.ink3))
                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                        )
                        .child(
                            div()
                                .mt(px(6.))
                                .type_style(type_scale::body())
                                .text_color(rgb(t.ink2))
                                .child("A keyboard-first video editor."),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .type_style(type_scale::caption())
                                .text_color(rgb(t.ink3))
                                .child("Icons from Lucide (ISC).")
                                .child("Fonts under the SIL Open Font License."),
                        )
                        ,
                )
                .into_any_element(),
        )
    }
}

impl Player {
    /// The window's close request over unsaved work, in the dialog shell. The
    /// title and the body are read off the state as it is drawn: a question
    /// about edits says so, a question about exports says so, and a question
    /// about neither (an export finished while the dialog was up) names nothing
    /// it does not have. Save (primary, `Enter`), Don't save (`D`), Stay (`Esc`).
    /// The keys are the root handler's (`quit_ask` owns the keyboard); the
    /// buttons are the pointer's door to the same answers.
    fn unsaved_dialog(&self, bottom: f32, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::kit::{self, Button, ButtonStyle};
        let edits = self.close_dirty();
        let exports = self.queue.unfinished();
        let queue = self.queue_quit_note();
        let name = crate::files::file_name(&self.project_path);
        let (title, text) = match (edits, exports > 0) {
            (true, _) => {
                let title = match name.is_empty() {
                    true => "Save changes before closing?".to_string(),
                    false => format!("Save changes to {name}?"),
                };
                let body = match name.is_empty() {
                    true => "This project has changes that are not saved yet.".to_string(),
                    false => format!("{name} has changes that are not saved yet."),
                };
                (title, [Some(body), queue].into_iter().flatten().collect::<Vec<_>>().join(" "))
            }
            (false, true) => ("Close with exports unfinished?".to_string(), queue.unwrap_or_default()),
            (false, false) => ("Close edith?".to_string(), "Nothing here is unsaved.".to_string()),
        };
        let actions = kit::dialog_actions()
            .child(
                Button::new("quit-stay")
                    .label("Stay")
                    .build()
                    .tooltip(crate::ui::widgets::overlay_tip_hover("Keep editing", "Esc", None))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.quit_ask = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("quit-discard")
                    .label(if edits { "Don't save" } else { "Close anyway" })
                    .build()
                    .tooltip(crate::ui::widgets::overlay_tip_hover(
                        if edits { "Close without saving" } else { "Close and stop the exports" },
                        "D",
                        None,
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, _| {
                        // Only the edits are let go. Exports and a pending recovery
                        // are not answered by Don't save.
                        if this.close_dirty() {
                            this.discard_unsaved_edits();
                        }
                        this.queue.cancel_all();
                        window.remove_window();
                    })),
            )
            .children(edits.then(|| {
                Button::new("quit-save")
                    .label("Save")
                    .style(ButtonStyle::Primary)
                    .build()
                    .tooltip(crate::ui::widgets::overlay_tip_hover("Save and close", "Enter", None))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        // A save that failed keeps the window open: the notice says why.
                        if this.save_project(cx) {
                            window.remove_window();
                        }
                    }))
            }));
        kit::dialog_layer(bottom)
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
            .child(
                kit::dialog("unsaved-dialog", 400.)
                    .child(kit::dialog_title(title))
                    .child(kit::dialog_text(text))
                    .child(actions),
            )
            .into_any_element()
    }

    /// Recover could not bring the unsaved edits back: the plain reason, and that the
    /// saved version is open. OK closes it; the autosave is still on disk.
    fn recovery_failed_dialog(&self, why: &str, bottom: f32, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::kit::{self, Button, ButtonStyle};
        kit::dialog_layer(bottom)
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
            .child(
                kit::dialog("recovery-failed-dialog", 440.)
                    .child(kit::dialog_title("Couldn't bring back the unsaved edits"))
                    .child(kit::dialog_text(why.to_string()))
                    .child(
                        kit::dialog_actions().child(
                            Button::new("recovery-failed-ok")
                                .label("OK")
                                .style(ButtonStyle::Primary)
                                .build()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.recovery_failure = None;
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// Saving over a project file this window never opened or wrote: Overwrite
    /// (`Enter`) or Keep the file (`Esc`).
    fn overwrite_dialog(&self, bottom: f32, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::kit::{self, Button, ButtonStyle};
        kit::dialog_layer(bottom)
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
            .child(
                kit::dialog("overwrite-dialog", 400.)
                    .child(kit::dialog_title("Replace the existing file?"))
                    .child(kit::dialog_text(format!(
                        "A file named {} already exists in this folder. Replace it?",
                        crate::files::file_name(&self.project_path)
                    )))
                    .child(
                        kit::dialog_actions()
                            .child(
                                Button::new("overwrite-keep")
                                    .label("Keep the file")
                                    .build()
                                    .tooltip(crate::ui::widgets::overlay_tip_hover("Do not save", "Esc", None))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.save_ask = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("overwrite-go")
                                    .label("Replace")
                                    .style(ButtonStyle::Primary)
                                    .build()
                                    .tooltip(crate::ui::widgets::overlay_tip_hover("Replace the file", "Enter", None))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.save_project(cx);
                                        this.save_ask = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plate_is_at_least_as_wide_as_its_widest_row() {
        let rows = [
            ("Reload project from disk", "Ctrl+Shift+R"),
            ("Paste (global)", "Ctrl+V"),
            ("Regroup", ""),
        ];
        let w = popup_w(rows, false);
        for (l, h) in rows {
            let need = text_w(l, type_scale::LABEL_ROW_PX, false)
                + text_w(h, type_scale::CHORD_METADATA_MIN_PX, true);
            assert!(w >= need + MENU_PAD * 2., "{l}: {w} < {need}");
        }
    }

    #[test]
    fn a_short_menu_keeps_a_floor_width_and_a_long_one_a_ceiling() {
        assert!(popup_w([("Cut", "X")], false) >= MENU_W * 0.75);
        assert_eq!(popup_w([(&"x".repeat(200)[..], "")], false), POPUP_MAX_W);
    }

    #[test]
    fn a_context_menu_opens_with_its_corner_on_the_pointer_clamped_to_the_window() {
        let view = size(px(1000.), px(700.));
        let (x, y) = crate::oracle::menu_at_w(point(px(300.), px(200.)), view, 240., 150.);
        assert_eq!((x, y), (300., 200.));
        let (x, y) = crate::oracle::menu_at_w(point(px(990.), px(690.)), view, 240., 150.);
        assert!(x + 240. <= 1000. && y + 150. <= 700.);
    }

    #[test]
    fn no_menu_row_prints_unbound_and_help_has_about() {
        let keymap = crate::keymap::Keymap::defaults();
        for title in crate::ui::menubar::Title::ALL {
            for entry in title.entries() {
                use crate::ui::menubar::Entry::*;
                if let Act(a) | Switch(a) = entry {
                    let hint = shortcut(&keymap, a);
                    assert!(!hint.contains("unbound"), "{a:?}");
                }
            }
        }
        let help = crate::ui::menubar::Title::Help.entries();
        assert!(help.contains(&crate::ui::menubar::Entry::Act(ActionId::About)));
        assert_eq!(ActionId::About.label(), "About edith…");
    }

    /// The clip menu's top level is short (the card openers live in the Adjust
    /// submenu) and, placed the way `context_card` places it, fits a 1280x720
    /// window whatever y the click was at.
    #[test]
    fn the_clip_menu_fits_a_720_window_at_any_click_y() {
        let all: Vec<ActionId> = crate::menus::MENU_ITEMS.to_vec();
        let top = all
            .iter()
            .filter(|a| !crate::menus::adjust_item(**a) && !crate::menus::compound_item(**a))
            .count()
            + 3; // Adjust, Compound and Properties
        assert!(top <= 19, "{top} rows");
        let view = size(px(1280.), px(720.));
        let list_h = crate::oracle::menu_rows_h(top + 1, view);
        let h = MENU_PAD * 2. + list_h;
        for y in (0..=720).step_by(10) {
            let at = point(px(300.), px(y as f32));
            let at = crate::ui::stance::menu_above(at, view, 0., h);
            let (_, top_y) = crate::oracle::menu_at_w(at, view, 260., h);
            assert!(top_y >= 0. && top_y + h <= 720., "y {y}: {top_y} + {h}");
        }
    }
}
