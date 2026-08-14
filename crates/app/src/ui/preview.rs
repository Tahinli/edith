//! The picture: what is drawn over it and what is read off it.

use crate::*;

impl Player {
    /// The cues the *subtitle lanes* put on screen at `at`, over the picture and
    /// nothing else: bottom-centred where every player puts them, white on a
    /// plate so the film underneath cannot swallow them, and each cue its own
    /// plate so two at one moment stack rather than run together.
    ///
    /// What is drawn is what was *placed*: a track walked out of a file lands in
    /// the palette and nothing more, exactly as a picture or a song does, and it
    /// reaches the screen when a placement of it sits under the playhead
    /// ([`PlaybackSession::sub_lane_cues`], the same map the export writes the
    /// file with -- so what is read here is what the file will say). Every lane
    /// whose eye is open ([`Player::sub_lane_on`]) draws, so two enabled lanes
    /// are two plates: lane order bottom-up, the first lane where a single
    /// track's words have always been and each further one stacked above it.
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
        // Per lane and not one flat list: the plates stack by lane below, and a
        // picture's cache key needs the lane it came off ([`Player::sub_picture`]).
        let lanes: Vec<(Lane, Vec<engine::subtitle::Cue>)> =
            match self.session.as_ref().filter(|_| self.subs_on) {
                Some(session) => session
                    .subtitle_lanes()
                    .into_iter()
                    .filter(|&lane| self.sub_lane_on(lane))
                    .map(|lane| {
                        let cues = session.sub_lane_cues(lane);
                        (lane, cues_at(&cues, at).into_iter().cloned().collect())
                    })
                    .filter(|(_, cues): &(_, Vec<_>)| !cues.is_empty())
                    .collect(),
                None => Vec::new(),
            };
        if lanes.is_empty() {
            self.drop_sub_image(window);
            return None;
        }
        // The first picture cue up, decoded once and kept: two bitmap cues at
        // one moment is a thing PGS composes into one display set, so there is
        // never a second picture to stack under the first *on one lane*.
        //
        // corner-cut: two enabled lanes each showing a PGS track draw the lower
        // lane's picture alone -- one cache slot, one canvas over the whole
        // region, and a second canvas would cover the first anyway. The upgrade
        // path is a slot per lane, which wants the plates' stacking rule to mean
        // something for canvases too.
        let picture = lanes
            .iter()
            .find_map(|(lane, cues)| {
                cues.iter()
                    .find_map(|cue| Some((*lane, cue.start_us, cue.image.as_ref()?)))
            })
            .and_then(|(lane, start_us, image)| self.sub_picture(lane, start_us, image, window));
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
                // Reversed, because a column anchored at the bottom lays its
                // children downwards: the *last* lane is written first so the
                // first lane keeps the bottom line it has when it is the only
                // one, and a second lane stacks above it rather than pushing it
                // off its place.
                .children(lanes.into_iter().rev().flat_map(|(_, cues)| cues).filter(|c| c.image.is_none()).map(|cue| {
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
