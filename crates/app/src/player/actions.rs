//! Every action arrives here: the one dispatch, the notice queue, and the
//! delegations to the oracle that say whether an action can be asked for.

use super::editkeys::Part;
use super::timeline_edit::Switch;
use crate::*;

/// The name an action's step carries in Undo, Redo and the toast: its menu name without the
/// ellipsis a dialog-opening row wears. `None` for Undo and Redo, which take a step back
/// rather than make one, and for a name too long to read in a menu row.
pub(crate) fn step_name(action: ActionId) -> Option<&'static str> {
    let name = match action {
        ActionId::Undo | ActionId::Redo => return None,
        ActionId::ClipEnabled => "Enable/disable clip",
        ActionId::ClipLabel => "Set clip label",
        ActionId::NewSolid => "Add solid colour",
        ActionId::NewGradient => "Add gradient",
        ActionId::NewCountdown => "Add countdown",
        ActionId::NewBars => "Add colour bars",
        ActionId::NewTone => "Add tone",
        ActionId::NewBarsAndTone => "Add bars and tone",
        ActionId::NewAdjustment => "Add adjustment layer",
        ActionId::NewRectangle => "Add rectangle shape",
        ActionId::NewEllipse => "Add ellipse shape",
        ActionId::NewLine => "Add line shape",
        ActionId::NewArrow => "Add arrow shape",
        _ => action.label().trim_end_matches('\u{2026}'),
    };
    (name.chars().count() < 30).then_some(name)
}

/// What Undo or Redo says on a menu row and in a tooltip: the step it takes back or walks to
/// by name, plain "Undo" or "Redo" for a step nobody named, and "Nothing to undo" or
/// "Nothing to redo" when no step is left.
pub(crate) fn step_row_name(redo: bool, depth: usize, name: Option<&str>) -> String {
    let verb = if redo { "Redo" } else { "Undo" };
    match (depth, name) {
        (0, _) => format!("Nothing to {}", verb.to_lowercase()),
        (_, Some(name)) => format!("{verb} {name}"),
        (_, None) => verb.to_string(),
    }
}

/// The toast after a step is taken back or walked forward: "Undid Split", "Redid Split", or
/// plain "Undone" and "Redone" for a step nobody named.
pub(crate) fn step_toast(redo: bool, name: Option<&str>) -> String {
    match (redo, name) {
        (false, Some(name)) => format!("Undid {name}"),
        (true, Some(name)) => format!("Redid {name}"),
        (false, None) => "Undone".to_string(),
        (true, None) => "Redone".to_string(),
    }
}

impl Player {
    /// What Undo and Redo are called on a menu row or a toolbar tooltip: the action's own
    /// name, except that they name the step they take back or walk forward to
    /// ([`step_row_name`]).
    pub(crate) fn action_name(&self, action: ActionId) -> String {
        let session = self.session.as_ref();
        match action {
            ActionId::Undo => {
                let depth = session.map_or(0, |s| s.undo_depth());
                step_row_name(false, depth, session.and_then(|s| s.undo_label()))
            }
            ActionId::Redo => {
                let depth = session.map_or(0, |s| s.redo_depth());
                step_row_name(true, depth, session.and_then(|s| s.redo_label()))
            }
            _ => action.label().to_string(),
        }
    }

    /// What an action does, wherever it was asked for -- a stroke, or the clip
    /// menu item that names the same action. One table, so the two can never
    /// come to mean different things. The step an edit pushes is named after it
    /// ([`step_name`]), so Undo and Redo can say what they take back.
    pub(crate) fn act(&mut self, action: ActionId, window: &mut Window, cx: &mut Context<Self>) {
        let mark = self.session.as_mut().map(PlaybackSession::step_mark);
        self.run_action(action, window, cx);
        if let (Some(mark), Some(name)) = (mark, step_name(action))
            && let Some(session) = self.session.as_mut()
        {
            session.name_step(mark, name);
        }
    }

    /// Runs one write -- a card's, a gesture's -- and names the step it pushed, if it pushed
    /// one: a live sample pushes none, so only the committing write names anything.
    pub(crate) fn named_step<T>(&mut self, name: &str, write: impl FnOnce(&mut Self) -> T) -> T {
        let mark = self.session.as_mut().map(PlaybackSession::step_mark);
        let out = write(self);
        if let (Some(mark), Some(session)) = (mark, self.session.as_mut()) {
            session.name_step(mark, name);
        }
        out
    }

    /// The dispatch itself, behind [`Player::act`], which names the step it leaves.
    fn run_action(&mut self, action: ActionId, window: &mut Window, cx: &mut Context<Self>) {
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
        // The one choke point for every action that changes what a save would
        // write: the timeline's clips, the tracks that hold them, the subtitle
        // tracks. Marked before the match rather than once per arm below --
        // an autosave a beat early over a refusal (nothing to regroup, say)
        // costs nothing a real edit would not have earned anyway, and one list
        // here is the whole answer instead of fifteen scattered calls.
        //
        // The two exceptions are the history pair, whose doors answer "there was
        // a step to take" and are gated in their own arms below: `^z` with
        // nothing to undo is not a stroke that earned an autosave, and the
        // save-on-close question over a project nobody has touched is exactly
        // what the card doors' own gate exists to prevent.
        //
        // This list is the *action* doors only, and that is the class to mind
        // when adding an edit: a control that reaches the session from a
        // *click* -- the Clip tab's look chips and wheel, the settings rows,
        // the library row's own menu -- is behind no action, so nothing here
        // arms it. Such a door has to call `Player::mark_dirty` itself, on its
        // own "the write took" gate, or the edit is invisible to autosave, to
        // the ledger's `unsaved` ghost and to `Player::close_needs_answer`,
        // which all read that one flag: the work is silently lost on close,
        // with no prompt to answer.
        if matches!(
            action,
            ActionId::Paste
                | ActionId::Cut
                | ActionId::Regroup
                | ActionId::Detach
                | ActionId::Reverse
                | ActionId::NewSolid
                | ActionId::NewGradient
                | ActionId::NewCountdown
                | ActionId::NewBars
                | ActionId::NewTone
                | ActionId::NewBarsAndTone
                | ActionId::NewAdjustment
                | ActionId::NewRectangle
                | ActionId::NewEllipse
                | ActionId::NewLine
                | ActionId::NewArrow
                | ActionId::NewTitle
                | ActionId::Group
                | ActionId::Delete
                | ActionId::Lift
                | ActionId::AddVideoLane
                | ActionId::RemoveVideoLane
                | ActionId::AddAudioLane
                | ActionId::RemoveAudioLane
                | ActionId::AddSubtitleLane
                | ActionId::RemoveSubtitleLane
                | ActionId::ImportSubtitles
                // `Crossfade` and `Dissolve` are NOT here, and that is the
                // rule rather than an omission: their doors answer for
                // themselves ([`Player::crossfade_selected`],
                // [`Player::dissolve_selected`]) and the clip-menu row reaches
                // `dissolve_selected` directly, never through this table, so a
                // list entry armed the key path and left the menu path armed by
                // nothing.
                //
                // The three `Remove*Lane` entries ARE still here and are still
                // wrong in the same way -- `remove_lane` refuses a track that
                // holds clips and the last of its kind, and it now carries its
                // own arm ([`Player::remove_lane`]) -- but taking them off is
                // a wider pass over this list than this change makes, so they
                // stay until it does. Until then each of those removals arms
                // twice over, which is the lesser fault beside arming once for
                // a removal that did not happen.
                | ActionId::Visualizer
                | ActionId::NestClips
                | ActionId::BreakApart
                | ActionId::NewSequence
                | ActionId::DuplicateSequence
                | ActionId::DeleteSequence
        ) {
            self.mark_dirty();
        }
        match action {
            ActionId::Play => self.toggle_or_restart(cx),
            // No session touched at all: the pump reads the flag itself at
            // the one place that already knows the end was reached.
            ActionId::Loop => {
                self.loop_on = !self.loop_on;
                cx.notify();
            }
            ActionId::PlayAround => self.play_around(cx),
            // The clips' waveforms: kept with the project's view, so it reopens as it was left.
            ActionId::ToggleWaveforms => {
                self.waveforms = !self.waveforms;
                self.save_view_state();
                cx.notify();
            }
            // Sound while a ruler scrub is paused: kept with the view, like the waveforms.
            ActionId::ToggleScrubAudio => {
                self.scrub_audio = !self.scrub_audio;
                self.save_view_state();
                cx.notify();
            }
            // The shuttle (DESIGN §10.2): L plays forward and J backward, each press
            // while it runs stepping the rate 1x, 2x, 4x; K stops.
            ActionId::ShuttleBack => self.shuttle(false, cx),
            ActionId::ShuttleStop => {
                if let Some(session) = self.active_session_mut().filter(|s| s.is_playing()) {
                    session.pause();
                    session.set_rate(1.0);
                }
                cx.notify();
            }
            ActionId::ShuttleForward => self.shuttle(true, cx),
            // The shortcuts sheet: the same toggle the `?` stroke makes.
            ActionId::ShowKeys => {
                if self.keys_open {
                    self.keys_open = false;
                    cx.notify();
                } else {
                    self.show_keys(cx);
                }
            }
            ActionId::About => {
                self.about_open = !self.about_open;
                cx.notify();
            }
            ActionId::StepBack => self.step(-1, cx),
            ActionId::StepForward => self.step(1, cx),
            // A second is however many frames this timeline runs at.
            ActionId::JumpBack => self.step(-(self.fps.round() as i64), cx),
            ActionId::JumpForward => self.step(self.fps.round() as i64, cx),
            // The ends, as a step nothing can be far enough from.
            ActionId::GoStart => self.step(i64::MIN, cx),
            ActionId::GoEnd => self.step(i64::MAX, cx),
            ActionId::GoToTime => self.open_time_entry(cx),
            // Not a step at all: the grid these land on is the *source's*, and
            // where the next one is depends on the file rather than on the rate.
            ActionId::PrevSyncPoint => self.jump_sync(false, cx),
            ActionId::NextSyncPoint => self.jump_sync(true, cx),
            // While a library item is previewed the marks are the item's own
            // (the source monitor's in/out); otherwise the timeline's range.
            ActionId::SetIn if self.preview_session.is_some() => self.mark_source(true, cx),
            ActionId::SetOut if self.preview_session.is_some() => self.mark_source(false, cx),
            ActionId::ClearRange if self.preview_session.is_some() => self.clear_source_marks(cx),
            ActionId::SetIn => self.set_in(cx),
            ActionId::SetOut => self.set_out(cx),
            ActionId::ClearRange => {
                self.range = None;
                cx.notify();
            }
            ActionId::RenderInOut => self.render_preview(false, cx),
            ActionId::RenderAll => self.render_preview(true, cx),
            ActionId::CancelRender => self.cancel_render(),
            ActionId::DeleteRenderFiles => self.delete_render_files(cx),
            ActionId::InsertSource => self.source_edit(engine::SourceEdit::Insert, cx),
            ActionId::OverwriteSource => self.source_edit(engine::SourceEdit::Overwrite, cx),
            ActionId::Export => self.open_export(cx),
            // Asked before the first overwrite of a file this window never
            // put there ([`Player::save_guarded`]); the window's own file
            // saves straight through.
            ActionId::Save => self.save_guarded(cx),
            ActionId::ReloadProject => self.reload_request(cx),
            ActionId::NewProject => self.open_new_project(cx),
            ActionId::SaveVersion => self.open_save_version(cx),
            ActionId::SaveTemplate => self.open_save_template(cx),
            ActionId::ExportCaptions => self.open_export_captions(cx),
            ActionId::Versions => self.open_versions(cx),
            ActionId::CollectMedia => self.open_collect(cx),
            ActionId::ProjectNotes => self.open_project_notes(cx),
            ActionId::ExportTimelineOtio => self.export_timeline("otio", cx),
            ActionId::ExportTimelineFcpxml => self.export_timeline("fcpxml", cx),
            ActionId::ExportTimelineEdl => self.export_timeline("edl", cx),
            ActionId::ImportTimelineOtio => self.pick_and_import(cx),
            ActionId::Copy => self.copy_selected(),
            ActionId::Paste => self.paste(cx),
            ActionId::Cut => self.cut(cx),
            ActionId::Regroup => self.regroup(cx),
            ActionId::Crossfade => self.crossfade_selected(cx),
            ActionId::Dissolve => self.dissolve_selected(cx),
            ActionId::TransitionDissolve
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
            | ActionId::TransitionZoom => {
                if let Some(kind) = action.transition_kind() {
                    self.set_transition_kind(kind, cx);
                }
            }
            ActionId::Detach => self.detach(cx),
            ActionId::Group => self.group(cx),
            ActionId::Select => self.select_under_playhead(cx),
            ActionId::SelectNext => self.select_step(true, cx),
            ActionId::SelectPrev => self.select_step(false, cx),
            ActionId::SelectAll => self.select_all(cx),
            // Last in escape's precedence chain (render.rs): nothing else
            // claimed the key, so it empties every selection the timeline
            // holds -- clips, a picked gap, a title and a caption -- and the
            // Clip tab goes back to what it shows with nothing picked.
            // In player fullscreen Escape leaves it first, the way every video player does; the
            // picks stay where they are until the next Escape.
            ActionId::Deselect if self.player_fullscreen => self.run_action(ActionId::Fullscreen, window, cx),
            ActionId::Deselect => self.clear_picks(cx),
            // A picked gap is what Delete closes (DESIGN §7); a clip mark or
            // nothing at all falls through to the clip path as before.
            ActionId::Delete if self.gap_pick().is_some() => self.close_selected_gap(cx),
            // A picked envelope key is what Delete takes out before a clip is.
            ActionId::Delete if self.gain_pick.is_some() => {
                self.gain_delete_pick(cx);
            }
            ActionId::Delete => self.delete_selected(cx),
            ActionId::Lift => self.lift_selected(cx),
            ActionId::CloseGap => self.close_gap_here(cx),
            ActionId::Freeze => self.freeze_frame(cx),
            ActionId::PasteAttributes => self.paste_attributes(
                &[Part::Color, Part::Effects, Part::Transform, Part::Speed, Part::Audio],
                cx,
            ),
            ActionId::PasteColor => self.paste_attributes(&[Part::Color], cx),
            ActionId::PasteEffects => self.paste_attributes(&[Part::Effects], cx),
            ActionId::PasteTransform => self.paste_attributes(&[Part::Transform], cx),
            ActionId::PasteSpeed => self.paste_attributes(&[Part::Speed], cx),
            ActionId::PasteAudio => self.paste_attributes(&[Part::Audio], cx),
            ActionId::MatchFrame => self.match_frame(cx),
            ActionId::ReplaceEdit => self.replace_edit(cx),
            ActionId::SelectForward => self.select_forward(false, cx),
            ActionId::SelectForwardAll => self.select_forward(true, cx),
            ActionId::ExtendAudio => self.split_edit(true, cx),
            ActionId::CutSelectedWords => self.delete_selected_words(cx),
            ActionId::ExtendVideo => self.split_edit(false, cx),
            ActionId::NudgeBack => self.nudge_selected(-1, cx),
            ActionId::NudgeForward => self.nudge_selected(1, cx),
            ActionId::NudgeBack10 => self.nudge_selected(-10, cx),
            ActionId::NudgeForward10 => self.nudge_selected(10, cx),
            ActionId::Color => self.open_color(cx),
            ActionId::AutoColour => self.auto_colour(cx),
            ActionId::SaveLook => {
                if self.color_open.is_none() {
                    self.open_color(cx);
                }
                self.begin_look_save(cx);
            }
            ActionId::CompareOff => self.set_compare(crate::viewer_view::Compare::Off, cx),
            ActionId::CompareSplit => self.set_compare(crate::viewer_view::Compare::Split, cx),
            ActionId::CompareSide => self.set_compare(crate::viewer_view::Compare::Side, cx),
            ActionId::Transform => self.open_transform(cx),
            ActionId::Effects => self.open_effects(cx),
            ActionId::KeyPlacement => self.toggle_key_here(engine::project::KeyKind::Placement, cx),
            ActionId::KeyOpacity => self.toggle_key_here(engine::project::KeyKind::Opacity, cx),
            ActionId::KeyColor => self.toggle_key_here(engine::project::KeyKind::Color, cx),
            ActionId::KeyEasing => self.open_key_easing(self.ledger_anchor(LedgerItem::Clip), cx),
            ActionId::Fit => self.open_fit_list(self.ledger_anchor(LedgerItem::Clip), cx),
            ActionId::Resolution => self.open_picker(Pick::Resolution, self.ledger_anchor(LedgerItem::Size), cx),
            // The playhead is what a key zoom is aimed at: it is the one place
            // on the timeline the user is certainly looking at, and keeping it
            // still is what every editor does.
            ActionId::ZoomIn => self.zoom(ZOOM_STEP, None, cx),
            ActionId::ZoomOut => self.zoom(1. / ZOOM_STEP, None, cx),
            ActionId::ZoomFit => self.zoom_fit(cx),
            ActionId::ZoomToSelection => self.zoom_to_selection(cx),
            ActionId::TrackSmall => self.set_all_track_sizes(crate::ui::bench_stance::TrackSize::Small, cx),
            ActionId::TrackMedium => self.set_all_track_sizes(crate::ui::bench_stance::TrackSize::Medium, cx),
            ActionId::TrackLarge => self.set_all_track_sizes(crate::ui::bench_stance::TrackSize::Large, cx),
            ActionId::TracksSmaller => self.step_track_sizes(false, cx),
            ActionId::TracksLarger => self.step_track_sizes(true, cx),
            ActionId::Undo => {
                let name = self.session.as_ref().and_then(|s| s.undo_label().map(str::to_string));
                if self.undo(cx) {
                    self.mark_dirty();
                    self.settle_saved_point();
                    self.notify_user(step_toast(false, name.as_deref()).into());
                }
            }
            ActionId::Redo => {
                let name = self.session.as_ref().and_then(|s| s.redo_label().map(str::to_string));
                if self.redo(cx) {
                    self.mark_dirty();
                    self.settle_saved_point();
                    self.notify_user(step_toast(true, name.as_deref()).into());
                }
            }
            ActionId::History => self.open_history(cx),
            ActionId::AddVideoLane => self.add_lane(LaneKind::Video, cx),
            ActionId::AddAudioLane => self.add_lane(LaneKind::Audio, cx),
            // The last track of that kind: the one the add key put there, so the
            // two strokes undo each other press for press. Any other track goes
            // through the × in its own header.
            ActionId::RemoveVideoLane => self.remove_last_lane(LaneKind::Video, cx),
            ActionId::RemoveAudioLane => self.remove_last_lane(LaneKind::Audio, cx),
            // The third kind of track, added and taken back exactly as the two
            // above it: what a caption is dragged onto. The words themselves
            // arrive by the door below, which places nothing.
            ActionId::AddSubtitleLane => self.add_lane(LaneKind::Subtitle, cx),
            ActionId::RemoveSubtitleLane => self.remove_last_lane(LaneKind::Subtitle, cx),
            // The chooser the palette's own button opens: a file's tracks into
            // the list, the file itself joining nothing.
            ActionId::ImportSubtitles => self.pick_and_add_subtitles(cx),
            ActionId::AddFiles => self.pick_and_import(cx),
            ActionId::OpenProject => self.pick_open_project(cx),
            ActionId::PasteFilePath => self.paste_file_path(cx),
            ActionId::ToggleMute => self.set_volume(|volume| volume.muted = !volume.muted, cx),
            ActionId::VolumeUp => self.set_volume(|volume| volume.step(true), cx),
            ActionId::VolumeDown => self.set_volume(|volume| volume.step(false), cx),
            ActionId::Equalizer => self.open_eq(cx),
            ActionId::Speed => self.open_speed(cx),
            ActionId::Reverse => self.toggle_reverse(cx),
            ActionId::ClipEnabled => self.toggle_clip_enabled(cx),
            ActionId::ClipLabel => self.open_label_list(cx),
            ActionId::NewSolid
            | ActionId::NewGradient
            | ActionId::NewCountdown
            | ActionId::NewBars
            | ActionId::NewAdjustment
            | ActionId::NewRectangle
            | ActionId::NewEllipse
            | ActionId::NewLine
            | ActionId::NewArrow => self.new_generator(action, cx),
            ActionId::NewTone | ActionId::NewBarsAndTone => self.new_tone(action, cx),
            ActionId::NewTitle => self.new_title(cx),
            ActionId::NewBin => self.new_bin(0, Vec::new(), cx),
            ActionId::NewBinInside => self.new_bin_inside_selected(cx),
            ActionId::RenameBin => self.rename_selected_bin(cx),
            ActionId::RemoveBin => self.remove_selected_bin(cx),
            ActionId::ToggleFavourite => self.toggle_favourite_picked(cx),
            ActionId::MoveToBin => self.bin_picker_here(cx),
            ActionId::ShowFavourites => self.show_favourites(true, cx),
            ActionId::ShowAllMedia => self.show_favourites(false, cx),
            ActionId::SpeedRampUp => self.set_ramp_preset(RampPreset::Up, cx),
            ActionId::SpeedRampDown => self.set_ramp_preset(RampPreset::Down, cx),
            ActionId::SpeedRampSlowFastSlow => self.set_ramp_preset(RampPreset::SlowFastSlow, cx),
            ActionId::SpeedRampFastSlowFast => self.set_ramp_preset(RampPreset::FastSlowFast, cx),
            ActionId::SpeedRampKey => self.add_ramp_key(cx),
            ActionId::SpeedRampClear => self.clear_ramp(cx),
            ActionId::Silence => self.open_silence(cx),
            ActionId::Normalize => self.open_tools(crate::player::tools::Tool::Normalize, cx),
            // Starts the listening straight away; the Tools card opens under it so the
            // progress (and any plain reason it cannot start) is on screen.
            ActionId::Captions => {
                self.open_tools(crate::player::tools::Tool::Captions, cx);
                self.start_listening(false, cx);
            }
            ActionId::Fillers => self.open_tools(crate::player::tools::Tool::Fillers, cx),
            ActionId::Voiceover => self.open_tools(crate::player::tools::Tool::Voiceover, cx),
            ActionId::RecordVoiceover => self.open_record_voiceover(cx),
            ActionId::Visualizer => self.create_visualizer(cx),
            ActionId::MarkBeats => self.open_beats(cx),
            ActionId::Reframe => self.reframe_selected(cx),
            ActionId::TrackMask => self.track_selected_mask(cx),
            ActionId::NestClips => self.nest_selected(cx),
            ActionId::CreateMulticam => self.create_multicam_selected(cx),
            ActionId::FlattenMulticam => self.flatten_multicam_selected(cx),
            ActionId::ToggleAngleMonitor => self.toggle_angle_monitor(cx),
            ActionId::Angle1 => self.switch_angle_here(1, cx),
            ActionId::Angle2 => self.switch_angle_here(2, cx),
            ActionId::Angle3 => self.switch_angle_here(3, cx),
            ActionId::Angle4 => self.switch_angle_here(4, cx),
            ActionId::Angle5 => self.switch_angle_here(5, cx),
            ActionId::Angle6 => self.switch_angle_here(6, cx),
            ActionId::Angle7 => self.switch_angle_here(7, cx),
            ActionId::Angle8 => self.switch_angle_here(8, cx),
            ActionId::Angle9 => self.switch_angle_here(9, cx),
            ActionId::BreakApart => self.break_apart_selected(cx),
            ActionId::OpenCompound => self.open_compound_selected(cx),
            ActionId::CloseCompound => self.close_compound(cx),
            ActionId::NewSequence => self.new_sequence(cx),
            ActionId::DuplicateSequence => self.duplicate_sequence(cx),
            ActionId::RenameSequence => self.begin_seq_edit(cx),
            ActionId::DeleteSequence => self.delete_sequence(cx),
            ActionId::RenameLane => {
                // From the palette there is no head to name: the selected clip's track, else V1.
                let lane = self.selected.anchor().map_or(Lane::V1, |(lane, _)| lane);
                self.begin_lane_edit(lane, cx)
            }
            ActionId::Mix => self.open_mix(None, cx),
            ActionId::CleanVoice => match self.voice_target_lane() {
                Some(lane) => self.clean_mix_voice(lane, engine::voice::Strength::Medium, cx),
                None => {
                    self.notify_user("Nothing to clean — no audio track has sound".into());
                    cx.notify();
                }
            },
            ActionId::VoiceDynamics => {
                let lane = self.voice_target_lane();
                self.open_mix(lane, cx);
                if let Some(lane) = lane.filter(|&l| !self.mix_voice_open(l)) {
                    self.toggle_voice_section(lane, cx);
                }
            }
            ActionId::SubtitleStyle => self.open_subtitle_style(cx),
            ActionId::ToggleSnap => self.toggle_snap(cx),
            ActionId::ToggleSubtitles => self.toggle_subtitles(cx),
            ActionId::ToggleProxies => self.toggle_proxies(cx),
            ActionId::ToggleAutoProxies => self.toggle_auto_proxies(cx),
            // The keyboard's door to the same list the toolbar button opens.
            // At the window's corner, since a stroke names no place -- and
            // [`menu_at`] keeps it on screen from there.
            ActionId::Theme => self.open_picker(Pick::Theme, Point::default(), cx),
            ActionId::AccentColour => self.open_picker(Pick::Accent, Point::default(), cx),
            ActionId::ToggleOutputWindow => self.toggle_output_window(cx),
            ActionId::OutputDisplay => self.open_picker(Pick::OutputDisplay, Point::default(), cx),
            ActionId::ViewZoom => {
                let at = self.viewer_anchor(0);
                self.picker_under(Pick::ViewZoom, at, cx)
            },
            ActionId::PlaybackQuality => {
                let at = self.viewer_anchor(1);
                self.picker_under(Pick::ViewQuality, at, cx)
            },
            ActionId::GuideMask => {
                let at = self.viewer_anchor(3);
                self.picker_under(Pick::ViewMask, at, cx)
            },
            ActionId::GuideStyle => {
                let at = self.viewer_anchor(2);
                self.picker_under(Pick::ViewGuides, at, cx)
            }
            // Not the window's own toggle alone -- every consumer video
            // player fullscreens the *picture*, chrome and all, not just the
            // OS frame around it -- so this flips both: `player_fullscreen`
            // is what [`crate::render`] reads to draw the picture only, and
            // the platform's own toggle is what actually grows the window to
            // the monitor. The two are asked to agree rather than one being
            // derived from the other, because a compositor keybind can move
            // `window.is_fullscreen()` without this action ever firing.
            ActionId::Fullscreen => {
                self.player_fullscreen = !self.player_fullscreen;
                if window.is_fullscreen() != self.player_fullscreen {
                    window.toggle_fullscreen();
                }
            }
            // The ring's own door: nothing else in the app ever calls
            // `window.focus` on one of the three surface handles
            // (`ui::stance::Surface`), so without this the Tab handlers those
            // surfaces carry (`stance.rs`'s `bench`, `dock_stance.rs`'s
            // `cycle_on_key_down`) are unreachable dead code -- the ring never
            // starts. Lands on the cycle's first stop (`stance::SURFACE_CYCLE`
            // -- the dock); a second Tab from there walks it same as ever.
            // F6 walks the same cycle Tab does, one panel per press, from wherever the focus is
            // now (the media panel when it is on none of them), and names where it landed.
            ActionId::FocusPanels => {
                use crate::ui::stance::{Surface, next_surface};
                let now = [Surface::Dock, Surface::Bench, Surface::Inspector]
                    .into_iter()
                    .find(|s| self.focus_handle(*s).is_focused(window));
                let to = now.map_or(Surface::Dock, |s| next_surface(s, false));
                self.focus_surface(to, window, cx);
                let name = match to {
                    Surface::Bench => "the timeline",
                    Surface::Dock if self.dock_src_active => "Media",
                    Surface::Dock | Surface::Inspector => "the side panel",
                };
                self.notify_user(format!("Focus moved to {name}").into());
            }
            // `^esc` (or whatever the keymap now binds it to) reaches here
            // through the plain `act()` dispatch even while an export is
            // running (`ui::stance`'s key handler deliberately leaves
            // `exporting()` out of its modal guard so this keeps working):
            // was a no-op until now, the real gap the progress card's own
            // "the chord as well" doc promised.
            ActionId::CancelExport => self.cancel_export(),
            ActionId::ShowActions => self.show_actions(cx),
            ActionId::Settings => self.open_settings(cx),
            ActionId::Screenshot => self.take_screenshot(cx),
            // Cut machinery (DESIGN.md §6): the subject cut is the marked
            // clip ([`Selection::anchor`]) -- the oracle already refused
            // every one of these with nothing marked, so an anchor is here
            // to read.
            ActionId::AddMarker => self.add_marker_here(cx),
            ActionId::MarkerHere => self.add_marker_at_menu(cx),
            ActionId::RenameMarker => self.rename_marker_at_pick(cx),
            ActionId::DeleteMarker => self.delete_marker_at_pick(cx),
            ActionId::PlayheadHere => self.playhead_at_menu(cx),
            ActionId::NextMarker => self.jump_marker(true, cx),
            ActionId::PrevMarker => self.jump_marker(false, cx),
            ActionId::Markers => self.open_markers(cx),
            ActionId::WalkCutNext => self.walk_cut(true, cx),
            ActionId::WalkCutPrev => self.walk_cut(false, cx),
            ActionId::WalkCutNext10 => self.walk_cut_stride(true, 10, cx),
            ActionId::WalkCutPrev10 => self.walk_cut_stride(false, 10, cx),
            // `[` `]` close in on the clip like the brackets they are: `[`
            // trims a frame off the head, `]` a frame off the tail -- no-aim,
            // frame detents, clamped to the room the engine already answers
            // ([`nudge_edge`]), which is what makes them work at any zoom
            // down to a 4px clip with nothing to click.
            ActionId::TrimIn => self.nudge_cut(Edge::Start, 1, cx),
            ActionId::TrimOut => self.nudge_cut(Edge::End, -1, cx),
            ActionId::LoopTrim => self.toggle_loop_trim(cx),
            // Trim-to-playhead (debt #42): the same `Edge::Start`/`Edge::End`
            // split as the nudge pair above, at the playhead's frame instead
            // of a one-frame detent.
            ActionId::TrimInToPlayhead => self.trim_cut_to_playhead(Edge::Start, false, cx),
            ActionId::TrimOutToPlayhead => self.trim_cut_to_playhead(Edge::End, false, cx),
            ActionId::RippleTrimInToPlayhead => self.trim_cut_to_playhead(Edge::Start, true, cx),
            ActionId::RippleTrimOutToPlayhead => self.trim_cut_to_playhead(Edge::End, true, cx),
            // Roll, slip and slide at frame detents, the keyboard's version of
            // the shift-drags ([`Player::pro_nudge`]).
            ActionId::RollBack => self.pro_nudge(ProKind::Roll, -1, cx),
            ActionId::RollForward => self.pro_nudge(ProKind::Roll, 1, cx),
            ActionId::SlipBack => self.pro_nudge(ProKind::Slip, -1, cx),
            ActionId::SlipForward => self.pro_nudge(ProKind::Slip, 1, cx),
            ActionId::SlideBack => self.pro_nudge(ProKind::Slide, -1, cx),
            ActionId::SlideForward => self.pro_nudge(ProKind::Slide, 1, cx),
        }
    }

    /// Dispatches a type-level keyboard verb from its concrete lane header.
    /// The key still acts on the last lane of its kind; a header click must
    /// keep the lane the hand named, so it enters through this targeted door.
    pub(crate) fn act_lane(&mut self, action: ActionId, lane: Lane, cx: &mut Context<Self>) {
        match enable_lane(action, lane, self.ctx(None)) {
            Enable::Yes => {}
            Enable::No(why) => {
                self.notify_user(format!("{} — {why}", action.label()).into());
                cx.notify();
                return;
            }
            Enable::Hidden(_) => return,
        }
        match action {
            ActionId::Mix => self.open_mix(Some(lane), cx),
            ActionId::RenameLane => self.begin_lane_edit(lane, cx),
            // The arm rides the door's own answer, like every arm in this
            // window: `remove_lane` refuses a track that still holds clips or
            // captions, and the last track of its kind, and arming before the
            // ask lit the ledger's `unsaved` ghost, wrote a sidecar and put the
            // close prompt up over a project whose bytes had not moved.
            ActionId::RemoveVideoLane | ActionId::RemoveAudioLane => self.remove_lane(lane, cx),
            _ => unreachable!("only lane-header actions reach act_lane"),
        }
    }

    /// Says something to the user. The one door: every message in this editor
    /// comes through here, so "queued rather than overwritten" is a property of
    /// the field and not of seventy call sites remembering to be polite.
    ///
    /// A repeat of what is already at the back is dropped -- holding a key that
    /// refuses would otherwise fill the queue with one sentence, and the count
    /// on the bar would be a count of how long the key was held.
    pub(crate) fn notify_user(&mut self, message: SharedString) {
        if !message.starts_with(EXPORT_DONE) {
            push_notice(&mut self.notices, message);
            return;
        }
        // An export's outcome carries the way to its file: the toast's buttons
        // play it or reveal it (DESIGN §8). Every other message has no button.
        let path = self.export_path.clone();
        self.notify_user_actions(
            message,
            vec![
                ToastAction::Play(path.clone()),
                ToastAction::ShowInFolder(path),
            ],
        );
    }

    /// [`Player::notify_user`] for a message whose toast has a button: the one
    /// door that takes an action, and otherwise the same queue and the same
    /// policy.
    pub(crate) fn notify_user_action(&mut self, message: SharedString, action: ToastAction) {
        self.notify_user_actions(message, vec![action]);
    }

    /// [`Player::notify_user_action`] for a toast with up to two buttons, drawn
    /// in the order given.
    pub(crate) fn notify_user_actions(&mut self, message: SharedString, actions: Vec<ToastAction>) {
        let tone = Tone::of(&message);
        push_toast(&mut self.notices, message, tone, actions);
    }

    /// A toast's button was pressed: the toast goes, and the action at `index`
    /// runs. Any other button on it goes with the toast.
    pub(crate) fn toast_action(&mut self, id: u64, index: usize, cx: &mut Context<Self>) {
        let Some(action) = self
            .notices
            .iter()
            .find(|toast| toast.id == id)
            .and_then(|toast| toast.actions.get(index).cloned())
        else {
            return;
        };
        remove_toast(&mut self.notices, id);
        match action {
            ToastAction::Undo => {
                if self.undo(cx) {
                    self.mark_dirty();
                }
            }
            // The refusal was about the lock, so the lock is what goes: one
            // undo step, the same as the head's own switch.
            ToastAction::Unlock(lane) => self.set_lane_switch(lane, Switch::Locked, false, cx),
            // Another process starting: off the UI thread, as the library's
            // own Reveal does.
            ToastAction::ShowInFolder(path) => {
                cx.background_executor()
                    .spawn(async move { show_in_file_manager(&path) })
                    .detach();
            }
            // Same shape as Reveal: the player is another process, so it is
            // started off the UI thread and nothing here waits on it.
            ToastAction::Reload => self.reload_from_disk(cx),
            // The toast is the whole of it: the window keeps what it holds.
            ToastAction::KeepMine => {}
            ToastAction::Recover => self.recover_from_sidecar(cx),
            ToastAction::Discard => self.discard_recovery(),
            ToastAction::SaveThenOpen(path) => {
                self.save_project(cx);
                // A refused save keeps the edits: the other project waits.
                if !self.edited_since_save {
                    self.import(&path, cx);
                }
            }
            ToastAction::OpenDiscarding(path) => {
                // The edits are being let go, so their autosave must not come back as "recovery".
                for armed in [true, false] {
                    if let Some(autosave) = Self::autosave_target(&self.project_path, armed) {
                        let _ = std::fs::remove_file(autosave);
                    }
                }
                self.autosave_dirty = false;
                self.edited_since_save = false;
                self.import(&path, cx);
            }
            ToastAction::Play(path) => {
                cx.background_executor()
                    .spawn(async move { open_in_default_player(&path) })
                    .detach();
            }
        }
        cx.notify();
    }

    /// A toast leaves by its own ✕, or by its timer. A missing id is not an
    /// error: the timer for a toast that was already dismissed fires into
    /// nothing, and that is the point of the id.
    pub(crate) fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if remove_toast(&mut self.notices, id) {
            cx.notify();
        }
    }

    /// The magnet off and on again, in words: a snap that stops working
    /// silently reads as a bug, and one that starts working silently reads as
    /// one too. The line goes with it -- nothing is being promised any more.
    pub(crate) fn toggle_snap(&mut self, cx: &mut Context<Self>) {
        self.snap = !self.snap;
        self.snap_cue = None;
        self.ghost.clear();
        self.notify_user(match self.snap {
            true => "Snap on".into(),
            false => "Snap off".into(),
        });
        cx.notify();
    }

    /// The command palette (DESIGN §4.4), from Ctrl+K, the spine's `commands`
    /// or a menu row. It opens empty with its field ready for typing, and the
    /// whole list under it.
    pub(crate) fn show_actions(&mut self, cx: &mut Context<Self>) {
        self.palette_open = true;
        self.palette_query.clear();
        self.palette_sel = 0;
        self.palette_scroll.set_offset(point(px(0.), px(0.)));
        // One card at a time, the rule the other cards follow.
        self.export_open = false;
        cx.notify();
    }

    /// The keys list, from the spine's `?`: the dock swings to its KEYS tab,
    /// scrolled to the top -- a list that opens where the last look left it
    /// would hide most of itself for a reason nobody remembers.
    pub(crate) fn show_keys(&mut self, cx: &mut Context<Self>) {
        self.keys_open = true;
        self.scroll_keys(None);
        // One card at a time, the rule the other cards follow.
        self.export_open = false;
        self.transcript_open = false;
        cx.notify();
    }

    /// The palette's rows for what its field holds now, best match first.
    pub(crate) fn palette_list(&self) -> Vec<crate::menus::PaletteRow> {
        crate::menus::palette_rows(&self.palette_query, |a| self.enable(a, None))
    }

    /// Moves the palette highlight by `by` rows, clamped to the list, and
    /// scrolls it into view.
    pub(crate) fn palette_move(&mut self, by: isize) {
        let rows = self.palette_list();
        if rows.is_empty() {
            self.palette_sel = 0;
            return;
        }
        let last = rows.len() - 1;
        self.palette_sel = (self.palette_sel as isize + by).clamp(0, last as isize) as usize;
        let grouped = self.palette_query.trim().is_empty();
        let child = crate::menus::palette_child_index(&rows, self.palette_sel, grouped);
        self.palette_scroll.scroll_to_item(child);
    }

    /// Runs the highlighted palette row. A runnable row closes the palette
    /// first, so an action that opens a card is not drawn under it. A refused
    /// row goes through [`Player::act`], which says why as a toast, and the
    /// palette stays open for another pick.
    pub(crate) fn palette_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The design kit's gallery is reached by typing its name; it is no
        // action, so it has no row, no key and no menu entry.
        if self.palette_query.trim().eq_ignore_ascii_case(crate::ui::gallery::PALETTE_PHRASE) {
            self.palette_open = false;
            self.palette_query.clear();
            crate::ui::gallery::set_open(true);
            cx.notify();
            return;
        }
        let rows = self.palette_list();
        let Some(row) = rows.get(self.palette_sel).copied() else {
            cx.notify();
            return;
        };
        if crate::menus::palette_closes_on_run(row) {
            self.palette_open = false;
            self.palette_query.clear();
            // Command search reopening itself from its own row would be a
            // palette that cannot be left by picking from it.
            if row.action == ActionId::ShowActions {
                cx.notify();
                return;
            }
        }
        self.act(row.action, window, cx);
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
    pub(crate) fn scroll_keys(&self, by: Option<f32>) {
        let at = match by {
            Some(by) => (f32::from(self.keys_scroll.offset().y) + by)
                .clamp(-f32::from(self.keys_scroll.max_offset().height), 0.),
            None => 0.,
        };
        self.keys_scroll.set_offset(point(px(0.), px(at)));
    }

    /// The cues over the picture, off and on. Says which it is now *and* how
    /// much is placed underneath: a toggle whose answer is invisible -- the
    /// playhead between two cues, or a palette full of tracks nobody dragged
    /// onto a lane -- would read as broken.
    pub(crate) fn toggle_subtitles(&mut self, cx: &mut Context<Self>) {
        self.subs_on = !self.subs_on;
        // Lane facts and not the picked row: what draws is what is *placed*
        // ([`Player::subtitle_overlay`]), so naming the palette selection here
        // named a track the toggle never touched.
        let placed = self.placed_captions();
        self.notify_user(subtitle_toggle_notice(self.subs_on, placed).into());
        cx.notify();
    }

    /// How many captions the subtitle lanes hold, all lanes together: what the
    /// toggle and the toolbar's own label are about, now that the picture reads
    /// the lanes rather than a pick ([`Player::subtitle_overlay`]). Zero with no
    /// timeline, and zero for a palette full of tracks nobody placed -- both
    /// show nothing, so both read "No subs".
    pub(crate) fn placed_captions(&self) -> usize {
        let Some(session) = self.session.as_ref() else {
            return 0;
        };
        session
            .subtitle_lanes()
            .into_iter()
            .map(|lane| session.sub_lane(lane).len())
            .sum()
    }

    /// Whether the editor can be asked for `action` right now, and why not when
    /// it cannot. `on` is the clip the question is about -- the one a clip menu
    /// was opened on -- and `None` asks about the marked clip instead, which is
    /// what a menu that hangs over no clip in particular means by "this one".
    ///
    /// The player's half of [`enable`]: it reads the state, the table decides.
    pub(crate) fn enable(&self, action: ActionId, on: Option<(Lane, usize)>) -> Enable {
        // A display list of one has nothing to choose between: the row is not drawn.
        if action == ActionId::OutputDisplay && self.output_displays.len() < 2 {
            return Enable::Hidden("only one display");
        }
        // A transition joins the clip to the one after it, so a picture clip
        // with nothing right after it has no join to draw: said before the
        // table, as the state of this moment, the way the menu row says why.
        if action.transition_kind().is_some() && self.lacks_successor(on) {
            return Enable::No("no clip after this one");
        }
        enable(action, self.ctx(on))
    }

    /// Whether the picture clip the question is about (or the marked one) has
    /// no clip butted up against its right-hand end on its lane. Sound and
    /// caption lanes answer `false`: the table refuses those by their class.
    fn lacks_successor(&self, on: Option<(Lane, usize)>) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let Some((lane, idx)) = on.or(self.selected.anchor()) else {
            return false;
        };
        if lane.kind != LaneKind::Video {
            return false;
        }
        let clips = session.lane_clips(lane);
        clips.get(idx).is_some_and(|clip| !clips.get(idx + 1).is_some_and(|next| next.start == clip.end()))
    }

    /// The state every one of those questions is asked against, read off the
    /// player once: [`menu_items`] filters a whole menu with it, so the rows a
    /// menu draws and the answers it dims them by come from the same reading.
    pub(crate) fn ctx(&self, on: Option<(Lane, usize)>) -> Ctx {
        let Some(session) = &self.session else {
            return Ctx::default();
        };
        let on = on.or(self.selected.anchor());
        let clip =
            on.and_then(|(lane, idx)| session.lane_clips(lane).get(idx).map(|clip| (clip.clone(), lane)));
        let caption = on.is_some_and(|(lane, idx)| {
            lane.kind == LaneKind::Subtitle && idx < session.sub_lane(lane).len()
        });
        // The selection's shape, over the picks that still name something:
        // how many placements, and on how many lanes. The lane count is the
        // manual group's one-per-lane question -- equal counts say a group, a
        // short one says a lane is picked twice.
        let (picks, _) = self.marks();
        let mut lanes = Vec::new();
        for &(lane, _) in &picks {
            if !lanes.contains(&lane) {
                lanes.push(lane);
            }
        }
        let pick_lanes = lanes.len();
        Ctx {
            clip: clip.clone(),
            // A selected title is taken off by Delete the way a picked gap is closed by it.
            gap: self.gap_pick().is_some() || self.title_target().is_some(),
            gap_here: self.gap_here().is_some(),
            clip_here: !self.under_playhead().is_empty(),
            words: self.transcript.selection().is_some(),
            rendering: self.render.as_ref().is_some_and(|(job, _)| !job.is_finished()),
            asset: self.selected_asset.is_some() || self.source_target.is_some(),
            // The same pair read in the other index space: a subtitle lane's
            // own, where the box that was clicked is a caption.
            caption,
            caption_link: caption
                .then(|| {
                    on.and_then(|(lane, idx)| session.sub_lane(lane).get(idx))
                        .and_then(|s| s.link)
                })
                .flatten(),
            image: clip.as_ref().is_some_and(|(clip, _)| {
                session
                    .sources()
                    .get(clip.source)
                    .is_some_and(|s| engine::is_image(&s.path))
            }),
            // The probed stream table again, this time asking the other
            // question of it: an empty list is a silent file
            // ([`Player::streams`]), and a clip cut from one has no sound to
            // scan whatever lane it sits on -- said before the row is drawn
            // rather than after the scan comes back empty
            // ([`Player::start_silence_scan`]'s `Ok(None)` arm).
            no_sound: clip.as_ref().is_some_and(|(clip, _)| {
                session
                    .sources()
                    .get(clip.source)
                    .is_some_and(|s| self.streams.get(&s.path).is_some_and(Vec::is_empty))
            }),
            // The clip or caption the question is about, or anything picked,
            // sits on a locked lane ([`oracle::enable`] refuses the edits).
            locked: on.is_some_and(|(lane, _)| session.lane_locked(lane))
                || lanes.iter().any(|&lane| session.lane_locked(lane)),
            playhead: frame_at(session.now(), self.fps),
            timeline: true,
            clipboard: !self.clipboard.is_empty(),
            subtitles: !session.subtitles().is_empty(),
            playable: !nothing_to_play(self.active_session()),
            exporting: self.exporting().is_some(),
            picks: picks.len(),
            pick_lanes,
            nothing_to_undo: session.undo_depth() == 0,
            nothing_to_redo: session.redo_depth() == 0,
            alone: clip.as_ref().is_some_and(|(c, _)| {
                // Its link is shared by no other clip (the lane's own index
                // is the clip itself).
                c.link.is_some_and(|id| {
                    session
                        .lanes()
                        .into_iter()
                        .map(|l| {
                            session
                                .lane_clips(l)
                                .iter()
                                .filter(|o| o.link == Some(id))
                                .count()
                                + session.sub_lane(l).iter().filter(|s| s.link == Some(id)).count()
                        })
                        .sum::<usize>()
                        <= 1
                })
            }),
            no_partner: on.is_some_and(|(lane, idx)| {
                clip.is_some() && crate::timeline_math::span_partner(session, lane, idx).is_none()
            }),
            compound: clip.as_ref().is_some_and(|(c, _)| engine::project::is_sequence_source(c.source)),
            // Any piece of a multicam clip counts: the one asked about, any picked, or (with
            // nothing selected) the one under the playhead.
            multicam: on.is_some_and(|(lane, idx)| session.multicam_at(lane, idx).is_some())
                || picks.iter().any(|&(lane, idx)| session.multicam_at(lane, idx).is_some())
                || (on.is_none() && self.multicam_here().is_some()),
            media_picks: self.picked_sources().len(),
            monitor: self.angle_monitor,
            angles_here: self.multicam_here().map_or(0, |u| u.names.len()),
            sequences: session.sequences().len(),
            sequence_used: session.sequences().iter().any(|q| q.active && q.used > 0),
            parent: !self.seq_trail.is_empty(),
            no_join: !session.lanes().into_iter().any(|l| {
                rejoinable(session.lane_clips(l), frame_at(session.now(), self.fps))
            }),
        }
    }

    /// The same reading for a library row: whether this file can join this
    /// timeline -- the very answer the list greys the row by, so the menu over a
    /// row and the row under it cannot disagree -- and how many clips play it.
    /// [`Player::ctx`] for the other panel.
    pub(crate) fn row_ctx(&self, path: &Path, stream: usize) -> RowCtx {
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
                    // ...and the clips inside the other sequences, which play it too.
                    + session.source_uses_parked(idx).iter().map(|u| u.1).sum::<usize>()
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
            proxy_stops: matches!(
                self.proxies.get(path),
                Some(Proxy::Making(_) | Proxy::Cancelling(_))
            ),
            starred: sources.iter().any(|s| s.path == path && s.audio_stream == stream && s.fav),
            offline: self
                .session
                .as_ref()
                .is_some_and(|s| s.offline_sources().iter().any(|(_, p)| p == path)),
            media_picks: self.picked_sources().len(),
        }
    }

    /// Which of [`Repeat`]'s three the window is in, for the hold gate at the
    /// top of the key handler. Not [`Player::modal`]: that asks whether an
    /// overlay is up at all, and here the cards with sliders in them are
    /// exactly the ones that answer differently from the keys menu and the
    /// export card.
    pub(crate) fn repeat_scope(&self) -> Repeat {
        // A number being typed is a value under the arrows, exactly as a card's
        // slider is -- so a held arrow runs it. Asked before the export card
        // below, which otherwise repeats nothing.
        if self.budget_edit.is_some() {
            Repeat::Card
        } else if self.export_open || self.exporting().is_some() {
            Repeat::Nothing
        } else if self.eq_open.is_some()
            || self.color_open.is_some()
            || self.transform_open.is_some()
            || self.speed_open.is_some()
            || self.effects_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
            || self.markers_open
            || self.history_open
            || self.tools.is_some()
        {
            Repeat::Card
        } else {
            Repeat::Keymap
        }
    }

    /// One item of a library row's menu, done. Every one of them closes the
    /// menu first -- the list under it is about to be rebuilt -- except the one
    /// that turns the card over.
    pub(crate) fn act_on_row(&mut self, item: RowItem, cx: &mut Context<Self>) {
        let Some(menu) = self.library_menu.clone() else {
            return;
        };
        match item {
            RowItem::Relink => {
                self.library_menu = None;
                self.relink_pick(menu.path.clone(), cx);
            }
            RowItem::Properties => {
                if let Some(open) = &mut self.library_menu {
                    open.details = true;
                }
            }
            RowItem::Add => {
                self.library_menu = None;
                self.append_source(&menu.path, menu.stream, cx);
            }
            RowItem::Insert => {
                self.library_menu = None;
                self.insert_source(&menu.path, menu.stream, None, None, cx);
            }
            RowItem::Remove => {
                self.library_menu = None;
                self.remove_source(&menu.path, menu.stream, cx);
            }
            RowItem::RemoveWithClips => {
                self.library_menu = None;
                self.remove_source_and_clips(&menu.path, menu.stream, cx);
            }
            // The row's two former ghosts (`▷`, `○`), each still the one
            // implementation it always called (cleanse round 2).
            RowItem::Preview => {
                self.library_menu = None;
                self.open_preview(&menu.path, menu.stream, cx);
            }
            RowItem::CreateMulticam => {
                self.library_menu = None;
                self.create_multicam_selected(cx);
            }
            RowItem::Proxy => {
                self.library_menu = None;
                let stops = matches!(
                    self.proxies.get(&menu.path),
                    Some(Proxy::Making(_) | Proxy::Cancelling(_))
                );
                self.toggle_proxy(&menu.path, stops, cx);
            }
            RowItem::Favourite => {
                self.library_menu = None;
                self.toggle_favourite_of(menu.path.clone(), menu.stream, cx);
            }
            RowItem::MoveToBin => {
                self.bin_picker(menu.at, cx);
            }
            RowItem::NewBin => {
                self.library_menu = None;
                let rows = self.rows_for((menu.path.clone(), menu.stream));
                self.new_bin(0, rows, cx);
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

    /// The key-routed door into the picked row ([`crate::ui::dock_stance::media_key`]).
    /// No row menu is open on that path, so the row is the one
    /// [`Player::selected_asset`] names; the calls are the menu's own.
    pub(crate) fn act_on_picked_row(&mut self, item: RowItem, cx: &mut Context<Self>) {
        let Some((path, stream)) = self.selected_asset.clone() else {
            return;
        };
        match item {
            RowItem::Preview => self.open_preview(&path, stream, cx),
            // Asked of the row oracle like the menu's own Add: this key is a
            // second door to the same edit list, and an export is reading it.
            RowItem::Add => match crate::oracle::row_enable(item, self.row_ctx(&path, stream)) {
                Enable::No(why) => self.notify_user(format!("Add — {why}").into()),
                _ => self.append_source(&path, stream, cx),
            },
            RowItem::Insert => match crate::oracle::row_enable(item, self.row_ctx(&path, stream)) {
                Enable::No(why) => self.notify_user(format!("Insert — {why}").into()),
                _ => self.insert_source(&path, stream, None, None, cx),
            },
            _ => {}
        }
        cx.notify();
    }

    /// Marks the playhead as an export's in point. `ordered_range` keeps the
    /// pair legal whichever one lands past the other.
    pub(crate) fn set_in(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let at = frame_at(session.now(), self.fps);
        let len = frame_at(session.timeline_duration(), self.fps);
        self.range = Some(mark_in(self.range, at, len));
        cx.notify();
    }

    /// The out point's pair.
    pub(crate) fn set_out(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let at = frame_at(session.now(), self.fps);
        self.range = Some(mark_out(self.range, at));
        cx.notify();
    }

    /// The odometer (DESIGN.md §6, `,` `.`): the subject cut steps to its
    /// neighbour along its own lane, one at a time, clamped rather than
    /// wrapping at either end ([`walk_cut`]). The playhead follows it there,
    /// which is what puts the screen at rest on the new cut for the two-up
    /// to draw.
    pub(crate) fn walk_cut(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.walk_cut_stride(forward, 1, cx);
    }

    /// [`Player::walk_cut`] with an explicit stride -- ten for
    /// [`ActionId::WalkCutNext10`]/[`ActionId::WalkCutPrev10`], its own
    /// bound key (DESIGN.md §6's "shift") rather than a modifier read off
    /// the bare walk, since gpui never delivers shift alongside `.`/`,`
    /// (see [`crate::keymap::Chord`]'s doc).
    pub(crate) fn walk_cut_stride(&mut self, forward: bool, stride: usize, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let clips = session.lane_clips(lane);
        let next = walk_cut(idx, clips.len(), forward, stride);
        let at = clips.get(next).map(|c| f64::from(c.start) / self.fps);
        self.select((lane, next), cx);
        if let Some(at) = at {
            self.seek(at, cx);
        }
    }

    /// The subject cut's own span, `[start, end)` in timeline frames --
    /// what loop-trim loops and what the odometer's readout counts against.
    pub(crate) fn cut_span(&self, lane: Lane, idx: usize) -> Option<(u32, u32)> {
        self.session
            .as_ref()?
            .lane_clips(lane)
            .get(idx)
            .map(|c| (c.start, c.end()))
    }

    /// The no-aim trim (DESIGN.md §6, `[` `]`): one frame detent off the
    /// subject cut's chosen edge, clamped to the room the engine already
    /// answers ([`nudge_edge`]) -- the same wall a pointer drag is clamped
    /// to, so no aim and no hit-testing are needed at any zoom.
    pub(crate) fn nudge_cut(&mut self, edge: Edge, dir: i32, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let Some((lo, hi)) = session.trim_room(lane, idx, edge) else {
            return;
        };
        let Some(current) = session.lane_clips(lane).get(idx).map(|c| match edge {
            Edge::Start => c.start,
            Edge::End => c.end(),
        }) else {
            return;
        };
        let to = nudge_edge(current, dir, lo, hi);
        let trimmed = self
            .session
            .as_mut()
            .is_some_and(|s| s.trim_clip(lane, idx, edge, to));
        if trimmed {
            // The arm rides the door's own answer: `trim_clip` is `false` for an
            // edge already where it was asked to go -- the `[`/`]` pressed
            // against the wall the edge is already on -- so the flag, the
            // autosave sidecar and the save-on-close question stay off a
            // project nobody has changed.
            self.mark_dirty();
            self.reset_after_reseek();
            // Loop-trim follows the edge it is trimming: the whole point of
            // the mode is hearing the cut as it moves.
            if self.loop_trim.is_some() {
                self.loop_trim = self.cut_span(lane, idx);
            }
        }
        cx.notify();
    }

    /// Trim-to-playhead (debt #42): the keyboard's own version of the
    /// pointer's drag-the-edge-to-a-spot trim ([`Player::trim_to`] /
    /// [`Player::commit_trim`]) -- same [`PlaybackSession::trim_clip`]
    /// primitive either path ends at, so a hand on the keyboard gets the
    /// same undo step and the same room clamp a drag gets, just aimed at
    /// the playhead instead of wherever the pointer stopped. The oracle
    /// already refused this with no subject cut or a playhead off it, so an
    /// anchor and a frame are here to read.
    pub(crate) fn trim_cut_to_playhead(&mut self, edge: Edge, ripple: bool, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let to = frame_at(session.now(), self.fps);
        let trimmed = self.session.as_mut().is_some_and(|s| match ripple {
            true => s.ripple_trim_clip(lane, idx, edge, to),
            false => s.trim_clip(lane, idx, edge, to),
        });
        if trimmed {
            // The door's own answer again: an edge the playhead is already on is
            // `false`, no undo step, and nothing for the unsaved ghost to be
            // about.
            self.mark_dirty();
            self.reset_after_reseek();
            // Loop-trim follows the edge it is trimming, same as the nudge
            // pair above.
            if self.loop_trim.is_some() {
                self.loop_trim = self.cut_span(lane, idx);
            }
        }
        cx.notify();
    }

    /// Loop-trim (DESIGN.md §6, `/`): loops the transport around the subject
    /// cut's own span while `[`/`]` move its edges -- the modernized Avid
    /// trim mode. Neither direction touches play/pause: off drops the window
    /// and leaves playback exactly as it was found, and on arms the window
    /// without forcing a play -- a forced `session.play()` here used to start
    /// the clock as a side effect of arming, which left a paused transport
    /// already playing when the user's own `space` arrived next; that press
    /// then read as a toggle and paused it right back, pinned at the loop's
    /// start frame. Space is what starts it, playing or not.
    pub(crate) fn toggle_loop_trim(&mut self, cx: &mut Context<Self>) {
        if self.loop_trim.take().is_some() {
            self.notify_user("Loop trim off".into());
            cx.notify();
            return;
        }
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        self.loop_trim = self.cut_span(lane, idx);
        if self.loop_trim.is_some() {
            self.notify_user("Loop trim on".into());
        }
        cx.notify();
    }

    /// Whether there was a step to take, for the reason the action table arms
    /// nothing on its own: a `^z` with an empty history is not an edit, and it
    /// may not raise the save-on-close question over a project nobody has
    /// touched. The selection goes with the step for the same reason it used to
    /// go every time: a step back puts the indices on different clips, while a
    /// refused one leaves them naming exactly what they named.
    ///
    /// The step itself goes through [`Player::step_history`], which is what
    /// takes the window's own copy of the rate, the two proxy switches, the
    /// palette's pick and the plate over the picture back up with it.
    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let stepped = self.step_history(PlaybackSession::undo);
        if stepped {
            self.selected.clear();
            // A cue's number can name another cue after the step.
            self.drop_sel_cue();
            self.cue_menu = None;
        }
        cx.notify();
        stepped
    }

    /// [`Player::undo`]'s twin, the same answer and the same gate.
    pub(crate) fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let stepped = self.step_history(PlaybackSession::redo);
        if stepped {
            self.selected.clear();
            // A cue's number can name another cue after the step.
            self.drop_sel_cue();
            self.cue_menu = None;
        }
        cx.notify();
        stepped
    }

    /// One step of the session's own history through the door both `^z` and
    /// `^y` go through, and the window-side state it moves taken back up
    /// afterwards ([`history_step`]).
    ///
    /// The plate the pick drew goes with the pick
    /// ([`Player::remove_subtitle_track`]'s own drop, for its own reason) and
    /// the frame owed to the screen is cleared
    /// ([`Player::reset_after_reseek`]): an undo reseeks inside the engine, and
    /// a picture left where it was is the frame before the step.
    ///
    /// `false` where there was nothing to step: no session at all, or a history
    /// step that refused ([`PlaybackSession::undo`]'s own answer), in which case
    /// nothing in the window moves either.
    pub(crate) fn step_history(&mut self, step: impl FnOnce(&mut PlaybackSession) -> bool) -> bool {
        let mut caches = SessionCaches {
            fps: self.fps,
            proxies_on: self.proxies_on,
            auto_proxies_on: self.auto_proxies_on,
        };
        let before = HistoryBefore::of(
            self.session.as_ref().map_or(&[][..], |s| s.subtitles()),
            self.sub_track,
        );
        let Some(outcome) = self
            .session
            .as_mut()
            .and_then(|session| history_step(&before, &mut caches, session, step))
        else {
            return false;
        };
        self.fps = caches.fps;
        self.proxies_on = caches.proxies_on;
        self.auto_proxies_on = caches.auto_proxies_on;
        self.sub_track = outcome.sub_track;
        if outcome.drop_sub_image {
            // corner-cut: its atlas tile is not released -- `close_session`'s
            // note, for its reason and with its upgrade path.
            self.sub_image = None;
        }
        self.reset_after_reseek();
        true
    }
}

/// The three session settings a window keeps its own copy of, as a value so
/// that [`history_step`] can re-read them in place: the rate every
/// frame↔second conversion in the window counts in (`Player::timeline_at`,
/// `Scale::snap_frames`) and the two switches a project carries and the
/// Settings rows *show* (`settings_stance`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionCaches {
    pub(crate) fps: f64,
    pub(crate) proxies_on: bool,
    pub(crate) auto_proxies_on: bool,
}

/// The window-side state of the palette as a history step finds it: the pick,
/// how many rows there are, and which row the pick names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryBefore {
    pub(crate) sub_track: usize,
    pub(crate) sub_rows: usize,
    pub(crate) sub_row: Option<SubPickKey>,
}

impl HistoryBefore {
    /// `tracks` as they stand and the index picked there -- an empty palette
    /// for a window with no session, which is the state a step cannot happen in
    /// anyway.
    pub(crate) fn of(tracks: &[engine::subtitle::SubtitleTrack], sub_track: usize) -> Self {
        Self {
            sub_track,
            sub_rows: tracks.len(),
            sub_row: SubPickKey::of(tracks, sub_track),
        }
    }
}

/// What a history step leaves this window to take up beside the session's own
/// lanes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HistoryOutcome {
    pub(crate) sub_track: usize,
    /// The plate over the picture (`Player::sub_image`) has to go. Its tile is
    /// keyed by *where* a cue sits ([`Player::sub_picture`]'s `(lane,
    /// start_us)`) and carries no track, so a step that moves captions, takes
    /// one off, or renumbers the track a placement names
    /// ([`engine::Project::remove_subtitles`] walks `SubClip::track` down)
    /// cannot be asked of that cache -- the tile at a key can be served for a
    /// cue it was not drawn from. Conservative on purpose: at most one plate is
    /// redrawn by the next repaint.
    pub(crate) drop_sub_image: bool,
}

/// One step of the session's own history ([`PlaybackSession::undo`],
/// [`PlaybackSession::redo`]) with everything this window caches re-read after
/// it. `None` when the step did not happen -- nothing to undo, nothing to redo
/// -- and then nothing here moves either.
///
/// A free function over the session and the caches rather than a [`Player`]
/// method: this test binary has no `TestAppContext` to build a `Player` in, so
/// what `^z` does to the session *and* to the caches beside it is pinned here,
/// and [`Player::step_history`] is this call plus the repaint.
///
/// The rate and the two switches are read off the *session*, exactly as the
/// doors that own them do ([`Player::apply_frame_rate`], the `.edith` load): a
/// history entry carries the project's settings beside its lanes
/// (`Project::snapshot`), so a step puts them back -- and a window that kept
/// its old copies counts a cut at the rate just undone, keeps reporting a
/// switch the session no longer has, and pushes that switch at the next session
/// it makes.
pub(crate) fn history_step(
    before: &HistoryBefore,
    caches: &mut SessionCaches,
    session: &mut PlaybackSession,
    step: impl FnOnce(&mut PlaybackSession) -> bool,
) -> Option<HistoryOutcome> {
    if !step(session) {
        return None;
    }
    caches.fps = session.meta().frame_rate;
    caches.proxies_on = session.proxies();
    caches.auto_proxies_on = session.auto_proxies();
    let tracks = session.subtitles();
    let sub_track = sub_pick_after_restore(before.sub_track, before.sub_row.as_ref(), tracks);
    Some(HistoryOutcome {
        sub_track,
        drop_sub_image: tracks.len() != before.sub_rows || sub_track != before.sub_track,
    })
}

/// The half-open range a mark and a mark make, in either order: a person's two
/// keystrokes are not promised to land in order, so this is what turns
/// whichever came second into the pair `Player::start_export` wants a range in
/// -- and never an empty one, since a single frame marked in and out twice is
/// still a frame to export.
/// The range after an in mark at `at` on a timeline `len` frames long. A lone
/// in runs to the end, and an in at or past the out point does too -- the pair
/// is never a single frame the person did not ask for (that was a 0 s export).
pub(crate) fn mark_in(range: Option<(u32, u32)>, at: u32, len: u32) -> (u32, u32) {
    match range {
        Some((_, out)) if at < out => (at, out),
        _ => ordered_range(at, len.max(at)),
    }
}

/// The out mark's pair: a lone out starts at the top, and an out at or before
/// the in point does too.
pub(crate) fn mark_out(range: Option<(u32, u32)>, at: u32) -> (u32, u32) {
    match range {
        Some((start, _)) if at > start => (start, at),
        _ => ordered_range(0, at),
    }
}

pub(crate) fn ordered_range(a: u32, b: u32) -> (u32, u32) {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    if start == end {
        (start, start + 1)
    } else {
        (start, end)
    }
}

/// Opens a written file in the desktop's default player, the same way
/// [`show_in_file_manager`] reveals one: `xdg-open` hands the path to whatever
/// the user has registered for it. Failure is silent here, as the reveal's is:
/// the file is on disk and the toast's other button still reaches it.
pub(crate) fn open_in_default_player(path: &std::path::Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let _ = std::process::Command::new("xdg-open").arg(&path).status();
}

/// Whether two halves of one take meet at `frame` on a lane's clips: touching
/// on the timeline and consecutive frames of the same source -- what a cut
/// leaves and Regroup closes.
pub(crate) fn rejoinable(clips: &[Clip], frame: u32) -> bool {
    clips.windows(2).any(|w| {
        let (a, b) = (&w[0], &w[1]);
        a.end() == frame
            && b.start == frame
            && a.source == b.source
            && a.reversed == b.reversed
            && match a.reversed {
                true => a.in_frame == b.out_frame,
                false => a.out_frame == b.in_frame,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::{mark_in, mark_out, ordered_range, step_name, step_row_name, step_toast};
    use crate::keymap::ActionId;

    /// Undo and Redo name the step they take back or walk to, and say so when none is left;
    /// the toast names the step it just took. An unnamed step reads as plain Undo.
    #[test]
    fn undo_and_redo_name_their_step_in_the_menu_and_the_toast() {
        assert_eq!(step_row_name(false, 2, Some("Split")), "Undo Split");
        assert_eq!(step_row_name(false, 1, None), "Undo");
        assert_eq!(step_row_name(false, 0, Some("Split")), "Nothing to undo");
        assert_eq!(step_row_name(true, 1, Some("Add marker")), "Redo Add marker");
        assert_eq!(step_row_name(true, 1, None), "Redo");
        assert_eq!(step_row_name(true, 0, None), "Nothing to redo");
        assert_eq!(step_toast(false, Some("Split")), "Undid Split");
        assert_eq!(step_toast(true, Some("Split")), "Redid Split");
        assert_eq!(step_toast(false, None), "Undone");
        assert_eq!(step_toast(true, None), "Redone");
    }

    /// A step takes its name from the action's menu name, without the ellipsis. Undo and Redo,
    /// and a name too long for a menu row, carry none.
    #[test]
    fn a_step_takes_its_menu_name() {
        assert_eq!(step_name(ActionId::Cut), Some("Split"));
        assert_eq!(step_name(ActionId::Color), Some("Colour"));
        assert_eq!(step_name(ActionId::AddMarker), Some("Add marker"));
        assert_eq!(step_name(ActionId::Delete), Some("Delete"));
        assert_eq!(step_name(ActionId::ClipEnabled), Some("Enable/disable clip"));
        assert_eq!(step_name(ActionId::NewTone), Some("Add tone"));
        assert_eq!(step_name(ActionId::Undo), None);
        assert_eq!(step_name(ActionId::Redo), None);
        for action in ActionId::ALL {
            if let Some(name) = step_name(action) {
                assert!(name.chars().count() < 30, "{action:?} names its step {name:?}, too long");
                assert!(!name.contains('\u{2026}'), "{action:?} keeps the ellipsis in {name:?}");
            }
        }
    }

    #[test]
    fn lone_marks_run_to_the_end_or_the_top() {
        // The user's bug: I (or O) on its own made a one-frame range.
        assert_eq!(mark_in(None, 0, 3120), (0, 3120));
        assert_eq!(mark_in(None, 120, 3120), (120, 3120));
        assert_eq!(mark_out(None, 3120), (0, 3120));
        assert_eq!(mark_in(Some((10, 50)), 20, 3120), (20, 50));
        assert_eq!(mark_in(Some((10, 50)), 60, 3120), (60, 3120));
        assert_eq!(mark_out(Some((10, 50)), 40), (10, 40));
        assert_eq!(mark_out(Some((10, 50)), 5), (0, 5));
        assert_eq!(mark_in(None, 3120, 3120), (3120, 3121));
    }

    #[test]
    fn ordered_range_never_empty_and_swaps_a_reversed_pair() {
        assert_eq!(ordered_range(5, 10), (5, 10));
        assert_eq!(ordered_range(10, 5), (5, 10));
        assert_eq!(ordered_range(7, 7), (7, 8));
        assert_eq!(ordered_range(0, 0), (0, 1));
    }
}
