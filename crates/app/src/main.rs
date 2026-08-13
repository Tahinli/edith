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
pub(crate) use notices::*;
pub(crate) use oracle::*;
pub(crate) use subs::*;
pub(crate) use timeline_math::*;
pub(crate) use transport::*;
pub(crate) use viewport::*;

use ui::inspector::section_head;
use ui::theme::*;
use ui::toolbar::{EXPORT_SLOT_W, SNAP_SLOT_W, VOLUME_SLOT_W, ZOOM_SLOT_W};
use ui::widgets::*;

use keymap::{ActionId, Keymap};

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use engine::audio::StreamInfo;
use engine::color::ColorParams;
use engine::decode::Backend;
use engine::eq::{Band, BandKind, EqParams};
use engine::export::{AUDIO_KBPS, DEFAULT_AUDIO_KBPS, ExportSettings, Format};
use engine::limiter::Limiter;
use engine::project::{Edge, Lane, LaneKind, Source, Speed};
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
    /// The *track* is half the key because the four PGS tracks of a remux are
    /// one film's subtitles in four languages: they start at the same
    /// microsecond, and a picked row that only changed the language would go on
    /// showing the one before it.
    sub_image: Option<((usize, i64), Arc<RenderImage>)>,
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
    /// Which clip the edit keys act on: the lane it is in and its index there.
    /// The *clicked* half, not the group -- a group is what gets marked on
    /// screen, but Lift has to know which half it was aimed at. Indices move
    /// under every edit, so this is cleared by all of them.
    selected: Option<(Lane, usize)>,
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
    /// Whether the cue under the playhead is drawn over the picture. On by
    /// default -- a subtitle imported and then invisible is an import nobody can
    /// tell happened -- and off by one stroke
    /// ([`ActionId::ToggleSubtitles`](keymap::ActionId::ToggleSubtitles)) for
    /// anyone watching the picture rather than reading it. The player's, not the
    /// project's: it changes nothing that is saved and nothing that is exported.
    subs_on: bool,
    /// Which subtitle track is the one on screen: an index into
    /// [`PlaybackSession::subtitles`], since a file may carry several and only
    /// one can be read at a time. Cleared with the timeline like every other
    /// index here -- track 2 of one project is not track 2 of the next.
    sub_track: usize,
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

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A file that has just been opened sits on its first frame with the
        // clock stopped: opening is not playing, whichever way the file
        // arrived. The play binding and the transport button start it.
        if let Some(session) = &mut self.session {
            session.tick();
        }
        self.pump(window);
        // What every hover label asks before it paints: a card or a menu is
        // drawn over whatever the pointer is resting on.
        OVERLAID.store(self.overlaid(), Ordering::Relaxed);
        // A cleared seek is a frame delivered, which is the one readiness signal
        // there is: whatever a slider drag held back is written here.
        if self.seek_since.is_none() {
            self.flush_drag(cx);
        }
        self.poll_export();
        self.poll_import(cx);
        self.poll_silence();
        // Every way a source can arrive -- argv, an import, a project load --
        // has been through a repaint by the time its clips are drawn, so this
        // is the one place that has to notice a new one.
        self.cache_media(cx);
        self.cache_export_seat(cx);
        self.cache_hw_caps(cx);
        // What the compositor calls this window. Pushed only when it changes:
        // it is a protocol round trip and this runs at vsync.
        let title = window_title(&self.name);
        if title != self.titled {
            window.set_window_title(&title);
            self.titled = title;
        }
        // No shadow flag: the session is the only truth about play state, and
        // [`Player::transport`] is the one place it is read.
        let state = self.transport();
        // A paused timeline has nothing to animate; the toggle handlers notify,
        // which is what starts the loop again. A paused seek keeps the loop
        // running by itself until `pump` has the frame it asked for. An export
        // pauses playback and still needs the loop: its progress only reaches
        // the screen on a repaint. A notice does not: it waits to be dismissed
        // rather than for a clock, so keeping the loop alive for it would spin
        // the GPU until someone answered it.
        // An import does too, and for the same reason: its clock and its sweep
        // only reach the screen on a repaint, and a still line is the very
        // thing it exists to disprove.
        if state.is_playing()
            || self.seek_since.is_some()
            || self.export.is_some()
            || self.importing.is_some()
            // A silence scan too: its progress and its two clocks only reach
            // the screen on a repaint, and a still line is the very thing this
            // card was rewritten to disprove.
            || self.silence_scan.is_some()
        {
            window.request_animation_frame();
        }

        // Read per render, never cached: a delete shortens the timeline and the
        // timecode, the ruler and the clamp below all have to follow it -- and
        // so does the room a tail being dragged needs to grow into.
        let duration = self.drawn_duration();
        let position = self.playhead(duration);
        // Re-settled every frame against the duration this one is drawing: an
        // edit that shortens the timeline moves the far end of the view, and a
        // playhead that has run off the bed pulls the view after it -- which is
        // what makes a zoomed-in timeline scroll while it plays.
        // ...but only a playhead that is *going* somewhere pulls it: following
        // is what a moving one does, during playback and through a seek. A view
        // yanked back to a playhead nobody moved is a hand's own scroll undone
        // by the very next frame, which is what made the wheel look dead.
        // ...and a hand that scrolled the view away from the head keeps it
        // ([`Player::panned`]): a follow that centres the head again would undo
        // the notch before it was seen, which is what made the wheel look dead
        // while playing. It is given straight back below, the moment the head
        // is on the bed a person chose to look at -- so the scroll wins now and
        // the follow resumes by itself, with nothing to press.
        self.scale = match (state.is_playing() || self.seek_since.is_some()) && !self.panned {
            true => self.view().following(position),
            false => self.view().settled(),
        };
        if self.panned && self.view().shows(position) {
            self.panned = false;
        }

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                let ctrl = event.keystroke.modifiers.control;
                // `is_held` is the auto-repeat, and a value is the one thing
                // worth running on it: a held arrow on a card moves the slider
                // it picked, and a held volume key runs the volume. Everything
                // else is filtered exactly as it always was -- a repeat that
                // toggled playback, or cut the timeline, many times a second is
                // what this guard is for, and a row waiting for a stroke takes
                // none of it either (it would bind the key, then fire what it
                // just bound). See [`repeats`].
                if event.is_held
                    && !repeats(this.repeat_scope(), key, this.keymap.lookup(key, ctrl))
                {
                    return;
                }
                // Any key answers the message on the bar and brings the next of
                // them up, whatever it was -- and owes
                // the repaint itself: a notice no longer keeps the render loop
                // alive, and the arms below that do notify are not all of them
                // (an unbound key, or the copy chord, changes nothing else).
                if this.dismiss_notice() {
                    cx.notify();
                }
                // A row is waiting for a stroke, and while it is, that stroke is
                // data: it means the binding and nothing else, which is why this
                // answers before the export guard and before the keymap is
                // consulted at all.
                if let Some(action) = this.rebinding {
                    if key == ESCAPE {
                        this.rebinding = None;
                    } else if !is_bare_modifier(key) {
                        this.capture(action, key, ctrl);
                    }
                    cx.notify();
                    return;
                }
                // On linux gpui reports the copy chord as key "c" with the
                // control modifier set (the control code is mapped back), which
                // is why the keymap is keyed on the pair and never on the key
                // alone.
                let action = this.keymap.lookup(key, ctrl);
                // An export is reading the edit list every other action here
                // would change, so cancelling is the only one that means
                // anything until it is over.
                if this.exporting().is_some() {
                    if cancels_export(key, action) {
                        this.cancel_export();
                    }
                    cx.notify();
                    return;
                }
                // The overlay owns the keyboard while it is up -- but it types
                // now: a printable stroke is the search box's, which is why
                // nothing here reaches the keymap. A waiting row is answered
                // above and still wins, so a rebind onto "v" binds the key
                // rather than typing it.
                if this.keys_open {
                    if key == ESCAPE {
                        // Two steps out, the way a search box anywhere gets
                        // out: the filter first -- the whole list back --
                        // and the card only once there is no search to clear.
                        if this.keys_search.is_empty() {
                            this.keys_open = false;
                        } else {
                            this.keys_search.clear();
                            this.scroll_keys(None);
                        }
                    // The rows past the fold, without a wheel: forty actions
                    // are four times what the viewport shows, and the hand
                    // typing in the search box is already on the keyboard.
                    } else if key == "up" {
                        this.scroll_keys(Some(KEYS_ROW_H));
                    } else if key == "down" {
                        this.scroll_keys(Some(-KEYS_ROW_H));
                    } else if key == "backspace" {
                        this.keys_search.pop();
                        this.scroll_keys(None);
                    } else if let Some(c) = typed(key) {
                        this.keys_search.push(c);
                        this.scroll_keys(None);
                    }
                    cx.notify();
                    return;
                }
                // The export card owns it the same way, and for the same
                // reason. Escape closes it -- nothing has been written yet, so
                // there is nothing here to cancel -- and the card's own letters
                // are its input: it has no widget that takes focus (nothing in
                // it does), so this listener is its keyboard, exactly as it is
                // a waiting row's.
                if this.export_open {
                    // A list open over the card is the innermost thing on
                    // screen, so it is what a stroke closes first -- the rule
                    // every menu here follows, said before the card's own keys
                    // so escape does not take the card out from under it.
                    if this.picker.take().is_some() {
                        cx.notify();
                        return;
                    }
                    // A number being typed is the next thing in: while the
                    // field is open every stroke is text, which is what makes
                    // it a field and not a capture -- the card's letters cannot
                    // fire under it, and escape gives up the edit before it
                    // touches the card.
                    if let Some(edit) = &mut this.mbps_edit {
                        if key == ESCAPE {
                            this.mbps_edit = None;
                        } else if key == "enter" {
                            // Committed or refused in its own words; a refused
                            // one stays open on what was typed, so the number
                            // can be fixed rather than typed again.
                            if let Some(mbps) = edit.commit() {
                                this.custom_mbps = mbps;
                                this.quality = Quality::Custom;
                                this.mbps_edit = None;
                            }
                        } else if key == "backspace" {
                            edit.backspace();
                        } else if key == "up" {
                            edit.step(1);
                        } else if key == "down" {
                            edit.step(-1);
                        } else if let Ok(digit) = key.parse::<u32>() {
                            edit.digit(digit);
                        }
                        cx.notify();
                        return;
                    }
                    if key == ESCAPE {
                        this.export_open = false;
                    } else if key == "enter" {
                        // The card's own button, by keyboard: the one thing in
                        // it that writes a file must not be pointer-only either.
                        this.start_export(cx);
                    } else if let Some(format) = format_key(key, this.format) {
                        // The codec rows by their own letter, so the card can be
                        // driven without a mouse -- the same card-local input
                        // the typed bitrate is, and for the same reason: a
                        // choice reachable only by pointer is not reachable by
                        // everyone. Not a keymap binding: it means nothing
                        // outside this card, exactly like the digits.
                        this.set_format(format);
                    } else if key == "c" {
                        this.cycle_container();
                    } else if key == "q" {
                        this.cycle_quality();
                    } else if key == "b" {
                        // The sound's rate, `q`'s pair for the other half of
                        // the file. Not a digit: those are the picture's.
                        this.cycle_audio_kbps();
                    } else if key == "d" {
                        // The save dialog, which was the one row here a
                        // keyboard could not open.
                        this.pick_destination(cx);
                    } else if key == "g" {
                        this.export_grouped = !this.export_grouped;
                    } else if key == "r" {
                        this.export_refusals_inline = !this.export_refusals_inline;
                    } else if key == "n" {
                        // The custom row's field, by keyboard. The digits used
                        // to do this from anywhere in the card, which meant a
                        // stray keystroke changed the bitrate with nothing on
                        // screen to say it had: now a digit outside the field
                        // means nothing at all, and this is the way in.
                        this.edit_mbps();
                    }
                    cx.notify();
                    return;
                }
                // And the equalizer card, the same way again. Its own strokes
                // are the card's input, exactly as the export card's digits
                // are: a band reachable only by dragging is a band a keyboard
                // cannot move at all, and every one of them is listed in the
                // keys menu (keymap.rs `FIXED`) rather than being a secret.
                if this.eq_open.is_some() {
                    // Shift makes the two horizontal keys Q instead of
                    // frequency: both are the *width* of the same hump, so they
                    // sit on the same axis rather than on two keys nobody would
                    // guess. Wider is a lower Q, which is why left widens.
                    let shift = event.keystroke.modifiers.shift;
                    if key == ESCAPE {
                        // Nothing to undo: every change is already at the clip,
                        // and undo is undo's own key.
                        this.eq_open = None;
                        this.eq_dragging = false;
                    } else if key == "up" {
                        this.nudge_band(|b| b.gain_db += EQ_STEP, cx);
                    } else if key == "down" {
                        this.nudge_band(|b| b.gain_db -= EQ_STEP, cx);
                    } else if key == "left" && shift {
                        this.nudge_band(|b| b.q /= EQ_Q_STEP, cx);
                    } else if key == "right" && shift {
                        this.nudge_band(|b| b.q *= EQ_Q_STEP, cx);
                    } else if key == "left" {
                        this.nudge_band(|b| b.freq_hz /= EQ_FREQ_STEP, cx);
                    } else if key == "right" {
                        this.nudge_band(|b| b.freq_hz *= EQ_FREQ_STEP, cx);
                    } else if key == "r" {
                        for band in &mut this.eq_params.bands {
                            band.gain_db = 0.;
                        }
                        this.commit_eq(cx);
                    } else if key == "f" {
                        // This one band back to flat, which is the undo of one
                        // hand movement -- `r` is the undo of the whole card.
                        this.nudge_band(|b| b.gain_db = 0., cx);
                    } else if key == "a" {
                        this.add_band(cx);
                    } else if key == "x" {
                        this.remove_band(cx);
                    } else if key == "s" {
                        // The analyser off and on. Nothing is committed: it is
                        // what the card *shows*, so it survives no further than
                        // this window.
                        this.eq_spectrum = !this.eq_spectrum;
                    } else if let Ok(digit) = key.parse::<usize>() {
                        // As the keys are laid out: 1-9 then 0 for the tenth,
                        // which is the cap ([`EQ_BANDS_MAX`]). A digit past the
                        // last band picks nothing rather than panics.
                        let band = match digit {
                            0 => EQ_BANDS_MAX - 1,
                            n => n - 1,
                        };
                        if band < this.eq_params.bands.len() {
                            this.eq_band = band;
                        }
                    }
                    cx.notify();
                    return;
                }
                // The colour card owns the keyboard the same way the export
                // card does, and its keys mean nothing outside it: the arrows
                // pick a slider and move it, and `r` takes the grade off. Not
                // keymap bindings for exactly that reason -- see `FIXED`, where
                // the keys menu still lists them.
                if this.color_open.is_some() {
                    match color_key(key) {
                        Some(ColorKey::Close) => {
                            this.color_open = None;
                            this.color_dragging = false;
                        }
                        Some(ColorKey::Band(step)) => {
                            this.color_band = (this.color_band + step) % COLOR_BANDS.len();
                        }
                        Some(ColorKey::Nudge(steps)) => this.nudge_color(steps, cx),
                        Some(ColorKey::Reset) => {
                            this.set_color(ColorParams::default(), cx);
                        }
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                // The speed card, the same way again: its arrows move the rate
                // and `r` puts it back to real time, and neither means anything
                // outside the card -- so neither is a binding (see `FIXED`,
                // where the keys menu still lists them).
                if this.speed_open.is_some() {
                    match color_key(key) {
                        Some(ColorKey::Close) => {
                            this.speed_open = None;
                            this.speed_dragging = false;
                        }
                        // The card has one value, so the pair that picks a
                        // slider on the colour card moves this one by a whole
                        // preset's worth instead of a step.
                        Some(ColorKey::Band(step)) => {
                            this.nudge_speed(if step == 1 { -2 } else { 2 }, cx)
                        }
                        Some(ColorKey::Nudge(steps)) => this.nudge_speed(steps as i32, cx),
                        Some(ColorKey::Reset) => this.set_speed(Speed::NORMAL, cx),
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                // The silence card, the same way again: the arrows pick one of
                // its rows and move it, and its two apply keys are the two
                // things it can do to the timeline. Card-local, every one of
                // them -- and listed in the keys menu (keymap.rs `FIXED`),
                // because a key that cuts forty places at once is not a secret.
                if this.silence_open.is_some() {
                    if key == ESCAPE {
                        // Nothing to undo: a preview is not an edit.
                        this.close_silence();
                    } else if key == "down" {
                        this.silence_field = (this.silence_field + 1) % SILENCE_ROWS;
                    } else if key == "up" {
                        this.silence_field = (this.silence_field + SILENCE_ROWS - 1) % SILENCE_ROWS;
                    } else if key == "right" {
                        this.nudge_silence(1);
                    } else if key == "left" {
                        this.nudge_silence(-1);
                    } else if key == "enter" {
                        this.cut_silences(cx);
                    } else if key == "f" {
                        this.speed_silences(cx);
                    }
                    cx.notify();
                    return;
                }
                // The mix card, the same way again: ↑↓ pick a row -- a track's
                // fader, the limiter's ceiling or its switch -- and ←→ move it,
                // held or pressed. Card-local like the four above it.
                if this.mix_open {
                    let rows = this.mix_lanes().len() + MIX_MASTER_ROWS;
                    if key == ESCAPE {
                        this.mix_open = false;
                    } else if key == "down" {
                        this.mix_field = (this.mix_field + 1) % rows;
                    } else if key == "up" {
                        this.mix_field = (this.mix_field + rows - 1) % rows;
                    } else if key == "right" {
                        this.nudge_mix(1, cx);
                    } else if key == "left" {
                        this.nudge_mix(-1, cx);
                    }
                    cx.notify();
                    return;
                }
                // A clip menu names an index, and every edit below moves
                // indices -- so a stroke closes it before it acts. Escape means
                // that and nothing else, which is the `esc` the keys menu
                // already lists (keymap.rs `FIXED`).
                // Both menus, taken rather than short-circuited: the library's
                // one names a row the edits below can remove, so it closes on a
                // stroke exactly as the clip menu does.
                // A choice list goes the same way and for the same reason: it
                // names a clip index too, and escape is the way out of it.
                // An open list is the innermost thing on screen, so it takes
                // the keys before anything under it does: ↑↓ walk it, enter
                // takes the row, and escape falls through to the close below --
                // the same three strokes every list in this editor answers.
                if let Some(mut picker) = this.picker {
                    let rows = this.choices(picker.of);
                    if !rows.is_empty() && matches!(key, "up" | "down" | "enter") {
                        match key {
                            "down" => picker.sel = (picker.sel + 1) % rows.len(),
                            "up" => picker.sel = (picker.sel + rows.len() - 1) % rows.len(),
                            _ => {
                                let (choice, ..) = rows[picker.sel.min(rows.len() - 1)];
                                this.choose(choice, cx);
                                cx.notify();
                                return;
                            }
                        }
                        this.picker = Some(picker);
                        cx.notify();
                        return;
                    }
                }
                let clip_menu = this.context_menu.take().is_some();
                let row_menu = this.library_menu.take().is_some();
                let list = this.picker.take().is_some();
                if clip_menu || row_menu || list {
                    cx.notify();
                    if key == ESCAPE {
                        return;
                    }
                }
                if let Some(action) = action {
                    this.act(action, cx);
                }
            }))
            // The whole window is the drop target: gpui turns an external file
            // drop into an `ExternalPaths` drag (window.rs:3626) delivered as a
            // mouse-up to every hovered hitbox, and the root's is the only one
            // that covers the picture as well as the panel.
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                // The overlay owns the pointer as well as the keyboard, and a
                // drop is a click the scrim cannot swallow: gpui delivers it to
                // the root's hitbox, which is under the scrim but is not a
                // sibling it can stop. The export card is over the timeline for
                // the same reason: importing under it would change the very
                // edit list the card is about to write out.
                if this.modal() {
                    return;
                }
                for path in paths.paths() {
                    // One queue for all of them, in arrival order: the fork --
                    // a project replaces the timeline, media joins the library
                    // -- is made when each one's worker starts ([`arrival`]),
                    // and neither is read on this thread.
                    this.import(path, cx);
                }
            }))
            // A drop event carries no path of its own -- gpui only tells the
            // target that something landed -- so the line that promises where
            // it will land is fed by the drag's own moves, which do carry the
            // pointer (gpui div.rs:282). On the root, because a drag crosses
            // the window: it starts on a clip or on a library row and ends over
            // a lane, and only an ancestor of both hears all of it.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                // The clip the payload named, wherever an edit mid-drag has
                // since put it ([`Player::dragged`]): the line has to promise a
                // landing for the take actually in the hand.
                let drag = *event.drag(cx);
                if let Some(idx) = this.dragged(&drag) {
                    this.preview_drop(drag.lane, idx, event.event.position.x, cx);
                }
                // The shadow belongs to a *lane*, and which lane the pointer is
                // over is the one thing this element cannot see. Cleared here
                // and drawn again by the lane the pointer is actually inside
                // (`lane_row`), which gpui runs straight after this one: the
                // capture phase goes parent first, so a pointer over no lane at
                // all -- up in the library, say -- promises nothing.
                this.set_ghost(None, cx);
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                    this.preview_place(event.event.position.x, cx);
                    this.set_ghost(None, cx);
                }),
            )
            // Scrubbing is tracked on the root because the pointer leaves the
            // 6 px ruler on the first drag and its own listeners then stop
            // firing; the root's hitbox is the whole window.
            .on_mouse_move(cx.listener(Self::drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::drag_release))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_CANVAS()))
            .text_color(rgb(FG_PRIMARY()))
            .text_size(px(12.))
            // Four regions, the arrangement every consumer editor shares:
            // library left, picture centre, inspector right, and the timeline
            // full width along the bottom with its edit toolbar directly above
            // it. Nothing here moves when the state changes -- the regions are
            // fixed and the panels keep their room whether or not anything is
            // open in them.
            .child(self.topbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.library(
                        library_w(f32::from(window.viewport_size().width)),
                        f32::from(window.viewport_size().height),
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_hidden()
                                    // The bed the cue plate is placed against:
                                    // it hangs off the bottom of the picture
                                    // region, which is the one box that is the
                                    // picture and nothing else.
                                    .relative()
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .bg(rgb(BG_CANVAS()))
                                    .children(
                                        self.image
                                            .clone()
                                            .map(|i| {
                                                img(i)
                                                    .size_full()
                                                    .object_fit(gpui::ObjectFit::Contain)
                                                    .into_any_element()
                                            })
                                            // With no file open the letterbox
                                            // is the whole region, and a black
                                            // rectangle says only that
                                            // something is broken -- so it says
                                            // what it wants instead. The window
                                            // is already the drop target.
                                            .or_else(|| {
                                                self.session
                                                    .is_none()
                                                    .then(|| empty_hint().into_any_element())
                                            }),
                                    )
                                    // After the picture, so the plate is drawn
                                    // over it rather than under.
                                    .children(self.subtitle_overlay(position, window))
                                    // The three transient lines hang off the
                                    // bottom of the picture rather than taking
                                    // a row of the column: a notice that
                                    // arrives must not push the transport, the
                                    // toolbar and the timeline down by its own
                                    // height -- which is a control moving with
                                    // state, on every control below it at once.
                                    .child(
                                        div()
                                            .absolute()
                                            .bottom_0()
                                            .left_0()
                                            .right_0()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .children(self.import_bar(cx))
                                            .children(self.seek_bar())
                                            .children(self.notice_bar(cx)),
                                    ),
                            )
                            .child(self.transport_bar(
                                position,
                                state,
                                f32::from(window.viewport_size().width),
                                cx,
                            )),
                    )
                    // The settings cards live in here rather than over the
                    // timeline: adjusting a clip must not hide the clip.
                    .child(self.inspector(window.viewport_size(), cx)),
            )
            .child(self.toolbar(cx))
            .child(self.timeline(
                position,
                duration,
                state,
                f32::from(window.viewport_size().height),
                cx,
            ))
            // Over the region they were opened on, and under the modal cards:
            // they are only ever up while none of those is (`modal`).
            .children(self.context_card(window.viewport_size(), cx))
            .children(self.library_card(window.viewport_size(), cx))
            // The two that are genuinely modal -- the whole registry, and the
            // card that writes a file -- are the only sheets left over the
            // window.
            .children(self.keys_overlay(cx))
            .children(self.export_card(window.viewport_size(), cx))
            // Last, so it floats over whatever opened it -- an inspector row or
            // a clip menu -- rather than under it.
            .children(self.picker_card(window.viewport_size(), cx))
    }
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
                    // Full and unmuted, which is what the session it was just
                    // handed is already set to: nothing to push at startup.
                    volume: Volume::default(),
                    volume_bar: Rc::default(),
                    volume_dragging: false,
                    // Only ever used with a timeline; 30 keeps the empty
                    // timecode reading in frames rather than in NaN.
                    fps: 30.,
                    // The file being opened, from the first frame the window
                    // draws: the title bar and the header name it while its
                    // header is still being read.
                    name: name.clone(),
                    image: None,
                    sub_image: None,
                    held: None,
                    ruler: Rc::default(),
                    // A second is [`PPS_DEFAULT`] pixels wide until someone
                    // zooms or asks for the fit: a project opens at a scale, not
                    // at whatever its first import happens to be long.
                    scale: Scale::default(),
                    // Nobody has scrolled anything yet, so the follow has the
                    // view: the first frame is drawn where the head is.
                    panned: false,
                    selected: None,
                    context_menu: None,
                    picker: None,
                    library_menu: None,
                    selected_asset: None,
                    library_tab: LibraryTab::Media,
                    waves: HashMap::new(),
                    streams: HashMap::new(),
                    bitrates: HashMap::new(),
                    sizes: HashMap::new(),
                    syncs: HashMap::new(),
                    decoders: HashMap::new(),
                    export_seat: None,
                    hw_caps: None,
                    clipboard: None,
                    scrubbing: false,
                    trim: None,
                    grab: 0,
                    snap: true,
                    subs_on: true,
                    sub_track: 0,
                    snap_cue: None,
                    ghost: None,
                    lane_drop: None,
                    last_scrub: Instant::now(),
                    last_target: 0,
                    export: None,
                    cancelling: false,
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
                    color_open: None,
                    color_band: 0,
                    color_dragging: false,
                    color_bars: std::array::from_fn(|_| Rc::default()),
                    pending_color: None,
                    // Empty until the first frame is pumped, which draws as a
                    // flat line rather than as a shape nothing measured.
                    histogram: [[0; HIST_BINS]; 3],
                    // What an export is until someone says otherwise: the
                    // bitrate the picture asks for.
                    quality: Quality::Auto,
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
