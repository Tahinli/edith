//! The bars and the floating cards that are not inspector sections.

use crate::ui::hitmap;
use crate::ui::popup::{popup_plate, popup_row, popup_row_icon, popup_rule, popup_w, shortcut};
use crate::ui::stance::{menu_above, menu_floor, picture_bottom};
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::*;

impl Player {
    /// The running import holds its own bar above the notice's, for the notice
    /// bar's reason: the message is the point. Not a notice, though -- nothing
    /// dismisses it, because it is about work still going on, and it leaves by
    /// itself when the file lands.
    ///
    /// The bar under the words is a *sweep*, not a fill: neither read reports
    /// where in the file it is, so a fill would have to invent the one number
    /// this cannot know. What it does say truthfully is "something is still
    /// running", which is exactly the question a frozen-looking window raises.
    ///
    /// ...and beside them the way out. A read that may be twenty seconds of a
    /// cold 24 GB film needs one, and the only honest place for it is the line
    /// that says the read is happening ([`Player::cancel_import`]).
    pub(crate) fn import_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let import = self.importing.as_ref()?;
        let elapsed = import.started.elapsed().as_secs_f32();
        let line = import_line(
            &file_name(&import.path),
            import.seen,
            elapsed,
            import.since.elapsed().as_secs_f32(),
            self.imports.len(),
            arrival(self.opening.as_deref(), &import.path) != Landing::Import,
        );
        // A quarter of the bar, crossing it every three seconds and wrapping.
        // Tied to the elapsed clock, so it moves for as long as the read does
        // and stops dead the instant the file lands.
        const SWEEP: f32 = 0.25;
        let at = (elapsed / 3.).fract() * (1. + SWEEP) - SWEEP;
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(BG_RAISED()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().min_w(px(0.)).child(line))
                        .child(control(
                            "cancel-import",
                            // Hugs its label: the bar's own line is what wants
                            // the width, and this button never relabels.
                            0.,
                            // The ordinary plane -- the accent is Export's, and
                            // a way out of a read is not the primary action.
                            BG_RAISED(),
                            None,
                            "Cancel",
                            "stops this file and anything queued behind it, and the read with \
                             them where the container lets it stop"
                                .to_string(),
                            true,
                            cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_import(cx)),
                        )),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(2.))
                        .bg(rgb(BG_HOVER()))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left(relative(at.max(0.)))
                                // Clipped at both ends by hand: the segment
                                // enters from the left edge and leaves past the
                                // right one, and a width that overhung would
                                // paint outside the track.
                                .w(relative((at + SWEEP).min(1.) - at.max(0.)))
                                .h(px(2.))
                                .bg(rgb(ACCENT_PRIMARY())),
                        ),
                ),
        )
    }

    /// Whether the picture under the playhead comes from a file that is not on the disk.
    pub(crate) fn picture_is_missing(&self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let offline = session.offline_sources();
        !offline.is_empty()
            && self.under_playhead().into_iter().next().is_some_and(|(lane, idx)| {
                session.lane_clips(lane).get(idx).is_some_and(|c| offline.iter().any(|(i, _)| *i == c.source))
            })
    }

    /// The line a long-standing seek shows, in the import bar's place and by the
    /// import bar's rules: nothing dismisses it, and it leaves by itself the
    /// moment the frame lands. Once the picture is plainly not coming it says why. No sweep under it -- an open reports nothing about
    /// where it has got to, and the clock is the honest half of that bar anyway.
    pub(crate) fn seek_bar(&self) -> Option<impl IntoElement> {
        // A missing file is the banner's to say (`missing_clip_note`), once, at the top of the
        // picture: a second note here would repeat it. This line is for a read that failed.
        if self.picture_is_missing() {
            return None;
        }
        let line = seek_line(self.seek_since.map(|t| t.elapsed()))?;
        // The same plate as the top banner, centred, so the two read as one family.
        let t = crate::ui::theme::tokens();
        Some(
            div().flex_none().flex().justify_center().px(px(12.)).py(px(6.)).child(
                div()
                    .flex()
                    .items_center()
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .bg(rgba(crate::ui::theme::with_alpha(t.e0, 0xD9)))
                    .border_1()
                    .border_color(rgba(crate::ui::theme::with_alpha(t.warn, 0x80)))
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(div().whitespace_nowrap().child(line)),
            ),
        )
    }

    /// The menu a right-click on a clip opens: what that clip can be given,
    /// each item beside the stroke that does the very same thing, and a
    /// turn-over side that says what the clip *is*. An item that would do
    /// nothing where the playhead is standing is dimmed and takes no click
    /// rather than disappearing, so the menu reads the same every time.
    ///
    /// Every item goes through [`Player::act`], the table the key handler uses:
    /// an item is its stroke, asked for with the mouse. Plain divs like the
    /// rest of this window, so the root keeps the keyboard and escape still
    /// reaches the handler that closes this.
    pub(crate) fn context_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.context_menu?;
        let session = self.session.as_ref()?;
        // Both `None` when the menu was opened on a caption: a subtitle lane
        // holds no `Clip` and a caption plays no source file. The rows are the
        // oracle's either way ([`Ctx::caption`]), so what the card *has* is
        // asked for where it is used rather than at the door -- refusing here
        // would be a right-click that opens nothing.
        let idx = match menu.on {
            MenuOn::Clip(idx) => Some(idx),
            MenuOn::Gap(..) | MenuOn::Bench | MenuOn::Empty | MenuOn::Head | MenuOn::Dock | MenuOn::Viewer | MenuOn::SeqTab | MenuOn::Bin | MenuOn::Marker => None,
        };
        let clip = idx.and_then(|idx| session.lane_clips(menu.lane).get(idx).cloned());
        let source = clip.as_ref().and_then(|clip| session.sources().get(clip.source).cloned());
        let secs = |frames: u32| timecode(f64::from(frames) / self.fps, self.fps);
        // Every row is the shared popup row (`ui::popup`); `measure` collects
        // the text the plate's width is fitted to.
        let mut measure: Vec<(String, String)> = Vec::new();
        let mut sub_rows: Vec<AnyElement> = Vec::new();
        let mut sub_measure: Vec<(String, String)> = Vec::new();
        let mut adjust_at: Option<usize> = None;
        let mut comp_rows: Vec<AnyElement> = Vec::new();
        let mut comp_measure: Vec<(String, String)> = Vec::new();
        let mut rows: Vec<AnyElement> = Vec::new();
        // Whether the rule line before the destructive rows has been drawn.
        let mut ruled = false;
        if let (true, Some((clip, source))) = (menu.details, clip.as_ref().zip(source.clone())) {
            // Read-only, so no ids and no hover: this side is a card, not a
            // list of things to click. Each value is one truncated line, which
            // is what keeps the height below the one the clamp was given.
            // The file first, as the library's plate has it, then this clip's
            // own block: where it comes from and how it plays.
            let streams = self
                .streams
                .get(&source.path)
                .map_or(&[][..], Vec::as_slice);
            let mut lines = file_properties(
                &source.path,
                self.facts.get(&source.path).and_then(Option::as_ref),
                streams,
                self.bitrates.get(&source.path).copied().flatten(),
            );
            lines.extend([
                (
                    "Source range".to_string(),
                    format!("{} – {}", secs(clip.in_frame), secs(clip.out_frame)),
                ),
                // How long it is *where it sits*, which its rate decides -- and
                // the rate itself beside it, because a clip half its source's
                // length could be either a trim or a 2x.
                ("This clip".to_string(), secs(clip.frames())),
                (
                    "Speed".to_string(),
                    match speed_chip_text(clip).filter(|_| !clip.speed_keys.is_empty()) {
                        Some(ramp) => format!("{ramp} (ramp)"),
                        None => format!("{} (tape)", clip.speed),
                    },
                ),
                (
                    "Source duration".to_string(),
                    secs(session.file_frames(&source.path)),
                ),
            ]);
            for (label, value) in lines {
                measure.push((label.clone(), value.clone()));
                rows.push(
                    popup_row(("menu", rows.len()), label, value, None, None)
                        .cursor_default()
                        .into_any_element(),
                );
            }
            rows.push(copy_path_button(&source.path, cx));
        } else if let MenuOn::Gap(start, frames) = menu.on {
            // Gap rows are ripples, not clip actions. The first is the exact
            // hole under the pointer and wears the chord that closes a selected
            // gap (DESIGN §9: every command wears its chord); the second
            // appears only when the same track has another bounded hole too,
            // and has no chord of its own -- it is the menu's only door to it.
            let lane = menu.lane;
            let del = shortcut(&self.keymap, ActionId::Delete);
            measure.push(("Close gap".into(), del.clone()));
            rows.push(
                popup_row_icon(
                    ("menu", rows.len()),
                    "Close gap",
                    del,
                    Some(false),
                    Some(crate::ui::icons::Icon::ArrowLeftToLine),
                    self.close_gap_refusal(lane),
                )
                    .when(self.close_gap_refusal(lane).is_none(), |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.context_menu = None;
                            this.close_gap(lane, start, frames, cx);
                        }))
                    })
                    .into_any_element(),
            );
            if session.gap_count(lane) > 1 {
                measure.push(("Close all gaps on this track".into(), String::new()));
                rows.push(
                    popup_row_icon(
                        ("menu", rows.len()),
                        "Close all gaps on this track",
                        "",
                        Some(false),
                        Some(crate::ui::icons::Icon::ArrowLeftToLine),
                        None,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.context_menu = None;
                        this.close_all_gaps_on_lane(lane, cx);
                    }))
                    .into_any_element(),
                );
            }
        } else {
            // A grade on a waveform, an equalizer on a picture, a silence scan
            // on a still: things that do not exist for what was right-clicked,
            // so the menu is the list of what this clip can do rather than the
            // registry with most of it struck through. One filter, in
            // `menu_items`, so there is no second answer to keep in step. The
            // state refusals below stay, dimmed and saying why -- the next
            // click of the playhead lights them.
            // The clip's own state where a clip was clicked; the editor's
            // otherwise -- the bench and the lane head both ask about the
            // selection ([`Player::ctx`] falls back to it), which is exactly
            // what the cut machinery's rows act on.
            let ctx = self.ctx(idx.map(|idx| (menu.lane, idx)));
            // Three lists, one render: what the thing under the pointer can be
            // told (DESIGN §9). Nothing here is a second version of an action
            // -- every row is an [`ActionId`] the keyboard already reaches, so
            // the label and the chord still come out of the registry.
            let actions = match menu.on {
                MenuOn::Bench => BENCH_ITEMS.to_vec(),
                MenuOn::Empty => crate::menus::empty_items(menu.lane),
                MenuOn::Dock => DOCK_ITEMS.to_vec(),
                MenuOn::Viewer => VIEWER_ITEMS.to_vec(),
                MenuOn::SeqTab => SEQ_TAB_ITEMS.to_vec(),
                MenuOn::Marker => MARKER_ITEMS.to_vec(),
                MenuOn::Bin => BIN_ITEMS.to_vec(),
                MenuOn::Head => lane_items(menu.lane),
                _ => menu_items(ctx.clone()),
            };
            let fold = matches!(menu.on, MenuOn::Clip(_));
            let (sub_list, rest): (Vec<ActionId>, Vec<ActionId>) =
                actions.into_iter().partition(|a| fold && adjust_item(*a));
            let (comp_list, main_list): (Vec<ActionId>, Vec<ActionId>) =
                rest.into_iter().partition(|a| fold && crate::menus::compound_item(*a));
            let has_comp = !comp_list.is_empty();
            let sub_row = |rows: &mut Vec<AnyElement>,
                           measure: &mut Vec<(String, String)>,
                           which: bool,
                           cx: &mut Context<Self>| {
                let name = if which { "Compound" } else { "Adjust" };
                measure.push((name.into(), "▸".into()));
                rows.push(
                    popup_row_icon(
                        ("menu", 900usize + usize::from(which)),
                        name,
                        "▸",
                        Some(false),
                        Some(if which {
                            crate::ui::icons::Icon::Layers
                        } else {
                            crate::ui::icons::Icon::SlidersHorizontal
                        }),
                        None,
                    )
                        .on_hover(cx.listener(move |this, on: &bool, window, cx| {
                            this.adjust_hot = *on;
                            if !*on {
                                return;
                            }
                            // Where the pointer came onto the row: the apex of
                            // the safe triangle toward the submenu.
                            this.adjust_aim = Some(window.mouse_position());
                            if this.context_menu.is_some_and(|m| !m.adjust || m.compound != which) {
                                // Hover intent: opens once the pointer has
                                // rested on the row for a moment, so a
                                // pointer crossing it does not flash the
                                // submenu.
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(ADJUST_HOVER).await;
                                    let _ = this.update(cx, |this, cx| {
                                        if this.adjust_hot
                                            && let Some(m) = &mut this.context_menu
                                            && (!m.adjust || m.compound != which)
                                        {
                                            m.adjust = true;
                                            m.compound = which;
                                            cx.notify();
                                        }
                                    });
                                })
                                .detach();
                            }
                        }))
                        // A click opens it at once and never shuts it: the
                        // hover may have opened it a moment before.
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(m) = &mut this.context_menu {
                                m.adjust = true;
                                m.compound = which;
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            };
            let has_sub = !sub_list.is_empty() || has_comp;
            // The two submenu rows, Adjust then Compound, wherever the first is due.
            let sub_rows_at = |rows: &mut Vec<AnyElement>,
                               measure: &mut Vec<(String, String)>,
                               adjust_at: &mut Option<usize>,
                               cx: &mut Context<Self>| {
                *adjust_at = Some(rows.len());
                sub_row(rows, measure, false, cx);
                if has_comp {
                    sub_row(rows, measure, true, cx);
                }
            };
            // 0: the plate; 1: the Adjust submenu; 2: the Compound submenu.
            for (is_sub, action) in main_list
                .into_iter()
                .map(|a| (0u8, a))
                .chain(sub_list.into_iter().map(|a| (1, a)))
                .chain(comp_list.into_iter().map(|a| (2, a)))
            {
                if is_sub > 0 && adjust_at.is_none() {
                    // No destructive row came first.
                    sub_rows_at(&mut rows, &mut measure, &mut adjust_at, cx);
                }
                if destructive(action) && has_sub && adjust_at.is_none() {
                    sub_rows_at(&mut rows, &mut measure, &mut adjust_at, cx);
                }
                let (rows, measure) = match is_sub {
                    1 => (&mut sub_rows, &mut sub_measure),
                    2 => (&mut comp_rows, &mut comp_measure),
                    _ => (&mut rows, &mut measure),
                };
                let level = is_sub;
                let is_sub = is_sub > 0;
                // DESIGN §9's rule line: the verbs that take something away sit
                // under a seam, wherever they are listed. Drawn once, before
                // the first of them, and never as the plate's own top edge.
                if destructive(action) && !is_sub && !rows.is_empty() && !ruled {
                    ruled = true;
                    rows.push(popup_rule());
                }
                // The registry's own answer, the same one the actions card
                // dims a row with -- and a row that takes no click says *why*
                // rather than printing a stroke that would do nothing.
                let refusal = enable(action, ctx.clone());
                let enabled = refusal.yes();
                // The one item that is not about this clip says so, and says it
                // here rather than in the registry: the stroke is global too,
                // but its row in the keys menu is not sitting on a clip.
                // The registry's label is the verb itself now (cleanse round
                // 2, `a_label_names_the_verb_in_four_words`): four words at
                // most, so it fits a 260px plate beside its chord instead of
                // being cut mid-word ("Ungroup the selection (clips anc").
                // Nothing is trimmed here any more -- what a parenthesis used
                // to carry is the refusal string in the column beside it.
                let full = action.label();
                let label = match action {
                    // A switch says which way it is set: a row reading "Snap on
                    // / off" beside a chord tells nobody whether snapping is on
                    // right now, which is the one thing the spine's badge did
                    // say before it went.
                    ActionId::ToggleSnap => {
                        format!("Snap — {}", if self.snap { "on" } else { "off" })
                    }
                    ActionId::ClipLabel => "Label".to_string(),
                    // Plain words for the three that read as jargon; the action ids stay.
                    ActionId::LoopTrim => "Loop while trimming".to_string(),
                    ActionId::Dissolve => "Blend into next clip".to_string(),
                    ActionId::Regroup => "Group again".to_string(),
                    _ if matches!(menu.on, MenuOn::Head)
                        && crate::ui::bench_stance::TrackSize::of_action(action).is_some() =>
                    {
                        format!("Height: {}", crate::ui::bench_stance::TrackSize::of_action(action).map_or("", |s| s.word()))
                    }
                    ActionId::AddMarker if matches!(menu.on, MenuOn::Bench) => "Add marker here".to_string(),
                    ActionId::NewSolid
                    | ActionId::NewGradient
                    | ActionId::NewCountdown
                    | ActionId::NewBars
                    | ActionId::NewTone
                    | ActionId::NewBarsAndTone
                    | ActionId::NewAdjustment
                    | ActionId::NewRectangle
                    | ActionId::NewEllipse
                    | ActionId::NewLine
                    | ActionId::NewArrow
                    | ActionId::NewTitle => format!("New {}", full.to_lowercase()),
                    _ => full.to_string(),
                };
                let hint = match action {
                    ActionId::ClipLabel | ActionId::Fit => "▸".to_string(),
                    _ => shortcut(&self.keymap, action),
                };
                // A switch wears the menu's ✓ when on, the bar's own style.
                let check = match (action, menu.on) {
                    (ActionId::ClipEnabled, _) => Some({
                        !idx.and_then(|i| self.session.as_ref().map(|s| s.mark_of(menu.lane, i).off))
                            .unwrap_or(false)
                    }),
                    // The head's three heights are radio rows: the one the lane is at is ticked.
                    (_, MenuOn::Head) => crate::ui::bench_stance::TrackSize::of_action(action)
                        .map(|size| crate::ui::bench_stance::track_size(self, menu.lane) == size),
                    _ => None,
                };
                measure.push((label.clone(), hint.clone()));
                rows.push(
                    popup_row_icon(
                        ("menu", rows.len() + 1000 * usize::from(level)),
                        label,
                        hint,
                        Some(check.unwrap_or(false)),
                        crate::menus::icon(action),
                        refusal.why(),
                    )
                        .when(!is_sub && has_sub, |d| {
                            d.on_hover(cx.listener(|this, on: &bool, window, cx| {
                                // Another row under the pointer closes the
                                // submenu -- unless the pointer is crossing
                                // the safe triangle toward it.
                                let toward = match (this.adjust_aim, this.adjust_sub.get()) {
                                    (Some(a), Some((x, top, bottom))) => in_aim_triangle(
                                        window.mouse_position(),
                                        a,
                                        point(px(x), px(top)),
                                        point(px(x), px(bottom)),
                                    ),
                                    _ => false,
                                };
                                if let Some(m) = &mut this.context_menu
                                    && *on
                                    && m.adjust
                                    && !toward
                                {
                                    m.adjust = false;
                                    cx.notify();
                                }
                            }))
                        })
                        // Hover line for the pointer; it stands aside while the
                        // menu itself is up (`tip_may_paint`).
                        .when(enabled, |d| {
                            d.tooltip(crate::ui::widgets::tip_hover(full, "", None))
                        })
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)))
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        // Closed first: the action moves the very
                                        // indices this menu is holding.
                                        this.context_menu = None;
                                        // The one item that is a *choice* and not a
                                        // doing: four policies, so the pointer gets
                                        // the list of them on this clip rather than
                                        // the next one stepped to behind the click.
                                        // The stroke still steps -- same door.
                                        if let (ActionId::ClipLabel, Some(idx)) = (action, idx) {
                                            this.open_picker(
                                                Pick::Label(menu.lane, idx),
                                                event.position(),
                                                cx,
                                            );
                                        } else if let (ActionId::Fit, Some(idx)) = (action, idx) {
                                            this.open_picker(
                                                Pick::Fit(menu.lane, idx),
                                                event.position(),
                                                cx,
                                            );
                                        } else if let (MenuOn::Head, Some(size)) =
                                            (menu.on, crate::ui::bench_stance::TrackSize::of_action(action))
                                        {
                                            // A header's own height: this track only, not every track.
                                            this.set_lane_size(menu.lane, size, cx);
                                        } else if matches!(menu.on, MenuOn::Head)
                                            && matches!(
                                                action,
                                                ActionId::Mix
                                                    | ActionId::RenameLane
                                                    | ActionId::RemoveVideoLane
                                                    | ActionId::RemoveAudioLane
                                            )
                                        {
                                            // The head's own verbs, which left the
                                            // head itself for this menu (cleanse
                                            // round 2): they act on the lane the
                                            // hand named, the way the head's
                                            // buttons did, not on the last of the
                                            // kind the bare stroke reaches.
                                            this.act_lane(action, menu.lane, cx);
                                        } else {
                                            this.act(action, window, cx);
                                        }
                                    },
                                ))
                        })
                        .children(hitmap::dynamic(
                            move || (format!("menu.{action:?}"), full.to_string()),
                            enabled,
                        ))
                        .into_any_element(),
                );
            }
            if has_sub && adjust_at.is_none() {
                sub_rows_at(&mut rows, &mut measure, &mut adjust_at, cx);
            }
            // What a caption has no other side to turn over to: its file, its
            // source range and its bitrate are a clip's, and a row that opened
            // an empty card would be the stub this menu does not draw.
            rows.extend(clip.as_ref().map(|_| {
                measure.push(("Properties…".into(), String::new()));
                popup_row_icon(("menu", rows.len()), "Properties…", "", Some(false), Some(crate::ui::icons::Icon::Info), None)
                    .tooltip(crate::ui::widgets::tip_hover("Properties", "", None))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(menu) = &mut this.context_menu {
                            menu.details = true;
                        }
                        cx.notify();
                    }))
                    .children(hitmap::control("menu.properties", "Properties", true))
                    .into_any_element()
            }));
        }
        // The height the card is *placed* by and the height its list is drawn
        // to are one number: placed by a taller one, the card would hang off the
        // window's floor -- the very thing the clamp is for.
        // §5/§11 check 6: the picture is never covered. A right-click high on
        // the window would otherwise hang the menu at the pointer, straight
        // over the screen -- clamped below it into the bench/ledger/dock
        // footprint instead. The list is sized against that same
        // footprint (`room`), not the whole window, so a menu taller than
        // the footprint scrolls inside its own plate instead of walking its
        // clamped top edge back up over the picture ([`menu_floor`]).
        // ...and that floor is the picture's painted edge, so the black bars
        // of a letterboxed film are room the menu may use (R4).
        // A clip's menu is opened on the timeline, so the picture floor is
        // already above it; its own room is the whole window, and it opens
        // upward from the pointer when there is none below.
        let (at, room, floor) = match menu.on {
            // The viewer's menu is opened ON the picture: it hangs at the pointer. The dock
            // is beside the picture, not under it, so its menu hangs where it was opened too.
            MenuOn::Clip(_) | MenuOn::Viewer | MenuOn::Dock => (menu.at, viewport, 0.),
            _ => menu_floor(
                menu.at,
                viewport,
                self.split_px(Split::Bench, viewport),
                picture_bottom(self),
            ),
        };
        let list_h = menu_rows_h(rows.len(), room);
        let h = MENU_PAD * 2. + list_h;
        let pw = popup_w(measure.iter().map(|(l, h)| (l.as_str(), h.as_str())), true);
        let (x, y) = menu_at_w(menu_above(at, viewport, floor, h), viewport, pw, h);
        // The submenu that is out: Adjust's, or Compound's.
        let (sub_rows, sub_measure, sub_at) = match menu.compound {
            true => (comp_rows, comp_measure, adjust_at.map(|a| a + 1)),
            false => (sub_rows, sub_measure, adjust_at),
        };
        // The submenu's near edge, for the safe triangle (see `adjust_sub`).
        self.adjust_sub.set(match sub_rows.is_empty() {
            true => None,
            false => {
                let sw = popup_w(
                    sub_measure.iter().map(|(l, h)| (l.as_str(), h.as_str())),
                    true,
                );
                let sh = MENU_PAD * 2. + sub_rows.len() as f32 * MENU_ROW_H;
                let top = y + MENU_PAD + sub_at.unwrap_or(0) as f32 * MENU_ROW_H;
                let right = x + pw + sw + MENU_EDGE > f32::from(viewport.width);
                let sx = if right { x - sw - 2. } else { x + pw + 2. };
                let (sx, sy) = menu_at_w(point(px(sx), px(top)), viewport, sw, sh);
                Some((if right { sx + sw } else { sx }, sy, sy + sh))
            }
        });
        let full: SharedString = source
            .map(|source| source.path.display().to_string())
            .unwrap_or_default()
            .into();
        Some(
            scrim()
                // Click away closes it, either button, and the press is
                // swallowed so nothing under the menu also takes it. No tint,
                // unlike the modal cards: the timeline this menu is about has
                // to stay readable behind it.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.context_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.context_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    // One plate shape for all three hanging menus
                    // ([`menu_plate`]) -- the id is still this menu's own, so
                    // the details side keeps its tooltip.
                    popup_plate("menu-card", x, y, pw)
                        // Painted after the scrim, so this listener runs first
                        // (gpui bubbles mouse events in reverse, window.rs:3705)
                        // and a press meant for an item never closes the menu
                        // out from under its own click.
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        // The one value the details side has to truncate.
                        .when(menu.details, |d| {
                            d.tooltip(move |_, cx| cx.new(|_| Tip(full.clone())).into())
                        })
                        // The list scrolls where the card would otherwise grow
                        // past the window's floor -- an item hanging off the
                        // bottom edge is an item nobody can click.
                        .child(
                            div()
                                .id("menu-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                )
                // The Adjust submenu, beside the plate at its row; on the left
                // of it where the right edge has no room.
                .children((menu.adjust && !sub_rows.is_empty()).then(|| {
                    let sw = popup_w(
                        sub_measure.iter().map(|(l, h)| (l.as_str(), h.as_str())),
                        true,
                    );
                    let sh = MENU_PAD * 2. + sub_rows.len() as f32 * MENU_ROW_H;
                    let top = y + MENU_PAD + sub_at.unwrap_or(0) as f32 * MENU_ROW_H;
                    let sx = match x + pw + sw + MENU_EDGE > f32::from(viewport.width) {
                        true => x - sw - 2.,
                        false => x + pw + 2.,
                    };
                    let (sx, sy) = menu_at_w(point(px(sx), px(top)), viewport, sw, sh);
                    popup_plate("menu-sub", sx, sy, sw)
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .children(sub_rows)
                })),
        )
    }

    /// The open choice list: every value of one enumerated setting at once, the
    /// one in force marked, and a click on any of them picks *that* one -- what
    /// a button stepping one value on per click could never say. Built on the
    /// clip menu's machinery down to the scrim, the placement and the scroll
    /// cap, so it hangs and closes exactly as the menus do and fits the same
    /// 640x360 floor. The stroke for the setting is untouched: this is the
    /// pointer's door to it, not a second setting.
    pub(crate) fn picker_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let picker = self.picker?;
        let theme_picker = picker.of == Pick::Theme;
        let from_palette = picker.at == Point::default() && picker.above.is_none();
        let choices = self.choices(picker.of);
        let pw = crate::ui::popup::popup_w(
            choices.iter().map(|(_, label, detail, _)| (label.as_ref(), detail.as_ref())),
            true,
        );
        let rows: Vec<AnyElement> = choices
            .into_iter()
            .enumerate()
            .map(|(n, (choice, label, detail, picked))| {
                let hitmap_label = label.clone();
                let row_tip = label.clone();
                let hitmap_id = match theme_picker {
                    true => format!("theme.{n}.row"),
                    false => format!("picker.choice.{n}"),
                };
                // The shared popup row; the keyboard's row wears the focus ring.
                // A refused row (a format this timeline cannot be written in) is
                // dimmed and says why on hover; it takes no click.
                let why = self.export_choice_refusal(choice);
                crate::ui::popup::popup_row(("picker-row", n), label, detail, Some(picked), why.as_deref())
                    .when(n == picker.sel, |d| d.bg(rgb(DARK_RAISED())))
                    .when(why.is_none(), |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(choice, cx)))
                    })
                    .tooltip(crate::ui::widgets::overlay_tip_hover(&row_tip, "", why.as_deref()))
                    .children(hitmap::dynamic(
                        move || (hitmap_id, hitmap_label.to_string()),
                        true,
                    ))
                    .into_any_element()
            })
            .collect();
        // Anchored to the row that opened it: inside the window, flipping up
        // when the room below is short. Only a list opened over the picture
        // keeps the timeline floor; Settings is a dialog and owns its place.
        let (at, room, floor) = match self.settings_open || !picker.floored {
            true => (picker.at, viewport, 0.),
            false => menu_floor(
                picker.at,
                viewport,
                self.split_px(Split::Bench, viewport),
                picture_bottom(self),
            ),
        };
        let list_h = menu_rows_h(rows.len(), room);
        let h = MENU_PAD * 2. + list_h;
        // Settings: the list hangs from the foot of the row that was clicked
        // and flips above that row, never over it, when the room is short.
        let at = match self.settings_open || !picker.floored {
            true => settings_anchor(at, viewport, h),
            false => at,
        };
        let placed = match picker.above {
            Some(top) if f32::from(at.y) + h + crate::layout::MENU_EDGE > f32::from(viewport.height) => {
                point(at.x, px((top - h).max(0.)))
            }
            Some(_) => at,
            None => menu_above(at, viewport, floor, h),
        };
        let (x, y) = match from_palette {
            // Opened by name from the command palette (no control to hang from):
            // the list takes the palette's own place, where the eye already is.
            true => (((f32::from(viewport.width) - pw) / 2.).max(MENU_EDGE), (f32::from(viewport.height) / 6.).max(MENU_EDGE)),
            false => crate::oracle::menu_at_w(placed, viewport, pw, h),
        };
        // Deferred: a list opened from a dialog (Settings' Theme, Frame rate,
        // Subtitle font) paints over that dialog and takes its clicks. Settings
        // is painted after this layer in the root, so without the deferral the
        // dialog's own press handler closed Settings and the list stayed behind.
        Some(gpui::deferred(
            scrim()
                // Click away closes it, either button, swallowed so nothing
                // under the list also takes the press -- the clip menu's rule.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.picker = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.picker = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    crate::ui::popup::popup_plate("picker-card", x, y, pw)
                        // Painted after the scrim, so this listener runs first
                        // and a press meant for a row never closes the list out
                        // from under its own click (`context_card`).
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .child(
                            div()
                                .id("picker-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        ))
    }
}

/// The command palette's plate width (DESIGN §4.4): wide enough for a label,
/// its category and its key on one 32px row.
const PALETTE_W: f32 = 560.;
/// One palette row, the click target the palette is built from.
const PALETTE_ROW_H: f32 = 36.;
/// A category heading in the grouped (empty-query) list.
const PALETTE_HEAD_H: f32 = 24.;
/// How many rows show before the list scrolls.
/// How long the pointer rests on the Adjust row before its submenu opens.
const ADJUST_HOVER: Duration = Duration::from_millis(150);

/// Whether `p` lies inside the triangle `a`, `b`, `c` -- the corridor from
/// where the pointer left a menu row to the two near corners of its submenu.
pub(crate) fn in_aim_triangle(p: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>, c: Point<Pixels>) -> bool {
    let f = |p: Point<Pixels>| (f32::from(p.x), f32::from(p.y));
    let (p, a, b, c) = (f(p), f(a), f(b), f(c));
    let side = |u: (f32, f32), v: (f32, f32)| (p.0 - v.0) * (u.1 - v.1) - (u.0 - v.0) * (p.1 - v.1);
    let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
    let neg = d1 < 0. || d2 < 0. || d3 < 0.;
    let pos = d1 > 0. || d2 > 0. || d3 > 0.;
    !(neg && pos)
}

const PALETTE_ROWS_SHOWN: f32 = 10.;

impl Player {
    /// The command palette (DESIGN §4.4, Ctrl+K): a centred plate near the top
    /// of the window, over everything, with a field that has the keyboard and a
    /// list of every action under it. Click away or escape closes it; the keys
    /// are the root handler's (`ui::stance`).
    pub(crate) fn command_palette(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.palette_open {
            return None;
        }
        let rows = self.palette_list();
        let grouped = self.palette_query.trim().is_empty();
        let sel = self.palette_sel;
        let mut list: Vec<AnyElement> = Vec::new();
        let mut last_category = None;
        for (i, row) in rows.iter().enumerate() {
            let category = row.action.category();
            if grouped && last_category != Some(category) {
                list.push(
                    div()
                        .flex_none()
                        .h(px(PALETTE_HEAD_H))
                        .px(px(10.))
                        .flex()
                        .items_end()
                        .pb(px(2.))
                        .type_style(type_scale::section())
                        .text_color(rgb(crate::ui::theme::tokens().ink2))
                        .child(category.label())
                        .into_any_element(),
                );
                last_category = Some(category);
            }
            list.push(self.palette_row(i, *row, i == sel, grouped, cx));
        }
        if rows.is_empty() {
            list.push(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(10.))
                    .py(px(8.))
                    .child(
                        div()
                            .type_style(type_scale::label(type_scale::LABEL_ROW_PX, gpui::FontWeight::MEDIUM))
                            .text_color(rgb(INK2()))
                            .child("No command matches"),
                    )
                    .child(
                        div()
                            .type_style(type_scale::caption())
                            .text_color(rgb(INK3()))
                            .child("Try a shorter word, or clear the search"),
                    )
                    .into_any_element(),
            );
        }
        let query = self.palette_query.clone();
        let field = div()
            .flex_none()
            .h(px(44.))
            .border_b_1()
            .border_color(rgba(crate::ui::theme::tokens().line))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .child(crate::ui::icons::Icon::Search.el(crate::ui::icons::IconSize::Inline, crate::ui::theme::tokens().ink3))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .type_style(type_scale::emph())
                    .text_color(rgb(if query.is_empty() { crate::ui::theme::tokens().ink3 } else { crate::ui::theme::tokens().ink1 }))
                    .child(match query.is_empty() {
                        true => "Search commands".to_string(),
                        // The same caret every text field draws (a thin block after the text).
                        false => format!("{query}\u{258f}"),
                    }),
            );
        let list_h = PALETTE_ROWS_SHOWN * PALETTE_ROW_H;
        let plate_x = ((f32::from(viewport.width) - PALETTE_W) / 2.).max(MENU_EDGE);
        let plate_y = (f32::from(viewport.height) / 6.).max(MENU_EDGE);
        Some(
            scrim()
                // Click away closes it, either button, and the press goes no
                // further -- the picker's rule.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.palette_open = false;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.palette_open = false;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    crate::ui::kit::plate(8.)
                        .id("command-palette")
                        .absolute()
                        .left(px(plate_x))
                        .top(px(plate_y))
                        .w(px(PALETTE_W))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .p(px(4.))
                        // Painted after the scrim, so a press on the plate is
                        // its own and never closes the palette under the click.
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .child(field)
                        .child(
                            div()
                                .id("palette-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .track_scroll(&self.palette_scroll)
                                .children(list),
                        ),
                ),
        )
    }

    /// One palette row: the label in ink1 (ink4 when refused), its category in
    /// ink3, and on the right its key in mono -- or, for a refused row, the
    /// reason it cannot run (DESIGN §4.8: the reason stays readable in ink3).
    fn palette_row(
        &self,
        i: usize,
        row: crate::menus::PaletteRow,
        picked: bool,
        grouped: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let action = row.action;
        let refused = row.refused.is_some();
        // The shortcut column reads as the menus' does: the strokes, nothing
        // for an action with none. A refused row is dimmed and says why on
        // hover, never as text in the row.
        let right = crate::ui::popup::shortcut(&self.keymap, action);
        let why = row.refused;
        div()
            .id(("palette-row", i))
            .flex_none()
            .h(px(PALETTE_ROW_H))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .px(px(10.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(picked, |d| d.bg(rgba(crate::ui::theme::tokens().accent_wash)))
            .when(!picked, |d| d.hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER))))
            .when_some(why, |d, why| {
                d.tooltip(crate::ui::widgets::overlay_tip_hover(action.label(), "", Some(why)))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.palette_sel = i;
                this.palette_run(window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w(px(0.))
                    .child(
                        div().flex_none().w(px(16.)).h(px(16.)).children(
                            crate::menus::icon(action).map(|i| {
                                i.el(
                                    crate::ui::icons::IconSize::Inline,
                                    if refused { crate::ui::theme::tokens().ink4 } else { crate::ui::theme::tokens().ink2 },
                                )
                            }),
                        ),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .type_style(type_scale::body_strong())
                            .text_color(rgb(if refused { crate::ui::theme::tokens().ink4 } else { crate::ui::theme::tokens().ink1 }))
                            .child(action.label()),
                    )
                    // Under a category heading the tag would only repeat it.
                    .when(!grouped, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .type_style(type_scale::caption())
                                .text_color(rgb(crate::ui::theme::tokens().ink3))
                                .child(action.category().label()),
                        )
                    }),
            )
            .child(match right.is_empty() {
                true => div().flex_none(),
                false => crate::ui::kit::keycaps(&right),
            })
            .into_any_element()
    }
}

/// Where a list opened from a Settings row hangs: just under that row (a click
/// anywhere on it lands within half a row of its middle), or above the row
/// when the room under it does not hold the whole list.
pub(crate) fn settings_anchor(click: Point<Pixels>, viewport: Size<Pixels>, h: f32) -> Point<Pixels> {
    let below = f32::from(click.y) + crate::layout::CONTROL_H / 2.;
    match below + h + crate::layout::MENU_EDGE <= f32::from(viewport.height) {
        true => point(click.x, px(below)),
        // The list's own top, so it ends where the row begins.
        false => point(click.x, px((f32::from(click.y) - crate::layout::CONTROL_H / 2. - h).max(0.))),
    }
}

#[cfg(test)]
mod aim_tests {
    use super::*;

    #[test]
    fn a_diagonal_move_toward_the_submenu_stays_inside_the_safe_triangle() {
        // The pointer left the Adjust row at (100, 200); the submenu's near
        // edge is x = 300, from y 180 to 420.
        let (a, b, c) = (
            point(px(100.), px(200.)),
            point(px(300.), px(180.)),
            point(px(300.), px(420.)),
        );
        assert!(in_aim_triangle(point(px(200.), px(260.)), a, b, c));
        assert!(in_aim_triangle(point(px(290.), px(400.)), a, b, c));
        // Straight down the plate, or back up, leaves it.
        assert!(!in_aim_triangle(point(px(100.), px(300.)), a, b, c));
        assert!(!in_aim_triangle(point(px(150.), px(120.)), a, b, c));
        // Same answer whichever way the corners are wound.
        assert!(in_aim_triangle(point(px(200.), px(260.)), a, c, b));
    }

    #[test]
    fn the_adjust_submenu_opens_after_about_150_ms() {
        assert_eq!(ADJUST_HOVER, Duration::from_millis(150));
    }
}

#[cfg(test)]
mod settings_anchor_tests {
    use super::*;

    #[test]
    fn a_settings_list_hangs_under_its_row_or_above_it_and_stays_in_the_window() {
        let vp = size(px(1280.), px(720.));
        let under = settings_anchor(point(px(600.), px(192.)), vp, 120.);
        assert_eq!(f32::from(under.y), 192. + crate::layout::CONTROL_H / 2.);
        let flipped = settings_anchor(point(px(600.), px(500.)), vp, 300.);
        let (_, y) = crate::oracle::menu_at_w(crate::ui::stance::menu_above(flipped, vp, 0., 300.), vp, 200., 300.);
        assert!(y + 300. <= 500. - crate::layout::CONTROL_H / 2., "the flipped list ends above the row, got y={y}");
    }
}

impl Player {
    /// Why a gap on `lane` cannot be closed right now, in words; `None` when it can. The
    /// Close gap row and button show this as their tooltip and ignore the click while it holds.
    pub(crate) fn close_gap_refusal(&self, lane: Lane) -> Option<&'static str> {
        if self.exporting().is_some() {
            return Some("Wait for the export to finish");
        }
        if self.session.as_ref().is_some_and(|s| s.lane_locked(lane)) {
            return Some("This track is locked: unlock it to close its gaps");
        }
        None
    }
}
