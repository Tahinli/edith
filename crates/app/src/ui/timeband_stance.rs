//! The time band's own content (MOCK-SPEC.md "Time band", DESIGN.md §5):
//! hero timecode, ghost transport, cut readout, the contact strip (whole-film
//! minimap) and the boxed Export chip. `stance.rs::time_band()`'s frame draws
//! the strip; this module owns what fills it, the same split
//! `dock_stance.rs`/`bench_stance.rs` already make for their regions.
//!
//! Type note: DESIGN.md §3 says the hero timecode is 13px; the approved mock
//! (MOCK-SPEC.md) reads closer to ~20px. Kept at 13px here -- the binding
//! contract's own number, already what `stance.rs::time_band()` shipped --
//! rather than silently picking the mock's, and named here per this task's
//! own instruction to say so when the two disagree.

use crate::*;

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
/// glyph ~13px `ink2` over its chord ~9.5px `ink3`, read live off the keymap
/// so a rebind can never leave the band showing a stroke that no longer
/// fires it -- the same shape `stance::ghost` draws for the spine, kept local
/// here so this module owns its own region end to end.
fn ghost(player: &Player, glyph: &str, action: ActionId) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(1.))
        .text_size(px(13.))
        .text_color(rgb(INK2()))
        .child(glyph.to_string())
        .child(
            div()
                .text_size(px(9.))
                .text_color(rgb(INK3()))
                .child(player.keymap.display(action)),
        )
}

/// The hero timecode, digits `ink1` and colons `ink3` (DESIGN §3, MOCK-SPEC
/// "Hero timecode"): the one place in the band the string is split apart
/// rather than painted whole, so the colons can read a shade quieter than
/// the numbers either side of them.
fn hero_timecode(tc: &str) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .text_size(px(13.))
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
    div()
        .id("stance-cut-readout")
        .flex_none()
        .flex()
        .items_center()
        .px(px(8.))
        .py(px(4.))
        .rounded(px(2.))
        .bg(rgb(DARK_CANVAS()))
        .text_size(px(10.5))
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

/// The contact strip (MOCK-SPEC "Contact strip"): the whole-film minimap
/// filling the band's remaining width, with a 1px viewport bracket marking
/// where the bench's own window sits. Click jumps the playhead; drag pans
/// the bench window.
///
/// hook: the trace itself is splice positions (real clip boundaries, from
/// [`PlaybackSession::lanes`]/`lane_clips` -- data the app already has), not
/// a motion or audio trace -- that needs a per-frame decode of the whole
/// film this task did not budget for. What would make it real: a cached
/// low-res luma/RMS pass over each source at import, sampled here instead of
/// clip boundaries.
fn contact_strip(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
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
        .map(|f| ((f64::from(f) / fps) / duration).clamp(0., 1.) as f32)
        .collect();
    let strip_bounds = STRIP_BOUNDS.with(Rc::clone);
    div()
        .id("stance-contact-strip")
        .relative()
        .flex_1()
        .min_w(px(0.))
        .h(px(28.))
        .cursor_pointer()
        .tooltip(|_, cx| {
            cx.new(|_| Tip("Contact strip — click jumps, drag pans the bench".into()))
                .into()
        })
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
                    PAN_ANCHOR.with(|a| a.set(Some(f32::from(event.position.x))));
                }
            }),
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            let Some(last_x) = PAN_ANCHOR.with(Cell::get) else {
                return;
            };
            match event.pressed_button {
                Some(MouseButton::Left) => {
                    let bounds = STRIP_BOUNDS.with(Rc::clone).get();
                    let w = f32::from(bounds.size.width).max(1.);
                    let dx = f32::from(event.position.x) - last_x;
                    let duration = this.drawn_duration();
                    if duration > 0. {
                        let delta = f64::from(dx / w) * duration;
                        let span = this.view().span();
                        let max_start = (duration - span).max(0.);
                        this.scale.start = (this.scale.start + delta).clamp(0., max_start);
                    }
                    PAN_ANCHOR.with(|a| a.set(Some(f32::from(event.position.x))));
                    cx.notify();
                }
                _ => PAN_ANCHOR.with(|a| a.set(None)),
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|_, _, _, _| PAN_ANCHOR.with(|a| a.set(None))),
        )
        .child(bounds_probe(strip_bounds))
        // The baseline trace (see the `hook:` above).
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(13.))
                .h(px(2.))
                .bg(rgb(DARK_HAIRLINE())),
        )
        .children(ticks.into_iter().map(|frac| {
            div()
                .absolute()
                .left(relative(frac))
                .top(px(9.))
                .w(px(1.))
                .h(px(10.))
                .bg(rgb(LAMP_WHITE()))
        }))
        // The viewport bracket: the bench window's own span, with grip
        // notches at each edge (MOCK-SPEC: "1px viewport bracket (with grip
        // notches)").
        .child(
            div()
                .absolute()
                .left(relative(left_frac))
                .top_0()
                .h_full()
                .w(relative(width_frac.max(0.01)))
                .border_1()
                .border_color(rgb(INK1()))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .w(px(2.))
                        .h(px(6.))
                        .bg(rgb(INK1())),
                )
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .bottom_0()
                        .w(px(2.))
                        .h(px(6.))
                        .bg(rgb(INK1())),
                ),
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
    div()
        .id("stance-export")
        .flex_none()
        .px(px(10.))
        .py(px(6.))
        .rounded(px(3.))
        .border_1()
        .border_color(rgb(DARK_HAIRLINE()))
        .bg(rgb(DARK_RAISED()))
        .text_size(px(10.5))
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
        .child(format!(
            "{}  {}",
            if exporting { "Exporting" } else { "Export" },
            player.keymap.display(ActionId::Export)
        ))
}

/// The whole band, left to right per MOCK-SPEC: hero timecode, ghost
/// transport, cut readout, the contact strip filling the rest, the Export
/// chip at the end.
pub(crate) fn render(player: &mut Player, position: f64, cx: &mut Context<Player>) -> impl IntoElement {
    let tc = timecode(position, player.active_fps());
    div()
        .id("stance-time-band")
        .flex_none()
        .h_full()
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(12.))
        // The most-read element anchors its region (DESIGN §5): the
        // timecode leads.
        .child(hero_timecode(&tc))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(ghost(player, "◀◀", ActionId::JumpBack))
                .child(ghost(
                    player,
                    if player.transport().is_playing() { "❚❚" } else { "▶" },
                    ActionId::Play,
                ))
                .child(ghost(player, "▶▶", ActionId::JumpForward)),
        )
        .child(cut_readout(player))
        .child(contact_strip(player, cx))
        .child(export_chip(player, cx))
}
