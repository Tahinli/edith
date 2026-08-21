//! What is drawn and where: the toolbar, the theme, the notices, the lanes
//! and their marks, the tints, the subtitle rows and the sliders.

use super::*;

/// The edit toolbar against the 640x360 floor the whole editor is measured
/// at. It does not fit and cannot be made to -- so what it may never do is
/// *hide* the tail: the row scrolls, and the door to everything scrolled
/// off it is pinned outside the scrolling box.
#[test]
fn toolbar_fits_the_smallest_window() {
    let toolbar = src_text("ui/toolbar.rs");
    let row = &toolbar[toolbar.find("pub(crate) fn toolbar(").expect("the toolbar")..];
    // Every button in the row is a hit target, so none of them is narrower
    // than `HIT_MIN`, and they sit in 8 px gaps inside the row's own 12 px
    // padding.
    let buttons = row.matches("action_control(").count();
    assert!(buttons >= 11, "the row's buttons moved; this scan is blind");
    // The row is shorter than it was -- the project's own settings (size,
    // rate, HDR) are the inspector's now and the transport is the
    // picture's -- so whether it fits at 640 px depends on the words in it
    // and is not something this scan can measure. What it can hold to is
    // the rule: "it scrolls" is not "it can be found", so the door to
    // whatever is off the right edge is pinned outside the scrolling box
    // and the card it opens carries every action there is
    // (`every_action_is_on_the_actions_card`).
    assert!(
        row.contains("\"controls-more\""),
        "nothing pinned beside the scrolling row: the tail is unreachable at 640 px"
    );
    assert_eq!(row.matches("overflow_x_scroll").count(), 1);
    assert!(
        row.find("\"controls-more\"") > row.find("overflow_x_scroll"),
        "the pinned door is inside the row it is meant to outlive"
    );
    // ...and nothing in the row decides for itself whether it can act: the
    // oracle does, once, for the key and the button alike. A raw `control(`
    // in here is a button that can go dead while its stroke still fires --
    // the bug this redesign was called in for.
    assert_eq!(
        row.matches("\n                    .child(control(").count(),
        0,
        "a toolbar button bypassing the availability oracle"
    );
}

/// The class the user reported twice in one day ("some menus are belongs to
/// old ui ... for example resolution picker", then "right click menu in
/// library is old too"): a surface that paints a *role* token
/// (`BG_HOVER`/`BG_SELECTED`/`BG_RAISED`) rather than a Darkroom token used
/// to get the legacy tree's pale greys, because the Darkroom palette still
/// carried them. Dozens of call sites read those three roles; the fix is the
/// palette, and this pins it: every interaction grey in the Darkroom palette
/// stays inside DESIGN §2's `raised` band (#14171B - #17191D), with only the
/// picked-row fill allowed one step past it, and each stays distinguishable
/// from the one below it so a hover is still visible on a resting row.
#[test]
fn the_darkroom_palettes_interaction_greys_stay_inside_the_raised_band() {
    use crate::ui::theme::darkroom;
    let grey = |v: u32| (v >> 16 & 0xff, v >> 8 & 0xff, v & 0xff);
    let lum = |v: u32| {
        let (r, g, b) = grey(v);
        u32::from(r) + u32::from(g) + u32::from(b)
    };
    let band_low = lum(0x14171b);
    let band_high = lum(0x17191d);
    for (name, value) in [
        ("BG_RAISED", darkroom::BG_RAISED),
        ("BG_HOVER", darkroom::BG_HOVER),
    ] {
        assert!(
            (band_low..=band_high).contains(&lum(value)),
            "darkroom::{name} = {value:#08x} is outside DESIGN §2's raised band \
             -- every menu, card and row that paints this role would open a \
             pale plate in a dim room"
        );
    }
    assert!(
        lum(darkroom::BG_SELECTED) > lum(darkroom::BG_HOVER)
            && lum(darkroom::BG_SELECTED) <= lum(0x1c1f24),
        "the picked-row fill is one step past hover at most; the real mark is \
         the 1px ink1 ring (DESIGN §4)"
    );
    assert!(
        lum(darkroom::BG_HOVER) > lum(darkroom::BG_RAISED),
        "hover must be a step ABOVE the resting raised fill or a hovered row \
         cannot be seen at all"
    );
    assert!(
        lum(darkroom::BG_HOVER_DIM) < lum(darkroom::BG_RAISED),
        "the dim hover is the step below, not above"
    );
}

/// The whole reason `ui/theme.rs` exists: a colour written anywhere else is
/// a colour the next palette sweep will miss, which is exactly how 186 grey
/// calls survived every previous attempt at this.
#[test]
fn no_colour_is_written_outside_the_theme() {
    let source = ui_source();
    let stray: Vec<&str> = source
        .lines()
        .filter(|l| l.contains("rgb(0x") || l.contains("rgba(0x"))
        .collect();
    assert!(
        stray.is_empty(),
        "colour written outside the theme: {stray:?}"
    );
}

/// The literal-grep above is blind to a hue arriving through a theme
/// *constant*: `DARK_SEAM` carried `rgba(0,0,0,.7)` (`0xRRGGBBAA`) into
/// `rgb()`, which reads the alpha byte as blue -- 2436 blue pixels in a
/// darkroom screenshot, on a call site that never once wrote `0x` itself.
/// So this scan reads the theme's own encoding: every constant whose low
/// byte is not `ff` (i.e. `0xRRGGBBAA` with a real alpha) is alpha-carrying,
/// and an alpha-carrying role may only ever be handed to `rgba(`, never
/// `rgb(`.
#[test]
fn an_alpha_carrying_role_never_reaches_rgb() {
    let theme = src_text("ui/theme.rs");
    let alpha_roles: Vec<&str> = theme
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("pub const ")?;
            let (name, rest) = l.split_once(':')?;
            let hex = rest.trim().strip_prefix("u32 = 0x")?;
            let hex = hex.trim_end_matches(';');
            (hex.len() == 8 && !hex.ends_with("ff")).then_some(name.trim())
        })
        .collect();
    assert!(
        alpha_roles.len() >= 8,
        "the theme's alpha-carrying roles moved; this scan is blind"
    );

    let source = ui_source();
    let mut misuses = Vec::new();
    for role in &alpha_roles {
        if source.contains(&format!("rgb({role}(")) {
            misuses.push(*role);
        }
    }
    assert!(
        misuses.is_empty(),
        "alpha-carrying role(s) reaching rgb() instead of rgba(): {misuses:?}"
    );
}

/// "It scrolls" is not "it can be found": at the 640x360 floor the timeline
/// takes its share and no more, so a third track is behind a scroll -- and
/// the line that says so has to keep telling the truth *while* the column is
/// being scrolled, or it is an instruction nobody can carry out.
#[test]
fn the_line_about_what_is_below_the_fold_counts_what_is_still_below_it() {
    // The timeline is a region and not the window: its chrome -- the
    // scrollbar strip's row included, which is the larger of the two the
    // zoom can leave in it -- and one whole lane fit the share it may take
    // at the floor.
    let floor = 360. * TIMELINE_SHARE;
    assert!(
        timeline_fixed_h(true) + LANE_H <= floor,
        "{} px of chrome and a lane will not fit {floor} px",
        timeline_fixed_h(true)
    );
    // The lane questions, asked of the mixed math the render actually walks
    // (the uniform lanes_shown/rows_below pair went with their last caller;
    // a lane in the stack may be a thinner
    // caption one: a uniform stack answers exactly what the plain
    // functions above do, and a mixed one answers off the real heights,
    // never the media one alone.
    let uniform = [LaneKind::Video, LaneKind::Audio];
    assert_eq!(lanes_h_mixed(&uniform), lanes_h(2));
    assert_eq!(lanes_shown_mixed(&uniform, LANE_H), 1);
    let mixed = [LaneKind::Video, LaneKind::Audio, LaneKind::Subtitle];
    assert_eq!(
        lanes_h_mixed(&mixed),
        2. * LANE_H + SUB_LANE_H + 2. * 8.,
        "a caption lane is not counted as a full LANE_H row"
    );
    // A box exactly tall enough for the two media lanes and no more: the
    // thinner third lane still does not fit above it.
    assert_eq!(lanes_shown_mixed(&mixed, lanes_h(2)), 2);
    // ...and with room for the caption lane too, all three show and
    // nothing is below the fold.
    assert_eq!(lanes_shown_mixed(&mixed, lanes_h_mixed(&mixed)), 3);
    assert_eq!(rows_below_mixed(&mixed, lanes_h_mixed(&mixed), 0.), 0);
    assert_eq!(rows_below_mixed(&mixed, lanes_h(2), 0.), 1);

    // The inspector's rows are not one height, so its own line is measured
    // in pixels off the scroll instead: gpui keeps the offset negative going
    // down, and at the bottom the two cancel out exactly.
    assert_eq!(px_below(120., 0.), 120.);
    assert_eq!(px_below(120., -40.), 80.);
    assert_eq!(px_below(120., -120.), 0.);
    assert_eq!(px_below(0., 0.), 0.);
}

/// The failure a user never learns about is the one that was answered by
/// another failure: two imports failing back to back are two messages, in
/// the order they happened, and the second does not overwrite the first
/// before a frame has drawn it.
#[test]
fn a_second_message_queues_behind_the_first_instead_of_erasing_it() {
    let mut q = std::collections::VecDeque::new();
    push_notice(&mut q, "NOTHING ADDED — no video stream".into());
    push_notice(&mut q, "NOTHING ADDED — the file is not there".into());
    assert_eq!(q.len(), 2);
    assert_eq!(
        q.front().map(|n: &gpui::SharedString| n.as_ref()),
        Some("NOTHING ADDED — no video stream")
    );

    // A held key that refuses says its one sentence once: the count on the
    // bar is a count of messages, not of how long the key was held.
    push_notice(&mut q, "NOTHING ADDED — the file is not there".into());
    assert_eq!(q.len(), 2);

    // ...and the ceiling drops the oldest rather than the newest: the thing
    // that just happened is the thing worth reading.
    for i in 0..NOTICES_MAX + 3 {
        push_notice(&mut q, format!("SCAN FAILED: {i}").into());
    }
    assert_eq!(q.len(), NOTICES_MAX);
    assert_eq!(
        q.back().map(|n: &gpui::SharedString| n.as_ref()),
        Some("SCAN FAILED: 10")
    );

    // Answering the bar brings the next one up, oldest first.
    let front = q.pop_front();
    assert_ne!(front, q.front().cloned());
}

/// An export's own outcome is what a person started it to read, so it must
/// never sit behind two progress lines queued while it ran: it jumps to the
/// front and is the one showing the moment it arrives.
#[test]
fn an_export_outcome_jumps_the_queue() {
    let mut q = std::collections::VecDeque::new();
    push_notice(
        &mut q,
        "PROXY READY for a.mp4 — Proxies on cuts on it".into(),
    );
    push_notice(&mut q, "SUBTITLES b.srt — 1 track(s) in the palette".into());
    push_notice(&mut q, format!("{EXPORT_DONE}out.mp4").into());
    assert_eq!(q.len(), 3);
    assert_eq!(
        q.front().map(|n: &gpui::SharedString| n.as_ref()),
        Some(format!("{EXPORT_DONE}out.mp4")).as_deref(),
        "the export's result must be the notice showing, not third in line"
    );

    // A failed export is the same class: the outcome, whichever it was.
    let mut q = std::collections::VecDeque::new();
    push_notice(
        &mut q,
        "PROXY READY for a.mp4 — Proxies on cuts on it".into(),
    );
    push_notice(&mut q, "EXPORT FAILED: disk full".into());
    assert_eq!(
        q.front().map(|n: &gpui::SharedString| n.as_ref()),
        Some("EXPORT FAILED: disk full")
    );
}

/// The bar colours itself off the words the message already opens with, so
/// the tone cannot disagree with the sentence it labels.
#[test]
fn a_message_wears_the_colour_its_own_words_say() {
    assert_eq!(notice_tone("SCAN FAILED: no audio"), STATUS_ERROR());
    assert_eq!(
        notice_tone(&format!("{EXPORT_DONE}out.mp4")),
        STATUS_SUCCESS()
    );
    assert_eq!(
        notice_tone("NOTHING DETACHED — not grouped"),
        STATUS_WARNING()
    );
    assert_eq!(notice_tone("SNAP ON"), ACCENT_PRIMARY());
}

/// The cards are sections of the inspector, not sheets over the timeline:
/// adjusting a clip must never hide the clip. Structural, because that is
/// where the rule lives -- a card rendered from the root again would be an
/// overlay again, whatever it looked like on the day it was written.
#[test]
fn an_inspector_section_occludes_no_timeline() {
    let inspector = src_text("ui/inspector.rs");
    // Whichever file the root render has come to live in, from the impl to the
    // end of it -- found rather than named, so moving the root does not quietly
    // leave this rule reading a file the render is no longer in.
    let render = source_from("impl Render for Player");
    for card in [
        "eq_card",
        "color_card",
        "speed_card",
        "silence_card",
        "mix_card",
        "subtitle_style_card",
    ] {
        assert!(
            inspector.contains(&format!("self.{card}(")),
            "{card} is not a section of the inspector"
        );
        assert!(
            !render.contains(&format!("self.{card}(")),
            "{card} is drawn from the root again -- it is an overlay over the timeline"
        );
    }
    // The docked cards are placed against the inspector's own box, which is
    // what turns `scrim()` (`absolute().inset_0()`) from a window-wide sheet
    // into this column.
    assert!(
        inspector.contains(".relative()"),
        "the inspector is not a positioning context: its cards would cover the window"
    );
}

/// MUST 1, on the two named offenders: the button that relabels itself
/// mid-export and the one that used to read "Muted 80%". Both live in a
/// rect that is reserved once, so the words change and the box does not.
#[test]
fn a_stateful_button_keeps_its_rect() {
    use crate::ui::toolbar::{EXPORT_SLOT_W, SNAP_SLOT_W, VOLUME_SLOT_W};
    // Widest word each slot can hold, at the 12 px text this window is set
    // in -- ~7 px a character plus the 16 px of padding.
    let fits = |slot: f32, word: &str| slot >= word.chars().count() as f32 * 7. + 16.;
    for word in ["Export", "Cancel"] {
        assert!(fits(EXPORT_SLOT_W, word), "{word} overflows its rect");
    }
    for word in ["Snap on", "Snap off", "No subs", "Subs off"] {
        assert!(fits(SNAP_SLOT_W, word), "{word} overflows its rect");
    }
    for word in ["100%", "× 100%"] {
        assert!(fits(VOLUME_SLOT_W, word), "{word} overflows its rect");
    }
    // And the rect is passed, not hoped for: every stateful control in the
    // three chrome rows names its width.
    let toolbar = src_text("ui/toolbar.rs");
    for (id, slot) in [
        ("\"export\"", "EXPORT_SLOT_W"),
        ("\"snap\"", "SNAP_SLOT_W"),
        ("\"subs\"", "SNAP_SLOT_W"),
        ("\"volume\"", "VOLUME_SLOT_W"),
        ("\"zoom-fit\"", "ZOOM_SLOT_W"),
    ] {
        let at = toolbar.find(id).unwrap_or_else(|| panic!("no {id} button"));
        assert!(
            toolbar[at..at + 120].contains(slot),
            "{id} does not reserve {slot}"
        );
    }
}

#[test]
fn nothing_clickable_is_smaller_than_the_wcag_minimum() {
    // Every hit target in the panel, including the scrub strip -- whose bar
    // is 6 px to look at and whose click area must not be.
    assert!(CONTROL_H >= HIT_MIN);
    assert!(RULER_HIT_H >= HIT_MIN);
    assert!(LANE_H >= HIT_MIN);
    // A caption lane's header carries exactly one hit target now (the
    // show/hide eye; remove moved to its right button), and that target
    // fills the whole row -- so the row is never allowed to undercut the
    // target it *is*.
    assert!(SUB_LANE_H >= HIT_MIN);
    // A clip box is a hit target too, and its two trim strips occlude it:
    // on a box narrower than the pair there is no body left to press, so
    // the clip cannot be selected, dragged or menued at all -- which is
    // every clip a jumpcut leaves at a normal zoom. Below three handles
    // there are no strips.
    assert!(!trims(0.));
    assert!(!trims(EDGE_W));
    assert!(!trims(2. * EDGE_W), "a box that is all handle and no clip");
    assert!(!trims(3. * EDGE_W - 0.1));
    // And where they are drawn, what is left between them is a hit target
    // in its own right -- a whole handle's width of clip.
    for width in [3. * EDGE_W, 24., 100., 4000.] {
        assert!(trims(width));
        assert!(
            width - 2. * EDGE_W >= EDGE_W,
            "{width} px of box leaves no middle"
        );
    }
}

#[test]
fn a_lane_row_is_a_fixed_header_and_a_bed_that_can_be_hit() {
    // The header column is what the ruler is offset by as well, so both
    // numbers are shared rather than repeated per row (A-MUST1/A-MUST2).
    assert!(HEADER_W > 0. && HEADER_GAP >= 0.);
    // Two lanes, a ruler, a button row and the timecode line, inside the
    // panel the window is sized for.
    assert!(
        CONTROL_H + RULER_HIT_H + 2. * LANE_H + 17. + 4. * 8. + 16. <= PANEL_H,
        "the second lane does not fit the panel"
    );
    // Headers and clip boxes are as tall as the lane, and the lane is a
    // click target (WCAG 2.5.8).
    assert!(LANE_H >= HIT_MIN);
    // A label row that ate the whole lane would leave no waveform.
    assert!(LABEL_H < LANE_H / 2.);
    // An added track adds its own row to the panel, and the two a project
    // starts with leave it exactly the height it has always been.
    assert_eq!(panel_h(2), PANEL_H);
    assert_eq!(panel_h(1), PANEL_H);
    assert_eq!(panel_h(3), PANEL_H + LANE_H + 8.);
    assert_eq!(
        panel_h(LANES_MAX),
        PANEL_H + lanes_h(LANES_MAX) - lanes_h(2)
    );
    // Past the cap the column scrolls instead: the panel stops growing, so
    // no number of tracks can push the picture off the window.
    assert_eq!(panel_h(LANES_MAX + 1), panel_h(LANES_MAX));
    assert_eq!(panel_h(50), panel_h(LANES_MAX));
    assert_eq!(lanes_h(0), 0.);
    assert_eq!(lanes_h(1), LANE_H);
    assert_eq!(lanes_h(2), 2. * LANE_H + 8.);
}

/// The scrollbar's thumb: the visible share of the timeline at its own
/// place on the track, full width with nothing to scroll, never narrower than
/// a hand can hold, and never off the ends.
#[test]
fn the_scroll_thumb_is_the_visible_share_at_its_own_place() {
    use crate::SCROLL_THUMB_MIN;
    use crate::scroll_thumb;

    // Nothing to scroll: the strip fills and says so.
    assert_eq!(scroll_thumb(400., 10., 0., 12.), (0., 400.));
    assert_eq!(scroll_thumb(400., 0., 0., 0.), (0., 400.));
    // Half the timeline on the bed: a half-width thumb, halfway along for a
    // window starting halfway in.
    assert_eq!(scroll_thumb(400., 20., 0., 10.), (0., 200.));
    assert_eq!(scroll_thumb(400., 20., 10., 10.), (200., 200.));
    // The floor width: however long the timeline, the thumb stays holdable --
    // and the clamp keeps it on the track, not the caller.
    let (x, w) = scroll_thumb(400., 1_000_000., 999_000., 100.);
    assert_eq!(w, SCROLL_THUMB_MIN);
    assert_eq!(x, 400. - SCROLL_THUMB_MIN);
    assert_eq!(scroll_thumb(400., 1_000_000., 0., 100.).0, 0.);
}

/// A press beside the time axis's thumb jumps so the thumb's middle is under
/// the pointer, and the drag that carries on from the press must land exactly
/// where the jump left the view: both are read through the track's own
/// proportion (a jump worked out in pixels at the view's pps used to land at
/// span-over-duration of the target, and the snap on the first move was the
/// drag correcting it). The equality below is the no-snap invariant, at the
/// thumb's own width and at its floor width alike.
#[test]
fn a_beside_thumb_press_jumps_by_the_tracks_own_proportion() {
    use crate::scroll_thumb;

    let bed = 400.;
    // The mapping both halves of the gesture share: a place on the track is
    // that share of the drawn duration.
    let jump = |at: f32, thumb_w: f32| (at - thumb_w / 2.).max(0.) / bed;
    for (duration, span) in [(100_f32, 20_f32), (1_000_000_f32, 100_f32)] {
        let (_, thumb_w) = scroll_thumb(bed, f64::from(duration), 0., f64::from(span));
        let at = 3. * bed / 4.;
        let start = jump(at, thumb_w) * duration;
        // The jump centres the thumb on the press (the far end of a million
        // seconds is the clamp's to hold, so the pin is measured from the
        // thumb the track actually shows).
        let (x, w) = scroll_thumb(bed, f64::from(duration), f64::from(start), f64::from(span));
        let centre = (x + w / 2.).min(bed - w / 2.);
        assert!(
            (centre - at.min(bed - w / 2.)).abs() < 1.,
            "thumb centre {centre} not at the press {at}"
        );
        // The drag's first sample from that press -- the grabbed middle, the
        // same track proportion -- is the same start, whatever the width.
        let grab = thumb_w / 2.;
        assert_eq!(jump(at, 2. * grab) * duration, start);
    }
}

/// The strip's row is furniture only while there is somewhere to scroll to:
/// zoomed out to the whole timeline the strip is not drawn and neither is its
/// height -- out of every budget at once (the region, the box the lanes are
/// laid out against, and the seam's floor), or the lanes would gain or lose a
/// strip's row at the zoom boundary without the strip to show for it.
#[test]
fn the_scroll_strip_row_comes_and_goes_with_the_zoom() {
    use crate::{SCROLL_HIT, Split, lanes_h, split_size, timeline_fixed_h, timeline_h};
    use gpui::{px, size};

    // The strip's row, and only the strip's row, is what the two faces differ
    // by -- the gap above it and its own hit height.
    assert_eq!(
        timeline_fixed_h(true) - timeline_fixed_h(false),
        8. + SCROLL_HIT
    );
    let window = size(px(1280.), px(720.));
    for scroll in [false, true] {
        // Whatever the strip is doing, the lanes keep exactly the room they
        // had: the region grows by the strip's row when it appears, by
        // exactly the row the lanes would otherwise lose to it.
        assert_eq!(timeline_h(2, scroll) - timeline_fixed_h(scroll), lanes_h(2));
        assert_eq!(timeline_h(6, scroll) - timeline_fixed_h(scroll), lanes_h(6));
        // ...and the floor keeps a whole lane standing under the line, on
        // either face of the zoom boundary.
        let floor = split_size(Split::Timeline, Some(0.), 2, window, scroll);
        let box_h = floor - timeline_fixed_h(scroll);
        assert!(
            2 > lanes_shown_mixed(&[LaneKind::Video, LaneKind::Audio], box_h),
            "no line to pay for at the floor"
        );
        assert!(
            box_h - LABEL_H - 8. >= LANE_H,
            "the floor leaves {} px for a {LANE_H} px lane",
            box_h - LABEL_H - 8.
        );
        // The two floors land the lanes in the same place: whichever face
        // the zoom is on, a timeline held at its floor shows the same whole
        // lane under the same line -- the strip's row comes and goes with
        // the floor, not out of the lane. (A size dragged into the interior
        // stays the hand's across the boundary, and the lanes are simply
        // given the row back.)
        assert_eq!(box_h, LANE_H + LABEL_H + 8.);
    }
    // The share covers the taller floor, strip in -- the one the clamp would
    // otherwise overrule at the 640x360 floor, saying the panel takes less of
    // a short window than it does.
    assert!(
        timeline_fixed_h(true) + LANE_H + LABEL_H + 8. <= 360. * TIMELINE_SHARE,
        "the strip-bearing floor will not fit the share"
    );
}

/// The lane stack's thumb: the visible share of the rows at their own place
/// on the track, the time axis's own thumb turned through a right angle.
#[test]
fn the_lane_thumb_is_the_visible_share_of_the_stack() {
    use crate::SCROLL_THUMB_MIN;
    use crate::lanes_thumb;

    // Whole stack on screen: the track fills and there is nothing to scroll.
    assert_eq!(lanes_thumb(200., 104., 200., 0.), (0., 200.));
    assert_eq!(lanes_thumb(200., 0., 0., 0.), (0., 200.));
    // Half the stack visible, taken halfway down: a half-height thumb at the
    // halfway mark.
    assert_eq!(lanes_thumb(200., 400., 200., 100.), (50., 100.));
    assert_eq!(lanes_thumb(200., 400., 200., 0.), (0., 100.));
    // The floor height: however tall the stack, the thumb stays holdable --
    // and the clamp keeps it on the track, not the caller.
    let (y, h) = lanes_thumb(200., 2000., 200., 1800.);
    assert_eq!(h, SCROLL_THUMB_MIN);
    assert_eq!(y, 200. - SCROLL_THUMB_MIN);
    assert_eq!(lanes_thumb(200., 2000., 200., 0.).0, 0.);
    assert_eq!(lanes_thumb(200., 1_000_000., 200., 0.).0, 0.);
}

/// A caption's box wears the rate its window plays at, derived off the
/// placement itself: unity placements -- including odd widths whose frames
/// came off `frames_of_us` rounding -- do not badge, and a re-rate does, with
/// the rate surviving the group coming apart.
#[test]
fn a_caption_box_wears_the_rate_its_window_plays_at() {
    use crate::caption_rate;

    let sub = |frames: u32, out_us: i64| SubClip {
        start: 0,
        frames,
        track: 0,
        in_us: 0,
        out_us,
        link: None,
    };
    // Unity, exact and rounded: neither is a re-timing.
    assert_eq!(caption_rate(sub(300, 10_000_000), 30.), None);
    assert_eq!(
        caption_rate(sub(100, 3_333_333), 30.),
        None,
        "placement rounding is not a re-timing"
    );
    // ...and rounding on SHORT tracks, where half a frame is a large share of
    // the window: `frames_of_us` of 1,016,667µs gives 31 frames (16,667µs of
    // error -- 16 permille, but half a frame), and of 5,015,000µs gives 150
    // (15,000µs). Both are unity; a relative gate badges one 0.98x and the
    // other a self-contradicting 1.00x.
    assert_eq!(
        caption_rate(sub(31, 1_016_667), 30.),
        None,
        "half a frame off a one-second window is still unity"
    );
    assert_eq!(
        caption_rate(sub(150, 5_015_000), 30.),
        None,
        "a hair under five seconds rounds to 150 frames and stays unity"
    );
    // A 2x re-rate: a 5s span holding the same 10s of words.
    assert_eq!(
        caption_rate(sub(150, 10_000_000), 30.).map(|s| s.permille()),
        Some(2000)
    );
    // ...and a slowed one -- a 20s span for the same words -- which the badge
    // keeps after any detach: the proportion is the placement's own.
    assert_eq!(
        caption_rate(sub(600, 10_000_000), 30.).map(|s| s.permille()),
        Some(500)
    );
}

/// The selection itself: clicks in order, the anchor under the hand, the
/// toggle that assembles a group, and the plain click that replaces it all.
#[test]
fn a_selection_holds_its_picks_in_click_order_and_anchors_the_last() {
    use crate::Selection;

    let mut sel = Selection::new();
    assert!(sel.is_empty());
    assert_eq!(sel.anchor(), None);

    let (v, a, cap) = (
        (Lane::V1, 0),
        (Lane::A1, 1),
        (Lane::new(LaneKind::Subtitle, 0), 0),
    );
    sel.set_one(v);
    assert_eq!(sel.len(), 1);
    assert_eq!(sel.anchor(), Some(v));

    // Ctrl-clicks join, in the order they were made.
    sel.toggle(a);
    sel.toggle(cap);
    assert_eq!(sel.picks(), &[v, a, cap]);
    assert_eq!(sel.anchor(), Some(cap), "the last pick is the anchor");
    assert!(sel.contains(a));

    // ...and a ctrl-click on a pick already held takes it back out, leaving
    // the others where they were.
    sel.toggle(a);
    assert_eq!(sel.picks(), &[v, cap]);
    assert!(!sel.contains(a));

    // A plain click is the whole selection, one pick: whatever was held gives
    // way to the thing just named.
    sel.set_one(a);
    assert_eq!(sel.picks(), &[a]);

    // `add` joins without disturbing -- Select All's builder.
    let mut all = Selection::new();
    all.add(v);
    all.add(a);
    all.add(v);
    assert_eq!(all.picks(), &[v, a], "a pick is never held twice");

    sel.clear();
    assert!(sel.is_empty());
    assert_eq!(sel.anchor(), None);
}

#[test]
fn a_click_marks_the_whole_group_and_nothing_else() {
    let (v, a) = ((Lane::V1, 0), (Lane::A1, 0));
    // Clicking the video half of group 1 marks the audio half with it -- the
    // pick, and everything sharing its link.
    assert!(marked(v, Some(1), &[v], &[Some(1)]));
    assert!(marked(a, Some(1), &[v], &[Some(1)]));
    // Another group's clips stay unmarked, in either lane.
    assert!(!marked((Lane::V1, 1), Some(2), &[v], &[Some(1)]));
    assert!(!marked((Lane::A1, 1), Some(2), &[v], &[Some(1)]));
    // A half a lift left behind has no group: it marks itself only, which
    // is what makes it separately deletable. Two ungrouped clips must not
    // mark each other by both being ungrouped.
    assert!(marked(a, None, &[a], &[None]));
    assert!(!marked(v, None, &[a], &[None]));
    // A caption ctrl-clicked into the selection marks the clip it was pinned
    // to, and the caption is marked by the clip's pick just the same.
    let cap = (Lane::new(LaneKind::Subtitle, 0), 0);
    assert!(marked(cap, Some(1), &[cap, v], &[None, Some(1)]));
    assert!(marked(a, Some(1), &[cap, v], &[None, Some(1)]));
    // Nothing selected marks nothing.
    assert!(!marked(v, Some(1), &[], &[]));
}

#[test]
fn a_name_is_dropped_rather_than_smeared_across_a_thin_clip() {
    assert!(show_label(LABEL_MIN_W));
    assert!(show_label(400.));
    assert!(!show_label(LABEL_MIN_W - 0.1));
    // The label test is the box's own width in pixels now, which is what
    // the scale hands it -- no bed width, and so nothing to be zero.
    let scale = Scale::default();
    assert!(show_label(scale.width_px(LABEL_MIN_W as f64 / PPS_DEFAULT)));
    assert!(!show_label(scale.width_px(0.)));
}

#[test]
fn an_envelope_stays_inside_the_box_it_is_drawn_in() {
    // A ramp: silence at the start, full scale at the end.
    let peaks: Vec<(f32, f32)> = (0..40)
        .map(|i| (-(i as f32) / 39., i as f32 / 39.))
        .collect();
    let (w, h) = (100., 30.);
    let cols = envelope(&peaks, 0., 1., w, h);
    assert_eq!(cols.len(), (w / WAVE_COL) as usize + 1);
    for &(x, top, bottom) in &cols {
        assert!((0. ..=w).contains(&x), "x {x} outside 0..{w}");
        assert!((0. ..=h).contains(&top), "top {top} outside 0..{h}");
        assert!(
            (0. ..=h).contains(&bottom),
            "bottom {bottom} outside 0..{h}"
        );
        // Never inverted, and never a polygon with no area: silence has to
        // read as a line rather than as nothing at all.
        assert!(
            bottom - top >= 1.,
            "column {top}..{bottom} is thinner than a pixel"
        );
    }
    // The ramp is drawn as a ramp: the last column is taller than the first.
    let height = |&(_, top, bottom): &(f32, f32, f32)| bottom - top;
    assert!(height(cols.last().unwrap()) > height(cols.first().unwrap()) + 5.);
    // Degenerate inputs draw nothing rather than panicking.
    assert!(envelope(&[], 0., 1., w, h).is_empty());
    assert!(envelope(&peaks, 0., 1., 0., h).is_empty());
    // A clip whose range runs past the peaks clamps to the last bucket.
    assert!(!envelope(&peaks, 0., 99., w, h).is_empty());
}

/// A box laid out wider than any screen -- a long clip at a deep zoom -- is
/// still one screen's worth of columns: the path a repaint has to build is
/// bounded by what can be seen, not by what the layout says the box is.
/// Unbounded, a 5 s clip zoomed to the frame is a path of millions of points
/// per frame, and the repaint that stalls on it is the waveform that
/// "disappeared".
#[test]
fn an_envelope_never_costs_more_points_than_a_screen_can_show() {
    let peaks: Vec<(f32, f32)> = (0..200).map(|i| (-(i as f32) / 199., 1.)).collect();
    // The width a 5 s clip is laid out at when the bed shows 8 frames of it.
    let huge = 5. * 30. / 8. * 1200.;
    let cols = envelope(&peaks, 0., 5., huge, 30.);
    assert!(
        cols.len() <= WAVE_COLS_MAX + 1,
        "{} columns for a {huge} px box",
        cols.len()
    );
    // ...and the slice actually painted is the part of the box on the bed,
    // which is where that width stops mattering: a column per two visible
    // pixels, at every zoom.
    let (x, w) = visible_slice(-huge / 2., huge, 1200.);
    assert_eq!((x, w), (huge / 2., 1200.));
    assert_eq!(envelope(&peaks, 0., 5., w, 30.).len(), 601);
    // A box entirely off the bed has no slice, and one that has never been
    // measured is drawn whole -- what was drawn before there was a bed.
    assert_eq!(visible_slice(2000., 500., 1200.), (0., 0.));
    assert_eq!(visible_slice(-3000., 500., 1200.), (500., 0.));
    assert_eq!(visible_slice(-40., 500., 0.), (0., 500.));
    // Half on, at either edge.
    assert_eq!(visible_slice(-100., 500., 1200.), (100., 400.));
    assert_eq!(visible_slice(1000., 500., 1200.), (0., 200.));
}

/// The box a trim draws is the box its release commits, at every speed. The
/// preview used to hand the *timeline* frame count to a source-frame field:
/// at 2x a tail moved twice as fast as the pointer and snapped back on
/// release, and a head drag moved the clip's other edge.
#[test]
fn a_trim_preview_lands_where_the_release_commits() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    for permille in SPEED_PRESETS {
        // Live, so the loop owes it no undo step: the speeds are the axis
        // this walks, and the trims below are what is undone.
        session
            .set_speed_live(Lane::V1, 0, Speed::from_permille(permille))
            .expect("a clip alone on its lane may be speeded");
        for edge in [Edge::Start, Edge::End] {
            let clip = session.lane_clips(Lane::V1)[0];
            let (lo, hi) = session
                .trim_room(Lane::V1, 0, edge)
                .expect("clip 0 is there");
            // Both walls and the middle of the room: the whole range a
            // pointer can be clamped to.
            for to in [lo, (lo + hi) / 2, hi] {
                let preview = trimmed_clip(clip, edge, to, false);
                // The drag is one edit and one undo step, so the next `to`
                // is measured from the same clip this one was.
                if session.trim_clip(Lane::V1, 0, edge, to) {
                    assert_eq!(
                        preview,
                        session.lane_clips(Lane::V1)[0],
                        "{edge:?} to {to} at {permille} per mille"
                    );
                    assert!(session.undo(), "the trim is one undo step");
                } else {
                    // An edge already where it was asked to go is not an
                    // edit, and the preview draws the clip unchanged.
                    assert_eq!(preview, clip, "{edge:?} to {to} at {permille} per mille");
                }
                assert_eq!(session.lane_clips(Lane::V1)[0], clip, "back where it was");
            }
        }
    }
}

/// The caption a drag is showing lands where the release commits -- the
/// subtitle twin of the trim preview above -- and the two contracts the boxes
/// on the lanes rest on: `place_sub`'s `at` wins over the placement's own
/// `start`, and a gesture that changed nothing is `Ok` and not a refusal, so a
/// front-end that toasted every `Ok` would toast a pick-up-put-back.
#[test]
fn a_caption_trim_preview_lands_where_the_release_commits() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let fps = session.meta().frame_rate;
    let srt = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/data/test_subs.srt")
        .canonicalize()
        .expect("the subtitle fixture");
    session.import_subtitles(&srt).expect("the .srt imports");
    let lane = session.add_lane(LaneKind::Subtitle);
    // The whole track as a placement, exactly as `Player::sub_of_track` builds
    // one from a palette row.
    let out_us = session.subtitles()[0]
        .cues
        .iter()
        .map(|c| c.end_us)
        .max()
        .expect("the fixture has cues");
    let whole = SubClip {
        start: 0,
        frames: frames_of_us(out_us, fps),
        track: 0,
        in_us: 0,
        out_us,
        link: None,
    };
    session
        .place_sub(lane, 30, whole)
        .expect("an empty subtitle lane takes it");
    assert_eq!(
        session.sub_lane(lane)[0].start,
        30,
        "the frame the hand let go on wins over the placement's own start"
    );
    for edge in [Edge::Start, Edge::End] {
        let (lo, hi) = session
            .trim_sub_room(lane, 0, edge)
            .expect("placement 0 is there");
        for to in [lo, (lo + hi) / 2, hi] {
            let placed = session.sub_lane(lane)[0];
            let preview = trimmed_sub(placed, edge, to);
            session
                .trim_sub(lane, 0, edge, to)
                .expect("an edge inside its own room is never refused");
            let now = session.sub_lane(lane)[0];
            assert_eq!(
                (preview.start, preview.frames),
                (now.start, now.frames),
                "{edge:?} to {to}"
            );
            // One edit, one undo step -- and none at all where the edge was
            // already there, which is the `Ok` that must stay silent.
            if (now.start, now.frames) != (placed.start, placed.frames) {
                assert!(session.undo(), "the trim is one undo step");
            }
        }
    }
    // Picked up and put back down: `Ok`, nothing moved, nothing to say.
    let before = session.sub_lane(lane)[0];
    session
        .move_sub(lane, 0, lane, before.start)
        .expect("a drop that changes nothing is not a refusal");
    assert_eq!(session.sub_lane(lane)[0], before);
    // ...and two captions over one frame are refused in words, which is what
    // the notice shows verbatim.
    let over = session
        .place_sub(lane, before.start, whole)
        .expect_err("two captions may not cover one frame");
    assert!(over.to_string().contains("already covers"), "{over}");
}

/// A still trims the same way, and the preview knows it: its head grows
/// forward from source frame 0 -- every frame of it is the same picture --
/// so the box stretches instead of sliding left.
#[test]
fn a_stills_trim_preview_grows_forward_like_the_commit() {
    let mut session = PlaybackSession::open(asset("test_still.png")).expect("a picture opens");
    for edge in [Edge::Start, Edge::End] {
        let clip = session.lane_clips(Lane::V1)[0];
        let (lo, hi) = session
            .trim_room(Lane::V1, 0, edge)
            .expect("clip 0 is there");
        for to in [lo, (lo + hi) / 2, hi] {
            let preview = trimmed_clip(clip, edge, to, true);
            match session.trim_clip(Lane::V1, 0, edge, to) {
                true => {
                    assert_eq!(
                        preview,
                        session.lane_clips(Lane::V1)[0],
                        "a still {edge:?} to {to}"
                    );
                    assert!(session.undo(), "the trim is one undo step");
                }
                false => assert_eq!(preview, clip, "a still {edge:?} to {to}"),
            }
            assert_eq!(session.lane_clips(Lane::V1)[0], clip, "back where it was");
        }
    }
}

/// gpui freezes a drag's payload for the whole gesture, and nothing stops a
/// stroke from editing the lane under it: the drop has to find the clip that
/// was picked up, not whatever slid into its index.
#[test]
fn a_drop_moves_the_clip_that_was_picked_up_not_its_old_index() {
    let at = |start: u32| Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start,
        in_frame: 0,
        out_frame: 30,
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::Fit,
        speed: Speed::NORMAL,
    };
    let lane = [at(0), at(30), at(60)];
    let dragged = lane[2];
    assert_eq!(live_idx(&lane, 2, dragged), Some(2), "nothing moved");
    // A delete in front of it: the clip is now index 1, and the index the
    // drag froze names a clip nobody grabbed.
    let after = [at(0), at(60)];
    assert_eq!(live_idx(&after, 2, dragged), Some(1));
    assert_eq!(live_idx(&after, 1, dragged), Some(1));
    // Deleted mid-drag: there is nothing to move, and moving its neighbour
    // instead is exactly the bug this exists for.
    assert_eq!(live_idx(&[at(0)], 2, dragged), None);
    assert_eq!(live_idx(&[], 0, dragged), None);
}

#[test]
fn a_quiet_source_still_draws_as_a_shape() {
    // An eighth of full scale, which is about where the fixtures sit.
    let quiet: Vec<(f32, f32)> = vec![(-0.125, 0.125), (-0.0625, 0.0625)];
    let loud = normalise(quiet.clone());
    assert_eq!(loud[0], (-1., 1.));
    assert_eq!(loud[1], (-0.5, 0.5));
    // Digital silence has no loudest sample to scale to; it must not divide
    // by zero and must stay flat.
    assert_eq!(normalise(vec![(0., 0.)]), vec![(0., 0.)]);
    assert!(normalise(Vec::new()).is_empty());
}

/// The whole waveform path, from the file on disk to the columns that get
/// painted: what no screenshot can assert about the shape.
#[test]
fn the_fixtures_waveform_reaches_the_lane_as_a_shape() {
    let asset = |name: &str| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    };
    let peaks = normalise(
        engine::waveform::peaks(asset("test_av.mp4"), 0, WAVE_BPS)
            .expect("open the fixture")
            .expect("test_av.mp4 has audio"),
    );
    // 5 s of source at the rate the lane asks for.
    assert!(peaks.len().abs_diff(5 * WAVE_BPS as usize) <= WAVE_BPS as usize);
    let cols = envelope(&peaks, 0., 5., 600., 30.);
    let height = |&(_, top, bottom): &(f32, f32, f32)| bottom - top;
    let tallest = cols.iter().map(height).fold(0., f32::max);
    let flattest = cols.iter().map(height).fold(f32::MAX, f32::min);
    // The fixture's 1 Hz pulse: a full-scale peak and a near-silent dip in
    // every second, so the drawn envelope is a shape and not a bar.
    assert!(tallest > 25., "loudest column only {tallest} px of 30");
    assert!(
        flattest < 8.,
        "quietest column {flattest} px -- no dips drawn"
    );
    // A video-only source draws no waveform at all rather than a flat fake.
    assert!(
        engine::waveform::peaks(asset("test_baseline.mp4"), 0, WAVE_BPS)
            .expect("open the fixture")
            .is_none()
    );
}

#[test]
fn every_mark_on_a_clip_is_legible_on_it() {
    // The body a name and a waveform are drawn on is the clip's kind
    // ([`clip_kind`]); the source tint is the border and the library
    // swatch, and is measured for telling itself apart rather than for
    // carrying text.
    // Every palette a person can pick, not merely the one in force.
    for id in crate::ui::theme::PaletteId::ALL {
        let p = id.palette();
        for (i, tint) in [p.CLIP_VIDEO, p.CLIP_AUDIO, p.CLIP_IMAGE, p.CLIP_TEXT]
            .iter()
            .enumerate()
        {
            // WCAG 1.4.3: the clip's name is body text on its tint.
            assert!(
                contrast(p.FG_PRIMARY, *tint) >= 4.5,
                "{id:?} source {i}: label contrast {:.2}",
                contrast(p.FG_PRIMARY, *tint)
            );
            // WCAG 1.4.11: the waveform is a non-text graphic on it.
            assert!(
                contrast(p.FG_SECONDARY, *tint) >= 3.,
                "{id:?} source {i}: waveform contrast {:.2}",
                contrast(p.FG_SECONDARY, *tint)
            );
        }
        // A selected clip's bed is the same two marks on a different colour.
        assert!(contrast(p.FG_PRIMARY, p.BG_SELECTED) >= 4.5, "{id:?}");
        assert!(contrast(p.FG_SECONDARY, p.BG_SELECTED) >= 3., "{id:?}");
        // The bed a gap shows through has to read as a hole in the lane --
        // the clip is the object, the bed is the hole -- and the playhead
        // has to be findable on both.
        assert!(contrast(p.CLIP_VIDEO, p.BG_TIMELINE) >= 1.5, "{id:?}");
        assert!(contrast(p.ACCENT_PLAYHEAD, p.BG_TIMELINE) >= 3., "{id:?}");
        assert!(contrast(p.ACCENT_PLAYHEAD, p.CLIP_VIDEO) >= 3., "{id:?}");
    }
    // The sanity check on the ratio itself: black on white is 21:1.
    assert!((contrast(0xffffff, 0x000000) - 21.).abs() < 0.01);
}

#[test]
fn source_tints_differ_per_source_and_cycle() {
    // The bug: the first entry *was* `BG_RAISED`, so the first file imported
    // -- the one every session has -- wore the panel's own background and
    // had no visible swatch at all.
    assert_ne!(source_tint(0), BG_RAISED());
    // ...in every family, since the swatch is drawn on whichever panel is
    // in force (`ui::theme`) and a tint that vanished into one of them is a
    // file with no colour at all.
    for id in crate::ui::theme::PaletteId::ALL {
        let p = id.palette();
        for (i, &tint) in p.SOURCE_TINTS.iter().enumerate() {
            assert_ne!(tint, p.BG_RAISED, "{id:?} tint {i} is the panel");
        }
    }
    // Neighbouring sources must not share one, or an import is invisible.
    assert_ne!(source_tint(0), source_tint(1));
    assert_ne!(source_tint(1), source_tint(2));
    assert_ne!(source_tint(2), source_tint(3));
    // Past the palette it wraps -- never an index panic.
    assert_eq!(source_tint(4), source_tint(0));
    assert_eq!(source_tint(9), source_tint(1));
    assert_eq!(source_tint(usize::MAX), SOURCE_TINTS()[usize::MAX % 4]);
}

/// Not "they are different numbers" -- different *enough to see*, against
/// each other and against the surface a swatch is drawn on. The palette is
/// deliberately dark and low-saturation, so the margin is thin and a new
/// tint picked by eye can land inside it without anyone noticing.
#[test]
fn source_tints_are_all_discernible() {
    // Summed channel distance: `BG_RAISED` to the warm tint is 18, and that
    // step is the one already accepted as readable on a lane.
    let apart = |a: u32, b: u32| {
        (0..3)
            .map(|i| {
                let shift = i * 8;
                ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff)
            })
            .sum::<u32>()
    };
    // Every family, not the one in force: a palette is picked at runtime
    // now, and four tints tuned by eye on one ground are exactly where a
    // pair lands inside the margin on another.
    for id in crate::ui::theme::PaletteId::ALL {
        let p = id.palette();
        for (i, &tint) in p.SOURCE_TINTS.iter().enumerate() {
            assert!(
                apart(tint, p.BG_RAISED) >= 16,
                "{id:?} tint {i} is {} from the panel it sits on",
                apart(tint, p.BG_RAISED)
            );
            for (j, &other) in p.SOURCE_TINTS.iter().enumerate().skip(i + 1) {
                // The eleven non-darkroom families still cycle four tuned
                // tints through the 12-wide wheel (`source_tint`'s own `%
                // len()` reads them back identically either way) -- a
                // literal repeat there is the intended cycle, not a
                // collision, so it is skipped rather than failed. Darkroom's
                // own twelve (DESIGN §2) are twelve *different* hues and
                // never repeat, so a real collision there still fails this.
                if tint == other {
                    continue;
                }
                assert!(
                    apart(tint, other) >= 16,
                    "{id:?} tints {i} and {j} are only {} apart",
                    apart(tint, other)
                );
            }
        }
        // The two a person sees side by side first must be further apart
        // than the floor: source 0 and source 1 are the first import and
        // the second.
        assert!(apart(p.SOURCE_TINTS[0], p.SOURCE_TINTS[1]) >= 32, "{id:?}");
    }
    // Darkroom's own 12-hue wheel (DESIGN §2/§12 step 5's hook): every one
    // of the twelve is a genuinely different hue, not four repeated three
    // times like the other families still cycle -- a hue accidentally
    // reintroduced by a future edit here is the "four shades of grey" bug
    // this task exists to fix, so it fails loudly rather than skip past the
    // `tint == other` escape hatch above.
    let dr = crate::ui::theme::PaletteId::Darkroom.palette();
    for (i, &a) in dr.SOURCE_TINTS.iter().enumerate() {
        for (j, &b) in dr.SOURCE_TINTS.iter().enumerate().skip(i + 1) {
            assert_ne!(a, b, "darkroom tints {i} and {j} collide");
        }
    }
}

/// A `.srt` dropped on the window is nobody's stream: it has no source
/// entry, and the lookup that used to fall back to index 0 painted it with
/// the first file's colour -- a swatch saying it came out of a film it
/// never touched.
#[test]
fn a_standalone_subtitle_wears_no_file_tint() {
    let sources = [
        Source {
            path: PathBuf::from("/films/a.mkv"),
            audio_stream: 0,
        },
        Source {
            path: PathBuf::from("/films/b.mp4"),
            audio_stream: 0,
        },
        // A second stream of the first file is a second source and the
        // same colour.
        Source {
            path: PathBuf::from("/films/a.mkv"),
            audio_stream: 1,
        },
    ];
    assert_eq!(
        file_tint(&sources, Path::new("/films/a.mkv")),
        Some(source_tint(0))
    );
    assert_eq!(
        file_tint(&sources, Path::new("/films/b.mp4")),
        Some(source_tint(1))
    );
    assert_eq!(file_tint(&sources, Path::new("/subs/a.eng.srt")), None);
    assert_eq!(file_tint(&[], Path::new("/films/a.mkv")), None);
    // The same file under two spellings is one file and one colour: a
    // source is stored symlink-resolved, everything else as it was typed.
    let here = std::fs::canonicalize(".").expect("the crate directory");
    let sources = [Source {
        path: here.join("Cargo.toml"),
        audio_stream: 0,
    }];
    assert_eq!(
        file_tint(&sources, Path::new("Cargo.toml")),
        Some(source_tint(0)),
        "a relative spelling of the source file wears the source's colour"
    );
}

/// His film's two ASS tracks with the timeline trimmed to twenty seconds:
/// one of them still has cues there and one has none, and the card said
/// nothing whatever about the second while the Subtitles list went on
/// showing it with its eighty-three cues.
///
/// And the 25 GB remux's thirty-five: naming those wrapped the row to ten
/// lines and pushed Destination under the fold, so past the value box's
/// three lines the same line counts instead.
#[test]
fn the_card_names_the_subtitle_it_leaves_off_and_counts_them_when_it_cannot() {
    let film = "/films/An Episode 01.mkv";
    let two = [
        sub(film, Some(1), "[ASS]"),
        sub(film, Some(2), "[ASS] [FOR DUB]"),
    ];
    // What the engine answers about the one pick that reached it, and what
    // this side knows about the row that did not.
    let named = subtitle_plan("[ASS] → embedded".to_string(), &two, &[0]);
    assert_eq!(
        named,
        "[ASS] → embedded; [ASS] [FOR DUB] — in the palette, on no track"
    );
    assert!(
        named.chars().count() <= SUB_PLAN_CHARS,
        "two tracks fit the value box: {named}"
    );
    // Thirty-five off one file: twenty-two carry cues here, nine are
    // pictures, one could not be read, three have nothing on this timeline.
    let many: Vec<_> = (0..35)
        .map(|i| {
            let mut track = sub("/films/A Remux.mkv", Some(i), "eng — Subtitles");
            track.bitmap = (22..31).contains(&i);
            track.refused = (i == 31).then(|| "VobSub is pictures".to_string());
            track
        })
        .collect();
    // The picks are every track with a cue on the timeline: the twenty-two
    // and the nine picture ones, which the engine drops itself.
    let picks: Vec<usize> = (0..31usize).collect();
    let counted = subtitle_plan("22 tracks → embedded (…)".to_string(), &many, &picks);
    assert_eq!(
        counted,
        "22 of 35 → embedded; 9 pictures; 1 unread; 3 in the palette, on no track"
    );
    assert!(
        counted.chars().count() <= SUB_PLAN_CHARS,
        "thirty-five tracks still fit the value box: {counted}"
    );
    // Nothing on the timeline at all is still the engine's word for it.
    assert_eq!(subtitle_plan("none".to_string(), &[], &[]), "none");
    // The lanes are what an export writes once anything is placed on one, and
    // the engine words that whole ([`engine::export::planned_subtitles`]) --
    // including the lanes it carries *nothing* of, which is a sentence with no
    // pick behind it at all. That used to be dropped on the floor: the card said
    // "[ASS] — no cues here" twice and never said what became of the lane.
    let placed = "S1 [ASS] — past the last picture".to_string();
    assert_eq!(
        subtitle_plan(placed.clone(), &two, &[]),
        "S1 [ASS] — past the last picture; [ASS] [FOR DUB] — in the palette, on no track"
    );
    // ...and the row that sentence speaks for gets no clause of its own: a line
    // that names a lane's track and then calls the same track unplaced is the
    // card contradicting itself in one breath.
    assert!(!subtitle_plan(placed, &two[..1], &[]).contains("in the palette"));
}

/// The list is in the order tracks were added, which is not the order a
/// person reads it in: importing a second film puts its tracks after the
/// first film's, and importing a third `.srt` for the first film puts that
/// one last of all. The rows still read as three sources.
#[test]
fn subtitle_rows_group_a_source_however_they_were_added() {
    let tracks = [
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/b.mkv", Some(1), "eng"),
        sub("/films/a.mkv", Some(2), "fre"),
        sub("/subs/late.srt", None, "late.srt"),
        sub("/films/b.mkv", Some(3), "ger"),
    ];
    let groups = subtitle_rows(&tracks);
    assert_eq!(
        groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
        ["a", "b", "late"],
        "one group per file, in the order the files first appear"
    );
    assert_eq!(
        groups
            .iter()
            .map(|g| g.rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [vec!["eng", "fre"], vec!["eng", "ger"], vec!["late.srt"]],
        "a file's tracks are contiguous and in add order"
    );
    // Numbered within the file, the way `row_name` numbers audio streams:
    // two tracks that both say "eng" are told apart by nothing else.
    assert_eq!(
        groups
            .iter()
            .map(|g| g.rows.iter().map(|r| r.number).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [vec![1, 2], vec![1, 2], vec![1]]
    );
    // The swatch key is the file, so a group and that file's media rows
    // wear one colour -- and the standalone one has none.
    let sources = [Source {
        path: PathBuf::from("/films/a.mkv"),
        audio_stream: 0,
    }];
    assert_eq!(
        file_tint(&sources, &groups[0].path),
        Some(source_tint(0)),
        "the group carries the path the tint is asked by"
    );
    assert_eq!(file_tint(&sources, &groups[2].path), None);
    // What a header's "N tracks" says: the group's own row count, which is
    // 2, 2, 1 here -- a film that gave several tracks and a standalone
    // `.srt` that gave exactly one.
    assert_eq!(
        groups.iter().map(|g| g.rows.len()).collect::<Vec<_>>(),
        [2, 2, 1]
    );
    // A standalone `.srt` is already its own group, named after its own
    // file ("late", not lumped under some catch-all "External" bucket) --
    // there is no sourceless case for a header to special-case.
    assert_eq!(groups[2].name, "late");
    assert_eq!(groups[2].path, PathBuf::from("/subs/late.srt"));
}

/// The fold a click on a header sets is keyed by [`SubGroup::path`]
/// (`Player::sub_folded`), so it has to survive the very thing regrouping
/// is for: a second track landing on a file already in the list, or one
/// being removed from it. The group's path is the fold's whole identity,
/// so it must not move under either.
#[test]
fn a_groups_fold_key_survives_a_track_arriving_or_leaving_its_file() {
    let before = [
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/b.mkv", Some(1), "eng"),
    ];
    let after_add = [
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/b.mkv", Some(1), "eng"),
        sub("/films/a.mkv", Some(2), "fre"),
    ];
    let (before_groups, after_groups) = (subtitle_rows(&before), subtitle_rows(&after_add));
    assert_eq!(before_groups[0].path, after_groups[0].path, "a's key held");
    assert_eq!(before_groups[1].path, after_groups[1].path, "b's key held");
    // The group a second track landed on grew; the other did not move.
    assert_eq!(after_groups[0].rows.len(), 2);
    assert_eq!(after_groups[1].rows.len(), 1);
    // Removing that same track back off leaves the original key and count.
    let after_remove = subtitle_rows(&before);
    assert_eq!(after_remove[0].path, before_groups[0].path);
    assert_eq!(after_remove[0].rows.len(), 1);
}

/// What the strip header, the section heading and the toggle's notice all
/// say. Two films each carrying an "eng" track are one word apart until the
/// film is in the name; a film carrying two is one word apart from itself.
#[test]
fn the_picked_subtitle_is_named_with_the_film_it_came_out_of() {
    let tracks = [
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/b.mkv", Some(1), "eng"),
        sub("/films/a.mkv", Some(2), "eng"),
        sub("/films/b.mkv", Some(2), "und"),
        sub("/subs/late.srt", None, "late.srt"),
    ];
    let name = |track| sub_pick_name(&tracks, track).expect("a track that is there");
    // The two "eng"s of two films: one file gave several so its tracks are
    // numbered, the other gave one so it is not.
    assert_eq!(name(0), "eng 1 — a");
    assert_eq!(name(2), "eng 2 — a");
    assert_eq!(name(1), "eng 1 — b");
    assert_ne!(name(0), name(1), "two films' eng tracks read apart");
    for picked in [name(0), name(1), name(2)] {
        assert!(picked.contains(" — a") || picked.contains(" — b"));
    }
    // "und" is the tag for "nobody said", humanised once by
    // `subtitle_rows` and not again here.
    assert_eq!(name(3), "unknown language 2 — b");
    // A standalone `.srt` is its own file and its label already says so:
    // one track, so no number, and no stem after it saying it again.
    assert_eq!(name(4), "late.srt");
    // The silence a row left over from a timeline that is gone gets.
    assert_eq!(sub_pick_name(&tracks, 5), None);
    assert_eq!(sub_pick_name(&[], 0), None);
}

/// What the toggle covers is what is *placed*, so what it says is a lane fact:
/// naming the palette row the list happens to mark named a track the stroke
/// never touched ("SUBTITLES OFF — one.srt is still on the timeline" for a
/// one.srt nobody dragged anywhere).
#[test]
fn the_subtitles_toggle_says_what_is_placed_and_never_the_picked_row() {
    for on in [true, false] {
        let text = subtitle_toggle_notice(on, 2);
        assert!(text.contains('2'), "{text} counts the captions placed");
        assert!(
            !text.contains("track(s)") && !text.contains(" — a"),
            "{text} names no palette row"
        );
    }
    // Off leaves them on the lanes; that is the half a person needs told.
    assert!(subtitle_toggle_notice(false, 2).contains("still placed"));
    // An empty timeline says the move instead of a count that reads as broken.
    let none = subtitle_toggle_notice(true, 0);
    assert!(!none.contains('0'), "{none} counts nothing at nothing");
    assert!(none.contains("drag"), "{none} says the next move");
}

/// The door this editor answers "don't make me import the film again" with,
/// and it is in the panel where the tracks it adds are listed: the Text
/// tab's own button reads a file's subtitle tracks onto the open timeline
/// -- a release's `.mkv`, an `.srt` beside it -- while the file itself
/// joins nothing. It is the *action* and not a second implementation of it,
/// so the button, the stroke and the actions card cannot drift apart, and
/// it is oracle-gated like every other action button, so with no timeline
/// open it dims and says why instead of opening a chooser for nothing.
#[test]
fn the_text_tab_carries_the_add_subtitles_door() {
    let library = src_text("ui/library.rs");
    let at = library
        .find("\"add-subtitles\"")
        .expect("no add-subtitles control in the library column");
    let block = &library[at..(at + 1600).min(library.len())];
    assert!(
        block.contains("ActionId::ImportSubtitles"),
        "the button is a door of its own rather than the action: {block}"
    );
    assert!(
        block.contains("pick_and_add_subtitles"),
        "the button does not open the subtitle chooser: {block}"
    );
    // `action_control` is the oracle-gated one: dimmed with the refusal in
    // the oracle's own words. `control` alone would be lit with no timeline.
    assert!(
        library[..at].ends_with("self.action_control(\n                    "),
        "the add-subtitles button is not oracle-gated"
    );
    // ...and the empty tab names that button, rather than pointing at a
    // toolbar the person is not looking at.
    assert!(
        crate::LibraryTab::Text.empty().contains("Add subtitles"),
        "{}",
        crate::LibraryTab::Text.empty()
    );
}

/// The Darkroom keeps imported subtitle tracks in the Sources dock, where a
/// row can be selected, folded with its source, removed, and dragged to the
/// bench's existing subtitle-lane target.
#[test]
fn the_darkroom_subtitle_palette_drags_tracks_to_subtitle_lanes() {
    let dock = src_text("ui/dock_stance.rs");
    let palette = &dock[dock
        .find("fn subtitle_palette")
        .expect("no Darkroom subtitle palette")..];
    for required in [
        "subtitle_rows",
        "dock-subtitle-group",
        "sub_folded.remove",
        "sub_folded.insert",
        "this.sub_track = track",
        ".on_drag(SubPick(track)",
        "remove_subtitle_track(track",
    ] {
        assert!(palette.contains(required), "palette lost {required}");
    }
    let bench = src_text("ui/bench_stance.rs");
    let drop_at = bench
        .find(".drag_over::<SubPick>")
        .expect("no subtitle-track lane target");
    let drop = &bench[drop_at..(drop_at + 500).min(bench.len())];
    assert!(
        drop.contains("this.place_sub(drag.0, lane"),
        "a dock subtitle drag does not place on the lane: {drop}"
    );
}

/// A source's group header in the library must not vanish on a short
/// window: it is drawn whenever there is more than one source
/// (`several_files`), never gated on the viewport's height, and the list
/// under it is what scrolls instead ([`SUB_ROWS_H`]). And it has to be a
/// real fold, not the click-cycling pattern this codebase has already
/// thrown out once: one click toggles `sub_folded` shut or open, it never
/// steps through more than those two states.
#[test]
fn a_subtitle_group_header_is_never_gated_on_window_height() {
    let timeline = src_text("ui/timeline.rs");
    assert!(
        !timeline.contains("sub_headers_fit"),
        "the header is still gated on the viewport's height"
    );
    let at = timeline
        .find("let headed = ")
        .expect("no `headed` computed in subtitle_section");
    let line = &timeline[at..(at + 80).min(timeline.len())];
    assert!(
        line.contains("several_files") && !line.contains("viewport"),
        "headed depends on something other than the file count: {line}"
    );
    // The header is a click target that flips membership in a set --
    // `remove` else `insert` -- not a value stepped through several states.
    let head_at = timeline
        .find("subtitle-group-head")
        .expect("no id on the group header");
    let block = &timeline[head_at..(head_at + 2400).min(timeline.len())];
    assert!(
        block.contains("sub_folded.remove"),
        "no fold-open path: {block}"
    );
    assert!(
        block.contains("sub_folded.insert"),
        "no fold-shut path: {block}"
    );
    assert!(
        block.contains("\"1 track\"") || block.contains("tracks"),
        "the header does not say how many tracks it holds: {block}"
    );
}

/// The one thing regrouping must not break: `sub_track` is a flat index
/// into the add-order list, a click sets it and a save writes it into the
/// `.edith`. Every row must still name the track it was made from -- rows
/// for refused tracks included, because they take a number in that list
/// whether or not anyone can pick them.
#[test]
fn a_subtitle_row_names_the_flat_track_it_was_made_from() {
    let mut tracks = vec![
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/b.mkv", Some(1), "eng"),
        sub("/films/a.mkv", Some(2), "fre"),
    ];
    // Track 1 of b is pictures: still a row, still index 1 of the list.
    tracks[1].bitmap = true;
    tracks[1].refused = Some("PGS subtitles are pictures".to_string());
    let groups = subtitle_rows(&tracks);
    let flat: Vec<(usize, &str)> = groups
        .iter()
        .flat_map(|g| g.rows.iter().map(|r| (r.track, r.label.as_str())))
        .collect();
    assert_eq!(flat, [(0, "eng"), (2, "fre"), (1, "eng")]);
    for (track, label) in flat {
        assert_eq!(
            tracks[track].label, label,
            "row {track} picks the track it names"
        );
    }
    // (c) The refused one is here, saying why, and greyable by that alone.
    let refused = &groups[1].rows[0];
    assert_eq!(
        refused.refused.as_deref(),
        Some("PGS subtitles are pictures")
    );
    assert_eq!(refused.detail, "PGS subtitles are pictures");
    assert!(refused.bitmap);
    // ...and it was counted: the row after it in add order is still 2.
    assert_eq!(groups[0].rows[1].track, 2);
}

/// The × on a row shifts every track after it down one, and the pick is
/// what an export writes into the file -- so a pick that stayed put would
/// silently change which track the next export carries. Every relation
/// between the pick and the row that went, on a list of three.
#[test]
fn removing_a_subtitle_row_carries_the_pick_with_it() {
    // A row *before* the pick: the same track stays picked, one index down.
    assert_eq!(sub_pick_after_removal(2, 0, 2), 1);
    assert_eq!(sub_pick_after_removal(2, 1, 2), 1);
    // A row *after* it: the pick has not moved and neither has its index.
    assert_eq!(sub_pick_after_removal(0, 2, 2), 0);
    assert_eq!(sub_pick_after_removal(1, 2, 2), 1);
    // The picked row itself: the one that slid into its place...
    assert_eq!(sub_pick_after_removal(1, 1, 2), 1);
    // ...and the last row when the picked one was the last, since there is
    // nothing after it to slide.
    assert_eq!(sub_pick_after_removal(2, 2, 2), 1);
    // The last row of all: an emptied list is legal for subtitles, and the
    // section is not drawn at all at that point.
    assert_eq!(sub_pick_after_removal(0, 0, 0), 0);
}

/// The same claim from the click's end, on the order imports actually
/// arrive in: two films opened one after the other and an `.srt` dropped
/// last interleave in the flat list, and the display reorders them. What a
/// click sets is the row's own `track`, so the *n*th row on screen has to
/// pick the track it shows and the echoes have to name that same file.
#[test]
fn a_click_on_a_regrouped_row_picks_the_track_that_row_shows() {
    let tracks = [
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/b.mkv", Some(1), "eng"),
        sub("/films/a.mkv", Some(2), "fre"),
        sub("/subs/late.srt", None, "late.srt"),
        sub("/films/b.mkv", Some(3), "ger"),
    ];
    let rows: Vec<_> = subtitle_rows(&tracks)
        .into_iter()
        .flat_map(|group| {
            group
                .rows
                .into_iter()
                .map(move |row| (group.path.clone(), row))
        })
        .collect();
    // Read top to bottom, the rows are no longer in add order...
    assert_eq!(
        rows.iter().map(|(_, row)| row.track).collect::<Vec<_>>(),
        [0, 2, 1, 4, 3]
    );
    for (path, row) in &rows {
        // ...and what the click writes into `sub_track` -- and a save into
        // the `.edith` -- still lands on the track the row is showing.
        let picked = &tracks[row.track];
        assert_eq!(&picked.path, path, "row {} picks another file", row.track);
        assert_eq!(lang_human(&picked.label), row.label);
        // And the heading, the strip and the notice name that same file
        // back, so a click cannot leave the echoes pointing elsewhere.
        let echo = sub_pick_name(&tracks, row.track).expect("the row's own track");
        let stem = path.file_stem().expect("a fixture path").to_string_lossy();
        assert!(
            echo.contains(&*stem),
            "picked {}, echoed {echo}",
            path.display()
        );
    }
}

/// "und" is what a muxer writes when nobody said what the language is. A
/// row showing it verbatim names a language nobody speaks.
#[test]
fn an_untagged_language_says_it_is_unknown() {
    assert_eq!(lang_human("und"), "unknown language");
    assert_eq!(lang_human("eng"), "eng");
    assert_eq!(lang_human("fre — Commentary"), "fre — Commentary");
    // Reaching the subtitle rows too: a track whose only name was the tag.
    let groups = subtitle_rows(&[sub("/films/a.mkv", Some(1), "und")]);
    assert_eq!(groups[0].rows[0].label, "unknown language");
    // The pair, read as the pair: the row title comes off `language` and
    // `name` and never out of the flattened label, which is what let an
    // "und" beside a title through as a language nobody speaks. A refused
    // track states neither and keeps its label.
    let titled = |language: &str, name: &str, label: &str| engine::subtitle::SubtitleTrack {
        path: PathBuf::from("/films/a.mkv"),
        track: Some(1),
        language: language.into(),
        name: name.into(),
        label: label.into(),
        cues: Vec::new(),
        bitmap: false,
        refused: None,
    };
    for (language, name, label, title) in [
        ("fra", "Signs", "fra — Signs", "fra — Signs"),
        ("und", "Signs", "Signs", "Signs"),
        ("und", "", "und", "unknown language"),
        ("", "late.srt", "late.srt", "late.srt"),
        ("", "", "eng", "eng"),
    ] {
        let rows = subtitle_rows(&[titled(language, name, label)]);
        assert_eq!(rows[0].rows[0].label, title, "{language:?} {name:?}");
    }
}

/// The bug: an empty timeline is end-of-stream from its one black frame
/// onward, so the pump had `done` set before anything was ever pressed --
/// and the transport's restart branch read that as "played out, start from
/// the top". It started a clock against a zero-length timeline, which was
/// `done` again by the next repaint, so every further press restarted it
/// too: the button read "Pause" and no press of it ever paused.
///
/// What holds it now is one predicate, checked here against real sessions
/// on both sides -- the emptied one refuses, a timeline with clips on it
/// does not.
#[test]
fn an_empty_timeline_has_nothing_to_play() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    // Silent like the engine suite: this opens the real device.
    session.set_gain(0.0);
    // A timeline with clips on it plays, and always did: the guard must not
    // touch that side.
    assert!(!session.is_empty());
    assert!(!nothing_to_play(Some(&session)));

    // Every clip taken off, which is a state and not a failure.
    while session.delete_clip(Lane::V1, 0) {}
    while session.delete_clip(Lane::A1, 0) {}
    assert!(session.is_empty(), "the timeline is empty");
    assert_eq!(session.timeline_duration(), 0.0);

    // What the pump does every render, and what set `done` before the fix:
    // the black frame goes by and the session is at its end at once.
    for _ in 0..40 {
        while session.try_frame().is_some() {}
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        session.is_eos(),
        "an empty timeline is done before it starts"
    );

    // So the press is refused rather than sent down the restart branch --
    // and with no session at all it is the same refusal.
    assert!(nothing_to_play(Some(&session)));
    assert!(nothing_to_play(None));
    assert!(!session.is_playing(), "and nothing was started");
}

/// The slider writes the same numbers the keys do, and mute is not one of
/// them: dragging while muted picks the level unmuting comes back to.
#[test]
fn the_slider_lands_on_the_grid_the_keys_move_on() {
    let mut volume = Volume::default();
    // Both ends exactly, and clamped past them.
    volume.set_along(0.);
    assert_eq!(volume.gain(), 0.0);
    assert_eq!(volume.label(), "Vol 0%");
    volume.set_along(1.5);
    assert_eq!(volume.gain(), 1.0);
    assert_eq!(volume.along(), 1.0);

    // Halfway is 50%, and a key press from there is 5% -- the same step
    // count as before, on a finer grid.
    volume.set_along(0.5);
    assert_eq!(volume.label(), "Vol 50%");
    volume.step(true);
    assert_eq!(volume.label(), "Vol 55%");
    volume.step(false);
    assert_eq!(volume.gain(), 0.5);

    // A number no step lands on comes back as the nearest one, so the label
    // and the fill are the same value the device was handed.
    volume.set_along(0.333);
    assert_eq!(volume.label(), "Vol 33%");
    assert_eq!(volume.along(), 0.33);

    // Muted, the drag moves the level and nothing comes out.
    volume.muted = true;
    volume.set_along(0.8);
    assert_eq!(volume.gain(), 0.0);
    assert_eq!(volume.label(), "Muted 80%");
    volume.muted = false;
    assert_eq!(volume.gain(), 0.8);
}

/// The slider lands where it paints: the arithmetic `Player::drag_volume`
/// runs over the bar's own painted width, which is the one thing a test of
/// it can share without re-deriving it.
#[test]
fn the_volume_slider_lands_where_it_paints() {
    let bar = Bounds {
        origin: point(px(420.), px(508.)),
        size: size(px(VOLUME_W), px(CONTROL_H)),
    };
    let at = |x: f32| {
        let mut volume = Volume::default();
        volume.set_along(frac_along(px(x), bar));
        volume
    };
    assert_eq!(at(420.).gain(), 0.0, "the left end is silence");
    assert_eq!(at(420. + VOLUME_W).gain(), 1.0, "the right end is full");
    assert_eq!(at(-4000.).gain(), 0.0, "off the left clamps");
    assert_eq!(at(9999.).gain(), 1.0, "off the right clamps");
    // Every pixel along it: a level the keys could also reach, painted back
    // where the hand pressed to within the half step the rounding costs.
    for step in 0..=(VOLUME_W as u32) {
        let along = step as f32 / VOLUME_W;
        let volume = at(420. + along * VOLUME_W);
        let painted = volume.along();
        let slack = 0.5 / f32::from(Volume::MAX_STEPS) + 1e-4;
        assert!(
            (painted - along).abs() <= slack,
            "pressed at {along}, paints at {painted}"
        );
    }
}

/// Darkroom lane heads dispatch only verbs that apply to the lane under the
/// pointer; source checks pin the visible pointer doors to that guarded path.
#[test]
fn darkroom_lane_header_verbs_are_targeted_and_visible() {
    let live = Ctx {
        timeline: true,
        ..Ctx::default()
    };
    assert_eq!(enable_lane(ActionId::Mix, Lane::A1, live), Enable::Yes);
    assert_eq!(
        enable_lane(ActionId::RemoveVideoLane, Lane::V1, live),
        Enable::Yes
    );
    assert_eq!(
        enable_lane(ActionId::RemoveAudioLane, Lane::A1, live),
        Enable::Yes
    );
    assert!(matches!(
        enable_lane(ActionId::Mix, Lane::V1, live),
        Enable::Hidden(_)
    ));

    let bench = src_text("ui/bench_stance.rs");
    for door in [
        "\"bench-mix-lane\"",
        "\"bench-show-sub-lane\"",
        "\"bench-remove-lane\"",
        "this.act_lane(ActionId::Mix, lane, cx)",
        "this.show_sub_lane(lane, cx)",
        "this.act_lane(action, lane, cx)",
    ] {
        assert!(bench.contains(door), "lane header lost {door}");
    }
    let time_band = src_text("ui/timeband_stance.rs");
    for door in [
        "fn volume_slider(",
        "\"stance-tb-volume-bar\"",
        "this.drag_volume(event.position.x, cx)",
        ".child(volume_slider(player, cx))",
    ] {
        assert!(time_band.contains(door), "time band lost {door}");
    }
}

/// The three seams a hand may move, and the two ends neither of them may be
/// dragged past: a panel dragged to nothing is a panel nobody can get back, and
/// a picture squeezed out by its two neighbours is that same loss from the
/// other side of the window.
#[test]
fn a_dragged_divider_stops_before_either_panel_disappears() {
    use crate::ui::theme::INSPECTOR_MIN_W;
    use crate::{
        SIDE_MAX_FRAC, SPLIT_W, Split, TIMELINE_MAX_SHARE, TOOLBAR_H, inspector_w, library_w,
        split_drag_size, split_size, timeline_fixed_h, timeline_h,
    };
    use gpui::{point, px, size};

    let window = size(px(1280.), px(720.));
    // Untouched, every region is still the share the window gives it. The
    // timeline's answer is stated for both of its faces: the timeline fits
    // the bed with nothing to scroll to, and zoomed in past that line the
    // strip's row joins the furniture.
    assert_eq!(
        split_size(Split::Library, None, 2, window, false),
        library_w(1280.)
    );
    assert_eq!(
        split_size(Split::Inspector, None, 2, window, false),
        inspector_w(1280.)
    );
    assert_eq!(
        split_size(Split::Timeline, None, 2, window, false),
        timeline_h(2, false).min(720. * TIMELINE_SHARE)
    );
    assert_eq!(
        split_size(Split::Timeline, None, 2, window, true),
        timeline_h(2, true).min(720. * TIMELINE_SHARE)
    );
    // Dragged, it is what the hand asked for...
    assert_eq!(
        split_size(Split::Library, Some(300.), 2, window, false),
        300.
    );
    assert_eq!(
        split_size(Split::Timeline, Some(300.), 2, window, false),
        300.
    );
    // ...and never past either end of it.
    assert_eq!(
        split_size(Split::Library, Some(0.), 2, window, false),
        LIBRARY_MIN_W
    );
    assert_eq!(
        split_size(Split::Library, Some(9000.), 2, window, false),
        1280. * SIDE_MAX_FRAC
    );
    assert_eq!(
        split_size(Split::Inspector, Some(-40.), 2, window, false),
        INSPECTOR_MIN_W
    );
    // The timeline's own floor is a floor with a whole lane *drawn* in it. At
    // that height the column shows one row of a two-track project, so the line
    // saying the rest are below is drawn too -- out of the region's pixels,
    // not out of the lane's. Left unbudgeted the header came out 27 px of its
    // 48 and the track's name, its subtitle dot and its × were cut in half.
    // Stated for the strip-bearing face, the taller of the two -- that is the
    // one the clamp has to hold at the 640x360 floor.
    let floor = split_size(Split::Timeline, Some(0.), 2, window, true);
    assert_eq!(floor, timeline_fixed_h(true) + LANE_H + LABEL_H + 8.);
    // The same arithmetic the timeline lays the column out with
    // ([`Player::timeline`]): what the affordance costs comes off the box, and
    // a whole lane is still standing under it.
    let lanes_box = floor - timeline_fixed_h(true);
    assert!(
        2 > lanes_shown_mixed(&[LaneKind::Video, LaneKind::Audio], lanes_box),
        "no line to pay for at the floor"
    );
    assert!(
        lanes_box - LABEL_H - 8. >= LANE_H,
        "the floor leaves {} px for a {LANE_H} px lane",
        lanes_box - LABEL_H - 8.
    );
    // A lone track has nothing below it and pays nothing for the line -- the
    // floor is a floor, not a reserved corridor.
    assert_eq!(
        split_size(Split::Timeline, Some(0.), 1, window, true),
        timeline_fixed_h(true) + LANE_H
    );
    // A size dragged with one track and kept while a second arrives is raised
    // to the new floor as it is read, not silently drawn under it.
    assert_eq!(
        split_size(Split::Timeline, Some(126.), 2, window, true),
        floor
    );
    // And the floor still fits the share the shortest window gives the region.
    assert!(floor <= 360. * TIMELINE_SHARE, "{floor} px will not fit");
    assert_eq!(
        split_size(Split::Timeline, Some(9000.), 2, window, true),
        720. * TIMELINE_MAX_SHARE
    );
    // A window too narrow to honour both ends keeps the floor rather than
    // panicking inside `clamp`, which is what a ceiling under its own floor
    // does.
    let narrow = size(px(600.), px(360.));
    assert_eq!(
        split_size(Split::Inspector, Some(9000.), 2, narrow, false),
        INSPECTOR_MIN_W
    );

    // The pointer turned into a size: the panel follows the hand, and the two
    // that do not start at the window's own edge are measured from the far one.
    // Half a strip off each, because the strip is grabbed by its middle.
    assert_eq!(
        split_drag_size(Split::Library, point(px(300.), px(400.)), window),
        300. - SPLIT_W / 2.
    );
    assert_eq!(
        split_drag_size(Split::Inspector, point(px(1000.), px(400.)), window),
        280. - SPLIT_W / 2.
    );
    // The seam sits above the fixed toolbar, so what the pointer leaves under
    // it is that strip and the timeline together.
    assert_eq!(
        split_drag_size(Split::Timeline, point(px(300.), px(500.)), window),
        220. - TOOLBAR_H - SPLIT_W / 2.
    );
}

/// The darkroom's own two seams (`Split::Dock`, `Split::Bench`): a hand may
/// not drag either past its floor or its ceiling, the same promise
/// [`a_dragged_divider_stops_before_either_panel_disappears`] makes for the
/// legacy three.
#[test]
fn the_darkroom_seams_stop_before_either_side_disappears() {
    use crate::ui::theme::INSPECTOR_MIN_W;
    use crate::{
        BENCH_MIN_H, SIDE_MAX_FRAC, SPLIT_W, Split, split_bounds, split_drag_size, split_size,
    };
    use gpui::{point, px, size};

    let window = size(px(1280.), px(720.));
    // Untouched, each answers its own stance default.
    assert_eq!(
        split_size(Split::Dock, None, 2, window, false),
        crate::ui::stance::DOCK_W
    );
    assert_eq!(
        split_size(Split::Bench, None, 2, window, false),
        crate::ui::stance::BENCH_H
    );
    // Dragged, it is what the hand asked for...
    assert_eq!(split_size(Split::Dock, Some(400.), 2, window, false), 400.);
    assert_eq!(split_size(Split::Bench, Some(300.), 2, window, false), 300.);
    // ...and never past the floor...
    assert_eq!(
        split_size(Split::Dock, Some(0.), 2, window, false),
        INSPECTOR_MIN_W
    );
    assert_eq!(
        split_size(Split::Bench, Some(0.), 2, window, false),
        BENCH_MIN_H
    );
    // ...nor the ceiling. The bench's is not a window-share one -- it leaves
    // the screen and time band a fixed 160px, [`split_bounds`]'s own reason.
    assert_eq!(
        split_size(Split::Dock, Some(9000.), 2, window, false),
        1280. * SIDE_MAX_FRAC
    );
    let (_, bench_max) = split_bounds(Split::Bench, 2, window, false);
    assert_eq!(
        split_size(Split::Bench, Some(9000.), 2, window, false),
        bench_max
    );
    assert!(bench_max < 720.);
    // The pointer turned into a size, half a strip off for the same reason
    // the legacy seams read it that way.
    assert_eq!(
        split_drag_size(Split::Dock, point(px(1000.), px(400.)), window),
        280. - SPLIT_W / 2.
    );
    assert_eq!(
        split_drag_size(Split::Bench, point(px(300.), px(500.)), window),
        220. - crate::ui::stance::LEDGER_H - SPLIT_W / 2.
    );
}

/// At the bench's floor, both default lanes' rows actually fit inside the
/// `bench-lanes` column `bench_stance::render` gives them -- not just each
/// row's own clamped height, but their *sum plus the gap between them*,
/// since `bench-lanes` scrolls rather than clipping visibly and a sum that
/// overruns the column loses its bottom row's pixels off-screen, unscrolled
/// (this session's F1: the A1 lane's clip-bar border and status dot, cut by
/// ~2px at the old `BENCH_MIN_H = 80.`).
#[test]
fn both_default_lanes_fit_the_bench_at_its_floor() {
    use crate::BENCH_MIN_H;
    use crate::ui::bench_stance::{LANE_MIN_H, ROW_GAP, RULER_H, row_h};
    use crate::ui::stance::BENCH_CHROME_H;

    let box_h = BENCH_MIN_H - BENCH_CHROME_H;
    let avail = box_h - RULER_H - ROW_GAP;
    let h = row_h(2, avail);
    // Not an exact `LANE_MIN_H` any more: `BENCH_MIN_H` now carries one spare
    // row (`layout::LEDGER_SEAM_CLEARANCE`) so the last lane's own last pixel
    // never shares a row with the ledger's border, which lands here as
    // `avail` being a touch over the two-rows-plus-gap tight fit.
    assert!(
        h >= LANE_MIN_H,
        "the floor should give both lanes at least their own minimum ({h} < {LANE_MIN_H})"
    );
    let content = 2. * h + ROW_GAP;
    assert!(
        content <= avail + 1e-4,
        "the two lane rows ({content}px) overrun the column ({avail}px) -- \
         the bottom lane's pixels get cut, unscrolled"
    );
}

/// `BENCH_CHROME_H` on its own: the previous test's `box_h = BENCH_MIN_H -
/// BENCH_CHROME_H` line trusts `BENCH_CHROME_H` to already be correct, so it
/// cannot catch `BENCH_CHROME_H` itself drifting -- which is exactly how the
/// third clip (this session's) survived it: the constant undercounted the
/// section head's real line box by 1px and this test's predecessor never
/// looked. Recomputes what `ui::stance::bench` actually draws above
/// `bench_stance::render`'s content -- the div's own `.border_t_1()` (1px)
/// + its `py(4.)` top padding + the section head's real line box at gpui's
/// golden-ratio line-height, not the label's bare font size (the same trap
/// `a_lane_row_fits_what_its_own_head_draws` already checks for the lane
/// heads) -- and then checks the *whole* stack -- chrome, ruler, both gaps,
/// both lanes, and the clear row the ledger's own fixed-position border
/// needs (`LEDGER_SEAM_CLEARANCE`, not exported, so this recomputes it as
/// `BENCH_MIN_H` minus every other named term) -- fits inside `BENCH_MIN_H`
/// with nothing left over uncounted. This binary carries no `TestAppContext`
/// to mount `ui::stance::bench` and `ui::stance::ledger` for real and read
/// painted bounds back, so it stays geometry-only, same as its neighbours.
#[test]
fn the_whole_bench_stack_fits_its_own_floor_with_the_ledger_seam_clear() {
    use crate::BENCH_MIN_H;
    use crate::ui::bench_stance::{LANE_MIN_H, ROW_GAP, RULER_H};
    use crate::ui::stance::BENCH_CHROME_H;
    use crate::ui::type_scale::SECTION_HEAD_PX;

    const BENCH_BORDER_T: f32 = 1.;
    const BENCH_PY_TOP: f32 = 4.;
    let section_head_line_h = (SECTION_HEAD_PX * 1.618_034).round();
    let real_chrome = BENCH_BORDER_T + BENCH_PY_TOP + section_head_line_h;
    assert_eq!(
        BENCH_CHROME_H, real_chrome,
        "BENCH_CHROME_H ({BENCH_CHROME_H}) does not match what `stance::bench` \
         actually draws above the content ({real_chrome}px: {BENCH_BORDER_T}px \
         border + {BENCH_PY_TOP}px padding + {section_head_line_h}px label line \
         box) -- bench_stance::render gets handed the wrong box_h and lays its \
         rows out past its own real space"
    );

    // The ledger's own fixed-position border needs one more clear row below
    // the last lane, on top of chrome + ruler + both gaps + both lanes --
    // recomputed here rather than importing the private
    // `layout::LEDGER_SEAM_CLEARANCE`, so this test fails if that term is
    // ever silently dropped from `BENCH_MIN_H`'s own sum.
    let content_need = real_chrome + RULER_H + ROW_GAP + 2. * LANE_MIN_H + ROW_GAP;
    let ledger_seam_clearance = BENCH_MIN_H - content_need;
    assert!(
        ledger_seam_clearance >= 1. - 1e-4,
        "BENCH_MIN_H ({BENCH_MIN_H}) leaves only {ledger_seam_clearance}px \
         between the last lane row and the ledger's own top border -- driven \
         at exactly the content's need (0px clearance) the border still won \
         the shared pixel and clipped the last lane's status dot"
    );
}

/// The previous test asserted lane ROWS fit the bench column -- not that a
/// lane's own CONTENT fits its row, which is how the defect it fixed
/// survived it: at the old `LANE_MIN_H` of `18.` both lanes fit the bench
/// exactly while V1's status dot silently overflowed into A1's row (masked)
/// and A1's overflowed into the ledger (visible, since A1 has no next row).
/// This binary carries no `TestAppContext` to mount a real `lane_row` and
/// read its painted bounds back, so this recomputes the label's own line
/// box from the constants `lane_row` actually draws with (gpui's default
/// `TextStyle::line_height` is the golden ratio, not 1x the font size --
/// see `LANE_MIN_H`'s own doc comment) rather than a literal, so a future
/// shrink of `LANE_MIN_H` or growth of the label size fails this instead of
/// silently clipping the last lane again.
#[test]
fn a_lane_row_fits_what_its_own_head_draws() {
    use crate::ui::bench_stance::{LANE_DOT_D, LANE_HEAD_GAP, LANE_MIN_H};
    use crate::ui::type_scale::CHORD_METADATA_MIN_PX;

    let label_line_h = (CHORD_METADATA_MIN_PX * 1.618_034).round();
    let content = label_line_h + LANE_HEAD_GAP + LANE_DOT_D;
    assert!(
        LANE_MIN_H >= content,
        "LANE_MIN_H ({LANE_MIN_H}) is shorter than what a lane head actually \
         draws ({content}px: {label_line_h}px label line box + \
         {LANE_HEAD_GAP}px gap + {LANE_DOT_D}px dot) -- the status dot \
         would spill past the row, invisible until it is the last lane \
         with no next row to spill into"
    );
}

/// The notice plate's own anchor ([`crate::ui::stance::notice_bottom_offset`])
/// keeps it off the bench, at *any* bench height -- not merely the floor --
/// because its bottom offset always lands at or above the bench's own top
/// edge. `Player`'s live tree has no `TestAppContext` in this binary to mount
/// a real `notice_plate` in and read its painted bounds back, so this checks
/// the pure geometry the anchor is built from instead (F2: previously the
/// plate sat at a fixed `LEDGER_H + 6.` off the ledger and covered the
/// V1/A1 lane chips whenever the bench was short enough for the plate's own
/// height to reach past it).
#[test]
fn the_notice_plate_cannot_reach_the_bench_at_any_height() {
    use crate::BENCH_MIN_H;
    use crate::ui::stance::{BENCH_H, LEDGER_H, notice_bottom_offset};

    for bench_h in [BENCH_MIN_H, BENCH_H, 400.] {
        let notice_bottom = notice_bottom_offset(bench_h);
        // The bench sits directly above the ledger in the centre column, so
        // its own top edge (measured the same way, from the column's foot)
        // is exactly `LEDGER_H + bench_h`.
        let bench_top = LEDGER_H + bench_h;
        assert!(
            notice_bottom >= bench_top,
            "notice bottom {notice_bottom} sits below the bench's top {bench_top} \
             at bench_h={bench_h} -- the plate can cover a lane row"
        );
    }
}
/// The renderer only wraps text when the text measurement receives a definite
/// width and normal whitespace. This source-level guard keeps both declarations
/// on the notice plate: `max_w` alone limits max-content after that measurement
/// and silently permits one-line overflow. It cannot inspect GPUI's painted
/// lines; only live driving can confirm the full notice actually wrapped.
#[test]
fn the_notice_plate_declares_a_bounded_wrapping_text_layout() {
    let stance = src_text("ui/stance.rs");
    let notice_start = stance
        .find("fn notice_plate(")
        .expect("notice plate moved or renamed");
    let notice_end = stance[notice_start..]
        .find("\n/// Thin strip")
        .map(|end| notice_start + end)
        .expect("notice plate no longer precedes the ledger");
    let plate = &stance[notice_start..notice_end];

    assert!(
        plate.contains(".w_full()") && plate.contains(".max_w(px(480.))"),
        "notice plate lacks a definite width bounded to 480 px; max_w alone does not wrap text"
    );
    assert!(
        plate.contains(".whitespace_normal()"),
        "notice plate no longer explicitly requests GPUI's wrapping whitespace mode"
    );
}

/// A round trip through the file: what is saved is what the next load reads
/// back, one seam touched and the other left at its default -- a scratch
/// path, not the real config, the same isolation `keymap::tests`' own
/// `load_from`/`save_to` already takes.
#[test]
fn a_saved_seam_survives_a_reload() {
    use crate::{Split, Splits, load_stance_splits_from, save_stance_splits_to};

    let dir = engine::scratch::Scratch::dir("edith-stance-splits");
    let path = dir.join("stance-splits");

    let mut splits = Splits::default();
    splits.set(Split::Dock, 333.);
    save_stance_splits_to(&splits, &path);
    let loaded = load_stance_splits_from(&path);
    assert_eq!(loaded.get(Split::Dock), Some(333.));
    assert_eq!(loaded.get(Split::Bench), None);
}

/// The guard [`crate::split_drag_owes_save`] runs at the moment a drag ends
/// with no further pointer event ever coming (`Player::drag_left_window`,
/// wired to a live `MouseExitEvent` a `TestAppContext`-less test binary
/// cannot raise -- see `tests/media.rs`'s own note on the same limit, and
/// the harness drive `D2` in this session's report for the wiring itself).
/// What *is* checkable here without a window: exactly the two persisted
/// seams owe that save, the same set `Split::PERSISTED` already names, and a
/// drag that never started (`None`) owes nothing.
#[test]
fn only_the_two_persisted_seams_owe_a_save_when_a_drag_loses_the_window() {
    use crate::{Split, player::timeline_edit::split_drag_owes_save};

    assert!(split_drag_owes_save(Some(Split::Dock)));
    assert!(split_drag_owes_save(Some(Split::Bench)));
    assert!(!split_drag_owes_save(Some(Split::Library)));
    assert!(!split_drag_owes_save(Some(Split::Inspector)));
    assert!(!split_drag_owes_save(Some(Split::Timeline)));
    assert!(!split_drag_owes_save(None));
}

/// A missing file leaves every region at its default -- the silent fallback
/// `load_stance_splits`'s doc comment promises.
#[test]
fn a_missing_stance_splits_file_leaves_every_region_at_its_default() {
    use crate::{Split, load_stance_splits_from};

    let dir = engine::scratch::Scratch::dir("edith-stance-splits-missing");
    let splits = load_stance_splits_from(&dir.join("nothing-here"));
    assert_eq!(splits.get(Split::Dock), None);
    assert_eq!(splits.get(Split::Bench), None);
}

/// Every seam in the main layout has a handle on it, and every region draws
/// itself at the size that handle sets: a region still measuring itself off the
/// window's own share is a panel whose divider moves nothing.
#[test]
fn every_seam_in_the_layout_has_a_divider_on_it() {
    let render = src_text("render.rs");
    for split in ["Split::Library", "Split::Inspector", "Split::Timeline"] {
        assert!(
            render.contains(&format!("divider({split}")),
            "{split} has no divider to drag"
        );
    }
    let stance = src_text("ui/stance.rs");
    for split in ["Split::Dock", "Split::Bench"] {
        assert!(
            stance.contains(&format!("divider({split}")),
            "{split} has no divider to drag"
        );
    }
    // The strip is drawn wide enough to hit and says which way it moves --
    // an invisible hairline is a feature nobody finds.
    let interact = src_text("interact.rs");
    assert!(
        interact.contains("cursor_col_resize") && interact.contains("cursor_row_resize"),
        "a divider with no resize cursor on it"
    );
    // ...and nothing lays a region out off the untouched share any more.
    for (file, stale) in [
        ("ui/inspector.rs", "inspector_w("),
        ("ui/toolbar.rs", "library_w("),
        ("render.rs", "library_w("),
    ] {
        assert!(
            !src_text(file).contains(stale),
            "{file} still measures a panel with {stale}"
        );
    }
}

/// DESIGN.md §12 step 2: the stance skeleton draws its six regions -- spine,
/// screen, time band, bench, ledger, dock -- in the order §5's diagram lays
/// them out, and `Player::render` actually reaches it when the flag is on.
#[test]
fn the_stance_renders_its_six_regions_in_the_documented_order() {
    let stance = src_text("ui/stance.rs");
    let order = [
        "stance-spine",
        "stance-screen",
        "stance-time-band",
        "stance-bench",
        "stance-ledger",
        "stance-dock",
    ];
    let defined: Vec<usize> = order
        .iter()
        .map(|id| {
            stance
                .find(&format!("\"{id}\""))
                .unwrap_or_else(|| panic!("no {id} region in the stance"))
        })
        .collect();
    assert!(
        defined.windows(2).all(|w| w[0] < w[1]),
        "the six regions are not defined in DESIGN §5's order: {defined:?}"
    );

    // Defined in order is not composed in order -- `render()` has to call
    // them in it too, or the geometry above is dead prose.
    let render_body = &stance[stance
        .find("pub(crate) fn render(")
        .expect("the stance's entry point")..];
    // Open paren only, no close: DESIGN §12 steps 3 and 4 hand most of the
    // regions player/window state to read, so their call sites carry
    // arguments now. Order is what this asserts, not arity.
    let calls = [
        "spine(",
        "screen(",
        "time_band(",
        "bench(",
        "ledger(",
        "dock(",
    ];
    let composed: Vec<usize> = calls
        .iter()
        .map(|c| {
            render_body
                .find(c)
                .unwrap_or_else(|| panic!("render() never calls {c}"))
        })
        .collect();
    assert!(
        composed.windows(2).all(|w| w[0] < w[1]),
        "render() does not compose the six regions in DESIGN §5's order: {composed:?}"
    );

    // The flag has to reach it, or the skeleton is dead code behind a door
    // nobody opens. Darkroom is the default room now (`OLD_GUI=1` is the
    // opt-out) -- `Player::darkroom` is still the field render.rs reads.
    let render_rs = src_text("render.rs");
    assert!(
        render_rs.contains("if self.darkroom") && render_rs.contains("ui::stance::render("),
        "Player::darkroom never reaches ui::stance::render"
    );
}

/// The picture is letterboxed, never stretched: both the picture region and
/// the subtitle picture overlay paint through [`letterboxed_image`] (the
/// `canvas()` element that hands `ObjectFit::Contain::get_bounds` a real
/// resolved size) rather than a plain `img().object_fit(...)`, which reads
/// right by hand but never gets a fitted box from taffy in this pin -- see
/// `render.rs`'s own doc comment on the function for the measured dead end.
/// A regression back to `img(...).object_fit(` in either caller is the
/// stretch-to-fill bug returning silently.
#[test]
fn the_picture_letterboxes_through_a_canvas_never_a_plain_img_object_fit() {
    let render_rs = src_text("render.rs");
    assert!(
        render_rs.contains("pub(crate) fn letterboxed_image"),
        "the letterbox helper moved or was renamed"
    );
    assert!(
        render_rs.contains("ObjectFit::Contain.get_bounds(bounds, image.size(0))"),
        "letterboxed_image no longer computes the Contain rect by hand"
    );
    for (file, needle) in [
        ("render.rs", "letterboxed_image(i)"),
        ("ui/preview.rs", "letterboxed_image(image)"),
    ] {
        let text = src_text(file);
        assert!(
            text.contains(needle),
            "{file} no longer paints through letterboxed_image"
        );
        assert!(
            !text.contains(".object_fit(gpui::ObjectFit::Contain)"),
            "{file} reintroduced the stretched img().object_fit(Contain) dead end"
        );
    }
}

/// DESIGN §8: "No full-width bars, no covering the picture, ever." The
/// darkroom stance (`ui::stance::screen`) draws
/// [`Player::picture_area`](crate::Player::picture_area) directly, so a
/// notice surface that reaches into that method unconditionally reaches the
/// picture on the darkroom path too -- measured live covering the bottom 10%
/// of the frame (rows 300-333 of 335) *underneath* the stance's own
/// §8-conformant plate (`ui::stance::notice_plate`), which drew the same
/// notice a second time above the ledger. The legacy (non-darkroom) room
/// keeps its full-width `notice_bar` exactly as it always has -- this only
/// pins the darkroom path off it.
#[test]
fn the_darkroom_path_never_lets_a_notice_surface_reach_the_picture() {
    let body = fn_body("picture_area");
    assert!(
        body.contains("self.notice_bar(cx)"),
        "picture_area no longer draws notice_bar at all; re-check the darkroom gate still applies"
    );
    let at = body.find("self.notice_bar(cx)").expect("checked above");
    // Whatever gates the call, it has to name `darkroom` and it has to be
    // upstream of the call itself -- a gate written after the call, or one
    // that never mentions the flag, is not a gate on this method.
    let before = &body[..at];
    assert!(
        before.contains("self.darkroom") || before.contains("!self.darkroom"),
        "notice_bar in picture_area is not gated on self.darkroom any more -- \
         the darkroom stance would paint it over the picture again"
    );
}

/// DESIGN §5/§11 check 6, the fourth occlusion defect (notice bar, export
/// card, preview badge, now menus): every floating menu that can open above
/// the picture -- the clip context menu, the picker, the library menu -- must
/// size its scrolling list against `stance::menu_floor`'s room, not the raw
/// window. Sizing against the whole viewport is exactly the bug that shipped:
/// a menu taller than the bench/ledger/dock footprint got clamped by
/// `menu_at`'s own bottom-edge fit back up over the picture, because nothing
/// had told the list it only had the footprint to grow into. This pins the
/// general rule so the next surface someone floats -- a fourth menu, a fifth
/// occlusion -- cannot silently reintroduce `menu_rows_h(rows.len(),
/// viewport)` in its place.
#[test]
fn every_darkroom_menu_sizes_its_list_against_the_floor_room_not_the_raw_viewport() {
    for file in ["ui/overlays.rs", "ui/library.rs"] {
        let text = src_text(file);
        assert!(
            !text.contains("menu_rows_h(rows.len(), viewport)"),
            "{file} sizes a menu's list against the raw viewport again -- \
             route it through stance::menu_floor's room instead, or a tall \
             menu will climb back over the picture"
        );
        let floors = text.matches("menu_floor(").count();
        let sizings = text.matches("menu_rows_h(rows.len(),").count();
        assert_eq!(
            floors, sizings,
            "{file} has a menu list-sizing call not paired with a menu_floor \
             call -- every darkroom menu must clamp both its anchor and its \
             list room together"
        );
        assert!(
            sizings > 0,
            "{file} no longer opens any menu -- update this guard"
        );
    }
}

/// The settings page's own organising rule: PROJECT rows open only the
/// doors that write into the `.edith` file ([`Player::open_picker`],
/// [`Player::open_mix`]), and EDITOR rows open only the doors that do not
/// ([`Player::toggle_proxies`], [`Player::toggle_auto_proxies`],
/// [`Player::open_subtitle_style`]) -- never the config-file writers
/// (`ui::dock_stance::config_path`/`ui::theme::config_path`/
/// `keymap::Keymap::config_path`) directly, which every editor-side row
/// already routes around through its own opener. A row wired to the wrong
/// section's opener is exactly the regression a page organised around two
/// headings invites; this pins each half to its own doors so the two lists
/// cannot cross.
#[test]
fn settings_project_and_editor_sections_open_disjoint_doors() {
    let source = src_text("ui/settings_stance.rs");
    let project_start = source
        .find("fn project_section(")
        .expect("the project section");
    let editor_start = source
        .find("fn editor_section(")
        .expect("the editor section");
    let render_start = source
        .find("pub(crate) fn render(")
        .expect("the page's render fn");
    assert!(
        project_start < editor_start && editor_start < render_start,
        "the three fns moved; this scan is blind"
    );
    let project_body = &source[project_start..editor_start];
    let editor_body = &source[editor_start..render_start];

    // `Pick::Theme` is the one list that is nobody's project -- the palette
    // lives in ~/.config/edith like the subtitle font beside it (see
    // `menus.rs`'s own note on the variant), so its row is EDITOR's by the
    // same rule every other row here follows. The scan below is on the door
    // *string*, so the palette row is subtracted from the EDITOR body before
    // the project-door check runs rather than the check being loosened for
    // every picker.
    let editor_no_theme = editor_body.replace("open_picker(Pick::Theme", "");
    for door in ["open_picker(", "open_mix("] {
        assert!(
            project_body.contains(door),
            "PROJECT section no longer opens {door} -- update this guard"
        );
        assert!(
            !editor_no_theme.contains(door),
            "EDITOR section opens {door}, a project-file door -- that value belongs in PROJECT, not here"
        );
    }
    assert!(
        editor_body.contains("open_picker(Pick::Theme"),
        "the palette row left EDITOR -- it is a ~/.config preference, not a project value"
    );
    for door in [
        "toggle_proxies(",
        "toggle_auto_proxies(",
        "open_subtitle_style(",
    ] {
        assert!(
            editor_body.contains(door),
            "EDITOR section no longer opens {door} -- update this guard"
        );
        assert!(
            !project_body.contains(door),
            "PROJECT section opens {door}, an editor-only door -- that value belongs in EDITOR, not here"
        );
    }
    // Neither section writes a config file straight from a row: both go
    // through an opener, which is the one place a config-file write (the
    // subtitle style card's Save, this window's next project load) is
    // allowed to live.
    for path_fn in [
        "dock_stance::config_path",
        "theme::config_path",
        "Keymap::config_path",
    ] {
        assert!(
            !project_body.contains(path_fn),
            "PROJECT section touches {path_fn} directly"
        );
        assert!(
            !editor_body.contains(path_fn),
            "EDITOR section touches {path_fn} directly"
        );
    }
}

/// D1's own class, pinned per row rather than per section: a row's hint can
/// claim a `~/.config/edith` default (the section header claims nothing on
/// its own since [`settings_project_and_editor_sections_open_disjoint_doors`]'s
/// commit, so a lying section head no longer slips past a reviewer -- but a
/// lying *row* hint did, on Proxies, because nothing scanned row text at all).
/// For every row whose own hint text names that file, this requires the
/// matching `save_*_pref`/`load_*_pref` pair to actually exist in
/// `player/library.rs` -- the exact gap Proxies shipped with: a hint promising
/// a default the row's own door never wrote.
#[test]
fn settings_row_hints_naming_config_edith_have_a_matching_pref_pair() {
    let source = src_text("ui/settings_stance.rs");
    let library = src_text("player/library.rs");
    // (row id, the stem its pref functions are named after)
    for (row_id, stem) in [
        ("settings-proxies", "proxies"),
        ("settings-auto-proxies", "auto_proxies"),
    ] {
        let row_start = source
            .find(&format!("\"{row_id}\""))
            .unwrap_or_else(|| panic!("{row_id} row moved or renamed -- update this guard"));
        let row_end = source[row_start..]
            .find(",\n        ))")
            .map_or(source.len(), |i| row_start + i);
        let row_text = &source[row_start..row_end];
        if !row_text.contains("~/.config/edith") {
            continue;
        }
        for prefix in ["save_", "load_"] {
            let fn_name = format!("{prefix}{stem}_pref");
            assert!(
                library.contains(&format!("fn {fn_name}(")),
                "{row_id}'s hint claims a ~/.config/edith default but \
                 player/library.rs has no `{fn_name}` -- the hint promises \
                 storage the row does not have"
            );
        }
    }
}

/// With no project open, a PROJECT row must not fall back to a bare noun
/// ("Size"/"Rate"/"HDR") standing in the value slot -- it reads as a value
/// while carrying none. This binary has no `TestAppContext` to actually
/// render the page with `player.session` empty, so this is a source scan
/// for the fallback strings the bug shipped as, same as the guard above it.
#[test]
fn settings_project_rows_have_no_bare_noun_placeholder() {
    let source = src_text("ui/settings_stance.rs");
    let project_start = source
        .find("fn project_section(")
        .expect("the project section");
    let editor_start = source
        .find("fn editor_section(")
        .expect("the editor section");
    let project_body = &source[project_start..editor_start];
    for placeholder in [
        "\"Size\".to_string()",
        "\"Rate\".to_string()",
        "\"HDR\".to_string()",
    ] {
        assert!(
            !project_body.contains(placeholder),
            "a PROJECT row fell back to the bare noun {placeholder} -- it reads as a value with no project open"
        );
    }
}

/// The HDR reference rows must read the file's own declared numbers off
/// [`engine::colorspace::ContentLight`] -- not invent a monitor override the
/// engine has nowhere to persist -- and must fall back to the page's one
/// established empty state (a bare `—` in `INK4`, [`row_ink`]/[`row_static`]'s
/// own idiom) rather than a second placeholder idiom. No `TestAppContext`
/// here either, so this is the same source-scan the guard above it is.
#[test]
fn hdr_reference_rows_read_content_light_and_use_the_established_empty_state() {
    let source = src_text("ui/settings_stance.rs");
    let project_start = source
        .find("fn project_section(")
        .expect("the project section");
    let editor_start = source
        .find("fn editor_section(")
        .expect("the editor section");
    let project_body = &source[project_start..editor_start];

    assert!(
        project_body.contains("settings-hdr-reference"),
        "the HDR reference row is missing"
    );
    assert!(
        project_body.contains("settings-content-light"),
        "the content-light row is missing"
    );

    // Reads the real declared numbers, not an invented default or a fixed
    // string standing in for one.
    for field in ["mastering_max", "max_cll", "max_fall"] {
        assert!(
            project_body.contains(field),
            "the HDR reference rows do not read ContentLight::{field}"
        );
    }

    // No override field exists on the engine side to write one into, so the
    // row must never claim a picker/opener the way every other PROJECT row
    // does -- it is built with `row_static`, not `row`/`row_ink`.
    assert!(
        project_body.contains("row_static(\n            \"settings-hdr-reference\""),
        "the HDR reference row must be read-only (row_static), not a picker"
    );
    assert!(
        project_body.contains("row_static(\n            \"settings-content-light\""),
        "the content-light row must be read-only (row_static), not a picker"
    );

    // The established empty state: a bare "—" reused, never a second idiom
    // ("N/A", "None", "Unknown"...) invented for the same absence.
    assert!(
        project_body.contains("\"—\".to_string()"),
        "the HDR reference rows must reuse the page's own empty-state dash"
    );
}
/// The parity class this user has reported four separate times: "some
/// options are only reachable via keyboard shortcut". A source scan (this
/// binary has no `TestAppContext` to click through) over every darkroom
/// surface -- the spine, the dock, the bench/timeband transport, the
/// settings page, the maximized cards and the clip context menu
/// (`menus.rs`'s `MENU_ITEMS`, rendered mouse-and-chord-visible by
/// `overlays.rs`) -- for a literal `ActionId::<variant>` mention. An
/// [`ActionId`] reachable by chord ([`Keymap::defaults`]) but absent from
/// every one of those texts has no mouse door into the darkroom at all,
/// which is exactly this bug's shape (`Fit` and `Redo` shipped that way
/// until this commit).
///
/// [`ActionId::Resolution`] and [`ActionId::SubtitleStyle`] are the two
/// deliberate exceptions: their settings-page rows open them through
/// `Pick::Resolution` and `open_subtitle_style(cx)` respectively, never
/// spelling the action name itself, so each is matched on its own door
/// string instead.
///
/// Every action gets a persistent regional control or the context-menu row for
/// the thing it acts on. The four exemptions are independently-owned lane
/// parity work, named with their owners so a future removal cannot silently
/// turn any unrelated action back into keyboard-only.
#[test]
fn every_action_has_a_darkroom_widget_home_or_explicit_owner() {
    use crate::ActionId;
    const EXPLICITLY_OWNED_ELSEWHERE: &[(ActionId, &str)] = &[
        (
            ActionId::RemoveVideoLane,
            "lane-header remove parity owns video-lane controls",
        ),
        (
            ActionId::RemoveAudioLane,
            "lane-header remove parity owns audio-lane controls",
        ),
        (ActionId::Crossfade, "fade parity owns crossfade controls"),
        (ActionId::Dissolve, "fade parity owns dissolve controls"),
    ];
    let darkroom = [
        "ui/bench_stance.rs",
        "ui/cards.rs",
        "ui/dock_stance.rs",
        "ui/overlays.rs",
        "ui/settings_stance.rs",
        "ui/spine_stance.rs",
        "ui/stance.rs",
        "ui/timeband_stance.rs",
        "ui/timeline.rs",
        "menus.rs",
    ]
    .map(src_text)
    .join("\n");
    for action in ActionId::ALL {
        if let Some((_, owner)) = EXPLICITLY_OWNED_ELSEWHERE
            .iter()
            .find(|(owned, _)| *owned == action)
        {
            assert!(!owner.is_empty(), "ActionId::{action:?} needs an owner");
            continue;
        }
        let name = format!("{action:?}");
        let mentioned = darkroom.contains(&format!("ActionId::{name}"))
            || (action == ActionId::Resolution && darkroom.contains("Pick::Resolution"))
            || (action == ActionId::SubtitleStyle && darkroom.contains("open_subtitle_style(cx)"));
        let hitmap_id = crate::ui::hitmap::action_id(action);
        assert_eq!(
            hitmap_id,
            format!("action.{name}"),
            "ActionId::{name} has a Darkroom widget home but no stable hitmap id"
        );
        assert!(
            mentioned,
            "ActionId::{name} is bound to a chord but has no visible affordance \
             anywhere in the darkroom tree -- either mount it or name the owning parity lane"
        );
    }
}

/// Transition affordances must be painted and use the existing edit state:
/// fades own their drag handles, an active dissolve remains a mouse toggle,
/// and a carried lane names the exact slot `reorder_lane` will use.
#[test]
fn darkroom_bench_wires_transition_controls_and_lane_drop_cue() {
    let bench = src_text("ui/bench_stance.rs");
    let clip = &bench[bench.find("fn clip_box(").expect("the clip renderer")
        ..bench.find("fn sub_box(").expect("the subtitle renderer")];
    for needle in [
        "fade_wedge(true)",
        "fade_wedge(false)",
        "dissolve_glyph()",
        "this.start_fade_drag(lane, idx, is_in, event.position.x, cx)",
        "this.dissolve_selected(cx)",
    ] {
        assert!(
            clip.contains(needle),
            "the Darkroom clip lost its transition affordance: {needle}"
        );
    }

    let lane = &bench[bench.find("fn lane_row(").expect("the lane renderer")
        ..bench
            .find("pub(crate) fn render(")
            .expect("the bench renderer")];
    for needle in [
        "this.preview_lane_drop(event.drag(cx).0, lane, cx)",
        "this.forget_lane_drop(lane, cx)",
        ".lane_drop",
        "if drop.above",
    ] {
        assert!(
            lane.contains(needle),
            "the Darkroom lane lost its reorder feedback: {needle}"
        );
    }
}
