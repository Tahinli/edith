//! Leftovers of the quality sweep: the EQ band number fields, and the wording of editing copies.

use super::*;
use crate::menus::RowItem;
use crate::ui::cards::numedit::NumTarget;

#[test]
fn eq_numbers_read_the_way_the_card_shows_them() {
    assert_eq!(NumTarget::Gain.shown(4.5), "+4.5");
    assert_eq!(NumTarget::Gain.shown(-3.0), "\u{2212}3.0");
    assert_eq!(NumTarget::Width.shown(0.707), "0.71");
}

#[test]
fn the_proxy_switches_say_proxy_so_a_search_finds_them() {
    assert_eq!(RowItem::Proxy.label(), "Make editing copy");
    assert!(keymap::ActionId::ToggleProxies.label().to_lowercase().contains("proxies"));
    assert!(keymap::ActionId::ToggleAutoProxies.label().to_lowercase().contains("proxies"));
}

#[test]
fn only_a_subtitle_clip_gets_the_subtitle_style_section() {
    let src = include_str!("../ui/dock_stance.rs");
    assert!(src.contains("let substyle = match on_subtitle {"));
}

#[test]
fn freq_numbers_scrub_by_octaves_and_read_in_hz_or_khz() {
    assert_eq!(NumTarget::Freq.shown(1234.4), "1.2");
    assert_eq!(NumTarget::Freq.shown(432.4), "432");
    let up = NumTarget::Freq.scrub(100., 50., false);
    assert!((up - 200.).abs() < 1e-6);
    assert_eq!(NumTarget::Freq.scrub(100., -10_000., false), 20.);
}
