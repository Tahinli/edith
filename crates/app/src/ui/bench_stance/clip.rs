//! A placed clip's box: ink spine, trace, name plate and edge trims (DESIGN §5, §7).

use super::*;
use crate::ui::icons::Icon;
use crate::ui::theme::{self, ClipKind, ClipTone, tokens};

/// The height of a clip's name strip: 18 on a picture lane, 14 on audio (no
/// thumbnails to make room for).
const STRIP_H_VIDEO: f32 = 18.;
const STRIP_H_AUDIO: f32 = 14.;
/// A clip box sits this far in from the lane's top and bottom, so its 2px
/// selection outline fits inside the lane and selecting changes no geometry.
const CLIP_INSET: f32 = 2.;

/// What kind of clip a placed clip is, for its colours. Single door: the
/// generator and adjustment kinds slot in here.
pub(super) fn clip_kind(lane: Lane, compound: bool, multicam: bool, image: bool) -> ClipKind {
    match (compound, multicam, image, lane.kind) {
        (true, ..) => ClipKind::Compound,
        (_, true, ..) => ClipKind::Multicam,
        (.., LaneKind::Audio) => ClipKind::Audio,
        (.., true, _) => ClipKind::Image,
        (.., LaneKind::Subtitle) => ClipKind::Caption,
        _ => ClipKind::Video,
    }
}

/// The colours of one clip kind in the live theme.
pub(super) fn clip_tone(kind: ClipKind) -> ClipTone {
    tokens().clip(kind)
}

/// The text colour on a kind's name strip: the theme's own where it names one,
/// else white or near-black, whichever reads better on the strip.
pub(super) fn strip_label(tone: ClipTone) -> u32 {
    match theme::contrast(strip_fill(tone), 0xffffff) >= 4.5 {
        true => 0xffffff,
        false => match tone.label {
            0 => 0x14161a,
            label => label,
        },
    }
}

/// The name strip's fill: the kind's own strip colour, deepened just far enough (never more
/// than half way to black) that white ink on it reads at 4.5:1 -- the audio green was too
/// light for any light label.
pub(super) fn strip_fill(tone: ClipTone) -> u32 {
    (0..=10_u8)
        .map(|i| theme::mix(tone.strip, 0x000000, f32::from(i) * 0.05))
        .find(|c| theme::contrast(*c, 0xffffff) >= 4.5)
        .unwrap_or(tone.strip)
}

/// A hover plate lifted above its handle: the fade handle sits on the name strip, and a plate
/// hanging below the pointer covers the next clip's label.
struct AboveTip(gpui::AnyView);

impl Render for AboveTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().relative().top(px(-52.)).child(self.0.clone())
    }
}

/// The warning amber of the live theme, for clips whose media is missing.
fn theme_warn() -> u32 {
    tokens().warn
}

/// What a character is taken as when the face cannot say (an average Inter 12px medium glyph).
const LABEL_CHAR_W: f32 = 6.8;

/// The name as it may be shown in `room` px: whole when it fits, cut with an ellipsis where it
/// does not, and `None` when not even one letter and the ellipsis would fit. Measured with the
/// strip's own face (`caption_strong`), so the cut is the one the text paints -- a guessed
/// width let "shapes" through whole and then painted it past the strip.
pub(crate) fn fit_label(cx: &App, name: &str, room: f32) -> Option<String> {
    let style = type_scale::caption_strong();
    let system = cx.text_system();
    let id = system.resolve_font(&style.font);
    fit_with(name, room, |ch| system.advance(id, style.size, ch).map_or(LABEL_CHAR_W, |s| f32::from(s.width)))
}

/// [`fit_label`]'s rule over any advance: `adv` gives one character's width in px.
pub(super) fn fit_with(name: &str, room: f32, adv: impl Fn(char) -> f32) -> Option<String> {
    if name.chars().map(&adv).sum::<f32>() <= room {
        return Some(name.to_string());
    }
    let ell = adv('\u{2026}');
    let mut w = 0.;
    let mut keep = 0;
    for (i, ch) in name.char_indices() {
        w += adv(ch);
        if w + ell > room {
            break;
        }
        keep = i + ch.len_utf8();
    }
    (keep > 0).then(|| format!("{}\u{2026}", name[..keep].trim_end()))
}

/// Everything [`clip_box`] is drawn from about one clip's place in its lane:
/// the clip as placed, its neighbours on the lane (the room its trim zones may
/// spill into) and the selection state of the lane around it.
pub(super) struct ClipSlot<'a> {
    pub(super) lane: Lane,
    pub(super) idx: usize,
    pub(super) clip: Clip,
    pub(super) prev: Option<Clip>,
    pub(super) next: Option<Clip>,
    pub(super) picks: &'a [(Lane, usize)],
    pub(super) pick_links: &'a [Option<u32>],
}

/// A clip's ink spine + body, degrading by [`Tier`] (DESIGN §5, §7). A single
/// placeholder ink stands in for every source until real extraction lands.
///
/// hook: §12 step 5 -- per-source film ink attaches here, replacing
/// `INK2()` with the source's own quantized hue.
pub(super) fn clip_box(
    player: &Player,
    slot: ClipSlot,
    scale: Scale,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let ClipSlot {
        lane,
        idx,
        clip: placed,
        prev,
        next,
        picks,
        pick_links,
    } = slot;
    // The placed clip, for the drag payload -- and the clip as an in-flight
    // edge trim is showing it, for the box (`Player::trimmed`, the same
    // split `ui/timeline.rs`'s own clip box makes).
    let clip = &player.trimmed(lane, idx, placed.clone());
    let (start, len) = (
        f64::from(clip.start) / player.fps,
        f64::from(clip.frames()) / player.fps,
    );
    let span = scale.width_px(len);
    let width = clip_width(span);
    let left = scale.px_at(start);
    let on = marked((lane, idx), clip.link, picks, pick_links);
    let t = tier(span);
    let source = player.sources().get(clip.source);
    let audio = lane.kind == LaneKind::Audio;
    // The name, extension dropped (`files::stem`, the ledger's and the dock's
    // helper too). The audio half of a linked insert wears it as well, like the
    // picture half: a lane read alone still says whose sound it is.
    // A compound clip plays another sequence: no file, so it wears its sequence's name and a
    // neutral stacked look in place of a film ink and a thumbnail.
    let compound = engine::project::sequence_id_of(clip.source)
        .map(|id| player.session.as_ref().and_then(|s| s.sequence_name(id)).unwrap_or_default());
    let label = source.map(|s| stem(&s.path)).or_else(|| compound.clone());
    // A compound wears the Layers icon beside its name (drawn below, not a glyph in the text).
    let is_compound = source.is_none() && compound.is_some();
    // A multicam clip names the angle each segment shows: "2 cam_b".
    let label = match (label, player.session.as_ref().and_then(|s| s.multicam_at(lane, idx))) {
        (Some(l), Some((_, names))) => {
            let n = usize::from(clip.angle.video);
            Some(format!("{l} · {} {}", n + 1, names.get(n).map_or("", String::as_str)))
        }
        (l, _) => l,
    };
    // A generated clip has no file and no thumbnail: it draws what it makes.
    let generated = source.and_then(|s| engine::generate::Gen::parse(&s.path)).filter(|_| !audio);
    let mark = clip.mark;
    let fade_in = player.shown_fade_in(lane, idx, clip);
    let fade_out = player.shown_fade_out(lane, idx, clip);
    let dissolves = lane.kind == LaneKind::Video
        && clip.transition_out > 0
        && next.as_ref().is_some_and(|next| next.start == clip.end());
    let dissolve_tip: SharedString = format!(
        "Remove {} — {}",
        clip.transition_kind.name().replace('-', " "),
        player.keymap.display(ActionId::Dissolve)
    )
    .into();
    // The overlap the join plays over: the outgoing clip's tail, and the
    // incoming clip's head across the same cut, each a band named by the
    // kind's short label once it is wide enough, with its length on hover.
    let band_text = |wide: f32| {
        // The full label if it fits, else the kind's word alone, else nothing.
        // Caption text is about 6.5 px a glyph; 8 px of inset on each side.
        let fits = |text: &str| text.chars().count() as f32 * 6.5 + 8. <= wide;
        let full = clip.transition_kind.label();
        let word = match clip.transition_kind {
            engine::transitions::TransitionKind::DipBlack | engine::transitions::TransitionKind::DipWhite => "Dip",
            kind => kind.title(),
        };
        match (fits(&full), fits(word)) {
            (true, _) => Some(full),
            (false, true) => Some(word.to_string()),
            (false, false) => None,
        }
    };
    let transition_tip = |frames: u32| {
        format!(
            "{} \u{b7} {:.2} s",
            clip.transition_kind.title(),
            f64::from(frames) / player.fps
        )
    };
    let transition_region = dissolves.then(|| {
        let frames = clip.transition_out.min(clip.frames());
        let wide = scale.width_px(f64::from(frames) / player.fps).min(width);
        (wide, band_text(wide), transition_tip(frames))
    });
    // The incoming half of the join: the previous clip's overlap, drawn at this
    // clip's head. Only where the engine says the two really abut.
    let transition_in = prev
        .as_ref()
        .filter(|p| lane.kind == LaneKind::Video && p.transition_out > 0 && p.end() == clip.start)
        .map(|p| {
            let frames = p.transition_out.min(p.frames());
            let wide = scale.width_px(f64::from(frames) / player.fps).min(width);
            (wide, band_text(wide), transition_tip(frames))
        });
    let wave = source
        .and_then(|s| player.waves.get(&(s.path.clone(), s.audio_stream)))
        .cloned();
    // hook: §12 step 5 -- per-source film ink attaches here, replacing
    // `source_tint` (library_meta.rs's own placeholder wheel, index-keyed and
    // already different per source) with the source's own quantized hue.
    let ink = match compound {
        Some(_) => INK2(),
        None => source_tint(clip.source),
    };
    let multicam = player
        .session
        .as_ref()
        .is_some_and(|s| s.multicam_at(lane, idx).is_some());
    let image = source.is_some_and(|s| engine::is_image(&s.path));
    let kind = clip_kind(lane, compound.is_some(), multicam, image);
    let tone = clip_tone(kind);
    let on_strip = strip_label(tone);
    let strip_bg = strip_fill(tone);
    let strip_h = match audio {
        true => STRIP_H_AUDIO,
        false => STRIP_H_VIDEO,
    };
    let thumb = (!audio)
        .then(|| source.and_then(|s| player.thumbs.get(&s.path)))
        .flatten()
        .cloned();
    // The trace (thumbnail, waveform) needs room to read, the name plate more
    // still; below the trace floor the box is one solid ink fill, and the
    // 3px spine stays for as long as it is 3px wide at all (DESIGN §7).
    // A Small track (the floor, `LANE_MIN_H`) has no room for a thumbnail or a
    // waveform beside its name strip, so it keeps the name alone; every taller
    // track draws the trace it always has.
    let has_trace = t != Tier::Sliver && lane_height(player, lane) > LANE_MIN_H;
    let has_fill = t == Tier::Sliver;
    // With the waveforms switched off an audio clip is its colour, whole, with its
    // name on the strip; the trace is what the switch takes away.
    let flat_audio = audio && has_trace && !player.waveforms;
    let has_spine = span >= SPINE_MIN_W;
    let has_label = matches!(t, Tier::Full | Tier::NoChip);
    // The trace starts under the strip when there is one.
    let trace_top = if has_label { strip_h } else { 0. };
    // A compound's trace is drawn only where a clip's is: a Small track keeps the name alone.
    let compound_trace = (compound.is_some() && has_trace)
        .then(|| compound_trace(player, lane, clip, scale, trace_top));
    // An audio clip with keys or a clip gain shows its volume envelope over the
    // trace (display only). Built from the trimmed clip's own fields, so an
    // in-flight trim moves the keys with the box.
    // The line is drawn on a picked audio clip even with no keys, so a key can be
    // added to it; the keys shown are the ones a drag is holding, if one is.
    let picked = player.selected.contains((lane, idx));
    let keys = player
        .shown_gain_keys(lane, idx)
        .unwrap_or_else(|| clip.gain_keys.clone());
    let envelope = (audio
        && has_trace
        && clip.speed_keys.is_empty()
        && (picked || clip_envelope::has_envelope(&keys, clip.gain_db)))
    .then(|| {
        clip_envelope::envelope(
            &keys,
            clip.gain_db,
            clip.in_frame,
            clip.speed.as_f64(),
            (scale.pps / player.fps) as f32,
            width,
        )
    });
    // The envelope's own layer: over the line and its keys only. It takes the press
    // (adds, grabs, or takes out a key) once the pointer is on one, and is see-through
    // everywhere else, so the clip under it still selects and drags.
    let gain_layer = envelope.is_some().then(|| {
        let probe = Rc::new(Cell::new(Bounds::default()));
        let hot = player.gain_hot(lane, idx);
        let cursor = match player.gain_hover {
            Some((l, i, clip_envelope::EnvHit::Key(_))) if (l, i) == (lane, idx) => CursorStyle::PointingHand,
            Some((l, i, clip_envelope::EnvHit::Line(_))) if (l, i) == (lane, idx) => CursorStyle::ResizeUpDown,
            _ => CursorStyle::Arrow,
        };
        let inset = if trims(span) { EDGE_W } else { 0. };
        let (probe_move, probe_down) = (Rc::clone(&probe), Rc::clone(&probe));
        div()
            .id(("bench-clip-envelope", lane.ord * 1000 + idx))
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(inset))
            .right(px(inset))
            .child(bounds_probe(probe))
            .when(hot, |d| d.occlude().cursor(cursor))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.gain_hover_end(lane, idx, cx);
                }
            }))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                // While an envelope drag is held, this layer is what the pointer is over, so
                // the root (which hears no moves under an occluder) cannot carry it: the layer does.
                if this.gain_edit.is_some() {
                    match event.pressed_button {
                        Some(MouseButton::Left) => this.gain_move(event.position, cx),
                        _ => this.gain_release(cx),
                    }
                    return;
                }
                let b = probe_move.get();
                let local = point(event.position.x - b.origin.x, event.position.y - b.origin.y);
                this.gain_hover_at(lane, idx, local, b.size, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.gain_release(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let b = probe_down.get();
                    if this.gain_press_at(lane, idx, event, b, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
    });
    // The dB a dragged key reads, on its own plate beside the key (the trim plate's look).
    let gain_plate = player.gain_edit.as_ref().filter(|e| (e.lane, e.idx) == (lane, idx)).and_then(|e| match e.grab {
        crate::interact::GainGrab::Key(i) => {
            let key = *e.shown.get(i)?;
            let ppf = (scale.pps / player.fps) as f32;
            let x = clip_envelope::key_x(key.at, e.in_frame, e.speed, ppf);
            let y = clip_envelope::y_of(clip_envelope::height_of(e.clip_gain + key.db), f32::from(e.size.height));
            let db = e.clip_gain + key.db;
            // The plate is 24 px tall and sits 8 px clear of the key, above it when that
            // fits below the name strip, else below it. It never goes over the strip or
            // past the box's foot.
            const PLATE_H: f32 = 24.;
            const GAP: f32 = 8.;
            let box_h = f32::from(e.size.height);
            let floor = trace_top + 2.;
            let above = y - GAP - PLATE_H;
            let top = if above >= floor { above } else { y + GAP };
            let top = top.min(box_h - PLATE_H - 2.).max(floor);
            Some((x, top, format!("{db:.1} dB")))
        }
        crate::interact::GainGrab::Segment(_) => None,
    });
    // The clip a move is carrying is shown dimmed where it stands, and an edge
    // the pointer has pulled past the media's end flashes warn (`TrimFlash`).
    let placed_span = (placed.start, placed.frames());
    let dragging = player.drag_src == Some((lane, idx));
    // Nothing hovers or pops up while a hand is on the timeline: a trim, a roll,
    // slip or slide, or a move in flight.
    let busy = player.trim.is_some() || player.pro_drag.is_some() || cx.has_active_drag();
    let flash = |edge: Edge| player.trim_flash.is_some_and(|f| f.lit(lane, idx, edge));
    // The edge being dragged on this clip, for its plate (`trim_plate`).
    let dragged_edge = player
        .trim
        .filter(|trim| (trim.lane, trim.idx) == (lane, idx))
        .map(|trim| (trim, clip.frames()));
    // The box wears its speed -- `2×`, or the ramp's keys (`1×→2×`) -- so a ramp
    // reads off the timeline.
    let chip = (t == Tier::Full).then(|| speed_chip_text(clip)).flatten();
    let missing = source.is_some_and(|s| !engine::source_present(&s.path));
    let (in_frame, out_frame) = (
        f64::from(clip.in_frame) / player.fps,
        f64::from(clip.out_frame) / player.fps,
    );
    // The duration on the strip, `0:18` (never a trim delta: the plate shows that
    // while an edge is in the hand).
    let readout = mmss(len);
    // The duration chip is left off where the playhead runs through it (it would be cut by the
    // line), and the name takes what the strip leaves: whole, ellipsised, or just the dot.
    let playhead_x = scale.px_at(player.session.as_ref().map_or(0., PlaybackSession::now)) - left;
    let chip_w = readout.chars().count() as f32 * LABEL_CHAR_W + 6.;
    let chip_clear = !(playhead_x > width - 6. - chip_w - 8. && playhead_x < width + 4.);
    // Up to three 12px badges: speed, effects, link (only when picked), mute.
    let badges: Vec<(Icon, Option<String>)> = [
        (clip.speed.as_f64() != 1.0 || !clip.speed_keys.is_empty()).then(|| (Icon::Gauge, chip.clone())),
        (clip.eq.is_some() || clip.color.is_some() || clip.transform.is_some()).then_some((Icon::Sparkles, None)),
        (on && clip.link.is_some()).then_some((Icon::Link, None)),
        (audio && clip.gain_db <= -60.).then_some((Icon::VolumeX, None)),
    ]
    .into_iter()
    .flatten()
    .take(3)
    .collect();
    let group: SharedString = format!("clip-{}-{}-{}", lane.ord, lane.kind as usize, idx).into();
    // For the drag ghost and the fallback label both -- kept before `label`
    // is consumed by the name plate below.
    let ghost: SharedString = label.clone().unwrap_or_else(|| lane.label()).into();
    let show_chip = t == Tier::Full && chip_clear;
    // The name strip's own row before the name: the dot and its gap, and the
    // icon and gap a missing or compound clip puts there too.
    let budget = width
        - 3.
        - 12.
        - 12.
        - 12.
        - badges.len() as f32 * 18.
        - if show_chip { chip_w + 6. } else { 0. }
        - if missing { 18. } else { 0. }
        - if is_compound { 18. } else { 0. };
    let full_name = label.clone();
    let label = label.and_then(|l| fit_label(cx, &l, budget));
    // The name in a tooltip when the strip could not hold it whole, and the reason when the
    // file is gone.
    let cut = has_label && full_name.as_ref().is_some_and(|n| label.as_deref() != Some(n.as_str()));
    let clip_tip = ((cut || missing) && !busy).then(|| {
        let name = full_name.clone().unwrap_or_else(|| lane.label());
        crate::ui::widgets::tip_hover(
            &name,
            "",
            missing.then_some("Media missing — relink it in the Media tab"),
        )
    });
    // The room a trim zone may spill into on each side: up to the middle of
    // the gap to the neighbour there (`interact::trim_zone`), unbounded at
    // the end of a lane. Where two clips meet, each keeps its own half.
    let room_before = prev.map_or(f32::INFINITY, |p| {
        scale.width_px(f64::from(clip.start.saturating_sub(p.end())) / player.fps)
    });
    let room_after = next.map_or(f32::INFINITY, |n| {
        scale.width_px(f64::from(n.start.saturating_sub(clip.end())) / player.fps)
    });
    let body = div()
        .id((
            "bench-clip",
            lane.ord * 1000 + lane.kind as usize * 100 + idx,
        ))
        .group(group.clone())
        .absolute()
        .top(px(CLIP_INSET))
        .bottom(px(CLIP_INSET))
        .left_0()
        // 1px between neighbours: the splice gap.
        .w(px((width - 1.).max(1.)))
        .overflow_hidden()
        .rounded(px(crate::layout::redesign::R_CLIP))
        // A picked clip's body is 8% lighter; its outline is drawn around the
        // box (below), so selecting moves nothing.
        .bg(rgb(match on {
            true => theme::mix(tone.body, 0xffffff, 0.08),
            false => tone.body,
        }))
        .when(dragging, |d| d.opacity(0.85))
        .cursor_pointer()
        .when_some(clip_tip, |d, tip| d.tooltip(tip))
        // The status bar's drag hint: the body under the pointer, and the
        // modifiers as they change over it (`drag_status`).
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            set_hover(
                this,
                lane,
                idx,
                HoverZone::Body,
                *hovered,
                window.modifiers(),
                cx,
            );
        }))
        .on_modifiers_changed(cx.listener(
            move |this, event: &gpui::ModifiersChangedEvent, _, cx| {
                hover_modifiers(this, lane, idx, HoverZone::Body, event.modifiers, cx);
            },
        ))
        // Selection: reuses `Player::pick`, the same call the legacy
        // timeline's clip box makes.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                // Shift-drag: slip, or slide with alt ([`Player::start_pro`]).
                if event.modifiers.shift {
                    let kind = match event.modifiers.alt {
                        true => ProKind::Slide,
                        false => ProKind::Slip,
                    };
                    this.start_pro(lane, idx, kind, None, event.position.x, cx);
                    return;
                }
                // A press on either half of a take takes the whole take (Alt: this half alone).
                this.press_take((lane, idx), event.modifiers.control, event.modifiers.alt, cx);
            }),
        )
        // A plain click (press and release with no drag between -- gpui
        // drops the pending click once a drag starts, see `on_drag` below)
        // collapses a multi-pick down to the one clicked, the same way
        // `select` does. `pick` above deliberately keeps the whole set on
        // the press so a set-drag has something to move; this is where the
        // collapse a plain click promises actually lands.
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if !press_modifiers(event).control {
                this.select_take((lane, idx), press_modifiers(event).alt, cx);
                // A double-click on a compound clip opens it; on any other clip it does nothing more.
                if event.click_count() >= 2 {
                    this.open_compound(lane, idx, cx);
                }
            }
        }))
        // The right button selects exactly as the left one does -- the menu
        // acts on the clip it names -- then hangs the clip menu at the
        // pointer (DESIGN §9's "verbs of the thing under the cursor"). Same
        // call and same surface the legacy timeline's clip box opens
        // (`ui/timeline.rs`'s `Player::open_menu`; the card itself is
        // `context_card`, already mounted at `stance.rs:695`).
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.open_menu(lane, idx, event.position, cx);
            }),
        )
        // Dragged, it moves: to the frame and the lane it is let go over
        // (`Player::move_clip`, `ClipDrag`) -- the same payload and the same
        // ghost tooltip `ui/timeline.rs`'s own clip box drags.
        .on_drag(
            ClipDrag {
                lane,
                idx,
                clip: placed,
            },
            {
                let ghost = ghost.clone();
                let me = cx.entity();
                move |_, _, _, cx| {
                    // A fresh gesture inherits nothing: `Player::drag_lane`
                    // and `Player::drag_x` are the *last live sample* of the
                    // drag being made, and a drag whose first sample lands
                    // outside every row would otherwise be let go onto the
                    // lane the previous one ended over.
                    me.update(cx, |this, cx| {
                        // A slip, slide or roll shares the press with the move: it has the
                        // gesture, so the move leaves no shadow and takes no drop.
                        if this.pro_drag.is_some() {
                            return;
                        }
                        // Whatever the last gesture left up (a shadow, a
                        // snap line) goes before this one's source is set.
                        this.end_drag_preview(cx);
                        this.drag_lane = None;
                        this.drag_x = None;
                        this.drag_src = Some((lane, idx));
                    });
                    cx.new(|_| Tip(ghost.clone()))
                }
            },
        )
        .children(hitmap::dynamic(
            move || {
                (
                    format!("clip.{}.{}.{}", lane.label(), lane.ord, idx),
                    "Timeline clip".into(),
                )
            },
            true,
        ))
        // Below the trace floor the box is its source's ink, whole.
        .when(has_fill || flat_audio, |d| {
            d.child(div().absolute().top_0().left_0().size_full().bg(rgb(tone.strip)))
        })
        // Ink spine: 3px left edge, the source's own ink (hook above).
        .when(has_spine, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .h_full()
                    .w(px(3.))
                    .bg(rgb(ink)),
            )
        })
        // Trace: real waveform for audio, a real decoded thumbnail for video
        // (falling back to a flat placeholder body while it loads or if the
        // file never yields one).
        .when(has_trace && audio && player.waveforms, |d| {
            d.children(wave.and_then(|w| {
                if super::lane::wave_is_silent(&w) {
                    return Some(super::lane::silent_marker(tone.ink).into_any_element());
                }
                match w {
                    Wave::Peaks(peaks) => Some(
                        div()
                            .absolute()
                            .left(px(3.))
                            .right_0()
                            .top(px(trace_top))
                            .bottom_0()
                            .child(waveform_ink(peaks, in_frame, out_frame, tone.ink))
                            // 1px centre line, white 30%.
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top_1_2()
                                    .h(px(1.))
                                    .bg(rgba(theme::with_alpha(0xffffff, 0x4d))),
                            )
                            .into_any_element(),
                    ),
                    _ => None,
                }
            }))
        })
        // The volume envelope over the trace: an audio clip with keys or a gain.
        .when_some(envelope, |d, env| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .child(clip_envelope::overlay(env)),
            )
        })
        .when_some(gain_layer, |d, layer| d.child(layer))
        .when_some(gain_plate, |d, (x, top, text)| {
            d.child(
                div()
                    .absolute()
                    .left(px((x + 8.).max(2.)))
                    .top(px(top))
                    .px(px(8.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(crate::layout::redesign::R_CARD))
                    .bg(rgb(tokens().e3))
                    .border_1()
                    .border_color(rgba(tokens().line_hi))
                    .whitespace_nowrap()
                    .type_style(type_scale::caption_strong())
                    .text_color(rgb(tokens().ink1))
                    .child(text),
            )
        })
        // A compound clip draws like any clip, from the clips it stands for: their film
        // thumbnails on a picture lane, their sound on an audio lane. What sets it apart is a
        // double edge along the top of the trace, the look of a stack.
        .when_some(compound_trace, |d, trace| {
            d.child(trace).child(
                div()
                    .absolute()
                    .left(px(3.))
                    .right_0()
                    .top(px(trace_top))
                    .h(px(4.))
                    .border_t_1()
                    .border_b_1()
                    .border_color(rgb(INK2())),
            )
        })
        .when_some(generated.filter(|_| has_trace), |d, g| d.child(generated_face(g)))
        .when(has_trace && !audio && compound.is_none() && generated.is_none(), |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(3.))
                    .right_0()
                    .top(px(trace_top))
                    .bottom_0()
                    .overflow_hidden()
                    .bg(rgb(tone.body))
                    .when_some(thumb, |d, thumb| match thumb {
                        Thumb::Ready(image) => d.child(film_image(image)),
                        _ => d,
                    }),
            )
        })
        // Fades shade their own ramps over every clip, including the live
        // `FadeDrag` value, so the ramp follows the hand before release.
        .when(fade_in > 0 || fade_out > 0, |d| {
            d.children(
                [
                    (fade_in > 0).then(|| {
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(px(scale
                                .width_px(f64::from(fade_in) / player.fps)
                                .min(width)))
                            .child(fade_wedge(true))
                    }),
                    (fade_out > 0).then(|| {
                        div()
                            .absolute()
                            .right_0()
                            .top_0()
                            .h_full()
                            .w(px(scale
                                .width_px(f64::from(fade_out) / player.fps)
                                .min(width)))
                            .child(fade_wedge(false))
                    }),
                ]
                .into_iter()
                .flatten(),
            )
        })
        // The clip's colour label washes its body; a switched-off clip is dimmed.
        .when(mark.label > 0, |d| {
            let [r, g, b] = engine::project::LABELS[usize::from(mark.label - 1)].1;
            let c = (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | 0x66;
            d.child(div().absolute().top_0().left_0().size_full().bg(rgba(c)))
        })
        .when(mark.off, |d| {
            d.child(div().absolute().top_0().left_0().size_full().bg(rgba((DARK_PANEL() << 8) | 0xaa)))
        })
        // The name strip: the kind's colour at 90% with the film-ink dot, the name
        // (Inter caption, middle of the strip), badges and the duration on the right.
        .when(has_label, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(3.))
                    .right_0()
                    .top_0()
                    .h(px(strip_h))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .overflow_hidden()
                    .bg(rgba(theme::with_alpha(strip_bg, 0xe6)))
                    .group_hover(group.clone(), |s| s.bg(rgba(theme::with_alpha(theme::mix(tone.strip, 0xffffff, 0.06), 0xe6))))
                    .type_style(type_scale::caption_strong())
                    .text_color(rgba(theme::with_alpha(on_strip, 0xeb)))
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(6.))
                                    .rounded(px(3.))
                                    .bg(rgb(ink)),
                            )
                            .when(missing, |d| {
                                d.child(div().flex_none().child(Icon::TriangleAlert.el_px(px(12.), theme_warn())))
                            })
                            .when(is_compound, |d| {
                                d.child(div().flex_none().child(Icon::Layers.el_px(px(12.), theme::with_alpha(on_strip, 0xeb))))
                            })
                            .when_some(label, |d, label| {
                                d.child(div().min_w(px(0.)).truncate().child(label))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .children(badges.into_iter().map(|(icon, text)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .child(icon.el_px(px(12.), on_strip))
                                    .children(text)
                            }))
                            .when(show_chip, |d| {
                                d.child(
                                    div()
                                        .text_color(rgba(theme::with_alpha(on_strip, 0xb3)))
                                        .child(readout),
                                )
                            }),
                    ),
            )
        })
        // The transition's bands: the tail the join overlaps on this clip, and
        // the head of the next one across the cut.
        .when_some(transition_region, |d, (wide, text, tip)| {
            d.child(transition_band(("bench-transition-out", lane.ord * 1000 + idx), wide, text, tip, true))
        })
        .when_some(transition_in, |d, (wide, text, tip)| {
            d.child(transition_band(("bench-transition-in", lane.ord * 1000 + idx), wide, text, tip, false))
        })
        // A selected clip's two trim handles, drawn inside its edges
        // (DESIGN §4.3). The hit zones around them are the trim strips
        // laid over the box below. An edge pinned at the media's end flashes.
        .when(on && trims(span), |d| {
            // 8x20 pills, white 85%, inside both edges.
            let pill = |edge: Edge| {
                div()
                    .w(px(8.))
                    .h(px(20.))
                    .rounded(px(2.))
                    .bg(match flash(edge) {
                        true => rgb(STATUS_WARNING()),
                        false => rgba(theme::with_alpha(0xffffff, 0xd9)),
                    })
            };
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(3.))
                    .flex()
                    .items_center()
                    .child(pill(Edge::Start)),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(1.))
                    .flex()
                    .items_center()
                    .child(pill(Edge::End)),
            )
        })
        // A clip whose file is gone: an amber stripe along its foot, on every width.
        .when(missing, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(3.))
                    .bg(rgb(theme_warn())),
            )
            .child(div().absolute().top_0().left_0().size_full().bg(rgba(theme::with_alpha(theme_warn(), 0x1f))))
        })
        // The 1px inner border, black 45%, over the thumbnails and strip.
        .child(
            div()
                .absolute()
                .size_full()
                .rounded(px(crate::layout::redesign::R_CLIP))
                .border_1()
                .border_color(rgba(theme::with_alpha(0x000000, 0x73))),
        );
    // Zones and handles are siblings of the box, not children: the box clips
    // its contents to its own edges, and a 24 px zone must reach past them.
    div()
        .absolute()
        .top_0()
        .h_full()
        .left(px(left))
        .w(px(width))
        // What an edge trim has taken off, left as a dim ghost of the clip where
        // it stood; the box (opaque, drawn after) covers the part still kept.
        .when(placed_span != (clip.start, clip.frames()), |d| {
            let at = |frame: u32| scale.px_at(f64::from(frame) / player.fps) - left;
            d.child(
                div()
                    .absolute()
                    .top(px(CLIP_INSET))
                    .bottom(px(CLIP_INSET))
                    .left(px(at(placed_span.0)))
                    .w(px(scale.width_px(f64::from(placed_span.1) / player.fps).max(1.)))
                    .rounded(px(crate::layout::redesign::R_CLIP))
                    .bg(rgba(theme::with_alpha(tone.body, 0x40)))
                    .border_1()
                    .border_color(rgba(theme::with_alpha(tone.ink, 0x59))),
            )
        })
        .child(body)
        // The picked clip's 2px accent outline, outside the box: an overlay, so
        // the clip's own geometry never changes with selection.
        .when(on, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(CLIP_INSET - 2.))
                    .bottom(px(CLIP_INSET - 2.))
                    .left(px(-2.))
                    .w(px((width - 1.).max(1.) + 4.))
                    .rounded(px(crate::layout::redesign::R_CLIP + 2.))
                    .border_2()
                    .border_color(rgb(tokens().accent)),
            )
        })
        // The two edge strips a drag lengthens or shortens the clip by
        // (`Player::start_trim`), gated on the same `trims(span)` floor the
        // legacy strips use -- a clip too narrow to aim at keeps its whole
        // padded box as a body to select and drag by instead (the `[`/`]`
        // chords still trim it either way).
        .children(
            [Edge::Start, Edge::End]
                .into_iter()
                .filter(|_| trims(span))
                .map(|edge| {
                    let (inside, outside) = trim_zone(
                        span,
                        match edge {
                            Edge::Start => room_before,
                            Edge::End => room_after,
                        },
                    );
                    let mut zone = div()
                        // An id only so the strip can carry a hover plate --
                        // gpui's `.tooltip` lives on a stateful div.
                        .id((
                            "bench-clip-trim",
                            (lane.ord * 1000 + idx) * 2 + usize::from(edge == Edge::End),
                        ))
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(px(inside + outside))
                        .occlude()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .when(!busy, |z| z.hover(|s| s.bg(rgba((INK1() << 8) | 0x4d))))
                        // The status bar's drag hint for an edge (`drag_status`).
                        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                            set_hover(
                                this,
                                lane,
                                idx,
                                HoverZone::Edge,
                                *hovered,
                                window.modifiers(),
                                cx,
                            );
                        }))
                        .on_modifiers_changed(cx.listener(
                            move |this, event: &gpui::ModifiersChangedEvent, _, cx| {
                                hover_modifiers(
                                    this,
                                    lane,
                                    idx,
                                    HoverZone::Edge,
                                    event.modifiers,
                                    cx,
                                );
                            },
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                // Shift-drag on an edge rolls the cut there.
                                if event.modifiers.shift {
                                    this.start_pro(
                                        lane,
                                        idx,
                                        ProKind::Roll,
                                        Some(edge),
                                        event.position.x,
                                        cx,
                                    );
                                    return;
                                }
                                this.start_trim(lane, idx, edge, event.modifiers.control, cx);
                            }),
                        )
                        // Occluded, so the box's own right-button listener
                        // never fires here: the same menu, opened by the same
                        // call, exactly as the legacy clip's edge strip does.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.open_menu(lane, idx, event.position, cx);
                            }),
                        )
                        .when(!busy, |z| {
                            z.tooltip(crate::ui::widgets::tip_hover("Trim clip edge", "", Some("Drag to trim the clip's edge")))
                        })
                        .children(hitmap::dynamic(
                            move || {
                                (
                                    format!(
                                        "clip.{}.{}.{}.trim-{}",
                                        lane.label(),
                                        lane.ord,
                                        idx,
                                        if edge == Edge::Start { "l" } else { "r" }
                                    ),
                                    format!(
                                        "Trim clip {} edge",
                                        if edge == Edge::Start { "left" } else { "right" }
                                    ),
                                )
                            },
                            true,
                        ));
                    zone = match edge {
                        Edge::Start => zone.left(px(-outside)),
                        Edge::End => zone.right(px(-outside)),
                    };
                    zone
                }),
        )
        // The plate of the edge in the hand: the delta it has moved and what
        // the clip's length went from and to (`trim_plate`).
        .when_some(dragged_edge, |d, (trim, new)| {
            d.child(trim_plate(trim, new, player.fps))
        })
        // The dissolve is the join's click target: it remains visible only
        // where the engine says two video clips really abut, and a click
        // selects that leading clip then takes the same toggle as Ctrl+X.
        .when(dissolves, |d| {
            d.child(
                div()
                    .id(("bench-dissolve", lane.ord * 1000 + idx))
                    .absolute()
                    .right_0()
                    .top_0()
                    .w(px(HIT_MIN))
                    .h(px(HIT_MIN))
                    .cursor_pointer()
                    .tooltip(move |_, cx| cx.new(|_| Tip(dissolve_tip.clone())).into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick((lane, idx), false, cx);
                        this.dissolve_selected(cx);
                    }))
                    .children(hitmap::dynamic(
                        move || {
                            (
                                format!("clip.{}.{}.{}.dissolve", lane.label(), lane.ord, idx),
                                "Toggle dissolve".into(),
                            )
                        },
                        true,
                    ))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .w(px(FADE_HANDLE_W))
                            .h(px(FADE_HANDLE_H))
                            .child(dissolve_glyph()),
                    ),
            )
        })
        // The fade handles sit inside the trim strips, so their small top
        // corners change a ramp while the outer full-height strips still trim.
        .when(trims(span), |d| {
            d.children([Edge::Start, Edge::End].into_iter().map(|edge| {
                let is_in = edge == Edge::Start;
                let mut handle = div()
                    .id((
                        "bench-clip-fade",
                        (lane.ord * 1000 + idx) * 2 + usize::from(!is_in),
                    ))
                    .absolute()
                    .top_0()
                    .w(px(HIT_MIN))
                    .h(px(HIT_MIN))
                    .occlude()
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.start_fade_drag(lane, idx, is_in, event.position.x, cx);
                        }),
                    )
                    .when(!busy, |h| {
                        h.tooltip({
                            let tip = crate::ui::widgets::tip_hover("Fade handle", "", Some("Drag to change the fade length"));
                            move |window, cx| {
                                let inner = tip(window, cx);
                                cx.new(|_| AboveTip(inner)).into()
                            }
                        })
                    })
                    .children(hitmap::dynamic(
                        move || {
                            (
                                format!(
                                    "clip.{}.{}.{}.fade-{}",
                                    lane.label(),
                                    lane.ord,
                                    idx,
                                    if is_in { "in" } else { "out" }
                                ),
                                format!("Adjust fade {}", if is_in { "in" } else { "out" }),
                            )
                        },
                        true,
                    ));
                // The visible 10 px dot sits in the corner of its 24 px hit area.
                handle = handle.child(
                    div()
                        .id((
                            "bench-clip-fade-dot",
                            (lane.ord * 1000 + idx) * 2 + usize::from(!is_in),
                        ))
                        .absolute()
                        .top_0()
                        .when(is_in, |d| d.left_0())
                        .when(!is_in, |d| d.right_0())
                        .w(px(FADE_HANDLE_W))
                        .h(px(FADE_HANDLE_H))
                        .hover(|s| s.bg(rgb(INK2()))),
                );
                handle = match edge {
                    Edge::Start => handle.left_0(),
                    Edge::End => handle.right_0(),
                };
                handle
            }))
        })
}

/// A signed distance in plain seconds, `−1.6 s`: the one way every drag readout
/// (the plate on the edge, the status bar) writes how far the hand has gone.
/// The minus is the real one; a zero distance has no sign.
pub(super) fn delta_text(delta: i64, fps: f64) -> String {
    let secs = delta.unsigned_abs() as f64 / fps.max(1.);
    let sign = match delta.signum() {
        -1 => "\u{2212}",
        1 => "+",
        _ => "",
    };
    format!("{sign}{secs:.1} s")
}

/// What an edge the hand has moved reads, plate and status bar alike:
/// `−1.6 s · 12.4 s long`, the distance and the length the clip now has.
pub(super) fn trim_plate_text(delta: i64, new_frames: u32, fps: f64) -> String {
    format!(
        "{} \u{00b7} {:.1} s long",
        delta_text(delta, fps),
        f64::from(new_frames) / fps.max(1.)
    )
}

/// The plate a dragged clip edge carries: the [`trim_plate_text`] line in mono
/// ink1 on the panel's plate, hung at the edge the hand has by the bottom
/// corner of the clip, so it reads as belonging to the edge.
pub(super) fn trim_plate(trim: Trim, new: u32, fps: f64) -> impl IntoElement {
    let delta = i64::from(trim.to) - i64::from(trim.from);
    let plate = div()
        .absolute()
        .bottom(px(4.))
        .px(px(8.))
        .h(px(24.))
        .flex()
        .items_center()
        .rounded(px(crate::layout::redesign::R_CARD))
        .bg(rgb(tokens().e3))
        .border_1()
        .border_color(rgba(tokens().line_hi))
        .whitespace_nowrap()
        .type_style(type_scale::caption_strong())
        .text_color(rgb(tokens().ink1))
        .child(trim_plate_text(delta, new, fps));
    match trim.edge {
        Edge::Start => plate.left(px(3.)),
        Edge::End => plate.right(px(3.)),
    }
}

/// Which part of a clip the pointer is resting on, for the status bar's drag
/// hint ([`hover_hint_text`]): the body (move, slip, slide) or an edge strip
/// (trim, roll).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HoverZone {
    Body,
    Edge,
}

/// The clip under the pointer and the modifiers held over it, kept on the
/// root ([`Player::hover_hint`]) so the status bar can say what a drag there
/// would do. The clip is named so the strip that lost the pointer clears only
/// its own hint, not the one the strip it moved onto has just set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DragHover {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    pub(crate) zone: HoverZone,
    pub(crate) shift: bool,
    pub(crate) alt: bool,
}

/// The status bar's line while the pointer rests on a clip: what a drag does
/// with the modifiers held *now*, and what the other modifiers would make of
/// it. Plain words, no jargon beyond the three trim names the sheet also
/// uses, and the same line for every clip so the geography never moves.
pub(crate) fn hover_hint_text(zone: HoverZone, shift: bool, alt: bool) -> &'static str {
    match (zone, shift, alt) {
        (HoverZone::Edge, false, false) => {
            "Drag edge: trim \u{00b7} Shift: trim both sides"
        }
        (HoverZone::Edge, true, _) => {
            "Shift-drag edge: trim both sides, both clips change"
        }
        (HoverZone::Edge, false, true) => "Alt-drag edge: trim with snapping off",
        (HoverZone::Body, false, false) => {
            "Drag: move \u{00b7} Shift-drag: change which part of the clip plays"
        }
        (HoverZone::Body, true, false) => {
            "Shift-drag: change which part of the clip plays \u{00b7} add Alt to slide"
        }
        (HoverZone::Body, true, true) => {
            "Shift+Alt-drag: slide, neighbours give and take"
        }
        (HoverZone::Body, false, true) => "Alt-drag: move with snapping off",
    }
}

/// The status bar's line while a trim, roll, slip or slide is in flight:
/// `Rolling cut · +1.6 s`, and for a trim the length it leaves, the same
/// words the plate on the edge uses ([`trim_plate_text`]).
pub(crate) fn drag_progress_text(doing: &str, by: i64, fps: f64) -> String {
    format!("{doing} \u{00b7} {}", delta_text(by, fps))
}

/// The status bar's line while a library item is in the hand over the bed: the lane it would
/// land on and how. A clip in the hand has its own drag and says nothing here.
pub(crate) fn asset_drop_status(player: &Player) -> Option<String> {
    if player.drag_src.is_some() {
        return None;
    }
    // Below the last lane there is no row to aim at: the drop makes a track.
    if player.drag_below {
        return Some("Drop to add a new track".to_string());
    }
    let lane = player.drag_lane?;
    if player.ghost.is_empty() {
        return None;
    }
    if player.drag_ctrl {
        return Some("Insert: later clips move right".to_string());
    }
    Some(format!(
        "Drop on {}: replaces what is under it \u{00b7} Ctrl: insert and push the rest along",
        lane.label()
    ))
}

/// What the status bar says about the pointer and the timeline: the drag in
/// flight and how far it has gone, else the hint for the clip under the
/// pointer, else nothing. A hover left behind by a clip that is gone (deleted
/// under a still pointer) says nothing either.
pub(crate) fn drag_status(player: &Player) -> Option<String> {
    if let Some(line) = trim_in_flight(player) {
        return Some(line);
    }
    if let Some(line) = asset_drop_status(player) {
        return Some(line);
    }
    let hover = player.hover_hint?;
    player
        .session
        .as_ref()?
        .lane_clips(hover.lane)
        .get(hover.idx)?;
    Some(hover_hint_text(hover.zone, hover.shift, hover.alt).to_string())
}

/// The line for a trim, roll, slip or slide in the hand: `Trimming both sides · +1.6 s`,
/// `Slipping the footage · −0.4 s`, `Sliding the clip · +1.2 s`. `None` when nothing is in hand.
pub(crate) fn trim_in_flight(player: &Player) -> Option<String> {
    if let Some(drag) = player.pro_drag {
        let doing = match drag.kind {
            ProKind::Roll => "Trimming both sides",
            ProKind::Slip => "Slipping the footage",
            ProKind::Slide => "Sliding the clip",
        };
        return Some(drag_progress_text(doing, drag.by, player.fps));
    }
    if let Some(trim) = player.trim {
        let doing = match trim.edge {
            Edge::Start => "Trimming the start",
            Edge::End => "Trimming the end",
        };
        let delta = i64::from(trim.to) - i64::from(trim.from);
        let shown = player
            .session
            .as_ref()
            .and_then(|s| s.lane_clips(trim.lane).get(trim.idx).cloned())
            .map(|c| player.trimmed(trim.lane, trim.idx, c).frames());
        return Some(match shown {
            Some(len) => format!("{doing} \u{00b7} {}", trim_plate_text(delta, len, player.fps)),
            None => drag_progress_text(doing, delta, player.fps),
        });
    }
    None
}

/// A strip gained or lost the pointer: records the hover with the modifiers
/// held, or clears it -- only if the hover is still this strip's own, since
/// the strip the pointer moved onto has set its own in the same frame.
fn set_hover(
    player: &mut Player,
    lane: Lane,
    idx: usize,
    zone: HoverZone,
    on: bool,
    modifiers: gpui::Modifiers,
    cx: &mut Context<Player>,
) {
    let mine = |h: &DragHover| (h.lane, h.idx, h.zone) == (lane, idx, zone);
    let next = match on {
        true => Some(DragHover {
            lane,
            idx,
            zone,
            shift: modifiers.shift,
            alt: modifiers.alt,
        }),
        false => player.hover_hint.filter(|h| !mine(h)),
    };
    if next != player.hover_hint {
        player.hover_hint = next;
        cx.notify();
    }
}

/// A modifier went down or up while the pointer rests on a strip: the hint
/// follows, so pressing shift over an edge reads "roll" before the drag.
fn hover_modifiers(
    player: &mut Player,
    lane: Lane,
    idx: usize,
    zone: HoverZone,
    modifiers: gpui::Modifiers,
    cx: &mut Context<Player>,
) {
    if player
        .hover_hint
        .is_some_and(|h| (h.lane, h.idx, h.zone) == (lane, idx, zone))
    {
        set_hover(player, lane, idx, zone, true, modifiers, cx);
    }
}

/// The inlined clips of a compound clip, laid across its box at their own places: the film
/// thumbnails of its picture, the waveforms of its sound.
fn compound_trace(player: &Player, lane: Lane, clip: &Clip, scale: Scale, top: f32) -> impl IntoElement + use<> {
    let audio = lane.kind == LaneKind::Audio;
    let pieces = player
        .session
        .as_ref()
        .map_or_else(Vec::new, |s| s.compound_pieces(lane.kind, clip));
    let body = pieces.iter().filter_map(|p| {
        let src = player.sources().get(p.source)?;
        let at = scale.width_px(f64::from(p.start) / player.fps);
        let wide = scale.width_px(f64::from(p.frames()) / player.fps);
        let cell = div().absolute().left(px(at)).top_0().h_full().w(px(wide)).overflow_hidden();
        if audio {
            let peaks = match player.waves.get(&(src.path.clone(), src.audio_stream))? {
                Wave::Peaks(peaks) => peaks.clone(),
                _ => return None,
            };
            let (from, to) = (f64::from(p.in_frame) / player.fps, f64::from(p.out_frame) / player.fps);
            Some(cell.child(waveform_ink(peaks, from, to, clip_tone(ClipKind::Compound).ink)))
        } else {
            let cell = cell.bg(rgb(clip_tone(ClipKind::Compound).body));
            Some(match player.thumbs.get(&src.path) {
                // The same film strip as a plain clip's: frames at the strip's height, whole.
                Some(Thumb::Ready(image)) => cell.child(film_image(image.clone())),
                _ => cell,
            })
        }
    });
    div()
        .absolute()
        .left(px(3.))
        .right_0()
        .top(px(top))
        .bottom_0()
        .overflow_hidden()
        .children(body)
}

/// What a generated clip looks like in its box: its colour, a gradient, bars, the number, or (for
/// the adjustment layer) a flat grey of its own.
fn generated_face(g: engine::generate::Gen) -> impl IntoElement {
    use engine::generate::Kind;
    let fill = div().absolute().left(px(3.)).right_0().top(px(2.)).bottom(px(2.)).overflow_hidden();
    match g.kind {
        Kind::Solid { rgb: c } => fill.bg(rgb(c)).into_any_element(),
        Kind::Gradient { a, b, angle } => fill
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        window.paint_quad(gpui::fill(
                            bounds,
                            gpui::linear_gradient(
                                90. + f32::from(angle),
                                gpui::linear_color_stop(rgb(a), 0.),
                                gpui::linear_color_stop(rgb(b), 1.),
                            ),
                        ));
                    },
                )
                .size_full(),
            )
            .into_any_element(),
        Kind::Bars => fill
            .flex()
            .children([0xbfbfbf, 0xbfbf00, 0x00bfbf, 0x00bf00, 0xbf00bf, 0xbf0000, 0x0000bf].map(|c| div().flex_1().h_full().bg(rgb(c))))
            .into_any_element(),
        Kind::Countdown { from, .. } => fill
            .bg(rgb(DARK_PANEL()))
            .flex()
            .items_center()
            .justify_center()
            .type_style(type_scale::label_tabular(type_scale::CHORD_METADATA_MIN_PX, gpui::FontWeight::BOLD))
            .text_color(rgb(INK1()))
            .child(from.to_string())
            .into_any_element(),
        Kind::Adjust => fill.bg(rgb(INK3())).into_any_element(),
        // A shape on its clip: a small mark of itself on the strip, in its own paint.
        Kind::Shape(sh) => {
            use engine::generate::Form;
            let ink = sh.fill.filter(|_| sh.form.fills()).or(sh.stroke).unwrap_or(0xffffff);
            let line = sh.stroke.unwrap_or(ink);
            let mark = div().flex_none();
            let mark = match sh.form {
                Form::Rectangle => mark.w(px(18.)).h(px(11.)).rounded(px(if sh.radius > 0 { 4. } else { 1. })).bg(rgb(ink)),
                Form::Ellipse => mark.w(px(18.)).h(px(11.)).rounded_full().bg(rgb(ink)),
                Form::Line => mark.w(px(22.)).h(px(3.)).rounded_full().bg(rgb(line)),
                Form::Arrow => mark.w(px(22.)).h(px(3.)).rounded_full().bg(rgb(line)),
            };
            fill.flex().items_center().justify_center().child(mark).into_any_element()
        }
    }
}

/// One transition band over a join: a tint of the accent across the full height
/// of the clip, on its tail (`tail`) or its head, with its label when it fits and
/// its length on hover.
fn transition_band(
    id: (&'static str, usize),
    wide: f32,
    text: Option<String>,
    tip: String,
    tail: bool,
) -> impl IntoElement {
    let t = tokens();
    let tip: SharedString = tip.into();
    div()
        .id(id)
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(wide))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .when(tail, |d| d.right_0().border_l_2())
        .when(!tail, |d| d.left_0().border_r_2())
        .bg(rgba(theme::with_alpha(t.accent, 0x7a)))
        .border_color(rgba(t.accent))
        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
        .when_some(text, |r, text| {
            r.type_style(type_scale::caption())
                .text_color(rgb(t.ink1))
                .child(text)
        })
}

#[cfg(test)]
mod tests {
    use super::{HoverZone, clip_kind, drag_progress_text, hover_hint_text, trim_plate_text};
    use crate::ui::theme::ClipKind;
    use crate::{Lane, LaneKind};

    /// One function names a clip's kind, and so its colours: audio lanes green,
    /// compound and multicam win over the lane, a still is an image.
    #[test]
    fn a_clips_kind_comes_from_one_door() {
        assert_eq!(clip_kind(Lane::V1, false, false, false), ClipKind::Video);
        assert_eq!(clip_kind(Lane::V1, false, false, true), ClipKind::Image);
        assert_eq!(clip_kind(Lane::A1, false, false, false), ClipKind::Audio);
        assert_eq!(clip_kind(Lane::V1, true, true, false), ClipKind::Compound);
        assert_eq!(clip_kind(Lane::V1, false, true, false), ClipKind::Multicam);
        assert_eq!(clip_kind(Lane::new(LaneKind::Subtitle, 0), false, false, false), ClipKind::Caption);
    }

    /// Every zone and modifier state has its own line, and each one names
    /// the trim it makes in the sheet's words.
    #[test]
    fn the_hover_hint_names_what_each_drag_does() {
        let edge = hover_hint_text(HoverZone::Edge, false, false);
        assert!(edge.starts_with("Drag edge: trim"), "{edge}");
        assert!(edge.contains("Shift: trim both sides"), "{edge}");
        assert!(hover_hint_text(HoverZone::Edge, true, false).contains("trim both sides"));
        assert!(hover_hint_text(HoverZone::Edge, true, true).contains("trim both sides"));
        assert!(hover_hint_text(HoverZone::Edge, false, true).contains("snapping off"));
        let body = hover_hint_text(HoverZone::Body, false, false);
        assert!(body.starts_with("Drag: move"), "{body}");
        assert!(body.contains("Shift-drag: change which part of the clip plays"), "{body}");
        assert!(hover_hint_text(HoverZone::Body, true, false).contains("change which part of the clip plays"));
        assert!(hover_hint_text(HoverZone::Body, true, true).contains("slide"));
        assert!(hover_hint_text(HoverZone::Body, false, true).contains("snapping off"));
        // No jargon: the lines say what happens, never the engine's names.
        for zone in [HoverZone::Edge, HoverZone::Body] {
            for (shift, alt) in [(false, false), (true, false), (false, true), (true, true)] {
                let line = hover_hint_text(zone, shift, alt);
                assert!(
                    !line.contains("ripple") && !line.contains("ProKind"),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn the_drag_progress_and_the_plate_share_one_format() {
        assert_eq!(drag_progress_text("Rolling the cut", 30, 25.), "Rolling the cut \u{00b7} +1.2 s");
        assert_eq!(drag_progress_text("Slipping the footage", -3, 25.), "Slipping the footage \u{00b7} \u{2212}0.1 s");
        assert_eq!(drag_progress_text("Sliding the clip", 0, 25.), "Sliding the clip \u{00b7} 0.0 s");
    }

    /// The `?` sheet lists the three pointer trims and the plain edge drag
    /// under Editing, in plain words, with the keys that make them.
    #[test]
    fn the_shortcuts_sheet_says_how_to_roll_slip_and_slide() {
        use crate::keymap::{Category, FIXED, Reach};
        for (chord, word) in [
            ("drag edge", "Trim"),
            ("Shift+drag edge", "Roll"),
            ("Shift+drag clip", "Slip"),
            ("Shift+Alt+drag clip", "Slide"),
            ("Alt+drag", "snapping off"),
        ] {
            let row = FIXED
                .iter()
                .find(|f| f.chord == chord)
                .unwrap_or_else(|| panic!("{chord} is not on the sheet"));
            assert!(row.label.contains(word), "{chord}: {}", row.label);
            assert_eq!(row.category, Category::Editing, "{chord}");
            assert_eq!(row.reach, Reach::Gesture, "{chord}");
        }
    }

    #[test]
    fn the_plate_reads_the_signed_delta_and_the_new_length() {
        assert_eq!(trim_plate_text(300, 250, 25.), "+12.0 s \u{00b7} 10.0 s long");
        assert_eq!(trim_plate_text(-40, 310, 25.), "\u{2212}1.6 s \u{00b7} 12.4 s long");
        assert_eq!(trim_plate_text(0, 250, 25.), "0.0 s \u{00b7} 10.0 s long");
    }
}
