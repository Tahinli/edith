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
    /// A lane head: the verbs of the track itself, on the lane the menu
    /// already names ([`oracle::lane_items`]).
    Head,
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
    /// What the export's *sound* is coded at. Opened from the card's Sound row,
    /// which is the only place it means anything.
    #[allow(dead_code)] // opened from Settings now
    AudioRate,
    /// Which HDR-to-SDR rendition the project is watched and exported in
    /// ([`engine::tonemap::Preset`]). Opened from the panel, beside the two
    /// other settings that are the project's rather than the media's.
    Tone,
    /// Which palette the window is painted in ([`ui::theme`]). The one setting
    /// here that is nobody's project: it is the person's, so it outlives the
    /// timeline and every file opened in it.
    Theme,
    /// Which encoder an export writes the picture with
    /// ([`engine::export::EncoderSeat`]). Opened from the card's Encoder row,
    /// which is the only place it means anything.
    #[allow(dead_code)] // opened from Settings now
    Encoder,
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
    /// `None` is "source" -- the rate derived from the first audio source,
    /// [`engine::PlaybackSession::set_sample_rate`]'s own default.
    SampleRate(Option<u32>),
    Fit(Lane, usize, FitPolicy),
    AudioRate(u32),
    Tone(Preset),
    Theme(ui::theme::PaletteId),
    Encoder(EncoderSeat),
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
    Add,
    Remove,
    /// Remove that first deletes every clip playing the row. The plain
    /// [`RowItem::Remove`] is refused while any clip plays a source (the
    /// engine's own rule), which used to leave a placed source with no way
    /// out of the library at all -- this is that way out, and it is
    /// destructive, so it is listed only when it is the one that applies and
    /// it says how many clips it takes with it.
    RemoveWithClips,
    Reveal,
    Properties,
}

pub(crate) const ROW_ITEMS: [RowItem; 5] = [
    RowItem::Add,
    RowItem::Remove,
    RowItem::RemoveWithClips,
    RowItem::Reveal,
    RowItem::Properties,
];

impl RowItem {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Add => "Add at playhead",
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
        }
    }

    /// The dim right-hand column: the *stroke* that does the same thing, the
    /// one thing DESIGN §4 puts there ("every command wears its chord"), and
    /// nothing at all where no stroke reaches the verb.
    ///
    /// It used to carry a prose gloss on every row -- a phrase about the whole
    /// file, one about the desktop's file browser, one about clips -- which is
    /// §8's instructional copy sitting in the column the sibling clip menu
    /// spends on chords, and which the user read as the crowding it is. The
    /// only row a stroke reaches is the add (`ui/stance.rs`'s `enter` branch,
    /// live while a dock row is picked); its own row in the dock already wears
    /// the same `↵`. A refusal still prints here in place of the chord
    /// (`oracle::row_enable`) -- that is state, not instruction.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::Add => "↵",
            Self::Remove | Self::RemoveWithClips | Self::Reveal | Self::Properties => "",
        }
    }
}

/// What the menu offers, in the order it lists them. Every one of these is an
/// action a stroke already reaches -- the menu is a second way *to* the actions
/// and never a second version of them -- so both the label and the hint come
/// out of the keymap registry and the two can never disagree.
pub(crate) const MENU_ITEMS: [ActionId; 16] = [
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
    // The two joins, one per kind, each hidden over the other kind
    // (`oracle::enable`, DESIGN §8's class refusal): sound crossfades, picture
    // dissolves, and neither is ever offered over the wrong waveform.
    ActionId::Crossfade,
    ActionId::Dissolve,
    // The picture cards...
    ActionId::Color,
    ActionId::Transform,
    // ...and the sound ones, the same class refusal the other way round.
    ActionId::Equalizer,
    ActionId::Silence,
    ActionId::Speed,
    ActionId::Fit,
    // Last, under the rule line the render draws before the first of them
    // (DESIGN §9): the two that take the clip off the lane.
    ActionId::Delete,
    ActionId::Lift,
];

// Not rows, and deliberately: the group trio (`Group`, `Detach`, `Regroup`)
// is the ctrl-click grammar plus its chords -- a group is made by picking the
// halves and taking it apart the same way -- and the mute is the *mix's*,
// which the transport already carries a button for. A clip menu is what this
// clip can be told (DESIGN §9, "Never a junk drawer"); those are told to the
// selection and to the project.

/// What a right-click on the bench itself offers -- the ruler and the empty
/// stretch under the lanes. The verbs of the *timeline*: walking its cuts at
/// either stride (DESIGN §6's odometer), what it is looked at through, and the
/// undo pair, which is the one thing every editor expects on empty canvas.
/// Not a junk drawer (§9): nothing here acts on a clip, and everything here
/// acts on the thing the pointer is over.
pub(crate) const BENCH_ITEMS: [ActionId; 10] = [
    ActionId::WalkCutPrev,
    ActionId::WalkCutNext,
    ActionId::WalkCutPrev10,
    ActionId::WalkCutNext10,
    ActionId::ZoomOut,
    ActionId::ZoomIn,
    ActionId::ZoomFit,
    ActionId::ToggleSnap,
    ActionId::Undo,
    ActionId::Redo,
];

/// The tracks a lane head's menu can add, in the order it lists them. Its
/// remove is the one of the clicked lane's own kind and comes from
/// [`oracle::lane_items`], which is what puts it under the rule line.
pub(crate) const HEAD_ADDS: [ActionId; 3] = [
    ActionId::AddVideoLane,
    ActionId::AddAudioLane,
    ActionId::AddSubtitleLane,
];

/// Whether a menu row takes something away -- what DESIGN §9 puts below a rule
/// line, wherever the row is listed. One answer, so the clip menu and the lane
/// head's menu cannot come to disagree about which of their rows is the
/// dangerous one.
pub(crate) fn destructive(action: ActionId) -> bool {
    matches!(
        action,
        ActionId::Delete
            | ActionId::Lift
            | ActionId::RemoveVideoLane
            | ActionId::RemoveAudioLane
            | ActionId::RemoveSubtitleLane
    )
}


/// One row of the actions card, in the order it lists them: a heading, then
/// every action the registry files under it, then the strokes the modal cards
/// answer themselves.
///
/// A list rather than a loop inside the render, so the card and
/// `every_action_is_on_the_actions_card` read the *same* order: an action that
/// reaches no row fails a test instead of quietly becoming pointer-unreachable.
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
        rows.extend(
            ActionId::ALL
                .into_iter()
                .filter(|a| a.category() == category)
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
