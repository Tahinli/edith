//! The notice queue, and the toasts it draws (DESIGN §8).

use crate::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::ops::Deref;
use std::path::PathBuf;

/// `n` of a thing in words: "1 silence", "2 silences". One door, so no
/// message ships a "(s)" or a "1 silences".
pub(crate) fn count_noun(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// How many messages wait in the queue before the oldest is dropped. A queue
/// with no ceiling is a way for a stuck loop to eat the heap; eight is more than
/// a user will ever answer in a row. Only the newest is drawn, in the status bar,
/// and a new message replaces it, so in practice the queue holds one line plus
/// any questions still waiting for an answer.
pub(crate) const NOTICES_MAX: usize = 8;

/// How long an info or warn toast stays before it hides itself (DESIGN §8).
/// Errors have no such timer: they name a remedy and wait for a ✕.
pub(crate) const TOAST_HIDE: Duration = Duration::from_secs(5);

/// How many buttons one toast can carry. Two is what an export's outcome needs
/// (Play, and Show in folder); a third would crowd the message off the bar.
pub(crate) const TOAST_ACTIONS_MAX: usize = 2;

/// How loud a toast is. The tone picks the stripe colour and whether the toast
/// times out at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Info,
    Warn,
    Err,
}

impl Tone {
    /// The tone is read off the words a message already opens with: a refusal or
    /// a failure is an error, a no-op or an empty result is a warning, and
    /// everything else -- a save, a snap -- is information.
    pub(crate) fn of(message: &str) -> Tone {
        // The keywords are matched as whole words in capitals whatever the case
        // of the message, so a sentence-case line reads the same as the old
        // ALL-CAPS one, and "piano" does not read as "NO".
        let words: Vec<String> = message
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_uppercase)
            .collect();
        let has = |word: &str| words.iter().any(|w| w == word);
        let has_pair =
            |first: &str, second: &str| words.windows(2).any(|p| p[0] == first && p[1] == second);
        if has("FAILED")
            || has("ERROR")
            || has("REFUSED")
            || has("CANNOT")
            || has_pair("COULD", "NOT")
        {
            Tone::Err
        } else if has("NOTHING") || has("NO") || has("EMPTY") {
            Tone::Warn
        } else {
            Tone::Info
        }
    }

    /// Whether a toast of this tone hides on its own. Errors do not: a failure
    /// that vanished before it was read is the failure nobody learns about.
    pub(crate) fn auto_hides(self) -> bool {
        self != Tone::Err
    }
}

/// What a toast's button does. The action is the toast's own: the button acts on
/// the thing the message is about, and the toast goes with the click.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToastAction {
    /// Takes back the last timeline edit, as `ctrl+z` does. The delete and lift
    /// toasts in `timeline_edit.rs` offer it.
    Undo,
    /// Reveals a written file in the file manager.
    ShowInFolder(PathBuf),
    /// Opens a written file in the system's default player.
    Play(PathBuf),
    /// Unlocks the lane an edit was refused on (DESIGN §7). The refusal names
    /// the lane, so the button can name it too.
    Unlock(Lane),
    /// Reloads the open project from disk, dropping the edits the window
    /// holds. Offered when the project file changed under unsaved edits.
    Reload,
    /// Dismisses the question and keeps the window's edits. The file stays as
    /// the other side wrote it until the next save.
    KeepMine,
    /// Takes an autosave's unsaved edits over the saved project, as one undo
    /// step. Offered when a project opens with a newer autosave beside it.
    Recover,
    /// Deletes the autosave that was offered for recovery.
    Discard,
    /// Saves the open project, then opens the one that was asked for.
    SaveThenOpen(PathBuf),
    /// Opens the one that was asked for and lets this project's unsaved edits go.
    OpenDiscarding(PathBuf),
}

/// One notice on screen or waiting behind the ones on screen. `Deref`s to its
/// text, so a reader that only wants the words reads them as it always did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Toast {
    /// Unique for the life of the process. A hide timer names the toast it was
    /// started for, so a timer that fires late finds nothing with that id and
    /// cannot hide a newer toast that happens to say the same thing.
    pub(crate) id: u64,
    pub(crate) text: SharedString,
    pub(crate) tone: Tone,
    /// The buttons, in the order they are drawn. At most [`TOAST_ACTIONS_MAX`].
    pub(crate) actions: Vec<ToastAction>,
    /// Whether this toast's hide timer has been started. Set the first frame
    /// the toast is drawn, which is where the timer can be spawned from.
    pub(crate) timer_armed: Cell<bool>,
    /// Stays until its button is pressed, whatever its tone. A question the
    /// person must answer (the recovery offer) is this: a 5 s hide is a question
    /// that disappears unanswered, and the edits it guards go with it.
    pub(crate) sticky: bool,
}

impl Toast {
    /// A toast whose tone is read off its words and that has no action.
    pub(crate) fn new(text: SharedString) -> Toast {
        let tone = Tone::of(&text);
        Toast::with(text, tone, Vec::new(), false)
    }

    fn with(text: SharedString, tone: Tone, mut actions: Vec<ToastAction>, sticky: bool) -> Toast {
        // A process-wide counter, not a per-queue one: a timer must be able to
        // tell its own toast from one that was pushed later, whatever queue
        // either lives in.
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        actions.truncate(TOAST_ACTIONS_MAX);
        Toast {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            text,
            tone,
            actions,
            timer_armed: Cell::new(false),
            sticky,
        }
    }

    /// Whether this toast's hide timer runs at all: an info or warn toast that
    /// is not sticky. Errors and sticky toasts wait for a button or the ✕.
    pub(crate) fn hides(&self) -> bool {
        self.tone.auto_hides() && !self.sticky
    }
}

impl Deref for Toast {
    type Target = SharedString;

    fn deref(&self) -> &SharedString {
        &self.text
    }
}

/// The whole of the queue's policy, where it can be read at once and tested
/// without a window: dedupe against the back, a ceiling, oldest out first.
/// [`Player::notify_user`] is the door every message comes through; this is
/// what the door does for a message with no action.
pub(crate) fn push_notice(notices: &mut VecDeque<Toast>, message: SharedString) {
    let tone = Tone::of(&message);
    push_toast(notices, message, tone, Vec::new());
}

/// [`push_notice`] with its buttons attached, for the door that takes them.
pub(crate) fn push_toast(
    notices: &mut VecDeque<Toast>,
    message: SharedString,
    tone: Tone,
    actions: Vec<ToastAction>,
) {
    push_toast_with(notices, message, tone, actions, false);
}

/// [`push_toast`] for a question that stays on screen until it is answered
/// ([`Toast::sticky`]).
pub(crate) fn push_sticky_toast(
    notices: &mut VecDeque<Toast>,
    message: SharedString,
    tone: Tone,
    actions: Vec<ToastAction>,
) {
    push_toast_with(notices, message, tone, actions, true);
}

fn push_toast_with(
    notices: &mut VecDeque<Toast>,
    message: SharedString,
    tone: Tone,
    actions: Vec<ToastAction>,
    sticky: bool,
) {
    // A repeat of what is already at the back is dropped -- holding a key that
    // refuses would otherwise fill the queue with one sentence, and the toasts
    // would be a count of how long the key was held.
    if notices.back().is_some_and(|toast| toast.text == message) {
        return;
    }
    // One slot: the newest message replaces the one on screen, so an old failure
    // never lingers under a newer line, and a success replaces the failure it
    // follows. A question that waits for its answer is kept beside it.
    notices.retain(|toast| toast.sticky);
    if notices.len() >= NOTICES_MAX {
        // The oldest goes, but never an unanswered question while anything else
        // could go instead.
        match notices.iter().position(|toast| !toast.sticky) {
            Some(at) => {
                notices.remove(at);
            }
            None => {
                notices.pop_front();
            }
        }
    }
    notices.push_back(Toast::with(message, tone, actions, sticky));
}

/// The toast the status bar draws: the newest sticky one (a question still
/// waiting for its answer), else the newest notice of all.
pub(crate) fn current_toast(notices: &VecDeque<Toast>) -> Option<&Toast> {
    notices
        .iter()
        .rev()
        .find(|toast| toast.sticky)
        .or_else(|| notices.back())
}

/// Takes one toast out of the queue by id. `true` when there was one to take.
/// Every way a toast leaves -- its timer, its ✕, its action -- comes through
/// here, and matching on the id is what keeps a late timer from taking a newer
/// toast with the same words.
pub(crate) fn remove_toast(notices: &mut VecDeque<Toast>, id: u64) -> bool {
    let before = notices.len();
    notices.retain(|toast| toast.id != id);
    notices.len() != before
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
    format!(" — no audio: {reason}")
}

/// The same fact said on its own, for a device that dies *after* a file was
/// opened: [`audio_notice`] is a tail to append to "OPENED x", and a death
/// mid-session has no line to hang off -- so it leads with the state word the
/// strip reads first, and the tail is the same one.
pub(crate) fn audio_lost_line(reason: &str) -> String {
    format!("Audio lost{}", audio_tail(reason))
}

/// A decode failure that arrives *after* a file was opened, said on its own
/// line: the same shape as [`audio_lost_line`] and for the same reason -- a
/// death mid-session has no open line to hang a tail off, so it leads with the
/// state word the strip reads first.
///
/// The picture twin of the sound line above, and it exists because a decode
/// failure used to reach nobody at all: the worker printed it to stderr and
/// dropped the channel, and a closed channel is what a finished film looks like
/// from the app's side. A file with a damaged middle froze on its last good
/// picture and the transport said ENDED. The reason is the engine's own words,
/// verbatim -- the app spells none of them.
pub(crate) fn decode_failed_line(reason: &str) -> String {
    format!("Decode failed — {reason}")
}

/// What a frame of a reason watch decides, as a pure value: the cache's own
/// session generation against the frame's says whether the two are even talking
/// about the same session, and only then what the reason did.
///
/// One type for every reason the engine states, not one per source: the sound
/// device and the decode worker both write a string the app has to notice
/// changed, and the thing that decides is the same comparison in both cases.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReasonStep {
    /// The cache describes the session *before* this one
    /// (`Player::session_swapped`): it is seeded and nothing is announced -- a
    /// preview that opens or closes is not a device that came back.
    Seed,
    /// The same session's reason arrived, or changed to a new one.
    Push,
    /// ...and the same session's reason is gone: the line it earned is retracted.
    Clear,
    /// The ordinary frame.
    Nothing,
}

/// [`Player::watch_audio`]'s and [`Player::watch_decode`]'s whole decision,
/// pure: `cached_gen` is the session
/// generation the cache describes, `session_gen` the one the frame is serving, and
/// two reasons are what the engine says the sound is (`None` for sound that is
/// up).
///
/// The generation is what keeps another session's reason out of this session's
/// cache: `pump` serves the preview while one is up and the timeline otherwise,
/// so a flip reads a reason that was never about the cached session at all -- a
/// live device on one side and a dead one on the other is not a change, it is
/// two different questions.
pub(crate) fn reason_step(
    cached_gen: u64,
    session_gen: u64,
    was: Option<&str>,
    now: Option<&str>,
) -> ReasonStep {
    if cached_gen != session_gen {
        return ReasonStep::Seed;
    }
    match (was, now) {
        (Some(was), Some(now)) if was == now => ReasonStep::Nothing,
        (_, Some(_)) => ReasonStep::Push,
        (Some(_), None) => ReasonStep::Clear,
        (None, None) => ReasonStep::Nothing,
    }
}

/// What a *change* in the engine's sound reason does to the notice queue, as a
/// pure step: the line pushed when a reason arrives, the same line taken back
/// when the reason clears. [`Player::watch_audio`] is the only caller and owns
/// the caching; this owns the queue, so a test can drive both directions without
/// a window and without a device that dies on cue.
///
/// The reason is the engine's to write, and the engine's writer is the wave
/// branch's: `tick`'s dead branch sets the lost-device reason and `rearm_audio`
/// clears it (its own test observes both). This side is the reader and never
/// spells the engine's words, so the end-to-end flip is asserted on the merged
/// tree while this lane drives the same step through the seam below -- on this
/// lane's base engine no path flips the reason at all.
pub(crate) fn audio_change_notice(
    notices: &mut VecDeque<Toast>,
    was: Option<&str>,
    now: Option<&str>,
) {
    match (was, now) {
        (_, Some(reason)) if was != Some(reason) => {
            // A lost device is an error: the film plays in silence until it is
            // fixed, and the toast stays until someone has read it.
            push_toast(
                notices,
                audio_lost_line(reason).into(),
                Tone::Err,
                Vec::new(),
            );
        }
        // The sound is back: the line goes with it, or the strip keeps saying NO
        // AUDIO over a film that plays. Only that line -- whatever else is
        // queued is none of this step's business.
        (Some(was), None) => {
            let line = audio_lost_line(was);
            notices.retain(|notice| notice.text.as_ref() != line.as_str());
        }
        _ => {}
    }
}

/// What a *change* in the engine's **decode** reason does to the notice queue:
/// the line pushed when a reason arrives, the same line taken back when the
/// reason clears. [`audio_change_notice`]'s twin, step for step, because the
/// thing that changed is the same kind of thing -- a reason the engine started
/// or stopped stating between two frames.
///
/// The retraction is what keeps this from becoming noise: a decode that fails,
/// is seeked away from and succeeds has to take its line back, or the strip
/// keeps saying DECODE FAILED over a film that plays.
pub(crate) fn decode_change_notice(
    notices: &mut VecDeque<Toast>,
    was: Option<&str>,
    now: Option<&str>,
) {
    match (was, now) {
        (_, Some(reason)) if was != Some(reason) => {
            push_toast(
                notices,
                decode_failed_line(reason).into(),
                Tone::Err,
                Vec::new(),
            );
        }
        (Some(was), None) => {
            let line = decode_failed_line(was);
            notices.retain(|notice| notice.text.as_ref() != line.as_str());
        }
        _ => {}
    }
}

/// A toast's line is one line: what a too-long line must never do is lose its
/// *state* word, which sits at the front, or stop mid-word with nothing saying
/// it stopped (user report, `OPENED he_is_not_the_only_one.mp4 — 1 subtitle
/// track(s) in `). Every message is a handful of words plus a file name, so the
/// one part long enough to overflow alone is the name -- elided through its
/// middle here, head and tail kept, before gpui's own end ellipsis
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

    fn texts(notices: &VecDeque<Toast>) -> Vec<&str> {
        notices.iter().map(|toast| toast.text.as_ref()).collect()
    }

    /// The class this guards: a notice queued behind an earlier one is never
    /// *lost* -- `push_notice` still appends it to the back -- and the toast
    /// status bar draws the newest, so the newest message is the
    /// one a reader of `back()` always reaches.
    ///
    /// This is a value-level check on the queue only -- this binary has no
    /// `TestAppContext`, so it cannot measure what actually painted.
    #[test]
    fn a_notice_queued_behind_an_unread_one_is_still_reachable_at_the_back() {
        let mut notices = VecDeque::new();
        push_notice(&mut notices, "SAVED test_h264.edith".into());
        let refusal = "Can't close this gap: it would put A1 out of sync.";
        push_notice(&mut notices, refusal.into());
        // One slot: the refusal replaces the SAVED toast on screen, and it is
        // reachable whole -- not truncated, because `push_notice` never
        // touches a message's text.
        assert_eq!(notices.len(), 1);
        assert_eq!(notices.back().unwrap().text.as_ref(), refusal);
    }

    /// A repeat of what is already on screen adds nothing, so holding a key
    /// that refuses cannot keep re-announcing one sentence. A repeat after a
    /// different message is a new toast, and it replaces that one.
    #[test]
    fn a_repeat_of_the_back_is_dropped_and_a_repeat_after_another_is_not() {
        let mut notices = VecDeque::new();
        push_notice(&mut notices, "SNAP ON".into());
        push_notice(&mut notices, "SNAP ON".into());
        assert_eq!(texts(&notices), vec!["SNAP ON"]);
        push_notice(&mut notices, "SNAP OFF".into());
        assert_eq!(texts(&notices), vec!["SNAP OFF"]);
        push_notice(&mut notices, "SNAP ON".into());
        assert_eq!(texts(&notices), vec!["SNAP ON"]);
    }

    /// The queue has a ceiling, and the oldest message is the one that goes.
    #[test]
    fn a_sticky_question_never_hides_and_is_drawn_over_newer_notices() {
        let mut notices = VecDeque::new();
        push_sticky_toast(
            &mut notices,
            "Unsaved edits from 3 minutes ago were found".into(),
            Tone::Info,
            vec![ToastAction::Recover, ToastAction::Discard],
        );
        assert!(!notices.back().unwrap().hides(), "a question waits for its answer");
        push_notice(&mut notices, "Snapped to 1.0s".into());
        assert!(notices.back().unwrap().hides(), "an ordinary notice still hides");
        assert_eq!(
            current_toast(&notices).map(|t| t.text.as_ref()),
            Some("Unsaved edits from 3 minutes ago were found"),
            "the question stays on the bar over a notice pushed after it"
        );
    }

    /// The question is kept beside the one ordinary slot: no notice pushed
    /// after it can drop it.
    #[test]
    fn a_sticky_question_survives_newer_notices() {
        let mut notices = VecDeque::new();
        push_sticky_toast(&mut notices, "Unsaved edits were found".into(), Tone::Info, Vec::new());
        for n in 0..NOTICES_MAX + 3 {
            push_notice(&mut notices, format!("NOTE {n}").into());
        }
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().any(|t| t.sticky), "the question is the one never dropped");
        assert_eq!(notices.back().unwrap().text.as_ref(), format!("NOTE {}", NOTICES_MAX + 2));
    }

    #[test]
    fn with_no_question_the_bar_shows_the_newest_notice() {
        let mut notices = VecDeque::new();
        push_notice(&mut notices, "A".into());
        push_notice(&mut notices, "B".into());
        assert_eq!(current_toast(&notices).map(|t| t.text.as_ref()), Some("B"));
        assert!(current_toast(&VecDeque::new()).is_none());
    }

    /// One slot: however many notices arrive, only the newest is kept.
    #[test]
    fn the_newest_notice_replaces_the_one_before_it() {
        let mut notices = VecDeque::new();
        for n in 0..NOTICES_MAX + 2 {
            push_notice(&mut notices, format!("NOTE {n}").into());
        }
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices.back().unwrap().text.as_ref(),
            format!("NOTE {}", NOTICES_MAX + 1)
        );
    }

    /// Every toast has its own id, so a timer started for one cannot hide
    /// another -- including one that says exactly the same thing.
    #[test]
    fn a_stale_timer_cannot_hide_a_newer_toast_with_the_same_words() {
        let mut notices = VecDeque::new();
        push_notice(&mut notices, "SNAP ON".into());
        let first = notices.back().unwrap().id;
        assert!(remove_toast(&mut notices, first));
        push_notice(&mut notices, "SNAP ON".into());
        let second = notices.back().unwrap().id;
        assert_ne!(first, second);
        assert!(
            !remove_toast(&mut notices, first),
            "the old timer found something"
        );
        assert_eq!(notices.len(), 1);
        assert!(remove_toast(&mut notices, second));
        assert!(notices.is_empty());
    }

    /// The tone is read off the words the way the colour always was: a failure
    /// or refusal is an error, a no-op is a warning, and the rest is information.
    #[test]
    fn the_tone_is_read_off_the_words() {
        assert_eq!(Tone::of("SCAN FAILED: no audio"), Tone::Err);
        assert_eq!(Tone::of("the clip REFUSED to move"), Tone::Err);
        assert_eq!(Tone::of("Nothing detached — not grouped"), Tone::Warn);
        assert_eq!(Tone::of("SNAP ON"), Tone::Info);
        assert_eq!(
            Tone::of(&format!("{}out.mp4", crate::EXPORT_DONE)),
            Tone::Info
        );
    }

    /// The same words in sentence case read the same tone: the sweep to sentence
    /// case must not move a warning or an error to information.
    #[test]
    fn the_tone_is_read_off_the_words_in_any_case() {
        assert_eq!(Tone::of("Scan failed: no audio"), Tone::Err);
        assert_eq!(Tone::of("The clip refused to move"), Tone::Err);
        assert_eq!(Tone::of("Cannot move the clip"), Tone::Err);
        assert_eq!(Tone::of("Nothing detached: not grouped"), Tone::Warn);
        assert_eq!(
            Tone::of("Nothing added: this file has no playable stream"),
            Tone::Warn
        );
        assert_eq!(Tone::of("Snap on"), Tone::Info);
        assert_eq!(Tone::of("Clip deleted"), Tone::Info);
    }

    /// A keyword counts only as a whole word: "piano" is not "NO", and
    /// "nothing" inside a longer word is not the warning.
    #[test]
    fn the_tone_keywords_match_whole_words_only() {
        assert_eq!(Tone::of("Playing the piano track"), Tone::Info);
        assert_eq!(Tone::of("Piano sample saved"), Tone::Info);
        assert_eq!(Tone::of("Renamed the piano clip"), Tone::Info);
        assert_eq!(Tone::of("No audio on this clip"), Tone::Warn);
        assert_eq!(Tone::of("Could not open the file"), Tone::Err);
    }

    /// Info and warn time out; errors stay until they are dismissed. This is
    /// the rule the stack's timer is built on, so it is pinned where a change
    /// to the tone would have to show up.
    #[test]
    fn only_errors_stay_on_screen_until_dismissed() {
        assert!(Tone::Info.auto_hides());
        assert!(Tone::Warn.auto_hides());
        assert!(!Tone::Err.auto_hides());
        assert_eq!(Toast::new("SNAP ON".into()).tone, Tone::Info);
        assert!(!Toast::new("Decode failed — x".into()).tone.auto_hides());
    }

    /// The engine losing the sound is an error, and getting it back takes the
    /// same line away again -- and nothing else in the queue.
    #[test]
    fn a_lost_sound_is_an_error_and_its_return_retracts_only_its_line() {
        // A question stays on screen whatever else arrives, so it is the
        // line that must survive the retraction.
        let mut notices = VecDeque::new();
        push_sticky_toast(&mut notices, "Unsaved edits were found".into(), Tone::Info, Vec::new());
        audio_change_notice(&mut notices, None, Some("device gone"));
        let lost = audio_lost_line("device gone");
        assert_eq!(notices.back().unwrap().text.as_ref(), lost);
        assert_eq!(notices.back().unwrap().tone, Tone::Err);
        audio_change_notice(&mut notices, Some("device gone"), None);
        assert_eq!(texts(&notices), vec!["Unsaved edits were found"]);
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

    /// The delete and lift toasts carry Undo as their action, read as
    /// information (so they hide on their own, DESIGN §8), and are removed by
    /// id the way the button removes them before it acts.
    #[test]
    fn a_delete_toast_carries_undo_and_goes_by_its_id() {
        let mut notices = VecDeque::new();
        push_toast(
            &mut notices,
            "Clip deleted".into(),
            Tone::of("Clip deleted"),
            vec![ToastAction::Undo],
        );
        let toast = notices.back().unwrap();
        assert_eq!(toast.actions, [ToastAction::Undo]);
        assert_eq!(toast.tone, Tone::Info);
        assert!(toast.tone.auto_hides());
        let id = toast.id;
        assert!(remove_toast(&mut notices, id));
        assert!(notices.is_empty());
    }

    /// An export's outcome offers Play and Show in folder, in that order, and
    /// a toast keeps at most two buttons however many it is handed.
    #[test]
    fn a_toast_keeps_up_to_two_buttons_in_the_order_given() {
        let path = PathBuf::from("/tmp/out.mp4");
        let mut notices = VecDeque::new();
        push_toast(
            &mut notices,
            "Export done → out.mp4".into(),
            Tone::Info,
            vec![
                ToastAction::Play(path.clone()),
                ToastAction::ShowInFolder(path.clone()),
            ],
        );
        assert_eq!(
            notices.back().unwrap().actions,
            [
                ToastAction::Play(path.clone()),
                ToastAction::ShowInFolder(path.clone()),
            ]
        );
        push_toast(
            &mut notices,
            "three buttons".into(),
            Tone::Info,
            vec![
                ToastAction::Undo,
                ToastAction::Unlock(Lane::V1),
                ToastAction::ShowInFolder(path),
            ],
        );
        assert_eq!(notices.back().unwrap().actions.len(), TOAST_ACTIONS_MAX);
    }

    /// A message with no button carries none, and the export's own toast is
    /// still a toast that hides on its own rather than waiting for a ✕.
    #[test]
    fn a_plain_notice_has_no_buttons() {
        let mut notices = VecDeque::new();
        push_notice(&mut notices, "Clip moved".into());
        let toast = notices.back().unwrap();
        assert!(toast.actions.is_empty());
        assert!(toast.tone.auto_hides());
    }
}
