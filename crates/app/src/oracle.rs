//! Whether an action can be asked for -- the one answer every surface reads.

use crate::*;

/// Whether an action can be asked for, and what to say when it cannot. Two
/// kinds of no: `Hidden` is about the *kind* of thing the action was aimed at
/// -- an audio clip has no picture, so a grade is not a thing that exists for
/// it, whatever the editor does next -- and `No` is about the state of this
/// moment, which the next click of the playhead can change. The clip menu
/// leaves the class refusals *out* and dims the state ones ([`Enable::listed`]);
/// the actions card, which is the whole registry laid out, dims both with their
/// reason -- an action missing from the one surface that lists everything would
/// read as an action that does not exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Enable {
    Yes,
    No(&'static str),
    Hidden(&'static str),
}

impl Enable {
    /// Whether the row takes a click.
    pub(crate) fn yes(self) -> bool {
        self == Enable::Yes
    }

    /// Whether a menu *about this clip* draws the row at all: a class refusal
    /// is a thing that does not exist for what was clicked, and a row nothing
    /// the user does could ever light is noise between the ones they came for.
    pub(crate) fn listed(self) -> bool {
        !matches!(self, Enable::Hidden(_))
    }

    /// What the row says instead of its stroke, if it says anything.
    pub(crate) fn why(self) -> Option<&'static str> {
        match self {
            Enable::Yes => None,
            Enable::No(why) | Enable::Hidden(why) => Some(why),
        }
    }
}

/// What an enablement question is asked *about*: the clip in question, if there
/// is one, and the little of the editor's state the answers need. Handed in
/// rather than read off the player, so [`enable`] is a pure function a test can
/// ask about a clip without building a window.
#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx {
    /// The clip the question is about -- the one a menu was opened on, or the
    /// marked one. `None` means the question is about the editor as a whole,
    /// and the clip-relative answers stand aside: those actions find their own
    /// clip under the playhead and word their own refusal.
    pub(crate) clip: Option<(Clip, Lane)>,
    /// The clip plays a still ([`engine::is_image`]), which has no sound to
    /// reach at all -- not the lane's business, because a still sits on a video
    /// lane exactly like a take whose sound is one lane down.
    pub(crate) image: bool,
    pub(crate) playhead: u32,
    /// A timeline is open.
    pub(crate) timeline: bool,
    /// Something has been copied.
    pub(crate) clipboard: bool,
    /// This timeline has at least one subtitle track: a toggle with nothing to
    /// show is a switch that does nothing, and it says so rather than flipping.
    pub(crate) subtitles: bool,
    /// There is something on the lanes to play. A transport with nothing under
    /// it is the one refusal that is about the timeline's contents rather than
    /// about a clip.
    pub(crate) playable: bool,
    pub(crate) exporting: bool,
}

/// Whether `action` can be asked for, on `ctx`. One arm per action and nothing
/// else in the editor asks the question: the clip menu dims a row with this,
/// the actions card dims a row with this, and the two can never come to
/// disagree about what an action needs -- exactly the reason [`Player::act`] is
/// one table too.
pub(crate) fn enable(action: ActionId, ctx: Ctx) -> Enable {
    // The one action that is about the editor rather than about the timeline:
    // the list of what everything does, and where a key is changed. An empty
    // window has no clips and still has keys -- so this answers ahead of the
    // timeline question, and only an export shuts it (a waiting row would
    // swallow the escape the progress line promises cancels the export).
    // The window's own colours: there is always a window, and repainting it
    // touches no timeline -- so this one is live even while an export is
    // reading one, which is the only state that dims the list above.
    if action == ActionId::Theme {
        return Enable::Yes;
    }
    if action == ActionId::ShowActions {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    // The four that are about the *editor* and its monitoring rather than
    // about the edit list: they work with nothing open, the keyboard has
    // always fired them there, and so their buttons are live there too.
    if matches!(
        action,
        ActionId::ToggleSnap | ActionId::ToggleMute | ActionId::VolumeUp | ActionId::VolumeDown
    ) {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    if !ctx.timeline {
        return Enable::No("no timeline open");
    }
    // An export is reading the edit list every other action would change, which
    // is the rule the key handler already follows.
    if ctx.exporting {
        return match action {
            ActionId::CancelExport => Enable::Yes,
            _ => Enable::No("an export is running"),
        };
    }
    match action {
        // -- class: what kind of thing the action is about. The equalizer
        // filters samples, and a video clip has none of its own here: the sound
        // is the audio lane's, clip for clip.
        ActionId::Equalizer => match ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Audio => Enable::Hidden("this clip is picture"),
            _ => Enable::Yes,
        },
        // A grade is a picture setting and an audio clip has no picture. A fit
        // policy is a picture setting for the same reason.
        ActionId::Color | ActionId::Fit => match ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Video => Enable::Hidden("this clip is sound"),
            _ => Enable::Yes,
        },
        // The scan reads samples, and a still has none -- ever, unlike a video
        // clip whose sound may be one lane down or simply silent. Exactly what
        // `unscannable` says after the fact, said before the row is drawn so
        // there is no row left to click.
        ActionId::Silence if ctx.image => Enable::Hidden("this clip is a still"),
        // -- state: true of this clip now, and the next playhead click or the
        // next selection changes the answer. Splits this clip only from inside
        // it: at either edge there is nothing to split off -- and, on a speeded
        // clip, only at a frame its own rate can address, which is the same
        // question `splittable` asks.
        ActionId::Cut => match ctx.clip {
            Some((clip, _)) if !(clip.start < ctx.playhead && ctx.playhead < clip.end()) => {
                Enable::No("only from inside a clip")
            }
            // Inside it, and still not a cut: a slowed clip shows one frame of
            // the file for several frames of the timeline, and only the first of
            // those is a frame the file has ([`Speed::split_at`]). Cutting
            // between two showings of one frame would leave halves whose lengths
            // no longer add up, so it is refused -- and it says *that* rather
            // than repeating "inside a clip" at a playhead that plainly is.
            Some((clip, _))
                if clip
                    .speed
                    .split_at(clip.len(), ctx.playhead - clip.start)
                    .is_none() =>
            {
                Enable::No("this speed holds one frame here — step to the next")
            }
            _ => Enable::Yes,
        },
        // Rejoins whatever meets at the playhead, so it can mean something only
        // at an edge of this clip. Whether those two halves were ever one take
        // is the engine's question, and it words that refusal itself.
        ActionId::Regroup => match ctx.clip {
            Some((clip, _)) if ctx.playhead != clip.start && ctx.playhead != clip.end() => {
                Enable::No("only where two clips meet")
            }
            _ => Enable::Yes,
        },
        // Nothing to take apart in a clip that names no group at all. Whether
        // the group it names still has another half is the engine's question,
        // like the regroup above.
        ActionId::Detach => match ctx.clip {
            Some((clip, _)) if clip.link.is_none() => Enable::No("this clip is not grouped"),
            _ => Enable::Yes,
        },
        // The three that act on the marked clip and on nothing else: with none
        // marked they would silently do nothing, which is what the Delete
        // button's own dimming has always said.
        ActionId::Copy | ActionId::Delete | ActionId::Lift if ctx.clip.is_none() => {
            Enable::No("click a clip first")
        }
        ActionId::Paste if !ctx.clipboard => Enable::No("nothing copied yet"),
        // Nothing to draw over the picture, so nothing to switch off: the
        // library says how subtitles arrive, and this row would flip a state
        // with no visible half either way.
        //
        // The × on a palette row is refused by this very fact and asks this very
        // arm ([`Player::remove_subtitle_track`]): an empty list is a list with
        // no row to take off, said in the same words the toggle says it in.
        ActionId::ToggleSubtitles if !ctx.subtitles => Enable::No("no subtitles yet"),
        // Not `No`: with nothing running there is no export for this to be
        // about at all, which is what keeps `esc` -- the same key -- a quiet
        // way out of a card rather than a line about exports.
        ActionId::CancelExport => Enable::Hidden("nothing is exporting"),
        // A clock started against an empty timeline is a clock counting
        // nothing: the transport says so by being dim, which is what its own
        // ad-hoc boolean used to say before the oracle knew the question.
        ActionId::Play if !ctx.playable => Enable::No("put a clip on a lane first"),
        // A rate applies to a clip of either kind and to its whole group, so
        // there is no lane it means nothing on, and the engine words the one
        // refusal there is (no room). Everything else is the editor's own and
        // needs nothing but a timeline.
        _ => Enable::Yes,
    }
}

/// The rows a clip menu draws, for the clip it was opened on: the registry
/// filtered by the one availability oracle, and the *only* way that menu is
/// built. An action that means nothing for what was right-clicked -- a grade on
/// a waveform, an equalizer on a picture -- is not a row at all, so a future
/// action cannot appear where it does not apply by being added to
/// [`MENU_ITEMS`] alone.
pub(crate) fn menu_items(ctx: Ctx) -> Vec<ActionId> {
    MENU_ITEMS
        .into_iter()
        .filter(|&action| enable(action, ctx).listed())
        .collect()
}

/// What a library row's items are asked *about*: the file that was
/// right-clicked and the little of the editor's state the answers need. The
/// library's [`Ctx`], handed in for the same reason.
#[derive(Clone, Copy, Default)]
pub(crate) struct RowCtx {
    /// A timeline to put it on.
    pub(crate) timeline: bool,
    pub(crate) exporting: bool,
    /// This row can join *this* timeline: what greys it in the list, and what
    /// the engine would otherwise refuse the Add with after the click.
    pub(crate) usable: bool,
    /// How many clips play this exact row -- a source with any is one the
    /// engine will not take out of the list.
    pub(crate) placed: usize,
}

/// Whether a library row's item can be asked for. The library's half of
/// [`enable`], and the same rule: one table, no second policy in the render.
pub(crate) fn row_enable(item: RowItem, ctx: RowCtx) -> Enable {
    match item {
        // The two that change the timeline, so an export reading it stops them
        // both -- the key handler's rule, applied to a menu.
        RowItem::Add | RowItem::Remove if ctx.exporting => Enable::No("an export is running"),
        RowItem::Add | RowItem::Remove if !ctx.timeline => Enable::No("no timeline open"),
        // Dimmed and saying why rather than clicked and refused afterwards: the
        // row's own grey already says the file cannot join this timeline.
        RowItem::Add if !ctx.usable => Enable::No("it cannot join this one"),
        RowItem::Remove if ctx.placed > 0 => Enable::No("clips play it"),
        // Neither of these touches the timeline: a file can be found on disk and
        // described whatever the editor is doing, with no timeline at all.
        _ => Enable::Yes,
    }
}

/// The rows a library menu draws, for the row it was opened on -- the clip
/// menu's [`menu_items`] on the other panel, and the only way that menu is
/// built.
pub(crate) fn row_items(ctx: RowCtx) -> Vec<RowItem> {
    ROW_ITEMS
        .into_iter()
        .filter(|&item| row_enable(item, ctx).listed())
        .collect()
}

/// How tall a menu's list may draw: the whole of it where the window has room,
/// and what the window has where it has not -- only then does the list scroll.
/// A cap fixed at twelve rows put the last items behind a scroll on a window
/// with room to spare, which reads as a menu cut off by the bottom edge.
pub(crate) fn menu_rows_h(rows: usize, viewport: Size<Pixels>) -> f32 {
    let room = f32::from(viewport.height) - MENU_PAD * 2.;
    (rows as f32 * MENU_ROW_H).min(room.max(MENU_ROW_H))
}

/// Where the menu actually hangs: at the pointer, pulled back inside the window
/// when it would not fit -- an item off the bottom edge is an item nobody can
/// click. Never negative, so a window smaller than the menu loses the bottom of
/// it rather than the top, where the items are.
pub(crate) fn menu_at(at: Point<Pixels>, viewport: Size<Pixels>, height: f32) -> (f32, f32) {
    let fit = |v: f32, size: f32, room: f32| v.min(room - size).max(0.);
    (
        fit(f32::from(at.x), MENU_W, f32::from(viewport.width)),
        fit(f32::from(at.y), height, f32::from(viewport.height)),
    )
}

/// Whether the Add button does anything: a row picked, a timeline to put it on,
/// and no export reading that timeline. A button that would do nothing is dimmed
/// and takes no click, like every other one here.
pub(crate) fn can_add(picked: Option<&(PathBuf, usize)>, timeline: bool, exporting: bool) -> bool {
    picked.is_some() && timeline && !exporting
}
