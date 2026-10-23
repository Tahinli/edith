//! The dock's own content: the Sources/Clip tab pair and what each shows
//! (DESIGN.md §5, §12 step 4). `stance.rs::dock()` owns the panel's frame
//! (width, surfaces, border); this module owns what fills it.
//!
//! The Sources tab used to be `Player::library` verbatim -- the legacy
//! panel's pill tabs, Media/Audio/Text row, filled Import button and amber
//! accent, wearing the darkroom surfaces only because its tokens happened to
//! alias into them. The user named that: "this place is not fitting design
//! language. it's same as our original." This module now builds the tab
//! fresh, off MOCK-SPEC.md's "Dock" section -- `library.rs`/`library_meta.rs`
//! still supply the row facts ([`library_rows`], [`source_tint`],
//! [`clip_middle`]...), only the anatomy around them is new.

use crate::layout::redesign as r;
use crate::player::sections::Section;
use crate::player::project_dialogs::PathFor;
use crate::ui::hitmap;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{self, Button, ButtonSize, ButtonStyle, Segment};
use crate::ui::stance::{Surface, is_focus_cycle_key, is_focus_exit_key, next_surface};
use crate::ui::theme::tokens;
use crate::ui::type_scale::{self, Typeset, head, label, label_tabular};
use crate::*;
use gpui::{FontWeight, PathBuilder, point};

/// Where the dock tab pick lives: one word beside the theme and the
/// keybindings (`ui::theme::config_path`/`save`/`load` is the exact pattern
/// this follows -- a small, silent, config-file round trip is the mechanism
/// this editor already uses for a preference that outlives the window, and
/// the playhead's own continuity lives in the *project* file, which a dock
/// tab pick is not: it is not part of the timeline, so it does not belong in
/// a `.edith`).
pub(crate) fn config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("dock-tab")
}

/// The pick from the last session, if there was one. Anything unreadable or
/// unknown leaves the default (`Src`) in force, exactly as a bad theme file
/// does -- neither is the user's work, so neither is worth a startup notice.
pub(crate) fn load() -> bool {
    std::fs::read_to_string(config_path())
        .map(|text| text.trim() != "Clip")
        .unwrap_or(true)
}

/// Writes the pick. One word, written whole.
pub(crate) fn save(src_active: bool) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if src_active { "Src\n" } else { "Clip\n" });
}

/// Whether the room's one open param card is maximized -- the same
/// small-file round trip as the dock tab pick above, its own word beside it
/// ("a room reopens exactly as left", DESIGN.md:135), so a maximized EQ stays
/// maximized across a close and reopen of the whole room, not just the card.
pub(crate) fn maximized_config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("card-maximized")
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn load_maximized() -> bool {
    std::fs::read_to_string(maximized_config_path())
        .map(|text| text.trim() == "1")
        .unwrap_or(false)
}

pub(crate) fn save_maximized(maximized: bool) {
    let path = maximized_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if maximized { "1\n" } else { "0\n" });
}

/// One tab of the Src/Clip pair (MOCK-SPEC "Dock": no pill, no fill, no
/// rounded button -- a 1px `ink1` top rule and `ink1` text mark the showing
/// tab; the resting one is `ink3`). The mock's own trailing letters (`L`,
/// `I`) are dropped rather than bound: both bare keys are already this
/// editor's own busiest edit strokes (`l` lifts a clip, `i` marks in), and
/// DESIGN §11's frequency check puts a lift and a mark far ahead of a tab
/// switch -- wearing a chord this room does not answer to would be exactly
/// the lie DESIGN §4 forbids, so the letters go instead.
fn dock_tab(
    id: &'static str,
    label_text: &'static str,
    active: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // REDESIGN §3.7: 36px, `emph` label, `ink2` resting / `ink1` showing, a
    // 2px `accent` underline on the showing tab. The underline is always in
    // flow (transparent at rest), so picking a tab shifts nothing.
    let t = tokens();
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .h(px(r::CONTROL_XL))
        .border_b_2()
        .border_color(if active { rgb(t.accent).into() } else { gpui::transparent_black() })
        .hover(move |s| s.bg(rgb(t.e2)))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            // The keys list is body state, not a tab (user 2026-09-09:
            // "remove keys section from here since we also have it next to
            // ledger") -- picking either tab is also the way out of it, and
            // the pair itself (`dock_src_active`) is what the file keeps.
            this.keys_open = false;
            if id == "dock-tab-export" {
                this.open_export(cx);
                return;
            }
            if id == "dock-tab-transcript" {
                this.open_transcript(cx);
                return;
            }
            this.transcript_open = false;
            this.transcript.search_edit = false;
            // Leaving for Media or Clip puts away whatever the other tab had
            // up: the export panel (an export that runs carries on) or a card.
            this.close_card();
            let surface = match id == "dock-tab-src" {
                true => Surface::Dock,
                false => Surface::Inspector,
            };
            // Same dead-handle class as a tab swap: the tab being left
            // takes its `FocusHandle`'s element out of the tree with it, so a
            // ring that was on that handle has to move with the tab or the
            // keyboard goes silent. Only when the ring is actually in the
            // dock -- a tab picked while the bench (or nothing) has focus
            // does not steal it.
            let in_ring = window
                .focused(cx)
                .is_some_and(|f| f == this.focus_dock || f == this.focus_inspector);
            match in_ring {
                true => this.focus_surface(surface, window, cx),
                false => {
                    this.dock_src_active = id == "dock-tab-src";
                    save(this.dock_src_active);
                }
            }
            cx.notify();
        }))
        .tooltip(crate::ui::widgets::tip_hover(
            match id {
                "dock-tab-src" => "Imported files",
                "dock-tab-clip" => "Tools for the selected clip",
                _ => "Write the timeline to a file",
            },
            if id == "dock-tab-export" { "E" } else { "" },
            None,
        ))
        .children(hitmap::control(id, label_text, true))
        .child(
            div()
                .type_style(type_scale::emph())
                .text_color(rgb(if active { t.ink1 } else { t.ink2 }))
                .child(label_text),
        )
}

/// A 12px uppercase Archivo section head, `ink3` (DESIGN §3).
fn section_head(text: impl Into<SharedString>) -> impl IntoElement {
    let style = head();
    div()
        .flex_none()
        .font(style.font.clone())
        .text_size(style.size)
        .text_color(rgb(INK3()))
        .child(text.into())
}

/// The empty search box's words.
pub(crate) const SEARCH_PLACEHOLDER: &str = "Search media";

/// The media list's one empty line.
pub(crate) const EMPTY_LIBRARY_HINT: &str = "Imported media appears here.";

/// What a source on no lane says about itself: where it is, not a state of
/// the file ("unused" read as the file's own condition).
pub(crate) const NOT_ON_TIMELINE: &str = "not on timeline";

/// A refused row's height: one sentence on one line (the full text is the
/// tooltip and `Details`), so it stays the fixed [`SOURCE_ROW_H`] (REDESIGN §3.8).
pub(crate) const REFUSED_ROW_H: f32 = 64.;

/// A poster frame's box in a media row (16:9).
const POSTER_W: f32 = 88.;
const POSTER_H: f32 = 50.;

/// The row's height: fixed for a usable source, taller for a refused one.
pub(crate) fn source_row_h(usable: bool) -> f32 {
    if usable { SOURCE_ROW_H } else { REFUSED_ROW_H }
}

/// The poster frame of a source row or tile, or a quiet stand-in with the
/// kind's icon while it loads, or if the file has none (or cannot be read).
fn poster(player: &Player, path: &std::path::Path, show_frame: bool, radius: f32) -> Div {
    let t = tokens();
    let image = match player.thumbs.get(path) {
        Some(Thumb::Ready(image)) if show_frame => Some(image.clone()),
        _ => None,
    };
    // A file with no picture gets the wave, whatever its extension says (a song muxed into an .mp4).
    let icon = if engine::is_image(path) {
        Icon::Image
    } else if engine::is_audio(path) || !player.has_video(path) {
        Icon::AudioWaveform
    } else {
        Icon::Film
    };
    let base = div()
        .relative()
        .flex_none()
        .rounded(px(radius))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(t.e2));
    match image {
        Some(image) => base.child(crate::ui::bench_stance::cover_image(image)),
        None => base.child(icon.el(IconSize::Inline, t.ink3)),
    }
}

/// Every lane a source plays on right now, as its own `V1`/`A1` labels
/// (MOCK-SPEC "Dock" §4's usage line). The clip *count* that used to trail it
/// ("2 uses") is gone with cleanse round 2 (2026-09-10): the lanes are the
/// answer to "is it in the film?", the number is a statistic, and the
/// Properties side of the row menu still prints it whole.
fn usage_labels(player: &Player, source_idx: usize) -> Vec<String> {
    let lanes: Vec<String> = player.session.as_ref().map_or_else(Vec::new, |session| {
        session
            .lanes()
            .into_iter()
            .filter(|lane| {
                session
                    .lane_clips(*lane)
                    .iter()
                    .any(|c| c.source == source_idx)
            })
            .map(Lane::label)
            .collect()
    });
    // A source used only inside another sequence names that sequence, not "not on timeline".
    // Named only in a project that has another sequence to tell apart; with one, the lanes say it all.
    let inside: Vec<String> = player.session.as_ref().map_or_else(Vec::new, |session| {
        if session.sequences().len() < 2 {
            return Vec::new();
        }
        session
            .source_uses_parked(source_idx)
            .into_iter()
            .filter_map(|(id, _)| session.sequence_name(id))
            .collect()
    });
    lanes.into_iter().chain(inside).collect()
}

/// A row's detail line with the decoder seats taken out of it, which is what
/// the dock draws: the seat is how this machine happens to read the file and
/// the transport line and the Properties side both still say it, so the row
/// says what the file *is* instead.
///
/// Only a whole part is dropped, and a part is a seat only when it is exactly
/// a seat's word. That is what keeps a refusal: the row's detail for a file
/// the decoder would not take is the refusal itself
/// ([`crate::Decoder::Refused`]), and no seat is ever named by a sentence --
/// so the one line that has to say why the file will not play reaches the
/// dock whole.
pub(crate) fn without_seats(detail: &str, seats: &[&str]) -> String {
    detail
        .split(" · ")
        .filter(|part| !seats.contains(part))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A name cut to fit `width_px` at the body size, with an ellipsis where it is cut, so
/// the cut shows as a cut and never as a letter sliced in half. Measured the way the
/// popups measure ([`crate::ui::popup::text_w`]); the whole name is the tooltip.
pub(crate) fn fit_name(text: &str, width_px: f32, size_px: f32) -> String {
    if crate::ui::popup::text_w(text, size_px, false) <= width_px {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    for keep in (0..chars.len()).rev() {
        let cut: String = chars[..keep].iter().collect::<String>() + "\u{2026}";
        if crate::ui::popup::text_w(&cut, size_px, false) <= width_px {
            return cut;
        }
    }
    "\u{2026}".to_string()
}

/// A name cut to fit `width_px` with the ellipsis in the MIDDLE: the start and the end both
/// survive, so two names that share a long start still read apart by their endings.
pub(crate) fn fit_middle(text: &str, width_px: f32, size_px: f32) -> String {
    if crate::ui::popup::text_w(text, size_px, false) <= width_px {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    for keep in (0..chars.len()).rev() {
        let head = keep.div_ceil(2);
        let tail = keep - head;
        let cut: String = chars[..head].iter().collect::<String>()
            + "\u{2026}"
            + &chars[chars.len() - tail..].iter().collect::<String>();
        if crate::ui::popup::text_w(&cut, size_px, false) <= width_px {
            return cut;
        }
    }
    "\u{2026}".to_string()
}

/// A row's name cut to fit like [`fit_middle`], but a multi-stream file's tag (`name [audio 2]`)
/// is kept whole at the end: the stream is what tells the rows of one file apart, so the cut
/// falls on the file's name and never on the part that says which track it is.
pub(crate) fn fit_row_name(name: &str, width_px: f32, size_px: f32) -> String {
    match name.rsplit_once(" [") {
        Some((file, tail)) if tail.ends_with(']') => {
            let tag = format!(" \u{00b7} {}", &tail[..tail.len() - 1]);
            let room = width_px - crate::ui::popup::text_w(&tag, size_px, false);
            format!("{}{tag}", fit_middle(file, room, size_px))
        }
        _ => fit_middle(name, width_px, size_px),
    }
}

/// The width the media panel is drawn at, the one the list and the grid are laid out in.
fn dock_width() -> f32 {
    let w = f32::from_bits(crate::ui::widgets::DOCK_W_BITS.load(std::sync::atomic::Ordering::Relaxed));
    if w > 0. { w } else { r::DOCK_W }
}

/// The file's name without its extension -- what the editor calls the film,
/// not what the filesystem calls the bytes (cleanse round 2). A row that
/// names several streams of one file keeps the stream half it was given
/// ([`library_meta::row_name`] appends it after the file name).
///
/// corner-cut: private to the dock while U2's shared `stem()` is unlanded;
/// the lead reconciles the two into `ui/widgets.rs` or `files.rs`.
pub(crate) fn stem(name: &str) -> String {
    let (file, tail) = match name.split_once(" [") {
        Some((file, tail)) => (file, format!(" [{tail}")),
        None => (name, String::new()),
    };
    match file.rsplit_once('.') {
        // A name that is *all* extension (".srt") keeps it: it is the name.
        Some(("", _)) | None => format!("{file}{tail}"),
        Some((base, _)) => format!("{base}{tail}"),
    }
}

/// Which row item a key answers while the dock's Media panel has focus, or
/// `None` to let it fall through to the global keymap (DESIGN §4: a panel's
/// keys act only while that panel has focus). With a row picked, a bare
/// Space previews it and `↵` adds it -- the two chords the row's own menu
/// prints (DESIGN §6.7 "Preview Space"). Anything with a modifier, or any
/// key with no row picked, keeps its keymap meaning (Space is Play).
pub(crate) fn media_key(
    key: &str,
    mods: gpui::Modifiers,
    focused: bool,
    selected: bool,
) -> Option<RowItem> {
    if !focused || !selected || mods != gpui::Modifiers::none() {
        return None;
    }
    match key {
        "space" | "enter" => Some(RowItem::Preview),
        _ => None,
    }
}

/// A media row's height, fixed (DESIGN §6.7; REDESIGN §3.8: 104 -> 64): the
/// list reads as one even column whatever a row's metadata happens to say.
pub(crate) const SOURCE_ROW_H: f32 = 64.;

/// The list/grid pick, remembered like the dock tab (one word beside it).
static GRID_VIEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static GRID_LOADED: std::sync::Once = std::sync::Once::new();

fn grid_config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("media-view")
}

/// Whether the Media tab shows tiles (`true`) or rows (`false`).
pub(crate) fn grid_view() -> bool {
    GRID_LOADED.call_once(|| {
        let on = std::fs::read_to_string(grid_config_path()).is_ok_and(|t| t.trim() == "grid");
        GRID_VIEW.store(on, std::sync::atomic::Ordering::Relaxed);
    });
    GRID_VIEW.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_grid_view(grid: bool) {
    let _ = grid_view();
    GRID_VIEW.store(grid, std::sync::atomic::Ordering::Relaxed);
    let path = grid_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if grid { "grid\n" } else { "list\n" });
}

/// The All / Favourites pick, remembered beside the list/grid one.
static FAV_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FAV_LOADED: std::sync::Once = std::sync::Once::new();

fn fav_config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("media-filter")
}

/// Whether the Media tab lists only the starred sources.
pub(crate) fn fav_only() -> bool {
    FAV_LOADED.call_once(|| {
        let on = std::fs::read_to_string(fav_config_path()).is_ok_and(|t| t.trim() == "favourites");
        FAV_ONLY.store(on, std::sync::atomic::Ordering::Relaxed);
    });
    FAV_ONLY.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn set_fav_only(only: bool) {
    let _ = fav_only();
    FAV_ONLY.store(only, std::sync::atomic::Ordering::Relaxed);
    let path = fav_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if only { "favourites\n" } else { "all\n" });
}

thread_local! {
    /// The Media tab's sort (0 name, 1 date added, 2 length) and type filter (0 all, 1 video, 2
    /// audio, 3 images), and whether their dropdown is open (with when it last closed, so the
    /// click that closes it by pressing the button again does not reopen it).
    static SORT: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
    static KIND: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SORT_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SORT_CLOSED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
    /// The list's scroll, for the fade that says there is more below.
    static LIST_SCROLL: gpui::ScrollHandle = gpui::ScrollHandle::new();
    /// The clip panel's scroll, and the pick it was last showing: a new pick starts at the top,
    /// so the editor a pick opens is in view rather than wherever the last one was scrolled to.
    /// The pick is every kind at once (clip, caption, title), so any change of selection resets it.
    static CLIP_SCROLL: gpui::ScrollHandle = gpui::ScrollHandle::new();
    static CLIP_PICKED: std::cell::Cell<PanelPick> = const { std::cell::Cell::new((None, None, None)) };
}

/// What the clip panel was showing: the picked clip, the picked caption and the picked title.
type PanelPick = (Option<(engine::project::Lane, usize)>, Option<(engine::project::Lane, usize, usize)>, Option<usize>);

const SORTS: [&str; 3] = ["Name", "Date added", "Length"];
const KINDS: [&str; 4] = ["All", "Video", "Audio", "Images"];

/// Whether `path` is the kind the type filter asks for.
pub(crate) fn kind_matches(kind: usize, path: &std::path::Path) -> bool {
    match kind {
        1 => !engine::is_audio(path) && !engine::is_image(path),
        2 => engine::is_audio(path),
        3 => engine::is_image(path),
        _ => true,
    }
}

/// The distinct files of the project that are gone from disk.
fn offline_files(player: &Player) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for (_, path) in player.session.as_ref().map_or_else(Vec::new, |s| s.offline_sources()) {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// A source whose file is gone from disk: it stays in the library (its clips
/// stay on the timeline), reads "Missing" and offers `Relink…`.
fn is_offline(player: &Player, path: &std::path::Path) -> bool {
    player.session.as_ref().is_some_and(|s| s.offline_sources().iter().any(|(_, p)| p == path))
}

impl Player {
    /// `Relink…`: asks for the replacement file (the chooser is another
    /// process, so on a background thread) and points the source at it. One
    /// undo step ([`PlaybackSession::relink_source`]); a refusal is told in
    /// plain words and changes nothing.
    pub(crate) fn relink_pick(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picked = cx.background_executor().spawn(async { crate::files::pick_file("edith — relink media") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| {
                match picked {
                    Ok(Some(new)) => this.relink_to(path, new, cx),
                    Ok(None) => {}
                    // No chooser here: the same question, typed, for the same missing file.
                    Err(_) => this.open_path_entry_from(PathFor::Relink, Some(path), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Points the missing source at `path` at `new`, and the same-named missing files of the same
    /// length follow it. Shared by the chooser's answer and the typed path's.
    pub(crate) fn relink_to(&mut self, path: PathBuf, new: PathBuf, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else { return };
        let Some(idx) = session.sources().iter().position(|s| s.path == path) else { return };
        let missing = session.offline_sources();
        // What each missing source was, in timeline frames, read before anything moves.
        let recorded: Vec<(usize, u32)> = missing.iter().map(|(i, p)| (*i, session.file_frames(p))).collect();
        let fps = self.fps;
        // The file the user picked goes through the engine's own check, which refuses
        // a wrong file in words and changes nothing.
        let Some(session) = self.session.as_mut() else { return };
        match session.relink_source(idx, &new) {
            Ok(warnings) => {
                let followers = relink_followers(&missing, idx, &path, &new, |p| p.is_file(), |i, candidate| {
                    let want = recorded.iter().find(|(j, _)| *j == i).map_or(0, |(_, f)| *f);
                    same_length(want, candidate, fps)
                });
                let moved = match followers.is_empty() {
                    true => 0,
                    false => self.session.as_mut().map_or(0, |s| s.relink_all(&followers)),
                };
                let still = self.session.as_ref().map_or(0, |s| s.offline_sources().len());
                self.mark_dirty();
                self.reset_after_reseek();
                let tail = warnings.first().map(|w| format!(" \u{2014} {w}")).unwrap_or_default();
                self.notify_user(format!("{}{tail}", relink_words(1 + moved, still)).into());
            }
            Err(e) => self.notify_user(format!("Could not relink: {e}").into()),
        }
        cx.notify();
    }
}

/// The other missing sources a relink of one missing file also points at: every missing source
/// whose file of the same name sits in the picked file's new folder, and whose length matches
/// what the source was ([`same_length`], decided by `matches(source index, candidate)`). The
/// picked file itself is not here: the engine's own check takes it.
fn relink_followers(
    missing: &[(usize, PathBuf)],
    picked_idx: usize,
    old: &std::path::Path,
    new: &std::path::Path,
    is_file: impl Fn(&std::path::Path) -> bool,
    matches: impl Fn(usize, &std::path::Path) -> bool,
) -> Vec<(usize, PathBuf)> {
    let mut pairs = Vec::new();
    let Some(dir) = new.parent() else { return pairs };
    for (idx, path) in missing {
        if *idx == picked_idx || path == old {
            continue;
        }
        let Some(file) = path.file_name() else { continue };
        let candidate = dir.join(file);
        if is_file(&candidate) && matches(*idx, &candidate) {
            pairs.push((*idx, candidate));
        }
    }
    pairs
}

/// The length rule relink uses: a file's length in timeline frames is within one frame of the
/// frames the source was recorded at (the engine's own tolerance for a replacement). A still
/// has no length of its own, so it always matches; a file that cannot be read never does.
fn same_length(recorded: u32, candidate: &std::path::Path, fps: f64) -> bool {
    if engine::is_image(candidate) {
        return true;
    }
    let frames = match engine::is_audio(candidate) {
        true => engine::AudioSession::duration_secs(candidate).ok().flatten().map(|secs| secs * fps),
        false => engine::demux::Demuxer::open(candidate)
            .ok()
            .map(|(meta, _)| f64::from(meta.frame_count) / meta.frame_rate * fps),
    };
    frames.is_some_and(|f| recorded.abs_diff((f.max(0.)).round() as u32) <= 1)
}

/// The relink's answer in plain words: how many files moved, and how many are still missing.
fn relink_words(moved: usize, still: usize) -> String {
    let files = match moved {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    match still {
        0 => format!("Relinked {files}."),
        1 => format!("Relinked {files}. 1 is still missing."),
        n => format!("Relinked {files}. {n} are still missing."),
    }
}

/// One media row (REDESIGN §3.8): an 88x50 poster with the length on it, the
/// name, one plain-facts caption line, and where the source is used as small
/// chips. `+` adds it at the playhead. Offline and refused rows keep the same
/// 64px: the poster wears an `unlink` / `triangle-alert` badge and the line
/// under the name says why in a sentence, with `Relink…` / `Details` beside
/// it. Drag (gesture 1), `↵` (gesture 2, wired in `render.rs`'s key handler),
/// double-click (gesture 3) and right-click (gesture 4) live in
/// [`source_gestures`], shared with the grid tile.
fn source_row(
    player: &Player,
    i: usize,
    row: &Row,
    placed: usize,
    picked: bool,
    star: Option<bool>,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let t = tokens();
    let usable = row.unusable.is_none();
    let offline = is_offline(player, &row.path);
    let name: SharedString = stem(&row.name).into();
    let facts = row_facts(player, row);
    let chips = if placed == 0 { Vec::new() } else { usage_labels(player, row.tint) };
    let can_insert = usable && !offline && player.session.is_some() && player.exporting().is_none();
    let (path, stream) = (row.path.clone(), row.stream);
    let add_path = path.clone();
    let details_path = path.clone();
    let dot_path = path.clone();
    let tip: SharedString = match (&row.unusable, offline) {
        // The row says Missing and the banner above the list offers Relink; a plate here would only cover the next row.
        (_, true) => SharedString::default(),
        (Some(why), _) => refusal_sentence(why).into(),
        _ if picked => PICK_HINT.into(),
        _ if placed == 0 => "Not on the timeline — drag it to a lane".into(),
        _ => "Drag it to a lane".into(),
    };
    // Second line: the facts, or the reason in the error ink.
    // A layout the timeline cannot take is not an error in the file: it is ink2 under the warning icon.
    let layout_refusal = row.unusable.as_deref().is_some_and(is_layout_refusal);
    // The full sentence is the detail's tooltip; the row shows the short form so it is never cut.
    let line2_tip: String = match &row.unusable {
        Some(why) if layout_refusal => refusal_sentence(why),
        _ => String::new(),
    };
    let (line2, line2_ink) = match (&row.unusable, offline) {
        (_, true) => (String::new(), t.ink3),
        (Some(_), _) if layout_refusal => (LAYOUT_SHORT.to_string(), t.ink2),
        (Some(why), _) => (why.clone(), t.err),
        _ => (length_line(player, row, &facts), t.ink3),
    };
    // A refused file wears the alert in the error ink; a missing one in the warning ink.
    let poster = poster(player, &row.path, usable && !offline, r::R_CLIP)
        .w(px(POSTER_W))
        .h(px(POSTER_H))
        .when(!usable || offline, |d| d.child(poster_badge(Icon::TriangleAlert, if offline { t.warn } else { t.err }, false)));
    let group: SharedString = format!("dock-source-row-{i}").into();
    // The star sits on the poster's top-right corner (the badges are on the left): always laid
    // out, drawn when starred, picked or hovered, so the text beside the poster keeps its width.
    let poster = poster.when_some(star, |d, starred| {
        d.child(
            div()
                .absolute()
                .top(px(2.))
                .right(px(2.))
                .child(star_button(("dock-star", i), starred, picked, group.clone(), row.path.clone(), row.stream, cx)),
        )
    });
    let el = div()
        .id(("dock-source", i))
        .group(group.clone())
        .relative()
        .flex_none()
        .h(px(source_row_h(usable)))
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .rounded(px(r::R_CLIP))
        // Picked is the accent wash with an accent edge; hover is the plain raised fill.
        .border_1()
        .border_color(if picked { rgba((t.accent << 8) | 0x59).into() } else { gpui::transparent_black() })
        .when(picked, |d| d.bg(rgba(t.accent_wash)))
        .when(!picked, |d| d.hover(move |s| s.bg(rgb(t.e2))))
        .when(picked, |d| {
            d.child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(rgb(t.accent)))
        });
    source_gestures(el, i, &row.path, row.stream, usable && !offline, name.clone(), tip, cx)
        .child(poster)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(film_dot(i, row.tint, usable, dot_path, stream, cx))
                        .child(
                            div()
                                .id(("dock-name", i))
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(type_scale::body_strong())
                                .text_color(rgb(if usable && !offline { t.ink1 } else { t.ink2 }))
                                .child(fit_row_name(&name, dock_width() - 170., type_scale::BODY_PX))
                                .tooltip(crate::ui::widgets::tip_hover(&name, "", None)),
                        )
                        .when(usable && !offline, |d| {
                            // Drawn only on hover or selection, but always laid out: no shift. It sits on
                            // the name's line so the detail line below keeps the row's whole width.
                            d.child(div().flex_none().when(!picked, |d| d.invisible().group_hover(group.clone(), |s| s.visible())).child(
                Button::new("dock-add-at-playhead-btn")
                    .icon(Icon::Plus)
                    .style(ButtonStyle::Ghost)
                    .enabled(can_insert)
                    .build()
                    .id(("dock-add-at-playhead", i))
                    .when(can_insert, |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.append_source(&add_path, stream, cx);
                            cx.notify();
                        }))
                    })
                    .children(hitmap::dynamic(
                        move || (format!("source.{i}.add"), "Append to timeline end".into()),
                        can_insert,
                    ))
                    .tooltip(crate::ui::widgets::tip_hover("Append to timeline end", "", None)),
            ))
                        }),
                )
                .child(
                    div()
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .overflow_hidden()
                        .when(!line2.is_empty(), |d| {
                            d.child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .overflow_hidden()
                                    .when(layout_refusal, |d| d.child(Icon::TriangleAlert.el(IconSize::Inline, t.warn)))
                                    .child(
                                        div()
                                            .id(("dock-detail", i))
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_ellipsis()
                                            .truncate()
                                            .whitespace_nowrap()
                                            .type_style(type_scale::caption())
                                            .text_color(rgb(line2_ink))
                                            .child(fit_name(&line2, dock_width() - 130., type_scale::CAPTION_PX))
                                            .tooltip(crate::ui::widgets::tip_hover(if layout_refusal { &line2_tip } else { &line2 }, "", None)),
                                    ),
                            )
                        })
                        .when(offline, |d| d.child(missing_chip())),
                )
                .child(
                    div()
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .overflow_hidden()
                        .when(!usable && !offline, |d| {
                            d.child(
                                ghost_link(("dock-details", i), Icon::Info, "Details")
                                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.selected_asset = Some((details_path.clone(), stream));
                                        this.library_menu = Some(LibraryMenu {
                                            path: details_path.clone(),
                                            stream,
                                            at: event.position(),
                                            details: true,
                                        });
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(!chips.is_empty(), |d| d.child(usage_text(&chips, false))),
                ),
        )
}

/// The small corner badge a poster wears for a problem: an icon on a dark chip, top-left in a
/// row (where the star is top-right) and bottom-left in a tile (where the star is top-left).
fn poster_badge(icon: Icon, ink: u32, tile: bool) -> impl IntoElement {
    div()
        .absolute()
        .when(!tile, |d| d.top(px(2.)))
        .when(tile, |d| d.bottom(px(2.)))
        .left(px(2.))
        .size(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r::R_CLIP))
        .bg(rgba(SUB_SHADE()))
        .child(icon.el(IconSize::Inline, ink))
}

/// The star of a source row or tile: always laid out (24 px), drawn when the source is a
/// favourite, picked, or the pointer is on its row -- so toggling never shifts anything.
fn star_button(
    id: (&'static str, usize),
    starred: bool,
    picked: bool,
    group: SharedString,
    path: PathBuf,
    stream: usize,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let t = tokens();
    let tip = if starred { "Remove from favourites" } else { "Favourite" };
    div()
        .id(id)
        .flex_none()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r::R_CLIP))
        .cursor_pointer()
        .bg(rgba(crate::ui::theme::with_alpha(t.e0, 0x99)))
        .hover(move |s| s.bg(rgba(crate::ui::theme::with_alpha(t.e0, 0xd9))))
        .when(!starred && !picked, |d| d.invisible().group_hover(group, |s| s.visible()))
        .child((if starred { Icon::StarFilled } else { Icon::Star }).el(IconSize::Inline, if starred { t.accent } else { t.ink2 }))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            this.toggle_favourite_of(path.clone(), stream, cx);
        }))
        .children(hitmap::dynamic(move || (format!("source.{}.star", id.1), tip.into()), true))
        .tooltip(crate::ui::widgets::tip_hover(tip, "", None))
}

/// A bin row of the media list (28 px): disclosure, folder, name, how many files it holds. A
/// click folds or opens it and picks it, a double-click retypes the name, a right-click opens
/// its menu, and media dragged onto it are filed in it. The name is edited in place without
/// the row changing size.
fn bin_row(
    player: &Player,
    bin: &engine::Bin,
    depth: usize,
    count: usize,
    open: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let t = tokens();
    let id = bin.id;
    let picked = player.selected_bin == Some(id);
    let editing = player.bin_edit.as_ref().filter(|(b, _)| *b == id).map(|(_, f)| f.clone());
    let name: SharedString = match &editing {
        Some(f) if f.all => f.text.clone().into(),
        Some(f) => format!("{}▏", f.text).into(),
        None => bin.name.clone().into(),
    };
    let selected_all = editing.as_ref().is_some_and(|f| f.all && !f.text.is_empty());
    div()
        .id(("dock-bin", id as usize))
        .flex_none()
        .relative()
        .h(px(28.))
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(6. + 14. * depth as f32))
        .pr(px(8.))
        .rounded(px(r::R_CLIP))
        .cursor_pointer()
        .when(picked, |d| d.bg(rgba(t.accent_wash)))
        .when(!picked, |d| d.hover(move |s| s.bg(rgb(t.e2))))
        .drag_over::<AssetDrag>(move |d, _, _, _| d.bg(rgb(crate::ui::theme::mix(t.well, t.accent, 0.22))))
        .on_drop(cx.listener(move |this, drag: &AssetDrag, _, cx| {
            cx.stop_propagation();
            let rows = this.rows_for((drag.0.clone(), drag.1));
            this.move_assets_to_bin(rows, id, cx);
        }))
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if this.bin_edit.is_some() {
                return;
            }
            this.selected_bin = Some(id);
            this.toggle_bin_open(id, cx);
            if event.click_count() >= 2 {
                // The first click of the pair folded it; the second puts it back and renames.
                this.toggle_bin_open(id, cx);
                this.begin_bin_edit(id, cx);
            }
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.open_bin_menu(id, event.position, cx);
            }),
        )
        .when(picked, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(rgb(t.accent))))
        .child((if open { Icon::ChevronDown } else { Icon::ChevronRight }).el(IconSize::Inline, t.ink3))
        .child((if open { Icon::FolderOpen } else { Icon::Folder }).el(IconSize::Inline, t.ink2))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .h(px(20.))
                .flex()
                .items_center()
                .px(px(2.))
                .rounded(px(r::R_CLIP))
                .when(editing.is_some(), |d| d.bg(rgb(t.e3)))
                .when(selected_all, |d| d.bg(rgb(t.ink3)).text_color(rgb(t.e1)))
                .type_style(type_scale::body_strong())
                .text_color(rgb(if selected_all { t.e1 } else { t.ink1 }))
                .child(div().w_full().min_w(px(0.)).truncate().child(fit_row_name(&name, dock_width() - 84. - 14. * depth as f32, type_scale::BODY_PX))),
        )
        .child(
            div()
                .flex_none()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink3))
                .child(count.to_string()),
        )
        .tooltip(crate::ui::widgets::tip_hover(&bin.name, "click to fold, double-click to rename", None))
        .children(hitmap::dynamic(move || (format!("bin.{id}"), "Bin".into()), true))
}

/// A source's one caption line of plain facts: codec, picture height, and the
/// stream half where a file has several. No decoder seat, no mono.
fn row_facts(player: &Player, row: &Row) -> String {
    // The decoder seat (`HW`/`SW`) is how this machine happens to read the
    // file: the transport line and the Properties side still say it.
    let seats = [Backend::Opening, Backend::Hardware, Backend::Software, Backend::Still, Backend::Gap]
        .map(Backend::label);
    let resolution = player
        .facts
        .get(&row.path)
        .and_then(Option::as_ref)
        .and_then(|facts| facts.video.as_ref())
        .filter(|video| video.height > 0)
        .map(|video| resolution_label(video.height))
        .unwrap_or_default();
    let detail = spoken_streams(player, row, &without_seats(&row.detail, &seats));
    // A file with no picture says so in words, in place of the codec list its refusal names.
    let detail = match player.has_video(&row.path) {
        true => detail,
        false => match detail.split_once(" · ") {
            Some((first, rest)) if first.starts_with("no ") => format!("Audio only · {rest}"),
            None if detail.starts_with("no ") => "Audio only".to_string(),
            _ => detail,
        },
    };
    join_detail(&detail, &resolution)
}

/// The stream half of a row's facts with its language spelt out: a track tagged `fra` reads
/// "French", and one tagged `und` (nobody said) loses the word rather than showing a code.
fn spoken_streams(player: &Player, row: &Row, detail: &str) -> String {
    let lang = player
        .streams
        .get(&row.path)
        .and_then(|of_file| of_file.iter().find(|s| s.index == row.stream))
        .and_then(|info| info.lang.clone());
    let Some(lang) = lang else { return detail.to_string() };
    let name = language_name(&lang);
    detail
        .split(' ')
        .filter_map(|word| match word == lang {
            true => name,
            false => Some(word),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The English name of an ISO 639 language code as a muxer writes it, `None` for a code
/// nobody named ("und") or one this table does not know. Both the 639-2 and the 639-1 forms.
fn language_name(code: &str) -> Option<&'static str> {
    Some(match code.to_ascii_lowercase().as_str() {
        "eng" | "en" => "English",
        "fra" | "fre" | "fr" => "French",
        "deu" | "ger" | "de" => "German",
        "spa" | "es" => "Spanish",
        "ita" | "it" => "Italian",
        "por" | "pt" => "Portuguese",
        "rus" | "ru" => "Russian",
        "jpn" | "ja" => "Japanese",
        "zho" | "chi" | "zh" => "Chinese",
        "kor" | "ko" => "Korean",
        "ara" | "ar" => "Arabic",
        "hin" | "hi" => "Hindi",
        "nld" | "dut" | "nl" => "Dutch",
        "pol" | "pl" => "Polish",
        "swe" | "sv" => "Swedish",
        "nor" | "nob" | "no" => "Norwegian",
        "dan" | "da" => "Danish",
        "fin" | "fi" => "Finnish",
        "tur" | "tr" => "Turkish",
        "ell" | "gre" | "el" => "Greek",
        "heb" | "he" => "Hebrew",
        "ces" | "cze" | "cs" => "Czech",
        "hun" | "hu" => "Hungarian",
        "ron" | "rum" | "ro" => "Romanian",
        "tha" | "th" => "Thai",
        "vie" | "vi" => "Vietnamese",
        "ind" | "id" => "Indonesian",
        _ => return None,
    })
}

/// Where a source is used, as quiet text: "On V1, A1".
/// `bare` drops the "On" for a tile whose line shares its width with the Missing chip.
fn usage_text(labels: &[String], bare: bool) -> impl IntoElement {
    let full = format!("On {}", labels.join(", "));
    div()
        .id("dock-usage")
        .flex_1()
        .min_w(px(0.))
        .truncate()
        .whitespace_nowrap()
        .type_style(type_scale::caption())
        .text_color(rgb(tokens().ink3))
        .tooltip(crate::ui::widgets::overlay_tip_hover("Used on", "", Some(&labels.join(", "))))
        .child(if bare { labels.join(", ") } else { full })
}

/// Whether a refusal is the timeline's layout (`the timeline is mono`): a track that is
/// fine in its file and only does not match the timeline, which is a warning, not an error.
fn is_layout_refusal(why: &str) -> bool {
    why.starts_with("the timeline is ")
}

/// A refusal in plain words. A layout mismatch says what happened and what to do; anything
/// else is the engine's own reason, shown as it was given.
/// The short form of a layout refusal, for a row's detail line (its full sentence is the tooltip).
const LAYOUT_SHORT: &str = "Different sound layout";

fn refusal_sentence(why: &str) -> String {
    match why.strip_prefix("the timeline is ") {
        Some(layout) => format!(
            "This track doesn't match the timeline's {layout} sound, so it can't go on the timeline. Use another track of this file that is {layout}."
        ),
        None => why.to_string(),
    }
}

/// The one quiet word a missing file's row or tile wears.
fn missing_chip() -> impl IntoElement {
    let t = tokens();
    div()
        .flex_none()
        .h(px(16.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(r::R_CLIP))
        .bg(rgb(t.e2))
        .type_style(type_scale::caption())
        .text_color(rgb(t.ink2))
        .child("Missing")
}

/// A row's second line: the length (a still has none), then the plain facts.
fn length_line(player: &Player, row: &Row, facts: &str) -> String {
    if engine::is_image(&row.path) {
        return if facts.is_empty() { "Still image".to_string() } else { facts.to_string() };
    }
    let secs = f64::from(row.frames) / player.fps.max(1.);
    join_detail(&short_length(secs), facts)
}

/// A clip's length for a badge or a line: `M:SS`, or under a second in tenths ("0.4 s"),
/// because a clock reads a quarter-second clip as "0:00".
fn short_length(secs: f64) -> String {
    match secs < 1. {
        true => format!("{:.1} s", secs.max(0.)),
        false => clock(secs as f32),
    }
}

/// A compact ghost button with an icon and a word, for a row's own action.
fn ghost_link(id: (&'static str, usize), icon: Icon, text: &'static str) -> gpui::Stateful<Div> {
    let t = tokens();
    div()
        .id(id)
        .flex_none()
        .h(px(18.))
        .px(px(4.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(r::R_CLIP))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(t.e3)))
        .type_style(type_scale::caption_strong())
        .text_color(rgb(t.accent))
        .child(icon.el_px(px(12.), t.accent))
        .child(text)
}

/// The length badge on a poster's bottom-right corner.
fn duration_badge(row: &Row, fps: f64) -> impl IntoElement {
    let secs = f64::from(row.frames) / fps.max(1.);
    div()
        .absolute()
        .right(px(3.))
        .bottom(px(3.))
        .px(px(4.))
        .rounded(px(2.))
        .bg(rgba(SUB_SHADE()))
        .type_style(type_scale::caption())
        .text_color(rgb(tokens().ink1))
        .child(short_length(secs))
}

/// The 6px film ink dot (REDESIGN §0: one of the four places a source's ink
/// lives). Right-click opens the row menu, as the whole row does.
fn film_dot(
    i: usize,
    tint: usize,
    usable: bool,
    path: PathBuf,
    stream: usize,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    div()
        .id(("dock-source-dot", i))
        .flex_none()
        .size(px(6.))
        .rounded_full()
        .bg(rgb(source_tint(tint)))
        .when(usable, |d| {
            d.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.selected_asset = Some((path.clone(), stream));
                    this.library_menu = Some(LibraryMenu { path: path.clone(), stream, at: event.position, details: false });
                    cx.notify();
                }),
            )
        })
}

/// The picked media row's tooltip: how to add more rows to the picks.
const PICK_HINT: &str = "Ctrl-click to pick more";

/// Whether a click on a source row adds the row to the picks: Ctrl, Shift or the platform key
/// held, and not a double click (which opens the source). Otherwise the click picks that row alone.
pub(crate) fn click_adds_to_picks(held: gpui::Modifiers, click_count: usize) -> bool {
    (held.control || held.shift || held.platform) && click_count < 2
}

/// The gestures every media row/tile answers, in one place: right-click
/// anywhere opens the row menu (Add, Remove, Reveal, Properties -- DESIGN §9),
/// a click picks (ctrl/shift add to the picks, a multicam clip is made of
/// several), a double click opens it in the source viewer, and a drag carries the
/// source to a lane (the bench's `AssetDrag` drop target takes it).
#[allow(clippy::too_many_arguments)]
fn source_gestures(
    el: gpui::Stateful<Div>,
    i: usize,
    path: &std::path::Path,
    stream: usize,
    usable: bool,
    ghost: SharedString,
    tip: SharedString,
    cx: &mut Context<Player>,
) -> gpui::Stateful<Div> {
    let menu_path = path.to_path_buf();
    let click_path = path.to_path_buf();
    let drag = AssetDrag(path.to_path_buf(), stream);
    el.child(crate::ui::anchor::probe_row(path, stream)).on_mouse_down(
        MouseButton::Right,
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.selected_asset = Some((menu_path.clone(), stream));
            this.library_menu = Some(LibraryMenu { path: menu_path.clone(), stream, at: event.position, details: false });
            cx.notify();
        }),
    )
    .when(usable, |d| {
        d.cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let held = crate::ui::widgets::press_modifiers(event);
                if click_adds_to_picks(held, event.click_count()) {
                    this.toggle_asset_pick(click_path.clone(), stream);
                } else {
                    this.pick_one_asset(click_path.clone(), stream);
                }
                if event.click_count() >= 2 {
                    this.open_preview(&click_path, stream, cx);
                }
                cx.notify();
            }))
            .on_drag(drag, {
                let me = cx.entity();
                move |_, _, _, cx| {
                    // A fresh gesture inherits nothing: the last live sample of
                    // the previous drag must not land this one on its lane.
                    me.update(cx, |this, _| {
                        this.drag_lane = None;
                        this.drag_x = None;
                    });
                    cx.new(|_| Tip(ghost.clone()))
                }
            })
    })
    .when(!tip.is_empty(), |d| d.tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into()))
    .children(hitmap::dynamic(move || (format!("source.{i}.row"), "Source row".into()), usable))
}

/// One grid tile (REDESIGN §3.8): a 16:9 poster with the length and the `+`
/// on it, the name under it. Same gestures as a row.
fn source_tile(
    player: &Player,
    i: usize,
    row: &Row,
    picked: bool,
    star: Option<bool>,
    tile_w: f32,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let t = tokens();
    let usable = row.unusable.is_none();
    let offline = is_offline(player, &row.path);
    let ok = usable && !offline;
    let name: SharedString = stem(&row.name).into();
    let can_insert = ok && player.session.is_some() && player.exporting().is_none();
    let (path, stream) = (row.path.clone(), row.stream);
    let add_path = path.clone();
    let tip: SharedString = match (&row.unusable, offline) {
        // The tile says Missing and the banner above the list offers Relink; a plate would cover the next tile.
        (_, true) => SharedString::default(),
        (Some(why), _) => refusal_sentence(why).into(),
        _ if picked => PICK_HINT.into(),
        // The name is under the poster; only a name the tile cuts needs saying whole, and a plate
        // over the poster would cover its length badge.
        _ if crate::ui::popup::text_w(&name, type_scale::CAPTION_PX + 1., false) <= tile_w - 24. => SharedString::default(),
        _ => name.clone(),
    };
    let thumb_h = (tile_w - 12.) * 9. / 16.;
    let group: SharedString = format!("dock-source-tile-{i}").into();
    let star_path = row.path.clone();
    let el = div()
        .id(("dock-source", i))
        .group(group.clone())
        .flex_none()
        .w(px(tile_w))
        .p(px(5.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .rounded(px(r::R_CARD))
        .border_1()
        .border_color(if picked { rgba((t.accent << 8) | 0x59).into() } else { gpui::transparent_black() })
        .when(picked, |d| d.bg(rgba(t.accent_wash)))
        .when(!picked, |d| d.hover(move |s| s.bg(rgb(t.e2))));
    source_gestures(el, i, &row.path, row.stream, ok, name.clone(), tip, cx)
        .child(
            poster(player, &row.path, ok, r::R_CLIP)
                .w_full()
                .h(px(thumb_h))
                .relative()
                .when(!usable || offline, |d| d.child(poster_badge(Icon::TriangleAlert, if offline { t.warn } else { t.err }, true)))
                .when_some(star, |d, starred| {
                    d.child(
                        div().absolute().left(px(2.)).top(px(2.)).child(star_button(
                            ("dock-tile-star", i),
                            starred,
                            picked,
                            group.clone(),
                            star_path,
                            stream,
                            cx,
                        )),
                    )
                })
                .when(ok, |d| {
                    // A still has no length to show: its badge would read a made-up "10:00".
                    d.when(!engine::is_image(&row.path), |d| d.child(duration_badge(row, player.fps)))
                    .child(
                        // On hover or selection only, like the list's: laid out always, so nothing shifts.
                        div().absolute().right(px(2.)).top(px(2.))
                            .when(!picked, |d| d.invisible().group_hover(group.clone(), |s| s.visible()))
                            .child(
                            Button::new("dock-tile-add-btn")
                                .icon(Icon::Plus)
                                .style(ButtonStyle::Secondary)
                                .size(ButtonSize::Small)
                                .enabled(can_insert)
                                .build()
                                .id(("dock-add-at-playhead", i))
                                .when(can_insert, |d| {
                                    d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.append_source(&add_path, stream, cx);
                                        cx.notify();
                                    }))
                                })
                                .children(hitmap::dynamic(
                                    move || (format!("source.{i}.add"), "Append to timeline end".into()),
                                    can_insert,
                                ))
                                .tooltip(crate::ui::widgets::tip_hover("Append to timeline end", "", None)),
                        ),
                    )
                })
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(film_dot(i, row.tint, ok, path, stream, cx))
                .child(
                    div()
                        .id(("dock-tile-name", i))
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .type_style(type_scale::body_strong())
                        .text_color(rgb(if ok { t.ink1 } else { t.ink2 }))
                        .child(fit_row_name(&name, tile_w - 30., type_scale::BODY_PX))
                        .tooltip(crate::ui::widgets::tip_hover(&name, "", None)),
                ),
        )
        // The status line, always there so the tiles of a pair stay the same height: a missing
        // file's chip, then where the source is used.
        .child({
            let chips = usage_labels(player, row.tint);
            div()
                .h(px(18.))
                .flex()
                .items_center()
                .gap(px(6.))
                .overflow_hidden()
                .when(offline, |d| d.child(missing_chip()))
                .when(!chips.is_empty(), |d| d.child(usage_text(&chips, offline)))
        })
}

/// One entry of the Sources list, planned before anything is drawn: either a
/// media row or the subtitle group of some file. Plain data so the *order* --
/// the branchy part -- can be tested without a window.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum Slot {
    /// Index into the media rows.
    Media(usize),
    /// Index into the subtitle groups.
    Subs(usize),
}

/// Where each subtitle group belongs in the one Sources list: directly under
/// the source that carries it, never at the end of the list (user 2026-09-10,
/// "you put a video related subtitle to the end") -- a track that belongs to a
/// file is that file's subset and reads as one, so it follows the *last* row
/// naming that file, the way `library_rows` keeps a file's audio streams
/// together. A standalone `.srt` belongs to nobody and stays a top-level row
/// in arrival order, after the sources.
pub(crate) fn source_list_order(media: &[PathBuf], subs: &[PathBuf]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (i, path) in media.iter().enumerate() {
        slots.push(Slot::Media(i));
        if media[i + 1..].contains(path) {
            continue; // a later row still names this file; its subs go under that one
        }
        slots.extend(
            subs.iter()
                .enumerate()
                .filter(|(_, sub)| *sub == path)
                .map(|(j, _)| Slot::Subs(j)),
        );
    }
    slots.extend(
        subs.iter()
            .enumerate()
            .filter(|(_, sub)| !media.contains(sub))
            .map(|(j, _)| Slot::Subs(j)),
    );
    slots
}

/// Imported subtitle tracks, grouped by their source -- the Text tab's own
/// rows (DESIGN.md's "text section ... not serving to anything", user
/// 2026-08-27: relocated off the unconditional spot under the source list
/// and into the tab that names them). A row selects and drags its track; its
/// header folds that source's rows without becoming a cycle. Styled off
/// [`source_row`] -- same dot, same name/detail ink and type scale, same
/// hover fill and selection ring -- so Text stops reading as a second
/// dialect from Media/Audio. Hands back the track count for the tab header's
/// own count line, same as `rows.len()` does for the other two tabs.
fn subtitle_tab_rows(
    player: &Player,
    parents: &[PathBuf],
    cx: &mut Context<Player>,
) -> (usize, Vec<(PathBuf, usize, AnyElement)>) {
    let mut groups = match player.session.as_ref() {
        Some(session) => subtitle_rows(session.subtitles()),
        None => Vec::new(),
    };
    // The same search box the other two tabs answer to -- a visible filter
    // that a whole tab ignored would be a dead control. It matches a track's
    // label or its file's name; a group with no surviving track leaves with
    // its rows.
    let filter = player.dock_filter.to_lowercase();
    if !filter.is_empty() {
        for group in &mut groups {
            if group.name.to_lowercase().contains(&filter) {
                continue;
            }
            group
                .rows
                .retain(|row| row.label.to_lowercase().contains(&filter));
        }
        groups.retain(|group| !group.rows.is_empty());
    }
    // Which lane(s) actually hold each track's captions right now -- the
    // row's second line answers "is it showing?", so it has to read the
    // timeline, not stay the literal "not placed" it was born saying (the
    // old palette's badge never updated after a drop).
    let placed_on = |track: usize| -> Option<String> {
        let session = player.session.as_ref()?;
        let lanes: Vec<String> = session
            .lanes()
            .into_iter()
            .filter(|lane| session.sub_lane(*lane).iter().any(|sub| sub.track == track))
            .map(|lane| lane.label())
            .collect();
        if lanes.is_empty() {
            None
        } else {
            Some(lanes.join(" "))
        }
    };
    let count: usize = groups.iter().map(|group| group.rows.len()).sum();
    let rows: Vec<_> = groups
        .into_iter()
        .enumerate()
        .map(|(group_ord, group)| {
            // A source already in the list is this group's parent row: its
            // tracks hang under it bare, with no `â¸ name 1 track` row of their
            // own to separate them from it. Past three, the disclosure comes
            // back -- and arrives shut, so membership of `sub_folded` means
            // the editor *opened* it there.
            let nested = parents.contains(&group.path);
            let group_path = group.path.clone();
            let fold_path = group.path.clone();
            let tint = file_tint(player.sources(), &group.path);
            let track_count = group.rows.len();
            let tracks: Vec<_> = group
                .rows
                .into_iter()
                .map(|row| {
                    let track = row.track;
                    let picked = track == player.sub_track;
                    let usable = row.refused.is_none();
                    let title: SharedString = match track_count {
                        1 => row.label,
                        _ => format!("{} {}", row.label, row.number),
                    }
                    .into();
                    let detail: SharedString = row
                        .refused
                        .or_else(|| placed_on(track))
                        .unwrap_or_else(|| NOT_ON_TIMELINE.to_string())
                        .into();
                    let ghost = title.clone();
                    let hitmap_title = title.clone();
                    let select_title = title.clone();
                    div()
                        .id(("dock-subtitle-track", track))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        // A track is the header's subset, and the row says
                        // so the way an outline does: stepped in under the
                        // header's own name (user 2026-08-27, "indentation
                        // is needed for subset").
                        .pl(px(30.))
                        .pr(px(8.))
                        .py(px(4.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(match picked {
                            // The ring is always in flow -- transparent at rest, ink1 when
                            // picked -- because a `when(picked, border_1())` added a pixel of
                            // box on selection and shoved every row's text sideways.
                            true => rgb(INK1()).into(),
                            false => gpui::transparent_black(),
                        })
                        .when(!usable, |d| d.opacity(0.5))
                        .when(usable, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(DARK_RAISED())))
                                .on_drag(SubPick(track), {
                                    let me = cx.entity();
                                    move |_, _, _, cx| {
                                        // A fresh gesture inherits nothing: `Player::drag_lane`
                                        // and `Player::drag_x` are the *last live sample* of the
                                        // drag being made, and a drag whose first sample lands
                                        // outside every row would otherwise be let go onto the
                                        // lane the previous one ended over.
                                        me.update(cx, |this, _| {
                                            this.drag_lane = None;
                                            this.drag_x = None;
                                        });
                                        cx.new(|_| Tip(ghost.clone()))
                                    }
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.sub_track = track;
                                    cx.notify();
                                }))
                        })
                        .tooltip(crate::ui::widgets::tip_hover(
                            "Subtitle track",
                            "drag to a lane",
                            None,
                        ))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("subtitle.{group_ord}.{track}.row"),
                                    hitmap_title.to_string(),
                                )
                            },
                            usable,
                        ))
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!("subtitle.{group_ord}.{track}.select"),
                                    format!("Select {select_title}"),
                                )
                            },
                            usable,
                        ))
                        .child(
                            // The same round dot `source_row`'s own name line
                            // opens with, not the old vertical bar -- one dot
                            // shape for every row this dock draws.
                            div()
                                .flex_none()
                                .w(px(8.))
                                .h(px(8.))
                                .rounded(px(4.))
                                .when_some(tint, |d, tint| d.bg(rgb(tint))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                // A step below the header's LABEL_ROW_PX --
                                // subset smaller than superset (user
                                // 2026-08-27), same ink so it stays a name,
                                // not a footnote.
                                .type_style(label_tabular(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK1()))
                                .child(title),
                        )
                        .child(
                            div()
                                .flex_none()
                                .truncate()
                                .type_style(label_tabular(type_scale::FLOOR_PX, FontWeight::MEDIUM))
                                .text_color(rgb(INK3()))
                                .child(detail),
                        )
                        .child(
                            div()
                                .id(("dock-subtitle-remove", track))
                                .flex_none()
                                .w(px(HIT_MIN))
                                .h(px(HIT_MIN))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_subtitle_track(track, cx);
                                }))
                                .tooltip(crate::ui::widgets::tip_hover(
                                    "Remove subtitle track",
                                    "",
                                    None,
                                ))
                                .children(hitmap::dynamic(
                                    move || {
                                        (
                                            format!("subtitle.{track}.remove"),
                                            "Remove subtitle".into(),
                                        )
                                    },
                                    true,
                                ))
                                .child("×"),
                        )
                })
                .collect();
            let has_header = !nested || track_count > 3;
            let folded = has_header && (nested != player.sub_folded.contains(&group_path));
            let header = div()
                .id(("dock-subtitle-group", group_ord))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                // Stepped in under its parent when it has one, the
                // same outline the track rows below it read as.
                .when(nested, |d| d.pl(px(22.)))
                .py(px(4.))
                .rounded(px(3.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if !this.sub_folded.remove(&fold_path) {
                        this.sub_folded.insert(fold_path.clone());
                    }
                    cx.notify();
                }))
                .tooltip(crate::ui::widgets::tip_hover(
                    "Fold this source",
                    "click",
                    None,
                ))
                .children(hitmap::dynamic(
                    move || {
                        (
                            format!("subtitle-group.{group_ord}.fold"),
                            "Toggle subtitle group".into(),
                        )
                    },
                    true,
                ))
                // Bare, the glyph inherited the panel's dim ink and
                // vanished ("the little arrow on left side is hard
                // to see", user 2026-08-27) -- it reads at the same
                // ink as the name it folds.
                .child(
                    div()
                        .flex_none()
                        .w(px(10.))
                        .text_color(rgb(INK1()))
                        .child(if folded { "▸" } else { "▾" }),
                )
                .when_some(tint, |d, tint| {
                    d.child(
                        div()
                            .flex_none()
                            .w(px(8.))
                            .h(px(8.))
                            .rounded(px(4.))
                            .bg(rgb(tint)),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .type_style(label_tabular(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM))
                        .text_color(rgb(INK1()))
                        .child(group.name),
                )
                .child(
                    div()
                        .flex_none()
                        .type_style(label_tabular(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM))
                        .text_color(rgb(INK3()))
                        .child(format!(
                            "{track_count} track{}",
                            if track_count == 1 { "" } else { "s" }
                        )),
                );
            (
                group_path,
                track_count,
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(has_header, |d| d.child(header))
                    .when(!folded, |d| d.children(tracks))
                    .into_any_element(),
            )
        })
        .collect();
    (count, rows)
}

/// The dock's own menu ([`DOCK_ITEMS`]): the three ways a file gets in, on a
/// right-click anywhere in the Sources body a row does not claim first. It is
/// where `Paste path` and `Import subtitles` went when cleanse round 2 took
/// their footer rows, so both keep a pointer door -- including on an empty
/// library, which is when a pasted path is worth most.
fn opens_dock_menu(
    cx: &mut Context<Player>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(|this, event: &MouseDownEvent, _, cx| {
        if this.modal() || this.session.is_none() {
            return;
        }
        cx.stop_propagation();
        this.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::Dock,
            at: event.position,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    })
}

/// The Sources tab, cleanse round 2 (2026-09-10, user "seems cool, let's
/// apply"): one filter and one list of everything this project holds --
/// picture, sound, and the subtitle tracks beside them -- with what a row
/// *is* read off its own metadata line instead of off a sub-tab the editor
/// had to pick first. The `MEDIA` head, the `Media / Audio / Text` strip, the
/// `↕ Recent` sort cycle and the `IMPORT` head are all gone with it; the row
/// facts are still [`library_rows`]' and [`subtitle_tab_rows`]'.
fn sources_tab(
    player: &Player,
    _window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let sources = player
        .session
        .as_ref()
        .map_or(&[][..], PlaybackSession::sources);
    let all_rows: Vec<Row> = library_rows(
        sources,
        &player.streams,
        &player.decoders,
        player.timeline_audio(),
        |path| {
            player
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(path))
        },
    );
    let filter = player.dock_filter.to_lowercase();
    let favs_only = fav_only();
    let kind = KIND.get();
    // The source a row stands for: a file's further audio streams are rows before they are
    // sources, and have no bin or star.
    let source_of = |row: &Row| sources.iter().position(|s| s.path == row.path && s.audio_stream == row.stream);
    let mut rows: Vec<(Row, usize)> = all_rows
        .into_iter()
        .map(|row| {
            let placed = player.row_ctx(&row.path, row.stream).placed;
            (row, placed)
        })
        .filter(|(row, placed)| {
            (filter.is_empty()
                || row.name.to_lowercase().contains(&filter)
                || row.detail.to_lowercase().contains(&filter)
                || (NOT_ON_TIMELINE.contains(&filter) && *placed == 0))
                && (!favs_only || source_of(row).is_some_and(|i| sources[i].fav))
                && kind_matches(kind, &row.path)
        })
        .collect();
    // Sort: the project's own order is "date added"; the others reorder the rows (a stable sort,
    // so a file's streams stay together).
    match SORT.get() {
        0 => rows.sort_by_cached_key(|(row, _)| stem(&row.name).to_lowercase()),
        2 => rows.sort_by_key(|(row, _)| std::cmp::Reverse(row.frames)),
        _ => {}
    }
    // The subtitle tracks join the same list rather than hiding behind a tab
    // of their own: a standalone `.srt` is a source in the sense that matters
    // here -- something that came in and can go on a lane -- and the tracks
    // inside a container are drawn *under* that file's own row
    // ([`source_list_order`]), not after every other source.
    let media: Vec<PathBuf> = rows.iter().map(|(row, _)| row.path.clone()).collect();
    let (_, subtitles) = subtitle_tab_rows(player, &media, cx);
    let subs: Vec<PathBuf> = subtitles.iter().map(|(path, ..)| path.clone()).collect();
    // Past three tracks the group under a parent arrives shut, so the count
    // goes on the parent's own metadata line -- the number stays readable
    // while the rows it counts are folded away.
    for (i, (row, _)) in rows.iter_mut().enumerate() {
        if media[i + 1..].contains(&row.path) {
            continue;
        }
        if let Some((_, n, _)) = subtitles
            .iter()
            .find(|(path, n, _)| *path == row.path && *n > 3)
        {
            row.detail = join_detail(&row.detail, &format!("{n} subs"));
        }
    }
    let mut subtitles: Vec<Option<AnyElement>> =
        subtitles.into_iter().map(|(.., el)| Some(el)).collect();
    let mut row_elements: Vec<AnyElement> = Vec::new();
    let grid = grid_view();
    // The bins: media are listed under the bin they are filed in, bins first. A search or the
    // favourites filter opens every bin that still has something in it and hides the rest, so a
    // match is never behind a closed fold.
    let bins: &[engine::Bin] = player.session.as_ref().map_or(&[][..], PlaybackSession::bins);
    let narrowed = !filter.is_empty() || favs_only || kind != 0;
    let order = source_list_order(&media, &subs);
    let mut by_bin: std::collections::HashMap<u32, Vec<Slot>> = std::collections::HashMap::new();
    {
        let (mut here, mut last): (u32, Option<&PathBuf>) = (0, None);
        for slot in &order {
            match *slot {
                Slot::Media(i) => {
                    here = source_of(&rows[i].0).map_or(0, |s| sources[s].bin);
                    last = Some(&media[i]);
                    by_bin.entry(here).or_default().push(*slot);
                }
                // A file's subtitle tracks stay under the file; a standalone track is top level.
                Slot::Subs(j) => {
                    let bin = if last == Some(&subs[j]) { here } else { 0 };
                    by_bin.entry(bin).or_default().push(*slot);
                }
            }
        }
    }
    let media_in = |bin: u32| by_bin.get(&bin).map_or(0, |v| v.iter().filter(|s| matches!(s, Slot::Media(_))).count());
    enum Entry {
        Bin(u32, usize, usize, bool),
        Hint(usize),
        Slot(Slot, usize),
    }
    let mut entries: Vec<Entry> = Vec::new();
    for top in bins.iter().filter(|b| b.parent == 0) {
        let kids: Vec<&engine::Bin> = bins.iter().filter(|b| b.parent == top.id).collect();
        let total = media_in(top.id) + kids.iter().map(|k| media_in(k.id)).sum::<usize>();
        if narrowed && total == 0 {
            continue;
        }
        let open = narrowed || !player.collapsed_bins.contains(&top.id);
        entries.push(Entry::Bin(top.id, 0, total, open));
        if !open {
            continue;
        }
        for kid in kids {
            let n = media_in(kid.id);
            if narrowed && n == 0 {
                continue;
            }
            let kid_open = narrowed || !player.collapsed_bins.contains(&kid.id);
            entries.push(Entry::Bin(kid.id, 1, n, kid_open));
            if kid_open {
                entries.extend(by_bin.get(&kid.id).into_iter().flatten().map(|s| Entry::Slot(*s, 2)));
                if n == 0 {
                    entries.push(Entry::Hint(2));
                }
            }
        }
        entries.extend(by_bin.get(&top.id).into_iter().flatten().map(|s| Entry::Slot(*s, 1)));
        if media_in(top.id) == 0 && total == 0 {
            entries.push(Entry::Hint(1));
        }
    }
    entries.extend(by_bin.get(&0).into_iter().flatten().map(|s| Entry::Slot(*s, 0)));
    let dock_w = f32::from_bits(DOCK_W_BITS.load(std::sync::atomic::Ordering::Relaxed));
    let tile_w = ((if dock_w > 0. { dock_w } else { r::DOCK_W } - 24. - 4.) / 2.).floor();
    let mut tiles: Vec<AnyElement> = Vec::new();
    let flush = |tiles: &mut Vec<AnyElement>, out: &mut Vec<AnyElement>| {
        if !tiles.is_empty() {
            out.push(div().flex_none().flex().gap(px(4.)).children(tiles.drain(..)).into_any_element());
        }
    };
    for entry in entries {
        let (slot, depth) = match entry {
            Entry::Bin(id, depth, count, open) => {
                flush(&mut tiles, &mut row_elements);
                if let Some(bin) = bins.iter().find(|b| b.id == id) {
                    row_elements.push(bin_row(player, bin, depth, count, open, cx).into_any_element());
                }
                continue;
            }
            Entry::Hint(depth) => {
                flush(&mut tiles, &mut row_elements);
                row_elements.push(
                    div()
                        .flex_none()
                        .h(px(28.))
                        .pl(px(34. + 14. * (depth - 1) as f32))
                        .flex()
                        .items_center()
                        .type_style(type_scale::caption())
                        .text_color(rgb(tokens().ink3))
                        .child("Empty — drag media here")
                        .into_any_element(),
                );
                continue;
            }
            Entry::Slot(slot, depth) => (slot, depth),
        };
        match slot {
            Slot::Media(i) => {
                let (row, placed) = &rows[i];
                let picked = player
                    .selected_asset
                    .as_ref()
                    .is_some_and(|p| *p == (row.path.clone(), row.stream))
                    || player.asset_picks.contains(&(row.path.clone(), row.stream));
                let star = source_of(row).map(|s| sources[s].fav);
                if grid {
                    tiles.push(source_tile(player, i, row, picked, star, tile_w, cx).into_any_element());
                    if tiles.len() == 2 {
                        flush(&mut tiles, &mut row_elements);
                    }
                } else {
                    let el = source_row(player, i, row, *placed, picked, star, cx).into_any_element();
                    // Rows inside a bin are indented under it.
                    row_elements.push(match depth {
                        0 => el,
                        d => div().flex_none().pl(px(14. * d as f32)).child(el).into_any_element(),
                    });
                }
            }
            Slot::Subs(j) => {
                flush(&mut tiles, &mut row_elements);
                if let Some(el) = subtitles[j].take() {
                    row_elements.push(match depth {
                        0 => el,
                        d => div().flex_none().pl(px(14. * d as f32)).child(el).into_any_element(),
                    });
                }
            }
        }
    }
    flush(&mut tiles, &mut row_elements);
    let filter_text: SharedString = player.dock_filter.clone().into();
    // One banner for every missing file, one Relink for the first (the export sheet's rule).
    // The Favourites view counts only the missing files it shows, so its banner is about its own rows.
    let missing: Vec<PathBuf> = offline_files(player)
        .into_iter()
        .filter(|p| !favs_only || sources.iter().any(|s| s.path == *p && s.fav))
        .collect();
    let banner = (!missing.is_empty()).then(|| {
        let t = tokens();
        let first = missing[0].clone();
        let words = match missing.len() {
            1 => "1 file is missing".to_string(),
            n => format!("{n} files are missing"),
        };
        div()
            .id("dock-missing-banner")
            .flex_none()
            .h(px(40.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(r::R_CONTROL))
            .bg(rgb(t.e2))
            .child(Icon::TriangleAlert.el(IconSize::Inline, t.warn))
            .child(div().flex_1().min_w(px(0.)).truncate().type_style(type_scale::body()).text_color(rgb(t.ink1)).child(words))
            .child(
                Button::new("dock-relink-all")
                    .label("Relink\u{2026}")
                    .style(ButtonStyle::Secondary)
                    .size(ButtonSize::Small)
                    .build()
                    .tooltip(crate::ui::widgets::tip_hover("Point this missing file at its new place; same-named files in that folder follow", "", None))
                    .children(hitmap::control("dock.relink", "Relink", true))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.relink_pick(first.clone(), cx))),
            )
    });
    // The sort-and-show list, hung under its button.
    let sort_list = SORT_OPEN.get().then(|| {
        let me = cx.entity();
        let heading = |text: &'static str| kit::section_head(text, None).h(px(24.)).px(px(8.));
        let mut plate = kit::plate(6.)
            .id("dock-sort-list")
            .absolute()
            .top(px(8. + r::CONTROL_M + 4.))
            .right(px(12.))
            .w(px(188.))
            .p(px(crate::layout::MENU_PAD))
            .flex()
            .flex_col()
            .occlude()
            .on_mouse_down_out(cx.listener(|_, _: &MouseDownEvent, _, cx| {
                SORT_OPEN.set(false);
                SORT_CLOSED.set(Some(std::time::Instant::now()));
                cx.notify();
            }))
            .child(heading("Sort by"));
        for (n, label) in SORTS.iter().enumerate() {
            let me = me.clone();
            plate = plate.child(kit::choice_row(("dock-sort-opt", n), *label, SORT.get() == n, move |_, cx| {
                SORT.set(n);
                me.update(cx, |_, cx| cx.notify());
            }));
        }
        plate = plate.child(crate::ui::popup::popup_rule()).child(heading("Show"));
        for (n, label) in KINDS.iter().enumerate() {
            let me = me.clone();
            plate = plate.child(kit::choice_row(("dock-kind-opt", n), *label, KIND.get() == n, move |_, cx| {
                KIND.set(n);
                me.update(cx, |_, cx| cx.notify());
            }));
        }
        gpui::deferred(plate)
    });
    div()
        .id("dock-sources")
        .relative()
        .track_focus(&player.focus_dock)
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            cycle_on_key_down(Surface::Dock)(this, event, window, cx)
        }))
        // Right-click anywhere the rows are not: the ways *in* (Add files,
        // Paste path, Import subtitles), which is what the dock itself can be
        // told -- and the door those last two kept when their rows left the
        // footer. A row's own right-click stops here first (`source_row`).
        .on_mouse_down(MouseButton::Right, opens_dock_menu(cx))
        // The focus ring is not painted (user 2026-09-09: "clicking through
        // timeline draws a white overlay around the timeline, same happens
        // for library section too"). The border stays in flow, transparent in
        // every state, so keyboard focus still moves and nothing shifts.
        .border_1()
        .border_color(gpui::transparent_black())
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(12.))
        .pt(px(8.))
        .child({
            let t = tokens();
            let editing = player.dock_filter_edit;
            let has_text = !player.dock_filter.is_empty();
            // REDESIGN §3.8 row 36: search field, list/grid toggle, import.
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id("dock-filter")
                        .flex_1()
                        .min_w(px(0.))
                        .cursor_text()
                        // A box, not a bare glass: the `e2` fill says "type
                        // here"; the border (always in flow) lights to the
                        // accent while typing, so focusing shifts nothing.
                        .h(px(r::CONTROL_M))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(r::R_CONTROL))
                        .border_1()
                        .border_color(if editing { rgb(t.accent).into() } else { gpui::transparent_black() })
                        .bg(rgb(t.e2))
                        .type_style(type_scale::body())
                        .text_color(rgb(if editing || has_text { t.ink1 } else { t.ink3 }))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.dock_filter_edit = true;
                            cx.notify();
                        }))
                        .tooltip(crate::ui::widgets::tip_hover(
                            "Search media",
                            "type to narrow",
                            None,
                        ))
                        .children(hitmap::control("dock.filter", "Search media", true))
                        .child(Icon::Search.el(IconSize::Inline, t.ink3))
                        .child(
                            div().flex_1().min_w(px(0.)).truncate().child(match has_text {
                                false => SEARCH_PLACEHOLDER.to_string(),
                                true => filter_text.to_string(),
                            }),
                        )
                        .when(has_text, |d| {
                            d.child(
                                div()
                                    .id("dock-filter-clear")
                                    .flex_none()
                                    .size(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(r::R_CLIP))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(rgb(t.e3)))
                                    .child(Icon::X.el(IconSize::Inline, t.ink2))
                                    .tooltip(crate::ui::widgets::tip_hover("Clear search", "Esc", None))
                                    .children(hitmap::control("dock.filter.clear", "Clear search", true))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.dock_filter.clear();
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .child(
                    Button::new("dock-sort")
                        .icon(Icon::SlidersHorizontal)
                        .style(if SORT.get() != 1 || kind != 0 { ButtonStyle::Secondary } else { ButtonStyle::Ghost })
                        .build()
                        .tooltip(crate::ui::widgets::tip_hover("Sort and show", "by name, date or length; video, audio or images", None))
                        .children(hitmap::control("dock.sort", "Sort and show", true))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            // The press that closed the list (it fires outside the list) is not a second
                            // press on the button.
                            let just_closed = SORT_CLOSED.get().is_some_and(|at| at.elapsed().as_millis() < 300);
                            SORT_OPEN.set(!SORT_OPEN.get() && !just_closed);
                            this.dock_filter_edit = false;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("dock-import-plus")
                        .icon(Icon::Plus)
                        .style(ButtonStyle::Ghost)
                        .build()
                        .relative()
                        .child(crate::ui::anchor::probe("dock-add-anchor"))
                        .tooltip(crate::ui::widgets::tip_hover("Import media or add a bin", "", None))
                        .children(hitmap::control("dock.add", "Import media or add a bin", true))
                        // The ways in and the new bin: the dock's own menu, under the button.
                        .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                            if this.modal() || this.session.is_none() {
                                this.pick_and_import(cx);
                                return;
                            }
                            // Under the button, its right edge on the dock's (the placer keeps it
                            // inside the window, and flips it above when there is no room below).
                            let at = crate::ui::anchor::control_at(event.position()).map_or(event.position(), |b| {
                                point(b.origin.x + b.size.width, b.origin.y + b.size.height + px(2.))
                            });
                            this.context_menu = Some(ContextMenu {
                                lane: Lane::V1,
                                on: MenuOn::Dock,
                                at,
                                details: false,
                                adjust: false,
                                compound: false,
                            });
                            cx.notify();
                        })),
                )
        })
        .child(
            // The second row: which media to list, and how.
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .id("dock-which")
                        .flex_none()
                        .tooltip(crate::ui::widgets::tip_hover("Which media", "", None))
                        .child(kit::segmented(
                            "dock-which-seg",
                            &[Segment::text("All"), Segment::text("Favourites")],
                            usize::from(favs_only),
                            true,
                            None,
                            {
                                let me = cx.entity();
                                move |pick, _, cx| {
                                    set_fav_only(pick == 1);
                                    me.update(cx, |_, cx| cx.notify());
                                }
                            },
                        )),
                )
                .child(
                    div()
                        .id("dock-view")
                        .flex_none()
                        .tooltip(crate::ui::widgets::tip_hover("List or grid", "", None))
                        .child(kit::segmented(
                            "dock-view-seg",
                            &[Segment::text("List"), Segment::text("Grid")],
                            usize::from(grid),
                            true,
                            None,
                            {
                                let me = cx.entity();
                                move |pick, _, cx| {
                                    set_grid_view(pick == 1);
                                    me.update(cx, |_, cx| cx.notify());
                                }
                            },
                        )),
                ),
        )
        .children(banner)
        .child(
            div().relative().flex_1().min_h(px(0.)).flex().flex_col().child(
            div()
                .id("dock-source-rows")
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_y_scroll()
                .track_scroll(&LIST_SCROLL.with(Clone::clone))
                // The list fills the dock, so the empty stretch under the last
                // row is *this* element and not its parent: the menu opens off
                // both, or a right-click below the rows finds nothing.
                .on_mouse_down(MouseButton::Right, opens_dock_menu(cx))
                .when(row_elements.is_empty(), |d| {
                    let library_empty = sources.is_empty() && player.dock_filter.is_empty();
                    d.child(empty_state(player, library_empty, favs_only, kind, cx))
                })
                .children(row_elements),
            )
            .child(crate::ui::widgets::more_below(LIST_SCROLL.with(Clone::clone))),
        )
        // One door in, pinned at the bottom (REDESIGN §3.8): a real secondary
        // button, always drawn. `Paste path` (`^l`) and `Import subtitles`
        // (`^i`) keep their strokes and the dock's right-click menu; `Add
        // files` takes a `.srt`/`.vtt` as happily as an `.mkv`.
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .py(px(12.))
                .children(import_button(player, cx)),
        )
        .children(sort_list)
}

/// The list's empty face, one for every reason it can be empty: an icon, one plain line, and the
/// one thing that gets out of it.
fn empty_state(player: &Player, library_empty: bool, favs_only: bool, kind: usize, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let typing = !player.dock_filter.is_empty();
    let (icon, text, id, action): (Icon, String, &'static str, &'static str) = if library_empty {
        (Icon::Film, EMPTY_LIBRARY_HINT.to_string(), "dock-empty-import", "Import media\u{2026}")
    } else if favs_only && !typing && kind == 0 {
        (Icon::Star, "No favourites yet. Star a file and it waits here.".to_string(), "dock-empty-all", "Show all media")
    } else if !typing && kind != 0 {
        (Icon::Film, format!("No {} in this project.", KINDS[kind].to_lowercase()), "dock-empty-types", "Show all types")
    } else {
        (Icon::Search, "Nothing matches.".to_string(), "dock-empty-clear", "Clear search")
    };
    div()
        .id("dock-empty")
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .py(px(28.))
        .child(icon.el(IconSize::Hero, t.ink3))
        .child(div().text_center().type_style(type_scale::body()).text_color(rgb(t.ink2)).child(text))
        .child(
            Button::new(id)
                .label(action)
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Small)
                .build()
                .tooltip(crate::ui::widgets::tip_hover(action, "", None))
                .children(hitmap::control(id, action, true))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    match id {
                        "dock-empty-import" => this.pick_and_import(cx),
                        "dock-empty-all" => set_fav_only(false),
                        "dock-empty-types" => KIND.set(0),
                        _ => {
                            this.dock_filter.clear();
                            KIND.set(0);
                        }
                    }
                    cx.notify();
                })),
        )
}

/// The footer's `Import media…` button: secondary, full width, the chord only
/// in its tooltip; dimmed with the reason when the room refuses an import.
fn import_button(player: &Player, cx: &mut Context<Player>) -> Option<impl IntoElement> {
    let action = ActionId::AddFiles;
    let enabled = player.enable(action, None);
    if !enabled.listed() {
        return None;
    }
    let on = enabled.yes();
    Some(
        div().flex_1().child(
            Button::new("dock-import-files")
                .label("Import media…")
                .icon(Icon::Download)
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Large)
                .enabled(on)
                .build()
                .w_full()
                .tooltip(crate::ui::widgets::action_hover(player, action))
                .children(hitmap::action(action, on))
                .when(on, |d| d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)))),
        ),
    )
}

/// The transition a clip carries into its immediate successor, if any -- a
/// dissolve on a video lane ([`Clip::transition_out`]) or a crossfade on an
/// audio lane, told apart from a plain one-sided fade drag by
/// [`Project::crossfade`]'s own signature: both ends of the join set
/// together, not just this clip's tail. `cap` is what the pair actually
/// offers -- the shorter of the two clips' own lengths -- the same ceiling
/// [`Project::set_transition_out`]/[`Project::crossfade`] clamp to, so the
/// row this feeds never claims more room than a step could land in.
pub(crate) fn transition_of(
    lane_kind: LaneKind,
    clip: &Clip,
    next: Option<&Clip>,
) -> Option<(&'static str, u32, u32)> {
    let next = next.filter(|n| n.start == clip.end())?;
    let cap = clip.frames().min(next.frames());
    match lane_kind {
        LaneKind::Video if clip.transition_out > 0 => {
            Some((clip.transition_kind.title(), clip.transition_out, cap))
        }
        LaneKind::Audio if clip.fade_out > 0 && next.fade_in > 0 => {
            Some(("Crossfade", clip.fade_out, cap))
        }
        _ => None,
    }
}

/// The transition block under the Clip tab's verbs, for the join the anchor
/// makes with its right-hand neighbour. On a video lane it always shows, so
/// the kind can be picked before there is a transition: `None` clears it, a
/// shape sets it (a fresh one is a second long), a moving shape takes its
/// direction, and a transition that exists shows its length. A crossfade on an
/// audio lane shows its length only ([`transition_of`]). Every control writes
/// through the same doors as the palette ([`Player::set_transition_kind`],
/// [`Player::set_transition_frames`]).
/// The Transition section's rows: Kind, its one-line description, Direction, Length,
/// and the cap, on one grid (a label column, then the control column). Every row is
/// always there, so switching kinds moves nothing: a row that does not apply to the
/// kind is shown dimmed, with the reason as its tooltip. Returns the section's title
/// and its rows; `None` when the clip has no neighbour to join.
fn transition_rows(player: &Player, cx: &mut Context<Player>) -> Option<(&'static str, gpui::Stateful<gpui::Div>)> {
    use engine::transitions::{Dir, TransitionKind};
    let (lane, idx) = player.selected.anchor()?;
    let session = player.session.as_ref()?;
    let clips = session.lane_clips(lane);
    let clip = clips.get(idx)?;
    let next = clips.get(idx + 1);
    let (frames, cap) = match lane.kind {
        LaneKind::Video => {
            let next = next.filter(|n| n.start == clip.end())?;
            (clip.transition_out, clip.frames().min(next.frames()))
        }
        _ => {
            let (_, frames, cap) = transition_of(lane.kind, clip, next)?;
            (frames, cap)
        }
    };
    let fps = player.fps.max(1.);
    let seconds = |f: u32| f64::from(f) / fps;
    let title = match lane.kind {
        LaneKind::Video => "Transition",
        _ => "Crossfade",
    };
    let current = clip.transition_kind;
    let me = cx.entity();
    // One row: a label column of the same width everywhere, then the control.
    let row = |id: &'static str, name: &'static str, control: gpui::AnyElement| {
        div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(r::CONTROL_M))
            .px(px(6.))
            .child(div().flex_none().w(px(112.)).type_style(type_scale::body()).text_color(rgb(INK2())).child(name))
            .child(control)
    };
    let mut block = div().id("dock-transition").flex().flex_col().gap(px(4.));
    if lane.kind == LaneKind::Video {
        // The kind row: None, then the seven shapes. A dip's colour and a
        // moving shape's direction are their own rows, so a shape is one pick.
        let dir = current.dir().unwrap_or(Dir::Left);
        let shapes = [
            TransitionKind::Dissolve,
            TransitionKind::DipBlack,
            TransitionKind::DipWhite,
            TransitionKind::Wipe(dir),
            TransitionKind::Slide(dir),
            TransitionKind::Push(dir),
            TransitionKind::Zoom,
        ];
        // One list row per kind, each with a plain line of what it does. None
        // first: it removes the transition.
        const KINDS: [(&str, &str); 8] = [
            ("None", "No transition: a straight cut"),
            ("Dissolve", "Fades one clip into the next"),
            ("Dip black", "Fades out to black, then in"),
            ("Dip white", "Fades out to white, then in"),
            ("Wipe", "A line reveals the next clip"),
            ("Slide", "The next clip slides over"),
            ("Push", "Next clip pushes the old out"),
            ("Zoom", "Old clip zooms into the next"),
        ];
        let at = match frames {
            0 => 0,
            _ => 1 + shapes.iter().position(|k| k.shape_name() == current.shape_name()).unwrap_or(0),
        };
        let pick = move |this: &mut Player, i: usize, cx: &mut Context<Player>| {
            this.transition_kind_open = false;
            match i {
                0 => this.set_transition_frames(lane, idx, 0, cx),
                n => this.set_transition_kind(shapes[n - 1], cx),
            }
        };
        let open = player.transition_kind_open;
        let field = kit::dropdown_field("transition-kind", KINDS[at].0, true).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.transition_kind_open = !this.transition_kind_open;
            cx.notify();
        }));
        let list = open.then(|| {
            let t = crate::ui::theme::tokens();
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.transition_kind_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            // One line a kind: its name, then what it does. Eight of them fit under the
            // field on a 720-line window, so the list never runs off the bottom.
            let mut plate = kit::plate(6.).id("transition-kind-list").w(px(360.)).p(px(crate::layout::MENU_PAD)).flex().flex_col().occlude();
            for (i, &(name, why)) in KINDS.iter().enumerate() {
                plate = plate.child(
                    div()
                        .id(("transition-kind-row", i))
                        .flex_none()
                        .h(px(crate::layout::MENU_ROW_H))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)))
                        .child(div().flex_none().size(px(16.)).children((i == at).then(|| Icon::Check.el(IconSize::Inline, t.accent))))
                        .child(div().flex_none().w(px(84.)).whitespace_nowrap().type_style(type_scale::body_strong()).text_color(rgb(t.ink1)).child(name))
                        .child(div().flex_1().min_w(px(0.)).whitespace_nowrap().overflow_hidden().type_style(label(type_scale::LABEL_ROW_PX, FontWeight::NORMAL)).text_color(rgb(INK3())).child(why))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| pick(this, i, cx))),
                );
            }
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .child(backdrop)
                    .child(div().absolute().top(px(r::CONTROL_M + 2.)).right(px(0.)).child(plate)),
            )
        });
        block = block.child(row(
            "dock-transition-kind",
            "Kind",
            div().relative().flex_none().w(px(150.)).child(field).children(list).into_any_element(),
        ));
        // The kind's line sits right under its row, quiet, one line.
        block = block.child(
            div()
                .h(px(20.))
                .px(px(6.))
                .whitespace_nowrap()
                .overflow_hidden()
                .type_style(label(type_scale::LABEL_ROW_PX, FontWeight::NORMAL))
                .text_color(rgb(INK3()))
                .child(KINDS[at].1),
        );
        // Direction: the four sides for a moving shape; dimmed with the reason otherwise.
        let dir_on = frames > 0 && current.dir().is_some();
        let dirs = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];
        let dir_at = dirs.iter().position(|&d| Some(d) == current.dir()).unwrap_or(usize::MAX);
        let me_dir = me.clone();
        let pick_dir = move |i: usize, _: &mut Window, app: &mut App| {
            me_dir.update(app, |this, cx| this.set_transition_kind(current.with_dir(dirs[i]), cx));
        };
        let dir_row = row(
            "dock-transition-direction",
            "Direction",
            div()
                .flex_none()
                .w(px(150.))
                .child(kit::segmented_equal(
                    "transition-dirs",
                    &["Left", "Right", "Up", "Down"].map(Segment::text),
                    dir_at,
                    dir_on,
                    None,
                    pick_dir,
                ))
                .into_any_element(),
        );
        block = block.child(match dir_on {
            true => dir_row,
            false => dir_row.tooltip(crate::ui::widgets::tip_hover("Direction", "", Some("Only for wipe, slide and push"))),
        });
    }
    let style = label(type_scale::LABEL_ROW_PX, FontWeight::NORMAL);
    let shown = format!("{:.1}", seconds(frames));
    let editing = player.transition_edit.as_ref();
    // The length is typed only once a kind is picked; before that it reads 0.0 s, dimmed.
    let len_on = frames > 0;
    let length = kit::number_field("transition-duration-field", 150., shown, Some("s"), editing, len_on);
    let length = match len_on {
        true => length
            .tooltip(crate::ui::widgets::tip_hover("Transition length", "click to type", None))
            .children(hitmap::control("transition-duration-field", "Transition length, type-in", true))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.transition_edit.is_none() {
                    this.edit_transition(frames);
                }
                cx.notify();
            }))
            .into_any_element(),
        false => length.into_any_element(),
    };
    let length_row = row("dock-transition-length", "Length", div().flex_none().child(length).into_any_element());
    block = block.child(match len_on {
        true => length_row,
        false => length_row.tooltip(crate::ui::widgets::tip_hover("Length", "", Some("Pick a kind to set its length"))),
    });
    // The cap sits under the box, on the control column.
    block = block.child(
        div()
            .h(px(20.))
            .pl(px(126.))
            .whitespace_nowrap()
            .type_style(style)
            .text_color(rgb(INK3()))
            .child(format!("up to {:.1} s", seconds(cap))),
    );
    Some((title, block))
}

/// The Transition section in the Clip tab: a head like Speed or Colour (icon,
/// chevron, name), folding its rows open and shut. Only for a clip with a neighbour.
fn transition_section(player: &Player, cx: &mut Context<Player>) -> Option<AnyElement> {
    let (title, rows) = transition_rows(player, cx)?;
    let open = player.transition_section_open;
    let t = tokens();
    let ink = if open { t.ink1 } else { t.ink2 };
    let head = div()
        .id("dock-verb-transition")
        .flex_none()
        .h(px(r::CONTROL_L))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_t_1()
        .border_color(rgba(t.line))
        .when(open, |d| d.bg(rgb(t.e2)))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(t.e2)))
        .tooltip(crate::ui::widgets::tip_hover(title, "", None))
        .children(hitmap::control("dock-verb-transition", title, true))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.transition_section_open = !this.transition_section_open;
            cx.notify();
        }))
        .child(if open { Icon::ChevronDown } else { Icon::ChevronRight }.el_px(px(12.), t.ink3))
        .child(Icon::Layers.el(IconSize::Inline, ink))
        .child(div().type_style(type_scale::section()).text_color(rgb(ink)).child(title));
    Some(
        div()
            .id("dock-transition-section")
            .flex_none()
            .flex()
            .flex_col()
            .child(head)
            .children(open.then(|| div().id("dock-transition-body").flex_none().px(px(12.)).pb(px(12.)).child(rows)))
            .into_any_element(),
    )
}

/// The Clip tab: the four verbs DESIGN §5 names, as ghosts, over whichever
/// param-row card they open -- [`Player::eq_card`], [`Player::color_card`],
/// [`Player::transform_card`], [`Player::speed_card`] verbatim, the same
/// param-row rendering `inspector.rs`'s selection section already opens
/// these onto. Drag-while-playing and every other gesture on a row is
/// whatever that card already does; nothing about the gesture is reimplemented
/// here.
fn viz_style_verbs(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let clip = player.selected.anchor().and_then(|(lane, idx)| {
        let session = player.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx)?;
        (clip.visualizer & 1 != 0).then_some(clip.clone())
    });
    let Some(clip) = clip else {
        return div().id("dock-viz-styles");
    };
    let flags = clip.visualizer;
    // The ink the wheel draws is the one a drag is holding, when one is: the
    // clip still carries the sample before it until the worker lands the
    // holding frame (`viz_held_paint`).
    let paint = viz_held_paint(clip.viz_paint, player.pending_viz);
    let pick = |id: &'static str, label_text: &'static str, style: u8, on: bool| {
        let row = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
        div()
            .id(id)
            .flex_none()
            .h(px(CONTROL_H))
            .px(px(8.))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_color(rgb(if on { INK1() } else { INK3() }))
            .font(row.font)
            .text_size(row.size)
            .tooltip(crate::ui::widgets::tip_hover(label_text, "", None))
            .children(hitmap::control(id, label_text, true))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_viz_style(style, cx);
            }))
            .child(if on {
                format!("· {label_text}")
            } else {
                label_text.to_string()
            })
            .into_any_element()
    };
    let bit = |id: &'static str, label_text: &'static str, bit: u8, on: bool| {
        let style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
        div()
            .id(id)
            .flex_none()
            .h(px(CONTROL_H))
            .px(px(8.))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_color(rgb(if on { INK1() } else { INK3() }))
            .font(style.font.clone())
            .text_size(style.size)
            .tooltip(crate::ui::widgets::tip_hover(label_text, "", None))
            .children(hitmap::control(id, label_text, true))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_viz_flag(bit, cx);
            }))
            .child(if on {
                format!("· {label_text}")
            } else {
                label_text.to_string()
            })
            .into_any_element()
    };
    div()
        .id("dock-viz-styles")
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex_none()
                .pt(px(4.))
                .px(px(8.))
                .child(section_head("VISUALIZER")),
        )
        .child(pick(
            "dock-viz-fill",
            "Fill",
            engine::decode::VIZ_STYLE_FILL,
            engine::decode::viz_style(flags) == engine::decode::VIZ_STYLE_FILL,
        ))
        .child(pick(
            "dock-viz-ribbon",
            "Ribbon",
            engine::decode::VIZ_STYLE_RIBBON,
            engine::decode::viz_style(flags) == engine::decode::VIZ_STYLE_RIBBON,
        ))
        .child(pick(
            "dock-viz-ring",
            "Ring",
            engine::decode::VIZ_STYLE_RING,
            engine::decode::viz_style(flags) == engine::decode::VIZ_STYLE_RING,
        ))
        .child(pick(
            "dock-viz-wave",
            "Waveform",
            engine::decode::VIZ_STYLE_WAVE,
            engine::decode::viz_style(flags) == engine::decode::VIZ_STYLE_WAVE,
        ))
        .child(bit(
            "dock-viz-fast",
            "Fast",
            engine::decode::VIZ_FAST,
            flags & engine::decode::VIZ_FAST != 0,
        ))
        .child(viz_paint_knobs(player, paint, cx))
}

/// Which of the clip's two inks the wheel and the colour field are on. The
/// picked chip wears the solid ink and the other the quiet one -- the style
/// chips' own shape (`viz_style_verbs`'s `pick`, one row up the same column).
///
/// The Strand chip is dead while the clip has one strand: a strand colour is
/// painted on the ring's *dual* half and a one-strand ring has none, so the
/// chip would be wired to nothing -- and it says so on hover rather than doing
/// nothing in silence (`widgets::control`'s own rule for a button with no
/// action).
fn viz_slot_chips(player: &Player, paint: u64, cx: &mut Context<Player>) -> impl IntoElement {
    let picked = player.viz_slot.picked(paint);
    let strand_live = engine::decode::viz_strands_of(paint) >= 2;
    let chip = |id: &'static str, label_text: &'static str, slot: VizSlot, live: bool| {
        let row = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
        let on = picked == slot;
        div()
            .id(id)
            .flex_none()
            .h(px(CONTROL_H))
            .px(px(8.))
            .flex()
            .items_center()
            .font(row.font)
            .text_size(row.size)
            .text_color(rgb(if on { INK1() } else { INK3() }))
            .tooltip(crate::ui::widgets::tip_hover(
                label_text,
                "",
                match live {
                    true => None,
                    false => Some("needs two strands"),
                },
            ))
            .children(hitmap::control(id, label_text, live))
            .when(!live, |d| d.opacity(0.4).cursor_not_allowed())
            .when(live, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_viz_slot(slot, cx);
                    }))
            })
            .child(label_text.to_string())
            .into_any_element()
    };
    div()
        .id("dock-viz-slots")
        .flex()
        .items_center()
        .child(chip("dock-viz-colour", "Colour", VizSlot::Main, true))
        .child(chip(
            "dock-viz-strand",
            "Strand",
            VizSlot::Strand,
            strand_live,
        ))
}

/// The picked slot's colour as text, and the way in: a click opens the type-in
/// seeded with exactly what the row is showing
/// ([`Player::edit_viz_hex`]). While the field is open the row is the buffer
/// and its refusal instead -- `transition_row`'s own rule for its own field.
fn viz_hex_row(player: &Player, paint: u64, cx: &mut Context<Player>) -> impl IntoElement {
    let (hue, sat) = player.viz_slot.picked(paint).hue_sat(paint);
    let hex = engine::decode::viz_color_hex(hue, sat);
    // The seed the click opens on is the line the row is showing, so a click
    // that lands while the field is open cannot seed it with something else.
    let seed = hex.clone();
    let field = player.viz_hex_edit.as_ref();
    let row = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    div()
        .id("dock-viz-hex-field")
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .font(row.font)
        .text_size(row.size)
        .text_color(rgb(INK2()))
        .cursor_pointer()
        .tooltip(crate::ui::widgets::tip_hover(
            "Colour",
            "click to type",
            None,
        ))
        .children(hitmap::control(
            "dock-viz-hex-field",
            "Colour hex, type-in",
            true,
        ))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.edit_viz_hex(&seed);
            cx.notify();
        }))
        .child(match field {
            Some(edit) => edit.detail(),
            None => hex,
        })
}

fn viz_color_wheel(player: &Player, paint: u64, cx: &mut Context<Player>) -> impl IntoElement {
    // What the mark is on, read out here because the canvas closure paints and
    // so cannot borrow the player: the picked slot's ink
    // ([`VizSlot::hue_sat`]) -- the clip's own colour, or, for a strand the
    // clip has not chosen, the automatic complement, so the hand starts where
    // the picture already is.
    let (mark_hue, mark_sat) = player.viz_slot.picked(paint).hue_sat(paint);
    div()
        .id("dock-viz-hs")
        .w(px(112.))
        .h(px(112.))
        .relative()
        .cursor_pointer()
        .tooltip(crate::ui::widgets::tip_hover(
            "Colour wheel",
            "drag to pick",
            None,
        ))
        .children(hitmap::control("dock-viz-hs", "Colour wheel", true))
        .child(bounds_probe(player.viz_wheel.clone()))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let o = bounds.origin;
                    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                    let cx = f32::from(o.x) + w / 2.;
                    let cy = f32::from(o.y) + h / 2.;
                    let radius = w.min(h) / 2. - 1.;
                    let wedges = 72;
                    let rings = 10;
                    for ring in 0..rings {
                        let s0 = ring as f32 / rings as f32;
                        let s1 = (ring + 1) as f32 / rings as f32;
                        let sat = (s0 + s1) / 2.;
                        for i in 0..wedges {
                            let a0 = (i as f32 / wedges as f32) * std::f32::consts::TAU;
                            let a1 = ((i + 1) as f32 / wedges as f32) * std::f32::consts::TAU;
                            let hue = i as f32 / wedges as f32 * 360.;
                            let (r, g, b) = engine::decode::viz_hsv_rgb(hue, sat, 0.84);
                            let color = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
                            let p00 = point(
                                px(cx + radius * s0 * a0.sin()),
                                px(cy - radius * s0 * a0.cos()),
                            );
                            let p01 = point(
                                px(cx + radius * s0 * a1.sin()),
                                px(cy - radius * s0 * a1.cos()),
                            );
                            let p11 = point(
                                px(cx + radius * s1 * a1.sin()),
                                px(cy - radius * s1 * a1.cos()),
                            );
                            let p10 = point(
                                px(cx + radius * s1 * a0.sin()),
                                px(cy - radius * s1 * a0.cos()),
                            );
                            let mut path = PathBuilder::fill();
                            path.add_polygon(&[p00, p01, p11, p10], false);
                            if let Ok(path) = path.build() {
                                window.paint_path(path, rgb(color));
                            }
                        }
                    }
                    // Selection mark.
                    let hue = f32::from(mark_hue) / 256. * std::f32::consts::TAU;
                    let sat = f32::from(mark_sat) / 255.;
                    let mx = cx + radius * sat * hue.sin();
                    let my = cy - radius * sat * hue.cos();
                    let mut mark = PathBuilder::stroke(px(2.));
                    let r = 4.;
                    mark.add_polygon(
                        &[
                            point(px(mx - r), px(my)),
                            point(px(mx), px(my - r)),
                            point(px(mx + r), px(my)),
                            point(px(mx), px(my + r)),
                        ],
                        true,
                    );
                    if let Ok(path) = mark.build() {
                        window.paint_path(path, rgb(INK1()));
                    }
                },
            )
            .size_full(),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.viz_hs_dragging = true;
                this.drag_viz_hs(event.position, true, cx);
            }),
        )
}

fn viz_paint_knobs(player: &Player, paint: u64, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(8.))
        .child(
            div()
                .id("dock-viz-wheel")
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(viz_slot_chips(player, paint, cx))
                .child(viz_color_wheel(player, paint, cx))
                .child(viz_hex_row(player, paint, cx)),
        )
        .child(viz_strands_slider(paint, cx))
        .child(viz_glow_slider(paint, cx))
}

/// The strand count a slider share lands on: 1 at the left end, 32 at the
/// right, one count per step.
pub(crate) fn viz_strands_at(along: f32) -> u8 {
    1 + (along.clamp(0., 1.) * 31.).round() as u8
}

/// The glow a slider share lands on: 0 at the left end, 16 at the right.
pub(crate) fn viz_glow_at(along: f32) -> u8 {
    (along.clamp(0., 1.) * 16.).round() as u8
}

/// The strands slider, 1 to 32 (the engine's own clamp). A press sets the
/// count on the spot and the picture follows; the reset goes back to the
/// azure default's count.
fn viz_strands_slider(paint: u64, cx: &mut Context<Player>) -> impl IntoElement {
    let strands = engine::decode::viz_strands_of(paint);
    let default = engine::decode::viz_strands_of(engine::decode::VIZ_PAINT_AZURE);
    // The probe and the press share this box: it is recorded at paint and read
    // by the press that the same paint's listener was built for.
    let bar = Rc::new(Cell::new(Bounds::default()));
    let press_bar = bar.clone();
    let reset: SliderReset<Player> = Rc::new(move |this: &mut Player, cx: &mut Context<Player>| {
        this.set_viz_strands(default, cx);
    });
    div()
        .id("dock-viz-strands-field")
        .tooltip(crate::ui::widgets::tip_hover(
            "Strands",
            "",
            Some("Lines the visualizer draws, 1 to 32"),
        ))
        .child(slider_row(
            SliderRow {
                id: "dock-viz-strands",
                label: "Strands".into(),
                value: format!("{strands}").into(),
                fraction: slider_fraction(f32::from(strands), 1., 32.),
                changed: strands != default,
                bar,
                on_press: Box::new(
                    move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                        let along = frac_along(x, press_bar.get());
                        this.set_viz_strands(viz_strands_at(along), cx);
                    },
                ),
                on_reset: Some(reset),
                // Typed as the count the row shows, 1 to 32.
                typed: Some(SliderTyped {
                    lo: 1.,
                    hi: 32.,
                    set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                        this.set_viz_strands(v.round() as u8, cx);
                    }),
                }),
            },
            cx,
        ))
}

/// The glow slider, 0 to 16 (the engine's own clamp). Same press and reset
/// rules as [`viz_strands_slider`].
fn viz_glow_slider(paint: u64, cx: &mut Context<Player>) -> impl IntoElement {
    let glow = engine::decode::viz_glow_of(paint);
    let default = engine::decode::viz_glow_of(engine::decode::VIZ_PAINT_AZURE);
    let bar = Rc::new(Cell::new(Bounds::default()));
    let press_bar = bar.clone();
    let reset: SliderReset<Player> = Rc::new(move |this: &mut Player, cx: &mut Context<Player>| {
        this.set_viz_glow(default, cx);
    });
    div()
        .id("dock-viz-glow-field")
        .tooltip(crate::ui::widgets::tip_hover(
            "Glow",
            "",
            Some("How strongly the picture glows, 0 to 16"),
        ))
        .child(slider_row(
            SliderRow {
                id: "dock-viz-glow",
                label: "Glow".into(),
                value: format!("{glow}").into(),
                fraction: slider_fraction(f32::from(glow), 0., 16.),
                changed: glow != default,
                bar,
                on_press: Box::new(
                    move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                        let along = frac_along(x, press_bar.get());
                        this.set_viz_glow(viz_glow_at(along), cx);
                    },
                ),
                on_reset: Some(reset),
                // Typed as the level the row shows, 0 to 16.
                typed: Some(SliderTyped {
                    lo: 0.,
                    hi: 16.,
                    set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                        this.set_viz_glow(v.round() as u8, cx);
                    }),
                }),
            },
            cx,
        ))
}

fn clip_tab(
    player: &Player,
    width: f32,
    window_size: Size<Pixels>,
    _window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // The room actually given here, not the window's: `eq_card_w`/
    // `card_max_w` are asked "how wide may I draw" and answered with the
    // *whole viewport's* width when handed `window_size` verbatim -- but this
    // tab is a ~280-390px strip inside the dock, not the window, so a card
    // capped at up to 720px overflowed past the window's own right edge.
    // Height stays the window's: none of the un-maximized cards' own
    // arithmetic uses it.
    let room = Size::new(px(width - 24.), window_size.height);
    let anchor = player.selected.anchor();
    let title_up = player.title_target().is_some();
    // A selected caption is not a clip either: the tab is its panel, then its style section.
    let cue_up = anchor.is_none() && !title_up && player.sel_cue_span().is_some();
    let empty = anchor.is_none() && !player.clip_card_open() && !title_up && !cue_up;
    // One inspector: a header for the clip, then a section head per card; the
    // open card sits right under its head (REDESIGN §3.9). The cards keep the
    // "one card up at a time" rule (`close_card`), so opening a section folds
    // the previous one; the heads and everything below them move, nothing
    // above does.
    let mut first = true;
    let mut head = |id: &'static str,
                    title: &'static str,
                    icon: Icon,
                    action: ActionId,
                    open: bool,
                    on_click: fn(&mut Player, &ClickEvent, &mut Context<Player>)|
     -> Option<AnyElement> {
        let section = Section::ALL.into_iter().find(|s| s.action() == action);
        let changed = section.is_some_and(|s| player.section_changed(s));
        let el = section_verb(id, title, icon, action, open, !first, (section, changed), player, on_click, cx)?;
        first = false;
        Some(el.into_any_element())
    };
    let speed = head("dock-verb-speed", "Speed", Icon::Gauge, ActionId::Speed, player.speed_open.is_some(), |t, _, cx| t.open_speed(cx));
    let color = head("dock-verb-color", "Colour", Icon::Palette, ActionId::Color, player.color_open.is_some(), |t, _, cx| t.open_color(cx));
    let transform = head("dock-verb-transform", "Transform", Icon::Move, ActionId::Transform, player.transform_open.is_some(), |t, _, cx| t.open_transform(cx));
    let effects = head("dock-verb-effects", "Effects", Icon::Sparkles, ActionId::Effects, player.effects_open.is_some(), |t, _, cx| t.open_effects(cx));
    let eq = head("dock-verb-eq", "EQ", Icon::SlidersHorizontal, ActionId::Equalizer, player.eq_open.is_some(), |t, _, cx| t.open_eq(cx));
    let silence = head("dock-verb-silence", "Silence", Icon::VolumeX, ActionId::Silence, player.silence_open.is_some(), |t, _, cx| t.open_silence(cx));
    // Mark the beats: a sound clip's section, listed only where there is a sound to listen to.
    let beats = match player.beats_open.is_some() || player.beats_available() {
        true => head("dock-verb-beats", "Mark the beats", Icon::Timer, ActionId::MarkBeats, player.beats_open.is_some(), |t, _, cx| t.open_beats(cx)),
        false => None,
    };
    let mix = head("dock-verb-mix", "Mix", Icon::Volume2, ActionId::Mix, player.mix_open, |t, _, cx| t.open_mix(None, cx));
    let markers = head("dock-verb-markers", "Markers in this sequence", Icon::Flag, ActionId::Markers, player.markers_open, |t, _, cx| t.open_markers(cx));
    let history = head("dock-verb-history", "History", Icon::Undo2, ActionId::History, player.history_open, |t, _, cx| t.open_history(cx));
    // Tools: Normalize, Captions, Fillers and Voiceover in one card.
    let tools = head("dock-verb-tools", "Tools", Icon::Wrench, ActionId::Normalize, player.tools.is_some(), |t, _, cx| {
        t.open_tools(crate::player::tools::Tool::Normalize, cx)
    });
    // Subtitle style: the look of the cue plate, offered while a subtitle clip is selected.
    let on_subtitle = cue_up || anchor.is_some_and(|(lane, _)| !matches!(lane.kind, LaneKind::Video | LaneKind::Audio));
    let substyle = match on_subtitle {
        true => head("dock-verb-substyle", "Subtitle style", Icon::Captions, ActionId::SubtitleStyle, player.subtitle_style_open, |t, _, cx| {
            t.open_subtitle_style(cx)
        }),
        false => None,
    };
    // Fit: a list of four, not a card, so it never reads as open -- the click
    // opens the list (`overlays.rs`'s `Pick::Fit`, the one the clip's
    // right-click menu opens).
    let fit = head("dock-verb-fit", "Fit to frame", Icon::Scan, ActionId::Fit, false, |t, ev, cx| {
        t.open_fit_list(ev.position(), cx);
        if let Some(p) = &mut t.picker {
            p.floored = false;
        }
    });
    // The Transition section (video and audio joins alike), after the list of sections.
    let transition = transition_section(player, cx);
    let picked: PanelPick = (anchor, player.sel_cue, player.title_sel);
    if CLIP_PICKED.with(std::cell::Cell::get) != picked {
        CLIP_PICKED.with(|c| c.set(picked));
        CLIP_SCROLL.with(|s| s.set_offset(gpui::point(px(0.), px(0.))));
    }
    div()
        .id("dock-clip")
        .track_scroll(&CLIP_SCROLL.with(Clone::clone))
        .track_focus(&player.focus_inspector)
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            cycle_on_key_down(Surface::Inspector)(this, event, window, cx)
        }))
        // The focus ring is not painted (user 2026-09-09); the border stays in
        // flow, transparent in every state, so keyboard focus still moves and
        // nothing shifts.
        .border_1()
        .border_color(gpui::transparent_black())
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .when(empty, |d| d.child(clip_hero()))
        // A title is not a clip: with one selected the tab is its section and nothing else.
        .when(title_up && anchor.is_none() && !player.clip_card_open(), |d| {
            d.children(title_header(player)).children(section_body(player.title_card(room, cx)))
        })
        .when(!empty && !(title_up && anchor.is_none() && !player.clip_card_open()), |d| {
            // A selected caption is its own panel and the Subtitle style section, nothing of a clip's.
            if cue_up {
                return d
                    .children(player.caption_card(cx))
                    .children(substyle)
                    .children(section_body(player.subtitle_style_card(room, cx)));
            }
            d.children(clip_header(player))
                .children(section_body(player.generator_card(room, cx)))
                .children(speed)
                .children(section_body(player.speed_card(room, cx)))
                .children(color)
                .children(section_body(player.color_card(room, cx)))
                .children(transform)
                .children(section_body(player.transform_card(room, cx)))
                .children(effects)
                .children(section_body(player.effects_card(room, cx)))
                .children(eq)
                .children(section_body(player.eq_card(room, cx)))
                .children(silence)
                .children(section_body(player.silence_card(room, cx)))
                .children(beats)
                .children(section_body(player.beats_card(room, cx)))
                .children(mix)
                .children(section_body(player.mix_card(room, cx)))
                .children(markers)
                .children(section_body(player.markers_card(room, cx)))
                .children(history)
                .children(section_body(player.history_card(room, cx)))
                .children(tools)
                .children(section_body(player.tools_card(room, cx)))
                .children(fit)
                // The Transition section: its head joins the others, its rows fold under it.
                .children(transition)
                // Its head is listed only while a subtitle clip is selected; the card
                // is mounted regardless, or its chord (`y`) would open an invisible modal.
                .children(substyle)
                // The body only under its own head: a style left open by a caption stays open
                // (its section state persists), but it never shows headless under a clip.
                .children(on_subtitle.then(|| section_body(player.subtitle_style_card(room, cx))).flatten())
                .child(
                    div()
                        .id("dock-clip-verbs")
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .child(viz_style_verbs(player, cx)),
                )
        })
}

/// The open card, under its section head.
fn section_body<E: IntoElement>(card: Option<E>) -> Option<gpui::Stateful<Div>> {
    card.map(|c| div().id("dock-section-body").flex_none().px(px(12.)).pb(px(12.)).child(c))
}

/// The Clip tab with nothing to adjust (REDESIGN §3.11): one icon, one line.
fn clip_hero() -> impl IntoElement {
    let t = tokens();
    div()
        .id("dock-clip-hero")
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .p(px(24.))
        .child(Icon::SlidersHorizontal.el(IconSize::Hero, t.ink3))
        .child(
            div()
                .type_style(type_scale::body())
                .text_color(rgb(t.ink2))
                .child("Select a clip to adjust its speed, colour and sound"),
        )
}

/// The inspector's header (REDESIGN §3.9, 44px): the clip's kind icon, its
/// name, its length, and where it sits under the name.
fn clip_header(player: &Player) -> Option<impl IntoElement> {
    let t = tokens();
    let (lane, idx) = player.selected.anchor()?;
    let session = player.session.as_ref()?;
    let clip = session.lane_clips(lane).get(idx).cloned()?;
    // A compound clip plays a sequence, not a file: it wears that sequence's name.
    let name = match engine::project::sequence_id_of(clip.source) {
        Some(id) => session.sequence_name(id).unwrap_or_default().to_string(),
        None => session
            .sources()
            .get(clip.source)
            .map(|s| stem(&crate::file_name(&s.path)))
            .unwrap_or_default(),
    };
    let icon = match lane.kind {
        LaneKind::Video => Icon::Film,
        LaneKind::Audio => Icon::AudioWaveform,
        _ => Icon::Captions,
    };
    let secs = clip.frames() as f32 / player.fps.max(1.) as f32;
    Some(
        div()
            .id("dock-clip-header")
            .flex_none()
            .h(px(44.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon.el(IconSize::Inline, t.ink2))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .type_style(type_scale::emph())
                            .text_color(rgb(t.ink1))
                            .child(name),
                    )
                    .child(
                        div()
                            .type_style(type_scale::caption())
                            .text_color(rgb(t.ink3))
                            .child(format!("{} · clip {}", lane.label(), idx + 1)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .type_style(type_scale::caption())
                    .text_color(rgb(t.ink3))
                    .child(clock(secs)),
            ),
    )
}

/// The header a selected title wears: the clip header's twin (icon, the title's own words, "Title",
/// length), so the inspector opens the same way on every kind of thing on the timeline.
fn title_header(player: &Player) -> Option<impl IntoElement> {
    let t = tokens();
    let (_, title) = player.title_target()?;
    let title = player.shown_title(&title);
    // The title's own words are its heading; an empty one has no clip number to fall back on.
    let name = title.text.lines().next().unwrap_or_default().trim().to_string();
    let name = if name.is_empty() { "Untitled title".to_string() } else { name };
    let secs = title.frames as f32 / player.fps.max(1.) as f32;
    Some(
        div()
            .id("dock-title-header")
            .flex_none()
            .h(px(44.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(Icon::Type.el(IconSize::Inline, t.ink2))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(div().truncate().type_style(type_scale::emph()).text_color(rgb(t.ink1)).child(name))
                    .child(
                        div()
                            .type_style(type_scale::caption())
                            .text_color(rgb(t.ink3))
                            .child("Title"),
                    ),
            )
            .child(div().flex_none().type_style(type_scale::caption()).text_color(rgb(t.ink3)).child(clock(secs))),
    )
}

/// One section head of the Clip inspector (REDESIGN §3.9, 32px): chevron,
/// icon, title; a 1px `line` above all but the first. Clicking opens the card
/// under it, or folds it when it is the open one. A section the clip's kind
/// can never use ([`Enable::Hidden`]) is left out; one the state refuses
/// dims and says why on hover. The chord is in the tooltip, not on the row.
#[allow(clippy::too_many_arguments)]
fn section_verb(
    id: &'static str,
    title: &'static str,
    icon: Icon,
    action: ActionId,
    open: bool,
    rule: bool,
    (section, changed): (Option<Section>, bool),
    player: &Player,
    on_click: fn(&mut Player, &ClickEvent, &mut Context<Player>),
    cx: &mut Context<Player>,
) -> Option<impl IntoElement> {
    let enabled = player.enable(action, None);
    if !enabled.listed() {
        return None;
    }
    let on = enabled.yes();
    let t = tokens();
    let chord: String = match player.keymap.chord(action).as_ref() {
        "--" => String::new(),
        k => k.to_string(),
    };
    let ink = if open { t.ink1 } else { t.ink2 };
    Some(
        div()
            .id(id)
            .flex_none()
            .h(px(r::CONTROL_L))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            // The rule is always in flow (transparent on the first head).
            .border_t_1()
            .border_color(if rule { rgba(t.line).into() } else { gpui::transparent_black() })
            .when(open, |d| d.bg(rgb(t.e2)))
            .when(!on, |d| {
                d.tooltip(crate::ui::widgets::action_hover(player, action))
                    .opacity(0.4)
                    .cursor_not_allowed()
            })
            .when(on, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.e2)))
                    .tooltip(crate::ui::widgets::tip_hover(title, &chord, None))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        if open {
                            // Folds this section alone: the others stay open.
                            match section {
                                Some(section) => section.close(this),
                                None => this.close_card(),
                            }
                            cx.notify();
                        } else {
                            on_click(this, ev, cx);
                        }
                    }))
            })
            .children(hitmap::action(action, on))
            .child(
                if open { Icon::ChevronDown } else { Icon::ChevronRight }
                    .el_px(px(12.), t.ink3),
            )
            .child(icon.el(IconSize::Inline, ink))
            .child(
                div()
                    .type_style(type_scale::section())
                    .text_color(rgb(ink))
                    .child(title),
            )
            // A dot when this clip's values differ from the defaults. Its slot is always there
            // (transparent when not), so a head never changes width or height.
            // The hit area is 16 px square around the 6 px dot, so the hover is easy to find;
            // nothing is drawn in the extra room.
            .child(
                div()
                    .id("dock-section-dot")
                    .flex_none()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(changed, |d| {
                        d.tooltip(crate::ui::widgets::overlay_tip_hover("Changed from default", "", None))
                    })
                    .child(div().flex_none().size(px(6.)).rounded(px(3.)).when(changed, |d| d.bg(rgb(t.accent)))),
            ),
    )
}

/// The shortcuts sheet: every bound command, sectioned by [`keymap::Category`]
/// exactly as [`keys_rows`] files them (reusing that registry-driven order is
/// what makes an action added anywhere land here without a second list to
/// forget), in the shared dialog shell ([`kit::dialog`]) over the picture --
/// not a dock tab, so opening it moves no other tab. Each stroke is drawn as
/// [`kit::keycaps`]. An action the room would refuse right now is dimmed to
/// `ink4` and says why on hover, never hidden: the list is the full truth about
/// the keyboard. The root's key handler owns the filter box (typing, Esc,
/// backspace), so the sheet needs no focus of its own.
pub(crate) fn keys_sheet(player: &Player, viewport: Size<Pixels>, bottom_inset: f32, cx: &mut Context<Player>) -> impl IntoElement + use<> {
    let t = tokens();
    // A reference, not a state view: every row reads the same whether the action can run now or
    // not, so nothing is greyed here. An action with no stroke leaves its key cell empty.
    let pair = |id: usize, text: String, strokes: Vec<String>| {
        div()
            .id(("keys-row", id))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .h(px(crate::layout::MENU_ROW_H))
            .px(px(4.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(text),
            )
            .child(
                div().flex_none().flex().items_center().gap(px(6.)).children(strokes.into_iter().enumerate().flat_map(|(i, s)| {
                    let mut parts: Vec<AnyElement> = Vec::new();
                    if i > 0 {
                        parts.push(div().type_style(type_scale::caption()).text_color(rgb(t.ink3)).child("or").into_any_element());
                    }
                    parts.push(kit::keycaps(&s).into_any_element());
                    parts
                })),
            )
            .into_any_element()
    };
    let rows: Vec<AnyElement> = keys_rows_matching(&player.keys_filter, &player.keymap)
        .into_iter()
        .enumerate()
        .map(|(n, row)| match row {
            KeyRow::Head(category) => div()
                .flex_none()
                .pt(px(12.))
                .pb(px(2.))
                .px(px(4.))
                .type_style(type_scale::section())
                .text_color(rgb(t.ink2))
                .child(category.label())
                .into_any_element(),
            // Every stroke the action answers to, not the menu's one-key column.
            KeyRow::Act(action) => pair(n, action.label().to_string(), player.keymap.strokes(action)),
            KeyRow::Fixed(i) => {
                let f = &keymap::FIXED[i];
                pair(n, f.label.to_string(), f.chord.split(" or ").map(str::to_string).collect())
            }
        })
        .collect();
    let editing = !player.keys_filter.is_empty();
    let list_max = (f32::from(viewport.height) - bottom_inset - crate::layout::MENU_BAR_H - 48. - 112.).max(120.);
    let filter = div()
        .id("keys-filter")
        .flex_none()
        .h(px(r::CONTROL_M))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(r::R_CONTROL))
        .bg(rgb(t.e2))
        .cursor_text()
        .type_style(type_scale::body())
        .text_color(rgb(if editing { t.ink1 } else { t.ink3 }))
        .tooltip(crate::ui::widgets::overlay_tip_hover("Filter shortcuts", "type to filter", None))
        .children(hitmap::control("dock.keys-filter", "Filter keys", true))
        .child(Icon::Search.el(IconSize::Inline, t.ink3))
        .child(match editing {
            false => "Search shortcuts".to_string(),
            true => format!("{}\u{258f}", player.keys_filter),
        });
    let list = match rows.is_empty() {
        true => kit::dialog_text("No shortcut matches.").into_any_element(),
        false => div()
            .id("keys-sheet-rows")
            .track_scroll(&player.keys_scroll)
            .flex()
            .flex_col()
            .max_h(px(list_max))
            .overflow_y_scroll()
            .children(rows)
            .into_any_element(),
    };
    kit::dialog_layer(bottom_inset)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.keys_open = false;
                cx.notify();
                cx.stop_propagation();
            }),
        )
        .child(
            kit::dialog("keys-sheet", 560.)
                .child(kit::dialog_title("Keyboard shortcuts"))
                .child(filter)
                .child(list),
        )
}

/// The dock's content, under `stance.rs::dock()`'s tab-bar-and-body frame:
/// the tab row, then whichever tab is showing.
///
/// Degradation (DESIGN §7): the dock is a fixed-width side panel, not a lane
/// bed, so it has no width ladder of its own to walk -- the panel is either
/// on screen at its one width or, at the narrowest floors this editor draws
/// to, is the first region asked to give up its width entirely (a step
/// `layout.rs`'s split budget already owns for the legacy inspector/library
/// pair). What degrades *inside* fixed width is the two tabs' own content:
/// Sources scrolls its own row list; Clip's param rows are whatever their
/// card already draws at this width.
pub(crate) fn render(
    player: &Player,
    width: f32,
    window_size: Size<Pixels>,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let export = player.export_open;
    let transcript = player.transcript_open && !export;
    let clip_card = player.clip_card_open();
    let src_active = player.dock_src_active && !export && !transcript && !clip_card;
    div()
        .id("stance-dock-body")
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .child(
            div()
                .id("stance-dock-tabs")
                .flex_none()
                .flex()
                .bg(rgb(tokens().e1))
                .border_b_1()
                .border_color(rgba(tokens().line))
                .child(dock_tab("dock-tab-src", "Media", src_active, cx))
                .child(dock_tab("dock-tab-clip", "Clip", !src_active && !export && !transcript, cx))
                .child(dock_tab("dock-tab-transcript", "Transcript", transcript, cx))
                .child(dock_tab("dock-tab-export", "Export", export, cx)),
        )
        .child(match (export, src_active) {
            (true, _) => player.export_panel(cx).into_any_element(),
            _ if transcript => crate::ui::transcript_panel::transcript_tab(player, cx).into_any_element(),
            (false, true) => sources_tab(player, window, cx).into_any_element(),
            (false, false) => clip_tab(player, width, window_size, window, cx).into_any_element(),
        })
}

/// The Tab/Shift-Tab handler shared by the dock's two tabs (`sources_tab`'s
/// "dock/library" and `clip_tab`'s "inspector") -- only [`is_focus_cycle_key`]
/// is ever answered here; every other key is left un-stopped so it bubbles
/// to the room's root handler (`ui::stance::render`'s `on_key_down`), which
/// is the one every other keybind, Tab included when nothing in the ring
/// has focus yet, still hangs off.
fn cycle_on_key_down(
    surface: Surface,
) -> impl Fn(&mut Player, &KeyDownEvent, &mut Window, &mut Context<Player>) + 'static {
    move |this, event, window, cx| {
        let key = event.keystroke.key.as_str();
        if is_focus_cycle_key(key) {
            let next = next_surface(surface, event.keystroke.modifiers.shift);
            this.focus_surface(next, window, cx);
            cx.stop_propagation();
        } else if is_focus_exit_key(key) && !this.any_text_field() {
            // A dismissible overlay (context menu, library menu, picker, any
            // card, a live preview) opened by a click into the dock/inspector
            // takes this escape first -- the same door the root handler
            // itself closes it through (`Player::escape_closes_overlay`) --
            // so it does not go on sitting open just because focus happened
            // to be inside this ring when escape was pressed (the "esc is
            // not closing menus opened by clicking in the clip section"
            // report).
            if this.escape_closes_overlay(cx) {
                cx.stop_propagation();
                return;
            }
            // Leaves the ring for the root, and lets the stroke bubble on to it: the root
            // answers escape as Deselect (clears the clip, title or caption selection) and
            // routes it into an open text field. A text field open here is left to the root too.
            window.focus(&this.focus);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> gpui::Modifiers {
        gpui::Modifiers::none()
    }

    #[test]
    fn space_and_enter_answer_a_picked_row_only_while_the_media_panel_has_focus() {
        // Focused with a row picked: Space previews, Enter adds (DESIGN §6.7).
        assert!(media_key("space", bare(), true, true) == Some(RowItem::Preview));
        assert!(media_key("enter", bare(), true, true) == Some(RowItem::Preview));
        // Not focused: Space stays Play, Enter stays whatever the keymap says.
        assert!(media_key("space", bare(), false, true).is_none());
        assert!(media_key("enter", bare(), false, true).is_none());
        // No row picked: fall through to Play.
        assert!(media_key("space", bare(), true, false).is_none());
        // A chord keeps its keymap meaning even with the panel focused.
        let shift = gpui::Modifiers {
            shift: true,
            ..bare()
        };
        assert!(media_key("space", shift, true, true).is_none());
        // Other keys are never routed here.
        assert!(media_key("j", bare(), true, true).is_none());
    }

    /// One relink of one missing file also points the other missing files of the same name in its
    /// new folder at their copies, but only those whose length matches; a mismatched one stays
    /// missing, and a file of another kind is never followed.
    #[test]
    fn relinking_one_missing_file_follows_only_the_same_names_of_the_same_length() {
        use std::path::{Path, PathBuf};
        let missing = vec![
            (0, PathBuf::from("/old/a.mp4")),
            (1, PathBuf::from("/old/b.mp4")),
            (2, PathBuf::from("/old/c.mp4")),
            (3, PathBuf::from("/old/d.mp4")),
        ];
        let new = Path::new("/new/a.mp4");
        // b.mp4 is there with the matching length, c.mp4 is there but a different length, d.mp4 is not there.
        let there = |p: &Path| p == Path::new("/new/b.mp4") || p == Path::new("/new/c.mp4") || p == new;
        let length_ok = |_: usize, p: &Path| p == Path::new("/new/b.mp4");
        let followers = relink_followers(&missing, 0, Path::new("/old/a.mp4"), new, there, length_ok);
        assert_eq!(followers, vec![(1, PathBuf::from("/new/b.mp4"))], "matching length follows, mismatched is skipped");
        assert_eq!(relink_words(5, 2), "Relinked 5 files. 2 are still missing.");
        assert_eq!(relink_words(1, 0), "Relinked 1 file.");
        assert_eq!(relink_words(3, 1), "Relinked 3 files. 1 is still missing.");
    }

    /// The length rule: within one frame of what the source was recorded at, and a file that
    /// cannot be read never matches.
    #[test]
    fn a_relinked_file_must_match_the_recorded_length_within_a_frame() {
        assert!(!same_length(100, Path::new("/no/such/file.mp4"), 25.));
        assert!(engine::is_image(Path::new("x.png")) && same_length(0, Path::new("x.png"), 25.));
    }

    /// A name that does not fit is cut with an ellipsis, never mid-letter, and a short one is left whole.
    #[test]
    fn a_long_name_is_cut_with_an_ellipsis_and_a_short_one_is_whole() {
        assert_eq!(fit_name("clip", 500., 13.), "clip");
        let cut = fit_name("test_multiaudio [audio 2] long name", 60., 13.);
        assert!(cut.ends_with('\u{2026}'), "{cut}");
        assert!(crate::ui::popup::text_w(&cut, 13., false) <= 60.);
    }

    /// Two names sharing a long start read apart: the cut keeps the end, so the rows differ.
    #[test]
    fn names_sharing_a_long_start_are_cut_in_the_middle_and_stay_distinct() {
        let short = fit_row_name("test_multiaudio_fr [audio 1]", 110., 13.);
        let long = fit_row_name("test_multiaudio_fr_commentary [audio 1]", 110., 13.);
        assert_ne!(short, long, "{short} / {long}");
        assert!(short.contains('\u{2026}') && long.contains('\u{2026}'), "{short} / {long}");
        assert!(short.ends_with("\u{00b7} audio 1") && long.ends_with("\u{00b7} audio 1"));
        assert!(crate::ui::popup::text_w(&long, 13., false) <= 110.);
    }

    /// A stream tag stays whole when the file's name is cut, so three tracks of one file stay apart.
    #[test]
    fn a_stream_tag_is_kept_when_the_file_name_is_cut() {
        let cut = fit_row_name("test_multiaudio [audio 2]", 120., 13.);
        assert!(cut.ends_with("\u{00b7} audio 2"), "{cut}");
        assert!(cut.contains('\u{2026}'), "{cut}");
        assert!(crate::ui::popup::text_w(&cut, 13., false) <= 120.);
        assert_eq!(fit_row_name("clip", 500., 13.), "clip");
    }

    /// Language codes read as names; an untagged track loses the word.
    #[test]
    fn a_stream_language_reads_as_a_name() {
        assert_eq!(language_name("fra"), Some("French"));
        assert_eq!(language_name("eng"), Some("English"));
        assert_eq!(language_name("und"), None);
        assert!(is_layout_refusal("the timeline is mono"));
        assert!(refusal_sentence("the timeline is mono").contains("mono"));
        assert!(LAYOUT_SHORT.len() < 40, "the row's short line must fit the panel");
        assert_eq!(refusal_sentence("aac is not supported"), "aac is not supported");
    }
}
