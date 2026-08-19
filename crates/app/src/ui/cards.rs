//! The property cards -- docked into the inspector, or floated when modal.

use crate::*;
use crate::ui::widgets::*;

impl Player {
    /// What an export is going to be, before there is one: the codec, the box
    /// it goes into, the bitrate, where it lands -- and, above the button, the
    /// two lines that state the file all of that adds up to. The same scrim and
    /// row shape as the keybindings overlay (two cards of different builds over
    /// one window read as two different programs) and the same plain divs, so
    /// the root keeps the keyboard and the custom row's field is typed into
    /// through it ([`NumberEdit`]).
    ///
    /// Every row carries the key that picks it, so the card is drivable end to
    /// end without a pointer *and* without a legend to memorise -- and every
    /// row is clickable, the bitrate's steppers included, so it is drivable end
    /// to end without a keyboard as well.
    ///
    /// Two shapes of it, behind `g` and `r`: sections against one flat list,
    /// and the codecs with no encoder collapsed into a footer against a dimmed
    /// row each. Grouped and collapsed is what opens.
    pub(crate) fn export_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.export_open {
            return None;
        }
        // The cap is what keeps the card inside the smallest window; a taller
        // one gets a taller list rather than a scroll past empty space, and the
        // card is still only as tall as the rows it has.
        let rows_h = (f32::from(viewport.height) - EXPORT_FIXED_H - 24.).max(EXPORT_ROWS_H);
        let row = |id: (&'static str, usize)| {
            div()
                .id(id)
                .flex()
                // The floor, not the height: the destination's path wraps on a
                // long name and must not paint over the row under it. Which it
                // did: inside a capped, scrolling list a row shrinks to fit by
                // default, so a wrapped detail was drawn over the row beneath
                // it (the Sound row's "the source's own packets are copied…"
                // over Subtitles). The list scrolls -- a row is as tall as what
                // it has to say.
                .flex_none()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        // A row that cannot be picked is dimmed and takes no click, exactly as
        // an inapplicable item in the clip menu is: it still says its piece.
        let live = |d: Stateful<Div>, enabled: bool| {
            d.when(!enabled, |d| {
                d.cursor_not_allowed().text_color(rgb(FG_SECONDARY()))
            })
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgb(BG_HOVER()))))
        };
        // A row as this card writes them: the mark saying which one is picked,
        // the key that picks it, its name, and what the choice means. The mark
        // is a glyph and not only a colour -- a background alone is gone the
        // moment a hover lands on the row, and invisible to anyone who cannot
        // tell the two greys apart (WCAG 1.4.1).
        let entry = |id: (&'static str, usize),
                     key: &str,
                     label: SharedString,
                     detail: SharedString,
                     picked: bool,
                     enabled: bool| {
            // The small print of a picked row sits on the highlight, where the
            // dim ink is only 3.3:1 -- the row it lands on lifts it (WCAG
            // 1.4.3, and the fit test pins both numbers).
            let ink = match picked {
                true => FG_PRIMARY(),
                false => FG_SECONDARY(),
            };
            live(row(id), enabled)
                .when(picked, |d| d.bg(rgb(BG_SELECTED())))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .child(div().w(px(10.)).child(match picked {
                            true => "✓",
                            false => " ",
                        }))
                        .child(
                            div()
                                .w(px(EXPORT_KEY_W))
                                .text_size(px(11.))
                                .text_color(rgb(ink))
                                .child(SharedString::from(key.to_string())),
                        )
                        .child(label),
                )
                // Wraps rather than runs off the row: a refusal is the longest
                // thing in this column and the half of it past the edge is the
                // half that says what to do instead.
                .child(
                    div()
                        .min_w(px(0.))
                        .flex_shrink()
                        .text_size(px(11.))
                        .text_color(rgb(ink))
                        .child(detail),
                )
        };
        let header = |text: &'static str| {
            div()
                .flex_none()
                .px(px(6.))
                .pt(px(4.))
                .text_size(px(10.))
                .text_color(rgb(FG_SECONDARY()))
                .child(text)
                .into_any_element()
        };
        let mut list: Vec<AnyElement> = Vec::new();
        // The primary pane: destination first (the one thing every export
        // needs regardless of what it is), then the bundles most exports
        // actually are, then the button to the rest. Everything under it is
        // exactly the codec, quality, sound and encoder rows the old flat
        // card opened on -- a bundle here only sets the same two fields the
        // Advanced pane's own Format and Quality rows set, so nothing here
        // is a setting of its own to fall out of step with them.
        let destination = entry(
            ("destination", 0),
            "d",
            "Destination".into(),
            file_name(&self.export_path).into(),
            false,
            true,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_destination(cx)))
        .into_any_element();
        list.push(destination);
        let current_preset = ExportPreset::from_state(self.format, self.quality);
        for (i, preset) in ExportPreset::ALL.into_iter().enumerate() {
            let mut r = entry(
                ("preset", i),
                "",
                preset.label().into(),
                preset.detail().into(),
                preset == current_preset,
                true,
            );
            r = r.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                match preset.bundle() {
                    Some((format, quality)) => {
                        this.set_format(format);
                        this.quality = quality;
                    }
                    // Custom sets nothing -- it only opens the pane where the
                    // codec and the quality are picked apart.
                    None => this.export_advanced_open = true,
                }
                cx.notify();
            }));
            list.push(r.into_any_element());
        }
        let advanced_detail = match self.export_advanced_open {
            true => "codec, container, quality, sound, encoder, subtitles — s collapses them"
                .to_string(),
            false => "codec, container, quality, sound, encoder, subtitles — s for the rows"
                .to_string(),
        };
        list.push(
            entry(
                ("export-advanced", 0),
                "s",
                "Advanced".into(),
                advanced_detail.into(),
                self.export_advanced_open,
                true,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.export_advanced_open = !this.export_advanced_open;
                cx.notify();
            }))
            .into_any_element(),
        );
        // Everything from here down is the Advanced pane: the codec, its
        // container, the quality rows, sound, encoder, subtitles, the two
        // display switches and what this machine can encode with -- all of
        // it built exactly as the flat card built it, only not pushed at all
        // while the pane is shut.
        if self.export_advanced_open {
        if self.export_grouped {
            list.push(header("FORMAT"));
        }
        // The codecs first: which one is being written decides whether every
        // row under it means anything. One a *this* timeline cannot be written
        // as (an audio-only edit, for a picture codec) reads exactly like one
        // there is no encoder for -- dimmed, unclickable, and carrying its own
        // reason where its detail was, with nothing to click to find that out.
        let mut refusals: Vec<String> = Vec::new();
        for (i, (boxes, key, label, detail)) in FORMATS.into_iter().enumerate() {
            let format = same_box(boxes, self.format);
            let refused = format
                .zip(self.session.as_ref())
                .and_then(|(f, s)| format_refusal(s, f));
            // A codec with no encoder here at all is the other kind of refusal,
            // and the one the footer collects: it can never become pickable, so
            // a row each is dead rows above the fold.
            if format.is_none() {
                refusals.push(format!("{label} — {detail}"));
                if !self.export_refusals_inline {
                    continue;
                }
            }
            let detail: SharedString = match &refused {
                Some(why) => why.clone().into(),
                None => detail.into(),
            };
            let format = format.filter(|_| refused.is_none());
            let mut r = entry(
                ("format", i),
                key,
                label.into(),
                detail,
                boxes.contains(&self.format),
                format.is_some(),
            );
            if let Some(format) = format {
                r = r.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_format(format);
                    cx.notify();
                }));
            }
            list.push(r.into_any_element());
        }
        if self.export_grouped {
            list.push(header("DETAILS"));
        }
        // The container, and only where the picked codec has more than one box:
        // a row offering a single choice reads as a choice.
        let boxes = containers(self.format);
        if boxes.len() > 1 {
            let next = next_container(self.format);
            list.push(
                entry(
                    ("container", 0),
                    "c",
                    "Container".into(),
                    format!(
                        "{} — c for {}",
                        self.format.ext().to_uppercase(),
                        next.ext().to_uppercase()
                    )
                    .into(),
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.cycle_container();
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        match bitrate_refusal(self.format) {
            // One dimmed row carrying the reason, rather than five rows of a
            // figure that will not be written: the quality rows are the
            // *picture's* bitrate, and this file has no picture in it.
            Some(why) => list.push(
                entry(
                    ("quality", 0),
                    "q",
                    "Quality".into(),
                    why.into(),
                    false,
                    false,
                )
                .into_any_element(),
            ),
            None => {
                for (i, quality) in Quality::ALL.into_iter().enumerate() {
                    // The custom row is a field: `n` opens it, a click in it
                    // opens it too, and while it is open the row shows what is
                    // being typed with a caret in it rather than the number in
                    // force. The other four are picked whole.
                    let field = self.mbps_edit.as_ref().filter(|_| quality == Quality::Custom);
                    let mut r = entry(
                        ("quality", i),
                        match quality {
                            Quality::Custom => "n",
                            _ => "q",
                        },
                        quality.label().into(),
                        match field {
                            Some(edit) => edit.detail().into(),
                            None => quality.detail(self.custom_mbps).into(),
                        },
                        self.quality == quality,
                        true,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            match quality {
                                Quality::Custom => this.edit_mbps(),
                                _ => this.quality = quality,
                            }
                            cx.notify();
                        },
                    ));
                    if quality == Quality::Custom {
                        // The wheel anywhere over the row moves the number, the
                        // buttons being one step each: the range is fifty wide
                        // now, and a number only a repeated press can walk to is
                        // a number nobody walks to. Swallowed, so the notch that
                        // moved the bitrate does not scroll the list under it as
                        // well -- one gesture, one thing changed.
                        r = r
                            .on_scroll_wheel(cx.listener(
                                |this, event: &ScrollWheelEvent, _, cx| {
                                    this.wheel_mbps(event);
                                    cx.stop_propagation();
                                    cx.notify();
                                },
                            ))
                            .child(self.mbps_steppers(cx));
                    }
                    list.push(r.into_any_element());
                }
            }
        }
        // The other half of the file, and the one this card used to write at a
        // fixed rate without saying so: what the *sound* is coded at, for every
        // format that codes it -- the AAC inside a video export as much as an
        // MP3. Four rates, so the pointer gets the *list* of them
        // ([`Pick::AudioRate`]) rather than a button clicked round -- the
        // resolution row's rule, and the key still steps as `ctrl+r` still
        // does. Dimmed with the reason where this timeline has no rate to pick,
        // like the quality rows are.
        let sound = self.audio_rate_refusal();
        let mut r = entry(
            ("sound", 0),
            "b",
            "Sound".into(),
            match sound {
                Some(why) => why.into(),
                None => format!("{} kbps — b steps", self.audio_kbps).into(),
            },
            false,
            sound.is_none(),
        );
        if sound.is_none() {
            r = r.on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.open_picker(Pick::AudioRate, event.position(), cx)
            }));
        }
        list.push(r.into_any_element());
        // Which encoder writes the picture, which was an env pin and therefore
        // no choice at all. Three seats, so the pointer gets the *list* of them
        // ([`Pick::Encoder`]) and never a button clicked round -- the Sound
        // row's rule, one row above. Only where there is a picture to encode:
        // a WAV has no seat to pick and the row would be a question about
        // nothing.
        if self.format.has_video() {
            let seat = self.encoder_seat();
            list.push(
                entry(
                    ("encoder", 0),
                    "e",
                    "Encoder".into(),
                    format!("{} — e steps", encoder_label(seat)).into(),
                    false,
                    true,
                )
                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    this.open_picker(Pick::Encoder, event.position(), cx)
                }))
                .into_any_element(),
            );
            // ...and, under it, the one thing about that pick a person cannot
            // know from the words in the list: this box's driver reset itself
            // on the vendored AV1 encoder. A dimmed row and not a modal -- the
            // pick is theirs, and the export runs.
            if let Some(warning) = av1_hw_warning(self.format, seat) {
                list.push(
                    entry(("encoder-av1", 0), "", "".into(), warning.into(), false, false)
                        .into_any_element(),
                );
            }
        }
        // What happens to the subtitles on this timeline, in one line: how many
        // are written into the file and under what names, or the reason each one
        // is not. Which tracks travel is not a pick -- everything with a cue on
        // the timeline goes ([`Player::export_subs`]) -- so the row says rather
        // than offers, like the machine lines below.
        // ...and the ones that do not travel are named here too, which they were
        // not: `export_subs` had already filtered a track with no cue on *this*
        // timeline out of the engine's sight, so the card said nothing about a
        // row the list was still showing ([`subtitle_plan`]).
        let plan = self.subtitle_line();
        // The row used to end in "click for <fmt> in MKV" whenever the picture
        // was going into an mp4, on the grounds that only Matroska carries a
        // text track. An mp4 carries one now (`Mp4Muxer::write_subtitles` writes
        // `tx3g`), so that was a refusal offering a way out of nothing: the
        // container the card is already set to embeds them. The refusals left in
        // `planned_subtitles` are the true ones -- a sound-only format has
        // nowhere to put a track, and a PGS track is pictures whatever the box.
        list.push(
            entry(
                ("subtitles", 0),
                "",
                "Subtitles".into(),
                plan.into(),
                false,
                false,
            )
            .into_any_element(),
        );
        // The card's own two layout switches. They were `g` and `r` and nothing
        // else, while the status line under the title advertised both of them:
        // a hand on the mouse read what they do and had nothing to press.
        list.push(
            entry(
                ("export-layout", 0),
                "g",
                "Layout".into(),
                match self.export_grouped {
                    true => "sections — g for one flat list".into(),
                    false => "one flat list — g for sections".into(),
                },
                self.export_grouped,
                true,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.export_grouped = !this.export_grouped;
                cx.notify();
            }))
            .into_any_element(),
        );
        list.push(
            entry(
                ("export-refusals", 0),
                "r",
                "Codecs with no encoder".into(),
                match self.export_refusals_inline {
                    true => "a dimmed row each — r collapses them".into(),
                    false => "one line at the foot — r shows a row each".into(),
                },
                self.export_refusals_inline,
                true,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.export_refusals_inline = !this.export_refusals_inline;
                cx.notify();
            }))
            .into_any_element(),
        );
        if !self.export_refusals_inline && !refusals.is_empty() {
            // Last, and one line: the reason travels with the name (a footer
            // that only listed them would be the "why not?" the rows exist to
            // answer), but nothing here can ever be picked, so it sits under
            // every row that can rather than above them.
            list.push(
                div()
                    .flex_none()
                    .px(px(6.))
                    .py(px(2.))
                    .text_size(px(11.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child(format!("cannot write: {}", refusals.join(" · ")))
                    .into_any_element(),
            );
        }
        // What is doing the work, on this machine and in this build: the GPU
        // half is asked of the driver through the plugin (a different answer on
        // every machine, and "none" where there is no plugin at all), the build
        // half is the crates that were compiled in. Last in the list, because it
        // is what a user checks rather than what they pick -- and a listing
        // rather than rows, since none of it can be clicked.
        if self.export_grouped {
            list.push(header("THIS MACHINE"));
        }
        let note = |text: String| {
            div()
                .flex_none()
                .px(px(6.))
                .py(px(2.))
                .text_size(px(11.))
                .text_color(rgb(FG_SECONDARY()))
                .child(text)
                .into_any_element()
        };
        list.push(note(format!(
            "GPU: {}",
            self.hw_caps.clone().unwrap_or_else(|| "asking…".into())
        )));
        list.push(note(format!("Built in: {}", engine::caps::software())));
        } // self.export_advanced_open
        // What the rows add up to, which is the one thing that has to be right:
        // codec, box, size, rate, sound, where it goes and about how big. Two
        // lines, outside the scrolling list, so it is on screen whatever the
        // list is scrolled to and whatever is picked.
        let picture = self
            .session
            .as_ref()
            .map(|s| (s.resolution(), s.meta().frame_rate));
        // The probe's answer where it has one -- the export's own decision,
        // sound included ([`engine::export::planned_seats`]) -- and the pure
        // prediction from the format alone until it lands, which is the same
        // line this card always showed.
        let seats = self.export_seat.as_ref().and_then(|(.., seats)| *seats);
        let audio = seats.map_or_else(
            || {
                self.session
                    .as_ref()
                    .map_or("", |s| s.planned_audio(self.format))
            },
            |(_, audio)| audio,
        );
        let settings =
            export_settings(
            self.quality,
            self.custom_mbps,
            self.format,
            self.audio_kbps,
            self.encoder_seat(),
        );
        let size = estimated_bytes(
            settings.bitrate.filter(|_| self.format.has_video()),
            self.session
                .as_ref()
                .map_or(0., PlaybackSession::timeline_duration),
        );
        let head = summary_head(self.format, picture, audio);
        let tail = summary_tail(
            &self.export_path,
            size,
            seats.and_then(|(video, _)| video),
            self.format.has_video(),
        );
        // The button says the refusal *before* it is pressed: the picked codec
        // can go invalid one edit after it was picked (a cleared video lane),
        // and a button that looks ready until it is pressed is the lie the
        // dimmed rows above it are there to avoid.
        let blocked = self
            .session
            .as_ref()
            .and_then(|s| format_refusal(s, self.format));
        let action: SharedString = match &blocked {
            Some(why) => {
                format!("Cannot export — {}", why.split(" — ").next().unwrap_or(why)).into()
            }
            None => "Export".into(),
        };
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(EXPORT_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(div().flex_none().px(px(6.)).child("Export"))
                        // The status line, where a refusal from the save dialog
                        // lands: the notice bar it would otherwise take is under
                        // the scrim. The keys are on the rows, so this says how
                        // the card as a whole is answered rather than listing
                        // them a second time.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(match (&self.mbps_edit, self.notices.front()) {
                                    // A field being typed into says so here as
                                    // well as in its row: this line is outside
                                    // the scrolling list and on screen at every
                                    // window size, and at 360 px the custom row
                                    // itself can be below the fold -- a number
                                    // typed where it cannot be seen is the
                                    // blind capture this field replaced.
                                    (Some(edit), _) => {
                                        SharedString::from(format!("Custom bitrate {}", edit.detail()))
                                    }
                                    (None, Some(notice)) => notice.clone(),
                                    (None, None) => "the keys are on the rows · enter exports · \
                                                     a click away or esc closes · g/r change the layout"
                                        .into(),
                                }),
                        )
                        // Capped and scrolling like the keybindings list: the
                        // rows are more than a 360 px window has room for, and
                        // it is the list that scrolls, never the card that
                        // grows -- the summary and the button below stay put.
                        .child(
                            div()
                                .id("export-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .max_h(px(rows_h))
                                .overflow_y_scroll()
                                .children(list),
                        )
                        .child(div().flex_none().px(px(6.)).text_size(px(11.)).child(head))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(tail),
                        )
                        .child(
                            div()
                                .id("export-confirm")
                                .mt(px(4.))
                                .flex()
                                .h(px(CONTROL_H))
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .bg(rgb(match blocked.is_some() {
                                    true => BG_PANEL(),
                                    false => BG_SELECTED(),
                                }))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.start_export(cx)
                                    }),
                                )
                                .child(action),
                        ),
                ),
        )
    }

    /// The export while it runs, on the same sheet it was asked for on: an
    /// editor that takes no edit until the worker is done says so as a card and
    /// not as a strip under a panel nobody may touch. Same scrim, same width,
    /// same raised box as [`Player::export_card`] -- the answer arrives where
    /// the question was put -- and the timeline is still read around it.
    ///
    /// Two things it does *not* do. It never closes: not on a press away, not
    /// on `esc`. There is nothing here to dismiss while the export is still
    /// running, and a modal that vanishes leaves a locked editor with no reason
    /// on screen. And its cancel is two presses, never one -- an hour of
    /// encoding must not end on a stray click, which is why the stroke is a
    /// chord as well ([`cancels_export`]).
    pub(crate) fn export_progress_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let export = self.exporting()?;
        let progress = export.progress().clamp(0., 1.);
        let elapsed = self
            .export_started
            .map_or(0., |t| t.elapsed().as_secs_f32());
        // Two numbers that must both be honest: the one that counts up is
        // measured, the one that counts down is a guess and says so.
        let left = eta_secs(&self.export_marks, elapsed, progress).map_or_else(
            || "estimating…".to_owned(),
            |s| format!("~{} left", clock(s)),
        );
        // What is being read, the same files the engine names on stderr
        // ("export source:"): the project's own sources, never a stand-in.
        let source = match self.sources() {
            [] => "the timeline".to_owned(),
            [one] => file_name(&one.path),
            [first, rest @ ..] => format!("{} +{} more", file_name(&first.path), rest.len()),
        };
        let note = |text: SharedString| {
            div()
                .flex_none()
                .px(px(6.))
                .text_size(px(11.))
                .text_color(rgb(FG_SECONDARY()))
                .child(text)
        };
        let armed = self.cancel_armed;
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // No `close_card` here, unlike every other sheet: a press away
                // is not a way out of a running export, and the only thing that
                // ends this card is the export ending.
                .on_mouse_down(MouseButton::Left, swallow)
                .child(
                    div()
                        .w(px(EXPORT_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(div().flex_none().px(px(6.)).child("Exporting"))
                        // The bar itself: the same number as the percentage
                        // below it, and it only ever moves forward -- the
                        // worker's progress is a monotone `fetch_max`.
                        .child(
                            div()
                                .flex_none()
                                .mx(px(6.))
                                .h(px(6.))
                                .rounded(px(3.))
                                .bg(rgb(BG_PANEL()))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(progress))
                                        .rounded(px(3.))
                                        .bg(rgb(STATUS_PROGRESS())),
                                ),
                        )
                        .child(div().flex_none().px(px(6.)).text_size(px(11.)).child(
                            SharedString::from(format!(
                                "{}% · {} elapsed · {left}",
                                (progress * 100.) as u32,
                                clock(elapsed),
                            )),
                        ))
                        // The row that was picked, then the seats the worker
                        // actually opened -- so a fallback to the software
                        // encoder shows here rather than being invisible.
                        .child(note(
                            format!(
                                "{} · {}",
                                format_label(self.format),
                                export
                                    .encoders()
                                    .unwrap_or_else(|| "opening the encoder".to_string()),
                            )
                            .into(),
                        ))
                        .child(note(
                            format!("{source} → {}", file_name(&self.export_path)).into(),
                        ))
                        .child(note(
                            match armed {
                                true => "cancelling deletes what has been written so far"
                                    .to_string(),
                                false => format!(
                                    "{} cancels · esc alone does nothing while this runs · the \
                                     timeline is read-only until it finishes",
                                    self.keymap.display(ActionId::CancelExport)
                                ),
                            }
                            .into(),
                        ))
                        // One button, or the pair that answers it: never a
                        // control that cycles -- both choices are on screen at
                        // once, each saying which one it is.
                        .child(
                            div()
                                .mt(px(2.))
                                .flex()
                                .gap(px(6.))
                                .justify_end()
                                .when(armed, |d| {
                                    d.child(control(
                                        "export-keep",
                                        140.,
                                        ACCENT_PRIMARY(),
                                        None,
                                        "Keep exporting",
                                        "leaves the export running".to_string(),
                                        true,
                                        cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.cancel_armed = false;
                                            cx.notify();
                                        }),
                                    ))
                                })
                                .child(control(
                                    "export-cancel",
                                    140.,
                                    BG_RAISED(),
                                    None,
                                    "Cancel export",
                                    match armed {
                                        true => "stops the worker and deletes the part file"
                                            .to_string(),
                                        false => "asks first -- one press only offers the choice"
                                            .to_string(),
                                    },
                                    true,
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        match this.cancel_armed {
                                            true => this.cancel_export(),
                                            false => this.cancel_armed = true,
                                        }
                                        cx.notify();
                                    }),
                                )),
                        ),
                ),
        )
    }

    /// The custom bitrate's two pointer buttons. The typed digits were the last
    /// control in this card a mouse could not reach at all -- and a number that
    /// can only be typed is a number a hand on the pointer has to leave the
    /// card to change. `HIT_MIN` square, like every other target here.
    pub(crate) fn mbps_steppers(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let step = |id: &'static str, label: &'static str, by: i32, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .w(px(HIT_MIN))
                .h(px(HIT_MIN))
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .bg(rgb(BG_PANEL()))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.nudge_mbps(by);
                    cx.notify();
                }))
                .child(label)
        };
        div()
            .flex()
            .gap(px(4.))
            .child(step("mbps-down", "−", -1, cx))
            .child(step("mbps-up", "+", 1, cx))
    }

    /// The equalizer of one audio clip: its frequency response drawn as a
    /// curve, a handle per band sitting on it, each band's own bell under the
    /// sum, and a row that reads and moves the picked band's three numbers. The
    /// curve is the clip's actual filter (`EqParams::response_db` reads the
    /// coefficients the samples go through), and it is redrawn on every pointer
    /// sample of a drag, so the shape bends under the hand.
    ///
    /// Wider than the other cards and wider still on a bigger window
    /// ([`eq_card_w`]): every pixel across is frequency resolution, which none
    /// of the row-shaped cards have any use for. The same scrim and the same
    /// plain divs -- nothing here takes focus, so the root keeps the keyboard
    /// and the card's own strokes (a digit, the arrows, `a`, `x`, `f`, `r`,
    /// `s`) reach it.
    ///
    /// Every change is written at the clip as it is made
    /// ([`Player::commit_eq`]), so what the card shows is always what is
    /// playing: there is no OK button to forget, and closing it changes
    /// nothing. What takes a curve back off is undo, like every other edit.
    pub(crate) fn eq_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.eq_open?;
        // The rate the engine filters this timeline at, so the drawn curve is
        // the one those coefficients make -- near the top of the axis a 44.1 kHz
        // clip and a 48 kHz one are not the same shape.
        let sample_rate = self.timeline_audio().map_or(48_000, |(rate, _)| rate);
        let picked = self.eq_params.bands.get(self.eq_band);
        // What is playing, drawn behind the curve -- and only while something
        // *is* playing: the tap freezes with the device, and a still spectrum
        // under a paused timeline would look like sound that is not there.
        let spectrum = self
            .eq_spectrum
            .then_some(self.session.as_ref())
            .flatten()
            .filter(|session| session.is_playing())
            .and_then(PlaybackSession::audio_tap)
            .map(|(samples, rate)| eq_spectrum(&samples, rate))
            .filter(|levels| !levels.is_empty())
            .map(eq_spectrum_curve);
        let handles: Vec<_> = self
            .eq_params
            .bands
            .iter()
            .enumerate()
            .map(|(i, band)| {
                div()
                    .absolute()
                    .left(relative(eq_x(band.freq_hz)))
                    .top(relative(eq_y(band.gain_db)))
                    // Centred on its own point: it hangs off the graph's corner,
                    // so it is pulled back by half of itself both ways.
                    .ml(px(-EQ_HANDLE / 2.))
                    .mt(px(-EQ_HANDLE / 2.))
                    .w(px(EQ_HANDLE))
                    .h(px(EQ_HANDLE))
                    .rounded(px(EQ_HANDLE / 2.))
                    .bg(rgb(match i == self.eq_band {
                        true => ACCENT_PRIMARY(),
                        false => FG_SECONDARY(),
                    }))
            })
            .collect();
        let graph = div()
            // Ided like the band rows it replaces: what the pointer presses on
            // is one element with its own hitbox, which is what a drag is
            // tracked from.
            .id("eq-graph")
            .relative()
            .flex_none()
            .h(px(EQ_GRAPH_H))
            .rounded(px(3.))
            .bg(rgb(BG_HOVER_DIM()))
            .cursor_pointer()
            // The press picks the band under it *and* is already the first
            // sample of the drag, so a plain click sets a value. A second click
            // takes that band back to flat instead: the gesture that undoes one
            // handle, with no modifier to remember.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.eq_band = this.nearest_band(event.position.x);
                    if event.click_count >= 2 {
                        this.eq_dragging = false;
                        this.nudge_band(|b| b.gain_db = 0., cx);
                        return;
                    }
                    this.eq_dragging = true;
                    this.drag_band(event.position, cx);
                }),
            )
            .child(bounds_probe(self.eq_graph.clone()))
            // The analyser first, so everything else is drawn on top of it.
            .children(spectrum)
            // The decades, so a hump can be read as "around 200 Hz" without
            // dropping the eye to the labels along the bottom. The two ends
            // are the box's own edges and rule nothing.
            .children(
                EQ_TICKS
                    .iter()
                    .filter(|(freq, _)| *freq > EQ_FREQ_LOW && *freq < EQ_FREQ_HIGH)
                    .map(|(freq, _)| {
                        div()
                            .absolute()
                            .left(relative(eq_x(*freq)))
                            .w(px(1.))
                            .h_full()
                            .bg(rgb(EQ_GRID()))
                    })
                    .collect::<Vec<_>>(),
            )
            // Half way to each limit, each carrying its own number: the curve
            // is a curve of decibels, and until now only one of them was drawn.
            .children(EQ_DB_GRID.map(|db| {
                div()
                    .absolute()
                    .top(relative(eq_y(db)))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(EQ_GRID()))
                    .child(
                        div()
                            .absolute()
                            .left(px(4.))
                            .top(px(-11.))
                            .text_size(px(9.))
                            .text_color(rgb(FG_SECONDARY()))
                            .child(format!("{db:+.0}")),
                    )
            }))
            // 0 dB: the line a boost is a boost *from*.
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(BG_HOVER())),
            )
            .child(eq_curve(self.eq_params.clone(), sample_rate))
            .children(handles)
            .children(EQ_TICKS.map(|(freq, label)| {
                div()
                    .absolute()
                    .left(relative(eq_x(freq)))
                    // Pulled back so the mark reads as sitting *at* its
                    // frequency; the two ends then hug the corners.
                    .ml(px(-12.))
                    .bottom(px(1.))
                    .w(px(24.))
                    .text_align(TextAlign::Center)
                    .text_size(px(9.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child(label)
            }))
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(4.))
                    .text_size(px(9.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child(format!("+{EQ_GAIN_LIMIT:.0} dB")),
            );
        // The bottom of the axis is not named: -12 dB would land in the same
        // corner as the 20 Hz tick, and the two lines above it (+6 and -6)
        // already say what the box is worth per pixel.
        //
        // The same affordance the column itself carries, read back off the
        // card's own handle: this is the tallest card in the inspector -- a
        // 132 px graph with a row of numbers and a row of buttons under it --
        // and at the 360 px floor it is taller than the column it is docked in.
        let can_scroll = f32::from(self.eq_scroll.max_offset().height) > 1.;
        let below = px_below(
            f32::from(self.eq_scroll.max_offset().height),
            f32::from(self.eq_scroll.offset().y),
        );
        Some(
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        // A cap, not a width: the card is docked in the
                        // inspector now, and that column is narrower than the
                        // floor a graph wants -- asking for [`eq_card_w`]
                        // outright hung the card's right-hand third off the edge
                        // of the window. The two cards beside it are built this
                        // way for the same reason.
                        .w_full()
                        .max_w(px(eq_card_w(f32::from(viewport.width))))
                        // And a cap the other way, for the same reason the width
                        // has one: the card is docked in a column now, and at the
                        // 360 px floor it is taller than that column -- its title
                        // ran off the top and its buttons off the bottom, with
                        // neither reachable. Capped here, scrolled below.
                        .max_h(relative(1.))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(
                            div()
                                .id("eq-card-rows")
                                // Shrinkable below its content, which is what
                                // makes the cap above a scroll rather than a
                                // clip; the rows themselves keep their heights.
                                .min_h(px(0.))
                                .overflow_y_scroll()
                                .track_scroll(&self.eq_scroll)
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                // Which clip, because the card is modal and the lane it
                                // was opened from is behind a scrim by the time it is up.
                                .child(div().flex_none().px(px(6.)).child(format!(
                                    "Equalizer — {} clip {}",
                                    lane.label(),
                                    idx + 1
                                )))
                                .child(
                                    div()
                                        .flex_none()
                                        .px(px(6.))
                                        .text_size(px(11.))
                                        .text_color(rgb(FG_SECONDARY()))
                                        .child(self.notices.front().cloned().unwrap_or_else(|| {
                                            "drag a handle, or a digit picks a band — ←→ moves it, ↑↓ its gain, shift+←→ its width; a adds, x removes, f flattens it, r all, s spectrum; a click away or esc closes".into()
                                        })),
                                )
                                .child(graph)
                                // Which band the keyboard is holding and every number it
                                // is set to, each with the pair of buttons that moves it:
                                // the curve shows the sum, and a band pushed against one
                                // pulling the other way is not readable off it.
                                .child(self.eq_numbers(picked, cx))
                                .child(
                                    div()
                                        .mt(px(4.))
                                        .flex()
                                        // The row of numbers above wraps for this
                                        // reason and so does this one: four buttons
                                        // are wider than the column at the floor,
                                        // and the one that ran off the edge --
                                        // the spectrum switch -- was then reachable
                                        // by its key alone.
                                        .flex_wrap()
                                        .gap(px(4.))
                                        .child(
                                            div()
                                                .id("eq-reset")
                                                .flex()
                                                .flex_1()
                                                .h(px(CONTROL_H))
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(3.))
                                                .bg(rgb(BG_SELECTED()))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    for band in &mut this.eq_params.bands {
                                                        band.gain_db = 0.;
                                                    }
                                                    this.commit_eq(cx);
                                                }))
                                                .child("Flatten all"),
                                        )
                                        // The two that change how many bands there are.
                                        // The engine takes any cascade -- the count was
                                        // only ever fixed because this card had no way
                                        // to say otherwise.
                                        .child(
                                            div()
                                                .id("eq-add")
                                                .flex()
                                                .flex_1()
                                                .h(px(CONTROL_H))
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(3.))
                                                .bg(rgb(BG_PANEL()))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.add_band(cx)
                                                }))
                                                .child("Add band"),
                                        )
                                        .child(
                                            div()
                                                .id("eq-remove")
                                                .flex()
                                                .flex_1()
                                                .h(px(CONTROL_H))
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(3.))
                                                .bg(rgb(BG_PANEL()))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.remove_band(cx)
                                                }))
                                                .child("Remove band"),
                                        )
                                        // The analyser's switch, next to the one other
                                        // button the card has: `s` does the same, and a
                                        // toggle only a keystroke can reach is one most
                                        // people never find.
                                        .child(
                                            div()
                                                .id("eq-spectrum")
                                                .flex()
                                                .flex_1()
                                                .h(px(CONTROL_H))
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(3.))
                                                .bg(rgb(match self.eq_spectrum {
                                                    true => BG_SELECTED(),
                                                    false => BG_HOVER_DIM(),
                                                }))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.eq_spectrum = !this.eq_spectrum;
                                                    cx.notify();
                                                }))
                                                .child(match self.eq_spectrum {
                                                    true => "Spectrum on",
                                                    false => "Spectrum off",
                                                }),
                                        ),
                                ),
                        )
                        // The column's own line, on the card that needs it for
                        // the column's reason: a row nobody knows is under the
                        // fold is a row that is not there.
                        .when(can_scroll, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .h(px(LABEL_H))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .px(px(6.))
                                    .text_size(px(10.))
                                    .text_color(rgb(match below > 1. {
                                        true => ACCENT_PRIMARY(),
                                        false => FG_SECONDARY(),
                                    }))
                                    .child(match below > 1. {
                                        true => "more below — scroll the card",
                                        false => "the end — scroll up for the rest",
                                    }),
                            )
                        }),
                ),
        )
    }

    /// The picked band's three numbers -- where it sits, how far it pushes and
    /// how wide it is -- each beside the pair of buttons that moves it. The
    /// arrows do the same three things, but a value only a key can change is a
    /// value a hand on the pointer cannot reach at all, which is the same reason
    /// [`Player::mbps_steppers`] exists.
    pub(crate) fn eq_numbers(&self, picked: Option<&Band>, cx: &mut Context<Self>) -> impl IntoElement {
        let row = div()
            .flex_none()
            .flex()
            // Three numbers and their steppers are wider than the inspector
            // column: without this the Q pair and the flatten button hang off
            // the card, which off the right-hand column means off the window.
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .px(px(6.))
            .text_size(px(11.));
        let Some(band) = picked.copied() else {
            return row.child("no bands — a adds one");
        };
        let step = |id: &'static str,
                    label: &'static str,
                    change: fn(&mut Band),
                    cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .w(px(HIT_MIN))
                .h(px(HIT_MIN))
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .bg(rgb(BG_PANEL()))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.nudge_band(change, cx)
                }))
                .child(label)
        };
        let number = |value: String,
                      ids: (&'static str, &'static str),
                      by: (fn(&mut Band), fn(&mut Band)),
                      cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(div().child(value))
                .child(step(ids.0, "−", by.0, cx))
                .child(step(ids.1, "+", by.1, cx))
        };
        row.child(format!(
            "Band {} of {}",
            self.eq_band + 1,
            self.eq_params.bands.len()
        ))
        .child(number(
            band_label(&band),
            ("eq-freq-down", "eq-freq-up"),
            (
                |b| b.freq_hz /= EQ_FREQ_STEP,
                |b| b.freq_hz *= EQ_FREQ_STEP,
            ),
            cx,
        ))
        .child(number(
            format!("{:+.1} dB", band.gain_db),
            ("eq-gain-down", "eq-gain-up"),
            (|b| b.gain_db -= EQ_STEP, |b| b.gain_db += EQ_STEP),
            cx,
        ))
        // Q is width, so its buttons are labelled by what they *do* to the
        // hump rather than by which way the number goes: a wider band is a
        // smaller Q, and nobody should have to know that to use the card.
        .child(number(
            format!("Q {:.2}", band.q),
            ("eq-q-wider", "eq-q-narrower"),
            (|b| b.q /= EQ_Q_STEP, |b| b.q *= EQ_Q_STEP),
            cx,
        ))
        .child(
            div()
                .id("eq-flat-band")
                .flex()
                .h(px(HIT_MIN))
                .px(px(8.))
                .items_center()
                .rounded(px(3.))
                .bg(rgb(BG_PANEL()))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.nudge_band(|b| b.gain_db = 0., cx)
                    }),
                )
                .child("Flatten this"),
        )
    }

    /// The colour card: the graded frame's histogram over a row per control,
    /// each row a bar the pointer drags straight to a value -- no stepper
    /// buttons, because a slider is a thing to pull, and the arrow keys still
    /// move the same value for anyone not using a pointer. Same scrim, surface
    /// and row shape as the other two cards, and the same plain divs, so the
    /// root keeps the keyboard.
    ///
    /// The values are read from the project every render: what is drawn is what
    /// the decoder is grading with, never a copy that could drift from it. The
    /// graph above them is counted off the frame that came *back* through that
    /// grade ([`histogram`]), so pulling exposure tilts it while the hand moves.
    pub(crate) fn color_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, idx) = self.color_open?;
        let params = self.color_params();
        let rows: Vec<_> = COLOR_BANDS
            .iter()
            .enumerate()
            .map(|(i, &(label, low, high))| {
                let mut read = params;
                let value = *band_mut(&mut read, i);
                let frac = ((value - low) / (high - low)).clamp(0., 1.);
                let picked = i == self.color_band;
                div()
                    .id(("color-row", i))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(picked, |d| d.bg(rgb(BG_SELECTED())))
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.color_band = i;
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                    .child(
                        // The bar is 4 px to look at and a whole row to hit
                        // (WCAG 2.5.8), the same split the ruler makes between
                        // what is drawn and what is grabbed. The press is
                        // already the first sample of the drag, so a plain click
                        // sets the value it landed on.
                        div()
                            .id(("color-bar", i))
                            .relative()
                            .flex_1()
                            .min_w(px(0.))
                            .max_w(px(COLOR_BAR_W))
                            .h(px(KEYS_ROW_H))
                            .flex()
                            .items_center()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.color_band = i;
                                    this.color_dragging = true;
                                    this.drag_color(event.position.x, true, cx);
                                }),
                            )
                            .child(bounds_probe(self.color_bars[i].clone()))
                            .child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .rounded(px(2.))
                                    .bg(rgb(BG_PANEL()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(frac))
                                            .rounded(px(2.))
                                            .bg(rgb(ACCENT_PRIMARY())),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .text_size(px(11.))
                            .text_color(rgb(FG_SECONDARY()))
                            .child(format!("{value:.2}")),
                    )
            })
            .collect();
        Some(
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Colour — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(
                                    "drag a bar, or ↑↓ picks one and ←→ moves it, r resets — a click away or esc closes",
                                ),
                        )
                        // The frame as it is being graded, over the controls
                        // grading it: the three lines are what the picture is
                        // made of, and every sample of a drag reseeks, so they
                        // move with the bar under the hand.
                        .child(
                            div()
                                .flex_none()
                                .h(px(HIST_H))
                                .rounded(px(3.))
                                .bg(rgb(BG_HOVER_DIM()))
                                .relative()
                                .child(hist_curves(self.histogram)),
                        )
                        .children(rows)
                        .child(
                            div()
                                .id("color-reset")
                                .mt(px(4.))
                                .flex()
                                .h(px(CONTROL_H))
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .bg(rgb(BG_SELECTED()))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_color(ColorParams::default(), cx);
                                }))
                                .child("Reset"),
                        ),
                ),
        )
    }

    /// The speed card: one bar from a quarter speed to four times it, the rates
    /// people name as buttons under it, and the clip's new length in frames --
    /// which is the number a person is actually choosing. Built like the colour
    /// card down to the scrim and the bar's own hit height, because it is the
    /// same kind of card: one continuous value on one clip, live at the clip as
    /// it moves.
    ///
    /// Honest about what it does: the sound is *resampled*, so the pitch goes up
    /// with the rate, which is what the tape in the title means.
    pub(crate) fn speed_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, idx) = self.speed_open?;
        let speed = self.card_speed();
        let session = self.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx).copied()?;
        let lo = f32::from(Speed::MIN.permille());
        let hi = f32::from(Speed::MAX.permille());
        let frac = ((f32::from(speed.permille()) - lo) / (hi - lo)).clamp(0., 1.);
        let presets: Vec<_> = SPEED_PRESETS
            .into_iter()
            .map(|permille| {
                let at = Speed::from_permille(permille);
                div()
                    .id(("speed-preset", usize::from(permille)))
                    .flex_1()
                    .flex()
                    .h(px(CONTROL_H))
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .bg(rgb(match at == speed {
                        true => BG_SELECTED(),
                        false => BG_PANEL(),
                    }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_speed(at, cx);
                    }))
                    .child(format!("{at}"))
            })
            .collect();
        Some(
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Speed (tape) — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(
                                    "drag the bar or ←→ moves it, r is 1.00x — the pitch moves with the rate; a click away or esc closes",
                                ),
                        )
                        .child(
                            // 4 px to look at and a whole row to hit (WCAG
                            // 2.5.8), the split the colour sliders and the ruler
                            // both make.
                            div()
                                .id("speed-bar")
                                .relative()
                                .w_full()
                                .h(px(KEYS_ROW_H))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        this.speed_dragging = true;
                                        this.drag_speed(event.position.x, true, cx);
                                    }),
                                )
                                .child(bounds_probe(self.speed_bar.clone()))
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(rgb(BG_PANEL()))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(relative(frac))
                                                .rounded(px(2.))
                                                .bg(rgb(ACCENT_PRIMARY())),
                                        ),
                                ),
                        )
                        .child(div().flex().gap(px(4.)).children(presets))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                // What the choice *is*, in the numbers the
                                // timeline is measured in: the source range
                                // never moves, the room it takes does.
                                .child(format!(
                                    "{speed} — {} source frames over {} on the timeline ({})",
                                    clip.len(),
                                    clip.frames(),
                                    timecode(f64::from(clip.frames()) / self.fps, self.fps)
                                )),
                        ),
                ),
        )
    }

    /// The mix card: one fader per audio track and the master limiter under
    /// them -- the two settings that belong to the *sound of the whole
    /// timeline* rather than to any clip on it.
    ///
    /// A track's fader moves everything on that track by the same amount,
    /// every frequency of it: it is not the equalizer (one take, one band) and
    /// it is not the volume in the panel, which is what this machine monitors
    /// at and is written to no file. The limiter is over the sum of them all,
    /// which is where a mix can pass full scale and where a clamp used to
    /// square it off.
    ///
    /// The silence card's shape, down to the steppers: a row is a label, a
    /// value and the two presses that move it, and the arrows pick a row and
    /// move it too. The rows scroll rather than the card growing past the
    /// window -- a timeline may hold more tracks than a 360 px window has room
    /// for faders.
    pub(crate) fn mix_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.mix_open {
            return None;
        }
        let session = self.session.as_ref();
        let lanes = self.mix_lanes();
        let limiter = session.map_or_else(Limiter::default, PlaybackSession::limiter);
        let mut rows: Vec<(String, String)> = lanes
            .iter()
            .map(|&lane| {
                let db = session.map_or(0., |s| s.lane_gain_db(lane));
                (format!("{} plays at", lane.label()), format!("{db:+.0} dB"))
            })
            .collect();
        // The ceiling in dBFS and the faders in dB, the silence card's rule:
        // a ceiling is a level below full scale, a fader is a change.
        rows.push((
            "Limiter ceiling".into(),
            format!("{:+.0} dBFS", limiter.ceiling_db),
        ));
        rows.push((
            "Limiter".into(),
            match limiter.on {
                true => "on".into(),
                false => "off".into(),
            },
        ));
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(n, (label, value))| {
                div()
                    .id(("mix-row", n))
                    .flex()
                    .flex_none()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .rounded(px(3.))
                    .bg(rgb(match n == self.mix_field {
                        true => BG_SELECTED(),
                        false => BG_PANEL(),
                    }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.mix_field = n;
                        cx.notify();
                    }))
                    .child(div().text_color(rgb(FG_SECONDARY())).child(label))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(value)
                            .children([-1, 1].map(|steps: i32| {
                                div()
                                    .id(("mix-step", n * 2 + usize::from(steps > 0)))
                                    .flex_none()
                                    .w(px(HIT_MIN))
                                    .h(px(KEYS_ROW_H))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .bg(rgb(BG_PANEL()))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(BG_HOVER())))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        // Picked as well as moved, the silence
                                        // card's rule: the row a press lands on
                                        // is the row the arrows carry on from.
                                        this.mix_field = n;
                                        this.nudge_mix(steps, cx);
                                    }))
                                    .child(match steps > 0 {
                                        true => "+",
                                        false => "−",
                                    })
                            })),
                    )
            })
            .collect();
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .max_h(px(360. - 24.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .child("Mix — track volumes and the master limiter"),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(
                                    "− and + move a setting, or ↑↓ picks one and ←→ moves it (hold to run it) — a track fader moves everything on that track; a click away or esc closes",
                                ),
                        )
                        .child(
                            div()
                                .id("mix-rows")
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                // What the choice *is*: the limiter's own line,
                                // because "on" alone says nothing about what it
                                // does to a mix that never reaches the ceiling.
                                .child(match limiter.on {
                                    true => format!(
                                        "the mix is held under {:+.0} dBFS — quieter passages are untouched",
                                        limiter.ceiling_db
                                    ),
                                    false => "the limiter is out of circuit — a hot mix clips at full scale".to_string(),
                                }),
                        ),
                ),
        )
    }

    /// The silence card: what the scan is looking for, what it found, and the
    /// two things that can be done about it.
    ///
    /// Its scrim is the lightest of the cards' on purpose. Every other card is
    /// about the clip it names and can black the timeline out; this one is
    /// *about* the timeline -- the marks under it are the whole preview -- so
    /// the bed stays readable and the card sits up in the picture area rather
    /// than over the lanes.
    pub(crate) fn silence_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, idx) = self.silence_open?;
        let cfg = self.silence;
        // The unit is a label, never a conversion: the threshold is a level
        // below full scale whichever of the two the row says (`silence_dbfs`).
        let unit = match self.silence_dbfs {
            true => "dBFS",
            false => "dB",
        };
        let rows = [
            ("Apply to", self.silence_scope.label(&self.silence_lanes())),
            (
                "Silence is under",
                format!("{:.0} {unit}", cfg.threshold_db),
            ),
            ("Level read in", format!("{unit} (0 = full scale)")),
            (
                "Forgive quiet shorter than",
                format!("{:.2} s", cfg.min_silence),
            ),
            (
                "Keep either side of speech",
                format!("{:.2} s", cfg.padding),
            ),
            (
                "Swallow kept slivers under",
                format!("{:.2} s", cfg.min_keep),
            ),
            ("Speed-up plays at", format!("{}", self.silence_factor)),
        ];
        let found = self.silence_marks.len();
        let secs =
            f64::from(self.silence_marks.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        // The line under the rows: where a scan still running has got to -- a
        // card is up from the frame it is asked for, numbers or no numbers --
        // or what the settings found in levels already read.
        let status = match &self.silence_scan {
            Some(scan) => silence_line(
                scan.seen as f32 / 10.,
                scan.progress.total.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.,
                scan.started.elapsed().as_secs_f32(),
                scan.since.elapsed().as_secs_f32(),
            ),
            None => match found {
                0 => "nothing quiet enough for long enough".to_string(),
                1 => format!("1 silence, {}", secs_label(secs)),
                n => format!("{n} silences, {}", secs_label(secs)),
            },
        };
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(n, (label, value))| {
                div()
                    .id(("silence-row", n))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .rounded(px(3.))
                    .bg(rgb(match n == self.silence_field {
                        true => BG_SELECTED(),
                        false => BG_PANEL(),
                    }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.silence_field = n;
                        cx.notify();
                    }))
                    .child(div().text_color(rgb(FG_SECONDARY())).child(label))
                    // The value and the two steps that move it. Every other
                    // card has something to drag or press; this one had the
                    // arrow keys and nothing else, so a row was a setting a
                    // pointer could pick but never change. One press each, the
                    // same call the arrows make -- the hold-to-run is the
                    // keyboard's own and is not a thing a button has.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(value)
                            .children([-1, 1].map(|steps: i32| {
                                div()
                                    .id(("silence-step", n * 2 + usize::from(steps > 0)))
                                    .flex_none()
                                    .w(px(HIT_MIN))
                                    .h(px(KEYS_ROW_H))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .bg(rgb(BG_PANEL()))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(BG_HOVER())))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        // Picked as well as moved: the row a
                                        // press lands on is the row the arrows
                                        // carry on from.
                                        this.silence_field = n;
                                        this.nudge_silence(steps);
                                        cx.notify();
                                    }))
                                    .child(match steps > 0 {
                                        true => "+",
                                        false => "−",
                                    })
                            })),
                    )
            })
            .collect();
        // The two buttons the ask names, side by side: a mode toggle would hide
        // one of them behind the other, and there are only two.
        let button = |n: usize, text: String, act: fn(&mut Self, &mut Context<Self>)| {
            div()
                .id(("silence-apply", n))
                .flex_1()
                .flex()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .bg(rgb(match found {
                    0 => BG_PANEL(),
                    _ => BG_SELECTED(),
                }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| act(this, cx)))
                .child(text)
        };
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(HEADER_H + 8.))
                // Light enough to read the lanes and the marks on them through:
                // the preview is the point of this card.
                .bg(rgba(SCRIM_LIGHT()))
                // Click away closes it, as on every card here -- and the marks
                // go with it, which is what makes this one a call and not a flag.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Silences — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(
                                    "− and + move a setting, or ↑↓ picks one and ←→ moves it (hold to run it) — the marks on the lane are what would go; a click away or esc closes",
                                ),
                        )
                        .children(rows)
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_color(rgb(match (&self.silence_scan, found) {
                                    (None, 1..) => ACCENT_PRIMARY(),
                                    _ => FG_SECONDARY(),
                                }))
                                .child(status),
                        )
                        .child(div().flex().gap(px(4.)).children([
                            button(0, "Cut them out (enter)".into(), Self::cut_silences),
                            button(
                                1,
                                format!("Play them at {} (f)", self.silence_factor),
                                Self::speed_silences,
                            ),
                        ])),
                ),
        )
    }
}
