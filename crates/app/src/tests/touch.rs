//! The EQ frequency field reads like an NLE's, and the Deinterlace row and the Properties plate
//! say things plainly and sit where they belong.

use super::*;
use crate::ui::cards::numedit::NumTarget;

#[test]
fn a_frequency_reads_in_hz_below_a_kilohertz_and_in_khz_above() {
    assert_eq!(NumTarget::Freq.shown(80.), "80");
    assert_eq!(NumTarget::Freq.unit(80.), "Hz");
    assert_eq!(NumTarget::Freq.shown(11093.), "11.1");
    assert_eq!(NumTarget::Freq.unit(11093.), "kHz");
    assert_eq!(NumTarget::Freq.shown(1000.), "1.0");
    assert_eq!(NumTarget::Freq.shown(20000.), "20.0");
    assert_eq!(NumTarget::Freq.unit(999.6), "kHz");
    assert_eq!(NumTarget::Freq.shown(999.6), "1.0");
}

#[test]
fn the_deinterlace_label_has_no_jargon_and_the_properties_plate_sits_by_its_row() {
    let card = src_text("ui/cards/transform.rs");
    assert!(card.contains(".child(\"Deinterlace\")"));
    assert!(card.contains("This video's lines come bottom field first"));
    let lib = src_text("ui/library.rs");
    assert!(lib.contains("crate::ui::anchor::row_of(&path, menu.stream)"));
}
