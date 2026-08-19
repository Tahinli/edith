//! The media library: the left panel's rows and the menu over them.

use crate::*;
use crate::ui::widgets::*;

impl Player {
    /// The media library: a row per source the timeline knows, in the order
    /// they arrived, each wearing the tint its clips wear in the lanes -- the
    /// swatch *is* what says which boxes down there came from this file. A
    /// click picks a row, the button under the list drops that source in at the
    /// playhead, and a row dragged onto either lane does the same thing through
    /// the same call.
    ///
    /// Import lives here, because this is the list it adds to. Plain divs like
    /// the rest of this window: nothing in it takes focus, so the root keeps
    /// the keyboard and the play key still works after a row is clicked
    /// (ledger:182).
    pub(crate) fn library(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let exporting = self.exporting().is_some();
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        // Every source matches the first in size and rate or it was refused at
        // the door (the import policy, ledger:436), so the session's own meta
        // describes every row and nothing has to be probed to say so.
        let meta = self.session.as_ref().map(PlaybackSession::meta);
        // Its own length, not one derived from what is on the lanes: a row
        // imported and never placed is a row with a length, and it is the
        // length a drag would put down.
        let rows: Vec<_> = library_rows(
            sources,
            &self.streams,
            &self.decoders,
            self.timeline_audio(),
            |path| {
                self.session
                    .as_ref()
                    .map_or(0, |session| session.file_frames(path))
            },
        )
        .into_iter()
        // The tab is the one filter: `enumerate` stays *before* it, so a row's
        // index is still its index in the whole library and the tints, the
        // menus and the drags all keep naming the same file whichever tab is
        // open.
        .enumerate()
        .filter(|(_, row)| self.library_tab.holds(&row.path))
        .map(|(i, row)| {
            let picked = self
                .selected_asset
                .as_ref()
                .is_some_and(|p| *p == (row.path.clone(), row.stream));
            let name: SharedString = row.name.clone().into();
            let says: String = match (&row.unusable, meta) {
                // A greyed row says why in full, where its length would be:
                // the list is the one place the file's own tracks are named.
                (Some(why), _) => format!("{} — {why}", row.path.display()),
                // A file with no picture has no size and no frame rate to
                // report, and only one lane it can go on: saying so is the
                // difference between a hint and a lie.
                (None, _) if engine::is_audio(&row.path) => format!(
                    "{} — audio only · drag onto the audio lane, or Add at playhead",
                    row.path.display()
                ),
                // A still is the mirror: its own size (the timeline's meta
                // describes video, and a picture placed on that canvas is not
                // the same shape), no frame rate, and one kind of lane.
                (None, _) if engine::is_image(&row.path) => format!(
                    "{} — still image{} · drag onto a video lane, or Add at playhead",
                    row.path.display(),
                    match self.sizes.get(&row.path).copied().flatten() {
                        Some((w, h)) => format!(" · {w}x{h}"),
                        None => String::new(),
                    }
                ),
                // The file's *own* frame rate, not the timeline's: a clip shot
                // at another rate plays at the speed it was shot at, and the
                // row that says where it came from has to say which rate that
                // was.
                (None, Some(meta)) => format!(
                    "{} — {}x{} @ {:.2} fps · drag it where you want it, or Add at playhead",
                    row.path.display(),
                    meta.width,
                    meta.height,
                    self.session
                        .as_ref()
                        .map_or(meta.frame_rate, |session| session.file_fps(&row.path))
                ),
                (None, None) => row.path.display().to_string(),
            };
            // The menu is the third way into a row, after the click and the
            // drag, and a right-click nothing advertises is one nobody finds.
            let tip: SharedString = format!("{says} · right-click for more").into();
            let ghost = name.clone();
            // What the second line says: the stream, then either its length or
            // the reason it cannot be used.
            let under = match &row.unusable {
                Some(why) => join_detail(&row.detail, why),
                // A still has no length to report -- the ten minutes it is
                // *held* to is a wall, not a duration -- so the line says what
                // it is and how big it is instead.
                None if engine::is_image(&row.path) => join_detail(
                    &row.detail,
                    &match self.sizes.get(&row.path).copied().flatten() {
                        Some((w, h)) => format!("still image · {w}x{h}"),
                        None => "still image".to_string(),
                    },
                ),
                None => join_detail(
                    &row.detail,
                    &timecode(f64::from(row.frames) / self.fps, self.fps),
                ),
            };
            // ...and what this file's stand-in is up to, after its length: a
            // percentage while one is being made, the word while there is one,
            // and nothing at all for a film that wants none ([`Proxy::detail`]).
            let under = join_detail(
                &under,
                &self
                    .proxies
                    .get(&row.path)
                    .map_or_else(String::new, Proxy::detail),
            );
            // A stand-in being made is minutes of this machine, and the row
            // that says how far it has got is where the way out of it belongs.
            // Gone the moment the worker settles, like the percentage beside
            // it: there is nothing left to stop.
            // The switch, and what it is showing. One control for the whole
            // life of a stand-in ([`Player::toggle_proxy`]): a filled dot for
            // one that exists, a stop square while one is being made -- with
            // how far it has got drawn under it -- and a hollow dot for a row
            // that has none. Never an ×: that shape means the file leaves the
            // library, and taking a *stand-in* off a row is not that.
            let (glyph, proxy_on, progress, waiting) = match self.proxies.get(&row.path) {
                Some(Proxy::Ready) => ("●", true, None, false),
                Some(Proxy::Making(job)) => ("■", true, Some(job.progress()), false),
                // Asked for and already winding down: the same square, dimmed,
                // so a second click does not look like the first one missed.
                Some(Proxy::Cancelling(_)) => ("■", true, None, true),
                Some(Proxy::Asked) => ("○", false, None, true),
                _ => ("○", false, None, false),
            };
            // What the row is drawing right now, carried into the click: a
            // stand-in that finishes between this paint and the pointer going
            // down must not turn a stop into a deletion
            // ([`Player::toggle_proxy`]).
            let showed_stop = glyph == "■";
            let stop_path = row.path.clone();
            let stop_tip: SharedString = match self.proxies.get(&row.path) {
                Some(Proxy::Making(_)) => format!(
                    "Stop making the stand-in for {} — nothing of it is kept, and the film itself \
                     is what plays",
                    row.name
                ),
                Some(Proxy::Cancelling(_)) => {
                    format!("Stopping the stand-in for {}…", row.name)
                }
                Some(Proxy::Ready) => format!(
                    "Stand-in ON for {} — click to delete it; the film itself is what plays after \
                     that, and nothing of the film is touched",
                    row.name
                ),
                _ => format!("Stand-in OFF for {} — click to make one", row.name),
            }
            .into();
            let usable = row.unusable.is_none();
            // What is left of the row for words: the switch and the gap before
            // it, on the rows that carry one. Both lines are held to it -- the
            // name alone was, and the detail line under it ran on under the
            // switch and was cut mid-word -- and an unusable row draws no switch
            // and so pays nothing for one.
            // Two switches now sit at the end of a usable row -- the preview
            // play button beside the stand-in toggle -- so the text gives up
            // twice the room it gave up for one.
            let text_w = row_text_w(width)
                - match usable {
                    true => 2. * (HIT_MIN + 6.),
                    false => 0.,
                };
            let (path, stream) = (row.path.clone(), row.stream);
            let dragged = (path.clone(), stream);
            let menu_path = path.clone();
            let preview_path = path.clone();
            let preview_tip: SharedString =
                format!("Preview {} — plays it without touching the timeline", row.name).into();
            div()
                .id(("asset", i))
                .flex_none()
                .h(px(ROW_H))
                .flex()
                .items_center()
                .gap(px(6.))
                .pr(px(6.))
                .rounded(px(3.))
                // A row that cannot be placed takes no click and no drag, and
                // reads as unavailable rather than merely unlucky.
                .when(!usable, |d| d.text_color(rgb(FG_SECONDARY())).opacity(0.55))
                .when(usable, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(rgb(BG_HOVER())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.selected_asset = Some((path.clone(), stream));
                            cx.notify();
                        }))
                        // The drag carries the row's file and stream and the
                        // row's name: one for the drop to insert, one for the
                        // pointer to carry, so what is being dragged is legible
                        // on the way down.
                        .on_drag(AssetDrag(dragged.0, dragged.1), move |_, _, _, cx| {
                            cx.new(|_| Tip(ghost.clone()))
                        })
                })
                // The right button hangs the row's own menu at the pointer.
                // Every row takes it, greyed ones included: a file that cannot
                // join this timeline can still be revealed, described and taken
                // out of the list, and Add is the one item that then refuses --
                // in the engine's words, where the row's grey already says why.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        if this.modal() {
                            return;
                        }
                        // Picked as a left-click would pick it, so the row the
                        // menu is about is the row that reads as chosen -- but
                        // only a row that *can* be picked, which is what keeps
                        // the Add button under the list honest.
                        if usable {
                            this.selected_asset = Some((menu_path.clone(), stream));
                        }
                        this.library_menu = Some(LibraryMenu {
                            path: menu_path.clone(),
                            stream,
                            at: event.position,
                            details: false,
                        });
                        cx.notify();
                    }),
                )
                .when(picked, |d| d.bg(rgb(BG_SELECTED())).border_1())
                .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                // Full height and hard against the edge: the tint reads as
                // the lane's colour continuing into the list, not as a chip
                // that happens to be near it.
                .child(
                    div()
                        .flex_none()
                        .w(px(SWATCH_W))
                        .h_full()
                        .rounded(px(2.))
                        .bg(rgb(source_tint(row.tint))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        // Two lines rather than two columns: at the width
                        // this panel yields to, a name and a timecode side
                        // by side leave room for neither.
                        // Cut out of the middle, not the end: two episodes off
                        // one release are the same words up to the number, and
                        // the number is at the end.
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                // Less the stand-in switch and the gap before
                                // it: a media row carries one where a subtitle
                                // row -- built to the same `row_text_w` -- does
                                // not, so the difference is taken here.
                                .child(clip_middle(&name, text_w)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(10.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(clip_middle(&under, text_w)),
                        ),
                )
                // The stand-in switch, on every row that can carry one. A
                // `HIT_MIN` target that stops the click there: the row under it
                // picks the file, and turning a stand-in on or off is not a way
                // of choosing what to place. The selection language the rest of
                // the window speaks says which way it is set -- lit like a
                // picked row when there is one, dimmed like an unusable one
                // when there is not.
                .when(usable, |d| {
                    d.child(
                        div()
                            .id(("preview-play", i))
                            .flex_none()
                            .w(px(HIT_MIN))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .text_size(px(11.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(BG_HOVER())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(preview_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.open_preview(&preview_path, cx);
                            }))
                            .child("▶"),
                    )
                })
                .when(usable, |d| {
                    d.child(
                        div()
                            .id(("proxy-toggle", i))
                            .flex_none()
                            .w(px(HIT_MIN))
                            .h_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(2.))
                            .rounded(px(3.))
                            .text_size(px(10.))
                            .when(proxy_on, |d| d.bg(rgb(BG_SELECTED())))
                            .when(!proxy_on, |d| d.text_color(rgb(FG_SECONDARY())))
                            .when(waiting, |d| d.opacity(0.55))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(BG_HOVER())))
                            .tooltip(move |_, cx| cx.new(|_| Tip(stop_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_proxy(&stop_path, showed_stop, cx);
                            }))
                            .child(glyph)
                            // How far the encode has come, under the stop it is
                            // stopped with: a bar rather than only the row's
                            // "proxy 37%", so the control reads as a job that is
                            // running and not as a thing that removes something.
                            .children(progress.map(|p| {
                                div()
                                    .flex_none()
                                    .w(px(PROXY_BAR_W))
                                    .h(px(2.))
                                    .rounded(px(1.))
                                    .bg(rgb(BG_RAISED()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(px(PROXY_BAR_W * p.clamp(0., 1.)))
                                            .rounded(px(1.))
                                            .bg(rgb(ACCENT_PRIMARY())),
                                    )
                            })),
                    )
                })
        })
        .collect();
        div()
            .id("library")
            .flex_none()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .bg(rgb(BG_PANEL()))
            // The column itself scrolls at a short window. Its five stacked
            // things want 140 px and the 640x360 floor gives the region 136, so
            // the list -- the only child that may shrink -- was being squeezed to
            // nothing: tabs, an Import button, a way to add at the playhead, and
            // no sight of what had been imported. Everything keeps its own size
            // and the column carries the remainder, which is the same answer the
            // lanes and the inspector give.
            .overflow_y_scroll()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            // Wraps rather than clipping: at the 640 px floor
                            // this column is 128 px wide and the third tab
                            // would hang off its edge, which is a control the
                            // pointer cannot reach.
                            .flex_wrap()
                            .gap(px(2.))
                            .children(LIBRARY_TABS.map(|tab| {
                                let on = tab == self.library_tab;
                                div()
                                    .id(("library-tab", tab as usize))
                                    .flex_none()
                                    .h(px(HIT_MIN))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(3.))
                                    .text_size(px(11.))
                                    .text_color(rgb(match on {
                                        true => FG_PRIMARY(),
                                        false => FG_SECONDARY(),
                                    }))
                                    // One selection language everywhere: the
                                    // same stroke a picked clip and a picked
                                    // row wear.
                                    .when(on, |d| {
                                        d.bg(rgb(BG_SELECTED()))
                                            .border_b_2()
                                            .border_color(rgb(STROKE_SELECTED()))
                                    })
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(BG_HOVER())))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.library_tab = tab;
                                        cx.notify();
                                    }))
                                    .child(tab.label())
                            })),
                    )
                    ,
            )
            // The tab says what the one full-width control is for. Text holds
            // no rows of its own ([`LibraryTab::holds`]) -- it is the tracks
            // under it -- and what a person opens it to do is put subtitles on
            // this timeline: off a release's `.mkv`, off an `.srt` beside it,
            // without the file itself joining the library or a lane. So that is
            // the button there, where the tracks it adds are listed; media goes
            // on getting in from the other two tabs, from a drop, and from the
            // actions card.
            .when(self.library_tab == LibraryTab::Text, |d| {
                d.child(div().flex_none().child(self.action_control(
                    "add-subtitles",
                    width - 16.,
                    match self.session.as_ref().is_some_and(|s| !s.subtitles().is_empty()) {
                        // The primary action of this tab while it has nothing:
                        // the same rule the Import button follows one tab over.
                        false => ACCENT_PRIMARY(),
                        true => BG_RAISED(),
                    },
                    None,
                    // The column is 128 px at the 640x360 floor and the whole
                    // name does not fit it: a centred label wider than its
                    // button is clipped at *both* ends ("d subtitles from a
                    // fil"), which is a button that says nothing. The short
                    // form there; the full name is what the tooltip, the
                    // actions card and the stroke go on saying either way.
                    match (width - 16.) / LIST_CHAR_W >= 26. {
                        true => "Add subtitles from a file…",
                        false => "Add subtitles",
                    },
                    "reads a file's subtitle tracks into the palette below — drag one onto an S \
                     track to place it; the file itself joins nothing",
                    ActionId::ImportSubtitles,
                    cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_add_subtitles(cx)),
                )))
            })
            .when(self.library_tab != LibraryTab::Text, |d| {
                d.child(div().flex_none().child(control(
                        "import",
                        // Full width of the column: the way media gets in is
                        // the one affordance of the tabs that hold media,
                        // however narrow the window is.
                        width - 16.,
                        // Filled with the accent while there is nothing to work
                        // on, because with an empty library this *is* the
                        // primary action -- and Export, which wears the accent
                        // the rest of the time, is dimmed until something is
                        // imported. One live accent in the window either way.
                        match rows.is_empty() {
                            true => ACCENT_PRIMARY(),
                            false => BG_RAISED(),
                        },
                        None,
                        "Import",
                        "adds a file to this list — or drop one on the window".to_string(),
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)),
                    )))
            })
            .child(
                div()
                    .id("library-rows")
                    .flex_1()
                    // Never nothing: a media list with no room left is a library
                    // column that has stopped being a library column.
                    .min_h(px(ROW_H))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .overflow_y_scroll()
                    // Never a blank column: with nothing imported the list is
                    // where the way in is said.
                    .when(rows.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(self.library_tab.empty()),
                        )
                    })
                    .children(rows),
            )
            // Under the media it belongs to: a subtitle track is not a source --
            // it goes on no lane and is dragged nowhere -- but it is a thing the
            // timeline holds, and this is the list of those.
            .when(self.library_tab == LibraryTab::Text, |d| {
                d.children(self.subtitle_section(width, cx))
            })
            .child(control(
                "add-asset",
                0.,
                BG_RAISED(),
                None,
                "Add at playhead",
                match self.selected_asset {
                    Some(_) => "inserts the picked file at the playhead".to_string(),
                    None => "click a file above first — or drag one where you want it".to_string(),
                },
                can_add(
                    self.selected_asset.as_ref(),
                    self.session.is_some(),
                    exporting,
                ),
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some((path, stream)) = this.selected_asset.clone() {
                        // No lane: the button means "wherever this belongs",
                        // which for a file with no picture is the audio lane.
                        this.insert_source(&path, stream, None, None, cx);
                    }
                }),
            ))
    }

    /// The menu a right-click on a library row opens: what can be done with the
    /// *file* rather than with a clip of it, and a turn-over side saying what
    /// that file is. Built like [`Player::context_card`] down to the scrim, the
    /// row height and the clamp, because it is the same menu on the other panel
    /// -- a click away or any stroke closes it.
    pub(crate) fn library_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.library_menu.clone()?;
        let path = menu.path.clone();
        let row = |n: usize| {
            div()
                .id(("library-menu", n))
                .flex()
                .min_h(px(MENU_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        // What every item of this menu is answered from, read once.
        let ctx = self.row_ctx(&path, menu.stream);
        if menu.details {
            // What the library knows about this row and nothing probed for the
            // card: the streams table is filled once per file at import.
            let info = self
                .streams
                .get(&path)
                .and_then(|of_file| of_file.iter().find(|s| s.index == menu.stream));
            let frames = self
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(&path));
            // How many clips play from this exact row -- the number that
            // decides whether Remove is refused, so the card answers the
            // question the refusal would otherwise raise.
            let placed = ctx.placed;
            // A still is described by what it has -- a picture, a size, and a
            // longest it may be held for -- where a media file is described by
            // its streams and its length. Same card, the rows that mean
            // something for this kind of source.
            let image = engine::is_image(&path);
            let kind = match self.sizes.get(&path).copied().flatten() {
                Some((w, h)) => format!("still image · {w}x{h}"),
                None => "still image".to_string(),
            };
            for (label, value) in [
                ("File", file_name(&path)),
                ("Path", path.display().to_string()),
                match image {
                    true => ("Picture", kind),
                    false => (
                        "Audio",
                        info.map_or_else(|| "no track of its own".to_string(), stream_detail),
                    ),
                },
                (
                    "Bitrate",
                    bitrate_detail(
                        self.bitrates.get(&path).copied().flatten(),
                        self.streams.get(&path).map_or(0, Vec::len),
                    ),
                ),
                match image {
                    true => (
                        "Longest hold",
                        timecode(f64::from(frames) / self.fps, self.fps),
                    ),
                    false => ("Length", timecode(f64::from(frames) / self.fps, self.fps)),
                },
                ("On the timeline", format!("{placed} clips")),
            ] {
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(value),
                        )
                        .into_any_element(),
                );
            }
        } else {
            // The oracle's list, exactly as the clip menu takes its rows from
            // `menu_items`: an item that means nothing for the file that was
            // right-clicked is not a row, and one this moment refuses is drawn
            // dimmed and says why in place of its hint.
            for item in row_items(ctx) {
                let refusal = row_enable(item, ctx);
                let enabled = refusal.yes();
                rows.push(
                    row(rows.len())
                        .child(item.label())
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .text_color(rgb(FG_SECONDARY()))
                                .child(refusal.why().unwrap_or_else(|| item.hint())),
                        )
                        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.act_on_row(item, cx);
                                }))
                        })
                        .into_any_element(),
                );
            }
        }
        // Placed by the height it is drawn to, and drawn to what the window has
        // room for -- the clip menu's rule, one function for all three.
        let list_h = menu_rows_h(rows.len(), viewport);
        let (x, y) = menu_at(menu.at, viewport, MENU_PAD * 2. + list_h);
        let full: SharedString = path.display().to_string().into();
        Some(
            scrim()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.library_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.library_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .id("library-menu-card")
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(MENU_W))
                        .flex()
                        .flex_col()
                        .p(px(MENU_PAD))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        // Painted after the scrim, so this listener runs first
                        // and a press meant for an item never closes the menu
                        // out from under its own click (`context_card`).
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .when(menu.details, |d| {
                            d.tooltip(move |_, cx| cx.new(|_| Tip(full.clone())).into())
                        })
                        // Scrolls where the window has no room for the list,
                        // like the clip menu's -- an item hanging off the bottom
                        // edge is an item nobody can click.
                        .child(
                            div()
                                .id("library-menu-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        )
    }
}
