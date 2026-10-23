//! What is drawn and where: the toolbar, the theme, the notices, the lanes
//! and their marks, the tints, the subtitle rows and the sliders.

use super::*;

/// The class the user reported twice in one day ("some menus are belongs to
/// old ui ... for example resolution picker", then "right click menu in
/// library is old too"): a surface that paints a *role* token
/// (`BG_HOVER`/`BG_SELECTED`/`BG_RAISED`) rather than a Darkroom token used
/// to get the legacy tree's pale greys, because the Darkroom palette still
/// carried them. Dozens of call sites read those three roles; the fix is the
/// palette, and this pins it: the Darkroom surfaces are DESIGN §2.1's mock
/// values (canvas, panel, raised, hover), they step up in that order so a
/// hovered row is visible on a resting one, the picked-row fill sits between
/// raised and hover (the real selection mark is the 1px ink1 ring, DESIGN §4),
/// and the dim hover stays below raised.
#[test]
fn the_darkroom_palettes_interaction_greys_follow_design_2_1() {
    use crate::ui::theme::darkroom;
    let grey = |v: u32| (v >> 16 & 0xff, v >> 8 & 0xff, v & 0xff);
    let lum = |v: u32| {
        let (r, g, b) = grey(v);
        r + g + b
    };
    // Pinned by value, so a palette sweep that drifts a surface fails here
    // rather than in a screenshot.
    assert_eq!(darkroom::BG_CANVAS, 0x0b0c0e, "canvas is DESIGN §2.1");
    assert_eq!(darkroom::BG_PANEL, 0x131518, "panel is DESIGN §2.1");
    assert_eq!(darkroom::BG_RAISED, 0x1b1e22, "raised is DESIGN §2.1");
    assert_eq!(darkroom::BG_HOVER, 0x24282d, "hover is DESIGN §2.1");
    assert!(
        lum(darkroom::BG_CANVAS) < lum(darkroom::BG_PANEL)
            && lum(darkroom::BG_PANEL) < lum(darkroom::BG_RAISED)
            && lum(darkroom::BG_RAISED) < lum(darkroom::BG_HOVER),
        "the surfaces must step up canvas < panel < raised < hover -- a \
         button or menu that paints a lower step than its panel sinks into it"
    );
    assert!(
        lum(darkroom::BG_RAISED) < lum(darkroom::BG_SELECTED)
            && lum(darkroom::BG_SELECTED) <= lum(darkroom::BG_HOVER),
        "the picked-row fill sits between raised and hover; past hover it \
         would read as a hover, and at raised it would not read at all"
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

/// One slot: a second message replaces the first on screen, so the newest
/// line is always the one a person reads and an old failure never lingers
/// under a newer line.
#[test]
fn a_second_message_replaces_the_first_on_screen() {
    let mut q = std::collections::VecDeque::new();
    push_notice(&mut q, "Nothing added: no video stream".into());
    push_notice(&mut q, "Nothing added: the file is not there".into());
    assert_eq!(q.len(), 1);
    assert_eq!(
        q.back().map(|n: &crate::notices::Toast| n.text.as_ref()),
        Some("Nothing added: the file is not there")
    );

    // A held key that refuses says its one sentence once.
    push_notice(&mut q, "Nothing added: the file is not there".into());
    assert_eq!(q.len(), 1);

    // However many messages arrive, the newest is the one kept.
    for i in 0..NOTICES_MAX + 3 {
        push_notice(&mut q, format!("Scan failed: {i}").into());
    }
    assert_eq!(q.len(), 1);
    assert_eq!(
        q.back().map(|n: &crate::notices::Toast| n.text.as_ref()),
        Some(format!("Scan failed: {}", NOTICES_MAX + 2).as_str())
    );
}

/// An export's own outcome is what a person started it to read, so it must
/// never be drawn out of sight: it replaces whatever was on screen, and it is
/// the newest, the one the status bar draws.
#[test]
fn an_export_outcome_is_the_newest_and_is_drawn() {
    let mut q = std::collections::VecDeque::new();
    push_notice(
        &mut q,
        "Editing copy ready for a.mp4: Editing copies on cuts on it".into(),
    );
    push_notice(&mut q, "Subtitles b.srt: 1 track(s) in the palette".into());
    push_notice(&mut q, format!("{EXPORT_DONE}out.mp4").into());
    assert_eq!(q.len(), 1);
    assert_eq!(
        q.back().map(|n: &crate::notices::Toast| n.text.as_ref()),
        Some(format!("{EXPORT_DONE}out.mp4")).as_deref(),
        "the export's result must be the newest notice, the one the stack draws"
    );

    // A failed export is the same class: the outcome, whichever it was.
    let mut q = std::collections::VecDeque::new();
    push_notice(
        &mut q,
        "Editing copy ready for a.mp4: Editing copies on cuts on it".into(),
    );
    push_notice(&mut q, "Export failed: disk full".into());
    assert_eq!(
        q.back().map(|n: &crate::notices::Toast| n.text.as_ref()),
        Some("Export failed: disk full")
    );
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
    let cards = src_text("ui/cards");
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
    const {
        assert!(CONTROL_H >= HIT_MIN);
    }
    const {
        assert!(RULER_HIT_H >= HIT_MIN);
    }
    const {
        assert!(LANE_H >= HIT_MIN);
    }
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
    const {
        assert!(
            CONTROL_H + RULER_HIT_H + 2. * LANE_H + 17. + 4. * 8. + 16. <= PANEL_H,
            "the second lane does not fit the panel"
        );
    }
    // Headers and clip boxes are as tall as the lane, and the lane is a
    // click target (WCAG 2.5.8).
    const {
        assert!(LANE_H >= HIT_MIN);
    }
    // A label row that ate the whole lane would leave no waveform.
    const {
        assert!(LABEL_H < LANE_H / 2.);
    }
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
    use crate::{SCROLL_ROW_H, Split, lanes_h, split_size, timeline_fixed_h, timeline_h};
    use gpui::{px, size};

    // The strip's row, and only the strip's row, is what the two faces differ
    // by -- the bar is centred in it. The hit area is taller than the bar and
    // overlays the lanes above it, so it costs no row.
    assert_eq!(
        timeline_fixed_h(true) - timeline_fixed_h(false),
        SCROLL_ROW_H
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
            let clip = session.lane_clips(Lane::V1)[0].clone();
            let (lo, hi) = session
                .trim_room(Lane::V1, 0, edge)
                .expect("clip 0 is there");
            // Both walls and the middle of the room: the whole range a
            // pointer can be clamped to.
            for to in [lo, (lo + hi) / 2, hi] {
                let preview = trimmed_clip(clip.clone(), edge, to, false);
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
        let clip = session.lane_clips(Lane::V1)[0].clone();
        let (lo, hi) = session
            .trim_room(Lane::V1, 0, edge)
            .expect("clip 0 is there");
        for to in [lo, (lo + hi) / 2, hi] {
            let preview = trimmed_clip(clip.clone(), edge, to, true);
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
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
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
    let dragged = lane[2].clone();
    assert_eq!(live_idx(&lane, 2, &dragged), Some(2), "nothing moved");
    // A delete in front of it: the clip is now index 1, and the index the
    // drag froze names a clip nobody grabbed.
    let after = [at(0), at(60)];
    assert_eq!(live_idx(&after, 2, &dragged), Some(1));
    assert_eq!(live_idx(&after, 1, &dragged), Some(1));
    // Deleted mid-drag: there is nothing to move, and moving its neighbour
    // instead is exactly the bug this exists for.
    assert_eq!(live_idx(&[at(0)], 2, &dragged), None);
    assert_eq!(live_idx(&[], 0, &dragged), None);
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
    // Past the palette it wraps -- never an index panic. The cycle length is
    // the palette's own length (12 hues, DESIGN §2.2), not a fixed 4.
    let n = SOURCE_TINTS().len();
    assert_eq!(source_tint(n), source_tint(0));
    assert_eq!(source_tint(n + 5), source_tint(5));
    assert_eq!(source_tint(usize::MAX), SOURCE_TINTS()[usize::MAX % n]);
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
            audio_stream: 0, ..Default::default()
        },
        Source {
            path: PathBuf::from("/films/b.mp4"),
            audio_stream: 0, ..Default::default()
        },
        // A second stream of the first file is a second source and the
        // same colour.
        Source {
            path: PathBuf::from("/films/a.mkv"),
            audio_stream: 1, ..Default::default()
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
        audio_stream: 0, ..Default::default()
    }];
    assert_eq!(
        file_tint(&sources, Path::new("Cargo.toml")),
        Some(source_tint(0)),
        "a relative spelling of the source file wears the source's colour"
    );
}

/// A film's two ASS tracks with the timeline trimmed to twenty seconds:
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
        audio_stream: 0, ..Default::default()
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

/// The door this editor answers "don't make me import the film again" with:
/// it reads a file's subtitle tracks onto the open timeline -- a release's
/// `.mkv`, an `.srt` beside it -- while the file itself joins nothing.
///
/// Cleanse round 2 (2026-09-10) took its ghost row out of the dock's footer
/// along with the `IMPORT` head: one `Add` row is the dock's one door, and
/// `Add` itself forks a subtitle file to the same place. The verb keeps its
/// stroke, its KEYS row, and a pointer door of its own -- the dock's
/// right-click menu ([`DOCK_ITEMS`]), which an *empty* library carries too.
/// It is the action and not a second implementation of it, so the row, the
/// stroke and the actions card cannot drift apart, and the menu row is
/// oracle-gated like every other, so with no timeline open it dims and says
/// why instead of opening a chooser for nothing.
#[test]
fn the_dock_menu_carries_the_add_subtitles_door() {
    use crate::{ActionId, DOCK_ITEMS};
    assert!(
        DOCK_ITEMS.contains(&ActionId::ImportSubtitles)
            && DOCK_ITEMS.contains(&ActionId::PasteFilePath)
            && DOCK_ITEMS.contains(&ActionId::AddFiles),
        "the dock's right-click menu lost one of the three ways in"
    );
    // ...and that menu is actually opened from the Sources body.
    let dock = src_text("ui/dock_stance.rs");
    assert!(
        dock.contains("on: MenuOn::Dock,"),
        "nothing in the dock opens the menu that now carries the two imports"
    );
    // The rows are drawn from `DOCK_ITEMS` by the one menu render.
    let overlays = src_text("ui/overlays.rs");
    assert!(
        overlays.contains("MenuOn::Dock => DOCK_ITEMS.to_vec(),"),
        "the dock menu draws something other than the three import actions"
    );
    // `act(ActionId::ImportSubtitles, ...)` routes to `pick_and_add_subtitles`
    // -- the row names the action, not a second implementation of it.
    let actions = src_text("player/actions.rs");
    assert!(
        actions.contains("ActionId::ImportSubtitles => self.pick_and_add_subtitles(cx)"),
        "ImportSubtitles no longer opens the subtitle chooser"
    );
    // A subtitle file taken through the plain `Add files` door lands as
    // subtitles rather than as a source that cannot be read.
    let library = src_text("player/library.rs");
    assert!(
        library.contains("if is_subtitle(path) {"),
        "Add files no longer forks a .srt/.vtt to the subtitle list"
    );
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
    let bench = src_text("ui/bench_stance");
    let drop_at = bench
        .find(".drag_over::<SubPick>")
        .expect("no subtitle-track lane target");
    let drop = &bench[drop_at..(drop_at + 500).min(bench.len())];
    assert!(
        drop.contains("this.place_sub(drag.0, lane"),
        "a dock subtitle drag does not place on the lane: {drop}"
    );
}

/// A file's subtitle tracks are that file's subset and are listed as one:
/// directly under the row that names it, never after every unrelated file
/// (user 2026-09-10, screenshot -- two screen recordings sat between a film
/// and its own subtitle group). A `.srt` nobody carries stays top-level, in
/// arrival order, at the end.
#[test]
fn a_file_subtitle_group_follows_its_own_row() {
    use crate::ui::dock_stance::{Slot, source_list_order};
    let film = PathBuf::from("/films/a.mkv");
    let other = PathBuf::from("/rec/b.mp4");
    let loose = PathBuf::from("/subs/late.srt");
    assert_eq!(
        source_list_order(
            &[film.clone(), other.clone()],
            &[film.clone(), loose.clone()]
        ),
        [Slot::Media(0), Slot::Subs(0), Slot::Media(1), Slot::Subs(1)],
        "the film's own tracks must sit between it and the next file"
    );
    // A file with several audio streams has several rows; the tracks follow
    // the last of them, so the file's own rows stay together.
    assert_eq!(
        source_list_order(
            &[film.clone(), other.clone(), film.clone()],
            std::slice::from_ref(&film)
        ),
        [
            Slot::Media(0),
            Slot::Media(1),
            Slot::Media(2),
            Slot::Subs(0)
        ],
    );
    // Nobody's `.srt`: still a top-level row, still last.
    assert_eq!(
        source_list_order(&[other], &[loose]),
        [Slot::Media(0), Slot::Subs(0)],
    );
}

/// ...and the Sources list is *built* through that ordering rather than by
/// appending the subtitle groups after the media rows, which is what put a
/// film's own tracks at the end of the list.
#[test]
fn the_sources_list_is_built_through_the_ordering() {
    let dock = src_text("ui/dock_stance.rs");
    let tab = &dock[dock.find("fn sources_tab").expect("no sources tab")..];
    assert!(
        tab.contains("source_list_order(&media, &subs)"),
        "the Sources list no longer goes through source_list_order"
    );
    assert!(
        !tab.contains("row_elements.extend(subtitles)"),
        "the subtitle groups are appended after every source again"
    );
    // A parent already in the list gets the tracks bare, under it.
    let palette = &dock[dock.find("fn subtitle_tab_rows").expect("no subtitle rows")..];
    assert!(
        palette.contains("let nested = parents.contains(&group.path)")
            && palette.contains("let has_header = !nested || track_count > 3"),
        "a group under its own parent still draws a group row"
    );
}

/// Two files whose rows read the same word are told apart on the metadata
/// line -- the folder, or `#n` when the folder is shared too.
#[test]
fn same_named_files_are_told_apart_on_the_metadata_line() {
    let three: Vec<Source> = ["/a/rec.mp4", "/b/rec.mp4", "/a/solo.mp4"]
        .into_iter()
        .map(|path| Source {
            path: PathBuf::from(path),
            audio_stream: 0, ..Default::default()
        })
        .collect();
    let rows = library_rows(&three, &HashMap::new(), &HashMap::new(), None, |_| 90);
    assert_eq!(
        rows.iter()
            .map(|row| row.detail.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", ""],
        "twins say which folder they came out of"
    );
    let twins: Vec<Source> = ["/a/rec.mp4", "/a/rec.mkv"]
        .into_iter()
        .map(|path| Source {
            path: PathBuf::from(path),
            audio_stream: 0, ..Default::default()
        })
        .collect();
    let rows = library_rows(&twins, &HashMap::new(), &HashMap::new(), None, |_| 90);
    assert_eq!(
        rows.iter()
            .map(|row| row.detail.as_str())
            .collect::<Vec<_>>(),
        ["#1", "#2"],
        "one folder, one stem: arrival order tells them apart"
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

/// A `^z` moves rows without saying which one moved, so the pick is followed by
/// what it *names* ([`sub_pick_after_restore`]) rather than by its index: the
/// same three rows as the removal test above, restored instead of taken away,
/// where the index is the wrong answer in the first case and the right one in
/// the second.
#[test]
fn a_restored_palette_carries_the_pick_by_what_it_names() {
    let palette = [
        sub("/films/a.mkv", Some(1), "eng"),
        sub("/films/a.mkv", Some(2), "fre"),
        sub("/films/a.mkv", Some(3), "ger"),
    ];
    // The pick on "ger" at index 1 of the two rows a removal of the first left.
    let short = [palette[1].clone(), palette[2].clone()];
    let picked = SubPickKey::of(&short, 1).expect("a row the pick names");
    assert_eq!(
        sub_pick_after_restore(1, Some(&picked), &palette),
        2,
        "the row above it came back"
    );
    // The row that came back *below* the pick leaves it where it was.
    let short = [palette[0].clone(), palette[1].clone()];
    let picked = SubPickKey::of(&short, 1).expect("a row the pick names");
    assert_eq!(
        sub_pick_after_restore(1, Some(&picked), &palette),
        1,
        "a row after it is not its business"
    );
    // The picked row itself gone from the restored palette: the old index
    // clamped into it, which is the removal door's own fallback.
    let picked = SubPickKey::of(&palette, 2).expect("a row the pick names");
    assert_eq!(sub_pick_after_restore(2, Some(&picked), &palette[..2]), 1);
    assert_eq!(
        sub_pick_after_restore(0, None, &[]),
        0,
        "an emptied palette is a state, not a panic"
    );
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
    assert_eq!(enable_lane(ActionId::Mix, Lane::A1, live.clone()), Enable::Yes);
    assert_eq!(
        enable_lane(ActionId::RemoveVideoLane, Lane::V1, live.clone()),
        Enable::Yes
    );
    assert_eq!(
        enable_lane(ActionId::RemoveAudioLane, Lane::A1, live.clone()),
        Enable::Yes
    );
    assert!(matches!(
        enable_lane(ActionId::Mix, Lane::V1, live),
        Enable::Hidden(_)
    ));

    // The head itself carries a dot, a name and its switches, and nothing else
    // (cleanse round 2, DESIGN §7). Its mix and its remove are rows
    // in its own right-click menu instead, and they still reach the lane the
    // hand named rather than the last of the kind a bare stroke walks to:
    // `overlays.rs` routes those rows through the same `act_lane` door the
    // head's buttons used.
    let bench = src_text("ui/bench_stance");
    for gone in [
        "\"bench-mix-lane\"",
        "\"bench-remove-lane\"",
        "lane.{}.remove",
    ] {
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
    // Cleanse round 2 (2026-09-10): the monitoring cluster left the band
    // (chord + KEYS row only). The export range's two grips moved from the
    // contact strip onto the overview above the ruler with the strip itself.
    let overview = src_text("ui/bench_stance/overview.rs");
    for door in [
        "fn mark_grip(",
        "\"stance-strip-mark-in\"",
        "\"stance-strip-mark-out\"",
        "MARK_DRAG.with(|m| m.set(Some(action)))",
    ] {
        assert!(overview.contains(door), "overview lost {door}");
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
    use crate::{
        BENCH_MIN_H, BENCH_SHARE, DOCK_MIN_W, SIDE_MAX_FRAC, SPLIT_W, Split, split_bounds,
        split_drag_size, split_size,
    };
    use gpui::{point, px, size};

    let window = size(px(1280.), px(720.));
    // Untouched, each answers its own default: the dock its stance width, the
    // bench a share of the window's height (DESIGN §5, about 42%).
    assert_eq!(
        split_size(Split::Dock, None, 2, window, false),
        crate::ui::stance::DOCK_W
    );
    assert_eq!(
        split_size(Split::Bench, None, 2, window, false),
        720. * crate::layout::bench_share(720.)
    );
    // The DESIGN §5 numbers themselves: a 320px dock, a 280px dock floor, a
    // 30px ledger and a timeline block at about 42% of the window.
    assert_eq!(crate::ui::stance::DOCK_W, 320.);
    assert_eq!(DOCK_MIN_W, 280.);
    assert_eq!(crate::ui::stance::LEDGER_H, 24.);
    assert!((BENCH_SHARE - 0.46).abs() < 1e-6);
    // Dragged, it is what the hand asked for...
    assert_eq!(split_size(Split::Dock, Some(400.), 2, window, false), 400.);
    assert_eq!(split_size(Split::Bench, Some(300.), 2, window, false), 300.);
    // ...and never past the floor...
    assert_eq!(
        split_size(Split::Dock, Some(0.), 2, window, false),
        DOCK_MIN_W
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
    use crate::ui::bench_stance::{LANE_MIN_H, ROW_GAP, RULER_H};
    use crate::ui::stance::BENCH_CHROME_H;

    let box_h = BENCH_MIN_H - BENCH_CHROME_H;
    let avail = box_h - RULER_H - ROW_GAP;
    // Lanes are no longer split evenly out of the box: each is drawn at its
    // own height, never below `LANE_MIN_H`, so the floor is the two default
    // lanes at that minimum plus their gap. `BENCH_MIN_H` carries one spare row
    // (`layout::LEDGER_SEAM_CLEARANCE`) so the last lane's own last pixel never
    // shares a row with the ledger's border.
    let content = 2. * LANE_MIN_H + ROW_GAP;
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
/// `bench_stance::render`'s content -- the div's own `.border_t_1()` (1px) plus
/// its `py(4.)` top padding + the section head's real line box at gpui's
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
    use crate::ui::bench_stance::{LANE_DOT_D, LANE_MIN_H};
    use crate::ui::type_scale::CHORD_METADATA_MIN_PX;

    // gpui's line-height ratio, which happens to sit near the golden ratio; it
    // is not that constant, so the lint's suggestion would be wrong here.
    #[allow(
        clippy::approx_constant,
        reason = "gpui's default line-height ratio, not the golden ratio constant"
    )]
    let label_line_h = (CHORD_METADATA_MIN_PX * 1.618_034).round();
    // The head is one row -- dot, name, switches -- centred in the lane, so
    // what it needs is its tallest child: the 24px switch hit area (and the
    // subtitle name's own hit area), not a stack of label, gap and dot.
    let content = label_line_h.max(LANE_DOT_D).max(HIT_MIN);
    assert!(
        LANE_MIN_H >= content,
        "LANE_MIN_H ({LANE_MIN_H}) is shorter than what a lane head actually \
         draws ({content}px: the tallest of the {label_line_h}px label line \
         box, the {LANE_DOT_D}px dot and the {HIT_MIN}px switch hit areas) -- \
         the head would spill past its row"
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
    // Whitespace-collapsed, so the call still matches however rustfmt wraps it.
    let stance = src_text("ui/stance.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for split in ["Split::Dock", "Split::Bench"] {
        assert!(
            stance.contains(&format!("divider( {split},")),
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
    let calls = ["screen(", "time_band(", "bench(", "ledger(", "dock("];
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
        ("render.rs", "viewer_picture("),
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

/// DESIGN §6.2: the picture sits 12px in from the viewer's edges, the corner
/// plate is the only thing drawn over it, and the OUT|IN strip floats over the
/// bottom edge only while it is shown. Its layout box must never reach the
/// picture, or the picture's box moves on every cut walk again.
#[test]
fn the_viewer_pads_the_picture_and_floats_its_overlays() {
    let stance = src_text("ui/stance.rs");
    assert!(
        stance.contains("const VIEWER_PAD: f32 = 12.;"),
        "the viewer padding is no longer the DESIGN §6.2 12px"
    );
    let screen = &stance[stance.find("fn screen(").expect("the Darkroom screen")..];
    assert!(
        screen.contains(".p(px(VIEWER_PAD))") && !screen.contains("corner_plate"),
        "the screen no longer pads the picture, or a boxed chip is back over it"
    );
    assert!(
        screen.contains("player.trim_two_up()") && !screen.contains("player.two_up()"),
        "the OUT|IN split is mounted only through trim_two_up, never as a flow sibling"
    );

    let preview = src_text("ui/preview.rs");
    let two = &preview[preview.find("fn trim_two_up(").expect("the trim two-up")..];
    assert!(
        two.contains(".absolute()") && preview.contains("fn editing_cut("),
        "the trim two-up must be absolute (no layout box) and only while an edit point is worked"
    );
    assert!(
        !preview.contains("viewer-plate") && !preview.contains("fn corner_plate") && !preview.contains("fn two_up("),
        "nothing boxed is drawn over the picture: the timecode is the transport's, the name the status bar's"
    );
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
    let file = "ui/overlays.rs";
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
    let library = src_text("ui/library.rs");
    assert!(
        !library.contains("menu_floor("),
        "ui/library.rs routes its menu through menu_floor's picture-floor \
         clamp again -- a right-click high in the dock teleports back down \
         to the picture floor (the 'menu in somewhere nonsense' defect)"
    );
    assert!(
        library.contains("menu_at_w(at, viewport, pw, h)") && !library.contains("ROW_CLEAR"),
        "ui/library.rs no longer anchors its menu on the pointer through \
         menu_at_w; update this guard"
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
/// section's opener is exactly the regression a page organised around named
/// sections invites -- and the tab strip did not loosen it: a tab decides
/// which section is *shown*, never which door a row opens. This pins each
/// half to its own doors so the two lists cannot cross.
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
    // `Pick::Autosave` is the same kind of thing: how often this machine writes
    // the recovery copy beside the project, kept in ~/.config/edith/autosave.
    let editor_no_theme = editor_body
        .replace("open_picker(Pick::Theme", "")
        .replace("open_picker(Pick::Autosave", "");
    {
        let door = "open_picker(";
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
    // The proxy switches are the project's (DESIGN §9.2: written into the
    // .edith once one is open), so they moved from EDITOR to PROJECT; the
    // subtitle font stays this machine's.
    for door in ["toggle_proxies(", "toggle_auto_proxies("] {
        assert!(
            project_body.contains(door),
            "PROJECT section no longer opens {door} -- the proxy switches are the project's"
        );
        assert!(
            !editor_no_theme.contains(door),
            "EDITOR section opens {door}, a project-file door -- that value belongs in PROJECT, not here"
        );
    }
    assert!(
        editor_body.contains("open_subtitle_style("),
        "EDITOR section no longer opens the subtitle font -- update this guard"
    );
    assert!(
        !project_body.contains("open_subtitle_style("),
        "PROJECT section opens the subtitle font, an editor-only door -- that value belongs in EDITOR, not here"
    );
    // Neither section writes a config file straight from a row: both go
    // through an opener, which is the one place a config-file write (the
    // subtitle style card's Save, this window's next project load) is
    // allowed to live.
    // The file helpers after the EXPORT section (the export pick's path, the
    // keys' save) are the openers' own home, so the rows' scan stops short of
    // them; the export pick's path is then checked where it is defined.
    let rows_body = &editor_body[..editor_body
        .find("/// Every format the page can pick")
        .unwrap_or(editor_body.len())];
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
            !rows_body.contains(path_fn),
            "EDITOR section touches {path_fn} directly"
        );
    }
    let export_ui = src_text("export_ui.rs");
    assert!(
        export_ui.contains(
            "fn export_defaults_path() -> PathBuf {\n    crate::keymap::Keymap::config_path()"
        ),
        "the export pick's file moved out of its one helper"
    );
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

/// The HDR group (the look HDR gets on a normal screen) shows only when the
/// project has an HDR source, and the read-only reference rows are gone from
/// Settings: nothing there is a fact about one file.
#[test]
fn the_hdr_group_shows_only_with_hdr_and_the_reference_rows_are_gone() {
    let source = src_text("ui/settings_stance.rs");
    let project_start = source.find("fn project_section(").expect("the project section");
    let editor_start = source.find("fn editor_section(").expect("the editor section");
    let project_body = &source[project_start..editor_start];
    assert!(project_body.contains("has_hdr.then("), "the HDR group is no longer conditional");
    assert!(project_body.contains("\"When HDR plays on a normal screen\""));
    for gone in ["settings-hdr-reference", "settings-content-light", "settings-mix", "\"Mix\""] {
        assert!(!project_body.contains(gone), "Settings still has {gone}");
    }
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
/// the thing it acts on. Any exemption is independently-owned lane parity work,
/// named with its owner so a future removal cannot silently turn any unrelated
/// action back into keyboard-only.
#[test]
fn every_action_has_a_darkroom_widget_home_or_explicit_owner() {
    use crate::ActionId;
    // Empty: every action now has a row on the menu bar (DESIGN §4.4), so no
    // action is exempt. The entries this list held (the selection walk, the
    // shuttle, the sync walk, the trims at the playhead and the rest) each
    // became a menu row. An exemption added later needs an owner, and it goes
    // back here.
    const EXPLICITLY_OWNED_ELSEWHERE: &[(ActionId, &str)] = &[];
    /// `ActionId::Loop` is a prefix of `ActionId::LoopTrim`, and the keys
    /// menu's own label arm (`ActionId::ToggleMute | ActionId::Paste =>
    /// format!("{full} (global)")`) shapes a string rather than opening a
    /// door -- neither is a widget home, so neither counts as a mention.
    fn mentions(text: &str, name: &str) -> bool {
        text.lines()
            .filter(|l| !l.contains("=> format!(\"{full} (global)\")"))
            .any(|line| {
                line.match_indices(&format!("ActionId::{name}"))
                    .any(|(i, m)| {
                        !line[i + m.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    })
            })
    }
    let darkroom = [
        "ui/bench_stance",
        "ui/cards",
        "ui/dock_stance.rs",
        "ui/menubar.rs",
        "ui/overlays.rs",
        "ui/settings_stance.rs",
        "ui/stance.rs",
        "ui/timeband_stance.rs",
        "ui/toolbar.rs",
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
                !mentions(&darkroom, &format!("{action:?}")),
                "ActionId::{action:?} has a darkroom home now -- delete its \
                 EXPLICITLY_OWNED_ELSEWHERE entry ({owner})"
            );
            continue;
        }
        let name = format!("{action:?}");
        let mentioned = mentions(&darkroom, &name)
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
    let clip_src = src_text("ui/bench_stance/clip.rs");
    let clip = &clip_src[clip_src.find("fn clip_box(").expect("the clip renderer")..];
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

    let lane_src = src_text("ui/bench_stance/lane.rs");
    let from = lane_src.find("fn lane_row(").expect("the lane renderer");
    let lane = &lane_src[from..from + lane_src[from..].find("\n}\n").expect("its end")];
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

/// DESIGN §7: the head's switches are text glyphs at a 24px hit area, the head
/// is 140px wide, and a hidden or muted lane's clips sit at 40%. The
/// subtitle lane's show toggle is its name now, so the eye glyph is the hide.
#[test]
fn the_lane_head_is_140_wide_with_glyph_switches_and_dims_hidden_lanes() {
    let bench = src_text("ui/bench_stance");
    assert!(
        bench.contains("const HEAD_W: f32 = 156."),
        "the lane head is no longer 140px wide"
    );
    for glyph in ["◉", "♪", "▣"] {
        assert!(bench.contains(glyph), "the head lost its {glyph} switch");
    }
    let lane = src_text("ui/bench_stance/lane.rs");
    assert!(
        lane.contains("const DIMMED_OPACITY: f32 = 0.4;") && lane.contains(".opacity(dim)"),
        "a hidden or muted lane's clips are not dimmed to 40%"
    );
    let name = source_from("fn lane_name(");
    let name = &name[..name.find("\n}\n").expect("the name's end")];
    assert!(
        !name.contains("◉") && name.contains("show_sub_lane(lane, cx)"),
        "the subtitle show toggle still wears the eye glyph, or is not the name"
    );
}

/// The pointer harness can only drive controls that report the bounds of the
/// element that owns the gesture. Keep every dynamic Darkroom entry surface
/// named, so UI verification never falls back to guessed pixels.
#[test]
fn hitmap_names_every_darkroom_pointer_entry_surface() {
    let bench = src_text("ui/bench_stance");
    for needle in [
        "clip.{}.{}.{}.fade-{}",
        "clip.{}.{}.{}.trim-{}",
        "clip.{}.{}.{}.dissolve",
        "lane.{}.reorder",
        "lane.{}.eye",
        "lane.{}.mute",
        "lane.{}.lock",
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
        "source.{i}.row",
        "source.{i}.add",
        "dock.filter",
    ] {
        assert!(
            dock.contains(needle),
            "dock control missing hitmap contract: {needle}"
        );
    }

    for (path, needles) in [
        (
            "ui/bench_stance/overview.rs",
            &[
                "timeline.overview",
                "stance-overview-window",
                "stance-strip-mark-in",
                "stance-strip-mark-out",
                "timeline.scrollbar",
                "stance-scroll-thumb",
            ][..],
        ),
        ("ui/preview.rs", &["source.ruler"][..]),
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
    let bench = src_text("ui/bench_stance");
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
    // The ruler is its own file now; it is mounted in the content between the
    // container's listener and the lane column, and must add none of its own.
    let ruler = src_text("ui/bench_stance/ruler.rs");
    assert!(
        !ruler.contains(".on_scroll_wheel("),
        "the ruler answers the wheel on its own again -- one notch, two scrolls"
    );
    let content_src = src_text("ui/bench_stance/mod.rs");
    let mounted = content_src.find(".child(ruler(").expect("the ruler is not mounted");
    let lanes = content_src
        .find(".id(\"bench-lanes\")")
        .expect("no lane column");
    assert!(
        !content_src[mounted..lanes].contains(".on_scroll_wheel("),
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
    // gets after it no longer needs it (a `bench=207` -- a press with no
    // drag in it -- beside a dragged `dock=277`).
    assert!(
        body.find(".group(group.clone())")
            .expect("the band's group")
            < body.find("border_t_1()").expect("the hairline"),
        "the hairline is painted outside the band: {body}"
    );
}

/// The band the hand actually gets, in seam coordinates. It once reached 13px
/// past the line to cover the `BENCH` label row a hand aimed at; that row is
/// gone (cleanse round 2) and the ruler under the seam answers presses of its
/// own (it scrubs), so the bench seam takes every other seam's reach and no
/// more -- a deeper band would eat scrubs at the film's own top edge. Every
/// seam shares one 24 px band ([`HIT_MIN`], WCAG 2.5.8), hung one row above
/// the strip, so every seam reaches the same 17 px past its own line.
#[test]
fn the_bench_seam_reaches_no_further_than_every_other_seam() {
    let above = 1.;
    let reach = |grab: f32| grab - above - crate::layout::SPLIT_W;
    assert_eq!(crate::layout::GRAB_W, HIT_MIN, "a seam's band is 24 px");
    assert_eq!(reach(crate::layout::GRAB_W), 17.);
    assert_eq!(
        reach(crate::layout::BENCH_GRAB_H),
        reach(crate::layout::GRAB_W),
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
/// the `HEAD_W` px head column, so V1/A1 and their verbs disappear under the first
/// take as soon as the bench is wheeled past frame 0. The mask belongs on the
/// bed itself so a half-scrolled clip still drags on the half that shows.
#[test]
fn the_lane_bed_clips_its_clips_at_the_pinned_heads() {
    let src = src_text("ui/bench_stance");
    let start = src.find(r#".id(("bench-bed""#).expect("the bed");
    let bed = &src[start
        ..start
            + src[start..]
                .find(".bg(rgb(DARK_CANVAS()))")
                .expect("its fill")];
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
    // REDESIGN §3.8: the name is the one flexible child beside a fixed poster
    // and a 28px `+`, and it ends in an ellipsis rather than collapsing.
    assert!(
        row.contains(".flex_1()\n                                .min_w(px(0.))\n                                .truncate()"),
        "the row name can no longer shrink to an ellipsis"
    );
    // Cleanse round 2: the name is what the editor calls the film, not what
    // the filesystem calls the bytes.
    assert!(
        row.contains("let name: SharedString = stem(&row.name).into();"),
        "the row name still wears its extension"
    );
    assert_eq!(
        crate::ui::dock_stance::stem("he_is_not_the_only_one.mp4"),
        "he_is_not_the_only_one"
    );
    // A file with several audio streams keeps the stream half of its name and
    // loses only the extension; a name that is all extension keeps it.
    assert_eq!(
        crate::ui::dock_stance::stem("A Film.mkv [audio 2]"),
        "A Film [audio 2]"
    );
    assert_eq!(crate::ui::dock_stance::stem(".srt"), ".srt");
    // One ghost on the right edge, and one only: preview and the stand-in
    // moved into the row's own menu (`RowItem::Preview`/`Proxy`).
    // REDESIGN §3.8: a 28px ghost `plus` icon button, always drawn; the key is
    // nowhere on the row (§5 rule 1).
    assert!(row.contains("Icon::Plus"), "the right-edge verb lost its plus");
    assert!(!row.contains(r#".child("↵")"#), "a key is back on the row");
    for gone in [r#".child("▷")"#, "dock-proxy-toggle", "dock-preview"] {
        assert!(!row.contains(gone), "the row still carries `{gone}`");
    }
    assert!(
        row.contains("source.{i}.add"),
        "the add ghost lost its hitmap entry"
    );
    // The metadata line is codec · length · lanes: no decoder seat, no use
    // count (`usage_line` and the `seats` filter above it).
    assert!(
        row.contains(".map(Backend::label);") && row.contains("fn row_facts("),
        "the metadata line no longer drops the decoder seat"
    );
    let usage = &dock[dock.find("fn usage_labels(").expect("the usage chips")..start];
    assert!(
        !usage.contains("use{}"),
        "the metadata line still counts uses"
    );
    // DESIGN §8: the permanent footer telling the editor how to use a list
    // ("drag · ↵ add · double-click plays") is instructional copy and is gone.
    assert!(
        !dock.contains("double-click plays"),
        "the dock still carries an instruction footer"
    );
    // ...an empty library says so in words, and a filter that matches
    // nothing keeps the one noun.
    assert!(
        dock.contains("EMPTY_LIBRARY_HINT") && dock.contains("Nothing matches"),
        "the empty dock lost its empty-library words or the no-match noun"
    );
}

/// DESIGN §6.7: the Media tab is one list of rows at a fixed height, with
/// `+ Add` as a text button, and the footer's import button named for what
/// it does.
#[test]
fn the_media_tab_rows_are_fixed_and_the_footer_names_the_import() {
    assert_eq!(crate::ui::dock_stance::SOURCE_ROW_H, 64.0);
    let dock = src_text("ui/dock_stance.rs");
    let row = &dock[dock.find("fn source_row(").expect("the source row")..];
    assert!(
        row.contains(".h(px(source_row_h(usable)))"),
        "a media row is no longer fixed at 72px"
    );
    assert!(
        dock.contains(r#""Import media…""#),
        "the footer no longer names the import"
    );
    // The tab is found by its id, not by its label, so a rename cannot
    // silently break which surface a click opens.
    assert!(
        dock.contains(r#"match id == "dock-tab-src""#)
            && dock.contains(r#"this.dock_src_active = id == "dock-tab-src";"#),
        "the tab still compares its label to pick its surface"
    );
}

/// The dock's metadata line drops the decoder *seat* and nothing else. A file
/// the decoder refused has no seat to drop: its detail is the refusal, and it
/// is the only thing the line has to say.
#[test]
fn the_dock_keeps_a_refusal_where_it_drops_a_seat() {
    use crate::ui::dock_stance::without_seats;
    use engine::decode::Backend;

    let seats = [
        Backend::Opening,
        Backend::Hardware,
        Backend::Software,
        Backend::Still,
        Backend::Gap,
    ]
    .map(Backend::label);
    assert_eq!(
        without_seats("h264 · HW", &seats),
        "h264",
        "the seat is how this machine reads it, and the row says what the file is"
    );
    let refusal = "no Segment element: not a Matroska file";
    assert_eq!(
        without_seats(refusal, &seats),
        refusal,
        "a refusal is not a seat, and reaches the row whole"
    );
    // The halves keep their separator either way: a stream detail beside a
    // refusal is still two things said in order.
    assert_eq!(
        without_seats(&format!("{refusal} · stereo · 48 kHz"), &seats),
        format!("{refusal} · stereo · 48 kHz")
    );
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
        2,
        "the dock's key handler stops a stroke outside its tab/escape branches"
    );
    for branch in ["is_focus_cycle_key(key)", "is_focus_exit_key(key)"] {
        assert!(
            body.contains(branch),
            "{branch} left the dock's key handler"
        );
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
        8,
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
    for (id, label) in [("dock-tab-src", "Media"), ("dock-tab-clip", "Clip"), ("dock-tab-transcript", "Transcript")] {
        assert!(
            dock.contains(&format!("dock_tab(\"{id}\", \"{label}\"")),
            "the dock's tab strip lost {label}"
        );
    }
    // The sheet is a dialog over the picture, not a dock tab (nothing in the strip moves).
    assert!(
        !dock.contains("dock_tab(\"dock-tab-keys\""),
        "the shortcuts sheet is a dock tab again"
    );
    assert!(
        dock[dock.find("fn keys_sheet(").unwrap()..].contains("\"Keyboard shortcuts\""),
        "the shortcuts sheet draws no title"
    );
    assert!(
        dock.contains("keys_rows_matching("),
        "the shortcuts sheet draws something other than the action registry"
    );
    // A reference sheet, not a state view: no row dims for what the room can do right now.
    assert!(
        !dock.contains("Enable::No(why) | Enable::Hidden(why) => Some(why)"),
        "the shortcuts sheet greys rows by state; it is a reference"
    );
    assert!(
        dock[dock.find("fn keys_sheet(").unwrap()..].contains(".overflow_y_scroll()"),
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

/// DESIGN §6.9: the status bar keeps the selected clip or gap and export
/// progress on the left, the picture's format and the two room verbs
/// (`commands`, `shortcuts`) on the right. Project identity, save state, the
/// CC/settings/full verbs and the position timecode are gone from it; the
/// menu bar carries identity and save state now.
#[test]
fn the_ledger_carries_the_two_room_verbs_and_no_rail_remains() {
    let src = src_text("ui/stance.rs");
    for gone in ["stance-spine", "spine_stance", "SPINE_W", "fn spine("] {
        assert!(
            !src.contains(gone),
            "the rail is still in the stance: {gone}"
        );
    }
    let ledger = &src[src.find("fn ledger(").expect("the ledger strip")
        ..src.find("/// The dock:").expect("the dock frame")];
    // REDESIGN §3.13: no key hints in the status bar. The palette is the menu bar's
    // search field and the shortcuts sheet is in the Help menu.
    for gone in ["\"commands\"", "\"shortcuts\"", "ActionId::ShowActions", "ActionId::ShowKeys", "fn ledger_verb("] {
        assert!(!src.contains(gone) || gone.starts_with("ActionId") && !ledger.contains(gone), "the status bar kept {gone}");
    }
    assert!(src_text("ui/menubar.rs").contains("\"menubar-search\""));
    for gone in [
        "\"CC\"",
        "\"settings\"",
        "\"full\"",
        "ActionId::ToggleSubtitles",
        "ActionId::Settings",
        "ActionId::Fullscreen",
        "ActionId::Save",
        "project_path",
        "timecode(position",
    ] {
        assert!(
            !ledger.contains(gone),
            "the status bar still carries {gone}"
        );
    }
    // The format readout lives in the viewer header alone; the status bar repeats none of it.
    assert!(
        !ledger.contains("s.resolution()") && !ledger.contains("fps_label(player.fps)"),
        "the status bar repeats the viewer header's format"
    );
    assert!(src.contains("size_label(w, h)"), "the viewer header lost the picture's format");
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
    let bench = src_text("ui/bench_stance");
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
        bench.contains("if x >= plate_w && x + label_w <= bed_w && x >= label_end + LABEL_GAP_PX {"),
        "the tick loop still keeps a band clear for a control that is gone"
    );
}

/// The rest of cleanse round 2's bench + ledger, which is all about *one*
/// saying per thing: the head is a dot and a name (its verbs left, checked
/// above), a source's name is drawn once per source -- the audio half of a
/// linked insert has no plate, because the picture plate directly above it
/// and the shared dot already say whose it is -- names lose their extension
/// through one helper, and the ledger's state word is Save's only door now
/// (the time band's parking-spot ghost, filed there because the ledger was a
/// concurrent builder's file, is deleted).
#[test]
fn one_name_per_source_one_stem_helper_and_one_save_door() {
    let bench = src_text("ui/bench_stance");
    assert!(
        !bench.contains(".filter(|_| !(audio && clip.link.is_some()))"),
        "the audio half of a linked insert lost its name again (P7: it is named like the picture half)"
    );
    assert!(
        bench.contains("stem(&s.path)") && !bench.contains("file_name(&s.path)"),
        "a name plate shows the file's extension again"
    );
    let stance = src_text("ui/stance.rs");
    // Project identity and the save state moved from the status bar to the
    // menu bar (DESIGN §6.9), which keeps the same one-stem, one-door rule.
    let menubar = src_text("ui/menubar.rs");
    assert!(
        menubar.contains("stem(&player.project_path)"),
        "the menu bar shows the project's extension again"
    );
    assert!(
        menubar.contains("ActionId::Save"),
        "the menu bar's unsaved state is no longer Save's door"
    );
    // The close request is intercepted and routed through the predicate, and
    // the plate it raises is answered: this repo can build no window in a
    // test ([`Player`] needs a `TestAppContext`), so the wiring itself is
    // what is guarded -- the hook, the rule it asks, and the three answers.
    let main = src_text("main.rs");
    assert!(
        main.contains("on_window_should_close") && main.contains("close_needs_answer()"),
        "alt+F4 closes the window without asking about unsaved work"
    );
    assert!(
        stance.contains("if this.quit_ask {")
            && stance.contains("if this.save_project(cx) {")
            && stance.contains("window.remove_window()"),
        "the unsaved plate is up with no key answering it"
    );
    let popup = src_text("ui/popup.rs");
    assert!(
        popup.contains("fn unsaved_dialog(") && popup.contains("Save changes to {name}?") && popup.contains("Don't save"),
        "the unsaved prompt is no longer a dialog"
    );
    // The overwrite guard (user 2026-09-11: saving over an existing file
    // must warn first, same pattern as the exit guard): Save no longer
    // writes through the action directly -- it routes through the guarded
    // door, which raises the plate with the target's own name, and the
    // plate is answered by its own key block beside the quit plate's.
    let actions = src_text("player/actions.rs");
    assert!(
        actions.contains("ActionId::Save => self.save_guarded(cx)")
            && !actions.contains("ActionId::Save => self.save_project(cx)"),
        "^s overwrites a foreign .edith without asking"
    );
    assert!(
        stance.contains("if this.save_ask {") && popup.contains("fn overwrite_dialog("),
        "the overwrite plate is up with no key answering it"
    );
    assert!(
        !src_text("ui/timeband_stance.rs").contains("ActionId::Save"),
        "Save has two doors: the ledger's state word and the time band's ghost"
    );
    // The helper itself, since three surfaces read one file's name through
    // it: a plain name loses its extension, a name with none is itself, and
    // a path with no name at all still reads as something.
    use crate::files::{file_name, stem};
    use std::path::Path;
    assert_eq!(
        stem(Path::new("/x/he_is_not_the_only_one.mp4")),
        "he_is_not_the_only_one"
    );
    assert_eq!(stem(Path::new("/x/untitled")), "untitled");
    assert_eq!(stem(Path::new("/x/a.b.edith")), "a.b");
    assert_eq!(stem(Path::new("/")), file_name(Path::new("/")));
}

/// DESIGN §4/§5: the hero timecode leads the time band and the ledger carries
/// position. The ruler's left plate was a third resting copy of the same
/// reading -- it stays only for the state change it belongs to, a live scrub.
#[test]
fn the_ruler_plate_shows_the_timecode_only_while_a_scrub_is_live() {
    let bench = src_text("ui/bench_stance");
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
    let start = src
        .find(r#".id("stance-time-band-row")"#)
        .expect("the band row");
    let row = &src[start
        ..start
            + src[start..]
                .find(".child(timecode_cell)")
                .expect("the timecode")];
    assert!(
        row.contains(".overflow_hidden()"),
        "the band row lets its children paint (and be clicked) under the dock: {row}"
    );
    assert!(
        src.contains(".id(\"stance-transport\")") && src.contains(".id(\"stance-tail\")"),
        "the transport group is the flex_1 that centres the glyphs; Loop, volume and Mute close the row"
    );
}

/// DESIGN §7's ladder, cleanse round 2 (2026-09-10): the band is five
/// things -- timecode, the three transport glyphs, the odometer, the contact
/// strip and Export -- and the only sheddable layer left is the chord row
/// under the glyphs. This pins the one threshold (measured in the harness:
/// 454px of fixed groups + the strip's old 120px floor, rounded up -- a 640x600 window
/// reads 539px of band and draws no chords, a 1280x720 one reads 982px and
/// draws them) and that it never comes back off.
#[test]
fn the_band_sheds_its_chords_and_nothing_else() {
    use crate::ui::timeband_stance::band_layers;
    assert_eq!(
        [2000., 982., 580., 579., 539., 0.].map(|w| band_layers(w).chords),
        [true, true, true, false, false, false],
        "the band's one rung is the chord row at 580px"
    );
    // Monotone: a wider band never shows less than a narrower one.
    let mut prev = band_layers(0.);
    for w in (0..2400).step_by(10).map(|w| w as f32) {
        let now = band_layers(w);
        assert!(now.chords || !prev.chords, "layer came back off at {w}px");
        prev = now;
    }
}

/// Every door in this band is at least `HIT_MIN` wide and tall (WCAG 2.5.8).
/// The hitmap read `SetIn w=8`, `VolumeDown w=12`, `Loop w=13`, `SetOut w=15`,
/// `Prev/NextSyncPoint w=16` before this: padding on the shared `ghost`, not a
/// bigger glyph. Those five glyphs left the band in cleanse round 2; the
/// three that stayed are drawn by the same `ghost`, so the floor is still
/// checked where it is spent.
#[test]
fn every_ghost_in_the_time_band_carries_a_full_hit_area() {
    let src = src_text("ui/timeband_stance.rs");
    let start = src.find("fn icon_button(").expect("the band's icon button");
    let button = &src[start..start + src[start..].find("\n}\n").expect("its end")];
    // A Large button is 32px square: a full hit area.
    assert!(button.contains("ButtonSize::Large"), "the band's icon button shrank: {button}");
    const {
        assert!(HIT_MIN >= 24.);
    }
}

/// Mute wore `"{volume}%"` as its glyph while the slider beside it drew the
/// same value: one number, two places. The slider is back on the band (DESIGN
/// §6.3) and the UX audit (U-12) asked for its value beside it: the band
/// writes the level exactly once, in its own `volume_value`, and never as
/// Mute's word again.
#[test]
fn the_band_writes_the_monitoring_level_once_beside_its_slider() {
    let src = src_text("ui/timeband_stance.rs");
    assert_eq!(
        src.matches("player.volume.percent()").count(),
        1,
        "the level is written once, in the slider's tooltip"
    );
    assert!(!src.contains("fn volume_value("), "the % number is back beside the slider");
    assert!(
        src.contains("if player.volume.muted { Icon::VolumeX } else { Icon::Volume2 }"),
        "Mute swaps its icon with its state"
    );
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
    // Cleanse round 2: the shuttle pair left the band, so the three that
    // stayed are what must still differ by shape.
    let shapes = ["Play", "StepBack", "StepForward"].map(glyph);
    let mut seen = shapes.to_vec();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        shapes.len(),
        "two transport verbs share one glyph: {shapes:?}"
    );
}

/// The band's odometer is gone (DESIGN §6.3: hero timecode, transport, then
/// Loop, volume and Mute). Its cut count stayed off the band only while the
/// trim readout is armed, so it must not sneak back into the source.
#[test]
fn the_band_has_no_cut_odometer() {
    let src = src_text("ui/timeband_stance.rs");
    for gone in ["odometer", "cut_readout", "stance-cut-readout"] {
        assert!(
            !src.contains(gone),
            "the odometer is back in the time band: {gone}"
        );
    }
}

/// DESIGN §6.3: "Right: Loop, and a volume slider." Mute sits after the slider
/// (its glyph shows the state beside the level it returns to), and the trim
/// readout is the only thing that may join the group, and only while armed.
#[test]
fn the_transport_closes_with_loop_volume_and_mute() {
    let src = src_text("ui/timeband_stance.rs");
    let at = |needle: &str| {
        src.find(needle)
            .unwrap_or_else(|| panic!("the band lost {needle}"))
    };
    let (loop_at, tail, mute_at, volume_at) = (
        at(r#""stance-tb-loop""#),
        at(r#".id("stance-tail")"#),
        at(r#""stance-tb-mute""#),
        at(".child(volume_slider(player, cx))"),
    );
    assert!(
        loop_at < tail && tail < mute_at && mute_at < volume_at,
        "the transport closes with Loop, then the tail reads Mute, volume"
    );
    // The trim line is read by the status strip, not the band: the band never changes while a
    // trim is in hand.
    assert!(
        !src.contains("trim_readout(player)"),
        "the trim readout must not sit in the transport band"
    );
    let ledger = src_text("ui/stance.rs");
    assert!(
        ledger.contains("trim_text(player)") && ledger.contains("trim_in_flight(player)"),
        "the trim line must replace the status strip's readout while a trim is armed or in hand"
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
    // The source preview's one line says what it is and the one way to use it.
    const ALLOWED: &[&str] = &[
        "No file chooser",
        "No file dialog",
        "No folder dialog",
        "Previewing {}. Drag it onto the timeline to use it.",
        // The one wording the path sheet's empty paste is asked to use.
        "Copy a file's path first",
        // The reason Zoom to selection gives with nothing selected, as the feature asks for it.
        "Select a clip first",
        // The reason Mark the beats gives on a picture-only clip, as the feature asks for it.
        "Pick a clip with sound",
    ];
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

/// The Picture list holds the nine files in one order -- a codec's two boxes side by side,
/// the sound-only formats last.
#[test]
fn the_picture_list_pairs_codec_and_container() {
    use crate::ui::settings_stance::{PICTURE_CYCLE, picture_label};
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
            Format::Opus,
        ]
    );
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
    use crate::ui::settings_stance::short_reason;
    let long = "HEVC needs the VA-API plugin, which this machine has not built";
    assert_eq!(short_reason(long), "HEVC needs the VA-API plugin, which");
    assert!(short_reason(long).split_whitespace().count() <= 6);
    assert_eq!(short_reason("no encoder here"), "no encoder here");
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
                // Clips and subtitles carry no tooltip on purpose: the status bar
                // reads the drag hint under the pointer instead. An empty lane bed
                // carries none either: its drop highlight is its feedback.
                None if file == "clip.rs" || file == "sub.rs" || head.contains(".bed\"") => {}
                None => naked.push(format!("{file}:{}: {}", i + 1, line.trim())),
            }
        }
    }
    // The floor moved 40 -> 34 with cleanse round 2 (2026-09-10): the band lost its
    // shuttle/marks/monitoring cluster, the bench its head verbs and `all`, and the dock
    // gave up five named controls in one diff (the three sub-tabs, the sort
    // cycle, the row's preview and stand-in ghosts, against one Add row kept)
    // and a floor above the true count is a red gate, not a blindness check.
    assert!(
        controls >= 34,
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
    let bench = src_text("ui/bench_stance");
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

/// The user could not find the three columns (2026-09-10: "I couldn't realize
/// them when I look at"), because their heads were 12px `ink3` -- the room's
/// quietest type -- with a prose tail running on after each one. The page's
/// answer has since become a tab strip (user 2026-09-19): three columns at
/// once asked the eye to find three anchors in one glance and then to read a
/// column that began mid-screen. The pin moved with the page: the sections are
/// tabs now, each tab wears the dock's own tab look -- [`crate::ui::dock_stance`]'s
/// `dock_tab`, reproduced here because that fn is private to its module -- and
/// each section's one-line explanation rides the tab's hover plate
/// (DESIGN §8), never running on after a head as ambient text.
#[test]
fn the_settings_sections_are_tabs_that_carry_their_own_explanation() {
    let source = src_text("ui/settings_stance.rs");
    let start = source.find("fn settings_tab(").expect("the tab builder");
    let body = &source[start..source[start..].find("\n}\n").unwrap() + start];
    // The dock's own tab look: no pill, no fill; a 1px `ink1` top rule and
    // `ink1` text mark the showing tab, the resting one is `ink3`. Every tab
    // is a `hitmap::control` wearing a plate of its own (the hover-line gate
    // below counts the plates one to one).
    for needle in [
        "type_scale::body()",
        "border_b_2()",
        "tokens().ink1",
        "tokens().ink2",
        ".cursor_pointer()",
        ".tooltip(",
        "hitmap::control(",
    ] {
        assert!(body.contains(needle), "a settings tab lost {needle}");
    }
    assert!(
        !body.contains("type_scale::head()"),
        "the settings tabs fell back to the 12px in-list section head"
    );
    // Three tabs, one per section, named the way the room names the section.
    for tab in ["project", "editor", "keys"] {
        assert!(
            source.contains(&format!("\"settings-tab-{tab}\"")),
            "the settings strip has no {tab} tab"
        );
    }
    // Each section's one-line explanation is the tab's plate now.
    for explanation in [
        "stored in the .edith file",
        "this machine, this window",
        "every shortcut, and how to rebind it",
    ] {
        assert!(
            source.contains(explanation),
            "a settings tab lost its explanation: {explanation}"
        );
    }
    // ...and none of them runs on after a head as ambient text.
    for run_on in ["Project \u{b7}", "Editor \u{b7}", "Export \u{b7}"] {
        assert!(
            !source.contains(run_on),
            "a settings head grew its prose tail back: {run_on}"
        );
    }
    // One section shows at a time: the plate mounts the showing tab's own
    // section and nothing else -- a page that drew all three under the strip
    // would be the three columns back with the strip doing nothing.
    for (tab, section) in [
        ("SettingsTab::Project", "project_section(player, cx)"),
        ("SettingsTab::Editor", "editor_section(player, cx)"),
    ] {
        assert!(
            source.contains(&format!("{tab} => {section}")),
            "the {tab} tab no longer shows {section}"
        );
    }
}

/// The page's own geometry, after the user retired the below-picture sheet
/// (2026-09-19: "this settings section should be a modal instead of covering
/// the timeline"). The page is a centred modal now -- a centred scrim and a
/// plate of one tabbed column's own measure, never `w_full()`, which put a
/// `justify_between` row's value a window away from its label, and never the
/// [`crate::ui::stance::below_picture_floor`] anchor that laid the sheet over
/// the bench at the window's full width. The height is a viewport cap with
/// headroom, the rows scroll under the strip when a short window cannot hold
/// them, and the modal keeps the two things its callers read: the
/// `settings-card` id and the press-away exit (`this.close_card()`, the same
/// one every other card's scrim leads with). A source scan, like every other
/// guard on this page -- this binary renders no window of its own.
#[test]
fn the_settings_page_is_a_centred_modal_with_one_column_of_measure() {
    let source = src_text("ui/settings_stance.rs");
    let start = source
        .find("pub(crate) fn render(")
        .expect("the page's render fn");
    let body = &source[start..];
    let end = body.find("\n}\n").map_or(body.len(), |at| at + 2);
    let body = &body[..end];

    // Centred on both axes, and the below-picture sheet gone with the rest
    // of it: no floor arithmetic, no bottom-edge alignment, no full-width
    // plate.
    // (The kit dialog shell centres it on both axes.)
    for needle in ["kit::dialog_layer(", "kit::dialog(\"settings-card\""] {
        assert!(body.contains(needle), "the settings modal lost {needle}");
    }
    for gone in ["below_picture_floor", "items_end()", "w_full()"] {
        assert!(
            !body.contains(gone),
            "the settings page drew {gone} again -- that is the below-picture sheet, \
             not the centred modal the user asked for"
        );
    }
    // The plate: one column's own measure with its yield for a narrow centre
    // column, a capped height, and the rows scrolled inside the cap.
    for needle in [
        "\"settings-card\"",
        "SETTINGS_W",
        ".max_w(relative(1.))",
        "\"settings-rows\"",
        "overflow_y_scroll()",
        "more_below(",
    ] {
        assert!(
            body.contains(needle),
            "the settings modal lost {needle} -- the plate is no longer one capped \
             column with its rows scrolling"
        );
    }
    // One measure for every tab (user 2026-09-20: "switching between them
    // feels weird" -- the plate hugged its rows, so PROJECT's seven and
    // EXPORT's four resized it under the hand). The rows area wears the
    // tallest section's fixed height, and that height is PROJECT's own
    // arithmetic: count the section's rows out of its body rather than
    // trusting the constant's comment.
    assert!(
        body.contains(".h(px(rows_h))"),
        "the settings rows lost their fixed measure -- a tab switch resizes the plate again"
    );
    let project = &source[source
        .find("fn project_section(")
        .expect("the PROJECT section")..];
    let project = &project[..project.find("\n}\n").map_or(project.len(), |at| at + 2)];
    assert_eq!(
        project.matches("saved_line(").count(),
        1,
        "the PROJECT section no longer ends on its storage line"
    );
    // PROJECT at its tallest: three list rows, the HDR row and its two-line
    // hint, both switches and the storage line.
    let tallest = 7. * CONTROL_H + 36. + 7. * 2.;
    assert!(
        crate::ui::settings_stance::SETTINGS_ROWS_H >= tallest,
        "SETTINGS_ROWS_H no longer holds the PROJECT section"
    );
    assert!(
        body.contains("this.settings_open = false"),
        "the settings scrim swallows the press without closing the modal"
    );
}

/// The monitoring level is not a setting: it lives on the transport (slider and mute) and on
/// `m` / the volume keys. Settings -> Editor carries no Level row, and the footer note does not
/// mention one.
#[test]
fn the_editor_section_has_no_monitoring_level_row() {
    let source = src_text("ui/settings_stance.rs");
    let editor_start = source
        .find("fn editor_section(")
        .expect("the editor section");
    let render_start = source
        .find("pub(crate) fn render(")
        .expect("the page's render fn");
    let editor_body = &source[editor_start..render_start];
    for needle in ["\"settings-level\"", "\"Level\"", "Level is not saved"] {
        assert!(
            !editor_body.contains(needle),
            "the EDITOR section grew the monitoring level back ({needle})"
        );
    }
}

/// The wheel's own arithmetic: one notch is one key press (5%), and the count
/// stops at both ends rather than wrapping silence into full volume -- the
/// saturating pair [`Volume::step`] is written for. Pure, so it runs without
/// a window.
#[test]
fn the_level_wheel_steps_five_percent_and_clamps_at_both_ends() {
    use crate::transport::Volume;
    let mut volume = Volume::default();
    assert_eq!(volume.percent(), 100);
    for expected in [95, 90, 85, 80] {
        volume.step(false);
        assert_eq!(volume.percent(), expected, "a notch down is 5%");
    }
    for _ in 0..40 {
        volume.step(false);
    }
    assert_eq!(volume.percent(), 0, "the level floors at silence");
    volume.step(false);
    assert_eq!(volume.percent(), 0, "a notch below silence is silence");
    for _ in 0..40 {
        volume.step(true);
    }
    assert_eq!(volume.percent(), 100, "the level ceilings at full");
    volume.step(true);
    assert_eq!(volume.percent(), 100, "a notch above full is full");
    // Turning it down while muted is not what makes sound come out.
    volume.muted = true;
    volume.step(false);
    assert!(volume.muted && volume.percent() == 95);
}

/// The type twin of `no_colour_is_written_outside_the_theme`: DESIGN §2.4
/// sets 12px as the floor, and `ui::type_scale` is where that floor lives.
/// A literal size under it written at a call site is a size the next type
/// sweep will not see -- how 9px and 10.5px text reached the preview's
/// labels and 11px reached the menus. Every literal is read in both forms a
/// call site writes one: `text_size(px(N))`, and `label(N, ..)` / `mono(N, ..)`
/// taking it as the first argument.
#[test]
fn no_ui_text_is_set_literally_below_the_floor() {
    const FLOOR: f32 = 12.;
    let source = ui_source();
    assert!(
        source.matches("text_size(").count() >= 20,
        "the text-size call sites moved; this scan is blind"
    );

    let mut stray = Vec::new();
    for needle in ["text_size(px(", "label(", "mono("] {
        for (at, _) in source.match_indices(needle) {
            let prev = source[..at].chars().next_back();
            let ident_before = prev.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if needle != "text_size(px(" && ident_before {
                continue;
            }
            let literal: String = source[at + needle.len()..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(size) = literal.parse::<f32>()
                && size < FLOOR
            {
                stray.push(format!("{needle}{literal}"));
            }
        }
    }
    assert!(
        stray.is_empty(),
        "literal text size(s) under DESIGN §2.4's {FLOOR}px floor: {stray:?}"
    );
}

/// DESIGN §2.3 and §4: every family is held to the same floor on every surface
/// a line of text can sit on. INK3 is the tertiary tier -- labels, keys and
/// metadata -- so it has to be legible too, and it has to be its own colour:
/// a family whose INK3 aliased INK4 would put the disabled grey on information.
#[test]
fn every_palette_has_three_legible_inks_on_every_surface() {
    for id in crate::ui::theme::PaletteId::ALL {
        let p = id.palette();
        for (name, ink) in [("ink1", p.INK1), ("ink2", p.INK2), ("ink3", p.INK3)] {
            for (surface, bg) in [
                ("canvas", p.DARK_CANVAS),
                ("panel", p.DARK_PANEL),
                ("raised", p.DARK_RAISED),
            ] {
                assert!(
                    contrast(ink, bg) >= 4.5,
                    "{id:?} {name} on {surface}: {:.2}",
                    contrast(ink, bg)
                );
            }
        }
        assert_ne!(p.INK3, p.INK4, "{id:?}: INK3 aliases the disabled ink");
        assert_ne!(p.INK3, p.INK2, "{id:?}: INK3 aliases the secondary ink");
    }
}

/// DESIGN §2.1 is the mock's numbers, and the default is the mock's room: a
/// fresh install opens on it. A saved pick still wins, since `load` only falls
/// back to the default when there is nothing to read.
#[test]
fn darkroom_is_the_default_and_matches_design_2_1() {
    use crate::ui::theme::{PaletteId, darkroom};
    assert_eq!(PaletteId::DEFAULT, PaletteId::Darkroom);
    assert_eq!(darkroom::BG_CANVAS, 0x0b0c0e);
    assert_eq!(darkroom::BG_PANEL, 0x131518);
    assert_eq!(darkroom::BG_RAISED, 0x1b1e22);
    assert_eq!(darkroom::BG_HOVER, 0x24282d);
    assert_eq!(darkroom::STROKE_DIVIDER, 0x2a2e34);
    assert_eq!(darkroom::INK1, 0xe8eaed);
    assert_eq!(darkroom::INK2, 0xb4bac1);
    assert_eq!(darkroom::INK3, 0x8d959e);
    assert_eq!(darkroom::INK4, 0x777f88);
    assert_eq!(darkroom::NOTICE_LOOK, 0xd1b564);
    assert_eq!(darkroom::NOTICE_DECIDE, 0xe07a7a);
}

/// DESIGN §4.3 and §4.10: the hit sizes a hand is given. A trim zone is 24 px
/// centred on its edge, shrinks inside a short clip so the middle of it stays
/// a move target, and gives its outside half to the clip under the pointer
/// where two clips meet.
#[test]
fn a_trim_zone_is_24px_centred_on_the_edge_and_yields_at_a_join() {
    use crate::{TRIM_HANDLE, TRIM_REACH, trim_zone};

    assert_eq!(2. * TRIM_REACH, HIT_MIN, "24 px, centred on the edge");
    const { assert!(TRIM_HANDLE <= TRIM_REACH, "a drawn handle fits in its zone") };
    // A long clip with room on both sides: the full reach each way.
    assert_eq!(
        trim_zone(400., f32::INFINITY),
        (TRIM_REACH, TRIM_REACH),
        "12 px inside, 12 px outside"
    );
    // A short clip gives up inside reach, never the middle third of itself.
    for width in [18., 24., 36., 47.] {
        let (inside, outside) = trim_zone(width, f32::INFINITY);
        assert!(inside <= width / 4. + 1e-4, "{width} px: inside {inside}");
        assert!(
            width - 2. * inside >= width / 3.,
            "{width} px leaves no middle third"
        );
        assert!(inside + outside <= HIT_MIN);
    }
    assert_eq!(trim_zone(48., f32::INFINITY).0, TRIM_REACH);
    // Touching clips: the outside half is nothing, so the join belongs to the
    // clip the pointer is on, entirely.
    assert_eq!(trim_zone(400., 0.).1, 0.);
    // A gap: each side takes half of it, and the two halves meet exactly.
    let gap = 10.;
    assert_eq!(trim_zone(400., gap).1, gap / 2.);
    assert_eq!(trim_zone(400., -3.).1, 0., "an overlap reaches nothing");
}

#[test]
fn a_clip_draws_its_trims_outside_its_clipped_box() {
    let src = src_text("ui/bench_stance/clip.rs");
    let start = src.find("fn clip_box(").expect("the clip box");
    let body = &src[start..start + src[start..].find("\n}\n").expect("its end")];
    // The box clips its own contents, so the zones must be siblings of it:
    // the body is built first, then the wrapper carries the zones after it.
    let boxed = body.find("let body = div()").expect("the box");
    assert!(body.find(".overflow_hidden()").expect("clipped") > boxed);
    assert!(
        body.find(".child(body)").expect("the wrapper") > boxed,
        "the zones hang off the wrapper, not the clipped box: {body}"
    );
    assert!(
        body.find("trim_zone(").expect("the zones") > body.find(".child(body)").expect("wrapper"),
        "a zone built inside the box would be clipped to its edges"
    );
    // A selected clip shows its two 12 px handles, and its outline is 2 px.
    // (REDESIGN §3.6: 8x20 pills, and the 2px accent outline is an overlay on the wrapper.)
    assert!(body.contains(".w(px(8.))") && body.contains(".h(px(20.))"));
    assert!(body.contains(".border_2()") && body.contains("tokens().accent"));
    // Fades and the dissolve are hit areas of the full 24 px, drawn at 10 px.
    assert!(body.matches(".w(px(HIT_MIN))").count() >= 2, "{body}");
    assert!(body.contains("FADE_HANDLE_W"));
    // Their own hit areas sit above the trim zones, which come first.
    assert!(
        body.find("trim_zone(").expect("zones") < body.find("bench-clip-fade").expect("fades"),
        "a fade must be painted above the trim strip under it"
    );
    // The subtitle cue's outline matches the clip's.
    let sub_src = src_text("ui/bench_stance/sub.rs");
    let sub = sub_src.find("fn sub_box(").expect("the subtitle cue");
    let sub = &sub_src[sub..sub + sub_src[sub..].find("\n}\n").expect("its end")];
    assert!(sub.contains(".border_2()") && sub.contains("tokens().accent"));
}

/// DESIGN §8: a toast never covers the timeline or the picture. One at a time,
/// it lives in the status bar, whose height is fixed.
#[test]
fn a_toast_lives_in_the_status_bar_and_never_over_the_timeline() {
    let text = src_text("ui/stance.rs");
    assert!(
        !text.contains("fn toast_stack(") && !text.contains("stance-toasts"),
        "a floating toast stack is back over the timeline"
    );
    let at = text.find("fn ledger(").expect("the status bar");
    let ledger = &text[at..at + text[at..].find("\n}\n").expect("its end")];
    assert!(
        ledger.contains("current_toast(&player.notices)") && ledger.contains("toast_card("),
        "the status bar must show one toast: the open question, else the newest"
    );
    assert!(text.contains(".h(px(LEDGER_H - 2.))"), "the toast must fit the bar's height");
}

#[test]
fn ink4_is_only_for_disabled_states() {
    // ink4 (`INK4()`, which is `FG_DISABLED`) is for disabled controls only
    // (DESIGN §2.1, §4.1). Every use counted here must be a disabled state: a
    // blocked export, a refused format's value, an action the room cannot take
    // right now. Labels, keys, metadata, hints and empty states are information
    // and take INK3(). A new use is a new information-or-disabled decision, so
    // check which it is, and only then change the count below.
    let mut uses = Vec::new();
    for path in source_files() {
        if path.file_name() == Some("theme.rs".as_ref()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a source file");
        for (line, code) in text.lines().enumerate() {
            if code.contains("INK4()") || code.contains("FG_DISABLED") {
                let shown = path.strip_prefix(src_dir()).unwrap_or(&path);
                uses.push(format!("{}:{}", shown.display(), line + 1));
            }
        }
    }
    // (The bed's gap hatch was the ninth; REDESIGN §3.6 removed every hatch.)
    // None remain: P5 moved them to `tokens().ink4`, and the shortcuts sheet (which held the
    // last two) now dims a refused row with opacity and gives the reason on hover.
    assert_eq!(
        uses.len(),
        0,
        "a new INK4()/FG_DISABLED line must be a disabled control, not information: {uses:#?}"
    );
}

/// A first media into an empty timeline lands and fits (DESIGN §7), by the
/// launch argument and by an import alike. The fit is worked out against the
/// bed the timeline was painted on, and a bed that has not been painted yet
/// has no width to fit into: the fit waits for the first frame that has one
/// rather than falling back to the default 60 px/s, which is the screenshot
/// that made a 12 s clip fill a third of a 1560 px bed.
#[test]
fn a_first_media_fits_the_bed_it_is_painted_on_and_waits_for_one_if_unpainted() {
    let fit = |bed: f32| {
        View {
            scale: Scale::default(),
            bed,
            duration: 12.,
            fps: 24.,
        }
        .fit()
    };
    let painted = fit(1560.);
    assert!(
        (painted.pps - 1560. / 12.).abs() < 1e-9 && painted.start == 0.,
        "a 12 s edit on a 1560 px bed is 130 px/s from the head, not {painted:?}"
    );
    assert_eq!(
        fit(0.).pps,
        PPS_DEFAULT,
        "an unpainted bed is the fallback this test exists to keep out of a fit"
    );

    let view = src_text("player/view.rs");
    let zoom = &view[view.find("fn zoom_fit(").expect("zoom_fit")..];
    let zoom = &zoom[..zoom.find("\n    }\n").expect("zoom_fit body")];
    assert!(
        zoom.contains("false => self.fit_pending = true"),
        "zoom_fit must defer when the bed is unpainted: {zoom}"
    );
    let render = src_text("render.rs");
    let frame = &render[render.find("if self.fit_pending {").expect("render consumes the fit")..];
    assert!(
        frame.contains("request_animation_frame") && frame.contains("self.fit_pending = false"),
        "the deferred fit must be taken on the first measured frame and keep the loop alive until then"
    );
    let library = src_text("player/library.rs");
    let launch = &library[library
        .find("Landed::Media(opened, place) => {")
        .expect("the launch arm of take_import")..];
    let launch = &launch[..launch.find("cx.notify();").expect("arm end")];
    assert!(
        launch.contains("if place") && launch.contains("self.zoom_fit(cx);"),
        "the file argv named must fit when it lands as the timeline: {launch}"
    );
}

/// A fit spreads the whole edit across about 98% of the bed, and the rest is the
/// margin (DESIGN §7). The share is read off the source, so the test and the
/// fit cannot drift apart.
#[test]
fn a_fit_leaves_about_two_percent_of_the_bed_as_margin() {
    let view = src_text("player/view.rs");
    let share: f32 = view
        .lines()
        .find_map(|line| {
            line.strip_prefix("const FIT_SHARE: f32 = ")
                .and_then(|rest| rest.strip_suffix(';'))
                .and_then(|num| num.parse().ok())
        })
        .expect("FIT_SHARE is a plain f32 literal");
    assert!(
        (0.97..=0.99).contains(&share),
        "a fit is about 98% of the bed, not {share}"
    );
    let bed = 1560.;
    let fit = View {
        scale: Scale::default(),
        bed: bed * share,
        duration: 12.,
        fps: 24.,
    }
    .fit();
    assert!(
        (fit.width_px(12.) - bed * share).abs() < 0.5 && fit.start == 0.,
        "the whole 12 s edit must end at the margin, 98% of a 1560 px bed: {fit:?}"
    );
    let fit_to_bed = &view[view.find("fn fit_to_bed(").expect("fit_to_bed")..];
    let fit_to_bed = &fit_to_bed[..fit_to_bed.find("\n    }\n").expect("fit_to_bed body")];
    assert!(
        fit_to_bed.contains("bed: view.bed * FIT_SHARE"),
        "fit_to_bed must fit into the share of the bed, not the whole bed: {fit_to_bed}"
    );
}

/// A fit stays in force while the bed changes width: the first frame measures
/// the initial window size, the compositor then tiles the window, and the next
/// frame refits for the new width. A hand's zoom, scroll or thumb drag ends the
/// fit, so a resize after one keeps the view the hand chose (DESIGN §7).
#[test]
fn a_fit_follows_a_resized_bed_until_a_hand_zooms_or_scrolls() {
    fn body<'a>(text: &'a str, sig: &str) -> &'a str {
        let from = &text[text.find(sig).unwrap_or_else(|| panic!("{sig}"))..];
        &from[..from.find("\n    }\n").unwrap_or_else(|| panic!("end of {sig}"))]
    }
    let player = src_text("main.rs");
    assert!(
        player.contains("fitted_bed: Option<f32>") && player.contains("fitted_bed: None,"),
        "Player must hold the bed width of the fit, and start with none"
    );

    let view = src_text("player/view.rs");
    assert!(
        body(&view, "fn fit_to_bed(").contains("self.fitted_bed = Some(view.bed);"),
        "a fit must record the bed width it was worked out for"
    );
    let zoom_fit = body(&view, "fn zoom_fit(");
    assert!(
        zoom_fit.contains("self.fit_to_bed();"),
        "zoom_fit must fit through fit_to_bed, so the fit is the sticky one: {zoom_fit}"
    );
    for sig in ["fn zoom(", "fn scroll_view(", "fn pan_window_to("] {
        assert!(
            body(&view, sig).contains("self.fitted_bed = None;"),
            "{sig} is a hand's view and must end the sticky fit"
        );
    }
    // The thumb and the overview's window box both pan through pan_window_to,
    // and a drag holds against the follow the way a scroll does.
    assert!(
        body(&view, "fn pan_window_to(").contains("self.panned = true;"),
        "a window drag must hold the view against the playhead's follow"
    );
    assert!(
        body(&view, "fn scroll_drag_to(").contains("self.pan_window_to("),
        "the thumb's drag must pan through the shared door"
    );

    let render = src_text("render.rs");
    let frame = &render[render
        .find("if let Some(fitted) = self.fitted_bed {")
        .expect("render refits a fit still in force")..];
    let frame = &frame[..frame.find("ui::stance::render").expect("render end")];
    assert!(
        frame.contains("bed != fitted")
            && frame.contains("bed > 0.")
            && frame.contains("self.fit_to_bed();"),
        "render must refit a sticky fit when the bed width differs, and never fit a zero width: \
         {frame}"
    );
}

/// An action's hover plate reads its key off the keymap, not off a literal
/// written beside the action (DESIGN §4.5): the name, `Keymap::display` as the
/// key, and a refused action's reason on its own line.
#[test]
fn an_action_tooltip_reads_its_key_from_the_keymap() {
    use crate::oracle::Enable;
    use crate::ui::widgets::action_parts;
    let keymap = crate::keymap::Keymap::defaults();
    for action in crate::keymap::ActionId::ALL {
        let parts = action_parts(&keymap, action, Enable::Yes);
        assert_eq!(parts.name.to_string(), action.label());
        assert_eq!(
            parts.key.map(|k| k.to_string()),
            Some(keymap.display(action)),
            "{action:?}: the plate's key must be the keymap's own reading"
        );
        assert_eq!(parts.why, None, "{action:?}: an enabled action gives no reason");
    }
    let refused = action_parts(
        &keymap,
        crate::keymap::ActionId::Cut,
        Enable::No("Put the playhead over a clip"),
    );
    assert_eq!(
        refused.why.map(|w| w.to_string()),
        Some("Put the playhead over a clip".to_string()),
        "a refused action's reason is the oracle's, on its own line"
    );
    assert_eq!(
        refused.key.map(|k| k.to_string()),
        Some(keymap.display(crate::keymap::ActionId::Cut)),
    );
    // And the live helper takes its parts from that same keymap, not from a
    // second source of its own.
    let body = fn_body("action_hover");
    assert!(
        body.contains("action_parts(&player.keymap, action"),
        "action_hover must build its plate from the player's keymap: {body}"
    );
}

/// The edit toolbar keeps DESIGN §6.4's group order, with a hairline rule
/// between the groups, and every labelled button reads its key from the
/// keymap rather than typing one. The commands must appear in the source in
/// that order (a sequential search, so a swap or a missing rule fails), and the
/// two helpers that draw a key must take it from `Keymap::display` or from
/// `action_hover`, which reads the keymap's chord.
#[test]
fn the_edit_toolbar_keeps_its_group_order_and_reads_every_key_from_the_keymap() {
    let src = src_text("ui/toolbar.rs");
    let render = &src[src
        .find("pub(crate) fn render(")
        .expect("the toolbar's render")..];
    let order = [
        "ActionId::Cut",
        "ActionId::Delete",
        "ActionId::Lift",
        "rule()",
        "ActionId::Undo",
        "ActionId::Redo",
        "rule()",
        "ActionId::ToggleSnap",
        "ActionId::SetIn",
        "ActionId::SetOut",
        "ActionId::ZoomOut",
        "zoom_slider(",
        "ActionId::ZoomIn",
        "ActionId::ZoomFit",
    ];
    let mut at = 0;
    for needle in order {
        let found = render[at..]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} must follow the groups before it: {render}"));
        at += found + needle.len();
    }
    let helper = |sig: &str| {
        let start = src.find(sig).unwrap_or_else(|| panic!("{sig} must exist"));
        let rest = &src[start + sig.len()..];
        let end = rest.find("\n}\n").expect("the helper's closing brace");
        &rest[..end]
    };
    assert!(
        helper("fn button(").contains("toolbar_tip(player, action)")
            && helper("fn toolbar_tip(").contains("player.keymap.display(action)"),
        "a toolbar button must name its key through the keymap's hover, not print it"
    );
    assert!(!src.contains("keymap.primary("), "the toolbar prints a key");
}

/// REDESIGN §3.8: a refused row says why in one line and stays the row height.
#[test]
fn a_refused_media_row_keeps_the_row_height() {
    use crate::ui::dock_stance::{REFUSED_ROW_H, SOURCE_ROW_H, source_row_h};
    assert_eq!(source_row_h(true), SOURCE_ROW_H);
    assert_eq!(source_row_h(false), REFUSED_ROW_H);
    assert_eq!(source_row_h(false), source_row_h(true));
    let dock = src_text("ui/dock_stance.rs");
    assert!(
        !dock.contains(".when(usable, |d| d.truncate())"),
        "the meta line truncates a refusal again"
    );
}

/// Plain words: a source on no lane says where it is, never "unused" or
/// "not placed".
#[test]
fn the_media_list_says_not_on_timeline_in_plain_words() {
    use crate::ui::dock_stance::NOT_ON_TIMELINE;
    assert_eq!(NOT_ON_TIMELINE, "not on timeline");
    let dock = src_text("ui/dock_stance.rs");
    let code: String = dock
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("\"unused\"") && !code.contains("\"not placed\""),
        "jargon is back in the media list"
    );
}

/// The search box is a bordered field with a placeholder, and the media
/// rows carry a 64x36 poster.
#[test]
fn the_search_field_is_bordered_and_rows_carry_a_poster() {
    use crate::ui::dock_stance::SEARCH_PLACEHOLDER;
    assert!(SEARCH_PLACEHOLDER.contains("Search media"));
    let dock = src_text("ui/dock_stance.rs");
    let field = &dock[dock.find(r#".id("dock-filter")"#).expect("the field")..];
    let field = &field[..field.find(".child(match").expect("its text")];
    assert!(
        field.contains(".border_1()") && field.contains("t.e2"),
        "the search field lost its border"
    );
    assert!(
        dock.contains("const POSTER_W: f32 = 88.;") && dock.contains("const POSTER_H: f32 = 50.;")
    );
    assert!(dock.contains("let poster = poster(player, &row.path"));
}

/// The empty states each carry a call to action.
#[test]
fn the_empty_viewer_media_list_and_lanes_each_say_what_to_do() {
    use crate::ui::bench_stance::lane::{empty_lane_hint, wave_is_silent};
    assert_eq!(empty_lane_hint(LaneKind::Video), "Drop media here");
    assert_eq!(empty_lane_hint(LaneKind::Subtitle), "Drop captions here");
    let dock = src_text("ui/dock_stance.rs");
    assert!(dock.contains("EMPTY_LIBRARY_HINT") && dock.contains("pick_and_import"));
    let render = src_text("render.rs");
    assert!(
        render.contains("self.empty_viewer(cx)"),
        "the viewer lost its Import box"
    );
    assert!(!src_text("ui/widgets.rs").contains("No project"));
    // Silent is an answer; loading is not.
    assert!(wave_is_silent(&Wave::Silent));
    let flat = std::sync::Arc::new(vec![(0., 0.); 4]);
    let loud = std::sync::Arc::new(vec![(-0.5, 0.5)]);
    assert!(wave_is_silent(&Wave::Peaks(flat)));
    assert!(!wave_is_silent(&Wave::Peaks(loud)));
    assert!(!wave_is_silent(&Wave::Loading));
}

/// No layout shift (Nielsen): a toggle whose word changes with its state keeps
/// the width of its widest spelling, so the row around it does not move. The
/// crate has no gpui harness, so this reads the sources the way its siblings do.
#[test]
fn a_toggle_whose_word_changes_keeps_one_width() {
    let band = src_text("ui/timeband_stance.rs");
    assert!(
        !band.contains("\"Muted\""),
        "the transport has icons, not words that change"
    );
    let bar = src_text("ui/toolbar.rs");
    assert!(
        bar.contains("\"Snap\"") && !bar.contains("Snap on") && !bar.contains("Snap off"),
        "Snap keeps one word; its state is the fill"
    );
    let stance = src_text("ui/stance.rs");
    assert!(
        stance.contains("// `Exporting 5%` to `Exporting 100%`: held at the widest."),
        "the status bar's export readout must hold its width"
    );
}

/// 1280x720: the toolbar sheds its keys (kept in the tooltips) instead of
/// pushing the zoom group and Export off the window.
#[test]
fn the_toolbar_sheds_its_keys_rather_than_clip_at_a_narrow_column() {
    let bar = src_text("ui/toolbar.rs");
    assert!(
        bar.contains("const COMPACT_BELOW: f32 = 900.;") && bar.contains("words.then_some"),
        "the toolbar no longer sheds words at narrow widths"
    );
}

/// P6: monospace is the transport timecode's face. The media list, the status bar, the lanes, the
/// viewer and its welcome hero speak in Inter roles (`ui::type_scale`), never `mono`.
#[test]
fn the_media_list_status_bar_lanes_and_viewer_use_no_mono() {
    for file in [
        "ui/dock_stance.rs",
        "ui/stance.rs",
        "ui/preview.rs",
        "ui/timeband_stance.rs",
        "ui/bench_stance/lane.rs",
        "ui/bench_stance/sub.rs",
        "ui/bench_stance/clip.rs",
        "ui/toolbar.rs",
    ] {
        let text = src_text(file);
        assert!(
            !text.contains("mono(") && !text.contains("SPLINE_SANS_MONO"),
            "{file} sets the mono face; use a type_scale role"
        );
    }
}
