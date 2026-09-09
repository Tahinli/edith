//! The time band's own content (MOCK-SPEC.md "Time band", DESIGN.md §5):
//! hero timecode, one three-glyph transport, the cut odometer, the contact
//! strip (whole-film minimap, carrying the export range's two mark grips)
//! and the boxed Export chip. `stance.rs::time_band()`'s frame draws
//! the strip; this module owns what fills it, the same split
//! `dock_stance.rs`/`bench_stance.rs` already make for their regions.
//!
//! Type note: DESIGN.md §3's approved scale gives the hero timecode 18px,
//! close to the mock's ~20px reading. Every size in this module comes from
//! `ui::type_scale` (role, not a bare `px()` literal).

use crate::ui::hitmap;
use crate::ui::type_scale::{self, label, mono};
use crate::*;
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
    /// Which export mark a live drag on the strip is moving (the grips of
    /// `mark_grip` below), `None` when no mark drag is up. Same transient
    /// shape as `PAN_ANCHOR` above, and the same ceiling.
    static MARK_DRAG: Cell<Option<ActionId>> = const { Cell::new(None) };
    /// The band row's own measured width, read by the frame after the one
    /// that took it (`width_probe` asks for that frame whenever it changes).
    /// This is what the degradation ladder below keys off: the band cannot
    /// know its column's width any other way without a `Window` this
    /// module's `render` is not handed.
    static BAND_W: Rc<Cell<Pixels>> = Rc::new(Cell::new(px(0.)));
    /// Set once per `render` from the ladder, read by `ghost` (which the
    /// band's own helpers call three levels down) -- a render-scoped
    /// constant rather than a parameter threaded through every call site.
    static SHOW_CHORDS: Cell<bool> = const { Cell::new(true) };
}

/// The contact strip's floor: the flex_1 element absorbs the band's slack,
/// but a minimap narrower than this is not a map.
pub(crate) const STRIP_MIN_W: f32 = 120.;

/// The band's own width probe, `timeline_math::height_probe`'s shape on the
/// other axis: a measurement is read by the frame after the one that took it,
/// so a change has to ask for that frame or a resize would leave the ladder
/// one layer behind until something else happened to draw.
fn width_probe(into: Rc<Cell<Pixels>>) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            // Only on a change: an unconditional request is a repaint loop.
            if into.replace(bounds.size.width) != bounds.size.width {
                window.request_animation_frame();
            }
        },
        |_, _, _, _| (),
    )
    .absolute()
    .size_full()
}

/// Which layers of the band are still drawn at a given column width
/// (DESIGN §7's ladder). Cleanse round 2 (2026-09-10) left the band five
/// things -- timecode, the three transport glyphs, the odometer, the strip
/// and the Export chip -- and only one of them has anything left to shed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BandLayers {
    /// Chord badges under the transport glyphs.
    pub(crate) chords: bool,
}

/// The ladder itself, now two rungs. Every threshold is a MEASURED number,
/// taken off `$EDITH_HITMAP` in the harness: the row's own content box (what
/// `width_probe` reads -- the band's column less its 12px padding either
/// side) holds 454px of fixed groups beside the strip that absorbs the rest,
/// and the same band with the chords dropped holds 419px (each glyph's door
/// falls back to its `HIT_MIN` floor, so the chord row costs 35px of width
/// and 22px of height). Fixed width + `STRIP_MIN_W`, rounded up to the next
/// 10px, is what each rung costs:
///
/// | drawn                                        | fixed | needs  |
/// |----------------------------------------------|-------|--------|
/// | chords under every glyph                     |  454  |  580px |
/// | chords dropped (-35px, doors keep `HIT_MIN`) |  419  |  540px |
///
/// The five things the band is *for* are on every rung; below the floor's
/// own 540px there is nothing left to shed and `overflow_hidden` is what
/// keeps the rest inside the column. Everything the older four-rung ladder
/// used to drop -- the monitoring cluster, the sync pair and loop, the
/// `I O ×` marks -- left the band entirely in cleanse round 2, so the rungs
/// that shed them went with them.
pub(crate) fn band_layers(band_w: f32) -> BandLayers {
    BandLayers {
        chords: band_w >= 580.,
    }
}

/// The odometer's cut (DESIGN §6, "the cut readout is the odometer"): the cut
/// the playhead rests on, or -- when it rests in a gap -- the next one ahead
/// of it. `None` only when there is no cut at or after the playhead, which is
/// when the caller falls back to the selection.
pub(crate) fn odometer_cut(clips: &[Clip], frame: u32) -> Option<usize> {
    clips
        .iter()
        .position(|c| frame >= c.start && frame < c.end())
        .or_else(|| clips.iter().position(|c| c.start > frame))
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
        .justify_center()
        // WCAG 2.5.8, the same `HIT_MIN` floor `interact.rs` holds clip edges
        // to: a glyph may be 8px wide, its door may not. Padding only --
        // glyph sizes and the groups' own gaps are untouched.
        .min_w(px(HIT_MIN))
        .min_h(px(HIT_MIN))
        .px(px(2.))
        .gap(px(1.))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.act(action, window, cx);
        }))
        .children(hitmap::action(action, player.enable(action, None).yes()))
        // The glyphs he could not name (`|◂`, `∞`, `‹|`): the hover line is built
        // where the ghost is, so a glyph added to this band tomorrow cannot
        // ship without one.
        .tooltip(crate::ui::widgets::action_hover(player, action))
        .font(glyph_style.font)
        .text_size(glyph_style.size)
        .text_color(rgb(if active { INK1() } else { INK2() }))
        .child(glyph.into())
        .when(SHOW_CHORDS.with(Cell::get), |el| {
            el.child(
                div()
                    .flex_none()
                    .font(chord_style.font)
                    .text_size(chord_style.size)
                    .text_color(rgb(INK3()))
                    .child(player.keymap.chord(action)),
            )
        })
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
fn cut_readout(player: &Player, position: f64) -> impl IntoElement {
    let sep = || div().text_color(rgb(INK3())).child(" · ");
    let val = |s: String| div().text_color(rgb(INK2())).child(s);
    let anchor = player.selected.anchor();
    // The odometer reads the PLAYHEAD, not the selection (DESIGN 6): right
    // after a split the two halves are on the bench and nothing is picked,
    // and a readout saying `cut -/-` while the playhead sits on the new cut
    // is an odometer that stopped turning. The selection only answers when
    // the playhead is off every cut on its lane.
    let frame = (position * player.active_fps()).max(0.).round() as u32;
    let at_playhead = player.session.as_ref().and_then(|s| {
        let lane = anchor
            .map(|(lane, _)| lane)
            .or_else(|| s.lanes().into_iter().find(|l| l.kind == LaneKind::Video))?;
        Some((lane, odometer_cut(s.lane_clips(lane), frame)?))
    });
    let odometer = at_playhead
        .or(anchor)
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
        // Cleanse round 2: the word `roll` sat in the band dim at all times,
        // naming a mode that is off. The STATE stays -- when roll is armed
        // the word is there in `ink1` -- but at rest the odometer reads
        // `cut 14/37` and nothing else.
        .when(roll_on, |el| {
            el.child(sep())
                .child(div().text_color(rgb(INK1())).child("roll"))
        })
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
                    let Wave::Peaks(peaks) = player
                        .waves
                        .get(&(source.path.clone(), source.audio_stream))?
                        .clone()
                    else {
                        return None;
                    };
                    let (in_f, out_f) = (
                        f64::from(clip.in_frame) / fps,
                        f64::from(clip.out_frame) / fps,
                    );
                    Some((
                        frac(clip.start),
                        frac(clip.end()),
                        peaks,
                        in_f,
                        out_f,
                        source_tint(clip.source),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let strip_bounds = STRIP_BOUNDS.with(Rc::clone);
    div()
        .id("stance-contact-strip")
        .relative()
        .flex_1()
        // The strip is what absorbs the band's slack, and it measured w=0 at
        // 1280 before this: it holds a floor now and the ladder sheds groups
        // above it instead of squeezing the map to nothing.
        .min_w(px(STRIP_MIN_W))
        .h(px(28.))
        .cursor_pointer()
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
                    MARK_DRAG.with(|m| m.set(None));
                    return;
                }
                // A mark grip is under the hand: the drag sets that mark
                // instead of scrubbing or panning (the grip's own press
                // armed this and stopped propagation, so this branch is the
                // only thing a mark drag does).
                if let Some(action) = MARK_DRAG.with(Cell::get) {
                    let duration = this.drawn_duration();
                    if duration > 0. {
                        let frac = frac_along(event.position.x, bounds.get());
                        let at = (f64::from(frac) * duration * this.active_fps()).max(0.).round()
                            as u32;
                        let (lo, hi) = this.range.unwrap_or((at, at));
                        let (a, b) = if action == ActionId::SetIn {
                            (at, hi)
                        } else {
                            (lo, at)
                        };
                        // Dragged past its partner, the mark under the hand
                        // becomes the OTHER mark (`ordered_range` is what
                        // keeps the pair legal, and `i`/`o` swap the same
                        // way). Without this flip a live drag re-orders on
                        // every move event and both ticks walk along with
                        // the pointer, which is what the first harness run
                        // caught.
                        if crate::player::actions::ordered_range(a, b) != (a, b) {
                            MARK_DRAG.with(|m| {
                                m.set(Some(if action == ActionId::SetIn {
                                    ActionId::SetOut
                                } else {
                                    ActionId::SetIn
                                }))
                            });
                        }
                        this.range = Some(crate::player::actions::ordered_range(a, b));
                    }
                    cx.notify();
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
            cx.listener(|_, _, _, _| {
                PAN_ANCHOR.with(|a| a.set(None));
                MARK_DRAG.with(|m| m.set(None));
            }),
        )
        // The plate sits beside the id it names: the mark-drag branch above
        // pushed the two more than `every_hitmap_control_wears_a_hover_line`
        // reaches when this hung at the top of the chain.
        .tooltip(|_, cx| {
            cx.new(|_| Tip("Contact strip — drag scrubs the playhead, click jumps; drag the bracket's grips to pan the bench".into()))
                .into()
        })
        .children(hitmap::control("timeline.contact-strip", "Timeline contact strip", true))
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
        // The export range's own two grips (DESIGN §5 as amended
        // 2026-09-10): the `I O ×` trio left the band, so the marks live on
        // the film itself. Nothing is drawn until a mark exists.
        .when_some(
            player.range.filter(|_| duration > 0.),
            |el, (in_f, out_f)| {
                el.child(mark_grip(
                    player,
                    "stance-strip-mark-in",
                    frac(in_f),
                    "Mark in",
                    ActionId::SetIn,
                ))
                .child(mark_grip(
                    player,
                    "stance-strip-mark-out",
                    frac(out_f),
                    "Mark out",
                    ActionId::SetOut,
                ))
            },
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

/// One export-range mark, drawn on the contact strip at its own film
/// position: an `ink1` tick beside the bracket's lamp-white playhead, with a
/// ±6px door around it that takes precedence over the strip's own
/// click-jump/drag-scrub (`stop_propagation` on its press, the same split
/// [`grip`] below makes for the bench-window pan). Dragging it sets that
/// mark; `i`, `o` and `^u` are unchanged.
fn mark_grip(
    player: &Player,
    id: &'static str,
    frac: f32,
    name: &'static str,
    action: ActionId,
) -> impl IntoElement {
    let chord = player.keymap.chord(action);
    div()
        .id(id)
        .absolute()
        .left(relative(frac))
        .top_0()
        .h_full()
        .ml(px(-MARK_GRAB))
        .w(px(MARK_GRAB * 2. + 1.))
        .flex()
        .justify_center()
        .cursor_col_resize()
        .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
            MARK_DRAG.with(|m| m.set(Some(action)));
            cx.stop_propagation();
        })
        .tooltip(crate::ui::widgets::tip_hover(name, &chord, None))
        .children(hitmap::action(action, player.enable(action, None).yes()))
        .child(div().w(px(1.)).h_full().bg(rgb(INK1())))
}

/// How far either side of a mark tick the hand still grabs it.
const MARK_GRAB: f32 = 6.;

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
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
            PAN_ANCHOR.with(|a| a.set(Some(f32::from(event.position.x))));
            cx.stop_propagation();
        })
        .tooltip(crate::ui::widgets::tip_hover("Pan the timeline window", "drag", None))
        .children(hitmap::control(id, "Timeline viewport grip", true))
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
        .tooltip(crate::ui::widgets::action_hover(player, if exporting { ActionId::CancelExport } else { ActionId::Export }))
        .children(hitmap::action(
            if exporting {
                ActionId::CancelExport
            } else {
                ActionId::Export
            },
            true,
        ))
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

/// The whole band, left to right per MOCK-SPEC: hero timecode, ghost
/// transport, cut readout, the contact strip filling the rest, the Export
/// chip at the end.
pub(crate) fn render(
    player: &mut Player,
    position: f64,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let tc = timecode(position, player.active_fps());
    let band_w = BAND_W.with(Rc::clone);
    // Layer decision for THIS frame, off the width the last frame measured
    // (`width_probe` asks for a frame whenever that width changes, so a
    // resize settles in one).
    let layers = band_layers(f32::from(band_w.get()));
    SHOW_CHORDS.with(|c| c.set(layers.chords));
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
        // Nothing in this band may ever paint (or be clicked) outside its own
        // column again: the Export chip used to land at x=1113 under the
        // dock, where a click on blank dock space opened the export card.
        .overflow_hidden()
        .gap(px(12.))
        .px(px(12.))
        .child(width_probe(band_w))
        // The most-read element anchors its region (DESIGN §5): the
        // timecode leads.
        .child(hero_timecode(&tc))
        // Cleanse round 2 (2026-09-10): one transport, three glyphs. The
        // shuttle pair (`◀◀ ▶▶`), the home/end pair, the sync points, the
        // loop toggle, the monitoring cluster, the `I O ×` marks and Save
        // all left the band -- each keeps its chord, its KEYS-tab row and
        // (where it has one) its right-click home, and the marks became the
        // contact strip's own two grips.
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(ghost(
                    "stance-tb-step-back",
                    player,
                    "|◂",
                    ActionId::StepBack,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-play",
                    player,
                    if player.transport().is_playing() {
                        "❚❚"
                    } else {
                        "▶"
                    },
                    ActionId::Play,
                    false,
                    cx,
                ))
                .child(ghost(
                    "stance-tb-step-forward",
                    player,
                    "▸|",
                    ActionId::StepForward,
                    false,
                    cx,
                )),
        )
        .child(cut_readout(player, position))
        .child(contact_strip(player, position, cx))
        .child(export_chip(player, cx))
}
