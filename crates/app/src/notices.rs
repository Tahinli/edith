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
/// without a window: dedupe against the back, a ceiling, oldest out first.
/// [`Player::notify_user`] is the door every message comes through; this is what
/// the door does.
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
    notices.push_back(message);
}

/// The tail an open/load notice grows when the file has sound the engine cannot
/// decode: it plays perfectly, in silence, and that is the one thing the window
/// would otherwise never say (the engine's own word for it, verbatim).
pub(crate) fn audio_notice(session: &PlaybackSession) -> Option<String> {
    session
        .audio_disabled_reason()
        .map(|reason| format!(" — NO AUDIO: {reason}"))
}
