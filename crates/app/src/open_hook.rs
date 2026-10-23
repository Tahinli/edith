//! `EDITH_OPEN`: a start-up hook that opens panels, so a headless screenshot
//! run can reach every one of them without synthesising clicks. Unset, it does
//! nothing at all. Comma-separated: `export`, `settings`, `palette`, `keys`,
//! `markers`, `menu:<file|edit|clip|timeline|view|help>` and `clip:<card>`
//! (`color`, `transform`, `speed`, `eq`, `mix`, `silence`), the last of which
//! first selects the first timeline clip the card can act on.

use crate::ui::menubar::Title;
use engine::project::LaneKind;
use gpui::{Context, Timer};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Card {
    Color,
    Transform,
    Speed,
    Eq,
    Mix,
    Silence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Open {
    Export,
    Settings,
    Palette,
    Keys,
    Markers,
    Menu(Title),
    Clip(Card),
}

/// The values `EDITH_OPEN` names; anything unknown is skipped (and said so).
pub(crate) fn parse(spec: &str) -> Vec<Open> {
    spec.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter_map(|v| {
            let found = match v.split_once(':') {
                None => match v {
                    "export" => Some(Open::Export),
                    "settings" => Some(Open::Settings),
                    "palette" => Some(Open::Palette),
                    "keys" => Some(Open::Keys),
                    "markers" => Some(Open::Markers),
                    _ => None,
                },
                Some(("menu", name)) => match name {
                    "file" => Some(Title::File),
                    "edit" => Some(Title::Edit),
                    "clip" => Some(Title::Clip),
                    "timeline" => Some(Title::Timeline),
                    "playback" => Some(Title::Playback),
                    "view" => Some(Title::View),
                    "help" => Some(Title::Help),
                    _ => None,
                }
                .map(Open::Menu),
                Some(("clip", card)) => match card {
                    "color" => Some(Card::Color),
                    "transform" => Some(Card::Transform),
                    "speed" => Some(Card::Speed),
                    "eq" => Some(Card::Eq),
                    "mix" => Some(Card::Mix),
                    "silence" => Some(Card::Silence),
                    _ => None,
                }
                .map(Open::Clip),
                _ => None,
            };
            if found.is_none() {
                eprintln!("EDITH_OPEN: unknown value {v:?}, skipped");
            }
            found
        })
        .collect()
}

impl crate::Player {
    /// Waits for the argv file to land (a launch with none lands at once), then
    /// applies `wanted` once. Gives up waiting after a minute.
    pub(crate) fn start_open_hook(&mut self, wanted: Vec<Open>, cx: &mut Context<Self>) {
        if wanted.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            for _ in 0..300 {
                Timer::after(Duration::from_millis(200)).await;
                let ready = this
                    .update(cx, |p, _| {
                        p.importing.is_none() && p.opening.is_none() && p.imports.is_empty()
                    })
                    .unwrap_or(true);
                if ready {
                    break;
                }
            }
            // One more beat for the fit and the first frame.
            Timer::after(Duration::from_millis(400)).await;
            this.update(cx, |p, cx| p.apply_open(&wanted, cx)).ok();
        })
        .detach();
    }

    fn apply_open(&mut self, wanted: &[Open], cx: &mut Context<Self>) {
        for open in wanted {
            match *open {
                Open::Export => self.open_export(cx),
                Open::Settings => self.open_settings(cx),
                Open::Palette => self.show_actions(cx),
                Open::Keys => self.show_keys(cx),
                Open::Markers => self.open_markers(cx),
                Open::Menu(title) => {
                    self.menu_open = Some(title);
                    cx.notify();
                }
                Open::Clip(card) => {
                    let wants_sound = matches!(card, Card::Eq | Card::Mix | Card::Silence);
                    let pick = self.session.as_ref().and_then(|s| {
                        let lanes = s.lanes();
                        let by_kind = |kind| {
                            lanes
                                .iter()
                                .copied()
                                .find(|l| l.kind == kind && !s.lane_clips(*l).is_empty())
                        };
                        let (first, second) = match wants_sound {
                            true => (LaneKind::Audio, LaneKind::Video),
                            false => (LaneKind::Video, LaneKind::Audio),
                        };
                        by_kind(first).or_else(|| by_kind(second))
                    });
                    if let Some(lane) = pick {
                        self.select((lane, 0), cx);
                    }
                    // The cards mount in the dock's Clip tab; a fresh profile
                    // starts on Media, where the card would stay out of shot.
                    self.dock_src_active = false;
                    match card {
                        Card::Color => self.open_color(cx),
                        Card::Transform => self.open_transform(cx),
                        Card::Speed => self.open_speed(cx),
                        Card::Eq => self.open_eq(cx),
                        Card::Mix => self.open_mix(pick, cx),
                        Card::Silence => self.open_silence(cx),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_menu_and_clip_values() {
        assert_eq!(
            parse("export, settings,palette,keys,markers"),
            [Open::Export, Open::Settings, Open::Palette, Open::Keys, Open::Markers]
        );
        assert_eq!(parse("menu:clip"), [Open::Menu(Title::Clip)]);
        assert_eq!(
            parse("clip:color,clip:eq"),
            [Open::Clip(Card::Color), Open::Clip(Card::Eq)]
        );
    }

    #[test]
    fn unknown_and_empty_values_are_skipped() {
        assert!(parse("").is_empty());
        assert!(parse(",,").is_empty());
        assert_eq!(parse("bogus,menu:nope,clip:,palette"), [Open::Palette]);
    }

    /// A clip card mounts in the dock's Clip tab, so `clip:<card>` must leave
    /// the Media tab or the shot shows no card at all.
    #[test]
    fn clip_cards_switch_the_dock_to_the_clip_tab() {
        let src = include_str!("open_hook.rs");
        let branch = src
            .split("Open::Clip(card) => {")
            .nth(1)
            .expect("the Clip branch");
        let before_card = branch.split("match card {").next().unwrap();
        assert!(before_card.contains("self.dock_src_active = false;"));
    }
}
