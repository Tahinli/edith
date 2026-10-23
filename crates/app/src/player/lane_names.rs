//! Track names on the lane heads: a label the user gives a track (the address, V1 or A2, stays
//! what every verb takes). Retyped in place on the head, double-click or the head menu's
//! "Rename track…"; enter saves (one undo step), escape gives up.

use crate::*;
use engine::project::MAX_LANE_NAME_CHARS;

/// The end of `text` that fits `fit` characters beside the caret: what a head shows of a name
/// longer than the box while it is retyped.
pub(crate) fn edit_tail(text: &str, fit: usize) -> String {
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(fit.saturating_sub(1))).collect()
}

/// `text` cut to `fit` characters with a trailing `…` when it is longer: what a head shows of a
/// long name at rest (the tooltip has the whole of it).
pub(crate) fn ellipsise(text: &str, fit: usize) -> String {
    if text.chars().count() <= fit {
        return text.to_string();
    }
    let head: String = text.chars().take(fit.saturating_sub(1)).collect();
    format!("{head}…")
}

impl Player {
    /// What a head shows for `lane`: its name, else its address.
    pub(crate) fn lane_title(&self, lane: Lane) -> String {
        self.session.as_ref().map_or_else(|| lane.label(), |s| s.lane_title(lane))
    }

    /// Starts retyping `lane`'s name on its head, from the name it has. A locked track offers
    /// its Unlock instead.
    pub(crate) fn begin_lane_edit(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.exporting().is_some() || self.session.is_none() || self.lane_locked_refusal(lane) {
            cx.notify();
            return;
        }
        self.context_menu = None;
        let name = self.session.as_ref().and_then(|s| s.lane_name(lane)).unwrap_or_default().to_string();
        self.lane_edit = Some((lane, crate::player::text_field::TextField::selected(name)));
        cx.notify();
    }

    /// Writes what was typed as the track's name (one undo step); empty clears it, unchanged is
    /// no edit.
    pub(crate) fn commit_lane_edit(&mut self, cx: &mut Context<Self>) {
        let Some((lane, field)) = self.lane_edit.take() else {
            return;
        };
        let text = field.text;
        if let Some(session) = self.session.as_mut() {
            match session.set_lane_name(lane, &text) {
                Ok(true) => self.mark_dirty(),
                Ok(false) => {}
                Err(e) => self.notify_user(format!("Not renamed — {e}").into()),
            }
        }
        cx.notify();
    }

    /// The keyboard while a track name is being retyped (ahead of the palette and every chord):
    /// enter saves, escape gives up, the rest is text.
    pub(crate) fn lane_edit_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        key_char: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        use crate::player::text_field::FieldKey;
        let Some((_, field)) = self.lane_edit.as_mut() else {
            return;
        };
        match field.key(key, ctrl, alt, key_char, MAX_LANE_NAME_CHARS) {
            FieldKey::Cancel => self.lane_edit = None,
            FieldKey::Commit => self.commit_lane_edit(cx),
            FieldKey::Edited => {}
        }
        cx.notify();
    }
}
