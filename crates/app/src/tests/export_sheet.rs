//! The export sheet's behaviour, as values: presets, the range radio, the
//! estimate, the geometry against the timeline, the overwrite answers and the
//! "More settings" disclosure (DESIGN §9).

use super::*;
use crate::export_ui::*;
use crate::ui::widgets::{load_folds_from, save_folds_to};
use engine::export::{HdrTransfer, ProResProfile};
use engine::export_presets::{self as presets, Category};

fn base() -> Picked {
    Picked {
        format: Format::Av1,
        bps: 3_000_000,
        audio_kbps: 128,
        size: None,
        depth: None,
        loudness: None,
        prores: Default::default(),
        hdr: Default::default(),
    }
}

#[test]
fn a_preset_tile_lays_the_engine_preset_over_the_settings_and_custom_follows_a_change() {
    let youtube = presets::by_id("youtube_1080p").expect("the table has it");
    let picked = picked_with(base(), youtube);
    assert_eq!(picked.format, youtube.format);
    assert_eq!(Some(picked.bps), youtube.bitrate);
    assert_eq!(picked.size, youtube.target.size);
    // AAC 384 is above the rates the engine writes; it takes the top one.
    assert_eq!(picked.audio_kbps, *AUDIO_KBPS.last().unwrap());
    assert!(preset_applies(picked, youtube));
    assert_eq!(matching_preset(picked, Some("youtube_1080p")).map(|p| p.id), Some("youtube_1080p"));
    // Any setting moved off the preset is Custom.
    let changed = Picked { bps: 9_000_000, ..picked };
    assert!(matching_preset(changed, Some("youtube_1080p")).is_none());
    let changed = Picked { size: Some((1280, 720)), ..picked };
    assert!(matching_preset(changed, Some("youtube_1080p")).is_none());
    // A sound preset carries its loudness target and drops the canvas.
    let podcast = presets::by_id("podcast").expect("the table has it");
    let picked = picked_with(Picked { size: Some((1920, 1080)), ..base() }, podcast);
    assert_eq!(picked.format, Format::M4a);
    assert_eq!(picked.loudness, podcast.loudness);
    assert_eq!(matching_preset(picked, None).map(|p| p.id), Some("podcast"));
    // Every row of the engine's table applies to itself: no tile can be
    // pressed and immediately read as Custom.
    for p in presets::presets() {
        assert!(preset_applies(picked_with(base(), p), p), "{} is never lit", p.id);
    }
}

#[test]
fn every_preset_tile_says_what_it_makes_under_a_named_group() {
    for p in presets::presets() {
        let said = preset_result(p, Some((1920, 1080)));
        assert!(!said.is_empty());
        match p.target.output {
            presets::Output::Video => assert!(said.contains('\u{d7}'), "{said}"),
            presets::Output::AudioOnly => assert!(!said.contains('\u{d7}'), "{said}"),
        }
    }
    assert_eq!(preset_result(presets::by_id("youtube_1080p").unwrap(), None), "1920\u{d7}1080 \u{b7} H.264");
    for c in [Category::Web, Category::Social, Category::Audio, Category::Archive] {
        assert!(presets::presets().iter().any(|p| p.target.category == c));
    }
}

#[test]
fn the_range_radio_names_both_lengths_and_says_why_the_marks_are_off() {
    let opts = range_options(Some((30, 150)), true, 900, 30.);
    assert_eq!(opts.whole, "30.0 s long");
    let marks = opts.marks.expect("valid marks are a choice");
    assert_eq!(marks, format!("1 s \u{2013} 5 s \u{b7} {} long", span_word(120, 30.)));
    assert_eq!(opts.marks_off, None);
    // No marks at all: off, with how to set them.
    let opts = range_options(None, false, 900, 30.);
    assert!(opts.marks.is_none());
    assert!(opts.marks_off.unwrap().contains("In and Out"));
    // Marks that lie past the end: off, and said differently.
    let opts = range_options(None, true, 900, 30.);
    assert!(opts.marks_off.unwrap().contains("past the end"));
}

#[test]
fn the_estimate_never_says_zero_for_a_range_with_something_in_it() {
    let one_frame = 1. / 30.;
    for &(format, ..) in SHEET_FORMATS.iter() {
        for fixed in [true, false] {
            let est = size_estimate(format, BPS_MIN, 128, true, one_frame, 48_000, fixed);
            assert!(est.low > 0 && est.high >= est.low, "{format:?}");
            let said = size_words(est);
            assert!(said.starts_with("about "), "{said}");
            assert!(!said.contains(" 0 ") && !said.ends_with(" 0 KB"), "{said}");
        }
    }
    // An hour at 8 Mb/s with 256 kbps of sound.
    let hour = size_estimate(Format::Mp4, 8_000_000, 256, true, 3600., 48_000, true);
    assert_eq!(size_words(hour), "about 3.73 GB");
    // A quality-driven encode is a span, not a single number.
    let span = size_estimate(Format::Av1, 8_000_000, 256, true, 60., 48_000, false);
    assert!(size_words(span).contains(" to "), "{}", size_words(span));
    // The time: measured or nothing at all, never a guess and never a fake zero.
    assert_eq!(export_time_words(0.01, Some(30.), false), "about 1 s to export");
    assert_eq!(export_time_words(100., Some(1.), false), "about 1 min 40 s to export");
    assert_eq!(export_time_words(100., None, true), "");
    assert_eq!(export_time_words(100., None, false), "");
}

#[test]
fn tall_and_square_sizes_are_offered_and_named_by_their_shape() {
    assert!(RESOLUTIONS.contains(&(1080, 1920)), "vertical is on the project list");
    assert!(RESOLUTIONS.contains(&(1080, 1080)), "square is on the project list");
    assert_eq!(size_name(1920, 1080), "1080p");
    assert_eq!(size_name(1080, 1920), "1080\u{d7}1920");
    assert_eq!(size_row_label(1080, 1920), "1080\u{d7}1920 (vertical 9:16)");
    assert_eq!(size_row_label(1080, 1080), "1080\u{d7}1080 (square)");
    assert_eq!(size_row_label(1280, 720), "720p");
    // The settings list shows them, the media's own shape cycled in beside them.
    let rows = pending_resolution_choices(None);
    assert!(rows.iter().any(|r| r.1.as_ref() == "1080\u{d7}1920 (vertical 9:16)"));
    let rows = resolution_choices((1080, 1920), (1080, 1920));
    assert_eq!(rows.iter().filter(|r| r.0 == Choice::Size(1080, 1920)).count(), 1);
    assert_eq!(rows.iter().find(|r| r.0 == Choice::Size(1080, 1920)).map(|r| r.2.to_string()), Some("matches your media".to_string()));
}

#[test]
fn a_tall_preset_names_its_size_in_pixels_not_as_a_height() {
    let shorts = presets::by_id("shorts_reels").expect("the Shorts preset exists");
    assert!(preset_spec(shorts, Some((1920, 1080))).starts_with("1080\u{d7}1920 "), "{}", preset_spec(shorts, None));
    let landscape = presets::by_id("youtube_1080p").expect("the YouTube preset exists");
    assert!(preset_spec(landscape, None).starts_with("1920\u{d7}1080 "), "{}", preset_spec(landscape, None));
}

#[test]
fn an_export_never_writes_over_a_source() {
    let src = std::path::Path::new("/nonexistent-edith-test/speech.mp4");
    assert!(is_a_source(src, &[src]));
    assert!(!is_a_source(std::path::Path::new("/nonexistent-edith-test/speech-edit.mp4"), &[src]));
}

#[test]
fn a_new_project_starts_on_the_footage_rate() {
    use crate::player::project_dialogs::{default_rate_index, NEW_RATES};
    let rate = |footage: Option<f64>| NEW_RATES[default_rate_index(footage)];
    assert_eq!(rate(Some(25.)), 25.);
    assert_eq!(rate(Some(23.976)), 24.);
    assert_eq!(rate(Some(29.97)), 30.);
    assert_eq!(rate(None), 30.);
    assert_eq!(rate(Some(12.)), 30.);
}

#[test]
fn the_window_title_follows_the_project_name() {
    use std::path::Path;
    let none = Path::new("");
    assert_eq!(crate::library_meta::shown_title("speech", none, None, false), "speech — edith");
    let saved = Path::new("/nonexistent-edith-test/speech.edith");
    assert_eq!(crate::library_meta::shown_title("Untitled", saved, None, true), "speech — edith");
    assert_eq!(crate::library_meta::shown_title("Untitled", saved, None, false), "Untitled — edith");
    assert_eq!(crate::library_meta::shown_title("Untitled", none, Some(saved), false), "speech — edith");
}

/// The three summary rows for the settings a preset describes.
fn rows_for(p: &presets::Preset, size: &str) -> [(&'static str, String); 3] {
    let next = picked_with(base(), p);
    summary_rows(
        &SummaryIn {
        gif: Default::default(),
            format: next.format,
            target: next.size.unwrap_or((1280, 720)),
            fps: 25.,
            bps: next.bps,
            audio_kbps: next.audio_kbps,
            sound: true,
            seat: EncoderSeat::Software,
            depth: next.depth,
            prores: next.prores,
            hdr: next.hdr,
            sound_bits: None,
            hevc_speed: Default::default(),
            size,
        },
        "~1 s",
    )
}

#[test]
fn the_summary_says_the_file_in_plain_words() {
    let p = presets::by_id("youtube_1080p").unwrap();
    let rows = rows_for(p, "about 150 MB");
    assert_eq!(rows[0], ("Video", "H.264 \u{b7} 1920\u{d7}1080 \u{b7} 25\u{a0}fps \u{b7} 8\u{a0}Mb/s".to_string()));
    assert_eq!(rows[1], ("Audio", "AAC stereo \u{b7} 320\u{a0}kb/s".to_string()));
    assert_eq!(rows[2], ("File", "MP4 \u{b7} Software \u{b7} ~150 MB \u{b7} ~1 s".to_string()));
}

#[test]
fn the_summary_matches_the_settings_of_every_preset() {
    for p in presets::presets() {
        let next = picked_with(base(), p);
        let [(_, video), (_, audio), (_, file)] = rows_for(p, "about 1 MB");
        assert!(file.starts_with(container_word(p.format)), "{}: {file}", p.id);
        assert!(file.contains("~1 MB"), "{}: {file}", p.id);
        match p.target.output {
            presets::Output::Video => {
                // An HDR file names its transfer in the codec's place, to fit the line.
                let head = if p.hdr == HdrTransfer::Sdr { codec_word(p.format) } else { hdr_word(p.hdr) };
                assert!(video.starts_with(head), "{}: {video}", p.id);
                let (w, h) = p.target.size.unwrap_or((1280, 720));
                assert!(video.contains(&format!("{w}\u{d7}{h}")), "{}: {video}", p.id);
                assert!(file.contains("Software"), "{}: {file}", p.id);
                // A ProRes profile is its own rate and its sound is PCM, not AAC.
                if p.format == Format::ProRes {
                    assert!(video.starts_with("ProRes 422") && audio.starts_with("PCM stereo"), "{}: {video} / {audio}", p.id);
                    continue;
                }
                let mbps = rate_label(next.bps).trim_end_matches(".0").to_string();
                assert!(video.contains(&format!("{mbps}\u{a0}Mb/s")), "{}: {video}", p.id);
                let sound = if p.format == Format::Webm { "Opus" } else { "AAC" };
                assert!(audio.starts_with(&format!("{sound} stereo")) && audio.contains(&next.audio_kbps.to_string()), "{}: {audio}", p.id);
            }
            presets::Output::AudioOnly => {
                assert!(!video.contains('\u{d7}') && !file.contains("Software"), "{}: {video} / {file}", p.id);
                assert!(audio.contains("stereo"), "{}: {audio}", p.id);
            }
        }
        // No row is long enough to wrap in the dock (about 40 characters).
        for line in [&video, &audio, &file] {
            assert!(line.chars().count() <= 44, "{}: {line}", p.id);
        }
    }
    let [_, (_, none), _] = summary_rows(&SummaryIn { sound: false, ..summary_in(Format::Flac) }, "");
    assert_eq!(none, "None");
    let [(_, ten), ..] = summary_rows(&SummaryIn { depth: Some(10), ..summary_in(Format::Av1) }, "");
    assert!(ten.starts_with("AV1 10-bit"), "{ten}");
}

#[test]
fn the_summary_names_the_prores_quality_the_hdr_and_the_sound_depth() {
    let pro = SummaryIn { prores: ProResProfile::Hq, sound_bits: Some(16), ..summary_in(Format::ProRes) };
    let [(_, video), (_, audio), (_, file)] = summary_rows(&pro, "");
    assert_eq!(video, "ProRes 422 HQ \u{b7} 1920\u{d7}1080 \u{b7} 30\u{a0}fps", "no Mb/s for a profile");
    assert_eq!(audio, "PCM stereo \u{b7} 16-bit");
    assert!(file.starts_with("MOV"), "{file}");
    let [_, (_, audio), _] = summary_rows(&SummaryIn { sound_bits: None, ..pro }, "");
    assert!(audio.ends_with("24-bit"), "{audio}");
    let hdr = SummaryIn { hdr: HdrTransfer::Pq, depth: Some(10), ..summary_in(Format::HevcMp4) };
    let [(_, video), ..] = summary_rows(&hdr, "");
    assert!(video.starts_with("HDR10 \u{b7}"), "{video}");
    let [(_, plain), ..] = summary_rows(&summary_in(Format::HevcMp4), "");
    assert!(plain.starts_with("HEVC \u{b7}"), "{plain}");
}

#[test]
fn the_prores_and_hdr_presets_lay_their_settings_over_the_sheet_and_back() {
    let master = presets::by_id("archive_prores").expect("the table has it");
    let picked = picked_with(base(), master);
    assert_eq!((picked.format, picked.prores, picked.hdr), (Format::ProRes, ProResProfile::Hq, HdrTransfer::Sdr));
    assert!(preset_applies(picked, master));
    let hdr = presets::by_id("hdr10_hevc").expect("the table has it");
    let picked = picked_with(picked, hdr);
    assert_eq!((picked.format, picked.hdr, picked.depth), (Format::HevcMp4, HdrTransfer::Pq, Some(10)));
    // A preset of another file kind clears the HDR tag the engine would refuse on it.
    let plain = picked_with(picked, presets::by_id("youtube_1080p").unwrap());
    assert_eq!(plain.hdr, HdrTransfer::Sdr);
    // Every format has a container and codec row to pick it by, ProRes included.
    assert!(COMBOS.iter().any(|&(c, k, f)| (c, k, f) == ("MOV", "ProRes", Format::ProRes)));
    assert_eq!(format_in_container("MOV", Format::Mp4), Some(Format::ProRes));
}

fn summary_in(format: Format) -> SummaryIn<'static> {
    SummaryIn {
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
        hevc_speed: Default::default(),
        sound_bits: None,
        size: "about 30 MB",
    }
}

#[test]
fn a_detail_flips_the_preset_box_to_custom_from_the_one_before() {
    assert_eq!(preset_box_text(Some("youtube_1080p"), Some("youtube_1080p")), "YouTube 1920\u{d7}1080");
    assert_eq!(preset_box_text(None, Some("youtube_1080p")), "Custom");
    assert_eq!(preset_box_text(None, None), "Custom");
    // The flip is a flag, not a comparison: a detail that happens to land on
    // another preset's numbers is still Custom (from the one picked).
    let player = fn_body_of("player/export.rs", "fn current_preset");
    assert!(player.contains("export_sheet.custom"), "{player}");
    let pick = fn_body_of("player/export.rs", "fn apply_export_pick");
    assert!(pick.contains("export_sheet.custom = true"), "{pick}");
    let choose = fn_body_of("player/export.rs", "fn choose_preset");
    assert!(choose.contains("export_sheet.custom = false"), "{choose}");
}

#[test]
fn every_container_and_codec_pair_is_one_engine_format_and_reachable() {
    for f in [
        Format::Mp4, Format::Av1, Format::Av1Mp4, Format::Hevc, Format::HevcMp4, Format::Webm, Format::Wav,
        Format::Flac, Format::Mp3, Format::Ogg, Format::M4a, Format::Opus,
    ] {
        assert!(COMBOS.iter().any(|(.., g)| *g == f), "{f:?} has no row");
        let (c, k, _) = *COMBOS.iter().find(|(.., g)| *g == f).unwrap();
        assert_eq!((c, k), (container_word(f), if f == Format::Wav { "PCM" } else { codec_word(f) }), "{f:?}");
        assert_eq!(format_in_container(c, f), Some(f), "{f:?}");
    }
    // AV1 in MP4 moved to MKV stays AV1; H.264 moved to MKV lands on its first.
    assert_eq!(format_in_container("MKV", Format::Av1Mp4), Some(Format::Av1));
    assert_eq!(format_in_container("MKV", Format::Mp4), Some(Format::Hevc));
    assert_eq!(format_in_container("WAV", Format::Mp4), Some(Format::Wav));
}

#[test]
fn the_details_rows_are_direct_choices_and_the_button_stays_pinned() {
    let card = src_text("ui/cards/export.rs");
    let more = fn_body_of("ui/cards/export.rs", "fn sheet_more");
    for row in [
        "Container", "Codec", "Encoder", "Bitrate", "Resolution", "Frame rate", "Bit depth", "Sample rate",
        "Channels", "Loudness", "Chapters", "Captions", "Quality", "HDR", "Sound depth",
    ] {
        assert!(more.contains(&format!("\"{row}\"")), "Details lost {row}");
    }
    // Every list row opens a list of every value; none steps one on per click.
    for row in [
        "Container", "Codec", "Seat", "Bitrate", "Size", "Depth", "Profile", "Hdr", "SoundDepth", "Kbps", "Loudness",
        "Captions",
    ] {
        assert!(more.contains(&format!("ExportRow::{row}")), "{row} has no list");
    }
    assert!(!more.contains("cycle") && !more.contains("step"), "a Details row steps on click");
    // The footer is outside the scrolling body, so Export is reachable at 720.
    let face = fn_body_of("ui/cards/export.rs", "fn sheet_settings");
    assert!(face.contains(".overflow_y_scroll()") && face.contains("sheet_footer("), "{face}");
    assert!(face.find(".overflow_y_scroll()").unwrap() < face.find("sheet_footer(").unwrap());
    assert!(card.contains("fn sheet_footer") && card.contains(".flex_none()"));
    // Details start closed, and the size estimate is on the simple layer.
    assert!(face.contains("more.then(|| self.sheet_more(cx))"));
    assert!(fn_body_of("ui/cards/export.rs", "fn sheet_summary").contains("size_words(size)"));
}

/// A function's source, from its signature to the next top-level or method item.
fn fn_body_of(file: &str, sig: &str) -> String {
    let text = src_text(file);
    let at = text.find(sig).unwrap_or_else(|| panic!("{sig} not in {file}"));
    let rest = &text[at..];
    let end = ["\n    pub(crate) fn ", "\n    fn ", "\nfn ", "\npub(crate) fn "]
        .iter()
        .filter_map(|m| rest[10..].find(m))
        .min()
        .map_or(rest.len(), |e| e + 10);
    rest[..end].to_string()
}

#[test]
fn the_warnings_say_upscaling_starved_bitrate_and_a_dropped_picture() {
    let ctx = WarnCtx {
        format: Format::Mp4,
        target: (3840, 2160),
        project: (1920, 1080),
        fps: 30.,
        video_bps: 1_000_000,
        timeline_has_picture: true,
        timeline_has_sound: true,
        seconds: 10.,
    };
    let said = warnings(&ctx).join("\n");
    assert!(said.contains("Stretched up"), "{said}");
    assert!(said.contains("Low bitrate"), "{said}");
    let fine = WarnCtx { target: (1920, 1080), video_bps: 8_000_000, ..ctx };
    assert!(warnings(&fine).is_empty(), "{:?}", warnings(&fine));
    let song = WarnCtx { format: Format::Mp3, ..fine };
    assert!(warnings(&song).join("").contains("picture is left out"));
    let empty = WarnCtx { seconds: 0., ..fine };
    assert!(warnings(&empty).join("").contains("nothing to export"));
}

/// The hard rule: nothing covers the timeline. Export is a dock tab now, in
/// flow beside Media and Clip: the panel has no absolute box of its own and
/// the old centred sheet's rectangle is gone.
#[test]
fn the_export_panel_is_a_dock_tab_in_flow() {
    let card = src_text("ui/cards/export.rs");
    let panel = fn_body("export_panel");
    assert!(!panel.contains("absolute()") && !card.contains("SCRIM"), "the panel floats");
    assert!(!src_text("export_ui.rs").contains("fn sheet_rect("));
    let dock = src_text("ui/dock_stance.rs");
    for tab in ["dock-tab-src", "dock-tab-clip", "dock-tab-export"] {
        assert!(dock.contains(&format!("\"{tab}\"")), "{tab} is not a tab");
    }
    let (src, clip, export) = (
        dock.find("dock_tab(\"dock-tab-src\"").unwrap(),
        dock.find("dock_tab(\"dock-tab-clip\"").unwrap(),
        dock.find("dock_tab(\"dock-tab-export\"").unwrap(),
    );
    assert!(src < clip && clip < export, "tabs are Media | Clip | Export");
    // The full-width primary button is the footer's only control, and the
    // settings face has no "More below" fade.
    assert!(!card.contains("More below"));
    let footer = &card[card.find("fn sheet_footer(").unwrap()..];
    let footer = &footer[..footer.find("\n}\n").unwrap()];
    assert!(footer.contains("\"export-confirm\"") && !footer.contains("export-dismiss"));
}

#[test]
fn a_preset_row_reads_size_codec_and_rate_on_one_line() {
    let p = engine::export_presets::presets()
        .iter()
        .find(|p| p.target.output == engine::export_presets::Output::Video && p.bitrate.is_some())
        .expect("a video preset with a rate");
    let spec = preset_spec(p, Some((1920, 1080)));
    assert!(spec.contains("\u{d7}") && spec.contains(" \u{b7} ") && spec.contains("Mb/s"), "{spec}");
    assert!(!spec.contains('\n'));
}

/// The sheet's body says when more of it is below: on while the body can
/// still scroll down, off at the end and when it does not scroll at all.
#[test]
fn the_sheet_body_says_more_is_below_until_its_end() {
    assert!(more_below_shows(0., 300.));
    assert!(more_below_shows(-150., 300.));
    assert!(!more_below_shows(-300., 300.));
    assert!(!more_below_shows(0., 0.));
    // One fade for both scrolled bodies: the sheet's and the Settings rows'.
    for (file, source, track) in [
        (
            "ui/cards/export.rs",
            include_str!("../ui/cards/export.rs"),
            ".track_scroll(&self.export_body_scroll)",
        ),
        (
            "ui/settings_stance.rs",
            include_str!("../ui/settings_stance.rs"),
            ".track_scroll(&player.settings_rows_scroll)",
        ),
    ] {
        assert!(source.contains(track), "{file} lost its tracked scroll");
        assert!(
            source.contains("crate::ui::widgets::more_below(") || source.contains("plate_more_below("),
            "{file} no longer draws the shared more-below fade"
        );
        assert!(!source.contains("fn more_below("), "{file} grew its own copy of the fade");
    }
}

#[test]
fn keep_both_writes_beside_the_file_and_the_fields_refuse_in_plain_words() {
    let taken = |p: &Path| {
        [PathBuf::from("/v/film.mp4"), PathBuf::from("/v/film (2).mp4")].contains(&p.to_path_buf())
    };
    assert_eq!(unique_path(Path::new("/v/film.mp4"), taken), PathBuf::from("/v/film (3).mp4"));
    assert_eq!(unique_path(Path::new("/v/new.mp4"), taken), PathBuf::from("/v/new.mp4"));
    assert_ne!(Overwrite::Replace, Overwrite::KeepBoth);
    assert_eq!(parse_stem("  holiday "), Ok("holiday".to_string()));
    assert!(parse_stem("").is_err() && parse_stem("a/b").is_err());
    let home = std::env::temp_dir();
    assert_eq!(parse_folder("~", home.to_str()), Ok(home.clone()));
    assert_eq!(parse_folder("/no/such/folder/edith", None), Err("That folder does not exist."));
    // The overwrite question's three answers are on the sheet's keys and buttons.
    let handler = fn_body("export_moment_key");
    for (key, answer) in [
        ("\"enter\"", "Overwrite::Replace"),
        ("\"k\"", "Overwrite::KeepBoth"),
        ("ESCAPE", "Overwrite::Cancel"),
    ] {
        assert!(handler.contains(&format!("{key} => self.answer_overwrite({answer}, cx)")));
    }
}

#[test]
fn more_settings_starts_closed_and_remembers_being_opened() {
    let mut folds = std::collections::BTreeSet::new();
    assert!(!folds.contains(EXPORT_MORE_FOLD), "the disclosure opens closed");
    assert!(toggle_fold(&mut folds, EXPORT_MORE_FOLD));
    let path = std::env::temp_dir().join(format!("edith-folds-{}", std::process::id()));
    save_folds_to(&folds, &path);
    assert!(load_folds_from(&path).contains(EXPORT_MORE_FOLD));
    assert!(!toggle_fold(&mut folds, EXPORT_MORE_FOLD));
    save_folds_to(&folds, &path);
    assert!(!load_folds_from(&path).contains(EXPORT_MORE_FOLD));
    std::fs::remove_file(path).ok();
}

/// The trial write that prices the time: software seat only (never Auto or
/// Hardware, whatever the sheet's encoder is set to), a bounded range, a file
/// in the temporary folder, deleted when it lands and when a real export
/// takes over, and started on the engine's worker rather than run here.
#[test]
fn the_timing_trial_is_software_only_temporary_and_cleaned_up() {
    for &(format, ..) in SHEET_FORMATS.iter() {
        let s = pace_bench_settings(8_000_000, format, 256, Some((1280, 720)), Some(10), 30);
        assert_eq!(s.seat, EncoderSeat::Software, "{format:?}");
        assert_eq!(s.range, Some((0, 30)));
        let out = pace_bench_path(format);
        assert!(out.starts_with(std::env::temp_dir()), "{}", out.display());
        assert_eq!(out.extension().and_then(|e| e.to_str()), Some(format.ext()));
    }
    let body = fn_body("cache_export_pace");
    assert!(body.contains("pace_bench_settings(") && body.contains("pace_bench_path("));
    assert!(!body.contains("self.encoder_seat()") && !body.contains("EncoderSeat::Auto"));
    assert!(body.contains("std::fs::remove_file(out)"), "a landed trial is not deleted");
    assert!(body.contains("session.export_to_with(&out, &settings)"));
    let start = fn_body("start_export");
    assert!(
        start.contains("bench.handle.cancel()") && start.contains("std::fs::remove_file(&bench.out)"),
        "a real export does not stop and delete the trial"
    );
}

/// The panel's body scrolls with the wheel and the keys (up/down/page/home/
/// end), and only the dock's Clip-tab frame decides where a card opens.
#[test]
fn the_export_panel_scrolls_by_wheel_and_keys_and_cards_share_one_frame() {
    assert!(src_text("ui/cards/export.rs").contains(".track_scroll(&self.export_scroll)"));
    let keys = fn_body("export_moment_key");
    for key in ["\"up\"", "\"down\"", "\"pageup\"", "\"pagedown\"", "\"home\"", "\"end\""] {
        assert!(keys.contains(&format!("{key} =>")), "{key} does not scroll the panel");
    }
    // A card closes back to the Clip list, never to Media; only a docked
    // card is in flow; the head has no jargon glyph; label-only tooltips go.
    assert!(fn_body("close_card").contains("self.dock_src_active = false"));
    let common = src_text("ui/cards/common.rs");
    assert!(!common.contains("fills the room") && !common.contains("card-head-maximize"));
    assert!(common.contains(".when(!chord.is_empty(), |d| {"));
    assert!(src_text("ui/dock_stance.rs").contains("\"dock-verb-tools\""));
    assert!(!fn_body("toggle_maximize").contains("!self.card_maximized"));
}

/// A slider's knob sits where its readout says: every colour and transform
/// band maps value to fraction through its own range, neutral values land
/// inside the track (brightness 0 mid-way), and the card rows all use
/// `slider_fraction`, the one mapping.
#[test]
fn a_slider_knob_maps_its_value_through_the_same_range_as_the_readout() {
    use crate::transform_ui::*;
    use crate::ui::widgets::slider_fraction;
    let neutral = engine::color::ColorParams::default();
    for (band, &(_, lo, hi)) in COLOR_BANDS.iter().enumerate() {
        let mut p = neutral;
        let v = *crate::export_ui::band_mut(&mut p, band);
        let at = slider_fraction(v, lo, hi);
        assert!((at * (hi - lo) + lo - v).abs() < 1e-5, "band {band}");
        assert!(at > 0. && at < 1., "neutral band {band} sits on a track end");
        assert_eq!(slider_fraction(lo, lo, hi), 0.);
        assert_eq!(slider_fraction(hi, lo, hi), 1.);
    }
    // Every transform slider, rotation included: the neutral placement sits
    // inside the track, a signed turn puts 0 degrees mid-track and -90 at a
    // quarter, and a drag reads back through the same range the row draws.
    let identity = engine::transform::TransformParams::default();
    for band in 0..TRANSFORM_BANDS.len() {
        let (lo, hi) = slider_range(band);
        let v = *transform_band_mut(&mut identity.clone(), band);
        let at = knob_fraction(band, v);
        assert!((0.0..=1.0).contains(&at), "band {band}");
        if band == ROTATE_BAND {
            assert_eq!(knob_fraction(band, 0.), 0.5);
            assert_eq!(knob_fraction(band, 270.), 0.25);
            assert_eq!(knob_fraction(band, 90.), 0.75);
        } else {
            assert_eq!(knob_fraction(band, lo), 0.);
            assert_eq!(knob_fraction(band, hi), 1.);
            assert!((at * (hi - lo) + lo - v).abs() < 1e-4 || at == 0. || at == 1., "band {band}");
        }
    }
    assert_eq!(slider_fraction(0., -1., 1.), 0.5);
    assert_eq!(slider_fraction(1., 0., 2.), 0.5);
    for card in ["color.rs", "transform.rs"] {
        let text = src_text(&format!("ui/cards/{card}"));
        assert!(text.contains("fraction: slider_fraction(") || text.contains("let fraction = slider_fraction(") || text.contains("fraction: knob_fraction("), "{card}");
    }
    // Colour lists all four controls, rotation is one row, and hint text is sans.
    assert!(src_text("ui/cards/color.rs").contains("(0..COLOR_BANDS.len())"));
    let help = &src_text("ui/cards/common.rs");
    let help = &help[help.find("pub(super) fn dark_help").unwrap()..];
    assert!(!help[..help.find("\n}\n").unwrap()].contains("mono("));
}

#[test]
fn an_opus_export_is_called_opus_everywhere_and_never_aac() {
    assert_eq!(Format::Opus.ext(), "opus");
    assert_eq!(codec_word(Format::Opus), "Opus");
    assert_eq!(sheet_format_name(Format::Opus), "Opus");
    assert_eq!(format_label(Format::Opus), "Opus");
    assert!(SHEET_FORMATS.iter().any(|(f, ..)| *f == Format::Opus));
    let line = summary_rows(&summary_in(Format::Opus), "")[1].1.clone();
    assert!(line.contains("Opus") && !line.contains("AAC"), "{line}");
    let tile = presets::by_id("podcast_opus").expect("the table has it");
    assert_eq!(tile.format, Format::Opus);
    let result = preset_result(tile, None);
    assert!(result.contains("Opus") && !result.contains("AAC"), "{result}");
}

#[test]
fn match_project_is_the_first_preset_and_custom_waits_for_a_change() {
    assert_eq!(preset_box_text(Some(MATCH_PROJECT), None), "Match project");
    assert_eq!(preset_box_text(None, Some(MATCH_PROJECT)), "Custom");
    let now = fn_body_of("player/export.rs", "fn preset_now");
    assert!(now.contains("export_sheet.custom") && now.contains("MATCH_PROJECT") && now.contains("DEFAULT_AUDIO_KBPS"));
    let rows = fn_body_of("player/export.rs", "fn export_choices");
    assert!(rows.find("MATCH_PROJECT").unwrap() < rows.find("presets::presets()").unwrap(), "it is not listed first");
    let choose = fn_body_of("player/export.rs", "fn choose_preset");
    assert!(choose.contains("self.custom_bps = 0") && choose.contains("Format::Mp4"));
}

#[test]
fn the_warning_has_one_reserved_line_and_read_only_rows_are_plain_text() {
    let summary = fn_body_of("ui/cards/export.rs", "fn sheet_summary");
    assert!(summary.contains("\"export-warning\"") && summary.contains(".h(px(18.))"), "the warning line is not reserved");
    let said = warnings(&WarnCtx {
        format: Format::Mp4,
        target: (1920, 1080),
        project: (1280, 720),
        fps: 25.,
        video_bps: 8_000_000,
        timeline_has_picture: true,
        timeline_has_sound: true,
        seconds: 2.,
    });
    assert_eq!(said, vec!["Stretched up from 1280\u{d7}720 \u{2014} won't look sharper".to_string()]);
    let more = fn_body_of("ui/cards/export.rs", "fn sheet_more");
    assert!(more.contains("info_note(") && !more.contains("info_box"));
    let field = fn_body_of("ui/cards/export.rs", "fn field_row");
    assert!(!field.contains("mono("), "the Name and Folder fields are mono again");
}

#[test]
fn a_point_on_the_timeline_is_said_in_plain_seconds() {
    use crate::export_ui::point_word;
    assert_eq!(point_word(0, 30.), "0 s");
    assert_eq!(point_word(30, 30.), "1 s");
    assert_eq!(point_word(45, 30.), "1.5 s");
    assert_eq!(point_word(30 * 65, 30.), "1 min 5 s");
}

#[test]
fn a_clicked_field_keeps_its_text_and_a_tabbed_one_selects_it_all() {
    use crate::player::text_field::TextField;
    assert!(!TextField::clicked("name").all);
    assert!(TextField::tabbed("name").all);
}

#[test]
fn a_refused_export_name_or_folder_is_never_dropped_by_a_click_elsewhere() {
    // The click on Export blurs the fields first: a refused edit must stay open with its note,
    // and the export start must take the same edit and refuse to run on the old folder.
    let blur = include_str!("../player/blur.rs");
    let body = blur
        .split_once("fn blur_text_fields")
        .and_then(|(_, rest)| rest.split_once("if self.cue_edit"))
        .map(|(body, _)| body)
        .expect("blur_text_fields takes the export field first");
    assert!(body.contains("self.commit_field();"), "the typed text is taken");
    assert!(!body.contains("export_sheet.field = None"), "a refused edit is not dropped: {body}");
    assert!(!body.contains("export_sheet.note = None"), "the refusal note stays: {body}");
    let export = include_str!("../player/export.rs");
    let start = export
        .split_once("pub(crate) fn start_export")
        .map(|(_, rest)| rest)
        .expect("start_export exists");
    assert!(
        start.split_once("let settings").is_some_and(|(head, _)| head.contains("commit_pending_field()")),
        "the export takes the pending edit before anything is written"
    );
}

/// A typed name is a file under the format's extension, except a numbered run, which is a folder
/// named as typed: `gui_seq`, never `gui_seq.png`.
#[test]
fn a_typed_name_for_a_numbered_run_is_a_folder_without_an_extension() {
    assert_eq!(typed_name("gui_seq", Format::PngSequence), "gui_seq");
    assert_eq!(typed_name("take.v2", Format::PngSequence), "take.v2");
    assert_eq!(typed_name("take", Format::Still), "take.png");
    assert_eq!(typed_name("take", Format::Gif), "take.gif");
}
