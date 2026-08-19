mod keymap;
mod player;
mod ui;

mod audio_cards;
mod color_ui;
mod eq_ui;
mod export_ui;
mod files;
mod importing;
mod interact;
mod layout;
mod library_meta;
mod menus;
mod notices;
mod oracle;
mod render;
mod subs;
mod timeline_math;
mod transport;
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
pub(crate) use render::escape_leaves_player_fullscreen;
pub(crate) use notices::*;
pub(crate) use oracle::*;
pub(crate) use subs::*;
pub(crate) use timeline_math::*;
pub(crate) use transport::*;
pub(crate) use viewport::*;

use ui::theme::*;
use ui::widgets::*;

use keymap::{ActionId, Keymap};

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use engine::audio::StreamInfo;
use engine::color::ColorParams;
use engine::decode::Backend;
use engine::eq::{Band, BandKind, EqParams};
use engine::export::{AUDIO_KBPS, DEFAULT_AUDIO_KBPS, EncoderSeat, ExportSettings, Format};
use engine::limiter::Limiter;
use engine::project::{Edge, Lane, LaneKind, Source, Speed, SubClip};
use engine::scale::FitPolicy;
use engine::tonemap::Preset;
use engine::{Clip, Codec, ExportHandle, Frame, MediaBitrate, PlaybackSession};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, CursorStyle, Div, DragMoveEvent,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, RenderImage, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    SharedString, Size, Stateful, TextAlign, TitlebarOptions, Window, WindowBounds, WindowOptions,
    canvas, div, img, point, prelude::*, px, relative, rgb, rgba, size,
};

struct Player {
    /// The timeline, once there is one. A run with no file opens without it and
    /// waits: the first media import or project load is what fills it, and
    /// until then every action that needs a timeline says so instead of acting.
    session: Option<PlaybackSession>,
    /// A resolution and/or frame rate picked before any file exists to derive
    /// them from -- what [`Player::cycle_resolution`] and its fps sibling edit
    /// instead of refusing, with nothing open yet. Applied as the explicit
    /// override [`engine::PlaybackSession::set_resolution`]/`set_frame_rate`
    /// already are for a `.edith`'s own saved values ([`Player::install_media`])
    /// the moment the first session exists, and cleared there: consumed once,
    /// the way a project's own saved settings are read once at
    /// [`engine::PlaybackSession::open_project`].
    pending_settings: (Option<(u32, u32)>, Option<f64>, Option<u32>),
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
    /// The picture alone, filling the window -- what `Fullscreen` gives every
    /// consumer video player, and not what gpui's own `toggle_fullscreen`
    /// gives on its own (the OS window, chrome and all). Kept apart from
    /// `window.is_fullscreen()` because the two can fall out of step: a
    /// compositor keybind changes the window's without this flag hearing
    /// about it, so [`Player::act`] reconciles them rather than trusting
    /// either alone.
    player_fullscreen: bool,
    /// Timeline seconds -> frame index, so the clock can be compared to what
    /// the decoder hands over.
    fps: f64,
    name: SharedString,
    image: Option<Arc<RenderImage>>,
    /// The picture of the bitmap cue on screen, and which cue that is: the
    /// track it is a row of and where it starts on the timeline. A PGS display
    /// set is run-length and has to be walked into a canvas-sized buffer to be
    /// drawn ([`engine::subtitle::CueImage::rgba`]), which is a thing to do
    /// when the cue changes -- every few seconds -- and not at every repaint.
    ///
    /// The *lane* is half the key because the four PGS tracks of a remux are
    /// one film's subtitles in four languages: placed one over the other they
    /// start at the same microsecond, and a lane whose eye was shut would leave
    /// the one before it on screen.
    sub_image: Option<((Lane, i64), Arc<RenderImage>)>,
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
    /// Which placements the edit keys act on: the lane each is in and its
    /// index there, every ctrl-click in the order it was made. The *clicked*
    /// half and not the group -- a group is what gets marked on screen, but
    /// Lift has to know which half it was aimed at -- and the last pick is the
    /// [`anchor`](Selection::anchor) every single-thing action uses. Indices
    /// move under every edit, so this is cleared by all of them.
    selected: Selection,
    /// The clip menu a right-click opened, if one is up. Holds an index like
    /// `selected` does, so it is closed by anything that can move indices --
    /// every stroke, and every item of its own.
    context_menu: Option<ContextMenu>,
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
    /// Which category of the library is being looked at. A tab and not a
    /// filter box: the categories are what the media *is*, and every editor
    /// this one is measured against splits its pool the same way.
    library_tab: LibraryTab,
    /// What is known about each source's audio, taken once and kept. Keyed on
    /// the path *and stream* -- two streams of one file are two envelopes -- and
    /// the key is inserted the moment the decode is *started*: presence means
    /// "asked", so a repaint mid-decode cannot ask again.
    waves: HashMap<(PathBuf, usize), Wave>,
    /// What each source's stand-in is up to ([`engine::proxy`]), filled like
    /// `waves`: presence means "asked", so the encode is started once per file
    /// however many times the library is drawn. The switch that decides whether
    /// they are *played* is the session's own -- it is the project's and is
    /// saved with it, where this map is this window's view of the cache.
    proxies: HashMap<PathBuf, Proxy>,
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
    /// How big each still source's picture is, read from its header once and
    /// kept -- what a library row and its card say about a file that has no
    /// streams to describe. Filled like `streams`: presence means "asked", and
    /// `None` is a file with no picture to report (every source that is not an
    /// image, and one whose header would not read).
    sizes: HashMap<PathBuf, Option<(u32, u32)>>,
    /// Every frame of each source a decoder may be started from -- its sync
    /// points ([`engine::demux::sync_points`]), which are the frames a cut may
    /// be placed on for an export to *copy* the film instead of coding all of
    /// it again. Filled like `bitrates` and off the render thread for the same
    /// reason, and more so: the answer is that Matroska cluster walk. An empty
    /// list is a source with no grid to offer (an mp4, a still, a song), which
    /// must stay in the map or every repaint would ask again.
    syncs: HashMap<PathBuf, Vec<u32>>,
    /// Which decoder each source will run on, probed once at import and kept:
    /// the codec (`None` for a still) and the seat the engine picked for it.
    /// What a library row says *before* anything plays; the running answer is
    /// the session's own (`PlaybackSession::decode_backend`), which follows a
    /// fallback this cannot. Filled like `sizes`: presence means "asked", and
    /// `None` is a source with no decoder to name -- a song, or one the probe
    /// refused -- which must stay in the map or every repaint would ask again.
    decoders: HashMap<PathBuf, Option<(Option<Codec>, Backend)>>,
    /// What an export of the picked settings would open -- the picture's seat
    /// or the copy that means it opens none, and the sound's -- and what it was
    /// asked about: the settings, the canvas and the *cuts*, since where they
    /// land is what decides whether the picture is copied at all. The probe
    /// opens a real VA-API encoder (~100 ms) and reads every source's header,
    /// so it runs off the render thread and only while the export card is up.
    /// The inner `None` is "asked, not answered yet".
    export_seat: Option<(
        ExportSettings,
        (u32, u32),
        Vec<Clip>,
        Option<(Option<&'static str>, &'static str)>,
    )>,
    /// What this machine's GPU decodes and encodes, as the plugin answered it:
    /// asked once, off the render thread like `export_seat` and for the same
    /// reason (a VA-API init), and kept for the life of the process because the
    /// answer cannot change while we run. `None` is "not asked yet".
    hw_caps: Option<SharedString>,
    /// The copied clip. Frame ranges only, so it survives the clip it was taken
    /// from being deleted -- and it outlives the selection.
    clipboard: Option<Clip>,
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
    /// A drag that started on a clip's edge, tracked on the root for
    /// `scrubbing`'s reason: a 6 px strip is not where the pointer stays. See
    /// [`Trim`].
    trim: Option<Trim>,
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
    /// Whether the subtitle lanes draw at all -- the mute over the shown lane
    /// ([`Player::sub_lane`]), not a lane of its own. On by default and off by
    /// one stroke
    /// ([`ActionId::ToggleSubtitles`](keymap::ActionId::ToggleSubtitles)) for
    /// anyone watching the picture rather than reading it. The player's, not the
    /// project's: it changes nothing that is saved and nothing that is exported.
    subs_on: bool,
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
    /// The box the same gesture is about to fill, or `None` while the pointer is
    /// over no lane: the shadow every proper editor draws under a drag. Set by
    /// the lane the pointer is actually over -- that is the one question the
    /// line above does not answer -- and drawn only while a drag is live, for
    /// [`Player::snap_cue`]'s reason.
    ghost: Option<Ghost>,
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
    /// Where the save action writes: the project this timeline was loaded from,
    /// or the one derived beside the media it started as. Saving twice
    /// overwrites the same file rather than making a second one.
    project_path: PathBuf,
    /// Which stroke means what, and what every shortcut on screen is called.
    /// The one place either question is answered.
    keymap: Keymap,
    /// How loud the monitoring is, and whether it is muted. Lives here rather
    /// than in the session so it survives closing one file and opening the
    /// next -- it is a setting of the player, not of the timeline, which is
    /// also why it is not written to the project file and cannot reach an
    /// export. [`Player::apply_volume`] is what pushes it at a session.
    volume: Volume,
    /// Where the volume slider was last painted, and whether a hand is on it --
    /// the speed bar's pair, for the speed bar's reason: the pointer moves
    /// arrive at the root, so the bar's own geometry has to be readable there.
    volume_bar: Rc<Cell<Bounds<Pixels>>>,
    volume_dragging: bool,
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
    /// The keybindings overlay is up. While it is, it owns the keyboard and the
    /// pointer: a stroke or a click meant for a row must not also cut the
    /// timeline.
    keys_open: bool,
    /// What has been typed into the card's search box, which is the card's own
    /// input exactly as the export card's digits are (nothing in it takes
    /// focus, so the root's key handler is the field). Emptied every time the
    /// card opens: a search is a look at the list, not a setting.
    keys_search: String,
    /// Where that list is scrolled to. Held here rather than left to the
    /// wheel alone: forty actions are four times what a 360 px window shows,
    /// and the rows past the fold have to be reachable from the keyboard that
    /// is already typing in the search box.
    keys_scroll: ScrollHandle,
    /// Where the lane column and the inspector's rows have been taken to. Read
    /// back at render, not only written by the wheel: the line each of them
    /// carries about what is below the fold is a count of what is *still* below
    /// it, and a scroll that nothing reads is a scroll the affordance cannot
    /// follow.
    lanes_scroll: ScrollHandle,
    inspector_scroll: ScrollHandle,
    /// And where the equalizer card's own body has been taken to. It is the
    /// tallest card in the column -- a graph with a row of numbers and a row of
    /// buttons under it -- and at the 360 px floor its title and its buttons
    /// were off both ends of the column with no way to reach them.
    eq_scroll: ScrollHandle,
    /// The export options card is up: what the export action opens now, so
    /// nothing is written until the card's own button says so. One card at a
    /// time -- opening either closes the other, since both are the whole window
    /// and two stacked scrims say nothing about which one is listening.
    export_open: bool,
    /// How the card lays its rows out, and where the formats this program
    /// cannot write are said. Two shapes of the same card, kept behind `g` and
    /// `r` so the choice between them can be made by looking at both rather
    /// than by argument: sections with headers against one flat list, and a
    /// collapsed "cannot write" footer against a dimmed row each. The defaults
    /// are grouped and collapsed -- the five dead rows used to eat the fold.
    /// Not persisted: this is a look, not a setting.
    export_grouped: bool,
    export_refusals_inline: bool,
    /// Whether the card's Advanced pane -- codec, container, quality,
    /// sound, encoder, subtitles, this machine -- is open under the primary
    /// pane's destination and preset rows. Closed by default: most exports
    /// are one of the bundled presets, and the fifteen-odd rows under them
    /// are what a person who is not one of those goes looking for, kept
    /// behind one row rather than eaten by the fold. Not persisted, like the
    /// two switches above it -- this is a look, not a setting.
    export_advanced_open: bool,
    /// Which quality row the card has picked, and the megabits typed against
    /// the custom one. Kept across closes, so a second export offers what the
    /// first one chose.
    quality: Quality,
    custom_mbps: u32,
    /// The custom row's number *while it is being typed*, or `None` when nobody
    /// is typing one. A field with a caret in it and not a key capture: digits
    /// used to change the bitrate from anywhere in the card, with no caret to
    /// say where they were landing and nothing to look at before the number
    /// took effect. Nothing in this card takes gpui focus (the root keeps the
    /// keyboard), so the field is a modal state on the player exactly as a
    /// waiting rebind row is, and the root's handler is what types into it.
    mbps_edit: Option<NumberEdit>,
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
    /// The mix card is up: every audio track's own volume and the master
    /// limiter, which are project settings and not any clip's -- so unlike the
    /// four clip cards there is no handle to hold, only whether it is open.
    mix_open: bool,
    /// Which of its rows the arrow keys move -- a fader, the limiter's ceiling
    /// or its switch. The card's own focus, since nothing in it takes gpui's
    /// (ledger:182).
    mix_field: usize,
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
    /// The subtitle style card is up.
    subtitle_style_open: bool,
    /// Which of its rows the arrow keys move: 0 is the size stepper, every
    /// row after it a family in [`Self::subtitle_fonts`]. The card's own
    /// focus, since nothing in it takes gpui's (ledger:182).
    subtitle_style_field: usize,
    /// Every family the platform can draw, asked for once when the card
    /// opens and kept for as long as it is up: [`gpui::TextSystem::all_font_names`]
    /// walks the whole font registry, which is not a repaint's business.
    subtitle_fonts: Vec<String>,
    /// The silence card is up on this clip -- the lane it is on and its index
    /// there, exactly as the speed card's handle is.
    silence_open: Option<(Lane, usize)>,
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
    /// Each slider's box, recorded at prepaint: a mouse listener is handed the
    /// window position only, so this is what a press and a drag are read against
    /// ([`frac_along`]). One per band, because the press picks the row it landed
    /// on and the drag then belongs to that row's range.
    color_bars: [Rc<Cell<Bounds<Pixels>>>; COLOR_BANDS.len()],
    /// The grade the hand is on, held back because the worker still owes a
    /// frame ([`Player::flush_drag`]): a live write into a busy worker only
    /// cancels the open the picture is already waiting for, so a bar-wide sweep
    /// would pay for forty of them and show one. What the sliders draw while it
    /// stands, and never lost -- the frame that lands writes it, and so does the
    /// release.
    pending_color: Option<ColorParams>,
    /// The frame on screen counted into `HIST_BINS` bins per channel -- the
    /// *graded* frame, because the grade is applied in the decode worker and
    /// what arrives here is already through it. Refilled by every pumped frame,
    /// which is what makes the colour card's graph move as a slider is dragged:
    /// each live write reseeks, and the reseek's frame is the next count.
    histogram: [[u32; HIST_BINS]; 3],
    /// The action whose row is waiting for a stroke. The next key that is
    /// neither escape nor a lone modifier becomes the whole of what reaches it.
    rebinding: Option<ActionId>,
    /// What the file actions have had to say, oldest first. A *queue* and not a
    /// slot: two imports that fail back to back used to be one message, because
    /// the second overwrote the first before a frame had drawn it -- the failure
    /// a user never learns about is the one that was answered by another
    /// failure. The front holds its own bar above the panel until it is answered
    /// -- any key retires it, so does a click on it -- the bar says how many are
    /// behind it, and answering it brings the next one up.
    notices: std::collections::VecDeque<SharedString>,
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
    dropped: u32,
    /// When the picture was last restarted at the clock for falling behind it
    /// ([`should_resync`]). The cool-down's only state.
    resynced: Option<Instant>,
    /// Wall clock of the first displayed frame -- the real-speed measurement.
    started: Option<Instant>,
    focus: FocusHandle,
}

#[cfg(test)]
mod tests;

fn main() {
    // A keymap file that cannot be read leaves the defaults in force, and takes
    // the notice slot ahead of an open or import refusal: it is about every key
    // the window has, and those refusals are on stderr either way.
    let (keymap, notice) = Keymap::load();
    if let Some(text) = &notice {
        eprintln!("{text}");
    }
    // The palette the last session picked, before the first paint: a window that
    // opened cool and turned warm a frame later would be the theme announcing
    // itself. Silent on a missing or unreadable file -- the default is a whole
    // answer, and nothing of the user's is lost by it.
    ui::theme::load();
    // The subtitle style the last session picked, same silence on a missing
    // or unreadable file.
    let (sub_family, sub_text) = load_subtitle_style();
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

    Application::new().run(move |cx: &mut App| {
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
                    preview_session: None,
                    preview_playing: false,
                    player_fullscreen: false,
                    // Full and unmuted, which is what the session it was just
                    // handed is already set to: nothing to push at startup.
                    volume: Volume::default(),
                    volume_bar: Rc::default(),
                    volume_dragging: false,
                    // What a session comes up at, so the first one opened is
                    // pushed the values it already holds.
                    proxies_on: false,
                    auto_proxies_on: true,
                    // Only ever used with a timeline; 30 keeps the empty
                    // timecode reading in frames rather than in NaN.
                    fps: 30.,
                    ruler: Rc::default(),
                    scroll_track: Rc::default(),
                    scroll_drag: None,
                    lanes_drag: None,
                    // draws: the title bar and the header name it while its
                    // header is still being read.
                    name: name.clone(),
                    image: None,
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
                    selected: Selection::new(),
                    context_menu: None,
                    picker: None,
                    library_menu: None,
                    selected_asset: None,
                    library_tab: LibraryTab::Media,
                    waves: HashMap::new(),
                    proxies: HashMap::new(),
                    streams: HashMap::new(),
                    bitrates: HashMap::new(),
                    sizes: HashMap::new(),
                    syncs: HashMap::new(),
                    decoders: HashMap::new(),
                    export_seat: None,
                    hw_caps: None,
                    clipboard: None,
                    scrubbing: false,
                    splits: Splits::default(),
                    split_drag: None,
                    trim: None,
                    grab: 0,
                    snap: true,
                    subs_on: true,
                    sub_track: 0,
                    sub_folded: HashSet::new(),
                    sub_lane: None,
                    snap_cue: None,
                    ghost: None,
                    lane_drop: None,
                    last_scrub: Instant::now(),
                    last_target: 0,
                    export: None,
                    cancelling: false,
                    cancel_armed: false,
                    export_started: None,
                    export_marks: Vec::new(),
                    // Both derived from the file when it lands, by the same
                    // `open_media`/`load_project` a drop goes through: an
                    // export beside the picture, a save beside it too.
                    export_path: PathBuf::new(),
                    project_path: PathBuf::new(),
                    keymap: keymap.clone(),
                    keys_open: false,
                    keys_search: String::new(),
                    keys_scroll: ScrollHandle::new(),
                    lanes_scroll: ScrollHandle::new(),
                    inspector_scroll: ScrollHandle::new(),
                    eq_scroll: ScrollHandle::new(),
                    export_open: false,
                    export_grouped: true,
                    export_refusals_inline: false,
                    export_advanced_open: false,
                    eq_open: None,
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
                    mix_open: false,
                    mix_field: 0,
                    sub_text,
                    sub_family,
                    subtitle_style_open: false,
                    subtitle_style_field: 0,
                    subtitle_fonts: Vec::new(),
                    silence_open: None,
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
                    silence_marks: Vec::new(),
                    silence_levels: HashMap::new(),
                    silence_scan: None,
                    silence_bg: HashMap::new(),
                    color_open: None,
                    color_band: 0,
                    color_dragging: false,
                    color_bars: std::array::from_fn(|_| Rc::default()),
                    pending_color: None,
                    // Empty until the first frame is pumped, which draws as a
                    // flat line rather than as a shape nothing measured.
                    histogram: [[0; HIST_BINS]; 3],
                    // What an export is until someone says otherwise: the Web
                    // bundle, so `ExportPreset::from_state` opens on a real
                    // preset rather than a `Custom` nobody picked -- `Auto`
                    // paired with `Format::default()`'s MP4 matched no bundle
                    // at all.
                    quality: Quality::Medium,
                    custom_mbps: 0,
                    mbps_edit: None,
                    // ...and the rate the sound has always been written at.
                    audio_kbps: DEFAULT_AUDIO_KBPS,
                    // Picture and sound, which is what an export was before
                    // there was anything to pick.
                    format: Format::default(),
                    rebinding: None,
                    notices: notice.clone().map(SharedString::from).into_iter().collect(),
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
                    started: None,
                    focus: cx.focus_handle(),
                });
                // Nothing else takes focus, and without it the key listener
                // above is never reached.
                window.focus(&player.read(cx).focus);
                player
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
