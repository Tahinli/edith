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
#[derive(Clone, Default)]
pub(crate) struct Ctx {
    /// The clip the question is about -- the one a menu was opened on, or the
    /// marked one. `None` means the question is about the editor as a whole,
    /// and the clip-relative answers stand aside: those actions find their own
    /// clip under the playhead and word their own refusal.
    pub(crate) clip: Option<(Clip, Lane)>,
    /// A gap is picked ([`Player::gap_pick`]): Delete closes it, whether or
    /// not any clip is marked.
    pub(crate) gap: bool,
    /// A gap is picked or the playhead stands in one ([`Player::gap_here`]).
    pub(crate) gap_here: bool,
    /// Some clip is under the playhead.
    pub(crate) clip_here: bool,
    /// Transcript words are picked and the transcript is up to date, so the words can be cut.
    pub(crate) words: bool,
    /// A render preview is being made ([`Player::render_preview`]), so there is one to stop.
    pub(crate) rendering: bool,
    /// A library item is selected (or was last previewed) to edit from.
    pub(crate) asset: bool,
    /// What was clicked is a placed caption and not a clip: a subtitle lane
    /// holds no `Clip` at all, so [`Ctx::clip`] is `None` there and the
    /// clip-relative answers would read it as "nothing marked". Its own flag,
    /// so the one action a caption has -- coming off the lane again -- is a
    /// live row while the rest are not rows at all.
    pub(crate) caption: bool,
    /// The clip plays a still ([`engine::is_image`]), which has no sound to
    /// reach at all -- not the lane's business, because a still sits on a video
    /// lane exactly like a take whose sound is one lane down.
    pub(crate) image: bool,
    /// The clip's source is known, from the probed stream table
    /// ([`Player::streams`]), to carry no audio track at all -- a video shot
    /// mute, not merely a video whose sound is quiet. `false` (the default)
    /// covers both "has sound" and "not probed yet": a row nobody has asked
    /// about is not one this hides a working action from.
    pub(crate) no_sound: bool,
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
    /// How many placements the selection holds, and on how many distinct
    /// lanes -- the manual group's two questions in one reading. Picks that
    /// name nothing (a stroke moved the indices under them) count as none.
    pub(crate) picks: usize,
    pub(crate) pick_lanes: usize,
    /// The group id of the marked caption, when what is marked is one: the
    /// detach question, asked over a box the clip index space cannot name.
    pub(crate) caption_link: Option<u32>,
    /// What the question is about sits on a locked lane: the clip the menu was
    /// opened on or the marked one, or the caption. The edits that would change
    /// it are refused with [`LOCKED_REASON`] (DESIGN §7); the answers that only
    /// read it are not.
    pub(crate) locked: bool,
    /// The edit history is empty on that side: nothing for undo to take back,
    /// nothing for redo to walk forward. Negative on purpose, so a default
    /// `Ctx` keeps both live and only a real reading dims them.
    pub(crate) nothing_to_undo: bool,
    pub(crate) nothing_to_redo: bool,
    /// The clip's group holds no other placement (its link names only itself),
    /// so there is nothing for Ungroup to take it out of. Negative like the
    /// two above.
    pub(crate) alone: bool,
    /// No lane has two halves of one take meeting at the playhead, so Regroup
    /// would find no cut to close. Negative like the above.
    pub(crate) no_join: bool,
    /// No clip on another track covers exactly this clip's frames, so a lone
    /// Group has no partner to take it to. Negative like the above.
    pub(crate) no_partner: bool,
    /// The clip the question is about plays another sequence (a compound clip).
    pub(crate) compound: bool,
    /// The clip plays a multicam source ([`engine::project::Project::multicam_of`]).
    pub(crate) multicam: bool,
    /// How many library rows are picked, and whether the angle monitor is on.
    pub(crate) media_picks: usize,
    pub(crate) monitor: bool,
    /// The angle count of the multicam clip under the playhead; 0 with none there.
    pub(crate) angles_here: usize,
    /// How many sequences the project holds, and whether the open one is played
    /// by a compound clip somewhere (so cannot be deleted).
    pub(crate) sequences: usize,
    pub(crate) sequence_used: bool,
    /// The open sequence was entered from a compound clip: there is a parent to go back to.
    pub(crate) parent: bool,
}

/// The refusal every edit to a locked lane's item carries, in the words the
/// toast and the dimmed row both say.
pub(crate) const LOCKED_REASON: &str = "The track is locked";

/// Whether `action` changes the clip or caption it is asked about: the ones a
/// lock refuses. Selection, playback, the playhead and the editor's own
/// switches are not in it -- a locked lane still plays and still reads.
fn edits_placement(action: ActionId) -> bool {
    matches!(
        action,
        ActionId::Cut
            | ActionId::Regroup
            | ActionId::Detach
            | ActionId::Group
            | ActionId::Delete
            | ActionId::Lift
            | ActionId::Paste
            | ActionId::PasteAttributes
            | ActionId::PasteColor
            | ActionId::PasteEffects
            | ActionId::PasteTransform
            | ActionId::PasteSpeed
            | ActionId::PasteAudio
            | ActionId::NudgeBack
            | ActionId::NudgeForward
            | ActionId::NudgeBack10
            | ActionId::NudgeForward10
            | ActionId::ExtendAudio
            | ActionId::ExtendVideo
            | ActionId::Crossfade
            | ActionId::Dissolve
            | ActionId::TransitionDissolve
            | ActionId::TransitionDipBlack
            | ActionId::TransitionDipWhite
            | ActionId::TransitionWipeLeft
            | ActionId::TransitionWipeRight
            | ActionId::TransitionWipeUp
            | ActionId::TransitionWipeDown
            | ActionId::TransitionSlideLeft
            | ActionId::TransitionSlideRight
            | ActionId::TransitionSlideUp
            | ActionId::TransitionSlideDown
            | ActionId::TransitionPushLeft
            | ActionId::TransitionPushRight
            | ActionId::TransitionPushUp
            | ActionId::TransitionPushDown
            | ActionId::TransitionZoom
            | ActionId::Color
            | ActionId::Fit
            | ActionId::Reframe
            | ActionId::TrackMask
            | ActionId::NestClips
            | ActionId::BreakApart
            | ActionId::Transform
            | ActionId::Effects
            | ActionId::KeyPlacement
            | ActionId::KeyOpacity
            | ActionId::KeyColor
            | ActionId::KeyEasing
            | ActionId::Equalizer
            | ActionId::Silence
            | ActionId::Reverse
            | ActionId::SpeedRampUp
            | ActionId::SpeedRampDown
            | ActionId::SpeedRampSlowFastSlow
            | ActionId::SpeedRampFastSlowFast
            | ActionId::SpeedRampKey
            | ActionId::SpeedRampClear
            | ActionId::Mix
            | ActionId::CleanVoice
            | ActionId::VoiceDynamics
            | ActionId::TrimIn
            | ActionId::TrimOut
            | ActionId::TrimInToPlayhead
            | ActionId::TrimOutToPlayhead
            | ActionId::RippleTrimInToPlayhead
            | ActionId::RippleTrimOutToPlayhead
            | ActionId::RollBack
            | ActionId::RollForward
            | ActionId::SlipBack
            | ActionId::SlipForward
            | ActionId::SlideBack
            | ActionId::SlideForward
            | ActionId::LoopTrim
    )
}

/// Whether `action` can be asked for, on `ctx`. One arm per action and nothing
/// else in the editor asks the question: the clip menu dims a row with this,
/// the actions card dims a row with this, and the two can never come to
/// disagree about what an action needs -- exactly the reason [`Player::act`] is
/// one table too.
pub(crate) fn enable(action: ActionId, ctx: Ctx) -> Enable {
    if action == ActionId::CancelRender && !ctx.rendering {
        return Enable::No("nothing is rendering");
    }
    if action == ActionId::CutSelectedWords && !ctx.words {
        return Enable::No("no transcript words picked");
    }
    // The one action that is about the editor rather than about the timeline:
    // the list of what everything does, and where a key is changed. An empty
    // window has no clips and still has keys -- so this answers ahead of the
    // timeline question, and only an export shuts it (a waiting row would
    // swallow the escape the progress line promises cancels the export).
    // The window's own colours: there is always a window, and repainting it
    // touches no timeline -- so this one is live even while an export is
    // reading one, which is the only state that dims the list above.
    // ...and whether that window fills the screen: also always there, also
    // touching no timeline, live through an export the same way.
    // ...and the cue plate's own font and size: a reading preference like the
    // theme, never burned into an export, live the same way.
    //
    // Settings is not in this list, on purpose: it opens the same modal slot
    // the exporting card owns (`Player::open_settings`, one card at a time),
    // so a permission this oracle granted here would be a permission nobody
    // could ever observe. It is refused below, with every other card.
    if action == ActionId::Theme
        || action == ActionId::AccentColour
        || action == ActionId::Fullscreen
        || action == ActionId::SubtitleStyle
        // Moving the keyboard ring is the same kind of always-there,
        // touches-no-timeline action as the three above: there is always a
        // dock/bench/inspector to land the ring on, export or no.
        || action == ActionId::FocusPanels
        // The shortcuts sheet is the same kind of door: it opens and closes
        // a tab and touches no timeline, so it is live through an export too.
        || action == ActionId::ShowKeys
        || action == ActionId::About
        // The viewer's own look (zoom, playback quality, guides, mask): view state, no timeline.
        || action == ActionId::ViewZoom
        || action == ActionId::PlaybackQuality
        || action == ActionId::GuideStyle
        || action == ActionId::GuideMask
        // The video output window and its display: a second window onto the picture, no timeline.
        || action == ActionId::ToggleOutputWindow
        || action == ActionId::OutputDisplay
    {
        return Enable::Yes;
    }
    if action == ActionId::Settings {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    if action == ActionId::ShowActions {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    // The six that are about the *editor* and its monitoring rather than
    // about the edit list: they work with nothing open, the keyboard has
    // always fired them there, and so their buttons are live there too.
    //
    // The two stand-in switches are here because they are *import* options:
    // what an arriving film gets made for it, and what the picture is decoded
    // from once it has. A person who wants no encode started decides that
    // before the first file, and a switch that could only be set once a film
    // was already in was a switch that came too late to mean anything.
    if matches!(
        action,
        ActionId::ToggleSnap
            | ActionId::ToggleMute
            | ActionId::VolumeUp
            | ActionId::VolumeDown
            | ActionId::ToggleProxies
            | ActionId::ToggleAutoProxies
            // The project settings decide what the *next* file scaffolds as
            // (`Player::pending_settings`), so refusing them before the first
            // file was refusing them exactly when they mean the most.
            | ActionId::Resolution
            // The dock's import pair: how a first file gets in at all, so
            // both are live with no timeline open, exactly like the two
            // stand-in switches above.
            | ActionId::AddFiles
            | ActionId::PasteFilePath
            // The way into a project from nothing: live exactly when there is none.
            | ActionId::NewProject
            // Opening a project file replaces whatever is open, so it is live with or without one.
            | ActionId::OpenProject
    ) {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    // Export writes what is on the timeline, so an empty one has nothing to write. The
    // export that is already running answers for itself below (its progress button stays live).
    if action == ActionId::Export && !ctx.exporting && !ctx.playable {
        return Enable::No("the timeline is empty");
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
    // A lock is the one refusal about the item itself rather than the editor:
    // the item is there and can be looked at, only the edits that would change
    // it are turned away, and say why in the toast's own words.
    if ctx.locked && edits_placement(action) {
        return Enable::No(LOCKED_REASON);
    }
    // A caption is not a clip: the actions that act on *the marked clip and
    // nothing else* have nothing to act on over a placed subtitle, so they are
    // refusals of kind -- not rows at all, rather than rows saying "click a clip
    // first" over a clicked caption. Delete is the one that means something: it
    // routes to [`Player::lift_sub`] when what is marked is a caption, and so
    // are Group and Detach now that a caption may be grouped by hand -- the one
    // over the selection the menu was opened on, the other over the group the
    // marked caption is in.
    //
    // And it is *only* those. The playhead actions are about the timeline rather
    // than about what is marked -- the split and the regroup act at the line
    // ([`Player::cut`], [`Player::regroup`]), and the grade, the rate, the fit
    // and the scan fall back to the clip under it -- so a caption in hand is no
    // reason for them to stop. Hiding them here made them dead *and* silent
    // ([`Player::act`] returns without a word on `Hidden`) from the moment a
    // caption was clicked until something else was, which no click on the bed
    // undoes: there is no handler there to clear the mark.
    if ctx.caption
        && matches!(
            action,
            ActionId::Copy
                | ActionId::Delete
                | ActionId::Lift
                | ActionId::Equalizer
                | ActionId::NestClips
                | ActionId::BreakApart
                | ActionId::OpenCompound
        )
    {
        return match action {
            ActionId::Delete => Enable::Yes,
            _ => Enable::Hidden("this is a caption"),
        };
    }
    // A compound clip plays another sequence: its own rate, grade, placement and the scans of a
    // file are not applied to it (grade the clips inside), so those rows are not offered.
    if ctx.compound
        && ctx.clip.is_some()
        && matches!(
            action,
            ActionId::Speed
                | ActionId::SpeedRampUp
                | ActionId::SpeedRampDown
                | ActionId::SpeedRampSlowFastSlow
                | ActionId::SpeedRampFastSlowFast
                | ActionId::SpeedRampKey
                | ActionId::SpeedRampClear
                | ActionId::Reverse
                | ActionId::Color
                | ActionId::AutoColour
                | ActionId::SaveLook
                | ActionId::Transform
                | ActionId::Effects
                | ActionId::KeyPlacement
                | ActionId::KeyOpacity
                | ActionId::KeyColor
                | ActionId::KeyEasing
                | ActionId::Fit
                | ActionId::Equalizer
                | ActionId::Silence
                | ActionId::Visualizer
                | ActionId::Reframe
                | ActionId::TrackMask
                | ActionId::Mix
                | ActionId::CleanVoice
                | ActionId::VoiceDynamics
        )
    {
        return Enable::Hidden("a compound clip plays a sequence");
    }
    match action {
        ActionId::Undo if ctx.nothing_to_undo => return Enable::No("nothing to undo"),
        ActionId::Redo if ctx.nothing_to_redo => return Enable::No("nothing to redo"),
        _ => {}
    }
    match action {
        // -- class: what kind of thing the action is about. The equalizer
        // filters samples, and a video clip has none of its own here: the sound
        // is the audio lane's, clip for clip.
        // The crossfade is the sound join, for the same reason: it fades one
        // clip's samples into the next one's, and a picture has none.
        ActionId::Equalizer | ActionId::Crossfade => match ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Audio => {
                Enable::Hidden("this clip is picture")
            }
            _ => Enable::Yes,
        },
        // A grade is a picture setting and an audio clip has no picture. A fit
        // policy is a picture setting for the same reason.
        // The dissolve is the picture join and the crossfade's mirror: two
        // pictures cross, and there is no picture on a waveform to cross.
        a if matches!(
            a,
            ActionId::Color
                | ActionId::Fit
                | ActionId::Transform
                | ActionId::Effects
                | ActionId::KeyPlacement
                | ActionId::KeyOpacity
                | ActionId::KeyColor
                | ActionId::KeyEasing
                | ActionId::Dissolve
        ) || a.transition_kind().is_some() =>
        {
            match ctx.clip {
                Some((_, lane)) if lane.kind != LaneKind::Video => {
                    Enable::Hidden("this clip is sound")
                }
                _ => Enable::Yes,
            }
        }
        // The scan reads samples, and a still has none -- ever, unlike a video
        // clip whose sound may be one lane down or simply silent. Exactly what
        // `unscannable` says after the fact, said before the row is drawn so
        // there is no row left to click. The mixer's track volumes are the
        // same class refusal over the same clip: nothing to mix for a picture
        // with no sound at all.
        // Beats need a sound to listen to: a picture-only clip is refused by name, dimmed.
        ActionId::MarkBeats if ctx.image || ctx.no_sound => Enable::No("Pick a clip with sound"),
        ActionId::Silence | ActionId::Mix | ActionId::CleanVoice | ActionId::VoiceDynamics
            if ctx.image => Enable::Hidden("this clip is a still"),
        // A still has no footage to run the other way.
        ActionId::Reverse if ctx.image => Enable::Hidden("this clip is a still"),
        // Sequences: always there with a timeline, bar the cap and the rules
        // the engine would refuse them by.
        ActionId::NewSequence | ActionId::DuplicateSequence if ctx.sequences >= engine::project::MAX_SEQUENCES => {
            Enable::No("the project holds as many sequences as it can")
        }
        ActionId::NewSequence | ActionId::DuplicateSequence | ActionId::RenameSequence => Enable::Yes,
        ActionId::DeleteSequence if ctx.sequences <= 1 => Enable::No("this is the only sequence"),
        ActionId::DeleteSequence if ctx.sequence_used => Enable::No("a compound clip plays this sequence"),
        ActionId::DeleteSequence => Enable::Yes,
        ActionId::CloseCompound if !ctx.parent => Enable::No("this sequence was not opened from a compound clip"),
        ActionId::CloseCompound => Enable::Yes,
        ActionId::ZoomToSelection if ctx.picks == 0 => Enable::No("Select a clip first"),
        ActionId::NestClips if ctx.picks == 0 && ctx.clip.is_none() => Enable::No("select the clips to nest"),
        ActionId::NestClips => Enable::Yes,
        ActionId::CreateMulticam if ctx.media_picks < 2 => Enable::No("Needs two or more picked clips"),
        ActionId::CreateMulticam => Enable::Yes,
        ActionId::FlattenMulticam if ctx.multicam => Enable::Yes,
        ActionId::FlattenMulticam => match ctx.clip {
            None => Enable::No("select a multicam clip"),
            Some(_) => Enable::Hidden("not a multicam clip"),
        },
        ActionId::ToggleAngleMonitor | ActionId::ToggleWaveforms | ActionId::ToggleScrubAudio | ActionId::CompareOff | ActionId::CompareSplit | ActionId::CompareSide => Enable::Yes,
        // The number keys are the monitor's: silent anywhere else, so a digit is never eaten.
        ActionId::Angle1
        | ActionId::Angle2
        | ActionId::Angle3
        | ActionId::Angle4
        | ActionId::Angle5
        | ActionId::Angle6
        | ActionId::Angle7
        | ActionId::Angle8
        | ActionId::Angle9
            if !ctx.monitor || ctx.angles_here == 0 =>
        {
            Enable::Hidden("the angle monitor is off or no multicam clip is under the playhead")
        }
        ActionId::Angle1
        | ActionId::Angle2
        | ActionId::Angle3
        | ActionId::Angle4
        | ActionId::Angle5
        | ActionId::Angle6
        | ActionId::Angle7
        | ActionId::Angle8
        | ActionId::Angle9 => {
            let n = action.angle_number().unwrap_or(0);
            if n > ctx.angles_here { Enable::No("this multicam clip has fewer angles") } else { Enable::Yes }
        }
        ActionId::BreakApart | ActionId::OpenCompound => match ctx.clip {
            None => Enable::No("select a compound clip"),
            Some(_) if !ctx.compound => Enable::Hidden("not a compound clip"),
            Some(_) => Enable::Yes,
        },
        // Tracking follows a mask's box through footage, so it needs a picture clip with a mask on it.
        ActionId::TrackMask => match &ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Video => Enable::Hidden("this clip is sound"),
            Some((c, _)) if !c.effects.iter().any(|e| matches!(e.kind, engine::effects::EffectKind::RectMask | engine::effects::EffectKind::EllipseMask)) => {
                Enable::No("no rect or ellipse mask")
            }
            Some(_) => Enable::Yes,
            None => Enable::No("select a video clip"),
        },
        // Reframing follows a subject through footage: a still has none to follow.
        ActionId::Reframe => match ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Video => Enable::Hidden("this clip is sound"),
            Some(_) if ctx.image => Enable::Hidden("this clip is a still"),
            Some(_) => Enable::Yes,
            None => Enable::No("select a video clip"),
        },
        ActionId::Visualizer if ctx.image => Enable::Hidden("this clip is a still"),
        ActionId::Visualizer if ctx.no_sound => Enable::Hidden("this clip has no sound"),
        ActionId::Visualizer => match ctx.clip {
            Some(_) => Enable::Yes,
            None => Enable::No("select a clip with sound"),
        },
        ActionId::Silence | ActionId::Mix | ActionId::CleanVoice | ActionId::VoiceDynamics
            if ctx.no_sound => {
            Enable::Hidden("this clip has no sound")
        }
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
            Some((clip, _)) if !clip.splits_at(ctx.playhead) => Enable::No(match clip.speed_keys.is_empty() {
                true => "this speed holds one frame here",
                false => "this speed ramp cannot be cut at this frame",
            }),
            _ => Enable::Yes,
        },
        // Rejoins whatever meets at the playhead, so it can mean something only
        // at an edge of this clip. Whether those two halves were ever one take
        // is the engine's question, and it words that refusal itself.
        ActionId::Regroup => match ctx.clip {
            Some((clip, _)) if ctx.playhead != clip.start && ctx.playhead != clip.end() => {
                Enable::No("only where two clips meet")
            }
            Some(_) if ctx.no_join => Enable::No("no cut under the playhead to close"),
            _ => Enable::Yes,
        },
        // Nothing to take apart in a placement that names no group at all --
        // a clip by its own field, a caption by the one the mark carries.
        // Whether the group it names still has another half is the engine's
        // question, like the regroup above.
        ActionId::Detach => match (ctx.clip, ctx.caption) {
            (Some((clip, _)), _) if clip.link.is_none() || ctx.alone => {
                Enable::No("this clip is not grouped")
            }
            (None, true) if ctx.caption_link.is_none() => Enable::No("this caption is not grouped"),
            _ => Enable::Yes,
        },
        // The manual group: a *selection* of placements, at most one per lane.
        // Two picks or more, all on their own lanes, is a group waiting to be
        // made; a lane picked twice is the one thing a group may never hold;
        // and a single pick is the partner hunt it always was, which the engine
        // words itself.
        ActionId::Group => match ctx.picks {
            2.. if ctx.pick_lanes == ctx.picks => Enable::Yes,
            2.. => Enable::No("a lane is picked twice"),
            _ if ctx.clip.is_some() && ctx.no_partner => {
                Enable::No("needs a clip on another track at the same time")
            }
            _ => Enable::Yes,
        },
        // The three that act on the marked clip and on nothing else: with none
        // marked they would silently do nothing, which is what the Delete
        // button's own dimming has always said.
        // Delete over a picked gap closes the gap, so it is live with nothing
        // marked (DESIGN §7); the clip-only rows beside it stay refused.
        ActionId::Delete if ctx.gap => Enable::Yes,
        ActionId::Copy | ActionId::Delete | ActionId::Lift if ctx.clip.is_none() => {
            Enable::No("nothing selected")
        }
        // The cut machinery (DESIGN.md §6): the odometer, the trim pair and
        // the loop-trim all act on the subject cut, and with none marked
        // there is nothing to walk, trim or loop -- named the same way the
        // three above it refuse an empty mark.
        ActionId::WalkCutNext
        | ActionId::WalkCutPrev
        | ActionId::WalkCutNext10
        | ActionId::WalkCutPrev10
        | ActionId::TrimIn
        | ActionId::TrimOut
        | ActionId::LoopTrim
            if ctx.clip.is_none() =>
        {
            Enable::No("no clip is selected")
        }
        // Slip shifts which part of the file a clip shows, and a still has no
        // more of a file to show -- a class refusal like the scan's.
        ActionId::SlipBack | ActionId::SlipForward if ctx.image => {
            Enable::Hidden("this clip is a still")
        }
        // The pro trims (roll, slip, slide) act on the subject clip and its
        // neighbours; whether a neighbour is there is the engine's question,
        // and it words that refusal itself.
        ActionId::RollBack
        | ActionId::RollForward
        | ActionId::SlipBack
        | ActionId::SlipForward
        | ActionId::SlideBack
        | ActionId::SlideForward
            if ctx.clip.is_none() =>
        {
            Enable::No("no clip is selected")
        }
        // Trim-to-playhead (debt #42): the keyboard's own version of the
        // pointer's drag-the-edge-to-a-spot trim, so it needs the two things
        // that gesture needs -- a subject cut, and the playhead actually
        // resting somewhere on it, or the "spot" the edge would snap to is
        // not on the clip at all.
        ActionId::TrimInToPlayhead
        | ActionId::TrimOutToPlayhead
        | ActionId::RippleTrimInToPlayhead
        | ActionId::RippleTrimOutToPlayhead => match ctx.clip {
            None => Enable::No("no clip is selected"),
            Some((clip, _)) if ctx.playhead < clip.start || ctx.playhead > clip.end() => {
                Enable::No("the playhead is off this clip")
            }
            _ => Enable::Yes,
        },
        ActionId::Paste if !ctx.clipboard => Enable::No("nothing copied yet"),
        ActionId::CloseGap if !ctx.gap_here => Enable::No("no gap under the playhead"),
        ActionId::Freeze | ActionId::MatchFrame if !ctx.clip_here => Enable::No("nothing under the playhead"),
        ActionId::ReplaceEdit if !ctx.clip_here => Enable::No("nothing under the playhead"),
        ActionId::ReplaceEdit if !ctx.asset => Enable::No("no library item selected"),
        ActionId::PasteAttributes
        | ActionId::PasteColor
        | ActionId::PasteEffects
        | ActionId::PasteTransform
        | ActionId::PasteSpeed
        | ActionId::PasteAudio
            if !ctx.clipboard =>
        {
            Enable::No("nothing copied yet")
        }
        ActionId::PasteAttributes
        | ActionId::PasteColor
        | ActionId::PasteEffects
        | ActionId::PasteTransform
        | ActionId::PasteSpeed
        | ActionId::PasteAudio
        | ActionId::NudgeBack
        | ActionId::NudgeForward
        | ActionId::NudgeBack10
        | ActionId::NudgeForward10
            if ctx.picks == 0 && ctx.clip.is_none() =>
        {
            Enable::No("no clips selected")
        }
        ActionId::ExtendAudio | ActionId::ExtendVideo => {
            let want = if action == ActionId::ExtendAudio { LaneKind::Audio } else { LaneKind::Video };
            match ctx.clip {
                None => Enable::No("no clip selected"),
                Some((_, lane)) if lane.kind == want => Enable::Yes,
                Some((clip, _)) if clip.link.is_none() || ctx.alone => Enable::No(match want {
                    LaneKind::Audio => "select the sound clip, or group it with the picture",
                    _ => "select the picture clip, or group it with the sound",
                }),
                Some(_) => Enable::Yes,
            }
        }
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
        // Play is refused while an export runs (`toggle_or_restart`), and so
        // are the shuttle's forward and pause: the same clock, the same reason.
        ActionId::ShuttleStop | ActionId::ShuttleBack | ActionId::ShuttleForward if ctx.exporting => {
            Enable::No("an export is running")
        }
        ActionId::Play | ActionId::Loop | ActionId::PlayAround | ActionId::ShuttleBack | ActionId::ShuttleForward
            if !ctx.playable => {
            Enable::No("the timeline is empty")
        }
        // A rate applies to a clip of either kind and to its whole group, so
        // there is no lane it means nothing on, and the engine words the one
        // refusal there is (no room). Everything else is the editor's own and
        // needs nothing but a timeline.
        _ => Enable::Yes,
    }
}

/// Whether a lane-header verb applies to this particular lane as well as to
/// the editor state. The shared action oracle still owns timeline/export
/// refusal; this narrow gate prevents a header from borrowing a type-level
/// chord for the wrong kind of lane.
pub(crate) fn enable_lane(action: ActionId, lane: Lane, ctx: Ctx) -> Enable {
    match (action, lane.kind) {
        (ActionId::Mix, LaneKind::Audio)
        | (ActionId::RemoveVideoLane, LaneKind::Video)
        | (ActionId::RemoveAudioLane, LaneKind::Audio) => enable(action, ctx),
        (ActionId::RenameLane, _) => Enable::Yes,
        (ActionId::TrackSmall | ActionId::TrackMedium | ActionId::TrackLarge, _) => Enable::Yes,
        _ => Enable::Hidden("this verb does not apply to this lane"),
    }
}

/// The rows a lane head's menu draws: the three tracks a project can gain,
/// and -- under the rule line the render draws before it ([`destructive`]) --
/// the one remove that means anything on the lane that was right-clicked. A
/// video head never offers to take an audio track off: DESIGN §9's "verbs of
/// the thing under the cursor", read literally.
///
/// The removes take the *last* track of the kind (the engine's own rule,
/// which their labels say), so this lists at most one of them and never a
/// second row arguing about the same act.
pub(crate) fn lane_items(lane: Lane) -> Vec<ActionId> {
    let mut items = HEAD_ADDS.to_vec();
    // The verbs that used to hang off the head itself (cleanse round 2): the
    // head is a dot and a name now, and this menu is where the pointer reaches
    // its mix -- an audio lane's alone, the way `enable_lane` reads it.
    if lane.kind == LaneKind::Audio {
        items.push(ActionId::Mix);
    }
    items.push(ActionId::RenameLane);
    // This track's own height, three radio rows: the same three sizes the
    // Timeline menu sets for every track at once (`overlays` words them for one).
    items.extend([ActionId::TrackSmall, ActionId::TrackMedium, ActionId::TrackLarge]);
    items.push(match lane.kind {
        LaneKind::Video => ActionId::RemoveVideoLane,
        LaneKind::Audio => ActionId::RemoveAudioLane,
        LaneKind::Subtitle => ActionId::RemoveSubtitleLane,
    });
    items
}

/// The rows a clip menu draws, for the clip it was opened on: the registry
/// filtered by the one availability oracle, and the *only* way that menu is
/// built. An action that means nothing for what was right-clicked -- a grade on
/// a waveform, an equalizer on a picture -- is not a row at all, so a future
/// action cannot appear where it does not apply by being added to
/// [`MENU_ITEMS`] alone.
///
/// Being *live* and *belonging in this menu* are two different questions, and
/// only this one is the menu's. The playhead actions stay live over a marked
/// caption -- [`enable`] says so, and the strokes and the toolbar read that --
/// but a split, a rate, a grade or a fit listed under a right-clicked caption
/// reads as something the caption is about to get, when what they reach is the
/// video clip the playhead is standing in. So a caption's menu carries the rows
/// that are the caption's -- its removal ([`Player::lift_sub`], reached through
/// Delete) and, since a caption may be grouped by hand, the group pair over the
/// selection and its own group -- and the rows the card already prints
/// "(global)" beside, which say for themselves that they are not about what was
/// clicked.
pub(crate) fn menu_items(ctx: Ctx) -> Vec<ActionId> {
    MENU_ITEMS
        .into_iter()
        .filter(|&action| enable(action, ctx.clone()).listed())
        .filter(|&action| {
            !ctx.caption
                || matches!(
                    action,
                    ActionId::Delete
                        | ActionId::Paste
                        | ActionId::ToggleMute
                        | ActionId::Group
                        | ActionId::Detach
                )
        })
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
    /// A stand-in is being made or wound down, so the proxy row reads "Stop
    /// proxy" and its click stops it ([`RowItem::label_in`]).
    pub(crate) proxy_stops: bool,
    /// The source is starred, so the favourite row reads "Remove from favourites".
    pub(crate) starred: bool,
    /// The file is gone from disk: it can only be relinked, not played or added.
    pub(crate) offline: bool,
    /// The library rows picked for a multicam clip ([`Player::picked_sources`]).
    pub(crate) media_picks: usize,
}

/// Whether a library row's item can be asked for. The library's half of
/// [`enable`], and the same rule: one table, no second policy in the render.
pub(crate) fn row_enable(item: RowItem, ctx: RowCtx) -> Enable {
    match item {
        RowItem::Relink if !ctx.offline => Enable::Hidden("the file is not missing"),
        RowItem::Relink if ctx.exporting => Enable::No("an export is running"),
        RowItem::Relink => Enable::Yes,
        RowItem::CreateMulticam if ctx.media_picks < 2 => Enable::No("Needs two or more picked clips"),
        RowItem::CreateMulticam => Enable::Yes,
        // A file that is not on disk has nothing to play, add or copy.
        RowItem::Add | RowItem::Insert | RowItem::Preview | RowItem::Proxy if ctx.offline => {
            Enable::No("the file is missing")
        }
        // The two that change the timeline, so an export reading it stops them
        // both -- the key handler's rule, applied to a menu.
        RowItem::Favourite | RowItem::MoveToBin | RowItem::NewBin if ctx.exporting => {
            Enable::No("an export is running")
        }
        RowItem::Favourite | RowItem::MoveToBin | RowItem::NewBin if !ctx.timeline => {
            Enable::No("no timeline open")
        }
        RowItem::Add | RowItem::Insert | RowItem::Remove | RowItem::RemoveWithClips if ctx.exporting => {
            Enable::No("an export is running")
        }
        RowItem::Add | RowItem::Insert | RowItem::Remove | RowItem::RemoveWithClips if !ctx.timeline => {
            Enable::No("no timeline open")
        }
        // Dimmed and saying why rather than clicked and refused afterwards: the
        // row's own grey already says the file cannot join this timeline.
        RowItem::Add | RowItem::Insert if !ctx.usable => Enable::No("it cannot join this one"),
        // A file the engine cannot read has nothing to play and nothing to
        // make a stand-in from: hidden rather than dimmed, the way the two
        // removes are, because neither verb has a state that would light it.
        RowItem::Preview | RowItem::Proxy if !ctx.usable => Enable::Hidden("it cannot be read"),
        // The two removes are listed together, always (DESIGN §6.7): the plain
        // one is refused while a clip plays the file, and the clips-too one is
        // refused while none does, each saying why in the column where its
        // chord would be. Dimmed rather than hidden, so the pointer can see
        // both ways out and which one applies right now.
        RowItem::Remove if ctx.placed > 0 => Enable::No("It's used on the timeline"),
        RowItem::RemoveWithClips if ctx.placed == 0 => Enable::No("no clip uses it"),
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
/// with room to spare, which reads as a menu cut off by the bottom edge. Both
/// [`MENU_EDGE`] margins come off the room first, so the plate `menu_at` places
/// from this always fits between them whatever the anchor.
pub(crate) fn menu_rows_h(rows: usize, viewport: Size<Pixels>) -> f32 {
    let room = f32::from(viewport.height) - MENU_PAD * 2. - MENU_EDGE * 2.;
    (rows as f32 * MENU_ROW_H).min(room.max(MENU_ROW_H))
}

/// Where the menu actually hangs: at the pointer, pulled back inside the window
/// when it would not fit -- an item off the bottom edge is an item nobody can
/// click. All four edges, [`MENU_EDGE`] clear of each: a plate flush on the
/// frame reads as a list that was cut, not one that ended. Never past the top
/// or left edge, so a window smaller than the menu loses the bottom of it
/// rather than the top, where the items are.
#[cfg(test)]
pub(crate) fn menu_at(at: Point<Pixels>, viewport: Size<Pixels>, height: f32) -> (f32, f32) {
    menu_at_w(at, viewport, MENU_W, height)
}

/// [`menu_at`] for a plate that fits its content and so is `width` wide.
pub(crate) fn menu_at_w(
    at: Point<Pixels>,
    viewport: Size<Pixels>,
    width: f32,
    height: f32,
) -> (f32, f32) {
    let fit = |v: f32, size: f32, room: f32| v.min(room - MENU_EDGE - size).max(MENU_EDGE);
    (
        fit(f32::from(at.x), width, f32::from(viewport.width)),
        fit(f32::from(at.y), height, f32::from(viewport.height)),
    )
}
