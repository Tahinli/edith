//! What is drawn and where: the toolbar, the theme, the notices, the lanes
//! and their marks, the tints, the subtitle rows and the sliders.

use super::*;

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
    // Whichever file the root render has come to live in, from the impl to the
    // end of it -- found rather than named, so moving the root does not quietly
    // leave this rule reading a file the render is no longer in.
    let render = source_from("impl Render for Player");
    let dock = src_text("ui/dock_stance.rs");
    let stance = src_text("ui/stance.rs");
    for card in [
        "eq_card",
        "color_card",
        "speed_card",
        "silence_card",
        "mix_card",
        "subtitle_style_card",
    ] {
        // Docked (unmaximized), the card is a section of the dock's own
        // narrow column; maximized, it escapes to `stance-centre`'s window-
        // space positioning context instead -- either way it is mounted
        // through the darkroom's own structure, never straight off the
        // root render.
        assert!(
            dock.contains(&format!("player.{card}("))
                || stance.contains(&format!("player.{card}(")),
            "{card} is not mounted anywhere in the darkroom"
        );
        assert!(
            !render.contains(&format!("self.{card}(")),
            "{card} is drawn from the root again -- it is an overlay over the timeline"
        );
    }
    // The docked cards are placed against the dock's own box, and the
    // maximized escape against `stance-centre` -- both positioning contexts,
    // which is what turns `scrim()` (`absolute().inset_0()`) from a
    // window-wide sheet into a contained one.
    assert!(
        stance.contains(".relative()"),
        "the darkroom has no positioning context: its cards would cover the window"
    );
}

#[test]
fn a_stateful_button_keeps_its_rect() {
    // A toggle's own state must never gate whether the button exists --
    // only how it looks. Flip the state and the pointer's target (the
    // element the hitmap id names, and the rect a click lands on) is
    // unchanged; only the label and the `active` styling read the flag.
    // "eq-spectrum" and "export-cancel" are the darkroom's own stateful
    // toggles: the first flips `self.eq_spectrum`, the second reads
    // `armed` (whether cancel is armed), and neither may gate its own
    // button out of the tree.
    let cards = src_text("ui/cards.rs");
    for (id, flag) in [
        ("\"eq-spectrum\"", "self.eq_spectrum"),
        ("\"export-cancel\"", "armed"),
    ] {
        let at = cards.find(id).unwrap_or_else(|| panic!("no {id} button"));
        let before = &cards[at.saturating_sub(350)..at];
        assert!(
            !before.contains(&format!(".when({flag}"))
                && !before.contains(&format!(".when(!{flag}")),
            "{id} is gated on the very flag it flips -- its rect would move when the state does"
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
        assert!(box_h < lanes_h(2), "no line to pay for at the floor");
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

/// `Player::pick`'s own decision, mirrored here against `Selection` alone
/// (`pick` itself needs a `Context<Player>` this module has no window to
/// build): a plain press on a clip already in a multi-pick must leave the
/// set alone, since the press is where a set-drag begins -- collapsing here
/// would make dragging a member of a selection move only that one clip.
/// Collapsing to the one pressed belongs to a plain click, once it is a
/// click and not a drag (`ui/bench_stance.rs`/`ui/timeline.rs`'s `on_click`).
#[test]
fn a_plain_press_on_an_already_picked_clip_keeps_the_set() {
    use crate::Selection;

    let (v, a, cap) = (
        (Lane::V1, 0),
        (Lane::A1, 0),
        (Lane::new(LaneKind::Subtitle, 0), 0),
    );
    let mut sel = Selection::new();
    sel.toggle(v);
    sel.toggle(a);
    sel.toggle(cap);
    assert_eq!(sel.picks(), &[v, a, cap]);

    let press = |sel: &mut Selection, target, ctrl| match ctrl {
        true => sel.toggle(target),
        false if sel.contains(target) => {}
        false => sel.set_one(target),
    };

    press(&mut sel, a, false);
    assert_eq!(
        sel.picks(),
        &[v, a, cap],
        "a plain press on a member of the set keeps it, for a set-drag to move"
    );

    // A plain press on something outside the set still collapses to it --
    // the ordinary single-select case.
    let outside = (Lane::V1, 1);
    press(&mut sel, outside, false);
    assert_eq!(sel.picks(), &[outside]);
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
    assert_eq!(envelope(&peaks, 0., 5., 1200., 30.).len(), 601);
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
    // An empty timeline says the state, not a count that reads as broken --
    // and DESIGN §8 (2026-08-27) keeps this a state report, not a "drag it"
    // instruction.
    let none = subtitle_toggle_notice(true, 0);
    assert!(!none.contains('0'), "{none} counts nothing at nothing");
    assert!(
        none.contains("nothing placed"),
        "{none} says the state, not the move"
    );
}

/// The door this editor answers "don't make me import the film again" with,
/// and it is in the dock's IMPORT section beside the other two imports
/// (`ui/dock_stance.rs`'s own doc comment: it moved off the old rail's crowded
/// TRACK group here): reads a file's subtitle tracks onto the open timeline
/// -- a release's `.mkv`, an `.srt` beside it -- while the file itself
/// joins nothing. It is the *action* and not a second implementation of it,
/// so the button, the stroke and the actions card cannot drift apart, and
/// it is oracle-gated like every other verb in the dock, so with no timeline
/// open it dims and says why instead of opening a chooser for nothing.
#[test]
fn the_text_tab_carries_the_add_subtitles_door() {
    let dock = src_text("ui/dock_stance.rs");
    let at = dock
        .find("\"dock-import-subtitles\"")
        .expect("no import-subtitles control in the dock");
    let block = &dock[at..(at + 500).min(dock.len())];
    assert!(
        block.contains("ActionId::ImportSubtitles"),
        "the button is a door of its own rather than the action: {block}"
    );
    assert!(
        block.contains("this.act(ActionId::ImportSubtitles"),
        "the button does not dispatch through the action oracle: {block}"
    );
    // `act(ActionId::ImportSubtitles, ...)` routes to `pick_and_add_subtitles`
    // -- the button names the action, not a second implementation of it.
    let actions = src_text("player/actions.rs");
    assert!(
        actions.contains("ActionId::ImportSubtitles => self.pick_and_add_subtitles(cx)"),
        "ImportSubtitles no longer opens the subtitle chooser"
    );
    // `ghost_verb` is the oracle-gated one: dimmed with the refusal in the
    // oracle's own words (`player.enable(action, None)` in its own body).
    assert!(
        dock[..at].ends_with("ghost_verb(\n                    "),
        "the add-subtitles button is not built from the oracle-gated ghost_verb"
    );
    // ...and the empty tab stays a noun, not a sentence pointing at the
    // button (DESIGN §8, 2026-08-27: no instructional copy).
    assert_eq!(crate::LibraryTab::Text.empty(), "No subtitles");
}

/// The Darkroom keeps imported subtitle tracks in the Text tab itself (moved
/// off the unconditional spot under the Sources list, user 2026-08-27), where
/// a row can be selected, folded with its source, removed, and dragged to the
/// bench's existing subtitle-lane target.
#[test]
fn the_darkroom_subtitle_palette_drags_tracks_to_subtitle_lanes() {
    let dock = src_text("ui/dock_stance.rs");
    let palette = &dock[dock
        .find("fn subtitle_tab_rows")
        .expect("no Darkroom subtitle rows")..];
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

/// A subtitle group's header in the Text tab must not vanish on a short
/// window: `subtitle_tab_rows` draws one per group unconditionally -- there
/// is no viewport-height gate on it at all any more, so the list under it
/// is what would scroll instead, never the header itself. And it has to be
/// a real fold, not the click-cycling pattern this codebase has already
/// thrown out once: one click toggles `sub_folded` shut or open, it never
/// steps through more than those two states.
#[test]
fn a_subtitle_group_header_is_never_gated_on_window_height() {
    let dock = src_text("ui/dock_stance.rs");
    assert!(
        !dock.contains("sub_headers_fit"),
        "the header is still gated on the viewport's height"
    );
    // The header is a click target that flips membership in a set --
    // `remove` else `insert` -- not a value stepped through several states.
    let head_at = dock
        .find("\"dock-subtitle-group\"")
        .expect("no id on the group header");
    // Wide enough to reach the count line past the header's styling and its
    // commentary -- a window that stops short reads as the line deleted
    // (the same fixed-window misfire the modal-guard scan had, 2026-08-27).
    let block = &dock[head_at..(head_at + 3600).min(dock.len())];
    assert!(
        block.contains("sub_folded.remove"),
        "no fold-open path: {block}"
    );
    assert!(
        block.contains("sub_folded.insert"),
        "no fold-shut path: {block}"
    );
    assert!(
        block.contains("{track_count} track"),
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

    // The head itself carries a dot, a name and -- on a subtitle lane -- its
    // eye, and nothing else (cleanse round 2). Its mix and its remove are rows
    // in its own right-click menu instead, and they still reach the lane the
    // hand named rather than the last of the kind a bare stroke walks to:
    // `overlays.rs` routes those rows through the same `act_lane` door the
    // head's buttons used.
    let bench = src_text("ui/bench_stance.rs");
    for gone in ["\"bench-mix-lane\"", "\"bench-remove-lane\"", "lane.{}.remove"] {
        assert!(!bench.contains(gone), "the lane head still wears {gone}");
    }
    for door in ["\"bench-show-sub-lane\"", "this.show_sub_lane(lane, cx)"] {
        assert!(bench.contains(door), "lane header lost {door}");
    }
    assert!(
        lane_items(Lane::A1).contains(&ActionId::Mix)
            && lane_items(Lane::A1).contains(&ActionId::RemoveAudioLane)
            && lane_items(Lane::V1).contains(&ActionId::RemoveVideoLane)
            && !lane_items(Lane::V1).contains(&ActionId::Mix),
        "a head verb left the head with no row in the head's own menu"
    );
    assert!(
        src_text("ui/overlays.rs").contains("this.act_lane(action, menu.lane, cx)"),
        "the head menu lane verbs no longer act on the lane that was clicked"
    );
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
    assert!(lanes_box < lanes_h(2), "no line to pay for at the floor");
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

    const BENCH_BORDER_T: f32 = 1.;
    const BENCH_PY_TOP: f32 = 4.;
    // No section head any more (cleanse round 2): the border and the padding
    // are the whole of what `stance::bench` draws above the ruler.
    let real_chrome = BENCH_BORDER_T + BENCH_PY_TOP;
    assert_eq!(
        BENCH_CHROME_H, real_chrome,
        "BENCH_CHROME_H ({BENCH_CHROME_H}) does not match what `stance::bench` \
         actually draws above the content ({real_chrome}px: {BENCH_BORDER_T}px \
         border + {BENCH_PY_TOP}px padding) -- bench_stance::render gets handed the wrong box_h and lays its \
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

/// A round trip through the file: what is saved is what the next load reads
/// back, all five seams touched -- a scratch path, not the real config, the
/// same isolation `keymap::tests`' own `load_from`/`save_to` already takes.
/// One of the five is written past its own ceiling (the library, past
/// `SIDE_MAX_FRAC` of the window it is loaded back into): a file written at
/// a wider window must not hand a narrower one an illegal layout, so the
/// load clamps it back down rather than reading it verbatim.
#[test]
fn a_saved_seam_survives_a_reload() {
    use crate::{
        BENCH_MIN_H, SIDE_MAX_FRAC, Split, Splits, load_stance_splits_from, save_stance_splits_to,
    };
    use gpui::{Pixels, Size, px, size};

    let dir = engine::scratch::Scratch::dir("edith-stance-splits");
    let path = dir.join("stance-splits");
    let window: Size<Pixels> = size(px(1280.), px(720.));

    let mut splits = Splits::default();
    splits.set(Split::Library, 9000.);
    splits.set(Split::Inspector, 200.);
    splits.set(Split::Timeline, 250.);
    splits.set(Split::Dock, 333.);
    splits.set(Split::Bench, 200.);
    save_stance_splits_to(&splits, &path);
    let loaded = load_stance_splits_from(&path, window);
    assert_eq!(
        loaded.get(Split::Library),
        Some(1280. * SIDE_MAX_FRAC),
        "a library past its own ceiling should clamp to it on load"
    );
    assert_eq!(loaded.get(Split::Inspector), Some(200.));
    assert_eq!(loaded.get(Split::Timeline), Some(250.));
    assert_eq!(loaded.get(Split::Dock), Some(333.));
    assert_eq!(loaded.get(Split::Bench), Some(200.));

    // A bench saved below its own floor clamps up to it, the same ceiling
    // logic mirrored on the one seam whose bound is a minimum.
    let mut low_bench = Splits::default();
    low_bench.set(Split::Bench, 1.);
    save_stance_splits_to(&low_bench, &path);
    let loaded = load_stance_splits_from(&path, window);
    assert_eq!(loaded.get(Split::Bench), Some(BENCH_MIN_H));
}

/// The guard [`crate::split_drag_owes_save`] runs at the moment a drag ends
/// with no further pointer event ever coming (`Player::drag_left_window`,
/// wired to a live `MouseExitEvent` a `TestAppContext`-less test binary
/// cannot raise -- see `tests/media.rs`'s own note on the same limit, and
/// the harness drive `D2` in this session's report for the wiring itself).
/// What *is* checkable here without a window: exactly the persisted
/// seams owe that save, the same set `Split::PERSISTED` already names, and a
/// drag that never started (`None`) owes nothing.
#[test]
fn only_the_persisted_seams_owe_a_save_when_a_drag_loses_the_window() {
    use crate::{Split, player::timeline_edit::split_drag_owes_save};

    assert!(split_drag_owes_save(Some(Split::Dock)));
    assert!(split_drag_owes_save(Some(Split::Bench)));
    assert!(split_drag_owes_save(Some(Split::Library)));
    assert!(split_drag_owes_save(Some(Split::Inspector)));
    assert!(split_drag_owes_save(Some(Split::Timeline)));
    assert!(!split_drag_owes_save(None));
}

/// A missing file leaves every region at its default -- the silent fallback
/// `load_stance_splits`'s doc comment promises.
#[test]
fn a_missing_stance_splits_file_leaves_every_region_at_its_default() {
    use crate::{Split, load_stance_splits_from};
    use gpui::{Pixels, Size, px, size};

    let dir = engine::scratch::Scratch::dir("edith-stance-splits-missing");
    let window: Size<Pixels> = size(px(1280.), px(720.));
    let splits = load_stance_splits_from(&dir.join("nothing-here"), window);
    assert_eq!(splits.get(Split::Library), None);
    assert_eq!(splits.get(Split::Inspector), None);
    assert_eq!(splits.get(Split::Timeline), None);
    assert_eq!(splits.get(Split::Dock), None);
    assert_eq!(splits.get(Split::Bench), None);
}

/// Every seam in the main layout has a handle on it, and every region draws
/// itself at the size that handle sets: a region still measuring itself off the
/// window's own share is a panel whose divider moves nothing.
#[test]
fn every_seam_in_the_layout_has_a_divider_on_it() {
    // The darkroom collapsed Library/Inspector/Timeline into rooms with no
    // seam of their own; render.rs no longer draws any divider at all --
    // ui::stance::render is the whole root now -- so the only two seams a
    // hand can still drag are the ones the stance itself owns.
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
    // ...and nothing lays a region out off the untouched share any more:
    // the legacy panels that read `library_w`/`inspector_w` (`ui/library.rs`,
    // `ui/inspector.rs`, `ui/toolbar.rs`) are gone or gone from the render
    // path, so what remains is layout.rs's own Split::Library/Inspector
    // arms -- kept for save-file compat, never drawn.
    assert!(
        !src_text("render.rs").contains("library_w("),
        "render.rs still measures a panel with library_w("
    );
}

/// DESIGN.md §12 step 2 as amended 2026-09-09 (user decision "option C"):
/// the stance skeleton draws its five regions -- screen, time band, bench,
/// ledger, dock -- in the order §5's diagram lays them out, and
/// `Player::render` actually reaches it when the flag is on. The sixth, the
/// 56px rail, is deleted -- the room root is centre | dock.
#[test]
fn the_stance_renders_its_five_regions_in_the_documented_order() {
    let stance = src_text("ui/stance.rs");
    let order = [
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
        "the five regions are not defined in DESIGN §5's order: {defined:?}"
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

    // The skeleton is reached unconditionally now -- the darkroom is the
    // only room, and `self.darkroom`'s branch is gone with the legacy tree
    // it used to choose between.
    let render_rs = src_text("render.rs");
    assert!(
        render_rs.contains("ui::stance::render("),
        "render.rs no longer reaches ui::stance::render"
    );
    assert!(
        !render_rs.contains("self.darkroom"),
        "render.rs still branches on self.darkroom after the legacy tree was removed"
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
/// of the frame (rows 300-333 of 335). The legacy full-width `notice_bar`
/// and its non-darkroom caller are gone with the rest of the old tree, and
/// the floating notice plate went after them (user 2026-08-27: the ledger's
/// own "last action" strip is the one notice channel now), so this pins
/// `picture_area` itself never growing a notice surface back.
#[test]
fn the_darkroom_path_never_lets_a_notice_surface_reach_the_picture() {
    // The ledger strip (`ui::stance::ledger` reading `notices.back()`) is
    // the only notice channel left, and the invariant is that
    // picture_area's body never draws one over the picture.
    let body = fn_body("picture_area");
    assert!(
        !body.contains("notice_bar"),
        "picture_area draws a notice surface over the picture again (the old occlusion defect)"
    );
}

/// DESIGN §5/§11 check 6, the fourth occlusion defect (notice bar, export
/// card, preview badge, now menus): every floating menu that can open above
/// the picture -- the clip context menu, the picker -- must size its
/// scrolling list against `stance::menu_floor`'s room, not the raw window.
/// Sizing against the whole viewport is exactly the bug that shipped: a menu
/// taller than the bench/ledger/dock footprint got clamped by `menu_at`'s
/// own bottom-edge fit back up over the picture, because nothing had told
/// the list it only had the footprint to grow into. This pins the general
/// rule so the next surface someone floats -- a fourth menu, a fifth
/// occlusion -- cannot silently reintroduce `menu_rows_h(rows.len(),
/// viewport)` in its place.
///
/// The library menu is the deliberate exception (user 2026-08-27, "right
/// clicking a library media opens a menu in somewhere nonsense"): it anchors
/// to a pointer that is always inside the dock, where no picture exists to
/// protect, and `menu_floor`'s clamp teleported it down to the picture floor
/// -- so `ui/library.rs` must open at the pointer through `menu_at` alone,
/// and this guard pins that it never routes back through `menu_floor`.
#[test]
fn every_darkroom_menu_sizes_its_list_against_the_floor_room_not_the_raw_viewport() {
    for file in ["ui/overlays.rs"] {
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
    let library = src_text("ui/library.rs");
    assert!(
        !library.contains("menu_floor("),
        "ui/library.rs routes its menu through menu_floor's picture-floor \
         clamp again -- a right-click high in the dock teleports back down \
         to the picture floor (the 'menu in somewhere nonsense' defect)"
    );
    assert!(
        library.contains("menu_at(at, viewport, h)")
            && library.contains("point(menu.at.x, menu.at.y + px(ROW_CLEAR))"),
        "ui/library.rs no longer anchors its menu to the pointer -- one row \
         clear of it, so the menu cannot cover the row it names -- through \
         menu_at; update this guard"
    );
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
/// surface -- the dock, the bench/timeband transport, the
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
            ActionId::FocusPanels,
            "keyboard-only by design: a mouse already focuses whatever it \
             clicks, so a click-through door for \"move keyboard focus\" \
             has nothing to do that a click has not already done",
        ),
        (
            ActionId::Deselect,
            "clicking empty timeline space already clears the selection with the mouse; \
             bare escape is a keyboard-only accelerator for that existing door, not a new one",
        ),
        (
            ActionId::SelectNext,
            "clicking a clip already selects it with the mouse; `}` is a keyboard-only \
             accelerator for cycling that existing selection, not a new door -- legacy had no \
             toolbar button for it either",
        ),
        (
            ActionId::SelectPrev,
            "clicking a clip already selects it with the mouse; `{` is a keyboard-only \
             accelerator for cycling that existing selection, not a new door -- legacy had no \
             toolbar button for it either",
        ),
        // The group trio: a group is *made* with the pointer already --
        // ctrl-click the halves and the grammar is the selection itself
        // (DESIGN §9) -- so a row per verb in the clip menu was three rows
        // saying what the click had already said. Chords and KEYS rows keep
        // them reachable.
        (
            ActionId::Group,
            "made by ctrl-clicking the halves: the selection is the grammar",
        ),
        (
            ActionId::Detach,
            "made by ctrl-clicking the halves: the selection is the grammar",
        ),
        (
            ActionId::Regroup,
            "made by ctrl-clicking the halves: the selection is the grammar",
        ),
        // The keyboard's own trim: `^[` and `^]` do with a stroke what the
        // pointer does by dragging the clip edge to the spot it wants, and
        // that drag is the door -- a menu row for it would be a second name
        // for a gesture the bench already answers.
        (
            ActionId::TrimInToPlayhead,
            "the pointer's version is dragging the clip edge to the playhead",
        ),
        (
            ActionId::TrimOutToPlayhead,
            "the pointer's version is dragging the clip edge to the playhead",
        ),
        // The rail's own eighteen are gone from this list: the right-click
        // parity (DESIGN §5 as amended 2026-09-09, user decision "option C")
        // gave every one of them a door on the thing it acts on -- the clip
        // menu, the ruler's, the lane head's -- and the staleness check below
        // is what deleted their entries as each landed.
        (
            ActionId::Screenshot,
            "the picture the frame is written from is the *watched* frame and \
             nothing else is under the pointer to right-click for it: keyboard \
             and the KEYS card, like the selection accelerators above",
        ),
    ];
    let darkroom = [
        "ui/bench_stance.rs",
        "ui/cards.rs",
        "ui/dock_stance.rs",
        "ui/overlays.rs",
        "ui/settings_stance.rs",
        "ui/stance.rs",
        "ui/timeband_stance.rs",
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
            // An exemption is a debt, not a licence: once the owning lane
            // has mounted the action, the entry has to go, or the sweep
            // goes blind on an action that is covered.
            assert!(
                !darkroom.contains(&format!("ActionId::{action:?}")),
                "ActionId::{action:?} has a darkroom home now -- delete its \
                 EXPLICITLY_OWNED_ELSEWHERE entry ({owner})"
            );
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
    let dissolve = &clip[clip
        .find(".when(dissolves, |d| {")
        .expect("the dissolve affordance")
        ..clip.find(".when(trims(span),").expect("the fade handles")];
    assert!(
        dissolve.contains(".w(px(FADE_HANDLE_W))"),
        "dissolve click target no longer matches fade-handle width"
    );
    assert!(
        dissolve.contains(".h(px(FADE_HANDLE_H))"),
        "dissolve click target no longer matches fade-handle height"
    );
    assert!(
        !dissolve.contains("h_full()"),
        "dissolve click target still covers the clip body"
    );
    assert!(
        !dissolve.contains(".w(px(scale"),
        "dissolve hit test still tracks transition width"
    );

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

/// The pointer harness can only drive controls that report the bounds of the
/// element that owns the gesture. Keep every dynamic Darkroom entry surface
/// named, so UI verification never falls back to guessed pixels.
#[test]
fn hitmap_names_every_darkroom_pointer_entry_surface() {
    let bench = src_text("ui/bench_stance.rs");
    for needle in [
        "clip.{}.{}.{}.fade-{}",
        "clip.{}.{}.{}.trim-{}",
        "clip.{}.{}.{}.dissolve",
        "lane.{}.reorder",
        "lane.{}.eye",
    ] {
        assert!(
            bench.contains(needle),
            "bench control missing hitmap id: {needle}"
        );
    }
    assert!(
        bench.contains(".when(trims(span), |d| {"),
        "fade handles must cover video clips as well as audio clips"
    );

    let dock = src_text("ui/dock_stance.rs");
    for needle in [
        "subtitle.{group_ord}.{track}.row",
        "subtitle.{group_ord}.{track}.select",
        "source.{i}.preview",
        "source.{i}.proxy",
        "Proxy making",
    ] {
        assert!(
            dock.contains(needle),
            "dock control missing hitmap contract: {needle}"
        );
    }

    for (path, needles) in [
        (
            "ui/timeband_stance.rs",
            &[
                "timeline.contact-strip",
                "stance-strip-grip-lt",
                "stance-strip-grip-rb",
            ][..],
        ),
        ("ui/preview.rs", &["preview.stop", "preview.scrub"][..]),
        (
            "ui/overlays.rs",
            &[
                "menu.{action:?}",
                "menu.properties",
                "theme.{n}.row",
                "let enabled = refusal.yes()",
                "hitmap::dynamic(\n                            move || (format!(\"menu.{action:?}\"",
            ][..],
        ),
        (
            "ui/settings_stance.rs",
            &[".children(hitmap::control(id, label_text, !exporting))"][..],
        ),
    ] {
        let source = src_text(path);
        for needle in needles {
            assert!(
                source.contains(needle),
                "{path} missing hitmap contract: {needle}"
            );
        }
    }
}

/// An external file drag (gpui `ExternalPaths`, Wayland `text/uri-list`) has
/// to be heard over the DOCK -- the Sources/library panel -- and not only
/// over the centre column: the dock is the centre's flex sibling, so the
/// handler `stance-centre` used to carry covered none of
/// the library panel, which is exactly the shipped "drag and drop file
/// import is not working on library panel" defect. The listener therefore
/// belongs to the room root (`stance-room`), above every region, and it must
/// route the dropped paths through `import` -- the same door the Import
/// action uses.
///
/// A scan, not a click: this crate has no gpui `VisualTestContext` harness
/// (nothing in `tests/` opens a window), and a drop cannot be simulated
/// without one -- the compositor half (`wl_data_device`) is out of reach of
/// the test process either way.
#[test]
fn an_external_file_drop_is_heard_by_the_whole_room_and_imports() {
    let stance = src_text("ui/stance.rs");
    let root_at = stance
        .find(".id(\"stance-room\")")
        .expect("no stance root div");
    let centre_at = stance
        .find(".id(\"stance-centre\")")
        .expect("no stance centre column");
    let drop_at = stance
        .find(".on_drop(cx.listener(|this, paths: &gpui::ExternalPaths")
        .expect("no external-file drop handler in the stance");
    assert!(
        drop_at > root_at && drop_at < centre_at,
        "the external drop handler is not on the room root -- \
         a drop over the dock's library panel is heard by nothing"
    );
    let body = &stance[drop_at..(drop_at + 700).min(stance.len())];
    assert!(
        body.contains("this.import(path, cx)"),
        "a dropped file does not go through the import door: {body}"
    );
    // ...and the library panel says so while the file is over it.
    let dock_at = stance.find(".id(\"stance-dock\")").expect("no dock div");
    let dock = &stance[dock_at..(dock_at + 700).min(stance.len())];
    assert!(
        dock.contains(".drag_over::<gpui::ExternalPaths>"),
        "the dock answers an external file drag with nothing: {dock}"
    );
}

/// The wheel is the bench's, not a clip's: the user rolled it over the bench
/// and nothing moved unless the pointer happened to sit on a track, because
/// the only listeners were one per bed and one on the ruler strip. The row's
/// listener now sits on the whole row (head column included) and stops there,
/// and `bench-content` answers everything else -- the strip left of the ruler,
/// the ruler itself, the gaps between rows, the space below the last track.
/// Exactly two listeners, so no strip answers one notch twice.
#[test]
fn the_bench_answers_a_wheel_notch_anywhere_over_it() {
    let bench = src_text("ui/bench_stance.rs");
    assert_eq!(
        bench.matches(".on_scroll_wheel(").count(),
        2,
        "the bench has a wheel listener per region again -- a notch would be answered twice"
    );
    let row = bench
        .find(".id((\"bench-lane\", lane.ord")
        .expect("no lane row");
    let bed = bench.find(".id((\"bench-bed\"").expect("no lane bed");
    let row_wheel = bench[row..bed]
        .find(".on_scroll_wheel(")
        .map(|at| &bench[row + at..bed])
        .expect("the lane row does not answer the wheel -- only its bed does");
    assert!(
        row_wheel[..200].contains("cx.stop_propagation();")
            && row_wheel[..200].contains("this.timeline_wheel(event, cx)"),
        "the row's wheel lost the mapping or its stop: {}",
        &row_wheel[..200]
    );
    let content = bench
        .find(".id(\"bench-content\")")
        .expect("no bench content");
    let content_wheel = bench[content..]
        .find(".on_scroll_wheel(")
        .expect("the bench container does not answer the wheel");
    assert!(
        content_wheel < 700 && bench[content..].contains("this.timeline_wheel(event, cx)"),
        "the container's wheel listener is not on the container itself"
    );
    let ruler = bench.find(".id(\"bench-ruler\")").expect("no ruler");
    let lanes = bench.find(".id(\"bench-lanes\")").expect("no lane column");
    assert!(
        !bench[ruler..lanes].contains(".on_scroll_wheel("),
        "the ruler answers the wheel on its own again -- one notch, two scrolls"
    );
}

/// The way back from a layout dragged somewhere unusable: a double press on
/// the seam forgets the size, and the region is the window's own share again
/// -- the *same* share an untouched window gives it, not a second default
/// kept beside the first ([`Splits::clear`]).
#[test]
fn a_double_pressed_divider_forgets_the_size_it_was_dragged_to() {
    use crate::layout::{Split, Splits, split_size};
    use gpui::{px, size};

    let window = size(px(1280.), px(720.));
    let mut splits = Splits::default();
    for split in Split::PERSISTED {
        let default = split_size(split, None, 2, window, false);
        // A hand takes the seam somewhere else...
        splits.set(split, split_size(split, Some(300.), 2, window, false));
        assert_ne!(
            splits.get(split),
            None,
            "{split:?} kept nothing of the drag"
        );
        // ...and the double press puts it back where an untouched window
        // would have drawn it.
        splits.clear(split);
        assert_eq!(splits.get(split), None, "{split:?} still holds a size");
        assert_eq!(
            split_size(split, splits.get(split), 2, window, false),
            default,
            "{split:?} came back to something other than its own share"
        );
    }
}

/// The seam is a ghost (DESIGN §11.2): 6 px of hit area with *nothing* painted
/// at rest, one hairline of dim ink under the pointer, the same line one step
/// brighter while it is held. The scan is the guard because the failure it
/// catches -- a `bg` on the strip, which is what the divider shipped with --
/// draws a 6 px band of colour down the room at rest and reads as the chrome
/// the user called crowded.
#[test]
fn the_seam_paints_nothing_until_a_pointer_finds_it() {
    let src = src_text("interact.rs");
    let start = src.find("pub(crate) fn divider(").expect("the divider");
    let body = &src[start..start + src[start..].find("\n}\n").expect("its end")];
    assert!(
        !body.contains(".bg("),
        "the divider fills its strip at rest: {body}"
    );
    assert!(
        body.contains("gpui::transparent_black()"),
        "the divider's line is inked at rest: {body}"
    );
    for step in [
        ".group_hover(",
        "GRAB_W",
        "BENCH_GRAB_H",
        "STROKE_DIVIDER()",
        "INK3()",
        "border_t_1()",
        "border_l_1()",
    ] {
        assert!(body.contains(step), "the divider is missing {step}");
    }
    // The hairline is the *band's* child, not the strip's own border: a line
    // raised only by the 6 px that already grab the seam is feedback the hand
    // gets after it no longer needs it (his `bench=207` -- a press with no
    // drag in it -- beside a `dock=277` he did drag).
    assert!(
        body.find(".group(group.clone())").expect("the band's group")
            < body.find("border_t_1()").expect("the hairline"),
        "the hairline is painted outside the band: {body}"
    );
}

/// The band the hand actually gets, in seam coordinates. It once reached 13px
/// past the line to cover the `BENCH` label row a hand aimed at; that row is
/// gone (cleanse round 2) and the ruler under the seam answers presses of its
/// own (it scrubs), so the bench seam takes every other seam's reach and no
/// more -- a deeper band would eat scrubs at the film's own top edge.
#[test]
fn the_bench_seam_reaches_no_further_than_every_other_seam() {
    let above = 1.;
    let reach = |grab: f32| grab - above - crate::layout::SPLIT_W;
    assert_eq!(reach(crate::layout::GRAB_W), 7.);
    assert_eq!(
        reach(crate::layout::BENCH_GRAB_H),
        7.,
        "the bench band reaches past the ruler row, which answers presses itself"
    );
}


/// The clamp table, driven: the seam refuses exactly two things, and neither
/// is anywhere near where a hand drags. Measured live at 2560x1440 --
/// `bench=105` at the floor, `bench=1164` at the ceiling.
#[test]
fn the_bench_seam_stops_at_its_floor_and_its_ceiling() {
    use crate::{BENCH_MIN_H, Split, split_bounds, split_size};
    use gpui::{px, size};

    let window = size(px(2560.), px(1440.));
    let (min, max) = split_bounds(Split::Bench, 2, window, false);
    assert_eq!(min, BENCH_MIN_H);
    assert_eq!(
        max,
        1440. - crate::ui::stance::TIME_BAND_H - crate::ui::stance::LEDGER_H - 160.
    );
    for (asked, want) in [(0., min), (-942., min), (5000., max), (207., 207.)] {
        assert_eq!(
            split_size(Split::Bench, Some(asked), 2, window, false),
            want,
            "a bench dragged to {asked}"
        );
    }
}

/// DESIGN §7: "lanes scroll behind the pinned ruler and track heads" -- pinned
/// means pinned. A clip that starts left of the view sits at a negative `left`
/// inside its bed, and with no clip mask gpui paints it (and its hitbox) over
/// the 72 px head column, so V1/A1 and their verbs disappear under the first
/// take as soon as the bench is wheeled past frame 0. The mask belongs on the
/// bed itself so a half-scrolled clip still drags on the half that shows.
#[test]
fn the_lane_bed_clips_its_clips_at_the_pinned_heads() {
    let src = src_text("ui/bench_stance.rs");
    let start = src.find(r#".id(("bench-bed""#).expect("the bed");
    let bed =
        &src[start..start + src[start..].find(".bg(rgb(DARK_CANVAS()))").expect("its fill")];
    assert!(
        bed.contains(".overflow_hidden()"),
        "the bed lets its clips paint outside itself, straight over the \
         pinned lane heads: {bed}"
    );
}

/// A source row is its name first (user 2026-09-09: "clunky, crowded and
/// problematic"). At DOCK_W 280 the name used to be the only flexible child
/// among six, so it measured 0px and the library showed
/// `● V1 A1 · 2 uses Preview Add ↵ ○` -- every part of a row except the one
/// thing the row is for. The fix is subtraction, not a hover gate (DESIGN §8):
/// the name keeps 60% of the row, usage joins the metadata line under it, and
/// the three verbs shrink to glyph ghosts at the right edge.
#[test]
fn a_source_row_shows_its_name_before_anything_else() {
    let dock = src_text("ui/dock_stance.rs");
    let start = dock.find("fn source_row(").expect("the source row");
    let row = &dock[start..dock.find("/// Imported subtitle tracks").expect("its end")];
    assert!(
        row.contains(".min_w(relative(0.6))"),
        "the row name can still collapse under its siblings"
    );
    assert!(
        !row.contains(".min_w(px(0.))"),
        "the row name is still allowed to measure nothing"
    );
    // The usage moved to the second line, beside codec/length, in ink3.
    assert!(
        row.contains(r#".child(format!("{usage} · {under}"))"#),
        "usage is not folded into the metadata line"
    );
    // Preview / Add / proxy are glyph+chord ghosts now, not word buttons:
    // ~66px for the three together, inside the 96px the name can spare.
    for glyph in [r#".child("▷")"#, r#".child("+")"#, r#".child("↵")"#] {
        assert!(row.contains(glyph), "the right-edge verbs lost {glyph}");
    }
    for word in [r#".child("Preview")"#, r#".child("Add")"#] {
        assert!(!row.contains(word), "{word} still spends the name's width");
    }
    // Every verb keeps its own hitmap name and its chord.
    for id in ["source.{i}.preview", "source.{i}.add", "source.{i}.proxy"] {
        assert!(row.contains(id), "{id} lost its hitmap entry");
    }
    // DESIGN §8: the permanent footer telling the editor how to use a list
    // ("drag · ↵ add · double-click plays") is instructional copy and is gone.
    assert!(
        !dock.contains("double-click plays"),
        "the dock still carries an instruction footer"
    );
    // ...and an Audio tab with no audio-only file in it is a single noun, not
    // the claim `No sound` over a source whose A1 stream is on the bench.
    assert_eq!(crate::LibraryTab::Audio.empty(), "none");
}

/// A room key pressed while the ring sits in the dock has to reach the room.
/// Two ways it did not: the dock's own handler could stop a key it does not
/// answer, and the body the dock mounts could leave the focused
/// `FocusHandle` with no element -- gpui dispatches only along the rendered
/// focus node's ancestors (`gpui-0.2.2` `window.rs:3982`, falling back to the
/// *window* root, which is not the room's div), so an orphaned handle kills
/// every key until the next click. Measured 2026-09-09: with KEYS open, a
/// second `?` and `space` reached no listener at all.
#[test]
fn every_dock_body_mounts_the_ring_it_is_standing_in_for() {
    let dock = src_text("ui/dock_stance.rs");
    for (body, id) in [
        ("sources", "\"dock-sources\""),
        ("clip", "\"dock-clip\""),
        ("keys", "\"dock-keys-rows\""),
    ] {
        let after = dock
            .split(id)
            .nth(1)
            .unwrap_or_else(|| panic!("the dock's {body} body is gone"));
        let head: String = after.lines().take(4).collect();
        assert!(
            head.contains(".track_focus(") && head.contains(".on_key_down("),
            "the dock's {body} body mounts no focus handle: a ring left on it dies"
        );
    }
    // ...and the shared handler answers exactly two keys, stopping the stroke
    // only there; anything else bubbles to `ui::stance::render`'s handler.
    let handler = dock
        .split("fn cycle_on_key_down(")
        .nth(1)
        .expect("the dock's shared key handler is gone");
    let body = handler.split("\n}\n").next().unwrap();
    assert_eq!(
        body.matches("cx.stop_propagation()").count(),
        3,
        "the dock's key handler stops a stroke outside its tab/escape branches"
    );
    for branch in ["is_focus_cycle_key(key)", "is_focus_exit_key(key)"] {
        assert!(body.contains(branch), "{branch} left the dock's key handler");
    }
}

/// Focus and selection rings are drawn, never inserted: a `when(focused,
/// border_1())` puts a pixel of box into the flow and every row in the dock
/// steps sideways the moment the surface takes focus (hunter: h-clip-tab.png
/// vs i-library-menu.png).
#[test]
fn a_dock_ring_never_moves_what_it_rings() {
    let dock = src_text("ui/dock_stance.rs");
    for gate in [
        ".when(picked, |d| d.border_1()",
        ".when(focused, |d| d.border_1()",
    ] {
        assert!(
            !dock.contains(gate),
            "a dock ring is still added on state ({gate}), shifting its content"
        );
    }
    assert_eq!(
        dock.matches("gpui::transparent_black()").count(),
        4,
        "a dock ring lost its at-rest transparent border"
    );
    // ...and the focus ring is not painted at all (user 2026-09-09:
    // "clicking through timeline draws a white overlay around the timeline,
    // same happens for library section too"). Keyboard focus is unchanged --
    // only the paint is gone -- so no dock surface may reach for the focus
    // stroke or for `ink1` on a focused border.
    assert!(
        !dock.contains("STROKE_FOCUS"),
        "a dock surface paints the focus ring again"
    );
    assert!(
        !dock.contains("is_focused(window)"),
        "a dock surface reads focus for paint again"
    );
    assert!(
        dock.contains(".track_focus(&player.focus_dock)")
            && dock.contains(".track_focus(&player.focus_inspector)"),
        "dropping the ring's paint took the surface's keyboard focus with it"
    );
}

/// DESIGN §9 as amended 2026-09-09: the keys list lives in the dock BODY,
/// not a plate over the bench and not a tab of its own (user: "remove keys
/// section from here since we also have it next to ledger"). The strip is
/// the SOURCES/CLIP pair; the list rides `keys_open` under a KEYS section
/// head, and the rows still come off `keys_rows` with the refused ones
/// greyed rather than hidden (§8).
#[test]
fn the_keys_list_is_dock_body_state_not_a_tab() {
    let stance = src_text("ui/stance.rs");
    assert!(
        !stance.contains("keys_overlay") && !stance.contains("stance-keys-overlay"),
        "the keys plate is still mounted over the bench"
    );
    let dock = src_text("ui/dock_stance.rs");
    for (id, label) in [("dock-tab-src", "SOURCES"), ("dock-tab-clip", "CLIP")] {
        assert!(
            dock.contains(&format!("dock_tab(\"{id}\", \"{label}\"")),
            "the dock's tab strip lost {label}"
        );
    }
    assert!(
        !dock.contains("dock-tab-keys"),
        "KEYS is still a tab in the strip"
    );
    assert!(
        dock[dock.find("fn keys_tab(").unwrap()..].contains("section_head(\"KEYS\")"),
        "the keys list draws no KEYS head in the dock body"
    );
    assert!(
        dock.contains("fn keys_tab(") && dock.contains("keys_rows()"),
        "the KEYS tab draws something other than the action registry"
    );
    assert!(
        dock.contains("Enable::Yes => INK2(),") && dock.contains("_ => INK4(),"),
        "a refused action's row no longer greys to ink4 (DESIGN §8)"
    );
    assert!(
        dock[dock.find("fn keys_tab(").unwrap()..].contains(".overflow_y_scroll()"),
        "the keys list does not scroll like its neighbours"
    );
    // The Sources filter box belongs to Sources: it is built inside
    // `sources_tab`, never in the shared frame `render` draws.
    let frame = &dock[dock.find("pub(crate) fn render(").unwrap()..];
    assert!(
        !frame.contains("dock-filter"),
        "the Sources filter box leaked into the shared dock frame"
    );
}

/// The user's screenshot: the old rail ended with `?` over `?`. The rule
/// outlived the rail -- a ledger room ghost whose primary chord IS its own
/// name wears no badge; every other one still does.
#[test]
fn a_room_ghost_wears_no_chord_that_repeats_its_name() {
    let src = src_text("ui/stance.rs");
    assert!(
        src.contains(".when(player.keymap.chord(action) != name, |el| {"),
        "a room ghost draws its chord badge even when the badge repeats its name"
    );
}

/// DESIGN §5 as amended 2026-09-09 (user decision "option C"): there is no
/// rail. The room root is centre | dock, `spine_stance` is deleted, and the
/// four verbs that act on the ROOM -- the only ones with no thing under the
/// cursor to be right-clicked -- live at the ledger's right end, each
/// wearing its chord, `CC` carrying the shown/hidden state the rail's glyph
/// used to.
#[test]
fn the_ledger_carries_the_rooms_four_verbs_and_no_rail_remains() {
    let src = src_text("ui/stance.rs");
    for gone in ["stance-spine", "spine_stance", "SPINE_W", "fn spine("] {
        assert!(!src.contains(gone), "the rail is still in the stance: {gone}");
    }
    let ledger = &src[src.find("fn ledger(").expect("the ledger strip")
        ..src.find("/// The dock:").expect("the dock frame")];
    for (name, action) in [
        ("CC", "ActionId::ToggleSubtitles"),
        ("settings", "ActionId::Settings"),
        ("keys", "ActionId::ShowActions"),
        ("full", "ActionId::Fullscreen"),
    ] {
        assert!(
            ledger.contains(&format!("\"{name}\",")) && ledger.contains(action),
            "the ledger lost its {name} room verb"
        );
    }
    // Before the position timecode, which stays the strip's last word.
    assert!(
        ledger.find("room_ghost(").expect("a room verb")
            < ledger.find(".child(tc),").expect("the position"),
        "the room verbs drew after the position timecode"
    );
    // Each wears its chord, live off the keymap (DESIGN §4), and `CC` reads
    // active while subtitles are shown.
    let ghost = &src[src.find("fn room_ghost(").expect("the room ghost")..];
    assert!(
        ghost.contains("player.keymap.chord(action)") && ghost.contains("widgets::action_hover"),
        "a room verb lost its live chord or its hover plate"
    );
    assert!(
        ledger.contains("player.subs_on"),
        "`CC` no longer shows whether subtitles are shown"
    );
}

/// The `all` control left the ruler (cleanse round 2, DESIGN §5): the ruler
/// row is the bench's own top edge and carries ticks and the playhead alone.
/// Every removed control keeps a door -- `^a` still fires it, and the ruler's
/// own right-click menu ([`BENCH_ITEMS`]) is where the pointer reaches it --
/// so this checks the control is gone AND that the row exists, not just the
/// deletion.
#[test]
fn select_all_left_the_ruler_for_the_rulers_own_menu() {
    use crate::ActionId;
    use crate::menus::BENCH_ITEMS;
    let bench = src_text("ui/bench_stance.rs");
    for gone in ["bench-select-all", "select_all_band", "SELECT_ALL_PAD"] {
        assert!(!bench.contains(gone), "the ruler still carries {gone}");
    }
    assert!(
        BENCH_ITEMS.contains(&ActionId::SelectAll),
        "`all` left the ruler with no menu row behind it"
    );
    // And the band it reserved is the ticks' again: nothing subtracts a
    // control's width from the bed's right edge any more.
    assert!(
        bench.contains("if x >= plate_w && x + label_w <= bed_w {"),
        "the tick loop still keeps a band clear for a control that is gone"
    );
}

/// DESIGN §4/§5: the hero timecode leads the time band and the ledger carries
/// position. The ruler's left plate was a third resting copy of the same
/// reading -- it stays only for the state change it belongs to, a live scrub.
#[test]
fn the_ruler_plate_shows_the_timecode_only_while_a_scrub_is_live() {
    let bench = src_text("ui/bench_stance.rs");
    let at = bench
        .find(".child(playhead_tc)")
        .expect("no playhead timecode in the ruler");
    let before = &bench[..at];
    let guard = before
        .rfind(".when(player.scrubbing")
        .expect("the ruler plate draws its timecode at rest -- the third copy");
    assert!(
        before[guard..].matches(".child(").count() <= 2,
        "the scrubbing guard is not the plate's own: {}",
        &before[guard..guard + 200]
    );
    assert!(
        bench.contains("let plate_w = if player.scrubbing { PLATE_W } else { 0. };"),
        "the ticks still lose the plate's width at rest"
    );
}


/// The band overflowed its own column at 1280x720 (`$EDITH_HITMAP`:
/// `action.Export x=1113 w=80` with the column ending at x=995, the contact
/// strip measured `w=0`), so the Export chip was invisible under the dock and
/// a click on blank dock space at (1130,435) opened the export card. Three
/// things hold that shut: the row clips itself, the strip keeps a floor, and
/// the ladder sheds groups above it.
#[test]
fn the_time_band_never_paints_outside_its_own_column() {
    let src = src_text("ui/timeband_stance.rs");
    let start = src.find(r#".id("stance-time-band-row")"#).expect("the band row");
    let row = &src[start..start + src[start..].find(".child(hero_timecode(").expect("the timecode")];
    assert!(
        row.contains(".overflow_hidden()"),
        "the band row lets its children paint (and be clicked) under the dock: {row}"
    );
    assert!(
        src.contains(".min_w(px(STRIP_MIN_W))"),
        "the contact strip is the flex_1 that absorbs the slack; without a floor it measures 0"
    );
    assert!(crate::ui::timeband_stance::STRIP_MIN_W >= 120.);
}

/// DESIGN §7's ladder, one layer per threshold: the four things the band is
/// *for* -- timecode, cut readout, contact strip, Export -- survive every
/// rung, and the sheddable groups go in a fixed order (chords, then the
/// monitoring cluster, then sync/loop, then the range marks), never all at
/// once. Thresholds are the measured group widths; this pins their order and
/// the 1280x720 column (939px measured when a 56px rail still stood left of
/// it; the rail is gone since 2026-09-09, so the same window measures ~989px)
/// landing on the
/// marks-only rung, which is what makes Export fit there (it ended at x=991
/// inside a column ending at 995, against x=1113 under the dock before).
#[test]
fn the_band_sheds_one_layer_per_threshold_in_a_fixed_order() {
    use crate::ui::timeband_stance::band_layers;
    let ladder = [2000., 1300., 1100., 939., 800., 0.]
        .map(|w| band_layers(w))
        .map(|l| (l.chords, l.volume, l.sync, l.marks));
    assert_eq!(
        ladder,
        [
            (true, true, true, true),
            (false, true, true, true),
            (false, false, true, true),
            (false, false, false, true),
            (false, false, false, false),
            (false, false, false, false),
        ],
        "the band must drop chords, then the monitoring cluster, then sync/loop, then the marks"
    );
    // Monotone: a wider band never shows less than a narrower one.
    let mut prev = band_layers(0.);
    for w in (0..2400).step_by(10).map(|w| w as f32) {
        let now = band_layers(w);
        for (a, b) in [
            (prev.chords, now.chords),
            (prev.volume, now.volume),
            (prev.sync, now.sync),
            (prev.marks, now.marks),
        ] {
            assert!(b || !a, "layer came back off at {w}px");
        }
        prev = now;
    }
}

/// Every door in this band is at least `HIT_MIN` wide and tall (WCAG 2.5.8).
/// The hitmap read `SetIn w=8`, `VolumeDown w=12`, `Loop w=13`, `SetOut w=15`,
/// `Prev/NextSyncPoint w=16` before this: padding on the shared `ghost`, not a
/// bigger glyph.
#[test]
fn every_ghost_in_the_time_band_carries_a_full_hit_area() {
    let src = src_text("ui/timeband_stance.rs");
    let start = src.find("fn ghost(").expect("the band's ghost");
    let ghost = &src[start..start + src[start..].find(".child(glyph.into())").expect("its glyph")];
    for needle in [".min_w(px(HIT_MIN))", ".min_h(px(HIT_MIN))"] {
        assert!(ghost.contains(needle), "the band's ghost lost {needle}: {ghost}");
    }
    assert!(HIT_MIN >= 24.);
}

/// Mute wore `"{volume}%"` as its glyph while the slider beside it drew the
/// same value: one number, two places, and a toggle with no verb of its own.
/// The level is a mono readout beside the slider now, once.
#[test]
fn the_level_is_written_once_and_mute_wears_a_glyph() {
    let src = src_text("ui/timeband_stance.rs");
    assert_eq!(
        src.matches(r#"player.volume.percent()"#).count(),
        1,
        "the volume level must be written in exactly one place in the band"
    );
    assert!(
        src.contains(r#"if player.volume.muted { "◁×" } else { "◁))" }"#),
        "mute must wear a speaker glyph, struck when muted"
    );
    assert!(src.contains("fn volume_readout("), "the level readout is the one place it is written");
}

/// `Play` and `StepForward` drew the same solid right-triangle: two verbs one
/// shape, told apart only by their chords. Step wears the frame-step pair.
#[test]
fn transport_verbs_differ_by_shape_not_only_by_chord() {
    let src = src_text("ui/timeband_stance.rs");
    let glyph = |action: &str| {
        let at = src.find(&format!("ActionId::{action},")).expect("the call");
        src[..at].rsplit('"').nth(1).expect("its glyph").to_string()
    };
    let shapes = ["Play", "StepBack", "StepForward", "JumpBack", "JumpForward"].map(glyph);
    let mut seen = shapes.to_vec();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), shapes.len(), "two transport verbs share one glyph: {shapes:?}");
}

/// DESIGN §6: "the cut readout is the odometer". Keyed off the selection it
/// read `cut —/—` immediately after a split -- two clips on the bench, the
/// playhead resting on the new cut, nothing picked. It reads the playhead
/// now: the cut under it, else the next one ahead, else the selection.
#[test]
fn the_cut_readout_counts_from_the_playhead() {
    use crate::ui::timeband_stance::odometer_cut;
    let at = |start: u32, len: u32| Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start,
        in_frame: 0,
        out_frame: len,
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::Fit,
        speed: Speed::NORMAL,
    };
    let lane = [at(0, 30), at(30, 30)];
    assert_eq!(odometer_cut(&lane, 0), Some(0));
    assert_eq!(odometer_cut(&lane, 29), Some(0));
    // The split's own frame: the playhead rests on the SECOND cut's head.
    assert_eq!(odometer_cut(&lane, 30), Some(1));
    assert_eq!(odometer_cut(&lane, 59), Some(1));
    // Past the last cut there is nothing to count -- the selection answers.
    assert_eq!(odometer_cut(&lane, 60), None);
    // In a gap: the next cut ahead of the playhead.
    let gapped = [at(0, 10), at(40, 10)];
    assert_eq!(odometer_cut(&gapped, 20), Some(1));
    assert_eq!(odometer_cut(&[], 0), None);
    assert!(
        src_text("ui/timeband_stance.rs").contains("odometer_cut(s.lane_clips(lane), frame)"),
        "the readout must derive its odometer from the playhead"
    );
}

/// DESIGN §8's rejected pattern, swept over the source rather than argued
/// per message: "the room never explains itself in prose ... any new string
/// over ~4 words that instructs rather than reports state is a defect". The
/// notices are what the ledger strip paints, and a sentence there is what cut
/// mid-word in the user's own screenshot (`OPENED ... 1 subtitle track(s) in `).
///
/// The scan reads every literal handed to a notice constructor, found by its
/// *call site* and not by its shape: the head of a composed refusal is the
/// verb's own label ("Delete — click a clip first", seen live 2026-09-09), so
/// a caps-headed filter walked past every one of them. Any imperative tell in
/// such a literal is a defect at any length. A refusal *claim* may still be
/// long: those are the allowlist below, each one naming a genuine absent
/// capability, never a lesson in how to use the room.
#[test]
fn no_notice_teaches_the_room_in_prose() {
    // Long refusal claims that report a capability genuinely absent from this
    // machine (DESIGN §8, "a refusal string is a claim"), kept whole on
    // purpose: the remedy is not a chord in this room because the missing
    // thing is not in this room.
    const ALLOWED: &[&str] = &["NO FILE CHOOSER"];
    const TELLS: &[&str] = &[
        "drag ",
        "press ",
        "pick ",
        "type ",
        "click ",
        "to show",
        "to use",
        "takes it back",
        "puts it back",
        " first",
    ];
    let mut offenders = Vec::new();
    for path in source_files() {
        let text = std::fs::read_to_string(&path).expect("a source file");
        for literal in notice_literals(&text) {
            if ALLOWED.iter().any(|allowed| literal.contains(allowed)) {
                continue;
            }
            let lower = literal.to_lowercase();
            if TELLS.iter().any(|tell| lower.contains(tell)) {
                offenders.push(format!("{}: {literal}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a notice that instructs instead of reporting state (DESIGN §8):\n{}",
        offenders.join("\n")
    );
}

/// Every string literal that reaches the user as a notice: the arguments of
/// the doors a message comes through -- the queue's own two, and the oracle's
/// refusal words, which the dispatch pastes behind an action label
/// ([`crate::player::actions`]). Reading the call site rather than the string
/// is the point: a message's case says nothing about whether it teaches.
fn notice_literals(text: &str) -> Vec<String> {
    const CALLS: &[&str] = &[
        "notify_user(",
        "push_notice(",
        "Enable::No(",
        "Enable::Hidden(",
    ];
    let code: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    for call in CALLS {
        for (at, _) in code.match_indices(call) {
            let open = at + call.len();
            // The call's own argument list, to its matching paren: a literal
            // written two lines below the opening one is still this call's.
            let mut depth = 1usize;
            let mut in_string = false;
            let mut escaped = false;
            let mut end = open;
            for (offset, c) in code[open..].char_indices() {
                end = open + offset;
                match (in_string, c) {
                    (true, '\\') => escaped = !escaped,
                    (true, '"') if !escaped => in_string = false,
                    (true, _) => escaped = false,
                    (false, '"') => in_string = true,
                    (false, '(') => depth += 1,
                    (false, ')') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.extend(string_literals(&code[open..end]));
        }
    }
    out
}

/// Every string literal in `text`, comments dropped first so the prose about
/// a message is never read as the message.
fn string_literals(text: &str) -> Vec<String> {
    let code: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut literal = String::new();
        while let Some(c) = chars.next() {
            match c {
                // A line continuation joins the halves of a wrapped message;
                // every other escape is not a word either way.
                '\\' => {
                    if chars.next() == Some('\n') {
                        while chars.peek() == Some(&' ') {
                            chars.next();
                        }
                    }
                }
                '"' => break,
                _ => literal.push(c),
            }
        }
        out.push(literal);
    }
    out
}

/// The export moment holds the file and the budget; everything a project is
/// *delivered as* is a settings row now (user: the export card was "too
/// complicated"). Four rows, and a fifth would be the card growing back --
/// this is a source scan for the same reason every guard on this page is.
#[test]
fn the_export_section_is_four_rows_and_no_fifth() {
    let source = src_text("ui/settings_stance.rs");
    let start = source
        .find("fn export_section(")
        .expect("the export section");
    let body = &source[start..];
    let end = body.find("\n/// The page itself").expect("the page's render fn");
    let body = &body[..end];
    let rows = [
        "settings-export-picture",
        "settings-export-sound",
        "settings-export-encoder",
        "settings-export-range",
    ];
    for row in rows {
        assert!(body.contains(row), "the EXPORT section has no {row} row");
    }
    // The Sound row is written twice -- a rated codec's keyed row and an
    // unrated one's readout, never both at once -- so the count is by label,
    // not by id.
    for label in ["\"Picture\"", "\"Sound\"", "\"Encoder\"", "\"Range\""] {
        assert!(body.contains(label), "the EXPORT section has no {label} row");
    }
    let ids = body.match_indices("\"settings-export-").count();
    assert_eq!(ids, 5, "the EXPORT section grew a row past the four (the two Sound rows are one row's two shapes)");
    assert!(
        source.contains("what a delivery is written as"),
        "the section lost its head"
    );
    // Every row wears its chord (DESIGN §4): three keyed rows plus the
    // range's own marks, which are keymap actions.
    for chord in ["\"c\"", "\"b\"", "\"g\"", "ActionId::SetIn"] {
        assert!(body.contains(chord), "an EXPORT row lost its chord {chord}");
    }
}

/// `c` walks the nine files in one order -- a codec's two boxes side by side,
/// the sound-only formats last -- and never stops on a format this machine
/// cannot write, because [`Player::set_format`] would only refuse it back.
#[test]
fn the_picture_row_cycles_codec_and_container_together_and_skips_a_refusal() {
    use crate::ui::settings_stance::{PICTURE_CYCLE, next_picture, picture_label};
    assert_eq!(
        PICTURE_CYCLE,
        [
            Format::Mp4,
            Format::Av1Mp4,
            Format::Av1,
            Format::Hevc,
            Format::HevcMp4,
            Format::Wav,
            Format::Flac,
            Format::Mp3,
            Format::Ogg,
        ]
    );
    // Nothing refused: the whole ring, wrapping back to the head.
    let mut at = Format::Mp4;
    let mut walked = vec![at];
    for _ in 1..PICTURE_CYCLE.len() {
        at = next_picture(at, |_| false);
        walked.push(at);
    }
    assert_eq!(walked, PICTURE_CYCLE.to_vec());
    assert_eq!(next_picture(at, |_| false), Format::Mp4);

    // HEVC unavailable (no plugin): `c` steps over both of its boxes rather
    // than parking on a pick the setter refuses.
    let no_hevc = |f: Format| matches!(f, Format::Hevc | Format::HevcMp4);
    assert_eq!(next_picture(Format::Av1, no_hevc), Format::Wav);
    // Everything refused: the row keeps what it has instead of cycling to
    // nothing.
    assert_eq!(next_picture(Format::Mp4, |_| true), Format::Mp4);

    assert_eq!(picture_label(Format::Mp4), "H.264 · MP4");
    assert_eq!(picture_label(Format::Av1), "AV1 · MKV");
    assert_eq!(picture_label(Format::Av1Mp4), "AV1 · MP4");
    assert_eq!(picture_label(Format::Wav), "WAV");
}

/// A refused codec is greyed with its reason, never hidden (DESIGN §8), and
/// the reason is short enough to sit beside a value: six words, the rest of
/// the sentence still said by the export moment's own banner. The sound row
/// wears a chord only where its codec has a rate to step.
#[test]
fn a_refusal_is_six_words_beside_the_value_and_soundless_codecs_wear_no_chord() {
    use crate::ui::settings_stance::{encoder_word, short_reason, sound_codec};
    let long = "HEVC needs the VA-API plugin, which this machine has not built";
    assert_eq!(short_reason(long), "HEVC needs the VA-API plugin, which");
    assert!(short_reason(long).split_whitespace().count() <= 6);
    assert_eq!(short_reason("no encoder here"), "no encoder here");

    for (format, codec, rated) in [
        (Format::Mp4, "AAC", true),
        (Format::Av1, "AAC", true),
        (Format::Mp3, "MP3", true),
        (Format::Wav, "PCM", false),
        (Format::Flac, "FLAC", false),
        (Format::Ogg, "Vorbis", false),
    ] {
        assert_eq!(sound_codec(format), (codec, rated), "{format:?}");
    }

    assert_eq!(encoder_word(EncoderSeat::Auto), "auto");
    assert_eq!(encoder_word(EncoderSeat::Hardware), "GPU");
    assert_eq!(encoder_word(EncoderSeat::Software), "software");
}

/// The one amber line on the page is the AV1-on-the-GPU seat, and it is the
/// only hue the EXPORT rows introduce -- every other value is an ink.
#[test]
fn the_only_hue_in_the_export_section_is_the_av1_gpu_notice() {
    let source = src_text("ui/settings_stance.rs");
    let start = source
        .find("fn export_section(")
        .expect("the export section");
    let body = &source[start..];
    let body = &body[..body.find("\n/// The page itself").expect("the render fn")];
    assert!(
        body.contains("NOTICE_LOOK()"),
        "the AV1-on-the-GPU row lost its amber"
    );
    assert_eq!(
        crate::ui::settings_stance::AV1_GPU_NOTICE,
        "ignores the budget \u{2014} constant quality"
    );
    for hue in ["ACCENT_", "STATUS_", "NOTICE_TELL", "NOTICE_DECIDE"] {
        assert!(
            !body.contains(hue),
            "the EXPORT rows reached for a second hue ({hue})"
        );
    }
}

/// Every control the hitmap names wears a hover line (the user, 2026-09-09,
/// over a screenshot of the time band: "each and every button needs hover
/// over information ... I don't know some of them"). The hitmap already
/// carries the pair a plate needs -- an id and a label -- so the sweep walks
/// the same emitters: a builder chain that records a control and never calls
/// `.tooltip` is a glyph the user cannot name.
///
/// A scan and not a render: this crate has no gpui window in its tests, so
/// the text is the finest grain available. Each emitter must claim a
/// `.tooltip(` of *its own* within [`REACH`] lines -- one to one, nearest
/// first. Counting them per function instead let an unrelated plate (the
/// empty-dock hint) pay for a control that had none: `dock.sort.cycle` sat
/// bare under a green gate until this pairing was written.
///
/// Controls outside the hitmap are outside this gate by charter. Named for
/// the record, since each is a control the pointer can press: a card's own
/// picker rows (colour/transform/mix/subtitle/silence bands, the speed
/// chips) and the two menus' rows, all of which already read as
/// `<label> <value-or-chord>` on the row itself -- a plate would repeat the
/// row it hangs off, which is DESIGN §8's prose rule the other way round.
#[test]
fn every_hitmap_control_wears_a_hover_line() {
    /// How far from a control's `hitmap::` line its plate may be attached.
    /// A builder chain runs long here (the contact strip's tooltip sits 65
    /// lines above its emitter), so the reach is generous; what it buys is
    /// that a plate can be spent only once.
    const REACH: usize = 80;
    // Ids that name a control a *previous* emitter in the same chain already
    // plated -- the second id is the harness's aim point, not a second
    // control -- with the reason each is exempt.
    const DOUBLE_NAMED: &[&str] = &[
        // `subtitle.N.M.row` and `subtitle.N.M.select` are one row.
        ".select\")",
    ];
    let mut controls = 0;
    let mut naked = Vec::new();
    for path in source_files() {
        let text = std::fs::read_to_string(&path).expect("a source file");
        if !text.contains("hitmap::") {
            continue;
        }
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a file name")
            .to_string();
        let lines: Vec<&str> = text.lines().collect();
        let mut tips: Vec<(usize, bool)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(".tooltip("))
            .map(|(i, _)| (i, false))
            .collect();
        for (i, line) in lines.iter().enumerate() {
            let emitter = ["hitmap::control(", "hitmap::dynamic(", "hitmap::action("]
                .iter()
                .any(|needle| line.contains(needle));
            if !emitter {
                continue;
            }
            // The id a `dynamic` builds sits a line or three below its call.
            let head = lines[i..(i + 6).min(lines.len())].join("\n");
            if DOUBLE_NAMED.iter().any(|id| head.contains(id)) {
                continue;
            }
            controls += 1;
            let mut free: Vec<usize> = (0..tips.len())
                .filter(|n| !tips[*n].1 && tips[*n].0.abs_diff(i) <= REACH)
                .collect();
            free.sort_by_key(|n| tips[*n].0.abs_diff(i));
            match free.first() {
                Some(n) => tips[*n].1 = true,
                None => naked.push(format!("{file}:{}: {}", i + 1, line.trim())),
            }
        }
    }
    assert!(
        controls >= 38,
        "the hover sweep found only {controls} controls -- it has gone blind"
    );
    assert!(
        naked.is_empty(),
        "controls with no hover line of their own (attach \
         `widgets::tip_hover`/`action_hover` in the chain that emits the \
         hitmap id): {naked:#?}"
    );
}

/// The one hover shape for the whole room: `<name> · <chord>`, and a control
/// the state refuses says why instead of naming a stroke that will not fire
/// (DESIGN §4/§8 -- a tooltip is not a sentence).
#[test]
fn a_hover_line_is_a_name_and_a_chord() {
    use crate::ui::widgets::tip_line;
    assert_eq!(
        tip_line("One frame forward", "right", None),
        "One frame forward · right"
    );
    assert_eq!(
        tip_line("Split", "s", Some("nothing selected")),
        "Split · nothing selected"
    );
    // `Keymap::chord`'s unbound badge is not a stroke.
    assert_eq!(tip_line("Add files", "--", None), "Add files");
    assert_eq!(tip_line("Source", "", None), "Source");
}

/// A region is a place, not a control: none of the region roots paints a
/// border because it holds focus (user 2026-09-10, "clicking through
/// timeline draws a white overlay around the timeline"; DESIGN §4's ring
/// line is about the *selected thing*). Focus routing itself is untouched.
#[test]
fn no_region_root_wears_a_focus_ring() {
    let stance = src_text("ui/stance.rs");
    let bench = src_text("ui/bench_stance.rs");
    let band = src_text("ui/timeband_stance.rs");
    for (name, text) in [
        ("stance.rs", &stance),
        ("bench_stance.rs", &bench),
        ("timeband_stance.rs", &band),
    ] {
        for line in text.lines() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            assert!(
                !(code.contains("is_focused(") || code.contains("when(focused"))
                    || !code.contains("border"),
                "{name} still paints a focus-conditional border: {code}"
            );
            assert!(
                !code.contains("STROKE_FOCUS"),
                "{name} paints the focus stroke on a region: {code}"
            );
        }
    }
}
