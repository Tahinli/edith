//! Editing the timeline itself: the cuts, the clipboard, the drags, the
//! snapping and the trims.

use crate::ui::kit::NumberEdit;
use crate::ui::widgets::CueEdit;
use crate::*;
use crate::ui::bench_stance::TrackSize;
use crate::ui::bench_stance::clip_envelope::{self, EnvHit};

/// One of a lane's three head switches (DESIGN §7): the eye hides a video or
/// subtitle lane, the note mutes an audio lane, the lock refuses edits to any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Switch {
    Hidden,
    Muted,
    Locked,
}

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

/// How far (px) the pointer must travel from the press before a seam follows it.
pub(crate) const SPLIT_CLICK_SLOP: f32 = 3.;

/// Whether the pointer has gone far enough from where a seam was pressed to call it a drag.
pub(crate) fn split_travelled(origin: Point<Pixels>, at: Point<Pixels>) -> bool {
    let d = at - origin;
    f32::from(d.x).hypot(f32::from(d.y)) >= SPLIT_CLICK_SLOP
}

impl Player {
    /// Whether the seam being held has been dragged, not merely clicked.
    fn split_moved(&self) -> bool {
        self.split_press.is_some_and(|(_, moved)| moved)
    }

    /// Splits the clip under the playhead. Metadata only: the timeline->source
    /// mapping is unchanged, so nothing reseeks and no flag is touched.
    pub(crate) fn cut(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(lane) = self.marked_locked_lane() {
            self.lane_locked_refusal(lane);
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
            let cut = session.cut_at(f64::from(at) / self.fps);
            if cut {
                // Only a split that happened drops the selection: a refusal is
                // "Nothing to split", and the clip in hand is still the clip the
                // next Delete, `x` or menu acts on -- clearing it there left the
                // user with nothing picked and told them nothing about it. A
                // real split is the other way round: the cut it leaves makes the
                // picked index name a different clip, so the mark has to go.
                // [`Player::regroup`] below has always cleared inside its own
                // success branch; this is the same rule on the other half of the
                // pair.
                self.selected.clear();
                self.notify_user("Split".into());
            } else {
                self.notify_user("Nothing to split — the playhead is already on a cut".into());
            }
        }
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
                self.notify_user("Nothing to rejoin — no cut under the playhead".into());
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
                        "Nothing detached — that clip is not grouped with another".into(),
                    );
                }
            }
            (Some(_), None) => self.notify_user("Nothing detached — nothing selected".into()),
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
            (_, None, _) => self.notify_user("Nothing grouped — nothing selected".into()),
            // The hand's group: every pick, one id.
            (Some(session), _, 2..) => {
                if let Err(e) = session.group_all(&picks) {
                    self.notify_user(format!("Not grouped — {e}").into());
                }
            }
            // One pick that no longer names anything.
            (Some(_), Some(_), 0) => {
                self.notify_user("Nothing grouped — that clip is no longer there".into())
            }
            // One pick: the partner flow it always was.
            (Some(session), Some((lane, idx)), _) => match span_partner(session, lane, idx) {
                Some((other, o_idx)) => {
                    if let Err(e) = session.group(lane, idx, other, o_idx) {
                        self.notify_user(format!("Not grouped — {e}").into());
                    }
                }
                None => {
                    self.notify_user("Nothing to group with — no clip covers these frames".into())
                }
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
        if let Some(lane) = self.marked_locked_lane() {
            self.lane_locked_refusal(lane);
            return;
        }
        // A title selected in the Clip tab (no clip picked) is what the key takes off.
        if self.title_target().is_some() {
            self.remove_title_selected(cx);
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
                        self.notify_user_action("Caption deleted".into(), ToastAction::Undo);
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
                true => session.delete_clip(lane, idx).then_some("Clip deleted"),
                false => session.lift_clip(lane, idx).then_some("Clip lifted"),
            },
            _ => None,
        };
        if selected.is_some() && deleted.is_none() {
            self.notify_user("Nothing deleted — that clip is no longer there".into());
        }
        if let Some(text) = deleted {
            self.reset_after_reseek();
            self.notify_user_action(text.into(), ToastAction::Undo);
        }
        cx.notify();
    }

    /// The placements linked to `target` -- one take with it -- on every lane
    /// they sit on, `target` itself left out. Empty for a clip in no group.
    pub(crate) fn linked_partners(&self, target: (Lane, usize)) -> Vec<(Lane, usize)> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let link = match target.0.kind {
            LaneKind::Subtitle => session.sub_lane(target.0).get(target.1).and_then(|s| s.link),
            LaneKind::Video | LaneKind::Audio => {
                session.lane_clips(target.0).get(target.1).and_then(|c| c.link)
            }
        };
        let Some(link) = link else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for lane in session.lanes() {
            if session.lane_locked(lane) {
                continue;
            }
            match lane.kind {
                LaneKind::Subtitle => out.extend(
                    session
                        .sub_lane(lane)
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.link == Some(link))
                        .map(|(i, _)| (lane, i)),
                ),
                LaneKind::Video | LaneKind::Audio => out.extend(
                    session
                        .lane_clips(lane)
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.link == Some(link))
                        .map(|(i, _)| (lane, i)),
                ),
            }
        }
        out.retain(|&p| p != target);
        out
    }

    /// A plain click on a clip selects its whole take: a click on either half
    /// picks both, with the one clicked last, so Delete and Lift act on the pair.
    /// `alt` picks the one half alone, as Premiere does.
    pub(crate) fn select_take(&mut self, target: (Lane, usize), alt: bool, cx: &mut Context<Self>) {
        self.select(target, cx);
        // A refused select (a locked lane) leaves the selection as it was, and so does Alt.
        if alt || self.selected.anchor() != Some(target) {
            return;
        }
        let mut picks = Selection::new();
        for partner in self.linked_partners(target) {
            picks.add(partner);
        }
        picks.add(target);
        self.selected = picks;
        cx.notify();
    }

    /// A press on a clip: a press on a clip already picked keeps the set (a set
    /// drag needs it), and any other plain press selects the take as a click does.
    /// Ctrl and Alt presses go through [`Player::pick`] as they always did.
    pub(crate) fn press_take(&mut self, target: (Lane, usize), ctrl: bool, alt: bool, cx: &mut Context<Self>) {
        if ctrl || alt || self.selected.contains(target) {
            self.pick(target, ctrl, cx);
            return;
        }
        self.select_take(target, false, cx);
    }

    /// Lifts the selected half out and leaves the hole: black picture there if
    /// it was the video lane, silence if it was the audio one, and nothing else
    /// moves. A half of a take lifts its partners too, in one undo step, so the
    /// take is never left with a hole on one lane only. What Delete is not.
    pub(crate) fn lift_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(lane) = self.marked_locked_lane() {
            self.lane_locked_refusal(lane);
            return;
        }
        let selected = self.selected.anchor();
        let lifts: Vec<(Lane, usize)> = match selected {
            Some(anchor) => std::iter::once(anchor).chain(self.linked_partners(anchor)).collect(),
            None => Vec::new(),
        };
        self.selected.clear();
        match (&mut self.session, selected) {
            (Some(session), Some(_)) => {
                session.begin_batch();
                let lifted = lifts
                    .iter()
                    .filter(|(lane, _)| lane.kind != LaneKind::Subtitle)
                    .fold(false, |any, &(lane, idx)| session.lift_clip(lane, idx) | any);
                session.end_batch();
                if lifted {
                    self.reset_after_reseek();
                    self.notify_user_action("Clip lifted".into(), ToastAction::Undo);
                } else {
                    self.notify_user("Nothing lifted — that half is no longer there".into());
                }
            }
            (Some(_), None) => self.notify_user("Nothing lifted — nothing selected".into()),
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
            .filter_map(|&(lane, idx)| Some((lane, session.lane_clips(lane).get(idx).cloned()?)))
            .collect();
        if !picks.is_empty() {
            self.clipboard = picks;
            self.copied_look = self.capture_look();
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
        // Paste lands on the lanes the copied items came from, so one of them
        // being locked refuses the whole paste.
        let locked = self
            .clipboard
            .iter()
            .map(|&(lane, _)| lane)
            .find(|&lane| self.session.as_ref().is_some_and(|s| s.lane_locked(lane)));
        if let Some(lane) = locked {
            self.lane_locked_refusal(lane);
            return;
        }
        let pasted = match (&mut self.session, self.clipboard.as_slice()) {
            (Some(session), [(_, clip)]) => session.paste_at(session.now(), clip.clone()),
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
                "Not pasted — another clip already covers where one of the copied clips would \
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
    pub(crate) fn move_clip(&mut self, from: Lane, idx: usize, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        self.named_step("Move clip", |this| this.move_clip_now(from, idx, to, x, cx));
    }

    fn move_clip_now(&mut self, from: Lane, idx: usize, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        // The drop ends the gesture whatever it does: the original is no
        // longer carried, and the line promised a landing that is now made.
        self.drag_src = None;
        self.snap_cue = None;
        // A shift-drag is a slip or a slide and its release lands in
        // `drag_release`: the gpui drag the body also starts must not move it.
        if self.exporting().is_some() || self.pro_drag.is_some() {
            return;
        }
        // Lifted off a locked lane or dropped onto one: both are refused.
        if self.lane_locked_refusal(from) || self.lane_locked_refusal(to) {
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
        // A clip let go on another lane lands where the hand asked or not at all. The
        // engine would clamp the travel into the gap instead, and that clamp moves the
        // clip's linked partners in time along with it: a drop onto a lane beside
        // another clip must not shift anything else.
        if to != from {
            let len = self
                .session
                .as_ref()
                .and_then(|session| session.lane_clips(from).get(idx).map(|c| c.frames()))
                .unwrap_or(0);
            let neighbours = self.lane_neighbours(from, idx, to);
            if !Self::cross_lane_fits(start, len, &neighbours) {
                self.drag_lane = None;
                self.notify_user(
                    format!("Not moved — another clip already covers those frames on {}", to.label())
                        .into(),
                );
                cx.notify();
                return;
            }
        }
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
                // The arm rides the door's own answer: `move_clip_to` /
                // `move_selection_to` answer `false` for a clip let go where it
                // was picked up (`Project::move_selection` refuses before its
                // snapshot) and for a lane of the wrong kind, and the arms below
                // say both in words. Armed before the call it dirtied a project
                // nobody had touched -- the ledger's `unsaved` ghost and the
                // save-on-close question over an unchanged file.
                self.mark_dirty();
                // Every pick named a `(lane, ...)` by index into that lane's
                // clips, sorted by start -- exactly what an insert or a lane
                // change reorders. Re-read by the frame each pick's clip now
                // starts at (recorded before the move moved anything), so the
                // selection survives naming the clips it named, not the slots
                // they used to sit in.
                if set_move {
                    self.selected =
                        self.remap_selection(&picks, &pre_starts, from, idx, to, (was, start));
                } else {
                    self.selected.clear();
                }
                self.reset_after_reseek();
                // The strip names the edit that just happened: a move that
                // lands silently leaves the last word (`Split`) standing, and
                // an editor reads that as the drag having done nothing.
                self.notify_user("Moved".into());
            }
            // The three ways a drag is refused, told apart by what the
            // front-end already knows: a lane's kind, and where the clip was.
            // Everything else that could refuse (a clip that is not there)
            // cannot be dragged.
            false if from.kind != to.kind => self.notify_user(
                format!("Not on {} — a {kind} clip takes a {lanes} lane", to.label()).into(),
            ),
            // Picked up and put back down where it was: a click, and a click
            // says nothing.
            false if from == to && start == was => {}
            false if set_move => self.notify_user(
                format!(
                    "Not moved — {} clips selected, and another clip already covers where one of \
                     them would land on {}",
                    picks.len(),
                    to.label()
                )
                .into(),
            ),
            false => self.notify_user(
                format!(
                    "Not moved — another clip already covers those frames on {}",
                    to.label()
                )
                .into(),
            ),
        }
        // The gesture is over: the lane it ended over is nobody's answer any
        // more (`Player::drag_lane`, read by `bench-content`'s catch-all).
        self.drag_lane = None;
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
        (was, landed): (u32, u32),
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
        let end_us = cues.iter().map(|c| c.end_us).max()?;
        // A palette row arrives in no group: pinning its words to a clip is
        // a hand's decision, made with the Group row afterwards.
        Some(SubClip::whole_track(track, end_us, self.fps))
    }

    /// A palette row let go at window `x` over subtitle lane `to`: the whole
    /// track goes down where the hand left it, one undo step
    /// ([`PlaybackSession::place_sub`]). Every refusal there is words -- an
    /// overlap, a lane that is not a subtitle lane, a track that is not there --
    /// and they are shown as the engine worded them.
    pub(crate) fn place_sub(&mut self, track: usize, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() || self.lane_locked_refusal(to) {
            return;
        }
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
                Err(e) => Some(format!("Not placed — {e}")),
            },
            // The row is greyed and says why in the list; here it says why at
            // the moment somebody tried to use it anyway.
            (None, Some(_)) => Some("Not placed — that subtitle track has no cues".to_string()),
            (_, None) => Some("Not placed — no file open".to_string()),
        };
        // The arm rides the door's own answer: `text` is `None` exactly when
        // `Project::place_sub` took the track, and every refusal -- an overlap,
        // a lane that is not a subtitle lane, "no file open", "no cues" --
        // leaves the project byte-for-byte what it was. Armed ahead of the
        // answer it wrote an autosave sidecar, the ledger's `unsaved` ghost and
        // the save-on-close question over a project nobody had changed.
        if text.is_none() {
            self.mark_dirty();
        }
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
            &drag.sub,
        )
    }

    /// A placed caption let go at window `x` over subtitle lane `to`: it lands
    /// with its head where the hand carried it, on the lane it was dropped on --
    /// one undo step, and none at all for a drop that changed nothing, which is
    /// why an `Ok` is never a toast ([`Project::move_sub`]). An overlap and a
    /// lane of the wrong kind are refused in the engine's own words.
    pub(crate) fn move_sub(&mut self, drag: &SubDrag, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        // The drop ends the gesture whatever it does, as `move_clip`'s does: the
        // caption stops being dimmed where it stands.
        self.drag_src = None;
        if self.exporting().is_some()
            || self.lane_locked_refusal(drag.lane)
            || self.lane_locked_refusal(to)
        {
            return;
        }
        self.snap_cue = None;
        self.ghost.clear();
        let Some(idx) = self.dragged_sub(drag) else {
            return;
        };
        // Where the caption stands *now*, so the arm below can tell a move from
        // a drag let go where it began: `Project::move_sub` answers `Ok` for a
        // pick-up-put-back too ("`Ok`, and no undo step, because nothing
        // changed"), so its own answer is not a change report.
        let was = self
            .session
            .as_ref()
            .and_then(|session| session.sub_lane(drag.lane).get(idx).map(|sub| sub.start));
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
                self.notify_user(format!("Not moved — {e}").into());
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
                // The arm rides the caption's own place having moved -- the
                // engine's `Ok` covers the put-back that takes no undo step at
                // all (`Project::move_sub`), and a project nobody has touched
                // must not light the `unsaved` ghost.
                let landed = mark.and_then(|(lane, i)| {
                    self.session
                        .as_ref()?
                        .sub_lane(lane)
                        .get(i)
                        .map(|sub| (lane, sub.start))
                });
                if landed != was.map(|start| (drag.lane, start)) {
                    self.mark_dirty();
                }
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
        if self.exporting().is_some() || self.lane_locked_refusal(lane) {
            return;
        }
        let lifted = self
            .session
            .as_mut()
            .is_some_and(|session| session.lift_sub(lane, idx));
        let text = match lifted {
            true => format!(
                "Caption lifted · undo {}",
                self.keymap.display(ActionId::Undo)
            ),
            false => "Nothing lifted — that caption is not there any more".to_string(),
        };
        if lifted {
            // The arm rides the door's own answer: `false` is "Nothing lifted —
            // that caption is not there any more", and the project is exactly
            // what it was, so the flag, the autosave sidecar and the
            // save-on-close question must stay off it.
            self.mark_dirty();
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
        // A lifted caption's toast offers Undo (DESIGN §8); a refusal has
        // nothing to take back.
        match lifted {
            true => self.notify_user_action(text.into(), ToastAction::Undo),
            false => self.notify_user(text.into()),
        }
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
                "{} is shown — {} is off the picture; every subtitle track is still exported",
                lane.label(),
                old.label()
            ),
            None => format!(
                "{} is shown — its captions are over the picture",
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

    /// Whether one of `lane`'s three head switches is on (DESIGN §7).
    pub(crate) fn lane_switch(&self, lane: Lane, which: Switch) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        match which {
            Switch::Hidden => session.lane_hidden(lane),
            Switch::Muted => session.lane_muted(lane),
            Switch::Locked => session.lane_locked(lane),
        }
    }

    /// Whether the source patch sends the source monitor's picture (video lane) or sound (audio
    /// lane) to `lane` on Insert and Overwrite. A subtitle lane is never patched.
    pub(crate) fn source_patch_on(&self, lane: Lane) -> bool {
        match lane.kind {
            LaneKind::Video => self.patch_video == lane,
            LaneKind::Audio => self.patch_audio == lane,
            LaneKind::Subtitle => false,
        }
    }

    /// Whether the source patch boxes show on the lane heads: only while the source monitor has
    /// an item loaded.
    pub(crate) fn source_patch_shown(&self) -> bool {
        self.preview_session.is_some()
    }

    /// Points the source patch at `lane`: its kind's half of an Insert or Overwrite goes there
    /// from now on. Nothing is edited, so nothing is undone.
    pub(crate) fn set_source_patch(&mut self, lane: Lane, cx: &mut Context<Self>) {
        match lane.kind {
            LaneKind::Video => self.patch_video = lane,
            LaneKind::Audio => self.patch_audio = lane,
            LaneKind::Subtitle => return,
        }
        cx.notify();
    }

    /// Flips one of a lane's head switches to `on`. One undo step, from the
    /// engine. Deliberately not refused on a locked lane: locking is what
    /// turns the lock off again, and hiding or muting a lane edits nothing.
    pub(crate) fn set_lane_switch(
        &mut self,
        lane: Lane,
        which: Switch,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let changed = match self.session.as_mut() {
            Some(session) => match which {
                Switch::Hidden => session.set_lane_hidden(lane, on),
                Switch::Muted => session.set_lane_muted(lane, on),
                Switch::Locked => session.set_lane_locked(lane, on),
            },
            None => false,
        };
        if changed {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The lock's door, for every entry point that would edit a lane's items
    /// (DESIGN §7). `true`, and a toast offering Unlock, when `lane` is locked:
    /// the caller returns before it touches anything.
    pub(crate) fn lane_locked_refusal(&mut self, lane: Lane) -> bool {
        let locked = self.session.as_ref().is_some_and(|s| s.lane_locked(lane));
        if locked {
            self.notify_user_action(
                format!("{} is locked", lane.label()).into(),
                ToastAction::Unlock(lane),
            );
        }
        locked
    }

    /// The first locked lane among the marked items, for the edits that act on
    /// the whole selection (delete, lift, paste's lanes).
    pub(crate) fn marked_locked_lane(&self) -> Option<Lane> {
        let session = self.session.as_ref()?;
        self.marks()
            .0
            .iter()
            .map(|&(lane, _)| lane)
            .find(|&lane| session.lane_locked(lane))
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
            self.snap_live(),
            self.drop_snap_frames(),
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
        let (start, cue) = self.sub_drop_frame(drag.sub, x);
        let ghost = Ghost {
            lane: to,
            start,
            frames: drag.sub.frames,
            tint: CLIP_TEXT(),
            refused: to.kind != LaneKind::Subtitle,
        };
        self.set_cue(cue, x, cx);
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
        let (start, cue) = self.place_frame(x);
        let ghost = Ghost {
            lane: to,
            start,
            frames: self.sub_of_track(track).map_or(0, |sub| sub.frames),
            tint: CLIP_TEXT(),
            refused: to.kind != LaneKind::Subtitle,
        };
        self.set_cue(cue, x, cx);
        self.set_ghost(vec![ghost], cx);
    }

    /// Where a clip let go at window `x` over lane `to` wants its head: the
    /// frame under the pointer, less however far into the box the hand grabbed
    /// it (so the clip does not jump under the pointer), pulled onto a
    /// neighbouring edge when it lands within [`Player::drop_snap_frames`] of one. `None` when
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
        let clip = self.session.as_ref()?.lane_clips(from).get(idx).cloned()?;
        let marks = self.snap_targets(Some((from, idx)));
        Some(landing(
            self.frame_under(x),
            self.grab,
            clip.frames(),
            self.snap_live(),
            self.drop_snap_frames(),
            &marks,
        ))
    }

    /// The spans on `to` a clip dragged from `from`/`idx` has to fit between: every
    /// clip on `to` but the dragged one and its link group.
    pub(crate) fn lane_neighbours(&self, from: Lane, idx: usize, to: Lane) -> Vec<(u32, u32)> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let dragged = session.lane_clips(from).get(idx).cloned();
        session
            .lane_clips(to)
            .iter()
            .enumerate()
            .filter(|&(i, clip)| {
                (to, i) != (from, idx)
                    && !(clip.link.is_some()
                        && dragged.as_ref().is_some_and(|d| clip.link == d.link))
            })
            .map(|(_, clip)| (clip.start, clip.end()))
            .collect()
    }

    /// Whether a clip `len` frames long, let go on another lane with its head at `at`,
    /// lands exactly there: [`cross_room`] answers `at` itself, not a clamped spot.
    pub(crate) fn cross_lane_fits(at: u32, len: u32, neighbours: &[(u32, u32)]) -> bool {
        cross_room(at, len, neighbours) == Some(at)
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
            self.snap_live(),
            self.drop_snap_frames(),
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
        live_idx(session.lane_clips(drag.lane), drag.idx, &drag.clip)
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
            self.set_cue(None, x, cx);
            self.set_ghost(Vec::new(), cx);
            return;
        };
        let Some((asked, cue)) = self.drop_frame(drag.lane, idx, x) else {
            self.set_cue(None, x, cx);
            self.set_ghost(Vec::new(), cx);
            return;
        };
        // What the release would refuse: a lane of the other kind, and a head
        // let go on top of a take already there ([`Project::move_selection`]
        // changes nothing rather than overwrite one). Drawn as refused so a
        // drop that will not happen is seen before the hand lets go, never as a
        // silent no-op. The neighbours are the destination lane's clips less
        // whatever travels with this one -- itself and its link group.
        let neighbours = self.lane_neighbours(drag.lane, idx, to);
        // Staying on its own lane, the engine clamps the travel into the gap
        // the clip sits in rather than refusing it, so the shadow rests where
        // the release will: flush against the take it was dragged into. Landing
        // on *another* lane there is no clamp: a head that does not fit exactly
        // where the hand asked is refused, because a clamp would shift the
        // clip's linked partners in time and a vertical drag must move nothing
        // in time ([`Player::cross_lane_fits`]).
        let len = drag.clip.frames();
        let (start, refused) = match to == drag.lane {
            true => (gap_clamp(drag.clip.start, len, asked, &neighbours), false),
            false => (asked, !Self::cross_lane_fits(asked, len, &neighbours)),
        };
        // The line only stands where the head really lands on the mark: a
        // neighbour's clamp or a refusal moved it off the frame the line names.
        self.set_cue(cue.filter(|_| start == asked && !refused), x, cx);
        let anchor = Ghost {
            lane: to,
            start,
            frames: len,
            tint: self.clip_tint(drag.clip.source),
            refused: drag.lane.kind != to.kind || refused,
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
                    .and_then(|session| session.lane_clips(lane).get(i).cloned())
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
        let (start, cue) = self.place_frame(x);
        let ghost = Ghost {
            lane: to,
            start,
            frames: self
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(path)),
            // A path with no source entry has no colour of its own, and the
            // shadow wears the lane's own instead of borrowing another file's.
            tint: file_tint(self.sources(), path).unwrap_or(BG_RAISED()),
            refused: lane_refuses(path, to, self.has_video(path)).is_some(),
        };
        self.set_cue(cue, x, cx);
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
        // The comment markers' edges and the chapter starts: a drag lands on a note
        // or a chapter as readily as on a cut.
        let mut notes: Vec<u32> = session
            .markers()
            .iter()
            .flat_map(|m| [m.frame, m.frame.saturating_add(m.length)])
            .collect();
        notes.extend(session.chapters().iter().map(|c| c.frame));
        snap_marks(
            &clips,
            skip,
            skip_link,
            frame_at(session.now(), self.fps),
            self.range,
            &notes,
        )
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
                    .filter(|_| clip.speed.is_normal() && clip.speed_keys.is_empty())
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
                true => "No sync points — this source has no keyframe grid to jump by (or it is \
                         still being read)"
                    .into(),
                false => "No sync point that way — the playhead is past the last one".into(),
            }),
        }
    }

    /// Whether the magnet is on for the gesture happening *now*: the switch
    /// ([`Player::snap`]) unless `alt` is held ([`Player::drag_alt`]), which
    /// turns it off for this drag or trim alone -- the temporary override
    /// Resolve, Premiere and Final Cut all put on the same key. The switch is
    /// not touched, so the stroke that follows the drag finds it where it was.
    /// Every landing on this timeline asks it, so the shadow, the line and the
    /// drop can never disagree about whether a magnet was on.
    pub(crate) fn snap_live(&self) -> bool {
        snap_live(self.snap, self.drag_alt)
    }

    /// [`SNAP_PX`] in timeline frames at the scale the bed is drawn at: the
    /// bed's own width drops out of it, since a pixel is now worth the same
    /// stretch of timeline wherever the view sits. The edge trims' magnet --
    /// a drop asks [`Player::drop_snap_frames`].
    pub(crate) fn snap_frames(&self) -> u32 {
        self.scale.snap_frames(self.fps)
    }

    /// [`Player::snap_frames`]'s twin for a drag's own release
    /// ([`DROP_SNAP_PX`]): every `place_frame`/`drop_frame`/`sub_drop_frame`
    /// asks this instead, so a dropped clip is aimed with the wider magnet
    /// while an edge trim keeps the finer one.
    pub(crate) fn drop_snap_frames(&self) -> u32 {
        self.scale.drop_snap_frames(self.fps)
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
        if !self.menu_allowed() {
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
            adjust: false,
            compound: false,
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
        if !self.menu_allowed() {
            return;
        }
        self.context_menu = Some(ContextMenu {
            lane,
            on: MenuOn::Gap(start, frames),
            at,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    }

    /// Opens the bench's own menu: what a right-click on the ruler, or on the
    /// empty stretch under the last lane, names -- the timeline rather than
    /// anything on it ([`BENCH_ITEMS`]). No lane and no selection: the lane
    /// field is `V1` because the struct has one, and no row reads it.
    ///
    /// Refused with no timeline open, like every menu here: a card that paints
    /// nothing while `overlaid()` swallows the keys is an invisible modal
    /// (`tests/cards.rs`'s FAULT 2 rule).
    /// The ruler's menu, opened on the frame under the pointer ([`Self::menu_frame`]).
    pub(crate) fn open_bench_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.session.is_none() {
            return;
        }
        self.menu_frame = Some(self.frame_under(at.x));
        self.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::Bench,
            at,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    }

    /// Opens the viewer's menu ([`VIEWER_ITEMS`]) at the pointer. Refused with
    /// no timeline open, like every menu here.
    /// An empty stretch of `lane`'s bed with no gap to close: the paste and track adds
    /// ([`MenuOn::Empty`]), opened on the frame under the pointer.
    pub(crate) fn open_empty_menu(&mut self, lane: Lane, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.session.is_none() {
            return;
        }
        self.menu_frame = Some(self.frame_under(at.x));
        self.context_menu = Some(ContextMenu {
            lane,
            on: MenuOn::Empty,
            at,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    }

    /// "Move playhead here": seeks to the frame the ruler or empty-track menu was opened on.
    pub(crate) fn playhead_at_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(frame) = self.menu_frame {
            self.seek(f64::from(frame) / self.fps, cx);
        }
    }

    pub(crate) fn open_viewer_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.session.is_none() {
            return;
        }
        self.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::Viewer,
            at,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    }

    /// Opens a sequence tab's menu ([`SEQ_TAB_ITEMS`]) at the pointer. The tab is opened first,
    /// so the verbs act on the sequence that was right-clicked.
    pub(crate) fn open_seq_tab_menu(&mut self, id: u32, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.session.is_none() {
            return;
        }
        self.switch_sequence_tab(id, cx);
        // Hung from the tab's foot like the menu bar's popups, so the menu never covers the
        // tab strip it was opened from.
        let at = crate::ui::anchor::control_at(at).map_or(at, |tab| crate::ui::anchor::hang(tab).0);
        self.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::SeqTab,
            at,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    }

    /// Opens a lane head's menu on the track that was right-clicked: the tracks
    /// a project can gain, and the one remove that applies to this lane's kind
    /// ([`oracle::lane_items`]). Nothing is selected -- a head owns no clip --
    /// which is [`Player::open_gap_menu`]'s rule on the other empty target.
    pub(crate) fn open_head_menu(&mut self, lane: Lane, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.session.is_none() {
            return;
        }
        self.context_menu = Some(ContextMenu {
            lane,
            on: MenuOn::Head,
            at,
            details: false,
            adjust: false,
            compound: false,
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
                        "Gap closed on {where_} · undo {}",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// The gap that is picked and still there, if one is. A gap owns no clip
    /// index, so nothing else keeps it honest: an edit or an undo can move or
    /// remove the hole under it, and a clip mark means the pointer is on a clip
    /// instead. Both answer `None` here, which is what keeps Delete, the
    /// ledger's readout and the bed's outline from acting on a stale hole.
    pub(crate) fn gap_pick(&self) -> Option<GapPick> {
        if self.selected.anchor().is_some() {
            return None;
        }
        live_gap(self.session.as_ref()?, self.gap?)
    }

    /// Picks the gap `(start, frames)` on `lane`: the clip marks go, since the
    /// two are one selection and not two (a gap and a clip are never both picked).
    pub(crate) fn select_gap(
        &mut self,
        lane: Lane,
        start: u32,
        frames: u32,
        cx: &mut Context<Self>,
    ) {
        self.selected.clear();
        self.drop_title_selection(cx);
        self.drop_sel_cue();
        self.gap = Some(GapPick {
            lane,
            start,
            frames,
        });
        cx.notify();
    }

    /// Drops the gap pick, if any. A click on a clip or on bare bench does this.
    pub(crate) fn clear_gap(&mut self, cx: &mut Context<Self>) {
        if self.gap.take().is_some() {
            cx.notify();
        }
    }

    /// Delete with a gap picked: closes it through [`Player::close_gap`], so
    /// the ripple and its one undo step are the ones a right-click gives. The
    /// pick is dropped only when the hole is gone, so a refused close (a take
    /// that does not match on its other lane) keeps the gap under the mark.
    pub(crate) fn close_selected_gap(&mut self, cx: &mut Context<Self>) {
        let Some(pick) = self.gap_pick() else {
            self.gap = None;
            return;
        };
        if self.exporting().is_some() {
            return;
        }
        self.close_gap(pick.lane, pick.start, pick.frames, cx);
        if self.gap_pick().is_none() {
            self.gap = None;
        }
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
            (0, 0) => format!("No gaps to close on {}", lane.label()),
            (0, 1) => format!(
                "No gaps closed on {} — 1 linked gap skipped at {}",
                lane.label(),
                report.skipped[0].start
            ),
            (0, n) => format!(
                "No gaps closed on {} — {n} linked gaps skipped",
                lane.label()
            ),
            (n, 0) => format!(
                "{n} gaps closed on {} — {} takes them back",
                lane.label(),
                self.keymap.display(ActionId::Undo)
            ),
            (n, 1) => format!(
                "{n} gaps closed on {}; 1 skipped at {} — match linked gaps or detach",
                lane.label(),
                report.skipped[0].start
            ),
            (n, skipped) => format!(
                "{n} gaps closed on {}; {skipped} skipped — match linked gaps or detach",
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
        if self.covers_room() || self.exporting().is_some() || self.lane_locked_refusal(lane) {
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
            .and_then(|session| session.lane_clips(lane).get(idx).cloned())
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
        // The same 10 px of aim a drop is given ([`DROP_SNAP_PX`]): one
        // threshold for every gesture on this bed, so an edge and a whole clip
        // grip a cut at the same distance from it.
        let (at, cue) = snap_cue(
            self.snap_live(),
            self.frame_under(x),
            0,
            self.drop_snap_frames(),
            &marks,
        );
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
        // A clip's edge that the pointer pulled past the source's own end
        // flashes its handle warn -- the one stop a hand cannot see on the
        // picture. Captions have no media wall ([`PlaybackSession::trim_sub_room`]
        // is all they are asked), so they never flash.
        let walls = match trim.lane.kind == LaneKind::Subtitle {
            true => None,
            false => self
                .session
                .as_ref()
                .and_then(|session| session.trim_walls(trim.lane, trim.idx, trim.edge)),
        };
        match walls.and_then(|walls| media_wall_stop(at, to, walls)) {
            // Armed once per stop: a pointer that stays past the same wall is
            // one flash, not one every sample, and the timer below is the only
            // repaint the flash needs.
            Some(wall) => {
                let pinned = self.trim_flash.is_some_and(|flash| {
                    flash.lane == trim.lane
                        && flash.idx == trim.idx
                        && flash.edge == trim.edge
                        && flash.wall == wall
                });
                if !pinned {
                    self.trim_flash = Some(TrimFlash {
                        lane: trim.lane,
                        idx: trim.idx,
                        edge: trim.edge,
                        wall,
                        since: Instant::now(),
                    });
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(TRIM_FLASH).await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                }
            }
            // Back inside the room, or held by something other than the media:
            // the next stop at a wall is a new one.
            None => self.trim_flash = None,
        }
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
        self.named_step("Trim", |this| this.commit_trim_now(cx));
    }

    fn commit_trim_now(&mut self, cx: &mut Context<Self>) {
        let Some(trim) = self.trim.take() else {
            return;
        };
        // The strip the drag started on may be gone or moved: its hover is over with the drag.
        self.hover_hint = None;
        // The arm rides each door's own answer or the caption's own edges --
        // never the call: a trim released where the edge already stood is
        // `Ok`/`true` with no undo step, and it must not leave the project
        // looking edited to autosave, the `unsaved` ghost or the close
        // question.
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
            // The caption's own window, before and after: the only reading that
            // can tell a trim from an edge released against the wall it already
            // stood on.
            let edges = |player: &Self| {
                player.session.as_ref().and_then(|session| {
                    session
                        .sub_lane(trim.lane)
                        .get(trim.idx)
                        .map(|sub| (sub.start, sub.frames, sub.in_us, sub.out_us))
                })
            };
            let before = edges(self);
            let trimmed = self
                .session
                .as_mut()
                .map(|session| session.trim_sub(trim.lane, trim.idx, trim.edge, trim.to));
            match trimmed {
                Some(Err(e)) => self.notify_user(format!("Not trimmed — {e}").into()),
                Some(Ok(())) => {
                    if edges(self) != before {
                        self.mark_dirty();
                    }
                    if grouped {
                        self.reset_after_reseek();
                    }
                }
                None => {}
            }
            cx.notify();
            return;
        }
        let trimmed = self
            .session
            .as_mut()
            .is_some_and(|session| session.trim_clip(trim.lane, trim.idx, trim.edge, trim.to));
        if trimmed {
            // The arm rides the door's own answer: `trim_clip` is `false` for an
            // edge already where it was asked to go, which is no undo step and
            // no edit.
            self.mark_dirty();
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
        if self.covers_room() || self.exporting().is_some() || self.lane_locked_refusal(lane) {
            return;
        }
        let Some(clip) = self
            .session
            .as_ref()
            .and_then(|session| session.lane_clips(lane).get(idx).cloned())
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
        // The ramp the clip is carrying *now*, so the arm below can tell a drag
        // from a press-and-release that left the handle where it was: `set_fade_in`
        // / `set_fade_out` answer `true` for any index that exists and take an
        // undo step either way, so their own answer is not a change report.
        let was = match fade.is_in {
            true => self
                .session
                .as_ref()
                .map(|session| session.fade_in_of(fade.lane, fade.idx)),
            false => self
                .session
                .as_ref()
                .map(|session| session.fade_out_of(fade.lane, fade.idx)),
        };
        let set = self
            .session
            .as_mut()
            .is_some_and(|session| match fade.is_in {
                true => session.set_fade_in(fade.lane, fade.idx, fade.to),
                false => session.set_fade_out(fade.lane, fade.idx, fade.to),
            });
        if set {
            let now = match fade.is_in {
                true => self
                    .session
                    .as_ref()
                    .map(|session| session.fade_in_of(fade.lane, fade.idx)),
                false => self
                    .session
                    .as_ref()
                    .map(|session| session.fade_out_of(fade.lane, fade.idx)),
            };
            if now != was {
                self.mark_dirty();
            }
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
            self.notify_user("Nothing to crossfade — no audio clip selected".into());
            cx.notify();
            return;
        };
        let frames = self.fps.round().max(1.) as u32;
        if self.session.is_none() {
            return;
        }
        // The arm rides the write, and rides the join's own edges as well as
        // the door's answer: `Project::crossfade` answers true for any two
        // adjacent clips, so a crossfade already at the room the two clips
        // offer writes the value the join already carried and still says yes.
        // This door carries its own arm rather than leaning on the action
        // table's list, which fires before any door is asked.
        let before = self.transition_edges(lane, idx);
        let took = match &mut self.session {
            Some(session) => session.crossfade(lane, idx, frames),
            None => false,
        };
        if took && self.transition_edges(lane, idx) != before {
            self.mark_dirty();
        } else if !took {
            self.notify_user("Nothing to crossfade — no neighbour end to end".into());
        }
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
            self.notify_user("Nothing to dissolve — no video clip selected".into());
            cx.notify();
            return;
        };
        if self.session.is_none() {
            cx.notify();
            return;
        }
        let removing = self.transition_out_of_selected(lane, idx);
        let frames = if removing {
            0
        } else {
            self.fps.round().max(1.) as u32
        };
        // The arm rides the write, and the join's own edges as well as the
        // door's answer -- `set_transition_out`'s own rule
        // ([`Player::crossfade_selected`]'s). This door carries its own arm
        // rather than leaning on the action table's list, which fires before
        // any door is asked: the clip-menu row
        // ([`crate::ui::bench_stance`]) reaches this door directly, never
        // through the table at all, and was armed by nothing.
        let before = self.transition_edges(lane, idx);
        let took = match &mut self.session {
            Some(session) => session.set_transition_out(lane, idx, frames),
            None => false,
        };
        if took && self.transition_edges(lane, idx) != before {
            self.mark_dirty();
            if removing {
                self.notify_user("Dissolve removed — the clips cut again".into());
            } else {
                self.notify_user_action("Dissolve added".into(), ToastAction::Undo);
            }
        } else if !took {
            self.notify_user("Nothing to dissolve — no neighbour end to end".into());
        }
        cx.notify();
    }

    /// Sets how the join the selection names is drawn
    /// ([`engine::transitions::TransitionKind`]), making it a second long if the
    /// clip has none: the picker's and the palette's door. Two picks on one
    /// video lane take the pair, one pick takes it and its right-hand neighbour,
    /// as [`Player::dissolve_selected`] does. One undo step; a join that already
    /// wears the kind writes nothing.
    pub(crate) fn set_transition_kind(
        &mut self,
        kind: engine::transitions::TransitionKind,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let Some((lane, idx)) = (match self.selected.picks() {
            [a, b] if a.0 == b.0 && a.1.abs_diff(b.1) == 1 => Some((a.0, a.1.min(b.1))),
            _ => self.selected.anchor(),
        }) else {
            self.notify_user("Nothing to transition — no video clip selected".into());
            cx.notify();
            return;
        };
        let Some(session) = &self.session else {
            cx.notify();
            return;
        };
        let current = session.transition_out_of(lane, idx);
        let frames = if current > 0 {
            current
        } else {
            self.fps.round().max(1.) as u32
        };
        let before = self.transition_edges(lane, idx);
        let took = match &mut self.session {
            Some(session) => session.set_transition(lane, idx, kind, frames),
            None => false,
        };
        if took && self.transition_edges(lane, idx) != before {
            self.mark_dirty();
        } else if !took {
            self.notify_user("Nothing to transition — no neighbour end to end".into());
        }
        cx.notify();
    }

    /// The dissolve the join at `idx` of `lane` already carries, read before
    /// the session is borrowed mutably: `0` for a lane, index or join that has
    /// none, and `false` if there is no session to ask.
    fn transition_out_of_selected(&self, lane: Lane, idx: usize) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.transition_out_of(lane, idx) > 0)
    }

    /// The transition's absolute length, through the setter the kind and
    /// length controls share (`set_transition_out`/`crossfade`, each already
    /// clamping to the shorter neighbour's frames), so the typed length
    /// ([`Player::commit_transition`]) and the picker's "None" land a number
    /// the same way, and undo and persistence never tell the doors apart.
    pub(crate) fn set_transition_frames(
        &mut self,
        lane: Lane,
        idx: usize,
        frames: u32,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        if self.session.is_none() {
            return;
        }
        // The engine's bool answers "the two clips are adjacent" -- both
        // setters snapshot and clamp, and a `frames` already in force (a step
        // past the cap, a number retyped in the field) writes the value the
        // clip already carried and still says true. The arm therefore rides
        // the join's own edges read back, the same before/after comparison
        // `move_sub` and `commit_fade` make.
        let before = self.transition_edges(lane, idx);
        let ok = match &mut self.session {
            Some(session) => match lane.kind {
                LaneKind::Video => session.set_transition_out(lane, idx, frames),
                LaneKind::Audio => session.crossfade(lane, idx, frames),
                _ => false,
            },
            None => false,
        };
        if ok && self.transition_edges(lane, idx) != before {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// What the join at `idx` of `lane` carries on both sides, for the doors
    /// that only the *pair* can answer for: a crossfade writes `idx`'s
    /// `fade_out` and its neighbour's `fade_in`, so a comparison of the
    /// anchor's own field alone would miss a neighbour-side no-op. `None`
    /// for a lane with no such pair, which is a value no write can equal.
    fn transition_edges(&self, lane: Lane, idx: usize) -> Option<(u32, u32)> {
        let session = self.session.as_ref()?;
        match lane.kind {
            LaneKind::Video => Some((
                session.transition_out_of(lane, idx),
                session.transition_kind_of(lane, idx).ordinal() + 1,
            )),
            LaneKind::Audio => {
                let clips = session.lane_clips(lane);
                Some((clips.get(idx)?.fade_out, clips.get(idx + 1)?.fade_in))
            }
            _ => None,
        }
    }

    /// Opens the transition length's field on the frame count the block is
    /// carrying, in seconds -- the row's own click, `Player::edit_mbps`'s
    /// convention. The engine clamps a typed length to what the two clips
    /// offer ([`Player::set_transition_frames`]), so the field needs no cap.
    pub(crate) fn edit_transition(&mut self, frames: u32) {
        let fps = self.fps.max(1.);
        self.viz_hex_edit = None;
        self.transition_edit = Some(NumberEdit::begin(&format!("{:.2}", f64::from(frames) / fps)));
    }

    /// One key while the transition length is typed: digits, `.` and `-`
    /// are typed, Enter writes the length, Escape drops it, and backspace
    /// erases. Any other key is ignored: the field keeps the keyboard, so no
    /// shortcut runs from a stroke typed into it. `false` only when no length
    /// is being typed.
    pub(crate) fn transition_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        use crate::player::text_field::FieldKey;
        let Some(mut edit) = self.transition_edit.take() else {
            return false;
        };
        let typed = key.parse::<u32>().is_ok() || matches!(key, "." | "," | "-");
        // A stroke that is not a number or an editing key is ignored, not passed on: the field
        // holds the keyboard until it is left.
        if !typed && !matches!(key, "enter" | "escape" | "backspace") {
            self.transition_edit = Some(edit);
            return true;
        }
        match edit.key(key, false, false, typed.then_some(key)) {
            FieldKey::Edited => self.transition_edit = Some(edit),
            FieldKey::Cancel => {}
            FieldKey::Commit => {
                self.transition_edit = Some(edit);
                self.commit_transition(cx);
            }
        }
        cx.notify();
        true
    }

    /// The typed length's enter: the seconds become frames at the project's
    /// rate and go through [`Player::set_transition_frames`], which clamps
    /// into the neighbour's room. A buffer that is not a number writes nothing.
    pub(crate) fn commit_transition(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.transition_edit.take() else {
            return;
        };
        let fps = self.fps.max(1.);
        if let (Some(secs), Some((lane, idx))) = (edit.commit(0., f64::from(u32::MAX) / fps), self.selected.anchor()) {
            self.set_transition_frames(lane, idx, (secs * fps).round() as u32, cx);
        }
        cx.notify();
    }

    /// Opens the in-place editor on one cue block: a double-click on it
    /// (DESIGN §7). The buffer starts on the words the cue has now, with the
    /// caret at the end. Refused on a locked lane, with the lock's own toast,
    /// and not offered for a picture cue, which has no words to type over. One
    /// inline editor at a time, so the colour and transition fields give way.
    pub(crate) fn open_cue_edit(
        &mut self,
        lane: Lane,
        track: usize,
        cue: usize,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() || self.lane_locked_refusal(lane) {
            return;
        }
        let words = self
            .session
            .as_ref()
            .and_then(|s| s.subtitles().get(track))
            .and_then(|t| t.cues.get(cue))
            .filter(|c| c.image.is_none())
            .map(|c| c.text.clone());
        let Some(words) = words else {
            return;
        };
        self.viz_hex_edit = None;
        self.transition_edit = None;
        self.cue_edit = Some(CueEdit::new(lane, track, cue, &words));
        cx.notify();
    }

    /// The in-place editor's enter. Writes the words only when they differ from
    /// the cue's ([`CueEdit::commit`]), as one undo step through
    /// [`PlaybackSession::set_cue_text`]; an edit that changed nothing leaves the
    /// project as it was. The editor is closed either way, and a lane that was
    /// locked while the block was open writes nothing.
    pub(crate) fn commit_cue_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.cue_edit.take() else {
            return;
        };
        if self.lane_locked_refusal(edit.lane) {
            cx.notify();
            return;
        }
        let current = self
            .session
            .as_ref()
            .and_then(|s| s.subtitles().get(edit.track))
            .and_then(|t| t.cues.get(edit.cue))
            .map(|c| c.text.clone());
        if let Some(words) = current.and_then(|current| edit.commit(&current))
            && let Some(session) = self.session.as_mut()
            && session.set_cue_text(edit.track, edit.cue, &words)
        {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// A press anywhere outside the in-place editor's plate ends the edit the
    /// way Enter does, so a click away is never a silent discard of the words.
    /// The plate stops its own presses before they reach the root.
    pub(crate) fn cue_edit_press_outside(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cue_edit.is_some() {
            self.commit_cue_edit(cx);
        }
        // A panel number typed and left is written the way Enter writes it.
        if let Some((which, edit)) = self.cue_num.take() {
            if let Some(secs) = edit.commit(0., 36_000.) {
                self.set_cue_num(which, (secs * 1e6).round() as i64, cx);
            }
            cx.notify();
        }
        // The title's words are written the same way; the text field stops its own presses.
        self.commit_title_draft(cx);
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
        let now = self.session.as_ref().map_or(0., PlaybackSession::now);
        // A take runs the clock past the last frame, and the timecode goes on with it.
        if self.record_running() {
            return now;
        }
        let ended = self.preview_session.is_none() && self.transport() == Transport::Ended;
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
        if split_drag_owes_save(self.split_drag) && self.split_moved() {
            save_stance_splits(&self.splits);
        }
        // A number being scrubbed (pan, title fields) is committed here too: its
        // release outside the window never arrives, and it would otherwise be
        // dropped on the next move back in.
        self.pan_scrub_end(cx);
        self.title_scrub_end(cx);
        self.cue_num_scrub_end(cx);
        cx.notify();
    }

    /// Sets a lane's height from the pointer while its edge is held: the
    /// pointer's y less the grab `interact::lane_resize_band` took at the press,
    /// held to the lane range. Live, so the lane follows the hand; the file is
    /// written on release.
    pub(crate) fn drag_lane_resize(
        &mut self,
        lane: Lane,
        grab: f32,
        y: Pixels,
        cx: &mut Context<Self>,
    ) {
        let h = crate::ui::bench_stance::clamp_lane_h(f32::from(y) - grab);
        let h = crate::ui::bench_stance::snap_lane_h(lane.kind, h);
        crate::ui::bench_stance::save_lane_h(&mut self.lane_h, lane, h);
        cx.notify();
    }

    /// A double-click on a lane's edge: the lane goes back to its kind's
    /// default, and the file forgets the height it was dragged to.
    pub(crate) fn reset_lane_h(&mut self, lane: Lane, cx: &mut Context<Self>) {
        self.lane_resize = None;
        crate::ui::bench_stance::forget_lane_h(&mut self.lane_h, lane);
        self.save_view_state();
        cx.notify();
    }

    /// Sets one lane to a height size from its header's menu, and keeps it
    /// in the project's view state like a drag does.
    pub(crate) fn set_lane_size(&mut self, lane: Lane, size: TrackSize, cx: &mut Context<Self>) {
        self.lane_resize = None;
        crate::ui::bench_stance::set_track_size(&mut self.lane_h, lane, size);
        self.save_view_state();
        cx.notify();
    }

    /// Sets every lane to one size: the Timeline menu's "All tracks" rows.
    pub(crate) fn set_all_track_sizes(&mut self, size: TrackSize, cx: &mut Context<Self>) {
        let lanes = self.session.as_ref().map_or_else(Vec::new, PlaybackSession::lanes);
        for lane in lanes {
            crate::ui::bench_stance::set_track_size(&mut self.lane_h, lane, size);
        }
        self.save_view_state();
        cx.notify();
    }

    /// One size taller or shorter for every lane, from each lane's own size:
    /// the keys that make the tracks larger or smaller.
    pub(crate) fn step_track_sizes(&mut self, up: bool, cx: &mut Context<Self>) {
        let lanes = self.session.as_ref().map_or_else(Vec::new, PlaybackSession::lanes);
        let sizes: Vec<(Lane, TrackSize)> = lanes
            .into_iter()
            .map(|lane| (lane, crate::ui::bench_stance::track_size(self, lane).step(up)))
            .collect();
        for (lane, size) in sizes {
            crate::ui::bench_stance::set_track_size(&mut self.lane_h, lane, size);
        }
        self.save_view_state();
        cx.notify();
    }

    pub(crate) fn drag_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pan_scrub.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.pan_scrub_move(f32::from(event.position.x), cx),
                _ => {
                    self.pan_scrub_end(cx);
                }
            }
            return;
        }
        // An envelope drag follows the pointer; a button-up move ends it with its commit.
        if self.gain_edit.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.gain_move(event.position, cx),
                _ => self.gain_release(cx),
            }
            return;
        }
        // A title number or block being turned follows the pointer; a button-up move ends it.
        if self.title_scrub.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.title_scrub_move(f32::from(event.position.x), cx),
                _ => {
                    self.title_scrub_end(cx);
                }
            }
            return;
        }
        // A caption panel number dragged sideways follows the pointer the same way.
        if self.cue_num_scrub.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.cue_num_scrub_move(f32::from(event.position.x), event.modifiers.shift, cx),
                _ => {
                    self.cue_num_scrub_end(cx);
                }
            }
            return;
        }
        // A box select follows the pointer wherever it goes; a button-up move
        // (released off the window) ends it without selecting.
        if let Some(m) = self.marquee.as_mut() {
            match event.pressed_button {
                Some(MouseButton::Left) => {
                    m.to = event.position;
                    cx.notify();
                }
                _ => {
                    self.marquee = None;
                    cx.notify();
                }
            }
            return;
        }
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
        // A lane's bottom edge is held the same way: its band is the only thing
        // under the pointer, and a release off the window ends it here, with the
        // save `drag_release` would have paid.
        if let Some((lane, grab)) = self.lane_resize {
            match event.pressed_button {
                Some(MouseButton::Left) => {
                    self.drag_lane_resize(lane, grab, event.position.y, cx);
                }
                _ => {
                    self.lane_resize = None;
                    self.save_view_state();
                }
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
                    if split_drag_owes_save(Some(split)) && self.split_moved() {
                        save_stance_splits(&self.splits);
                    }
                    self.split_press = None;
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
        if self.pro_drag.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.pro_to(event.position.x, cx),
                _ => self.commit_pro(cx),
            }
            return;
        }
        // A caption held by its edge or body is tracked here too, for the same reason.
        if self.cue_drag.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.cue_drag_to(event.position.x, cx),
                _ => self.commit_cue_drag(cx),
            }
            return;
        }
        if self.trim.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => {
                    self.drag_alt = event.modifiers.alt;
                    self.trim_to(event.position.x, cx);
                }
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
        // A colour wheel's puck or master bar: the same live writes.
        if let Some((wheel, master)) = self.color_wheel_drag {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_wheel(wheel, master, event.position, false, cx);
            } else {
                self.color_wheel_drag = None;
                self.flush_drag(cx);
            }
            return;
        }
        // The ink wheel, on the root for the same 4 px reason and gated on the
        // worker like the bars above: one sample here is a whole picture
        // rebuilt, so what is written is one ink per frame the worker delivers
        // rather than one per pixel the pointer crossed.
        if self.viz_hs_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_viz_hs(event.position, false, cx);
            } else {
                // The release happened outside the window, so the sample the
                // hand let go on is owed like any other ending.
                self.viz_hs_dragging = false;
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
        // The accent sliders in Settings: live while held, kept on release.
        if let Some(slider) = self.accent_drag {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_accent(slider, event.position.x, cx);
            } else {
                self.end_accent_drag();
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
        // The effects bars hold a draft and write it once, on release.
        if self.effects_drag.is_some() {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_effect(event.position.x, cx);
            } else {
                self.release_effect(cx);
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

    /// Takes away what a clip drag shows while it is in the air: the dimmed
    /// source, the snap line and the shadow. Escape calls it, because a drag
    /// cancelled without a release sends no release to clear them. Repaints
    /// only when something was up.
    pub(crate) fn end_drag_preview(&mut self, cx: &mut Context<Self>) {
        let showing = self.drag_src.take().is_some()
            | self.snap_cue.take().is_some()
            | !self.ghost.is_empty();
        self.ghost.clear();
        if showing {
            cx.notify();
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
        // Any release ends a clip move in flight, including one let go off
        // every lane (no drop fires there): the original stops being dimmed
        // and the line stops promising a landing. A trim keeps its line until
        // its own commit below.
        self.drag_src = None;
        if self.gain_edit.is_some() {
            self.gain_release(cx);
            return;
        }
        if self.pan_scrub_end(cx) || self.title_scrub_end(cx) || self.cue_num_scrub_end(cx) {
            return;
        }
        if self.trim.is_none() && self.snap_cue.take().is_some() {
            cx.notify();
        }
        if let Some(mut m) = self.marquee.take() {
            m.to = event.position;
            self.finish_marquee(m, event.modifiers.control, cx);
            return;
        }
        if self.scroll_drag.take().is_some() {
            self.scroll_drag_to(event.position.x, cx);
            return;
        }
        if self.lanes_drag.take().is_some() {
            self.lanes_drag_to(event.position.y, cx);
            return;
        }
        if let Some((lane, grab)) = self.lane_resize.take() {
            // The height lands where the hand let go, and the view is written
            // once here -- the same round trip a seam's release makes.
            self.drag_lane_resize(lane, grab, event.position.y, cx);
            self.save_view_state();
            return;
        }
        if let Some(split) = self.split_drag.take() {
            self.drag_split(split, event.position, window, cx);
            let moved = self.split_moved();
            self.split_press = None;
            // Every seam outlives the window: written once, on the release
            // that ends the gesture, the same small-file round trip
            // `ui::dock_stance`'s tab pick uses -- not on every sample in
            // between, which `drag_move` above never owed a write.
            if split_drag_owes_save(Some(split)) && moved {
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
        if self.pro_drag.is_some() {
            self.pro_to(event.position.x, cx);
            self.commit_pro(cx);
            return;
        }
        if self.cue_drag.is_some() {
            self.cue_drag_to(event.position.x, cx);
            self.commit_cue_drag(cx);
            return;
        }
        if self.trim.is_some() {
            // The release lands exactly, then the gesture is
            // written once -- one edit, one undo step.
            self.drag_alt = event.modifiers.alt;
            self.trim_to(event.position.x, cx);
            self.trim_flash = None;
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
        if let Some((wheel, master)) = self.color_wheel_drag.take() {
            self.drag_wheel(wheel, master, event.position, false, cx);
            self.flush_drag(cx);
            return;
        }
        if std::mem::take(&mut self.viz_hs_dragging) {
            // The release lands exactly where the hand let go, and the flush
            // is what makes that true while the worker is still busy: the
            // sample above would only be held.
            self.drag_viz_hs(event.position, false, cx);
            self.flush_drag(cx);
            return;
        }
        if std::mem::take(&mut self.transform_dragging) {
            self.drag_transform(event.position.x, false, cx);
            self.flush_drag(cx);
            return;
        }
        if let Some(slider) = self.accent_drag {
            self.drag_accent(slider, event.position.x, cx);
            self.end_accent_drag();
            return;
        }
        if std::mem::take(&mut self.speed_dragging) {
            self.drag_speed(event.position.x, false, cx);
            self.flush_drag(cx);
            return;
        }
        if self.effects_drag.is_some() {
            self.drag_effect(event.position.x, cx);
            self.release_effect(cx);
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
        // A press that has not travelled is a click, not a drag: the grab zone
        // is wider than the seam, so taking the pointer's y as the seam's
        // would move it by the offset of the click.
        if let Some((origin, moved)) = &mut self.split_press
            && !*moved
        {
            if !split_travelled(*origin, at) {
                return;
            }
            *moved = true;
        }
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

/// `pick`, if the session still has that exact hole on its lane: the same
/// `(start, frames)` answered by [`PlaybackSession::gap_at`] at its first frame.
/// A hole that moved, shrank or closed answers `None`, which is how a stale
/// pick is dropped. Free, and not a method, so a test can ask it of a session.
pub(crate) fn live_gap(session: &PlaybackSession, pick: GapPick) -> Option<GapPick> {
    (session.gap_at(pick.lane, pick.start) == Some((pick.start, pick.frames))).then_some(pick)
}

/// The volume envelope's gestures on an audio clip: a press adds or grabs a key, a drag moves
/// it (or a segment of the line), a release commits the whole drag as one undo step, and Delete
/// or a double-click takes a key out. The geometry is [`clip_envelope`]'s own, so what is drawn
/// is what is hit.
impl Player {
    /// The clip's envelope geometry at the timeline's current zoom: its keys, its clip gain, its
    /// source range and speed, and the pixels per timeline frame. `None` for a video clip or a
    /// clip that is not there.
    fn gain_geometry(&self, lane: Lane, idx: usize) -> Option<(Vec<engine::GainKey>, f32, u32, u32, f64, f32)> {
        let session = self.session.as_ref()?;
        if lane.kind != LaneKind::Audio {
            return None;
        }
        let clip = session.lane_clips(lane).get(idx)?;
        let ppf = (self.scale.pps / self.fps) as f32;
        Some((clip.gain_keys.clone(), clip.gain_db, clip.in_frame, clip.out_frame, clip.speed.as_f64(), ppf))
    }

    /// The envelope's keys as the drag shows them, for the clip that is being dragged.
    pub(crate) fn shown_gain_keys(&self, lane: Lane, idx: usize) -> Option<Vec<engine::GainKey>> {
        self.gain_edit
            .as_ref()
            .filter(|e| (e.lane, e.idx) == (lane, idx))
            .map(|e| e.shown.clone())
    }

    /// Whether the pointer is over a key or the line of this clip's envelope: what makes its
    /// layer take the press and show its cursor.
    pub(crate) fn gain_hot(&self, lane: Lane, idx: usize) -> bool {
        matches!(self.gain_hover, Some((l, i, _)) if (l, i) == (lane, idx))
    }

    /// The envelope's hit under the clip-local point `local` in a box of `size`, or `None`.
    fn gain_hit_at(&self, lane: Lane, idx: usize, local: Point<Pixels>, size: Size<Pixels>) -> Option<EnvHit> {
        let (keys, gain, in_frame, _, speed, ppf) = self.gain_geometry(lane, idx)?;
        clip_envelope::hit(
            &keys,
            gain,
            in_frame,
            speed,
            ppf,
            (f32::from(size.width), f32::from(size.height)),
            (f32::from(local.x), f32::from(local.y)),
        )
    }

    /// The pointer moved over a clip's envelope layer: the hover that sets its cursor.
    pub(crate) fn gain_hover_at(&mut self, lane: Lane, idx: usize, local: Point<Pixels>, size: Size<Pixels>, cx: &mut Context<Self>) {
        let hit = self.gain_hit_at(lane, idx, local, size).filter(|h| matches!(h, EnvHit::Key(_)) || matches!(h, EnvHit::Line(_)));
        let next = hit.map(|h| (lane, idx, h));
        if self.gain_hover != next {
            self.gain_hover = next;
            cx.notify();
        }
    }

    /// The pointer left a clip's envelope layer.
    pub(crate) fn gain_hover_end(&mut self, lane: Lane, idx: usize, cx: &mut Context<Self>) {
        if self.gain_hot(lane, idx) {
            self.gain_hover = None;
            cx.notify();
        }
    }

    /// A press on a clip's envelope layer. Returns whether it was taken here (so the clip body
    /// under it does not also select). Ctrl on the line adds a key where the line is; a press on a
    /// key grabs it (two clicks take it out); a press on the line grabs its segment.
    pub(crate) fn gain_press_at(
        &mut self,
        lane: Lane,
        idx: usize,
        event: &MouseDownEvent,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let (window, origin, size) = (event.position, bounds.origin, bounds.size);
        let (ctrl, clicks) = (event.modifiers.control, event.click_count);
        if self.covers_room() || self.exporting().is_some() || self.lane_locked_refusal(lane) {
            return false;
        }
        let local = point(window.x - origin.x, window.y - origin.y);
        let Some(hit) = self.gain_hit_at(lane, idx, local, size) else {
            return false;
        };
        let Some((keys, gain, in_frame, out_frame, speed, ppf)) = self.gain_geometry(lane, idx) else {
            return false;
        };
        let (lo, hi) = (engine::project::MIN_CLIP_GAIN_DB, engine::project::MAX_CLIP_GAIN_DB);
        match (hit, ctrl) {
            (EnvHit::Line(_), true) => {
                // Where the line is, so the key adds no change of sound.
                let src = f64::from(in_frame) + f64::from(local.x) / f64::from(ppf) * speed;
                let at = (src.round().max(0.) as u32).clamp(in_frame, out_frame);
                let db = engine::project::gain_keys_db_at(&keys, f64::from(at)).clamp(lo, hi);
                if self.session.as_mut().is_some_and(|s| s.add_gain_key(lane, idx, engine::GainKey::new(at, db))) {
                    self.mark_dirty();
                }
                self.gain_pick = Some((lane, idx, at));
                self.pick((lane, idx), false, cx);
                cx.notify();
                true
            }
            (EnvHit::Key(i), _) => {
                let at = keys[i].at;
                if clicks >= 2 {
                    if self.session.as_mut().is_some_and(|s| s.remove_gain_key(lane, idx, at)) {
                        self.mark_dirty();
                    }
                    self.gain_pick = None;
                    cx.notify();
                    return true;
                }
                self.gain_pick = Some((lane, idx, at));
                self.pick((lane, idx), false, cx);
                let grab_db = (gain + keys[i].db) - clip_envelope::db_of_y(f32::from(local.y), f32::from(size.height));
                self.gain_edit = Some(GainEdit {
                    lane,
                    idx,
                    grab: GainGrab::Key(i),
                    press: window,
                    origin,
                    size,
                    shown: keys.clone(),
                    keys,
                    clip_gain: gain,
                    in_frame,
                    out_frame,
                    speed,
                    px_per_frame: ppf,
                    grab_db,
                });
                cx.notify();
                true
            }
            (EnvHit::Line(Some(i)), false) => {
                let grab_db = clip_envelope::db_of_y(f32::from(local.y), f32::from(size.height));
                self.pick((lane, idx), false, cx);
                self.gain_edit = Some(GainEdit {
                    lane,
                    idx,
                    grab: GainGrab::Segment(i),
                    press: window,
                    origin,
                    size,
                    shown: keys.clone(),
                    keys,
                    clip_gain: gain,
                    in_frame,
                    out_frame,
                    speed,
                    px_per_frame: ppf,
                    grab_db,
                });
                cx.notify();
                true
            }
            // The flat line past the keys is the clip's own, not the envelope's: the press
            // falls through to the body.
            (EnvHit::Line(_), false) => false,
        }
    }

    /// The held envelope drag follows the pointer: a key moves in time and dB within its
    /// neighbours and the clip's source range, a segment moves both keys by one dB amount.
    pub(crate) fn gain_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(edit) = self.gain_edit.as_mut() else {
            return;
        };
        let local_y = f32::from(position.y - edit.origin.y);
        let height = f32::from(edit.size.height);
        let (lo_db, hi_db) = (engine::project::MIN_CLIP_GAIN_DB, engine::project::MAX_CLIP_GAIN_DB);
        let dx = f32::from(position.x - edit.press.x);
        let mut shown = edit.keys.clone();
        match edit.grab {
            GainGrab::Key(i) => {
                let k = edit.keys[i];
                let prev = i.checked_sub(1).map(|j| edit.keys[j].at + 1).unwrap_or(edit.in_frame);
                let next = edit.keys.get(i + 1).map(|n| n.at.saturating_sub(1)).unwrap_or(edit.out_frame);
                let (lo, hi) = (prev.max(edit.in_frame), next.min(edit.out_frame));
                let src = f64::from(k.at) + f64::from(dx) / f64::from(edit.px_per_frame) * edit.speed;
                let at = if lo <= hi { (src.round().max(0.) as u32).clamp(lo, hi) } else { k.at };
                let disp = clip_envelope::db_of_y(local_y, height) + edit.grab_db;
                let db = (disp - edit.clip_gain).clamp(lo_db, hi_db);
                shown[i] = engine::GainKey { at, db, ..k };
            }
            GainGrab::Segment(i) => {
                let delta = clip_envelope::db_of_y(local_y, height) - edit.grab_db;
                let pair = [edit.keys[i], edit.keys[i + 1]];
                let most_up = pair.iter().map(|k| hi_db - k.db).fold(f32::INFINITY, f32::min);
                let most_down = pair.iter().map(|k| lo_db - k.db).fold(f32::NEG_INFINITY, f32::max);
                let delta = if most_down <= most_up { delta.clamp(most_down, most_up) } else { 0. };
                for j in [i, i + 1] {
                    shown[j] = engine::GainKey { db: edit.keys[j].db + delta, ..edit.keys[j] };
                }
            }
        }
        edit.shown = shown;
        cx.notify();
    }

    /// The release of an envelope drag: one edit, one undo step, and nothing at all when the
    /// hand came back to where it was.
    pub(crate) fn gain_release(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.gain_edit.take() else {
            return;
        };
        let moved = edit.shown.iter().zip(&edit.keys).any(|(a, b)| a.at != b.at || a.db != b.db);
        if moved && self.session.as_mut().is_some_and(|s| s.set_gain_keys(edit.lane, edit.idx, edit.shown)) {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// Delete with a key picked takes that key out, one undo step. `false` when no key is picked.
    pub(crate) fn gain_delete_pick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((lane, idx, at)) = self.gain_pick.take() else {
            return false;
        };
        if self.session.as_mut().is_some_and(|s| s.remove_gain_key(lane, idx, at)) {
            self.mark_dirty();
        }
        cx.notify();
        true
    }
}
