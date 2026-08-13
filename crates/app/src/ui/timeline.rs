//! The bottom region: the ruler, the lanes and the subtitle strip.

use crate::*;
use crate::ui::widgets::*;

impl Player {
    /// The bottom region, full width under everything else: the timecode and
    /// what is decoding, the ruler, the lanes and the subtitle strip. The edit
    /// buttons are the row directly above it ([`Player::toolbar`]) -- the
    /// arrangement every consumer editor shares, and the reason this is no
    /// longer one "panel" that owned both.
    pub(crate) fn timeline(
        &self,
        position: f64,
        duration: f64,
        state: Transport,
        viewport_h: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Where the playhead is *on the bed*, in pixels from its left edge.
        // Clamped to the bed because it is drawn as a width as well as an
        // offset, and the view follows the playhead anyway, so it is never off
        // the bed for long.
        let bed_w = f32::from(self.ruler.get().size.width);
        let filled = self.scale.px_at(position).clamp(0., bed_w);
        // An export owns the hint slot and the ruler while it runs: the
        // percentage and the accent bar are the same number, so the playhead
        // fill doubles as the progress bar for free.
        let exporting = self.exporting().is_some();
        let key = |action| self.keymap.display(action);
        // The lanes the project has, or the pair a fresh one starts with so the
        // panel reads the same before a file is open as after.
        let lanes = self
            .session
            .as_ref()
            .map_or_else(|| vec![Lane::V1, Lane::A1], PlaybackSession::lanes);
        // A loop rather than a `map`: each row takes `cx` in turn, where a
        // closure would hold it for as long as the iterator lives.
        let mut rows = Vec::new();
        for &lane in &lanes {
            rows.push(self.lane_row(lane, filled, cx));
        }
        // Built from the same playhead pixel the lanes are, and before the
        // export takes `filled` over below: the strip is a picture of the
        // timeline, not of an export's progress.
        let strip = self.subtitle_strip(filled, cx);
        let strip_h = subtitle_strip_h(strip.is_some());
        let (hint, filled) = if let Some(export) = self.exporting() {
            let progress = export.progress();
            let elapsed = self
                .export_started
                .map_or(0., |t| t.elapsed().as_secs_f32());
            // Two numbers that must both be honest: the one that counts up is
            // measured, the one that counts down is a guess and says so.
            let left = eta_secs(&self.export_marks, elapsed, progress).map_or_else(
                || "estimating…".to_owned(),
                |s| format!("~{} left", clock(s)),
            );
            (
                format!(
                    // Clocks, then the way out, then what is encoding: at the
                    // 640 px floor the tail is what truncation eats, and the
                    // codec pair is the one part of this line the card already
                    // said. The escape must not be what goes missing.
                    "EXPORTING {}% · {} elapsed · {left} — {} cancels · {} · {}",
                    (progress * 100.) as u32,
                    clock(elapsed),
                    key(ActionId::CancelExport),
                    // The row that was picked; the engine's line below names the
                    // seats alone, since the library is what identifies a codec.
                    format_label(self.format),
                    // What the worker actually opened, so a fallback to the
                    // software encoder shows here rather than being invisible.
                    export
                        .encoders()
                        .unwrap_or_else(|| "opening the encoder".to_string()),
                ),
                progress,
            )
        } else {
            // The strokes no button carries; the rest ride on the buttons'
            // tooltips. Keys first: at a 640 px window the tail is what a
            // truncation eats, and the two hints at the end are also on the
            // ruler's and Import's tooltips.
            // While it plays, what is decoding it goes first: it is the
            // answer that changes as the playhead crosses a cut, and the tail
            // of this line is what a narrow window truncates.
            // ...and ahead of even that, where the playhead is standing when
            // that is a frame a cut is free at: it decides whether an export of
            // this film is minutes of copying or hours of encoding, which is
            // the one thing on this line worth interrupting the hints for.
            let sync = match self.on_sync_point() {
                true => "SYNC POINT — a cut here is copied, not re-encoded",
                false => "",
            };
            (
                join_detail(
                    sync,
                    &join_detail(
                        &self.live_decode(position, state.is_playing()),
                        &format!(
                            "{} copy · {} paste · {} undo · click the bar to seek · drop a file \
                             to import",
                            key(ActionId::Copy),
                            key(ActionId::Paste),
                            key(ActionId::Undo)
                        ),
                    ),
                ),
                filled,
            )
        };
        // What the region got, and what is left inside it for the lanes: the
        // number a scroll is measured against, and the one the affordance below
        // counts with. The line it costs is taken off the box before the count,
        // or the row that says "1 more" would be the row hiding it.
        let region_h = (timeline_h(lanes.len()) + strip_h).min(viewport_h * TIMELINE_SHARE);
        let lanes_box = (region_h - TIMELINE_FIXED_H - strip_h).max(LANE_H);
        let overflows = lanes.len() > lanes_shown(lanes_box);
        let lanes_box = match overflows {
            true => (lanes_box - LABEL_H - 8.).max(LANE_H),
            false => lanes_box,
        };
        // Live, not a count of the lanes: the column reports where it has been
        // taken to, so the line empties itself as the last track comes up
        // instead of insisting there is still something below.
        let scrolled = -f32::from(self.lanes_scroll.offset().y);
        let below = rows_below(lanes.len(), lanes_box, scrolled);
        div()
            .flex_none()
            // Never more than its share of a short window: the lane column
            // scrolls inside whatever it gets, and a timeline that pushes the
            // picture off the screen at the 640x360 floor is not a timeline.
            .h(px(region_h))
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .bg(rgb(BG_TIMELINE()))
            .border_t_1()
            .border_color(rgb(STROKE_DIVIDER()))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    // Fixed width and one line: changing digits must not push
                    // the row around, nor wrap and change its height.
                    // The timeline's own length, never the drawn one: a tail
                    // being dragged inflates the bed to the room that edge has
                    // ([`Player::drawn_duration`]), and for a still that room is
                    // ten minutes -- a total that jumped to 10:00:00 the moment
                    // a picture's edge was pressed, and back on release.
                    .child(div().flex_none().w(px(TIME_W)).truncate().child(format!(
                        "{} / {}",
                        timecode(position, self.fps),
                        timecode(
                            self.session
                                .as_ref()
                                .map_or(0., PlaybackSession::timeline_duration),
                            self.fps
                        )
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(rgb(FG_SECONDARY()))
                            .child(hint),
                    ),
            )
            // Press to seek, drag to scrub: the move and release halves live on
            // the root, since the pointer leaves the bar immediately. The bar
            // stays 6 px to look at; the strip that takes the click is 24, so
            // it can be hit without aiming (WCAG 2.5.8).
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(HEADER_GAP))
                    // The lanes' header column, empty here: the ruler's own bar
                    // has to start where their beds start, or the playhead
                    // would point at a different moment in each row.
                    .child(div().flex_none().w(px(HEADER_W)))
                    .child(
                        div()
                            .id("ruler")
                            .flex_1()
                            .min_w(px(0.))
                            .h(px(RULER_HIT_H))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(BG_HOVER_DIM())))
                            // The strip carries no text, so the tooltip is the only
                            // place it can say what it is.
                            .tooltip(|_, cx| {
                                cx.new(|_| {
                                    Tip("Seek — click or drag; wheel scrolls, ctrl+wheel zooms"
                                        .into())
                                })
                                .into()
                            })
                            // Ctrl+wheel zooms about the pointer and a bare one
                            // scrolls the view along -- [`Player::timeline_wheel`],
                            // the same answer the lanes below give, because a
                            // hand does not aim at a strip to zoom a timeline.
                            .on_scroll_wheel(cx.listener(
                                |this, event: &ScrollWheelEvent, _, cx| {
                                    this.timeline_wheel(event, cx);
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    this.scrubbing = true;
                                    this.scrub_to(event.position.x, true, cx);
                                }),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(6.))
                                    .rounded(px(3.))
                                    .bg(rgb(BG_RAISED()))
                                    .child(bounds_probe(self.ruler.clone()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(px(filled))
                                            .rounded(px(3.))
                                            .bg(rgb(ACCENT_PRIMARY())),
                                    ),
                            ),
                    ),
            )
            // Every lane the project has, in its own order -- and its own
            // column, so a project with more lanes than the panel is tall
            // scrolls its tracks instead of pushing the picture off the window.
            // The gap is the panel's own, so two lanes lay out exactly as they
            // did when they were two children of it.
            .child(
                div()
                    .id("lanes")
                    // Takes whatever the region has left and scrolls inside it:
                    // at the 640x360 floor the region is capped
                    // ([`TIMELINE_SHARE`]) and a fixed column would be clipped
                    // by it rather than scrolled.
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .max_h(px(lanes_h(LANES_MAX)))
                    .overflow_y_scroll()
                    .track_scroll(&self.lanes_scroll)
                    .children(rows),
            )
            // "It scrolls" is not "it can be found": when the region is too
            // short for every track -- which is what the 640x360 floor does to a
            // three-track project -- the lanes below the fold say so, in the one
            // place the eye is already looking. Its own flex_none row under a
            // flex_1 column, so the thing announcing the overflow cannot be the
            // thing the overflow pushes off the window.
            .when(overflows, |d| {
                d.child(
                    div()
                        .flex_none()
                        .h(px(LABEL_H))
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_size(px(10.))
                        .text_color(rgb(match below {
                            0 => FG_SECONDARY(),
                            _ => ACCENT_PRIMARY(),
                        }))
                        .child(match below {
                            // Over the track names, not over the beds: a wheel
                            // on a bed moves the *view* along the timeline now,
                            // so the column names its own scroll surface.
                            0 => "the last track — scroll the names for the rest".to_string(),
                            1 => "1 more track below — scroll the track names".to_string(),
                            n => format!("{n} more tracks below — scroll the track names"),
                        }),
                )
            })
            // Under the tracks and outside their scrolling column: a subtitle is
            // not a track -- nothing can be dropped on it and nothing dragged
            // along it -- and a strip that scrolled away with the sixth lane
            // would be a strip nobody could see while editing.
            .children(strip)
    }

    /// The subtitle tracks this timeline holds, under the media list: one row
    /// each, the picked one marked, and a click makes another one the picked one
    /// -- which is the whole of choosing between the two tracks of a film. A row
    /// per track and no cycle: three of them is an ordinary number for a remux,
    /// and a key that steps through three is a key nobody can aim.
    ///
    /// A track that could not be read is *here*, greyed and saying why, exactly
    /// as a media row the timeline cannot take is: PGS subtitles are pictures,
    /// and a film carrying four of them says so instead of listing nothing.
    ///
    /// Every row names the file it came out of itself, in words and in the tint
    /// that file's clips wear on the lanes -- but only where there is more than
    /// one file to tell apart, the way [`row_name`] numbers audio streams only
    /// where a file gave several. Where the window is tall enough for it
    /// ([`sub_headers_fit`]) each file's block is headed by its name as well: a
    /// label and nothing more, no click and nothing to fold, so the rows under
    /// it are the only things anybody has to aim at. At the 640x360 floor the
    /// headers are gone and the rows still say whose they are.
    ///
    /// `None` when there are none -- an empty heading is a section about
    /// nothing.
    pub(crate) fn subtitle_section(
        &self,
        width: f32,
        viewport_h: f32,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let tracks = self.session.as_ref()?.subtitles();
        if tracks.is_empty() {
            return None;
        }
        let groups = subtitle_rows(tracks);
        // What the row's × says the way back is, in the stroke this keymap
        // actually binds: a file's subtitles go on without the file.
        let add_key = self.keymap.display(ActionId::AddSubtitleTrack);
        // One file's tracks need no prefix saying which file: it is the only
        // one, and every row would carry the same word.
        let several_files = groups.len() > 1;
        let headed = several_files && sub_headers_fit(viewport_h);
        let text_w = row_text_w(width);
        let rows: Vec<_> = groups
            .into_iter()
            .map(|SubGroup { name, path, rows }| {
                // The file's own colour, the one its media rows and its clips
                // wear -- and none at all for a standalone `.srt`, which came
                // off no file on this timeline.
                let tint = file_tint(self.sources(), &path);
                let numbered = rows.len() > 1;
                // The name twice over: all of it the header can hold, and the
                // share of a row a prefix may take in front of the label.
                let head = clip_middle(&name, text_w);
                let prefix = clip_middle(&name, text_w * SUB_STEM_SHARE);
                let rows: Vec<_> = rows
                    .into_iter()
                    .map(|row| {
                        let track = row.track;
                        let picked = track == self.sub_track;
                        let usable = row.refused.is_none();
                        // Two tracks off one remux that both say "eng" are told
                        // apart by their number and by nothing else -- the same
                        // count [`sub_pick_name`] echoes.
                        let title = match numbered {
                            true => format!("{} {}", row.label, row.number),
                            false => row.label,
                        };
                        // A standalone `.srt` is named after its own file, so
                        // the file in front of it says the same word twice
                        // ("Legend.of.… · Legend.of.…"). [`sub_pick_name`]'s
                        // rule, on the row it is about.
                        let owned = several_files && !title.starts_with(name.as_str());
                        // The whole path, never clipped: the row says which
                        // file, and the tooltip says which one on disk.
                        let tip: SharedString = match &row.refused {
                            Some(why) => format!("{} — {why}", path.display()),
                            None => format!(
                                "{} — click to show this track over the picture",
                                path.display()
                            ),
                        }
                        .into();
                        // Named, because a × is the same glyph on every row and
                        // the tooltip is what says which track it takes off.
                        let remove_tip: SharedString =
                            format!(
                                "Remove {title} — {} puts a file's subtitles back on",
                                add_key
                            )
                                .into();
                        div()
                            // The *flat* index into the session's add-order
                            // list, which is what a pick is and what a save
                            // writes: the grouping moved the row on screen and
                            // never the track it stands for.
                            .id(("subtitle-track", track))
                            .flex_none()
                            .h(px(ROW_H))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pr(px(6.))
                            .rounded(px(3.))
                            .when(!usable, |d| d.text_color(rgb(FG_SECONDARY())).opacity(0.55))
                            .when(usable, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.bg(rgb(BG_HOVER())))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.sub_track = track;
                                        // Picking a track is asking to see it: a
                                        // click that changed nothing on screen
                                        // because the toggle was off would read
                                        // as a dead row.
                                        this.subs_on = true;
                                        cx.notify();
                                    }))
                            })
                            .when(picked, |d| d.bg(rgb(BG_SELECTED())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                            // The media rows' bar, same width and hard against
                            // the same edge: one association across the panel
                            // and the lanes. Kept as room rather than dropped
                            // where there is no tint, so a standalone `.srt`
                            // still lines its words up with the rest.
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(SWATCH_W))
                                    .h_full()
                                    .rounded(px(2.))
                                    .when_some(tint, |d, tint| d.bg(rgb(tint))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .text_size(px(11.))
                                            // In front, because the column
                                            // truncates from the right: an
                                            // ownership word at the end is a
                                            // word the floor never shows.
                                            .when(owned, |d| {
                                                d.child(
                                                    div()
                                                        .flex_none()
                                                        // Said twice where a
                                                        // header says it above:
                                                        // still there, out of
                                                        // the way.
                                                        .when(headed, |d| {
                                                            d.text_color(rgb(FG_SECONDARY()))
                                                        })
                                                        .child(format!("{prefix} · ")),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .child(title),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(10.))
                                            .text_color(rgb(FG_SECONDARY()))
                                            .child(row.detail),
                                    ),
                            )
                            // The way back off the timeline, on every row and on
                            // the last one too -- a list of subtitles is allowed
                            // to be empty, unlike a lane. A `HIT_MIN` target and
                            // never hidden, the lane header's ×, and it stops
                            // the click there: the row under it picks a track,
                            // and picking the track that has just gone would
                            // leave the pick naming nothing.
                            .child(
                                div()
                                    .id(("subtitle-remove", track))
                                    .flex_none()
                                    .w(px(HIT_MIN))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(BG_HOVER())))
                                    .tooltip(move |_, cx| cx.new(|_| Tip(remove_tip.clone())).into())
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.remove_subtitle_track(track, cx);
                                        },
                                    ))
                                    .child("×"),
                            )
                    })
                    .collect();
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    // The header: which film these came out of, in words and not
                    // in colour alone. No id, no click, nothing to fold -- a
                    // label, which is why it is allowed under `HIT_MIN`.
                    .when(headed, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .h(px(SUB_HEAD_H))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(10.))
                                .text_color(rgb(FG_SECONDARY()))
                                .when_some(tint, |d, tint| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .w(px(SWATCH_W))
                                            .h_full()
                                            .rounded(px(2.))
                                            .bg(rgb(tint)),
                                    )
                                })
                                .child(div().flex_1().min_w(px(0.)).truncate().child(head)),
                        )
                    })
                    .children(rows)
            })
            .collect();
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(rgb(FG_SECONDARY()))
                        .child(match (self.subs_on, sub_pick_name(tracks, self.sub_track)) {
                            // Which of them is on screen, named by its film:
                            // the heading over a list of five is where the one
                            // being shown is worth saying out loud.
                            (true, Some(pick)) => format!("Subtitles — {pick}"),
                            (true, None) => "Subtitles".to_string(),
                            // The toggle's state where the tracks are listed: a
                            // list of subtitles nothing on screen is showing has
                            // to say that it is showing none.
                            (false, _) => "Subtitles — hidden".to_string(),
                        }),
                )
                .child(
                    div()
                        .id("subtitle-rows")
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .max_h(px(SUB_ROWS_H))
                        .overflow_y_scroll()
                        .children(rows),
                ),
        )
    }

    /// One lane of the edit list made visible: a fixed header saying which lane
    /// it is, then a bed with a box per clip, placed and sized by its share of
    /// the timeline. A cut adds a box without moving anything, a delete closes
    /// the hole, a lift leaves one. A clip too narrow for its name says what it
    /// is by its tint, and the tooltip is where every box says what clicking it
    /// does. Never focusable, so the root keeps focus and the play binding still
    /// works after a click (ledger:182).
    /// The subtitle strip: where the picked track's cues are along the very same
    /// bed the lanes are drawn on, through the very same [`Scale`], so a cue
    /// lines up with the take it belongs to at every zoom.
    ///
    /// Display only -- no drag, no trim, no drop. What it is *for* is the
    /// question "is there a subtitle here", which a timeline could not answer at
    /// all before: the cues are the project's and are edited in the file they
    /// came from.
    ///
    /// `None` with no track picked, so a timeline without subtitles is the panel
    /// it has always been.
    pub(crate) fn subtitle_strip(
        &self,
        filled: f32,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let track = self.subtitle_track()?;
        let scale = self.scale;
        // The pick with its film on it, not the raw tag: two films' "eng"
        // tracks read alike otherwise, and a header saying "und" names a
        // language nobody speaks.
        let label = sub_pick_name(self.session.as_ref()?.subtitles(), self.sub_track)?;
        // The colour the file's own rows and clips wear, so the strip says
        // whose subtitles these are before the tooltip is asked. `None` for a
        // standalone `.srt` -- nobody's stream, and the first film's colour
        // would be a lie about where it came from.
        let tint = file_tint(self.sources(), &track.path).unwrap_or(BG_RAISED());
        // The whole of what the row says in words, since a 40 px column can hold
        // three characters of it: which track of which file, how many cues, the
        // file itself in full -- one stem is two films when they are two cuts of
        // it -- and, for a track that could not be read, the engine's own reason,
        // where the library row's grey already says the same thing.
        let tip: SharedString = match track.refused.is_some() {
            true => format!(
                "Subtitles: {label} — {} — {}",
                subtitle_detail(track),
                track.path.display()
            ),
            false => format!(
                "Subtitles: {label} — {}, {} hides them — {}",
                subtitle_detail(track),
                self.keymap.display(ActionId::ToggleSubtitles),
                track.path.display()
            ),
        }
        .into();
        // Where the cues are on *this* timeline and not in the file they came
        // from ([`PlaybackSession::timeline_cues`]) -- the same map the plate
        // over the picture and the export both go through, which is what keeps
        // a mark under the take it is spoken over after a cut.
        let cues: Vec<(f32, f32)> = self
            .session
            .as_ref()?
            .timeline_cues(self.sub_track)
            .iter()
            .map(|cue| cue_box(scale, cue))
            .collect();
        Some(
            div()
                .flex_none()
                .h(px(SUB_LANE_H))
                .flex()
                .gap(px(HEADER_GAP))
                .child(
                    // Identified for the tooltip's sake and for nothing else:
                    // the whole of what a 40 px column can say is three
                    // characters, and the rest of it hangs off the hover.
                    div()
                        .id("subtitle-lane")
                        .flex_none()
                        .w(px(HEADER_W))
                        .h_full()
                        .flex()
                        .items_center()
                        // From the left rather than centred, unlike a lane's
                        // `V1`: a track label is a language or a file name, and
                        // a centred truncation eats both of its ends.
                        .px(px(2.))
                        .rounded(px(3.))
                        .bg(rgb(tint))
                        .text_size(px(9.))
                        .text_color(rgb(FG_SECONDARY()))
                        .truncate()
                        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                        .child(label),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w(px(0.))
                        .h_full()
                        .rounded(px(3.))
                        .bg(rgb(BG_TIMELINE()))
                        .overflow_hidden()
                        // The same bed under the same [`Scale`] answers the same
                        // wheel ([`Player::timeline_wheel`]): ctrl zooms about
                        // the pointer, bare scrolls the view along. A hand that
                        // wants a cue closer aims at the cue, and a timeline
                        // surface that ignores the wheel is a dead one.
                        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                            cx.stop_propagation();
                            this.timeline_wheel(event, cx);
                        }))
                        .children(cues.into_iter().map(|(left, width)| {
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(px(left))
                                .w(px(width))
                                .rounded(px(2.))
                                .bg(rgb(BG_SELECTED()))
                        }))
                        // The playhead again, last and in the lanes' own colour:
                        // the strip is only worth anything beside them if it
                        // reads as the same moment.
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(px(filled))
                                .w(px(2.))
                                .bg(rgb(ACCENT_PLAYHEAD())),
                        ),
                ),
        )
    }

    pub(crate) fn lane_row(
        &self,
        lane: Lane,
        // Where the playhead is on the bed, in pixels: worked out once by the
        // panel so the ruler's line and every lane's draw the same one.
        filled: f32,
        cx: &mut Context<Self>,
        // Borrows nothing it was given (`use<>`): the rows are built one after
        // another into a list, and a row still holding `cx` would be the only
        // one that could be built.
    ) -> impl IntoElement + use<> {
        // The mapping, copied out once: every box in the row is placed through
        // it, so all of them move together when it does. No bed width is needed
        // to place them any more -- a second is so many pixels wherever it is.
        let scale = self.scale;
        // How much bed there is to be seen on, measured off the ruler's own
        // probe like every other question about it: what a box draws *inside*
        // itself is clipped to this ([`visible_slice`]), because a box at a deep
        // zoom is far wider than the strip it is being watched through.
        let bed = f32::from(self.ruler.get().size.width);
        // Where the snap line stands, in the same pixels every box is placed
        // through -- and only while a gesture is actually live: gpui drops a
        // drag without telling anyone, so this asks whether one is in flight
        // (`App::has_active_drag`) rather than remembering that one was.
        let cue = self
            .snap_cue
            .filter(|_| self.trim.is_some() || cx.has_active_drag())
            .map(|frame| scale.px_at(f64::from(frame) / self.fps));
        // The shadow, on the one lane the pointer is over -- and, like the line,
        // only while the drag that asked for it is still in flight.
        let ghost = self
            .ghost
            .filter(|g| g.lane == lane && cx.has_active_drag());
        let clips = self
            .session
            .as_ref()
            .map_or(&[][..], |session| session.lane_clips(lane));
        // The group ids some *other* lane carries: a clip whose id is in here
        // has a half elsewhere, and one whose is not is a detached half however
        // many lanes there are.
        let others: Vec<u32> = self.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .lanes()
                .into_iter()
                .filter(|&other| other != lane)
                .flat_map(|other| session.lane_clips(other))
                .filter_map(|clip| clip.link)
                .collect()
        });
        let name = lane.label();
        let row_id: SharedString = format!("{name}-clip").into();
        let remove_id: SharedString = format!("{name}-remove").into();
        let header_id: SharedString = format!("{name}-header").into();
        let header_tip: SharedString =
            format!("{name} — drag this header onto another to reorder the tracks").into();
        // The header ghost, the same shape a clip's is: the track's own name
        // under the pointer, so what is in the hand is legible while it moves.
        let header_ghost: SharedString = name.clone().into();
        // The line this row draws between itself and its neighbour while a
        // header is being carried -- and, like every other drag cue here, only
        // while the gesture is still in flight.
        let drop = self
            .lane_drop
            .filter(|d| d.lane == lane && cx.has_active_drag());
        let remove_tip: SharedString = format!(
            "Remove {name} — it must be empty first, and {} brings it back",
            self.keymap.display(ActionId::Undo)
        )
        .into();
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        let (sel, sel_link) = (self.selected, self.selected_link());
        let audio = lane.kind == LaneKind::Audio;
        // What this track plays at, on the header it belongs to: shown only
        // when it is not unity, because a column 40 px wide has room for a
        // number or for a name, and the name is what a header is for. The
        // press opens the mix card on this very track.
        let gain_db = self
            .session
            .as_ref()
            .map_or(0., |session| session.lane_gain_db(lane));
        let gain_tip: SharedString = format!(
            "{name} plays at {gain_db:+.0} dB — opens the mix ({}); the whole track, every frequency, unlike the equalizer",
            self.keymap.display(ActionId::Mix)
        )
        .into();
        let tip: SharedString = format!(
            "Select (or {} under the playhead, {}/{} along the lane) — drag it to move it, an end to trim, {} removes the take, {} leaves a gap, {} rejoins a cut",
            self.keymap.display(ActionId::Select),
            self.keymap.display(ActionId::SelectPrev),
            self.keymap.display(ActionId::SelectNext),
            self.keymap.display(ActionId::Delete),
            self.keymap.display(ActionId::Lift),
            self.keymap.display(ActionId::Regroup)
        )
        .into();
        div()
            .flex_none()
            .h(px(LANE_H))
            .flex()
            .gap(px(HEADER_GAP))
            // A header let go anywhere along this row lands the track in the
            // hand in this row's place: the whole width is the target, not the
            // 40 px column, because a slot is what is being aimed at. The bed's
            // own drops carry other payloads and never see this one.
            .relative()
            .on_drop(cx.listener(move |this, drag: &LaneDrag, _, cx| {
                this.reorder_lane(drag.0, lane, cx);
            }))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<LaneDrag>, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    this.preview_lane_drop(event.drag(cx).0, lane, cx);
                },
            ))
            // The fixed column the ruler above is offset by as well. Full lane
            // height, so it reads as the bed continuing rather than as a chip.
            .child(
                div()
                    // Dragged, the whole track moves in the stack -- the
                    // gesture every editor reorders tracks with, and the reason
                    // the column is a handle rather than a caption.
                    .id(header_id)
                    .cursor(CursorStyle::OpenHand)
                    .tooltip(move |_, cx| cx.new(|_| Tip(header_tip.clone())).into())
                    .on_drag(LaneDrag(lane), move |_, _, _, cx| {
                        cx.new(|_| Tip(header_ghost.clone()))
                    })
                    .flex_none()
                    .w(px(HEADER_W))
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .bg(rgb(BG_RAISED()))
                    .text_size(px(11.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child(match audio {
                        // A button, not a label: the one setting a track has of
                        // its own used to be reachable from nowhere.
                        true => div()
                            .id(("mix-lane", lane.ord))
                            .flex_1()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(BG_HOVER())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(gain_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.open_mix(Some(lane), cx)
                            }))
                            .child(match gain_db == 0. {
                                true => name.clone(),
                                false => format!("{name} {gain_db:+.0}"),
                            })
                            .into_any_element(),
                        false => div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .child(name.clone())
                            .into_any_element(),
                    })
                    // The one thing a header does: take this track away again.
                    // A `HIT_MIN` target rather than a glyph-sized one, and it
                    // stays put on a track holding clips instead of hiding --
                    // the refusal names them, and a control that vanishes
                    // teaches nothing.
                    .child(
                        div()
                            .id(remove_id)
                            .flex_none()
                            .w_full()
                            .h(px(HIT_MIN))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(BG_HOVER())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(remove_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove_lane(lane, cx)
                            }))
                            .child("×"),
                    ),
            )
            .child(
                // Clips are placed at their own start rather than queued edge
                // to edge: a lift leaves a hole in the lane, and the bare bed
                // showing through it *is* how a gap looks.
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .rounded(px(3.))
                    .bg(rgb(BG_TIMELINE()))
                    .overflow_hidden()
                    // The wheel over the clips themselves, which is where a
                    // hand already is: ctrl zooms about the pointer, bare
                    // scrolls the view along ([`Player::timeline_wheel`]).
                    // Stopped here, because gpui runs the lane *column's* own
                    // overflow scroll on the same notch (div.rs:2403) and a
                    // gesture that slid the view sideways and the tracks upward
                    // at once is two answers to one notch. The track headers
                    // beside this bed are still the column's scroll surface.
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        cx.stop_propagation();
                        this.timeline_wheel(event, cx);
                    }))
                    // A library row let go over a lane is the same insert the
                    // Add button makes, through the same call -- but where the
                    // pointer let it go, not at the playhead: a hand that
                    // carried a file to a place on the bed named that place.
                    // gpui hands a drop no event, so the pointer is read off
                    // the window, which took it from the release that fired
                    // this (gpui window.rs:3602).
                    .on_drop(cx.listener(move |this, drag: &AssetDrag, window, cx| {
                        // Onto the edges near it, exactly as a clip carried by
                        // hand lands: the line drawn while it was in flight is
                        // the frame it goes down on.
                        let at = this.place_frame(window.mouse_position().x).0;
                        this.insert_source(&drag.0.clone(), drag.1, Some(lane), Some(at), cx)
                    }))
                    .drag_over::<AssetDrag>(|s, _, _, _| s.bg(rgb(BG_HOVER_DIM())))
                    // The shadow of the row in flight, drawn by the lane the
                    // pointer is inside: `on_drag_move` fires on every painted
                    // element while a drag of its type is live, wherever the
                    // pointer is, and hands each one its own box -- which is how
                    // a lane knows the pointer is over *it* (gpui div.rs:282).
                    // The root cleared it a moment ago, so exactly one lane
                    // draws one.
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                            if !event.bounds.contains(&event.event.position) {
                                return;
                            }
                            let path = event.drag(cx).0.clone();
                            this.preview_ghost_asset(&path, lane, event.event.position.x, cx);
                        },
                    ))
                    // ...and a clip let go over a lane lands the same way: on
                    // the track it was dropped on, at the frame it was carried
                    // to -- its own included, which is the drag that moves a
                    // take along its track.
                    .on_drop(cx.listener(move |this, drag: &ClipDrag, window, cx| {
                        // Against the lane as it is *now* ([`Player::dragged`]),
                        // and then snapped by `move_clip` like any other drop:
                        // which clip is being moved and where it lands are two
                        // questions, and this is the first one.
                        let Some(idx) = this.dragged(drag) else {
                            return;
                        };
                        this.move_clip(drag.lane, idx, lane, window.mouse_position().x, cx)
                    }))
                    .drag_over::<ClipDrag>(|s, _, _, _| s.bg(rgb(BG_HOVER_DIM())))
                    // ...and the same shadow for the clip in the hand, seated on
                    // this lane when the pointer is inside it.
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                            if !event.bounds.contains(&event.event.position) {
                                return;
                            }
                            let drag = *event.drag(cx);
                            this.preview_ghost(&drag, lane, event.event.position.x, cx);
                        },
                    ))
                    .children(clips.iter().enumerate().map(|(i, clip)| {
                        // The clip as the lane holds it, for the drag payload:
                        // what a drop looks itself up by has to be the placed
                        // clip, never the preview an edge drag is drawing.
                        let placed = *clip;
                        // What a drag on an edge is showing, which is the clip
                        // itself while nothing is being dragged.
                        let clip = &self.trimmed(lane, i, *clip);
                        // Its *timeline* length, which a speed halves or
                        // quadruples: the box is as wide as the clip is long
                        // where it sits, not as long as the source it reads.
                        let (start, len) = (
                            f64::from(clip.start) / self.fps,
                            f64::from(clip.frames()) / self.fps,
                        );
                        let on = marked((lane, i), clip.link, sel, sel_link);
                        // A group with a half in the other lane wears its tint;
                        // one without is outlined, so a detached half is visible
                        // as detached before anyone clicks it.
                        let grouped = clip.link.is_some_and(|link| others.contains(&link));
                        // Tinted by *file*, not by source entry: two audio
                        // streams of one file are two sources, and the library
                        // gives them one swatch because they are one file.
                        let tint = self.clip_tint(clip.source);
                        // ...and painted by *kind*, which is the one thing a
                        // glance down a timeline has to answer: video blue,
                        // audio green, a still teal, the cues purple, the way
                        // every editor with more than one track colours them.
                        // The source tint stays the identity of the file and is
                        // the border and the library swatch.
                        let kind = clip_kind(
                            lane.kind,
                            sources
                                .get(clip.source)
                                .is_some_and(|s| engine::is_image(&s.path)),
                        );
                        // What the clip is worth in pixels, and how wide its box
                        // is drawn -- the two part company on a take too short
                        // to be hit at this zoom ([`clip_width`]).
                        let span = scale.width_px(len);
                        let width = clip_width(span);
                        let left = scale.px_at(start);
                        // The slice of this box that is on the bed: where its
                        // name, its badge and its waveform go, so none of the
                        // three is drawn out at a zoomed-in box's own edges.
                        let (vis_x, vis_w) = visible_slice(left, width, bed);
                        let label = sources.get(clip.source).map(|s| file_name(&s.path));
                        let wave = sources
                            .get(clip.source)
                            .and_then(|s| self.waves.get(&(s.path.clone(), s.audio_stream)))
                            .cloned();
                        // The source seconds that slice plays -- not the clip's
                        // whole range: the envelope is drawn for the part of the
                        // box that can be seen, at the resolution of the pixels
                        // it actually has, and never one column per two pixels
                        // of a box millions of pixels wide.
                        let along = |x: f32| match width > 0. {
                            true => {
                                f64::from(clip.in_frame)
                                    + f64::from(clip.out_frame - clip.in_frame)
                                        * f64::from(x / width)
                            }
                            false => f64::from(clip.in_frame),
                        };
                        let (from, to) = (along(vis_x) / self.fps, along(vis_x + vis_w) / self.fps);
                        let tip = tip.clone();
                        // What the pointer carries on the way to another lane:
                        // the file the box is showing, the same ghost a library
                        // row makes. A box too narrow for its own label still
                        // says what is moving.
                        let ghost: SharedString =
                            label.clone().unwrap_or_else(|| lane.label()).into();
                        // Its head in frames, for the press below: the `start`
                        // above is the same moment in seconds, which is what
                        // the box is *drawn* from.
                        let head = clip.start;
                        div()
                            // Named per lane: two rows numbering their clips
                            // from zero would hand gpui the same id twice.
                            .id((row_id.clone(), i))
                            .absolute()
                            .top_0()
                            .h_full()
                            // Negative once the clip's head has been scrolled
                            // off the left edge: the bed clips what hangs out
                            // of it, so a half-visible clip is drawn as the
                            // half of itself that is on screen.
                            .left(px(left))
                            .w(px(width))
                            .overflow_hidden()
                            .rounded(px(3.))
                            .border_1()
                            .border_color(rgb(if on {
                                STROKE_SELECTED()
                            } else if grouped {
                                tint
                            } else {
                                FG_SECONDARY()
                            }))
                            .bg(rgb(if on { BG_SELECTED() } else { kind }))
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(ACCENT_PRIMARY())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                            // Dragged, it *moves*: to the frame it was let go on
                            // and to the lane it was let go over. The click that
                            // starts the drag still selects, so picking a clip
                            // up and putting it back down where it was is
                            // exactly a click.
                            .on_drag(
                                ClipDrag {
                                    lane,
                                    idx: i,
                                    clip: placed,
                                },
                                move |_, _, _, cx| cx.new(|_| Tip(ghost.clone())),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    // Where in the box the hand took hold of it,
                                    // for the drag this press may become: the
                                    // clip has to move with the pointer rather
                                    // than jump its head under it.
                                    this.grab =
                                        this.frame_under(event.position.x).saturating_sub(head);
                                    this.select((lane, i), cx);
                                }),
                            )
                            // The right button selects exactly as the left one
                            // does -- the menu acts on the clip it names, so
                            // opening one has to pick it -- and then hangs the
                            // menu at the pointer.
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.open_menu(lane, i, event.position, cx);
                                }),
                            )
                            // The two strips a drag *lengthens* the clip by,
                            // one at each end. They occlude the box behind
                            // them, which is what keeps one gesture one thing:
                            // a press here trims, a press anywhere else on the
                            // box still starts the move to another lane.
                            //
                            // Asked of the clip's own width and not of the box's
                            // floor: a take drawn at `HIT_MIN` because it is
                            // shorter than that has no *pixels* to trim by -- one
                            // would move it by seconds -- so it keeps all of its
                            // padded box as a body to select and drag by, and is
                            // trimmed after zooming in, exactly as [`trims`] says.
                            .children(
                                [Edge::Start, Edge::End]
                                    .into_iter()
                                    .filter(|_| trims(span))
                                    .map(|edge| {
                                        let mut zone = div()
                                            .absolute()
                                            .top_0()
                                            .h_full()
                                            .w(px(EDGE_W))
                                            .occlude()
                                            .cursor(CursorStyle::ResizeLeftRight)
                                            .hover(|s| s.bg(rgb(ACCENT_PRIMARY())))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    move |this, _: &MouseDownEvent, _, cx| {
                                                        this.start_trim(lane, i, edge, cx);
                                                    },
                                                ),
                                            )
                                            // ...and occluding takes the *wheel* off
                                            // the lane's bed with it (gpui stops the
                                            // hit test at an occluder, ancestors
                                            // included), so a notch aimed at a cut --
                                            // the one place a hand aims when it wants
                                            // to see a cut closer -- would be a dead
                                            // strip. The same answer as the bed's.
                                            .on_scroll_wheel(cx.listener(
                                                |this, event: &ScrollWheelEvent, _, cx| {
                                                    cx.stop_propagation();
                                                    this.timeline_wheel(event, cx);
                                                },
                                            ))
                                            // Occluded, so the box's own right-button
                                            // listener never fires here: the menu is
                                            // the same menu, opened by the same call.
                                            .on_mouse_down(
                                                MouseButton::Right,
                                                cx.listener(
                                                    move |this, event: &MouseDownEvent, _, cx| {
                                                        this.open_menu(lane, i, event.position, cx);
                                                    },
                                                ),
                                            );
                                        zone = match edge {
                                            Edge::Start => zone.left_0(),
                                            Edge::End => zone.right_0(),
                                        };
                                        zone
                                    }),
                            )
                            // Under the label row, never through it.
                            .children(wave.filter(|_| audio && vis_w > 0.).and_then(|wave| {
                                let inner: AnyElement = match wave {
                                    Wave::Peaks(peaks) => {
                                        waveform(peaks, from, to).into_any_element()
                                    }
                                    // A bed while the decode runs, and dimmer
                                    // than any waveform is drawn: a flat
                                    // `FG_SECONDARY` line here would be the shape a
                                    // silent file makes, which this file is not
                                    // known to be yet.
                                    Wave::Loading => div()
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .child(div().w_full().h(px(1.)).bg(rgb(BG_HOVER())))
                                        .into_any_element(),
                                    // No audio track: nothing, never a fake.
                                    Wave::Silent => return None,
                                    // Could not be read: a band that says so in
                                    // words, because the empty band a silent
                                    // file draws would claim this file has no
                                    // sound. The reason itself went to the log.
                                    Wave::Failed => div()
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .truncate()
                                        .text_size(px(9.))
                                        .text_color(rgb(FG_SECONDARY()))
                                        .child("audio unreadable")
                                        .into_any_element(),
                                };
                                Some(
                                    div()
                                        .absolute()
                                        .left(px(vis_x))
                                        .w(px(vis_w))
                                        .top(px(LABEL_H))
                                        .bottom_0()
                                        .child(inner),
                                )
                            }))
                            // A speeded clip says so on the box, in the corner
                            // the label does not reach: the box's width alone
                            // cannot say whether a short clip is a trim or a
                            // clip at 4x, and that is the difference between a
                            // cut and a re-time.
                            // Against the right edge of what is *visible* of the
                            // box, not of the box: zoomed in, the box's own
                            // right edge is off the screen and the badge with
                            // it, which is a clip that stops saying it is
                            // speeded exactly when it is being looked at
                            // closely.
                            .when(!clip.speed.is_normal() && vis_w > 0., |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(px(vis_x))
                                        .w(px(vis_w))
                                        .flex()
                                        .justify_end()
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .flex_none()
                                                .px(px(3.))
                                                .rounded(px(3.))
                                                .bg(rgb(ACCENT_PRIMARY()))
                                                .text_size(px(9.))
                                                .text_color(rgb(BG_RAISED()))
                                                .child(format!("{}", clip.speed)),
                                        ),
                                )
                            })
                            // ...and the name sits at the left edge of the same
                            // slice, for the same reason: a box scrolled half
                            // off names itself on the half that is on screen.
                            .when_some(label.filter(|_| show_label(vis_w)), |d, label| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(px(vis_x))
                                        .w(px(vis_w))
                                        .h(px(LABEL_H))
                                        .px(px(4.))
                                        .truncate()
                                        .text_size(px(10.))
                                        .child(label),
                                )
                            })
                    }))
                    // What the silence card found, over the clips it found them
                    // in and over the waveform band that shows why: on the lane
                    // the scan ran on and no other, because that is the only
                    // lane whose sound was read. Drawn before anything is cut,
                    // and replaced -- never stacked -- by every re-run.
                    .children(
                        self.silence_marks
                            .iter()
                            .filter(|_| self.silence_open.is_some_and(|(on, _)| on == lane))
                            .map(|&(at, len)| {
                                div()
                                    .absolute()
                                    .top_0()
                                    .h_full()
                                    .left(px(scale.px_at(f64::from(at) / self.fps)))
                                    // Floored like a cue's mark and for the same
                                    // reason: a half-second silence on a zoomed-
                                    // out bed rounds to nothing, and a preview
                                    // that draws nothing reads as a scan that
                                    // found nothing.
                                    .w(px(scale
                                        .width_px(f64::from(len) / self.fps)
                                        .max(SUB_CUE_MIN_W)))
                                    .bg(rgba(ACCENT_WASH()))
                            }),
                    )
                    // Where the thing in the hand would come to rest, at the size
                    // it would come to rest at: the shadow a proper editor draws
                    // under a drag. Over the clips (it is translucent, so what
                    // it would cover shows through) and under the line, which
                    // marks the frame this box merely fills.
                    .children(ghost.map(|g| {
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(scale.px_at(f64::from(g.start) / self.fps)))
                            // A row whose length the engine has not measured
                            // draws a head marker rather than nothing: where it
                            // lands is known, how long it is is not.
                            .w(px(scale
                                .width_px(f64::from(g.frames) / self.fps)
                                .max(GHOST_MIN)))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(rgb(if g.refused { DROP_REFUSE() } else { FG_PRIMARY() }))
                            // The file's own swatch at a third of its weight, so
                            // the box beneath is still legible through it -- and
                            // the refusal red instead, for a lane that will not
                            // take this drop at all.
                            .bg(rgba(
                                ((if g.refused { DROP_REFUSE() } else { g.tint }) << 8) | GHOST_ALPHA,
                            ))
                    }))
                    // What the gesture in flight is about to land on, drawn on
                    // every lane so a clip lining up with a take one track over
                    // can be seen to line up with it. Under the playhead's line
                    // and in another colour, since the two mean different
                    // things and often stand on the same pixel.
                    .children(cue.map(|x| {
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(x))
                            .w(px(1.))
                            .bg(rgb(FG_PRIMARY()))
                    }))
                    // Last, so it is over the clips: the same fraction in both
                    // lanes, which is the playhead being one line.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(filled))
                            .w(px(2.))
                            .bg(rgb(ACCENT_PLAYHEAD())),
                    ),
            )
            // The drop indicator, last of all so it is over the header and the
            // bed both: a line along the edge the track in the hand comes in
            // at, which is the slot the release commits to. *Inside* the row
            // rather than in the gutter between two, because the lane column
            // scrolls ([`Player::lanes_scroll`]) and anything drawn past the
            // first row's top edge is clipped away by it -- an indicator that
            // vanished on exactly the drop everybody tries first.
            .children(drop.map(|d| {
                let line = div()
                    .absolute()
                    .left_0()
                    .w_full()
                    .h(px(3.))
                    .bg(rgb(ACCENT_PRIMARY()));
                match d.above {
                    true => line.top_0(),
                    false => line.bottom_0(),
                }
            }))
    }
}
