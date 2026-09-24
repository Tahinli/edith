//! The notice bar's queue.

use crate::*;

/// How many messages wait behind the one on the bar before the oldest is
/// dropped. A queue with no ceiling is a way for a stuck loop to eat the heap;
/// eight is more than a user will ever answer in a row.
pub(crate) const NOTICES_MAX: usize = 8;

/// The whole of the queue's policy, where it can be read at once and tested
/// without a window: dedupe against the back, a ceiling, oldest out first --
/// except an export's own outcome, which jumps to the front. [`Player::notify_user`]
/// is the door every message comes through; this is what the door does.
pub(crate) fn push_notice(
    notices: &mut std::collections::VecDeque<SharedString>,
    message: SharedString,
) {
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
    session.audio_disabled_reason().map(audio_tail)
}

/// [`audio_notice`]'s own tail, for a reason already read -- the one place the
/// engine's word is spelled into a line, so the open path and the mid-session
/// one cannot drift apart.
pub(crate) fn audio_tail(reason: &str) -> String {
    format!(" — NO AUDIO: {reason}")
}

/// The same fact said on its own, for a device that dies *after* a file was
/// opened: [`audio_notice`] is a tail to append to "OPENED x", and a death
/// mid-session has no line to hang off -- so it leads with the state word the
/// strip reads first, and the tail is the same one.
pub(crate) fn audio_lost_line(reason: &str) -> String {
    format!("AUDIO LOST{}", audio_tail(reason))
}

/// What a *change* in the engine's sound reason does to the notice queue, as a
/// pure step: the death line pushed when a reason arrives, the same line taken
/// back when the reason clears ([`PlaybackSession::audio_disabled_reason`]'s
/// `AUDIO_LOST` and its own re-arm). [`Player::watch_audio`] is the only caller
/// and owns the caching; this owns the queue, so a test can drive both
/// directions without a window and without a device that dies on cue.
pub(crate) fn audio_change_notice(
    notices: &mut std::collections::VecDeque<SharedString>,
    was: Option<&str>,
    now: Option<&str>,
) {
    match (was, now) {
        (_, Some(reason)) if was != Some(reason) => {
            push_notice(notices, audio_lost_line(reason).into());
        }
        // The sound is back: the line goes with it, or the strip keeps saying NO
        // AUDIO over a film that plays. Only that line -- whatever else is
        // queued is none of this step's business.
        (Some(was), None) => {
            let line = audio_lost_line(was);
            notices.retain(|notice| notice.as_ref() != line.as_str());
        }
        _ => {}
    }
}

/// The strip's own width is finite and its message is one line: what a
/// too-long line must never do is lose its *state* word, which sits at the
/// front, or stop mid-word with nothing saying it stopped (user report,
/// `OPENED he_is_not_the_only_one.mp4 — 1 subtitle track(s) in `). Every
/// message is a handful of words plus a file name now (DESIGN §8), so the one
/// part long enough to overflow the strip alone is the name -- elided through
/// its middle here, head and tail kept, before gpui's own end ellipsis
/// ([`crate::ui::stance`]) ever has to cut a word.
pub(crate) const LEDGER_WORD_MAX: usize = 28;

pub(crate) fn ledger_line(message: &str) -> String {
    message
        .split(' ')
        .map(|word| match word.chars().count() > LEDGER_WORD_MAX {
            false => word.to_string(),
            // Chars, not bytes: a name is whatever the filesystem holds.
            true => {
                let chars: Vec<char> = word.chars().collect();
                let head = LEDGER_WORD_MAX / 2 - 1;
                let tail = LEDGER_WORD_MAX - head - 1;
                format!(
                    "{}…{}",
                    chars[..head].iter().collect::<String>(),
                    chars[chars.len() - tail..].iter().collect::<String>()
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The class this guards: a notice queued behind an earlier one is never
    /// *lost* -- `push_notice` still appends it to the back -- but a reader
    /// of `front()` alone would never see it, since nothing dismisses the
    /// queue any more (the floating plate and its keystroke-dismiss went
    /// with it, user 2026-08-27). `back()` is the read that survives that:
    /// it always names the newest message, which is why the ledger's "last
    /// action" -- the one notice channel left -- reads it.
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

    /// The line the strip paints keeps its state word and never stops
    /// mid-word without a sign: a long name loses its middle, everything
    /// short is untouched, and the count at the end survives.
    #[test]
    fn a_long_name_loses_its_middle_rather_than_the_state_word() {
        assert_eq!(ledger_line("SNAP OFF"), "SNAP OFF");
        let long =
            ledger_line("OPENED he_is_not_the_only_one_at_all_here.mp4 · 1 subtitle track(s)");
        assert!(long.starts_with("OPENED "), "{long}");
        assert!(long.ends_with(" · 1 subtitle track(s)"), "{long}");
        assert!(long.contains('…'), "{long}");
        assert!(
            long.split(' ')
                .all(|w| w.chars().count() <= LEDGER_WORD_MAX),
            "{long}"
        );
    }
}
