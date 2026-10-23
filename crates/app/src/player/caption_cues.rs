//! Captions as blocks of their own on a subtitle lane, the way an NLE draws them: each cue is a
//! block with its words inside, a click picks that one cue, an edge drag trims it and a body drag
//! moves it between its neighbours, and a right-click opens the caption menu.
//!
//! The engine keeps the words on the **track** and a lane only holds windows of it
//! ([`engine::project::SubClip`]), so every edit here is a cue edit on the track
//! ([`PlaybackSession::set_cue_times`], [`PlaybackSession::merge_cue_next`],
//! [`PlaybackSession::split_cues_at`], [`PlaybackSession::delete_cue`]): one undo step each, and the
//! placement is never touched.

use engine::PlaybackSession;
use engine::project::MIN_CUE_US;

use crate::*;

/// Which part of a cue block a drag holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CueGrab {
    Start,
    End,
    Body,
}

/// A caption being dragged. All times are on the track's own clock; the pointer is turned into
/// that clock through the placement that shows the cue ([`PlaybackSession::cue_track_us_at`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CueDrag {
    pub(crate) lane: Lane,
    pub(crate) track: usize,
    pub(crate) cue: usize,
    pub(crate) grab: CueGrab,
    /// Where the pointer stood at the press.
    pub(crate) press_us: i64,
    /// The cue's times at the press, and as the drag has them now.
    pub(crate) orig: (i64, i64),
    pub(crate) now: (i64, i64),
    /// The neighbours' walls ([`PlaybackSession::cue_room`]).
    pub(crate) room: (i64, i64),
}

/// A number of the caption panel: where the caption starts and ends on the timeline. Its length
/// is the difference of the two, so it only reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CueNum {
    Start,
    End,
}

/// A panel number being dragged sideways: the number it is, where the drag began, the value it
/// has now, and whether the pointer has left the press. Written once on the release, so a scrub
/// is one undo step; a press that never moves is a click that types the number instead.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CueNumScrub {
    pub(crate) which: CueNum,
    pub(crate) x0: f32,
    pub(crate) v0: i64,
    pub(crate) cur: i64,
    pub(crate) moved: bool,
}

/// Seconds a panel number moves per pixel of drag (a fine drag, with shift, moves a tenth).
const CUE_NUM_SECS_PER_PX: f64 = 0.02;

/// The value a panel number scrubs to: `v0` moved by `dx` pixels, on whole frames, inside the
/// range a caption can span (a frame, up to ten hours).
pub(crate) fn scrubbed_us(v0: i64, dx: f32, fine: bool, fps: f64) -> i64 {
    let fps = if fps.is_finite() && fps > 0. { fps } else { 25. };
    let secs = crate::ui::kit::scrub_value(v0 as f64 / 1e6, dx, CUE_NUM_SECS_PER_PX, fine, 0., 36_000.);
    ((secs * fps).round() / fps * 1e6).round() as i64
}

/// The open caption menu: the cue it names and where the click was.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CueMenu {
    pub(crate) lane: Lane,
    pub(crate) track: usize,
    pub(crate) cue: usize,
    pub(crate) at: Point<Pixels>,
}

/// Where a drag leaves a cue: the edge in the hand follows `to`, a body keeps its length, and
/// every result stays inside `room` and at least [`MIN_CUE_US`] long.
pub(crate) fn dragged_times(grab: CueGrab, orig: (i64, i64), room: (i64, i64), press: i64, to: i64) -> (i64, i64) {
    match grab {
        CueGrab::Start => (to.clamp(room.0, (orig.1 - MIN_CUE_US).max(room.0)), orig.1),
        CueGrab::End => (orig.0, to.clamp((orig.0 + MIN_CUE_US).min(room.1), room.1)),
        CueGrab::Body => {
            let by = (to - press).clamp(room.0 - orig.0, room.1 - orig.1);
            (orig.0 + by, orig.1 + by)
        }
    }
}

impl Player {
    /// The cue `(lane, track, cue)` if it is still there and a text one.
    fn cue_ref(&self, track: usize, cue: usize) -> Option<&engine::subtitle::Cue> {
        self.session.as_ref()?.subtitles().get(track)?.cues.get(cue)
    }

    /// Lets go of the selected caption, and of what its panel was holding open.
    pub(crate) fn drop_sel_cue(&mut self) {
        self.sel_cue = None;
        self.cue_num = None;
        self.cue_num_scrub = None;
        if self.cue_edit.as_ref().is_some_and(|e| e.in_panel) {
            self.cue_edit = None;
        }
    }

    /// Picks one cue: the placement's own selection goes, so the ring is on the one block.
    pub(crate) fn select_cue(&mut self, lane: Lane, track: usize, cue: usize, cx: &mut Context<Self>) {
        if self.lane_locked_refusal(lane) {
            return;
        }
        self.drop_title_selection(cx);
        self.selected.clear();
        self.gap = None;
        self.cue_menu = None;
        self.cue_num = None;
        self.cue_num_scrub = None;
        self.sel_cue = Some((lane, track, cue));
        cx.notify();
    }

    /// The track's clock under window x, for the cue's placement, with the pointer snapped to
    /// whole frames (a cue edge lands on a frame, as every cut does).
    fn cue_us_under(&self, lane: Lane, track: usize, cue: usize, x: Pixels) -> Option<i64> {
        let secs = self.scale.time_at(px_along(x, self.ruler.get()));
        let tl_us = ((secs * self.fps).round() / self.fps * 1e6).round() as i64;
        self.session.as_ref()?.cue_track_us_at(lane, track, cue, tl_us)
    }

    /// A press on a cue's edge or body: picks it and arms the drag. Nothing is written until the
    /// release, and a press that never moves is only the pick.
    pub(crate) fn start_cue_drag(
        &mut self,
        lane: Lane,
        track: usize,
        cue: usize,
        grab: CueGrab,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        if self.covers_room() || self.exporting().is_some() || self.lane_locked_refusal(lane) {
            return;
        }
        let Some(c) = self.cue_ref(track, cue).filter(|c| c.image.is_none()) else {
            return;
        };
        let orig = (c.start_us, c.end_us);
        let Some(room) = self.session.as_ref().and_then(|s| s.cue_room(track, cue)) else {
            return;
        };
        let press_us = self.cue_us_under(lane, track, cue, x).unwrap_or(orig.0);
        self.select_cue(lane, track, cue, cx);
        self.cue_drag = Some(CueDrag { lane, track, cue, grab, press_us, orig, now: orig, room });
        cx.notify();
    }

    /// The pointer moved with a cue held.
    pub(crate) fn cue_drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.cue_drag else {
            return;
        };
        let Some(to) = self.cue_us_under(drag.lane, drag.track, drag.cue, x) else {
            return;
        };
        let now = dragged_times(drag.grab, drag.orig, drag.room, drag.press_us, to);
        if now != drag.now {
            self.cue_drag = Some(CueDrag { now, ..drag });
            cx.notify();
        }
    }

    /// The release: the whole drag reaches the engine as one edit, so it is one undo step.
    pub(crate) fn commit_cue_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.cue_drag.take() else {
            return;
        };
        if drag.now != drag.orig
            && let Some(session) = self.session.as_mut()
        {
            match session.set_cue_times(drag.track, drag.cue, drag.now.0, drag.now.1) {
                Ok(true) => self.mark_dirty(),
                Ok(false) => {}
                Err(e) => self.notify_user(format!("Caption not changed — {e}").into()),
            }
        }
        cx.notify();
    }

    /// The times a cue is drawn at while it is being dragged, if it is the one.
    pub(crate) fn dragged_cue(&self, track: usize, cue: usize) -> Option<CueDrag> {
        self.cue_drag.filter(|d| (d.track, d.cue) == (track, cue))
    }

    /// The selected caption's own numbers on the timeline: `(start, end)` in microseconds, through
    /// the placement that shows it. `None` when the selection no longer names a shown caption.
    pub(crate) fn sel_cue_span(&self) -> Option<(Lane, usize, usize, i64, i64)> {
        let (lane, track, cue) = self.sel_cue?;
        let span = self
            .session
            .as_ref()?
            .sub_lane_cue_spans(lane)
            .into_iter()
            .find(|s| s.track == track && s.cue == cue)?;
        Some((lane, track, cue, span.start_us, span.end_us))
    }

    /// The caption panel's text field was pressed: the words open for typing in the panel.
    pub(crate) fn open_panel_cue_edit(&mut self, cx: &mut Context<Self>) {
        let Some((lane, track, cue)) = self.sel_cue else {
            return;
        };
        if self.cue_edit.as_ref().is_some_and(|e| (e.track, e.cue) == (track, cue)) {
            return;
        }
        self.cue_edit = None;
        self.open_cue_edit(lane, track, cue, cx);
        if let Some(edit) = self.cue_edit.as_mut() {
            edit.in_panel = true;
        }
    }

    /// A number of the panel was pressed: it opens for typing on what it shows.
    pub(crate) fn begin_cue_num(&mut self, which: CueNum, cx: &mut Context<Self>) {
        self.commit_cue_edit(cx);
        let Some((_, _, _, start, end)) = self.sel_cue_span() else {
            return;
        };
        let us = match which {
            CueNum::Start => start,
            CueNum::End => end,
        };
        self.cue_num = Some((which, crate::ui::kit::NumberEdit::begin(&cue_secs(us))));
        cx.notify();
    }

    /// A panel number was pressed for a drag: the number follows the pointer sideways until the
    /// button comes up ([`Player::cue_num_scrub_end`]). Nothing is written before the release.
    pub(crate) fn begin_cue_num_scrub(&mut self, which: CueNum, x: f32, cx: &mut Context<Self>) {
        self.commit_cue_edit(cx);
        let Some((lane, _, _, start, end)) = self.sel_cue_span() else {
            return;
        };
        if self.session.as_ref().is_some_and(|s| s.lane_locked(lane)) {
            return;
        }
        let v0 = match which {
            CueNum::Start => start,
            CueNum::End => end,
        };
        self.cue_num_scrub = Some(CueNumScrub { which, x0: x, v0, cur: v0, moved: false });
        cx.notify();
    }

    /// The pointer moved with a panel number held: the number shows the value it would take.
    pub(crate) fn cue_num_scrub_move(&mut self, x: f32, fine: bool, cx: &mut Context<Self>) {
        let Some(s) = self.cue_num_scrub else {
            return;
        };
        let dx = x - s.x0;
        let cur = scrubbed_us(s.v0, dx, fine, self.fps);
        let moved = s.moved || dx.abs() > 3.;
        if cur != s.cur || moved != s.moved {
            self.cue_num_scrub = Some(CueNumScrub { cur, moved, ..s });
            cx.notify();
        }
    }

    /// The button came up over a panel number: a drag that moved is written as one cue edit.
    /// A press that never moved is left to the field's click, which types it. `true` when a
    /// scrub was in flight, so the release goes no further.
    pub(crate) fn cue_num_scrub_end(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = self.cue_num_scrub.take() else {
            return false;
        };
        if s.moved && s.cur != s.v0 {
            self.set_cue_num(s.which, s.cur, cx);
        } else {
            cx.notify();
        }
        true
    }

    /// Escape while a caption's words are typed: they are given up, as a title's are, so the old
    /// words stay and a toast says so. `false` when no words were being typed.
    pub(crate) fn escape_cue_edit(&mut self) -> bool {
        if self.cue_edit.take().is_none() {
            return false;
        }
        self.notify_user("Caption edit cancelled".into());
        true
    }

    /// The keyboard while a caption's words are typed, in the box over its block or in the Clip
    /// tab's panel: enter saves, shift+enter starts a new line, escape gives up, Ctrl+A selects the
    /// words, and the rest is text.
    pub(crate) fn cue_edit_key(&mut self, key: &str, ctrl: bool, shift: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        match key {
            "escape" => {
                self.escape_cue_edit();
            }
            "enter" if shift => {
                if let Some(edit) = self.cue_edit.as_mut() {
                    edit.newline();
                }
            }
            "enter" => self.commit_cue_edit(cx),
            "backspace" => {
                if let Some(edit) = self.cue_edit.as_mut() {
                    edit.backspace();
                }
            }
            "a" if ctrl && !alt => {
                if let Some(edit) = self.cue_edit.as_mut() {
                    edit.select_all();
                }
            }
            _ => {
                // The character the layout made, not the key's name: a non-ASCII letter has no
                // key name. A chord with ctrl or alt is not a letter.
                if !ctrl
                    && !alt
                    && let Some(edit) = self.cue_edit.as_mut()
                    && let Some(chars) = key_char
                {
                    for c in chars.chars() {
                        edit.typed(c);
                    }
                }
            }
        }
        cx.notify();
    }

    /// One key while a panel number is typed: Enter writes it, Escape drops it.
    pub(crate) fn cue_num_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        use crate::player::text_field::FieldKey;
        let Some((which, mut edit)) = self.cue_num.take() else {
            return;
        };
        if self.sel_cue.is_none() {
            return cx.notify();
        }
        match edit.key(key, ctrl, alt, key_char) {
            FieldKey::Edited => self.cue_num = Some((which, edit)),
            FieldKey::Cancel => {}
            FieldKey::Commit => {
                if let Some(secs) = edit.commit(0., 36_000.) {
                    self.set_cue_num(which, (secs * 1e6).round() as i64, cx);
                }
            }
        }
        cx.notify();
    }

    /// Writes one panel number as one cue edit: the times are the timeline's, turned into the
    /// track's clock, and kept inside the neighbours and at least [`MIN_CUE_US`] long.
    pub(crate) fn set_cue_num(&mut self, which: CueNum, us: i64, cx: &mut Context<Self>) {
        let Some((lane, track, cue, _, _)) = self.sel_cue_span() else {
            return;
        };
        if self.lane_locked_refusal(lane) {
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some(room) = session.cue_room(track, cue) else {
            return;
        };
        let Some(c) = self.cue_ref(track, cue).filter(|c| c.image.is_none()) else {
            return;
        };
        let (s0, e0) = (c.start_us, c.end_us);
        let to_track = |tl: i64| session.cue_track_us_at(lane, track, cue, tl).unwrap_or(tl);
        let (s, e) = match which {
            CueNum::Start => (to_track(us).clamp(room.0, (e0 - MIN_CUE_US).max(room.0)), e0),
            CueNum::End => (s0, to_track(us).clamp((s0 + MIN_CUE_US).min(room.1), room.1)),
        };
        if (s, e) == (s0, e0) {
            return;
        }
        if let Some(session) = self.session.as_mut() {
            match session.set_cue_times(track, cue, s, e) {
                Ok(true) => self.mark_dirty(),
                Ok(false) => {}
                Err(e) => self.notify_user(format!("Caption not changed — {e}").into()),
            }
        }
        cx.notify();
    }

    /// Opens the caption menu on a cue (a right-click on its block).
    pub(crate) fn open_cue_menu(&mut self, lane: Lane, track: usize, cue: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.lane_locked_refusal(lane) {
            return;
        }
        self.context_menu = None;
        self.drop_title_selection(cx);
        self.sel_cue = Some((lane, track, cue));
        self.selected.clear();
        self.gap = None;
        self.cue_menu = Some(CueMenu { lane, track, cue, at });
        cx.notify();
    }

    /// Why Split at playhead cannot run on this cue, if it cannot.
    pub(crate) fn cue_split_why(&self, m: CueMenu) -> Option<&'static str> {
        let session = self.session.as_ref()?;
        let c = self.cue_ref(m.track, m.cue)?;
        if c.image.is_some() {
            return Some("A picture caption cannot be split");
        }
        let at = session.cue_track_us_at(m.lane, m.track, m.cue, (session.now() * 1e6).round() as i64);
        match at {
            Some(t) if c.start_us < t && t < c.end_us => None,
            _ => Some("Move the playhead onto this caption first"),
        }
    }

    /// Why Merge with next cannot run on this cue, if it cannot.
    pub(crate) fn cue_merge_why(&self, m: CueMenu) -> Option<&'static str> {
        let cues = &self.session.as_ref()?.subtitles().get(m.track)?.cues;
        match (cues.get(m.cue), cues.get(m.cue + 1)) {
            (Some(a), Some(b)) if a.image.is_none() && b.image.is_none() => None,
            (Some(a), Some(_)) if a.image.is_some() => Some("A picture caption cannot be merged"),
            _ => Some("This is the last caption"),
        }
    }

    /// Splits the cue at the playhead.
    pub(crate) fn split_cue_at_playhead(&mut self, m: CueMenu, cx: &mut Context<Self>) {
        self.cue_menu = None;
        if let Some(why) = self.cue_split_why(m) {
            self.notify_user(why.into());
            return cx.notify();
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let now = (session.now() * 1e6).round() as i64;
        if let Some(t) = session.cue_track_us_at(m.lane, m.track, m.cue, now)
            && session.split_cues_at(m.track, t)
        {
            self.sel_cue = Some((m.lane, m.track, m.cue));
            self.mark_dirty();
        } else {
            self.notify_user("Not split — a caption needs a word on each side".into());
        }
        cx.notify();
    }

    /// Joins the cue with the next one.
    pub(crate) fn merge_cue_with_next(&mut self, m: CueMenu, cx: &mut Context<Self>) {
        self.cue_menu = None;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.merge_cue_next(m.track, m.cue) {
            Ok(()) => self.mark_dirty(),
            Err(e) => self.notify_user(format!("Not merged — {e}").into()),
        }
        cx.notify();
    }

    /// Takes the cue out of its track.
    pub(crate) fn delete_cue_at(&mut self, m: CueMenu, cx: &mut Context<Self>) {
        self.cue_menu = None;
        if self.lane_locked_refusal(m.lane) {
            return;
        }
        if let Some(session) = self.session.as_mut()
            && PlaybackSession::delete_cue(session, m.track, m.cue)
        {
            self.drop_sel_cue();
            self.mark_dirty();
        }
        cx.notify();
    }
}

/// Seconds as the panel's fields show them: two places.
pub(crate) fn cue_secs(us: i64) -> String {
    format!("{:.2}", us as f64 / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dragged_cue_stays_inside_its_neighbours_and_keeps_its_length() {
        let (orig, room) = ((3_000_000, 5_000_000), (2_000_000, 6_000_000));
        // An edge stops at the neighbour and at the other edge less the minimum.
        assert_eq!(dragged_times(CueGrab::Start, orig, room, 0, 1_000_000), (2_000_000, 5_000_000));
        assert_eq!(dragged_times(CueGrab::Start, orig, room, 0, 9_000_000), (5_000_000 - MIN_CUE_US, 5_000_000));
        assert_eq!(dragged_times(CueGrab::End, orig, room, 0, 9_000_000), (3_000_000, 6_000_000));
        assert_eq!(dragged_times(CueGrab::End, orig, room, 0, 0), (3_000_000, 3_000_000 + MIN_CUE_US));
        // A body moves as a whole and stops when either end meets a wall.
        assert_eq!(dragged_times(CueGrab::Body, orig, room, 4_000_000, 4_500_000), (3_500_000, 5_500_000));
        assert_eq!(dragged_times(CueGrab::Body, orig, room, 4_000_000, 9_000_000), (4_000_000, 6_000_000));
        assert_eq!(dragged_times(CueGrab::Body, orig, room, 4_000_000, 0), (2_000_000, 4_000_000));
    }
}
