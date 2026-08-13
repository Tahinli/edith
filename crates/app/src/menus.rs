//! The lists the window opens over itself: the clip menu, the pickers, the key rows.

use crate::*;

/// An open clip menu: which clip it was opened on, where it hangs, and whether
/// it has been turned over to show what that clip *is* instead of what can be
/// done to it. The lane and index are the ones the same click selected, so
/// every item acts on exactly the box under the pointer.
#[derive(Clone, Copy)]
pub(crate) struct ContextMenu {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
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
    Fit(Lane, usize),
    /// What the export's *sound* is coded at. Opened from the card's Sound row,
    /// which is the only place it means anything.
    AudioRate,
    /// Which HDR-to-SDR rendition the project is watched and exported in
    /// ([`engine::tonemap::Preset`]). Opened from the panel, beside the two
    /// other settings that are the project's rather than the media's.
    Tone,
    /// Which palette the window is painted in ([`ui::theme`]). The one setting
    /// here that is nobody's project: it is the person's, so it outlives the
    /// timeline and every file opened in it.
    Theme,
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
    Fit(Lane, usize, FitPolicy),
    AudioRate(u32),
    Tone(Preset),
    Theme(ui::theme::PaletteId),
}

/// One row of an open list: the value, its name, the small print beside it, and
/// whether it is the one in force.
pub(crate) type ChoiceRow = (Choice, SharedString, SharedString, bool);

/// What a library row's menu offers, in the order it lists them. Unlike the clip
/// menu's items none of these is a stroke -- there is no keyboard way to a row --
/// so the label and the hint are written here rather than read off the keymap.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RowItem {
    Add,
    Remove,
    Reveal,
    Properties,
}

pub(crate) const ROW_ITEMS: [RowItem; 4] = [
    RowItem::Add,
    RowItem::Remove,
    RowItem::Reveal,
    RowItem::Properties,
];

impl RowItem {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Add => "Add at playhead",
            Self::Remove => "Remove from library",
            Self::Reveal => "Reveal in files",
            Self::Properties => "Properties",
        }
    }

    /// The dim right-hand column, where the clip menu prints the stroke: what
    /// the item will do to the timeline, so nothing here is a surprise.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            // Short enough to sit beside its label inside `MENU_W`, the clip
            // menu's rule for a refusal: this column truncates, and a hint cut
            // off mid-word says less than a shorter one.
            Self::Add => "the whole file",
            Self::Remove => "nothing plays it",
            Self::Reveal => "file manager",
            Self::Properties => "…",
        }
    }
}

/// What the menu offers, in the order it lists them. Every one of these is an
/// action a stroke already reaches -- the menu is a second way *to* the actions
/// and never a second version of them -- so both the label and the hint come
/// out of the keymap registry and the two can never disagree.
pub(crate) const MENU_ITEMS: [ActionId; 14] = [
    ActionId::Cut,
    // The clipboard pair, which had no door but a chord: copy takes the clip the
    // menu names, and paste is the timeline's rather than this clip's -- the
    // same kind of global item the mute below already is.
    ActionId::Copy,
    ActionId::Paste,
    ActionId::Delete,
    ActionId::Lift,
    ActionId::Regroup,
    ActionId::Detach,
    ActionId::Group,
    ActionId::Equalizer,
    ActionId::Speed,
    // The scan is a clip card like the two above it -- opened on whichever half
    // was clicked -- and a card only a stroke could open is one a pointer never
    // finds.
    ActionId::Silence,
    ActionId::Color,
    ActionId::Fit,
    ActionId::ToggleMute,
];

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

/// [`keys_rows`] with a search applied: the rows whose label or whose stroke
/// carries `needle`, and the heading each one lives under. A heading with
/// nothing left beneath it goes with them -- an empty "Playback" over a gap
/// reads as a list that lost its rows rather than as a search that found none.
///
/// Each row keeps the index it has in the unfiltered list, so an element id is
/// the same one before and after a keystroke: filtering is a look at the list,
/// and gpui's per-element state is keyed on that id.
///
/// Case-insensitive substring, on both columns: people look for an action by
/// what it does ("vol") and for a stroke by what they pressed ("ctrl").
pub(crate) fn keys_filter(needle: &str, keymap: &Keymap) -> Vec<(usize, KeyRow)> {
    let needle = needle.trim().to_lowercase();
    let rows = keys_rows().into_iter().enumerate();
    if needle.is_empty() {
        return rows.collect();
    }
    let hit = |label: &str, chord: &str| {
        label.to_lowercase().contains(&needle) || chord.to_lowercase().contains(&needle)
    };
    let mut out: Vec<(usize, KeyRow)> = Vec::new();
    // The heading above the row being looked at, until a row under it earns it
    // a place -- then it goes in once, ahead of that row.
    let mut pending: Option<(usize, KeyRow)> = None;
    for (i, row) in rows {
        match &row {
            KeyRow::Head(_) => pending = Some((i, row)),
            KeyRow::Act(action) => {
                if hit(action.label(), &keymap.display(*action)) {
                    out.extend(pending.take());
                    out.push((i, row));
                }
            }
            KeyRow::Fixed(f) => {
                let fixed = &keymap::FIXED[*f];
                if hit(fixed.label, &fixed.chord) {
                    out.extend(pending.take());
                    out.push((i, row));
                }
            }
        }
    }
    out
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
