//! The export panel (DESIGN §9): the dock's third tab, Media | Clip | Export.
//! One panel, four faces -- the settings, the overwrite question inside them,
//! the running export and the finished (or failed) file. It is in flow in the
//! dock like every other tab, so nothing floats over the viewer or the lanes.

use super::common::*;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{self, Button, ButtonSize, ButtonStyle};
use crate::ui::theme::tokens;
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// A clickable surface on the sheet.
type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

impl Player {
    /// The export panel (DESIGN §9): presets first, then the range, the
    /// destination and a plain-words summary, with every detailed setting one
    /// closed "More settings" disclosure away -- simplified, never hidden.
    /// While an export runs the same panel is its progress, and when it lands
    /// it offers the file.
    pub(crate) fn export_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let folder = dest_dir_text(&self.export_path);
        let face: AnyElement = match (self.exporting(), &self.export_sheet.done, &self.export_sheet.failed) {
            (Some(export), ..) => self.sheet_progress(export, cx).into_any_element(),
            (None, Some(done), _) => sheet_done(done, cx).into_any_element(),
            (None, None, Some(why)) => sheet_failed(why, &folder, cx).into_any_element(),
            (None, None, None) if self.export_sheet.cancelled => sheet_cancelled(cx).into_any_element(),
            _ => self.sheet_settings(cx).into_any_element(),
        };
        // The settings face carries the queue inside its own scroll; the other faces (progress,
        // done, failed, cancelled) keep it under them, so a person who left the sheet up still sees it.
        let settings_face = self.exporting().is_none()
            && self.export_sheet.done.is_none()
            && self.export_sheet.failed.is_none()
            && !self.export_sheet.cancelled;
        div()
            .id("export-panel")
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(face)
            .when(!settings_face && !self.queue.jobs.is_empty(), |d| {
                d.child(
                    div()
                        .id("export-queue-foot")
                        .flex_none()
                        .max_h(px(260.))
                        .overflow_y_scroll()
                        .border_t_1()
                        .border_color(rgba(tokens().line))
                        .px(px(8.))
                        .py(px(8.))
                        .child(self.sheet_queue(cx)),
                )
            })
            .into_any_element()
    }

    /// The queue: one row per job and the three controls. Queued jobs keep the
    /// timeline as it was when they were added, and the section says so.
    fn sheet_queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::export_queue::JobState;
        let t = tokens();
        let q = &self.queue;
        let going = q.is_going();
        let (go_label, go_why) = match (going, q.waiting()) {
            (true, _) => ("Pause after this job", None),
            (false, 0) => ("Start queue", Some("Nothing is waiting")),
            (false, _) => ("Start queue", None),
        };
        let go_btn = Button::new("queue-go")
            .label(go_label)
            .style(if going { ButtonStyle::Secondary } else { ButtonStyle::Primary })
            .size(ButtonSize::Default)
            .enabled(go_why.is_none())
            .build()
            .tooltip(crate::ui::widgets::overlay_tip_hover(go_label, "", go_why));
        let go_btn = go_btn.when(go_why.is_none(), |b| {
            b.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| match going {
                true => this.queue_pause(cx),
                false => this.queue_start(cx),
            }))
        });
        let clear_why = (q.finished() == 0).then_some("No finished exports to clear");
        let clear_btn = Button::new("queue-clear")
            .label("Clear finished")
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Default)
            .enabled(clear_why.is_none())
            .build()
            .tooltip(crate::ui::widgets::overlay_tip_hover("Clear finished", "", clear_why));
        let clear_btn = clear_btn.when(clear_why.is_none(), |b| {
            b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.queue_clear_finished(cx)))
        });
        let rows = q.jobs.iter().map(|job| {
            let id = job.id;
            let (state_text, state_color) = match &job.state {
                JobState::Waiting => ("Waiting".to_string(), t.ink2),
                JobState::Running if job.cancelling => ("Stopping\u{2026}".to_string(), t.ink2),
                JobState::Running => {
                    let p = job.handle.as_ref().map_or(0., ExportHandle::progress).clamp(0., 1.);
                    (format!("Exporting {}%", (p * 100.) as u32), t.ink1)
                }
                JobState::Done { bytes, .. } => (format!("Done \u{b7} {}", bytes_words(*bytes)), t.ok),
                JobState::Failed(why) => (format!("Failed \u{b7} {why}"), t.warn),
                JobState::Cancelled => ("Cancelled".to_string(), t.ink3),
            };
            let progress = match (&job.state, &job.handle) {
                (JobState::Running, Some(h)) => Some(h.progress().clamp(0., 1.)),
                _ => None,
            };
            let (tip, icon_id) = match job.state {
                JobState::Running => ("Cancel this export", "queue-cancel"),
                JobState::Waiting => ("Remove from the queue", "queue-remove"),
                _ => ("Remove from the list", "queue-remove"),
            };
            let running = job.state == JobState::Running;
            let waiting = job.state == JobState::Waiting;
            let done = matches!(job.state, JobState::Done { .. });
            div()
                .id(("queue-row", id as usize))
                .flex()
                .flex_col()
                .gap(px(2.))
                .p(px(8.))
                .rounded(px(6.))
                .bg(rgb(t.e2))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(type_scale::body_strong())
                                .text_color(rgb(t.ink1))
                                .child(file_name(&job.out)),
                        )
                        .child(
                            div()
                                .id((icon_id, id as usize))
                                .flex_none()
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(t.e3)))
                                .tooltip(crate::ui::widgets::overlay_tip_hover(tip, "", None))
                                .child(Icon::X.el(IconSize::Inline, t.ink2))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| match running {
                                    true => this.queue_cancel(id, cx),
                                    false => this.queue_remove(id, cx),
                                })),
                        ),
                )
                .child(dark_help(job.summary.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .id(("queue-state", id as usize))
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(type_scale::caption())
                                .text_color(rgb(state_color))
                                .tooltip(crate::ui::widgets::overlay_tip_hover(&state_text, "", None))
                                .child(state_text.clone()),
                        )
                        .when(done, |d| {
                            d.child(
                                div()
                                    .id(("queue-show", id as usize))
                                    .flex_none()
                                    .type_style(type_scale::caption())
                                    .text_color(rgb(t.accent))
                                    .cursor_pointer()
                                    .child("Show in folder")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.queue_show_in_folder(id, cx))),
                            )
                        }),
                )
                // Always there, so a row keeps its height from Waiting to Done.
                .child(
                    div()
                        .h(px(4.))
                        .rounded(px(2.))
                        .when(waiting || running, |d| d.bg(rgb(t.e3)))
                        .children(progress.map(|p| div().h_full().w(relative(p)).rounded(px(2.)).bg(rgb(t.accent)))),
                )
        });
        div()
            .id("export-queue")
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().type_style(type_scale::section()).text_color(rgb(t.ink2)).child(format!("Queue ({})", q.jobs.len())))
            .child(dark_help("Queued exports use the timeline as it was when they were added."))
            .children(rows)
            .child(div().flex().items_center().gap(px(8.)).child(go_btn).child(clear_btn))
    }

    /// The settings face, simple layer first: the preset, where it goes, the
    /// range and a two-line summary of what will be written. Every detailed
    /// setting is one closed "Details" disclosure away -- simplified, never
    /// hidden -- and the Export button is pinned under the scrolling body.
    fn sheet_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let blocked = self
            .session
            .as_ref()
            .and_then(|s| format_refusal(s, self.format))
            .map(|why| why.split(" \u{2014} ").next().unwrap_or("cannot export").to_string());
        let more = self.folds.contains(EXPORT_MORE_FOLD);
        let off = self.export_block_reason().or_else(|| self.export_source_clash());
        let missing = self.export_missing();
        let body = div()
            .id("export-panel-body")
            .track_scroll(&self.export_scroll)
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.export_body_scroll)
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(8.))
            .py(px(8.))
            .child(self.sheet_preset(cx))
            .child(self.sheet_destination(cx))
            .child(self.sheet_range(cx))
            .children((!missing.is_empty()).then(|| sheet_missing(&missing, cx)))
            .child(self.sheet_summary(blocked.as_deref()))
            .children((!self.queue.jobs.is_empty()).then(|| self.sheet_queue(cx)))
            .child(more_toggle(more, cx))
            .children(more.then(|| self.sheet_more(cx)));
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .child(body)
                    .child(crate::ui::widgets::more_below(self.export_body_scroll.clone())),
            )
            .child(match self.export_ask {
                true => self.sheet_overwrite(cx).into_any_element(),
                false => sheet_footer(off, self, cx).into_any_element(),
            })
    }

    /// Why the export would write over one of the project's own sources, if it
    /// would: the Export button is off and says so, and no question is asked.
    fn export_source_clash(&self) -> Option<String> {
        self.is_source_path(&self.export_path).then(|| SOURCE_NAME_TAKEN.to_string())
    }

    /// The preset: one dropdown in the shared popup look. Lit as "Custom (from
    /// X)" whenever the settings match no preset, so a change is never silent.
    fn sheet_preset(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let text = preset_box_text(self.preset_now(), self.export_sheet.preset);
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(section("Preset"))
            .child(drop_box("export-preset", text, Some(Self::pick_open(ExportRow::Preset, cx))))
    }

    /// The click that opens one Details list where it was pressed.
    fn pick_open(row: ExportRow, cx: &mut Context<Self>) -> OnClick {
        Box::new(cx.listener(move |this, e: &ClickEvent, _, cx| {
            this.picker_at_control(Pick::Export(row), e.position(), cx);
        }))
    }

    /// The range: the whole timeline or the marks, as two segments with one
    /// dim line under them (always there, so nothing moves); the marks dim
    /// with the reason when they cannot be used.
    fn sheet_range(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let frames = self
            .session
            .as_ref()
            .map_or(0, |s| crate::subs::frame_at(s.timeline_duration(), self.fps));
        let valid = self.export_range();
        let opts = range_options(valid, self.range.is_some(), frames, self.fps);
        let on_marks = self.export_span().is_some();
        let note = match (&opts.marks, on_marks) {
            (Some(span), true) => span.clone(),
            _ => opts.whole.clone(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(section("Range"))
            .child(
                seg_track()
                    .child(seg(
                        "export-range-whole",
                        "Whole timeline",
                        !on_marks,
                        None,
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.export_sheet.whole = true;
                            cx.notify();
                        })),
                    ))
                    .child(seg(
                        "export-range-marks",
                        "In\u{2013}Out",
                        on_marks,
                        match valid {
                            Some(_) => None,
                            None => Some(opts.marks_off.unwrap_or("Set In and Out on the timeline first").to_string()),
                        },
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if this.export_range().is_some() {
                                this.export_sheet.whole = false;
                            }
                            cx.notify();
                        })),
                    )),
            )
            .child(dim_line(note))
    }

    /// The destination: the file's name with its extension beside it, and the
    /// folder shortened (the full path on hover) with a Change button for the
    /// desktop's own dialog. Both fields can still be typed into.
    fn sheet_destination(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let typing = self.export_sheet.field.as_ref();
        // How many characters of the dock's width a field shows; a longer
        // path is cut on its left, so the folder you are in stays in view.
        let dock = f32::from_bits(crate::ui::widgets::DOCK_W_BITS.load(std::sync::atomic::Ordering::Relaxed));
        let dock = if dock > 0. { dock } else { 320. };
        let chars = (((dock - 16. - 64. - 14. - 34. - 20.) / 7.4) as usize).clamp(12, 80);
        let shown = |field: SheetField, text: String| match typing {
            Some((f, t)) if *f == field => (format!("{}\u{258f}", tail_of(t, chars)), true),
            _ => (text, false),
        };
        let stem = self
            .export_path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        // The typed name, checked while it is typed: a source's name is said under the field.
        let typed_clash = match typing {
            Some((SheetField::Name, text)) => parse_stem(text).is_ok_and(|stem| {
                self.is_source_path(&self.export_path.with_file_name(crate::export_ui::typed_name(&stem, self.format)))
            }),
            _ => false,
        };
        let (name, name_on) = shown(SheetField::Name, stem);
        let full = dest_dir_text(&self.export_path);
        let (folder, folder_on) = shown(SheetField::Folder, tail_of(&full, chars));
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(section("Save as"))
            .child(field_row(
                "export-name",
                "Name",
                name,
                (self.format != Format::PngSequence).then(|| format!(".{}", self.format.ext())),
                name_on,
                Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.begin_field(SheetField::Name);
                    cx.notify();
                })),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("export-folder-wrap")
                            .flex_1()
                            .min_w(px(0.))
                            .tooltip(crate::ui::widgets::overlay_tip_hover(&full, "", None))
                            .child(field_row(
                                "export-folder",
                                "Folder",
                                folder,
                                None,
                                folder_on,
                                Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.begin_field(SheetField::Folder);
                                    cx.notify();
                                })),
                            )),
                    )
                    .child(
                        Button::new("export-destination")
                            .icon(Icon::FolderOpen)
                            .style(ButtonStyle::Ghost)
                            .size(ButtonSize::Default)
                            .build()
                            .tooltip(crate::ui::widgets::overlay_tip_hover("Choose folder\u{2026}", "", None))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_destination(cx))),
                    ),
            )
            // A typed name or folder that was refused: why, under the fields, until it is fixed.
            .children(match typed_clash {
                true => Some(warn_line(SOURCE_NAME_TAKEN.to_string())),
                false => self.export_sheet.note.clone().map(warn_line),
            })
    }

    /// What will be written, in plain words: the file kind, codec, size, rate
    /// and bitrate; then the sound, the encoder and the size on disk; then how
    /// long it takes. The warnings and the reason the button is off sit under.
    fn sheet_summary(&self, blocked: Option<&str>) -> impl IntoElement {
        let seconds = self.export_seconds();
        let sound = self.export_has_sound();
        let target = self.export_canvas();
        let project = self.session.as_ref().map_or(target, PlaybackSession::resolution);
        let rate = self
            .session
            .as_ref()
            .and_then(PlaybackSession::sample_rate)
            .unwrap_or(48_000);
        // A GPU seat writes AV1 to a quality and not to the budget, so the
        // picture's part of the size is a span there.
        let seat = self.encoder_seat();
        let hardware = self
            .export_seat
            .as_ref()
            .and_then(|(.., seats)| *seats)
            .and_then(|(video, _)| video)
            .is_some_and(|seat| seat.contains("VA-API") || seat.contains("GPU"));
        let video_fixed = seat == EncoderSeat::Software || !hardware;
        let size = size_estimate(
            self.format,
            self.budget_bps(),
            self.audio_kbps,
            sound,
            seconds,
            rate,
            video_fixed,
        );
        let time = export_time_words(seconds, self.sheet_pace(), self.pace_pending());
        let size_text = size_words(size);
        let rows = summary_rows(&SummaryIn {
            format: self.format,
            target,
            fps: self.fps,
            bps: self.budget_bps(),
            audio_kbps: self.audio_kbps,
            sound,
            seat,
            depth: self.export_sheet.bit_depth,
            prores: self.export_sheet.prores,
            hdr: self.export_sheet.hdr,
            hevc_speed: self.export_sheet.hevc_speed,
            sound_bits: self.export_sheet.sound_bits,
            size: &size_text,
            gif: self.export_sheet.gif,
        }, &time_short(&time));
        let warns = warnings(&WarnCtx {
            format: self.format,
            target,
            project,
            fps: self.fps,
            video_bps: self.budget_bps(),
            timeline_has_picture: self.export_has_picture(),
            timeline_has_sound: sound,
            seconds,
        });
        // WebM carries no subtitle track: the engine's own reason, said here.
        let subtitle_note = self
            .session
            .as_ref()
            .filter(|_| self.format == Format::Webm)
            .map(|s| s.planned_subtitles(self.format, 0..s.subtitles().len()))
            .filter(|why| why != "none");
        div()
            .id("export-summary")
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(section("Summary"))
            .child(
                div()
                    .id("export-summary-box")
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .p(px(12.))
                    .rounded(px(6.))
                    .bg(rgb(tokens().e2))
                    .children(rows.into_iter().map(|(label, text)| summary_row(label, text))),
            )
            // One reserved line: a warning shows here and never moves anything.
            .child(
                div()
                    .id("export-warning")
                    .h(px(18.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .type_style(type_scale::caption())
                    .text_color(rgb(tokens().warn))
                    .when(warns.len() > 1, |d| {
                        d.tooltip(crate::ui::widgets::overlay_tip_hover("Notes", "", Some(&warns.join(". "))))
                    })
                    .child(warns.first().cloned().unwrap_or_default()),
            )
            .children(subtitle_note.map(warn_line))
            .children(blocked.map(|why| {
                warn_line(format!("Cannot export: {why}. Pick another format under Details."))
            }))
    }

    /// The Details: grouped, labelled rows, each a direct choice. A codec's own
    /// rows (quality, bit depth, HDR, speed) are only listed for the codecs
    /// they can change.
    fn sheet_more(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let video = self.format.has_video();
        let mut col = div().id("export-more").flex().flex_col().gap(px(10.));
        // Video.
        let seat = self.encoder_seat();
        let budget = match &self.budget_edit {
            // Empty, the field shows its example, and the caret sits before it.
            Some(text) if text.is_empty() => "\u{258f}e.g. 8 Mb/s".to_string(),
            Some(text) => format!("{text}\u{258f} Mb/s"),
            None => format!("{} Mb/s", rate_label(self.budget_bps()).trim_end_matches(".0")),
        };
        let size = match self.export_sheet.size {
            Some((w, h)) => format!("{w}\u{d7}{h}"),
            None => self
                .session
                .as_ref()
                .map_or("Same as project".to_string(), |s| {
                    let (w, h) = s.resolution();
                    format!("{w}\u{d7}{h} (project)")
                }),
        };
        let av1 = matches!(self.format, Format::Av1 | Format::Av1Mp4 | Format::Webm);
        let prores = self.format == Format::ProRes;
        let hevc = self.format.is_hevc();
        let depth = match self.export_sheet.bit_depth {
            Some(d) => format!("{d}-bit"),
            None => "Same as source".to_string(),
        };
        // A sound-only file has no picture settings at all, so the group is
        // just the file kind; a video file lists the picture settings it has.
        col = col
            .child(sub_head(if video { "Video" } else { "Format" }))
            .child(detail_row("export-d-container", "Container", drop_box("export-c-container", container_word(self.format).into(), Some(Self::pick_open(ExportRow::Container, cx))), None))
            .child(detail_row("export-d-codec", "Codec", drop_box("export-c-codec", codec_word(self.format).into(), Some(Self::pick_open(ExportRow::Codec, cx))), None));
        if video {
            col = col
                .child(detail_row("export-d-seat", "Encoder", drop_box("export-c-seat", encoder_label(seat).into(), Some(Self::pick_open(ExportRow::Seat, cx))), None))
                .child(dim_line(seat_plain(seat).to_string()))
                .child(detail_row(
                    "export-d-bitrate",
                    "Bitrate",
                    drop_box("export-c-bitrate", budget, (!prores).then(|| Self::pick_open(ExportRow::Bitrate, cx))),
                    prores.then(|| "ProRes sets its own rate: pick a quality".to_string()),
                ))
                .child(detail_row("export-d-size", "Resolution", drop_box("export-c-size", size, Some(Self::pick_open(ExportRow::Size, cx))), None))
                .child(detail_row("export-d-fps", "Frame rate", info_note(format!("{} fps", fps_label(self.fps)), "follows the project"), None));
            // A row only shows for a codec it can change: a setting with no
            // possible value for this file is noise, so it is left out, not dimmed.
            if prores {
                col = col.child(detail_row(
                    "export-d-profile",
                    "Quality",
                    drop_box("export-c-profile", self.export_sheet.prores.name().into(), Some(Self::pick_open(ExportRow::Profile, cx))),
                    None,
                ));
            }
            if av1 || hevc || prores {
                col = col.child(detail_row(
                    "export-d-depth",
                    "Bit depth",
                    drop_box(
                        "export-c-depth",
                        if prores { "Always 10-bit".to_string() } else { depth },
                        (!prores).then(|| Self::pick_open(ExportRow::Depth, cx)),
                    ),
                    prores.then(|| "ProRes is always 10-bit".to_string()),
                ));
            }
            if hevc {
                col = col
                    .child(detail_row(
                        "export-d-hdr",
                        "HDR",
                        drop_box("export-c-hdr", hdr_row_word(self.export_sheet.hdr).into(), Some(Self::pick_open(ExportRow::Hdr, cx))),
                        None,
                    ))
                    .child(self.speed_row(seat, cx))
                    .child(dim_line(match self.hevc_on_gpu(seat) {
                        true => "Only for the software encoder".to_string(),
                        false => hevc_speed_plain(self.export_sheet.hevc_speed).to_string(),
                    }));
            }
        }
        // Pictures and no sound: no encoder and no audio rows, only the picture's own settings.
        if is_picture_set(self.format) {
            if self.format == Format::Gif {
                let looping = if self.export_sheet.gif.loop_forever { "Loop forever" } else { "Play once" };
                col = col
                    .child(sub_head("GIF"))
                    .child(detail_row(
                        "export-d-gifwidth",
                        "Width",
                        drop_box("export-c-gifwidth", format!("{} px", self.export_sheet.gif.width), Some(Self::pick_open(ExportRow::GifWidth, cx))),
                        None,
                    ))
                    .child(detail_row(
                        "export-d-giffps",
                        "Frame rate",
                        drop_box("export-c-giffps", format!("{} fps", self.export_sheet.gif.fps), Some(Self::pick_open(ExportRow::GifFps, cx))),
                        None,
                    ))
                    .child(detail_row(
                        "export-d-gifloop",
                        "Loop",
                        drop_box("export-c-gifloop", looping.to_string(), Some(Self::pick_open(ExportRow::GifLoop, cx))),
                        None,
                    ));
            }
            return col.child(dim_line("These settings are remembered for next time.".to_string()));
        }
        // Audio.
        let rated = self.audio_rate_refusal().is_none();
        let rate = self.session.as_ref().and_then(PlaybackSession::sample_rate).unwrap_or(48_000);
        let loud = match self.export_sheet.loudness {
            None => "Off".to_string(),
            Some(t) => LOUDNESS_CHOICES
                .iter()
                .find(|&&(_, lufs, peak)| t.lufs == lufs && t.true_peak_dbtp == peak)
                .map_or_else(|| format!("{} LUFS", t.lufs), |&(label, ..)| label.to_string()),
        };
        let lossless = matches!(self.format, Format::Wav | Format::Flac);
        col = col
            .child(sub_head("Audio"))
            .child(detail_row("export-d-acodec", "Codec", info_note(sound_codec_word(self.format).to_string(), "set by the file kind"), None));
        // A lossless codec has no bitrate to choose.
        if !lossless {
            // Sound that is copied or coded by quality has no rate to pick: the row
            // says why in plain words, both on hover and in a caption under it.
            let why = self.audio_rate_refusal().map(audio_rate_words);
            col = col.child(detail_row(
                "export-d-kbps",
                "Bitrate",
                drop_box("export-c-kbps", format!("{} kb/s", self.audio_kbps), rated.then(|| Self::pick_open(ExportRow::Kbps, cx))),
                why.clone(),
            ));
            col = col.children(why.map(dim_line));
        }
        col = col.child(detail_row("export-d-rate", "Sample rate", info_note(format!("{} kHz", f64::from(rate) / 1000.), "follows the project"), None));
        if prores {
            col = col.child(detail_row(
                "export-d-soundbits",
                "Sound depth",
                drop_box(
                    "export-c-soundbits",
                    format!("{}-bit", self.export_sheet.sound_bits.unwrap_or(24)),
                    Some(Self::pick_open(ExportRow::SoundDepth, cx)),
                ),
                None,
            ));
        } else if self.format == Format::Wav {
            col = col.child(detail_row("export-d-soundbits", "Sound depth", info_note("16-bit".to_string(), "set by the file kind"), None));
        }
        col = col
            .child(detail_row("export-d-channels", "Channels", info_note("Stereo".to_string(), "the mix is stereo"), None))
            .child(detail_row("export-d-loud", "Loudness", drop_box("export-c-loud", loud, Some(Self::pick_open(ExportRow::Loudness, cx))), None));
        // Extras.
        let chapters = self.session.as_ref().map_or(0, |s| s.chapters().len());
        let tracks = self.export_subs().len();
        let captions = match (tracks, self.export_sheet.no_captions) {
            (_, true) => "Leave out",
            _ => "Keep as a track",
        };
        // The chapters as description text ("00:00 Intro" lines), for a video's description.
        let copy_chapters = kit::Button::new("export-copy-chapters")
            .label("Copy chapters as text")
            .style(ButtonStyle::Secondary)
            .enabled(chapters > 0)
            .build()
            .tooltip(crate::ui::widgets::overlay_tip_hover(
                "Copy chapters as text",
                "",
                (chapters == 0).then_some("Add chapters first: timeline markers"),
            ))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_chapters(cx)));
        col = col.child(sub_head("Extras")).child(detail_row(
            "export-d-chapters",
            "Chapters",
            info_note(
                match chapters {
                    0 => "None".to_string(),
                    n => n.to_string(),
                },
                "from timeline markers",
            ),
            None,
        ));
        col = col.child(detail_row("export-d-chapters-copy", "", copy_chapters, None));
        // No captions on the timeline: there is nothing to keep or leave out.
        if tracks > 0 {
            col = col.child(detail_row(
                "export-d-captions",
                "Captions",
                drop_box("export-c-captions", captions.to_string(), Some(Self::pick_open(ExportRow::Captions, cx))),
                None,
            ));
        }
        col
        .child(dim_line("These settings are remembered for next time.".to_string()))
    }

    /// Whether the picture is written by the GPU: Hardware picked, or Auto with a seat found.
    fn hevc_on_gpu(&self, seat: EncoderSeat) -> bool {
        let found = self
            .export_seat
            .as_ref()
            .and_then(|(.., seats)| *seats)
            .and_then(|(video, _)| video)
            .is_some_and(|seat| seat.contains("VA-API") || seat.contains("GPU"));
        seat == EncoderSeat::Hardware || (seat == EncoderSeat::Auto && found)
    }

    /// Encoding speed for an HEVC file: a segmented control, disabled with the
    /// reason on hover when the GPU encodes it.
    fn speed_row(&self, seat: EncoderSeat, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let gpu = self.hevc_on_gpu(seat);
        let segments: Vec<kit::Segment> = HEVC_SPEEDS.iter().map(|&s| kit::Segment::text(hevc_speed_label(s))).collect();
        let at = HEVC_SPEEDS.iter().position(|&s| s == self.export_sheet.hevc_speed).unwrap_or(0);
        let me = cx.entity();
        let on_select = move |i: usize, _: &mut Window, cx: &mut App| {
            if let Some(&s) = HEVC_SPEEDS.get(i) {
                me.update(cx, |this, cx| this.apply_export_pick(ExportPick::HevcSpeed(s), cx));
            }
        };
        detail_row(
            "export-d-speed",
            "Speed",
            kit::segmented_equal("export-c-speed", &segments, at, !gpu, None, on_select),
            gpu.then(|| "Only for the software encoder".to_string()),
        )
    }

    /// "That file already exists": three answers as buttons, in the footer's
    /// place, so the question sits where the press that raised it was.
    fn sheet_overwrite(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("export-overwrite")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(8.))
            .py(px(12.))
            .border_t_1()
            .border_color(rgba(tokens().line))
            .child(
                div()
                    .type_style(type_scale::body_strong())
                    .text_color(rgb(tokens().warn))
                    .child(format!(
                        "{} already exists in this folder.",
                        file_name(&self.export_path)
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(sheet_button(
                        "export-overwrite-cancel",
                        "Cancel",
                        Some("esc"),
                        false,
                        true,
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.answer_overwrite(Overwrite::Cancel, cx);
                        })),
                    ))
                    .child(sheet_button(
                        "export-overwrite-keep",
                        "Keep both",
                        Some("k"),
                        false,
                        true,
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.answer_overwrite(Overwrite::KeepBoth, cx);
                        })),
                    ))
                    .child(sheet_button(
                        "export-overwrite-replace",
                        "Replace",
                        Some("\u{21b5}"),
                        true,
                        true,
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.answer_overwrite(Overwrite::Replace, cx);
                        })),
                    )),
            )
    }

    /// The running export: the bar, the two clocks, the route, and the way
    /// out -- Keep editing closes the sheet and the export carries on (the
    /// Export chip and the status bar keep its percentage); Cancel is two
    /// presses, never one.
    fn sheet_progress(&self, export: &ExportHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let progress = export.progress().clamp(0., 1.);
        let elapsed = self.export_started.map_or(0., |t| t.elapsed().as_secs_f32());
        // A stage before the encode (a stabilised clip's shake being measured)
        // fills the bar with its own fraction and says its name; the encode's
        // guess at what is left means nothing until it starts.
        let stage = export.stage();
        // The clock that counts up is measured; the one that counts down is a
        // rolling estimate and says so until it has one.
        let left = eta_secs(&self.export_marks, elapsed, progress)
            .map_or_else(|| "estimating\u{2026}".to_owned(), |s| format!("about {} left", left_words(s)));
        let percent_line = progress_words(stage, progress, &left);
        let progress = stage.map_or(progress, |(_, f)| f.clamp(0., 1.));
        let armed = self.cancel_armed;
        // The engine's seat names are jargon; the only part worth saying is
        // its notice about frames past the last picture.
        let route = plain_route(sheet_format_name(self.format), export.encoders().as_deref());
        let t = tokens();
        div()
            .id("export-progress")
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(8.))
            .py(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .type_style(type_scale::title())
                    .text_color(rgb(t.ink1))
                    .child(Icon::Upload.el(IconSize::Toolbar, t.accent))
                    .child("Exporting"),
            )
            .child(
                div()
                    .truncate()
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink2))
                    .child(file_name(&self.export_path)),
            )
            .child(
                // The bar only moves forward: the worker's progress is monotone.
                div().h(px(6.)).rounded(px(3.)).bg(rgb(t.e3)).child(
                    div()
                        .h_full()
                        .w(relative(progress))
                        .rounded(px(3.))
                        .bg(rgb(t.accent)),
                ),
            )
            .child(
                div()
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(percent_line),
            )
            .child(dark_help(route))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .pt(px(6.))
                    .when(armed, |d| {
                        d.child(sheet_button(
                            "export-keep",
                            "Keep exporting",
                            None,
                            false,
                            true,
                            Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cancel_armed = false;
                                cx.notify();
                            })),
                        ))
                    })
                    .child(sheet_button(
                        "export-cancel",
                        if armed { "Yes, cancel export" } else { "Cancel export" },
                        None,
                        false,
                        true,
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            match this.cancel_armed {
                                true => this.cancel_export(),
                                false => this.cancel_armed = true,
                            }
                            cx.notify();
                        })),
                    ))
            )
    }
}

/// The settings face's foot: one full-width Export button.
fn sheet_footer(off: Option<String>, player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let enabled = off.is_none();
    div()
        .flex_none()
        .px(px(8.))
        .py(px(8.))
        .border_t_1()
        .border_color(rgba(tokens().line))
        .flex()
        .flex_col()
        .gap(px(6.))
        // Why the button is off, beside it and in words (the tooltip says it too).
        .children(off.as_ref().map(|why| {
            div()
                .id("export-off-why")
                .type_style(type_scale::caption())
                .text_color(rgb(tokens().ink2))
                .child(why.clone())
        }))
        .child({
            let key = player.keymap.chord(ActionId::Export);
            let queued = player.queue.is_going();
            let b = Button::new("export-confirm")
                .label("Export")
                .style(ButtonStyle::Primary)
                .size(ButtonSize::Hero)
                .enabled(enabled)
                .build()
                .flex_1()
                .tooltip(crate::ui::widgets::overlay_tip_hover(
                    if queued { "Add to the end of the queue" } else { "Export" },
                    key.as_ref(),
                    off.as_deref(),
                ));
            let q = Button::new("export-enqueue")
                .label("Add to queue")
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Hero)
                .enabled(enabled)
                .build()
                .flex_1()
                .tooltip(crate::ui::widgets::overlay_tip_hover(
                    "Add to queue",
                    "",
                    off.as_deref(),
                ));
            div()
                .flex()
                .gap(px(8.))
                .child(b.when(enabled, |b| {
                    b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_export(cx)))
                }))
                .child(q.when(enabled, |b| {
                    b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.enqueue_export(cx)))
                }))
        })
}

/// The calm warning for files the chosen range reads that are not on disk: how many, a Relink
/// button for the first, and each one by name.
fn sheet_missing(missing: &[PathBuf], cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let first = missing[0].clone();
    div()
        .id("export-missing")
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .type_style(type_scale::body())
                        .text_color(rgb(t.warn))
                        .child(match missing.len() {
                            1 => "1 file missing".to_string(),
                            n => format!("{n} files missing"),
                        }),
                )
                .child(chip(
                    "export-relink",
                    "Relink\u{2026}".to_string(),
                    false,
                    Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| this.relink_pick(first.clone(), cx))),
                )),
        )
        .children(missing.iter().take(4).map(|p| {
            div()
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink3))
                .child(file_name(p))
        }))
        .children((missing.len() > 4).then(|| {
            div().type_style(type_scale::caption()).text_color(rgb(t.ink3)).child(format!("and {} more", missing.len() - 4))
        }))
}

/// The finished file: where it is, how big, how long it took, and the two
/// things a person does next.
fn sheet_done(done: &ExportDone, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let took = match done.wall_secs {
        w if w < 1. => "Done in under a second".to_string(),
        w if w < 60. => format!("Done in {} s", w.round() as u64),
        w => format!("Done in {}", left_words(w as f32)),
    };
    div()
        .id("export-done")
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(8.))
        .py(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .type_style(type_scale::title())
                .text_color(rgb(t.ink1))
                .child(Icon::CircleCheck.el(IconSize::Toolbar, t.ok))
                .child("Exported"),
        )
        .child(
            div()
                .truncate()
                .type_style(type_scale::body_strong())
                .text_color(rgb(t.ink1))
                .child(file_name(&done.path)),
        )
        .child(dark_help(format!("{} \u{b7} {took}", bytes_words(done.bytes))))
        .child(
            div()
                .id("export-done-folder")
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink3))
                .tooltip(crate::ui::widgets::overlay_tip_hover(
                    &dest_dir_text(&done.path),
                    "",
                    None,
                ))
                .child(saved_in(&dest_dir_text(&done.path))),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .pt(px(6.))
                .child(sheet_button(
                    "export-show-file",
                    "Show in folder",
                    None,
                    false,
                    true,
                    Box::new(cx.listener(|this, _: &ClickEvent, _, cx| this.show_export_in_folder(cx))),
                ))
                .child(sheet_button(
                    "export-play",
                    "Open",
                    None,
                    false,
                    true,
                    Box::new(cx.listener(|this, _: &ClickEvent, _, cx| this.play_export(cx))),
                ))
                .child(sheet_button(
                    "export-finish",
                    "Done",
                    Some("\u{21b5}"),
                    true,
                    true,
                    Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.leave_sheet_face();
                        cx.notify();
                    })),
                )),
        )
}

/// The folder a finished file landed in, in words: "Saved in Home" for the home folder.
fn saved_in(folder: &str) -> String {
    match folder {
        "~" => "Saved in Home".to_string(),
        f => format!("Saved in {}", short_folder(f)),
    }
}

/// A cancelled export: said in the sheet, with the way back to the settings.
fn sheet_cancelled(cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    div()
        .id("export-cancelled")
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(8.))
        .py(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .type_style(type_scale::title())
                .text_color(rgb(t.ink1))
                .child(Icon::X.el(IconSize::Toolbar, t.ink2))
                .child("Export cancelled"),
        )
        .child(
            div()
                .type_style(type_scale::body())
                .text_color(rgb(t.ink2))
                .child("Nothing was kept."),
        )
        .child(
            div().flex().flex_wrap().gap(px(8.)).pt(px(6.)).child(sheet_button(
                "export-back",
                "Back to settings",
                None,
                true,
                true,
                Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.leave_sheet_face();
                    cx.notify();
                })),
            )),
        )
}

/// A failed export in plain words, with the raw error on hover; the way back is
/// Retry or the settings, both in the Export tab.
fn sheet_failed(why: &str, folder: &str, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("export-failed")
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(8.))
        .py(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .type_style(type_scale::title())
                .text_color(rgb(tokens().ink1))
                .child(Icon::TriangleAlert.el(IconSize::Toolbar, tokens().warn))
                .child("Export did not finish"),
        )
        .child(
            // The raw engine text is kept for the hover; the sheet says what to do.
            div()
                .id("export-failed-why")
                .type_style(type_scale::body())
                .text_color(rgb(tokens().ink2))
                .tooltip(crate::ui::widgets::overlay_tip_hover(why, "", None))
                .child(crate::export_ui::failure_words(why, folder)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .pt(px(6.))
                .child(sheet_button(
                    "export-retry",
                    "Retry",
                    None,
                    true,
                    true,
                    Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.export_sheet.failed = None;
                        this.start_export(cx);
                    })),
                ))
                .child(sheet_button(
                    "export-back",
                    "Back to settings",
                    None,
                    false,
                    true,
                    Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.leave_sheet_face();
                        cx.notify();
                    })),
                )),
        )
}

/// "Details": the one closed disclosure, remembered in the folds file.
fn more_toggle(open: bool, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    div()
        .id("export-more-toggle")
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(HIT_MIN))
        .cursor_pointer()
        .hover(|s| s.text_color(rgb(tokens().ink1)))
        .type_style(type_scale::section())
        .text_color(rgb(t.ink2))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            toggle_fold(&mut this.folds, EXPORT_MORE_FOLD);
            save_folds(&this.folds);
            cx.notify();
        }))
        .child(
            match open {
                true => Icon::ChevronDown,
                false => Icon::ChevronRight,
            }
            .el(IconSize::Inline, t.ink2),
        )
        .child("Details")
}

/// Time left in words: "6 s", "1 min 5 s" -- never a clock face.
pub(crate) fn length_words(secs: f32) -> String {
    left_words(secs)
}

fn left_words(secs: f32) -> String {
    let secs = secs.max(0.).round() as u64;
    match secs {
        0..=59 => format!("{secs} s"),
        _ => format!("{} min {} s", secs / 60, secs % 60),
    }
}

/// The running line in plain words: "42% \u{b7} about 12 s left" under the
/// heading, or the stage's own name while one runs before the encode.
pub(crate) fn progress_words(stage: Option<(&str, f32)>, progress: f32, left: &str) -> String {
    match stage {
        Some((name, f)) => format!("{name} \u{b7} {}%", (f.clamp(0., 1.) * 100.) as u32),
        None => format!("{}% \u{b7} {left}", (progress.clamp(0., 1.) * 100.) as u32),
    }
}

/// The line under the bar: the format, and the engine's truncation notice if
/// it has one; never the encoder's internal name.
pub(crate) fn plain_route(format_name: &str, encoders: Option<&str>) -> String {
    match encoders.and_then(|e| e.split(" \u{b7} ").find(|p| p.contains("past the last picture"))) {
        Some(notice) => format!("{format_name} \u{b7} {notice}"),
        None => format_name.to_string(),
    }
}

/// A folder shortened to its last two parts, `\u{2026}/Videos/exports`.
pub(crate) fn short_folder(dir: &str) -> String {
    let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0..=2 => dir.to_string(),
        n => format!("\u{2026}/{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// The last `max` characters of `text`, led by an ellipsis when something was cut: what a field
/// being typed into shows, so the caret is always in view.
pub(crate) fn tail_of(text: &str, max: usize) -> String {
    let n = text.chars().count();
    match n > max {
        true => format!("\u{2026}{}", text.chars().skip(n - (max - 1)).collect::<String>()),
        false => text.to_string(),
    }
}

fn section(text: &'static str) -> impl IntoElement {
    div().type_style(type_scale::section()).text_color(rgb(tokens().ink2)).child(text)
}

fn sub_head(text: &'static str) -> impl IntoElement {
    div().pt(px(4.)).type_style(type_scale::section()).text_color(rgb(tokens().ink2)).child(text)
}

fn warn_line(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .type_style(type_scale::caption())
        .text_color(rgb(tokens().warn))
        .child(text.into())
}

/// A destination field: its name, the text (with a caret while typed into),
/// and a fixed suffix such as the extension.
fn field_row(
    id: &'static str,
    label: &'static str,
    text: String,
    suffix: Option<String>,
    typing: bool,
    on_click: OnClick,
) -> Div {
    let t = tokens();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_none()
                .w(px(56.))
                .type_style(type_scale::body())
                .text_color(rgb(t.ink2))
                .child(label),
        )
        .child(
            div()
                .id(id)
                .flex_1()
                .min_w(px(0.))
                .h(px(28.))
                .flex()
                .items_center()
                .px(px(8.))
                .rounded(px(4.))
                .border_1()
                .border_color(match typing {
                    true => rgb(t.accent).into(),
                    false => gpui::hsla(0., 0., 0., 0.),
                })
                .bg(rgb(t.e2))
                .cursor_text()
                .hover(|s| s.bg(rgb(tokens().e3)))
                .on_click(on_click)
                .type_style(type_scale::body())
                .text_color(rgb(t.ink1))
                .child(div().min_w(px(0.)).truncate().child(text))
                .children(suffix.map(|s| div().flex_none().text_color(rgb(t.ink3)).child(s))),
        )
}

/// A dropdown box: the value in force and a chevron; a click opens its list
/// where it was pressed. `None` is a box that takes no click.
fn drop_box(id: &'static str, text: String, on_click: Option<OnClick>) -> gpui::Stateful<Div> {
    let t = tokens();
    let live = on_click.is_some();
    div()
        .id(id)
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .type_style(type_scale::body())
        .text_color(rgb(if live { t.ink1 } else { t.ink3 }))
        // A box that takes no click is not a field: plain muted text, no inset.
        .when(live, |d| {
            d.px(px(8.)).rounded(px(4.)).bg(rgb(t.e2)).cursor_pointer().hover(|s| s.bg(rgb(tokens().e3)))
        })
        .when(!live, |d| d.cursor_not_allowed())
        .when_some(on_click, |d, f| d.on_click(f))
        .child(crate::ui::anchor::probe(id))
        .child(div().flex_1().min_w(px(0.)).truncate().child(text))
        .when(live, |d| d.child(Icon::ChevronDown.el(IconSize::Inline, t.ink3)))
}

/// A value the project owns, as plain text with a short dim note.
fn info_note(text: String, note: &'static str) -> gpui::Stateful<Div> {
    let t = tokens();
    div()
        .id(SharedString::from(format!("info-{note}-{text}")))
        .w_full()
        .h(px(28.))
        .flex()
        .items_center()
        .gap(px(6.))
        .overflow_hidden()
        .whitespace_nowrap()
        .type_style(type_scale::body())
        .text_color(rgb(t.ink1))
        .child(div().flex_none().child(text))
        .child(
            div()
                .min_w(px(0.))
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink3))
                .child(format!("\u{b7} {note}")),
        )
}

/// One Details row: a fixed-width label, then the value. `off` is the reason
/// the row does not apply: the row dims and says so on hover; it never moves.
fn detail_row(id: &'static str, label: &'static str, value: impl IntoElement, off: Option<String>) -> gpui::Stateful<Div> {
    let tip: SharedString = off.clone().unwrap_or_default().into();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_none()
                .w(px(84.))
                .type_style(type_scale::body())
                .text_color(rgb(if off.is_some() { tokens().ink3 } else { tokens().ink2 }))
                .child(label),
        )
        .child(div().flex_1().min_w(px(0.)).child(value))
        .when(off.is_some(), |d| {
            d.cursor_not_allowed().tooltip(crate::ui::widgets::overlay_tip_hover(label, "", Some(&tip)))
        })
}

/// One summary row: a dim fixed label column and the value on one line. A value
/// that does not fit is cut at its end, never wrapped, so no number is split
/// across two lines ("30 / Mb/s").
fn summary_row(label: &'static str, text: String) -> Div {
    let t = tokens();
    div()
        .flex()
        .gap(px(6.))
        .type_style(type_scale::caption())
        .child(div().flex_none().w(px(40.)).text_color(rgb(t.ink3)).child(label))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .whitespace_nowrap()
                .truncate()
                .text_color(rgb(t.ink1))
                .child(text),
        )
}

/// One dim helper line, a single line that truncates rather than wraps.
fn dim_line(text: String) -> impl IntoElement {
    div()
        .truncate()
        .type_style(type_scale::caption())
        .text_color(rgb(tokens().ink3))
        .child(text)
}

/// One half of a two-way segmented control (the kit's look: pills on an `e2`
/// track); `off` is the reason it cannot be picked (dim, on hover, no click).
fn seg(id: &'static str, label: &'static str, active: bool, off: Option<String>, on_click: OnClick) -> gpui::Stateful<Div> {
    let t = tokens();
    let live = off.is_none();
    let tip: SharedString = off.unwrap_or_default().into();
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .h(px(24.))
        .rounded(px(3.))
        .when(active, |d| d.bg(rgb(t.e3)))
        .type_style(type_scale::body_strong())
        .text_color(rgb(match (active, live) {
            (_, false) => t.ink4,
            (true, _) => t.ink1,
            _ => t.ink2,
        }))
        .when(live, |d| {
            d.cursor_pointer()
                .when(!active, |d| d.hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)).text_color(rgb(tokens().ink1))))
                .on_click(on_click)
        })
        .when(!live, |d| {
            d.cursor_not_allowed().tooltip(crate::ui::widgets::overlay_tip_hover(label, "", Some(&tip)))
        })
        .child(label)
}

/// The track a [`seg`] pair sits in.
fn seg_track() -> Div {
    div().flex().gap(px(2.)).p(px(2.)).h(px(28.)).rounded(px(4.)).bg(rgb(tokens().e2))
}

/// A small ghost button with an icon and a word.
fn chip(id: &'static str, label: String, _active: bool, on_click: OnClick) -> gpui::Stateful<Div> {
    Button::new(id)
        .icon(Icon::FolderOpen)
        .label(label)
        .style(ButtonStyle::Ghost)
        .size(ButtonSize::Default)
        .build()
        .on_click(on_click)
}

/// A sheet button: a label and, where there is one, its key (in the tooltip,
/// not on the button). `primary` is the press the face is for.
fn sheet_button(
    id: &'static str,
    label: &'static str,
    chord: Option<&str>,
    primary: bool,
    enabled: bool,
    on_click: OnClick,
) -> impl IntoElement {
    let b = Button::new(id)
        .label(label)
        .style(if primary { ButtonStyle::Primary } else { ButtonStyle::Secondary })
        .size(ButtonSize::Large)
        .enabled(enabled)
        .build()
        .flex_1();
    let b = match (enabled, chord) {
        (true, Some(k)) if !k.is_empty() => {
            b.tooltip(crate::ui::widgets::overlay_tip_hover(label, k, None))
        }
        _ => b,
    };
    b.when(enabled, |b| b.on_click(on_click))
}

#[cfg(test)]
mod plain_words_tests {
    use super::*;

    #[test]
    fn progress_speaks_plainly_and_names_the_measuring_stage() {
        assert_eq!(progress_words(None, 0.42, "about 12 s left"), "42% \u{b7} about 12 s left");
        assert_eq!(progress_words(Some(("Measuring shake", 0.4)), 0.9, "x"), "Measuring shake \u{b7} 40%");
    }

    #[test]
    fn the_route_never_prints_an_encoder_name() {
        let line = plain_route("MP4 \u{b7} H.264", Some("SW encode (ec-h264) \u{b7} AAC"));
        assert_eq!(line, "MP4 \u{b7} H.264");
        assert!(!plain_route("MP4", Some("SW encode (ec-h264)")).contains("ec-"));
        assert!(plain_route("MP4", Some("x \u{b7} 3 frames of the edit past the last picture")).contains("past the last picture"));
    }

    #[test]
    fn a_long_folder_is_shortened_to_its_last_two_parts() {
        assert_eq!(short_folder("~/Videos/exports/2026/final"), "\u{2026}/2026/final");
        assert_eq!(short_folder("~/Videos"), "~/Videos");
        assert_eq!(tail_of("/tmp/sweep/f/out", 14), "\u{2026}p/sweep/f/out");
        assert_eq!(tail_of("abcdef", 4), "\u{2026}def");
    }

    #[test]
    fn a_failed_write_says_what_to_do_and_keeps_the_raw_error_out_of_sight() {
        use crate::export_ui::failure_words;
        assert_eq!(failure_words("Permission denied (os error 13)", "~/ro"), "edith can't write to ~/ro. Pick another folder.");
        assert_eq!(failure_words("No space left on device (os error 28)", "~"), "The disk is full.");
        assert_eq!(failure_words("No such file or directory (os error 2)", "/gone"), "The folder /gone no longer exists.");
        assert_eq!(
            failure_words("No such file or directory (os error 2) \u{2014} missing: a.mp4, b.mp4", "~"),
            "A file is missing: a.mp4, b.mp4."
        );
        assert_eq!(failure_words("encoder stalled", "~"), "Something went wrong while writing the file.");
    }

    #[test]
    fn the_done_line_says_where_the_file_landed_never_a_bare_tilde() {
        assert_eq!(saved_in("~"), "Saved in Home");
        assert_eq!(saved_in("~/Videos/exports/2026/final"), "Saved in \u{2026}/2026/final");
    }
}
