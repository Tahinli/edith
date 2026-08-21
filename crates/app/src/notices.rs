//! The notice bar's queue.

use crate::*;

/// How many messages wait behind the one on the bar before the oldest is
/// dropped. A queue with no ceiling is a way for a stuck loop to eat the heap;
/// eight is more than a user will ever answer in a row.
pub(crate) const NOTICES_MAX: usize = 8;

/// The narrowest a notice's *message* is allowed to be squeezed before its hint
/// gives up the line and drops below it.
///
/// The bar is a message beside a hint, and at the 640x360 floor the picture
/// region it hangs under is narrower than the hint alone -- so the message was
/// squeezed to nothing and wrapped one character per line, which is a failure
/// rendered as a column of letters. Wide enough that a line of it is a phrase
/// rather than a word ladder, and narrow enough to still fit beside the hint in
/// any window worth putting two things on a line in.
pub(crate) const NOTICE_MIN_W: f32 = 180.;

/// The whole of the queue's policy, where it can be read at once and tested
/// without a window: dedupe against the back, a ceiling, oldest out first --
/// except an export's own outcome, which jumps to the front. [`Player::notify_user`]
/// is the door every message comes through; this is what the door does.
pub(crate) fn push_notice(notices: &mut std::collections::VecDeque<SharedString>, message: SharedString) {
    // A repeat of what is already at the back is dropped -- holding a key that
    // refuses would otherwise fill the queue with one sentence, and the count on
    // the bar would be a count of how long the key was held.
    if notices.back() == Some(&message) {
        return;
    }
    if notices.len() >= NOTICES_MAX {
        notices.pop_front();
    }
    // Minutes of an export land here the moment it ends, and behind two or
    // three progress lines (a proxy, a caption) queued while it ran, its own
    // result would sit unseen several dismissals deep. It is the one thing a
    // person started the export to read, so it goes to the front -- the
    // notice showing now -- rather than the back of the line.
    match is_completion(&message) {
        true => notices.push_front(message),
        false => notices.push_back(message),
    }
}

/// An export's own outcome: the one class of notice that outranks whatever
/// is already queued ([`push_notice`]). Named by the same prefixes
/// [`crate::player::export`] writes them with.
fn is_completion(message: &str) -> bool {
    message.starts_with(EXPORT_DONE) || message.starts_with("EXPORT FAILED")
}

/// The tail an open/load notice grows when the file has sound the engine cannot
/// decode: it plays perfectly, in silence, and that is the one thing the window
/// would otherwise never say (the engine's own word for it, verbatim).
pub(crate) fn audio_notice(session: &PlaybackSession) -> Option<String> {
    session
        .audio_disabled_reason()
        .map(|reason| format!(" — NO AUDIO: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The class this guards: a notice queued behind an earlier, undismissed
    /// one is never *lost* -- `push_notice` still appends it to the back --
    /// but a reader of `front()` alone (what `stance::notice_plate` read
    /// before this fix) would never see it, because nothing but a keystroke
    /// ever calls `dismiss_notice`, and most of what fills this queue (a
    /// click on a gap, a menu row, a drag) is not one. `back()` is the read
    /// that survives that: it always names the newest message, which is why
    /// the plate and the ledger's "last action" both read it now.
    ///
    /// This is a value-level check on the queue only -- this binary has no
    /// `TestAppContext`, so it cannot measure what actually painted, and this
    /// test does not claim to.
    #[test]
    fn a_notice_queued_behind_an_unread_one_is_still_reachable_at_the_back() {
        let mut notices = std::collections::VecDeque::new();
        push_notice(&mut notices, "SAVED test_h264.edith".into());
        let refusal = "the V1 clip at frame 0 is one take with the A1 clip at frame 0: \
                        closing this gap alone would pull the take out of sync — close A1's \
                        gap there too, or detach them first";
        push_notice(&mut notices, refusal.into());
        // Nobody dismissed the SAVED notice (no keystroke happened) -- it is
        // still sitting at the front, exactly the scenario that froze the
        // plate on stale text.
        assert_eq!(notices.front().unwrap().as_ref(), "SAVED test_h264.edith");
        // The refusal, remedy clause and all, is reachable at the back --
        // whole, not truncated, because `push_notice` never touches a
        // message's text.
        assert_eq!(notices.back().unwrap().as_ref(), refusal);
    }
}
