//! Multicam on the window's side: making a multicam clip from the picked library rows, switching
//! the angle at the playhead (the number keys, a click on a monitor tile), flattening one, and the
//! angle monitor -- the grid of every angle at the playhead that sits beside the program picture
//! in the viewer (never over the timeline). The model is the engine's
//! ([`engine::project`]'s multicam sequences); every change here is one undo step.

use crate::*;
use crate::ui::type_scale::{self, Typeset};
use engine::project::{Lane, MulticamUnder};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// What a monitor tile set was composed for: the clip, the frame and the edit state, so a tile
/// set is redone exactly when one of them moved.
pub(crate) type ShotKey = (Lane, usize, u32, usize, usize, usize);

/// The monitor's tiles: one picture per angle, composed off the window's thread.
#[derive(Default)]
pub(crate) struct AngleShots {
    key: Option<ShotKey>,
    images: Vec<Option<Arc<RenderImage>>>,
    busy: bool,
}

/// Angles being synced on a thread ([`Player::create_multicam_selected`]).
pub(crate) struct McamRun {
    at: f64,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<engine::MulticamPlan, String>>>>,
}

/// Longest side of a monitor tile in pixels.
const TILE_W: u32 = 384;

impl Player {
    /// The multicam clip under the playhead, if any.
    pub(crate) fn multicam_here(&self) -> Option<MulticamUnder> {
        let session = self.session.as_ref()?;
        session.multicam_under(frame_at(session.now(), self.fps))
    }

    /// The library rows picked for a multicam clip, as source indices in pick order: the
    /// ctrl-click set, or the one clicked row.
    pub(crate) fn picked_sources(&self) -> Vec<usize> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let mut rows = self.asset_picks.clone();
        if rows.is_empty() {
            rows.extend(self.selected_asset.clone());
        }
        let mut out = Vec::new();
        for (path, stream) in rows {
            let found = session
                .sources()
                .iter()
                .position(|s| s.path == path && s.audio_stream == stream)
                .or_else(|| session.sources().iter().position(|s| s.path == path));
            if let Some(i) = found
                && !out.contains(&i)
            {
                out.push(i);
            }
        }
        out
    }

    /// Ctrl- or shift-click on a library row: adds it to the picks or takes it off.
    pub(crate) fn toggle_asset_pick(&mut self, path: PathBuf, stream: usize) {
        let row = (path, stream);
        toggle_pick(&mut self.asset_picks, self.selected_asset.clone(), row.clone());
        self.selected_asset = Some(row);
    }

    /// A plain click on a row: it alone is picked.
    pub(crate) fn pick_one_asset(&mut self, path: PathBuf, stream: usize) {
        self.asset_picks.clear();
        self.selected_asset = Some((path, stream));
    }

    pub(crate) fn toggle_angle_monitor(&mut self, cx: &mut Context<Self>) {
        self.angle_monitor = !self.angle_monitor;
        self.angle_shots = AngleShots::default();
        cx.notify();
    }

    /// Syncs the picked media and makes a multicam clip of it at the playhead on V1. The cameras'
    /// sound is measured on a thread of its own, shown as a percent in the status bar
    /// ([`Self::mcam_words`]); [`Self::poll_multicam`] makes the clip -- one undo step -- when it
    /// lands, and the monitor opens so the angles are in front of the editor at once.
    pub(crate) fn create_multicam_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self.mcam_run.is_some() {
            self.notify_user("Angles are already being synced".into());
            cx.notify();
            return;
        }
        let picks = self.picked_sources();
        if picks.len() < 2 {
            self.notify_user("Multicam needs two or more media clips picked".into());
            cx.notify();
            return;
        }
        if self.lane_locked_refusal(Lane::V1) {
            cx.notify();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let at = session.now();
        let job = match session.multicam_job(&picks, engine::MulticamSync::Audio) {
            Ok(job) => job,
            Err(e) => {
                self.notify_user(format!("Not made — {e}").into());
                cx.notify();
                return;
            }
        };
        let run = McamRun {
            at,
            progress: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
        };
        let (progress, cancel, result) = (run.progress.clone(), run.cancel.clone(), run.result.clone());
        std::thread::spawn(move || {
            let r = job.run(&progress, &cancel).map_err(|e| e.to_string());
            if let Ok(mut slot) = result.lock() {
                *slot = Some(r);
            }
        });
        self.mcam_run = Some(run);
        cx.notify();
    }

    /// The status bar's words for angles being synced, `Syncing angles 40%`.
    pub(crate) fn mcam_words(&self) -> Option<String> {
        let run = self.mcam_run.as_ref()?;
        Some(format!("Syncing angles {}%", run.progress.load(Ordering::Relaxed) / 10))
    }

    /// Lands a finished sync: the clip is made at the playhead the sync started from, as one undo
    /// step, and a failure is said once.
    pub(crate) fn poll_multicam(&mut self) {
        let Some(run) = &self.mcam_run else { return };
        let Some(outcome) = run.result.lock().ok().and_then(|mut r| r.take()) else { return };
        let run = self.mcam_run.take().expect("checked above");
        let plan = match outcome {
            Ok(plan) => plan,
            Err(e) => {
                self.notify_user(format!("Not made — {e}").into());
                return;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.apply_multicam(&plan, None, Lane::V1, run.at) {
            Ok(report) => {
                let names = session.angle_names(report.made.sequence).unwrap_or_default();
                let synced = report.how.iter().filter(|h| matches!(h, engine::AngleSync::Audio { .. })).count();
                let loose = report
                    .how
                    .iter()
                    .filter(|h| matches!(h, engine::AngleSync::Weak { .. } | engine::AngleSync::NoSound))
                    .count();
                let how = if loose > 0 {
                    format!("{synced} synced by sound, {loose} aligned at the start")
                } else if synced > 0 {
                    format!("synced by sound ({synced} matched)")
                } else {
                    "aligned at the start".to_string()
                };
                self.selected.clear();
                if let Some(&first) = report.made.clips.first() {
                    self.selected.set_one(first);
                }
                self.angle_monitor = true;
                self.angle_shots = AngleShots::default();
                self.mark_dirty();
                self.notify_user(format!("Multicam clip of {} angles — {how}", names.len()).into());
            }
            Err(e) => self.notify_user(format!("Not made — {e}").into()),
        }
        self.reset_after_reseek();
    }

    /// Puts the selected multicam clip's pieces back as plain clips. One undo step.
    pub(crate) fn flatten_multicam_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let mut picks = self.marks().0;
        if picks.is_empty() {
            picks.extend(self.selected.anchor());
        }
        if let Some(under) = self.multicam_here() {
            picks.push((under.lane, under.idx));
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(&(lane, idx)) = picks.iter().find(|&&(l, i)| session.multicam_at(l, i).is_some()) else {
            self.notify_user("No multicam clip selected".into());
            cx.notify();
            return;
        };
        match session.flatten_multicam(lane, idx) {
            Ok(_) => {
                self.selected.clear();
                self.angle_shots = AngleShots::default();
                self.mark_dirty();
                self.notify_user("Multicam clip flattened to plain clips".into());
            }
            Err(e) => self.notify_user(format!("Not flattened — {e}").into()),
        }
        self.reset_after_reseek();
        cx.notify();
    }

    /// Cuts the multicam clip at the playhead and shows angle `n` (from 1) from there on. One
    /// undo step. Only live with the monitor on and a multicam clip under the playhead.
    pub(crate) fn switch_angle_here(&mut self, n: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(under) = self.multicam_here() else {
            self.notify_user("No multicam clip under the playhead".into());
            cx.notify();
            return;
        };
        if self.lane_locked_refusal(under.lane) {
            cx.notify();
            return;
        }
        if n == 0 || n > under.names.len() {
            self.notify_user(format!("This multicam clip has {} angles", under.names.len()).into());
            cx.notify();
            return;
        }
        if under.shown == n - 1 {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let at = session.now();
        match session.switch_angle_at(None, at, Some((n - 1) as u8), None) {
            Ok(_) => {
                self.mark_dirty();
                self.angle_shots = AngleShots::default();
                self.notify_user(format!("Angle {n} — {}", under.names[n - 1]).into());
            }
            Err(e) => self.notify_user(format!("Not switched — {e}").into()),
        }
        self.reset_after_reseek();
        cx.notify();
    }

    /// Composes the tiles for the frame the playhead is on, off the window's thread, when the
    /// ones held were made for something else.
    fn refresh_angle_shots(&mut self, under: &MulticamUnder, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let frame = frame_at(session.now(), self.fps);
        let key = (under.lane, under.idx, frame, session.undo_depth(), session.redo_depth(), under.shown);
        if self.angle_shots.busy || Some(key) == self.angle_shots.key {
            return;
        }
        let Some((_, views, meta)) = session.angle_views(frame) else {
            return;
        };
        self.angle_shots.busy = true;
        let work = cx.background_executor().spawn(async move {
            let w = TILE_W;
            let h = (u64::from(w) * u64::from(meta.height) / u64::from(meta.width.max(1))).max(2) as u32;
            views
                .iter()
                .map(|p| {
                    let f = engine::export::compose_at(p, &meta, frame, w, h).ok()?;
                    let buf = image::RgbaImage::from_raw(f.width, f.height, f.bgra)?;
                    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let images = work.await;
            this.update(cx, |this, cx| {
                this.angle_shots = AngleShots { key: Some(key), images, busy: false };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The angle grid (Premiere's multi-camera monitor): every angle at the playhead in rows
    /// of [`monitor_rows`] tiles, the one the clip shows ringed, a click on a tile cutting to
    /// it. Beside the program picture. With no multicam clip under the playhead there is no
    /// monitor at all and the picture has the whole row.
    pub(crate) fn angle_monitor_view(&mut self, cx: &mut Context<Self>, max_cols: usize) -> Option<AnyElement> {
        let Some(under) = self.multicam_here() else {
            // The monitor was asked for but the playhead is elsewhere: the viewer's own empty-state
            // look (a hero icon, a title, a plain line on how to make one), not a bordered box.
            use crate::ui::icons::{Icon, IconSize};
            let t = crate::ui::theme::tokens();
            return Some(
                div()
                    .id("angle-monitor")
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .child(Icon::Layers.el(IconSize::Hero, t.ink3))
                    .child(
                        div()
                            .type_style(type_scale::title())
                            .text_color(rgb(t.ink2))
                            .child("No multicam clip under the playhead"),
                    )
                    .child(
                        div()
                            .type_style(type_scale::body())
                            .text_color(rgb(t.ink3))
                            .child("To make one, pick two or more clips in Media (Ctrl-click), then right-click one and choose Create multicam."),
                    )
                    .into_any_element(),
            );
        };
        self.refresh_angle_shots(&under, cx);
        let shell = div().id("angle-monitor").flex_1().min_w(px(0.)).min_h(px(0.)).flex().flex_col().gap(px(6.));
        let n = under.names.len();
        let rows = monitor_rows_within(n, max_cols);
        let mut grid = shell;
        let mut first = 0;
        for tiles in rows {
            let mut row = div().flex_1().min_h(px(0.)).flex().gap(px(6.));
            for i in first..first + tiles {
                let active = i == under.shown;
                let image = self.angle_shots.images.get(i).cloned().flatten();
                let label = format!("{} · {}", i + 1, under.names[i]);
                row = row.child(
                    div()
                        .id(SharedString::from(format!("angle-tile-{}", i + 1)))
                        .flex_1()
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .flex()
                        .flex_col()
                        .bg(rgb(DARK_PANEL()))
                        .border_2()
                        // The live angle wears the accent ring (Premiere's multi-camera monitor);
                        // every tile has the same 2px border so the ring changes no size.
                        .border_color(if active { rgb(ACCENT_PRIMARY()) } else { rgba(DARK_SEAM()) })
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(DARK_RAISED())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.switch_angle_here(i + 1, cx)))
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .relative()
                                .bg(rgb(DARK_CANVAS()))
                                .children(image.map(letterboxed_image)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .py(px(2.))
                                .when(active, |d| d.bg(rgba(ACCENT_WASH())))
                                .text_size(px(type_scale::FLOOR_PX))
                                .text_color(rgb(if active { INK1() } else { INK2() }))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(label),
                        ),
                );
            }
            first += tiles;
            grid = grid.child(row);
        }
        Some(grid.into_any_element())
    }
}

/// The picks after a Ctrl-click on `row`. The first pick takes the row already selected along
/// with it; then the row joins the picks, or leaves them when it was one of them.
pub(crate) fn toggle_pick(picks: &mut Vec<(PathBuf, usize)>, selected: Option<(PathBuf, usize)>, row: (PathBuf, usize)) {
    if picks.is_empty() {
        picks.extend(selected);
    }
    match picks.iter().position(|r| *r == row) {
        Some(at) => {
            picks.remove(at);
        }
        None => picks.push(row),
    }
}

/// How many tiles each row of the angle monitor holds for `n` angles: one column of two for
/// two angles, 2x2 for four, three across beyond that. A short last row stretches its tiles
/// across the width, so no empty slot is ever drawn.
#[cfg(test)]
pub(crate) fn monitor_rows(n: usize) -> Vec<usize> {
    monitor_rows_within(n, 3)
}

/// [`monitor_rows`] with at most `max_cols` tiles across: the strip beside the program picture
/// is narrow, so it holds two across.
pub(crate) fn monitor_rows_within(n: usize, max_cols: usize) -> Vec<usize> {
    let cols = match n {
        0..=2 => 1,
        3..=4 => 2,
        _ => 3,
    }
    .min(max_cols.max(1));
    let mut rows = vec![cols; n / cols];
    if !n.is_multiple_of(cols) {
        rows.push(n % cols);
    }
    rows
}
