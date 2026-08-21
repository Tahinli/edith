//! The time band's own content (MOCK-SPEC.md "Time band", DESIGN.md §5):
//! hero timecode, ghost transport, cut readout, the contact strip (whole-film
//! minimap) and the boxed Export chip. `stance.rs::time_band()`'s frame draws
//! the strip; this module owns what fills it, the same split
//! `dock_stance.rs`/`bench_stance.rs` already make for their regions.
//!
//! Type note: DESIGN.md §3's approved scale gives the hero timecode 18px,
//! close to the mock's ~20px reading. Every size in this module comes from
//! `ui::type_scale` (role, not a bare `px()` literal).

use crate::*;
use crate::ui::type_scale::{self, label, mono};
use gpui::FontWeight;

thread_local! {
    // corner-cut: every other drag surface in this codebase (the bench
    // scrollbar's `scroll_drag`, the volume slider's `volume_dragging`) keeps
    // its anchor and its measured bounds as `Player` fields (`main.rs`). That
    // file belongs to a concurrent builder this session, so the contact
    // strip's own transient gesture state lives here instead, in the same
    // `Rc<Cell<Bounds<Pixels>>>` shape `bounds_probe` already takes. Ceiling:
    // fold both into `Player` once `main.rs` is free again.
    static STRIP_BOUNDS: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    static PAN_ANCHOR: Cell<Option<f32>> = Cell::new(None);
}

/// A stacked ghost command (DESIGN §4, MOCK-SPEC "Ghost transport"/"spine"):
/// glyph 18px `ink2` over its chord 13px `ink3`, read live off the keymap
/// so a rebind can never leave the band showing a stroke that no longer
/// fires it -- the same shape `stance::ghost` draws for the spine, kept local
/// here so this module owns its own region end to end.
///
/// FAULT 2 fix: this used to read [`Keymap::display`], the full-sentence
/// form (`ctrl+left`) meant for the keys overlay's own list -- MOCK-SPEC's
/// band wants the compact badge (`J`, `spc`, `L`) the spine already reads
/// via [`Keymap::chord`] (`stance::ghost`). Switched to `chord` here so every
/// badge in the band matches that same compact grammar.
///
/// Also FAULT 2: this glyph drew no `on_click` at all -- a badge that names
/// a stroke but does not fire it on a click is the same "glyph, badge and
/// what fires must agree" defect the chord text was, so it now dispatches
/// `action` through [`Player::act`] the same way `stance::ghost` (spine) and
/// `spine_stance`'s own ghost already do, `id` making the three transport
/// ghosts distinct elements gpui can track.
/// `active` (added alongside the volume/loop cluster this task adds) tints
/// the glyph `ink1` instead of `ink2` -- the same on/off convention
/// `spine_stance::glyph` already uses for loop-trim -- so a toggle sitting
/// in this band (`Loop`, `ToggleMute`) can show its own state without a
/// second widget shape.
fn ghost(
    id: &'static str,
    player: &Player,
    glyph: impl Into<SharedString>,
    action: ActionId,
    active: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let glyph_style = label(type_scale::HERO_TIMECODE_PX, FontWeight::MEDIUM);
    // The chord names a key, so DESIGN §3's mono rule ("if a string is about
    // ... a key, it is mono") applies here same as everywhere else in the band.
    let chord_style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(1.))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.act(action, window, cx);
        }))
        .font(glyph_style.font)
        .text_size(glyph_style.size)
        .text_color(rgb(if active { INK1() } else { INK2() }))
        .child(glyph.into())
        .child(
            div()
                .font(chord_style.font)
                .text_size(chord_style.size)
                .text_color(rgb(INK3()))
                .child(player.keymap.chord(action)),
        )
}

/// The hero timecode, digits `ink1` and colons `ink3` (DESIGN §3, MOCK-SPEC
/// "Hero timecode"): the one place in the band the string is split apart
/// rather than painted whole, so the colons can read a shade quieter than
/// the numbers either side of them.
fn hero_timecode(tc: &str) -> impl IntoElement {
    let style = mono(type_scale::HERO_TIMECODE_PX, FontWeight::BOLD);
    div()
        .flex_none()
        .flex()
        .font(style.font)
        .text_size(style.size)
        .children(tc.chars().map(|c| {
            let ink = if c == ':' { INK3() } else { INK1() };
            div().text_color(rgb(ink)).child(c.to_string())
        }))
}

/// A signed `±M:SS` delta, the shape MOCK-SPEC's cut readout writes trim
/// deltas in (`−02:10`, `+01:04`) -- the sign says which way the edge moved
/// since roll armed, [`clock`] says how far.
fn signed_delta(frames: i64, fps: f64) -> String {
    let secs = (frames.unsigned_abs() as f32) / (fps as f32).max(1.);
    format!("{}{}", if frames < 0 { "−" } else { "+" }, clock(secs))
}

/// The cut readout (MOCK-SPEC "Cut readout"): the odometer (`cut 14/37`),
/// the subject cut's own trim deltas against where roll armed it (only real
/// once [`Player::loop_trim`] has a baseline to measure against), and the
/// roll word itself, on or dim. `·` separators in `ink3`, values in `ink2`.
fn cut_readout(player: &Player) -> impl IntoElement {
    let sep = || div().text_color(rgb(INK3())).child(" · ");
    let val = |s: String| div().text_color(rgb(INK2())).child(s);
    let anchor = player.selected.anchor();
    let odometer = anchor
        .and_then(|(lane, idx)| {
            player
                .session
                .as_ref()
                .map(|s| format!("cut {}/{}", idx + 1, s.lane_clips(lane).len()))
        })
        .unwrap_or_else(|| "cut —/—".to_string());
    // The baseline a roll session captured at arm time (`toggle_loop_trim`)
    // doubles as the trim deltas' zero point: the same span it loops is the
    // span its own edges are measured from.
    let deltas = anchor.and_then(|(lane, idx)| {
        let (lo, hi) = player.loop_trim?;
        let clip = player.session.as_ref()?.lane_clips(lane).get(idx)?;
        let fps = player.active_fps();
        Some((
            signed_delta(i64::from(clip.start) - i64::from(lo), fps),
            signed_delta(i64::from(clip.end()) - i64::from(hi), fps),
        ))
    });
    let roll_on = player.loop_trim.is_some();
    let style = mono(type_scale::CHORD_METADATA_MAX_PX, FontWeight::MEDIUM);
    div()
        .id("stance-cut-readout")
        .flex_none()
        .flex()
        .items_center()
        // FAULT 1a: this used to fill+round a `DARK_CANVAS` rectangle over
        // the band's own `DARK_PANEL` ground -- a second hard-edged plate
        // that read as a chip beside Export, the room's one bordered
        // commitment (DESIGN §4). A readout is still a plate in the
        // language's own words, but this band already has one (Export); the
        // fix is to drop the competing fill, not add a second box the
        // Export chip has to compete with. Values stay in `ink2`, the
        // separators in `ink3` -- the readout is still legible sitting
        // straight on the band ground.
        .px(px(4.))
        .font(style.font)
        .text_size(style.size)
        .child(val(odometer))
        .when_some(deltas, |el, (in_d, out_d)| {
            el.child(sep())
                .child(val(format!("out {out_d}")))
                .child(sep())
                .child(val(format!("in {in_d}")))
        })
        .child(sep())
        .child(
            div()
                .text_color(rgb(if roll_on { INK1() } else { INK4() }))
                .child("roll"),
        )
}
/// Export-range marks belong beside the cut readout they constrain (DESIGN §9):
/// each is a direct mouse door to the same action its chord asks for.
fn range_marks(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-range-marks")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(ghost("stance-mark-in", player, "I", ActionId::SetIn, false, cx))
        .child(ghost("stance-mark-out", player, "O", ActionId::SetOut, false, cx))
        .child(ghost(
            "stance-clear-range",
            player,
            "×",
            ActionId::ClearRange,
            false,
            cx,
        ))
}

/// The contact strip (MOCK-SPEC "Contact strip"): the whole-film minimap
/// filling the band's remaining width, with a 1px viewport bracket marking
/// where the bench's own window sits. Click jumps the playhead; drag pans
/// the bench window.
///
/// The trace is real audio, not a placeholder: [`Player::waves`] already
/// caches each source's peaks for the bench's own waveform clips
/// ([`bench_stance`]'s `clip_box`), so every audio-lane clip draws its own
/// stretch of that same envelope here, positioned by its timeline fraction
/// instead of `bench_stance`'s per-lane pixel scale -- the cheapest honest
/// whole-film trace this app can draw without a new decode pass. Splice
/// ticks (video-lane clip boundaries) still mark cut points on top of it.
fn contact_strip(player: &Player, position: f64, cx: &mut Context<Player>) -> impl IntoElement {
    let duration = player.drawn_duration();
    let view = player.view();
    let (left_frac, width_frac) = if duration > 0. {
        (
            (view.scale.start / duration).clamp(0., 1.) as f32,
            (view.span() / duration).clamp(0., 1.) as f32,
        )
    } else {
        (0., 1.)
    };
    let fps = player.active_fps();
    let frac = |f: u32| ((f64::from(f) / fps) / duration).clamp(0., 1.) as f32;
    let ticks: Vec<f32> = player
        .session
        .as_ref()
        .map(|s| {
            s.lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Video)
                .flat_map(|l| s.lane_clips(l).iter().map(Clip::end).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|_| duration > 0.)
        .map(frac)
        .collect();
    let traces: Vec<(f32, f32, Arc<Vec<(f32, f32)>>, f64, f64, u32)> = player
        .session
        .as_ref()
        .filter(|_| duration > 0.)
        .map(|s| {
            s.lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Audio)
                .flat_map(|l| s.lane_clips(l).to_vec())
                .filter_map(|clip| {
                    let source = player.sources().get(clip.source)?;
                    let Wave::Peaks(peaks) = player.waves.get(&(source.path.clone(), source.audio_stream))?.clone() else {
                        return None;
                    };
                    let (in_f, out_f) = (f64::from(clip.in_frame) / fps, f64::from(clip.out_frame) / fps);
                    Some((frac(clip.start), frac(clip.end()), peaks, in_f, out_f, source_tint(clip.source)))
                })
                .collect()
        })
        .unwrap_or_default();
    let strip_bounds = STRIP_BOUNDS.with(Rc::clone);
    div()
        .id("stance-contact-strip")
        .relative()
        .flex_1()
        .min_w(px(0.))
        .h(px(28.))
        .cursor_pointer()
        .tooltip(|_, cx| {
            cx.new(|_| Tip("Contact strip — drag scrubs the playhead, click jumps; drag the bracket's grips to pan the bench".into()))
                .into()
        })
        // Division of the plain drag (MOCK-SPEC "Contact strip", task's own
        // instruction to decide honestly): a person reaches for this strip to
        // move through the FILM far more often than to slide the bench's own
        // viewport window, so the strip's own drag now scrubs the playhead
        // continuously -- press-and-move is not just a jump on release. The
        // bracket keeps panning, but only from its own grip notches
        // (`viewport_bracket` below), which `stop_propagation` on their own
        // press so a pan-start never also fires a scrub-jump underneath it.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener({
                let bounds = strip_bounds.clone();
                move |this, event: &MouseDownEvent, _, cx| {
                    let duration = this.drawn_duration();
                    if duration > 0. {
                        let frac = frac_along(event.position.x, bounds.get());
                        this.seek(f64::from(frac) * duration, cx);
                    }
                }
            }),
        )
        .on_mouse_move(cx.listener({
            let bounds = strip_bounds.clone();
            move |this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    PAN_ANCHOR.with(|a| a.set(None));
                    return;
                }
                match PAN_ANCHOR.with(Cell::get) {
                    // A pan is live (started on the bracket's own grips): slide
                    // the bench viewport instead of the playhead.
                    Some(last_x) => {
                        let b = STRIP_BOUNDS.with(Rc::clone).get();
                        let w = f32::from(b.size.width).max(1.);
                        let dx = f32::from(event.position.x) - last_x;
                        let duration = this.drawn_duration();
                        if duration > 0. {
                            let delta = f64::from(dx / w) * duration;
                            let span = this.view().span();
                            let max_start = (duration - span).max(0.);
                            this.scale.start = (this.scale.start + delta).clamp(0., max_start);
                        }
                        PAN_ANCHOR.with(|a| a.set(Some(f32::from(event.position.x))));
                    }
                    // No pan armed: the plain drag scrubs, continuously
                    // reseeking to wherever the hand is along the strip.
                    None => {
                        let duration = this.drawn_duration();
                        if duration > 0. {
                            let frac = frac_along(event.position.x, bounds.get());
                            this.seek(f64::from(frac) * duration, cx);
                        }
                    }
                }
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|_, _, _, _| PAN_ANCHOR.with(|a| a.set(None))),
        )
        .child(bounds_probe(strip_bounds))
        // Baseline: a quiet hairline under the real traces below it, so an
        // audio-free stretch of film still reads as film rather than gap.
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(13.))
                .h(px(2.))
                .bg(rgb(DARK_HAIRLINE())),
        )
        // The real trace: each audio-lane clip's own envelope, in the
        // source's ink, positioned by timeline fraction (see the fn doc
        // above) -- the whole-film minimap MOCK-SPEC asks for, not a
        // placeholder line.
        .children(traces.into_iter().map(|(from, to, peaks, in_f, out_f, ink)| {
            div()
                .absolute()
                .left(relative(from))
                .top(px(4.))
                .bottom(px(4.))
                .w(relative((to - from).max(0.002)))
                .child(waveform_ink(peaks, in_f, out_f, ink))
        }))
        .children(ticks.into_iter().map(|frac| {
            div()
                .absolute()
                .left(relative(frac))
                .top(px(2.))
                .w(px(1.))
                .h(px(24.))
                .bg(rgb(LAMP_WHITE()))
        }))
        // The viewport bracket (MOCK-SPEC: "1px viewport bracket (with grip
        // notches)"): FAULT 1's box fix -- DESIGN §4 reserves the full
        // bordered rectangle for the room's one commit chip (Export), so this
        // draws only the two edges (left/right verticals + corner notches),
        // never a closed box, and stays visually distinct from the trace it
        // sits over (ink1 lines/notches vs the trace's film ink fills).
        .child(
            div()
                .absolute()
                .left(relative(left_frac))
                .top_0()
                .h_full()
                .w(relative(width_frac.max(0.01)))
                .border_l_1()
                .border_r_1()
                .border_color(rgb(INK1()))
                .child(grip("stance-strip-grip-lt", true, true))
                .child(grip("stance-strip-grip-lb", true, false))
                .child(grip("stance-strip-grip-rt", false, true))
                .child(grip("stance-strip-grip-rb", false, false)),
        )
        // The lamp-white playhead marker: this is what turns the strip from
        // a trace into a slider with a handle (task's own wording) -- the
        // one 1px line on the whole band that names *this* film moment,
        // drawn last so it always sits above the trace and the bracket.
        .when(duration > 0., |el| {
            let frac = ((position / duration).clamp(0., 1.)) as f32;
            el.child(
                div()
                    .id("stance-strip-playhead")
                    .absolute()
                    .left(relative(frac))
                    .top_0()
                    .h_full()
                    .w(px(1.))
                    .bg(rgb(LAMP_WHITE())),
            )
        })
}

/// One grip notch on the viewport bracket (MOCK-SPEC "grip notches"): the
/// only part of the contact strip that pans the bench window rather than
/// scrubbing the playhead (see [`contact_strip`]'s own doc on that split).
/// `stop_propagation` on its own press so starting a pan here never also
/// fires the strip's scrub-jump underneath it.
fn grip(id: &'static str, left: bool, top: bool) -> impl IntoElement {
    div()
        .id(id)
        .absolute()
        .when(left, |d| d.left(px(-3.)))
        .when(!left, |d| d.right(px(-3.)))
        .when(top, |d| d.top_0())
        .when(!top, |d| d.bottom_0())
        .w(px(8.))
        .h(px(10.))
        .cursor_col_resize()
        .child(
            div()
                .absolute()
                .when(left, |d| d.left(px(3.)))
                .when(!left, |d| d.right(px(3.)))
                .when(top, |d| d.top_0())
                .when(!top, |d| d.bottom_0())
                .w(px(2.))
                .h(px(6.))
                .bg(rgb(INK1())),
        )
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, _, cx| {
                PAN_ANCHOR.with(|a| a.set(Some(f32::from(event.position.x))));
                cx.stop_propagation();
            },
        )
}

/// The Export chip (MOCK-SPEC "Export chip", DESIGN §4): the room's single
/// bordered control. Opens the export card exactly the way the legacy room's
/// own Export button does ([`Player::open_export`], `ui/toolbar.rs`'s
/// `action_control` click) -- this is the fix for the shipped defect: the
/// darkroom used to suppress `ActionId::Export` outright because it drew no
/// surface for the card to land on; `stance.rs::render` now dispatches it
/// like every other action, and the card itself
/// ([`Player::export_card`]/`export_progress_card`) is mounted over the room
/// by [`super::stance::render`] the same way the keys overlay is.
fn export_chip(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let exporting = player.exporting().is_some();
    let label_style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    let chord_style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
    div()
        .id("stance-export")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(3.))
        .border_1()
        .border_color(rgb(DARK_HAIRLINE()))
        .bg(rgb(DARK_RAISED()))
        .text_color(rgb(INK1()))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_PANEL())))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            if this.exporting().is_some() {
                this.cancel_export();
            } else {
                this.open_export(cx);
            }
            cx.notify();
        }))
        .child(
            div()
                .font(label_style.font)
                .text_size(label_style.size)
                .child(if exporting { "Exporting" } else { "Export" }),
        )
        .child(
            div()
                .font(chord_style.font)
                .text_size(chord_style.size)
                .text_color(rgb(INK3()))
                // FAULT 2: compact badge everywhere in the band, same fix as
                // `ghost` above (`Export  ^e`, not `Export  ctrl+e`).
                .child(player.keymap.chord(ActionId::Export)),
        )
}

/// The monitoring level's continuous pointer door. It shares the Player's
/// measured bar and root drag plumbing with the legacy slider, but keeps the
/// Darkroom's achromatic track and ink ladder.
fn volume_slider(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let volume = player.volume;
    let enabled = player.enable(ActionId::ToggleMute, None).yes();
    div()
        .id("stance-tb-volume-bar")
        .relative()
        .flex_none()
        .w(px(VOLUME_W))
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .tooltip(|_, cx| {
            cx.new(|_| Tip("Volume — drag to set the level; the button mutes".into())).into()
        })
        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
        .when(enabled, |d| {
            d.cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.volume_dragging = true;
                        this.drag_volume(event.position.x, cx);
                    }),
                )
                .child(bounds_probe(player.volume_bar.clone()))
        })
        .child(
            div()
                .w_full()
                .h(px(4.))
                .rounded(px(2.))
                .bg(rgb(DARK_RAISED()))
                .child(
                    div()
                        .h_full()
                        .w(relative(volume.along()))
                        .rounded(px(2.))
                        .bg(rgb(if volume.muted { INK3() } else { INK2() })),
                ),
        )
}

/// The whole band, left to right per MOCK-SPEC: hero timecode, ghost
/// transport, cut readout, the contact strip filling the rest, the Export
/// chip at the end.
pub(crate) fn render(player: &mut Player, position: f64, cx: &mut Context<Player>) -> impl IntoElement {
    let tc = timecode(position, player.active_fps());
    div()
        .id("stance-time-band-row")
        // FAULT 1 fix: this div used to be `flex_none`, which sized it to its
        // own content inside `stance.rs::time_band()`'s row -- leaving the
        // contact strip's `flex_1` nothing to grow into (its "free space" was
        // zero, since the parent itself never claimed the band's full width).
        // `flex_1` here is what actually lets the strip fill the band.
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(12.))
        // The most-read element anchors its region (DESIGN §5): the
        // timecode leads.
        .child(hero_timecode(&tc))
        // FAULT 2, the J/K/L question: MOCK-SPEC reads this row's chords as
        // `J`/`spc`/`L`, which DESIGN §6 names as the shuttle. No shuttle
        // action exists anywhere in this codebase (grepped -- `keymap.rs`,
        // `player/actions.rs`, the engine: nothing), and `j`/`k`/`l` are
        // already bound to Speed/Color/Lift (`keymap.rs`'s Clip-verb group),
        // not to this row -- rebinding them here would silently steal those,
        // and `keymap.rs` is not this task's file to make that call in. So
        // the glyphs below stay wired to what actually exists and already
        // fires correctly (`JumpBack`/`Play`/`JumpForward`, real one-second
        // stepping): each badge now reads that action's own compact chord
        // (`^←`/`spc`/`^→`) instead of the mismatched mock label, which is
        // what keeps glyph, badge and action honestly in agreement per this
        // task's own rule -- the mock's `J`/`L` reading stays unimplemented
        // and is named here rather than faked.
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(ghost(
                    "stance-tb-jumpback",
                    player,
                    "◀◀",
                    ActionId::JumpBack,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-play",
                    player,
                    if player.transport().is_playing() { "❚❚" } else { "▶" },
                    ActionId::Play,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-jumpforward",
                    player,
                    "▶▶",
                    ActionId::JumpForward,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-step-back",
                    player,
                    "◀",
                    ActionId::StepBack,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-step-forward",
                    player,
                    "▶",
                    ActionId::StepForward,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-start",
                    player,
                    "↤",
                    ActionId::GoStart,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-end",
                    player,
                    "↦",
                    ActionId::GoEnd,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-sync-prev",
                    player,
                    "‹|",
                    ActionId::PrevSyncPoint,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-sync-next",
                    player,
                    "|›",
                    ActionId::NextSyncPoint,
                    false,
                    cx,
                ))
                // Loop (homeless per this task's audit): the playback-loop
                // toggle -- burst-use during editing per the charter's own
                // classification -- earns the transport cluster it plays
                // alongside, not the CUT group's `LoopTrim` two files over
                // (a different verb already drawn on the spine, "↻" taken).
                // "∞" reads as "keeps going" without borrowing that glyph.
                .child(ghost(
                    "stance-tb-loop",
                    player,
                    "∞",
                    ActionId::Loop,
                    player.loop_on,
                    cx,
                )),
        )
        // The audio-monitoring cluster (homeless per this task's audit):
        // ToggleMute/VolumeUp/VolumeDown, burst-use per the charter, placed
        // beside the transport it plays with rather than the spine (the
        // rail is already tightened, and this is a play-time concern, same
        // land as J/spc/L above it). The level itself is the mute button's
        // own label (legacy `toolbar.rs`'s own convention: an "×" prefix
        // marks muted rather than a second colour), so one click toggles
        // mute and the two ghosts flanking it nudge the level -- three
        // homeless actions sharing one cluster rather than three unrelated
        // rows.
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(ghost("stance-tb-vol-down", player, "−", ActionId::VolumeDown, false, cx))
                .child(ghost(
                    "stance-tb-mute",
                    player,
                    if player.volume.muted {
                        format!("× {}%", player.volume.percent())
                    } else {
                        format!("{}%", player.volume.percent())
                    },
                    ActionId::ToggleMute,
                    player.volume.muted,
                    cx,
                ))
                .child(volume_slider(player, cx))
                .child(ghost("stance-tb-vol-up", player, "+", ActionId::VolumeUp, false, cx)),
        )
        .child(range_marks(player, cx))
        .child(cut_readout(player))
        .child(contact_strip(player, position, cx))
        .child(save_verb(player, cx))
        .child(export_chip(player, cx))
}

/// `Save` (homeless per this task's audit): not boxed -- DESIGN §4 reserves
/// the room's one border for the commit-class Export beside it -- but filed
/// right next to it anyway. `keymap.rs`'s own `Category::File` already
/// files Save beside Export and Screenshot on the strength of what each one
/// writes, and the ledger this task named as Save's natural home
/// (`name.edith · saved`/`unsaved`) lives in `stance.rs`, a concurrent
/// builder's file this task does not touch -- so the verb lands here
/// instead, a ghost sharing the export chip's own end-of-band land rather
/// than competing with the ledger strip below it for the same state.
fn save_verb(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let label_style = label(type_scale::LABEL_ROW_PX, FontWeight::MEDIUM);
    let chord_style = mono(type_scale::CHORD_METADATA_MIN_PX, FontWeight::MEDIUM);
    div()
        .id("stance-tb-save")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .cursor_pointer()
        .hover(|s| s.text_color(rgb(INK1())))
        .text_color(rgb(INK2()))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.act(ActionId::Save, window, cx);
        }))
        .child(
            div()
                .font(label_style.font)
                .text_size(label_style.size)
                .child("Save"),
        )
        .child(
            div()
                .font(chord_style.font)
                .text_size(chord_style.size)
                .text_color(rgb(INK3()))
                .child(player.keymap.chord(ActionId::Save)),
        )
}
