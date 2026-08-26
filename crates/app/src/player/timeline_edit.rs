//! Editing the timeline itself: the cuts, the clipboard, the drags, the
//! snapping and the trims.

use crate::*;

/// Whether a seam that just lost its drag owes a disk write -- exactly the
/// two persisted seams ([`Split::PERSISTED`]), same rule `drag_release`'s own
/// `matches!` already lived by. Pulled out to its own free function, taking
/// no `Context`, so [`Player::drag_left_window`]'s save-on-leave guard is
/// checkable without a `TestAppContext` this test binary has none of (see
/// `tests/media.rs`'s own note on the same limit) -- the wiring that calls it
/// from a live `MouseExitEvent` still cannot be, and is proven by driving
/// instead.
pub(crate) fn split_drag_owes_save(split: Option<Split>) -> bool {
    split.is_some_and(|split| Split::PERSISTED.contains(&split))
}

impl Player {
    /// Splits the clip under the playhead. Metadata only: the timeline->source
    /// mapping is unchanged, so nothing reseeks and no flag is touched.
    pub(crate) fn cut(&mut self, cx: &mut Context<Self>) {
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
            // The ledger's last action (DESIGN §5) reads the notice queue's
            // back, and a split used to leave nothing there -- two clips
            // appeared on the bench while the ledger still named whatever
            // came before. `NOTICE_TELL`'s grey "told you" (§8) self-fades,
            // so this does not linger over the picture; it just gives the
            // ledger something true to say. Held `,`/`.`/`[`/`]` (walk, trim)
            // stay silent on purpose -- those fire many times a second and a
            // notice per keystroke would turn the ledger into a firehose,
            // exactly what §8's "one at a time" is against; a single `s` is
            // not that.
            // The legacy room has no ledger reading this queue and never
            // notified on a split before -- gated so that room's behaviour
            // stays exactly as it was.
            let cut = session.cut_at(f64::from(at) / self.fps);
            if self.darkroom {
                if cut {
                    self.notify_user("SPLIT".into());
                } else {
                    self.notify_user("NOTHING TO SPLIT — the playhead is already on a cut".into());
                }
            }
        }
        self.selected.clear();
        cx.notify();
    }

    /// Rejoins whatever meets under the playhead and puts it back in one group
    /// -- the inverse of [`Player::cut`], and metadata only like it. The engine
    /// decides what is joinable; a refusal is worded here, because `false` is
    /// all it says and a key that looks broken is worse than one that explains
    /// itself.
    pub(crate) fn regroup(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(session) = &mut self.session {
            if session.regroup_at(session.now()) {
                self.selected.clear();
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
    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match (&mut self.session, self.selected.anchor()) {
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

    /// Puts the selection into one group. Two paths, by how much of a
    /// selection there is:
    ///
    /// * **One pick** -- the way back from [`Player::detach`], and the way to
    ///   group a picture with sound it was never opened with. The partner is
    ///   not clicked because there is nothing to choose: the clip covering
    ///   exactly the same frames on another track, and the engine words what to
    ///   do when none does.
    ///
    /// * **A ctrl-click selection** -- the group a hand builds: every pick it
    ///   names, clips and captions alike, into one id, whatever frames each of
    ///   them covers. The members keep their own offsets, which is the point:
    ///   from here they move, trim and delete together.
    pub(crate) fn group(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // The picks the selection holds that still name something: a stale
        // index (a stroke nobody saw) is not a thing to group.
        let picks = self.marks().0;
        match (&mut self.session, self.selected.anchor(), picks.len()) {
            (_, None, _) => {
                self.notify_user("NOTHING GROUPED — click one of the halves first".into())
            }
            // The hand's group: every pick, one id.
            (Some(session), _, 2..) => {
                if let Err(e) = session.group_all(&picks) {
                    self.notify_user(format!("NOT GROUPED — {e}").into());
                }
            }
            // One pick that no longer names anything.
            (Some(_), Some(_), 0) => {
                self.notify_user("NOTHING GROUPED — that clip is no longer there".into())
            }
            // One pick: the partner flow it always was.
            (Some(session), Some((lane, idx)), _) => match span_partner(session, lane, idx) {
                Some((other, o_idx)) => {
                    if let Err(e) = session.group(lane, idx, other, o_idx) {
                        self.notify_user(format!("NOT GROUPED — {e}").into());
                    }
                }
                None => self.notify_user(
                    "NOTHING TO GROUP WITH — no clip on another track covers exactly these \
                         frames; ctrl-click the clips to group instead"
                        .into(),
                ),
            },
            (None, ..) => {}
        }
        cx.notify();
    }

    /// Drops the selected clip and closes the hole: a whole take goes, both
    /// lanes of it, and everything after it moves up. A half with no take under
    /// it in the video lane -- what a lift leaves behind -- has nothing to
    /// ripple, so that one is lifted instead. The engine reseeks itself, so all
    /// this owes is the flag reset.
    pub(crate) fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let selected = self.selected.anchor();
        self.selected.clear();
        // A caption is marked in its own lane's index space -- that lane holds
        // no `Clip` -- so the same key reaches it through the one removal a
        // placed subtitle has ([`Player::lift_sub`], one undo step), unless a
        // hand put it in a group: then it goes the way its group goes.
        // Parity: the box on a subtitle lane goes by the stroke every other box
        // goes by.
        if let Some((lane, idx)) = selected.filter(|(lane, _)| lane.kind == LaneKind::Subtitle) {
            // A caption carrying a group id is a member, and a member goes the
            // way its group goes -- the engine's own door decides how much of
            // that is media (and so a reseek) and how much is a lift.
            let grouped = self.session.as_ref().is_some_and(|session| {
                session
                    .sub_lane(lane)
                    .get(idx)
                    .is_some_and(|s| s.link.is_some())
            });
            match grouped {
                true => {
                    if self
                        .session
                        .as_mut()
                        .is_some_and(|session| session.delete_sub(lane, idx))
                    {
                        self.reset_after_reseek();
                    }
                }
                false => self.lift_sub(lane, idx, cx),
            }
            cx.notify();
            return;
        }
        // Whichever lane it was clicked in: the index is that lane's own, and
        // the engine cuts what the clip covers out of the lanes it covers -- a
        // lone clip's span out of every lane, a grouped member's own span out
        // of its own lane and its caption members off their lanes. What is not
        // a whole take is lifted instead, which is what reaches a clip on an
        // added track ([`whole_take`]).
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
    pub(crate) fn lift_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let selected = self.selected.anchor();
        self.selected.clear();
        match (&mut self.session, selected) {
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

    /// Copies the selection: one entry for a plain click, the whole
    /// ctrl-click set -- every pick, its own lane and its own frames -- for
    /// one made over more than a clip. Out of the lane each was clicked in:
    /// the audio half of a group is a different clip from the video one, and
    /// copying the wrong lane's frames is a paste of the wrong thing. Nothing
    /// on screen changes, so no notify. Empty rather than touched at all for
    /// a selection naming nothing any more, so a stale Copy leaves the
    /// clipboard exactly as a stale Paste would find it.
    pub(crate) fn copy_selected(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let picks: Vec<(Lane, Clip)> = self
            .selected
            .picks()
            .iter()
            .filter_map(|&(lane, idx)| Some((lane, session.lane_clips(lane).get(idx).copied()?)))
            .collect();
        if !picks.is_empty() {
            self.clipboard = picks;
        }
    }

    /// The picks and their group ids, in click order: the two halves of what
    /// [`marked`] asks about every box on the bed -- which picks there are, and
    /// which groups they carry (a caption's included, which is what marks the
    /// clip it was pinned to). Picks an edit has left stale name nothing and
    /// contribute no id.
    pub(crate) fn marks(&self) -> (Vec<(Lane, usize)>, Vec<Option<u32>>) {
        let Some(session) = &self.session else {
            return (Vec::new(), Vec::new());
        };
        let (mut picks, mut links) = (Vec::new(), Vec::new());
        for &(lane, idx) in self.selected.picks() {
            let link = match lane.kind {
                LaneKind::Subtitle => session.sub_lane(lane).get(idx).map(|s| s.link),
                _ => session.lane_clips(lane).get(idx).map(|c| c.link),
            };
            if let Some(link) = link {
                picks.push((lane, idx));
                links.push(link);
            }
        }
        (picks, links)
    }

    /// Drops the copied clip -- or the whole copied set -- in at the
    /// playhead. A single clipboard entry takes the door it always has
    /// ([`PlaybackSession::paste_at`]): across `V1`/`A1`, splitting whatever
    /// it lands inside of and rippling the room open, byte-identical to
    /// before a set could be copied at all. More than one takes
    /// [`PlaybackSession::paste_set_at`] instead: every member lands on the
    /// lane it was copied off, at the same distance from the others it had on
    /// the bed it was copied from, refused whole rather than opening room --
    /// see [`Project::paste_set`] for why a set-paste cannot ripple. The
    /// engine reseeks itself either way, so like a delete this owes the flag
    /// reset -- and the selection, whose index the insert has just moved.
    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        let pasted = match (&mut self.session, self.clipboard.as_slice()) {
            (Some(session), [(_, clip)]) => session.paste_at(session.now(), *clip),
            (Some(session), items) if !items.is_empty() => {
                session.paste_set_at(session.now(), items)
            }
            _ => false,
        };
        if pasted {
            self.selected.clear();
            self.reset_after_reseek();
        } else if self.clipboard.len() > 1 {
            // The set-paste's own refusal, worded like a set-drag's
            // ([`Player::move_clip`]) -- a single-clip paste stays silent on
            // failure exactly as it always has.
            self.notify_user(
                "NOT PASTED — another clip already covers where one of the copied clips would \
                 land"
                    .into(),
            );
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
    ///
    /// A dragged clip that is *itself* one of the selection's picks carries the
    /// whole selection with it: every other pick moves the same frame delta,
    /// all-or-nothing ([`PlaybackSession::move_selection_to`]), and the picks
    /// survive the move -- their indices remapped rather than cleared, since a
    /// pick that travels still names the clip it named before. A clip dragged
    /// from outside the selection moves alone, exactly as it always has.
    pub(crate) fn move_clip(
        &mut self,
        from: Lane,
        idx: usize,
        to: Lane,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        self.mark_dirty();
        let (Some((start, _)), Some(was)) = (
            self.drop_frame(from, idx, x),
            self.session
                .as_ref()
                .and_then(|session| session.lane_clips(from).get(idx).map(|c| c.start)),
        ) else {
            return;
        };
        let set_move = self.selected.contains((from, idx)) && self.selected.len() > 1;
        let picks: Vec<(Lane, usize)> = self.selected.picks().to_vec();
        // Every pick's own start, read before the move touches anything -- the
        // only way to find a pick again afterwards, since a lane change or an
        // insert can move the index it was named by.
        let pre_starts: Vec<Option<u32>> = picks
            .iter()
            .map(|&(lane, i)| {
                self.session
                    .as_ref()
                    .and_then(|session| session.lane_clips(lane).get(i).map(|c| c.start))
            })
            .collect();
        let moved = self.session.as_mut().is_some_and(|session| match set_move {
            true => session.move_selection_to(&picks, from, idx, to, start),
            false => session.move_clip_to(from, idx, to, start),
        });
        let (kind, lanes) = match from.kind {
            LaneKind::Video => ("picture", "video"),
            LaneKind::Audio => ("sound", "audio"),
            // Never dragged from here yet -- a subtitle lane holds no `Clip`,
            // so `lane_clips` is empty and the drag above never starts on one.
            LaneKind::Subtitle => ("caption", "subtitle"),
        };
        match moved {
            true => {
                // Every pick named a `(lane, ...)` by index into that lane's
                // clips, sorted by start -- exactly what an insert or a lane
                // change reorders. Re-read by the frame each pick's clip now
                // starts at (recorded before the move moved anything), so the
                // selection survives naming the clips it named, not the slots
                // they used to sit in.
                if set_move {
                    self.selected =
                        self.remap_selection(&picks, &pre_starts, from, idx, to, was, start);
                } else {
                    self.selected.clear();
                }
                self.reset_after_reseek();
            }
            // The three ways a drag is refused, told apart by what the
            // front-end already knows: a lane's kind, and where the clip was.
            // Everything else that could refuse (a clip that is not there)
            // cannot be dragged.
            false if from.kind != to.kind => self.notify_user(
                format!(
                    "NOT ON {} — that is a {kind} clip; drop it on a {lanes} lane",
                    to.label()
                )
                .into(),
            ),
            // Picked up and put back down where it was: a click, and a click
            // says nothing.
            false if from == to && start == was => {}
            false if set_move => self.notify_user(
                format!(
                    "NOT MOVED — {} clips selected, and another clip already covers where one of \
                     them would land on {}",
                    picks.len(),
                    to.label()
                )
                .into(),
            ),
            false => self.notify_user(
                format!(
                    "NOT MOVED — another clip already covers those frames on {}",
                    to.label()
                )
                .into(),
            ),
        }
        cx.notify();
    }

    /// What a selection's picks are called after a set-move that shifted every
    /// one of them by `landed - was` timeline frames (the delta the dragged
    /// clip's own head travelled, which every pick travelled too -- see
    /// [`Project::move_selection`]): the clip a pick named is found again by
    /// where it now starts, on the lane it stays on (or `to`, for the one
    /// that changed lane), rather than by the index it had, which a lane
    /// change or an insert can have moved. A pick whose clip cannot be found
    /// there any more (a bad index handed in) is dropped rather than guessed
    /// at.
    fn remap_selection(
        &self,
        picks: &[(Lane, usize)],
        pre_starts: &[Option<u32>],
        from: Lane,
        dragged: usize,
        to: Lane,
        was: u32,
        landed: u32,
    ) -> Selection {
        let mut out = Selection::new();
        let Some(session) = self.session.as_ref() else {
            return out;
        };
        let delta = i64::from(landed) - i64::from(was);
        for (&pick, &old_start) in picks.iter().zip(pre_starts) {
            let Some(old_start) = old_start else {
                continue;
            };
            let want = (i64::from(old_start) + delta) as u32;
            // Every pick but the dragged clip itself stays on the lane it was
            // already on; only the exact clip the hand let go of changed
            // lane, so only its own pick is found on `to` rather than `from`.
            let now_lane = if pick == (from, dragged) { to } else { pick.0 };
            if let Some(found) = session
                .lane_clips(now_lane)
                .iter()
                .position(|c| c.start == want)
            {
                out.add((now_lane, found));
            }
        }
        out
    }

    /// The whole of a palette track as a placement: from its own first
    /// microsecond to its last cue, and as many timeline frames as that is worth
    /// at this rate. What a row dragged out of the Subtitles list carries, and
    /// what the ghost is drawn from -- `start` is ignored by
    /// [`PlaybackSession::place_sub`], which takes the frame the hand let go on.
    ///
    /// `None` for a track with nothing to place: one that could not be read, and
    /// one that genuinely has no cues.
    pub(crate) fn sub_of_track(&self, track: usize) -> Option<SubClip> {
        let cues = &self.session.as_ref()?.subtitles().get(track)?.cues;
        let out_us = cues.iter().map(|c| c.end_us).max()?;
        Some(SubClip {
            start: 0,
            frames: frames_of_us(out_us, self.fps),
            track,
            in_us: 0,
            out_us,
            // A palette row arrives in no group: pinning its words to a clip is
            // a hand's decision, made with the Group row afterwards.
            link: None,
        })
    }

    /// A palette row let go at window `x` over subtitle lane `to`: the whole
    /// track goes down where the hand left it, one undo step
    /// ([`PlaybackSession::place_sub`]). Every refusal there is words -- an
    /// overlap, a lane that is not a subtitle lane, a track that is not there --
    /// and they are shown as the engine worded them.
    pub(crate) fn place_sub(&mut self, track: usize, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mark_dirty();
        self.snap_cue = None;
        self.ghost.clear();
        let at = self.place_frame(x).0;
        // What the mark is on, before the lane is renumbered: a caption's start
        // frame is its name on its lane ([`sub_mark`]).
        let marked = self
            .selected
            .anchor()
            .filter(|&(lane, _)| lane == to)
            .and_then(|(lane, idx)| Some(self.session.as_ref()?.sub_lane(lane).get(idx)?.start));
        let text = match (self.sub_of_track(track), &mut self.session) {
            (Some(sub), Some(session)) => match session.place_sub(to, at, sub) {
                Ok(()) => None,
                Err(e) => Some(format!("NOT PLACED — {e}")),
            },
            // The row is greyed and says why in the list; here it says why at
            // the moment somebody tried to use it anyway.
            (None, Some(_)) => Some(
                "NOT PLACED — that subtitle track has no cues to place; the list says why"
                    .to_string(),
            ),
            (_, None) => Some("NOT PLACED — open a file first".to_string()),
        };
        match (text, marked) {
            (Some(text), _) => self.notify_user(text.into()),
            // It went down, and a lane holds its captions in start order
            // ([`Project::place_sub`]'s sorted insert), so one placed *before*
            // the marked caption slid it one along and the index left behind
            // names its new neighbour -- the caption the next Delete would take.
            // The mark goes back on the caption it was on, every *other* pick
            // this lane held goes with the indices that moved (they name
            // neighbours now), and picks on other lanes keep what they name:
            // a placement renumbers this lane alone.
            (None, Some(start)) => {
                let mark = self
                    .session
                    .as_ref()
                    .and_then(|session| sub_mark(session.sub_lane(to), start))
                    .map(|i| (to, i));
                let mut kept = Selection::new();
                for &(lane, idx) in self.selected.picks() {
                    if lane != to {
                        kept.add((lane, idx));
                    }
                }
                if let Some(mark) = mark {
                    kept.add(mark);
                }
                self.selected = kept;
            }
            // Nothing marked on this lane, but the placement renumbered it all
            // the same: a pick left on a slid index names the wrong caption,
            // and the discipline of every renumbering edit applies.
            (None, None) => {
                let mut kept = Selection::new();
                for &(lane, idx) in self.selected.picks() {
                    if lane != to {
                        kept.add((lane, idx));
                    }
                }
                self.selected = kept;
            }
        }
        cx.notify();
    }

    /// Which index its lane holds the dragged placement at *now*
    /// ([`Player::dragged`]'s twin, through the same [`live_idx`]): a stroke
    /// during the gesture -- an undo, another drop -- moves the indices gpui
    /// froze into the payload.
    pub(crate) fn dragged_sub(&self, drag: &SubDrag) -> Option<usize> {
        live_idx(
            self.session.as_ref()?.sub_lane(drag.lane),
            drag.idx,
            drag.sub,
        )
    }

    /// A placed caption let go at window `x` over subtitle lane `to`: it lands
    /// with its head where the hand carried it, on the lane it was dropped on --
    /// one undo step, and none at all for a drop that changed nothing, which is
    /// why an `Ok` is never a toast ([`Project::move_sub`]). An overlap and a
    /// lane of the wrong kind are refused in the engine's own words.
    pub(crate) fn move_sub(&mut self, drag: &SubDrag, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mark_dirty();
        self.snap_cue = None;
        self.ghost.clear();
        let Some(idx) = self.dragged_sub(drag) else {
            return;
        };
        // Asked before the edit, on the indices the press captured: the
        // engine's own grouped-with-clips answer, which is exactly when its
        // move reseeks -- a caption grouped only with other captions moves no
        // media, and a flag reset without a delivered frame would latch the
        // seek wait forever.
        let grouped = self
            .session
            .as_ref()
            .is_some_and(|session| session.caption_grouped_with_clips(drag.lane, idx));
        let start = self.sub_drop_frame(drag.sub, x).0;
        match self
            .session
            .as_mut()
            .map(|session| session.move_sub(drag.lane, idx, to, start))
        {
            Some(Err(e)) => {
                // Nothing moved ([`Project::move_sub`] refuses before its
                // snapshot), so nothing was renumbered and the mark is left
                // exactly where it was. Re-reading it off the frame the drop
                // *asked* for used to hand it to whatever caption already
                // covered that frame -- the drop at the left edge saturates to
                // 0 ([`landing`]), so a refused drag marked, and the next
                // Delete lifted, a caption the hand never touched.
                self.notify_user(format!("NOT MOVED — {e}").into());
            }
            // A lane holds its captions in start order, so the drop moved the
            // mark as well as the box: it is re-read off where the caption
            // landed rather than left on the index it had, which after the move
            // is a neighbour's. A grouped-with-clips caption dragged media with
            // it -- the engine reseeks itself, and this owes the flag reset a
            // clip's drop pays at the same moment.
            Some(Ok(())) => {
                let mark = self
                    .session
                    .as_ref()
                    .and_then(|session| sub_mark(session.sub_lane(to), start))
                    .map(|i| (to, i));
                self.selected = match mark {
                    Some(mark) => {
                        let mut sel = Selection::new();
                        sel.set_one(mark);
                        sel
                    }
                    None => Selection::new(),
                };
                if grouped {
                    self.reset_after_reseek();
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// A caption off its lane, leaving the gap, one undo step
    /// ([`PlaybackSession::lift_sub`]). The palette row it played stays in the
    /// list, which is what makes this a lift and not a removal -- and the way a
    /// subtitle lane is emptied so it can be taken off at all.
    ///
    /// The one door out for a placed caption: the Delete key
    /// ([`Player::delete_selected`]) and the Delete row of its own menu both
    /// land here, so there is one removal and one undo step however it is asked
    /// for.
    pub(crate) fn lift_sub(&mut self, lane: Lane, idx: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mark_dirty();
        let lifted = self
            .session
            .as_mut()
            .is_some_and(|session| session.lift_sub(lane, idx));
        let text = match lifted {
            true => format!(
                "CAPTION LIFTED — {} puts it back",
                self.keymap.display(ActionId::Undo)
            ),
            false => "NOTHING LIFTED — that caption is not there any more".to_string(),
        };
        if lifted {
            // Everything after it on that lane slid down one ([`Vec::remove`]),
            // so a mark or an open menu left at or past it names a *different*
            // caption now -- and the next Delete would take that one. Both go
            // with the caption they were on.
            if self
                .selected
                .anchor()
                .is_some_and(|(l, i)| l == lane && i >= idx)
            {
                self.selected.clear();
            }
            self.context_menu = None;
        }
        self.notify_user(text.into());
        cx.notify();
    }

    /// A subtitle lane's dot: the one lane whose captions are over the picture.
    /// A pick and not a toggle -- picking one puts the one that was showing
    /// away, because two lanes of different words at one moment is two plates
    /// nobody can read, and it is said out loud for that reason: what the click
    /// did *and* what it undid.
    ///
    /// The lanes it takes off the picture keep their captions and are still
    /// exported, every one of them ([`Player::start_export`]).
    pub(crate) fn show_sub_lane(&mut self, lane: Lane, cx: &mut Context<Self>) {
        let was = self.active_sub_lane();
        self.sub_lane = Some(lane);
        let text = match was.filter(|&old| old != lane) {
            Some(old) => format!(
                "{} IS SHOWN — {} is off the picture; every subtitle track is still exported",
                lane.label(),
                old.label()
            ),
            None => format!(
                "{} IS SHOWN — its captions are over the picture",
                lane.label()
            ),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// The subtitle lane whose captions are drawn: the pick resolved against the
    /// lanes the timeline has right now ([`active_lane`]), so it can never name
    /// a lane that is not there. `None` with no subtitle lane at all.
    pub(crate) fn active_sub_lane(&self) -> Option<Lane> {
        let lanes = self.session.as_ref()?.subtitle_lanes();
        active_lane(self.sub_lane, &lanes)
    }

    /// Whether that subtitle lane's captions are drawn -- read by the header
    /// that offers the pick and by the plate over the picture.
    pub(crate) fn sub_lane_on(&self, lane: Lane) -> bool {
        self.active_sub_lane() == Some(lane)
    }

    /// Where a caption let go at window `x` wants its head: [`Player::drop_frame`]'s
    /// twin, and the same [`landing`] -- the grab offset the press noted, and the
    /// snap onto the edges the rest of the timeline offers, so a caption lands on
    /// the cut it is spoken over.
    pub(crate) fn sub_drop_frame(&self, sub: SubClip, x: Pixels) -> (u32, Option<u32>) {
        let marks = self.snap_targets(None);
        landing(
            self.frame_under(x),
            self.grab,
            sub.frames,
            self.snap,
            self.snap_frames(),
            &marks,
        )
    }

    /// The shadow a caption in the hand would fill, on the lane the pointer is
    /// over: [`Player::preview_ghost`]'s twin, refused for any lane that is not a
    /// subtitle lane -- which is the answer [`Project::move_sub`] gives at the
    /// release.
    pub(crate) fn preview_ghost_sub(
        &mut self,
        drag: &SubDrag,
        to: Lane,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let (start, _) = self.sub_drop_frame(drag.sub, x);
        let ghost = Ghost {
            lane: to,
            start,
            frames: drag.sub.frames,
            tint: CLIP_TEXT(),
            refused: to.kind != LaneKind::Subtitle,
        };
        self.set_ghost(vec![ghost], cx);
    }

    /// The same for a palette row on its way down: it lands at the frame it is
    /// let go on ([`Player::place_frame`]) and is as long as the whole track it
    /// names.
    pub(crate) fn preview_ghost_pick(
        &mut self,
        track: usize,
        to: Lane,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let ghost = Ghost {
            lane: to,
            start: self.place_frame(x).0,
            frames: self.sub_of_track(track).map_or(0, |sub| sub.frames),
            tint: CLIP_TEXT(),
            refused: to.kind != LaneKind::Subtitle,
        };
        self.set_ghost(vec![ghost], cx);
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
    pub(crate) fn drop_frame(
        &self,
        from: Lane,
        idx: usize,
        x: Pixels,
    ) -> Option<(u32, Option<u32>)> {
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
    pub(crate) fn place_frame(&self, x: Pixels) -> (u32, Option<u32>) {
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
    pub(crate) fn dragged(&self, drag: &ClipDrag) -> Option<usize> {
        let session = self.session.as_ref()?;
        live_idx(session.lane_clips(drag.lane), drag.idx, drag.clip)
    }

    /// The line while the clip is still in the hand: the very answer
    /// [`Player::drop_frame`] will commit, worked out on every move of the drag,
    /// so what the eye was promised is where the release puts it. A pointer that
    /// has wandered off the bed promises nothing.
    pub(crate) fn preview_drop(
        &mut self,
        from: Lane,
        idx: usize,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let cue = self.drop_frame(from, idx, x).and_then(|(_, cue)| cue);
        self.set_cue(cue, x, cx);
    }

    /// The same line for a library row on its way to a lane: it goes down at
    /// the frame it is let go on ([`Player::place_frame`]), so that frame is
    /// what snaps and what is drawn.
    pub(crate) fn preview_place(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let cue = self.place_frame(x).1;
        self.set_cue(cue, x, cx);
    }

    /// The shadow the clip in the hand would fill, on the lane the pointer is
    /// over: its head where [`Player::drop_frame`] says the release will put it
    /// -- the same call the drop makes, so the box drawn and the box committed
    /// are one answer -- and its own length at this zoom. A lane of the other
    /// kind refuses the drop ([`Project::move_clip`]), and the shadow says so
    /// before the release does.
    ///
    /// When the dragged clip is itself one of a multi-pick selection (the same
    /// `set_move` test [`Player::move_clip`] commits by), every other pick
    /// draws its own shadow too, at the delta the anchor above is landing at --
    /// [`Project::move_selection`]'s own clamp is not reread here, so a wall
    /// that would narrow the group's travel is seen only at the release, not
    /// in the shadow; the anchor's own room is still exact; corner-cut, ceiling
    /// a shadow a member or two too wide on a tight bed, upgrade is exposing
    /// `move_room` to preview against.
    pub(crate) fn preview_ghost(
        &mut self,
        drag: &ClipDrag,
        to: Lane,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.dragged(drag) else {
            self.set_ghost(Vec::new(), cx);
            return;
        };
        let Some((start, _)) = self.drop_frame(drag.lane, idx, x) else {
            self.set_ghost(Vec::new(), cx);
            return;
        };
        let anchor = Ghost {
            lane: to,
            start,
            frames: drag.clip.frames(),
            tint: self.clip_tint(drag.clip.source),
            refused: drag.lane.kind != to.kind,
        };
        let mut ghosts = vec![anchor];
        if self.selected.contains((drag.lane, idx)) && self.selected.len() > 1 {
            let delta = i64::from(start) - i64::from(drag.clip.start);
            for &(lane, i) in self.selected.picks() {
                if (lane, i) == (drag.lane, idx) {
                    continue;
                }
                let Some(clip) = self
                    .session
                    .as_ref()
                    .and_then(|session| session.lane_clips(lane).get(i).copied())
                else {
                    continue;
                };
                let want = (i64::from(clip.start) + delta).max(0) as u32;
                ghosts.push(Ghost {
                    lane,
                    start: want,
                    frames: clip.frames(),
                    tint: self.clip_tint(clip.source),
                    refused: anchor.refused,
                });
            }
        }
        self.set_ghost(ghosts, cx);
    }

    /// The line the track in the hand would drop into, on the row the pointer
    /// is over: at that row's top edge when the header is coming up from below
    /// and at its bottom edge when it is going down, which is the slot
    /// [`Player::reorder_lane`] commits to at the release. Nothing at all over
    /// its own row, where a release changes nothing.
    pub(crate) fn preview_lane_drop(&mut self, from: Lane, onto: Lane, cx: &mut Context<Self>) {
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
    pub(crate) fn forget_lane_drop(&mut self, lane: Lane, cx: &mut Context<Self>) {
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
    pub(crate) fn preview_ghost_asset(
        &mut self,
        path: &Path,
        to: Lane,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
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
        self.set_ghost(vec![ghost], cx);
    }

    /// Sets the shadow, or takes it away, repainting only when it moved -- the
    /// listeners below run it on every pointer sample of a drag. Cleared by the
    /// root and set again by the lane under the pointer, in that order (gpui
    /// runs the capture phase parent-first), so a pointer over no lane at all
    /// leaves nothing drawn.
    pub(crate) fn set_ghost(&mut self, ghost: Vec<Ghost>, cx: &mut Context<Self>) {
        if ghost != self.ghost {
            self.ghost = ghost;
            cx.notify();
        }
    }

    /// The swatch a clip from source `n` wears: [`source_tint`] over the first
    /// source entry naming that *file*, since two audio streams of one file are
    /// two sources and one colour. Every box on a lane and every ghost a drag
    /// draws asks this, so the shadow is recognisably the thing in the hand.
    pub(crate) fn clip_tint(&self, source: usize) -> u32 {
        self.sources()
            .get(source)
            .and_then(|entry| file_tint(self.sources(), &entry.path))
            .unwrap_or_else(|| source_tint(source))
    }

    pub(crate) fn sources(&self) -> &[Source] {
        self.session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources)
    }

    /// Sets the line, or takes it away, and repaints only when it moved: a
    /// pointer dragged off the bed (up to the library, say) is not promising a
    /// landing any more.
    pub(crate) fn set_cue(&mut self, cue: Option<u32>, x: Pixels, cx: &mut Context<Self>) {
        let bed = self.ruler.get();
        let cue = cue.filter(|_| x >= bed.left() && x <= bed.right());
        if cue != self.snap_cue {
            self.snap_cue = cue;
            cx.notify();
        }
    }

    /// Every edge this timeline offers a gesture: [`snap_marks`] over all of its
    /// lanes, so a clip meets a take one track over as readily as one beside it.
    pub(crate) fn snap_targets(&self, skip: Option<(Lane, usize)>) -> Vec<u32> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let lanes = session.lanes();
        let clips: Vec<&[Clip]> = lanes.iter().map(|&lane| session.lane_clips(lane)).collect();
        // The dragged placement's group, whatever kind it is: the clips of the
        // group travel with a clip's drag, and a caption's drag -- which has no
        // clip of its own to skip by -- must not snap onto the very clips it is
        // carrying.
        let skip_link = skip.and_then(|(lane, idx)| {
            match lane.kind {
                LaneKind::Subtitle => session.sub_lane(lane).get(idx).map(|s| s.link),
                _ => session.lane_clips(lane).get(idx).map(|c| c.link),
            }
            .flatten()
        });
        let skip = skip.and_then(|(lane, idx)| Some((lanes.iter().position(|&l| l == lane)?, idx)));
        snap_marks(&clips, skip, skip_link, frame_at(session.now(), self.fps))
    }

    /// Where a gesture at `raw` lands and the mark that pulled it there, with
    /// the switch honoured: snapping off, nothing moves and no line is drawn.
    pub(crate) fn snap_to(&self, raw: u32, len: u32, marks: &[u32]) -> (u32, Option<u32>) {
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
    pub(crate) fn sync_frames(&self) -> Vec<u32> {
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
    pub(crate) fn cut_frame(&self, raw: u32) -> u32 {
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
    pub(crate) fn on_sync_point(&self) -> bool {
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
                                keys.binary_search(&(clip.in_frame + (now - clip.start)))
                                    .is_ok()
                            })
                })
        })
    }

    /// Puts the playhead on the sync point before or after it -- the keyboard's
    /// half of placing a cut where the export can copy it, and the only way to
    /// reach one exactly on a timeline zoomed out to a whole film, where one
    /// pixel is seconds.
    pub(crate) fn jump_sync(&mut self, forward: bool, cx: &mut Context<Self>) {
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
    pub(crate) fn snap_frames(&self) -> u32 {
        self.scale.snap_frames(self.fps)
    }

    /// Opens the clip menu on the box under the pointer, from the right button
    /// wherever it was pressed on that box -- its middle or one of its edge
    /// strips, which cover the middle's own listener. Selecting first is part of
    /// it: every item acts on the clip the menu names -- and a right-click on
    /// one of the clips a ctrl-click selection already holds keeps that
    /// selection, so the menu's Group is about the whole of it. A right-click
    /// anywhere else is the single mark, exactly as a left press is.
    pub(crate) fn open_menu(
        &mut self,
        lane: Lane,
        idx: usize,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.modal() {
            return;
        }
        if !self.selected.contains((lane, idx)) {
            self.select((lane, idx), cx);
        }
        self.context_menu = Some(ContextMenu {
            lane,
            on: MenuOn::Clip(idx),
            at,
            details: false,
        });
        cx.notify();
    }

    /// Opens the same menu on the gap under the pointer instead of a clip --
    /// the empty-bench-space door to [`Player::close_gap`]. Nothing is
    /// selected: a gap owns no clip to select, unlike [`Player::open_menu`].
    pub(crate) fn open_gap_menu(
        &mut self,
        lane: Lane,
        start: u32,
        frames: u32,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.modal() {
            return;
        }
        self.context_menu = Some(ContextMenu {
            lane,
            on: MenuOn::Gap(start, frames),
            at,
            details: false,
        });
        cx.notify();
    }

    /// Closes the gap `(start, frames)` -- Premiere's "Close Gap", scoped to
    /// `lane` and, when the gap borders a take, to the take's other lane too
    /// ([`engine::PlaybackSession::gap_take_scope`]): a lane not sharing the
    /// take never moves, so a neighbour that meant to keep its own silence is
    /// untouched, but the two halves of a linked A/V take close together --
    /// the alternative is a ripple that pulls a take's picture and sound out
    /// of step with each other, which no lane's own gap is worth. A take
    /// whose gap does not match on its other lane is refused in words, not
    /// silence ([`engine::Project::gap_take_scope`]).
    pub(crate) fn close_gap(
        &mut self,
        lane: Lane,
        start: u32,
        frames: u32,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let scope = match session.gap_take_scope(lane, start, frames) {
            Ok(scope) => scope,
            Err(e) => {
                self.notify_user(e.to_string().into());
                cx.notify();
                return;
            }
        };
        match session.cut_regions(&[(start, frames)], &scope) {
            Ok(()) => {
                self.mark_dirty();
                // The ripple moves every index after the gap on the scoped
                // lanes, the same reason a delete drops the selection.
                self.selected.clear();
                self.reset_after_reseek();
                let where_ = if scope.len() > 1 {
                    "its take".to_string()
                } else {
                    lane.label().to_string()
                };
                self.notify_user(
                    format!(
                        "GAP CLOSED on {where_} — {} takes it back",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Closes every bounded gap on `lane`, as one gesture. The scope stays the
    /// clicked track's scope -- not the whole timeline -- and each linked take is
    /// widened only when its other half has the same empty stretch. Unsafe gaps
    /// are left open and counted in the notice, so the sweep never fails as a
    /// silent no-op.
    pub(crate) fn close_all_gaps_on_lane(&mut self, lane: Lane, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let report = match session.close_all_gaps_on_lane(lane) {
            Ok(report) => report,
            Err(e) => {
                self.notify_user(e.to_string().into());
                cx.notify();
                return;
            }
        };
        if report.closed > 0 {
            self.mark_dirty();
            self.selected.clear();
            self.reset_after_reseek();
        }
        let skipped = report.skipped.len();
        let notice = match (report.closed, skipped) {
            (0, 0) => format!("NO GAPS TO CLOSE on {}", lane.label()),
            (0, 1) => format!(
                "NO GAPS CLOSED on {} — 1 skipped at {}; match linked gaps or detach",
                lane.label(),
                report.skipped[0].start
            ),
            (0, n) => format!(
                "NO GAPS CLOSED on {} — {n} skipped; match linked gaps or detach",
                lane.label()
            ),
            (n, 0) => format!(
                "{n} GAPS CLOSED on {} — {} takes them back",
                lane.label(),
                self.keymap.display(ActionId::Undo)
            ),
            (n, 1) => format!(
                "{n} GAPS CLOSED on {}; 1 skipped at {} — match linked gaps or detach",
                lane.label(),
                report.skipped[0].start
            ),
            (n, skipped) => format!(
                "{n} GAPS CLOSED on {}; {skipped} skipped — match linked gaps or detach",
                lane.label()
            ),
        };
        self.notify_user(notice.into());
        cx.notify();
    }

    /// A press on an edge: the start of the drag that changes how much of a
    /// source it plays. It picks the placement as a press anywhere else on the
    /// box does -- the edge strip covers the box's own listener (`occlude`), so
    /// this is the only one that fires there -- and ctrl makes that pick the
    /// toggle it is everywhere else on the bed.
    pub(crate) fn start_trim(
        &mut self,
        lane: Lane,
        idx: usize,
        edge: Edge,
        ctrl: bool,
        cx: &mut Context<Self>,
    ) {
        if self.modal() || self.exporting().is_some() {
            return;
        }
        // A caption's edge, on a lane that holds no `Clip` at all: the same
        // gesture, the same `Trim`, and the branch is the lane's kind wherever
        // the drag is asked about ([`Player::trim_to`],
        // [`Player::commit_trim`]). A caption *is* markable -- the mark is its
        // lane and its index like a clip's ([`Player::pick`]) -- and a caption
        // in a group carries its link, so its trim drags the group with it.
        if lane.kind == LaneKind::Subtitle {
            let Some(sub) = self
                .session
                .as_ref()
                .and_then(|session| session.sub_lane(lane).get(idx).copied())
            else {
                return;
            };
            self.pick((lane, idx), ctrl, cx);
            self.trim = Some(Trim {
                lane,
                idx,
                edge,
                // Where the edge already is: a press that never moves is not an
                // edit, and the engine refuses exactly that.
                from: match edge {
                    Edge::Start => sub.start,
                    Edge::End => sub.end(),
                },
                to: match edge {
                    Edge::Start => sub.start,
                    Edge::End => sub.end(),
                },
                link: sub.link,
            });
            cx.notify();
            return;
        }
        let Some(clip) = self
            .session
            .as_ref()
            .and_then(|session| session.lane_clips(lane).get(idx).copied())
        else {
            return;
        };
        self.pick((lane, idx), ctrl, cx);
        self.trim = Some(Trim {
            lane,
            idx,
            edge,
            // Where the edge already is: a press that never moves is not an
            // edit, and `Project::trim` refuses exactly that.
            from: match edge {
                Edge::Start => clip.start,
                Edge::End => clip.end(),
            },
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
    pub(crate) fn trim_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(trim) = self.trim else {
            return;
        };
        // The edge is pulled onto the same marks a whole clip is, by itself:
        // there is no other end travelling with it, so it snaps at length zero.
        let marks = self.snap_targets(Some((trim.lane, trim.idx)));
        let (at, cue) = self.snap_to(self.frame_under(x), 0, &marks);
        let Some((lo, hi)) = self.session.as_ref().and_then(|session| {
            match trim.lane.kind == LaneKind::Subtitle {
                // The walls a caption's edge has -- its neighbour, its own
                // track's first microsecond and its last cue -- asked at this
                // timeline's rate, which is why the session is the door and not
                // the project.
                true => session.trim_sub_room(trim.lane, trim.idx, trim.edge),
                false => session.trim_room(trim.lane, trim.idx, trim.edge),
            }
        }) else {
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
    pub(crate) fn frame_under(&self, x: Pixels) -> u32 {
        frame_at(self.scale.time_at(px_along(x, self.ruler.get())), self.fps)
    }

    /// The release: the whole drag reaches the engine as one edit, so it is one
    /// undo step. The selection survives it -- a trim inserts and removes
    /// nothing, so every index a lane had still names the clip it named.
    pub(crate) fn commit_trim(&mut self, cx: &mut Context<Self>) {
        let Some(trim) = self.trim.take() else {
            return;
        };
        self.mark_dirty();
        // A caption's edge reaches the engine through its own door, at this
        // timeline's rate. An `Ok` is *not* "something changed" -- an edge that
        // stopped at a wall it already stood against is `Ok` with no undo step
        // ([`Project::trim_sub`]) -- so only the refusal is ever said out loud.
        // A lone caption's trim -- and one grouped only with other captions --
        // moves nothing that plays and reseeks nothing; a caption grouped with
        // clips trimmed them with it and owes the flag reset a clip's trim
        // pays. The grouped question is asked before the edit moves indices,
        // in the engine's own words, so the two answers can never disagree.
        if trim.lane.kind == LaneKind::Subtitle {
            let grouped = self
                .session
                .as_ref()
                .is_some_and(|session| session.caption_grouped_with_clips(trim.lane, trim.idx));
            let trimmed = self
                .session
                .as_mut()
                .map(|session| session.trim_sub(trim.lane, trim.idx, trim.edge, trim.to));
            match trimmed {
                Some(Err(e)) => self.notify_user(format!("NOT TRIMMED — {e}").into()),
                Some(Ok(())) if grouped => self.reset_after_reseek(),
                _ => {}
            }
            cx.notify();
            return;
        }
        let trimmed = self
            .session
            .as_mut()
            .is_some_and(|session| session.trim_clip(trim.lane, trim.idx, trim.edge, trim.to));
        if trimmed {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// A press on a fade handle: the start of the drag that lengthens or
    /// shortens the ramp at that end of an audio clip. Ctrl-free, unlike a
    /// trim's press -- a fade handle sits inside the trim strip's own column
    /// ([`FADE_HANDLE_W`]) and a ctrl-click there would be read as picking the
    /// clip up rather than as the toggle it means on a trim.
    pub(crate) fn start_fade_drag(
        &mut self,
        lane: Lane,
        idx: usize,
        is_in: bool,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
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
        self.pick((lane, idx), false, cx);
        let start = match is_in {
            true => clip.fade_in,
            false => clip.fade_out,
        };
        self.fade_drag = Some(FadeDrag {
            lane,
            idx,
            is_in,
            press_x: x,
            start,
            to: start,
            cap: clip.frames(),
        });
        cx.notify();
    }

    /// Where the pointer has pulled a fade handle to, in the fade's own
    /// frames -- pulling away from the clip's edge lengthens the ramp for
    /// both handles alike, which is why the tail's own delta is negated: its
    /// handle sits at the *right* of the box, so a hand dragging left (toward
    /// the body) is the same "make it longer" motion the head's handle reads
    /// dragging right.
    pub(crate) fn fade_drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(fade) = self.fade_drag else {
            return;
        };
        let dx = f32::from(x) - f32::from(fade.press_x);
        let dx = match fade.is_in {
            true => dx,
            false => -dx,
        };
        let delta = fade_delta_frames(dx, self.scale.pps, self.fps);
        let to = (i64::from(fade.start) + delta).clamp(0, i64::from(fade.cap)) as u32;
        self.fade_drag = Some(FadeDrag { to, ..fade });
        cx.notify();
    }

    /// The release: one edit, one undo step, exactly as [`Player::commit_trim`]
    /// pays for a trim. Autosave's own reason a fade drag has to call
    /// [`Player::mark_dirty`] same as every other edit that reaches the
    /// engine.
    pub(crate) fn commit_fade(&mut self, cx: &mut Context<Self>) {
        let Some(fade) = self.fade_drag.take() else {
            return;
        };
        self.mark_dirty();
        let set = self
            .session
            .as_mut()
            .is_some_and(|session| match fade.is_in {
                true => session.set_fade_in(fade.lane, fade.idx, fade.to),
                false => session.set_fade_out(fade.lane, fade.idx, fade.to),
            });
        if set {
            cx.notify();
        }
    }

    /// The clip's fade-in as the drag is showing it: display only, same as
    /// [`Player::trimmed`] -- the engine hears about it once, at the release.
    pub(crate) fn shown_fade_in(&self, lane: Lane, idx: usize, clip: &Clip) -> u32 {
        match self.fade_drag {
            Some(f) if f.is_in && (f.lane, f.idx) == (lane, idx) => f.to,
            _ => clip.fade_in,
        }
    }

    /// [`Player::shown_fade_in`]'s twin, for the tail.
    pub(crate) fn shown_fade_out(&self, lane: Lane, idx: usize, clip: &Clip) -> u32 {
        match self.fade_drag {
            Some(f) if !f.is_in && (f.lane, f.idx) == (lane, idx) => f.to,
            _ => clip.fade_out,
        }
    }

    /// Crossfades across the join the selection names: two picks on one audio
    /// lane take the pair, one pick takes it and its right-hand neighbour. The
    /// engine owns adjacency ([`Project::crossfade`]); a refusal is worded
    /// here, in [`Player::regroup`]'s voice, because `false` is all it says.
    pub(crate) fn crossfade_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some((lane, idx)) = (match self.selected.picks() {
            [a, b] if a.0 == b.0 && a.1.abs_diff(b.1) == 1 => Some((a.0, a.1.min(b.1))),
            _ => self.selected.anchor(),
        }) else {
            self.notify_user(
                "NOTHING TO CROSSFADE — select an audio clip that has a neighbour".into(),
            );
            cx.notify();
            return;
        };
        let frames = self.fps.round().max(1.) as u32;
        if let Some(session) = &mut self.session {
            if !session.crossfade(lane, idx, frames) {
                self.notify_user(
                    "NOTHING TO CROSSFADE — it takes two audio clips sitting end to end on one lane"
                        .into(),
                );
            }
        }
        cx.notify();
    }

    /// Dissolves the join the selection names, into or back out of it: two
    /// picks on one video lane take the pair, one pick takes it and its
    /// right-hand neighbour. If the leading clip already carries a dissolve
    /// this removes it (`0` frames) instead of widening it -- a toggle, same
    /// key either way. The engine owns adjacency ([`Project::set_transition_out`]);
    /// a refusal is worded here, in [`Player::crossfade_selected`]'s voice.
    pub(crate) fn dissolve_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some((lane, idx)) = (match self.selected.picks() {
            [a, b] if a.0 == b.0 && a.1.abs_diff(b.1) == 1 => Some((a.0, a.1.min(b.1))),
            _ => self.selected.anchor(),
        }) else {
            self.notify_user(
                "NOTHING TO DISSOLVE — select a video clip that has a neighbour".into(),
            );
            cx.notify();
            return;
        };
        let Some(session) = &mut self.session else {
            cx.notify();
            return;
        };
        let removing = session.transition_out_of(lane, idx) > 0;
        let frames = if removing {
            0
        } else {
            self.fps.round().max(1.) as u32
        };
        if !session.set_transition_out(lane, idx, frames) {
            self.notify_user(
                "NOTHING TO DISSOLVE — it takes two video clips sitting end to end on one lane"
                    .into(),
            );
        } else if removing {
            self.notify_user("DISSOLVE REMOVED — the clips cut again".into());
        }
        cx.notify();
    }

    /// Widens or narrows the transition the anchor already carries, one
    /// frame at a step, from the inspector's duration row -- ctrl+f/ctrl+x
    /// stay the toggle that seeds a fresh one at one second; this is what
    /// moves it afterwards. The engine clamps both kinds to what the two
    /// clips actually offer ([`Project::set_transition_out`],
    /// [`Project::crossfade`]), so a step past the successor's own length
    /// lands at the successor's length instead of asking for more than
    /// there is.
    pub(crate) fn nudge_transition(&mut self, lane: Lane, idx: usize, step: i32, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        let current = match lane.kind {
            LaneKind::Video => session.transition_out_of(lane, idx),
            LaneKind::Audio => session.lane_clips(lane).get(idx).map_or(0, |c| c.fade_out),
            _ => return,
        };
        let frames = (current as i32 + step).max(0) as u32;
        let ok = match lane.kind {
            LaneKind::Video => session.set_transition_out(lane, idx, frames),
            LaneKind::Audio => session.crossfade(lane, idx, frames),
            _ => false,
        };
        if ok {
            self.mark_dirty();
            cx.notify();
        }
    }

    /// The clip as the drag is showing it: an edge under the pointer moves its
    /// own box, and the boxes of everything linked to it, before anything is
    /// committed. Display only -- the project is not touched until the release.
    /// A member of the group follows by the same delta, clamped to its own
    /// room, which is the arithmetic the release commits -- so a box let go of
    /// is the box that lands.
    pub(crate) fn trimmed(&self, lane: Lane, idx: usize, clip: Clip) -> Clip {
        let Some(trim) = self.trim.filter(|t| {
            (t.lane, t.idx) == (lane, idx) || (t.link.is_some() && t.link == clip.link)
        }) else {
            return clip;
        };
        let to = self.followed(
            trim,
            (lane, idx),
            match trim.edge {
                Edge::Start => clip.start,
                Edge::End => clip.end(),
            },
        );
        let still = self.session.as_ref().is_some_and(|session| {
            session
                .sources()
                .get(clip.source)
                .is_some_and(|s| engine::is_image(&s.path))
        });
        trimmed_clip(clip, trim.edge, to, still)
    }

    /// The caption as the drag is showing it, [`Player::trimmed`]'s twin:
    /// display only, and the engine hears about it once, at the release. A
    /// caption in the dragged group follows by the same delta, against its own
    /// walls.
    pub(crate) fn trimmed_sub(&self, lane: Lane, idx: usize, sub: SubClip) -> SubClip {
        let Some(trim) = self
            .trim
            .filter(|t| (t.lane, t.idx) == (lane, idx) || (t.link.is_some() && t.link == sub.link))
        else {
            return sub;
        };
        let to = self.followed(
            trim,
            (lane, idx),
            match trim.edge {
                Edge::Start => sub.start,
                Edge::End => sub.end(),
            },
        );
        trimmed_sub(sub, trim.edge, to)
    }

    /// The frame a trim is showing for the placement at `here`, whose edge
    /// stands at `at`: the drag's own `to` when `here` is the placement the
    /// press started on, and that `to`'s delta from where the press started,
    /// clamped to this placement's own room, when it is a member following.
    fn followed(&self, trim: Trim, here: (Lane, usize), at: u32) -> u32 {
        if (trim.lane, trim.idx) == here {
            return trim.to;
        }
        let delta = i64::from(trim.to) - i64::from(trim.from);
        let room = match here.0.kind {
            LaneKind::Subtitle => self
                .session
                .as_ref()
                .and_then(|session| session.trim_sub_room(here.0, here.1, trim.edge)),
            _ => self
                .session
                .as_ref()
                .and_then(|session| session.trim_room(here.0, here.1, trim.edge)),
        };
        room.map_or(trim.to, |(lo, hi)| {
            (i64::from(at) + delta).clamp(i64::from(lo), i64::from(hi)) as u32
        })
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
    pub(crate) fn drawn_duration(&self) -> f64 {
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
    ///
    /// `self.transport()` answers for whichever session is active, which is
    /// the preview while one is showing ([`Player::active_session`]) -- so a
    /// preview reaching its own end must not be read as *this* (the
    /// timeline's) playhead running out and snapping to `duration`. Gated on
    /// there being no preview session, so the ruler keeps tracking
    /// `self.session` -- untouched by a preview's own clock -- for as long as
    /// one is up.
    pub(crate) fn playhead(&self, duration: f64) -> f64 {
        let ended = self.preview_session.is_none() && self.transport() == Transport::Ended;
        let now = self.session.as_ref().map_or(0., PlaybackSession::now);
        playhead_position(now, ended, duration)
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
    /// The one event a release outside the window's own surface is
    /// guaranteed to raise: Wayland (and X11) tell a client its pointer has
    /// left, unconditionally, whether or not a button is down
    /// (`wl_pointer::Event::Leave`, gpui's `platform/linux/wayland/client.rs`)
    /// -- but a `Button::Released` that lands *after* that Leave is never
    /// forwarded at all, because gpui only dispatches it to the window that
    /// is still `mouse_focused_window`, and Leave just cleared that. So the
    /// seam's own release handler (`drag_release`, bound to `on_mouse_up`)
    /// can be skipped entirely by a drag that ends outside the frame, and
    /// `drag_move`'s "next incidental motion" fallback above only fires once
    /// the pointer comes back -- which a restart before that motion, or a
    /// release the user never revisits, may never do. This is the one
    /// dependable earlier moment: the size a `Split::Dock`/`Split::Bench`
    /// drag is holding right now is exactly what a release beyond this
    /// instant would have kept, since no further sample can arrive from
    /// outside the surface, so it is saved here rather than left to chance.
    /// Wired at the root via [`Player::mount_mouse_exit_listener`]'s
    /// `window.on_mouse_event`, the low-level door `Interactivity`'s fluent
    /// `on_mouse_*` builders do not expose for `MouseExitEvent`.
    pub(crate) fn drag_left_window(&mut self, cx: &mut Context<Self>) {
        if split_drag_owes_save(self.split_drag) {
            save_stance_splits(&self.splits);
        }
        cx.notify();
    }

    pub(crate) fn drag_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The scrollbar's thumb first: the pointer leaves the 6 px strip on
        // the first move, so the gesture is followed here for the ruler's own
        // reason -- and it must outrank every other drag, because a hand
        // holding the view is not holding anything on it.
        if self.scroll_drag.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.scroll_drag_to(event.position.x, cx),
                // A release outside the window never reaches the handler
                // below, so the first button-up move is when we learn the
                // drag is over.
                _ => self.scroll_drag = None,
            }
            return;
        }
        // The lane stack's own thumb, on exactly the same terms: the pointer
        // leaves the strip on the first move, so the root carries the gesture
        // -- and a hand holding the stack is not holding anything on it
        // either.
        if self.lanes_drag.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.lanes_drag_to(event.position.y, cx),
                _ => self.lanes_drag = None,
            }
            return;
        }
        // A divider is answered before every gesture below it: it is pressed on
        // a strip of its own that nothing else is under, so neither can swallow
        // the other -- a seam over the timeline never scrubs, and a scrub is
        // never mistaken for a resize.
        if let Some(split) = self.split_drag {
            match event.pressed_button {
                Some(MouseButton::Left) => self.drag_split(split, event.position, window, cx),
                // Released outside the window: the up below never came, so this
                // is where the gesture ends. The size is already written to
                // `self.splits`, but a persisted seam still owes the save
                // `drag_release` would have paid -- without it the drag
                // holds for the session and reverts on restart.
                _ => {
                    self.split_drag = None;
                    if split_drag_owes_save(Some(split)) {
                        save_stance_splits(&self.splits);
                    }
                }
            }
            return;
        }
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
        // A fade handle is [`FADE_HANDLE_W`] wide and the pointer leaves it
        // on the first drag, same reason and same shape as a trim's own
        // branch just above.
        if self.fade_drag.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.fade_drag_to(event.position.x, cx),
                _ => self.commit_fade(cx),
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
        // The transform card's sliders, [`Self::color_dragging`]'s own reason.
        if self.transform_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_transform(event.position.x, false, cx);
            } else {
                self.transform_dragging = false;
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
        // The preview's own scrub bar, tracked on the root for `scrubbing`'s
        // reason: it is a short bar and the pointer leaves it at once.
        if self.preview_scrubbing {
            if event.pressed_button == Some(MouseButton::Left) {
                self.preview_scrub_to(event.position.x, false, cx);
            } else {
                self.preview_scrubbing = false;
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
    pub(crate) fn drag_release(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The panel is left exactly where the hand let go, for the reason every
        // other release here lands one last sample.
        if self.scroll_drag.take().is_some() {
            self.scroll_drag_to(event.position.x, cx);
            return;
        }
        if self.lanes_drag.take().is_some() {
            self.lanes_drag_to(event.position.y, cx);
            return;
        }
        if let Some(split) = self.split_drag.take() {
            self.drag_split(split, event.position, window, cx);
            // Every seam outlives the window: written once, on the release
            // that ends the gesture, the same small-file round trip
            // `ui::dock_stance`'s tab pick uses -- not on every sample in
            // between, which `drag_move` above never owed a write.
            if split_drag_owes_save(Some(split)) {
                save_stance_splits(&self.splits);
            }
            return;
        }
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
        if self.fade_drag.is_some() {
            self.fade_drag_to(event.position.x, cx);
            self.commit_fade(cx);
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
        if std::mem::take(&mut self.transform_dragging) {
            self.drag_transform(event.position.x, false, cx);
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
        if std::mem::take(&mut self.preview_scrubbing) {
            self.preview_scrub_to(event.position.x, true, cx);
            return;
        }
        if std::mem::take(&mut self.scrubbing) {
            self.scrub_to(event.position.x, true, cx);
        }
    }

    /// Where the hand has taken a divider, live: the panel it belongs to is set
    /// to what the pointer says and the next frame draws it there. Nothing else
    /// is written -- a layout is not an edit, so there is no undo step to take
    /// and no worker to flush. The clamps are the *reader's*
    /// ([`split_size`]), so a size set in one window is still a size in the
    /// next one it is drawn in.
    fn drag_split(
        &mut self,
        split: Split,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let raw = split_drag_size(split, at, viewport);
        // Clamped here, through the same door [`Player::split_px`] draws
        // through, at the one place a hand's pick ever reaches
        // `self.splits` -- so what a later save writes to disk is never a
        // number the seam itself would have refused to draw at. A floor
        // enforced only on read still leaves a `bench=-1` line in the file.
        let lanes = self
            .session
            .as_ref()
            .map_or(2, |session| session.lanes().len());
        let view = self.view();
        let scroll = view.duration > view.span();
        self.splits
            .set(split, split_size(split, Some(raw), lanes, viewport, scroll));
        cx.notify();
    }
}
