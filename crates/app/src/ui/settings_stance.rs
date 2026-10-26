//! The settings page (user complaint: "we should have a settings page that
//! we can edit project and editor settings"). Sections, per the split
//! rule a rubric pass settled -- but never columns at once: they are
//! tabs of one strip since 2026-09-19 (user: the three-column row asked the
//! eye to find three anchors in one glance and then to read a column that
//! began mid-screen). One section shows under the strip; the strip wears the
//! dock's own tab look ([`crate::ui::dock_stance`]'s `dock_tab`, reproduced
//! here because that fn is private to its module) and each tab's hover plate
//! carries the one-line explanation the column head used to trail:
//!
//! Four tabs (DESIGN §9.2), each ending on a line that says where its values
//! are kept:
//!
//! - PROJECT: anything that changes the rendered output or is stored in the
//!   `.edith` file -- resolution, fps, sample-rate, the HDR tonemap, the mix,
//!   and the proxy switches. The window's own default answers before a project
//!   is open ([`crate::player::library::load_proxies_pref`]).
//! - EDITOR: this machine and this window -- the palette and the subtitle font,
//!   kept in `~/.config/edith` like the theme beside them, and the monitoring
//!   level, which is session-only.
//! - EXPORT: what a delivery is written as. The picture and the sound are this
//!   computer's default ([`load_export_defaults`]); the encoder is the project's.
//! - KEYS: every shortcut, rebindable, saved in the keybindings file. See
//!   [`keys_section`].
//!
//! A row with five answers or fewer shows all of them as chips
//! ([`crate::ui::choice`]): the tonemap, the two proxy switches, the sound
//! rate and the encoder seat -- what used to be "press to cycle" rows, where
//! only the current value showed (UX audit D-1). A longer list (the project's
//! size and rate, the palette, the export's file) is a dropdown off the row,
//! which wears a caret so it reads as one. The arrows walk the chip rows and
//! step them ([`settings_arrow`]); the focused row wears a 1px `ink2` top rule.
//!
//! Every row here reuses an opener or a picker this editor already has
//! ([`Player::open_picker`], [`Player::open_mix`], [`Player::open_subtitle_style`],
//! [`Player::toggle_proxies`], [`Player::toggle_auto_proxies`]) -- no new card
//! body, no second source of truth for what a value is or how it is set.
//! Values apply live through those same doors; there is no Apply button here
//! to be inconsistent with the rest of the room.
//!
//! Seams left for other builders (this task's scope boundary): a finer
//! fps/sample-rate/tone picker is the same builder's to extend; this page
//! only ever asks for [`Pick::Fps`]/[`Pick::SampleRate`]/[`Pick::Tone`]
//! exactly as `inspector.rs`'s legacy panel already does.
//!
//! The HDR reference numbers beside the tonemap row (`HDR reference` and
//! `Content light`) are the *file's* declared light level
//! ([`engine::colorspace::ContentLight`]), not a monitor fact -- read-only,
//! since nothing in [`engine::project::Project`] holds a place to persist an
//! override into.

use crate::keymap::{self, Binding, Chord, Keymap};
use crate::ui::choice;
use crate::ui::hitmap;
use crate::ui::type_scale;
use crate::*;
use gpui::{AnyElement, KeyDownEvent, SharedString};

/// Which of the page's four sections is showing. Kept on [`Player`] beside
/// [`Player::settings_open`] rather than in the leaf, so a close and a reopen
/// return to the section a hand left instead of snapping back to the first --
/// the dock keeps its own tab pick the same way ([`Player::dock_src_active`]).
/// Display state and nothing else: no row reads it, and picking a tab writes
/// no project value, so the pick never lights the ledger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsTab {
    Project,
    Editor,
    Keys,
}

/// What the KEYS tab is waiting on. Held on [`Player`] so a capture survives a
/// repaint and the tab's own key listener can read it. `None` (the field) is
/// the page at rest: keys the tab does not take fall through to the room, so
/// escape still closes the page.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum KeyEdit {
    /// The filter field was clicked: typed letters go to it.
    Filtering,
    /// Row `usize` (an index into [`Keymap::entries`]) takes the next key.
    Capturing(usize),
    /// `chord`, typed for row `row`, is already `holder`'s. The row asks,
    /// inline, before it moves the stroke.
    Conflict {
        row: usize,
        chord: Chord,
        holder: usize,
    },
}

/// One tab of the page's strip (PROJECT/EDITOR/EXPORT), wearing the dock's
/// own tab look -- [`crate::ui::dock_stance`]'s `dock_tab` verbatim: no pill,
/// no fill, no rounded button, a 1px `ink1` top rule and `ink1` text mark the
/// showing tab, the resting one is `ink3`, and the whole row is a
/// [`hitmap::control`] so the harness can press it. The section's one-line
/// explanation -- what used to run on after a column head as ambient prose --
/// is the tab's hover plate (DESIGN §8: a distinction that matters is a plate,
/// not ambient text).
fn settings_tab(
    id: &'static str,
    label_text: &'static str,
    explanation: &'static str,
    tab: SettingsTab,
    active: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let style = type_scale::body();
    crate::ui::kit::kbd_stop(div().id(id))
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .h(px(36.))
        .border_b_2()
        .border_color(rgba(0))
        .when(active, |d| d.border_color(rgb(crate::ui::theme::tokens().accent)))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_settings_tab(tab, cx)))
        .tooltip(crate::ui::widgets::tip_hover(
            &format!("Show {}", label_text.to_lowercase()),
            explanation,
            None,
        ))
        .children(hitmap::control(id, label_text, true))
        .child(
            div()
                .font(style.font.clone())
                .text_size(style.size)
                .text_color(rgb(if active { crate::ui::theme::tokens().ink1 } else { crate::ui::theme::tokens().ink2 }))
                .child(label_text),
        )
}

/// The shortcut column every settings row ends on: the stroke that also
/// reaches the setting, dim and right-aligned in a column of its own after the
/// value (the menus' shortcut column), never glued before it -- "M 100%" read
/// as one value. A row with no stroke keeps the column empty so the values
/// above and below it stay on one edge, and an unbound action shows nothing,
/// never a dash.
fn chord_col(chord: Option<SharedString>) -> impl IntoElement {
    div()
        .flex_none()
        // No stroke, no column: every row's value then ends on the same edge.
        .when(chord.is_some(), |d| d.min_w(px(SETTINGS_CHORD_W)))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(3.))
        // Each key of the stroke is its own keycap ("Ctrl" "M"), as in menus and tooltips.
        .children(chord.map(|text| {
            let parts: Vec<String> = match text.as_ref() {
                "+" => vec!["+".to_string()],
                t => t.split('+').filter(|p| !p.is_empty()).map(str::to_string).collect(),
            };
            parts.into_iter().map(crate::ui::kit::keycap).collect::<Vec<_>>()
        }).into_iter().flatten())
}

/// The least width of [`chord_col`]: one short stroke, so single-letter keys
/// line up and a longer one (`Ctrl+M`) grows the column rather than wrapping.
const SETTINGS_CHORD_W: f32 = 52.;

/// `action`'s first stroke, compact, or `None` when it has none: an unbound
/// action wears no badge (the keymap's own [`Keymap::chord`] answers `--`).
fn chord_of(player: &Player, action: ActionId) -> Option<SharedString> {
    player
        .keymap
        .entries()
        .iter()
        .any(|b| b.action == action)
        .then(|| player.keymap.chord(action).into())
}

/// The short plain sentence a value slot carries instead of a bare dash when
/// there is nothing to show: no project yet, or (`hdr_only`) a project whose
/// source declares nothing for this row.
pub(crate) fn no_value_reason(has_project: bool, hdr_only: bool) -> &'static str {
    match (has_project, hdr_only) {
        (false, _) => "Open a project first",
        (true, true) => "Only for HDR sources",
        (true, false) => "Add a clip first",
    }
}

/// A row for a setting with more answers than a row of chips holds: a
/// click opens its list ([`Pick`]), and the value wears a caret so the row
/// reads as a dropdown rather than a button that steps. `chord` is the
/// stroke that also reaches the setting, in the mono chord ink every other
/// darkroom surface wears it in (DESIGN §4 -- "every command wears its chord,
/// everywhere it appears"): the palette came here off the spine with its
/// badge, and keeps it.
#[allow(clippy::too_many_arguments)]
fn list_row(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    chord: Option<SharedString>,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    value_ink: u32,
) -> impl IntoElement {
    row_full(
        id, label_text, value, hint, chord, None, true, player, on_click, value_ink,
    )
}

/// The one row body the wrappers share. `chord` is the stroke that also
/// reaches this setting, drawn in mono `ink3` *beside* the value rather than
/// over it -- three settings came here off the spine and each one keeps the
/// badge it wore there (DESIGN §4). `caret` trails the value with `▾` for a
/// row whose click opens a list, the dock's own fold glyph, so a dropdown
/// and a door read differently before either is pressed.
#[allow(clippy::too_many_arguments)]
fn row_full(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    chord: Option<SharedString>,
    tail: Option<(SharedString, u32)>,
    caret: bool,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    value_ink: u32,
) -> impl IntoElement {
    let value: SharedString = value.into();
    let hint: SharedString = hint.into();
    let label_style = type_scale::body();
    let value_style = type_scale::body();
    let exporting = player.exporting().is_some();
    crate::ui::kit::kbd_stop(div().id(id))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .h(px(CONTROL_H))
        .px(px(8.))
        .rounded(px(3.))
        .tooltip(move |_, cx| cx.new(|_| Tip(hint.clone())).into())
        .when(exporting, |d| d.opacity(0.4).cursor_not_allowed())
        .when(!exporting, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())))
                .on_click(on_click)
        })
        .children(hitmap::control(id, label_text, !exporting))
        .child(
            div()
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child(label_text),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .children(tail.map(|(tail, ink)| {
                    let tail_style = type_scale::caption();
                    div()
                        .font(tail_style.font)
                        .text_size(tail_style.size)
                        .text_color(rgb(ink))
                        .child(tail)
                }))
                .child(
                    div()
                        .font(value_style.font.clone())
                        .text_size(value_style.size)
                        .text_color(rgb(value_ink))
                        .child(value),
                )
                .children(caret.then(|| {
                    crate::ui::icons::Icon::ChevronDown
                        .el(crate::ui::icons::IconSize::Inline, crate::ui::theme::tokens().ink3)
                }))
                .child(chord_col(chord)),
        )
}

/// A setting with five answers or fewer, shown as a row of chips rather
/// than a value that steps on a press: every answer visible, the one in
/// force lit, any other one click away. Listed in the order the keyboard
/// walks them ([`choice_rows`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChoiceRow {
    /// Which rendition HDR media is watched in ([`Preset::ALL`]).
    Tone,
    /// The two stand-in switches, Off/On.
    Proxies,
    AutoProxies,
}

/// The two words a switch answers with, and the index each one sits at.
const SWITCH: [&str; 2] = ["Off", "On"];

/// Where a switch's chip is: `Off` first, `On` second.
pub(crate) fn switch_index(on: bool) -> usize {
    usize::from(on)
}

/// The switch a chip index means ([`switch_index`]'s inverse).
pub(crate) fn switch_on(at: usize) -> bool {
    at == 1
}

/// Where `value` sits in `all` -- the chip to light.
pub(crate) fn index_of<T: PartialEq>(all: &[T], value: T) -> Option<usize> {
    all.iter().position(|v| *v == value)
}

impl ChoiceRow {
    /// Every choice row there is, for the guards below.
    #[cfg(test)]
    pub(crate) const ALL: [ChoiceRow; 3] = [
        ChoiceRow::Tone,
        ChoiceRow::Proxies,
        ChoiceRow::AutoProxies,
    ];

    /// The row's chips, in order, each a word read off the same table the
    /// value is picked from -- so a chip cannot name a value the setter
    /// does not take.
    pub(crate) fn labels(self) -> Vec<String> {
        match self {
            ChoiceRow::Tone => Preset::ALL
                .iter()
                .map(|&p| tone_label(p).to_string())
                .collect(),
            ChoiceRow::Proxies | ChoiceRow::AutoProxies => {
                SWITCH.iter().map(|w| w.to_string()).collect()
            }
        }
    }

    /// Whether the row can take a pick right now: the tonemap and the seat
    /// are the project's, so they need one open; the rest answer always.
    fn enabled(self, player: &Player) -> bool {
        match self {
            ChoiceRow::Tone => player.session.is_some(),
            ChoiceRow::Proxies | ChoiceRow::AutoProxies => true,
        }
    }
}

/// The choice rows a tab shows, top to bottom -- what the arrows walk. The
/// EXPORT tab's depend on the file: a codec with no rate has no Sound row,
/// and a song has no Encoder row ([`export_section`]'s own two gates).
pub(crate) fn choice_rows(tab: SettingsTab, _format: Format) -> Vec<ChoiceRow> {
    match tab {
        SettingsTab::Project => vec![ChoiceRow::Tone, ChoiceRow::Proxies, ChoiceRow::AutoProxies],
        SettingsTab::Editor | SettingsTab::Keys => Vec::new(),
    }
}

/// Which chip of `row` is in force: `None` where nothing is behind it yet.
pub(crate) fn current_choice(player: &Player, row: ChoiceRow) -> Option<usize> {
    match row {
        ChoiceRow::Tone => player
            .session
            .as_ref()
            .and_then(|session| index_of(&Preset::ALL, session.tone())),
        ChoiceRow::Proxies => Some(switch_index(player.proxies_on)),
        // The import switch waits on editing copies: while they are off it shows off, and its own
        // value is kept, so it comes back on when they are turned on again.
        ChoiceRow::AutoProxies => Some(switch_index(player.auto_proxies_on && player.proxies_on)),
    }
}

/// The arrows while the page is up ([`Player::param_card_key`]'s settings
/// branch): `↑`/`↓` move the focus between the showing tab's choice rows,
/// `←`/`→` step the focused row's value -- and, with no row focused yet,
/// only put the focus on the first one, so the first stroke shows where the
/// keyboard is before anything changes. `false` for any other key, and for
/// a tab with no choice rows, so those strokes fall through as before.
pub(crate) fn settings_arrow(player: &mut Player, key: &str, cx: &mut Context<Player>) -> bool {
    let rows = choice_rows(player.settings_tab, player.format);
    if rows.is_empty() {
        return false;
    }
    match key {
        "up" | "down" => {
            player.settings_focus =
                choice::move_focus(rows.len(), player.settings_focus, key == "down");
        }
        "left" | "right" => {
            let Some(at) = player.settings_focus.filter(|&at| at < rows.len()) else {
                player.settings_focus = choice::move_focus(rows.len(), None, true);
                cx.notify();
                return true;
            };
            let row = rows[at];
            if let Some(current) = current_choice(player, row) {
                let next = choice::step(row.labels().len(), current, key == "right");
                pick_choice(player, row, next, cx);
            }
        }
        _ => return false,
    }
    cx.notify();
    true
}

/// A choice row: the label, then the chord and the tail where the row has
/// them, then the chips ([`choice::segmented`]) in the value's slot. No
/// click on the row itself -- the chips are the controls -- and the row
/// the keyboard is on wears DESIGN §4.10's 1px `ink2` top rule.
#[allow(clippy::too_many_arguments)]
fn choice_row(
    id: &'static str,
    label_text: &'static str,
    hint: &'static str,
    chord: Option<SharedString>,
    tail: Option<(SharedString, u32)>,
    dependent: bool,
    row: ChoiceRow,
    player: &Player,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // The key is told in the tooltip only, never as a chip on the row.
    // A dependent row (the import switch) waits on the editing-copies switch.
    let waiting = dependent && !player.proxies_on;
    let tip: SharedString = match (&chord, waiting) {
        (_, true) => format!("{hint} (turn on editing copies first)").into(),
        (Some(key), false) => format!("{hint} ({key})").into(),
        (None, false) => hint.into(),
    };
    let label_style = type_scale::body();
    let badge_style = type_scale::caption();
    let exporting = player.exporting().is_some();
    let enabled = !exporting && !waiting && row.enabled(player);
    let focused = player.settings_focus.is_some()
        && index_of(&choice_rows(player.settings_tab, player.format), row) == player.settings_focus;
    let labels = row.labels();
    let chips = choice::chips(&labels, current_choice(player, row));
    let is_switch = labels == SWITCH;
    // A label too long to share a line with its chips takes a line of its own.
    let stacked = label_text.chars().count() > 26;
    let id_sw: &'static str = match row {
        ChoiceRow::Proxies => "settings-switch-proxies",
        _ => "settings-switch-auto-proxies",
    };
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .h(px(CONTROL_H))
        .px(px(8.))
        .when(dependent, |d| d.pl(px(28.)))
        .when(stacked, |d| d.flex_col().items_end().h_auto().min_h(px(CONTROL_H)).py(px(4.)).gap(px(4.)))
        .rounded(px(3.))
        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
        .when(focused, |d| d.border_t_1().border_color(rgb(INK2())))
        .when(exporting || waiting, |d| d.opacity(0.4))
        .child(
            div()
                .when(stacked, |d| d.w_full())
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child(label_text),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .children(tail.map(|(tail, ink)| {
                    div()
                        .font(badge_style.font.clone())
                        .text_size(badge_style.size)
                        .text_color(rgb(ink))
                        .child(tail)
                }))
                .child(match is_switch {
                    true => {
                        let on = chips.get(1).is_some_and(|c| c.active);
                        let sw = crate::ui::kit::switch(id_sw, on, enabled, None)
                            .children(hitmap::dynamic(
                                move || (format!("{id}-{}", usize::from(!on)), "toggle".to_string()),
                                enabled,
                            ));
                        match enabled {
                            true => sw
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    pick_choice(this, row, usize::from(!on), cx)
                                }))
                                .into_any_element(),
                            false => sw.into_any_element(),
                        }
                    }
                    false => choice::segmented(
                        id,
                        chips,
                        enabled,
                        hint,
                        cx,
                        move |this, at, cx| pick_choice(this, row, at, cx),
                    )
                    .into_any_element(),
                })
                // The empty key column keeps every row's value on one edge.
                .child(chord_col(None)),
        )
}

/// A size as a person says it: `1920×1080 (1080p)`, `3840×2160 (2160p, 4K)`;
/// a size with no common name is just its two numbers.
pub(crate) fn resolution_words(w: u32, h: u32) -> String {
    // A tall or square frame names its shape, not a height the list has no word for.
    if w <= h {
        return crate::export_ui::size_row_label(w, h);
    }
    let name = match h {
        720 => Some("720p"),
        1080 => Some("1080p"),
        1440 => Some("1440p"),
        2160 => Some("2160p (4K)"),
        _ => None,
    };
    match name {
        Some(name) => format!("{w}\u{d7}{h} ({name})"),
        None => format!("{w}\u{d7}{h}"),
    }
}

/// One quiet, wrapping line under a row: what its choices mean.
fn hint_line(id: &'static str, text: &'static str) -> impl IntoElement {
    let style = type_scale::caption();
    div()
        .id(id)
        .flex_none()
        .px(px(8.))
        .pb(px(4.))
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(INK3()))
        .child(text)
}

/// PROJECT: the canvas every clip is composed onto, the rate it is cut at,
/// how HDR looks on a normal screen (only when the project has HDR in it), and
/// the editing-copies switches. The mix is a live panel (Clip tab), not a row.
fn project_section(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    // corner-cut: no genuine default resolution or fps exists before either
    // a project opens or a pick is made, so the empty state is a bare dash
    // in INK3 -- quieter than a real value (INK1), never a noun that reads as one.
    let resolution = player.session.as_ref().map(PlaybackSession::resolution);
    let (res_val, res_ink) = match resolution.or(player.pending_settings.0) {
        Some((w, h)) => (resolution_words(w, h), INK1()),
        None => (no_value_reason(false, false).to_string(), INK3()),
    };
    let (fps_val, fps_ink) = match (player.session.is_some(), player.pending_settings.1) {
        (true, _) => (format!("{} fps", fps_label(player.fps)), INK1()),
        (false, Some(fps)) => (format!("{} fps", fps_label(fps)), INK1()),
        (false, None) => (no_value_reason(false, false).to_string(), INK3()),
    };
    // The rate keeps a "Match source" fallback: unlike resolution/fps it
    // states a real behaviour (derived from the sources) rather than standing
    // in for a value with nothing behind it.
    let rate_val = match player
        .session
        .as_ref()
        .map_or(player.pending_settings.2, |s| s.sample_rate())
    {
        Some(rate) => format!("{} kHz", f64::from(rate) / 1000.),
        None => "Match source".to_string(),
    };
    // The file's own declared light level is the sign of an HDR source.
    let has_hdr = player
        .session
        .as_ref()
        .is_some_and(|s| s.content_light().peak().is_some());
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(list_row(
            "settings-resolution",
            "Resolution",
            res_val,
            "the canvas every clip is composed onto, and the size the export comes out at; click for the list",
            None,
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Resolution, event.position(), cx)),
            res_ink,
        ))
        .child(list_row(
            "settings-fps",
            "Frame rate",
            fps_val,
            "the rate the whole timeline is cut and written at; click for the list",
            None,
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Fps, event.position(), cx)),
            fps_ink,
        ))
        .child(list_row(
            "settings-sample-rate",
            "Audio sample rate",
            rate_val,
            "the rate the project's own sound mix is run at; Match source follows the highest rate among the sources, 48 kHz if all are below 44.1 kHz",
            None,
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::SampleRate, event.position(), cx)),
            INK1(),
        ))
        .child(still_row(player, cx))
        // Only a project with HDR in it has anything to choose here.
        .children(has_hdr.then(|| {
            choice_row(
                "settings-tonemap",
                "When HDR plays on a normal screen",
                "which look HDR video gets on a normal (SDR) screen; SDR video is untouched",
                None,
                None,
                false,
                ChoiceRow::Tone,
                player,
                cx,
            )
        }))
        .children(has_hdr.then(|| {
            hint_line(
                "settings-tonemap-hint",
                "Faithful stays true to the standard; Balanced looks like a normal player; Bright is the richest colour.",
            )
        }))
        // The proxy switches are the project's once one is open (they are
        // written into its .edith); the window's default answers before that.
        // The progress of a copy being made rides as the first switch's tail
        // ([`Player::proxy_tail`], its own separator dropped).
        .child(choice_row(
            "settings-proxies",
            "Editing copies (proxies)",
            "Lighter copies for smooth playback; export always uses the originals",
            chord_of(player, ActionId::ToggleProxies),
            {
                let making = player.proxy_tail();
                let making = making.trim_start_matches([' ', '\u{b7}']).trim();
                (player.proxies_on && !making.is_empty()).then(|| (making.to_string().into(), INK3()))
            },
            false,
            ChoiceRow::Proxies,
            player,
            cx,
        ))
        .child(choice_row(
            "settings-auto-proxies",
            "Make them on import",
            "makes a lighter copy of every big video as it is imported",
            chord_of(player, ActionId::ToggleAutoProxies),
            None,
            true,
            ChoiceRow::AutoProxies,
            player,
            cx,
        ))
        .child(saved_line("Saved in this project"))
}

/// The still image length: a typed number of seconds, the number field's own look. A click opens
/// it for typing; Enter keeps it, Escape drops it. Off until a project is open, since the length
/// is the project's.
fn still_row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let secs = player.session.as_ref().map(PlaybackSession::still_secs);
    let enabled = secs.is_some();
    let shown = secs.map_or_else(|| "\u{2013}".to_string(), |s| format!("{s:.1}"));
    let typing = player.still_num.as_ref();
    let field = crate::ui::kit::number_field("settings-still-secs", 96., shown, enabled.then_some("s"), typing, enabled);
    let label_style = type_scale::body();
    let field = match enabled {
        false => field.cursor_not_allowed(),
        true => field.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.begin_still_num(cx))),
    };
    div()
        .id("settings-still")
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .h(px(CONTROL_H))
        .px(px(8.))
        .tooltip(crate::ui::widgets::tip_hover(
            "Still image length",
            "",
            Some("How long a picture or title card lasts when it is dropped on the timeline; clips already placed keep theirs"),
        ))
        .child(
            div()
                .font(label_style.font.clone())
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child("Still image length"),
        )
        .child(field)
}

/// A chip of `row` was picked (by a click, or by an arrow through
/// [`settings_arrow`]): the value at `at`, through the same door the row's
/// old cycle press and the keymap's own action go through -- the switches
/// toggle only when the chip differs from the state, so a click on the lit
/// chip is not a flip. Beside the PROJECT section because three of the five
/// rows are its, and the doors it opens are the project's.
fn pick_choice(player: &mut Player, row: ChoiceRow, at: usize, cx: &mut Context<Player>) {
    match row {
        ChoiceRow::Tone => {
            if let Some(&preset) = Preset::ALL.get(at) {
                player.apply_tone(preset, cx);
            }
        }
        ChoiceRow::Proxies => {
            if switch_on(at) != player.proxies_on {
                player.toggle_proxies(cx);
            }
        }
        ChoiceRow::AutoProxies => {
            if switch_on(at) != player.auto_proxies_on {
                player.toggle_auto_proxies(cx);
            }
        }
    }
}

/// The line a section ends on, saying where its values are kept (DESIGN §9.2).
/// Quiet `ink3` text on the row's own height, so it reads as a note, not a row.
fn saved_line(text: &'static str) -> impl IntoElement {
    let style = type_scale::caption();
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(px(CONTROL_H))
        .px(px(8.))
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(INK3()))
        .child(text)
}

/// EDITOR: this machine and this person's own reading preferences. Nothing
/// here touches the `.edith` file.
fn editor_section(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        // The room's own palette, moved off the spine (user 2026-08-21: the
        // rail was too crowded to fit its own rows) -- a preference kept in
        // ~/.config/edith, so the EDITOR section is where it belongs; the
        // same `Pick::Theme` list `ActionId::Theme` has always opened.
        .child(list_row(
            "settings-theme",
            "Theme",
            crate::ui::theme::active().label(),
            "the editor's colours; kept for every project; click for the list",
            chord_of(player, ActionId::Theme),
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| {
                this.open_picker(Pick::Theme, event.position(), cx)
            }),
            INK1(),
        ))
        .child(accent_row(player, cx))
        .child(vivid_row(player, cx))
        .child(custom_block(player, cx))
        // The family in force, said (user: "Subtitle font  Font…" -- the
        // value slot carried the door's noun, elided, never the pick). With
        // nothing picked the window draws its own, which is one word.
        .child(list_row(
            "settings-subtitle-style",
            "Subtitle font",
            player
                .sub_family
                .clone()
                .unwrap_or_else(|| "Default".to_string()),
            "the cue plate's own font and size -- kept in ~/.config/edith, never in a project",
            None,
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.open_subtitle_style(cx)),
            INK1(),
        ))
        .child(list_row(
            "settings-autosave",
            "Autosave",
            crate::player::library::autosave_label(player.autosave_secs),
            "how often unsaved work is written beside the project; click for the list",
            None,
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Autosave, event.position(), cx)),
            INK1(),
        ))
        .child(saved_line("Saved on this computer."))
}

/// "Accent colour": six round swatches, one click picks, the pick is in force
/// and kept at once. Every swatch has the same 22px box with a 2px border that
/// is transparent until it is the picked one, so picking never moves a neighbour.
fn accent_row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::ui::theme::{self, AccentId};
    let label_style = type_scale::body();
    let picked = theme::accent();
    let exporting = player.exporting().is_some();
    let mut swatches = div().flex_none().flex().items_center().gap(px(4.));
    for a in AccentId::ALL {
        let on = a == picked;
        let hint = SharedString::from(a.label());
        swatches = swatches.child(
            crate::ui::kit::kbd_stop(div().id(("settings-accent", a as usize)))
                .flex_none()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(match on {
                    true => gpui::Hsla::from(rgb(INK1())),
                    false => gpui::transparent_black(),
                })
                .cursor_pointer()
                .tooltip(move |_, cx| cx.new(|_| Tip(hint.clone())).into())
                .children(hitmap::dynamic(move || ("settings-accent".to_string(), format!("Accent colour {}", a.label())), !exporting))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    theme::set_accent(a);
                    this.keep_accent();
                    cx.notify();
                }))
                .child(match a == AccentId::Custom && theme::custom_colour().is_none() {
                    // Nothing of its own yet: a plus says "make one".
                    true => div()
                        .size(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(INK2()))
                        .child("+")
                        .into_any_element(),
                    false => div().size(px(12.)).rounded_full().bg(rgb(theme::swatch(a))).into_any_element(),
                }),
        );
    }
    div()
        .id("settings-accent-row")
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .h(px(CONTROL_H))
        .px(px(8.))
        .child(
            div()
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child("Accent colour"),
        )
        .child(swatches)
}

/// The Custom colour and Hue rows while Custom is the accent in force; otherwise one quiet line in
/// the same space, so picking an accent never moves the rows below.
fn custom_block(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let live = crate::ui::theme::accent() == crate::ui::theme::AccentId::Custom;
    let hint = type_scale::caption();
    div()
        .flex_none()
        .h(px(2. * CONTROL_H + 2.))
        .flex()
        .flex_col()
        .gap(px(2.))
        .when(live, |d| d.child(custom_row(player, cx)).child(hue_row(player, cx)))
        .when(!live, |d| {
            d.justify_center().child(
                div()
                    .px(px(8.))
                    .font(hint.font)
                    .text_size(hint.size)
                    .text_color(rgb(INK3()))
                    .child("Use + to pick any colour"),
            )
        })
}

/// Wraps one of the Custom rows: always on the page so nothing moves, dim and untouchable while
/// the accent in force is not Custom.
fn custom_only(live: bool, row: impl IntoElement) -> Div {
    let wrap = div().relative().flex_none().child(row);
    match live {
        true => wrap,
        false => wrap
            .opacity(0.4)
            .child(div().absolute().size_full().top_0().left_0().occlude()),
    }
}

/// "Vivid": how strong the accent is, 0 a faint tint, 50 the colours as designed, 100 bold.
fn vivid_row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::player::accent::AccentSlider;
    use crate::ui::widgets::{SliderRow, SliderTyped, slider_fraction, slider_row_inline};
    let vivid = crate::ui::theme::vivid();
    let reset: crate::ui::widgets::SliderReset<Player> = Rc::new(|this: &mut Player, _| {
        crate::ui::theme::set_vivid(crate::ui::theme::VIVID_DEFAULT);
        this.keep_accent();
    });
    div()
        .id("settings-vivid")
        .flex_none()
        .h(px(CONTROL_H))
        .px(px(8.))
        .flex()
        .items_center()
        .tooltip(crate::ui::widgets::overlay_tip_hover(
            "Vivid",
            "",
            Some("How strong the accent colour is: 0 is a faint tint, 50 keeps the colours as designed, 100 bold."),
        ))
        .child(slider_row_inline(
        SliderRow {
            id: "settings-vivid-bar",
            label: "Vivid".into(),
            value: format!("{vivid} %").into(),
            fraction: slider_fraction(f32::from(vivid), 0., 100.),
            changed: vivid != crate::ui::theme::VIVID_DEFAULT,
            bar: player.accent_vivid_bar.clone(),
            on_press: Box::new(|this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.accent_drag = Some(AccentSlider::Vivid);
                this.drag_accent(AccentSlider::Vivid, x, cx);
            }),
            on_reset: Some(reset),
            // Typed as the number the row shows, 0 to 100.
            typed: Some(SliderTyped {
                lo: 0.,
                hi: 100.,
                set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                    crate::ui::theme::set_vivid(v.round() as u8);
                    this.keep_accent();
                    cx.notify();
                }),
            }),
        },
        cx,
    ))
}

/// "Custom colour": the hex of the colour behind the Custom swatch. A valid edit is in force at
/// once; an invalid one keeps the last good colour and says what a colour looks like.
fn custom_row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::ui::kit::{self, ScrubLook};
    use crate::ui::theme::{self, AccentId};
    let label_style = type_scale::body();
    let live = theme::accent() == AccentId::Custom;
    let editing = player.accent_hex.as_ref();
    let value = match editing {
        Some(f) => f.text.clone(),
        None => theme::custom_colour().map_or("#______".to_string(), |c| format!("#{c:06x}")),
    };
    let invalid = editing.is_some_and(|f| !f.text.is_empty() && theme::parse_hex(&f.text).is_none());
    let hint_style = type_scale::caption();
    let field = kit::scrub_field(
        "settings-accent-hex",
        76.,
        value,
        None,
        live,
        editing.map(|_| ScrubLook::Typing),
    )
    .cursor(CursorStyle::IBeam)
    .tooltip(crate::ui::widgets::tip_hover("Custom colour", "type hex", Some("Six hex digits such as 3ddc97; enter to keep")))
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(|this, _: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.begin_accent_hex(cx);
        }),
    );
    custom_only(
        live,
        div()
            .id("settings-accent-custom-row")
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .h(px(CONTROL_H))
            .px(px(8.))
            .child(
                div()
                    .font(label_style.font)
                    .text_size(label_style.size)
                    .text_color(rgb(INK2()))
                    .child("Custom colour"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .font(hint_style.font)
                            .text_size(hint_style.size)
                            .text_color(rgb(INK3()))
                            .child(match invalid {
                                true => "Use six digits, like #3ddc97",
                                false => "",
                            }),
                    )
                    .child(field),
            ),
    )
}

/// The hue of the Custom colour, 0..360, on a rainbow track drawn at the Vivid in force. The
/// knob is white over it; the whole row is dim while Custom is not the accent.
fn hue_row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::player::accent::AccentSlider;
    use crate::ui::theme::{self, AccentId};
    const THUMB: f32 = 14.;
    const STEPS: usize = 24;
    let t = theme::tokens();
    let live = theme::accent() == AccentId::Custom;
    let hue = theme::custom_colour().map_or(0., |c| theme::to_oklch(c).2) as f32;
    let fraction = (hue / 360.).clamp(0., 1.);
    let vivid = theme::vivid();
    // One canvas, stripes from cumulative rounded edges: each stripe ends exactly where the next
    // begins, so no fractional width leaves a seam and no overlap doubles up under the dim.
    let colours: Vec<u32> = (0..STEPS)
        .map(|i| {
            let h = (i as f64 + 0.5) / STEPS as f64 * 360.;
            theme::vivid_adjust(theme::from_oklch(0.75, 0.14, h), vivid)
        })
        .collect();
    let track = div().relative().w_full().h(px(8.)).rounded(px(4.)).overflow_hidden().child(
        gpui::canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let (x0, w) = (f32::from(bounds.origin.x), f32::from(bounds.size.width));
                let edge = |i: usize| (x0 + (i as f32 * w / STEPS as f32)).round();
                for (i, colour) in colours.iter().enumerate() {
                    let (l, r) = (edge(i), if i + 1 == STEPS { x0 + w } else { edge(i + 1) });
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(point(px(l), bounds.origin.y), size(px(r - l), bounds.size.height)),
                        rgb(*colour),
                    ));
                }
            },
        )
        .size_full(),
    );
    let label_style = type_scale::body();
    let value_style = type_scale::caption();
    custom_only(
        live,
        div()
            .id("settings-hue")
            .flex_none()
            .h(px(CONTROL_H))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .w(px(96.))
                    .font(label_style.font)
                    .text_size(label_style.size)
                    .text_color(rgb(t.ink2))
                    .child("Hue"),
            )
            .child(
                div()
                    .id("settings-hue-bar")
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .h(px(HIT_MIN))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.accent_drag = Some(AccentSlider::Hue);
                            this.drag_accent(AccentSlider::Hue, event.position.x, cx);
                        }),
                    )
                    .child(crate::timeline_math::bounds_probe(player.accent_hue_bar.clone()))
                    .child(track)
                    .child(
                        div()
                            .absolute()
                            .top(px((HIT_MIN - THUMB) / 2.))
                            .left(relative(fraction))
                            .ml(px(-THUMB / 2.))
                            .size(px(THUMB))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(t.e0))
                            .bg(rgb(t.ink1)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(crate::layout::redesign::CONTROL_S))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(rgb(t.e2))
                    .font(value_style.font)
                    .text_size(value_style.size)
                    .text_color(rgb(t.ink2))
                    .child(format!("{}°", hue.round() as u32)),
            ),
    )
}

/// The nine files this editor writes, in the order the Picture row cycles
/// them: a codec's two boxes side by side (the container question the export
/// card asked in a row of its own, folded into this one), and the sound-only
/// formats after the pictures.
pub(crate) const PICTURE_CYCLE: [Format; 10] = [
    Format::Mp4,
    Format::Av1Mp4,
    Format::Av1,
    Format::Hevc,
    Format::HevcMp4,
    Format::Wav,
    Format::Flac,
    Format::Mp3,
    Format::Ogg,
    Format::Opus,
];

/// `H.264 · MP4`: the codec by the name it is picked under ([`format_label`])
/// and the box it lands in. An audio-only format names itself once -- `WAV ·
/// WAV` would be the one field on the line that says nothing.
pub(crate) fn picture_label(format: Format) -> String {
    match format.has_video() {
        true => format!("{} · {}", format_label(format), format.ext().to_uppercase()),
        false => format_label(format).to_string(),
    }
}

/// The Picture row's list ([`Pick::Picture`]): the nine files in the order
/// of [`PICTURE_CYCLE`], the one in force marked, and beside each one what this
/// machine says about it -- a refusal in six words where there is one
/// ([`short_reason`]), else the box or `sound only`. Refused rows are still
/// listed, since a reason a person can read is what tells them why the pick
/// is not theirs to make (DESIGN §8: never hidden).
pub(crate) fn picture_choices(
    current: Format,
    refusal: impl Fn(Format) -> Option<String>,
) -> Vec<crate::menus::ChoiceRow> {
    PICTURE_CYCLE
        .into_iter()
        .map(|format| {
            let detail: SharedString = match refusal(format) {
                Some(why) => short_reason(&why).into(),
                None if format.has_video() => "picture and sound".into(),
                None => "sound only".into(),
            };
            (
                Choice::Picture(format),
                picture_label(format).into(),
                detail,
                format == current,
            )
        })
        .collect()
}

/// A refusal in six words, which is what fits beside a value on one row --
/// the whole sentence is still said where the pick is actually refused
/// ([`Player::set_format`]'s own banner).
pub(crate) fn short_reason(why: &str) -> String {
    why.split_whitespace().take(6).collect::<Vec<_>>().join(" ")
}

/// Saves the keybindings file after a change. As with the export pick, a
/// failed write is said once and the new keys still work this session.
fn persist_keys(player: &mut Player) {
    if player.keymap.save().is_err() {
        player
            .notify_user("Could not save the keys. Check that ~/.config/edith is writable.".into());
    }
}

/// Whether a key name is a modifier alone. A capture waits through these: the
/// stroke is the key pressed with them, not the shift on its own.
fn is_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "shift" | "control" | "alt" | "platform" | "function" | "capslock"
    )
}

/// Takes `chord` for row `row`. A stroke the file cannot spell is refused and
/// the row keeps waiting; a stroke another row holds is offered as a swap, never
/// taken silently; any other stroke is simply the row's now.
fn capture_key(player: &mut Player, row: usize, chord: Chord) {
    if !chord.bindable() {
        player.notify_user("That key cannot be bound, so the shortcut is unchanged.".into());
        return;
    }
    match player.keymap.holder(&chord, row) {
        Some(holder) => {
            player.settings_key_edit = Some(KeyEdit::Conflict { row, chord, holder });
        }
        None => apply_key(player, row, chord),
    }
}

/// Puts `chord` on row `row`, handing the holder's stroke over when there is
/// one, and saves.
fn apply_key(player: &mut Player, row: usize, chord: Chord) {
    player.keymap.swap_in(row, chord);
    player.settings_key_edit = None;
    persist_keys(player);
}

/// Puts one action back to its shipped keys. Any action that lost one of them
/// to this is named, so the change is never silent.
fn reset_action_keys(player: &mut Player, action: ActionId) {
    let displaced = player.keymap.reset_action(action);
    player.settings_key_edit = None;
    persist_keys(player);
    if !displaced.is_empty() {
        let names: Vec<&str> = displaced.iter().map(|a| a.label()).collect();
        player.notify_user(
            format!(
                "{} is back to its keys. {} lost the one it took.",
                action.label(),
                names.join(", ")
            )
            .into(),
        );
    }
}

/// The KEYS tab's keyboard, asked first while its list holds focus (a click on
/// the filter or a row gives it focus). A capture takes every key but the
/// modifiers alone; the filter takes the letters typed into it. Anything it
/// does not take falls through to the room, so escape still closes the page
/// and the room's chords still work. `true` when the key was taken.
fn settings_key_down(player: &mut Player, event: &KeyDownEvent) -> bool {
    let key = event.keystroke.key.as_str();
    let mods = &event.keystroke.modifiers;
    match player.settings_key_edit.clone() {
        Some(KeyEdit::Capturing(row)) => {
            if key == "escape" {
                player.settings_key_edit = None;
            } else if !is_modifier_key(key) {
                let chord = Chord {
                    key: key.to_string(),
                    ctrl: mods.control,
                    shift: mods.shift,
                    alt: mods.alt,
                };
                capture_key(player, row, chord);
            }
            true
        }
        Some(KeyEdit::Conflict { row, chord, .. }) => {
            match key {
                "escape" => player.settings_key_edit = None,
                "enter" => apply_key(player, row, chord),
                // Tab and Shift+Tab fall through to the room, which moves the focus ring
                // between Swap and Cancel.
                _ => return false,
            }
            true
        }
        Some(KeyEdit::Filtering) => {
            // A chord is the room's, not the filter's.
            if mods.control || mods.alt {
                return false;
            }
            match key {
                "escape" => {
                    player.settings_key_edit = None;
                    player.settings_keys_filter.clear();
                }
                "enter" => player.settings_key_edit = None,
                "backspace" => {
                    player.settings_keys_filter.pop();
                }
                _ => {
                    if let Some(c) = crate::menus::typed(key) {
                        player.settings_keys_filter.push(c);
                    }
                }
            }
            true
        }
        None => false,
    }
}

/// A text button on a KEYS row or its footer: a hit area of its own, the label
/// in `ink1`, and `hint` as its tooltip. `key` names it for the harness, and
/// the click stops there so the row it sits in does not also take it.
fn text_button(
    key: String,
    label: &'static str,
    hint: &'static str,
    tab: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let style = type_scale::body();
    let hit_key = key.clone();
    div()
        .id(SharedString::from(key))
        .when(tab, crate::ui::kit::kbd_stop)
        .flex_none()
        .flex()
        .items_center()
        .h(px(CONTROL_H))
        .px(px(8.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_RAISED())))
        .tooltip(crate::ui::widgets::tip_hover(label, hint, None))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .children(hitmap::dynamic(move || (hit_key, label.to_string()), true))
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(INK1()))
        .child(label)
}

/// One value on a KEYS row, in mono, its ink the caller's: `ink1` for a stroke
/// the user changed or a prompt, `ink3` for the shipped one.
/// The strokes of one Keys row as keycaps ("Ctrl" "M"), words such as "hold" or
/// "drag" plain beside them; an action with no key shows a calm dash.
fn keys_cell(text: &str) -> AnyElement {
    let t = crate::ui::theme::tokens();
    let style = type_scale::caption();
    let word = |w: &str| {
        div()
            .flex_none()
            .font(style.font.clone())
            .text_size(style.size)
            .text_color(rgb(t.ink3))
            .child(w.to_string())
            .into_any_element()
    };
    let text = text.trim();
    if text.is_empty() || text == "unbound" || text == "--" {
        return div()
            .flex_none()
            .text_color(rgb(t.ink3))
            .child("\u{2014}")
            .into_any_element();
    }
    let mut parts: Vec<AnyElement> = Vec::new();
    for group in text.split(" or ") {
        if !parts.is_empty() {
            parts.push(word("or"));
        }
        for token in group.split_whitespace() {
            if token == "/" {
                parts.push(word("/"));
                continue;
            }
            let (token, comma) = match token.strip_suffix(',') {
                Some(t) if !t.is_empty() => (t, true),
                _ => (token, false),
            };
            for cap in cap_parts(token) {
                let plain = cap.len() >= 3 && cap.chars().all(|c| c.is_ascii_lowercase());
                parts.push(match plain {
                    true => word(&cap),
                    false => crate::ui::kit::keycap(cap).into_any_element(),
                });
            }
            if comma {
                parts.push(word(","));
            }
        }
    }
    div().flex_none().flex().items_center().gap(px(3.)).children(parts).into_any_element()
}

/// One stroke split into its keys: `Ctrl+Shift+M` is three, `Ctrl++` is
/// "Ctrl" and "+".
fn cap_parts(stroke: &str) -> Vec<String> {
    let chars: Vec<char> = stroke.chars().collect();
    let (mut out, mut cur) = (Vec::new(), String::new());
    for (i, c) in chars.iter().enumerate() {
        if *c == '+' && !cur.is_empty() && i + 1 < chars.len() {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(*c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn key_value(text: String, ink: u32) -> AnyElement {
    let style = type_scale::body();
    div()
        .flex_none()
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(ink))
        .child(text)
        .into_any_element()
}

/// The filter field above the list: a click gives it the keyboard, and typed
/// letters then narrow the rows.
fn filter_row(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let style = type_scale::body();
    let empty = player.settings_keys_filter.is_empty();
    let filtering = matches!(player.settings_key_edit, Some(KeyEdit::Filtering));
    let shown = match (empty, filtering) {
        (true, false) => "Search shortcuts".to_string(),
        (_, true) => format!("{}\u{258f}", player.settings_keys_filter),
        _ => player.settings_keys_filter.clone(),
    };
    div()
        .id("settings-keys-filter")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .mb(px(4.))
        .px(px(8.))
        .rounded(px(4.))
        .border_1()
        .border_color(match filtering {
            true => rgb(t.accent).into(),
            false => gpui::hsla(0., 0., 0., 0.),
        })
        .bg(rgb(t.e2))
        .cursor_text()
        .tooltip(crate::ui::widgets::tip_hover(
            "Search shortcuts",
            "click to type",
            None,
        ))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.settings_key_edit = Some(KeyEdit::Filtering);
            window.focus(&this.focus_settings);
            cx.notify();
        }))
        .children(hitmap::control("settings-keys-filter", "Filter keys", true))
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(match empty && !filtering {
            true => t.ink3,
            false => t.ink1,
        }))
        .child(crate::ui::icons::Icon::Search.el(crate::ui::icons::IconSize::Inline, t.ink3))
        .child(shown)
}

/// A read-only line on the KEYS list: the label, and the stroke in the dim
/// right-aligned key column (empty for an action with none).
fn key_static(label: &'static str, chord: String, why: &'static str) -> impl IntoElement {
    let label_style = type_scale::body();
    div()
        .id(SharedString::from(format!("settings-keys-static-{label}")))
        .tooltip(crate::ui::widgets::tip_hover(label, "", Some(why)))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .min_h(px(CONTROL_H))
        .py(px(4.))
        .px(px(8.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child(label),
        )
        .child(keys_cell(&chord))
}

/// One stroke on the KEYS list. A click takes the next key for it; while it
/// waits the row says so, and a stroke another row holds offers Swap and Cancel
/// in place. A changed action carries its own Reset. The value is `ink1` when
/// the stroke differs from the shipped one, `ink3` at the shipped one.
fn key_row(
    row: usize,
    binding: &Binding,
    edit: Option<&KeyEdit>,
    shipped: &Keymap,
    player: &Player,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let action = binding.action;
    let label = action.label();
    let chord = player.keymap.display(action);
    let changed = !player.keymap.is_default(action, shipped);
    let label_style = type_scale::body();
    let mut right: Vec<AnyElement> = Vec::new();
    match edit {
        Some(KeyEdit::Capturing(at)) if *at == row => {
            right.push(key_value("Press a key, Esc cancels".to_string(), INK1()));
        }
        Some(KeyEdit::Conflict {
            row: at, holder, ..
        }) if *at == row => {
            let holder_label = player.keymap.entries()[*holder].action.label();
            right.push(
                div()
                    .flex_none()
                    .font(label_style.font.clone())
                    .text_size(label_style.size)
                    .text_color(rgb(INK2()))
                    .child(format!("Used by {holder_label}"))
                    .into_any_element(),
            );
            right.push(
                text_button(
                    format!("settings-key-{row}-swap"),
                    "Swap",
                    "give this key to this action, and this action's old key to the other",
                    true,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(KeyEdit::Conflict { row, chord, .. }) =
                            this.settings_key_edit.clone()
                        {
                            apply_key(this, row, chord);
                        }
                        cx.notify();
                    }),
                )
                .into_any_element(),
            );
            right.push(
                text_button(
                    format!("settings-key-{row}-cancel"),
                    "Cancel",
                    "keep both keys as they are",
                    true,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.settings_key_edit = None;
                        cx.notify();
                    }),
                )
                .into_any_element(),
            );
        }
        _ => {
            right.push(keys_cell(&chord));
            if changed {
                right.push(
                    text_button(
                        format!("settings-key-{row}-reset"),
                        "Reset",
                        "back to the key this action shipped with",
                        false,
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            reset_action_keys(this, action);
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                );
            }
        }
    }
    crate::ui::kit::kbd_stop(div().id(("settings-key", row)))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .min_h(px(CONTROL_H))
        .py(px(4.))
        .px(px(8.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_RAISED())))
        .tooltip(crate::ui::widgets::tip_hover(
            label,
            &chord,
            Some("click, then press the new key"),
        ))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.settings_key_edit = Some(KeyEdit::Capturing(row));
            window.focus(&this.focus_settings);
            cx.notify();
        }))
        // Enter on the focused row takes the next key, as a click does. Only an idle row
        // starts: while a capture or a conflict is up, Enter is that edit's own key (the
        // list's listener reads it), and a focused Swap or Cancel never reaches here as Enter.
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
            let mods = &event.keystroke.modifiers;
            if event.keystroke.key == "enter" && !mods.control && !mods.alt && this.settings_key_edit.is_none() {
                this.settings_key_edit = Some(KeyEdit::Capturing(row));
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .children(hitmap::dynamic(
            move || (format!("settings-key-{row}"), label.to_string()),
            true,
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .children(right),
        )
}

/// KEYS: every shortcut the keymap holds, one row per action, filterable. A
/// click on a row (or Enter on the focused row) takes the next key as its
/// stroke; escape cancels. A stroke another row holds is offered as a swap:
/// Enter swaps, escape cancels, Tab reaches the two buttons. The footer resets
/// every key.
///
/// The list takes the keyboard through `track_focus`, so its own key listener
/// sees each stroke before the room's keymap does. It takes only what the page
/// is waiting for ([`settings_key_down`]).
fn keys_section(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let shipped = Keymap::defaults();
    let query = player.settings_keys_filter.trim().to_ascii_lowercase();
    let edit = player.settings_key_edit.clone();
    // The shortcuts sheet's own rows, in its order ([`crate::menus::keys_rows`]):
    // a heading per category, then one row per action -- not per stroke, so an
    // action with two keys ("? or F1") is listed once -- then the strokes the
    // cards answer themselves. A row whose action has a stroke the file can
    // spell takes a click to rebind; the rest are read-only, and an action
    // with no stroke shows an empty key column, never the word unbound.
    let rows: Vec<AnyElement> = crate::menus::keys_rows_matching(&query, &player.keymap)
        .into_iter()
        .map(|r| match r {
            crate::menus::KeyRow::Head(category) => {
                let head = type_scale::section();
                div()
                    .flex_none()
                    .px(px(8.))
                    .pt(px(10.))
                    .font(head.font)
                    .text_size(head.size)
                    .text_color(rgb(INK3()))
                    .child(category.label())
                    .into_any_element()
            }
            crate::menus::KeyRow::Act(action) => {
                let at = player
                    .keymap
                    .entries()
                    .iter()
                    .position(|b| b.action == action && b.chord.bindable());
                match at {
                    Some(row) => key_row(row, &player.keymap.entries()[row], edit.as_ref(), &shipped, player, cx)
                        .into_any_element(),
                    None => key_static(action.label(), String::new(), "No key is set for this. Use it from the menu").into_any_element(),
                }
            }
            crate::menus::KeyRow::Fixed(i) => {
                let f = &keymap::FIXED[i];
                key_static(f.label, f.chord.to_string(), "A fixed key: it cannot be changed").into_any_element()
            }
        })
        .collect();
    div()
        .id("settings-keys")
        .track_focus(&player.focus_settings)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            if settings_key_down(this, event) {
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(filter_row(player, cx))
        .child(hint_line("settings-keys-hint", "Click a shortcut, then press the new key"))
        .children(match rows.is_empty() {
            true => vec![crate::ui::kit::dialog_text("No shortcut matches.").px(px(8.)).into_any_element()],
            false => rows,
        })
        .child(
            div()
                .flex_none()
                .flex()
                .justify_between()
                .items_center()
                .child(saved_line("Saved on this computer"))
                .child(text_button(
                    "settings-keys-reset-all".to_string(),
                    "Reset all",
                    "every key back to the one it shipped with",
                    false,
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.keymap = Keymap::defaults();
                        this.settings_key_edit = None;
                        persist_keys(this);
                        cx.notify();
                    }),
                )),
        )
}

/// The scroll fade at the foot of the rows, in the plate's own colour (the
/// shared fade is the dock's, a different surface).
fn plate_more_below(scroll: gpui::ScrollHandle) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if !crate::export_ui::more_below_shows(f32::from(scroll.offset().y), f32::from(scroll.max_offset().height)) {
                return;
            }
            let plate = rgb(crate::ui::theme::tokens().e3);
            let clear = gpui::Rgba { a: 0., ..plate };
            window.paint_quad(gpui::fill(
                bounds,
                gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(clear, 0.),
                    gpui::linear_color_stop(plate, 0.7),
                ),
            ));
        },
    )
    .absolute()
    .left_0()
    .right_0()
    .bottom_0()
    .h(px(28.))
}

/// The centred modal's plate width: one tabbed column's own measure, wide
/// enough for the longest row a section carries (the EXPORT range's marks
/// line, its chords and all) beside its label -- and no wider. The sheet this
/// replaces was `w_full()`, which read every row as a line stretched across
/// the room with its value a window away from the label it belongs to.
/// [`KEYS_W`]'s class, and a step past its number: these rows carry the same
/// label-left/stroke-right anatomy with a value where that card has none.
const SETTINGS_W: f32 = 480.;

/// The gap the plate keeps to the menu bar above it and the time band below.
const SETTINGS_GAP: f32 = 8.;

/// The rows' one measure, spent by every tab alike: the tallest section's own
/// height (PROJECT's nine rows and its storage line at [`CONTROL_H`], the
/// sections' 2px gaps between them), so a tab switch swaps the rows and never resizes the plate
/// under the hand (user 2026-09-20: "switching between them feels weird"). A
/// shorter section leaves air below its last row; a short window still
/// scrolls, the plate's `max_h` cap shrinking this from `min_h(0)`.
pub(crate) const SETTINGS_ROWS_H: f32 = 7. * CONTROL_H + 36. + 7. * 2.;

/// The plate's height that is not its rows: the dialog's padding and gaps, the title and the tab
/// strip. The plate's top is set from it, so the tab strip sits on one line for every tab.
const SETTINGS_CHROME_H: f32 = 40. + 28. + CONTROL_H + 12.;

/// The Keys list's share of the window's height, and the most it may take: the list is the one
/// place a whole shortcut sheet is read, so it gets more room than PROJECT's rows.
const KEYS_SHARE: f32 = 0.6;
const KEYS_ROWS_MAX: f32 = 560.;

/// The dialog layer's own top and bottom padding ([`kit::dialog_layer`]).
const LAYER_PAD: f32 = 8.;

/// The page itself: a centred modal card, closed by a click away or `esc`
/// through [`Player::close_card`] like every other card here.
///
/// It held to [`crate::ui::stance::below_picture_floor`] from 2026-08-21
/// until 2026-09-19: a centred sheet then drew the whole page -- and every
/// picker opened off one of its rows -- over the picture (DESIGN §11 check
/// 6's occlusion rule, half of the user's own report about menus "not
/// aligning with our design"), so the page was anchored over the bench and
/// ledger at the window's full width. The user 2026-09-19 retired that read
/// -- "this settings section should be a modal instead of covering the
/// timeline" -- so the plate is centred again, at one tabbed column's own
/// measure: it floats over the room's middle, the timeline beneath is not
/// laid under a full-width sheet, and a short window scrolls the rows under
/// the strip rather than clipping them. The catcher still paints nothing
/// rather than tinting the frame, the same rule the menus it opens follow.
///
/// Centred on the whole window it still stood over the ruler and the V1/A1
/// lanes (its top near y=375 of 1080), so since 2026-10-19 the plate is
/// centred in [`settings_area`] -- the band over the viewer that ends above
/// the time band, where the export sheet sits -- and capped to it.
pub(crate) fn render(
    player: &Player,
    window_size: Size<Pixels>,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // The plate's top is one line for every tab: it is centred as PROJECT's rows (the tallest of
    // the other tabs) would centre it, and the tab strip stays on that line. Only the Keys tab
    // is taller, so its plate grows downward from there -- the strip never moves on a switch.
    let wh = f32::from(window_size.height);
    let proj_rows = SETTINGS_ROWS_H.min(wh - 2. * SETTINGS_GAP - 150.).max(120.);
    let top = LAYER_PAD + ((wh - 2. * LAYER_PAD - (SETTINGS_CHROME_H + proj_rows)) / 2.).max(0.);
    let rows_h = match player.settings_tab {
        SettingsTab::Keys => {
            // A third of the window is the list's, capped; what is left under the plate's
            // bottom edge is the most it may take, so the plate stays on screen.
            let want = (KEYS_SHARE * wh - SETTINGS_CHROME_H).min(KEYS_ROWS_MAX);
            let room = wh - LAYER_PAD - top - SETTINGS_CHROME_H;
            want.min(room).max(proj_rows)
        }
        _ => proj_rows,
    };
    crate::ui::kit::dialog_layer(0.)
        .items_start()
        .pt(px(top))
        .on_mouse_move(cx.listener(Player::drag_move))
        .on_mouse_up(MouseButton::Left, cx.listener(Player::drag_release))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                if this.any_text_field() {
                    this.blur_text_fields(window, cx);
                }
                // A capture or a filter is the page's own; a click away ends it
                // with the page, so the next opening starts at rest.
                this.settings_key_edit = None;
                this.settings_open = false;
                cx.notify();
                cx.stop_propagation();
            }),
        )
        .child(
            crate::ui::kit::dialog("settings-card", SETTINGS_W)
                .max_w(relative(1.))
                // The card sits over the room's own capture, so a press in it leaves
                // a field (the accent's hex) itself; the field's click then reopens it.
                .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if this.any_text_field() {
                        this.blur_text_fields(window, cx);
                    }
                }))
                .gap(px(6.))
                .child(crate::ui::kit::dialog_title("Settings"))
                        .child(
                            div()
                                .id("settings-tabs")
                                .flex_none()
                                .flex()
                                .child(settings_tab(
                                    "settings-tab-project",
                                    "Project",
                                    "stored in the .edith file",
                                    SettingsTab::Project,
                                    player.settings_tab == SettingsTab::Project,
                                    cx,
                                ))
                                .child(settings_tab(
                                    "settings-tab-editor",
                                    "Editor",
                                    "this machine, this window",
                                    SettingsTab::Editor,
                                    player.settings_tab == SettingsTab::Editor,
                                    cx,
                                ))
                                .child(settings_tab(
                                    "settings-tab-keys",
                                    "Keys",
                                    "every shortcut, and how to rebind it",
                                    SettingsTab::Keys,
                                    player.settings_tab == SettingsTab::Keys,
                                    cx,
                                )),
                        )
                        // The showing section alone, on the plate's whole width: the
                        // three columns this row used to hold side by side -- each
                        // with its own head and a hairline down its left edge -- asked
                        // the eye to find three anchors at once and then to read a
                        // column that began mid-screen. The strip above is the way
                        // between them; there is one section in the tree at a time, so
                        // a row that is not showing cannot be reached by a tab stroke
                        // or by the pointer either. The area's height is the tallest
                        // section's own ([`SETTINGS_ROWS_H`]), so a tab switch swaps
                        // the rows and never resizes the plate under the hand.
                        // A short window shortens the rows (the plate's
                        // `max_h`); the export sheet's fade says when more
                        // of them is below.
                        .child(
                            div()
                                .relative()
                                .h(px(rows_h))
                                .min_h(px(0.))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .id("settings-rows")
                                        .flex_1()
                                        .min_h(px(0.))
                                        .overflow_y_scroll()
                                        .track_scroll(&player.settings_rows_scroll)
                                        .child(match player.settings_tab {
                                            SettingsTab::Project => project_section(player, cx).into_any_element(),
                                            SettingsTab::Editor => editor_section(player, cx).into_any_element(),
                                            SettingsTab::Keys => keys_section(player, cx).into_any_element(),
                                        }),
                                )
                                .child(plate_more_below(player.settings_rows_scroll.clone())),
                        ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chip rows, their words and their tabs.
    #[test]
    fn every_choice_row_is_five_answers_or_fewer_each_a_plain_word() {
        for row in ChoiceRow::ALL {
            let labels = row.labels();
            assert!(
                (2..=5).contains(&labels.len()),
                "{row:?} has {} answers -- chips are for five or fewer",
                labels.len()
            );
            for label in &labels {
                assert!(
                    !label.is_empty() && !label.contains('?'),
                    "{row:?}: {label:?}"
                );
                assert!(
                    label.chars().count() <= 9,
                    "{row:?}: {label:?} is not a chip's word"
                );
            }
        }
    }

    /// Picking a chip sets the value it names: the index into the row is the
    /// index into the table the setter reads, both ways.
    #[test]
    fn a_chip_index_names_the_value_it_sets_and_back() {
        // The renditions, by the names every other surface calls them.
        assert_eq!(ChoiceRow::Tone.labels(), ["Faithful", "Balanced", "Bright"]);
        assert_eq!(index_of(&Preset::ALL, Preset::Vivid), Some(2));
        // A switch: Off is the first chip, On the second, and back.
        assert_eq!(ChoiceRow::Proxies.labels(), ["Off", "On"]);
        for on in [false, true] {
            assert_eq!(switch_on(switch_index(on)), on);
        }
        // A value no table holds lights nothing rather than a wrong chip.
        assert_eq!(index_of(&Preset::ALL, Preset::Vivid), Some(2));
        assert_eq!(index_of(&[1, 2], 9), None);
    }

    /// The current value is the lit chip -- exactly one, and the right one.
    #[test]
    fn the_chip_in_force_is_the_one_lit() {
        let chips = choice::chips(&ChoiceRow::Tone.labels(), index_of(&Preset::ALL, Preset::Standard));
        let lit: Vec<&str> = chips
            .iter()
            .filter(|c| c.active)
            .map(|c| c.label.as_ref())
            .collect();
        assert_eq!(lit, ["Balanced"]);
        // Nothing behind the setting (no project for the tonemap): no chip lit.
        let chips = choice::chips(&ChoiceRow::Tone.labels(), None);
        assert!(chips.iter().all(|c| !c.active));
    }

    /// `→` off the last chip is the first, `←` off the first is the last: the
    /// arrows cycle a row the way the press it replaces did.
    #[test]
    fn the_arrows_cycle_a_row_round() {
        for row in ChoiceRow::ALL {
            let n = row.labels().len();
            let mut at = 0;
            for _ in 0..n {
                at = choice::step(n, at, true);
            }
            assert_eq!(at, 0, "{row:?}: a full walk is a wrap");
            assert_eq!(choice::step(n, 0, false), n - 1, "{row:?}: ← wraps");
        }
    }

    /// Each tab's chip rows, in the order the keyboard walks them; the EXPORT
    /// tab's follow the file the way its rows are mounted.
    #[test]
    fn the_choice_rows_sit_in_the_tabs_they_belong_to() {
        assert_eq!(
            choice_rows(SettingsTab::Project, Format::Mp4),
            [ChoiceRow::Tone, ChoiceRow::Proxies, ChoiceRow::AutoProxies]
        );
        assert!(choice_rows(SettingsTab::Editor, Format::Mp4).is_empty());
        assert!(choice_rows(SettingsTab::Keys, Format::Mp4).is_empty());
        // The focus walks whatever the tab has, and nothing on an empty tab.
        assert_eq!(choice::move_focus(3, None, true), Some(0));
        assert_eq!(choice::move_focus(0, None, true), None);
    }

    /// The page fits a 1280x720 window with every chip row on one line: the
    /// plate's height under the cap, and each row's label, badge, tail and
    /// chips inside [`SETTINGS_W`] at a generous per-glyph measure.
    #[test]
    fn the_panel_and_its_chip_rows_fit_at_1280_by_720() {
        // PROJECT at its tallest (HDR row and its hint, both switches, the
        // storage line) still fits the rows area.
        let project_tallest = 7. * CONTROL_H + 36. + 7. * 2.;
        assert!(project_tallest <= SETTINGS_ROWS_H);
        // Width: a generous measure per glyph, so a real font only has room
        // to spare. (label, chord, tail, row)
        let keys = Keymap::defaults();
        let label_w = |text: &str| text.chars().count() as f32 * 9.;
        let mono_w = |text: &str| text.chars().count() as f32 * 8.;
        let _ = keys;
        // (label, tail, row); the key lives in the tooltip, the column is empty.
        let rows: [(&str, &str, ChoiceRow); 3] = [
            ("When HDR plays on a normal screen", "", ChoiceRow::Tone),
            ("Editing copies (proxies)", "100%", ChoiceRow::Proxies),
            ("      Make them on import", "", ChoiceRow::AutoProxies),
        ];
        for (label, tail, row) in rows {
            let chord = String::new();
            let parts = 1 + usize::from(!chord.is_empty()) + usize::from(!tail.is_empty());
            let label = if label.chars().count() > 26 { "" } else { label };
            let w = 16.
                + label_w(label)
                + mono_w(&chord)
                + mono_w(tail)
                + parts as f32 * 8.
                + choice::row_w(&row.labels());
            assert!(
                w <= SETTINGS_W,
                "{row:?} asks {w}px of a {SETTINGS_W}px plate -- its chips wrap"
            );
        }
    }

    /// The chips drive the same doors the keys and the old press did, the
    /// arrows reach the page, and the focused row is drawn.
    #[test]
    fn the_chip_rows_drive_the_same_setters_and_the_arrows_reach_them() {
        let page = include_str!("settings_stance.rs");
        let start = page.find("fn pick_choice(").expect("the chip dispatcher");
        let body = &page[start..start + page[start..].find("\n}\n").expect("its end")];
        for door in [
            "apply_tone(",
            "toggle_proxies(",
            "toggle_auto_proxies(",
        ] {
            assert!(body.contains(door), "a chip row no longer opens {door}");
        }
        // No settings row cycles on a press any more: the rows with few
        // answers are chips, the rest open their list.
        let rows = &page[..page.find("#[cfg(test)]\nmod tests").expect("the tests")];
        for press in [
            "cycle_export_picture()",
            "cycle_audio_kbps()",
            "cycle_encoder(",
        ] {
            assert!(
                !rows.contains(press),
                "a settings row still cycles on a press: {press}"
            );
        }
        // The focused row wears the 1px ink2 top rule (DESIGN §4.10).
        let start = page.find("fn choice_row(").expect("the chip row");
        let body = &page[start..start + page[start..].find("\n}\n").expect("its end")];
        assert!(body.contains(".when(focused, |d| d.border_t_1().border_color(rgb(INK2())))"));
        assert!(body.contains("choice::segmented("));
        // The room's key branch hands the arrows to the page.
        let cards = include_str!("../player/cards.rs");
        assert!(
            cards.contains("settings_arrow(self, key, cx)"),
            "the arrows never reach the page"
        );
    }

    /// The Picture list: the nine files in `c`'s order, one marked, a refused
    /// one carrying its reason rather than vanishing.
    #[test]
    fn the_picture_list_is_the_cycle_with_its_refusals_said() {
        let rows = picture_choices(Format::Av1, |f| {
            (f == Format::Hevc).then(|| {
                "HEVC needs the VA-API plugin, which this machine has not built".to_string()
            })
        });
        assert_eq!(rows.len(), PICTURE_CYCLE.len());
        let picked: Vec<&SharedString> = rows.iter().filter(|r| r.3).map(|r| &r.1).collect();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].as_ref(), "AV1 · MKV");
        let hevc = rows
            .iter()
            .find(|r| r.0 == Choice::Picture(Format::Hevc))
            .expect("HEVC");
        assert_eq!(hevc.2.as_ref(), "HEVC needs the VA-API plugin, which");
        let wav = rows
            .iter()
            .find(|r| r.0 == Choice::Picture(Format::Wav))
            .expect("WAV");
        assert_eq!(wav.2.as_ref(), "sound only");
    }

    #[test]
    fn a_modifier_alone_is_not_a_stroke() {
        for key in ["shift", "control", "alt", "platform", "function"] {
            assert!(is_modifier_key(key), "{key}");
        }
        assert!(!is_modifier_key("s"));
        assert!(!is_modifier_key("escape"));
    }

    #[test]
    fn the_keys_filter_matches_a_label_or_a_chord_and_nothing_else() {
        let keys = Keymap::defaults();
        let has = |q: &str| {
            crate::menus::keys_rows_matching(q, &keys)
                .iter()
                .any(|r| matches!(r, crate::menus::KeyRow::Act(ActionId::Save)))
        };
        assert!(has("save"));
        assert!(has("ctrl+s"));
        assert!(!has("zzz"));
    }

    /// The Keys tab is the shortcuts sheet's rows: one row per action, so
    /// "Keyboard shortcuts" (`?` and F1) is listed once, and the key column
    /// never says "unbound".
    #[test]
    fn the_keys_tab_lists_each_action_once_and_never_says_unbound() {
        let keys = Keymap::defaults();
        let rows = crate::menus::keys_rows_matching("", &keys);
        let shows = rows
            .iter()
            .filter(|r| matches!(r, crate::menus::KeyRow::Act(ActionId::ShowKeys)))
            .count();
        assert_eq!(shows, 1);
        let page = include_str!("settings_stance.rs");
        let body = &page[..page.find("#[cfg(test)]\nmod tests").unwrap()];
        // Only the key cell may name the absence (it draws a dash for it).
        let at = body.find("fn keys_cell(").unwrap();
        let body = format!("{}{}", &body[..at], &body[body[at..].find("\n}\n").unwrap() + at..]);
        assert!(!body.contains("\"unbound\""));
    }

    /// No value slot carries a bare dash: an empty row gives a plain reason.
    /// And a stroke is drawn after the value it belongs to, never before it
    /// ("M 100%", "c H.264" read as one value).
    #[test]
    fn no_value_is_a_bare_dash_and_no_stroke_leads_a_value() {
        let page = include_str!("settings_stance.rs");
        let body = &page[..page.find("#[cfg(test)]\nmod tests").unwrap()];
        // The key cell is the one place a dash is the answer: an action with no key.
        let at = body.find("fn keys_cell(").unwrap();
        let body = format!("{}{}", &body[..at], &body[body[at..].find("\n}\n").unwrap() + at..]);
        assert!(!body.contains("(\"\u{2014}\".to_string()"));
        assert!(!body.contains("(\"\u{2014}\""));
        assert!(!body.contains("\"--\""));
        for (project, hdr) in [(false, false), (false, true), (true, true), (true, false)] {
            let why = no_value_reason(project, hdr);
            assert!(why.split_whitespace().count() >= 3, "{why}");
            assert!(!why.starts_with(|c: char| c.is_ascii_alphabetic() && why.chars().nth(1) == Some(' ')));
        }
        // chord_col is the last child of each row body, after the value.
        for f in ["fn row_full(", "fn choice_row("] {
            let at = page.find(f).unwrap();
            let end = page[at..].find("\n}\n").unwrap() + at;
            let src = &page[at..end];
            let call = src.find("chord_col(chord)").or_else(|| src.find("chord_col(None)")).unwrap_or_else(|| panic!("{f} lacks the chord column"));
            let value = src.rfind(".child(value)").or_else(|| src.find("choice::segmented(")).unwrap();
            assert!(call > value, "{f}: the stroke comes before the value");
        }
        // An unbound action wears no badge.
        let keys = Keymap::defaults();
        assert!(keys.entries().iter().any(|b| b.action == ActionId::ToggleMute));
    }
}
