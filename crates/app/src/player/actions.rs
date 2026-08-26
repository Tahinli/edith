//! Every action arrives here: the one dispatch, the notice queue, and the
//! delegations to the oracle that say whether an action can be asked for.

use crate::*;

impl Player {
    /// What an action does, wherever it was asked for -- a stroke, or the clip
    /// menu item that names the same action. One table, so the two can never
    /// come to mean different things.
    pub(crate) fn act(&mut self, action: ActionId, window: &mut Window, cx: &mut Context<Self>) {
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
        if matches!(
            action,
            ActionId::Paste
                | ActionId::Cut
                | ActionId::Regroup
                | ActionId::Detach
                | ActionId::Group
                | ActionId::Delete
                | ActionId::Lift
                | ActionId::Undo
                | ActionId::Redo
                | ActionId::AddVideoLane
                | ActionId::RemoveVideoLane
                | ActionId::AddAudioLane
                | ActionId::RemoveAudioLane
                | ActionId::AddSubtitleLane
                | ActionId::RemoveSubtitleLane
                | ActionId::ImportSubtitles
                | ActionId::Crossfade
                | ActionId::Dissolve
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
            ActionId::SetIn => self.set_in(cx),
            ActionId::SetOut => self.set_out(cx),
            ActionId::ClearRange => {
                self.range = None;
                cx.notify();
            }
            ActionId::Export => self.open_export(cx),
            ActionId::Save => self.save_project(cx),
            ActionId::Copy => self.copy_selected(),
            ActionId::Paste => self.paste(cx),
            ActionId::Cut => self.cut(cx),
            ActionId::Regroup => self.regroup(cx),
            ActionId::Crossfade => self.crossfade_selected(cx),
            ActionId::Dissolve => self.dissolve_selected(cx),
            ActionId::Detach => self.detach(cx),
            ActionId::Group => self.group(cx),
            ActionId::Select => self.select_under_playhead(cx),
            ActionId::SelectNext => self.select_step(true, cx),
            ActionId::SelectPrev => self.select_step(false, cx),
            ActionId::SelectAll => self.select_all(cx),
            // Last in escape's precedence chain (render.rs): nothing else
            // claimed the key, so it just empties the selection.
            ActionId::Deselect => {
                self.selected = Selection::new();
                cx.notify();
            }
            ActionId::Delete => self.delete_selected(cx),
            ActionId::Lift => self.lift_selected(cx),
            ActionId::Color => self.open_color(cx),
            ActionId::Transform => self.open_transform(cx),
            ActionId::Fit => self.cycle_fit(cx),
            ActionId::Resolution => self.cycle_resolution(cx),
            // The playhead is what a key zoom is aimed at: it is the one place
            // on the timeline the user is certainly looking at, and keeping it
            // still is what every editor does.
            ActionId::ZoomIn => self.zoom(ZOOM_STEP, None, cx),
            ActionId::ZoomOut => self.zoom(1. / ZOOM_STEP, None, cx),
            ActionId::ZoomFit => self.zoom_fit(cx),
            ActionId::Undo => self.undo(cx),
            ActionId::Redo => self.redo(cx),
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
            ActionId::PasteFilePath => self.paste_file_path(cx),
            ActionId::ToggleMute => self.set_volume(|volume| volume.muted = !volume.muted, cx),
            ActionId::VolumeUp => self.set_volume(|volume| volume.step(true), cx),
            ActionId::VolumeDown => self.set_volume(|volume| volume.step(false), cx),
            ActionId::Equalizer => self.open_eq(cx),
            ActionId::Speed => self.open_speed(cx),
            ActionId::Silence => self.open_silence(cx),
            ActionId::Mix => self.open_mix(None, cx),
            ActionId::SubtitleStyle => self.open_subtitle_style(cx),
            ActionId::ToggleSnap => self.toggle_snap(cx),
            ActionId::ToggleSubtitles => self.toggle_subtitles(cx),
            ActionId::ToggleProxies => self.toggle_proxies(cx),
            ActionId::ToggleAutoProxies => self.toggle_auto_proxies(cx),
            // The keyboard's door to the same list the toolbar button opens.
            // At the window's corner, since a stroke names no place -- and
            // [`menu_at`] keeps it on screen from there.
            ActionId::Theme => self.open_picker(Pick::Theme, Point::default(), cx),
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
            ActionId::FocusPanels => {
                self.focus_surface(crate::ui::stance::Surface::Dock, window, cx);
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
            ActionId::RemoveVideoLane | ActionId::RemoveAudioLane => {
                self.mark_dirty();
                self.remove_lane(lane, cx);
            }
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
        push_notice(&mut self.notices, message);
    }

    /// Answers the message on the bar and brings up the next one. Whether there
    /// was one to answer, because a key that dismissed a notice owes a repaint
    /// and a key that dismissed nothing does not.
    pub(crate) fn dismiss_notice(&mut self) -> bool {
        self.notices.pop_front().is_some()
    }

    /// The magnet off and on again, in words: a snap that stops working
    /// silently reads as a bug, and one that starts working silently reads as
    /// one too. The line goes with it -- nothing is being promised any more.
    pub(crate) fn toggle_snap(&mut self, cx: &mut Context<Self>) {
        self.snap = !self.snap;
        self.snap_cue = None;
        self.ghost.clear();
        self.notify_user(match self.snap {
            true => "SNAP ON — drags land on clip edges, the playhead and the start".into(),
            false => "SNAP OFF — drags land exactly where the hand leaves them".into(),
        });
        cx.notify();
    }

    /// The actions card, from its key, from the panel button, or from its own
    /// row: open, scrolled to the top -- a card that opens where the last
    /// look left it would hide most of the list for a reason nobody remembers.
    pub(crate) fn show_actions(&mut self, cx: &mut Context<Self>) {
        self.keys_open = true;
        self.scroll_keys(None);
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
        enable(action, self.ctx(on))
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
            on.and_then(|(lane, idx)| session.lane_clips(lane).get(idx).map(|clip| (*clip, lane)));
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
            clip,
            // The same pair read in the other index space: a subtitle lane's
            // own, where the box that was clicked is a caption.
            caption,
            caption_link: caption
                .then(|| {
                    on.and_then(|(lane, idx)| session.sub_lane(lane).get(idx))
                        .and_then(|s| s.link)
                })
                .flatten(),
            image: clip.is_some_and(|(clip, _)| {
                session
                    .sources()
                    .get(clip.source)
                    .is_some_and(|s| engine::is_image(&s.path))
            }),
            playhead: frame_at(session.now(), self.fps),
            timeline: true,
            clipboard: !self.clipboard.is_empty(),
            subtitles: !session.subtitles().is_empty(),
            playable: !nothing_to_play(self.active_session()),
            exporting: self.exporting().is_some(),
            picks: picks.len(),
            pick_lanes,
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

    /// Which of [`Repeat`]'s three the window is in, for the hold gate at the
    /// top of the key handler. Not [`Player::modal`]: that asks whether an
    /// overlay is up at all, and here the cards with sliders in them are
    /// exactly the ones that answer differently from the keys menu and the
    /// export card.
    pub(crate) fn repeat_scope(&self) -> Repeat {
        // A number being typed is a value under the arrows, exactly as a card's
        // slider is -- so a held arrow runs it. Asked before the export card
        // below, which otherwise repeats nothing.
        if self.mbps_edit.is_some() {
            Repeat::Card
        } else if self.keys_open || self.export_open || self.exporting().is_some() {
            Repeat::Nothing
        } else if self.eq_open.is_some()
            || self.color_open.is_some()
            || self.transform_open.is_some()
            || self.speed_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
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
            RowItem::RemoveWithClips => {
                self.library_menu = None;
                self.remove_source_and_clips(&menu.path, menu.stream, cx);
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

    /// Marks the playhead as an export's in point. `ordered_range` keeps the
    /// pair legal whichever one lands past the other.
    pub(crate) fn set_in(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let at = frame_at(session.now(), self.fps);
        let out = self.range.map_or(at, |(_, e)| e);
        self.range = Some(ordered_range(at, out));
        cx.notify();
    }

    /// The out point's pair.
    pub(crate) fn set_out(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let at = frame_at(session.now(), self.fps);
        let start = self.range.map_or(at, |(s, _)| s);
        self.range = Some(ordered_range(start, at));
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
        self.mark_dirty();
        let trimmed = self
            .session
            .as_mut()
            .is_some_and(|s| s.trim_clip(lane, idx, edge, to));
        if trimmed {
            self.reset_after_reseek();
            // Loop-trim follows the edge it is trimming: the whole point of
            // the mode is hearing the cut as it moves.
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
            self.notify_user("LOOP-TRIM OFF".into());
            cx.notify();
            return;
        }
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        self.loop_trim = self.cut_span(lane, idx);
        if self.loop_trim.is_some() {
            self.notify_user("LOOP-TRIM ON — looping the subject cut while you trim".into());
        }
        cx.notify();
    }

    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) {
        if self.session.as_mut().is_some_and(PlaybackSession::undo) {
            self.reset_after_reseek();
        }
        self.selected.clear();
        cx.notify();
    }

    pub(crate) fn redo(&mut self, cx: &mut Context<Self>) {
        if self.session.as_mut().is_some_and(PlaybackSession::redo) {
            self.reset_after_reseek();
        }
        self.selected.clear();
        cx.notify();
    }
}

/// The half-open range a mark and a mark make, in either order: a person's two
/// keystrokes are not promised to land in order, so this is what turns
/// whichever came second into the pair `Player::start_export` wants a range in
/// -- and never an empty one, since a single frame marked in and out twice is
/// still a frame to export.
pub(crate) fn ordered_range(a: u32, b: u32) -> (u32, u32) {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    if start == end {
        (start, start + 1)
    } else {
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::ordered_range;

    #[test]
    fn ordered_range_never_empty_and_swaps_a_reversed_pair() {
        assert_eq!(ordered_range(5, 10), (5, 10));
        assert_eq!(ordered_range(10, 5), (5, 10));
        assert_eq!(ordered_range(7, 7), (7, 8));
        assert_eq!(ordered_range(0, 0), (0, 1));
    }
}
