//! The lists the window opens over itself: the clip menu, the pickers, the key rows.

use crate::*;

/// What a right-click on the bench named: a clip at an index, or the empty
/// stretch of the lane it landed in ([`engine::PlaybackSession::gap_at`]) --
/// two different things the one menu can be about, so it stays one struct and
/// one field to clear rather than a second `Option` beside `context_menu`
/// that every closer would have to learn about too.
#[derive(Clone, Copy)]
pub(crate) enum MenuOn {
    Clip(usize),
    /// `(start, frames)` of the gap, in timeline frames -- what
    /// [`Player::close_gap`] ripples shut.
    Gap(u32, u32),
    /// The bench itself: the ruler, or the empty stretch below the last lane.
    /// Nothing under the pointer is a clip, so what the menu offers is what
    /// the *timeline* can be told -- walking its cuts, its zoom, its snap and
    /// the undo pair ([`BENCH_ITEMS`]). The lane field means nothing here and
    /// no row reads it.
    Bench,
    /// An empty stretch of a lane's bed with no gap to close: Paste and the two track adds
    /// ([`empty_items`]). The lane names the track the stretch is on.
    Empty,
    /// A lane head: the verbs of the track itself, on the lane the menu
    /// already names ([`oracle::lane_items`]).
    Head,
    /// The dock's Sources body, anywhere its rows are not: the ways *in*
    /// ([`DOCK_ITEMS`]). Cleanse round 2 took `Paste path` and `Import
    /// subtitles` out of the footer, and this is the door they kept -- an
    /// empty library included, which is exactly when a pasted path is worth
    /// most. The lane field means nothing here and no row reads it.
    Dock,
    /// The viewer's picture: what the picture can be told ([`VIEWER_ITEMS`]).
    /// The lane field means nothing here and no row reads it.
    Viewer,
    /// A sequence tab: the verbs of the sequence ([`SEQ_TAB_ITEMS`]); the tab was opened first,
    /// so they act on the open sequence.
    SeqTab,
    /// A bin row in the media list: the verbs of the bin ([`BIN_ITEMS`]); the bin was picked
    /// first, so they act on it.
    Bin,
    /// A comment marker on the ruler: its verbs ([`MARKER_ITEMS`]); the marker was selected
    /// first, so they act on it.
    Marker,
}

/// An open clip menu: what it was opened on, where it hangs, and whether it
/// has been turned over to show what a clip *is* instead of what can be done
/// to it (`details` means nothing for a gap, which has no properties side).
/// The lane is the one the same click selected, so every item acts on
/// exactly the box -- or the hole -- under the pointer.
#[derive(Clone, Copy)]
pub(crate) struct ContextMenu {
    pub(crate) lane: Lane,
    pub(crate) on: MenuOn,
    pub(crate) at: Point<Pixels>,
    pub(crate) details: bool,
    /// The submenu out beside the plate, if any. One at a time.
    pub(crate) sub: Sub,
}

/// Which submenu of a clip menu is out beside the plate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sub {
    Closed,
    /// The card openers.
    Adjust,
    /// Nest, break apart, open, flatten.
    Compound,
    /// The colour list of the clip.
    Label,
    /// The fit policies of the clip.
    Fit,
}

/// An open library menu: which row it was opened on -- the file and the stream,
/// the pair [`Player::selected_asset`] holds, so a list rebuilt under it (a
/// probe landing, a source going) cannot slide another row beneath the menu the
/// way a row *index* would -- where it hangs, and whether it has been turned
/// over to show what the file *is*.
#[derive(Clone)]
pub(crate) struct LibraryMenu {
    pub(crate) path: PathBuf,
    pub(crate) stream: usize,
    pub(crate) at: Point<Pixels>,
    pub(crate) details: bool,
}

/// An open choice list: which setting it offers and where it hangs. What a
/// button that stepped one value on per click used to be -- a setting with more
/// than two values is a list to look at, not a thing to click round. Placed by
/// [`menu_at`] and closed by a stroke, exactly like the two menus above it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Picker {
    pub(crate) of: Pick,
    pub(crate) at: Point<Pixels>,
    /// Which row the keyboard is on, from the value in force when the list
    /// opened. The list answers ↑↓ and enter as well as a click: a setting whose
    /// only door is a pointer is a setting half this editor's users cannot
    /// reach, which is the rule `FIXED` already writes down for every card.
    pub(crate) sel: usize,
    /// Whether the list keeps off the picture and timeline (a list opened from the menu bar's
    /// own corner or the status bar) or hangs from the foot of the dock button that opened it.
    pub(crate) floored: bool,
    /// For a list hung from a recorded control ([`crate::ui::anchor`]): the control's top edge,
    /// where the plate's bottom goes when there is no room below.
    pub(crate) above: Option<f32>,
}

/// Which setting an open list is offering. The fit policy names the clip it is
/// about, like the clip menu it opens from -- indices move under every edit, so
/// the list closes on the first stroke as that menu does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pick {
    Resolution,
    Fps,
    /// The rate the project's own mix runs at
    /// ([`engine::PlaybackSession::set_sample_rate`]). Opened from the panel,
    /// beside the resolution and rate it shares a row style with.
    SampleRate,
    Fit(Lane, usize),
    /// The colour label of the clip at (lane, index) ([`engine::project::LABELS`]).
    Label(Lane, usize),
    /// The easing of the key on the playhead, for one kind of key.
    KeyEase(engine::project::KeyKind),
    /// The viewer's zoom, playback quality and aspect mask: lists on the viewer header.
    ViewZoom,
    OutputDisplay,
    ViewQuality,
    ViewMask,
    ViewGuides,
    /// The colour of marker `n`, from the markers card's swatch.
    MarkerColor(usize),
    /// What the export's *sound* is coded at. Opened from the card's Sound row,
    /// which is the only place it means anything.
    #[allow(dead_code)] // opened from Settings now
    AudioRate,
    /// Which HDR-to-SDR rendition the project is watched and exported in
    /// ([`engine::tonemap::Preset`]). Opened from the panel, beside the two
    /// other settings that are the project's rather than the media's.
    #[allow(dead_code)] // three chips on the settings page now
    Tone,
    /// Which palette the window is painted in ([`ui::theme`]). The one setting
    /// here that is nobody's project: it is the person's, so it outlives the
    /// timeline and every file opened in it.
    Theme,
    /// Which accent colour the window wears ([`ui::theme::AccentId`]); the person's, like [`Pick::Theme`].
    Accent,
    /// Which encoder an export writes the picture with
    /// ([`engine::export::EncoderSeat`]). Opened from the card's Encoder row,
    /// which is the only place it means anything.
    #[allow(dead_code)] // opened from Settings now
    Encoder,
    /// Which file an export is written as -- the codec and its box in one
    /// list ([`ui::settings_stance::picture_choices`]). Opened from the
    /// settings page's Picture row: nine answers are too many for chips.
    Picture,
    /// A Details row of the Export tab (preset, container, codec, ...).
    Export(crate::export_ui::ExportRow),
    /// The bin the picked media are filed in; the list offers every bin and "No bin".
    Bin,
    /// How often the session is autosaved beside its file, from Settings ([`player::library::AUTOSAVE_CHOICES`]).
    Autosave,
}

/// One value a list offers, carrying everything picking it needs -- so a click
/// goes straight to the value rather than to a position in a list that was
/// built somewhere else.
/// `Eq` is off it for the rate: a frame rate is the `f64` the engine is told,
/// bit for bit (23.976023976... is not 23.976), and nothing here keys a map on a
/// choice -- comparing two is all a list row ever does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Choice {
    Size(u32, u32),
    Fps(f64),
    /// `None` is "source" -- the rate derived from the sources (the highest
    /// among them, 48 kHz when all are below 44.1 kHz),
    /// [`engine::PlaybackSession::set_sample_rate`]'s own default.
    SampleRate(Option<u32>),
    Fit(Lane, usize, FitPolicy),
    /// A label number for the clip: 0 none, else 1..=[`engine::project::LABEL_COUNT`].
    Label(Lane, usize, u8),
    KeyEase(engine::project::KeyKind, engine::transform::KeyEase),
    ViewZoom(crate::viewer_view::Zoom),
    /// An index into [`Player::output_displays`].
    OutputDisplay(usize),
    ViewQuality(crate::viewer_view::Quality),
    ViewMask(crate::viewer_view::Mask),
    ViewGuides(crate::viewer_view::Guide),
    MarkerColor(usize, engine::project::MarkerColor),
    AudioRate(u32),
    Tone(Preset),
    Theme(ui::theme::PaletteId),
    Accent(ui::theme::AccentId),
    Encoder(EncoderSeat),
    Picture(Format),
    /// A Details row of the Export tab ([`Pick::Export`]).
    Export(crate::export_ui::ExportPick),
    /// File the picked media in bin `n` (`0`: out of every bin) ([`Pick::Bin`]).
    Bin(u32),
    /// The autosave period in seconds, `None` for Off ([`Pick::Autosave`]).
    Autosave(Option<u64>),
}

/// A status-bar item a shortcut-opened list hangs above.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LedgerItem {
    Size,
    Clip,
}

/// One row of an open list: the value, its name, the small print beside it, and
/// whether it is the one in force.
pub(crate) type ChoiceRow = (Choice, SharedString, SharedString, bool);

/// What a library row's menu offers, in the order it lists them. Unlike the clip
/// menu's items only one of these ([`RowItem::Add`]) is a stroke, and that one
/// is not a bindable [`ActionId`] but `ui/stance.rs`'s own `enter` branch, so
/// the label and the chord are written here rather than read off the keymap.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RowItem {
    /// Point a missing file at its new place ([`Player::relink_pick`]). Listed
    /// only on a row whose file is gone.
    Relink,
    /// Put the source on the end of the timeline, after the last clip on any lane
    /// ([`Player::append_source`]). The default: the first row, and what Enter does.
    Add,
    /// Insert the source at the playhead, pushing the clips after it right
    /// ([`Player::insert_source`] with no lane and no position).
    Insert,
    /// Play the source outside the timeline ([`Player::open_preview`]). The
    /// row's `▷` ghost until cleanse round 2 (2026-09-10); a double-click on
    /// the row is the other door.
    Preview,
    /// Make or stop the source's stand-in ([`Player::toggle_proxy`]) -- the
    /// row's `○` ghost until the same cleanse. Settings carries the policy;
    /// this is the one file. Reads "Stop editing copy" while one is being made.
    Proxy,
    /// Makes a multicam clip from the picked rows ([`Player::create_multicam_selected`]).
    /// Refused until two or more are picked, saying how.
    CreateMulticam,
    /// Take the source out of the library. Refused while any clip plays it
    /// (the engine's own rule), and destructive, so it sits below the rule.
    Remove,
    /// Remove that first deletes every clip playing the row. It is listed
    /// always and refused with a reason while nothing plays the file, so the
    /// pointer can see the way out exists; destructive, so it sits below the
    /// rule and says how many clips it takes with it.
    RemoveWithClips,
    Reveal,
    Properties,
    /// Stars the source, or takes the star off ([`Player::toggle_favourite`]).
    Favourite,
    /// Opens the list of bins to file the source in ([`Pick::Bin`]).
    MoveToBin,
    /// Makes a bin holding this source, named in place ([`Player::new_bin`]).
    NewBin,
}

/// Menu rows in the order the Media row menu lists them (DESIGN §6.7). The two
/// destructive rows are last, under the rule line ([`RowItem::destructive`]).
pub(crate) const ROW_ITEMS: [RowItem; 13] = [
    RowItem::Relink,
    RowItem::Add,
    RowItem::Insert,
    RowItem::Preview,
    RowItem::Proxy,
    RowItem::CreateMulticam,
    RowItem::Favourite,
    RowItem::MoveToBin,
    RowItem::NewBin,
    RowItem::Reveal,
    RowItem::Properties,
    RowItem::Remove,
    RowItem::RemoveWithClips,
];

impl RowItem {
    /// The icon the row wears in the media menu's icon column.
    pub(crate) fn icon(self) -> crate::ui::icons::Icon {
        use crate::ui::icons::Icon as I;
        match self {
            Self::Relink => I::Link,
            Self::Add | Self::Insert => I::Plus,
            Self::Preview => I::Play,
            Self::Proxy => I::Gauge,
            Self::Remove | Self::RemoveWithClips => I::Trash2,
            Self::Reveal => I::FolderOpen,
            Self::Properties => I::Info,
            Self::Favourite => I::Star,
            Self::MoveToBin | Self::NewBin => I::Folder,
            Self::CreateMulticam => I::Layers,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Relink => "Relink…",
            Self::Add => "Append to timeline end",
            Self::Insert => "Insert at playhead",
            Self::CreateMulticam => "Create multicam…",
            Self::Preview => "Open in source viewer",
            Self::Proxy => "Make editing copy",
            Self::Remove => "Remove from library",
            // The consequence is in the label now rather than in the hint
            // column beside a label identical to the plain remove's: that
            // column carries chords ([`RowItem::hint`]), and a destructive
            // verb whose difference from the row above it lived in dim grey
            // prose is a verb that reads as the same verb. It fits `MENU_W`
            // whole exactly because nothing is printed to its right.
            Self::RemoveWithClips => "Remove with its clips",
            Self::Reveal => "Reveal in files",
            Self::Properties => "Properties",
            Self::Favourite => "Favourite",
            Self::MoveToBin => "Move to bin…",
            Self::NewBin => "New bin",
        }
    }

    /// The label this row wears in `ctx`: the one state-dependent label, the
    /// proxy row's, reads "Stop editing copy" while a stand-in is being made.
    pub(crate) fn label_in(self, ctx: RowCtx) -> &'static str {
        match self {
            Self::Proxy if ctx.proxy_stops => "Stop editing copy",
            Self::Favourite if ctx.starred => "Remove from favourites",
            _ => self.label(),
        }
    }

    /// Whether the row takes something away -- the rows below the rule line
    /// ([`destructive`] is the same answer for the clip menu).
    pub(crate) fn destructive(self) -> bool {
        matches!(self, Self::Remove | Self::RemoveWithClips)
    }

    /// The dim right-hand column: the *stroke* that does the same thing, the
    /// one thing DESIGN §4 puts there ("every command wears its chord"), and
    /// nothing at all where no stroke reaches the verb.
    ///
    /// It used to carry a prose gloss on every row -- a phrase about the whole
    /// file, one about the desktop's file browser, one about clips -- which is
    /// §8's instructional copy sitting in the column the sibling clip menu
    /// spends on chords, and which the user read as the crowding it is. A
    /// refusal still prints here in place of the chord (`oracle::row_enable`)
    /// -- that is state, not instruction.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::Add | Self::Insert | Self::CreateMulticam => "",
            Self::Preview => "↵",
            Self::Relink
            | Self::Proxy
            | Self::Remove
            | Self::RemoveWithClips
            | Self::Reveal
            | Self::Properties
            | Self::Favourite
            | Self::MoveToBin
            | Self::NewBin => "",
        }
    }
}

/// What the menu offers, in the order it lists them. Every one of these is an
/// action a stroke already reaches -- the menu is a second way *to* the actions
/// and never a second version of them -- so both the label and the hint come
/// out of the keymap registry and the two can never disagree.
pub(crate) const MENU_ITEMS: [ActionId; 31] = [
    ActionId::Cut,
    // The cut machinery (DESIGN.md §6) on the very clip it is about: the
    // trims and the loop-trim were strokes the spine rail listed and nothing
    // else did, and the subject cut a right-click names is exactly the one
    // they act on. Their trim-to-playhead pair is *not* here: `^[` and `^]`
    // are the keyboard's own version of a gesture the pointer already has --
    // drag the edge to where you want it -- so a row for them in the menu the
    // pointer opens is the junk drawer §9 forbids. They keep their chords and
    // their KEYS rows.
    ActionId::TrimIn,
    ActionId::TrimOut,
    ActionId::LoopTrim,
    // The clipboard pair, which had no door but a chord: copy takes the clip
    // the menu names, and paste is the timeline's rather than this clip's.
    ActionId::Copy,
    ActionId::Paste,
    // The clip's colour label and its on/off switch (Premiere's Label and Enable).
    ActionId::ClipLabel,
    ActionId::ClipEnabled,
    // The two joins, one per kind, each hidden over the other kind
    // (`oracle::enable`, DESIGN §8's class refusal): sound crossfades, picture
    // dissolves, and neither is ever offered over the wrong waveform.
    ActionId::Crossfade,
    ActionId::Dissolve,
    // The picture cards...
    ActionId::Color,
    ActionId::Transform,
    ActionId::Effects,
    // ...and the sound ones, the same class refusal the other way round.
    ActionId::Equalizer,
    ActionId::Silence,
    ActionId::MarkBeats,
    ActionId::Visualizer,
    ActionId::Reframe,
    ActionId::TrackMask,
    ActionId::Speed,
    ActionId::Reverse,
    ActionId::Fit,
    // The compound clip's three verbs, folded into one "Compound" submenu like the cards.
    ActionId::NestClips,
    ActionId::BreakApart,
    ActionId::OpenCompound,
    ActionId::FlattenMulticam,
    // The group trio used to live only on chords + ctrl-click. A right-click
    // that cannot group is a missing pointer door (the same class Fit and
    // Redo shipped as). Dimmed in place when the selection cannot.
    ActionId::Group,
    ActionId::Detach,
    ActionId::Regroup,
    // Last, under the rule line the render draws before the first of them
    // (DESIGN §9): the two that take the clip off the lane.
    ActionId::Delete,
    ActionId::Lift,
];

// Mute stays off this list: it is the mix's, which the transport already
// carries a button for.

/// What a right-click on the bench itself offers -- the ruler and the empty
/// stretch under the lanes. The verbs of the *timeline*: walking its cuts at
/// either stride (DESIGN §6's odometer), what it is looked at through, and the
/// undo pair, which is the one thing every editor expects on empty canvas.
/// Not a junk drawer (§9): nothing here acts on a clip, and everything here
/// acts on the thing the pointer is over.
pub(crate) const BENCH_ITEMS: [ActionId; 22] = [
    // The two that act on the frame the pointer named, first and in this order: a marker
    // there, then the playhead there (the ruler's own verbs, not the playhead's).
    ActionId::MarkerHere,
    ActionId::PlayheadHere,
    // Where the playhead goes, then the range it marks: the ruler is the
    // strip a person clicks to seek, so its menu leads with the seek pair and
    // the in/out pair that frames a range on it. Nothing here is destructive
    // (`ClearRange` only lets go of the marks), so the list still ends on the
    // undo pair and the rule-free tail.
    ActionId::GoStart,
    ActionId::GoEnd,
    ActionId::SetIn,
    ActionId::SetOut,
    ActionId::ClearRange,
    // The `all` control left the ruler's right edge (cleanse round 2): this
    // row is where the pointer reaches "select everything" now, and `^a`
    // still fires it.
    ActionId::SelectAll,
    ActionId::Paste,
    ActionId::WalkCutPrev,
    ActionId::WalkCutNext,
    ActionId::WalkCutPrev10,
    ActionId::WalkCutNext10,
    // The comment markers: drop one at the playhead, walk to its neighbours, or open the
    // list -- the ruler is where they are drawn, so its menu is where they are reached.
    ActionId::PrevMarker,
    ActionId::NextMarker,
    ActionId::Markers,
    ActionId::ZoomOut,
    ActionId::ZoomIn,
    ActionId::ZoomFit,
    ActionId::ToggleSnap,
    ActionId::Undo,
    ActionId::Redo,
];

/// What a right-click on the viewer offers: the things the picture itself
/// answers to -- play it, loop it, keep the frame, go full screen -- and the
/// way to Settings. Every one is an [`ActionId`] the keyboard already reaches.
pub(crate) const VIEWER_ITEMS: [ActionId; 5] = [
    ActionId::Play,
    ActionId::Loop,
    ActionId::Screenshot,
    ActionId::Fullscreen,
    ActionId::Settings,
];

/// What a right-click on a sequence tab offers.
pub(crate) const SEQ_TAB_ITEMS: [ActionId; 3] =
    [ActionId::RenameSequence, ActionId::DuplicateSequence, ActionId::DeleteSequence];

/// What a right-click on a comment marker on the ruler offers: the marker was selected first,
/// so these act on it ([`Player::rename_marker_at_pick`], [`Player::delete_marker_at_pick`]).
pub(crate) const MARKER_ITEMS: [ActionId; 2] = [ActionId::RenameMarker, ActionId::DeleteMarker];

/// What a right-click on a bin row offers.
/// The media row's own doors for `ActionId::ToggleFavourite` (Favourite) and
/// `ActionId::MoveToBin`; the dock's All / Favourites control is the door for
/// `ActionId::ShowFavourites` and `ActionId::ShowAllMedia`.
pub(crate) const BIN_ITEMS: [ActionId; 3] = [ActionId::RenameBin, ActionId::NewBinInside, ActionId::RemoveBin];

/// What a right-click on the dock's Sources body offers: the three ways a
/// file gets in. Every one is an [`ActionId`] the keyboard already reaches,
/// so the row's label and its chord come out of the registry -- the dock
/// itself only draws one of them (`Add`) as a row of its own now.
pub(crate) const DOCK_ITEMS: [ActionId; 13] = [
    ActionId::AddFiles,
    ActionId::PasteFilePath,
    ActionId::ImportSubtitles,
    ActionId::NewBin,
    ActionId::CreateMulticam,
    ActionId::NewSolid,
    ActionId::NewGradient,
    ActionId::NewCountdown,
    ActionId::NewBars,
    ActionId::NewTone,
    ActionId::NewBarsAndTone,
    ActionId::NewAdjustment,
    ActionId::NewTitle,
];

/// The tracks a lane head's menu can add, in the order it lists them. Its
/// remove is the one of the clicked lane's own kind and comes from
/// [`oracle::lane_items`], which is what puts it under the rule line.
/// What an empty stretch of a bed offers with no gap to close: the clipboard's paste and the
/// two track adds, whichever track it is on.
pub(crate) fn empty_items(_lane: Lane) -> Vec<ActionId> {
    vec![ActionId::Paste, ActionId::AddVideoLane, ActionId::AddAudioLane]
}

pub(crate) const HEAD_ADDS: [ActionId; 3] = [
    ActionId::AddVideoLane,
    ActionId::AddAudioLane,
    ActionId::AddSubtitleLane,
];

/// Whether a menu row takes something away -- what DESIGN §9 puts below a rule
/// line, wherever the row is listed. One answer, so the clip menu and the lane
/// head's menu cannot come to disagree about which of their rows is the
/// dangerous one.
/// The card openers a clip's menu folds into its one "Adjust" submenu, so the
/// top level keeps the high-frequency rows and always fits the window.
pub(crate) fn adjust_item(action: ActionId) -> bool {
    matches!(
        action,
        ActionId::Color
            | ActionId::Transform
            | ActionId::Effects
            | ActionId::Equalizer
            | ActionId::Silence
            | ActionId::MarkBeats
            | ActionId::Visualizer
            | ActionId::Reframe
            | ActionId::TrackMask
            | ActionId::Speed
    )
}

/// The compound-clip verbs a clip's menu folds into its one "Compound" submenu.
pub(crate) fn compound_item(action: ActionId) -> bool {
    matches!(
        action,
        ActionId::NestClips | ActionId::BreakApart | ActionId::OpenCompound | ActionId::FlattenMulticam
    )
}

pub(crate) fn destructive(action: ActionId) -> bool {
    matches!(
        action,
        ActionId::Delete
            | ActionId::Lift
            | ActionId::RemoveVideoLane
            | ActionId::RemoveAudioLane
            | ActionId::RemoveSubtitleLane
            | ActionId::DeleteSequence
            | ActionId::RemoveBin
    )
}

/// One row of the actions card, in the order it lists them: a heading, then
/// every action the registry files under it, then the strokes the modal cards
/// answer themselves.
///
/// A list rather than a loop inside the render, so the card and
/// `every_action_is_on_the_actions_card` read the *same* order: an action that
/// reaches no row fails a test instead of quietly becoming pointer-unreachable.
#[derive(Clone, Copy, Debug)]
pub(crate) enum KeyRow {
    Head(keymap::Category),
    /// Click its label to do it, click its stroke to change that stroke.
    Act(ActionId),
    /// An index into [`keymap::FIXED`]. Shown and never offered: nothing may
    /// unbind the way out of a card.
    Fixed(usize),
}

/// Every action, under its heading, and the card-local strokes beside them.
/// Generated from the registry -- [`ActionId::ALL`] in its own order, under
/// [`keymap::Category::ALL`] -- so an action added there is on the card the
/// moment it exists and there is no second list here to forget.
pub(crate) fn keys_rows() -> Vec<KeyRow> {
    let mut rows = Vec::new();
    for category in keymap::Category::ALL {
        rows.push(KeyRow::Head(category));
        // The nine angle keys are one row: Angle1 is labelled "angle 1–9" and
        // the other eight are reached by the same number keys, so they stay in
        // the registry (and keep their keys) but get no row of their own.
        rows.extend(
            ActionId::ALL
                .into_iter()
                .filter(|a| a.category() == category)
                .filter(|a| a.angle_number().is_none_or(|n| n == 1))
                .map(KeyRow::Act),
        );
        rows.extend(
            keymap::FIXED
                .iter()
                .enumerate()
                .filter(|(_, f)| f.category == category)
                .map(|(i, _)| KeyRow::Fixed(i)),
        );
    }
    rows
}

/// The KEYS tab's type-to-filter: keep a heading only when one of its rows
/// still matches, match a row on its label or its chord.
pub(crate) fn keys_rows_matching(query: &str, keymap: &keymap::Keymap) -> Vec<KeyRow> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return keys_rows();
    }
    let rows = keys_rows();
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        match rows[i] {
            KeyRow::Head(cat) => {
                let mut kids = Vec::new();
                i += 1;
                while i < rows.len() && !matches!(rows[i], KeyRow::Head(_)) {
                    if key_row_hits(&rows[i], &q, keymap) {
                        kids.push(rows[i]);
                    }
                    i += 1;
                }
                if !kids.is_empty() {
                    out.push(KeyRow::Head(cat));
                    out.extend(kids);
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn key_row_hits(row: &KeyRow, q: &str, keymap: &keymap::Keymap) -> bool {
    match *row {
        KeyRow::Head(c) => c.label().to_ascii_lowercase().contains(q),
        KeyRow::Act(a) => {
            a.label().to_ascii_lowercase().contains(q)
                || keymap.display(a).to_ascii_lowercase().contains(q)
                || keymap::search_aliases(&keymap.display(a))
                    .to_ascii_lowercase()
                    .contains(q)
        }
        KeyRow::Fixed(i) => {
            let f = &keymap::FIXED[i];
            f.label.to_ascii_lowercase().contains(q)
                || f.chord.to_ascii_lowercase().contains(q)
                || keymap::search_aliases(&f.chord)
                    .to_ascii_lowercase()
                    .contains(q)
        }
    }
}

/// The character a stroke types into the actions card's search box, if it types
/// one. gpui reports a printable key as itself and the space bar by name
/// (platform.rs:866), and everything else -- the arrows, the function keys --
/// is a word this must not spell into the box letter by letter.
pub(crate) fn typed(key: &str) -> Option<char> {
    match key {
        "space" => Some(' '),
        _ => key
            .chars()
            .next()
            .filter(|c| c.is_ascii_graphic() && key.chars().count() == 1),
    }
}

/// One row of the command palette (DESIGN §4.4, Ctrl+K): an action, and the
/// reason the editor refuses it right now, if it does. A hidden action never
/// becomes a row ([`palette_rows`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaletteRow {
    pub(crate) action: ActionId,
    /// `Some(reason)` when the row is listed but cannot run. It stays on the
    /// list in ink4, with the reason where the key would be (DESIGN §4.8).
    pub(crate) refused: Option<&'static str>,
}

/// The palette's rows for `query`, in the order they list.
///
/// An empty query lists every action under its category, in the registry's
/// order. A query keeps the actions it matches on their label or category --
/// case-insensitive, each word a prefix or a substring -- and lists the best
/// matches first: a label that starts with the word beats a word inside it,
/// which beats a substring, which beats a match on the category alone. Ties
/// keep the registry's order. `enable` is the oracle's answer per action; its
/// `Hidden` answers are dropped, its `No` answers stay as refused rows.
pub(crate) fn palette_rows(
    query: &str,
    enable: impl Fn(ActionId) -> crate::oracle::Enable,
) -> Vec<PaletteRow> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    // (score, registry index, row): the index keeps every sort stable.
    let mut hits: Vec<(u32, usize, PaletteRow)> = Vec::new();
    for (i, action) in ActionId::ALL.into_iter().enumerate() {
        let refused = match enable(action) {
            crate::oracle::Enable::Hidden(_) => continue,
            crate::oracle::Enable::Yes => None,
            crate::oracle::Enable::No(why) => Some(why),
        };
        let Some(score) = palette_score(action, &tokens) else {
            continue;
        };
        hits.push((score, i, PaletteRow { action, refused }));
    }
    if tokens.is_empty() {
        hits.sort_by_key(|(_, i, row)| (category_rank(row.action.category()), *i));
    } else {
        hits.sort_by_key(|(score, i, _)| (*score, *i));
    }
    hits.into_iter().map(|(_, _, row)| row).collect()
}

/// How well `action` answers every word of the query, lower is better, or
/// `None` when one word matches nothing. A word scores 0 for a label prefix, 1
/// for a label word prefix, 2 for a label substring and 3 for a category match.
fn palette_score(action: ActionId, tokens: &[String]) -> Option<u32> {
    let label = action.label().to_lowercase();
    let category = action.category().label().to_lowercase();
    let mut total = 0;
    for token in tokens {
        let word_starts = |text: &str| {
            text.split(|c: char| !c.is_alphanumeric())
                .any(|w| w.starts_with(token.as_str()))
        };
        total += if label.starts_with(token.as_str()) {
            0
        } else if word_starts(&label) {
            1
        } else if label.contains(token.as_str()) {
            2
        } else if word_starts(&category) || category.contains(token.as_str()) {
            3
        } else {
            return None;
        };
    }
    Some(total)
}

/// Where a category sits in the registry's own order, for grouping an empty
/// query.
fn category_rank(category: keymap::Category) -> usize {
    keymap::Category::ALL
        .iter()
        .position(|c| *c == category)
        .unwrap_or(usize::MAX)
}

/// The list index of row `sel` as a child of the palette's list, which holds a
/// heading before each new category when the query is empty. The scroll handle
/// wants a child index, not a row index.
pub(crate) fn palette_child_index(rows: &[PaletteRow], sel: usize, grouped: bool) -> usize {
    if !grouped {
        return sel;
    }
    // Every heading at or above the row: one before each category's first row.
    let headings = (0..rows.len().min(sel + 1))
        .filter(|&i| i == 0 || rows[i - 1].action.category() != rows[i].action.category())
        .count();
    sel + headings
}

/// What Enter does on a palette row: a runnable row runs and the palette
/// closes; a refused row says why (`Player::act`'s toast) and stays open.
pub(crate) fn palette_closes_on_run(row: PaletteRow) -> bool {
    row.refused.is_none()
}

#[cfg(test)]
mod palette_tests {
    use super::*;
    use crate::oracle::Enable;

    fn all_yes(_: ActionId) -> Enable {
        Enable::Yes
    }

    #[test]
    fn empty_query_lists_every_action_grouped_by_category() {
        let rows = palette_rows("", all_yes);
        assert_eq!(rows.len(), ActionId::ALL.len());
        let cats: Vec<usize> = rows
            .iter()
            .map(|r| category_rank(r.action.category()))
            .collect();
        assert!(cats.windows(2).all(|w| w[0] <= w[1]), "not grouped: {cats:?}");
    }

    #[test]
    fn a_label_prefix_ranks_above_a_word_inside_a_label() {
        let rows = palette_rows("play", all_yes);
        let first = rows[0].action.label().to_lowercase();
        assert!(first.starts_with("play"), "best match was {first:?}");
        assert!(
            rows.iter().all(|r| {
                let l = r.action.label().to_lowercase();
                l.contains("play") || r.action.category().label().to_lowercase().contains("play")
            }),
            "a row matches neither label nor category"
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(palette_rows("PLAY", all_yes), palette_rows("play", all_yes));
    }

    #[test]
    fn a_category_word_lists_its_actions() {
        let rows = palette_rows("clips", all_yes);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.action.category() == keymap::Category::Clips
            || r.action.label().to_lowercase().contains("clip")));
    }

    #[test]
    fn every_word_of_the_query_must_match() {
        assert!(palette_rows("play xyzzy", all_yes).is_empty());
        assert!(palette_rows("xyzzy-no-such", all_yes).is_empty());
    }

    #[test]
    fn hidden_actions_are_omitted_and_refused_ones_stay_listed() {
        let rows = palette_rows("", |a| match a {
            ActionId::Cut => Enable::Hidden("no clip"),
            ActionId::Copy => Enable::No("nothing selected"),
            _ => Enable::Yes,
        });
        assert!(!rows.iter().any(|r| r.action == ActionId::Cut));
        let copy = rows.iter().find(|r| r.action == ActionId::Copy).unwrap();
        assert_eq!(copy.refused, Some("nothing selected"));
        let play = rows.iter().find(|r| r.action == ActionId::Play).unwrap();
        assert_eq!(play.refused, None);
    }

    #[test]
    fn only_a_runnable_row_closes_the_palette() {
        let yes = PaletteRow { action: ActionId::Play, refused: None };
        let no = PaletteRow { action: ActionId::Copy, refused: Some("nothing selected") };
        assert!(palette_closes_on_run(yes));
        assert!(!palette_closes_on_run(no));
    }

    #[test]
    fn child_index_counts_the_category_headings_above_the_row() {
        let rows = palette_rows("", all_yes);
        assert_eq!(palette_child_index(&rows, 0, true), 1);
        let last = rows.len() - 1;
        let blocks = keymap::Category::ALL
            .iter()
            .filter(|c| rows.iter().any(|r| r.action.category() == **c))
            .count();
        assert_eq!(palette_child_index(&rows, last, true), last + blocks);
        assert_eq!(palette_child_index(&rows, 3, false), 3);
    }
}

#[cfg(test)]
mod keys_search_tests {
    use super::*;

    /// The KEYS search finds a fixed row by the word a person says for its
    /// glyph, and by the glyph's own name for `Esc`.
    #[test]
    fn a_glyph_row_is_found_by_its_spoken_name() {
        let keymap = keymap::Keymap::defaults();
        let finds = |q: &str, chord: &str| {
            keys_rows_matching(q, &keymap).iter().any(|row| match row {
                KeyRow::Fixed(i) => keymap::FIXED[*i].chord == chord,
                _ => false,
            })
        };
        assert!(finds("enter", "↵"));
        assert!(finds("return", "↵"));
        assert!(finds("up", "↑"));
        assert!(finds("left arrow", "← / →"));
        assert!(finds("escape", "Esc"));
        assert!(finds("esc", "Esc"));
    }
}

/// The 16px icon a menu row wears beside its label (REDESIGN §3.1), where the
/// verb has an obvious picture; `None` leaves the icon column blank so labels
/// still line up. Pure decoration -- the label and the key stay the contract.
pub(crate) fn icon(action: ActionId) -> Option<crate::ui::icons::Icon> {
    use crate::ui::icons::Icon as I;
    Some(match action {
        ActionId::AddFiles => I::Plus,
        ActionId::OpenProject => I::FolderOpen,
        ActionId::NewProject => I::Clapperboard,
        ActionId::ImportSubtitles => I::Captions,
        ActionId::Save => I::Save,
        ActionId::ReloadProject => I::RefreshCw,
        ActionId::Export => I::Upload,
        ActionId::Screenshot => I::Image,
        ActionId::Settings => I::Settings,
        ActionId::Undo => I::Undo2,
        ActionId::Redo => I::Redo2,
        ActionId::History => I::Undo2,
        ActionId::Copy => I::Copy,
        ActionId::Paste | ActionId::PasteFilePath => I::Clipboard,
        ActionId::SelectAll => I::Layers,
        ActionId::Cut => I::Scissors,
        ActionId::Delete => I::Trash2,
        ActionId::Lift => I::Eraser,
        ActionId::Color => I::Palette,
        ActionId::Transform => I::Move,
        ActionId::Effects => I::Sparkles,
        ActionId::Equalizer | ActionId::Mix => I::SlidersHorizontal,
        ActionId::Speed => I::FastForward,
        ActionId::Group | ActionId::NestClips => I::Layers,
        ActionId::Detach => I::Unlink,
        ActionId::ToggleMute => I::VolumeX,
        ActionId::SetIn => I::SquareArrowDownRight,
        ActionId::SetOut => I::SquareArrowDownLeft,
        ActionId::ZoomIn => I::ZoomIn,
        ActionId::ZoomOut => I::ZoomOut,
        ActionId::ZoomFit | ActionId::ZoomToSelection => I::Scan,
        ActionId::ViewZoom => I::ZoomIn,
        ActionId::PlaybackQuality => I::Signal,
        ActionId::GuideStyle => I::Grid3x3,
        ActionId::GuideMask => I::Image,
        ActionId::ToggleOutputWindow | ActionId::OutputDisplay => I::Scan,
        ActionId::ToggleSnap => I::Magnet,
        // A track's height: the one resize glyph the set has, for the three sizes and the two steps.
        ActionId::TrackSmall | ActionId::TrackMedium | ActionId::TrackLarge | ActionId::TracksSmaller | ActionId::TracksLarger => I::Scaling,
        ActionId::AddMarker | ActionId::MarkerHere => I::Flag,
        ActionId::RenameMarker => I::Type,
        ActionId::DeleteMarker => I::Trash2,
        ActionId::PlayheadHere => I::SkipBack,
        ActionId::Captions => I::Captions,
        ActionId::Voiceover | ActionId::RecordVoiceover => I::Mic,
        ActionId::Play => I::Play,
        ActionId::StepBack => I::StepBack,
        ActionId::StepForward => I::StepForward,
        ActionId::GoStart => I::SkipBack,
        ActionId::GoEnd => I::SkipForward,
        ActionId::GoToTime => I::Timer,
        ActionId::Loop => I::Repeat,
        ActionId::PlayAround => I::Play,
        ActionId::ToggleWaveforms => I::AudioWaveform,
        ActionId::ToggleScrubAudio => I::Volume2,
        ActionId::CompareOff | ActionId::CompareSplit | ActionId::CompareSide => I::Layers,
        ActionId::AutoColour => I::WandSparkles,
        ActionId::SaveLook => I::Save,
        ActionId::ToggleSubtitles => I::Captions,
        ActionId::ShowActions => I::Search,
        ActionId::ShowKeys => I::Keyboard,
        ActionId::About => I::Info,
        ActionId::Normalize | ActionId::CleanVoice => I::AudioWaveform,
        ActionId::SubtitleStyle => I::Type,
        ActionId::Reframe => I::Scaling,
        ActionId::TrackMask => I::Scan,
        ActionId::TrimIn => I::ArrowLeftToLine,
        ActionId::TrimOut => I::ArrowRightToLine,
        ActionId::Regroup => I::Link,
        ActionId::LoopTrim => I::Repeat,
        ActionId::Reverse => I::RotateCcw,
        ActionId::Dissolve | ActionId::Crossfade | ActionId::TransitionDissolve => I::Sparkles,
        ActionId::TransitionDipBlack => I::Square,
        ActionId::TransitionDipWhite => I::Circle,
        ActionId::TransitionZoom => I::ZoomIn,
        ActionId::TransitionWipeLeft => I::SquareArrowDownLeft,
        ActionId::TransitionWipeRight => I::SquareArrowDownRight,
        ActionId::TransitionWipeUp => I::ArrowUp,
        ActionId::TransitionWipeDown => I::ArrowDown,
        ActionId::TransitionSlideLeft => I::ChevronLeft,
        ActionId::TransitionSlideRight => I::ChevronRight,
        ActionId::TransitionSlideUp => I::ChevronUp,
        ActionId::TransitionSlideDown => I::ChevronDown,
        ActionId::TransitionPushLeft | ActionId::TransitionPushRight | ActionId::TransitionPushUp | ActionId::TransitionPushDown => I::Layers,
        ActionId::ClipLabel => I::Flag,
        ActionId::AddVideoLane | ActionId::AddAudioLane | ActionId::AddSubtitleLane | ActionId::NewSequence => I::Plus,
        ActionId::RemoveVideoLane | ActionId::RemoveAudioLane | ActionId::RemoveSubtitleLane => I::Minus,
        ActionId::RenameLane | ActionId::RenameSequence | ActionId::RenameBin | ActionId::NewTitle => I::Type,
        ActionId::DeleteSequence | ActionId::RemoveBin => I::Trash2,
        ActionId::DuplicateSequence => I::Copy,
        ActionId::NewBin | ActionId::NewBinInside => I::Folder,
        ActionId::NewSolid => I::Square,
        ActionId::NewGradient => I::Palette,
        ActionId::NewCountdown => I::Timer,
        ActionId::NewBars => I::Grid3x3,
        ActionId::NewTone => I::Volume2,
        ActionId::NewBarsAndTone => I::AudioWaveform,
        ActionId::NewAdjustment => I::SlidersHorizontal,
        ActionId::NewRectangle => I::Square,
        ActionId::NewEllipse => I::Circle,
        ActionId::NewLine => I::Minus,
        ActionId::NewArrow => I::ArrowRight,
        ActionId::BreakApart => I::Unlink,
        ActionId::CancelExport | ActionId::Deselect => I::CircleX,
        ActionId::ClearRange => I::Eraser,
        ActionId::RenderInOut => I::SquareArrowDownRight,
        ActionId::RenderAll => I::Film,
        ActionId::CancelRender => I::CircleX,
        ActionId::DeleteRenderFiles => I::Trash2,
        ActionId::ClipEnabled | ActionId::ToggleAngleMonitor | ActionId::KeyOpacity => I::Eye,
        ActionId::CloseCompound | ActionId::OpenCompound | ActionId::CreateMulticam | ActionId::FlattenMulticam | ActionId::Angle1 => {
            I::Layers
        }
        ActionId::CloseGap | ActionId::TrimInToPlayhead | ActionId::RippleTrimInToPlayhead => I::ArrowLeftToLine,
        ActionId::ExtendAudio | ActionId::ExtendVideo | ActionId::TrimOutToPlayhead | ActionId::RippleTrimOutToPlayhead => {
            I::ArrowRightToLine
        }
        ActionId::CollectMedia | ActionId::InsertSource | ActionId::OverwriteSource => I::Download,
        ActionId::ExportTimelineOtio | ActionId::ExportTimelineFcpxml | ActionId::ExportTimelineEdl => I::Upload,
        ActionId::ImportTimelineOtio => I::Plus,
        ActionId::Fillers | ActionId::Silence | ActionId::Visualizer => I::AudioWaveform,
        ActionId::CutSelectedWords => I::Trash2,
        ActionId::MarkBeats => I::Timer,
        ActionId::Fit | ActionId::Resolution => I::Scaling,
        ActionId::Fullscreen => I::Scan,
        ActionId::FocusPanels => I::Layers,
        ActionId::Freeze | ActionId::ShuttleStop => I::Pause,
        ActionId::JumpBack | ActionId::PrevSyncPoint => I::SkipBack,
        ActionId::JumpForward | ActionId::NextSyncPoint => I::SkipForward,
        ActionId::KeyColor | ActionId::Theme | ActionId::AccentColour => I::Palette,
        ActionId::KeyEasing => I::Spline,
        ActionId::SpeedRampClear => I::Eraser,
        ActionId::SpeedRampDown => I::TrendingDown,
        ActionId::SpeedRampUp => I::TrendingUp,
        ActionId::SpeedRampSlowFastSlow => I::Activity,
        ActionId::SpeedRampFastSlowFast => I::Zap,
        ActionId::SpeedRampKey => I::Diamond,
        ActionId::ToggleProxies | ActionId::ToggleAutoProxies => I::Gauge,
        ActionId::KeyPlacement => I::Move,
        ActionId::Markers | ActionId::NextMarker | ActionId::PrevMarker => I::FlagTriangleRight,
        ActionId::MatchFrame | ActionId::ReplaceEdit => I::Film,
        ActionId::NudgeBack | ActionId::NudgeBack10 | ActionId::RollBack | ActionId::SlipBack | ActionId::SlideBack | ActionId::ShuttleBack => I::StepBack,
        ActionId::NudgeForward | ActionId::NudgeForward10 | ActionId::RollForward | ActionId::SlipForward | ActionId::SlideForward | ActionId::ShuttleForward => I::StepForward,
        ActionId::PasteAttributes | ActionId::PasteAudio | ActionId::PasteColor | ActionId::PasteEffects | ActionId::PasteSpeed | ActionId::PasteTransform => I::Clipboard,
        ActionId::SaveVersion | ActionId::SaveTemplate | ActionId::ExportCaptions => I::Save,
        ActionId::Versions => I::RotateCcw,
        ActionId::ProjectNotes => I::Clipboard,
        ActionId::Select | ActionId::SelectForward | ActionId::SelectForwardAll => I::Brackets,
        ActionId::SelectNext | ActionId::WalkCutNext | ActionId::WalkCutNext10 => I::ChevronRight,
        ActionId::SelectPrev | ActionId::WalkCutPrev | ActionId::WalkCutPrev10 => I::ChevronLeft,
        ActionId::VoiceDynamics => I::Mic,
        ActionId::VolumeDown | ActionId::VolumeUp => I::Volume2,
        _ => return None,
    })
}

/// The one-line plain description a menu row carries on hover, for the two
/// trims whose names alone do not say what they do.
pub(crate) fn note(action: ActionId) -> Option<&'static str> {
    Some(match action {
        ActionId::SlipBack | ActionId::SlipForward => "Slip: change which part of the footage shows, keeping the clip in place",
        ActionId::SlideBack | ActionId::SlideForward => "Slide: move the clip between its neighbours, keeping its footage",
        ActionId::Detach => "Unlink: the picture and the sound of a grouped clip edit separately",
        _ => return None,
    })
}

/// The icon beside a submenu row of the bar menus (REDESIGN §3.1).
pub(crate) fn icon_sub(name: &str) -> Option<crate::ui::icons::Icon> {
    use crate::ui::icons::Icon as I;
    Some(match name {
        "Paste attributes" => I::Clipboard,
        "Export timeline as" => I::Upload,
        "New from template" => I::Layers,
        "Compare grade" => I::Layers,
        "Trim" => I::Scissors,
        "Speed" => I::FastForward,
        "Adjust" => I::SlidersHorizontal,
        "Transitions" => I::Sparkles,
        "Compound" => I::Layers,
        "Markers" => I::Flag,
        "Select" => I::Brackets,
        "Captions" => I::Captions,
        "Audio" => I::AudioWaveform,
        "Tracks" => I::Film,
        "Keyframes" => I::KeyRound,
        "Go to" => I::SkipForward,
        "Sequence" => I::Film,
        "Render preview" => I::Zap,
        "New" => I::Plus,
        "Multicam" => I::Layers,
        "Nudge" => I::StepForward,
        "Open recent" => I::RotateCcw,
        _ => return None,
    })
}

#[cfg(test)]
mod icon_tests {
    use super::*;
    use crate::ui::menubar::Title;

    /// Every verb any menu lists wears an icon, so the icon column is never half empty: the
    /// bar's menus and flyouts, the clip, track, bench, viewer, tab, bin and dock menus.
    #[test]
    fn every_menu_row_has_an_icon() {
        let mut actions: Vec<ActionId> = Vec::new();
        for title in Title::ALL {
            actions.extend(title.entries().into_iter().filter_map(|e| e.action()));
        }
        for list in [
            &MENU_ITEMS[..], &BENCH_ITEMS[..], &VIEWER_ITEMS[..], &SEQ_TAB_ITEMS[..], &MARKER_ITEMS[..], &BIN_ITEMS[..], &DOCK_ITEMS[..], &HEAD_ADDS[..],
        ] {
            actions.extend_from_slice(list);
        }
        for kind in [LaneKind::Video, LaneKind::Audio, LaneKind::Subtitle] {
            actions.extend(crate::oracle::lane_items(Lane { kind, ord: 0 }));
        }
        let mut missing: Vec<String> = actions.into_iter().filter(|a| icon(*a).is_none()).map(|a| format!("{a:?}")).collect();
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "menu rows without an icon: {missing:?}");
        let mut subs: Vec<&str> = Vec::new();
        for title in Title::ALL {
            fn walk<'a>(list: &'a [crate::ui::menubar::Entry], out: &mut Vec<&'a str>) {
                for e in list {
                    if let crate::ui::menubar::Entry::Sub(name, inner) = e {
                        out.push(name);
                        walk(inner, out);
                    }
                }
            }
            walk(title.layout(), &mut subs);
        }
        let bare: Vec<&&str> = subs.iter().filter(|n| icon_sub(n).is_none()).collect();
        assert!(bare.is_empty(), "flyout rows without an icon: {bare:?}");
        // The media row menu has its own verbs: all of them wear one too.
        let _ = ROW_ITEMS.map(RowItem::icon);
    }
}
