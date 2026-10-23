//! The render preview from the window: starts a render of the In-Out range or of
//! the whole timeline, shows what is cached on the ruler's bar, polls the render,
//! and stops or deletes it. The engine does the work ([`engine::render_cache`]);
//! this is the window's view of it.

use engine::project::LaneKind;
use engine::render_cache;

use crate::*;

/// One stretch of the ruler's render bar: green where a render matches the
/// timeline, amber where a clip with work to render has none yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenderBand {
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) cached: bool,
}

/// The parts of `span` no cut covers, in order. `cuts` are sorted and may overlap.
fn uncovered(span: (u32, u32), cuts: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut left = Vec::new();
    let mut at = span.0;
    for &(from, to) in cuts {
        if to <= at || from >= span.1 {
            continue;
        }
        if from > at {
            left.push((at, from));
        }
        at = at.max(to);
    }
    if at < span.1 {
        left.push((at, span.1));
    }
    left
}

impl Player {
    /// Starts a render preview of the In-Out range, or of the whole timeline
    /// (`whole`). One render at a time; the progress and the result come back
    /// through [`Player::poll_render`].
    pub(crate) fn render_preview(&mut self, whole: bool, cx: &mut Context<Self>) {
        if self.render.as_ref().is_some_and(|(job, _)| !job.is_finished()) {
            self.notify_user("A render preview is already running".into());
            return;
        }
        let Some(session) = self.session.as_ref() else {
            self.notify_user("No project is open".into());
            return;
        };
        let end = (session.timeline_duration() * self.fps).round() as u32;
        let range = match (whole, self.range) {
            (false, Some(range)) => range,
            (false, None) => {
                self.notify_user("No in and out points are set".into());
                return;
            }
            (true, _) => (0, end),
        };
        let (project, meta) = session.export_snapshot();
        match render_cache::start(project, meta, range.0, range.1) {
            Ok(job) => {
                self.render = Some((job, range));
                self.render_bands_stamp = None;
                self.refresh_render_bands();
                cx.notify();
            }
            Err(e) => self.notify_user(format!("Render preview refused: {e}").into()),
        }
    }

    /// The status strip's words while a render preview runs: `Rendering 45%`,
    /// as wide as the export's slot holds.
    pub(crate) fn render_words(&self) -> Option<String> {
        let (_, done) = self.render_progress?;
        Some(format!("Rendering {:.0}%", done * 100.))
    }

    /// Stops the render running now and removes its partial file.
    pub(crate) fn cancel_render(&mut self) {
        match self.render.as_ref().filter(|(job, _)| !job.is_finished()) {
            Some((job, _)) => {
                job.cancel();
                self.notify_user("Stopping the render".into());
            }
            None => self.notify_user("Nothing is rendering".into()),
        }
    }

    /// Deletes every render. Refused while one is running, which would otherwise
    /// write its file back after the delete.
    pub(crate) fn delete_render_files(&mut self, cx: &mut Context<Self>) {
        if self.render.as_ref().is_some_and(|(job, _)| !job.is_finished()) {
            self.notify_user("Stop the render preview before deleting its files".into());
            return;
        }
        let text = match render_cache::delete_all() {
            Ok(0) => "No render files to delete".to_string(),
            Ok(n) => format!("Deleted {n} render file(s)"),
            Err(e) => format!("Could not delete the render files: {e}"),
        };
        self.notify_user(text.into());
        self.render_bands_stamp = None;
        self.refresh_render_bands();
        cx.notify();
    }

    /// Called each repaint: settles a render that finished (with its line of
    /// text) and keeps the ruler's bar in step with the timeline.
    pub(crate) fn poll_render(&mut self, cx: &mut Context<Self>) {
        self.render_progress = self
            .render
            .as_ref()
            .filter(|(job, _)| !job.is_finished())
            .map(|(job, span)| (*span, job.progress()));
        if let Some((job, (from, to))) = self.render.take_if(|(job, _)| job.is_finished()) {
            let secs = |f: u32| format!("{:.1}s", f64::from(f) / self.fps);
            let text = match job.outcome() {
                _ if job.is_cancelled() => "Render preview stopped".to_string(),
                Some(Err(e)) => format!("Render preview failed: {e}"),
                _ => format!("Render preview ready for {} to {}", secs(from), secs(to)),
            };
            self.notify_user(text.into());
            self.render_bands_stamp = None;
            cx.notify();
        }
        self.refresh_render_bands();
    }

    /// Works out the ruler's render bar again when the timeline has changed.
    /// The key is the revision, the timeline's length and its source count: the
    /// revision alone would repeat across two projects opened in turn.
    pub(crate) fn refresh_render_bands(&mut self) {
        let Some(session) = self.session.as_ref() else {
            self.render_bands.clear();
            self.render_bands_stamp = None;
            return;
        };
        let end = (session.timeline_duration() * self.fps).round() as u32;
        let stamp = (session.revision(), end, session.sources().len());
        if self.render_bands_stamp == Some(stamp) {
            return;
        }
        let (project, meta) = session.export_snapshot();
        let cached: Vec<(u32, u32)> = render_cache::segments(&project, &meta)
            .iter()
            .map(|s| (s.from, s.to))
            .collect();
        // Video clips with work in them, merged into stretches.
        let mut work: Vec<(u32, u32)> = Vec::new();
        for lane in session.lanes().into_iter().filter(|l| l.kind == LaneKind::Video) {
            for clip in session.lane_clips(lane) {
                if render_cache::clip_has_work(clip) {
                    work.push((clip.start, clip.end()));
                }
            }
        }
        work.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (from, to) in work {
            match merged.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(to),
                _ => merged.push((from, to)),
            }
        }
        let mut bands: Vec<RenderBand> = cached
            .iter()
            .map(|&(from, to)| RenderBand { from, to, cached: true })
            .collect();
        for &span in &merged {
            for (from, to) in uncovered(span, &cached) {
                bands.push(RenderBand { from, to, cached: false });
            }
        }
        bands.sort_by_key(|b| b.from);
        self.render_bands = bands;
        self.render_bands_stamp = Some(stamp);
    }
}
