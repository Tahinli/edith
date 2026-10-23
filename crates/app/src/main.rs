#![forbid(unsafe_code)]
mod keymap;
mod player;
mod ui;

mod creator;
mod audio_cards;
mod color_ui;
mod eq_ui;
mod export_queue;
mod export_ui;
mod files;
mod follow;
mod importing;
mod interact;
mod layout;
mod library_meta;
mod menus;
mod recent;
mod open_hook;
mod notices;
mod output_window;
mod oracle;
mod render;
mod subs;
mod timeline_math;
mod titles;
mod transform_ui;
mod transport;
mod view_state;
mod viewer_view;
mod viewport;

pub(crate) use audio_cards::*;
pub(crate) use color_ui::*;
pub(crate) use eq_ui::*;
pub(crate) use export_ui::*;
pub(crate) use files::*;
pub(crate) use importing::*;
pub(crate) use interact::*;
pub(crate) use layout::*;
pub(crate) use library_meta::*;
pub(crate) use menus::*;
// The Contain letterbox, shared with the subtitle picture overlay
// (ui/preview.rs) so both draw the same fitted rect the same way.
pub(crate) use notices::*;
pub(crate) use oracle::*;
pub(crate) use render::letterboxed_image;
pub(crate) use subs::*;
pub(crate) use timeline_math::*;
pub(crate) use transform_ui::*;
pub(crate) use transport::*;
pub(crate) use viewport::*;

use ui::theme::*;
use ui::widgets::*;

use keymap::{ActionId, Keymap};

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use engine::audio::StreamInfo;
use engine::color::ColorParams;
use engine::decode::Backend;
use engine::eq::{Band, BandKind, EqParams};
use engine::export::{
    AUDIO_KBPS, DEFAULT_AUDIO_KBPS, EncoderSeat, ExportSettings, Format, HdrOutput, HdrTransfer, HevcSpeed, ProResProfile,
};
use engine::limiter::Limiter;
use engine::project::{Edge, Lane, LaneKind, Source, Speed, SubClip};
use engine::scale::FitPolicy;
use engine::tonemap::Preset;
use engine::transform::TransformParams;
use engine::{Clip, Codec, ExportHandle, Frame, MediaBitrate, PlaybackSession};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, Corners, CursorStyle, Div,
    DragMoveEvent, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, RenderImage, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, SharedString, Size, Stateful, TextAlign, TitlebarOptions,
    DisplayId, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, point, prelude::*, px,
    relative, rgb, rgba, size,
};

/// The export probe's answer: the settings, the canvas, the cuts, and the
/// picture's seat (if any) plus the sound's.
type ExportSeat = (
    ExportSettings,
    (u32, u32),
    Vec<Clip>,
    Option<(Option<&'static str>, &'static str)>,
);

/// A gap picked on a lane: the empty stretch `start..start + frames` that
/// Delete closes (DESIGN §7). Held beside [`Player::selected`] and not in it,
/// since a gap owns no clip index. Checked against the timeline before every
/// use ([`Player::gap_pick`]): an edit or an undo that moves the hole drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GapPick {
    pub(crate) lane: Lane,
    pub(crate) start: u32,
    pub(crate) frames: u32,
}

/// The lane beds' boxes, in lane order.
type LaneBeds = Vec<(Lane, Bounds<Pixels>)>;

struct Player {
    /// The timeline, once there is one. A run with no file opens without it and
    /// waits: the first media import or project load is what fills it, and
    /// until then every action that needs a timeline says so instead of acting.
    session: Option<PlaybackSession>,
    /// A resolution and/or frame rate picked before any file exists to derive
    /// them from -- what the resolution and rate lists edit
    /// instead of refusing, with nothing open yet. Applied as the explicit
    /// override [`engine::PlaybackSession::set_resolution`]/`set_frame_rate`
    /// already are for a `.edith`'s own saved values ([`Player::install_media`])
    /// the moment the first session exists, and cleared there: consumed once,
    /// the way a project's own saved settings are read once at
    /// [`engine::PlaybackSession::open_project`].
    pending_settings: (Option<(u32, u32)>, Option<f64>, Option<u32>),
    /// The template a project started by File > New from template takes its lanes, names, caption
    /// style and export preset from, when its first footage arrives.
    pending_template: Option<String>,
    /// The welcome screen's From template list is open.
    welcome_templates: bool,
    /// A library row's own file, opened and playing in place of the
    /// timeline's picture, and never written to `session` or its undo stack:
    /// a preview is watched, not edited. `Some` is what [`Player::pump`] and
    /// [`Player::transport`] read first -- the timeline's own session and its
    /// silence scan sit untouched underneath until this clears again
    /// ([`Player::close_preview`]).
    preview_session: Option<PlaybackSession>,
    /// Whether the timeline was playing when a preview took it over
    /// ([`Player::open_preview`] pauses it so the two sounds never overlap):
    /// what [`Player::close_preview`] resumes, and only then.
    preview_playing: bool,
    /// In/out marks on library items, in seconds of the file and keyed by the
    /// row's `(file, audio stream)`: what `I`/`O` set while a preview is up
    /// ([`Player::mark_source`]) and what the three-point edit reads
    /// ([`Player::source_edit`]). Not saved with the project.
    source_marks: HashMap<(PathBuf, usize), (Option<f64>, Option<f64>)>,
    /// The library item the source edits act on: the one being previewed, or
    /// the last one that was.
    source_target: Option<(PathBuf, usize)>,
    /// The Source pill was pressed with nothing opened yet: the viewer says how to open a clip.
    source_hint: bool,
    /// Which halves of the source monitor's item an insert or overwrite
    /// places: the picture, the sound. Both on whenever a source opens.
    source_video: bool,
    source_audio: bool,
    /// The source patch (Premiere's V and A boxes on the lane heads): the video and audio lanes
    /// the source monitor's picture and sound go to on Insert and Overwrite. V1 and A1 at start.
    patch_video: Lane,
    patch_audio: Lane,
    /// The picture alone, filling the window -- what `Fullscreen` gives every
    /// consumer video player, and not what gpui's own `toggle_fullscreen`
    /// gives on its own (the OS window, chrome and all). Kept apart from
    /// `window.is_fullscreen()` because the two can fall out of step: a
    /// compositor keybind changes the window's without this flag hearing
    /// about it, so [`Player::act`] reconciles them rather than trusting
    /// either alone.
    player_fullscreen: bool,
    /// Whether the active session -- the timeline or a library preview,
    /// whichever [`Player::active_session`] is reading -- restarts from the
    /// top on reaching its end instead of stopping there. One flag for both,
    /// since only one of them is ever playing at a time.
    loop_on: bool,
    /// The in/out mark an export is clipped to, half-open `[start, end)` in
    /// timeline frames -- `None` for the whole timeline. Session state, like
    /// the mark itself: never saved to a `.edith` ([`ExportSettings::range`]).
    range: Option<(u32, u32)>,
    /// Loop-trim (DESIGN.md §6, `/`): the frame window `[start, end)` the
    /// transport loops inside while it is on -- the subject cut's own clip
    /// span, which is what a `[`/`]` nudge is moving. `None` is the mode off,
    /// which is also what lets [`Player::pump`] answer
    /// [`should_loop_restart`] with one field instead of asking whether the
    /// mode is on and then separately what the window is.
    loop_trim: Option<(u32, u32)>,
    /// Timeline seconds -> frame index, so the clock can be compared to what
    /// the decoder hands over.
    fps: f64,
    name: SharedString,
    image: Option<Arc<RenderImage>>,
    /// Frames the viewer has replaced and must drop from every window's sprite
    /// atlas. Dropped from `Player::render`, which is the one place that can
    /// reach every window: the output window shows the same picture.
    retired_images: Vec<Arc<RenderImage>>,
    /// The video output window, while it is open (View > Video output window).
    /// Session state only: never saved in a project.
    output: Option<WindowHandle<output_window::OutputView>>,
    /// The display the output window goes to; `None` is the primary one.
    output_display: Option<DisplayId>,
    /// The displays as the View menu lists them this frame, in order, with their names.
    output_displays: Vec<(DisplayId, SharedString)>,
    /// The picture of the bitmap cue on screen, and which cue that is: the
    /// track it is a row of and where it starts on the timeline. A PGS display
    /// set is run-length and has to be walked into a canvas-sized buffer to be
    /// drawn ([`engine::subtitle::CueImage::rgba`]), which is a thing to do
    /// when the cue changes -- every few seconds -- and not at every repaint.
    ///
    /// The *lane* is half the key because the four PGS tracks of a remux are
    /// one film's subtitles in four languages: placed one over the other they
    /// start at the same microsecond, and a lane whose eye was shut would leave
    /// the one before it on screen. The other half is the cue itself
    /// ([`crate::ui::preview::SubImageKey`]): two different cues can share a
    /// lane and a start too, and a key of those two alone served one caption's
    /// picture for another's.
    sub_image: Option<(crate::ui::preview::SubImageKey, Arc<RenderImage>)>,
    /// A frame that arrived before its time; shown on the tick it comes due.
    /// The pump's buffer, not transport state -- but a frame waiting here is
    /// what keeps a finished decoder from reading as [`Transport::Ended`] one
    /// tick early. See [`Player::transport`].
    held: Option<Frame>,
    /// A seek is waiting for its frame, and since when. Keeps the repaint loop
    /// alive while paused, which is the only way the new still ever reaches the
    /// screen; the instant is what a drag's samples are gated on
    /// ([`Player::flush_drag`]) and what says so in words when one open stands
    /// too long ([`seek_line`]).
    seek_since: Option<Instant>,
    /// The ruler's own box, recorded at prepaint: a mouse listener is handed
    /// the window position and nothing else.
    ruler: Rc<Cell<Bounds<Pixels>>>,
    /// Each lane bed's box, recorded at prepaint like the ruler's: the box
    /// select reads which lanes a pointer rectangle crosses off it.
    lane_beds: Rc<std::cell::RefCell<LaneBeds>>,
    /// A box select in flight, pressed on empty bed ([`Marquee`]).
    marquee: Option<Marquee>,
    /// The scrollbar track's laid-out box, recorded at prepaint like the
    /// ruler's: a thumb drag is followed on the root (the pointer leaves the
    /// strip at once), and the root has only window coordinates to work with.
    scroll_track: Rc<Cell<Bounds<Pixels>>>,
    /// A thumb in the hand: how far into the thumb the press landed, in
    /// track pixels. `None` with no drag in flight.
    scroll_drag: Option<f32>,
    /// A thumb of the lane stack's own scrollbar in the hand: how far into
    /// the thumb the press landed, in track pixels. `None` with no drag in
    /// flight. The stack's box itself is read straight off the scroll handle
    /// each frame, so unlike the time axis's strip it needs no probe of its
    /// own.
    lanes_drag: Option<f32>,
    /// How wide a second of timeline is drawn, and from which moment. Held here
    /// and nowhere else: every frame-to-pixel answer in the panel comes out of
    /// it, so the boxes, the playhead and the pointer cannot disagree.
    scale: Scale,
    /// A hand has moved the view itself -- a wheel scroll, a zoom about the
    /// pointer -- and until the playhead runs back into what it chose to look
    /// at, the view is the hand's and the follow keeps off it. Without this a
    /// notch during playback was undone by the very next frame: the follow
    /// centres a playhead that has left the bed, and a scroll away from it is
    /// exactly a playhead leaving the bed. Given back by [`Render`] the moment
    /// the head is on screen again, and by every transport ask (a seek, a
    /// play/pause) outright -- those are a person saying where to look.
    panned: bool,
    /// A fit asked for before the bed was measured ([`Player::zoom_fit`]): the
    /// bed is only known once the timeline has been painted, and a fit taken
    /// against no bed would fall back to the default scale. Consumed by
    /// [`Render`] on the first frame that has one.
    fit_pending: bool,
    /// The bed width the last fit was worked out for, while the view is still
    /// that fit: [`Render`] refits when the bed has a different width, and a
    /// hand's zoom or scroll sets it back to `None`, which keeps the view the
    /// hand chose through a resize. `None` for a view nobody fitted.
    fitted_bed: Option<f32>,
    /// Each lane's height by its key ("v1", "a2"), as a hand left it. Kept with
    /// the scale in [`view_state`] so a reopened project gets its lanes back.
    lane_h: Vec<(String, f32)>,
    /// The viewer's zoom, playback quality, guides and mask ([`viewer_view`]); view state, saved
    /// with the project's view and never part of the project or an export.
    viewer: viewer_view::Prefs,
    /// How far the zoomed picture is dragged from centred, in logical pixels.
    viewer_pan: (f32, f32),
    /// The pointer of a pan drag in progress, where it last was.
    viewer_drag: Option<Point<Pixels>>,
    /// A drag on the selected clip's picture or its corners, while one is held.
    viewer_handle: Option<viewer_view::ViewerHandle>,
    /// Where the viewer header's four controls paint, so their lists hang from them.
    viewer_btns: [Rc<Cell<Bounds<Pixels>>>; 4],
    /// The lane whose bottom edge is being dragged, with the grab that turns
    /// the pointer's y into its height (see `interact::lane_resize_band`).
    /// `None` when no edge is held.
    lane_resize: Option<(Lane, f32)>,
    /// Which placements the edit keys act on: the lane each is in and its
    /// index there, every ctrl-click in the order it was made. The *clicked*
    /// half and not the group -- a group is what gets marked on screen, but
    /// Lift has to know which half it was aimed at -- and the last pick is the
    /// [`anchor`](Selection::anchor) every single-thing action uses. Indices
    /// move under every edit, so this is cleared by all of them.
    selected: Selection,
    /// The gap a click picked, if one is (DESIGN §7). Mutually exclusive with
    /// `selected`: a gap pick clears the clip marks, and a live clip mark hides
    /// the gap ([`Player::gap_pick`]).
    gap: Option<GapPick>,
    /// The clip menu a right-click opened, if one is up. Holds an index like
    /// `selected` does, so it is closed by anything that can move indices --
    /// every stroke, and every item of its own.
    context_menu: Option<ContextMenu>,
    /// The menu-bar dropdown that is open, if one is (DESIGN §6.1). Closed by
    /// esc, a press below the bar, or running one of its rows.
    menu_open: Option<ui::menubar::Title>,
    /// The title a hover switched the open menu to: the click that follows on
    /// it keeps that menu open (a switch) instead of toggling it shut.
    menu_hopped: Option<ui::menubar::Title>,
    /// The flyout open beside the menu bar's plate: its title and the group's name.
    menu_sub: Option<(ui::menubar::Title, &'static str)>,
    /// A second flyout, beside the first: a list-row inside a group (Picture fit under Adjust).
    /// Drawn only while the first one is the group it hangs from.
    menu_sub2: Option<(ui::menubar::Title, &'static str)>,
    /// Where the status bar's size, rate and clip readout paint: what a shortcut-opened list hangs from.
    ledger_readout: Rc<Cell<Bounds<Pixels>>>,
    /// Where the status bar's toast text paints: its width is how much of the message fits.
    toast_text: Rc<Cell<Bounds<Pixels>>>,
    /// The clip menu's Adjust row has the pointer: the 150 ms timer opens the
    /// submenu only if it still does.
    adjust_hot: bool,
    /// Where the pointer came onto the Adjust row, and the submenu's near edge
    /// `(x, top, bottom)` (written while drawing): together the "safe triangle"
    /// a diagonal move toward the submenu may cross without closing it.
    adjust_aim: Option<Point<Pixels>>,
    adjust_sub: Cell<Option<(f32, f32, f32)>>,
    /// The choice list a click on an enumerated setting opened, if one is up:
    /// the project resolution or a clip's fit policy, every value on screen at
    /// once. Closed by anything that closes the menus, and for the same reason.
    picker: Option<Picker>,
    /// The library menu a right-click on a row opened, if one is up. Names its
    /// row by file and stream rather than by position, so it acts on the row it
    /// was opened on however the list is rebuilt under it.
    library_menu: Option<LibraryMenu>,
    /// Which library row is picked: the file and the audio stream that row
    /// names, which is what an insert needs and what survives a row list being
    /// rebuilt. Its own selection and not the timeline's: Delete keeps acting
    /// on the clip that was clicked in a lane, whatever the library is showing.
    selected_asset: Option<(PathBuf, usize)>,
    /// Library rows picked together (ctrl-click) for a multicam clip; empty means just `selected_asset`.
    asset_picks: Vec<(PathBuf, usize)>,
    /// The angle monitor is shown beside the program picture ([`Player::angle_monitor_view`]).
    angle_monitor: bool,
    /// The monitor's composed tiles.
    angle_shots: player::multicam::AngleShots,
    /// Which category of the library is being looked at. A tab and not a
    /// filter box: the categories are what the media *is*, and every editor
    /// this one is measured against splits its pool the same way.
    /// Which of the darkroom dock's Src/Clip tabs is showing (DESIGN §5).
    /// `Player`-owned rather than the process-global static it used to be
    /// (VIOLATION 3), and persisted the way `ui::theme`'s pick is
    /// (`ui::dock_stance::load`/`save`) so "a room reopens exactly as left"
    /// survives a quit and relaunch, not just this window.
    pub(crate) dock_src_active: bool,
    /// The Sources tab's own filter text (darkroom dock, MOCK-SPEC "Dock" §2)
    /// and whether it is the thing typed strokes go to right now -- the same
    /// capture-flag shape `keys_open` already uses, so a letter
    /// filters the list instead of firing a spine action while the box is
    /// focused, and does nothing to the keymap once it is not.
    pub(crate) dock_filter: String,
    pub(crate) dock_filter_edit: bool,
    /// Type-to-filter on the KEYS tab. Owned the same way the Sources filter
    /// is, except KEYS captures letters the moment the tab is showing -- the
    /// list is a lookup, and a letter that still fired a spine action while
    /// it was up is why search felt missing.
    pub(crate) keys_filter: String,
    /// Which of the four sort chips is showing (MOCK-SPEC "Dock" §3).
    /// What is known about each source's audio, taken once and kept. Keyed on
    /// the path *and stream* -- two streams of one file are two envelopes -- and
    /// the key is inserted the moment the decode is *started*: presence means
    /// "asked", so a repaint mid-decode cannot ask again.
    waves: HashMap<(PathBuf, usize), Wave>,
    /// The bench's poster frame per file ([`Thumb`]), filled like `waves` --
    /// presence means "asked" -- one per file rather than one per clip, since
    /// every clip cut from a file shares its picture.
    thumbs: HashMap<PathBuf, Thumb>,
    /// What each source's stand-in is up to ([`engine::proxy`]), filled like
    /// `waves`: presence means "asked", so the encode is started once per file
    /// however many times the library is drawn. The switch that decides whether
    /// they are *played* is the session's own -- it is the project's and is
    /// saved with it, where this map is this window's view of the cache.
    proxies: HashMap<PathBuf, Proxy>,
    /// The render preview being made and the frames it covers ([`player::render_cache`]).
    render: Option<(engine::render_cache::Job, (u32, u32))>,
    /// The ruler's render bar, and the key ([`Player::refresh_render_bands`]) it was worked out at.
    render_bands: Vec<player::render_cache::RenderBand>,
    render_bands_stamp: Option<(u64, u32, usize)>,
    /// The render being made, as the frames it covers and how far it has got.
    render_progress: Option<((u32, u32), f32)>,
    /// The stabilisation analyses this window started and is waiting on, as
    /// (source, first file frame, end): polled each frame, and the picture is
    /// redrawn when one lands ([`engine::stabilise`]).
    stab_waiting: Vec<(PathBuf, u32, u32)>,
    /// The mask tracks running or waiting to be written onto their masks, polled each frame
    /// ([`Player::poll_tracks`]).
    track_jobs: Vec<engine::playback::TrackJob>,
    /// The auto reframe being measured, if one is ([`Player::start_reframe`]).
    reframe_run: Option<player::cards::ReframeRun>,
    /// The last auto reframe that landed: its lane, clip and how many keys it wrote, so the
    /// Transform card can say the exact count for that clip ([`Player::poll_reframe`]).
    reframed: Option<(Lane, usize, usize)>,
    /// Angles being synced for a multicam clip ([`Player::create_multicam_selected`]).
    mcam_run: Option<player::multicam::McamRun>,
    /// The sequences compound clips were opened from, outermost first: Back pops one
    /// ([`Player::close_compound`]). A tab click starts the trail over.
    seq_trail: Vec<u32>,
    /// The open sequence's name being retyped on its tab, which owns the keyboard while it
    /// is `Some` ([`Player::seq_edit_key`]).
    seq_edit: Option<player::text_field::TextField>,
    /// A track's name being retyped on its head, which owns the keyboard while it is `Some`
    /// ([`Player::lane_edit_key`]).
    lane_edit: Option<(Lane, player::text_field::TextField)>,
    /// The transport timecode retyped as a time (Go to time), which owns the keyboard while it is
    /// `Some` ([`Player::time_edit_key`]). A click elsewhere cancels it ([`Player::blur_text_fields`]).
    time_edit: Option<player::text_field::TextField>,
    /// The compare view (View > Compare grade); preview only, never exported.
    compare: viewer_view::Compare,
    /// The picture as shot at [`Player::before_at`], the other half of the compare view.
    before: Option<Arc<RenderImage>>,
    /// The timeline frame `before` shows; `None` when it has to be read again.
    before_at: Option<u32>,
    /// A read of the picture as shot is in flight.
    before_busy: bool,
    /// The frame the pump last drew: its timeline index and pixel size.
    shown_frame: Option<(u32, u32, u32)>,
    /// The colour card's Looks dropdown is open.
    look_open: bool,
    /// Saved look names, read when the dropdown opens and after a save.
    saved_looks: Vec<String>,
    /// The name typed for "Save current as look…", while it is being typed.
    look_name: Option<player::text_field::TextField>,
    /// The look last applied to a clip, for the Looks row's name.
    look_mark: Option<player::grading::LookMark>,
    /// The bins the media list shows folded ([`Player::toggle_bin_open`]); kept while the project is open.
    collapsed_bins: std::collections::HashSet<u32>,
    /// Which Clip-tab sections are open per clip kind ([`player::sections`]), as the view file keeps them.
    sections_mem: Vec<(String, Vec<String>)>,
    /// The clip the sections were last synced to; a different one re-opens the remembered sections.
    sections_anchor: Option<(Lane, usize)>,
    /// The bin picked in the media list, if one is (what Rename bin and Remove bin act on).
    selected_bin: Option<u32>,
    /// A bin's name being retyped on its row, which owns the keyboard while it is `Some`
    /// ([`Player::bin_edit_key`]).
    bin_edit: Option<(u32, player::text_field::TextField)>,
    /// The title selected for editing, as an index into the session's titles; it shows in the
    /// Clip tab while no clip is selected ([`Player::title_target`]).
    title_sel: Option<usize>,
    /// The words of that title while its text field has the keyboard, which it owns while it is
    /// `Some` ([`Player::title_key`]). Written to the project when the field is left.
    title_draft: Option<player::text_field::TextField>,
    /// A colour of the selected title being typed as hex (the letters' or the box's), which owns
    /// the keyboard while it is `Some`; written to the project when the field is left.
    title_hex: Option<(player::titling::HexTarget, player::text_field::TextField)>,
    /// The hex of a shape's fill or stroke being typed in the Generator card.
    gen_hex: Option<(player::generators::GenPaint, player::text_field::TextField)>,
    /// A number of the Generator card being scrubbed: shown live, written on release.
    gen_scrub: Option<player::generators::GenScrub>,
    /// The Transform card's Deinterlace list is open.
    deinterlace_open: bool,
    /// The Transform card's Place list is open.
    place_open: bool,
    /// The Title card's Entrance and Exit lists are open.
    title_in_open: bool,
    title_out_open: bool,
    /// The index of the effect whose Blend mode list is open on the Effects card.
    effects_mode_open: Option<usize>,
    /// The EQ card's band Shape list is open.
    eq_shape_open: bool,
    /// The Clip tab's Transition Kind list is open.
    transition_kind_open: bool,
    /// The Clip tab's Transition section is folded open.
    transition_section_open: bool,
    /// The Custom accent's hex being typed in Settings, which owns the keyboard while it is `Some`;
    /// each valid edit is in force at once and the file is written when the field is left.
    accent_hex: Option<player::text_field::TextField>,
    /// The Vivid or hue slider under the hand, and the two tracks' boxes ([`frac_along`]).
    accent_drag: Option<player::accent::AccentSlider>,
    accent_vivid_bar: Rc<Cell<Bounds<Pixels>>>,
    accent_hue_bar: Rc<Cell<Bounds<Pixels>>>,
    /// A scrub of one of the selected title's numbers, or a move of its block on the timeline,
    /// in flight: shown live, written as one undo step on release.
    title_scrub: Option<player::titling::TitleScrub>,
    /// A pan field of the Mix section being scrubbed: shown live, written on release.
    pan_scrub: Option<player::pan::PanScrub>,
    /// Which audio streams each imported file has, as its header describes
    /// them: one library row per entry. Keyed and filled like `waves` --
    /// presence means "asked" -- and an empty list is a silent file, which is
    /// exactly one row and no stream tags.
    streams: HashMap<PathBuf, Vec<StreamInfo>>,
    /// What each source is coded at, read off its header once and kept: what a
    /// properties card says about a file's rate. Filled like `streams` --
    /// presence means "asked" -- and the inner `None` is "asked, not answered
    /// yet", which the card draws as an ellipsis: the probe walks a Matroska's
    /// clusters, so a big film answers in seconds rather than at once.
    bitrates: HashMap<PathBuf, Option<MediaBitrate>>,
    /// What each source's header says about its container, size and picture
    /// ([`engine::probe_media`]), probed once and kept for the properties
    /// plates. Filled like `bitrates`: presence means "asked", and the inner
    /// `None` is "asked, not answered yet".
    facts: HashMap<PathBuf, Option<engine::MediaFacts>>,
    /// How big each still source's picture is, read from its header once and
    /// kept -- what a library row and its card say about a file that has no
    /// streams to describe. Filled like `streams`: presence means "asked", and
    /// `None` is a file with no picture to report (every source that is not an
    /// image, and one whose header would not read).
    sizes: HashMap<PathBuf, Option<(u32, u32)>>,
    /// Whether a source's own header says it carries a picture at all,
    /// probed once and kept: what [`Player::has_video`] reads for the lane a
    /// drag may land on ([`timeline_math::lane_refuses`]). An extension can
    /// lie -- a song muxed into an `.mp4` -- and only the header says the
    /// truth (`engine::demux::NoVideoTrack`). Filled like `sizes`: presence
    /// means "asked", and until the probe answers a file is read by its
    /// extension, the same guess the ghost always made.
    has_video: HashMap<PathBuf, bool>,
    /// Every frame of each source a decoder may be started from -- its sync
    /// points ([`engine::demux::sync_points`]), which are the frames a cut may
    /// be placed on for an export to *copy* the film instead of coding all of
    /// it again. Filled like `bitrates` and off the render thread for the same
    /// reason, and more so: the answer is that Matroska cluster walk. An empty
    /// list is a source with no grid to offer (an mp4, a still, a song), which
    /// must stay in the map or every repaint would ask again.
    syncs: HashMap<PathBuf, Vec<u32>>,
    /// Which decoder each source will run on, probed once at import and kept:
    /// the codec (`None` for a still) and the seat the engine picked for it --
    /// or, where the decoder refused the file, the refusal in its own words.
    /// What a library row says *before* anything plays; the running answer is
    /// the session's own (`PlaybackSession::decode_backend`), which follows a
    /// fallback this cannot. Filled like `sizes`: presence means "asked", and
    /// `Decoder::Pending` is that asked-but-unanswered window -- which must
    /// stay in the map or every repaint would ask again.
    decoders: HashMap<PathBuf, Decoder>,
    /// What an export of the picked settings would open -- the picture's seat
    /// or the copy that means it opens none, and the sound's -- and what it was
    /// asked about: the settings, the canvas and the *cuts*, since where they
    /// land is what decides whether the picture is copied at all. The probe
    /// opens a real VA-API encoder (~100 ms) and reads every source's header,
    /// so it runs off the render thread and only while the export card is up.
    /// The inner `None` is "asked, not answered yet".
    export_seat: Option<ExportSeat>,
    /// What this machine's GPU decodes and encodes, as the plugin answered it:
    /// asked once, off the render thread like `export_seat` and for the same
    /// reason (a VA-API init), and kept for the life of the process because the
    /// answer cannot change while we run. `None` is "not asked yet".
    hw_caps: Option<SharedString>,
    /// The copied clip, or clips: frame ranges and the lane each was on, in
    /// click order, so it survives the clip (or clips) it was taken from
    /// being deleted -- and it outlives the selection. One entry for a plain
    /// Copy, the whole ctrl-click set for one made over a selection; either
    /// way [`Player::paste`] drops the set at the playhead keeping the gaps
    /// and lanes between its members intact.
    clipboard: Vec<(Lane, Clip)>,
    /// What the last copy remembers of the clip's look, for Paste attributes.
    copied_look: Option<crate::player::editkeys::CopiedLook>,
    /// A drag that started on the ruler. Moves anywhere in the window scrub
    /// while it is set; the release commits the exact position.
    scrubbing: bool,
    /// What the hand has made of the three panel seams, and which one it is
    /// holding now. Kept here rather than worked out per frame from the
    /// window's own shares: a size a person dragged is a size that has to
    /// survive the release ([`Splits`]).
    splits: Splits,
    /// A drag that started on a divider, tracked on the root for `scrubbing`'s
    /// reason -- the strip is 6 px and the pointer leaves it at once.
    split_drag: Option<Split>,
    /// Where the seam was pressed and whether the pointer has since gone far enough
    /// to call it a drag: a click that never moves changes and saves nothing.
    split_press: Option<(Point<Pixels>, bool)>,
    /// A drag that started on a clip's edge, tracked on the root for
    /// `scrubbing`'s reason: a 6 px strip is not where the pointer stays. See
    /// [`Trim`].
    trim: Option<Trim>,
    /// A shift-drag roll, slip or slide in flight, tracked on the root for
    /// `trim`'s reason. See [`ProDrag`].
    pro_drag: Option<ProDrag>,
    /// The clip under the pointer and the modifiers held over it, for the
    /// status bar's line on what a drag there would do
    /// ([`crate::ui::bench_stance::clip::drag_status`]). `None` off every clip.
    hover_hint: Option<crate::ui::bench_stance::clip::DragHover>,
    /// The media-wall stop the dragged edge is pinned against, while it is
    /// pinned: the handle flashes warn for [`TRIM_FLASH`] at the first sample
    /// that holds there, and no later one re-arms it. `None` once the pointer
    /// comes back inside the room, and between trims.
    trim_flash: Option<TrimFlash>,
    /// The clip a move is carrying, by the lane and index its press named it
    /// under: drawn dimmed in place until the drop or the release lets it go.
    /// `None` between drags.
    drag_src: Option<(Lane, usize)>,
    /// A drag that started on a fade handle, tracked on the root for the same
    /// reason a trim is: a 10 px square is not where the pointer stays. See
    /// [`FadeDrag`].
    fade_drag: Option<FadeDrag>,
    /// A drag on a clip's volume envelope: a key or the line between two keys. See [`GainEdit`].
    gain_edit: Option<crate::interact::GainEdit>,
    /// The envelope key the pointer is over, as `(lane, clip, key)`, which is what the cursor and
    /// the line's own hover read.
    gain_hover: Option<(Lane, usize, crate::ui::bench_stance::clip_envelope::EnvHit)>,
    /// The envelope key last pressed, as `(lane, clip, source frame)`: Delete takes it out.
    gain_pick: Option<(Lane, usize, u32)>,
    /// How far into the clip the last press on a box landed, in timeline
    /// frames: what a drag lets go of is the *point that was grabbed*, so the
    /// head lands that much in front of the pointer and the clip does not jump
    /// under the hand. Recorded at the press because gpui hands the drop only
    /// the value being dragged, and stale between drags, which costs nothing --
    /// no drag starts without a press on the box it moves.
    grab: u32,
    /// Whether a drag or a trim is pulled onto the edges near it. On by
    /// default, because clips meeting exactly is what a timeline is for, and off
    /// by one stroke ([`ActionId::ToggleSnap`]) for the frame-by-frame placement
    /// no magnet may take away.
    snap: bool,
    /// Whether `alt` was down on the last sample of the gesture in flight:
    /// snapping off for *this* drag only, the override every NLE has for the
    /// one clip that has to sit a frame off an edge, leaving [`Player::snap`]
    /// -- the switch -- exactly as the hand left it. Read by
    /// [`Player::snap_live`], written by the bench's own drag samples and by
    /// the trim's, and stale between gestures, which costs nothing: every
    /// gesture writes it before it asks.
    drag_alt: bool,
    /// Whether Ctrl is held during a library drag: the drop inserts and pushes the rest along,
    /// and the status bar says so ([`ui::bench_stance::clip::asset_drop_status`]).
    drag_ctrl: bool,
    /// Whether a library drag hovers the empty bench below the last lane, where the drop makes
    /// a new track ([`ui::bench_stance::clip::asset_drop_status`]). Set by the bench's own move
    /// sample, cleared by a drop and by a row the pointer enters.
    drag_below: bool,
    /// Whether the subtitle lanes draw at all -- the mute over the shown lane
    /// ([`Player::sub_lane`]), not a lane of its own. On by default and off by
    /// one stroke
    /// ([`ActionId::ToggleSubtitles`](keymap::ActionId::ToggleSubtitles)) for
    /// anyone watching the picture rather than reading it. The player's, not the
    /// project's: it changes nothing that is saved and nothing that is exported.
    subs_on: bool,
    /// Whether audio clips draw their waveform ([`ActionId::ToggleWaveforms`]). Kept
    /// with the project's view ([`view_state`]), so it reopens as it was left.
    waveforms: bool,
    /// Whether a ruler scrub while paused plays sound ([`ActionId::ToggleScrubAudio`]). Kept with
    /// the project's view, like the waveforms.
    scrub_audio: bool,
    /// The Play Around run in flight, if any: where the playhead came from, and
    /// where the run stops. See [`PlayAround`].
    play_around: Option<PlayAround>,
    /// Which palette row is *selected*: an index into
    /// [`PlaybackSession::subtitles`], which is what the × takes off
    /// ([`ActionId::RemoveSubtitleTrack`](keymap::ActionId::RemoveSubtitleTrack))
    /// and what the list marks. A selection and nothing more -- what is on
    /// screen is what is placed on a subtitle lane
    /// ([`Player::subtitle_overlay`]), so picking a row shows nothing by itself,
    /// exactly as clicking a media row plays nothing. Cleared with the timeline
    /// like every other index here -- track 2 of one project is not track 2 of
    /// the next.
    sub_track: usize,
    /// Which files' subtitle groups in the library are folded shut, keyed by
    /// the group's own path -- the same key [`subtitle_rows`] groups on, so a
    /// fold survives a track being added or removed off the file it belongs
    /// to. Session-lifetime only: a fold is a "not looking at this one right
    /// now", not a project setting, so it is never saved and every file opens
    /// unfolded.
    sub_folded: HashSet<PathBuf>,
    /// Which subtitle lane is *shown*: one of them and never two, the way a
    /// player has one subtitle track chosen at a time -- two hundred lanes of
    /// different words all drawn at once is two hundred plates over one
    /// picture, and nothing readable.
    ///
    /// `None` is nobody having picked yet, which reads as the first subtitle
    /// lane ([`Player::active_sub_lane`]) -- so one lane is shown without a
    /// pick, the first lane added draws, and a pick naming a lane that has
    /// since gone falls back to the first rather than showing nothing.
    ///
    /// Read where the cues are drawn ([`Player::subtitle_overlay`]) and nowhere
    /// else: what an *export* writes is every lane on the timeline
    /// ([`engine::export`]), which this pick has no say in. Not saved -- a
    /// `.edith` has no line for it yet.
    ///
    /// A [`Lane`] handle is a position among its kind, so a removal or a
    /// reorder leaves a pick naming another track: it is dropped there,
    /// exactly as the selection and the open cards are ([`Player::remove_lane`],
    /// [`Player::reorder_lane`]), which shows the first lane again.
    sub_lane: Option<Lane>,
    /// The frame the live gesture is about to land on, or `None` while it is
    /// over open bed: the line every lane draws so the snap is seen before it
    /// happens rather than discovered after the release. Stale between gestures,
    /// which costs nothing -- it is drawn only while one is live.
    snap_cue: Option<u32>,
    /// The frame a ruler or empty-track menu was opened on: what "Add marker here" and
    /// "Move playhead here" act on. Stale once the menu is gone, read only by those two rows.
    menu_frame: Option<u32>,
    /// The box the same gesture is about to fill, or `None` while the pointer is
    /// over no lane: the shadow every proper editor draws under a drag. Set by
    /// the lane the pointer is actually over -- that is the one question the
    /// line above does not answer -- and drawn only while a drag is live, for
    /// [`Player::snap_cue`]'s reason. More than one entry only while a
    /// set-drag ([`Player::move_clip`]'s `set_move`) is carrying more than
    /// the clip under the hand: every other pick draws its own shadow too, at
    /// the same delta the anchor's is landing at ([`Player::preview_ghost`]).
    ghost: Vec<Ghost>,
    /// The window `x` the last live `on_drag_move` sample over a bed used to
    /// draw [`Player::ghost`] -- read back by that drag's `on_drop` instead of
    /// a fresh `window.mouse_position()`. gpui's drop callback carries no
    /// event of its own to read a position off, and a second, later poll of
    /// the pointer is not guaranteed to answer what the ghost just showed
    /// (DESIGN: "drop == shadow, by construction"). `None` between drags.
    drag_x: Option<Pixels>,
    /// The lane the last live sample of the drag in flight was over -- the one
    /// a release off the rows themselves (left of the heads, between two rows,
    /// below the last track) lands on, so a gesture that has already promised a
    /// landing in the shadow cannot end in nothing. `None` before the pointer
    /// has been over any lane, where there is no landing to promise.
    drag_lane: Option<Lane>,
    /// The slot a track header being dragged is about to drop into, or `None`
    /// while the pointer is over no lane: the line drawn between two headers,
    /// for the reason [`Player::ghost`] draws a shadow -- where a gesture lands
    /// is seen before the release. Drawn only while a drag is live.
    lane_drop: Option<LaneDrop>,
    last_scrub: Instant,
    last_target: u32,
    /// The running export. While it owns the UI the editor is read-only.
    export: Option<ExportHandle>,
    /// The export above was cancelled and is only winding down. The editor is
    /// already free -- the worker took its own copy of the edit list -- but the
    /// handle is held until the worker settles, because its last act is to
    /// delete the output file and a second export must not be what it deletes.
    cancelling: bool,
    /// The pointer asked to cancel and the progress card is showing the pair
    /// that answers it ("Keep exporting" / "Cancel export"). One press is never
    /// the cancel itself: an hour of encoding must not end on a stray click,
    /// which is the reason the stroke is a chord too ([`cancels_export`]).
    cancel_armed: bool,
    /// When the running export started, and how far it had come at each sample
    /// since, as `(elapsed, progress)` marks. The elapsed clock and the
    /// rolling-window estimate the progress line reads; see [`note_progress`].
    export_started: Option<Instant>,
    export_marks: Vec<(f32, f32)>,
    /// Exports set up to run later, one after another, each from the timeline
    /// as it was when added ([`export_queue`]). Lives for the session.
    queue: export_queue::ExportQueue,
    /// The file an import worker is reading right now, and the files waiting
    /// behind it in arrival order. Unlike an export, an import owns nothing:
    /// the editor stays live, the timeline keeps playing, and the only thing
    /// this holds is the line above the panel ([`Player::import_bar`]).
    importing: Option<Import>,
    imports: std::collections::VecDeque<PathBuf>,
    /// The file argv named, until its read lands. It goes through the queue
    /// above like any other file -- that is what puts the window on screen
    /// before a byte of a 25 GB film is read -- and this is what tells
    /// [`Player::take_import`] that this one is an *open* and not an import:
    /// it becomes the timeline, and the clock, the title and the export path
    /// come from it. Cleared the moment it lands, so a later drop of the very
    /// same path is an import like any other.
    opening: Option<PathBuf>,
    /// Where an export writes. Built once from the source path, which is not
    /// otherwise kept.
    export_path: PathBuf,
    /// The export target this window itself last wrote or confirmed, so the
    /// overwrite question ([`Player::export_ask`]) can tell "the file exists"
    /// apart from "the file exists and is a stranger's": an export over a
    /// path this window never wrote asks first, a re-export over its own
    /// output does not (user 2026-09-11, "not just save but export too").
    /// Path equality, not a flag: retargeting the destination to somewhere
    /// else makes the answer foreign again.
    export_known: Option<PathBuf>,
    /// Where the save action writes: the project this timeline was loaded from,
    /// or the one derived beside the media it started as. Saving twice
    /// overwrites the same file rather than making a second one.
    project_path: PathBuf,
    /// Whether an edit has landed since the sidecar last caught up with it --
    /// set at every edit's own door ([`Player::mark_dirty`]) and cleared once
    /// [`Player::autosave_tick`] has written it out. `project_path` itself is
    /// never touched here: only [`Player::save_project`] writes the real file.
    autosave_dirty: bool,
    /// Edits landed since the autosave last caught up. Reaching
    /// [`AUTOSAVE_EDITS`] makes the next tick write without waiting for the timer.
    autosave_edits: u32,
    /// When the autosave was last actually written. A dirty session goes at
    /// most [`AUTOSAVE_PERIOD`] without one.
    autosave_last_run: Option<Instant>,
    /// A structural edit landed ([`Player::mark_structural`]): the next autosave goes out as
    /// soon as [`AUTOSAVE_THROTTLE`] allows, rather than at the next 20 edits.
    autosave_urgent: bool,
    /// An autosave found newer than the project it sits beside, offered as a
    /// notice with Recover and Discard buttons the moment that project opens
    /// ([`Player::install_project`]). The path is the autosave's own, so
    /// recovering needs nowhere else to look. Nothing is discarded unless the
    /// person says so -- `project_path` is never touched by either answer.
    recovery_sidecar: Option<PathBuf>,
    /// Why the last Recover could not bring the unsaved edits back, in plain words,
    /// while its dialog is up. The window is as it was and the autosave is kept.
    recovery_failure: Option<String>,
    /// Whether the unsaved-work question is on screen: raised by the window's
    /// own close request when [`Player::close_needs_answer`] says the timeline
    /// holds work the project file does not (user 2026-09-10, "if timeline is
    /// not empty and user does alt+f4 or exit somehow, ask user to save").
    /// While it is up it owns the keyboard -- three answers and nothing else
    /// ([`ui::stance`]'s root `on_key_down`).
    quit_ask: bool,
    /// Whether the overwrite question is on screen: raised by Save's own
    /// door ([`Player::save_guarded`]) when the save's target exists on
    /// disk but `autosave_armed` says this window never loaded or wrote it
    /// -- a `.edith` the timeline only derives its name from beside an
    /// imported media file (user 2026-09-11, "saving over an existing file
    /// must warn first"). Same shape as `quit_ask` above: while it is up
    /// it owns the keyboard, and answering `enter` confirms the overwrite
    /// -- the save then arms `autosave_armed`, so later saves on this
    /// file are silent.
    save_ask: bool,
    /// The File menu's project dialogs -- Save version, Versions, Collect media -- while one
    /// is up; it owns the keyboard ([`Player::project_dialog_key`]).
    proj_dialog: Option<player::project_dialogs::Dialog>,
    /// A project started from File > New > Project…: where it will be saved, and the name it
    /// goes by, until its first footage arrives and the file can exist (a project names at
    /// least one source). `new_project_fresh` says that arrival still owes the first save.
    new_project: Option<PathBuf>,
    new_project_fresh: bool,
    /// Whether the export's overwrite question is on screen: raised inside
    /// [`Player::start_export`] -- after its refusals, so an export that
    /// cannot run never asks -- when `export_path` exists on disk and
    /// `export_known` says this window never wrote it. Same shape as
    /// `save_ask` above: while it is up it owns the keyboard, and answering
    /// `enter` marks the target known and starts the export.
    export_ask: bool,
    /// The export sheet's own state beside the settings above: the range
    /// radio, the preset last picked, the destination fields, the measured
    /// pace and the finished face ([`export_ui::ExportSheet`]).
    export_sheet: export_ui::ExportSheet,
    /// Whether `project_path` names a file this window itself put there --
    /// loaded from a `.edith` ([`Player::install_project`]) or written by a
    /// manual save ([`Player::save_project`]) -- rather than one merely
    /// derived beside an imported media file. [`Player::autosave_tick`]
    /// requires it: a path derived from `media.mp4` names `media.edith`
    /// whether or not that file is the user's own saved project, and writing
    /// a sidecar beside a *foreign* `.edith` the user never opened is what
    /// let a scratch timeline's autosave outdate and "recover" over a real
    /// one. `mark_dirty` does not touch this -- an edit earns nothing an open
    /// or save has not already granted.
    autosave_armed: bool,
    /// The project file's stamp as the window last accounted for it: set on
    /// open, on its own save, and when a change on disk has been answered.
    /// [`Player::follow_tick`] compares the file against it.
    disk_known: Option<follow::Stamp>,
    /// Whether the window holds edits the project file does not. Unlike
    /// `autosave_dirty` this is not cleared by the sidecar write, so it
    /// still says the file is behind after an autosave.
    edited_since_save: bool,
    /// The engine's revision ([`engine::PlaybackSession::revision`]) of the
    /// timeline at the last save, open or discard. Undo and redo compare the
    /// revision they land on with it, so undoing back to the saved timeline
    /// clears `edited_since_save`. `None` when there is no saved timeline yet.
    saved_revision: Option<u64>,
    /// Which stroke means what, and what every shortcut on screen is called.
    /// The one place either question is answered.
    keymap: Keymap,
    /// How loud the monitoring is, and whether it is muted. Lives here rather
    /// than in the session so it survives closing one file and opening the
    /// next -- it is a setting of the player, not of the timeline, which is
    /// also why it is not written to the project file and cannot reach an
    /// export. This is the *owner*: the engine holds the level per session, on
    /// the device that runs it, and restores it when that device is replaced
    /// ([`engine::PlaybackSession::set_gain`]), so every session is a
    /// follower of this one copy and none of them can answer for it.
    /// [`Player::apply_volume`] is what pushes it, at every session there is.
    volume: Volume,
    /// Where the volume slider was last painted, and whether a hand is on it --
    /// the speed bar's pair, for the speed bar's reason: the pointer moves
    /// arrive at the root, so the bar's own geometry has to be readable there.
    volume_bar: Rc<Cell<Bounds<Pixels>>>,
    volume_dragging: bool,
    /// Where the preview's own seek bar was last painted, and whether a hand
    /// is on it -- `volume_bar`'s pair and `scrubbing`'s reason: the bar
    /// spans the preview's whole length rather than a zoomed [`Scale`], so
    /// it needs no `ruler` of its own, only its geometry.
    preview_bar: Rc<Cell<Bounds<Pixels>>>,
    preview_scrubbing: bool,
    /// The two stand-in switches ([`engine::proxy`]) as this window has them
    /// set: whether the picture is cut on the stand-ins, and whether an
    /// arriving film gets one made for it. Kept here for the volume's reason
    /// and one more of their own -- they are *import* options, so they have to
    /// be settable before there is any import to have a session about.
    ///
    /// The session's own pair is the project's (it is saved with it): these are
    /// pushed at every new one ([`Player::apply_proxies`]) and taken back from
    /// a project as it loads ([`Player::install_project`]), so the button, the
    /// switch and the file cannot come to disagree. Their initial values are
    /// the ones a fresh session comes up at.
    proxies_on: bool,
    auto_proxies_on: bool,
    /// The autosave period in seconds, `None` for Off ([`player::library::AUTOSAVE_CHOICES`]).
    autosave_secs: Option<u64>,
    /// The keybindings overlay is up. While it is, it owns the keyboard and the
    /// pointer: a stroke or a click meant for a row must not also cut the
    /// timeline.
    keys_open: bool,
    /// The About dialog is up (Help → About edith…).
    about_open: bool,
    /// The settings page is up (`?` overlay's sibling, but a latch rather
    /// than a hold -- it carries clickable rows, not a glance-and-release
    /// list). Cleared by [`Player::close_card`], the one reset list every
    /// opener routes through.
    settings_open: bool,
    /// Which of the settings page's three tabs is showing
    /// ([`ui::settings_stance::render`]'s strip). A field rather than state
    /// inside the page so a close and a reopen return to the section a hand
    /// left; it is deliberately *not* in [`Player::close_card`]'s reset list,
    /// the same way the dock's tab pick outlives its panel
    /// ([`Player::dock_src_active`]). Nothing but the strip reads it, and
    /// picking a tab writes no project value, so the pick never arms
    /// autosave.
    settings_tab: ui::settings_stance::SettingsTab,
    /// What the KEYS tab is waiting on (a filter, a capture, a swap question),
    /// and the filter's text. Display state only: nothing here reaches a
    /// project, and the keymap itself is saved once a stroke is taken.
    settings_key_edit: Option<ui::settings_stance::KeyEdit>,
    settings_keys_filter: String,
    /// Which of the showing tab's choice rows the keyboard is on
    /// ([`ui::settings_stance::settings_arrow`]): `↑`/`↓` move it, `←`/`→`
    /// step that row's value. Display state only, cleared with a tab switch.
    settings_focus: Option<usize>,
    /// Where the actions list is scrolled to. Held here rather than left to
    /// the wheel alone: forty actions are four times what a 360 px window
    /// shows, and the rows past the fold have to be reachable from the
    /// keyboard.
    keys_scroll: ScrollHandle,
    /// The export panel body's scroll (wheel, and up/down/page keys).
    export_scroll: ScrollHandle,
    /// The Ctrl+K command palette is up (DESIGN §4.4). While it is, it owns the
    /// keyboard: its field takes the typed letters and the arrows move the
    /// highlight. Opened and closed through `Player::show_actions` and the
    /// palette's own keys in `ui::stance`.
    pub(crate) palette_open: bool,
    /// What the palette's field holds, matched against every action's label.
    pub(crate) palette_query: String,
    /// The highlighted row, an index into `Player::palette_list`.
    pub(crate) palette_sel: usize,
    /// The palette's row list. Scrolled to keep the highlight in view.
    pub(crate) palette_scroll: ScrollHandle,
    /// Where the lane column and the inspector's rows have been taken to. Read
    /// back at render, not only written by the wheel: the line each of them
    /// carries about what is below the fold is a count of what is *still* below
    /// it, and a scroll that nothing reads is a scroll the affordance cannot
    /// follow.
    lanes_scroll: ScrollHandle,
    /// And where the equalizer card's own body has been taken to. It is the
    /// tallest card in the column -- a graph with a row of numbers and a row of
    /// buttons under it -- and at the 360 px floor its title and its buttons
    /// were off both ends of the column with no way to reach them.
    eq_scroll: ScrollHandle,
    /// The export sheet's scrolled body: read at paint time so its "more
    /// below" fade shows only while there is more below.
    pub(crate) export_body_scroll: ScrollHandle,
    /// The Settings rows' scroll, for the same fade.
    pub(crate) settings_rows_scroll: ScrollHandle,
    /// The export options card is up: what the export action opens now, so
    /// nothing is written until the card's own button says so. One card at a
    /// time -- opening either closes the other, since both are the whole window
    /// and two stacked scrims say nothing about which one is listening.
    export_open: bool,
    /// The dock's Transcript tab is showing, and its words.
    transcript_open: bool,
    transcript: player::transcript::Transcript,
    /// The export budget, in bits per second -- the moment's one number, and
    /// the only thing the encoder actually takes. Zero means nobody has
    /// chosen: the lever then reads the engine's own automatic figure for
    /// this picture ([`Player::budget_bps`]), which is what keeps it off the
    /// "0 Mbps" the old `custom_mbps: 0` default put on screen.
    custom_bps: u64,
    /// The budget *while it is being typed*, or `None` when nobody is typing.
    /// Free text and not digits: `850k`, `6.5M`, `6.5` and `1.2G` are all
    /// ways of saying one budget ([`parse_budget`]). Nothing on this surface
    /// takes gpui focus (the root keeps the keyboard), so the field is a
    /// modal state on the player and the root's handler types into it.
    budget_edit: Option<String>,
    /// The Clip tab's transition duration row, the same way (DEBT #111):
    /// the anchor clip's frame count while it is being typed, `None` when
    /// nobody is. Opened on the row's own click, closed on the same field's
    /// enter/esc the export card's field already answers to
    /// ([`Player::param_card_key`]).
    transition_edit: Option<ui::kit::NumberEdit>,
    /// What the *sound* is coded at, in kbps, for every format that encodes it
    /// -- the AAC inside a video export as much as an MP3. Kept across closes
    /// like the picture's quality, and starts at the figure this program wrote
    /// before the row existed ([`engine::export::DEFAULT_AUDIO_KBPS`]), so a
    /// user who never touches the row gets the file they always got.
    audio_kbps: u32,
    /// Which file the card will write. Kept across closes like the quality, and
    /// what [`Player::export_path`](Player) is named after.
    format: Format,
    /// The equalizer card is up on this clip -- the lane and index it was
    /// opened on. Held rather than re-read from `selected` every paint because
    /// the card is modal: while it is up nothing else can move an index, and
    /// the one edit it makes (`set_eq`) moves none.
    eq_open: Option<(Lane, usize)>,
    /// Whichever of the seven param cards is open, filling the room in place
    /// of the bench/ledger rows instead of its usual capped, floating size
    /// (`Player::toggle_maximize`). One flag, not one per card, because only
    /// one of them is ever open at a time (`close_card`'s own rule) and it
    /// outlives any one card's close/reopen, persisted like the dock tab pick
    /// (`ui::dock_stance::load_maximized`/`save_maximized`).
    card_maximized: bool,
    /// The curve the card is showing, which is the clip's own or the flat
    /// five-band default. Edited live and written at the clip once per gesture
    /// ([`Player::commit_eq`]): the project's equalizer table is append-only, so
    /// a write per pointer sample would be a table entry -- and an undo step --
    /// per pixel.
    eq_params: EqParams,
    /// Which band the keyboard moves, and which one a drag is holding.
    eq_band: usize,
    /// A handle on the curve is being dragged. Tracked on the root like
    /// `scrubbing`, for the same reason: a hand pulling a band to +12 dB runs
    /// off the top of the graph long before it lets go.
    eq_dragging: bool,
    /// The curve box, recorded at prepaint: gpui hands a mouse listener the
    /// window position only, so this is what a press and a drag are read
    /// against ([`frac_along`], [`frac_down`]).
    eq_graph: Rc<Cell<Bounds<Pixels>>>,
    /// Whether the analyser is drawn behind the curve. On by default -- what
    /// the curve is being *shaped against* is the point of drawing it -- and
    /// off with one press for anyone who would rather read the curve alone.
    /// Card state, not the project's: it changes nothing that plays.
    eq_spectrum: bool,
    /// The colour card is up on this clip -- the lane it is on and its index
    /// there. `None` when it is closed, which is the only place that state
    /// lives: the grade itself is the project's.
    color_open: Option<(Lane, usize)>,
    /// The speed card is up on this clip -- the lane it is on and its index
    /// there, exactly as the colour card's handle is. `None` when it is closed;
    /// the rate itself is the project's, so there is nothing else to hold.
    speed_open: Option<(Lane, usize)>,
    /// The speed bar's box, recorded at prepaint: a mouse listener is handed the
    /// window position only, so this is what a press and a drag are read against
    /// ([`frac_along`]).
    speed_bar: Rc<Cell<Bounds<Pixels>>>,
    /// The bar is being dragged. On the root like the colour card's, for the
    /// same reason: the pointer leaves a 4 px bar on the first move.
    speed_dragging: bool,
    /// The rate the hand is on, held back because the worker still owes a frame
    /// ([`Player::flush_drag`]). What the bar draws while it stands, so the
    /// handle stays under the hand even though the picture has not caught up.
    pending_speed: Option<Speed>,
    /// The speed card is in its ramp mode: the bar sets the speed of the key the next
    /// "Add key" puts at the playhead, not the clip's constant rate. On when the card
    /// opens on a clip that already has a ramp, and by the card's Ramp tab.
    speed_ramp_mode: bool,
    /// The speed the ramp mode's bar holds for the next key; `None` follows the
    /// ramp's own speed at the playhead.
    ramp_key_speed: Option<Speed>,
    /// The mix card is up: every audio track's own volume and the master
    /// limiter, which are project settings and not any clip's -- so unlike the
    /// four clip cards there is no handle to hold, only whether it is open.
    mix_open: bool,
    /// The markers card is up ([`Player::open_markers`]): the project's comment markers
    /// as rows, one card at a time like the rest.
    markers_open: bool,
    /// The undo history card (Edit > History), in the Clip tab beside the others.
    history_open: bool,
    /// The creator tools card is up ([`Player::open_tools`]): Normalize, Captions, Fillers and
    /// Voiceover on four tabs, one card at a time like the rest.
    tools: Option<player::tools::Tools>,
    /// Which run of a tool's background job is current, so a late result is dropped.
    tools_runs: u64,
    /// The effects card is up on this clip -- the lane it is on and its index
    /// ([`Player::open_effects`]).
    effects_open: Option<(Lane, usize)>,
    /// The stack a held bar is showing, written as one undo step on release.
    effects_draft: Option<Vec<engine::effects::Effect>>,
    /// The bar held: the effect's place in the stack and the parameter's in the effect.
    effects_drag: Option<(usize, usize)>,
    /// The kinds are listed under the card's Add effect button.
    effects_adding: bool,
    /// Each bar's box, recorded at prepaint, at `effect * MAX_PARAMS + param`.
    effects_bars: std::cell::RefCell<Vec<Rc<Cell<Bounds<Pixels>>>>>,
    /// The row the markers card has picked, an index into the session's markers.
    marker_sel: usize,
    /// The name or note being retyped on the card, which owns the keyboard while it is
    /// `Some` ([`Player::marker_edit_key`]).
    marker_edit: Option<MarkerEdit>,
    /// Which of its rows the arrow keys move -- a fader, the limiter's ceiling
    /// or its switch. The card's own focus, since nothing in it takes gpui's
    /// (ledger:182).
    mix_field: usize,
    /// The lanes soloed for listening. Not saved and not an undo step: the
    /// session is handed the set ([`PlaybackSession::set_solo`]), and an export
    /// never reads it.
    mix_solo: Vec<Lane>,
    /// The mix card's meters and slider bars, drawn from `&self`, so behind a
    /// `RefCell` ([`player::cards::MixPanel`]).
    mix_panel: std::cell::RefCell<player::cards::MixPanel>,
    /// The mix slider a drag is moving, if one is.
    mix_drag: Option<player::cards::MixDrag>,
    /// The cue plate's own size, in force this session and kept in
    /// `~/.config/edith/subtitle-style` beside the keybindings -- an
    /// app-global preference like the theme, since export never burns
    /// subtitles into the picture. [`SUB_TEXT`] is the default it opens at,
    /// never the value read at paint; [`sub_line_h`](Player::sub_line_h) is
    /// what preview.rs reads for the line height, derived from this.
    sub_text: f32,
    /// Which family the cue plate draws in, or the platform default with
    /// nothing picked -- `None` calls no [`gpui::Styled::font_family`] at
    /// all, which is what leaves the window's own choice in force.
    sub_family: Option<String>,
    /// The outline, the background box and the place of the cue plate: the
    /// rest of the look, kept in the same file as the size and the family.
    sub_outline: SubOutline,
    sub_boxed: bool,
    sub_place: SubPlace,
    /// The size slider is held: a move sets the size, the release keeps it.
    sub_dragging: bool,
    /// The caption style card's font dropdown is open.
    sub_font_open: bool,
    /// The words typed into the open font dropdown's search line (empty when it is closed).
    sub_font_query: String,
    /// The subtitle style card is up.
    subtitle_style_open: bool,
    /// Which of its rows the arrow keys move: 0 is the size stepper, every
    /// row after it a family in [`Self::subtitle_fonts`]. The card's own
    /// focus, since nothing in it takes gpui's (ledger:182).
    subtitle_style_field: usize,
    /// The Animation dropdown of the style card is open (its list hangs from the field).
    sub_anim_open: bool,
    /// The arrow keys have moved a row of the style card, so its pick wears the row
    /// wash; a mouse press on the card clears it again, so a mouse-only user sees no wash.
    sub_keys_used: bool,
    /// Every family the platform can draw, asked for once when the card
    /// opens and kept for as long as it is up: [`gpui::TextSystem::all_font_names`]
    /// walks the whole font registry, which is not a repaint's business.
    subtitle_fonts: Vec<String>,
    /// The silence card is up on this clip -- the lane it is on and its index
    /// there, exactly as the speed card's handle is.
    silence_open: Option<(Lane, usize)>,
    /// The mark-the-beats card is up on this clip, the same handle as `silence_open`.
    beats_open: Option<(Lane, usize)>,
    /// Which beat the card marks: 0 every beat, 1 every second, 2 every fourth.
    beats_every: usize,
    /// When the listening started, while the analysis runs; `None` when idle.
    beats_scan: Option<Instant>,
    /// What the last run said, shown under the section's controls.
    beats_last: Option<String>,
    /// What a scan is told to look for, and how fast the speed-up button plays
    /// what it found. Kept across closes like the export card's quality: a
    /// second run offers what the first one settled on.
    silence: engine::silence::Settings,
    silence_factor: Speed,
    /// How wide the apply reaches ([`Scope`]). Kept across closes for the same
    /// reason, and never *widened* on anyone's behalf: the whole point of it is
    /// that a track nobody named does not move.
    silence_scope: Scope,
    /// Which of the card's [`SILENCE_ROWS`] the arrow keys move. The card's own
    /// focus, since nothing in it takes gpui's (ledger:182).
    silence_field: usize,
    /// Whether the threshold is *labelled* dBFS or dB. Display only, and the
    /// number is the same either way: the setting is a level below full scale,
    /// so 0 is the loudest sample a file can hold and -40 is forty decibels
    /// under it -- "dBFS" names that reference out loud, "dB" leaves it unsaid.
    /// No conversion is hiding behind the row (there is no reference here worth
    /// inventing, and a made-up SPL would be a lie about what was measured);
    /// what it changes is which of the two spellings a person reads.
    silence_dbfs: bool,
    /// Keep only the silences: the card's applies act on the *complement* of
    /// what the scan found -- the speech between the quiet stretches -- so a
    /// cut removes the words and leaves the room tone. Off, today's jumpcut,
    /// until asked; kept across closes like [`Self::silence_dbfs`], which is
    /// the same kind of switch: how the card reads, not what it scans.
    silence_keep_only: bool,
    /// What the last scan found, in timeline frames: what the lane draws marks
    /// over and what an apply acts on -- *exactly* the previewed set, never a
    /// second scan at the moment of the press.
    silence_marks: Vec<(u32, u32)>,
    /// The levels of every stretch scanned this session, kept so moving a
    /// threshold is arithmetic rather than another decode. Keyed by
    /// [`ScanKey`], and not one entry: two films on one timeline would
    /// otherwise evict each other, and the decode being paid twice is the fifty
    /// seconds this card exists to not spend.
    silence_levels: HashMap<ScanKey, Arc<Vec<f32>>>,
    /// The scan a worker is running for the card, if one is. `None` means the
    /// card is drawing numbers it already has.
    silence_scan: Option<SilenceScan>,
    /// Whole-source background scans in flight, keyed the same as
    /// [`Self::silence_levels`]'s own whole-source entries
    /// ([`full_scan_key`]). Started at import ([`Player::cache_media`]) so a
    /// card opened later finds the levels already read; presence here is what
    /// stops a re-add of the same source from starting a second one, and its
    /// [`engine::silence::Progress::cancel`] is what a removed source is told.
    silence_bg: HashMap<(PathBuf, usize), Arc<engine::silence::Progress>>,
    /// Which of the card's four sliders the arrow keys and a drag move. The
    /// card's own focus, since nothing in it takes gpui's (ledger:182).
    color_band: usize,
    /// A slider is being dragged. Tracked on the root like `scrubbing` and the
    /// equalizer's drag, for the same reason: a 4 px bar is left by the pointer
    /// on the first move and its own listeners then stop firing.
    color_dragging: bool,
    /// HSV wheel on CLIP: same 4 px reason as `color_dragging` — the pointer
    /// leaves the disk on the first move, so samples live on the root.
    viz_hs_dragging: bool,
    /// The wheel's box, recorded at prepaint for a drag's window position.
    viz_wheel: Rc<Cell<Bounds<Pixels>>>,
    /// Where the picture actually lands on screen, probed by the picture area
    /// (`render.rs`): what tells the session to paint at the size the preview
    /// is *watched* at rather than at the project's own
    /// ([`engine::PlaybackSession::set_view_size`]).
    picture_box: Rc<Cell<Bounds<Pixels>>>,
    /// A screenshot asked for while the picture was painted at the preview's
    /// own size: `(session, stem, timecode)`, written by [`Player::flush_shot`]
    /// once the seat has painted one frame at the project's size. Taken at the
    /// ask so a shot mid-playback is named for the frame it was asked at, not
    /// for the one it landed on -- and *keyed to the session that asked*, so a
    /// swap mid-flight (a preview opened, another file opened, a session
    /// closed) cannot have the new session's frame written under the old name
    /// ([`Player::session_swapped`]).
    shot_pending: Option<(u64, String, String)>,
    /// The active session's identity, for the above: bumped by every door that
    /// swaps `session` or `preview_session`.
    session_gen: u64,
    /// The engine's own word on the sound device, as the last pump saw it: the
    /// cache [`Player::watch_audio`] compares each frame against so a reason is
    /// announced once when it *changes* and taken back when it clears. `None`
    /// while the sound is up; seeded by [`Player::session_swapped`], which every
    /// door that installs or drops a session already calls.
    audio_reason: Option<String>,
    /// The engine's last word on the *picture*, cached so a frame can tell a
    /// reason that changed from one that was merely read again. Keyed to
    /// `session_gen` for the same reason `audio_watch_gen` is: `pump` serves
    /// the preview while one is up and the timeline otherwise, and an unkeyed
    /// cache would read the other session's reason.
    decode_reason_cache: Option<String>,
    decode_watch_gen: u64,
    /// Which session generation (`session_gen`) the cached reason above describes:
    /// the watch is keyed to the session it is a statement about, so a preview
    /// opening or closing seeds the cache instead of announcing the *other*
    /// session's device.
    audio_watch_gen: u64,
    /// Which of the clip's two inks the look section's wheel and colour field
    /// act on ([`VizSlot`]): the clip's own colour, or the ring's dual half.
    /// The pick stays picked across clips, the way the dock tab's does.
    viz_slot: VizSlot,
    /// The colour field's text, while the Clip tab's colour row is being typed
    /// into ([`Player::edit_viz_hex`]). `None` is the row showing the colour
    /// itself; `Some` is the buffer, its refusal, and the caret on screen.
    viz_hex_edit: Option<ColorEdit>,
    /// The subtitle cue being retyped on its block, while one is open
    /// ([`Player::open_cue_edit`]). `None` is every block showing its words.
    cue_edit: Option<CueEdit>,
    /// The caption cue picked on a subtitle lane (`Lane`, track, cue): the one block a click
    /// selected, which the caption menu and the Delete key act on ([`player::caption_cues`]).
    sel_cue: Option<(Lane, usize, usize)>,
    /// A number of the caption panel being typed (start, end or length) and its buffer.
    cue_num: Option<(player::caption_cues::CueNum, ui::kit::NumberEdit)>,
    /// A caption panel number being dragged sideways ([`player::caption_cues::CueNumScrub`]).
    cue_num_scrub: Option<player::caption_cues::CueNumScrub>,
    /// The settings page's still image length, while it is typed in seconds.
    still_num: Option<ui::kit::NumberEdit>,
    /// A caption's edge or body being dragged, until the release writes it as one edit.
    cue_drag: Option<player::caption_cues::CueDrag>,
    /// The caption menu, while a right-click on a cue block has it open.
    cue_menu: Option<player::caption_cues::CueMenu>,
    /// The ink the hand is on, held back because the worker still owes a frame
    /// ([`Self::pending_color`]'s own reason, and a wheel sample is heavier
    /// than a grade's: it is not a table entry but the painter's own input, so
    /// the write is what rebuilds the picture). What the wheel draws while it
    /// stands, and never lost -- the frame that lands writes it, and so does
    /// the release. The slot the sample was taken on rides with it.
    pending_viz: Option<(VizSlot, u8, u8)>,
    /// Each slider's box, recorded at prepaint: a mouse listener is handed the
    /// window position only, so this is what a press and a drag are read against
    /// ([`frac_along`]). One per band, because the press picks the row it landed
    /// on and the drag then belongs to that row's range.
    color_bars: [Rc<Cell<Bounds<Pixels>>>; COLOR_SLOTS],
    /// The colour card's section on show ([`ColorSection`]): one at a time, so
    /// the card keeps its height whichever the hand is in.
    color_section: ColorSection,
    /// The Wheels section's wheel (lift, gamma, gain) and the Curves section's
    /// channel (master, red, green, blue).
    color_wheel: usize,
    /// A wheel's puck (`false`) or master bar (`true`) is being dragged, on that
    /// wheel: the root follows the pointer, as for `color_dragging`.
    color_wheel_drag: Option<(usize, bool)>,
    /// Each wheel's pad box and master bar box, recorded at prepaint.
    color_pads: [Rc<Cell<Bounds<Pixels>>>; 3],
    color_masters: [Rc<Cell<Bounds<Pixels>>>; 3],
    color_curve: usize,
    /// What the graph over the colour controls shows, and the scopes counted
    /// off the frame on screen while it shows one (`None` for the histogram,
    /// which counts nothing extra).
    scope_view: ScopeView,
    scopes: Option<engine::scopes::Scopes>,
    /// The grade the hand is on, held back because the worker still owes a
    /// frame ([`Player::flush_drag`]): a live write into a busy worker only
    /// cancels the open the picture is already waiting for, so a bar-wide sweep
    /// would pay for forty of them and show one. What the sliders draw while it
    /// stands, and never lost -- the frame that lands writes it, and so does the
    /// release.
    pending_color: Option<ColorParams>,
    /// The transform card is up on this clip -- the lane it is on and its
    /// index there, exactly as the colour card's handle is. `None` when it is
    /// closed: the placement itself is the project's.
    transform_open: Option<(Lane, usize)>,
    /// Which of the card's eight sliders the arrow keys and a drag move. The
    /// card's own focus, since nothing in it takes gpui's (ledger:182).
    transform_band: usize,
    /// A slider is being dragged. Tracked on the root like the colour card's,
    /// for the same reason: a 4 px bar is left by the pointer on the first
    /// move and its own listeners then stop firing.
    transform_dragging: bool,
    /// The opacity row's bar is being dragged: its samples go to the opacity, not a band.
    opacity_drag: bool,
    /// The opacity row's bar, recorded at prepaint like the transform bars.
    opacity_bar: Rc<Cell<Bounds<Pixels>>>,
    /// Each slider's box, recorded at prepaint -- [`Self::color_bars`]' own
    /// reason, one per band.
    transform_bars: [Rc<Cell<Bounds<Pixels>>>; TRANSFORM_BANDS.len()],
    /// The placement the hand is on, held back because the worker still owes a
    /// frame -- [`Self::pending_color`]'s own reason.
    pending_transform: Option<TransformParams>,
    /// The frame on screen counted into `HIST_BINS` bins per channel -- the
    /// *graded* frame, because the grade is applied in the decode worker and
    /// what arrives here is already through it. Refilled by every pumped frame,
    /// which is what makes the colour card's graph move as a slider is dragged:
    /// each live write reseeks, and the reseek's frame is the next count.
    histogram: [u32; HIST_BINS],
    /// The open `More …` folds, by panel id, kept in the panel-folds file
    /// ([`load_folds`] / [`save_folds`]) so a fold stays as the user left it.
    folds: BTreeSet<String>,
    /// What the file actions have had to say, oldest first. A *queue* and not a
    /// slot: two imports that fail back to back used to be one message, because
    /// the second overwrote the first before a frame had drawn it -- the failure
    /// a user never learns about is the one that was answered by another
    /// failure. The front holds its own bar above the panel until it is answered
    /// -- any key retires it, so does a click on it -- the bar says how many are
    /// behind it, and answering it brings the next one up.
    notices: std::collections::VecDeque<crate::notices::Toast>,
    /// How tall the transient bars hanging off the bottom of the picture (the
    /// import line, the seek line, the notice) came out at prepaint, so the cue
    /// plate can sit clear of them ([`sub_bottom`]). Measured and not counted:
    /// a notice wraps to as many lines as the window is narrow.
    notice_h: Rc<Cell<Pixels>>,
    /// What the last finished export wrote, so the notice can be the way to it.
    /// Only the [`EXPORT_DONE`] line reads it -- any later notice has replaced
    /// that text -- so a click never opens a file the bar is not naming.
    exported: Option<PathBuf>,
    /// What the compositor was last told this window is called. Setting a title
    /// is a protocol round trip and a repaint is sixty a second, so the title is
    /// pushed only when it is not this any more.
    titled: String,
    displayed: u32,
    /// Frames the picture skipped in the current run, because it was behind the clock
    /// ([`Player::dropped_words`]); restarts at each play.
    dropped: u32,
    /// Whether the transport was playing at the last pump, and when it last stopped, so the
    /// dropped-frames words stay up for a few seconds after a stop.
    playing_last: bool,
    drop_stop: Option<Instant>,
    /// When the picture was last restarted at the clock for falling behind it
    /// ([`should_resync`]). The cool-down's only state.
    resynced: Option<Instant>,
    /// Wall clock of the first displayed frame -- the real-speed measurement.
    started: Option<Instant>,
    focus: FocusHandle,
    /// The three focus-ring surfaces (`ui::stance::Surface`) Tab/Shift-Tab
    /// cycles between once one of them holds focus -- `focus` above stays
    /// the fallback none of these keys ever leave.
    focus_dock: FocusHandle,
    focus_bench: FocusHandle,
    focus_inspector: FocusHandle,
    /// The settings page's KEYS list. Takes the keyboard while the page is
    /// capturing or filtering, so its own key listener sees the strokes first.
    focus_settings: FocusHandle,
    /// Tab stops handed to a dialog's controls as it is built ([`ui::kit::tab_scope`]).
    dlg_pool: Vec<FocusHandle>,
    /// Whether a dialog was up at the last frame, to give the root the keyboard when one opens.
    dlg_was_up: bool,
    /// A [`Player::guard_focus`] check is already waiting for its frame.
    focus_guard_pending: bool,
}

#[cfg(test)]
mod tests;

fn main() {
    // What a killed editor left behind: `.part` proxies from an encode that
    // never got to unlink its own half-written file. Run once, here, before
    // anything in this process could claim one of its own -- every `.part` on
    // disk at this instant belongs to nobody still running.
    engine::proxy::sweep_stale_parts();
    // A keymap file that cannot be read leaves the defaults in force, and takes
    // the notice slot ahead of an open or import refusal: it is about every key
    // the window has, and those refusals are on stderr either way.
    let (keymap, notice) = Keymap::load();
    if let Some(text) = &notice {
        eprintln!("{text}");
    }
    // The palette the last session picked, before the first paint: a window that
    // opened in the default and turned warm a frame later would be the theme
    // announcing itself. Silent on a missing or unreadable file -- the default
    // (`PaletteId::DEFAULT`, darkroom) is a whole answer, and nothing of the
    // user's is lost by it.
    ui::theme::load();
    ui::theme::load_accent();
    // The subtitle style the last session picked, same silence on a missing
    // or unreadable file.
    let sub_style = load_subtitle_style();
    // The auto-proxies default the last session left the switch at, before
    // any project's own line ([`engine::edith::Document::auto_proxy`])
    // overrides it the moment one opens.
    let auto_proxies_on = player::library::load_auto_proxies_pref();
    // The Proxies default the last session left the switch at, same
    // before-any-project-line precedence as the auto-proxies default above.
    let proxies_on = player::library::load_proxies_pref();
    // The autosave period the last session left the Settings row at.
    let autosave_secs = player::library::load_autosave_pref();
    // The picture and sound this computer last exported with, same silence on
    // a missing or unreadable file as the switches above.
    let export_prefs = export_ui::load_export_prefs();
    // Nothing named on the command line is read here. The first file makes the
    // timeline -- a `.edith` restores a whole one, anything else *is* one --
    // and the rest are imports like any other: rows in the library, dragged
    // onto a lane when they are wanted there. All of them go through the queue
    // a drop uses ([`Player::import`]), which is the door with a progress line
    // on it, and their refusals arrive in the notice bar as a drop's do.
    //
    // Queued rather than opened because a 25 GB film cold is twelve seconds of
    // header walk, and it used to be twelve seconds of *no window at all* -- a
    // window that has not opened cannot say what it is waiting for. Now the
    // window is up in the time it takes to make one, naming the file and the
    // read that is running, and the timeline appears when that read lands
    // ([`Player::take_import`]).
    //
    // No argument at all opens the window empty, exactly as before: the library
    // then arrives by drop or by the Import button.
    let (arg, queue) = launch_queue(std::env::args().skip(1).map(PathBuf::from));
    let name: SharedString = arg
        .as_deref()
        .map_or_else(|| NO_FILE.into(), |arg| file_name(arg).into());

    Application::new().with_assets(ui::icons::Assets).run(move |cx: &mut App| {
        // Bundled Archivo / Spline Sans Mono, before the first paint --
        // DESIGN §3's two faces, embedded so no build depends on the host
        // having them installed (see `ui::type_scale`'s doc comment).
        ui::type_scale::register(cx);
        // 720p: the picture's own size is not known yet -- knowing it is the
        // twelve seconds this window exists to be up during -- and a window
        // that resized itself under a hand already dragging it would be worse
        // than one that opened at the size the empty window has always used.
        let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("edith".into()),
                    ..Default::default()
                }),
                // What a desktop groups this window under. The title beside it
                // is pushed from the render, because wayland takes neither of
                // them from the titlebar options above (gpui's wayland window
                // reads only `app_id`, window.rs:1202 / window.rs:939) and the
                // title has to follow the file being opened anyway.
                app_id: Some("edith".to_string()),
                ..Default::default()
            },
            |window, cx| {
                let queue = queue.clone();
                let player = cx.new(|cx| Player {
                    // Nothing to wait for yet: the file named on the command
                    // line is still queued, and the repaint that carries its
                    // poster frame to the screen is asked for when it lands
                    // (`open_media` -> `reset_after_reseek`).
                    seek_since: None,
                    resynced: None,
                    session: None,
                    pending_settings: (None, None, None),
                    pending_template: None,
                    welcome_templates: false,
                    preview_session: None,
                    preview_playing: false,
                    source_marks: HashMap::new(),
                    source_target: None,
                    source_hint: false,
                    source_video: true,
                    source_audio: true,
                    patch_video: Lane::V1,
                    patch_audio: Lane::A1,
                    player_fullscreen: false,
                    loop_on: false,
                    range: None,
                    loop_trim: None,
                    // Full and unmuted, which is what the session it was just
                    // handed is already set to: nothing to push at startup.
                    volume: Volume::default(),
                    volume_bar: Rc::default(),
                    preview_bar: Rc::default(),
                    preview_scrubbing: false,
                    volume_dragging: false,
                    // What a session comes up at, so the first one opened is
                    // pushed the values it already holds.
                    proxies_on,
                    auto_proxies_on,
                    autosave_secs,
                    // Only ever used with a timeline; 30 keeps the empty
                    // timecode reading in frames rather than in NaN.
                    fps: 30.,
                    ruler: Rc::default(),
                    lane_beds: Rc::default(),
                    marquee: None,
                    scroll_track: Rc::default(),
                    scroll_drag: None,
                    lanes_drag: None,
                    // draws: the title bar and the header name it while its
                    // header is still being read.
                    name: name.clone(),
                    image: None,
                    retired_images: Vec::new(),
                    output: None,
                    output_display: None,
                    output_displays: Vec::new(),
                    sub_image: None,
                    held: None,
                    notice_h: Rc::default(),
                    // A second is [`PPS_DEFAULT`] pixels wide until someone
                    // zooms or asks for the fit: a project opens at a scale, not
                    // at whatever its first import happens to be long.
                    scale: Scale::default(),
                    // Nobody has scrolled anything yet, so the follow has the
                    // view: the first frame is drawn where the head is.
                    panned: false,
                    fit_pending: false,
                    fitted_bed: None,
                    lane_h: Vec::new(),
                    viewer: viewer_view::Prefs::default(),
                    viewer_pan: (0., 0.),
                    viewer_drag: None,
                    viewer_handle: None,
                    viewer_btns: Default::default(),
                    lane_resize: None,
                    selected: Selection::new(),
                    gap: None,
                    context_menu: None,
                    menu_open: None,
                    menu_hopped: None,
                    menu_sub: None,
                    menu_sub2: None,
                    ledger_readout: Rc::default(),
                    toast_text: Rc::default(),
                    adjust_hot: false,
                    adjust_aim: None,
                    adjust_sub: Cell::new(None),
                    picker: None,
                    library_menu: None,
                    selected_asset: None,
                    asset_picks: Vec::new(),
                    angle_monitor: false,
                    angle_shots: Default::default(),
                    dock_src_active: ui::dock_stance::load(),
                    dock_filter: String::new(),
                    dock_filter_edit: false,
                    keys_filter: String::new(),
                    waves: HashMap::new(),
                    thumbs: HashMap::new(),
                    proxies: HashMap::new(),
                    render: None,
                    render_bands: Vec::new(),
                    render_bands_stamp: None,
                    render_progress: None,
                    stab_waiting: Vec::new(),
                    track_jobs: Vec::new(),
                    reframe_run: None,
                    reframed: None,
                    mcam_run: None,
                    seq_trail: Vec::new(),
                    seq_edit: None,
                    lane_edit: None,
                    time_edit: None,
                    compare: viewer_view::Compare::Off,
                    before: None,
                    before_at: None,
                    before_busy: false,
                    shown_frame: None,
                    look_open: false,
                    saved_looks: Vec::new(),
                    look_name: None,
                    look_mark: None,
                    collapsed_bins: Default::default(),
                    sections_mem: Vec::new(),
                    sections_anchor: None,
                    selected_bin: None,
                    bin_edit: None,
                    title_sel: None,
                    title_draft: None,
                    title_hex: None,
                    gen_hex: None,
                    gen_scrub: None,
                    deinterlace_open: false,
                    place_open: false,
                    title_in_open: false,
                    title_out_open: false,
                    effects_mode_open: None,
                    eq_shape_open: false,
                    transition_kind_open: false,
                    transition_section_open: false,
                    accent_hex: None,
                    accent_drag: None,
                    accent_vivid_bar: Rc::default(),
                    accent_hue_bar: Rc::default(),
                    title_scrub: None,
                    pan_scrub: None,
                    streams: HashMap::new(),
                    has_video: HashMap::new(),
                    bitrates: HashMap::new(),
                    facts: HashMap::new(),
                    sizes: HashMap::new(),
                    syncs: HashMap::new(),
                    decoders: HashMap::new(),
                    export_seat: None,
                    hw_caps: None,
                    clipboard: Vec::new(),
                    copied_look: None,
                    scrubbing: false,
                    splits: load_stance_splits(bounds.size),
                    split_drag: None,
                    split_press: None,
                    trim: None,
                    pro_drag: None,
                    hover_hint: None,
                    trim_flash: None,
                    drag_src: None,
                    fade_drag: None,
                    gain_edit: None,
                    gain_hover: None,
                    gain_pick: None,
                    grab: 0,
                    snap: true,
                    drag_alt: false,
                    drag_ctrl: false,
                    drag_below: false,
                    subs_on: true,
                    waveforms: true,
                    scrub_audio: true,
                    play_around: None,
                    sub_track: 0,
                    sub_folded: HashSet::new(),
                    sub_lane: None,
                    snap_cue: None,
                    menu_frame: None,
                    ghost: Vec::new(),
                    drag_x: None,
                    drag_lane: None,
                    lane_drop: None,
                    last_scrub: Instant::now(),
                    last_target: 0,
                    export: None,
                    cancelling: false,
                    cancel_armed: false,
                    export_started: None,
                    export_marks: Vec::new(),
                    queue: export_queue::ExportQueue::default(),
                    // Both derived from the file when it lands, by the same
                    // `open_media`/`load_project` a drop goes through: an
                    // export beside the picture, a save beside it too.
                    export_path: PathBuf::new(),
                    export_known: None,
                    project_path: PathBuf::new(),
                    autosave_dirty: false,
                    autosave_edits: 0,
                    autosave_last_run: None,
                    autosave_urgent: false,
                    recovery_sidecar: None,
                    recovery_failure: None,
                    quit_ask: false,
                    save_ask: false,
                    proj_dialog: None,
                    new_project: None,
                    new_project_fresh: false,
                    export_ask: false,
                    export_sheet: export_ui::ExportSheet {
                        size: export_prefs.size,
                        bit_depth: export_prefs.depth,
                        prores: export_prefs.prores,
                        hdr: export_prefs.hdr,
                        hevc_speed: export_prefs.hevc_speed,
                        sound_bits: export_prefs.sound_bits,
                        ..Default::default()
                    },
                    autosave_armed: false,
                    disk_known: None,
                    edited_since_save: false,
                    saved_revision: None,
                    keymap: keymap.clone(),
                    keys_open: false,
                    about_open: false,
                    settings_open: false,
                    settings_tab: ui::settings_stance::SettingsTab::Project,
                    settings_key_edit: None,
                    settings_keys_filter: String::new(),
                    settings_focus: None,
                    keys_scroll: ScrollHandle::new(),
                    export_scroll: ScrollHandle::new(),
                    palette_open: false,
                    palette_query: String::new(),
                    palette_sel: 0,
                    palette_scroll: ScrollHandle::new(),
                    lanes_scroll: ScrollHandle::new(),
                    eq_scroll: ScrollHandle::new(),
                    export_body_scroll: ScrollHandle::new(),
                    settings_rows_scroll: ScrollHandle::new(),
                    export_open: false,
                    transcript_open: false,
                    transcript: player::transcript::Transcript::new(),
                    eq_open: None,
                    card_maximized: false,
                    // Replaced by the clip's own curve the moment the card
                    // opens; nothing reads it before that.
                    eq_params: EqParams::default(),
                    eq_band: 0,
                    eq_dragging: false,
                    eq_graph: Rc::default(),
                    eq_spectrum: true,
                    speed_open: None,
                    speed_bar: Rc::default(),
                    speed_dragging: false,
                    pending_speed: None,
                    speed_ramp_mode: false,
                    ramp_key_speed: None,
                    mix_open: false,
                    markers_open: false,
                    history_open: false,
                    tools: None,
                    tools_runs: 0,
                    effects_open: None,
                    effects_draft: None,
                    effects_drag: None,
                    effects_adding: false,
                    effects_bars: Default::default(),
                    marker_sel: 0,
                    marker_edit: None,
                    mix_field: 0,
                    mix_solo: Vec::new(),
                    mix_panel: Default::default(),
                    mix_drag: None,
                    sub_text: sub_style.size,
                    sub_family: sub_style.family,
                    sub_outline: sub_style.outline,
                    sub_boxed: sub_style.boxed,
                    sub_place: sub_style.place,
                    sub_dragging: false,
                    sub_font_open: false,
                    sub_font_query: String::new(),
                    subtitle_style_open: false,
                    subtitle_style_field: 0,
                    sub_anim_open: false,
                    sub_keys_used: false,
                    subtitle_fonts: Vec::new(),
                    silence_open: None,
                    beats_open: None,
                    beats_every: 0,
                    beats_scan: None,
                    beats_last: None,
                    // The conservative defaults the engine documents: a first
                    // scan that leaves a little too much is one nobody undoes.
                    silence: engine::silence::Settings::default(),
                    silence_factor: Speed::MAX,
                    // The take, not the timeline: the narrower answer is the
                    // one a person can widen on purpose.
                    silence_scope: Scope::Take,
                    silence_field: 0,
                    // The reference named, which is what the card said before
                    // there was a choice about it.
                    silence_dbfs: true,
                    silence_keep_only: false,
                    silence_marks: Vec::new(),
                    silence_levels: HashMap::new(),
                    silence_scan: None,
                    silence_bg: HashMap::new(),
                    color_open: None,
                    color_band: 0,
                    color_dragging: false,
                    viz_hs_dragging: false,
                    viz_wheel: Rc::default(),
                    picture_box: Rc::default(),
                    shot_pending: None,
                    session_gen: 0,
                    // Nothing heard yet: the first pump seeds it off the session
                    // it serves.
                    audio_reason: None,
                    audio_watch_gen: 0,
                    decode_reason_cache: None,
                    decode_watch_gen: 0,
                    viz_slot: VizSlot::Main,
                    viz_hex_edit: None,
                    cue_edit: None,
                    sel_cue: None,
                    cue_num: None,
                    cue_num_scrub: None,
                    still_num: None,
                    cue_drag: None,
                    cue_menu: None,
                    pending_viz: None,
                    color_bars: std::array::from_fn(|_| Rc::default()),
                    color_section: ColorSection::Basic,
                    color_wheel: 0,
                    color_wheel_drag: None,
                    color_pads: std::array::from_fn(|_| Rc::default()),
                    color_masters: std::array::from_fn(|_| Rc::default()),
                    color_curve: 0,
                    scope_view: ScopeView::Histogram,
                    scopes: None,
                    pending_color: None,
                    transform_open: None,
                    transform_band: 0,
                    transform_dragging: false,
                    opacity_drag: false,
                    opacity_bar: Rc::default(),
                    transform_bars: std::array::from_fn(|_| Rc::default()),
                    pending_transform: None,
                    // Empty until the first frame is pumped, which draws as a
                    // flat line rather than as a shape nothing measured.
                    histogram: [0; HIST_BINS],
                    folds: load_folds(),
                    // Nobody has chosen a budget yet, which is not the same
                    // as a budget of nothing: the lever opens on the engine's
                    // own automatic figure for the picture that is open.
                    custom_bps: export_prefs.bps,
                    budget_edit: None,
                    transition_edit: None,
                    // ...and the rate the sound has always been written at.
                    audio_kbps: export_prefs.audio_kbps,
                    // The picture and sound this computer last used, or the
                    // ones an export always was before there was anything to pick.
                    format: export_prefs.format,
                    notices: notice
                        .clone()
                        .map(|text| Toast::new(text.into()))
                        .into_iter()
                        .collect(),
                    exported: None,
                    // The whole of argv, waiting for the first repaint to start
                    // it: the window is up before a byte of it is read.
                    importing: None,
                    imports: queue,
                    opening: arg.clone(),
                    // Nothing pushed yet, and never a real title: the first
                    // render is what names the window.
                    titled: String::new(),
                    displayed: 0,
                    dropped: 0,
                    playing_last: false,
                    drop_stop: None,
                    started: None,
                    focus: cx.focus_handle(),
                    focus_dock: cx.focus_handle(),
                    focus_bench: cx.focus_handle(),
                    focus_inspector: cx.focus_handle(),
                    focus_settings: cx.focus_handle(),
                    dlg_pool: (0..64).map(|_| cx.focus_handle().tab_stop(true)).collect(),
                    dlg_was_up: false,
                    focus_guard_pending: false,
                });
                // Nothing else takes focus, and without it the key listener
                // above is never reached.
                window.focus(&player.read(cx).focus);
                // The compositor's close request -- alt+F4, the titlebar's
                // button, `swaymsg kill`, a session logout -- is the one door
                // out of this window (there is no Quit action and nothing in
                // the tree calls `cx.quit()`). Answering `false` keeps the
                // window up, which is what turns "closing" into a question
                // when the timeline holds work no file has.
                let asking = player.clone();
                window.on_window_should_close(cx, move |window, cx| {
                    asking.update(cx, |player, cx| {
                        if !player.close_needs_answer() {
                            // The one door out with nothing to ask: the view is
                            // kept for the next open, best effort.
                            player.save_view_state();
                            return true;
                        }
                        player.quit_ask = true;
                        // The question owns the keyboard, so the root must hold the focus
                        // whatever had it when the request came in.
                        window.focus(&player.focus);
                        cx.notify();
                        false
                    })
                });
                player.update(cx, |player, cx| player.start_autosave(cx));
                player.update(cx, |player, cx| player.start_follow(cx));
                // `EDITH_OPEN`: screenshot runs reach panels; unset, nothing.
                if let Ok(spec) = std::env::var("EDITH_OPEN") {
                    let wanted = open_hook::parse(&spec);
                    player.update(cx, |player, cx| player.start_open_hook(wanted, cx));
                }
                player
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
