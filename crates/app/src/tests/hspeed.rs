//! The HEVC encoding-speed choice in the export sheet: what the summary says,
//! that it is remembered, and that it reaches the engine's settings.

use super::*;
use crate::export_ui::*;
use engine::export::HevcSpeed;

fn summary(format: Format, speed: HevcSpeed) -> String {
    let i = SummaryIn {
        gif: Default::default(),
        format,
        target: (1920, 1080),
        fps: 30.,
        bps: 6_000_000,
        audio_kbps: 128,
        sound: true,
        seat: EncoderSeat::Software,
        depth: None,
        prores: Default::default(),
        hdr: Default::default(),
        hevc_speed: speed,
        sound_bits: None,
        size: "about 30 MB",
    };
    summary_rows(&i, "")[2].1.clone()
}

#[test]
fn the_summary_names_a_quicker_speed_and_says_nothing_for_the_default() {
    assert_eq!(summary(Format::HevcMp4, HevcSpeed::Default), "MP4 \u{b7} Software \u{b7} ~30 MB");
    assert!(summary(Format::HevcMp4, HevcSpeed::Fast).contains("Software \u{b7} Faster \u{b7}"));
    assert!(summary(Format::HevcMp4, HevcSpeed::Faster).contains("Software \u{b7} Fastest \u{b7}"));
    // Other codecs have no such choice, whatever the sheet remembers.
    assert!(!summary(Format::Mp4, HevcSpeed::Faster).contains("Fastest"));
}

#[test]
fn the_speed_is_remembered_with_the_other_export_defaults() {
    for s in HEVC_SPEEDS {
        let p = ExportPrefs { format: Format::HevcMp4, hevc_speed: s, ..ExportPrefs::default() };
        assert_eq!(parse_export_prefs(&render_export_prefs(&p)).hevc_speed, s);
    }
    assert_eq!(parse_export_prefs("hevc-mp4\n192\n").hevc_speed, HevcSpeed::Default);
    assert_eq!(parse_export_prefs("hevc-mp4\n192\nhevc-speed warp\n").hevc_speed, HevcSpeed::Default);
}

#[test]
fn the_speed_travels_from_the_sheet_to_the_export_and_the_queue() {
    let text = include_str!("../player/export.rs");
    let at = text.find("fn export_settings_now").expect("export_settings_now");
    let body = &text[at..text[at..].find("\n    }\n").map_or(text.len(), |n| at + n)];
    assert!(body.contains("settings.hevc_speed") && body.contains("self.export_sheet.hevc_speed"));
    assert!(body.contains("is_hevc()"), "only an HEVC file carries it");
    // A queued job is added from the same settings.
    assert!(text.contains("let settings = self.export_settings_now();"));
    assert!(text.contains("ExportPick::HevcSpeed(s) => self.export_sheet.hevc_speed = s"));
}
