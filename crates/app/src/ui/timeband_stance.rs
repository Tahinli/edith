//! The transport band's own content (REDESIGN §3.3, DESIGN.md §6.3): the
//! timecode on the left, icon-only transport controls centred (go to start,
//! frame back, play, frame forward, go to end, loop), then mute, the volume
//! slider, the level meter and fullscreen on the right. Words and keys live in the
//! tooltips only. The trim deltas show only while a loop-trim roll is armed.
//! The band is 44px tall. The overview strip sits above the ruler
//! (`ui/bench_stance/overview.rs`); Export lives on the menu bar
//! (`ui/menubar.rs`). `stance.rs::time_band()`'s frame draws the band; this
//! module owns what fills it, the same split `dock_stance.rs`/`bench_stance.rs`
//! already make for their regions.

use crate::player::text_field::TextField;
use crate::ui::hitmap;
use crate::ui::type_scale::{self, Typeset};
use crate::*;

thread_local! {
    /// The band row's own measured width, read by the frame after the one
    /// that took it (`width_probe` asks for that frame whenever it changes).
    /// This is what the degradation ladder below keys off: the band cannot
    /// know its column's width any other way without a `Window` this
    /// module's `render` is not handed.
    static BAND_W: Rc<Cell<Pixels>> = Rc::new(Cell::new(px(0.)));
}

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

/// The centre column's width as the time band last measured it (0 before the
/// first frame). The toolbar shares the column, so it reads this instead of
/// owning a second probe.
pub(crate) fn column_w() -> f32 {
    BAND_W.with(|w| f32::from(w.get()))
}

/// Which layers of the band are still drawn at a given column width
/// (DESIGN §7's ladder). The band's groups are the timecode, the transport,
/// Loop, the volume and Mute, and only the chord row under the glyphs is
/// sheddable. The thresholds below were measured before the §6.3 right-end
/// controls landed, so they still need re-measuring with `$EDITH_HITMAP`.
///
/// Keys are no longer a rung: DESIGN §4.5 has every command wear its key, so
/// the chord badges under the glyphs are always drawn. The ladder's one rung
/// is kept only for the test that pins its thresholds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
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
/// and 22px of height). Fixed width + the strip's old 120px floor, rounded up
/// to the next 10px, is what each rung costs (the strip has left the band, but
/// these are the numbers the rung was measured at, and the test pins them):
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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn band_layers(band_w: f32) -> BandLayers {
    BandLayers {
        chords: band_w >= 580.,
    }
}

/// A signed delta in seconds to one decimal (`−0.5 s`, `+1.0 s`): the sign says which way the
/// edge moved since the trim-both-sides mode armed, the number how far.
fn signed_delta(frames: i64, fps: f64) -> String {
    let secs = frames as f64 / fps.max(1.);
    format!("{}{:.1} s", if frames < 0 { "−" } else { "+" }, secs.abs())
}

/// The loop-trim line (DESIGN §6.3), while a roll is armed: the mode in plain words and the
/// subject cut's own trim deltas against where the roll armed it. The deltas need a selected cut
/// to measure; the mode name does not. Read by the status line (`ui::stance`), not the band: a
/// line of it in the transport band moved the transport's own controls.
pub(crate) fn trim_text(player: &Player) -> Option<String> {
    let (lo, hi) = player.loop_trim?;
    // The baseline a roll session captured at arm time (`toggle_loop_trim`)
    // doubles as the trim deltas' zero point: the same span it loops is the
    // span its own edges are measured from.
    let deltas = player.selected.anchor().and_then(|(lane, idx)| {
        let clip = player.session.as_ref()?.lane_clips(lane).get(idx)?;
        let fps = player.active_fps();
        Some((
            signed_delta(i64::from(clip.start) - i64::from(lo), fps),
            signed_delta(i64::from(clip.end()) - i64::from(hi), fps),
        ))
    });
    Some(match deltas {
        Some((start_d, end_d)) => format!("Trimming both sides \u{00b7} Start {start_d} \u{00b7} End {end_d}"),
        None => "Trimming both sides".to_string(),
    })
}

/// The slider's length on the band (REDESIGN §3.3).
const VOLUME_TRACK_W: f32 = 96.;
/// The slider's thumb, round.
const VOLUME_THUMB: f32 = 12.;

/// The master volume (REDESIGN §3.3): a 3px track in `e3`, an `accent` fill and a
/// 12px `ink1` thumb, 24px tall to hit (WCAG 2.5.8). A press and every drag
/// sample after it write the level live through [`Player::drag_volume`]: a
/// monitoring level has nothing to undo. It never touches mute, so a muted
/// slider still shows what unmuting comes back to, drawn dim. No number is
/// printed beside it; the level reads in the tooltip.
fn volume_slider(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let fill = if player.volume.muted { t.ink3 } else { t.accent };
    let half = VOLUME_THUMB / 2.;
    let level = master_level(player);
    div()
        .id("stance-volume")
        .flex_none()
        .w(px(VOLUME_TRACK_W))
        .h(px(HIT_MIN))
        .px(px(half))
        .cursor_pointer()
        .tooltip(crate::ui::widgets::tip_hover(
            "Volume",
            "",
            Some(&format!("{}% \u{b7} the bright part is the sound playing now", player.volume.percent())),
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.volume_dragging = true;
                this.drag_volume(event.position.x, cx);
            }),
        )
        .child(
            // The probe box is the track itself (the thumb's centre travels it), so a
            // press maps to the share the thumb will sit at.
            div()
                .relative()
                .size_full()
                .child(bounds_probe(player.volume_bar.clone()))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px((HIT_MIN - 3.) / 2.))
                        .h(px(3.))
                        .rounded(px(2.))
                        .bg(rgb(t.e3))
                        .child(
                            div()
                                .relative()
                                .h_full()
                                .w(relative(player.volume.along()))
                                .rounded(px(2.))
                                // Playing, the set level dims to a bed and the live level
                                // rides inside it; at rest it is the plain fill.
                                .map(|d| match level {
                                    Some(_) => d.bg(rgba((fill << 8) | 0x55)),
                                    None => d.bg(rgb(fill)),
                                })
                                .children(level.map(|(frac, hot, clipped)| {
                                    let ink = if clipped {
                                        crate::ui::theme::STATUS_ERROR()
                                    } else if hot {
                                        crate::ui::theme::STATUS_WARNING()
                                    } else {
                                        fill
                                    };
                                    div().absolute().left_0().top_0().h_full().w(relative(frac)).rounded(px(2.)).bg(rgb(ink))
                                })),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(px((HIT_MIN - VOLUME_THUMB) / 2.))
                        .left(relative(player.volume.along()))
                        .ml(px(-half))
                        .size(px(VOLUME_THUMB))
                        .rounded(px(half))
                        .bg(rgb(if player.volume.muted { t.ink3 } else { t.ink1 })),
                ),
        )
}
/// The Loop hover's name: the same whether or not a range is set; what it loops is the second line
/// ([`loop_tip_hint`]).
pub(crate) fn loop_tip_name(_range: Option<(u32, u32)>) -> &'static str {
    "Loop playback"
}

/// The Loop hover's second line: what a loop would loop -- between the In and Out marks when both
/// are set, the whole timeline otherwise.
pub(crate) fn loop_tip_hint(range: Option<(u32, u32)>) -> &'static str {
    match range {
        Some(_) => "Between In and Out",
        None => "Set In and Out to loop a range",
    }
}

/// The hover of an icon control: the action's own, except Loop, which names its range.
type TipFn = Box<dyn Fn(&mut Window, &mut App) -> gpui::AnyView>;

fn icon_tip(player: &Player, action: ActionId) -> TipFn {
    if action == ActionId::Loop {
        let enable = player.enable(action, None);
        Box::new(crate::ui::widgets::tip_hover(
            loop_tip_name(player.range),
            &player.keymap.display(action),
            enable.why().or(Some(loop_tip_hint(player.range))),
        ))
    } else {
        Box::new(crate::ui::widgets::action_hover(player, action))
    }
}

/// The live output level for the volume slider's track, while sound is moving: how far along the
/// set level the louder side reaches (0..1 of the fill), and whether it is within 1 dB of full
/// scale or clipped. Read through [`Player::master_reading`] (at most 30 times a second, from the
/// engine's own stereo peaks, so the mix card's meters do not compete with it). The level lives
/// inside the slider rather than as bars beside it: a second little control next to the volume
/// read as noise, and inside the fill it says what the meter is for at a glance.
fn master_level(player: &Player) -> Option<(f32, bool, bool)> {
    use crate::player::cards::METER_FLOOR_DB;
    if !player.session.as_ref().is_some_and(|s| s.is_playing()) || player.volume.muted {
        return None;
    }
    let (meters, clipped) = player.master_reading(std::time::Instant::now());
    let db = meters[0].level_db.max(meters[1].level_db);
    let frac = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0., 1.);
    Some((frac, db >= -1., clipped))
}

/// The timecode on the transport (REDESIGN §3.3): the `timecode` role, digits in
/// `ink1`, the colons and the frames in `ink2`. The one mono string in the room.
fn hero_timecode(tc: &str) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let frames_from = tc.len().saturating_sub(2);
    div()
        .flex_none()
        .flex()
        .type_style(type_scale::timecode())
        .children(tc.char_indices().map(|(i, c)| {
            let ink = if c == ':' || i >= frames_from { t.ink2 } else { t.ink1 };
            div().text_color(rgb(ink)).child(c.to_string())
        }))
}

/// The timecode while it is being retyped (Go to time): the same mono role and size, so the
/// band does not move. The text is what was typed, all selected until a key replaces it, with
/// a caret after it otherwise.
fn timecode_field(field: &TextField) -> impl IntoElement {
    let t = crate::ui::theme::tokens();
    let typed = !field.text.is_empty();
    let shown: AnyElement = match field.all && typed {
        true => div().bg(rgba(t.accent_wash)).child(field.text.clone()).into_any_element(),
        false => SharedString::from(format!("{}\u{258f}", field.text)).into_any_element(),
    };
    div()
        .id("stance-timecode-field")
        .flex_none()
        .flex()
        .type_style(type_scale::timecode())
        .text_color(rgb(t.ink1))
        .child(shown)
}

/// One icon-only transport control: a 32px hit with a 20px icon and a tooltip
/// (name and key). It always dispatches through [`Player::act`] so a refused
/// press still says why, but draws dim when the oracle refuses it.
fn icon_button(
    id: &'static str,
    player: &Player,
    icon: crate::ui::icons::Icon,
    action: ActionId,
    on: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
    let yes = player.enable(action, None).yes();
    let tip = icon_tip(player, action);
    Button::new(id)
        .style(ButtonStyle::Ghost)
        .size(ButtonSize::Large)
        .icon(icon)
        .on(on)
        .enabled(yes)
        .build()
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.act(action, window, cx);
        }))
        .children(hitmap::action(action, yes))
        .tooltip(tip)
}

/// The play / pause button: 36px, `e2`, the glyph in `accent` while playing.
fn play_button(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::ui::icons::{Icon, IconSize};
    let t = crate::ui::theme::tokens();
    let playing = player.transport().is_playing();
    let yes = player.enable(ActionId::Play, None).yes();
    let name = if playing { "Pause" } else { "Play" };
    let key = player.keymap.primary(ActionId::Play);
    crate::ui::kit::states(
        div().id("stance-tb-play"),
        crate::ui::kit::Fills::solid(),
        yes,
        None,
    )
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .size(px(36.))
    .rounded(px(18.))
    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
        this.act(ActionId::Play, window, cx);
    }))
    .children(hitmap::action(ActionId::Play, yes))
    .tooltip(crate::ui::widgets::tip_hover(name, &key, Some("Shuttle: J K L")))
    .child(
        (if playing { Icon::Pause } else { Icon::Play })
            .el(IconSize::Toolbar, if playing { t.accent } else { t.ink1 }),
    )
}

/// The width of the timecode cluster and of the volume cluster: equal, so the
/// transport between them sits on the column's centre. The tail (Mute, volume,
/// Fullscreen, with 4px gaps) is 168px, inside it.
const SIDE_W: f32 = 184.;

/// The whole band, left to right: the timecode, the transport centred in the
/// rest of the row, then mute, the volume and the level meter (REDESIGN §3.3).
/// Icons and a tooltip each; no words, no keys, no `J K L`.
pub(crate) fn render(
    player: &mut Player,
    position: f64,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    use crate::ui::icons::Icon;
    let tc = timecode(player.hero_position(position), player.active_fps());
    let band_w = BAND_W.with(Rc::clone);
    // Go to time retypes the timecode in place; a click on it opens that.
    let timecode_cell = match player.time_edit.clone() {
        Some(field) => timecode_field(&field).into_any_element(),
        None => hero_timecode(&tc).into_any_element(),
    };
    let go_to = player.keymap.primary(ActionId::GoToTime);
    div()
        .id("stance-time-band-row")
        .relative()
        // This div must claim the band's full width: `flex_1` here is what lets
        // the transport group inside it take the slack and centre the icons.
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .items_center()
        // Nothing in this band may ever paint (or be clicked) outside its own column.
        .overflow_hidden()
        .gap(px(8.))
        .px(px(16.))
        .child(width_probe(band_w))
        .child(
            div()
                .id("stance-timecode")
                .flex_none()
                .w(px(SIDE_W))
                .overflow_hidden()
                .cursor_pointer()
                .tooltip(crate::ui::widgets::tip_hover("Go to time", &go_to, None))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_time_entry(cx)))
                .child(timecode_cell),
        )
        .child(
            div()
                .id("stance-transport")
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(icon_button("stance-tb-go-start", player, Icon::SkipBack, ActionId::GoStart, false, cx))
                .child(icon_button("stance-tb-step-back", player, Icon::StepBack, ActionId::StepBack, false, cx))
                .child(play_button(player, cx))
                .child(icon_button("stance-tb-step-forward", player, Icon::StepForward, ActionId::StepForward, false, cx))
                .child(icon_button("stance-tb-go-end", player, Icon::SkipForward, ActionId::GoEnd, false, cx))
                .child(div().flex_none().w(px(8.)))
                .child(icon_button("stance-tb-loop", player, Icon::Repeat, ActionId::Loop, player.loop_on, cx)),
        )
        .child(
            div()
                .id("stance-tail")
                .flex_none()
                .w(px(SIDE_W))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(4.))
                .child(icon_button(
                    "stance-tb-mute",
                    player,
                    if player.volume.muted { Icon::VolumeX } else { Icon::Volume2 },
                    ActionId::ToggleMute,
                    player.volume.muted,
                    cx,
                ))
                .child(volume_slider(player, cx))
                // Fullscreen closes the row at its right end, as video players do: the same
                // icon button as the transport's, dispatching `ActionId::Fullscreen` through `act`.
                .child(icon_button(
                    "stance-tb-fullscreen",
                    player,
                    Icon::Scan,
                    ActionId::Fullscreen,
                    player.player_fullscreen,
                    cx,
                )),
        )
}

/// Where a typed time sends the playhead: to a frame of the timeline, or by a number of frames
/// from where it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Jump {
    At(i64),
    By(i64),
}

/// The note a time that does not read gets.
const NOT_A_TIME: &str = "Not a time. Type 1:23, 83, 83.5, 00:01:23:12, +2s or -10f";

/// Reads a typed time at `fps`, in the forms the MCP help lists under TIMES: `450f` (frames),
/// `15s` or `15` (seconds, a fraction allowed), `1:23` or `1:23.5` (minutes and seconds),
/// `1:02:03` (hours) and `00:01:23:12` (timecode, frames last). A leading `+` or `-` makes it a
/// move from the playhead: `+2s`, `-10f`.
pub(crate) fn parse_jump(text: &str, fps: f64) -> Result<Jump, String> {
    if !(fps.is_finite() && fps > 0.) {
        return Err(format!("the project has no usable frame rate ({fps})"));
    }
    let t = text.trim();
    let (sign, body) = match t.chars().next() {
        Some('+') => (1, &t[1..]),
        Some('-') => (-1, &t[1..]),
        _ => (0, t),
    };
    let frames = absolute(body.trim(), fps).ok_or_else(|| NOT_A_TIME.to_string())?;
    Ok(match sign {
        0 => Jump::At(frames),
        s => Jump::By(s * frames),
    })
}

/// One time without its sign, as a frame count (or a distance in frames).
fn absolute(body: &str, fps: f64) -> Option<i64> {
    if body.is_empty() {
        return None;
    }
    let lower = body.to_ascii_lowercase();
    if let Some(n) = lower.strip_suffix('f') {
        return whole(n).map(|v| v as i64);
    }
    if let Some(n) = lower.strip_suffix('s') {
        return secs(n).map(|v| (v * fps).round() as i64);
    }
    if !body.contains(':') {
        return secs(body).map(|v| (v * fps).round() as i64);
    }
    let parts: Vec<&str> = body.split(':').collect();
    match parts.len() {
        4 => {
            let (h, m, s, f) = (whole(parts[0])?, whole(parts[1])?, whole(parts[2])?, whole(parts[3])?);
            if m >= 60 || s >= 60 || f >= fps.ceil() as u64 {
                return None;
            }
            let base = ((h * 3600 + m * 60 + s) as f64 * fps).round() as i64;
            Some(base + f as i64)
        }
        2 | 3 => {
            let (h, m, sec) = match parts.len() {
                3 => (whole(parts[0])?, whole(parts[1])?, secs(parts[2])?),
                _ => (0, whole(parts[0])?, secs(parts[1])?),
            };
            if (parts.len() == 3 && m >= 60) || sec >= 60. {
                return None;
            }
            let total = h as f64 * 3600. + m as f64 * 60. + sec;
            Some((total * fps).round() as i64)
        }
        _ => None,
    }
}

/// A whole, unsigned number, digits only.
fn whole(p: &str) -> Option<u64> {
    let t = p.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// A non-negative number of seconds, a fraction allowed.
fn secs(p: &str) -> Option<f64> {
    let v: f64 = p.trim().parse().ok()?;
    (v.is_finite() && v >= 0.).then_some(v)
}

#[cfg(test)]
mod time_entry_tests {
    use super::*;

    const F25: f64 = 25.;

    #[test]
    fn seconds_and_minutes_read_at_the_project_rate() {
        assert_eq!(parse_jump("83", F25), Ok(Jump::At(2075)));
        // 83.5 s is frame 2087.5, which rounds up like the MCP reader does.
        assert_eq!(parse_jump("83.5", F25), Ok(Jump::At(2088)));
        assert_eq!(parse_jump("1:23", F25), Ok(Jump::At(2075)));
        assert_eq!(parse_jump("1:23.5", F25), Ok(Jump::At(2088)));
        assert_eq!(parse_jump("1:02:03", F25), Ok(Jump::At(93075)));
        assert_eq!(parse_jump("15s", F25), Ok(Jump::At(375)));
        assert_eq!(parse_jump(" 15.0s ", F25), Ok(Jump::At(375)));
    }

    #[test]
    fn frames_and_timecode_are_whole_frames() {
        assert_eq!(parse_jump("450f", F25), Ok(Jump::At(450)));
        assert_eq!(parse_jump("450F", F25), Ok(Jump::At(450)));
        assert_eq!(parse_jump("00:01:23:12", F25), Ok(Jump::At(2087)));
        assert_eq!(parse_jump("00:00:01:00", F25), Ok(Jump::At(25)));
        assert_eq!(parse_jump("00:00:00:24", F25), Ok(Jump::At(24)));
    }

    #[test]
    fn a_sign_moves_from_the_playhead() {
        assert_eq!(parse_jump("+2s", F25), Ok(Jump::By(50)));
        assert_eq!(parse_jump("-10f", F25), Ok(Jump::By(-10)));
        assert_eq!(parse_jump("+0.5s", F25), Ok(Jump::By(13)));
        assert_eq!(parse_jump("-1:00", F25), Ok(Jump::By(-1500)));
        assert_eq!(parse_jump("-1:2:3", F25), Ok(Jump::By(-93075)));
        assert_eq!(parse_jump("+00:00:01:00", F25), Ok(Jump::By(25)));
    }

    #[test]
    fn garbage_is_refused_with_the_note() {
        for bad in ["", "   ", "abc", "1.2.3", "1:75", "1:23:45:25", "1:2:3:4:5", "--5", "-", "+", "15 s x"] {
            assert_eq!(parse_jump(bad, F25), Err(NOT_A_TIME.to_string()), "{bad:?}");
        }
    }

    #[test]
    fn a_project_with_no_frame_rate_refuses_every_time() {
        assert!(parse_jump("1", 0.).is_err());
        assert!(parse_jump("1", f64::NAN).is_err());
    }
}
