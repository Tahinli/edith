//! The Transcript dock tab's state and doings: listen to the timeline once, then edit by
//! words. Click a word to go there, select a run and delete it, remove the fillers.
//!
//! The listening is [`engine::speech`] (the Captions and Fillers cards' path) and every cut is
//! [`speech::cut_spans`] -> `PlaybackSession::cut_regions`, the call the MCP word cuts end in,
//! across every lane so linked picture and sound stay together, as one undo step. After a cut
//! the words are re-mapped through it ([`speech::remap_words`]); an undo or redo finds the
//! words that went with that state again, and any other edit to the timeline leaves the
//! transcript stale until it is run again.

use std::sync::Arc;

use engine::speech::{self, Hear, Job, Spoken};

use crate::creator;
use crate::keymap::ActionId;
use crate::player::tools::Running;
use crate::viewport::secs_label;
use crate::*;

/// A silence this long (seconds) starts a new paragraph, as does a sentence end once a
/// paragraph has run [`PARA_WORDS`] words.
const PARA_GAP_SECS: f64 = 1.2;
const PARA_WORDS: usize = 18;
/// How many states of the transcript are kept for undo and redo to find again.
const KEEP: usize = 64;

/// What identifies a state of the timeline well enough to tell whose words these are.
type Stamp = (usize, u32);

pub(crate) struct Transcript {
    /// The words as the timeline stands (timeline frames), `None` before the first listen.
    pub(crate) words: Option<Vec<Spoken>>,
    /// The timeline state `words` belong to, and the earlier and later ones undo can reach.
    stamp: Stamp,
    states: Vec<(Stamp, Vec<Spoken>)>,
    /// True when the timeline was edited some other way since: the words no longer line up.
    pub(crate) stale: bool,
    /// The selected run: where it was started and where it reaches now (word indices).
    pub(crate) anchor: Option<usize>,
    pub(crate) head: usize,
    pub(crate) dragging: bool,
    pub(crate) search: String,
    pub(crate) search_edit: bool,
    /// The match the search is on now (a word index), once it was typed or stepped to.
    pub(crate) search_at: Option<usize>,
    pub(crate) running: Option<Running>,
    /// The speech model being downloaded: its progress and cancel, and which download it is.
    pub(crate) download: Option<(Arc<Job>, u64)>,
    pub(crate) runs: u64,
    pub(crate) status: String,
    pub(crate) scroll: ScrollHandle,
    /// The paragraph the playing word was last scrolled to, so the view moves once per paragraph.
    pub(crate) followed: std::cell::Cell<Option<usize>>,
}

impl Transcript {
    pub(crate) fn new() -> Self {
        Transcript {
            words: None,
            stamp: (0, 0),
            states: Vec::new(),
            stale: false,
            anchor: None,
            head: 0,
            dragging: false,
            search: String::new(),
            search_edit: false,
            search_at: None,
            running: None,
            download: None,
            runs: 0,
            status: String::new(),
            scroll: ScrollHandle::new(),
            followed: std::cell::Cell::new(None),
        }
    }

    /// The selected run, first to last word, when there is one.
    pub(crate) fn selection(&self) -> Option<(usize, usize)> {
        // A stale transcript has no selection: its words no longer line up with the timeline.
        if self.stale {
            return None;
        }
        let n = self.words.as_ref()?.len();
        let a = self.anchor?;
        let (lo, hi) = (a.min(self.head), a.max(self.head));
        (n > 0).then(|| (lo.min(n - 1), hi.min(n - 1)))
    }

    /// Whether word `i` matches the search box (a word that contains what was typed).
    pub(crate) fn matches(&self, i: usize) -> bool {
        let q = speech::norm(&self.search);
        !q.is_empty() && self.words.as_ref().and_then(|w| w.get(i)).is_some_and(|w| speech::norm(&w.text).contains(&q))
    }

    /// Every word index that matches the search, in order.
    pub(crate) fn match_list(&self) -> Vec<usize> {
        self.words.as_ref().map_or_else(Vec::new, |w| (0..w.len()).filter(|&i| self.matches(i)).collect())
    }

    /// Paragraphs as `(first, last)` word indices.
    pub(crate) fn paragraphs(&self, fps: f64) -> Vec<(usize, usize)> {
        let Some(words) = self.words.as_ref() else {
            return Vec::new();
        };
        let gap = (PARA_GAP_SECS * fps).round() as u32;
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (i, w) in words.iter().enumerate() {
            let breaks = match out.last() {
                None => true,
                Some(&(first, _)) => {
                    let prev = &words[i - 1];
                    w.start_f.saturating_sub(prev.end_f) >= gap
                        || (i - first >= PARA_WORDS && prev.text.trim_end().ends_with(['.', '?', '!']))
                }
            };
            match breaks {
                true => out.push((i, i)),
                false => out.last_mut().unwrap().1 = i,
            }
        }
        out
    }

    /// The word the playhead is in, or the one that just ended.
    pub(crate) fn playing_word(&self, frame: u32, fps: f64) -> Option<usize> {
        let words = self.words.as_ref()?;
        let i = words.partition_point(|w| w.start_f <= frame).checked_sub(1)?;
        let hold = words[i].end_f.max(words[i].start_f + 1) + (fps / 4.0).round() as u32;
        (frame < hold).then_some(i)
    }

    fn adopt(&mut self, stamp: Stamp, words: Vec<Spoken>) {
        self.states.retain(|(s, _)| *s != stamp);
        self.states.push((stamp, words.clone()));
        if self.states.len() > KEEP {
            self.states.remove(0);
        }
        self.words = Some(words);
        self.stamp = stamp;
        self.stale = false;
    }
}

impl Player {
    fn timeline_stamp(&self) -> Option<Stamp> {
        let s = self.session.as_ref()?;
        Some((s.undo_depth(), (s.timeline_duration() * self.fps).round() as u32))
    }

    /// Once a frame while the tab is up: undo and redo bring back the words of the state they
    /// land on; a timeline that is in no state we know leaves the transcript stale.
    pub(crate) fn sync_transcript(&mut self) {
        if !self.transcript_open || self.transcript.words.is_none() {
            return;
        }
        let Some(now) = self.timeline_stamp() else {
            return;
        };
        if now == self.transcript.stamp {
            return;
        }
        let t = &mut self.transcript;
        match t.states.iter().find(|(s, _)| *s == now) {
            Some((_, words)) => {
                t.words = Some(words.clone());
                t.stamp = now;
                t.stale = false;
            }
            None => t.stale = true,
        }
        // Whatever was selected belonged to the words before; a stale transcript keeps none.
        t.anchor = None;
        t.dragging = false;
    }

    pub(crate) fn open_transcript(&mut self, cx: &mut Context<Self>) {
        self.close_card();
        self.transcript_open = true;
        self.transcript.followed.set(None);
        cx.notify();
    }

    /// The playhead's frame.
    pub(crate) fn transcript_frame(&self) -> u32 {
        self.session.as_ref().map_or(0, |s| (s.now() * self.fps).floor().max(0.0) as u32)
    }

    // ---- listening ----------------------------------------------------------------

    /// Listens to the whole mix on a thread, with progress and Cancel on the tab.
    pub(crate) fn start_transcribing(&mut self, cx: &mut Context<Self>) {
        if self.transcript.running.is_some() {
            return;
        }
        let fail = |this: &mut Self, why: String, cx: &mut Context<Self>| {
            this.transcript.status = why.clone();
            this.notify_user(why.into());
            cx.notify();
        };
        if let Err(why) = speech::available() {
            return fail(self, why, cx);
        }
        let Some(model) = speech::pick_model(|m| speech::check_model(m).is_ok()) else {
            return fail(self, "No speech model is installed".into(), cx);
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let heard = match speech::prepare(session, Hear::Mix) {
            Ok(h) => h,
            Err(why) => return fail(self, why, cx),
        };
        let job = Arc::new(Job::default());
        let id = self.transcript.runs.wrapping_add(1);
        self.transcript.runs = id;
        self.transcript.running = Some(Running { what: "Transcribing", job: job.clone(), id });
        self.transcript.status.clear();
        let work = cx.background_executor().spawn({
            let job = job.clone();
            async move { speech::listen(heard, model, "auto", true, &job) }
        });
        cx.spawn(async move |this, cx| {
            let heard = work.await;
            this.update(cx, |this, cx| this.finish_transcribing(id, heard, cx)).ok();
        })
        .detach();
        // The bar moves while the thread works: repaint until this run is over.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                let live = this
                    .update(cx, |this, cx| {
                        let live = this.transcript.running.as_ref().is_some_and(|r| r.id == id);
                        cx.notify();
                        live
                    })
                    .unwrap_or(false);
                if !live {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_transcribing(&mut self, id: u64, heard: Result<Vec<Spoken>, String>, cx: &mut Context<Self>) {
        if self.transcript.running.as_ref().is_none_or(|r| r.id != id) {
            return;
        }
        self.transcript.running = None;
        match heard {
            Ok(words) if words.is_empty() => self.transcript.status = "No speech found".into(),
            Ok(words) => {
                if let Some(stamp) = self.timeline_stamp() {
                    self.transcript.states.clear();
                    self.transcript.anchor = None;
                    self.transcript.adopt(stamp, words);
                }
            }
            Err(e) if e == speech::CANCELLED => self.transcript.status = "Cancelled".into(),
            Err(e) => {
                self.transcript.status = e.clone();
                self.notify_user(e.into());
            }
        }
        cx.notify();
    }

    /// Fetches the speech model (the base one) with progress, off the UI thread. Nothing is
    /// started while one is already coming.
    pub(crate) fn start_model_download(&mut self, cx: &mut Context<Self>) {
        if self.transcript.download.is_some() {
            return;
        }
        let job = Arc::new(Job::default());
        let id = self.transcript.runs.wrapping_add(1);
        self.transcript.runs = id;
        self.transcript.download = Some((job.clone(), id));
        self.transcript.status.clear();
        let work = cx.background_executor().spawn({
            let job = job.clone();
            async move { speech::download_model("base", &job) }
        });
        cx.spawn(async move |this, cx| {
            let done = work.await;
            this.update(cx, |this, cx| {
                if this.transcript.download.as_ref().is_none_or(|(_, d)| *d != id) {
                    return;
                }
                this.transcript.download = None;
                match done {
                    Ok(()) => this.notify_user("Speech model installed — Transcribe is ready".into()),
                    Err(e) if e == speech::CANCELLED => this.transcript.status = "Download cancelled".into(),
                    Err(e) => this.transcript.status = format!("Download failed: {e}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        // The bar moves while the thread works: repaint until this download is over.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                let live = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.transcript.download.as_ref().is_some_and(|(_, d)| *d == id)
                    })
                    .unwrap_or(false);
                if !live {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn cancel_model_download(&mut self, cx: &mut Context<Self>) {
        if let Some((job, _)) = self.transcript.download.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.transcript.status = "Download cancelled".into();
        }
        cx.notify();
    }

    /// Opens the folder the speech models live in with the desktop's own file manager.
    pub(crate) fn open_models_folder(&mut self, cx: &mut Context<Self>) {
        let dir = speech::models_dir();
        let _ = std::fs::create_dir_all(&dir);
        match std::process::Command::new("xdg-open").arg(&dir).stdin(std::process::Stdio::null()).spawn() {
            Ok(_) => {}
            Err(_) => self.notify_user(format!("Models go in {}", dir.display()).into()),
        }
        cx.notify();
    }

    pub(crate) fn cancel_transcribing(&mut self, cx: &mut Context<Self>) {
        if let Some(running) = self.transcript.running.take() {
            running.job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.transcript.status = "Cancelled".into();
        }
        cx.notify();
    }

    // ---- reading ------------------------------------------------------------------

    /// A press on word `i`: it starts a selection (shift extends the last one) and, unextended,
    /// takes the playhead to the word.
    pub(crate) fn press_word(&mut self, i: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(frame) = self.transcript.words.as_ref().and_then(|w| w.get(i)).map(|w| w.start_f) else {
            return;
        };
        let t = &mut self.transcript;
        match (extend, t.anchor) {
            (true, Some(_)) => t.head = i,
            _ => {
                t.anchor = Some(i);
                t.head = i;
                t.dragging = true;
                let at = f64::from(frame) / self.fps;
                self.seek(at, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn drag_word(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.transcript.dragging && self.transcript.head != i {
            self.transcript.head = i;
            cx.notify();
        }
    }

    pub(crate) fn release_word(&mut self, cx: &mut Context<Self>) {
        if self.transcript.dragging {
            self.transcript.dragging = false;
            cx.notify();
        }
    }

    /// Moves to the next (or previous) match: selects the word, takes the playhead there and
    /// scrolls its paragraph into view. The first step lands on the first match after the playhead
    /// (before it, going back); stepping past either end wraps round.
    pub(crate) fn step_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let frame = self.transcript_frame();
        let list = self.transcript.match_list();
        let Some(words) = self.transcript.words.as_ref() else {
            return;
        };
        let pos = match self.transcript.search_at.and_then(|c| list.iter().position(|&i| i == c)) {
            Some(p) if forward => (p + 1) % list.len(),
            Some(p) => (p + list.len() - 1) % list.len(),
            None if list.is_empty() => return,
            None if forward => list.iter().position(|&i| words[i].start_f > frame).unwrap_or(0),
            None => list.iter().rposition(|&i| words[i].start_f < frame).unwrap_or(list.len() - 1),
        };
        let i = list[pos];
        let secs = f64::from(words[i].start_f) / self.fps;
        self.transcript.search_at = Some(i);
        self.transcript.anchor = Some(i);
        self.transcript.head = i;
        self.scroll_transcript_to(i);
        self.seek(secs, cx);
    }

    /// The query changed: the current match is the first one, scrolled into view (the playhead
    /// and the selection stay where they are while typing).
    pub(crate) fn search_changed(&mut self) {
        let first = self.transcript.match_list().first().copied();
        self.transcript.search_at = first;
        if let Some(i) = first {
            self.scroll_transcript_to(i);
        }
    }

    /// Scrolls the paragraph holding word `i` into view.
    fn scroll_transcript_to(&self, i: usize) {
        if let Some(p) = self.transcript.paragraphs(self.fps).iter().position(|&(a, b)| (a..=b).contains(&i)) {
            self.transcript.scroll.scroll_to_item(p);
            self.transcript.followed.set(Some(p));
        }
    }

    /// Empties the search box, and with it the matches.
    pub(crate) fn clear_search(&mut self) {
        self.transcript.search.clear();
        self.transcript.search_at = None;
    }

    // ---- editing ------------------------------------------------------------------

    /// Cuts the selected words out of every lane, closing the hole: one undo step.
    pub(crate) fn delete_selected_words(&mut self, cx: &mut Context<Self>) {
        let Some((lo, hi)) = self.transcript.selection() else {
            return;
        };
        if self.transcript.stale {
            return;
        }
        let Some(words) = self.transcript.words.clone() else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let end = (session.timeline_duration() * self.fps).round() as u32;
        let pad = (0.06 * self.fps).round() as u32;
        let Some(span) = speech::word_span(&words, lo, hi, pad, end) else {
            return;
        };
        let lanes = session.lanes();
        let before = session.timeline_duration();
        let n = hi - lo + 1;
        let line = match speech::cut_spans(session, &[span], &lanes) {
            Ok(regions) => {
                let shorter = before - session.timeline_duration();
                let kept = speech::remap_words(&words, &regions);
                self.finish_word_edit(kept);
                format!(
                    "{n} word{} cut — {} shorter · undo {}",
                    if n == 1 { "" } else { "s" },
                    secs_label(shorter),
                    self.keymap.display(ActionId::Undo)
                )
            }
            Err(e) => format!("Nothing cut — {e}"),
        };
        self.notify_user(line.into());
        cx.notify();
    }

    /// Cuts every filler word out of every lane: one undo step.
    pub(crate) fn remove_transcript_fillers(&mut self, cx: &mut Context<Self>) {
        let Some(words) = self.transcript.words.clone() else {
            return;
        };
        if self.transcript.stale {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let found = creator::fillers_in(session, &words);
        if found.is_empty() {
            self.notify_user("No filler words".into());
            return cx.notify();
        }
        let before = session.timeline_duration();
        let line = match creator::cut_fillers_regions(session, &found) {
            Ok(regions) => {
                let shorter = before - session.timeline_duration();
                let n = found.len();
                self.finish_word_edit(speech::remap_words(&words, &regions));
                format!(
                    "{n} filler{} cut — {} shorter · undo {}",
                    if n == 1 { "" } else { "s" },
                    secs_label(shorter),
                    self.keymap.display(ActionId::Undo)
                )
            }
            Err(e) => format!("Nothing cut — {e}"),
        };
        self.notify_user(line.into());
        cx.notify();
    }

    /// What follows a cut: the words re-mapped become the transcript, the picture reseeks.
    fn finish_word_edit(&mut self, kept: Vec<Spoken>) {
        self.selected.clear();
        self.reset_after_reseek();
        self.mark_dirty();
        self.transcript.anchor = None;
        if let Some(stamp) = self.timeline_stamp() {
            self.transcript.adopt(stamp, kept);
        }
    }

    // ---- keys ---------------------------------------------------------------------

    /// The tab's own keys, asked ahead of the keymap. The search field owns the keyboard once it
    /// was clicked into; otherwise the tab answers only while it has focus, and only the strokes
    /// that edit its words (Delete, Backspace, Esc, arrows over a selection, Ctrl+A): Space, S and
    /// the rest still reach the timeline. `true` when the stroke was taken.
    pub(crate) fn transcript_key(&mut self, key: &str, ctrl: bool, shift: bool, alt: bool, focused: bool, cx: &mut Context<Self>) -> bool {
        if !self.transcript_open {
            return false;
        }
        // Typing ends when the focus goes elsewhere (a click on the timeline).
        if self.transcript.search_edit && !focused {
            self.transcript.search_edit = false;
        }
        if self.transcript.search_edit {
            match key {
                // Esc clears what was typed; with nothing typed it leaves the field.
                "escape" if self.transcript.search.is_empty() => self.transcript.search_edit = false,
                "escape" => self.clear_search(),
                "enter" => self.step_match(!shift, cx),
                "backspace" => {
                    self.transcript.search.pop();
                    self.search_changed();
                }
                // Any other stroke is the search box's too: it is not passed on to the words or the keymap.
                _ => {
                    if let Some(c) = crate::menus::typed(key).filter(|_| !ctrl && !alt) {
                        self.transcript.search.push(c);
                        self.search_changed();
                    }
                }
            }
            cx.notify();
            return true;
        }
        if !focused || alt {
            return false;
        }
        let n = self.transcript.words.as_ref().map_or(0, Vec::len);
        let sel = self.transcript.selection();
        let taken = match (key, ctrl) {
            ("delete" | "backspace", false) => {
                self.delete_selected_words(cx);
                true
            }
            ("a", true) if n > 0 => {
                self.transcript.anchor = Some(0);
                self.transcript.head = n - 1;
                true
            }
            ("escape", false) if !self.transcript.search.is_empty() => {
                self.clear_search();
                true
            }
            ("escape", false) if sel.is_some() => {
                self.transcript.anchor = None;
                true
            }
            ("left" | "right", false) if sel.is_some() && n > 0 => {
                let step = if key == "left" { -1i64 } else { 1 };
                let to = (self.transcript.head as i64 + step).clamp(0, n as i64 - 1) as usize;
                self.transcript.head = to;
                if !shift {
                    self.transcript.anchor = Some(to);
                    let frame = self.transcript.words.as_ref().map_or(0, |w| w[to].start_f);
                    self.seek(f64::from(frame) / self.fps, cx);
                }
                true
            }
            _ => false,
        };
        if taken {
            cx.notify();
        }
        taken
    }
}
