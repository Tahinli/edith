mod keymap;
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

impl Player {
    /// Catches the display up to the clock: everything already due is taken off
    /// the channel and only the last of them is shown, which *is* the
    /// drop-when-behind policy. A frame that is not due yet waits in `held`, and
    /// while the clock is paused *nothing* is due -- a repaint re-presents the
    /// frame already on screen, whatever asked for the repaint.
    fn pump(&mut self, window: &mut Window) {
        // Where the transport was before this drain, so the crossing into
        // `Ended` can be recognised as the one transition it is.
        let was = self.transport();
        // No timeline, nothing to catch up to: the window is showing its empty
        // state and there is no decoder to drain.
        let Some(session) = &mut self.session else {
            return;
        };
        let target = session.now() * self.fps;
        let mut newest: Option<Frame> = None;
        // A frame the screen is owed: a seek's landing, and the one readiness
        // signal there is ([`Player::reset_after_reseek`]).
        let owed = self.seek_since.is_some();
        // Paused, the clock is frozen and *nothing new is due*. Whatever the
        // decoder is still handing over is the backlog it was behind by when
        // the pause landed -- frames at a position the transport has already
        // left -- and taking one per repaint is what walked the picture on
        // after the sound had stopped, at exactly the rate the pointer was
        // moved over the timeline. Gated here, at the one place a frame ever
        // reaches the screen, rather than in the handlers that repaint: a
        // hover, a notice, a resize and a vsync are all the same event to this.
        // An owed frame is still taken, playing or not -- a scrub is paused by
        // definition, and its landing is the whole point of it.
        while session.is_playing() || owed {
            let frame = match self.held.take() {
                Some(frame) => frame,
                // Nothing waiting means either a clip boundary being rebuilt or
                // the real end of the timeline, and only the engine can tell
                // them apart -- `frame.index` is already a timeline index.
                None => match session.try_frame() {
                    Some(frame) => frame,
                    None => break,
                },
            };
            if f64::from(frame.index) <= target {
                self.dropped += u32::from(newest.is_some());
                newest = Some(frame);
            } else {
                self.held = Some(frame);
                break;
            }
        }

        // How far behind the master clock the picture just handed over is, in
        // seconds. Measured off a frame that really arrived and nothing else: a
        // clip boundary being reopened delivers nothing at all for hundreds of
        // milliseconds, and restarting *that* would only cancel the open it is
        // waiting on.
        let late = newest
            .as_ref()
            .map_or(0., |f| (target - f64::from(f.index)) / self.fps);

        if let Some(frame) = newest {
            self.displayed += 1;
            self.seek_since = None;
            self.started.get_or_insert_with(|| {
                eprintln!("first frame displayed (index {})", frame.index);
                Instant::now()
            });
            let buf = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                .expect("frame buffer sized width*height*4");
            // Counted here rather than under `color_open`, because the card
            // opens on a frame that was pumped before it: gating this on the
            // card would leave its graph flat until something reseeked. A
            // thousandth of the pixels, against a conversion that just touched
            // all of them.
            self.histogram = histogram(buf.as_raw());
            let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
            if let Some(old) = self.image.replace(next) {
                // Every RenderImage gets a fresh id and its own atlas tile:
                // without this the sprite atlas grows for the whole video.
                let _ = window.drop_image(old);
            }
        }

        // Audio is the master clock and a decoder that cannot keep up with it
        // never gets back on its own: it hands over every frame in order,
        // whether or not its moment has passed, so what it is behind by only
        // grows -- a minute in, the picture is seconds behind what is being
        // heard, and that is the whole of "the video can't catch the audio".
        // Past `LATE_RESYNC` the backlog is abandoned and the picture restarted
        // at the clock, which touches neither the sound nor the clock
        // (`PlaybackSession::resync_picture`), so nothing the ear is following
        // moves. Never on a frame a seek owed: that one is late by however long
        // its own reopen took, and answering it with another reopen is a loop.
        //
        // corner-cut: on a machine that cannot decode the file in real time at
        // all this settles into one restart per `RESYNC_GAP` -- in sync, and
        // stuttering, which is the honest picture of what that machine can do.
        // The upgrade path is dropping late frames *inside* the worker (skip
        // the convert and the send for anything already past due), which needs
        // the deadline shared with it.
        if !owed && session.is_playing() && should_resync(late, self.resynced) {
            eprintln!("picture {late:.3}s behind the clock: restarting it there");
            session.resync_picture();
            self.held = None;
            self.resynced = Some(Instant::now());
        }

        if self.transport() == Transport::Ended {
            // A seek whose worker never produced a frame (vanished file) would
            // otherwise repaint at vsync forever. Held clear for as long as the
            // state does, not just on the crossing: nothing else is coming.
            self.seek_since = None;
            if was != Transport::Ended {
                // Ended is a *stopped* transport, so the clock stops with it,
                // on the out point the timecode and the playhead have been
                // showing all along. Nothing else ever stopped it: past the
                // last frame wall time takes over and `now()` walks off the end
                // of the timeline for as long as the window is left open -- and
                // the playhead is what a cut, a paste, an insert and the
                // analyser all act at, so every one of them was aiming into
                // empty space (measured: a 5 s timeline recognised its end at
                // clock 17.5 s under a slow renderer). End of stream is left
                // set, so this is still `Ended` and the next press restarts.
                if let Some(session) = &mut self.session {
                    session.halt_at_end();
                }
                let elapsed = self.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
                eprintln!(
                    "eof after {elapsed:.3}s wall: {} frames displayed, {} dropped, clock {:.3}s",
                    self.displayed,
                    self.dropped,
                    self.session.as_ref().map_or(0., PlaybackSession::now)
                );
            }
        }
    }

    /// Where the transport is, asked of the session rather than remembered:
    /// end of stream is the engine's own flag (any seek clears it, which is why
    /// an edit past the end revives the picture) and so is the clock. A held
    /// frame is one still owed to the screen, so the end is not the end yet.
    fn transport(&self) -> Transport {
        let Some(session) = &self.session else {
            return Transport::Stopped;
        };
        transport(
            session.is_playing(),
            session.is_eos() && self.held.is_none(),
        )
    }

    /// A frame owed to the screen after a reseek, and the buffered one dropped:
    /// what stops the picture from staying frozen on the old last frame. The
    /// end-of-stream flag itself is the engine's and its own seek clears it --
    /// edits reseek inside the engine and still owe this.
    fn reset_after_reseek(&mut self) {
        self.held = None;
        // Restarted on every reseek, not only on the first: what it measures is
        // the open now standing, which is what a person is waiting on.
        self.seek_since = Some(Instant::now());
        // A seek is a person saying where to look, so it takes the view back
        // from an earlier scroll: the frame asked for is the one to be shown.
        self.panned = false;
        // An edit moves the indices a drag in flight is holding -- a stroke
        // during one is exactly that -- and an edge committed against a moved
        // index would trim a clip nobody grabbed. Dropping it is the whole fix:
        // nothing has been written yet.
        self.trim = None;
        // ...and the shadow a drag is drawn under promises a landing on a lane
        // this edit has just reshaped. The next move of the drag draws it
        // again; until then it says nothing.
        self.ghost = None;
    }

    /// What an action does, wherever it was asked for -- a stroke, or the clip
    /// menu item that names the same action. One table, so the two can never
    /// come to mean different things.
    fn act(&mut self, action: ActionId, cx: &mut Context<Self>) {
        // Two doors, one oracle. This used to be the asymmetry the whole
        // toolbar was built on: the buttons dimmed themselves off
        // [`enable`] while the keyboard walked straight past it, so with no
        // file open `s` toggled the snap and `v` added a track while the very
        // same controls sat dim and *dead* to the pointer. Whatever refuses the
        // button refuses the key, in the oracle's own words -- and a refusal
        // that is silent from the keyboard is a bug the same size.
        match self.enable(action, None) {
            Enable::Yes => {}
            // A state refusal is spoken: the thing exists and cannot happen
            // *now*, which is exactly what a silent key press fails to say.
            Enable::No(why) => {
                self.notify_user(format!("{} — {why}", action.label()).into());
                cx.notify();
                return;
            }
            // A class refusal is not: the action does not exist for what is in
            // front of the user, and `esc` with nothing exporting must not
            // answer with a line about exports.
            Enable::Hidden(_) => return,
        }
        match action {
            ActionId::Play => self.toggle_or_restart(cx),
            ActionId::StepBack => self.step(-1, cx),
            ActionId::StepForward => self.step(1, cx),
            // A second is however many frames this timeline runs at.
            ActionId::JumpBack => self.step(-(self.fps.round() as i64), cx),
            ActionId::JumpForward => self.step(self.fps.round() as i64, cx),
            // The ends, as a step nothing can be far enough from.
            ActionId::GoStart => self.step(i64::MIN, cx),
            ActionId::GoEnd => self.step(i64::MAX, cx),
            // Not a step at all: the grid these land on is the *source's*, and
            // where the next one is depends on the file rather than on the rate.
            ActionId::PrevSyncPoint => self.jump_sync(false, cx),
            ActionId::NextSyncPoint => self.jump_sync(true, cx),
            ActionId::Export => self.open_export(cx),
            ActionId::Save => self.save_project(cx),
            ActionId::Copy => self.copy_selected(),
            ActionId::Paste => self.paste(cx),
            ActionId::Cut => self.cut(cx),
            ActionId::Regroup => self.regroup(cx),
            ActionId::Detach => self.detach(cx),
            ActionId::Group => self.group(cx),
            ActionId::Select => self.select_under_playhead(cx),
            ActionId::SelectNext => self.select_step(true, cx),
            ActionId::SelectPrev => self.select_step(false, cx),
            ActionId::Delete => self.delete_selected(cx),
            ActionId::Lift => self.lift_selected(cx),
            ActionId::Color => self.open_color(cx),
            ActionId::Fit => self.cycle_fit(cx),
            ActionId::Resolution => self.cycle_resolution(cx),
            // The playhead is what a key zoom is aimed at: it is the one place
            // on the timeline the user is certainly looking at, and keeping it
            // still is what every editor does.
            ActionId::ZoomIn => self.zoom(ZOOM_STEP, None, cx),
            ActionId::ZoomOut => self.zoom(1. / ZOOM_STEP, None, cx),
            ActionId::ZoomFit => self.zoom_fit(cx),
            ActionId::Undo => self.undo(cx),
            ActionId::AddVideoLane => self.add_lane(LaneKind::Video, cx),
            ActionId::AddAudioLane => self.add_lane(LaneKind::Audio, cx),
            // The last track of that kind: the one the add key put there, so the
            // two strokes undo each other press for press. Any other track goes
            // through the × in its own header.
            ActionId::RemoveVideoLane => self.remove_last_lane(LaneKind::Video, cx),
            ActionId::RemoveAudioLane => self.remove_last_lane(LaneKind::Audio, cx),
            // The same chooser the + S button opens, and the picked row -- the
            // one the panel draws highlighted -- for the removal: the × on any
            // other row is that row's own door, and both doors are one call.
            ActionId::AddSubtitleTrack => self.pick_and_add_subtitles(cx),
            ActionId::RemoveSubtitleTrack => self.remove_subtitle_track(self.sub_track, cx),
            ActionId::ToggleMute => self.set_volume(|volume| volume.muted = !volume.muted, cx),
            ActionId::VolumeUp => self.set_volume(|volume| volume.step(true), cx),
            ActionId::VolumeDown => self.set_volume(|volume| volume.step(false), cx),
            ActionId::Equalizer => self.open_eq(cx),
            ActionId::Speed => self.open_speed(cx),
            ActionId::Silence => self.open_silence(cx),
            ActionId::Mix => self.open_mix(None, cx),
            ActionId::ToggleSnap => self.toggle_snap(cx),
            ActionId::ToggleSubtitles => self.toggle_subtitles(cx),
            // The keyboard's door to the same list the toolbar button opens.
            // At the window's corner, since a stroke names no place -- and
            // [`menu_at`] keeps it on screen from there.
            ActionId::Theme => self.open_picker(Pick::Theme, Point::default(), cx),
            // Nothing to cancel while nothing is exporting; the export guard in
            // the key handler is what answers this one while there is.
            ActionId::CancelExport => {}
            ActionId::ShowActions => self.show_actions(cx),
        }
    }

    /// Says something to the user. The one door: every message in this editor
    /// comes through here, so "queued rather than overwritten" is a property of
    /// the field and not of seventy call sites remembering to be polite.
    ///
    /// A repeat of what is already at the back is dropped -- holding a key that
    /// refuses would otherwise fill the queue with one sentence, and the count
    /// on the bar would be a count of how long the key was held.
    fn notify_user(&mut self, message: SharedString) {
        push_notice(&mut self.notices, message);
    }

    /// Answers the message on the bar and brings up the next one. Whether there
    /// was one to answer, because a key that dismissed a notice owes a repaint
    /// and a key that dismissed nothing does not.
    fn dismiss_notice(&mut self) -> bool {
        self.notices.pop_front().is_some()
    }

    /// The magnet off and on again, in words: a snap that stops working
    /// silently reads as a bug, and one that starts working silently reads as
    /// one too. The line goes with it -- nothing is being promised any more.
    fn toggle_snap(&mut self, cx: &mut Context<Self>) {
        self.snap = !self.snap;
        self.snap_cue = None;
        self.ghost = None;
        self.notify_user(match self.snap {
            true => "SNAP ON — drags land on clip edges, the playhead and the start".into(),
            false => "SNAP OFF — drags land exactly where the hand leaves them".into(),
        });
        cx.notify();
    }

    /// The actions card, from its key, from the panel button, or from its own
    /// row: open, with an empty search box -- a card that opens showing the
    /// last search would hide most of the list for a reason nobody remembers.
    fn show_actions(&mut self, cx: &mut Context<Self>) {
        self.keys_open = true;
        self.keys_search.clear();
        self.scroll_keys(None);
        self.rebinding = None;
        // One card at a time, the rule the other cards follow.
        self.export_open = false;
        cx.notify();
    }

    /// Moves the actions card's row list by `by` pixels, or puts it back at the
    /// top (`None`). Back to the top after every keystroke that changes the
    /// search: a filtered list is shorter than the offset a scrolled one left
    /// behind, and a card showing the empty space past its last row reads as a
    /// search that found nothing.
    ///
    /// Clamped to what there is to scroll, so the list cannot be pushed off
    /// either end -- `max_offset` is what the last paint measured, which is the
    /// only place that number exists.
    fn scroll_keys(&self, by: Option<f32>) {
        let at = match by {
            Some(by) => (f32::from(self.keys_scroll.offset().y) + by)
                .clamp(-f32::from(self.keys_scroll.max_offset().height), 0.),
            None => 0.,
        };
        self.keys_scroll.set_offset(point(px(0.), px(at)));
    }

    /// The cues over the picture, off and on. Says which it is now *and* what is
    /// on screen while they are on: a toggle whose answer is invisible when the
    /// playhead happens to sit between two cues would read as broken.
    fn toggle_subtitles(&mut self, cx: &mut Context<Self>) {
        self.subs_on = !self.subs_on;
        // Named with its film here too: a notice saying "SUBTITLES ON — eng"
        // over a timeline holding two films' eng tracks names neither.
        let label = self
            .session
            .as_ref()
            .and_then(|session| sub_pick_name(session.subtitles(), self.sub_track))
            .unwrap_or_else(|| "nothing imported".to_string());
        self.notify_user(
            match self.subs_on {
                true => format!("SUBTITLES ON — {label}"),
                false => format!("SUBTITLES OFF — {label} is still on the timeline"),
            }
            .into(),
        );
        cx.notify();
    }

    /// The subtitle track the overlay and the strip are showing: the one a
    /// library row picked, or the first there is. `None` with no timeline and
    /// for an index left over from one that is gone.
    fn subtitle_track(&self) -> Option<&engine::subtitle::SubtitleTrack> {
        self.session.as_ref()?.subtitles().get(self.sub_track)
    }

    /// Whether the editor can be asked for `action` right now, and why not when
    /// it cannot. `on` is the clip the question is about -- the one a clip menu
    /// was opened on -- and `None` asks about the marked clip instead, which is
    /// what a menu that hangs over no clip in particular means by "this one".
    ///
    /// The player's half of [`enable`]: it reads the state, the table decides.
    fn enable(&self, action: ActionId, on: Option<(Lane, usize)>) -> Enable {
        enable(action, self.ctx(on))
    }

    /// The state every one of those questions is asked against, read off the
    /// player once: [`menu_items`] filters a whole menu with it, so the rows a
    /// menu draws and the answers it dims them by come from the same reading.
    fn ctx(&self, on: Option<(Lane, usize)>) -> Ctx {
        let Some(session) = &self.session else {
            return Ctx::default();
        };
        let clip = on
            .or(self.selected)
            .and_then(|(lane, idx)| session.lane_clips(lane).get(idx).map(|clip| (*clip, lane)));
        Ctx {
            clip,
            image: clip.is_some_and(|(clip, _)| {
                session
                    .sources()
                    .get(clip.source)
                    .is_some_and(|s| engine::is_image(&s.path))
            }),
            playhead: frame_at(session.now(), self.fps),
            timeline: true,
            clipboard: self.clipboard.is_some(),
            subtitles: !session.subtitles().is_empty(),
            playable: !nothing_to_play(Some(session)),
            exporting: self.exporting().is_some(),
        }
    }

    /// The same reading for a library row: whether this file can join this
    /// timeline -- the very answer the list greys the row by, so the menu over a
    /// row and the row under it cannot disagree -- and how many clips play it.
    /// [`Player::ctx`] for the other panel.
    fn row_ctx(&self, path: &Path, stream: usize) -> RowCtx {
        let placed = self.session.as_ref().map_or(0, |session| {
            let of_row = session
                .sources()
                .iter()
                .position(|s| s.path == path && s.audio_stream == stream);
            of_row.map_or(0, |idx| {
                session
                    .lanes()
                    .into_iter()
                    .flat_map(|lane| session.lane_clips(lane))
                    .filter(|c| c.source == idx)
                    .count()
            })
        });
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        RowCtx {
            timeline: self.session.is_some(),
            exporting: self.exporting().is_some(),
            usable: library_rows(
                sources,
                &self.streams,
                &self.decoders,
                self.timeline_audio(),
                |path| {
                    self.session
                        .as_ref()
                        .map_or(0, |session| session.file_frames(path))
                },
            )
            .iter()
            .any(|row| row.path == path && row.stream == stream && row.unusable.is_none()),
            placed,
        }
    }

    /// The one place a clip becomes *the* selected one: a click, a right-click
    /// that opens the menu, and every selection key go through here, so what a
    /// keyboard marks and what a pointer marks are the same state marked the
    /// same way (group and all -- see [`marked`]).
    fn select(&mut self, target: (Lane, usize), cx: &mut Context<Self>) {
        self.selected = Some(target);
        cx.notify();
    }

    /// Every clip the playhead is over, one per lane, in the order the lanes are
    /// drawn -- video first, which is the order [`PlaybackSession::lanes`] comes
    /// in. What the select key walks.
    fn under_playhead(&self) -> Vec<(Lane, usize)> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let now = session.now();
        session
            .lanes()
            .into_iter()
            .filter_map(|lane| Some((lane, session.lane_clip_at(lane, now)?)))
            .collect()
    }

    /// Selects the clip under the playhead, and on a repeat press the next
    /// lane's -- so one key reaches every clip the playhead is over, which is
    /// what makes selection (and everything that acts on a selection: delete,
    /// lift, the equalizer, the grade) reachable with no pointer at all.
    fn select_under_playhead(&mut self, cx: &mut Context<Self>) {
        let under = self.under_playhead();
        let Some(&first) = under.first() else {
            self.notify_user("NOTHING UNDER THE PLAYHEAD — move it onto a clip first".into());
            cx.notify();
            return;
        };
        // Where the current selection sits in that walk decides what "again"
        // means; a selection off the playhead starts the walk over.
        let next = self
            .selected
            .and_then(|sel| under.iter().position(|&clip| clip == sel))
            .map_or(first, |at| under[(at + 1) % under.len()]);
        self.select(next, cx);
    }

    /// Walks the selection along its own lane, wrapping at either end. Nothing
    /// selected means nothing to walk from, so it selects under the playhead
    /// exactly as the select key does: either key can start as well as continue.
    fn select_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let clips = self
            .selected
            .zip(self.session.as_ref())
            .map_or(0, |((lane, _), session)| session.lane_clips(lane).len());
        match (self.selected, clips) {
            // An empty lane is a selection nothing can be stepped from -- as is
            // no selection at all, and the playhead answers both.
            (Some((lane, idx)), len) if len > 0 => {
                let next = if forward {
                    (idx + 1) % len
                } else {
                    (idx + len - 1) % len
                };
                self.select((lane, next), cx);
            }
            _ => self.select_under_playhead(cx),
        }
    }

    /// Cycles the fit policy of the clip the picture is coming from -- the
    /// clicked one when it is a video clip, else the composite's own, exactly as
    /// the colour card picks its target. A whole card for one four-valued
    /// setting would be a card to close; a stroke that cycles it and says what
    /// it landed on is the same setting with nothing to dismiss.
    ///
    /// Only means anything when the clip is not the project's size -- a clip
    /// that already fills the canvas looks the same under all four -- so the
    /// notice says the size it is placing, not just the word.
    fn cycle_fit(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            self.notify_user("no timeline to fit — open a file first".into());
            cx.notify();
            return;
        };
        let target = self
            .selected
            .filter(|(lane, _)| lane.kind == LaneKind::Video)
            .or_else(|| session.video_clip_at(session.now()));
        let Some((lane, idx)) = target else {
            self.notify_user("no clip under the playhead to fit".into());
            cx.notify();
            return;
        };
        let next = next_fit(session.fit_of(lane, idx));
        self.apply_fit(lane, idx, next, cx);
    }

    /// One clip's fit policy set, whichever asked: the stroke that steps to the
    /// next one and the list that names one outright come through here, so they
    /// cannot differ in what they do or in what they say they did.
    fn apply_fit(&mut self, lane: Lane, idx: usize, fit: FitPolicy, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_fit(lane, idx, fit)
        {
            let (w, h) = session.resolution();
            self.notify_user(format!("FIT POLICY: {} on {w}x{h}", fit_label(fit)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The scale against the bed it is drawn on and the timeline it is drawn
    /// from: every clamp, zoom and scroll is worked out through this, and the
    /// bed is measured off the ruler's probe rather than remembered, so a
    /// resized window is a resized view on the very next answer.
    fn view(&self) -> View {
        View {
            scale: self.scale,
            bed: f32::from(self.ruler.get().size.width),
            duration: self.drawn_duration(),
            fps: self.fps,
        }
    }

    /// Magnifies the timeline about a point that stays put: `anchor` is how many
    /// pixels along the bed to hold still (a ctrl+wheel holds the pointer), and
    /// with none it is the playhead -- so the frame being worked on is still the
    /// frame on screen after the zoom. Clamped at both ends by [`View`]: out
    /// stops at the whole timeline on the bed, in at a handful of frames.
    fn zoom(&mut self, factor: f32, anchor: Option<f32>, cx: &mut Context<Self>) {
        let view = self.view();
        let at = self.playhead(view.duration);
        let anchor = anchor.unwrap_or_else(|| self.scale.px_at(at).clamp(0., view.bed));
        self.scale = view.zoomed(factor, anchor);
        // The view a hand chose. A zoom about the playhead leaves it on the
        // bed, so this is given back on the very next frame and only a zoom
        // that took the head off screen -- ctrl+wheel away from it -- holds.
        self.panned = true;
        cx.notify();
    }

    /// All the way back out: the whole timeline across the bed, and the one
    /// thing that reads the timeline's own length to decide how wide a second
    /// is drawn.
    fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.scale = self.view().fit();
        cx.notify();
    }

    /// Slides the view along the timeline by `notches` of the wheel, later in
    /// time for a positive one and [`SCROLL_NOTCH_SHARE`] of the bed each. The
    /// scale is untouched: this is the timeline's scrollbar, and the only thing
    /// on the panel that moves what is on screen without magnifying it.
    fn scroll_view(&mut self, notches: f32, cx: &mut Context<Self>) {
        let view = self.view();
        // Nothing painted yet: there is no bed to measure a notch against, and
        // a start moved against a zero width would be a jump to the head.
        if view.bed <= 0. {
            return;
        }
        self.scale = view.scrolled(notches * view.bed * SCROLL_NOTCH_SHARE);
        // The one gesture whose whole purpose is to look away from the
        // playhead: while playing it wins over the follow, which is what every
        // editor does with a scroll during playback.
        self.panned = true;
        cx.notify();
    }

    /// One notch of the wheel anywhere over the timeline -- the ruler or a
    /// lane's bed alike, since a hand aims at the clip it is working on and not
    /// at the strip above it. Ctrl zooms about the pointer, bare scrolls the
    /// view along: the mapping Premiere, Movavi and CapCut share, and the one
    /// the user named.
    ///
    /// The anchor is measured off the ruler's probe wherever the pointer is,
    /// because that probe *is* the bed's x-to-time mapping ([`HEADER_W`]) and
    /// every lane is drawn through the same one.
    fn timeline_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let d = wheel_delta(event);
        if d == 0. {
            return;
        }
        let factor = match d > 0. {
            true => ZOOM_STEP,
            false => 1. / ZOOM_STEP,
        };
        match event.modifiers.control {
            true => {
                let anchor = px_along(event.position.x, self.ruler.get());
                self.zoom(factor, Some(anchor), cx);
            }
            // Up is back towards the head of the timeline, the way a wheel up
            // is back towards the top of a page.
            false => self.scroll_view(-d.signum(), cx),
        }
    }

    /// Cycles the *project's* resolution through [`RESOLUTIONS`], starting from
    /// the media's own -- the one size that must stay reachable, since a project
    /// moved off it has no other way back (the resolution is not an undo step).
    /// Every clip is recomposed onto it, so this is what makes "the project
    /// resolution and the media's are different things" a thing a user can see.
    fn cycle_resolution(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            self.notify_user("no timeline to resize — open a file first".into());
            cx.notify();
            return;
        };
        let (width, height) = next_resolution(session.resolution(), session.native_resolution());
        self.apply_resolution(width, height, cx);
    }

    /// The project resized, whichever asked: the stroke that steps to the next
    /// size and the list that names one outright come through here.
    fn apply_resolution(&mut self, width: u32, height: u32, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_resolution(width, height)
        {
            self.notify_user(format!("PROJECT: {width}x{height}").into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The project cut at another rate: the list names one and this is where it
    /// happens, the way [`apply_resolution`](Self::apply_resolution) is for a
    /// size. The whole timeline is conformed to it by the engine
    /// ([`PlaybackSession::set_frame_rate`]) -- same seconds, same footage --
    /// and the rate the app itself counts frames in follows, since every
    /// timecode, ruler mark and step key here is measured in it.
    fn apply_frame_rate(&mut self, fps: f64, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_frame_rate(fps)
        {
            self.fps = session.meta().frame_rate;
            self.notify_user(format!("PROJECT: {} fps", fps_label(fps)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The project's HDR media shown another way: the list names a rendition and
    /// this is where it happens, the way [`apply_resolution`](Self::apply_resolution)
    /// is for a size. The engine remaps the frame under the playhead at once
    /// ([`PlaybackSession::set_tone`]), so the picture on screen is the picked
    /// one before the notice has faded -- and an SDR project is unmoved, which
    /// is what the notice says rather than pretending something happened.
    fn apply_tone(&mut self, preset: Preset, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_tone(preset)
        {
            self.notify_user(format!("HDR: {} — affects HDR media", tone_label(preset)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Opens a choice list on a setting, where it was asked for. One floating
    /// thing at a time: the click that opens it is the click that closes
    /// whatever menu it was opened from.
    fn open_picker(&mut self, of: Pick, at: Point<Pixels>, cx: &mut Context<Self>) {
        // On the row that is in force, so the first ↑ or ↓ steps off the
        // current value rather than off the top of the list.
        let sel = self
            .choices(of)
            .iter()
            .position(|(.., picked)| *picked)
            .unwrap_or(0);
        self.context_menu = None;
        self.library_menu = None;
        self.picker = Some(Picker { of, at, sel });
        cx.notify();
    }

    /// A row of the open list was picked. Closes the list first -- the rule
    /// every menu item here follows -- then does exactly what the stroke for
    /// that setting does, through the same door.
    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.picker = None;
        match choice {
            Choice::Size(w, h) => self.apply_resolution(w, h, cx),
            Choice::Fps(fps) => self.apply_frame_rate(fps, cx),
            Choice::Fit(lane, idx, fit) => self.apply_fit(lane, idx, fit, cx),
            Choice::Tone(preset) => self.apply_tone(preset, cx),
            // In force for the next paint -- every token is read through
            // [`ui::theme::palette`], so one store repaints the whole window --
            // and kept for the next launch. A file that could not be written is
            // said out loud: the difference between "picked" and "picked for
            // good" is the user's to know.
            Choice::Theme(id) => {
                ui::theme::set(id);
                if let Err(e) = ui::theme::save(id) {
                    let path = ui::theme::config_path();
                    self.notify_user(
                        format!("THEME COULD NOT BE KEPT — {} — {e}", path.display()).into(),
                    );
                }
                cx.notify();
            }
            // The same field the row's key steps, set outright: a list picks a
            // value, it does not step to one.
            Choice::AudioRate(kbps) => {
                self.audio_kbps = kbps;
                cx.notify();
            }
        }
    }

    /// Every value the open list offers, in the order it lists them. Empty
    /// without a timeline, which is the state where nothing here has a value to
    /// offer -- and where the surfaces that open the list are dimmed anyway.
    fn choices(&self, of: Pick) -> Vec<ChoiceRow> {
        // The palette is not the project's, so it is offered before the
        // timeline is asked about: an empty window is painted too, and its
        // Theme button is live there like the snap beside it.
        if of == Pick::Theme {
            return ui::theme::PaletteId::ALL
                .into_iter()
                .map(|id| {
                    (
                        Choice::Theme(id),
                        id.label().into(),
                        id.detail().into(),
                        id == ui::theme::active(),
                    )
                })
                .collect();
        }
        let Some(session) = &self.session else {
            return Vec::new();
        };
        match of {
            Pick::Resolution => {
                resolution_choices(session.resolution(), session.native_resolution())
            }
            Pick::Fps => fps_choices(session.meta().frame_rate, session.native_frame_rate()),
            Pick::Fit(lane, idx) => {
                fit_choices(lane, idx, session.fit_of(lane, idx), session.resolution())
            }
            Pick::AudioRate => audio_rate_choices(self.audio_kbps),
            Pick::Tone => tone_choices(session.tone()),
            // Answered above, with or without a timeline.
            Pick::Theme => Vec::new(),
        }
    }

    /// Opens the colour card on the clip a grade would go on: the clip that was
    /// clicked when it is a video one, and otherwise the clip the picture is
    /// coming from -- the one the engine's own compositing rule picks, which is
    /// what a person means by "this shot". The fallback stands even now that a
    /// selection key exists: a grade asked for with nothing selected still means
    /// the shot on screen.
    fn open_color(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to grade — open a file first".into());
            cx.notify();
            return;
        };
        let target = self
            .selected
            .filter(|(lane, _)| lane.kind == LaneKind::Video)
            .or_else(|| session.video_clip_at(session.now()));
        match target {
            Some(clip) => {
                self.color_open = Some(clip);
                self.color_band = 0;
                self.color_dragging = false;
                // A sample the last card held back belongs to the clip it was
                // dragged on, and this may be another one.
                self.pending_color = None;
                // One card at a time, the rule both the others already follow.
                self.keys_open = false;
                self.export_open = false;
                self.context_menu = None;
            }
            None => self.notify_user("no clip under the playhead to grade".into()),
        }
        cx.notify();
    }

    /// What the card's clip is graded by right now -- the identity for one
    /// nobody has graded, which is what the sliders start at. A sample a drag is
    /// still holding wins over the clip's own: it is what the hand has asked
    /// for, so it is what the sliders show and what the next sample builds on.
    fn color_params(&self) -> ColorParams {
        if let Some(params) = self.pending_color {
            return params;
        }
        self.color_open
            .zip(self.session.as_ref())
            .and_then(|((lane, idx), session)| session.color_of(lane, idx).copied())
            .unwrap_or_default()
    }

    /// Puts `params` on the card's clip, or takes the grade off when they are
    /// the identity -- a slider walked back to the middle leaves the clip
    /// ungraded rather than carrying a do-nothing entry, which is what keeps an
    /// untouched project byte-identical. The engine reseeks on the edit, so the
    /// frame on screen repaints through the new grade; this only owes the flags
    /// that reseek clears.
    fn set_color(&mut self, params: ColorParams, cx: &mut Context<Self>) {
        self.write_color(params, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through
    /// (`PlaybackSession::set_color_live`). Either way the engine reseeks, so
    /// the picture -- and the histogram counted off it -- is regraded at once.
    fn write_color(&mut self, params: ColorParams, live: bool, cx: &mut Context<Self>) {
        // Any write supersedes a held sample, whichever way it arrived -- a key,
        // a reset, or the flush that took this one out of the stash.
        self.pending_color = None;
        let Some((lane, idx)) = self.color_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let grade = Some(params).filter(|p| !p.is_identity());
        let took = match live {
            true => session.set_color_live(lane, idx, grade),
            false => session.set_color(lane, idx, grade),
        };
        if took {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Moves the picked slider by `steps` of [`COLOR_STEP`], clamped to that
    /// band's range. One edit, so one undo step per press.
    fn nudge_color(&mut self, steps: f32, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        let (_, low, high) = COLOR_BANDS[self.color_band];
        let value = band_mut(&mut params, self.color_band);
        *value = (*value + steps * COLOR_STEP).clamp(low, high);
        self.set_color(params, cx);
    }

    /// Where the pointer sits along a slider, as that band's value: the left end
    /// of the bar is the bottom of its range and the right end the top. Called
    /// on every pointer sample, so the grade -- and the picture, and the
    /// histogram over it -- moves under the hand.
    ///
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live. That is why it writes even when
    /// the value did not change -- without that snapshot the rest of the drag
    /// would be unundoable.
    ///
    /// Values land on the [`COLOR_STEP`] grid the keys use, which also bounds
    /// one drag to forty-odd entries in the project's colour table.
    ///
    /// Samples crossed while the worker still owes a frame are held rather than
    /// written ([`stash_or_write`]): a reopen costs half a second on a big film,
    /// so a bar-wide sweep that wrote every step would queue forty opens, cancel
    /// thirty-nine of them and freeze the window for the sum. What is written is
    /// one grade per frame the worker actually delivers.
    fn drag_color(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let (_, low, high) = COLOR_BANDS[self.color_band];
        let along = frac_along(x, self.color_bars[self.color_band].get());
        let value = color_snap(low + along * (high - low)).clamp(low, high);
        let mut params = self.color_params();
        let at = band_mut(&mut params, self.color_band);
        if *at == value && !first {
            return;
        }
        *at = value;
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_color, params, first, busy) {
            Some(params) => self.write_color(params, !first, cx),
            // The sliders draw off the held sample, so the handle goes on
            // following the hand while the picture catches up.
            None => cx.notify(),
        }
    }

    /// Opens the speed card on the clip whose rate is to change: the selected
    /// one, or -- with nothing selected -- the clip the picture is coming from,
    /// which is what a person means by "this shot". Either half of a take will
    /// do: a rate applies to the whole group, so opening it on the sound and
    /// opening it on the picture are the same card.
    fn open_speed(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to re-time — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .or_else(|| session.video_clip_at(session.now()))
        {
            Some(clip) => {
                self.speed_open = Some(clip);
                self.speed_dragging = false;
                // The colour card's rule: a held sample is the last clip's.
                self.pending_speed = None;
                // One card at a time, the rule the other four follow.
                self.keys_open = false;
                self.export_open = false;
                self.eq_open = None;
                self.color_open = None;
                self.close_silence();
                self.context_menu = None;
            }
            None => self.notify_user("no clip under the playhead to re-time".into()),
        }
        cx.notify();
    }

    /// What the card's clip plays at right now -- real time for one nobody has
    /// touched, which is where the bar starts.
    fn card_speed(&self) -> Speed {
        if let Some(speed) = self.pending_speed {
            return speed;
        }
        self.speed_open
            .zip(self.session.as_ref())
            .map_or(Speed::NORMAL, |((lane, idx), session)| {
                session.speed_of(lane, idx)
            })
    }

    /// Writes a rate at the card's clip and its whole group -- one undo step for
    /// the lot ([`engine::PlaybackSession::set_speed`]). The engine reseeks, so
    /// the picture runs at the new rate and the sound is resampled from the next
    /// chunk on; a refusal (a slower clip would run into its neighbour) comes
    /// back in the engine's own words and *names* the clip in the way, because
    /// "it did not fit" is not something a person can go and fix.
    fn set_speed(&mut self, speed: Speed, cx: &mut Context<Self>) {
        self.write_speed(speed, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through -- so a drag from 1.00x to
    /// 2.00x is one undo press and lands back where the hand picked it up, and
    /// the whole linked group comes back with it.
    fn write_speed(&mut self, speed: Speed, live: bool, cx: &mut Context<Self>) {
        // The colour card's rule: a write supersedes whatever a drag was holding.
        self.pending_speed = None;
        let Some((lane, idx)) = self.speed_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        if speed != session.speed_of(lane, idx) {
            let wrote = match live {
                true => session.set_speed_live(lane, idx, speed),
                false => session.set_speed(lane, idx, speed),
            };
            match wrote {
                Ok(()) => self.reset_after_reseek(),
                Err(e) => self.notify_user(e.to_string().into()),
            }
        }
        cx.notify();
    }

    /// One [`SPEED_STEP`] per keystroke, clamped to what a [`Speed`] can hold.
    fn nudge_speed(&mut self, steps: i32, cx: &mut Context<Self>) {
        let at = i32::from(self.card_speed().permille()) + steps * SPEED_STEP;
        self.set_speed(speed_at(at), cx);
    }

    /// Where the pointer sits along the bar, as a rate: the left end is
    /// [`Speed::MIN`] and the right end [`Speed::MAX`], on the same
    /// [`SPEED_STEP`] grid the keys move on -- so a drag can land on exactly
    /// 1.00x and the same drag twice is one entry, not forty.
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live -- the colour card's rule, for the
    /// colour card's reason.
    fn drag_speed(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let along = frac_along(x, self.speed_bar.get());
        let lo = f32::from(Speed::MIN.permille());
        let hi = f32::from(Speed::MAX.permille());
        let raw = lo + along * (hi - lo);
        // Snapped to the grid, then to real time itself when it is within half a
        // step of it: 1.00x is the one value a hand must be able to hit, and
        // nothing about the bar's geometry guarantees a pixel lands on it.
        let stepped = (raw / SPEED_STEP as f32).round() as i32 * SPEED_STEP;
        // Held back while the worker is busy, the colour card's way and for a
        // sharper reason: a live rate also restarts the sound, so a sweep that
        // wrote every step would restart it forty times.
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_speed, speed_at(stepped), first, busy) {
            Some(speed) => self.write_speed(speed, !first, cx),
            None => cx.notify(),
        }
    }

    /// Writes what a slider drag held back, now that the worker has delivered.
    /// The gate is the frame that landed and never a timer: a 100 ms tick
    /// ([`SCRUB_GAP`]) says nothing about a reopen that costs half a second, and
    /// a drag gated on one would still queue opens nobody sees.
    ///
    /// Called again by the release, where readiness is beside the point: the
    /// value the hand let go on is owed whatever the worker is doing, and a
    /// gesture may not end on a sample that was dropped.
    fn flush_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(params) = self.pending_color.take() {
            self.write_color(params, true, cx);
        }
        if let Some(speed) = self.pending_speed.take() {
            self.write_speed(speed, true, cx);
        }
    }

    /// Opens the silence card on the clip to be scanned: the selected one, or
    /// -- with nothing selected -- the clip the picture is coming from, which is
    /// the rule the speed card follows and what a person means by "this shot".
    /// Either half of a take will do: both halves of an A/V take name the same
    /// file and play the same source frames, which is the whole of what a scan
    /// is of ([`ScanKey`]).
    ///
    /// The card is up on the next frame whatever the file is: a still is
    /// refused by name here, where the answer costs a look at the path, and
    /// everything the decoder has to open the file to know -- a track that is
    /// not there, a read that fails -- is refused the same way when the scan
    /// lands, because a fifty-second decode is not a thing to open a card
    /// behind ([`Player::start_silence_scan`]).
    fn open_silence(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to scan — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .or_else(|| session.video_clip_at(session.now()))
            .map(|clip| audio_half(session, clip))
        {
            Some((lane, idx)) => {
                let found = self.session.as_ref().and_then(|session| {
                    let clip = *session.lane_clips(lane).get(idx)?;
                    Some((session.sources().get(clip.source)?.clone(), clip))
                });
                // A still is asked *before* the decoder is: handing a png to the
                // mp4 demuxer answers "a box with a larger size than it", which
                // is a true sentence about a container and nothing a person can
                // act on. A picture has no sound for the same reason a silent
                // video has none, so it is refused in the same words.
                let Some((source, clip)) = found else {
                    cx.notify();
                    return;
                };
                if engine::is_image(&source.path) {
                    self.notify_user(unscannable(lane, idx, &source.path).into());
                    cx.notify();
                    return;
                }
                self.silence_open = Some((lane, idx));
                self.silence_field = 0;
                // One card at a time, the rule the other four follow.
                self.keys_open = false;
                self.export_open = false;
                self.eq_open = None;
                self.color_open = None;
                self.speed_open = None;
                self.context_menu = None;
                // The clip's own range, not the file's: the scan reads what this
                // clip plays and nothing else, so a take cut in half costs half
                // the decode and finds only what is still on the timeline.
                let key = (
                    source.path.clone(),
                    source.audio_stream,
                    clip.in_frame,
                    clip.out_frame,
                );
                match scan_plan(
                    self.silence_levels.contains_key(&key),
                    self.silence_scan.as_ref().map(|scan| &scan.key),
                    &key,
                ) {
                    ScanPlan::Marks => self.scan_silences(),
                    ScanPlan::Start => self.start_silence_scan(key, cx),
                    ScanPlan::Wait => {}
                }
            }
            None => self.notify_user("no clip under the playhead to scan".into()),
        }
        cx.notify();
    }

    /// Opens the mix card. `lane` is the row it lands on -- the track whose
    /// header was clicked -- and `None` starts at the top, which is what the
    /// stroke means.
    ///
    /// Nothing here is a clip's, so nothing is refused for want of a selection:
    /// a timeline with no audio track at all still has a limiter to set, and a
    /// fader on an empty track is the level the next take lands at.
    fn open_mix(&mut self, lane: Option<Lane>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mix_open = true;
        self.mix_field = lane
            .and_then(|lane| self.mix_lanes().iter().position(|&l| l == lane))
            .unwrap_or(0);
        // One card at a time, the rule the other five follow.
        self.keys_open = false;
        self.export_open = false;
        self.eq_open = None;
        self.color_open = None;
        self.speed_open = None;
        self.close_silence();
        self.context_menu = None;
        cx.notify();
    }

    /// The audio tracks the card shows a fader for, top to bottom: *every* one
    /// of them, empty ones included -- what the timeline lays out, not what the
    /// mixer happens to open (`Project::audio_lanes` leaves an empty track out,
    /// and a fader that disappeared when a track was cleared would be a setting
    /// nobody could reach).
    fn mix_lanes(&self) -> Vec<Lane> {
        self.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Audio)
                .collect()
        })
    }

    /// Moves the row the card has picked: a fader by [`MIX_DB_STEP`], the
    /// ceiling by the same, and the switch either way (a ring of two, like the
    /// silence card's unit row).
    ///
    /// Every one of them goes through the session, which hands it straight to
    /// the running mixer: what the ear hears while the arrow is held is the mix
    /// that is being set, and nothing is rebuilt to make that true -- no reseek,
    /// so no `reset_after_reseek` and no blink in the picture behind the card
    /// ([`engine::PlaybackSession::set_lane_gain_db`]).
    fn nudge_mix(&mut self, steps: i32, cx: &mut Context<Self>) {
        let lanes = self.mix_lanes();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match lanes.get(self.mix_field) {
            Some(&lane) => {
                let at = session.lane_gain_db(lane) + steps as f32 * MIX_DB_STEP;
                session.set_lane_gain_db(lane, at);
            }
            None => {
                let limiter = session.limiter();
                let at = match self.mix_field - lanes.len() {
                    0 => Limiter {
                        on: limiter.on,
                        ..limiter
                    }
                    .with_ceiling(limiter.ceiling_db + steps as f32 * MIX_DB_STEP),
                    _ => Limiter {
                        on: !limiter.on,
                        ..limiter
                    },
                };
                session.set_limiter(at);
            }
        }
        cx.notify();
    }

    /// Closes it and drops the preview with it: marks left on the lane after
    /// the card is gone would name frames the next edit has already moved.
    fn close_silence(&mut self) {
        self.silence_open = None;
        self.silence_marks.clear();
        self.cancel_silence_scan();
    }

    /// Tells the worker nobody is waiting any more. It gives up at its next
    /// chunk and the levels it had are dropped: half a track is not an answer,
    /// and the flag stays set on the [`Arc`] the landing closure holds, which is
    /// how that closure knows to keep its hands off the card.
    fn cancel_silence_scan(&mut self) {
        if let Some(scan) = self.silence_scan.take() {
            scan.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Hands the decode to a worker and returns at once -- the card is drawn by
    /// the very next frame, saying it is scanning. Fifty-one seconds on a 25 GB
    /// film is what this used to cost on the render thread, with the card
    /// marked open and nothing on screen.
    ///
    /// Whatever was scanning is cancelled first: one card, one scan, and the
    /// clip that has just been asked about is the one worth the disk.
    ///
    /// Only the clip's own `[in, out)` is read -- source frames over the
    /// project's rate, the same seconds [`engine::Project`] hands the decoder
    /// for playback -- so half a take is half a wait.
    fn start_silence_scan(&mut self, key: ScanKey, cx: &mut Context<Self>) {
        self.cancel_silence_scan();
        self.silence_marks.clear();
        let progress = Arc::new(engine::silence::Progress::default());
        let range = source_secs(&key, self.fps);
        let scan = cx.background_executor().spawn({
            let (key, progress) = (key.clone(), Arc::clone(&progress));
            async move { engine::silence::levels_with_progress(&key.0, key.1, range, &progress) }
        });
        let now = Instant::now();
        self.silence_scan = Some(SilenceScan {
            key: key.clone(),
            started: now,
            progress: Arc::clone(&progress),
            seen: 0,
            since: now,
        });
        cx.spawn(async move |this, cx| {
            let landed = scan.await;
            this.update(cx, |this, cx| {
                // Cancelled means the card moved on or closed: the levels are a
                // prefix of a track nobody asked about any more.
                if progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.silence_scan = None;
                match landed {
                    Ok(Some(levels)) => {
                        this.silence_levels.insert(key.clone(), Arc::new(levels));
                        this.scan_silences();
                    }
                    // A source with no audio track is not one long silence: it
                    // is a clip this card has nothing to say about, named so the
                    // user knows which one it meant.
                    Ok(None) => {
                        if let Some((lane, idx)) = this.silence_open {
                            this.notify_user(unscannable(lane, idx, &key.0).into());
                        }
                        this.close_silence();
                    }
                    Err(e) => {
                        this.close_silence();
                        this.notify_user(format!("SCAN FAILED: {e}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the scanning line's stall clock, for [`Player::poll_import`]'s
    /// reason: sampled once per frame rather than while drawing.
    fn poll_silence(&mut self) {
        if let Some(scan) = &mut self.silence_scan {
            scan.poll();
        }
    }

    /// Applies the settings to levels already in hand and replaces the preview
    /// -- never stacks on it. Arithmetic only: the decode is
    /// [`Player::start_silence_scan`]'s and happens once per source, so every
    /// run here is numbers already read, which is what makes moving a threshold
    /// feel like moving a slider. A source still being scanned has no marks yet
    /// and says so on the card.
    ///
    /// Changes nothing about the project: a preview is not an edit, and no undo
    /// step is spent until a button is pressed.
    fn scan_silences(&mut self) {
        let Some((lane, idx)) = self.silence_open else {
            return;
        };
        self.silence_marks.clear();
        // Copied out before anything is written back: the cache below lives on
        // the same struct the session does.
        let Some((clip, source)) = self.session.as_ref().and_then(|session| {
            let clip = *session.lane_clips(lane).get(idx)?;
            Some((clip, session.sources().get(clip.source)?.clone()))
        }) else {
            return;
        };
        // Nothing read yet: the worker is running and the card is drawing its
        // line. The marks arrive with the levels.
        let Some(levels) = self
            .silence_levels
            .get(&(
                source.path.clone(),
                source.audio_stream,
                clip.in_frame,
                clip.out_frame,
            ))
            .cloned()
        else {
            return;
        };
        self.silence_marks = engine::silence::timeline_regions(
            &clip,
            self.fps,
            &engine::silence::regions(&levels, self.silence),
        );
    }

    /// Moves the picked row by `steps` and re-runs the scan against it, so the
    /// marks on the lane are always what the numbers on the card say.
    fn nudge_silence(&mut self, steps: i32) {
        let secs = |at: f64| {
            (at + f64::from(steps) * SILENCE_SECS_STEP)
                .clamp(SILENCE_SECS_RANGE.0, SILENCE_SECS_RANGE.1)
        };
        match self.silence_field {
            // Round either way, like the fit policy's cycle: three choices are
            // a ring, not a range.
            0 => {
                let at = SCOPES.iter().position(|&s| s == self.silence_scope);
                let step = steps.rem_euclid(SCOPES.len() as i32) as usize;
                self.silence_scope = SCOPES[(at.unwrap_or(0) + step) % SCOPES.len()];
            }
            1 => {
                self.silence.threshold_db = (self.silence.threshold_db
                    + steps as f32 * SILENCE_DB_STEP)
                    .clamp(SILENCE_DB_RANGE.0, SILENCE_DB_RANGE.1)
            }
            // Two spellings of the same level, so either arrow flips it -- a
            // ring of two, like the scope row's.
            2 => self.silence_dbfs = !self.silence_dbfs,
            3 => self.silence.min_silence = secs(self.silence.min_silence),
            4 => self.silence.padding = secs(self.silence.padding),
            5 => self.silence.min_keep = secs(self.silence.min_keep),
            _ => {
                self.silence_factor =
                    silence_rate(i32::from(self.silence_factor.permille()) + steps * SPEED_STEP)
            }
        }
        // Neither the scope nor the rate is part of the scan, but re-running is
        // cheap (the levels are cached) and one path is one place for the marks
        // to come from.
        self.scan_silences();
    }

    /// Which lanes an apply reaches, as the card's scope row says it: the
    /// lanes of the take the scanned clip belongs to, that clip's lane alone,
    /// or every lane there is.
    ///
    /// The take's lanes are the ones carrying its group id -- a link is one
    /// span on however many lanes, so "the take" is exactly the set of lanes
    /// that would otherwise be pulled apart. Nothing widens behind the user's
    /// back: [`Project::cut_regions`] refuses a scope that would split a take,
    /// and this row is how the user says the take instead.
    fn silence_lanes(&self) -> Vec<Lane> {
        let (Some((lane, idx)), Some(session)) = (self.silence_open, self.session.as_ref()) else {
            return Vec::new();
        };
        match self.silence_scope {
            Scope::Track => vec![lane],
            Scope::Everything => session.lanes(),
            Scope::Take => match session.lane_clips(lane).get(idx).and_then(|c| c.link) {
                None => vec![lane],
                Some(id) => session
                    .lanes()
                    .into_iter()
                    .filter(|&l| {
                        l == lane || session.lane_clips(l).iter().any(|c| c.link == Some(id))
                    })
                    .collect(),
            },
        }
    }

    /// What an apply acts on: the previewed set and the lanes it reaches, or
    /// nothing at all with a notice saying so in the numbers that found
    /// nothing.
    fn previewed(&mut self) -> Option<(Vec<(u32, u32)>, Vec<Lane>)> {
        if self.silence_marks.is_empty() {
            self.notify_user(
                format!(
                    "no silence under {:.0} dBFS lasting {:.2} s — raise the threshold or forgive less",
                    self.silence.threshold_db, self.silence.min_silence
                )
                .into(),
            );
            return None;
        }
        Some((self.silence_marks.clone(), self.silence_lanes()))
    }

    /// What an apply says afterwards: which tracks it reached, and -- when that
    /// was not all of them -- that the rest were left where they were. The
    /// scope is a choice, so the confirmation has to name the choice.
    fn silence_reach(&self, lanes: &[Lane]) -> String {
        let named = lanes
            .iter()
            .map(|l| l.label())
            .collect::<Vec<_>>()
            .join("+");
        match self.silence_scope {
            Scope::Everything => "on every track".to_string(),
            _ => format!("on {named} — other tracks untouched"),
        }
    }

    /// Cuts every previewed silence out of the lanes the scope names, rippling
    /// each hole closed -- one edit and **one** undo press however many there
    /// were ([`engine::PlaybackSession::cut_regions`]). Tracks outside the
    /// scope do not move; a scope that would take half a take with it comes
    /// back refused in the engine's own words, naming both halves.
    fn cut_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let saved = f64::from(regions.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        let (count, reach) = (regions.len(), self.silence_reach(&lanes));
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.cut_regions(&regions, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Every hole closed moves the clips after it up a place, so the
                // selection now names a different clip than the one that is
                // highlighted -- dropped here as after every other edit that
                // moves indexes (a delete, a paste, an undo).
                self.selected = None;
                self.reset_after_reseek();
                self.notify_user(
                    format!(
                        "{count} SILENCES CUT {reach} — {} shorter, {} takes it back",
                        secs_label(saved),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Plays them fast instead of cutting them, closing the room each one no
    /// longer needs. One undo press like the cut, and the same scope; the
    /// refusals (a clip lapping over a silence, a scope that would split a
    /// take) come back in the engine's own words and name the lane and frame,
    /// and the card stays up so the numbers that produced it are still on
    /// screen.
    fn speed_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let (count, rate) = (regions.len(), self.silence_factor);
        let reach = self.silence_reach(&lanes);
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.speed_regions(&regions, rate, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Splitting each silence out and closing the room it no longer
                // needs moves indexes exactly as the cut does: the selection
                // goes with them.
                self.selected = None;
                self.reset_after_reseek();
                self.notify_user(
                    format!(
                        "{count} SILENCES AT {rate} {reach} — {} takes it back",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Whether a card owns the window. While one does the timeline under it is
    /// out of reach, so a right-click there opens no menu -- the same rule the
    /// key handler and the drop target already follow.
    /// Whether anything at all is drawn over the window -- a card, a menu or an
    /// open list. What the hover labels stand aside for ([`OVERLAID`]): a
    /// tooltip belongs to the surface the pointer is on, and while one of these
    /// is up that surface is behind it.
    fn overlaid(&self) -> bool {
        self.modal()
            || self.context_menu.is_some()
            || self.library_menu.is_some()
            || self.picker.is_some()
    }

    fn modal(&self) -> bool {
        self.keys_open
            || self.export_open
            || self.eq_open.is_some()
            || self.color_open.is_some()
            || self.speed_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
            || self.exporting().is_some()
    }

    /// The pointer's way out of whatever card is up: what every scrim's press
    /// calls, so `esc` is *a* way out and never the only one. One list, and the
    /// same one [`Player::modal`] reads -- a card that can be counted there but
    /// not closed here is a card a hand alone cannot shut, which is what
    /// `every_card_closes_without_the_keyboard` fails on.
    ///
    /// Every card at once because only one is ever up (`export_open`): closing
    /// "the" card and closing all of them are the same act.
    fn close_card(&mut self) {
        self.keys_open = false;
        self.keys_search.clear();
        self.rebinding = None;
        self.export_open = false;
        // The two things typed *into* the export card go with it: a field left
        // open would take the next keystroke for a card that is gone.
        self.mbps_edit = None;
        self.picker = None;
        self.eq_open = None;
        self.eq_dragging = false;
        self.color_open = None;
        self.speed_open = None;
        // Marks and a running scan go with this one, which is why it is a call
        // and not an assignment ([`Player::close_silence`]).
        if self.silence_open.is_some() {
            self.close_silence();
        }
        self.mix_open = false;
    }

    /// Which of [`Repeat`]'s three the window is in, for the hold gate at the
    /// top of the key handler. Not [`Player::modal`]: that asks whether an
    /// overlay is up at all, and here the cards with sliders in them are
    /// exactly the ones that answer differently from the keys menu and the
    /// export card.
    fn repeat_scope(&self) -> Repeat {
        // A number being typed is a value under the arrows, exactly as a card's
        // slider is -- so a held arrow runs it. Asked before the export card
        // below, which otherwise repeats nothing.
        if self.mbps_edit.is_some() {
            Repeat::Card
        } else if self.rebinding.is_some()
            || self.keys_open
            || self.export_open
            || self.exporting().is_some()
        {
            Repeat::Nothing
        } else if self.eq_open.is_some()
            || self.color_open.is_some()
            || self.speed_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
        {
            Repeat::Card
        } else {
            Repeat::Keymap
        }
    }

    /// Opens the equalizer on the selected clip. Audio only, and it says so
    /// rather than opening a card of bands that would reach nothing: a video
    /// clip carries no sound of its own here (the sound is the audio lane's),
    /// and the model would take the setting without anything ever playing it.
    fn open_eq(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let refusal = match (self.selected, &self.session) {
            (_, None) => Some("NO TIMELINE — open a file first".to_string()),
            (None, _) => Some(format!(
                "NOTHING SELECTED — click an audio clip or press {}, then ask again",
                self.keymap.display(ActionId::Select)
            )),
            (Some((lane, _)), _) if lane.kind != LaneKind::Audio => Some(
                "NOT AN AUDIO CLIP — the equalizer works on the sound, so pick a clip in an audio lane".to_string(),
            ),
            _ => None,
        };
        if let Some(refusal) = refusal {
            self.notify_user(refusal.into());
            cx.notify();
            return;
        }
        let (lane, idx) = self.selected.expect("checked above");
        let session = self.session.as_ref().expect("checked above");
        // What the clip already plays through, or the flat default -- so the
        // card opens on the curve that is in force and a reopen shows the last
        // drag rather than a fresh set of zeroes.
        self.eq_params = session
            .eq_of(lane, idx)
            .cloned()
            .unwrap_or_else(EqParams::default_layout);
        self.eq_band = 0;
        self.eq_dragging = false;
        self.eq_open = Some((lane, idx));
        // One card at a time, the rule the other two already follow.
        self.keys_open = false;
        self.export_open = false;
        self.context_menu = None;
        cx.notify();
    }

    /// Writes what the card is showing at its clip: one undo step, one entry in
    /// the append-only equalizer table, so this is called once per *gesture* --
    /// the end of a drag, a keystroke -- and never per pointer sample.
    ///
    /// A curve that moves nothing is stored as *no* equalizer at all, which is
    /// what keeps a clip nobody has touched on the identity path through
    /// playback and export (`engine::eq::EqParams::is_identity`).
    fn commit_eq(&mut self, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.eq_open else {
            return;
        };
        let params = (!self.eq_params.is_identity()).then(|| self.eq_params.clone());
        if let Some(session) = &mut self.session {
            session.set_eq(lane, idx, params);
        }
        // `set_eq` reseeks inside the engine -- that is what makes the change
        // audible at once -- and a reseek is what these flags are about.
        self.reset_after_reseek();
        cx.notify();
    }

    /// Changes the picked band in place and says whether anything moved. Every
    /// edit of a band goes through here -- the drag, each key, each stepper
    /// button -- so the card has exactly one place that clamps a band into what
    /// the graph can draw, and no caller has to remember the limits.
    fn set_band(&mut self, change: impl FnOnce(&mut Band)) -> bool {
        let Some(band) = self.eq_params.bands.get_mut(self.eq_band) else {
            return false;
        };
        let was = *band;
        change(band);
        band.freq_hz = band.freq_hz.clamp(EQ_FREQ_LOW, EQ_FREQ_HIGH);
        band.gain_db = band.gain_db.clamp(-EQ_GAIN_LIMIT, EQ_GAIN_LIMIT);
        band.q = band.q.clamp(EQ_Q_LOW, EQ_Q_HIGH);
        *band != was
    }

    /// The keyboard's and the buttons' version of a drag: one step on the picked
    /// band, committed straight away -- neither has a release to wait for.
    fn nudge_band(&mut self, change: impl FnOnce(&mut Band), cx: &mut Context<Self>) {
        if self.set_band(change) {
            self.commit_eq(cx);
        }
    }

    /// Where the pointer sits in the graph, as the picked band's frequency and
    /// gain: across is the frequency axis and down is the gain one, so the
    /// handle follows the hand both ways rather than sliding up a rail. Called
    /// on every pointer sample, so the curve bends under it; the write is the
    /// release's ([`commit_eq`](Player::commit_eq)).
    fn drag_band(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = self.eq_graph.get();
        let gain = (0.5 - frac_down(at.y, bounds)) * 2. * EQ_GAIN_LIMIT;
        let freq = eq_freq(frac_along(at.x, bounds));
        if self.set_band(|b| {
            b.gain_db = gain;
            b.freq_hz = freq;
        }) {
            cx.notify();
        }
    }

    /// A band added beside the picked one, at the frequency with the most room
    /// around it ([`inserted_band`]), and picked so the next keystroke moves the
    /// band that was just made. Refused rather than silently ignored at the cap.
    fn add_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() >= EQ_BANDS_MAX {
            self.notify_user(
                format!(
                    "EQUALIZER FULL — {EQ_BANDS_MAX} bands is all this card holds; move one instead"
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let band = inserted_band(&self.eq_params.bands, self.eq_band);
        self.eq_band = (self.eq_band + 1).min(self.eq_params.bands.len());
        self.eq_params.bands.insert(self.eq_band, band);
        self.commit_eq(cx);
    }

    /// Takes the picked band out. The last one stays: an equalizer of no bands
    /// is a card with nothing to edit, and flattening is what "off" means here.
    fn remove_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() <= 1 {
            self.notify_user("LAST BAND — flatten it instead (r), or close the card".into());
            cx.notify();
            return;
        }
        self.eq_params.bands.remove(self.eq_band);
        self.eq_band = self.eq_band.min(self.eq_params.bands.len() - 1);
        self.commit_eq(cx);
    }

    /// Which band a press on the graph grabs: the nearest one along the
    /// frequency axis, so the whole box is the handle rather than a 10 px dot
    /// -- and a press that misses every dot still moves the band it is under.
    fn nearest_band(&self, x: Pixels) -> usize {
        let at = frac_along(x, self.eq_graph.get());
        self.eq_params
            .bands
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (eq_x(a.freq_hz) - at)
                    .abs()
                    .total_cmp(&(eq_x(b.freq_hz) - at).abs())
            })
            .map_or(0, |(i, _)| i)
    }

    /// Jumps the timeline.
    fn seek(&mut self, t: f64, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        session.seek(t);
        self.reset_after_reseek();
        cx.notify();
    }

    /// The keyboard's seek: whole frames along the timeline, through the same
    /// door a ruler click uses -- so a step while playing keeps playing, exactly
    /// as a click does. It starts from the frame the transport is showing, which
    /// past the end is the last one, and that is what lets a step back off EOS
    /// revive the picture (the engine's seek leaves [`Transport::Ended`]). Both
    /// ends clamp, so the two go-to actions are this same step asked for more
    /// frames than the timeline has. Selection is untouched: a seek is not an
    /// edit, and nothing it does moves a clip index.
    fn step(&mut self, frames: i64, cx: &mut Context<Self>) {
        let ended = self.transport() == Transport::Ended;
        let Some(session) = &self.session else {
            return;
        };
        let last = ((session.timeline_duration() * self.fps).round() as i64 - 1).max(0);
        let now = match ended {
            true => last,
            false => i64::from(frame_at(session.now(), self.fps)),
        };
        let target = now.saturating_add(frames).clamp(0, last);
        self.seek(target as f64 / self.fps, cx);
    }

    /// Splits the clip under the playhead. Metadata only: the timeline->source
    /// mapping is unchanged, so nothing reseeks and no flag is touched.
    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // Snapped to the source's own grid where one is within reach
        // ([`Player::cut_frame`]): a cut a third of a second off a sync point
        // looks identical on the bed and turns an export that copies its
        // picture in minutes into one that codes every frame of it for hours.
        // The playhead goes with it -- what was cut has to be where the line
        // is, or the next stroke acts a few frames from where it looks.
        let Some(session) = &self.session else {
            return;
        };
        let now = frame_at(session.now(), self.fps);
        let at = self.cut_frame(now);
        if at != now {
            self.seek(f64::from(at) / self.fps, cx);
        }
        if let Some(session) = &mut self.session {
            session.cut_at(f64::from(at) / self.fps);
        }
        self.selected = None;
        cx.notify();
    }

    /// Rejoins whatever meets under the playhead and puts it back in one group
    /// -- the inverse of [`Player::cut`], and metadata only like it. The engine
    /// decides what is joinable; a refusal is worded here, because `false` is
    /// all it says and a key that looks broken is worse than one that explains
    /// itself.
    fn regroup(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(session) = &mut self.session {
            if session.regroup_at(session.now()) {
                self.selected = None;
            } else {
                self.notify_user(
                    "NOTHING TO REGROUP — put the playhead where two clips meet, on frames that were cut apart"
                        .into(),
                );
            }
        }
        cx.notify();
    }

    /// Takes the selected clip out of its group, so the picture and the sound
    /// under it are edited apart from here on: each half selects, moves, trims
    /// and is removed alone, and both draw outlined instead of tinted. The
    /// selection stays -- the half that was clicked is still the half in hand.
    fn detach(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match (&mut self.session, self.selected) {
            (Some(session), Some((lane, idx))) => {
                if !session.ungroup(lane, idx) {
                    self.notify_user(
                        "NOTHING DETACHED — that clip is not grouped with another".into(),
                    );
                }
            }
            (Some(_), None) => {
                self.notify_user("NOTHING DETACHED — click the take to take apart first".into())
            }
            (None, _) => {}
        }
        cx.notify();
    }

    /// Puts the selected clip back in a group with the clip covering exactly the
    /// same frames on another track -- the way back from [`Player::detach`], and
    /// the way to group a picture with sound it was never opened with. The
    /// partner is not clicked because there is nothing to choose: a group id
    /// names one span, so only a clip covering these very frames could join it,
    /// and the engine words what to do when none does.
    fn group(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let partner = match (&self.session, self.selected) {
            (Some(session), Some((lane, idx))) => span_partner(session, lane, idx),
            _ => None,
        };
        match (&mut self.session, self.selected, partner) {
            (Some(session), Some((lane, idx)), Some((other, o_idx))) => {
                if let Err(e) = session.group(lane, idx, other, o_idx) {
                    self.notify_user(format!("NOT GROUPED — {e}").into());
                }
            }
            (Some(_), Some(_), None) => {
                self.notify_user(
                    "NOTHING TO GROUP WITH — no clip on another track covers exactly these frames"
                        .into(),
                )
            }
            (Some(_), None, _) => {
                self.notify_user("NOTHING GROUPED — click one of the halves first".into())
            }
            (None, ..) => {}
        }
        cx.notify();
    }

    /// Drops the selected clip and closes the hole: a whole take goes, both
    /// lanes of it, and everything after it moves up. A half with no take under
    /// it in the video lane -- what a lift leaves behind -- has nothing to
    /// ripple, so that one is lifted instead. The engine reseeks itself, so all
    /// this owes is the flag reset.
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let selected = self.selected.take();
        // Whichever lane it was clicked in: the index is that lane's own, and
        // the ripple cuts the clip's span out of every lane -- a group covers
        // one span, so deleting a take by its audio half is the same edit as by
        // its picture. What is not a whole take is lifted instead, which is what
        // reaches a clip on an added track ([`whole_take`]).
        let deleted = match (&mut self.session, selected) {
            (Some(session), Some((lane, idx))) => match whole_take(session, lane, idx) {
                true => session.delete_clip(lane, idx),
                false => session.lift_clip(lane, idx),
            },
            _ => false,
        };
        if selected.is_some() && !deleted {
            self.notify_user("NOTHING DELETED — that clip is no longer there".into());
        }
        if deleted {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Lifts the selected half out and leaves the hole: black picture there if
    /// it was the video lane, silence if it was the audio one, and nothing else
    /// moves. What Delete is not.
    fn lift_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match (&mut self.session, self.selected.take()) {
            (Some(session), Some((lane, idx))) => {
                if session.lift_clip(lane, idx) {
                    self.reset_after_reseek();
                } else {
                    self.notify_user("NOTHING LIFTED — that half is no longer there".into());
                }
            }
            (Some(_), None) => {
                self.notify_user("NOTHING LIFTED — click the half to remove first".into())
            }
            (None, _) => {}
        }
        cx.notify();
    }

    /// Copies the selected clip. Nothing on screen changes, so no notify.
    fn copy_selected(&mut self) {
        let session = self.session.as_ref();
        // Out of the lane it was clicked in: the audio half of a group is a
        // different clip from the video one, and copying the wrong lane's
        // frames is a paste of the wrong thing.
        if let Some(clip) = self
            .selected
            .and_then(|(lane, idx)| session?.lane_clips(lane).get(idx).copied())
        {
            self.clipboard = Some(clip);
        }
    }

    /// Starts a peak decode -- and a stream probe -- for every source that has
    /// arrived since the last
    /// repaint. One call from the render rather than three at the doors,
    /// because argv, an import and a project load are all doors and only this
    /// one is guaranteed to run after each of them.
    ///
    /// The decode itself runs on a background thread, like the file chooser:
    /// whole-file audio decode is ~1 s for a half-hour source, and on the render
    /// path that is the window not painting for a second. The lane draws a bed
    /// meanwhile and the repaint comes with the peaks. The entry is written
    /// *before* the spawn, so the sixty repaints that happen while a decode runs
    /// start no further ones.
    fn cache_media(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        // How big each still is, for the row that has to say so. Inline, unlike
        // the two below: an image header is a few bytes off the front of the
        // file, where a sample table is a parse and a decode is a second.
        for path in unseen_paths(session.sources(), &self.sizes) {
            let size = engine::is_image(&path)
                .then(|| engine::image_size(&path).ok())
                .flatten();
            self.sizes.insert(path, size);
        }
        // Which audio streams each file has, for the library's rows. Header
        // only, but a big file's sample tables are not free to parse, so it
        // goes off the render thread like the peaks do.
        for path in unseen_paths(session.sources(), &self.streams) {
            self.streams.insert(path.clone(), Vec::new());
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::AudioSession::probe_streams(&path).unwrap_or_default() }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.streams.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // What each file is coded at, for the card that says so. Header and
        // sample table only, but a Matroska indexes no samples and its open
        // walks every cluster header -- 6.7 s on a 12.9 GB film -- so this of
        // all of them cannot be on the render thread.
        for path in unseen_paths(session.sources(), &self.bitrates) {
            self.bitrates.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::probe_bitrate(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.bitrates.insert(path, Some(probed));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // Where this file's own groups of pictures begin, for the cut that
        // wants to land on one ([`Player::sync_frames`]). The heaviest probe
        // here -- a Matroska's whole cluster walk, seconds on a film -- and the
        // one nothing waits for: until it answers, the snap is the clip-edge
        // snap it always was.
        for path in unseen_paths(session.sources(), &self.syncs) {
            self.syncs.insert(path.clone(), Vec::new());
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::demux::sync_points(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.syncs.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // Which decoder each file will run on, for the row that says so before
        // a frame of it plays. Off the render thread like the streams above: a
        // stream the plugin takes costs one VA-API init (~90 ms) to answer.
        for path in unseen_paths(session.sources(), &self.decoders) {
            self.decoders.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                // A song and a source no decoder here takes are both `None`:
                // the row says nothing about them rather than guessing, and
                // import refused the second at the door anyway.
                async move { engine::decode::probe(&path).ok() }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.decoders.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        for key in unseen_sources(session.sources(), &self.waves) {
            self.waves.insert(key.clone(), Wave::Loading);
            let decoded = cx.background_executor().spawn({
                let (path, stream) = key.clone();
                async move {
                    engine::waveform::peaks(&path, stream, WAVE_BPS)
                        .map(|peaks| peaks.map(|peaks| Arc::new(normalise(peaks))))
                        .inspect_err(|e| eprintln!("waveform: {}: {e}", path.display()))
                }
            });
            cx.spawn(async move |this, cx| {
                let decoded = decoded.await;
                this.update(cx, |this, cx| {
                    this.waves.insert(
                        key,
                        match decoded {
                            Ok(Some(peaks)) => Wave::Peaks(peaks),
                            // No audio track: an answer, and not worth asking
                            // about again.
                            Ok(None) => Wave::Silent,
                            // A file whose sound we could not read is not a
                            // silent one, and a lane that drew it as silent is
                            // how a broken decode passes for a design choice.
                            Err(_) => Wave::Failed,
                        },
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Probes what an export would open, once per (settings, resolution, cuts)
    /// and only while the export card is up -- it opens the very VA-API encoder
    /// the export would and asks [`engine::export::planned_seats`] the very
    /// question the export asks itself, which is what makes the card's line a
    /// measurement instead of a promise, and also what makes it too slow for
    /// the render thread. Written before the spawn, like the probes above, so
    /// the repaints during it start no second one.
    ///
    /// The cuts are in the key because they are in the answer: moving one onto
    /// a sync point is exactly what turns "SW encode" into "copy", and a card
    /// that kept the old line would be lying about the file it is about to
    /// write.
    fn cache_export_seat(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let settings =
            export_settings(self.quality, self.custom_mbps, self.format, self.audio_kbps);
        if !self.export_open {
            return;
        }
        // A format with no picture has no seat to probe -- and the *last*
        // format's is not its answer: cleared rather than left standing, or
        // picking MP3 after AV1 would read "SW encode (rav1e) · MP3 · SW
        // (rusty_mp3)", which names an encoder that will not run.
        if !settings.format.has_video() {
            self.export_seat = None;
            return;
        }
        // The timeline an export would be started with, owned so the probe can
        // run on a worker -- and the clips beside it, which are what tells this
        // that the question has changed.
        let (project, meta) = session.export_snapshot();
        let clips: Vec<Clip> = session
            .lanes()
            .into_iter()
            .flat_map(|lane| session.lane_clips(lane).to_vec())
            .collect();
        // Cloned rather than copied: the settings carry the picked subtitle
        // rows, which is a `Vec` ([`engine::export::ExportSettings`]).
        let key = (settings.clone(), (meta.width, meta.height), clips);
        if self
            .export_seat
            .as_ref()
            .is_some_and(|(asked, size, cuts, _)| (asked, size, cuts) == (&key.0, &key.1, &key.2))
        {
            return;
        }
        self.export_seat = Some((key.0.clone(), key.1, key.2.clone(), None));
        let probed = cx.background_executor().spawn(async move {
            engine::export::planned_seats(&project, &meta, &settings)
        });
        cx.spawn(async move |this, cx| {
            let probed = probed.await;
            this.update(cx, |this, cx| {
                // Only if the card is still asking the same question: a format
                // changed while the plugin opened has a probe of its own.
                if let Some(seat) = this.export_seat.as_mut().filter(|(asked, size, cuts, _)| {
                    (asked, size, cuts) == (&key.0, &key.1, &key.2)
                }) {
                    seat.3 = Some(probed);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks the plugin what this machine's GPU can do, once, the first time the
    /// export card is up to show it. Off the render thread for `cache_export_seat`'s
    /// reason: the plugin initialises VA-API to answer, and a driver that is
    /// slow to load must not be a frame the user waits for.
    fn cache_hw_caps(&mut self, cx: &mut Context<Self>) {
        if !self.export_open || self.hw_caps.is_some() {
            return;
        }
        // Written before the spawn, exactly as the probes above are, so the
        // repaints during it start no second one.
        self.hw_caps = Some("asking the driver…".into());
        let asked = cx
            .background_executor()
            .spawn(async move { engine::caps::hardware() });
        cx.spawn(async move |this, cx| {
            let line = asked.await;
            this.update(cx, |this, cx| {
                this.hw_caps = Some(line.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The group id of the clicked clip, which is what marks the other half.
    fn selected_link(&self) -> Option<u32> {
        let (lane, idx) = self.selected?;
        self.session.as_ref()?.lane_clips(lane).get(idx)?.link
    }

    /// Drops the copied clip in at the playhead. The engine reseeks itself, so
    /// like a delete this owes the flag reset -- and the selection, whose index
    /// the insert has just moved.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let pasted = match (&mut self.session, self.clipboard) {
            (Some(session), Some(clip)) => session.paste_at(session.now(), clip),
            _ => false,
        };
        if pasted {
            self.selected = None;
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// A clip let go at window `x` over lane `to`: it lands with its head where
    /// the hand is carrying it ([`Player::drop_frame`]), on the track it was
    /// dropped on, taking its whole take with it -- one undo step for the
    /// gesture. Dropped back where it was picked up it is not an edit at all, so
    /// nothing is said about it. The engine reseeks, so all this owes is the
    /// flag reset -- and the selection, whose index was that lane's own and now
    /// names a different clip there.
    fn move_clip(&mut self, from: Lane, idx: usize, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let (Some((start, _)), Some(was)) = (
            self.drop_frame(from, idx, x),
            self.session
                .as_ref()
                .and_then(|session| session.lane_clips(from).get(idx).map(|c| c.start)),
        ) else {
            return;
        };
        let moved = self
            .session
            .as_mut()
            .is_some_and(|session| session.move_clip_to(from, idx, to, start));
        let (kind, lanes) = match from.kind {
            LaneKind::Video => ("picture", "video"),
            LaneKind::Audio => ("sound", "audio"),
        };
        match moved {
            true => {
                self.selected = None;
                self.reset_after_reseek();
            }
            // The three ways a drag is refused, told apart by what the
            // front-end already knows: a lane's kind, and where the clip was.
            // Everything else that could refuse (a clip that is not there)
            // cannot be dragged.
            false if from.kind != to.kind => {
                self.notify_user(
                    format!(
                        "NOT ON {} — that is a {kind} clip; drop it on a {lanes} lane",
                        to.label()
                    )
                    .into(),
                )
            }
            // Picked up and put back down where it was: a click, and a click
            // says nothing.
            false if from == to && start == was => {}
            false => {
                self.notify_user(
                    format!(
                        "NOT MOVED — another clip already covers those frames on {}",
                        to.label()
                    )
                    .into(),
                )
            }
        }
        cx.notify();
    }

    /// Where a clip let go at window `x` over lane `to` wants its head: the
    /// frame under the pointer, less however far into the box the hand grabbed
    /// it (so the clip does not jump under the pointer), pulled onto a
    /// neighbouring edge when it lands within [`SNAP_PX`] of one. `None` when
    /// there is no such clip to move. The engine has the last word on where it
    /// may actually go -- this is the ask, not the answer.
    ///
    /// corner-cut: the bed now runs past the last frame whenever the timeline is
    /// shorter than the view ([`Scale::time_at`] clamps at the head only), so a
    /// clip *can* be dragged out there. Zoomed in against the far end it cannot:
    /// the scroll clamp pins the bed's right edge to the duration, and the
    /// pointer has no pixel past it. The upgrade is to let the scroll clamp
    /// leave a screen of empty bed after the end, the way every NLE does.
    fn drop_frame(&self, from: Lane, idx: usize, x: Pixels) -> Option<(u32, Option<u32>)> {
        let clip = self.session.as_ref()?.lane_clips(from).get(idx).copied()?;
        let marks = self.snap_targets(Some((from, idx)));
        Some(landing(
            self.frame_under(x),
            self.grab,
            clip.frames(),
            self.snap,
            self.snap_frames(),
            &marks,
        ))
    }

    /// The same answer for a library row on its way down: nothing is in the hand
    /// yet, so there is no grab offset to take off and no length to snap by --
    /// the file's own is not known until the engine has placed it -- and only
    /// its head lands. Asked by the line, by the ghost and by the drop itself
    /// ([`Player::insert_source`]), so all three name one frame.
    fn place_frame(&self, x: Pixels) -> (u32, Option<u32>) {
        let marks = self.snap_targets(None);
        landing(
            self.frame_under(x),
            0,
            0,
            self.snap,
            self.snap_frames(),
            &marks,
        )
    }

    /// Which index the clip in the hand is at *now*: [`live_idx`] against the
    /// lane the drag named, since a stroke during the gesture moves the indices
    /// gpui froze into the payload. Both halves of a drag ask it -- the line
    /// drawn in flight and the drop that commits -- so the promise and the
    /// landing are made about one clip.
    fn dragged(&self, drag: &ClipDrag) -> Option<usize> {
        let session = self.session.as_ref()?;
        live_idx(session.lane_clips(drag.lane), drag.idx, drag.clip)
    }

    /// The line while the clip is still in the hand: the very answer
    /// [`Player::drop_frame`] will commit, worked out on every move of the drag,
    /// so what the eye was promised is where the release puts it. A pointer that
    /// has wandered off the bed promises nothing.
    fn preview_drop(&mut self, from: Lane, idx: usize, x: Pixels, cx: &mut Context<Self>) {
        let cue = self.drop_frame(from, idx, x).and_then(|(_, cue)| cue);
        self.set_cue(cue, x, cx);
    }

    /// The same line for a library row on its way to a lane: it goes down at
    /// the frame it is let go on ([`Player::place_frame`]), so that frame is
    /// what snaps and what is drawn.
    fn preview_place(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let cue = self.place_frame(x).1;
        self.set_cue(cue, x, cx);
    }

    /// The shadow the clip in the hand would fill, on the lane the pointer is
    /// over: its head where [`Player::drop_frame`] says the release will put it
    /// -- the same call the drop makes, so the box drawn and the box committed
    /// are one answer -- and its own length at this zoom. A lane of the other
    /// kind refuses the drop ([`Project::move_clip`]), and the shadow says so
    /// before the release does.
    fn preview_ghost(&mut self, drag: &ClipDrag, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        let ghost = self
            .dragged(drag)
            .and_then(|idx| self.drop_frame(drag.lane, idx, x))
            .map(|(start, _)| Ghost {
                lane: to,
                start,
                frames: drag.clip.frames(),
                tint: self.clip_tint(drag.clip.source),
                refused: drag.lane.kind != to.kind,
            });
        self.set_ghost(ghost, cx);
    }

    /// The line the track in the hand would drop into, on the row the pointer
    /// is over: at that row's top edge when the header is coming up from below
    /// and at its bottom edge when it is going down, which is the slot
    /// [`Player::reorder_lane`] commits to at the release. Nothing at all over
    /// its own row, where a release changes nothing.
    fn preview_lane_drop(&mut self, from: Lane, onto: Lane, cx: &mut Context<Self>) {
        let lanes = self
            .session
            .as_ref()
            .map_or_else(Vec::new, PlaybackSession::lanes);
        let at = |lane: Lane| lanes.iter().position(|&l| l == lane);
        let next = match (at(from), at(onto)) {
            (Some(i), Some(j)) if i != j => Some(LaneDrop {
                lane: onto,
                above: j < i,
            }),
            _ => None,
        };
        // Only when it has actually changed: a drag move fires on every painted
        // frame, and a redraw per frame that draws the same line is a redraw
        // for nothing.
        if self.lane_drop != next {
            self.lane_drop = next;
            cx.notify();
        }
    }

    /// The line taken back down again, by the row that drew it and by no other:
    /// the pointer has been carried off `lane`, so the slot it was promising is
    /// no longer the one a release would commit to.
    fn forget_lane_drop(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.lane_drop.is_some_and(|d| d.lane == lane) {
            self.lane_drop = None;
            cx.notify();
        }
    }

    /// The same shadow for a library row: its head at [`Player::place_frame`],
    /// which is where the drop inserts it, and the file's own length for its
    /// width -- the length the library row already reports. A file this lane
    /// cannot hold ([`lane_refuses`]) is tinted as refused, which is the answer
    /// the release would give in words.
    fn preview_ghost_asset(&mut self, path: &Path, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        let ghost = Ghost {
            lane: to,
            start: self.place_frame(x).0,
            frames: self
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(path)),
            // A path with no source entry has no colour of its own, and the
            // shadow wears the lane's own instead of borrowing another file's.
            tint: file_tint(self.sources(), path).unwrap_or(BG_RAISED()),
            refused: lane_refuses(path, to).is_some(),
        };
        self.set_ghost(Some(ghost), cx);
    }

    /// Sets the shadow, or takes it away, repainting only when it moved -- the
    /// listeners below run it on every pointer sample of a drag. Cleared by the
    /// root and set again by the lane under the pointer, in that order (gpui
    /// runs the capture phase parent-first), so a pointer over no lane at all
    /// leaves nothing drawn.
    fn set_ghost(&mut self, ghost: Option<Ghost>, cx: &mut Context<Self>) {
        if ghost != self.ghost {
            self.ghost = ghost;
            cx.notify();
        }
    }

    /// The swatch a clip from source `n` wears: [`source_tint`] over the first
    /// source entry naming that *file*, since two audio streams of one file are
    /// two sources and one colour. Every box on a lane and every ghost a drag
    /// draws asks this, so the shadow is recognisably the thing in the hand.
    fn clip_tint(&self, source: usize) -> u32 {
        self.sources()
            .get(source)
            .and_then(|entry| file_tint(self.sources(), &entry.path))
            .unwrap_or_else(|| source_tint(source))
    }

    fn sources(&self) -> &[Source] {
        self.session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources)
    }

    /// Sets the line, or takes it away, and repaints only when it moved: a
    /// pointer dragged off the bed (up to the library, say) is not promising a
    /// landing any more.
    fn set_cue(&mut self, cue: Option<u32>, x: Pixels, cx: &mut Context<Self>) {
        let bed = self.ruler.get();
        let cue = cue.filter(|_| x >= bed.left() && x <= bed.right());
        if cue != self.snap_cue {
            self.snap_cue = cue;
            cx.notify();
        }
    }

    /// Every edge this timeline offers a gesture: [`snap_marks`] over all of its
    /// lanes, so a clip meets a take one track over as readily as one beside it.
    fn snap_targets(&self, skip: Option<(Lane, usize)>) -> Vec<u32> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let lanes = session.lanes();
        let clips: Vec<&[Clip]> = lanes.iter().map(|&lane| session.lane_clips(lane)).collect();
        let skip = skip.and_then(|(lane, idx)| Some((lanes.iter().position(|&l| l == lane)?, idx)));
        snap_marks(&clips, skip, frame_at(session.now(), self.fps))
    }

    /// Where a gesture at `raw` lands and the mark that pulled it there, with
    /// the switch honoured: snapping off, nothing moves and no line is drawn.
    fn snap_to(&self, raw: u32, len: u32, marks: &[u32]) -> (u32, Option<u32>) {
        snap_cue(self.snap, raw, len, self.snap_frames(), marks)
    }

    /// Every timeline frame that is a *source* sync point: each clip's own
    /// grid ([`Player::syncs`]), moved onto the frames the clip plays it at.
    /// Ascending, because the clips are and each grid is.
    ///
    /// This is the difference between an export that copies its picture and one
    /// that decodes and re-codes every frame of a feature film. A cut anywhere
    /// else leaves the copy path with a region that begins between two sync
    /// points -- pictures whose references are not in the file -- and the whole
    /// export falls back to the encoder ([`engine::export`] states the rule).
    ///
    /// Only clips at their own speed, and only video lanes: a re-timed clip is
    /// resampled pictures, which is not a copy at any cut, and a sound lane has
    /// no groups of pictures to begin with.
    fn sync_frames(&self) -> Vec<u32> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let sources = session.sources();
        let mut marks = Vec::new();
        for lane in session.lanes() {
            if lane.kind != LaneKind::Video {
                continue;
            }
            for clip in session.lane_clips(lane) {
                let Some(keys) = sources
                    .get(clip.source)
                    .and_then(|entry| self.syncs.get(&entry.path))
                    .filter(|_| clip.speed.is_normal())
                else {
                    continue;
                };
                marks.extend(
                    keys.iter()
                        .filter(|&&key| key >= clip.in_frame && key < clip.out_frame)
                        .map(|&key| clip.start + (key - clip.in_frame)),
                );
            }
        }
        marks.sort_unstable();
        marks
    }

    /// The frame a cut asked for at `raw` really lands on: the nearest source
    /// sync point within the snap's own tolerance, or `raw` itself where the
    /// magnet is off, where nothing is near enough, or where the source has no
    /// grid to offer (the walk has not answered yet, or the file is not one
    /// this project can copy at all).
    ///
    /// The same tolerance the clip-edge snap uses, so one switch and one
    /// distance govern every landing on this timeline.
    fn cut_frame(&self, raw: u32) -> u32 {
        if !self.snap {
            return raw;
        }
        let tol = self.snap_frames();
        self.sync_frames()
            .into_iter()
            .filter(|mark| mark.abs_diff(raw) <= tol)
            .min_by_key(|mark| mark.abs_diff(raw))
            .unwrap_or(raw)
    }

    /// Whether the playhead is standing exactly on one: what the timeline's own
    /// line says out loud, so "a cut here is copied" is on screen before the cut
    /// rather than discovered in the export card afterwards.
    ///
    /// Asked every repaint, so it walks the *playhead* into each clip's source
    /// and looks it up in that source's own sorted grid -- where
    /// [`Player::sync_frames`] builds the whole list, which is a film's worth of
    /// marks to allocate and sort sixty times a second.
    fn on_sync_point(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let now = frame_at(session.now(), self.fps);
        let sources = session.sources();
        session.lanes().into_iter().any(|lane| {
            lane.kind == LaneKind::Video
                && session.lane_clips(lane).iter().any(|clip| {
                    clip.speed.is_normal()
                        && (clip.start..clip.start + (clip.out_frame - clip.in_frame))
                            .contains(&now)
                        && sources
                            .get(clip.source)
                            .and_then(|entry| self.syncs.get(&entry.path))
                            .is_some_and(|keys| {
                                keys.binary_search(&(clip.in_frame + (now - clip.start))).is_ok()
                            })
                })
        })
    }

    /// Puts the playhead on the sync point before or after it -- the keyboard's
    /// half of placing a cut where the export can copy it, and the only way to
    /// reach one exactly on a timeline zoomed out to a whole film, where one
    /// pixel is seconds.
    fn jump_sync(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let now = frame_at(session.now(), self.fps);
        let marks = self.sync_frames();
        let mark = match forward {
            true => marks.iter().find(|&&mark| mark > now).copied(),
            false => marks.iter().rev().find(|&&mark| mark < now).copied(),
        };
        match mark {
            Some(mark) => self.seek(f64::from(mark) / self.fps, cx),
            // Said rather than swallowed: the two most likely reasons are a walk
            // that has not answered yet and a source with no grid at all, and a
            // key that does nothing looks broken either way.
            None => self.notify_user(match marks.is_empty() {
                true => "NO SYNC POINTS — this source has no keyframe grid to jump by (or it is \
                         still being read)"
                    .into(),
                false => "NO SYNC POINT THAT WAY — the playhead is past the last one".into(),
            }),
        }
    }

    /// [`SNAP_PX`] in timeline frames at the scale the bed is drawn at: the bed's
    /// own width drops out of it, since a pixel is now worth the same stretch of
    /// timeline wherever the view sits.
    fn snap_frames(&self) -> u32 {
        self.scale.snap_frames(self.fps)
    }

    /// Opens the clip menu on the box under the pointer, from the right button
    /// wherever it was pressed on that box -- its middle or one of its edge
    /// strips, which cover the middle's own listener. Selecting first is part of
    /// it: every item acts on the clip the menu names.
    fn open_menu(&mut self, lane: Lane, idx: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.modal() {
            return;
        }
        self.select((lane, idx), cx);
        self.context_menu = Some(ContextMenu {
            lane,
            idx,
            at,
            details: false,
        });
        cx.notify();
    }

    /// A press on a clip's edge: the start of the drag that changes how much of
    /// its source it plays. It selects the clip as a press anywhere else on the
    /// box does -- the edge strip covers the box's own listener (`occlude`), so
    /// this is the only one that fires there.
    fn start_trim(&mut self, lane: Lane, idx: usize, edge: Edge, cx: &mut Context<Self>) {
        if self.modal() || self.exporting().is_some() {
            return;
        }
        let Some(clip) = self
            .session
            .as_ref()
            .and_then(|session| session.lane_clips(lane).get(idx).copied())
        else {
            return;
        };
        self.select((lane, idx), cx);
        self.trim = Some(Trim {
            lane,
            idx,
            edge,
            // Where the edge already is: a press that never moves is not an
            // edit, and `Project::trim` refuses exactly that.
            to: match edge {
                Edge::Start => clip.start,
                Edge::End => clip.end(),
            },
            link: clip.link,
        });
        cx.notify();
    }

    /// Where the pointer has pulled the edge to, clamped to the room the engine
    /// says that edge has. Along the same bed the ruler is measured on and
    /// against the same duration the boxes are drawn to, so the edge tracks the
    /// pointer exactly.
    fn trim_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(trim) = self.trim else {
            return;
        };
        // The edge is pulled onto the same marks a whole clip is, by itself:
        // there is no other end travelling with it, so it snaps at length zero.
        let marks = self.snap_targets(Some((trim.lane, trim.idx)));
        let (at, cue) = self.snap_to(self.frame_under(x), 0, &marks);
        let Some((lo, hi)) = self
            .session
            .as_ref()
            .and_then(|session| session.trim_room(trim.lane, trim.idx, trim.edge))
        else {
            return;
        };
        let to = at.clamp(lo, hi);
        // The line only stands where the edge actually stopped: a mark the
        // engine's own room clamped away was never reached.
        self.set_cue(cue.filter(|_| to == at), x, cx);
        self.trim = Some(Trim { to, ..trim });
        cx.notify();
    }

    /// The timeline frame a pointer at window x is on: along the same bed the
    /// ruler is measured on, through the same [`Scale`] every box is drawn
    /// through, so a zoomed-in panel answers with the frame under the pointer
    /// and not with the one that would have been there unzoomed. The one
    /// question a trim, a grab and a drop all ask.
    fn frame_under(&self, x: Pixels) -> u32 {
        frame_at(
            self.scale.time_at(px_along(x, self.ruler.get())),
            self.fps,
        )
    }

    /// The release: the whole drag reaches the engine as one edit, so it is one
    /// undo step. The selection survives it -- a trim inserts and removes
    /// nothing, so every index a lane had still names the clip it named.
    fn commit_trim(&mut self, cx: &mut Context<Self>) {
        let Some(trim) = self.trim.take() else {
            return;
        };
        let trimmed = self
            .session
            .as_mut()
            .is_some_and(|session| session.trim_clip(trim.lane, trim.idx, trim.edge, trim.to));
        if trimmed {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The clip as the drag is showing it: an edge under the pointer moves its
    /// own box, and the boxes of the halves linked to it, before anything is
    /// committed. Display only -- the project is not touched until the release.
    fn trimmed(&self, lane: Lane, idx: usize, clip: Clip) -> Clip {
        let Some(trim) = self.trim.filter(|t| {
            (t.lane, t.idx) == (lane, idx) || (t.link.is_some() && t.link == clip.link)
        }) else {
            return clip;
        };
        let still = self.session.as_ref().is_some_and(|session| {
            session
                .sources()
                .get(clip.source)
                .is_some_and(|s| engine::is_image(&s.path))
        });
        trimmed_clip(clip, trim.edge, trim.to, still)
    }

    /// How long the timeline is *drawn* as: its own length, and while a tail is
    /// being dragged the furthest that tail may reach. A bed that ends exactly
    /// at the last frame has nowhere to put a pointer that means "longer", so
    /// without this the last clip on the timeline could be pulled in and never
    /// let back out.
    ///
    /// Scroll room only, now that a second is an absolute number of pixels
    /// ([`Scale`]): the extra length loosens [`View::settled`]'s clamp, which is
    /// where the pixels past the last frame come from, and moves no box by a
    /// pixel. It is still the *only* headroom at the tail -- zoomed in against
    /// the end, that clamp pins the bed's right edge to the duration and an
    /// End-trim of the last clip would have nowhere to be dragged to. What it
    /// must not do is be read as a length anyone is told: the timecode reads
    /// `PlaybackSession::timeline_duration` for exactly that reason.
    fn drawn_duration(&self) -> f64 {
        let Some(session) = &self.session else {
            return 0.;
        };
        let duration = session.timeline_duration();
        match self.trim {
            Some(trim) if trim.edge == Edge::End => {
                let (_, hi) = session
                    .trim_room(trim.lane, trim.idx, trim.edge)
                    .unwrap_or((0, 0));
                duration.max(f64::from(hi) / self.fps)
            }
            _ => duration,
        }
    }

    /// Where the playhead is, as the panel draws it: pinned to the out point
    /// once playback is done, and clamped to the drawn duration otherwise -- a
    /// tail being dragged draws past the timeline it is about to become.
    fn playhead(&self, duration: f64) -> f64 {
        if self.transport() == Transport::Ended {
            duration
        } else {
            self.session
                .as_ref()
                .map_or(0., PlaybackSession::now)
                .clamp(0., duration)
        }
    }

    /// The one way a library row reaches the timeline: the Add button and a row
    /// dragged onto a lane both come here, so there is a single answer to what
    /// "add this source" does. The whole source goes in as one grouped take at
    /// `at` -- the frame the pointer let it go on, or the playhead for the
    /// button, which names no place. It is the same insert a paste makes, so
    /// everything after it moves along rather than being painted over. Reseeks
    /// like every other edit, and drops the timeline's selection with it: the
    /// insert has just moved the indices it pointed at.
    fn insert_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        at: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The lane the pointer named cannot hold this kind of file: refused by
        // name, in the same words the ghost was tinted by on the way down
        // ([`lane_refuses`]). The Add button names no lane and so is never
        // refused here -- where a file goes when nobody says is the engine's
        // choice, in `place_stream_at`, not one made twice here.
        if let Some(why) = onto.and_then(|lane| lane_refuses(path, lane)) {
            self.notify_user(why.into());
            cx.notify();
            return;
        }
        // The engine's own length for the file, noted when the import took it
        // in: a row that has never been on a lane is placeable at its full
        // length, which is the whole point of an import that only fills the
        // library.
        let fps = self.fps;
        let placed = match &mut self.session {
            // Seconds, because that is what the engine's own door takes: the
            // frame the pointer named goes back through the same rate every box
            // on the bed is drawn at, so it lands on the frame it was let go on
            // rather than a neighbouring one.
            Some(session) => {
                let at = at.map_or_else(|| session.now(), |frame| f64::from(frame) / fps);
                session.place_stream_at(at, path, stream, onto)
            }
            None => Ok(false),
        };
        match placed {
            Ok(true) => {
                self.selected = None;
                self.reset_after_reseek();
            }
            // The engine's own words: a stream that cannot join this timeline
            // says which property disagrees, exactly as a refused import does.
            Err(e) => self.notify_user(format!("NOTHING ADDED — {e}").into()),
            Ok(false) => {
                self.notify_user("NOTHING ADDED — that file could not be placed here".into())
            }
        }
        cx.notify();
    }

    /// Takes a library row's file out of the list, which is the one thing a row
    /// can lose. Refused in the engine's own words while clips still play from
    /// it -- and those words name the lanes holding them, so the refusal says
    /// what to delete first. The list itself is the report that it worked: the
    /// row is gone from it.
    fn remove_source(&mut self, path: &Path, stream: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_source(path, stream));
        let text = match removed {
            Some(Ok(idx)) => {
                // The picked row may be the one that just went, and the engine
                // reseeks, so this owes the flag reset like every other edit.
                if self.selected_asset.as_ref() == Some(&(path.to_path_buf(), stream)) {
                    self.selected_asset = None;
                }
                // A copied clip names its source by *index*, and every index
                // past the one that went has just moved down: without this the
                // next paste puts some other file on the timeline.
                self.clipboard = clipboard_after_remove(self.clipboard, idx);
                self.reset_after_reseek();
                // The last row leaves a session naming no file: nothing to
                // play, nothing to save and nothing to show, which is the empty
                // window the editor launches as. The next import scaffolds a
                // fresh timeline from whatever file it is, at that file's own
                // rate -- which is why the session goes rather than lingering
                // on with the gone file's parameters.
                match self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.sources().is_empty())
                {
                    true => {
                        self.close_session();
                        format!(
                            "REMOVED {} — the library is empty; import a file to start again",
                            file_name(path)
                        )
                    }
                    // The undo stack goes with it (`Project::remove_source`):
                    // said here, because a `z` that does nothing afterwards
                    // would otherwise read as a bug.
                    false => format!(
                        "REMOVED {} — there is nothing left to undo",
                        file_name(path)
                    ),
                }
            }
            Some(Err(e)) => format!("NOT REMOVED — {e}"),
            None => "NO TIMELINE — open a file first".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// Back to the window the editor launches as: no timeline, no library, no
    /// picture -- and the hint that says to open a file. What removing the last
    /// library row leaves, since a session whose library is empty has nothing
    /// left to be ([`Player::remove_source`]).
    ///
    /// Everything a *loaded project* resets goes here for its reasons (an index
    /// into a timeline that is gone names nothing), plus the three per-file
    /// caches: they are keyed by path, and the next file to arrive fills them
    /// again.
    fn close_session(&mut self) {
        self.session = None;
        // The picture goes with it, or the empty window would keep showing the
        // last frame of a timeline that no longer exists.
        //
        // corner-cut: its atlas tile is not released -- `window.drop_image` wants
        // a `&mut Window` this door has no other reason to take. One tile per
        // emptied library, against one per displayed frame in `pump`; the
        // upgrade path is threading the window through `act_on_row`.
        self.image = None;
        // The drawn cue with it, and its tile for the same reason as above.
        self.sub_image = None;
        self.clipboard = None;
        self.selected = None;
        self.selected_asset = None;
        // The subtitle rows go with the timeline they were on.
        self.sub_track = 0;
        self.context_menu = None;
        self.library_menu = None;
        self.eq_open = None;
        self.color_open = None;
        self.speed_open = None;
        self.close_silence();
        self.waves.clear();
        self.streams.clear();
        self.bitrates.clear();
        self.sizes.clear();
        self.syncs.clear();
        // Scanned off sources that are not in the library any more.
        self.silence_levels.clear();
        // Every gesture in flight, dropped for `reset_after_reseek`'s reason
        // (it drops the trim below): a drag holds a bar, a clip or a band of a
        // timeline that has just stopped existing.
        self.scrubbing = false;
        self.volume_dragging = false;
        self.eq_dragging = false;
        self.speed_dragging = false;
        self.color_dragging = false;
        self.pending_color = None;
        self.pending_speed = None;
        self.displayed = 0;
        self.dropped = 0;
        self.started = None;
        // The empty window's own: no name in the titlebar, nowhere chosen to
        // export or save to yet, and a rate that only keeps the timecode
        // reading in frames until a file brings its own (`main`).
        self.name = NO_FILE.into();
        self.export_path = PathBuf::new();
        self.project_path = PathBuf::new();
        self.fps = 30.;
        // No decoder to wait for a frame from: the hint is what shows. The
        // transport reads `Stopped` from the session being gone, so there is no
        // end-of-stream state left to clear here.
        self.reset_after_reseek();
        self.seek_since = None;
    }

    /// One item of a library row's menu, done. Every one of them closes the
    /// menu first -- the list under it is about to be rebuilt -- except the one
    /// that turns the card over.
    fn act_on_row(&mut self, item: RowItem, cx: &mut Context<Self>) {
        let Some(menu) = self.library_menu.clone() else {
            return;
        };
        match item {
            RowItem::Properties => {
                if let Some(open) = &mut self.library_menu {
                    open.details = true;
                }
            }
            RowItem::Add => {
                self.library_menu = None;
                self.insert_source(&menu.path, menu.stream, None, None, cx);
            }
            RowItem::Remove => {
                self.library_menu = None;
                self.remove_source(&menu.path, menu.stream, cx);
            }
            RowItem::Reveal => {
                self.library_menu = None;
                // Another process starting: off the UI thread, exactly as the
                // export notice's own click starts it.
                cx.background_executor()
                    .spawn(async move { show_in_file_manager(&menu.path) })
                    .detach();
            }
        }
        cx.notify();
    }

    /// The rate and layout the whole timeline's audio is, taken from the stream
    /// of the first source that could have one: what a library row has to match
    /// to be placeable. `None` until that file has been probed, and then nothing
    /// is greyed for it.
    ///
    /// The first source that is *not a still*, which is the rule the engine
    /// holds every import to (`playback::audio_source_of`) -- a picture at the
    /// head of the list (a removal moves indexes) has no stream to describe
    /// anything with.
    fn timeline_audio(&self) -> Option<(u32, u16)> {
        let first = self
            .session
            .as_ref()?
            .sources()
            .iter()
            .find(|s| !engine::is_image(&s.path))?;
        let info = self
            .streams
            .get(&first.path)?
            .iter()
            .find(|s| s.index == first.audio_stream)?;
        Some((info.sample_rate, info.channels))
    }

    /// Queues a file for the library. Nothing is read here: the reading is
    /// [`read_ahead`] on a worker, and [`Player::take_import`] is what finally
    /// touches the timeline, one repaint later and with the pages warm. A drop
    /// is not a key press, so the export guard on the key handler does not
    /// cover it and this checks for itself.
    ///
    /// One file at a time, in arrival order: a drop can carry six and argv can
    /// name more, and six header walks racing over one disk finish no sooner
    /// than six in a row -- while the line above the panel has exactly one file
    /// to name.
    fn import(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.imports.push_back(path.to_path_buf());
        self.start_import(cx);
    }

    /// Starts the worker for the next queued file, if no worker is running.
    /// Called again as each import lands, which is what drains the queue.
    fn start_import(&mut self, cx: &mut Context<Self>) {
        if self.importing.is_some() {
            return;
        }
        let Some(path) = self.imports.pop_front() else {
            return;
        };
        let stage = Arc::new(std::sync::atomic::AtomicU8::new(ImportStage::Header as u8));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The fork is made here, once, and carried to the landing: an import is
        // *probed* on the worker and registered from what came back, while the
        // file argv named -- and a file arriving at a window with no timeline to
        // import into -- is *opened* on the worker and handed over whole. None
        // of the three leaves the UI thread anything to read: a cold 24 GB
        // header walk is twenty seconds, and the window keeps painting through
        // all of them.
        let what = arrival(self.opening.as_deref(), &path);
        // The timeline the file will be checked against, taken here because the
        // worker cannot reach the session: two clones and no disk
        // ([`PlaybackSession::import_gate`]). `None` is a window with nothing to
        // import into, which is the fork that opens the file outright.
        let gate = self.session.as_ref().map(PlaybackSession::import_gate);
        let read = cx.background_executor().spawn({
            let (path, stage) = (path.clone(), Arc::clone(&stage));
            async move { open_ahead(what, &path, &stage, gate) }
        });
        let now = Instant::now();
        self.importing = Some(Import {
            path: path.clone(),
            started: now,
            stage,
            seen: ImportStage::Header,
            since: now,
            cancelled: Arc::clone(&cancelled),
        });
        cx.spawn(async move |this, cx| {
            let landed = read.await;
            this.update(cx, |this, cx| {
                this.importing = None;
                // Cancelled while it read: the window was given back at the
                // click and said so then, so what the worker carried is dropped
                // without a second word ([`Player::cancel_import`]).
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.take_import(&path, landed, cx);
                // The next one is started by the repaint this notified, which
                // is also what starts the files argv named ([`poll_import`]).
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the import line's two clocks honest: the elapsed one runs from the
    /// worker, and the stall one from the last time the stage it is naming
    /// actually changed. Sampled here rather than while drawing, for
    /// [`Player::poll_export`]'s reason.
    ///
    /// ...and starts whatever is queued behind it, which is the one place the
    /// files argv named can begin: they are put in the queue before there is a
    /// context to spawn a worker from.
    fn poll_import(&mut self, cx: &mut Context<Self>) {
        match &mut self.importing {
            Some(import) => {
                import.poll();
            }
            None => self.start_import(cx),
        }
    }

    /// The Cancel beside the import line: the window is given back at once and
    /// the file does not land. Everything queued behind it goes too -- a person
    /// who has stopped an import of six dropped files has stopped the six, and
    /// leaving five to start themselves would be the same wait under another
    /// name.
    ///
    /// The read in flight is *not* stopped, for the reason [`Import::cancelled`]
    /// gives, and the notice says as much rather than promising the disk went
    /// quiet.
    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        let Some(import) = self.importing.take() else {
            return;
        };
        import
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let waiting = self.imports.len();
        self.imports.clear();
        let tail = match waiting {
            0 => String::new(),
            n => format!(" — {n} more dropped from the queue"),
        };
        let text = format!(
            "IMPORT CANCELLED: {}{tail} — the read already running finishes unheeded",
            file_name(&import.path)
        );
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a read-ahead file into the library and nowhere else: the timeline
    /// is not touched, and the row is dragged onto a lane when it is wanted
    /// there. Nothing moves, so nothing reseeks; a refusal is shown as the
    /// engine worded it and changes nothing.
    ///
    /// The export guard again, and not for the caller's sake: an export can
    /// have started during the seconds the worker was reading, and a drop
    /// during an export has always been a silent no-op.
    fn take_import(&mut self, path: &std::path::Path, landed: Landed, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // The file argv named is the one file in the queue that is not an
        // import: it *is* the timeline, and the worker has already opened it.
        // All that is left here is to hang everything derived from it off the
        // window -- the clock, the title, where an export and a save go -- and
        // that is arithmetic, not a read.
        let (subs, probe) = match landed {
            Landed::Read(subs, probe) => (subs, probe),
            what => {
                // Only when *this* is the file argv named: a project dropped
                // while that one is still being read must not make it land as
                // an import.
                if self.opening.as_deref() == Some(path) {
                    self.opening = None;
                }
                match what {
                    Landed::Project(opened) => self.install_project(path, opened, cx),
                    Landed::Media(opened, place) => {
                        let text = self.install_media(path, opened, place);
                        eprintln!("{text}");
                        self.notify_user(text.into());
                        cx.notify();
                    }
                    Landed::Read(..) => unreachable!("matched above"),
                }
                // The line a launch has always printed, now printed when the
                // file actually arrives: it is the mark that says the timeline
                // is up, as the window's own appearance is the other one.
                if let Some(meta) = self.session.as_ref().map(PlaybackSession::meta) {
                    println!(
                        "{}: {}x{} @ {:.2} fps, {} samples",
                        path.display(),
                        meta.width,
                        meta.height,
                        meta.frame_rate,
                        meta.frame_count
                    );
                }
                return;
            }
        };
        // An empty window has no library to add to yet: the file opens one, and
        // the timeline under it stays empty, because an import is an import
        // whether or not a session was already up. A file *named at launch* is
        // the other fork -- that one is an open, and it does become the
        // timeline (`main`).
        // A subtitle file is not a source and lands on no lane: it joins the
        // timeline's own list of them, which is what the library's subtitle
        // section shows and what the overlay draws. With no timeline open there
        // is nothing for the cues to be timed against, and it says so.
        if is_subtitle(path) {
            self.take_subtitles(path, subs, cx);
            return;
        }
        // The container was read on the worker and what came back is registered
        // here ([`engine::PlaybackSession::import_probed`]): no header walk, no
        // decoder open, no probe of the timeline's own first source -- the three
        // reads that used to be spent on this thread. A song and a still fork
        // before the demuxer and pay their own small read
        // ([`engine::PlaybackSession::import`]); a window whose timeline went
        // away while the worker read falls to the slow door below, which is the
        // one that can still open one.
        let registered = match (self.session.as_mut(), probe) {
            (Some(session), Some(Ok(probe))) => Some(session.import_probed(path, probe)),
            (Some(_), Some(Err(refused))) => Some(Err(refused)),
            (Some(session), None) => Some(session.import(path)),
            (None, _) => None,
        };
        let text = match registered {
            Some(Ok(_)) => {
                // The file's own subtitle tracks with it, exactly as an open
                // takes them: an import is the other door the same file arrives
                // through. The cues were read on the worker
                // ([`read_ahead`]); what happens here is the push.
                let tail = self
                    .session
                    .as_mut()
                    .and_then(|session| subtitle_tail(session, subs))
                    .unwrap_or_default();
                format!(
                    "IMPORTED {} to the library — drag it onto a lane to place it{tail}",
                    file_name(path)
                )
            }
            // Named, because two files can fail in one launch and the queue now
            // shows both: "No such file or directory" twice over, with nothing
            // saying which file, is two messages that answer nothing.
            Some(Err(e)) => format!("IMPORT FAILED: {} — {e}", file_name(path)),
            None => self.open_media(path, false, subs),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a file as the session an empty window is waiting for. Everything
    /// derived from the media -- the clock, the title, where an export and a
    /// save go -- is set here, exactly as a launch with a file argument sets
    /// it. Paused with its first frame showing, like every other way a timeline
    /// arrives.
    ///
    /// `place` is the difference between the two doors that come here: a file
    /// *opened* is the timeline, one *imported* into an empty window fills the
    /// library and leaves the lanes empty for a drag.
    fn open_media(&mut self, path: &std::path::Path, place: bool, subs: Subs) -> String {
        self.install_media(path, open_session(path, place, subs), place)
    }

    /// The second half of it: everything the window derives from a session that
    /// has already been opened. Split from the open itself because the file
    /// argv named is opened on a worker ([`open_ahead`]) -- the twelve seconds
    /// of a cold header walk are not the UI thread's to spend -- and lands
    /// here, where nothing is read and nothing blocks.
    fn install_media(
        &mut self,
        path: &std::path::Path,
        opened: Result<(PlaybackSession, String), String>,
        place: bool,
    ) -> String {
        match opened {
            Ok((session, subs)) => {
                self.fps = session.meta().frame_rate;
                // Read before the session moves: a file that plays silent says
                // so here or nowhere.
                let silent = audio_notice(&session);
                // A file replaces the one that was open, and track 3 of that one
                // is not track 3 of this.
                self.sub_track = 0;
                self.session = Some(session);
                // A fresh session comes up at full volume; the player's own
                // setting outlives the file, so it is pushed at every new one.
                self.apply_volume();
                // Beside the new file, but still the format the card is set to:
                // opening another clip is not a change of mind about that.
                self.export_path = retarget(&export_path(path), self.format);
                self.project_path = project_path(path);
                self.name = file_name(path).into();
                self.reset_after_reseek();
                let name = file_name(path);
                // The library is filled and the timeline is empty; the only
                // thing that says so is this line, so it says what to do next.
                let what = match place {
                    true => format!("OPENED {name}"),
                    false => {
                        format!("IMPORTED {name} to the library — drag it onto a lane to place it")
                    }
                };
                format!("{what}{}{subs}", silent.unwrap_or_default())
            }
            Err(e) => format!("OPEN FAILED: {e}"),
        }
    }

    /// The Import button: asks the desktop for a path and takes it the same way
    /// a drop would. The chooser is another process and the user may sit in it,
    /// so it runs on a background thread -- blocking here would freeze the
    /// window behind the dialog.
    fn pick_and_import(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — import") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                // One queue, and the fork is made when its worker starts
                // ([`arrival`]): a project replaces the timeline, media joins
                // the library, and neither is read on this thread.
                Ok(Some(path)) => this.import(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The `+ S` button and its key: asks the desktop for a file and takes the
    /// subtitle tracks out of it -- a standalone `.srt`/`.vtt`/`.ass` is one of
    /// them, a Matroska however many are inside. Only the subtitles: the file
    /// itself does not join the library, which is what the Import button beside
    /// this one is for.
    ///
    /// The chooser is another process and the user may sit in it, so it runs on
    /// a background thread, exactly as [`Player::pick_and_import`] does.
    fn pick_and_add_subtitles(&mut self, cx: &mut Context<Self>) {
        // What dims the `+ S` button, asked here as well so the key answers the
        // same question -- and *before* the chooser rather than after it: a door
        // that opens a dialog, waits for a file and only then says the timeline
        // was never there is the second door disagreeing with the first.
        if let Some(why) = self.enable(ActionId::AddSubtitleTrack, None).why() {
            let text = format!("NO SUBTITLES ADDED — {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — subtitles to add") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(path)) => this.add_subtitles(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a file's subtitle tracks onto the timeline, off the render thread.
    /// The walk reads the whole container for its cues
    /// (`engine::PlaybackSession::parse_subtitles`) -- ~200 ms on a two-hour 4K
    /// remux and 1.3 s on a cold 3 GB one -- and a button that costs the window
    /// that many frames is a button that freezes it. So the *parse* is the
    /// worker's, whole, and the UI thread only pushes what came back
    /// ([`PlaybackSession::add_subtitle_tracks`]): no borrow crosses the await,
    /// because the parse is an associated fn that owns nothing.
    fn add_subtitles(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        // Nothing to time the cues against: said now rather than after a walk
        // of a 25 GB file that was never going to be kept.
        if self.session.is_none() {
            self.landed_subtitles(path, None, cx);
            return;
        }
        self.notify_user(format!("READING {} for subtitles…", file_name(path)).into());
        let parsed = cx.background_executor().spawn({
            let path = path.to_path_buf();
            async move { engine::PlaybackSession::parse_subtitles(&path) }
        });
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let parsed = parsed.await;
            this.update(cx, |this, cx| {
                // The dedupe lives inside the push, so a second `+ S` on the
                // same file still answers 0 and still says so below.
                let added = this
                    .session
                    .as_mut()
                    .map(|session| parsed.and_then(|tracks| pushed(session, &path, tracks)));
                this.landed_subtitles(&path, added, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Every subtitle track a `.srt`/`.vtt`/`.ass` carries, onto the timeline
    /// and nowhere else: they are not clips and land on no lane. The cues came
    /// off the worker that read the file ([`read_ahead`]), like every other
    /// door's do, and what is left here is the push. The engine dedupes by
    /// (file, track), so the same `.srt` twice is one row and says so.
    fn take_subtitles(&mut self, path: &std::path::Path, subs: Subs, cx: &mut Context<Self>) {
        let added = self
            .session
            .as_mut()
            .map(|session| subs.and_then(|tracks| pushed(session, path, tracks)));
        self.landed_subtitles(path, added, cx);
    }

    /// What the timeline says once the tracks are on it, whichever worker did
    /// the reading: the `+ S` button and its key ([`Self::add_subtitles`]), a
    /// dropped or imported subtitle file ([`Self::take_subtitles`]), and a
    /// window with nothing to time cues against all word the outcome here,
    /// once, so no two doors can drift apart.
    fn landed_subtitles(
        &mut self,
        path: &std::path::Path,
        added: Option<engine::Result<usize>>,
        cx: &mut Context<Self>,
    ) {
        let text = match added {
            Some(Ok(0)) => format!(
                "{}'s subtitles are on the timeline already",
                file_name(path)
            ),
            Some(Ok(n)) => format!(
                "SUBTITLES {} — {n} track(s), showing over the picture, {} hides them",
                file_name(path),
                self.keymap.display(ActionId::ToggleSubtitles)
            ),
            Some(Err(e)) => format!("SUBTITLE IMPORT FAILED: {e}"),
            None => "NO SUBTITLES ADDED — open a file for them to run against first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// The × on a subtitle row, and the stroke that takes the picked one off:
    /// the track leaves the timeline and the pick moves with it. Every index
    /// past the one that went moves down
    /// ([`engine::Project::remove_subtitles`]), so a pick left where it was
    /// would name a *different* track -- and the pick is what an export writes
    /// into the file.
    ///
    /// Not an undo step: subtitles are not on the history's snapshots, so the
    /// way back is putting the file's subtitles on again -- which is a door of
    /// its own ([`Player::pick_and_add_subtitles`]) and reads the subtitles
    /// alone, never the media. The notice says that rather than promising a
    /// ctrl+z that would do nothing.
    fn remove_subtitle_track(&mut self, track: usize, cx: &mut Context<Self>) {
        // The one availability oracle, for the same reason the × on a row and
        // the stroke are one call: an empty list is not a failure, it is an
        // action with nothing to act on, and the engine's "there is no subtitle
        // track 0" is an index nobody typed. A real removal that fails still
        // says what the engine said, below.
        if let Some(why) = self.enable(ActionId::RemoveSubtitleTrack, None).why() {
            let text = format!("NO SUBTITLES REMOVED — {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        // Read before it goes: a notice naming an index names nothing.
        let name = self
            .session
            .as_ref()
            .and_then(|session| sub_pick_name(session.subtitles(), track))
            .unwrap_or_else(|| format!("subtitle track {track}"));
        let text = match self
            .session
            .as_mut()
            .map(|session| session.remove_subtitles(track))
        {
            Some(Ok(())) => {
                let left = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.subtitles().len());
                self.sub_track = sub_pick_after_removal(self.sub_track, track, left);
                // The drawn cue is keyed by that index ([`Player::sub_picture`])
                // and the index now stands for another track.
                //
                // corner-cut: its atlas tile is not released -- `close_session`'s
                // note, for its reason and with its upgrade path.
                self.sub_image = None;
                format!(
                    "{name} REMOVED — {} puts a file's subtitles back on, the file itself stays off",
                    self.keymap.display(ActionId::AddSubtitleTrack)
                )
            }
            Some(Err(e)) => format!("NO SUBTITLES REMOVED — {e}"),
            None => "NO SUBTITLES REMOVED — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Swaps the whole timeline for one restored from a `.edith`, for
    /// [`Player::install_media`]'s reason: the open is a worker's -- a project
    /// naming a 24 GB film opens that film, which is the same twenty seconds
    /// ([`arrival`] sends every `.edith` through the one queue) -- and this is
    /// what is left once it lands. Nothing is replaced until the new session is
    /// in hand, so a refusal is shown as the engine worded it and leaves what is
    /// playing alone.
    fn install_project(
        &mut self,
        path: &std::path::Path,
        opened: Result<PlaybackSession, String>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let text = match opened {
            Ok(session) => {
                self.fps = session.meta().frame_rate;
                let silent = audio_notice(&session);
                // A project is named after itself but still exports beside its
                // media: that is the only place an export has ever landed.
                self.export_path = retarget(&export_path(&session.sources()[0].path), self.format);
                self.session = Some(session);
                self.apply_volume();
                self.project_path = path.to_path_buf();
                self.name = file_name(path).into();
                // A copied clip names its source by index, which means a
                // different file -- or none -- in another project.
                self.clipboard = None;
                self.selected = None;
                // A menu can be up while a project is dropped on the window --
                // the scrim swallows clicks, never a drop -- and its index
                // would name some other timeline's clip. The two clip cards
                // hold a (lane, idx) of the old timeline for the same reason.
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                // Marks are timeline frames of the timeline that was.
                self.close_silence();
                // A different set of sources: the row that was picked is not
                // the file that index names any more -- and neither is the
                // subtitle track that was showing.
                self.selected_asset = None;
                self.sub_track = 0;
                // The counters describe one timeline; the eof line must not
                // report the old one's frames against the new one.
                self.displayed = 0;
                self.dropped = 0;
                self.started = None;
                // Loaded paused at its saved playhead, so the still it owes
                // reaches the screen the way a seek's does. The old picture is
                // released by the swap in `pump`, as after any other seek.
                self.reset_after_reseek();
                format!("LOADED {}{}", file_name(path), silent.unwrap_or_default())
            }
            Err(e) => format!("OPEN FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Writes the timeline back to its project file. Overwrites silently, like
    /// an export: the path was chosen once and the notice is the confirmation.
    fn save_project(&mut self, cx: &mut Context<Self>) {
        let saved = self
            .session
            .as_ref()
            .map(|session| session.save_project(&self.project_path));
        let text = match saved {
            Some(Ok(())) => format!("SAVED {}", file_name(&self.project_path)),
            Some(Err(e)) => format!("SAVE FAILED: {e}"),
            None => "NOTHING TO SAVE — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// A new empty track under the ones already there. One undo step in the
    /// engine, so the stroke that takes back an edit takes back a track too, and
    /// no reseek: nothing plays differently until something is dropped on it.
    /// The selection stays -- the lanes it indexes into have not moved.
    fn add_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match &mut self.session {
            Some(session) => {
                let lane = session.add_lane(kind);
                self.notify_user(
                    format!(
                        "{} ADDED — drag a clip onto it, {} takes it back",
                        lane.label(),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            None => self.notify_user("NO TRACK ADDED — open a file first".into()),
        }
        cx.notify();
    }

    /// The × in a track's header: the add taken back, one undo step, and the
    /// engine's own words when it refuses -- those name the clips still on the
    /// track, so the notice says what to delete first. A removal never deletes a
    /// clip.
    ///
    /// Everything holding a `(lane, idx)` is dropped, because the tracks below
    /// the one that went have just moved up an `ord`
    /// ([`engine::Project::remove_lane`]): a selection or an open card kept
    /// across it would be pointing at the *next* track's clip.
    fn remove_lane(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_lane(lane));
        let text = match removed {
            Some(Ok(())) => {
                self.selected = None;
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                self.speed_open = None;
                self.close_silence();
                format!(
                    "{} REMOVED — {} brings it back",
                    lane.label(),
                    self.keymap.display(ActionId::Undo)
                )
            }
            Some(Err(e)) => format!("NO TRACK REMOVED — {e}"),
            None => "NO TRACK REMOVED — open a file first".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// A header let go over another header: the track in the hand takes that
    /// one's place in the stack, clips and all
    /// ([`engine::Project::move_lane`]), one undo step. The gesture every
    /// editor reorders tracks with, and the only way the order is ever changed
    /// -- there is no second list of it to keep in step.
    ///
    /// Display order is the stack, so moving a video track past another video
    /// track changes which picture wins, here and in an export alike; audio is
    /// summed and does not care, which is what makes `A1` above `V1` a purely
    /// visual arrangement. A label is a position among the tracks of its kind,
    /// so a track that crossed one of its own kind comes back under a different
    /// name -- and everything holding a `(lane, idx)` is dropped exactly then,
    /// for [`Player::remove_lane`]'s reason: those handles now name another
    /// track's clip. A move that crossed only the other kind renames nothing
    /// and keeps the selection.
    fn reorder_lane(&mut self, lane: Lane, onto: Lane, cx: &mut Context<Self>) {
        self.lane_drop = None;
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(to) = session.lanes().iter().position(|&l| l == onto) else {
            return;
        };
        // Picked up and put back down where it was is a click, and a click says
        // nothing -- `move_lane` refuses it and every other no-op.
        let Some(moved) = session.move_lane(lane, to) else {
            cx.notify();
            return;
        };
        if moved != lane {
            self.selected = None;
            self.context_menu = None;
            self.eq_open = None;
            self.color_open = None;
            self.speed_open = None;
            self.close_silence();
        }
        self.notify_user(
            format!(
                "{} IS TRACK {} NOW — {} puts it back",
                moved.label(),
                to + 1,
                self.keymap.display(ActionId::Undo)
            )
            .into(),
        );
        cx.notify();
    }

    /// What the remove keys act on: the last track of that kind, which is the
    /// one the matching add key appended. Nothing at all before a file is open,
    /// where the timeline drawn is a placeholder pair.
    fn remove_last_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        let last = self.session.as_ref().and_then(|session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == kind)
                .next_back()
        });
        match last {
            Some(lane) => self.remove_lane(lane, cx),
            None => {
                self.notify_user("NO TRACK REMOVED — open a file first".into());
                cx.notify();
            }
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if self.session.as_mut().is_some_and(PlaybackSession::undo) {
            self.reset_after_reseek();
        }
        self.selected = None;
        cx.notify();
    }

    /// The stroke a waiting row was after: it becomes the whole of what reaches
    /// that action, which is what the row was showing. A chord another action
    /// already holds is refused by the keymap and the row keeps waiting, so the
    /// next stroke is another try rather than a lost one. A binding that took
    /// holds either way:
    /// what a failed write costs is only the next run, which is what the notice
    /// is for.
    fn capture(&mut self, action: ActionId, key: &str, ctrl: bool) {
        let chord = keymap::Chord {
            key: key.to_string(),
            ctrl,
        };
        // Only a stroke the file can spell and read back as itself: gpui reports
        // "+" for shift+=, which is the chord grammar's separator, so binding it
        // would write a line the next load would have to drop. Refused here, in
        // front of the user, rather than silently costing that binding later.
        // The row keeps waiting, as it does for a stroke already taken.
        if !chord.bindable() {
            let text = format!("THAT KEY CANNOT BE BOUND — {}", chord.pretty());
            eprintln!("{text}");
            self.notify_user(text.into());
            return;
        }
        let text = match self.keymap.rebind_action(action, chord.clone()) {
            Ok(()) => {
                self.rebinding = None;
                match self.keymap.save() {
                    Ok(()) => return,
                    Err(e) => format!(
                        "KEYBINDINGS NOT SAVED — {}: {e}",
                        Keymap::config_path().display()
                    ),
                }
            }
            Err(holder) => format!("ALREADY BOUND — {} is {}", chord.pretty(), holder.label()),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
    }

    /// Seeks to where the pointer sits along the ruler. `commit` is the press
    /// and the release, which must land exactly even when the throttle below
    /// would have skipped them.
    fn scrub_to(&mut self, x: Pixels, commit: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        // Clamped to the timeline here rather than in the mapping: there is bed
        // past the last frame now, and a seek out there is a seek to the end.
        let t = self
            .scale
            .time_at(px_along(x, self.ruler.get()))
            .clamp(0., session.timeline_duration());
        let target = (t * self.fps) as u32;
        if commit || scrub_due(target, self.last_target, self.last_scrub.elapsed()) {
            self.last_target = target;
            self.last_scrub = Instant::now();
            self.seek(t, cx);
        }
    }

    /// The play binding and the transport button share it: once the timeline is finished
    /// the only sensible "play" is from the top.
    /// Pushes the current volume at the session, which is the only place it is
    /// ever pushed: after a change here, and after a session arrives. A session
    /// starts at full volume, so a file opened while muted has to be told --
    /// that is the whole reason this is not just called from the key handler.
    /// Silent no-op with no timeline, or with a run that has no audio device.
    fn apply_volume(&self) {
        if let Some(session) = &self.session {
            session.set_gain(self.volume.gain());
        }
    }

    /// The mute key and the two volume keys, and the click on the button. The
    /// picture is not touched: silencing the output is not pausing it, so the
    /// clock -- which the device still drives -- runs straight through.
    fn set_volume(&mut self, change: impl FnOnce(&mut Volume), cx: &mut Context<Self>) {
        change(&mut self.volume);
        self.apply_volume();
        cx.notify();
    }

    /// Where the pointer sits along the slider, as a level. The press and every
    /// sample after it come here, so the sound follows the hand rather than the
    /// release -- there is nothing to undo about a monitoring level, which is
    /// why this writes live and keeps no gesture state beyond the flag.
    fn drag_volume(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let along = frac_along(x, self.volume_bar.get());
        self.set_volume(|volume| volume.set_along(along), cx);
    }

    /// One sample of whatever drag is in the hand: the equalizer's handle, a
    /// clip's edge, a colour bar, the speed bar, the volume slider or the
    /// playhead. Each of those starts on a strip a few pixels wide that the
    /// pointer leaves immediately, so none of them can be tracked from the
    /// element it started on -- the gesture is followed here instead, on a
    /// hitbox that covers everything the hand can reach.
    ///
    /// Registered on the root *and* on the scrim of every card that holds a
    /// slider ([`Player::drag_scrim`]). An occluding sheet ends gpui's hit test
    /// where it sits (`Hitbox::is_hovered`, window.rs:788), so while a card is
    /// up the root is not hovered anywhere under it and hears none of this: the
    /// press set a value and the drag then froze on it.
    fn drag_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A handle is 10 px across and the pointer leaves it at once, so
        // the equalizer drag is tracked here for the ruler's reason.
        if self.eq_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_band(event.position, cx);
            } else {
                // Released outside the window: the up below never came,
                // so this is where the gesture ends -- and it still owes
                // the one write the whole drag is worth.
                self.eq_dragging = false;
                self.commit_eq(cx);
            }
            return;
        }
        // A clip edge is 6 px wide and the pointer leaves it on the
        // first drag, so the gesture is tracked here for the same
        // reason -- and it ends here too when the button came up
        // outside the window, still owing its one edit.
        if self.trim.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.trim_to(event.position.x, cx),
                _ => self.commit_trim(cx),
            }
            return;
        }
        // A colour slider is 4 px tall and the pointer leaves it just as
        // fast; every sample is live, so the release owes no write of
        // its own -- what the last sample set is what the clip carries.
        if self.color_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_color(event.position.x, false, cx);
            } else {
                // The release happened outside the window, so this is
                // where the gesture ends -- and it may not end on a
                // sample the worker was too busy to take.
                self.color_dragging = false;
                self.flush_drag(cx);
            }
            return;
        }
        // The speed bar, the same 4 px and the same live writes: the
        // press took the undo step and every sample since is live.
        if self.speed_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_speed(event.position.x, false, cx);
            } else {
                self.speed_dragging = false;
                self.flush_drag(cx);
            }
            return;
        }
        // The volume slider, the same live writes: what the hand is on
        // is what the speakers are doing, and there is nothing to undo.
        if self.volume_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_volume(event.position.x, cx);
            } else {
                self.volume_dragging = false;
            }
            return;
        }
        if !self.scrubbing {
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            self.scrub_to(event.position.x, false, cx);
        } else {
            // A release outside the window never reaches the handler
            // below, so the first button-up move is when we learn the
            // drag is over. Without this the next hover would scrub.
            self.scrubbing = false;
        }
    }

    /// Where a drag ends: the release lands exactly, and whatever the gesture
    /// owes -- one undo step for the equalizer and the trim, a flush for the
    /// live-writing bars -- is paid here. On the root and on a card's scrim
    /// both, for [`Player::drag_move`]'s reason: a release over an open card
    /// never reaches the root.
    fn drag_release(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.eq_dragging) {
            // The release lands exactly, then the gesture is written
            // once -- the append-only table's whole reason.
            self.drag_band(event.position, cx);
            self.commit_eq(cx);
            return;
        }
        if self.trim.is_some() {
            // The release lands exactly, then the gesture is
            // written once -- one edit, one undo step.
            self.trim_to(event.position.x, cx);
            self.commit_trim(cx);
            return;
        }
        if std::mem::take(&mut self.color_dragging) {
            // The release lands exactly where the hand let go, and
            // it is a live write like every other sample: the undo
            // step the gesture rolls back to was the press's. The
            // flush is what makes "exactly" true while the worker is
            // still busy -- the sample above would only be held.
            self.drag_color(event.position.x, false, cx);
            self.flush_drag(cx);
            return;
        }
        if std::mem::take(&mut self.speed_dragging) {
            self.drag_speed(event.position.x, false, cx);
            self.flush_drag(cx);
            return;
        }
        if std::mem::take(&mut self.volume_dragging) {
            self.drag_volume(event.position.x, cx);
            return;
        }
        if std::mem::take(&mut self.scrubbing) {
            self.scrub_to(event.position.x, true, cx);
        }
    }

    fn toggle_or_restart(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // Nothing to play is a message, not a transport state. An empty
        // timeline is [`Transport::Ended`] from its one black frame onward, so
        // the restart below would start a clock against a zero-length timeline
        // -- and it is Ended again by the next repaint, so no later press could
        // ever stop it: the button would read "Pause" and never pause. A delete
        // can empty the timeline mid-play, and that press must still stop it.
        if nothing_to_play(self.session.as_ref()) {
            match self.session.as_mut().filter(|s| s.is_playing()) {
                Some(session) => session.pause(),
                None => self.notify_user(NOTHING_TO_PLAY.into()),
            }
            cx.notify();
            return;
        }
        // Pressing play is asking to watch, so a view scrolled away while
        // paused comes back to the head with it -- as a seek's does.
        self.panned = false;
        match self.transport() {
            // Nothing open: the button is dimmed and the key says nothing.
            Transport::Stopped => {}
            // Back to the top and away, for the key and the button alike --
            // whichever asked, the transport was showing Play.
            state if state.restarts() => {
                self.seek(0., cx);
                if let Some(session) = &mut self.session {
                    session.play();
                }
            }
            _ => {
                if let Some(session) = &mut self.session {
                    session.toggle();
                    // A paused timeline animates nothing; this is the repaint
                    // that puts the new glyph up.
                    cx.notify();
                }
            }
        }
    }

    /// The export that owns the UI, if any. A cancelled one does not: it has
    /// its own copy of the edit list and owes only its own cleanup.
    fn exporting(&self) -> Option<&ExportHandle> {
        self.export.as_ref().filter(|_| !self.cancelling)
    }

    /// What the export action does now: opens the card, which is where the
    /// quality, the destination and the decision to write at all are. Nothing
    /// is encoded until the button in it is pressed.
    fn open_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        // Nothing to write out, and a refusal rather than a card about it: the
        // window is empty and the export path is not even chosen yet.
        if self.session.is_none() {
            self.notify_user("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        }
        self.export_open = true;
        // One card at a time, and a waiting row must not outlive the card it
        // was waiting in. Nor may a half-typed number: the card opens on the
        // bitrate it will write, never on digits left behind by a closed one.
        self.keys_open = false;
        self.rebinding = None;
        self.mbps_edit = None;
        cx.notify();
    }

    /// A format row was clicked. The destination follows it at once -- a WAV
    /// written to a path ending in `.mp4` is a file every player will lie
    /// about -- keeping whatever stem the save dialog last left there.
    fn set_format(&mut self, format: Format) {
        // The one door both the row and its initial go through, so a format the
        // card greys out cannot be picked by keyboard either.
        if let Some(why) = self
            .session
            .as_ref()
            .and_then(|session| format_refusal(session, format))
        {
            self.notify_user(format!("NOT {} — {why}", format_label(format)).into());
            return;
        }
        self.format = format;
        self.export_path = retarget(&self.export_path, format);
    }

    /// The container row: the same codec in the other box, which retargets the
    /// destination exactly as picking a codec does -- and does nothing at all
    /// for a codec with only one box, so the stroke cannot invent a choice the
    /// card is not offering.
    fn cycle_container(&mut self) {
        self.set_format(next_container(self.format));
    }

    /// The quality rows by keyboard, wrapping. Refused by name where the format
    /// has no bitrate to pick: a key that silently does nothing is the card
    /// looking broken.
    fn cycle_quality(&mut self) {
        if let Some(why) = bitrate_refusal(self.format) {
            self.notify_user(why.into());
            return;
        }
        let at = Quality::ALL
            .iter()
            .position(|&q| q == self.quality)
            .unwrap_or(0);
        self.quality = Quality::ALL[(at + 1) % Quality::ALL.len()];
    }

    /// The sound's rate by keyboard, wrapping through the offered ones -- the
    /// picture's quality row for the other half of the file. Refused by name
    /// where this timeline in this format has no rate to pick, exactly as
    /// [`Player::cycle_quality`] is: a key that silently does nothing is the
    /// card looking broken.
    fn cycle_audio_kbps(&mut self) {
        if let Some(why) = self.audio_rate_refusal() {
            self.notify_user(why.into());
            return;
        }
        self.audio_kbps = next_audio_kbps(self.audio_kbps);
    }

    /// Why the sound row is not a choice right now, the engine answering about
    /// the very project it would export. No session is the same answer as no
    /// sound: there is nothing to write either way.
    fn audio_rate_refusal(&self) -> Option<&'static str> {
        match &self.session {
            Some(session) => session.audio_rate_refusal(self.format),
            None => Some("no sound to write"),
        }
    }

    /// The custom bitrate by pointer: the typed digits were the only control in
    /// this card a mouse could not reach. Clamped to the range the row states
    /// (the engine's own 1..50 Mbps), and picking the row is part of the step --
    /// a stepper that moves a number nobody is using would move nothing.
    fn nudge_mbps(&mut self, step: i32) {
        self.custom_mbps =
            (self.custom_mbps as i32 + step).clamp(MBPS_MIN as i32, MBPS_MAX as i32) as u32;
        self.quality = Quality::Custom;
    }

    /// The same number under the wheel, one step a notch, up for more: fifty
    /// presses of a stepper is not a way to reach the top of this range, and the
    /// wheel is what this editor already moves a value with (the timeline's
    /// zoom and scroll are the same gesture). Hold-to-run stays the keyboard's,
    /// as it is on every other card here -- a button that repeats while held is
    /// not a thing this program has.
    ///
    /// It moves the *field* while one is open, exactly as ↑↓ do, so the two
    /// ways in never disagree about which number is being changed.
    fn wheel_mbps(&mut self, event: &ScrollWheelEvent) {
        let by = wheel_delta(event);
        if by == 0. {
            return;
        }
        let by = by.signum() as i32;
        match &mut self.mbps_edit {
            Some(edit) => edit.step(by),
            None => self.nudge_mbps(by),
        }
    }

    /// Opens the custom bitrate's field on the number the row is carrying, and
    /// picks the row while it is at it: a field typed into is the row being
    /// chosen, and a number nobody is using would be a number typed at nothing.
    /// Nothing is committed here -- until enter, the card still exports at the
    /// bitrate it had.
    fn edit_mbps(&mut self) {
        self.quality = Quality::Custom;
        self.mbps_edit = Some(NumberEdit::new(self.custom_mbps));
    }

    /// The card's Destination row: the desktop's save dialog, on a background
    /// thread like the import chooser -- the user may sit in it and the window
    /// behind must not freeze. No chooser at all leaves the default path, which
    /// is what the refusal says.
    fn pick_destination(&mut self, cx: &mut Context<Self>) {
        let default = self.export_path.clone();
        let picked = cx
            .background_executor()
            .spawn(async move { pick_save(&default) });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| {
                // The dialog outlives the card: an export started meanwhile
                // took the old path and its notice must name what it wrote.
                if this.export.is_some() {
                    return;
                }
                match picked {
                    // The stem is the user's, the extension is the format's: a
                    // FLAC named `.mp4` is a file every player lies about.
                    Ok(Some(path)) => this.export_path = retarget(&path, this.format),
                    // Cancelled: the default stands, as it did before.
                    Ok(None) => {}
                    Err(text) => {
                        eprintln!("{text}");
                        this.notify_user(text.into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The subtitle tracks an export of this timeline carries: every one with a
    /// cue left in the exported range ([`PlaybackSession::timeline_cues`], the
    /// very map the file is written from), in the library's own order.
    ///
    /// Worked out from the cues each time rather than kept as a pick, which is
    /// what makes it impossible to desync: a row added or taken off shifts every
    /// index after it, and a stored list would then name tracks nobody chose.
    /// `Player::sub_track` stays what it always was -- which track the *overlay*
    /// draws -- and has no say here.
    ///
    /// The honest input and not the final answer: the engine filters it again
    /// per track (a track that could not be read, a picture one) and says so in
    /// the card's own words ([`engine::export::planned_subtitles`]).
    fn export_subs(&self) -> Vec<usize> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        (0..session.subtitles().len())
            .filter(|&i| !session.timeline_cues(i).is_empty())
            .collect()
    }

    /// That list in the card's words ([`subtitle_plan`]): what travels, and the
    /// reason beside every track that does not -- including the ones
    /// [`Self::export_subs`] filtered out before the engine ever saw them.
    fn subtitle_line(&self) -> String {
        let Some(session) = self.session.as_ref() else {
            return "none".to_string();
        };
        let picks = self.export_subs();
        let plan = session.planned_subtitles(self.format, picks.iter().copied());
        match self.format.has_video() {
            true => subtitle_plan(plan, session.subtitles(), &picks),
            // A format that is the sound alone has nowhere to put any of them
            // and the engine says that once, about the file. Naming the cues of
            // each track under it answers a question the format already closed.
            false => plan,
        }
    }

    /// Writes the edit list out, at the settings the card was left at. Playback
    /// stops first: the exporter opens its own decoder -- and, on the hardware
    /// path, an encoder -- so a running player would only compete with it for
    /// the GPU. A cancelled export still winding down holds this off for the
    /// frame it takes to notice, which is what keeps its `remove_file` off the
    /// new output.
    fn start_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        let mut settings =
            export_settings(self.quality, self.custom_mbps, self.format, self.audio_kbps);
        // Whatever is on the timeline travels -- every track with a cue in the
        // exported range, not the one row the overlay happens to be drawing.
        // Set here rather than inside `export_settings`, which the card also
        // calls for the *estimate* and which nothing else needs a subtitle for.
        settings.subtitles = self.export_subs();
        let Some(session) = &mut self.session else {
            self.notify_user("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        };
        // An emptied timeline is a timeline; it is simply not a file. Refused by
        // name here rather than written as a project of no frames -- and the
        // engine refuses it again on the worker (`export::start`), so a caller
        // that is not this button cannot get past it either. Two fences on
        // purpose: this one is the one with a keystroke to blame.
        if session.is_empty() {
            self.notify_user("NOTHING TO EXPORT — the timeline is empty".into());
            cx.notify();
            return;
        }
        // The format row can be refused *after* it was picked -- mp4 is the
        // default and an audio-only timeline (or a second audio lane) is one
        // edit away -- so the button asks again rather than starting a worker
        // that will only settle with the same refusal minutes later.
        if let Some(why) = format_refusal(session, self.format) {
            self.notify_user(format!("NOT EXPORTED — {why}").into());
            cx.notify();
            return;
        }
        session.pause();
        self.export = Some(session.export_to_with(&self.export_path, &settings));
        // The clock starts with the worker, not with the first repaint that
        // happens to notice it.
        self.export_started = Some(Instant::now());
        self.export_marks.clear();
        // The card has been answered; the progress line takes the panel from
        // here, and it is the running export's escape that matters now.
        self.export_open = false;
        cx.notify();
    }

    /// Gives the editor back at once and leaves the worker to stop at its next
    /// frame and delete what it has written.
    fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
            self.cancelling = true;
        }
    }

    /// Takes the export's verdict once it has one. The only place the app
    /// touches the handle's completion side.
    fn poll_export(&mut self) {
        // Sampled here rather than while drawing: a repaint stays a repaint,
        // and this runs once per repaint either way.
        if let (Some(progress), Some(started)) = (
            self.exporting().map(ExportHandle::progress),
            self.export_started,
        ) {
            note_progress(
                &mut self.export_marks,
                started.elapsed().as_secs_f32(),
                progress,
            );
        }
        let Some(result) = self.export.as_ref().and_then(ExportHandle::result) else {
            return;
        };
        self.export = None;
        self.export_started = None;
        // A cancellation is reported as an error, and the one who asked for it
        // has had the editor back since the keystroke. Nothing to say.
        if std::mem::take(&mut self.cancelling) {
            return;
        }
        let text = match result {
            Ok(()) => {
                // Written and still where it was written: the bar carries it
                // until some other notice takes the bar.
                self.exported = Some(self.export_path.clone());
                format!("{EXPORT_DONE}{}", file_name(&self.export_path))
            }
            Err(e) => format!("EXPORT FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
    }
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
