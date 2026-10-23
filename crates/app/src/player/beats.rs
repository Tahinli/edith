//! Mark the beats: the card that listens to a sound clip and puts a comment marker on each
//! beat, so a cut can be made on the music. The listening runs on the background executor
//! the way a silence scan does; the markers land in one undo step.

use crate::export_ui::audio_half;
use crate::*;
use engine::beats::{self, Found};
use std::time::{Duration, Instant};

/// What the card's three choices mean: every beat, every second beat, every fourth beat.
pub(crate) const BEAT_EVERY: [usize; 3] = [1, 2, 4];

impl Player {
    /// Opens the mark-the-beats card on the sound of the clip the hand is on, or the one under
    /// the playhead. A picture with no sound says so and opens nothing.
    pub(crate) fn open_beats(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("Nothing to mark — no file open".into());
            cx.notify();
            return;
        };
        let anchor = self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()));
        let Some(anchor) = anchor else {
            self.notify_user("No clip is picked or under the playhead, so there is no sound to mark".into());
            cx.notify();
            return;
        };
        let (lane, idx) = audio_half(session, anchor);
        let has_sound = session.lane_clips(lane).get(idx).and_then(|clip| {
            let source = session.sources().get(clip.source)?;
            Some(!engine::is_image(&source.path))
        });
        if has_sound != Some(true) {
            self.notify_user("The clip has no sound, so it has no beats to mark".into());
            cx.notify();
            return;
        }
        self.close_other_cards();
        self.beats_open = Some((lane, idx));
        self.beats_scan = None;
        self.context_menu = None;
        cx.notify();
    }

    /// Whether the selected clip (or the sound half of a picture) has a sound to listen to.
    pub(crate) fn beats_available(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let anchor = self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()));
        let Some(anchor) = anchor else {
            return false;
        };
        let (lane, idx) = audio_half(session, anchor);
        session
            .lane_clips(lane)
            .get(idx)
            .and_then(|clip| session.sources().get(clip.source))
            .is_some_and(|source| !engine::is_image(&source.path))
    }

    /// Takes the section down. A listening that is still running lands into nothing.
    pub(crate) fn close_beats(&mut self) {
        self.beats_open = None;
        self.beats_scan = None;
    }

    /// Starts listening to the clip the card is on. The analysis is off the UI thread; the card
    /// shows its clock until it lands.
    pub(crate) fn start_beats(&mut self, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.beats_open else {
            return;
        };
        if self.beats_scan.is_some() {
            return;
        }
        let fps = self.fps;
        let found = self.session.as_ref().and_then(|session| {
            let clip = session.lane_clips(lane).get(idx)?.clone();
            let source = session.sources().get(clip.source)?.clone();
            Some((clip, source))
        });
        let Some((clip, source)) = found else {
            self.close_beats();
            self.notify_user("The clip is gone, so there is no sound to mark".into());
            cx.notify();
            return;
        };
        let every = BEAT_EVERY[self.beats_every.min(BEAT_EVERY.len() - 1)];
        let range = (f64::from(clip.in_frame) / fps, f64::from(clip.out_frame) / fps);
        self.beats_scan = Some(Instant::now());
        let task = cx.background_executor().spawn(async move {
            beats::detect(&source.path, source.audio_stream, range)
        });
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                this.land_beats(lane, idx, every, fps, landed, cx);
            })
            .ok();
        })
        .detach();
        // The card's clock: a repaint a quarter second while the listening runs, and none after.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.beats_scan.is_some()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Puts the beats the analysis found on the clip, replacing the clip's earlier beat markers,
    /// in one undo step, and says what it did.
    fn land_beats(
        &mut self,
        lane: Lane,
        idx: usize,
        every: usize,
        fps: f64,
        landed: engine::Result<Found>,
        cx: &mut Context<Self>,
    ) {
        // Closed while listening: the result is for a card nobody has now.
        if self.beats_scan.take().is_none() {
            return;
        }
        self.beats_last = Some(match &landed {
            Ok(Found::Beats { bpm, times }) => {
                let n = beats::every_nth(times, every).len();
                format!("Last run: {n} beats at {bpm:.0} BPM")
            }
            Ok(Found::NoAudio) | Ok(Found::Silent) => "Last run: no sound to mark".to_string(),
            Ok(Found::NoPulse) => "Last run: no steady beat found".to_string(),
            Err(_) => "Last run: failed".to_string(),
        });
        match landed {
            Ok(Found::Beats { bpm, times }) => {
                let Some(session) = self.session.as_mut() else {
                    cx.notify();
                    return;
                };
                let Some(clip) = session.lane_clips(lane).get(idx).cloned() else {
                    self.beats_last = Some("Last run: the clip is gone".to_string());
                    self.notify_user("The clip is gone; no markers were added".into());
                    cx.notify();
                    return;
                };
                let frames = beats::timeline_beats(&clip, fps, &beats::every_nth(&times, every));
                session.begin_batch();
                let old = beats::beat_marker_indices(session.markers(), clip.start, clip.end());
                for &i in old.iter().rev() {
                    session.remove_marker(i);
                }
                let mut refused = None;
                for &frame in &frames {
                    if let Err(e) = session.add_marker(beats::beat_marker(frame)) {
                        refused = Some(e);
                        break;
                    }
                }
                session.end_batch();
                if let Some(e) = refused {
                    self.notify_user(format!("Marking the beats failed — {e}").into());
                } else if frames.is_empty() {
                    self.notify_user("No beat falls inside this clip".into());
                } else {
                    self.notify_user_action(
                        format!("Marked {} beats at {:.0} BPM", frames.len(), bpm).into(),
                        ToastAction::Undo,
                    );
                }
            }
            Ok(Found::NoAudio) => self.notify_user("The clip has no sound, so it has no beats to mark".into()),
            Ok(Found::Silent) => self.notify_user("No beat to mark — the sound is silent".into()),
            Ok(Found::NoPulse) => {
                self.notify_user("No steady beat found in this sound to mark".into())
            }
            Err(e) => self.notify_user(format!("Marking the beats failed — {e}").into()),
        }
        cx.notify();
    }
}
