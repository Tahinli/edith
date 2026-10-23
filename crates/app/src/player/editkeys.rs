//! The timeline-editing verbs every NLE has and edith lacked: close the gap
//! under the playhead, freeze a frame, paste attributes, match frame, replace
//! edit, select forward, split edits and the nudge. Each is one [`ActionId`], so
//! the menu row, the stroke and the palette are the same door, and each is one
//! undo step (a verb that touches several clips wraps them in a batch).

use crate::*;

pub(crate) use engine::editkeys::{matched_source, nudge, paste_look, look_of, replace_plan, split_edit_in, CopiedLook, Part, PasteReport};

/// The clips that have not ended by `frame` on each of `lanes`: what select
/// track forward marks. A locked lane is left out, as Select All leaves it.
pub(crate) fn forward_picks(session: &PlaybackSession, frame: u32, lanes: &[Lane]) -> Selection {
    let mut picked = Selection::new();
    for &l in lanes.iter().filter(|l| l.kind != LaneKind::Subtitle) {
        if session.lane_locked(l) {
            continue;
        }
        for (i, c) in session.lane_clips(l).iter().enumerate() {
            if c.end() > frame {
                picked.add((l, i));
            }
        }
    }
    picked
}

/// The selection a box makes: every clip or caption crossing frames `f1..=f2`
/// on the lanes whose bed spans overlap `rows` (top, bottom). `beds` is each
/// lane's `(top, bottom)`. `base` is kept when the box adds to the selection.
pub(crate) fn marquee_picks(
    session: &PlaybackSession,
    beds: &[(Lane, f32, f32)],
    rows: (f32, f32),
    frames: (u32, u32),
    base: Selection,
) -> Selection {
    let mut picked = base;
    for &(lane, top, bottom) in beds {
        if bottom <= rows.0 || top >= rows.1 || session.lane_locked(lane) {
            continue;
        }
        match lane.kind {
            LaneKind::Subtitle => {
                for (i, s) in session.sub_lane(lane).iter().enumerate() {
                    if s.start <= frames.1 && s.end() > frames.0 {
                        picked.add((lane, i));
                    }
                }
            }
            _ => {
                for (i, c) in session.lane_clips(lane).iter().enumerate() {
                    if c.start <= frames.1 && c.end() > frames.0 {
                        picked.add((lane, i));
                    }
                }
            }
        }
    }
    picked
}

/// The first gap `frame` stands in, lanes in the order they are drawn.
pub(crate) fn gap_under(session: &PlaybackSession, frame: u32) -> Option<(Lane, u32, u32)> {
    session
        .lanes()
        .into_iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .find_map(|l| crate::timeline_math::between_clips(session.gap_at(l, frame)).map(|(start, frames)| (l, start, frames)))
}


/// How long a freeze frame holds, in seconds.
pub(crate) const FREEZE_SECS: f64 = 2.;
/// How far a freeze may move the playhead to the nearest frame a slowed clip can be cut at.
const FREEZE_REACH: u32 = 12;

/// The file a still is kept in: `title.png`, or `title 2.png`, `title 3.png`... where a name
/// holds other pixels. A name already holding these bytes is reused, so a repeat freeze of
/// the same frame adds no file.
fn still_path(dir: &Path, title: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut n = 1;
    loop {
        let path = dir.join(still_name(title, n));
        match std::fs::read(&path) {
            Ok(old) if old == bytes => return Ok(path),
            Ok(_) => n += 1,
            Err(_) => {
                std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                return Ok(path);
            }
        }
    }
}

/// The name the `n`th still with `title` is kept under: the first is the title alone.
pub(crate) fn still_name(title: &str, n: u32) -> String {
    match n {
        1 => format!("{title}.png"),
        n => format!("{title} {n}.png"),
    }
}

impl Player {
    fn playhead_frame(&self) -> u32 {
        self.session.as_ref().map_or(0, |s| frame_at(s.now(), self.fps))
    }

    /// The gap Close gap acts on: the picked one, else the first gap the
    /// playhead stands in, lanes in the order they are drawn.
    pub(crate) fn gap_here(&self) -> Option<(Lane, u32, u32)> {
        if let Some(pick) = self.gap_pick() {
            return Some((pick.lane, pick.start, pick.frames));
        }
        gap_under(self.session.as_ref()?, self.playhead_frame())
    }

    /// Close gap: ripples shut the gap under the playhead (or the picked one),
    /// scoped like the right-click and Delete-on-a-gap doors.
    pub(crate) fn close_gap_here(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some((lane, start, frames)) = self.gap_here() else {
            self.notify_user("Close gap — no gap under the playhead".into());
            cx.notify();
            return;
        };
        if self.lane_locked_refusal(lane) {
            cx.notify();
            return;
        }
        self.close_gap(lane, start, frames, cx);
        if self.gap_pick().is_none() {
            self.gap = None;
        }
    }

    /// The clip a playhead verb acts on: the selected one when the playhead is
    /// over it, else the first clip under the playhead, video first.
    fn clip_here(&self) -> Option<(Lane, usize)> {
        let under = self.under_playhead();
        self.selected
            .anchor()
            .filter(|pick| under.contains(pick))
            .or_else(|| under.first().copied())
    }

    /// Freeze frame: holds the picture the playhead shows for [`FREEZE_SECS`].
    /// Every lane is cut at the playhead and what follows moves later, as the
    /// MCP `freeze` op does with `insert: true`; the still is composed from the
    /// timeline (grade, effects and all) and kept in `freeze/` beside the
    /// project. One undo step.
    pub(crate) fn freeze_frame(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // The picture the hand is holding (the selected clip when the playhead is over it), else
        // the first picture under the playhead: a PiP above a clip does not take the freeze.
        let under = self.under_playhead();
        let Some((lane, idx)) = self
            .clip_here()
            .filter(|(l, _)| l.kind == LaneKind::Video)
            .or_else(|| under.iter().copied().find(|(l, _)| l.kind == LaneKind::Video))
        else {
            self.notify_user("Freeze frame — no picture under the playhead".into());
            cx.notify();
            return;
        };
        let Some(dir) = self.project_path.parent().filter(|_| !self.project_path.as_os_str().is_empty()) else {
            self.notify_user("Freeze frame — the project is unsaved, and the still is kept beside it".into());
            cx.notify();
            return;
        };
        let dir = dir.join("freeze");
        let playhead = self.playhead_frame();
        let at = playhead;
        let fps = self.fps;
        let dur = (FREEZE_SECS * fps).round() as u32;
        let Some(session) = self.session.as_ref() else {
            return;
        };
        // A slowed clip can be cut only where its rate lands on a whole source frame: the still
        // is taken at the nearest such frame, so the held picture and the cut are the same frame.
        let at = session.cut_frame_near(at, FREEZE_REACH).unwrap_or(at);
        // The still is named for the clip it holds, so the timeline reads "Freeze – <clip>".
        let title = session
            .lane_clips(lane)
            .get(idx)
            .and_then(|c| session.sources().get(c.source))
            .and_then(|src| src.path.file_stem())
            .map_or_else(|| "Freeze".to_string(), |s| format!("Freeze \u{2013} {}", s.to_string_lossy()));
        if let Some(locked) = session.lanes().into_iter().find(|&l| session.lane_locked(l)) {
            self.lane_locked_refusal(locked);
            cx.notify();
            return;
        }
        let (proj, meta) = session.export_snapshot();
        let png = match engine::export::compose_at(&proj, &meta, at, meta.width, meta.height)
            .map_err(|e| e.to_string())
            .and_then(|frame| {
                let bytes = engine::decode::to_png_bytes(&frame).map_err(|e| e.to_string())?;
                still_path(&dir, &title, &bytes)
            }) {
            Ok(path) => path,
            Err(e) => {
                self.notify_user(format!("Freeze frame — {e}").into());
                cx.notify();
                return;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.begin_batch();
        let done = Self::freeze_edit(session, at, dur, fps, lane, &png);
        session.end_batch();
        match done {
            Ok(moved) => {
                self.selected.clear();
                self.reset_after_reseek();
                self.mark_dirty();
                // Said plainly when the still landed off the playhead, so the picture the person sees
                // and the frame the toast names are never different without a word about it. When the
                // clips after it moved, the toast says so: the hold is a ripple, not an overwrite.
                let held = format!("for {FREEZE_SECS:.1} s");
                let ripple = match moved {
                    true => format!(" \u{00b7} later clips moved {FREEZE_SECS:.1} s"),
                    false => String::new(),
                };
                let msg = match at == playhead {
                    true => format!("Froze {} {held}{ripple}", timecode(f64::from(at) / fps, fps)),
                    false => format!(
                        "Freeze frame added at {} (nearest frame the speed ramp allows){ripple}",
                        timecode(f64::from(at) / fps, fps)
                    ),
                };
                self.notify_user_action(msg.into(), ToastAction::Undo);
            }
            Err(e) => {
                // A refusal part-way (a slowed clip that cannot be cut here) leaves its cuts in the
                // batch's one step: take that step back so a refused freeze changes nothing.
                if let Some(session) = self.session.as_mut() {
                    session.undo();
                }
                self.reset_after_reseek();
                self.notify_user(format!("Freeze frame — {e}").into());
            }
        }
        cx.notify();
    }

    /// The edit of a freeze, inside the caller's batch.
    pub(crate) fn freeze_edit(
        session: &mut PlaybackSession,
        at: u32,
        dur: u32,
        fps: f64,
        lane: Lane,
        png: &Path,
    ) -> Result<bool, String> {
        let n = match session.sources().iter().position(|s| s.path == png) {
            Some(i) => i,
            None => session.import(png).map_err(|e| format!("cannot import the still: {e}"))?,
        };
        if dur > session.file_frames(png) {
            return Err("that is longer than a still can be held".into());
        }
        let straddles = |s: &PlaybackSession| {
            s.lanes().into_iter().filter(|l| l.kind != LaneKind::Subtitle).any(|l| {
                s.lane_clips(l).iter().any(|c| c.start < at && at < c.end())
            })
        };
        if straddles(session) {
            session.cut_at(f64::from(at) / fps);
            if straddles(session) {
                return Err("a slowed clip cannot be cut at this frame".into());
            }
        }
        let mut picks: Vec<(Lane, usize)> = Vec::new();
        for l in session.lanes() {
            match l.kind {
                LaneKind::Subtitle => {
                    picks.extend(session.sub_lane(l).iter().enumerate().filter(|(_, c)| c.start >= at).map(|(i, _)| (l, i)));
                }
                _ => picks.extend(session.lane_clips(l).iter().enumerate().filter(|(_, c)| c.start >= at).map(|(i, _)| (l, i))),
            }
        }
        let first_media = picks.iter().copied().find(|(l, _)| l.kind != LaneKind::Subtitle);
        match first_media {
            Some((dl, di)) => {
                let start = session.lane_clips(dl)[di].start + dur;
                if !session.move_selection_to(&picks, dl, di, dl, start) {
                    return Err("what follows cannot all move later".into());
                }
            }
            None => {
                for &(l, i) in picks.iter().rev() {
                    let start = session.sub_lane(l)[i].start + dur;
                    session.move_sub(l, i, l, start).map_err(|e| e.to_string())?;
                }
            }
        }
        let tail: Vec<usize> =
            session.titles().iter().enumerate().filter(|(_, t)| t.start >= at).map(|(i, _)| i).collect();
        for &i in tail.iter().rev() {
            let mut t = session.titles()[i].clone();
            t.start += dur;
            session.set_title(i, t);
        }
        session.remap_chapters(|f| if f >= at { f + dur } else { f });
        session.remap_markers(|f| if f >= at { f + dur } else { f });
        if !session.place_at(lane, f64::from(at) / fps, Clip::source_range(n, 0, dur)) {
            return Err("the still would not go there".into());
        }
        // Whether anything after the still moved to make room for it.
        Ok(!picks.is_empty())
    }

    /// The look of the selected clip, read for a copy.
    pub(crate) fn capture_look(&self) -> Option<CopiedLook> {
        look_of(self.session.as_ref()?, self.selected.anchor()?)
    }

    /// Paste attributes: the copied clip's `parts` onto every selected clip.
    /// A picture part reaches the picture of each pick (the video half of an
    /// audio pick's take), the audio part the sound. One undo step.
    pub(crate) fn paste_attributes(&mut self, parts: &[Part], cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(look) = self.copied_look.clone() else {
            self.notify_user("Paste attributes — nothing copied yet".into());
            cx.notify();
            return;
        };
        let picks: Vec<(Lane, usize)> =
            self.marks().0.into_iter().filter(|(l, _)| l.kind != LaneKind::Subtitle).collect();
        if picks.is_empty() {
            self.notify_user("Paste attributes — select the clips to paste onto".into());
            cx.notify();
            return;
        }
        if let Some(lane) = self.marked_locked_lane() {
            self.lane_locked_refusal(lane);
            cx.notify();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let PasteReport { touched, words, why } = paste_look(session, &look, &picks, parts);
        self.reset_after_reseek();
        if touched > 0 {
            self.mark_dirty();
            let mut text = format!("Pasted {} onto {} clip(s)", words.join(", "), picks.len());
            if !why.is_empty() {
                text.push_str(&format!(" — {}", why.join("; ")));
            }
            self.notify_user_action(text.into(), ToastAction::Undo);
        } else if why.is_empty() {
            self.notify_user("Paste attributes — they already match".into());
        } else {
            self.notify_user(format!("Paste attributes — {}", why.join("; ")).into());
        }
        cx.notify();
    }

    /// Match frame: opens the source of the clip under the playhead in the
    /// library preview, parked on the frame the playhead shows.
    pub(crate) fn match_frame(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some((lane, idx)) = self.clip_here() else {
            self.notify_user("Match frame — nothing under the playhead".into());
            cx.notify();
            return;
        };
        let (frame, fps) = (self.playhead_frame(), self.fps);
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some((path, stream, secs)) = matched_source(session, (lane, idx), frame, fps) else {
            self.notify_user("Match frame — a compound clip plays a sequence, not a file".into());
            cx.notify();
            return;
        };
        self.open_preview(&path, stream, cx);
        if self.preview_session.is_none() {
            return;
        }
        self.selected_asset = Some((path.clone(), stream));
        self.seek(secs, cx);
        if let Some(p) = self.preview_session.as_mut() {
            p.pause();
        }
        self.notify_user(
            format!("Matched frame {} of {}; esc returns to the timeline", timecode(secs, fps), file_name(&path))
                .into(),
        );
        cx.notify();
    }

    /// Replace edit: the clip under the playhead (with its linked sound) is
    /// replaced by the selected library item, from its in mark (or the start),
    /// keeping the place and the duration. One undo step.
    pub(crate) fn replace_edit(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some((path, stream)) = self.selected_asset.clone().or_else(|| self.source_target.clone()) else {
            self.notify_user("Replace edit — no library item selected".into());
            cx.notify();
            return;
        };
        let Some((lane, idx)) = self.clip_here() else {
            self.notify_user("Replace edit — nothing under the playhead".into());
            cx.notify();
            return;
        };
        if self.lane_locked_refusal(lane) {
            cx.notify();
            return;
        }
        let fps = self.fps;
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let want = canon(&path);
        let Some(source) = session
            .sources()
            .iter()
            .position(|s| s.audio_stream == stream && (s.path == path || canon(&s.path) == want))
        else {
            self.notify_user(format!("Replace edit — {} is not in this project", file_name(&path)).into());
            cx.notify();
            return;
        };
        let still = engine::is_image(&path);
        let picture = still || (!engine::is_audio(&path) && engine::probe_media(&path).video.is_some());
        let sound = !still
            && engine::AudioSession::probe_streams(&path).is_ok_and(|streams| streams.len() > stream);
        let total = if picture {
            session.file_frames(&path)
        } else {
            engine::AudioSession::stream_duration_secs(&path, stream)
                .ok()
                .flatten()
                .map_or(0, |secs| (secs * fps).ceil() as u32)
        };
        let in_f = match still {
            true => 0,
            false => self
                .source_marks
                .get(&(path.clone(), stream))
                .and_then(|m| m.0)
                .map_or(0, |t| (t.max(0.) * fps).round() as u32),
        };
        let (at, len, items) = match replace_plan(session, (lane, idx), source, in_f, total, (picture, sound, still)) {
            Ok(plan) => plan,
            Err(why) => {
                self.notify_user(format!("Replace edit — {why}").into());
                cx.notify();
                return;
            }
        };
        let done = match self.session.as_mut() {
            Some(s) => s.edit_from_source_at(engine::SourceEdit::Overwrite, f64::from(at) / fps, &items),
            None => return,
        };
        match done {
            Ok(()) => {
                self.selected.clear();
                self.reset_after_reseek();
                self.mark_dirty();
                self.notify_user_action(
                    format!("Replaced with {} for {}", file_name(&path), timecode(f64::from(len) / fps, fps)).into(),
                    ToastAction::Undo,
                );
            }
            Err(e) => self.notify_user(format!("Replace edit refused — {e}").into()),
        }
        cx.notify();
    }

    /// Select track forward (`all` false) or all tracks forward: every clip
    /// that has not ended by the playhead, on the selected clip's track (else
    /// the track under the playhead, else V1) or on every unlocked media track.
    pub(crate) fn select_forward(&mut self, all: bool, cx: &mut Context<Self>) {
        let frame = self.playhead_frame();
        let lane = self
            .selected
            .anchor()
            .map(|(l, _)| l)
            .or_else(|| self.under_playhead().first().map(|(l, _)| *l))
            .unwrap_or(Lane::V1);
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let lanes: Vec<Lane> = match all {
            true => session.lanes(),
            false => vec![lane],
        };
        let picked = forward_picks(session, frame, &lanes);
        if picked.is_empty() {
            let word = if all { "on any track".to_string() } else { format!("on {}", lane.label()) };
            self.notify_user(format!("Nothing after the playhead {word}").into());
        }
        self.selected = picked;
        self.gap = None;
        cx.notify();
    }

    /// Split edit: moves the audio's (or the picture's) edge of the selected
    /// take to the playhead while the other half stays put -- an L-cut when the
    /// playhead is past the cut, a J-cut when before it. The pair is taken out
    /// of its group first (a group trims together), as every NLE's split edit
    /// unlinks. One undo step.
    pub(crate) fn split_edit(&mut self, audio: bool, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(lane) = self.marked_locked_lane() {
            self.lane_locked_refusal(lane);
            cx.notify();
            return;
        }
        let frame = self.playhead_frame();
        let Some(anchor) = self.selected.anchor() else {
            self.notify_user("Split edit — no clip selected".into());
            cx.notify();
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match split_edit_in(session, anchor, audio, frame) {
            Ok((lane, idx, _, now)) => {
                self.selected.set_one((lane, idx));
                self.reset_after_reseek();
                self.mark_dirty();
                let fps = self.fps;
                let word = if audio { "Audio" } else { "Video" };
                self.notify_user_action(
                    format!("{word} edge moved to {} — the pair is unlinked", timecode(f64::from(now) / fps, fps))
                        .into(),
                    ToastAction::Undo,
                );
            }
            Err(e) => self.notify_user(format!("Split edit — {e}").into()),
        }
        cx.notify();
    }

    /// Nudge: the selected clips (and their groups) `frames` along the
    /// timeline, as one move, refused whole when a neighbour is in the way.
    pub(crate) fn nudge_selected(&mut self, frames: i64, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(lane) = self.marked_locked_lane() {
            self.lane_locked_refusal(lane);
            cx.notify();
            return;
        }
        let picks: Vec<(Lane, usize)> =
            self.marks().0.into_iter().filter(|(l, _)| l.kind != LaneKind::Subtitle).collect();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match nudge(session, &picks, frames) {
            Ok(()) => {
                self.reset_after_reseek();
                self.mark_dirty();
            }
            Err(why) => self.notify_user(format!("Nudge — {why}").into()),
        }
        cx.notify();
    }
}

impl Player {
    /// A left press on the bed of `lane` at `frame`: starts a box select unless
    /// a clip or caption is under the pointer (that press is the clip's own).
    pub(crate) fn begin_marquee(&mut self, lane: Lane, frame: u32, at: Point<Pixels>) {
        // The title and caption picks are let go on the release ([`Player::finish_marquee`]), once
        // the press is known to be a click on bare bed and not a drag that may still add to the
        // picks. A title left picked past the click is what Backspace then removed unseen.
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let covered = match lane.kind {
            LaneKind::Subtitle => session.sub_lane(lane).iter().any(|s| s.start <= frame && frame < s.end()),
            _ => session.lane_clips(lane).iter().any(|c| c.start <= frame && frame < c.end()),
        };
        if !covered && self.context_menu.is_none() {
            self.marquee = Some(Marquee { from: at, to: at });
        }
    }

    /// The release of a box select: every clip the rectangle touches, on every
    /// lane it crosses, becomes the selection (ctrl adds to it). A box that
    /// never travelled is a click and selects nothing.
    pub(crate) fn finish_marquee(&mut self, m: Marquee, add: bool, cx: &mut Context<Self>) {
        // One selection model: a press on bare bed is the one thing picked next. A title and a
        // caption go at once, and a plain click empties the clips too (a ctrl-click adds to them).
        self.drop_title_selection(cx);
        self.drop_sel_cue();
        if !m.live() {
            if !add {
                self.selected = Selection::new();
            }
            cx.notify();
            return;
        }
        let (left, top, right, bottom) = m.edges();
        let (f1, f2) = (self.frame_under(left), self.frame_under(right));
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let beds: Vec<(Lane, f32, f32)> = self
            .lane_beds
            .borrow()
            .iter()
            .map(|(l, b)| (*l, f32::from(b.top()), f32::from(b.bottom())))
            .collect();
        let base = if add { self.selected.clone() } else { Selection::new() };
        self.selected = marquee_picks(session, &beds, (f32::from(top), f32::from(bottom)), (f1, f2), base);
        self.gap = None;
        cx.notify();
    }
}
