//! The colour card's tools beside its sliders, and the viewer's compare view they are checked
//! against: Auto (a correction read off the picture as shot), Looks (built-in and saved grades,
//! each one undo step) and Compare (the picture as shot beside the graded one, preview only).
//!
//! The picture as shot is the timeline composed with every grade taken off
//! ([`engine::export::compose_ungraded_at`]). It is read in the background, one at a time, so a
//! playing timeline keeps its own pace and the compare view catches up when the picture settles.

use crate::player::text_field::{FieldKey, TextField};
use crate::viewer_view::Compare;
use crate::*;
use engine::color::ColorParams;
use gpui::RenderImage;
use std::sync::Arc;

/// The look last applied to a clip: which clip, its name, and the grade it left. The Looks row
/// shows the name while the grade is still that one, and "Custom" once a slider or wheel moves.
/// Kept in session state only, so the project format does not change. The clip is keyed by its
/// source and in point, which a move leaves alone.
#[derive(Clone, Debug)]
pub(crate) struct LookMark {
    pub(crate) clip: (usize, u32),
    pub(crate) name: String,
    pub(crate) params: ColorParams,
}

/// The most characters a look name may hold (the engine's own limit).
pub(crate) const LOOK_NAME_MAX: usize = 60;

/// The widest a picture is read at for Auto, in pixels: the statistics do not need more.
const READ_W: u32 = 640;

/// `(w, h)` scaled to at most [`READ_W`] wide, keeping the aspect.
fn read_size(w: u32, h: u32) -> (u32, u32) {
    if w <= READ_W || w == 0 {
        return (w.max(2), h.max(2));
    }
    (READ_W, (u64::from(h) * u64::from(READ_W) / u64::from(w)).max(2) as u32)
}

impl Player {
    /// View > Compare grade, and the card's Compare row: which compare view is on.
    pub(crate) fn set_compare(&mut self, mode: Compare, cx: &mut Context<Self>) {
        self.compare = mode;
        self.before_at = None;
        cx.notify();
    }

    /// Reads the picture as shot at the frame on screen, in the background, when the compare view
    /// is on and that frame is not in hand. One read runs at a time; when it lands, a frame that
    /// moved on in the meantime is read again.
    pub(crate) fn refresh_before(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((at, w, h)) = self.shown_frame else {
            return;
        };
        if self.compare == Compare::Off || self.preview_session.is_some() || self.before_busy || self.before_at == Some(at) {
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let (project, meta) = session.export_snapshot();
        self.before_busy = true;
        let read = cx.background_executor().spawn(async move {
            // The frame comes back at the size it was composed at, which can differ by a pixel
            // from the one asked for; the image is built from that size, or it is never shown.
            let r = engine::export::compose_ungraded_at(&project, &meta, at, w, h);
            r.ok().map(|f| (f.width, f.height, f.bgra))
        });
        cx.spawn_in(window, async move |this, cx| {
            let frame = read.await;
            this.update_in(cx, |this, window, cx| {
                this.before_busy = false;
                if let Some(buf) = frame.and_then(|(w, h, b)| image::RgbaImage::from_raw(w, h, b)) {
                    let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                    if let Some(old) = this.before.replace(next) {
                        let _ = window.drop_image(old);
                    }
                    this.before_at = Some(at);
                }
                this.refresh_before(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The colour card's Auto, and Edit > Auto colour: reads a correction off the card's clip
    /// as shot at its middle frame and writes it onto the grade in one undo step. The card opens
    /// on the playhead's clip first when none is open.
    pub(crate) fn auto_colour(&mut self, cx: &mut Context<Self>) {
        if self.color_open.is_none() {
            self.open_color(cx);
        }
        let Some((lane, idx)) = self.color_open else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let keyed = session.color_keys_of(lane, idx).len();
        if keyed > 0 {
            self.notify_user("Auto colour is off while colour keys grade this clip; the keys are kept".into());
            cx.notify();
            return;
        }
        let Some(clip) = session.lane_clips(lane).get(idx) else {
            return;
        };
        let mid = clip.start + clip.len() / 2;
        let (project, meta) = session.export_snapshot();
        let (w, h) = read_size(meta.width, meta.height);
        let read = cx.background_executor().spawn(async move {
            let f = engine::export::compose_ungraded_at(&project, &meta, mid, w, h).ok()?;
            let rgb: Vec<u8> = f.bgra.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
            engine::auto_color::analyse(&rgb, f.width as usize, f.height as usize)
        });
        cx.spawn(async move |this, cx| {
            let fix = read.await;
            this.update(cx, |this, cx| {
                let Some(fix) = fix else {
                    this.notify_user("Auto colour: there is no picture to read at this clip".into());
                    cx.notify();
                    return;
                };
                let Some(session) = this.session.as_ref() else {
                    return;
                };
                let next = fix.apply(session.color_of(lane, idx).copied().unwrap_or_default());
                this.named_step("Auto colour", |this| this.write_color_now(next, false, cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The names the Looks dropdown lists: the built-ins, then the user's saved looks.
    pub(crate) fn look_names(&self) -> Vec<String> {
        engine::looks::builtins()
            .iter()
            .map(|(n, _)| n.to_string())
            .chain(self.saved_looks.iter().cloned())
            .collect()
    }

    /// The card's clip, by its source and in point (see [`LookMark`]).
    fn look_key(&self) -> Option<(usize, u32)> {
        let (lane, idx) = self.color_open?;
        let clip = self.session.as_ref()?.lane_clips(lane).get(idx)?;
        Some((clip.source, clip.in_frame))
    }

    /// What the Looks row shows: the look applied to this clip while its grade is still that one,
    /// "Custom" once it has moved, and "Choose a look" before any look went on this clip.
    pub(crate) fn look_label(&self) -> String {
        match (&self.look_mark, self.look_key()) {
            (Some(mark), Some(clip)) if mark.clip == clip && self.color_params() == mark.params => mark.name.clone(),
            (Some(mark), Some(clip)) if mark.clip == clip => "Custom".to_owned(),
            _ => "Choose a look".to_owned(),
        }
    }

    /// Opens or closes the Looks dropdown; opening re-reads the saved looks.
    pub(crate) fn toggle_look_list(&mut self, cx: &mut Context<Self>) {
        self.look_open = !self.look_open;
        if self.look_open {
            self.saved_looks = engine::looks::saved_names();
        }
        cx.notify();
    }

    /// Puts the look called `name` on the card's clip, replacing its grade, as one undo step.
    pub(crate) fn apply_look(&mut self, name: &str, cx: &mut Context<Self>) {
        self.look_open = false;
        if self.color_open.is_none() {
            return;
        }
        match engine::looks::load(name) {
            Ok(params) => {
                let label = format!("Look {name}");
                self.named_step(&label, |this| this.write_color_now(params, false, cx));
                // The grade as the clip now holds it, so the name holds until something moves.
                self.look_mark = self.look_key().map(|clip| LookMark { clip, name: name.to_owned(), params: self.color_params() });
            }
            Err(why) => self.notify_user(why.into()),
        }
        cx.notify();
    }

    /// "Save current as look…": opens the name field under the Looks row.
    pub(crate) fn begin_look_save(&mut self, cx: &mut Context<Self>) {
        self.look_open = false;
        self.look_name = Some(TextField::selected(String::new()));
        cx.notify();
    }

    /// Keys for the look name field: Enter saves, Escape cancels, the rest types.
    pub(crate) fn look_name_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        let Some(field) = self.look_name.as_mut() else {
            return;
        };
        match field.key(key, ctrl, alt, key_char, LOOK_NAME_MAX) {
            FieldKey::Cancel => self.look_name = None,
            FieldKey::Commit => self.commit_look_save(cx),
            FieldKey::Edited => {}
        }
        cx.notify();
    }

    /// Saves the card's grade under the typed name. A name that will not do is said in a note
    /// and the field stays open with what was typed.
    pub(crate) fn commit_look_save(&mut self, cx: &mut Context<Self>) {
        let Some(field) = &self.look_name else {
            return;
        };
        let name = field.text.clone();
        let params: ColorParams = self.color_params();
        match engine::looks::save(&name, &params) {
            Ok(_) => {
                self.look_name = None;
                self.saved_looks = engine::looks::saved_names();
                self.notify_user(format!("Saved look {}", name.trim()).into());
            }
            Err(why) => self.notify_user(why.into()),
        }
        cx.notify();
    }
}
