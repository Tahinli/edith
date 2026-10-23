//! Voice-over recording: the Record voice-over sheet's logic. A take runs the engine's
//! recorder while the timeline plays from the start point, muted unless the sheet says
//! to hear it. Stop places the take on its lane as one undo step. The sheet is drawn by
//! `ui::project_dialogs`; the take's file is kept in the project's media folder.

use std::time::Duration;

use engine::pulse;
use engine::recorder::{Recorder, Take};

use super::project_dialogs::{Dialog, RecordVoiceover};
use crate::*;

/// The meter's reading in silence, in dBFS.
pub(crate) const SILENT_DB: f32 = engine::recorder::SILENCE_DB;

/// A take in progress: what Stop needs to place it and to put the output back.
pub(crate) struct InProgress {
    pub(crate) recorder: Recorder,
    /// Timeline seconds the take starts at: the playhead when Record was pressed.
    pub(crate) start: f64,
    /// The audio lane it lands on; `None` makes a new one.
    pub(crate) lane: Option<Lane>,
    /// The mute the output had before the take, which Stop restores.
    pub(crate) was_muted: bool,
    /// Whether the take started the timeline playing, so Stop pauses it again.
    pub(crate) started_playback: bool,
    /// The take's own seconds at the moment the timeline first moved past `start`. The
    /// recorder opens before the seek and the audio restart, so the take's first seconds
    /// are that wait: placement drops them, and the voice lines up with the picture.
    pub(crate) play_origin: Option<f64>,
}

impl Player {
    /// Whether a take is running. The sheet stays open for it, and its meter is refreshed.
    pub(crate) fn record_running(&self) -> bool {
        matches!(&self.proj_dialog, Some(Dialog::Record(d)) if d.take.is_some())
    }

    /// The audio lanes, in the order the timeline lists them.
    pub(crate) fn audio_lanes(&self) -> Vec<Lane> {
        self.session
            .as_ref()
            .map(|s| s.lanes().into_iter().filter(|l| l.kind == LaneKind::Audio).collect())
            .unwrap_or_default()
    }

    /// Timeline > Record voice-over: the sheet opens with the inputs the audio server lists.
    pub(crate) fn open_record_voiceover(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self.session.is_none() {
            self.notify_user("Open a project to record into".into());
            cx.notify();
            return;
        }
        self.close_card();
        let inputs = pulse::list_sources().unwrap_or_default();
        self.proj_dialog = Some(Dialog::Record(RecordVoiceover {
            inputs,
            input: None,
            lane: None,
            hear: false,
            take: None,
            error: None,
        }));
        cx.notify();
    }

    /// The input row: segment 0 is the server's default, then each input listed.
    pub(crate) fn record_pick_input(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Some(Dialog::Record(d)) = &mut self.proj_dialog
            && d.take.is_none()
        {
            d.input = at.checked_sub(1).filter(|i| *i < d.inputs.len());
            d.error = None;
            cx.notify();
        }
    }

    /// The lane row: the audio lanes in order, then a new one (which is `None`).
    pub(crate) fn record_pick_lane(&mut self, at: usize, cx: &mut Context<Self>) {
        let lane = self.audio_lanes().get(at).copied();
        if let Some(Dialog::Record(d)) = &mut self.proj_dialog
            && d.take.is_none()
        {
            d.lane = lane;
            cx.notify();
        }
    }

    /// The hear switch. During a take it changes the output at once.
    pub(crate) fn record_toggle_hear(&mut self, cx: &mut Context<Self>) {
        let (hear, live) = match &mut self.proj_dialog {
            Some(Dialog::Record(d)) => {
                d.hear = !d.hear;
                (d.hear, d.take.as_ref().map(|t| t.was_muted))
            }
            _ => return,
        };
        if let Some(was_muted) = live {
            self.volume.muted = was_muted || !hear;
            self.apply_volume();
        }
        cx.notify();
    }

    /// Record: starts the take at the playhead, mutes the output unless heard, and plays
    /// the timeline from there, so the picture runs along with the voice.
    pub(crate) fn record_start(&mut self, cx: &mut Context<Self>) {
        let (input, lane, hear) = match &self.proj_dialog {
            Some(Dialog::Record(d)) if d.take.is_none() => {
                (d.input.and_then(|i| d.inputs.get(i)).map(|s| s.name.clone()), d.lane, d.hear)
            }
            _ => return,
        };
        if self.project_path.as_os_str().is_empty() {
            self.record_error("Save the project first: the take is kept in a folder beside it".into(), cx);
            return;
        }
        let Some(start) = self.session.as_ref().map(|s| s.now()) else {
            self.record_error("Open a project to record into".into(), cx);
            return;
        };
        let folder = engine::collect::default_folder(&self.project_path);
        let recorder = match Recorder::start(&folder, input.as_deref()) {
            Ok(recorder) => recorder,
            Err(e) => {
                self.record_error(e, cx);
                return;
            }
        };
        let was_muted = self.volume.muted;
        self.volume.muted = was_muted || !hear;
        self.apply_volume();
        self.seek(start, cx);
        let started_playback = !self.active_session().is_some_and(|s| s.is_playing());
        if started_playback {
            self.toggle_or_restart(cx);
        }
        if let Some(Dialog::Record(d)) = &mut self.proj_dialog {
            d.error = None;
            d.take = Some(InProgress {
                recorder,
                start,
                lane,
                was_muted,
                started_playback,
                play_origin: None,
            });
        }
        self.record_refresh(cx);
    }

    /// Stop: ends the take, pauses the timeline if the take started it, restores the
    /// output, and places the take at its start point as one undo step.
    pub(crate) fn record_stop(&mut self, cx: &mut Context<Self>) {
        let take = match &mut self.proj_dialog {
            Some(Dialog::Record(d)) => d.take.take(),
            _ => None,
        };
        let Some(take) = take else {
            return;
        };
        // Past the end of the timeline the transport reads as ended, so a toggle here would
        // restart it from the top: pause the clock directly.
        if take.started_playback
            && let Some(session) = self.active_session_mut()
            && session.is_playing()
        {
            session.pause();
        }
        self.volume.muted = take.was_muted;
        self.apply_volume();
        match take.recorder.stop() {
            Ok(done) => match self.place_take(&done, take.lane, take.start, take.play_origin) {
                Ok(lane) => {
                    self.proj_dialog = None;
                    self.reset_after_reseek();
                    let undo = self.keymap.display(ActionId::Undo);
                    self.notify_user(format!("Voice-over placed on {} · undo {undo}", lane.label()).into());
                }
                Err(e) => self.record_error(e, cx),
            },
            Err(e) => self.record_error(format!("The take failed: {e}"), cx),
        }
        cx.notify();
    }

    /// Imports the take and places it, in one batch so that it is one undo step.
    /// Returns the lane it landed on.
    fn place_take(&mut self, take: &Take, lane: Option<Lane>, start: f64, origin: Option<f64>) -> Result<Lane, String> {
        let fps = self.fps;
        let session = self.session.as_mut().ok_or("Open a project to record into")?;
        let mark = session.step_mark();
        session.begin_batch();
        let source = match session.import(&take.path) {
            Ok(n) => n,
            Err(e) => {
                session.end_batch();
                return Err(format!("{}: refused: {e}", take.path.display()));
            }
        };
        let lane = lane.unwrap_or_else(|| session.add_lane(LaneKind::Audio));
        let frames = session.file_frames(&take.path);
        // Clip frames are timeline frames, so the origin is converted at the project's rate.
        let skip = origin.map_or(0, |t| (t * fps).round() as u32).min(frames);
        if skip >= frames {
            session.end_batch();
            session.undo();
            return Err("The timeline had not moved on from the start point, so there is no take to place".into());
        }
        let placed = session.place_at(lane, start, Clip::source_range(source, skip, frames));
        session.end_batch();
        if !placed {
            session.undo();
            return Err("The take would not go there: the lane is locked or the spot is taken".into());
        }
        session.name_step(mark, "Record voice-over");
        Ok(lane)
    }

    /// Shows a reason under the sheet's fields.
    pub(crate) fn record_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(Dialog::Record(d)) = &mut self.proj_dialog {
            d.error = Some(message);
        }
        cx.notify();
    }

    /// Notes, once, the take's own time at which the timeline moved past the start point.
    pub(crate) fn record_track(&mut self) {
        let running = self.record_running();
        let now = self.active_session().map(|s| s.now());
        // Past the last frame the picture is done, but the take is not: the clock goes on.
        if let Some(session) = self.active_session_mut()
            && running
            && session.is_eos()
        {
            session.run_past_end();
        }
        if let Some(Dialog::Record(d)) = &mut self.proj_dialog
            && let Some(take) = &mut d.take
            && take.play_origin.is_none()
            && now.is_some_and(|n| n > take.start)
        {
            take.play_origin = Some(take.recorder.status().elapsed_secs);
        }
    }

    /// Repaints the sheet while a take runs, so the meter, the clock and the take's
    /// start follow the timeline.
    pub(crate) fn record_refresh(&self, cx: &mut Context<Self>) {
        cx.notify();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(40)).await;
                let live = this
                    .update(cx, |this, cx| {
                        this.record_track();
                        cx.notify();
                        this.record_running()
                    })
                    .unwrap_or(false);
                if !live {
                    break;
                }
            }
        })
        .detach();
    }
}
