//! The creator tools card: Normalize, Captions, Fillers and Voiceover. The work is
//! [`crate::creator`]'s (the engine's, shared with the MCP server); this file is the
//! window's half -- the card's state, the background job the speech tools run on with its
//! progress and cancel, and the keys.
//!
//! Normalize is synchronous (it measures, sets gains and measures again, then it is done);
//! the three that listen or speak run off the render thread -- the sound is read where the
//! session is, the model runs on a thread, and the result is put on the timeline back here
//! as one undo step.

use std::sync::Arc;

use engine::speech::{self, Filler, Hear, Job, Spoken};

use crate::creator::{self, LANGUAGES, LUFS_RANGE, LUFS_STEP, PEAK_RANGE, PEAK_STEP, Said};
use crate::player::text_field::FieldKey;
use crate::ui::kit::{NumberEdit, scrub_value};
use crate::*;

/// The four tools the card holds, in the order its tabs are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tool {
    Normalize,
    Captions,
    Fillers,
    Voiceover,
}

impl Tool {
    pub(crate) const ALL: [Tool; 4] = [Tool::Normalize, Tool::Captions, Tool::Fillers, Tool::Voiceover];

    /// This tool's place in [`Tools::status`].
    pub(crate) fn slot(self) -> usize {
        Tool::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// The tab's own name on the card: what each tool does, in plain words.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Tool::Normalize => "Normalize",
            Tool::Captions => "Captions",
            Tool::Fillers => "Filler words",
            Tool::Voiceover => "Voiceover",
        }
    }
}

/// The two numbers Normalize takes, each a slider and a number field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NumField {
    Target,
    Ceiling,
}

impl NumField {
    pub(crate) fn range(self) -> (f64, f64) {
        match self {
            NumField::Target => LUFS_RANGE,
            NumField::Ceiling => PEAK_RANGE,
        }
    }

    /// The grid a slider press or a typed value lands on.
    pub(crate) fn step(self) -> f64 {
        match self {
            NumField::Target => LUFS_STEP,
            NumField::Ceiling => PEAK_STEP,
        }
    }

    /// Units a pixel of sideways drag moves the field by.
    fn per_px(self) -> f64 {
        match self {
            NumField::Target => 0.25,
            NumField::Ceiling => 0.05,
        }
    }

    /// The text the field shows at rest.
    pub(crate) fn shown(self, v: f64) -> String {
        match self {
            NumField::Target => format!("{v:.1}").replace('-', "\u{2212}"),
            NumField::Ceiling => format!("{v:.1}").replace('-', "\u{2212}"),
        }
    }

    fn snap(self, v: f64) -> f64 {
        let (lo, hi) = self.range();
        ((v / self.step()).round() * self.step()).clamp(lo, hi)
    }
}

/// A number field being scrubbed: where the press was and the value it started from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NumDrag {
    pub(crate) field: NumField,
    pub(crate) x0: f32,
    pub(crate) v0: f64,
    pub(crate) moved: bool,
}

/// A speech or voiceover job in flight: what it is called on the card, and the shared state
/// its thread reports through and listens to.
pub(crate) struct Running {
    pub(crate) what: &'static str,
    pub(crate) job: Arc<Job>,
    /// Which run this is, so a result that arrives after Cancel (or after the card was closed
    /// and reopened) finds a different number and is dropped.
    pub(crate) id: u64,
}

/// The card's state. `Some` on `Player::tools` while it is up.
pub(crate) struct Tools {
    pub(crate) tool: Tool,
    // Normalize
    pub(crate) lufs: f64,
    pub(crate) ceiling: f64,
    pub(crate) each: bool,
    // Captions and Fillers
    pub(crate) lang: usize,
    pub(crate) model: usize,
    pub(crate) hear: usize,
    /// Listen to the sound again even though a transcript is there to read from.
    pub(crate) relisten: bool,
    // Fillers
    pub(crate) found: Vec<(Filler, bool)>,
    pub(crate) pick: usize,
    // Voiceover
    pub(crate) text: String,
    pub(crate) typing: bool,
    pub(crate) voice: usize,
    /// What each tool's last run said, in the card's own words; each tab shows its own.
    pub(crate) status: [Said; 4],
    pub(crate) running: Option<Running>,
    /// The tool the run in flight was started by: its status is the one the result writes.
    pub(crate) run_tool: Tool,
    /// The dropdown that is open, if any.
    pub(crate) open: Option<Cycle>,
    /// A Normalize number being typed (`typing` is set with it, so the keys come here).
    pub(crate) num: Option<(NumField, NumberEdit)>,
    pub(crate) drag: Option<NumDrag>,
    /// A Normalize slider being dragged.
    pub(crate) slide: Option<NumField>,
}

impl Tools {
    fn new(tool: Tool) -> Self {
        Tools {
            tool,
            lufs: creator::LUFS_PRESETS[0].1,
            ceiling: engine::normalize::TARGET_TRUE_PEAK,
            each: false,
            lang: 0,
            model: 0,
            hear: 0,
            relisten: false,
            found: Vec::new(),
            pick: 0,
            text: String::new(),
            typing: false,
            voice: 0,
            status: Default::default(),
            running: None,
            run_tool: tool,
            open: None,
            num: None,
            drag: None,
            slide: None,
        }
    }

    pub(crate) fn said(&self, tool: Tool) -> &Said {
        &self.status[tool.slot()]
    }

    pub(crate) fn set_said(&mut self, tool: Tool, said: Said) {
        self.status[tool.slot()] = said;
    }

    /// The longest voiceover text the field takes: the engine's own limit.
    pub(crate) const TEXT_CAP: usize = engine::tts::TEXT_LIMIT;

    /// One printable character into the voiceover text.
    pub(crate) fn typed(&mut self, c: char) {
        if !c.is_control() && self.text.chars().count() < Self::TEXT_CAP {
            self.text.push(c);
        }
    }
}

/// The tool a listen runs for: captions, or fillers.
fn tool_of(fillers: bool) -> Tool {
    match fillers {
        true => Tool::Fillers,
        false => Tool::Captions,
    }
}

/// `n` steps along a ring of `len`.
fn ring(at: usize, len: usize, steps: i32) -> usize {
    match len {
        0 => 0,
        _ => (at as i64 + i64::from(steps)).rem_euclid(len as i64) as usize,
    }
}

impl Player {
    /// Opens the card on `tool`. Nothing here is a clip's, so nothing is refused for want of a
    /// selection; a missing timeline is.
    pub(crate) fn open_tools(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self.session.is_none() {
            self.notify_user("No timeline — no file open".into());
            cx.notify();
            return;
        }
        // The card stays on its numbers across tools: Normalize's target is still there after a
        // look at Captions.
        let mut tools = self.tools.take().unwrap_or_else(|| Tools::new(tool));
        tools.tool = tool;
        tools.typing = false;
        // One card at a time -- `close_card`'s own rule.
        self.close_other_cards();
        self.tools = Some(tools);
        self.context_menu = None;
        self.dock_src_active = false;
        crate::ui::dock_stance::save(false);
        cx.notify();
    }

    /// Cancels whatever the card has running and drops it. What `close_card` calls.
    pub(crate) fn close_tools(&mut self) {
        if let Some(tools) = self.tools.take()
            && let Some(running) = tools.running
        {
            running.job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Switches the tab. A running job keeps running: its result lands on the card whichever
    /// tab is showing.
    pub(crate) fn tools_tab(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if let Some(t) = self.tools.as_mut() {
            t.tool = tool;
            t.typing = false;
            t.num = None;
            t.open = None;
        }
        cx.notify();
    }

    fn tools_mut(&mut self) -> Option<&mut Tools> {
        self.tools.as_mut()
    }

    /// A tool's result that worked: its card line, and the toast says the same.
    pub(crate) fn tools_say(&mut self, tool: Tool, line: String) {
        self.notify_user(line.clone().into());
        if let Some(t) = self.tools_mut() {
            t.set_said(tool, Said::plain(line));
        }
    }

    /// A tool's result that did not work: the plain sentence on its card and in the toast, the
    /// whole error in the card's tooltip.
    pub(crate) fn tools_fail(&mut self, tool: Tool, why: &str) {
        let said = creator::plain_error(why);
        self.notify_user(said.line.clone().into());
        if let Some(t) = self.tools_mut() {
            t.set_said(tool, said);
        }
    }

    // ---- normalize ----------------------------------------------------------------

    pub(crate) fn tools_set_target(&mut self, lufs: f64, cx: &mut Context<Self>) {
        self.tools_set_num(NumField::Target, lufs, cx);
    }

    /// Writes one of Normalize's numbers, on its grid and inside its range.
    pub(crate) fn tools_set_num(&mut self, field: NumField, v: f64, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            let v = field.snap(v);
            match field {
                NumField::Target => t.lufs = v,
                NumField::Ceiling => t.ceiling = v,
            }
        }
        cx.notify();
    }

    /// A press or drag on a Normalize slider: the value `along` its track.
    pub(crate) fn tools_slide(&mut self, field: NumField, along: f32, cx: &mut Context<Self>) {
        let (lo, hi) = field.range();
        self.tools_set_num(field, lo + f64::from(along.clamp(0., 1.)) * (hi - lo), cx);
    }

    fn num_value(&self, field: NumField) -> f64 {
        self.tools.as_ref().map_or(0., |t| match field {
            NumField::Target => t.lufs,
            NumField::Ceiling => t.ceiling,
        })
    }

    /// A press on a Normalize slider: the drag that follows keeps moving it.
    pub(crate) fn begin_slide(&mut self, field: NumField) {
        if let Some(t) = self.tools_mut() {
            t.slide = Some(field);
        }
    }

    /// A press on a number field: a drag that follows scrubs it, a click without one types.
    pub(crate) fn begin_num_drag(&mut self, field: NumField, x: f32) {
        let v0 = self.num_value(field);
        if let Some(t) = self.tools_mut() {
            t.drag = Some(NumDrag { field, x0: x, v0, moved: false });
        }
    }

    /// The pointer moved with a number field pressed.
    pub(crate) fn num_drag_move(&mut self, x: f32, fine: bool, cx: &mut Context<Self>) {
        let Some(d) = self.tools.as_ref().and_then(|t| t.drag) else {
            return;
        };
        let dx = x - d.x0;
        if !d.moved && dx.abs() < 3. {
            return;
        }
        let (lo, hi) = d.field.range();
        let v = scrub_value(d.v0, dx, d.field.per_px(), fine, lo, hi);
        if let Some(t) = self.tools_mut() {
            t.drag = Some(NumDrag { moved: true, ..d });
        }
        self.tools_set_num(d.field, v, cx);
    }

    /// The button came up: a press that never moved opens the field for typing.
    pub(crate) fn num_drag_end(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut()
            && t.slide.take().is_some()
        {
            cx.notify();
            return;
        }
        let Some(d) = self.tools.as_ref().and_then(|t| t.drag) else {
            return;
        };
        let shown = d.field.shown(self.num_value(d.field)).replace('\u{2212}', "-");
        if let Some(t) = self.tools_mut() {
            t.drag = None;
            if !d.moved {
                t.num = Some((d.field, NumberEdit::begin(&shown)));
                t.typing = true;
            }
        }
        cx.notify();
    }

    pub(crate) fn tools_set_each(&mut self, each: bool, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            t.each = each;
        }
        cx.notify();
    }

    /// Normalizes to the card's target. One undo step; the card says the level before and after.
    pub(crate) fn run_normalize(&mut self, cx: &mut Context<Self>) {
        let Some((each, lufs, ceiling)) = self.tools.as_ref().map(|t| (t.each, t.lufs, t.ceiling)) else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match creator::normalize(session, each, lufs, ceiling) {
            Ok(report) => {
                self.mark_dirty();
                self.tools_say(Tool::Normalize, creator::normalize_line(&report, each));
            }
            Err(e) => self.tools_fail(Tool::Normalize, &format!("Not normalized: {e}")),
        }
        cx.notify();
    }

    // ---- ducking (the mix card's strips) -------------------------------------------

    /// Ducks `lane` under `key` (the voice), or takes that duck off when it is already so.
    pub(crate) fn toggle_duck(&mut self, lane: Lane, key: Lane, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let on = session.lane_duck_list(lane).iter().any(|d| d.under == key.ord);
        let done = match on {
            true => creator::duck_off(session, lane, key),
            false => creator::duck_under(session, lane, key),
        };
        match done {
            Ok(()) => self.mark_dirty(),
            Err(e) => self.notify_user(format!("Not ducked — {e}").into()),
        }
        cx.notify();
    }

    /// Moves one number of the duck `lane` has under `key`.
    pub(crate) fn step_duck(
        &mut self,
        lane: Lane,
        key: Lane,
        field: creator::DuckField,
        steps: i32,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match creator::duck_step(session, lane, key, field, steps) {
            Ok(()) => self.mark_dirty(),
            Err(e) => self.notify_user(format!("Duck unchanged — {e}").into()),
        }
        cx.notify();
    }

    // ---- captions and fillers (speech) ----------------------------------------------

    /// The things the sound can be heard from, as the card's picker ring lists them.
    pub(crate) fn tools_hears(&self) -> Vec<Hear> {
        self.session.as_ref().map_or_else(Vec::new, creator::hear_choices)
    }

    pub(crate) fn tools_models(&self) -> Vec<&'static str> {
        speech::installed_models()
    }

    /// Opens a dropdown, or shuts it when it is the one open.
    pub(crate) fn tools_toggle_drop(&mut self, what: Cycle, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            t.open = if t.open == Some(what) { None } else { Some(what) };
        }
        cx.notify();
    }

    /// Picks row `i` of a dropdown and shuts it.
    pub(crate) fn tools_pick(&mut self, what: Cycle, i: usize, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            match what {
                Cycle::Language => t.lang = i,
                Cycle::Model => t.model = i,
                Cycle::Hear => t.hear = i,
                Cycle::Voice => t.voice = i,
            }
            t.open = None;
        }
        cx.notify();
    }

    /// Opens the folder voices are installed in, for a computer with none yet.
    pub(crate) fn open_voices_folder(&mut self, cx: &mut Context<Self>) {
        let dir = engine::tts::voices_dir();
        let _ = std::fs::create_dir_all(&dir);
        match std::process::Command::new("xdg-open").arg(&dir).stdin(std::process::Stdio::null()).spawn() {
            Ok(_) => {}
            Err(_) => self.notify_user(format!("Voices go in {}", dir.display()).into()),
        }
        cx.notify();
    }

    /// The words the transcript already holds, when they still line up with the timeline: what
    /// captions and fillers can be made from without listening again.
    pub(crate) fn transcript_words(&self) -> Option<&Vec<Spoken>> {
        self.transcript.words.as_ref().filter(|w| !w.is_empty() && !self.transcript.stale)
    }

    /// Chooses between reading the transcript and listening to the sound again.
    pub(crate) fn tools_set_relisten(&mut self, relisten: bool, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            t.relisten = relisten;
            t.open = None;
        }
        cx.notify();
    }

    /// Starts listening for captions (`fillers` false) or fillers (true). With a transcript to
    /// read from (and no wish to listen again) the answer is instant; otherwise the sound is read
    /// here and the model runs on a thread, with progress on the card and Cancel.
    pub(crate) fn start_listening(&mut self, fillers: bool, cx: &mut Context<Self>) {
        let Some(tools) = self.tools.as_ref() else {
            return;
        };
        if tools.running.is_some() {
            return;
        }
        if !tools.relisten
            && let Some(words) = self.transcript_words().cloned()
        {
            match fillers {
                true => self.found_fillers(&words),
                false => self.place_generated_captions(&words),
            }
            cx.notify();
            return;
        }
        let (lang, model_at, hear_at) = (tools.lang, tools.model, tools.hear);
        if let Err(why) = speech::available() {
            return self.listen_fail(fillers, &why, cx);
        }
        let models = self.tools_models();
        let Some(&model) = models.get(model_at).or(models.first()) else {
            return self.listen_fail(fillers, "No speech model is installed", cx);
        };
        let hears = self.tools_hears();
        let hear = hears.get(hear_at).copied().unwrap_or(Hear::Mix);
        let lang = LANGUAGES[lang.min(LANGUAGES.len() - 1)].0;
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let heard = match speech::prepare(session, hear) {
            Ok(h) => h,
            Err(why) => return self.listen_fail(fillers, &why, cx),
        };
        let job = Arc::new(Job::default());
        let id = self.tools_runs.wrapping_add(1);
        self.tools_runs = id;
        let what = if fillers { "Listening for fillers" } else { "Listening for captions" };
        if let Some(t) = self.tools_mut() {
            let tool = tool_of(fillers);
            t.running = Some(Running { what, job: job.clone(), id });
            t.run_tool = tool;
            t.set_said(tool, Said::default());
            if fillers {
                t.found.clear();
            }
        }
        let work = cx.background_executor().spawn({
            let job = job.clone();
            async move { speech::listen(heard, model, lang, fillers, &job) }
        });
        cx.spawn(async move |this, cx| {
            let heard = work.await;
            this.update(cx, |this, cx| this.finish_listening(id, fillers, heard, cx)).ok();
        })
        .detach();
        // The bar moves while the thread works: repaint until this run is over.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                let live = this
                    .update(cx, |this, cx| {
                        let live = this.tools.as_ref().and_then(|t| t.running.as_ref()).is_some_and(|r| r.id == id);
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

    /// A listen that could not start: its sentence on the tool's card and in the toast.
    fn listen_fail(&mut self, fillers: bool, why: &str, cx: &mut Context<Self>) {
        self.tools_fail(tool_of(fillers), why);
        cx.notify();
    }

    fn finish_listening(&mut self, id: u64, fillers: bool, heard: Result<Vec<Spoken>, String>, cx: &mut Context<Self>) {
        // Cancelled, or the card went and came back: not this run's result any more.
        if self.tools.as_ref().and_then(|t| t.running.as_ref()).is_none_or(|r| r.id != id) {
            return;
        }
        if let Some(t) = self.tools_mut() {
            t.running = None;
        }
        let words = match heard {
            Ok(w) => w,
            Err(e) if e == speech::CANCELLED => {
                if let Some(t) = self.tools_mut() {
                    t.set_said(tool_of(fillers), Said::plain("Cancelled"));
                }
                cx.notify();
                return;
            }
            Err(e) => {
                self.listen_fail(fillers, &e, cx);
                return;
            }
        };
        match fillers {
            true => self.found_fillers(&words),
            false => self.place_generated_captions(&words),
        }
        cx.notify();
    }

    fn found_fillers(&mut self, words: &[Spoken]) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let found = creator::fillers_in(session, words);
        if let Some(t) = self.tools_mut() {
            let line = match found.len() {
                0 => "No filler words found".into(),
                n => format!("{n} filler word{} found. Untick any to keep, then cut.", if n == 1 { "" } else { "s" }),
            };
            t.set_said(Tool::Fillers, Said::plain(line));
            t.found = found.into_iter().map(|f| (f, true)).collect();
            t.pick = 0;
        }
    }

    fn place_generated_captions(&mut self, words: &[Spoken]) {
        let project = (!self.project_path.as_os_str().is_empty()).then(|| self.project_path.clone());
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match creator::place_captions(session, words, project.as_deref()) {
            Ok((n, lane, replaced)) => {
                self.mark_dirty();
                let said = match replaced {
                    true => format!("Captions replaced ({n}) on {}.", lane.label()),
                    false => format!("{n} captions added on {}.", lane.label()),
                };
                self.tools_say(Tool::Captions, said);
            }
            Err(e) => self.tools_fail(Tool::Captions, &format!("No captions: {e}")),
        }
    }

    /// Cancels the run in flight. The thread stops at its next poll and the kill is the engine's.
    pub(crate) fn cancel_tools_job(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut()
            && let Some(running) = t.running.take()
        {
            running.job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            let tool = t.run_tool;
            t.set_said(tool, Said::plain("Cancelled"));
        }
        cx.notify();
    }

    // ---- fillers ------------------------------------------------------------------

    pub(crate) fn toggle_filler(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut()
            && let Some(row) = t.found.get_mut(i)
        {
            row.1 = !row.1;
            t.pick = i;
        }
        cx.notify();
    }

    pub(crate) fn set_all_fillers(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            t.found.iter_mut().for_each(|row| row.1 = on);
        }
        cx.notify();
    }

    /// Takes the playhead to filler `i`, for a listen before the cut.
    pub(crate) fn seek_filler(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(frame) = self.tools.as_ref().and_then(|t| t.found.get(i)).map(|(f, _)| f.start_f) else {
            return;
        };
        if let Some(t) = self.tools_mut() {
            t.pick = i;
        }
        let at = f64::from(frame) / self.fps;
        self.seek(at, cx);
    }

    /// Cuts the ticked fillers out of every track, closing each hole. One undo step.
    pub(crate) fn cut_ticked_fillers(&mut self, cx: &mut Context<Self>) {
        let ticked: Vec<Filler> = self
            .tools
            .as_ref()
            .map(|t| t.found.iter().filter(|(_, on)| *on).map(|(f, _)| f.clone()).collect())
            .unwrap_or_default();
        if ticked.is_empty() {
            if let Some(t) = self.tools_mut() {
                t.set_said(Tool::Fillers, Said::plain("Tick the filler words to cut first."));
            }
            cx.notify();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let before = session.timeline_duration();
        match creator::cut_fillers(session, &ticked) {
            Ok(n) => {
                let shorter = before - session.timeline_duration();
                self.selected.clear();
                self.reset_after_reseek();
                self.mark_dirty();
                if let Some(t) = self.tools_mut() {
                    t.found.clear();
                    t.pick = 0;
                }
                let line = format!("Cut {n} filler word{}, {} shorter.", if n == 1 { "" } else { "s" }, secs_label(shorter));
                self.tools_say(Tool::Fillers, line);
            }
            Err(e) => self.tools_fail(Tool::Fillers, &format!("Nothing cut: {e}")),
        }
        cx.notify();
    }

    // ---- voiceover ----------------------------------------------------------------

    pub(crate) fn tools_voices(&self) -> Vec<engine::tts::Voice> {
        engine::tts::list_voices(&engine::tts::voices_dir()).unwrap_or_default()
    }

    /// Speaks the card's text and places it at the playhead. The synthesis runs on a thread.
    pub(crate) fn start_voiceover(&mut self, cx: &mut Context<Self>) {
        let Some(tools) = self.tools.as_ref() else {
            return;
        };
        if tools.running.is_some() {
            return;
        }
        let (text, voice_at) = (tools.text.clone(), tools.voice);
        let voices = self.tools_voices();
        let voice = voices.get(voice_at).map(|v| v.name.clone());
        let dir = creator::voiceover_dir((!self.project_path.as_os_str().is_empty()).then_some(self.project_path.as_path()));
        let at = self.session.as_ref().map_or(0.0, |s| s.now());
        let id = self.tools_runs.wrapping_add(1);
        self.tools_runs = id;
        let job = Arc::new(Job::default());
        if let Some(t) = self.tools_mut() {
            t.typing = false;
            t.running = Some(Running { what: "Speaking", job: job.clone(), id });
            t.run_tool = Tool::Voiceover;
            t.set_said(Tool::Voiceover, Said::default());
        }
        let work = cx.background_executor().spawn(async move {
            let rendered = engine::tts::render(&dir, &text, None, voice.as_deref(), 1.0, 0.0)?;
            // The window cannot hand a stop to piper from here: a cancelled run is simply not
            // placed (checked on the way back).
            Ok::<_, String>(rendered)
        });
        cx.spawn(async move |this, cx| {
            let rendered = work.await;
            this.update(cx, |this, cx| this.finish_voiceover(id, at, rendered, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_voiceover(&mut self, id: u64, at: f64, rendered: Result<engine::tts::Rendered, String>, cx: &mut Context<Self>) {
        if self.tools.as_ref().and_then(|t| t.running.as_ref()).is_none_or(|r| r.id != id) {
            return;
        }
        if let Some(t) = self.tools_mut() {
            t.running = None;
        }
        match rendered {
            Ok(r) => match self.session.as_mut() {
                Some(session) => match creator::place_voiceover(session, &r.file, at) {
                    Ok(lane) => {
                        self.mark_dirty();
                        self.selected.clear();
                        self.reset_after_reseek();
                        let line = format!("Voiceover ({}) placed on {} at {}.", r.voice, lane.label(), creator::clock(at));
                        self.tools_say(Tool::Voiceover, line);
                    }
                    Err(e) => self.tools_fail(Tool::Voiceover, &format!("Voiceover not placed: {e}")),
                },
                None => return,
            },
            Err(e) => self.tools_fail(Tool::Voiceover, &format!("No voiceover: {e}")),
        }
        cx.notify();
    }

    pub(crate) fn begin_voiceover_text(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.tools_mut() {
            t.typing = true;
            t.num = None;
            t.open = None;
        }
        cx.notify();
    }

    /// The keyboard while the voiceover text is being typed (ahead of the palette and every
    /// chord, as the marker field is): escape leaves the field, enter speaks it, the rest is
    /// text.
    pub(crate) fn tools_type_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        key_char: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some((field, mut edit)) = self.tools.as_ref().and_then(|t| t.num.clone()) {
            let done = edit.key(key, ctrl, alt, key_char);
            let (lo, hi) = field.range();
            let typed = (done == FieldKey::Commit).then(|| edit.commit(lo, hi)).flatten();
            if let Some(v) = typed {
                self.tools_set_num(field, v, cx);
            }
            if let Some(t) = self.tools_mut() {
                match done {
                    FieldKey::Edited => t.num = Some((field, edit)),
                    FieldKey::Commit | FieldKey::Cancel => {
                        t.num = None;
                        t.typing = false;
                    }
                }
            }
            cx.notify();
            return;
        }
        match key {
            "escape" => {
                if let Some(t) = self.tools_mut() {
                    t.typing = false;
                }
            }
            "enter" => self.start_voiceover(cx),
            "backspace" => {
                if let Some(t) = self.tools_mut() {
                    t.text.pop();
                }
            }
            _ => {
                if !ctrl
                    && !alt
                    && let Some(t) = self.tools_mut()
                    && let Some(chars) = key_char
                {
                    chars.chars().for_each(|c| t.typed(c));
                }
            }
        }
        cx.notify();
    }

    // ---- keys ---------------------------------------------------------------------

    /// The card's own keys while it is up and nothing is being typed: left and right switch
    /// tab, enter runs the tool, escape cancels a running job or shuts an open list. `true` once the
    /// card is up: it owns the strokes it does not answer, like every card.
    pub(crate) fn tools_card_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(tools) = self.tools.as_ref() else {
            return false;
        };
        let (tool, running) = (tools.tool, tools.running.is_some());
        match key {
            "escape" if self.card_maximized => self.toggle_maximize(),
            "escape" if running => self.cancel_tools_job(cx),
            // A dropdown shuts first; past that there is nothing here to leave: the section
            // stays open (its head folds it).
            "escape" => {
                if let Some(t) = self.tools_mut() {
                    t.open = None;
                }
            }
            "left" | "right" => {
                let at = Tool::ALL.iter().position(|t| *t == tool).unwrap_or(0);
                let to = ring(at, Tool::ALL.len(), if key == "left" { -1 } else { 1 });
                self.tools_tab(Tool::ALL[to], cx);
            }
            "up" | "down" if tool == Tool::Fillers => {
                let by = if key == "up" { -1 } else { 1 };
                if let Some(t) = self.tools_mut() {
                    t.pick = ring(t.pick, t.found.len(), by);
                }
            }
            "space" if tool == Tool::Fillers => {
                let i = self.tools.as_ref().map_or(0, |t| t.pick);
                self.toggle_filler(i, cx);
            }
            "enter" if !running => match tool {
                Tool::Normalize => self.run_normalize(cx),
                Tool::Captions => self.start_listening(false, cx),
                Tool::Fillers => match self.tools.as_ref().is_some_and(|t| !t.found.is_empty()) {
                    true => self.cut_ticked_fillers(cx),
                    false => self.start_listening(true, cx),
                },
                Tool::Voiceover => self.begin_voiceover_text(cx),
            },
            _ => {}
        }
        cx.notify();
        true
    }
}

/// Which picker a cycle press moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cycle {
    Language,
    Model,
    Hear,
    Voice,
}
