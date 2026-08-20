//! The picture: what is drawn over it and what is read off it.

use crate::*;
use crate::ui::type_scale::{self, Typeset};
use std::path::{Path, PathBuf};

impl Player {
    /// The one thing that says a preview is up rather than the timeline: a
    /// banner over the top of the picture, and a click of its own -- the
    /// keyboard has `esc`, and a hand with only a mouse needs a way out too.
    ///
    /// Legacy-only now (DEFECT 2, MOCK-SPEC.md): this is a full-width band
    /// drawn *on* the frame, which DESIGN §5 forbids for the darkroom
    /// ("nothing else over the picture, ever"). [`Player::preview_plate`] is
    /// the darkroom's own restyle of the same information, built as a flex
    /// sibling so the picture's own box shrinks for it rather than being
    /// painted over -- the same fix `two_up`'s doc comment describes for the
    /// OUT|IN plates. `OLD_GUI=1` keeps drawing this banner unchanged.
    pub(crate) fn preview_badge(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        (!self.darkroom && self.preview_session.is_some()).then(|| {
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

    /// The darkroom's own preview indicator (DEFECT 2, MOCK-SPEC.md): a
    /// plate (canvas-on-panel, §4), mono for what the film says, and a ghost
    /// `Stop` wearing its `esc` chord rather than the legacy bordered
    /// button -- no fill, no border, hover lifts one step like every other
    /// ghost in the room.
    ///
    /// corner-cut: built and unit-shaped but **not yet mounted** -- it needs
    /// to sit as a flex sibling of the picture inside `ui::stance::screen`
    /// (the box `two_up`'s own doc comment already fixed this exact way for
    /// the OUT|IN plates), which is `stance.rs`, out of this task's file
    /// ownership while another builder has it open concurrently. `#[allow
    /// It is mounted in `ui::stance::screen` as a flex sibling below the
    /// picture's own child, the way `two_up` is, so the frame's box shrinks to
    /// make room rather than being drawn over.
    pub(crate) fn preview_plate(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        (self.darkroom && self.preview_session.is_some()).then(|| {
            div()
                .id("preview-plate")
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .px(px(10.))
                .py(px(4.))
                .rounded(px(2.))
                .bg(rgb(DARK_PANEL()))
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK2()))
                .child("PREVIEW — not on the timeline")
                .child(
                    // Ghost Stop: glyph over its chord, no border, no fill
                    // at rest -- DESIGN §4's grammar, the same shape the
                    // spine's own commands use.
                    div()
                        .id("preview-stop")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(DARK_RAISED())))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_preview(cx)))
                        .child(div().text_color(rgb(INK2())).child("Stop"))
                        .child(
                            div()
                                .type_style(type_scale::mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    gpui::FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK3()))
                                .child("esc"),
                        ),
                )
        })
    }

    /// The preview's own scrub bar, drawn low over the picture the way the
    /// badge is drawn over its top: a preview has a duration and a playhead
    /// like anything else on screen, and unlike the timeline's ruler
    /// ([`Player::seek_bar`] is a different bar entirely, the "still
    /// seeking" status line) it is the only way to jump around one with a
    /// mouse -- `esc` and the arrow keys are the keyboard's own doors.
    ///
    /// The hit area is `RULER_HIT_H` (`HIT_MIN`, WCAG 2.5.8) tall and centred
    /// on a 6 px track, the ruler's own idiom: a bar thin enough to read is
    /// too thin to reliably click.
    pub(crate) fn preview_scrub_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let session = self.preview_session.as_ref()?;
        let filled = preview_progress_along(session.now(), session.timeline_duration());
        Some(
            div()
                .id("preview-scrub")
                .absolute()
                .left_0()
                .right_0()
                // Above the transient bars, not over them: the bar's 24px hit
                // area drawn on a one-line notice ate the notice's own "click
                // to dismiss". The probe below the bars says how tall they
                // came out this frame; zero with none up.
                .bottom(self.notice_h.get())
                .h(px(RULER_HIT_H))
                .flex()
                .flex_col()
                .justify_center()
                .px(px(8.))
                .cursor_pointer()
                .tooltip(|_, cx| cx.new(|_| Tip("Seek — click or drag".into())).into())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.preview_scrubbing = true;
                        this.preview_scrub_to(event.position.x, true, cx);
                    }),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(6.))
                        .rounded(px(3.))
                        .bg(rgba(SUB_SHADE()))
                        .child(bounds_probe(self.preview_bar.clone()))
                        .child(
                            div()
                                .h_full()
                                .w(relative(filled))
                                .rounded(px(3.))
                                .bg(rgb(ACCENT_PRIMARY())),
                        ),
                ),
        )
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
            // A preview is another file: the timeline's words over its
            // picture would caption the wrong film.
            .filter(|_| self.preview_session.is_none())
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
                    .child(letterboxed_image(image)),
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
                        .text_size(px(self.sub_text))
                        .text_color(rgb(SUB_FG()))
                        .text_align(TextAlign::Center)
                        .when_some(self.sub_family.clone(), |el, fam| el.font_family(fam))
                        // A line of the cue is a line on screen: the break the
                        // parser kept is not whitespace to be re-flowed. What a
                        // *long* line does is wrap inside its own div, which is
                        // what the width cap above is for.
                        .children(cue.text.split('\n').map(|line| {
                            div()
                                .min_h(px(sub_line_h_for(self.sub_text)))
                                .child(line.to_string())
                        }))
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

    /// Two-up OUT|IN judging (DESIGN.md §6): drawn *in* the screen, below the
    /// picture, only when the playhead rests exactly on one edge of the
    /// subject cut -- the outgoing clip's tail beside the incoming one's
    /// head, whichever edge the playhead is on. `None` off a cut, off the
    /// darkroom path, or with nothing marked, so the legacy picture and a
    /// mid-clip darkroom picture both draw untouched.
    ///
    /// DESIGN §5 "never covered" / §11 check 6: this used to be an
    /// `absolute()` plate pair over the bottom of the screen, blacking out
    /// real picture pixels. The caller (`ui::stance::screen`) now lays this
    /// out as a flex sibling *below* the picture instead of layering it on
    /// top, so the picture's own `flex_1` shrinks to make room -- a
    /// letterbox band, not an occlusion.
    ///
    /// corner-cut: each side names its neighbour (source, timecode) on a
    /// plate rather than decoding its edge frame into a second picture --
    /// the pixel-accurate two-up wants a frame read off the *other* clip's
    /// span while the shown picture is the marked one's, which is a second
    /// decode this step does not open. Upgrade path: a still pulled from the
    /// neighbour's edge frame ([`engine::PlaybackSession::video_source_frame_at`]
    /// is the same lookup [`Player::live_decode`] already makes) in place of
    /// the plate's text.
    ///
    /// Always the same fixed-height markup while the darkroom is up, at rest
    /// on a cut or not -- `.invisible()` (`Visibility::Hidden`, confirmed in
    /// `gpui_macros::styles::visibility_style_methods`: painting is skipped,
    /// the layout box is not) keeps the strip's room reserved instead of
    /// collapsing it, which is what let the picture's own `flex_1` above it
    /// resize on every cut walk (404px resting on a cut, 335px a frame off
    /// one, springing back on the very next). Only `!self.darkroom` still
    /// returns `None`: the legacy tree never reserved this room and does not
    /// start now.
    pub(crate) fn two_up(&self) -> Option<impl IntoElement> {
        if !self.darkroom {
            return None;
        }
        let resting = (|| {
            let (lane, idx) = self.selected.anchor()?;
            let session = self.session.as_ref()?;
            let clips = session.lane_clips(lane);
            let clip = clips.get(idx)?;
            let now = frame_at(session.now(), self.fps);
            let name_of = |source: usize| {
                session
                    .sources()
                    .get(source)
                    .map_or_else(|| "?".to_string(), |s| file_name(&s.path))
            };
            // Which edge, if either, the playhead is resting on -- the one
            // question "at rest on a cut" is.
            if now == clip.start {
                let out = idx
                    .checked_sub(1)
                    .and_then(|i| clips.get(i))
                    .map_or("— nothing before it".to_string(), |c| {
                        format!("{} @{}", name_of(c.source), c.end())
                    });
                Some((out, format!("{} @{}", name_of(clip.source), clip.start)))
            } else if now == clip.end() {
                let inn = clips
                    .get(idx + 1)
                    .map_or("— nothing after it".to_string(), |c| {
                        format!("{} @{}", name_of(c.source), c.start)
                    });
                Some((format!("{} @{}", name_of(clip.source), clip.end()), inn))
            } else {
                None
            }
        })();
        let on_cut = resting.is_some();
        let (out_label, in_label) = resting.unwrap_or_default();
        let plate = |label: &str, text: String| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(8.))
                .bg(rgb(DARK_CANVAS()))
                .rounded(px(2.))
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(rgb(INK3()))
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(rgb(INK1()))
                        .child(text),
                )
        };
        Some(
            div()
                .id("two-up")
                .flex_none()
                .w_full()
                .bg(rgb(DARK_PANEL()))
                .border_t_1()
                .border_color(rgba(DARK_SEAM()))
                .flex()
                .gap(px(1.))
                .p(px(8.))
                .when(!on_cut, |d| d.invisible())
                .child(plate("OUT", out_label))
                .child(plate("IN", in_label)),
        )
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
