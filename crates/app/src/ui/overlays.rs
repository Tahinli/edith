//! The bars and the floating cards that are not inspector sections.

use crate::ui::stance::menu_floor;
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

    /// The line a long-standing seek shows, in the import bar's place and by the
    /// import bar's rules: nothing dismisses it, and it leaves by itself the
    /// moment the frame lands. No sweep under it -- an open reports nothing about
    /// where it has got to, and the clock is the honest half of that bar anyway.
    pub(crate) fn seek_bar(&self) -> Option<impl IntoElement> {
        let line = seek_line(self.seek_since.map(|t| t.elapsed()))?;
        Some(
            div()
                .flex_none()
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(BG_RAISED()))
                .child(line),
        )
    }

    /// A notice holds its own bar, full width, until it is answered: any key
    /// retires it (the key handler) and so does a click on it. Its own surface
    /// because the message is the point -- a failure cut to the timecode's slot
    /// is a failure nobody read.
    ///
    /// The export's own line is more than a message: it names a file that is
    /// now on disk, so the same click that retires it shows that file in the
    /// desktop's file manager.
    pub(crate) fn notice_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notice = self.notices.front().cloned()?;
        // The path travels with the text it was written for: a later notice
        // holds the bar and the click is a plain dismissal again.
        let exported = self
            .exported
            .clone()
            .filter(|_| notice.starts_with(EXPORT_DONE));
        // How many are waiting behind this one, said out loud: a queue nobody is
        // told about is a queue whose second message reads as the first one
        // failing to go away.
        let behind = self.notices.len() - 1;
        let hint = match (exported.is_some(), behind) {
            (true, _) => "click — open file location".to_string(),
            (false, 0) => "click or press any key to dismiss".to_string(),
            (false, 1) => "click or press any key — 1 more message behind this".to_string(),
            (false, n) => format!("click or press any key — {n} more messages behind this"),
        };
        // What the message *is*, by colour, before it is read: the words the
        // editor shouts a failure in are the ones it opens with, so the bar can
        // colour itself off the same text every one of them already carries.
        let tone = notice_tone(&notice);
        Some(
            div()
                .id("notice")
                .when(exported.is_some(), |d| {
                    d.tooltip(|_, cx| {
                        cx.new(|_| {
                            Tip("Open file location — shows the export in the file manager".into())
                        })
                        .into()
                    })
                })
                .flex_none()
                .flex()
                // The hint keeps its own width, and at the 640x360 floor the
                // picture region is narrower than the hint alone: with both on
                // one line the message was squeezed to nothing and wrapped a
                // character per line -- a notice rendered as a column of
                // letters. Wrapping puts the hint on its own line instead, so
                // the message keeps the whole width and the queue counter
                // stays on screen under it.
                .flex_wrap()
                .items_start()
                .gap(px(12.))
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(BG_RAISED()))
                // The tone is a stripe down the leading edge rather than the
                // whole bar's colour: a full red bar at every refusal is an
                // alarm, and most of these are answers.
                .border_l(px(3.))
                .border_color(rgb(tone))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    // Another process starting: off the UI thread, and it
                    // outlives the notice it was asked from.
                    if let Some(path) = exported.clone() {
                        cx.background_executor()
                            .spawn(async move { show_in_file_manager(&path) })
                            .detach();
                    }
                    this.dismiss_notice();
                    cx.notify();
                }))
                // Shrinkable, because a gpui text element measures its
                // min-content as the *whole line* (`TextLayout::layout` only
                // wraps against a definite width) -- without a `min_w` the
                // message would refuse to shrink at all and run off the region
                // instead. With a floor under it, it shrinks to that floor,
                // which pushes the hint onto its own line and leaves the
                // message the width it needs to wrap like a sentence.
                .child(div().flex_1().min_w(px(NOTICE_MIN_W)).child(notice))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(rgb(FG_SECONDARY()))
                        .child(hint),
                ),
        )
    }

    /// Every binding, and the way to change one: a row per entry, click it and
    /// the next stroke becomes its chord. Over the whole window, because while
    /// it is up nothing under it may answer -- the scrim stops the clicks
    /// (gpui dispatches mouse listeners topmost-first, window.rs:3705) and the
    /// key handler's own branch stops the strokes.
    ///
    /// Plain divs, like every control here: nothing in it takes focus, so the
    /// root is still the one reading the keyboard -- which is exactly what a
    /// waiting row depends on.
    pub(crate) fn keys_overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.keys_open {
            return None;
        }
        // The way out, spelled the way the rows spell a chord.
        let out = keymap::Chord {
            key: ESCAPE.to_string(),
            ctrl: false,
        }
        .pretty();
        // Every action there is, under its heading, and the strokes the modal
        // cards answer to beside them -- `keys_rows` is the whole of the order
        // and every word in it comes off the registry.
        //
        // A row is two targets, not one: its label *does* the action, which is
        // the pointer's way to the ones no button carries, and its stroke
        // changes that stroke. An action with two strokes reads as one line
        // ("x or delete") and a rebind replaces that whole set.
        let mut rows: Vec<AnyElement> = Vec::new();
        let row = || {
            div()
                .flex()
                // The floor, not the height: a row that needed two lines
                // would otherwise paint over the one under it -- which it did
                // anyway until this said `flex_none`, a row inside a capped
                // scrolling list being shrunk to fit by default (the export
                // card's rows had the same shape and the same overlap).
                .flex_none()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        let found = keys_filter(&self.keys_search, &self.keymap);
        // What the search box says: the typed text, and how much of the list is
        // left under it -- a filter that found nothing has to say so, or the
        // card reads as a list that lost its rows.
        let search = if self.keys_search.is_empty() {
            "search: type to filter · ↑ ↓ scroll the list · a click away closes".to_string()
        } else {
            let rows = found
                .iter()
                .filter(|(_, r)| !matches!(r, KeyRow::Head(_)))
                .count();
            match rows {
                0 => format!(
                    "search: {} — nothing matches · {out} clears",
                    self.keys_search
                ),
                n => format!("search: {} — {n} shown · {out} clears", self.keys_search),
            }
        };
        for (i, key_row) in found {
            rows.push(match key_row {
                KeyRow::Head(category) => div()
                    .flex_none()
                    .px(px(6.))
                    .pt(px(4.))
                    .text_size(px(11.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child(category.label())
                    .into_any_element(),
                KeyRow::Act(action) => {
                    let capturing = self.rebinding == Some(action);
                    // Why the label half will not answer, if it will not: the
                    // registry's one answer, the same the clip menu dims by.
                    let refusal = self.enable(action, None);
                    let out = out.clone();
                    row()
                        .when(capturing, |d| d.bg(rgb(BG_SELECTED())))
                        .child(
                            div()
                                .id(("do", i))
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .min_h(px(KEYS_ROW_H))
                                .items_center()
                                // One line, cut where the stroke's column
                                // starts: a label longer than the room it has
                                // printed straight over the stroke beside it,
                                // and two overprinted words are less readable
                                // than one truncated one.
                                .truncate()
                                .child(action.label())
                                // The reason rides on the label rather than in
                                // the stroke column, which the rebind half
                                // needs whatever the editor's state is: an
                                // action nobody can ask for right now is still
                                // one whose key may be changed.
                                .when_some(refusal.why(), |d, why| {
                                    let why: SharedString = why.into();
                                    d.tooltip(move |_, cx| cx.new(|_| Tip(why.clone())).into())
                                })
                                .when(!refusal.yes(), |d| d.opacity(0.4).cursor_not_allowed())
                                .when(refusal.yes(), |d| {
                                    d.cursor_pointer()
                                        .hover(|s| s.bg(rgb(BG_HOVER())))
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                // The card goes first: several of
                                                // these open a card of their own,
                                                // and every edit moves the indices
                                                // the menus are holding.
                                                this.keys_open = false;
                                                this.rebinding = None;
                                                this.act(action, window, cx);
                                                cx.notify();
                                            },
                                        ))
                                }),
                        )
                        .child(
                            // corner-cut: the column is as wide as the stroke it
                            // prints, so a one-character chord gives this half a
                            // hit area under the 24px WCAG 2.5.8 floor -- tall
                            // enough, narrow. Upgrade: a min_w of HIT_MIN here.
                            div()
                                .id(("bind", i))
                                .flex_none()
                                .flex()
                                .min_h(px(KEYS_ROW_H))
                                .items_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(BG_HOVER())))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.rebinding = Some(action);
                                    cx.notify();
                                }))
                                .child(if capturing {
                                    div()
                                        .text_color(rgb(FG_SECONDARY()))
                                        .child(format!("press a key — {out} cancels"))
                                } else {
                                    div().child(self.keymap.display(action))
                                }),
                        )
                        .into_any_element()
                }
                KeyRow::Fixed(f) => {
                    let f = &keymap::FIXED[f];
                    row()
                        .child(f.label)
                        // Dim, and no hover: this one is not a row you can click.
                        .child(div().text_color(rgb(FG_SECONDARY())).child(f.chord.clone()))
                        .into_any_element()
                }
            });
        }
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                // The picture behind is out of reach, and looks it. The press is
                // swallowed here so a button under the scrim cannot take it --
                // and it closes the card on the way, which is the pointer's exit
                // from every card here ([`Player::close_card`]).
                .bg(rgba(SCRIM()))
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
                        .w(px(KEYS_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(BG_RAISED()))
                        // Title and instruction are two children, never one
                        // wrapping line: a fixed-height slot whose text wrapped
                        // painted its second line over the first row.
                        .child(div().flex_none().px(px(6.)).child("Actions & keys"))
                        // One status line, under the title where the eye starts.
                        // A refusal takes it over -- it is the more urgent of the
                        // two, and the notice bar it would otherwise appear in is
                        // under the scrim.
                        //
                        // The row list below is capped and scrolls, so neither a
                        // wrapped refusal here nor another action added to `ALL`
                        // can push the card past a 360 px window any more.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(self.notices.front().cloned().unwrap_or_else(|| {
                                    "click an action to do it · click its key to change it".into()
                                })),
                        )
                        // The search box: no focus and no text field -- the
                        // card's key handler is the field, exactly as the
                        // export card's bitrate is typed into it. Its own line
                        // above the list, so the rows below it are always the
                        // answer to what it says.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(FG_SECONDARY()))
                                .child(search),
                        )
                        // Capped and scrolling rather than as tall as the action
                        // list happens to be: the list grows with the editor,
                        // the smallest window does not.
                        .child(
                            div()
                                .id("keys-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .max_h(px(KEYS_ROWS_H))
                                // The line the list scrolls under: a row half
                                // out of the viewport sat against the search
                                // line with nothing between them, and read as a
                                // row painted over the heading rather than as a
                                // list with more above it.
                                .mt(px(4.))
                                .border_t_1()
                                .border_color(rgb(BG_PANEL()))
                                .pt(px(4.))
                                .overflow_y_scroll()
                                // The wheel's offset and the arrow keys' are
                                // the same one, so the two cannot disagree
                                // about where the list is.
                                .track_scroll(&self.keys_scroll)
                                .children(rows),
                        ),
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
            MenuOn::Gap(..) => None,
        };
        let clip = idx.and_then(|idx| session.lane_clips(menu.lane).get(idx).copied());
        let source = clip.and_then(|clip| session.sources().get(clip.source).cloned());
        let secs = |frames: u32| timecode(f64::from(frames) / self.fps, self.fps);
        // DESIGN §9/§3: a menu row is a plate row, Archivo at the room's
        // 10.5px row size -- `ink2`, dimmed to `ink3` on the disabled ones
        // below by the same `.opacity` every row already carried. Gated on
        // `self.darkroom` so `OLD_GUI=1` keeps its own unstyled rows exactly
        // as they draw today.
        let darkroom = self.darkroom;
        let row = move |n: usize| {
            div()
                .id(("menu", n))
                .flex()
                // The floor, not the height: a long label wrapping must not
                // paint over the item under it.
                .min_h(px(MENU_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
                .when(darkroom, |d| {
                    d.type_style(type_scale::label(
                        type_scale::LABEL_ROW_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(INK2()))
                })
        };
        // The chord/metadata half of a row: mono, `ink3`, DESIGN §3's
        // 9.5-10px band -- every value that is data about the film rather
        // than a room verb goes through this rather than the row's ambient
        // Archivo.
        let chord_style = |d: Div| -> Div {
            d.when(darkroom, |d| {
                d.type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
            })
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        if let (true, Some((clip, source))) = (menu.details, clip.zip(source.clone())) {
            // Read-only, so no ids and no hover: this side is a card, not a
            // list of things to click. Each value is one truncated line, which
            // is what keeps the height below the one the clamp was given.
            for (label, value) in [
                ("File", file_name(&source.path)),
                ("Path", source.path.display().to_string()),
                (
                    "Source range",
                    format!("{} – {}", secs(clip.in_frame), secs(clip.out_frame)),
                ),
                // How long it is *where it sits*, which its rate decides -- and
                // the rate itself beside it, because a clip half its source's
                // length could be either a trim or a 2x.
                ("This clip", secs(clip.frames())),
                ("Speed", format!("{} (tape)", clip.speed)),
                ("Source duration", secs(session.file_frames(&source.path))),
                (
                    "Bitrate",
                    bitrate_detail(
                        self.bitrates.get(&source.path).copied().flatten(),
                        self.streams.get(&source.path).map_or(0, Vec::len),
                    ),
                ),
            ] {
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(
                            chord_style(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    // A size smaller than the labels: a timecode
                                    // pair is 25 characters and has to fit beside
                                    // its label inside `MENU_W`.
                                    .text_size(px(11.))
                                    .text_color(rgb(FG_SECONDARY())),
                            )
                            .child(value),
                        )
                        .into_any_element(),
                );
            }
        } else if let MenuOn::Gap(start, frames) = menu.on {
            // Gap rows are ripples, not clip actions. The first is the exact
            // hole under the pointer; the second appears only when the same
            // track has another bounded hole too, and says the track in the row
            // so "all" cannot read as the whole timeline.
            let lane = menu.lane;
            rows.push(
                row(rows.len())
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.context_menu = None;
                        this.close_gap(lane, start, frames, cx);
                    }))
                    .child("Close gap")
                    .child(chord_style(div().text_color(rgb(FG_SECONDARY()))).child("ripples left"))
                    .into_any_element(),
            );
            if session.gap_count(lane) > 1 {
                rows.push(
                    row(rows.len())
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(BG_HOVER())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.context_menu = None;
                            this.close_all_gaps_on_lane(lane, cx);
                        }))
                        .child(format!("Close all {} gaps", lane.label()))
                        .child(
                            chord_style(div().text_color(rgb(FG_SECONDARY()))).child("this track"),
                        )
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
            let idx = idx.expect("MenuOn::Gap handled above");
            let ctx = self.ctx(Some((menu.lane, idx)));
            for action in menu_items(ctx) {
                // The registry's own answer, the same one the actions card
                // dims a row with -- and a row that takes no click says *why*
                // rather than printing a stroke that would do nothing.
                let refusal = enable(action, ctx);
                let enabled = refusal.yes();
                // The one item that is not about this clip says so, and says it
                // here rather than in the registry: the stroke is global too,
                // but its row in the keys menu is not sitting on a clip.
                // The registry's label is a sentence -- it has to be, the keys
                // overlay reads the same string -- and a sentence in a 260px
                // plate row is cut mid-word by the chord column beside it
                // ("Ungroup the selection (clips anc", "Group the selection
                // (ctrl-c"). DESIGN §7's rule for the bench applies to a menu
                // too: labels never truncate into soup. The verb is the head
                // of the sentence, up to its first parenthesis or em-dash;
                // the tail is what the row's tooltip carries, so nothing is
                // lost, it just stops being drawn over the chord.
                let full = action.label();
                let verb = full
                    .split_once(" (")
                    .map(|(head, _)| head)
                    .or_else(|| full.split_once(" — ").map(|(head, _)| head))
                    .or_else(|| full.split_once(": ").map(|(head, _)| head))
                    .unwrap_or(full);
                let label = if matches!(action, ActionId::ToggleMute | ActionId::Paste) {
                    format!("{verb} (global)")
                } else {
                    verb.to_string()
                };
                let say: SharedString = full.to_string().into();
                rows.push(
                    row(rows.len())
                        // Truncated and shrinkable: a long label ("Transform —
                        // position, scale, rotation, crop") used to run the
                        // hint column clean off the fixed-width menu instead
                        // of sharing the row with it, which is why Transform
                        // showed no stroke while a short label like Colour's
                        // did.
                        .tooltip(move |_, cx| cx.new(|_| Tip(say.clone())).into())
                        .child(div().min_w(px(0.)).flex_shrink().truncate().child(label))
                        .child(match refusal.why() {
                            // One truncated line, like the details side: a
                            // reason that wrapped would make the card taller
                            // than the height `menu_at` placed it by.
                            Some(why) => chord_style(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(rgb(FG_SECONDARY())),
                            )
                            .child(why),
                            // Every command wears its chord (DESIGN §4): mono,
                            // `ink3`, the same face the spine badges and the
                            // keys overlay already read theirs in.
                            None => {
                                chord_style(div().flex_shrink_0().text_color(rgb(FG_SECONDARY())))
                                    .child(self.keymap.display(action))
                            }
                        })
                        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(BG_HOVER())))
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
                                        if action == ActionId::Fit {
                                            this.open_picker(
                                                Pick::Fit(menu.lane, idx),
                                                event.position(),
                                                cx,
                                            );
                                        } else {
                                            this.act(action, window, cx);
                                        }
                                    },
                                ))
                        })
                        .into_any_element(),
                );
            }
            // What a caption has no other side to turn over to: its file, its
            // source range and its bitrate are a clip's, and a row that opened
            // an empty card would be the stub this menu does not draw.
            rows.extend(clip.map(|_| {
                row(rows.len())
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(menu) = &mut this.context_menu {
                            menu.details = true;
                        }
                        cx.notify();
                    }))
                    .child("Properties")
                    // No stroke reaches this one, and a blank column would read
                    // as one that was forgotten.
                    .child(chord_style(div().text_color(rgb(FG_SECONDARY()))).child("…"))
                    .into_any_element()
            }));
        }
        // The height the card is *placed* by and the height its list is drawn
        // to are one number: placed by a taller one, the card would hang off the
        // window's floor -- the very thing the clamp is for.
        // §5/§11 check 6: the picture is never covered. A right-click high on
        // the window would otherwise hang the menu at the pointer, straight
        // over the screen -- clamped below it into the bench/ledger/dock
        // footprint instead, darkroom only (the legacy tree has no such
        // fixed split to clamp against). The list is sized against that same
        // footprint (`room`), not the whole window, so a menu taller than
        // the footprint scrolls inside its own plate instead of walking its
        // clamped top edge back up over the picture ([`menu_floor`]).
        let (at, room) = menu_floor(
            menu.at,
            viewport,
            self.darkroom,
            self.split_px(Split::Bench, viewport),
        );
        let list_h = menu_rows_h(rows.len(), room);
        let (x, y) = menu_at(at, viewport, MENU_PAD * 2. + list_h);
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
                    menu_plate("menu-card", x, y, darkroom)
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
                ),
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
        let darkroom = self.darkroom;
        let rows: Vec<AnyElement> = self
            .choices(picker.of)
            .into_iter()
            .enumerate()
            .map(|(n, (choice, label, detail, picked))| {
                div()
                    .id(("picker-row", n))
                    .flex()
                    // The floor, not the height, and `HIT_MIN` of it: a row of a
                    // list is a click target like every other one here (WCAG
                    // 2.5.8).
                    .min_h(px(MENU_ROW_H))
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .when(darkroom, |d| {
                        d.type_style(type_scale::label(
                            type_scale::LABEL_ROW_PX,
                            gpui::FontWeight::MEDIUM,
                        ))
                        .text_color(rgb(INK2()))
                    })
                    .cursor_pointer()
                    // DEFECT (user 2026-08-21: "some menus are belongs to old
                    // ui ... for example resolution picker"): the list painted
                    // the legacy sheet's greys (`BG_RAISED`/`BG_HOVER`/
                    // `BG_SELECTED`, `FG_*`) in the darkroom too, so a room
                    // whose every other plate is canvas-on-panel opened one
                    // pale rounded card. The darkroom pass below is the same
                    // plate grammar `context_card`/`library_card` already
                    // draw: one `raised` fill step for hover, `ink1` for the
                    // row in force, no second grey.
                    .hover(|s| s.bg(rgb(if darkroom { DARK_RAISED() } else { BG_HOVER() })))
                    // The mark is a glyph as well as a highlight, like the
                    // export card's rows: a background alone is gone under a
                    // hover and invisible to anyone who cannot tell the two
                    // greys apart (WCAG 1.4.1).
                    .when(picked, |d| {
                        d.bg(rgb(if darkroom {
                            DARK_RAISED()
                        } else {
                            BG_SELECTED()
                        }))
                        .when(darkroom, |d| d.text_color(rgb(INK1())))
                    })
                    // Where the keyboard is, said after the mark so the cursor
                    // is visible on the picked row too -- and as a border in
                    // the darkroom (DESIGN §4's 1px `ink1` focus ring) rather
                    // than a second grey nobody can tell from the first.
                    .when(n == picker.sel, |d| match darkroom {
                        true => d.border_1().border_color(rgb(INK1())),
                        false => d.bg(rgb(BG_HOVER())),
                    })
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
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            // On the picked row the dim ink sits on the
                            // highlight, where it is only 3.3:1 (WCAG 1.4.3).
                            .text_color(rgb(match (darkroom, picked) {
                                (true, true) => INK2(),
                                (true, false) => INK3(),
                                (false, true) => FG_PRIMARY(),
                                (false, false) => FG_SECONDARY(),
                            }))
                            .when(darkroom, |d| {
                                d.type_style(type_scale::mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    gpui::FontWeight::MEDIUM,
                                ))
                            })
                            .child(detail),
                    )
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(choice, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        // The window's own room, and the list scrolls only where the window has
        // none -- the clip menu's rule, one function for both.
        let (at, room) = menu_floor(
            picker.at,
            viewport,
            darkroom,
            self.split_px(Split::Bench, viewport),
        );
        let list_h = menu_rows_h(rows.len(), room);
        let (x, y) = menu_at(at, viewport, MENU_PAD * 2. + list_h);
        Some(
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
                    menu_plate("picker-card", x, y, darkroom)
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
        )
    }
}
