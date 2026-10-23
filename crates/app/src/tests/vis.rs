//! The visual cards' number fields and shared headers.

use crate::player::generators::GenPaint;
use crate::ui::icons::Icon;

#[test]
fn generator_numbers_have_ranges_and_colours_do_not() {
    assert_eq!(GenPaint::Width.num_range(), Some((0., 400., 1.)));
    assert_eq!(GenPaint::Radius.num_range().map(|r| r.1), Some(2000.));
    assert!(GenPaint::Fill.num_range().is_none() && GenPaint::Key(0).num_range().is_none());
}

#[test]
fn the_arrows_reuse_the_one_arrow_file() {
    assert_eq!(Icon::ArrowUp.name(), Icon::ArrowRight.name());
    assert_eq!(Icon::ArrowDown.name(), Icon::ArrowRight.name());
}

#[test]
fn the_cards_do_not_repeat_the_section_name_under_its_head() {
    for (name, src) in [
        ("effects", include_str!("../ui/cards/effects.rs")),
        ("transform", include_str!("../ui/cards/transform.rs")),
        ("color", include_str!("../ui/cards/color.rs")),
    ] {
        assert!(!src.contains("section_header_row(\""), "{name} repeats its head");
    }
}
