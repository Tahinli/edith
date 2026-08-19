//! The picture: what is drawn over it and what is read off it.

use crate::*;
use std::path::{Path, PathBuf};

impl Player {
    /// The one thing that says a preview is up rather than the timeline: a
    /// banner over the top of the picture, and a click of its own -- the
    /// keyboard has `esc`, and a hand with only a mouse needs a way out too.
    pub(crate) fn preview_badge(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.preview_session.is_some().then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .p(px(4.))
                .bg(rgba(SUB_SHADE()))
                .text_size(px(11.))
                .text_color(rgb(FG_PRIMARY()))
                .child("PREVIEW — not on the timeline")
                .child(
                    div()
                        .id("preview-stop")
                        .px(px(6.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .bg(rgb(BG_RAISED()))
                        .hover(|s| s.bg(rgb(BG_HOVER())))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_preview(cx)))
                        .child("Stop (esc)"),
                )
        })
    }

    /// The frame on screen right now, written to a PNG: `image` and its
    /// `as_bytes` are gpui's own cached copy of what was last handed to the
    /// atlas ([`Player::pump`]), so this reads no decoder and races nothing --
    /// works during preview or on the timeline, whichever is showing.
    pub(crate) fn take_screenshot(&mut self, cx: &mut Context<Self>) {
        let no_frame = || "NO FRAME TO SAVE — nothing is showing yet".to_string();
        let Some(image) = self.image.clone() else {
            self.notify_user(no_frame().into());
            cx.notify();
            return;
        };
        let Some(bytes) = image.as_bytes(0) else {
            self.notify_user(no_frame().into());
            cx.notify();
            return;
        };
        let size = image.size(0);
        let stem = self
            .active_session()
            .and_then(|s| s.sources().first())
            .map_or_else(|| self.name.to_string(), |s| file_name(&s.path));
        let stem = std::path::Path::new(&stem)
            .file_stem()
            .map_or(stem.clone(), |s| s.to_string_lossy().into_owned());
        let tc = timecode(
            self.active_session().map_or(0., PlaybackSession::now),
            self.active_fps(),
        )
        .replace(':', "-");
        let text = match save_screenshot(bytes, size.width.0 as u32, size.height.0 as u32, &stem, &tc)
        {
            Ok(path) => format!("SAVED {}", path.display()),
            Err(e) => format!("SCREENSHOT FAILED: {e}"),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// The cues the *subtitle lanes* put on screen at `at`, over the picture and
    /// nothing else: bottom-centred where every player puts them, white on a
    /// plate so the film underneath cannot swallow them, and each cue its own
    /// plate so two at one moment stack rather than run together.
    ///
    /// What is drawn is what was *placed*: a track walked out of a file lands in
    /// the palette and nothing more, exactly as a picture or a song does, and it
    /// reaches the screen when a placement of it sits under the playhead
    /// ([`PlaybackSession::sub_lane_cues`], the same map the export writes the
    /// file with -- so what is read here is what the file will say). **One**
    /// lane draws, the one that is shown ([`Player::active_sub_lane`]): a
    /// picture carrying every lane's words at once is unreadable at three lanes
    /// and nonsense at two hundred, which is the same one-track-at-a-time every
    /// player offers. What an export writes is untouched by this -- every lane
    /// on the timeline becomes a subtitle track of the file.
    ///
    /// `None` -- no element at all -- while the toggle is off, with nothing
    /// placed, and between cues: the picture is what this window is for, and a
    /// permanent empty band across it would be in the way of exactly that.
    ///
    /// A cue off a PGS track is a *picture* and not a line
    /// ([`engine::subtitle::CueImage`]), and is drawn as one: the disc's whole
    /// canvas fitted over the picture region exactly as the picture itself is,
    /// which puts every cue where the disc put it relative to its own frame.
    ///
    /// corner-cut: exact only while the canvas and the encode are the same shape
    /// -- a 16:9 canvas over a 2.39:1 encode fits to the region's height and
    /// the film to its width, so a cue sits a little low on a scope film. The
    /// upgrade path is the picture's own rect, which wants `VideoMeta`'s aspect
    /// and the measured bounds rather than the shared `Contain`.
    pub(crate) fn subtitle_overlay(
        &mut self,
        at: f64,
        window: &mut Window,
    ) -> Option<impl IntoElement + use<>> {
        // One way out, and it lets the drawn picture go on the way: the toggle
        // going off, the file closing and the gap between two cues are the same
        // "nothing on screen", and an 8 MB atlas tile may not survive any of
        // them (an early return above this leaked one per toggle-off).
        // One lane's cues and never a walk of the lanes: the map is asked of the
        // shown lane alone, so two hundred subtitle lanes cost this frame what
        // one does.
        let shown: Option<(Lane, Vec<engine::subtitle::Cue>)> = self
            .active_sub_lane()
            .filter(|_| self.subs_on)
            .and_then(|lane| {
                let cues = self.session.as_ref()?.sub_lane_cues(lane);
                let now: Vec<_> = cues_at(&cues, at).into_iter().cloned().collect();
                (!now.is_empty()).then_some((lane, now))
            });
        let Some((lane, cues)) = shown else {
            self.drop_sub_image(window);
            return None;
        };
        // The first picture cue up, decoded once and kept: two bitmap cues at
        // one moment is a thing PGS composes into one display set, so there is
        // never a second picture to stack under the first on one lane -- and one
        // lane is all there is here.
        let picture = cues
            .iter()
            .find_map(|cue| Some((cue.start_us, cue.image.as_ref()?)))
            .and_then(|(start_us, image)| self.sub_picture(lane, start_us, image, window));
        // A picture is fitted onto the whole region and a plate hangs off the
        // bottom of it, and a track is one or the other -- so they are two
        // shapes and not one with the parts switched off.
        // What the transient bars along the bottom edge are taking up right now:
        // both shapes step up over it rather than be drawn under it.
        let bars = f32::from(self.notice_h.get());
        if let Some(image) = picture {
            // A *flex* box with the canvas as its one growing item: a percentage
            // size (`size_full`) inside an absolutely placed box has nothing to
            // be a percentage of and lays the picture out to nothing, while a
            // flex item is sized by the box itself. Fitted the way the picture
            // above it is -- `Contain` over the same box -- so a canvas of the
            // picture's own shape lands exactly on it.
            // Slid up by the bars' height and not *shrunk* by it: the top and
            // bottom insets cancel, so the box keeps the region's height and the
            // canvas keeps the fit (and so the scale) it had with nothing
            // hanging there -- a shorter box would refit the disc's canvas and
            // move every cue on it, not just the one over the bar. The picture
            // region clips (`overflow_hidden`), and what leaves the top of a
            // subtitle canvas is transparent.
            return Some(
                div()
                    .absolute()
                    .top(px(-bars))
                    .bottom(px(bars))
                    .left_0()
                    .right_0()
                    .flex()
                    .child(img(image).flex_1().h_full().object_fit(gpui::ObjectFit::Contain)),
            );
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(sub_bottom(bars)))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                // The plate takes no click: the picture behind it is still the
                // drop target the whole window is.
                //
                // In the lane's own order: two cues at one moment -- a sign and
                // a line of dialogue -- stack, the first on the bottom line
                // where a single cue sits.
                .children(cues.into_iter().filter(|c| c.image.is_none()).map(|cue| {
                    div()
                        .max_w(relative(0.9))
                        .px(px(6.))
                        .rounded(px(3.))
                        .bg(rgba(SUB_SHADE()))
                        .text_size(px(SUB_TEXT))
                        .text_color(rgb(SUB_FG()))
                        .text_align(TextAlign::Center)
                        // A line of the cue is a line on screen: the break the
                        // parser kept is not whitespace to be re-flowed. What a
                        // *long* line does is wrap inside its own div, which is
                        // what the width cap above is for.
                        .children(
                            cue.text
                                .split('\n')
                                .map(|line| div().min_h(px(SUB_LINE_H)).child(line.to_string())),
                        )
                })),
        )
    }

    /// The cue `lane` starts at `start_us` as a drawable picture, decoded on the
    /// first repaint it is up for and kept until another cue takes its place
    /// ([`Player::sub_image`]). `None` for a display set the decoder refuses,
    /// which draws nothing rather than failing the frame.
    ///
    /// Its atlas tile is released as the video's is: every [`RenderImage`] gets
    /// a fresh id and its own tile, so a film's worth of cues would grow the
    /// sprite atlas by the whole film.
    pub(crate) fn sub_picture(
        &mut self,
        lane: Lane,
        start_us: i64,
        image: &engine::subtitle::CueImage,
        window: &mut Window,
    ) -> Option<Arc<RenderImage>> {
        let key = (lane, start_us);
        if let Some((up, ready)) = &self.sub_image
            && *up == key
        {
            return Some(ready.clone());
        }
        let mut rgba = image.rgba()?;
        // gpui's atlas is BGRA with straight alpha; PGS decodes to RGBA with
        // straight alpha. The same swap the video frames get.
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let buf = image::RgbaImage::from_raw(image.width, image.height, rgba)?;
        let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        self.drop_sub_image(window);
        self.sub_image = Some((key, next.clone()));
        Some(next)
    }

    /// Lets go of the drawn cue and its atlas tile. Called where the cue stops
    /// being on screen, which is every gap between two of them: an 8 MB tile
    /// per cue is not a thing to leave behind a film.
    pub(crate) fn drop_sub_image(&mut self, window: &mut Window) {
        if let Some((_, old)) = self.sub_image.take() {
            let _ = window.drop_image(old);
        }
    }

    /// What is decoding the picture right now, for the transport line: the
    /// backend is the running worker's own (it is written where a hardware
    /// session falls back to software, so this follows reality), and the codec
    /// comes from the clip under the playhead. Empty when nothing is playing --
    /// the question is about what is happening, not about what would.
    pub(crate) fn live_decode(&self, position: f64, playing: bool) -> String {
        let Some(session) = self.session.as_ref().filter(|_| playing) else {
            return String::new();
        };
        let backend = session.decode_backend();
        let codec = session
            .video_clip_at(position)
            .and_then(|(lane, idx)| session.lane_clips(lane).get(idx).map(|clip| clip.source))
            .and_then(|source| session.sources().get(source))
            .and_then(|source| self.decoders.get(&source.path).copied().flatten())
            .and_then(|(codec, _)| codec);
        match backend {
            // Neither is a decode, and saying "SW" of them would be a lie.
            Backend::Gap => "gap · nothing to decode".to_string(),
            Backend::Still => "still · one decode, held".to_string(),
            _ => format!("{} decode", decode_label(codec, backend)),
        }
    }
}

/// `~/Pictures/edith`, or `.` with no `HOME` -- [`keymap::config_path_in`]'s
/// own fallback, for the same reason: a screenshot has to land somewhere even
/// on a machine with no desktop environment set up.
pub(crate) fn screenshots_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("Pictures")
        .join("edith")
}

/// Where a screenshot goes: `stem-tc.png` in `dir`, or the same name with a
/// numeric suffix if that one is already taken -- a screenshot never
/// overwrites another the way an export overwrites its own path.
pub(crate) fn screenshot_path(dir: &Path, stem: &str, tc: &str) -> PathBuf {
    let base = format!("{stem}-{tc}");
    let first = dir.join(format!("{base}.png"));
    if !first.exists() {
        return first;
    }
    let mut n = 2;
    loop {
        let path = dir.join(format!("{base}-{n}.png"));
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

/// The swap every BGRA frame needs to become the RGBA a PNG writes: gpui's
/// atlas is BGRA with straight alpha ([`Player::sub_picture`]'s own comment),
/// and `image::save` writes RGBA.
pub(crate) fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = bgra.to_vec();
    for pixel in out.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    out
}

/// Writes `bgra` (`w`x`h`, gpui's own layout) to a fresh PNG under
/// [`screenshots_dir`], creating the directory if this is the first one.
/// Never called on the render thread's own budget for more than a channel
/// swap and one `image::save` -- there is no decode here, only a copy already
/// in memory.
pub(crate) fn save_screenshot(
    bgra: &[u8],
    w: u32,
    h: u32,
    stem: &str,
    tc: &str,
) -> Result<PathBuf, String> {
    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = screenshot_path(&dir, stem, tc);
    let rgba = bgra_to_rgba(bgra);
    let buf = image::RgbaImage::from_raw(w, h, rgba)
        .ok_or_else(|| "frame buffer sized w*h*4".to_string())?;
    buf.save(&path).map_err(|e| e.to_string())?;
    Ok(path)
}
