//! The window's tests, out of `main.rs` and into a module of their own: the
//! file was sixteen thousand lines and two thirds of them were this. What
//! stays here is what every part of it shares -- the imports, the fixtures,
//! and the source scans the guards read.

mod cards;
mod editing;
mod layout;
mod media;
mod view;

use super::{
    ACCENT_PRIMARY, AUDIO_KBPS, BG_RAISED, COLOR_BANDS, COLOR_BAR_W, COLOR_STEP, COLOR_W,
    CONTROL_H, Clip, Ctx, DEFAULT_AUDIO_KBPS, EQ_BANDS_MAX, EQ_CURVE_STEPS, EQ_FFT, EQ_FREQ_HIGH,
    EQ_FREQ_LOW, EQ_FREQ_STEP, EQ_GAIN_LIMIT, EQ_GRAPH_H, EQ_HANDLE, EQ_Q_HIGH, EQ_Q_LOW,
    EQ_Q_STEP, EQ_SPECTRUM_DB, EQ_TICKS, EQ_W_MAX, ESCAPE, EXPORT_DONE, EXPORT_W, Enable,
    EncoderSeat, ExportPreset, FORMATS, Format, HEADER_H, HIST_BINS, HIST_H, HIST_SAMPLES, HIT_MIN,
    KEYS_ROW_H, KEYS_W, KeyRow, LABEL_H, LANE_H, LANES_MAX, LATE_RESYNC, LIBRARY_MAX_W,
    LIBRARY_MIN_W, Lane, MB_FLOOR, MBPS_DIGITS, MBPS_MAX, MBPS_MIN, MENU_ITEMS, MENU_PAD,
    MENU_ROW_H, MENU_W, NO_FILE, NOTICES_MAX, NumberEdit, OVERLAID, PANEL_H, Quality, RESYNC_GAP,
    ROW_ITEMS, RULER_HIT_H, RowCtx, RowItem, SCROLL_NOTCH_SHARE, SILENCE_ROWS, SOURCE_TINTS,
    SPEED_PRESETS, SPEED_STEP, STATUS_ERROR, STATUS_SUCCESS, STATUS_WARNING, SUB_PLAN_CHARS,
    Source, Speed, StreamInfo, TIMELINE_SHARE, Transport, VOLUME_W, Volume, WAVE_BPS, WAVE_COL,
    WAVE_COLS_MAX, Wave, ZOOM_MIN_FRAMES, active_lane, band_label, bitrate_detail, bitrate_refusal,
    clipboard_after_remove, color_snap, commit_mbps, containers, enable, enable_lane, envelope,
    eq_card_w, eq_freq, eq_freq_label, eq_graph_h, eq_spectrum, eq_x, eq_y, estimated_bytes,
    export_path, export_settings, format_line, format_refusal, fps_choices, fps_label, frac_along,
    frac_down, frame_at, frame_rate_ladder, histogram, inserted_band, is_project, keymap,
    keys_rows, lanes_h, marked, menu_at, menu_items, menu_rows_h, next_container, normalise,
    nothing_to_play, notice_tone, panel_h, project_path, proxies_to_start, push_notice, px_below,
    retarget, row_enable, row_items, scrub_due, secs_label, should_resync, silence_rate,
    size_label, snap_cue, snap_marks, snapped, source_tint, span_partner, speed_at,
    sub_pick_after_removal, subtitle_plan, summary_head, summary_tail, timecode, timeline_fixed_h,
    tip_may_paint, transport, typed, unseen_paths, unseen_sources, whole_take, window_title,
};

use super::{
    ActionId, Choice, EDGE_W, ETA_SPAN, Edge, FITS, FRAME_RATES, IMPORT_STALL, Import, ImportStage,
    Landing, LaneKind, PPS_DEFAULT, PPS_MIN, Preset, RESOLUTIONS, Repeat, SAMPLE_RATES, SEEK_STALL,
    Scale, ScanKey, ScanPlan, SilenceScan, SubClip, View, ZOOM_OUT_MARGIN, ZOOM_STEP, arrival,
    audio_rate_choices, av1_hw_warning, clip_width, clock, encoder_choices, encoder_label,
    eta_secs, file_name, file_uri, fit_choices, frames_of_us, full_scan_key, import_line, landing,
    lane_refuses, launch_queue, library_rows, live_idx, next_fit, next_resolution, note_progress,
    pending_fps_choices, pending_resolution_choices, px_along, read_ahead, repeats,
    resolution_choices, resolution_ladder, sample_rate_choices, scan_plan, seek_line,
    silence_cached, silence_line, slice_whole_levels, source_secs, stash_or_write, tone_choices,
    tone_label, trimmed_clip, trimmed_sub, trims, unscannable, unusable,
};

use super::{
    SUB_BOTTOM, SUB_CUE_MIN_W, SUB_LINE_H, SUB_TEXT, Subs, carries_subtitles, cue_box, cues_at,
    file_tint, is_subtitle, lang_human, sub_bottom, sub_pick_name, subtitle_detail,
    subtitle_notice, subtitle_rows, subtitle_tail, subtitle_toggle_notice, walk_subtitles,
};

use engine::PlaybackSession;

use engine::scale::FitPolicy;

use gpui::{Bounds, Pixels, point, px, size};

use std::collections::HashMap;

use std::path::{Path, PathBuf};

use std::time::{Duration, Instant};

use std::sync::{LazyLock, Mutex, MutexGuard};

static CONFIG_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn config_env_lock() -> MutexGuard<'static, ()> {
    CONFIG_ENV_LOCK
        .lock()
        .expect("config environment lock poisoned")
}

/// Every line of the window's source there is, as one string. The regions have
/// moved house twice -- one `main.rs`, then `ui/`, then the tests out of it --
/// and a scan that names its files passes a rule by simply not looking where
/// the code went. So the files are found rather than listed: every `.rs` under
/// `src/`, read at run time.
///
/// For asking *whether* a line is written anywhere. Never slice it: the files
/// are joined end to end, so a cut that runs to "the next `fn`" runs out of
/// the file it started in and reads the following one as more of the same
/// ([`source_from`]).
fn ui_source() -> String {
    source_files()
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("a source file"))
        .collect()
}

/// The one file `needle` is written in, from the needle to the end of *that*
/// file. Every scan that slices starts here, because the alternative is
/// slicing [`ui_source()`]: 42 files with 42 seams between them, and the last
/// fn of any of them -- `silence_card` in `ui/cards.rs` -- read as running on
/// into the file that happens to follow it. The file is found and not named,
/// so the code may move house without this going blind.
fn source_from(needle: &str) -> String {
    source_files()
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("a source file"))
        .find_map(|text| text.find(needle).map(|at| text[at..].to_string()))
        .unwrap_or_else(|| panic!("no {needle} in the window's source"))
}

/// One method's body: from its `fn` line to the `    }` that closes it. The
/// brace at the impl's own indent is where a method ends and nothing inside
/// one is written that far out -- which the old cut, "up to the next
/// `\n    fn `", was not: these methods are `pub(crate) fn`, so it matched
/// none of them and read every card's body as running to the end of the
/// window instead.
fn fn_body(name: &str) -> String {
    let text = source_from(&format!("fn {name}("));
    let end = text
        .find("\n    }\n")
        .map_or(text.len(), |at| at + "\n    }\n".len());
    text[..end].to_string()
}

/// One named file of that source, for the scans about a single region.
fn src_text(rel: &str) -> String {
    std::fs::read_to_string(src_dir().join(rel)).expect("a source file")
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every source file the scans read, in a stable order. The tests are not the
/// window, and the keymap is what the window is compared *against* -- a table
/// that vouched for itself would answer every question asked of it.
fn source_files() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![src_dir()];
    while let Some(dir) = dirs.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("the source directory")
            .map(|entry| entry.expect("a source entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                if path.file_name() != Some("tests".as_ref()) {
                    dirs.push(path);
                }
            } else if path.extension() == Some("rs".as_ref())
                && path.file_name() != Some("keymap.rs".as_ref())
            {
                found.push(path);
            }
        }
    }
    found
}

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// A source entry for `path` on `stream`, as the project keeps them.
fn source(path: &str, stream: usize) -> Source {
    Source {
        path: PathBuf::from(path),
        audio_stream: stream,
    }
}

fn info(index: usize, rate: u32, channels: u16, lang: Option<&str>, decodable: bool) -> StreamInfo {
    StreamInfo {
        index,
        codec: if decodable { "aac" } else { "unknown" }.into(),
        channels,
        sample_rate: rate,
        lang: lang.map(str::to_string),
        decodable,
    }
}

/// The bed the view tests are drawn on: 200 px at 30 fps, as the drop test
/// above uses.
const TEST_BED: f32 = 200.;

/// A scale against that bed and a timeline `duration` seconds long.
fn test_view(scale: Scale, duration: f64) -> View {
    View {
        scale,
        bed: TEST_BED,
        duration,
        fps: 30.,
    }
}

/// The two halves a worker hands back for an import, unwrapped: every import
/// lands as a [`Landed::Read`], and a test that has to say so at four call
/// sites is saying it once here.
fn read_parts(
    path: &std::path::Path,
    stage: &std::sync::atomic::AtomicU8,
    gate: Option<engine::ImportGate>,
) -> (Subs, crate::Probe) {
    match read_ahead(path, stage, gate) {
        crate::Landed::Read(subs, probe) => (subs, probe),
        _ => panic!("an import into a timeline that is up lands as a read"),
    }
}

/// WCAG 2.1 relative luminance of a packed `0xRRGGBB`.
fn luminance(colour: u32) -> f64 {
    let channel = |shift: u32| {
        let s = f64::from((colour >> shift) & 0xff) / 255.;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG 2.1 contrast ratio, 1..=21.
fn contrast(a: u32, b: u32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The two shapes the engine really builds: a track *of* a file states a
/// language and no title (`of_matroska`, `of_mp4`), and a standalone file
/// states no language and is its own name (`external`).
fn sub(path: &str, track: Option<u64>, label: &str) -> engine::subtitle::SubtitleTrack {
    let (language, name) = match track {
        Some(_) => (label.to_string(), String::new()),
        None => (String::new(), label.to_string()),
    };
    engine::subtitle::SubtitleTrack {
        path: PathBuf::from(path),
        track,
        language,
        name,
        label: label.to_string(),
        cues: Vec::new(),
        bitmap: false,
        refused: None,
    }
}
