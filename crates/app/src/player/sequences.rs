//! Sequences and compound clips on the window's side: the tab strip's verbs (new, duplicate,
//! rename, delete, switch), nesting the selection into a compound clip, breaking one apart,
//! opening one (double-click) and going back to its parent. The model is the engine's
//! ([`engine::project`]'s sequences); every change here is one undo step through the session,
//! except switching, which edits nothing.

use crate::*;
use engine::project::{MAX_SEQUENCE_NAME_CHARS, sequence_id_of};

impl Player {
    /// The sequence ids the strip draws, in tab order, with the open one marked.
    pub(crate) fn sequence_tabs(&self) -> Vec<engine::project::SequenceInfo> {
        let mut tabs = self.session.as_ref().map(|s| s.sequences()).unwrap_or_default();
        // A sequence with no name still has a tab worth reading: it is called by its place.
        for (i, tab) in tabs.iter_mut().enumerate() {
            if tab.name.trim().is_empty() {
                tab.name = format!("Sequence {}", i + 1);
            }
        }
        tabs
    }

    /// Opens sequence `id` for editing. What the old one had selected names nothing in the new
    /// one, and an edit being retyped is given up. Not an undo step.
    fn show_sequence(&mut self, id: u32, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if let Err(e) = session.switch_sequence(id) {
            self.notify_user(format!("Not opened — {e}").into());
            cx.notify();
            return false;
        }
        self.selected.clear();
        self.seq_edit = None;
        self.gap = None;
        self.reset_after_reseek();
        cx.notify();
        true
    }

    /// A tab clicked: that sequence opens, and the trail back through compound clips starts over.
    pub(crate) fn switch_sequence_tab(&mut self, id: u32, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self.session.as_ref().is_some_and(|s| s.active_sequence() != id) && self.show_sequence(id, cx) {
            self.mark_structural();
            self.seq_trail.clear();
        }
    }

    /// Makes a new empty sequence and opens it.
    pub(crate) fn new_sequence(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.new_sequence(None) {
            Ok(id) => {
                self.mark_structural();
                self.seq_trail.clear();
                if self.show_sequence(id, cx) {
                    let name = self.session.as_ref().and_then(|s| s.sequence_name(id)).unwrap_or_default();
                    self.notify_user(format!("New sequence “{name}”").into());
                }
            }
            Err(e) => self.notify_user(format!("No sequence made — {e}").into()),
        }
        cx.notify();
    }

    /// Copies the open sequence and opens the copy.
    pub(crate) fn duplicate_sequence(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.duplicate_sequence(session.active_sequence()) {
            Ok(id) => {
                self.mark_structural();
                self.seq_trail.clear();
                if self.show_sequence(id, cx) {
                    let name = self.session.as_ref().and_then(|s| s.sequence_name(id)).unwrap_or_default();
                    self.notify_user(format!("Duplicated as “{name}”").into());
                }
            }
            Err(e) => self.notify_user(format!("Not duplicated — {e}").into()),
        }
        cx.notify();
    }

    /// Deletes the open sequence; the engine picks the one that opens in its place.
    pub(crate) fn delete_sequence(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let (id, name) = (session.active_sequence(), session.sequence_name(session.active_sequence()));
        match session.delete_sequence(id) {
            Ok(_) => {
                self.selected.clear();
                self.seq_edit = None;
                self.seq_trail.retain(|&t| t != id);
                self.reset_after_reseek();
                self.notify_user_action(
                    format!("Deleted “{}”", name.unwrap_or_default()).into(),
                    crate::notices::ToastAction::Undo,
                );
            }
            Err(e) => self.notify_user(format!("Not deleted — {e}").into()),
        }
        cx.notify();
    }

    /// Starts retyping the open sequence's name on its tab, from the name it has.
    pub(crate) fn begin_seq_edit(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        self.seq_edit = session.sequence_name(session.active_sequence()).map(crate::player::text_field::TextField::selected);
        cx.notify();
    }

    /// Writes what was typed as the sequence's name (one undo step). An empty name is refused
    /// out loud and the old one stays; an unchanged one is no edit.
    pub(crate) fn commit_seq_edit(&mut self, cx: &mut Context<Self>) {
        let Some(field) = self.seq_edit.take() else {
            return;
        };
        let text = field.text;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let id = session.active_sequence();
        let text = text.trim().to_string();
        if text.is_empty() {
            self.notify_user(format!("A sequence needs a name (up to {MAX_SEQUENCE_NAME_CHARS} characters)").into());
        } else if session.sequence_name(id).as_deref() != Some(text.as_str()) {
            match session.rename_sequence(id, &text) {
                Ok(()) => self.mark_dirty(),
                Err(e) => self.notify_user(format!("Not renamed — {e}").into()),
            }
        }
        cx.notify();
    }

    /// The keyboard while a name is being retyped (ahead of the palette and every chord): enter
    /// saves, escape gives up, the rest is text.
    pub(crate) fn seq_edit_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        key_char: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        use crate::player::text_field::FieldKey;
        let Some(field) = self.seq_edit.as_mut() else {
            return;
        };
        match field.key(key, ctrl, alt, key_char, MAX_SEQUENCE_NAME_CHARS) {
            FieldKey::Cancel => self.seq_edit = None,
            FieldKey::Commit => self.commit_seq_edit(cx),
            FieldKey::Edited => {}
        }
        cx.notify();
    }

    /// Folds the selected clips (and their linked partners) into a new sequence and leaves one
    /// compound clip in their place. One undo step.
    pub(crate) fn nest_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let mut picks: Vec<(Lane, usize)> =
            self.marks().0.into_iter().filter(|(lane, _)| lane.kind != LaneKind::Subtitle).collect();
        if picks.is_empty() {
            picks.extend(self.selected.anchor().filter(|(lane, _)| lane.kind != LaneKind::Subtitle));
        }
        if let Some(lane) = picks.iter().map(|p| p.0).find(|&l| self.session.as_ref().is_some_and(|s| s.lane_locked(l))) {
            self.lane_locked_refusal(lane);
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.nest(&picks, None) {
            Ok(made) => {
                self.mark_structural();
                self.selected.clear();
                if let Some(&first) = made.clips.first() {
                    self.selected.set_one(first);
                }
                for &pick in made.clips.iter().skip(1) {
                    self.selected.add(pick);
                }
                let name = self.session.as_ref().and_then(|s| s.sequence_name(made.sequence)).unwrap_or_default();
                self.notify_user(format!("Nested into “{name}”").into());
            }
            Err(e) => self.notify_user(format!("Not nested — {e}").into()),
        }
        self.reset_after_reseek();
        cx.notify();
    }

    /// Puts the selected compound clips' contents back on the timeline. One undo step.
    pub(crate) fn break_apart_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picks: Vec<(Lane, usize)> = {
            let mut p = self.marks().0;
            if p.is_empty() {
                p.extend(self.selected.anchor());
            }
            p
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let compounds: Vec<(Lane, usize)> = picks
            .into_iter()
            .filter(|&(l, i)| l.kind != LaneKind::Subtitle && session.compound_at(l, i).is_some())
            .collect();
        if compounds.is_empty() {
            self.notify_user("Not broken apart — select a compound clip".into());
            cx.notify();
            return;
        }
        if let Some(lane) = compounds.iter().map(|p| p.0).find(|&l| session.lane_locked(l)) {
            self.lane_locked_refusal(lane);
            return;
        }
        match session.break_apart(&compounds) {
            Ok(clips) => {
                self.selected.clear();
                if let Some(&first) = clips.first() {
                    self.selected.set_one(first);
                }
                for &pick in clips.iter().skip(1) {
                    self.selected.add(pick);
                }
                self.notify_user(format!("Broke apart into {}", crate::notices::count_noun(clips.len(), "clip", "clips")).into());
            }
            Err(e) => self.notify_user(format!("Not broken apart — {e}").into()),
        }
        self.reset_after_reseek();
        cx.notify();
    }

    /// Opens the selected compound clip's sequence, remembering where it came from.
    pub(crate) fn open_compound_selected(&mut self, cx: &mut Context<Self>) {
        if let Some((lane, idx)) = self.selected.anchor() {
            self.open_compound(lane, idx, cx);
        }
    }

    /// Opens the sequence the compound clip at `(lane, idx)` plays. A clip that is not a
    /// compound is left alone (a double-click on any clip lands here).
    pub(crate) fn open_compound(&mut self, lane: Lane, idx: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some(id) = session.lane_clips(lane).get(idx).and_then(|c| sequence_id_of(c.source)) else {
            return;
        };
        let here = session.active_sequence();
        if self.show_sequence(id, cx) {
            self.seq_trail.push(here);
            // The playhead was the parent's: the compound opens at its own start, not at the
            // parent's time (which can lie past the end of the sequence just entered).
            self.seek(0., cx);
        }
    }

    /// Back to the sequence the open one was entered from. Parents that have since gone are skipped.
    pub(crate) fn close_compound(&mut self, cx: &mut Context<Self>) {
        let alive: Vec<u32> = self.sequence_tabs().iter().map(|q| q.id).collect();
        while let Some(parent) = self.seq_trail.pop() {
            if alive.contains(&parent) {
                self.show_sequence(parent, cx);
                return;
            }
        }
    }
}
