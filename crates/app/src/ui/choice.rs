//! Segmented choice: every answer a short setting takes, on the setting's own
//! row, the one in force lit, a click on any other picking it outright. The
//! settings page's answer to its "press to cycle" rows (UX audit D-1): a cycle
//! button shows one value and hides the rest behind a press, so a person
//! cannot know what a row offers without stepping all the way round it. Five
//! answers or fewer fit on one row as chips; a longer list is a dropdown
//! ([`crate::Pick`]) -- still one click away, and the row's caret says so.
//!
//! Keyboard: the settings page's own arrow branch
//! ([`crate::ui::settings_stance::settings_arrow`]) moves a focus between the
//! choice rows (`↑`/`↓`) and steps the focused row's value (`←`/`→`) -- the
//! radio group's convention, where an arrow both moves and picks, so there
//! is no second "confirm" stroke to learn. The focused row wears DESIGN
//! §4.10's 1px `ink2` top rule, so where the keyboard is looking is visible
//! before a key is pressed.
//!
//! A file of its own rather than a widget in `ui/widgets.rs`: another lane
//! changed that file at the same time, and this helper has no sibling there.

use crate::ui::hitmap;
use crate::ui::type_scale::{self, Typeset};
use crate::*;
use gpui::SharedString;

/// One chip of a segmented row: its word, and whether it is the value in
/// force.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chip {
    pub(crate) label: SharedString,
    pub(crate) active: bool,
}

/// The chips of a row from its labels and the index in force: exactly one is
/// lit, or none when `current` names no row (a setting with nothing behind
/// it yet, like the tonemap before a project opens).
pub(crate) fn chips<S: Into<SharedString> + Clone>(
    labels: &[S],
    current: Option<usize>,
) -> Vec<Chip> {
    labels
        .iter()
        .enumerate()
        .map(|(i, label)| Chip {
            label: label.clone().into(),
            active: current == Some(i),
        })
        .collect()
}

/// The index an arrow steps to from `current` in a row of `len`: the next
/// one, wrapping at either end, so the stroke never dead-ends on the last
/// chip -- the cycle buttons this replaces wrapped, and a hand used to that
/// keeps it.
pub(crate) fn step(len: usize, current: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    match forward {
        true => (current + 1) % len,
        false => (current + len - 1) % len,
    }
}

/// Where `↑`/`↓` lands among `len` rows: the next row, wrapping; from no
/// focus, the first row (`↓`) or the last (`↑`). `None` for a tab with no
/// choice rows, where there is nothing for a focus to sit on.
pub(crate) fn move_focus(len: usize, focused: Option<usize>, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match focused.filter(|&at| at < len) {
        Some(at) => step(len, at, forward),
        None if forward => 0,
        None => len - 1,
    })
}

/// A chip's side padding, and the gap between two chips.
pub(crate) const CHIP_PAD: f32 = 8.;
pub(crate) const CHIP_GAP: f32 = 2.;

/// A chip's width as the page measures it before it is drawn -- the mono
/// badge glyph's advance per character, generous, plus the padding. For the
/// fit guard in the tests, never the layout: gpui measures the real thing.
#[cfg(test)]
pub(crate) fn chip_w(label: &str) -> f32 {
    label.chars().count() as f32 * 7. + 2. * CHIP_PAD
}

/// The width a whole segmented row of `labels` asks for, chips and gaps.
#[cfg(test)]
pub(crate) fn row_w<S: AsRef<str>>(labels: &[S]) -> f32 {
    labels
        .iter()
        .map(|label| chip_w(label.as_ref()))
        .sum::<f32>()
        + CHIP_GAP * labels.len().saturating_sub(1) as f32
}

/// The segmented row: one chip per entry, each a hit area of its own
/// (`{id}-{i}`, so the harness can press any answer, not only the next one).
/// The chip in force wears the raised plate and `ink1`; the others are `ink3`
/// and light on hover. Disabled, every chip is `ink4` and takes no click --
/// the row stays in the tree saying what it offers, which is the point.
/// `hint` is the row's own sentence, on every chip's hover line. `pick` is
/// called with the chip's index; it is `Copy` so each chip can carry its own
/// listener.
pub(crate) fn segmented<V: 'static>(
    id: &'static str,
    chips: Vec<Chip>,
    enabled: bool,
    hint: &'static str,
    cx: &mut Context<V>,
    pick: impl Fn(&mut V, usize, &mut Context<V>) + Copy + 'static,
) -> impl IntoElement {
    let style = type_scale::body_strong();
    let t = crate::ui::theme::tokens();
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(CHIP_GAP))
        .p(px(2.))
        .rounded(px(4.))
        .bg(rgb(t.e2))
        .when(!enabled, |d| d.opacity(0.6))
        .children(chips.into_iter().enumerate().map(|(i, chip)| {
            let active = chip.active;
            let hit_label = chip.label.to_string();
            let tip = crate::ui::widgets::tip_hover(&hit_label, "", Some(hint));
            crate::ui::kit::kbd_stop(div().id(SharedString::from(format!("{id}-{i}"))))
                .flex_none()
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(CHIP_PAD))
                .rounded(px(3.))
                .when(active, |d| d.bg(rgb(t.e3)))
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .when(!active, |d| d.hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER))))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            pick(this, i, cx)
                        }))
                })
                .tooltip(tip)
                .children(hitmap::dynamic(
                    move || (format!("{id}-{i}"), hit_label),
                    enabled,
                ))
                .type_style(style.clone())
                .text_color(rgb(match (enabled, active) {
                    (false, _) => t.ink4,
                    (true, true) => t.ink1,
                    (true, false) => t.ink2,
                }))
                .child(chip.label)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_chip_is_lit_and_it_is_the_value_in_force() {
        let labels = ["Off", "On"];
        let row = chips(&labels, Some(1));
        assert_eq!(row.iter().filter(|c| c.active).count(), 1);
        assert!(row[1].active && !row[0].active);
        assert_eq!(row[1].label.as_ref(), "On");
        // Nothing behind the setting yet: every chip rests.
        assert!(chips(&labels, None).iter().all(|c| !c.active));
    }

    #[test]
    fn an_arrow_steps_and_wraps_at_both_ends() {
        assert_eq!(step(3, 0, true), 1);
        assert_eq!(
            step(3, 2, true),
            0,
            "→ off the last chip wraps to the first"
        );
        assert_eq!(
            step(3, 0, false),
            2,
            "← off the first chip wraps to the last"
        );
        assert_eq!(step(0, 0, true), 0, "an empty row has nowhere to step");
        // A full walk round is a wrap, never a dead end.
        let mut at = 0;
        for _ in 0..4 {
            at = step(4, at, true);
        }
        assert_eq!(at, 0);
    }

    #[test]
    fn focus_enters_at_an_end_and_walks_the_rows_round() {
        assert_eq!(
            move_focus(3, None, true),
            Some(0),
            "↓ from nowhere lands on the first row"
        );
        assert_eq!(
            move_focus(3, None, false),
            Some(2),
            "↑ from nowhere lands on the last"
        );
        assert_eq!(move_focus(3, Some(2), true), Some(0));
        assert_eq!(move_focus(3, Some(0), false), Some(2));
        assert_eq!(move_focus(0, Some(1), true), None, "no rows, no focus");
        // A focus left over from a longer tab is treated as none.
        assert_eq!(move_focus(2, Some(5), true), Some(0));
    }

    #[test]
    fn a_row_measures_its_chips_and_the_gaps_between_them() {
        assert_eq!(chip_w("On"), 2. * 7. + 2. * CHIP_PAD);
        assert_eq!(
            row_w(&["Off", "On"]),
            chip_w("Off") + chip_w("On") + CHIP_GAP
        );
        assert_eq!(row_w::<&str>(&[]), 0.);
    }
}
