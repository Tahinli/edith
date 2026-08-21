//! Where a moment is drawn: drops and snaps, the zoom and the scroll, the
//! paths a file is written to, and the strokes the handler answers.

use super::*;

/// What a drop reads: the frame under the pointer, through the same scale
/// the boxes are drawn through. Zoomed in, the same pixel is a different
/// frame -- which is the whole reason `Player::frame_under` goes through
/// [`Scale`] rather than through the duration alone.
#[test]
fn a_drop_reads_the_frame_under_the_pointer_at_every_zoom() {
    // A 200 px bed inset by the panel's padding, 10 seconds at 30 fps.
    let bed: Bounds<Pixels> = Bounds {
        origin: point(px(12.), px(400.)),
        size: size(px(200.), px(6.)),
    };
    let fps = 30.;
    // The frame a pointer at window `x` names, exactly as `frame_under`
    // composes it.
    let under = |scale: Scale, x: f32| frame_at(scale.time_at(px_along(px(x), bed)), fps);

    // The whole 10 s timeline across the 200 px bed: 20 px to the second,
    // so halfway along is frame 150.
    let fit = Scale {
        pps: 20.,
        start: 0.,
    };
    assert_eq!(under(fit, 12.), 0);
    assert_eq!(under(fit, 112.), 150);
    assert_eq!(under(fit, 212.), 300);

    // Four times in, starting at second 5: the same middle pixel is now the
    // frame in the middle of seconds 5..7.5, and the left edge is not 0.
    let zoomed = Scale {
        pps: 80.,
        start: 5.,
    };
    assert_eq!(under(zoomed, 12.), 150);
    assert_eq!(under(zoomed, 112.), 187);
    assert_eq!(under(zoomed, 212.), 225);
    // ...and a pointer that slid off either end of the bed names an end of
    // the bed, never a pixel outside it.
    assert_eq!(under(zoomed, 0.), 150);
    assert_eq!(under(fit, 9999.), 300);
}

/// The snap: a clip let go a few frames off a neighbour's edge lands *on*
/// it, by whichever of its own ends is nearer, and a clip let go in open bed
/// stays exactly where the hand left it.
#[test]
fn a_dropped_clip_snaps_to_the_edges_worth_landing_on() {
    // A neighbour covering [100, 160) and the playhead at 300.
    let marks = [100, 160, 300, 0];
    // Head a frame short of the neighbour's tail: laid end to end with it.
    assert_eq!(snapped(158, 40, 4, &marks), 160);
    // Tail a frame into its head: pulled back so the two meet exactly.
    assert_eq!(snapped(62, 40, 4, &marks), 60);
    // The playhead is an edge like any other.
    assert_eq!(snapped(298, 40, 4, &marks), 300);
    // Outside the tolerance nothing moves -- a gap the hand meant to leave
    // is a gap.
    assert_eq!(snapped(150, 40, 4, &marks), 150);
    // No tolerance at all (zoomed right in, where a few pixels are worth
    // less than a frame) is no snap: single frames are placed by hand.
    assert_eq!(snapped(158, 40, 0, &marks), 158);
    // The nearer edge wins when two are in reach.
    assert_eq!(snapped(101, 40, 8, &marks), 100);
    // ...and a mark closer to the head than `len` cannot pull the clip to a
    // negative start.
    assert_eq!(snapped(2, 40, 4, &marks), 0);
}

/// The shadow a drag draws and the drop that commits are one answer: both
/// ask [`landing`], so the box seen in flight is the box the release leaves
/// behind. What this pins down is the composition around the snap -- the
/// grab offset comes off *before* the magnet, or a clip taken by its tail
/// would land a boxful late.
#[test]
fn a_ghost_and_a_drop_are_one_landing() {
    // A neighbour covering [100, 160), the playhead at 300, and a 40 frame
    // clip taken hold of 12 frames in.
    let marks = [100, 160, 300, 0];
    let (len, grab, tol) = (40, 12, 4);
    // Pointer at frame 170: the head is 12 frames behind it, at 158, which
    // is a frame short of the neighbour's tail -- so both the ghost and the
    // drop say 160, and the line stands on the mark that pulled it.
    assert_eq!(landing(170, grab, len, true, tol, &marks), (160, Some(160)));
    // Without the grab taken off first, the same pointer would land the
    // box at 170 and no mark would be in reach: the offset is not cosmetic.
    assert_eq!(landing(170, 0, len, true, tol, &marks), (170, None));
    // A library row carries no grab and no length the engine has measured,
    // which is how `Player::place_frame` asks: only its head lands, on the
    // playhead here.
    assert_eq!(landing(298, 0, 0, true, tol, &marks), (300, Some(300)));
    // The magnet off, ghost and drop agree on the raw frame and no line is
    // drawn -- the frame-by-frame placement the switch is for.
    assert_eq!(landing(170, grab, len, false, tol, &marks), (158, None));
    // A pointer nearer the bed's start than the hand is into the box cannot
    // pull a head below zero.
    assert_eq!(landing(3, grab, len, true, tol, &marks), (0, Some(0)));
}

/// Which lanes tint that shadow as refused: the two kinds of file a lane
/// cannot hold, in the words the release would say them in -- one rule, so
/// what is shown as impossible is exactly what is refused.
#[test]
fn a_lane_refuses_the_files_it_cannot_hold_before_the_release_says_so() {
    let (video, audio) = (Lane::V1, Lane::A1);
    let sound = Path::new("/media/take.mp3");
    let still = Path::new("/media/card.png");
    let movie = Path::new("/media/take.mp4");
    assert_eq!(
        lane_refuses(sound, video).as_deref(),
        Some("NOT ON V1 — take.mp3 has no picture; drop it on an audio lane")
    );
    assert_eq!(
        lane_refuses(still, audio).as_deref(),
        Some("NOT ON A1 — card.png is a still image; drop it on a video lane")
    );
    // ...and every lane a file *can* go on says nothing at all, which is a
    // ghost drawn in the file's own colour.
    assert_eq!(lane_refuses(sound, audio), None);
    assert_eq!(lane_refuses(still, video), None);
    assert_eq!(lane_refuses(movie, video), None);
    // A file with a picture is not refused by an audio lane here: the
    // engine takes its sound onto one, and the words for a video-only file
    // are its own.
    assert_eq!(lane_refuses(movie, audio), None);
    // ...and a subtitle lane holds none of the three: a caption comes off the
    // Subtitles list, which is where the refusal points.
    for file in [sound, still, movie] {
        let why = lane_refuses(file, Lane::S1).expect("a subtitle lane takes no media");
        assert!(why.starts_with("NOT ON S1 — "), "{why}");
        assert!(why.contains("Subtitles list"), "{why}");
    }
    // How the two clocks a caption has meet, which is what a palette row's
    // length is worked out with: a second of words at 30 fps is 30 frames,
    // and a track shorter than one frame is still a caption to place.
    assert_eq!(frames_of_us(1_000_000, 30.), 30);
    assert_eq!(frames_of_us(500_000, 24.), 12);
    assert_eq!(frames_of_us(1_000, 30.), 1);
    assert_eq!(frames_of_us(1_000_000, 0.), 1);
}

/// Where those edges come from: every lane, not the one being dropped on --
/// and never the clip in the hand or the half of it one track down.
#[test]
fn the_marks_are_every_lane_the_playhead_and_the_start() {
    let clip = |start: u32, frames: u32, link| Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start,
        in_frame: 0,
        out_frame: frames,
        source: 0,
        link,
        eq: None,
        color: None,
        transform: None,
        fit: Default::default(),
        speed: Default::default(),
    };
    // A grouped take across two lanes at 100..160, and a lone one on the
    // audio lane at 400..430.
    let video = [clip(100, 60, Some(7))];
    let audio = [clip(100, 60, Some(7)), clip(400, 30, None)];
    let lanes: [&[Clip]; 2] = [&video, &audio];

    // Nothing in the hand: both lanes' edges, the playhead, and 0.
    let mut all = snap_marks(&lanes, None, None, 300);
    all.sort_unstable();
    assert_eq!(all, [0, 100, 100, 160, 160, 300, 400, 430]);

    // The video half in the hand, with its group named as the caller reads it
    // off the pick: its own edges are gone, and so are its group's on the
    // other lane -- both boxes travel with the drag. The lone audio clip is
    // still a target, which is the whole point: a take being carried on V1
    // lands flush with a sound on A1.
    let mut carried = snap_marks(&lanes, Some((0, 0)), Some(7), 300);
    carried.sort_unstable();
    assert_eq!(carried, [0, 300, 400, 430]);

    // An index that names no clip skips nothing and still answers.
    assert_eq!(snap_marks(&[], Some((3, 9)), None, 0), [0, 0]);
}

/// The line the bed draws, and the switch that turns the whole thing off.
#[test]
fn the_snap_names_the_mark_it_landed_on_unless_it_is_switched_off() {
    let marks = [100, 160, 300, 0];
    // Pulled by the tail: the clip lands at 60 and the line stands on the
    // edge its *tail* met, 100 -- not on the head it happens to have.
    assert_eq!(snap_cue(true, 62, 40, 4, &marks), (60, Some(100)));
    // Pulled by the head: line and landing are the same frame.
    assert_eq!(snap_cue(true, 158, 40, 4, &marks), (160, Some(160)));
    // A trim carries no length, so only its own edge lands.
    assert_eq!(snap_cue(true, 298, 0, 4, &marks), (300, Some(300)));
    // Open bed: nothing moves and nothing is drawn.
    assert_eq!(snap_cue(true, 200, 40, 4, &marks), (200, None));
    // Switched off, a gesture that would have snapped lands raw and draws
    // no line -- the frame-by-frame placement the toggle is for.
    assert_eq!(snap_cue(false, 158, 40, 4, &marks), (158, None));
}

/// The card's rate row: every component the container states and no other,
/// so a track the header is silent about is absent from the line rather
/// than sitting in it as a zero.
#[test]
fn a_rate_row_says_only_what_the_container_stated() {
    use crate::MediaBitrate;
    let all = MediaBitrate {
        total: Some(8_432_000),
        video: Some(7_918_000),
        audio: Some(128_000),
    };
    assert_eq!(
        bitrate_detail(Some(all), 1),
        "8.4 Mb/s · 7.9 video · 0.13 sound"
    );
    // A Matroska states no audio rate of its own: the sound is dropped from
    // the line, never drawn as "0".
    assert_eq!(
        bitrate_detail(Some(MediaBitrate { audio: None, ..all }), 1),
        "8.4 Mb/s · 7.9 video"
    );
    // A still, or anything that would not open: the probe answered, and the
    // answer is that nobody said.
    assert_eq!(
        bitrate_detail(Some(MediaBitrate::default()), 0),
        "not stated"
    );
    // Asked, not answered yet -- a 12 GB film's walk takes seconds.
    assert_eq!(bitrate_detail(None, 2), "…");
    // A dual-audio file: the number is the track that plays, and the line
    // says so rather than letting it stand for the AC-3 beside it.
    assert_eq!(
        bitrate_detail(Some(all), 2),
        "8.4 Mb/s · 7.9 video · 0.13 1 of 2"
    );
    // Every component of a tiny file is stated, and small: the line changes
    // unit, so not one of them reads as the zero it is not. In megabits
    // this file was "0.00 Mb/s · 0.00 video · 0.00 sound".
    assert_eq!(
        bitrate_detail(
            Some(MediaBitrate {
                total: Some(4_998),
                video: Some(113),
                audio: Some(2_400),
            }),
            1
        ),
        "5.0 kb/s · 0.11 video · 2.4 sound"
    );
}

/// The invariant under the row above, over every rate a container can
/// state: a component the file *does* state never renders as a zero. The
/// probe leaves out what is unstated, so a "0.00" on this card would be it
/// saying a track that plays is silent.
#[test]
fn a_stated_rate_never_renders_as_a_zero() {
    use crate::MediaBitrate;
    // Every decade from 1 bit a second to a 100 Mb/s master, the rounding
    // edges of each unit switch, and every pair of them in one line: the
    // line's unit is picked off its smallest component, so the widest
    // spread is the one that could round the biggest away.
    let edges: Vec<u64> = (0..12)
        .flat_map(|e| {
            let decade = 10_u64.pow(e);
            [decade, decade * 4, decade * 5, decade * 9]
        })
        .chain([MB_FLOOR - 1, MB_FLOOR, 999_999, 4_998, 113, 2_400])
        .collect();
    for &small in &edges {
        for &big in &edges {
            let line = bitrate_detail(
                Some(MediaBitrate {
                    total: Some(small.max(big)),
                    video: Some(big),
                    audio: Some(small),
                }),
                2,
            );
            for number in line
                .split(" · ")
                .filter_map(|part| part.split(' ').next())
                .filter_map(|n| n.parse::<f64>().ok())
            {
                assert!(
                    number > 0.,
                    "{small}/{big} bits a second rendered as {line:?}"
                );
            }
        }
    }
}

#[test]
fn timecode_counts_frames_inside_the_second() {
    assert_eq!(timecode(0., 30.), "00:00:00:00");
    assert_eq!(timecode(-1., 30.), "00:00:00:00"); // clamped, never negative
    assert_eq!(timecode(4.9667, 30.), "00:00:04:29"); // last frame of a 5 s clip
    assert_eq!(timecode(5., 30.), "00:00:05:00");
    assert_eq!(timecode(3661.5, 30.), "01:01:01:15");
    // Rounding must not spill into a frame the second does not have.
    assert_eq!(timecode(1. - f64::EPSILON, 30.), "00:00:00:29");
    assert_eq!(timecode(0.999, 29.97), "00:00:00:29");
}

#[test]
fn scrub_seeks_only_on_a_new_frame_and_only_every_100ms() {
    let (slow, fast) = (Duration::from_millis(100), Duration::from_millis(99));
    // Both halves must hold: a moved pointer that has not crossed a frame
    // boundary would reopen the decoder for the same picture.
    assert!(scrub_due(31, 30, slow));
    assert!(!scrub_due(30, 30, slow));
    assert!(!scrub_due(31, 30, fast));
    assert!(!scrub_due(30, 30, fast));
    // Exactly at the gap counts, and a long stall never blocks.
    assert!(scrub_due(1, 0, Duration::from_secs(9)));
}

#[test]
fn the_picture_is_restarted_only_when_it_is_really_behind_and_not_twice() {
    // A quantum's worth of lateness is what every playing frame has; only a
    // gap an eye reads as out of sync answers with a reopen.
    assert!(!should_resync(0., None));
    assert!(!should_resync(LATE_RESYNC, None));
    assert!(should_resync(LATE_RESYNC + 0.1, None));
    // ...and a decoder that is still behind right after a restart -- the
    // usual case, since it was too slow to begin with -- waits out the gap
    // instead of reopening at every repaint.
    assert!(!should_resync(9., Some(Instant::now())));
    assert!(should_resync(
        9.,
        Some(Instant::now() - RESYNC_GAP - Duration::from_millis(1))
    ));
}

#[test]
fn export_lands_beside_the_source_and_never_on_it() {
    // The whole point: the export of an .mp4 is never that .mp4.
    assert_eq!(
        export_path("assets/test_baseline.mp4"),
        std::path::Path::new("assets/test_baseline.export.mp4")
    );
    // A second export of an export is still not its own source.
    assert_eq!(
        export_path("a.export.mp4"),
        std::path::Path::new("a.export.export.mp4")
    );
    // Extensionless and dotted-directory names keep the directory intact.
    assert_eq!(export_path("clip"), std::path::Path::new("clip.export.mp4"));
    assert_eq!(
        export_path("/v.1/clip.MP4"),
        std::path::Path::new("/v.1/clip.export.mp4")
    );
}

#[test]
fn the_first_save_lands_beside_the_media() {
    assert_eq!(
        project_path("assets/test_av.mp4"),
        std::path::Path::new("assets/test_av.edith")
    );
    // The same rule an export follows: only the last extension moves.
    assert_eq!(
        project_path("a.export.mp4"),
        std::path::Path::new("a.export.edith")
    );
    assert_eq!(project_path("clip"), std::path::Path::new("clip.edith"));
    // Saving a loaded project writes the file it came from, not a second.
    assert_eq!(project_path("a.edith"), std::path::Path::new("a.edith"));
}

#[test]
fn only_an_exact_edith_extension_is_a_project() {
    let p = std::path::Path::new;
    assert!(is_project(p("a.edith")));
    // A dotted directory must not decide it -- the file name does.
    assert!(is_project(p("/v.mp4/a.edith")));
    assert!(!is_project(p("a.mp4")));
    assert!(!is_project(p("/v.edith/a.mp4")));
    // Exactly what `save_project` writes: a dropped `.EDITH` goes to the
    // demuxer and is refused there, not parsed as a project.
    assert!(!is_project(p("a.EDITH")));
    // An extension, never a bare name.
    assert!(!is_project(p("edith")));
    assert!(!is_project(p("a.edith.mp4")));
}

/// The keys menu is the registry drawn, so a *bindable* stroke cannot go
/// missing from it by construction. The strokes the modal cards read for
/// themselves are the ones that could: this reads the key handler's own
/// source and fails on any key it answers to that the menu never mentions.
#[test]
fn no_stroke_is_missing_from_the_keys_menu() {
    use keymap::{ActionId, Keymap};
    // Every file the window is written in -- the handler, and the helpers
    // it asks, wherever they have been moved to. The tests below compare
    // keys too, and are not shortcuts.
    let handler = ui_source();
    let handler = handler.as_str();
    let keymap = Keymap::defaults();
    let listed = |key: &str| {
        let pretty = keymap::Chord {
            key: key.to_string(),
            ctrl: false,
        }
        .pretty();
        keymap.lookup(key, false).is_some() || keymap::FIXED.iter().any(|f| f.chord == pretty)
    };
    let mut compared = 0;
    for (at, needle) in handler.match_indices("key == ") {
        let rest = &handler[at + needle.len()..];
        let key = match rest.strip_prefix('"') {
            Some(literal) => literal[..literal.find('"').expect("unterminated key")].to_string(),
            // A named constant: the escape the cards get out by.
            None if rest.starts_with("ESCAPE") => crate::ESCAPE.to_string(),
            None => panic!("a key compared against something this cannot read: {rest:.20}"),
        };
        assert!(
            listed(&key),
            "the handler answers to {key:?} and the keys menu never says so"
        );
        compared += 1;
    }
    // The scan is only a guard while it still finds the comparisons: a
    // rewrite that spells them differently must come back here.
    assert!(
        compared >= 4,
        "the key comparisons moved; this scan is blind"
    );
    // The one branch that is not a comparison -- any digit is a bitrate.
    assert!(handler.contains("key.parse::<u32>()"));
    assert!(keymap::FIXED.iter().any(|f| f.chord == "0–9"));
    // And every entry of both halves lands under a heading the menu draws.
    for action in ActionId::ALL {
        assert!(keymap::Category::ALL.contains(&action.category()));
    }
    for fixed in keymap::FIXED.iter() {
        assert!(keymap::Category::ALL.contains(&fixed.category));
    }
}

/// The mapping the whole panel is drawn and clicked through: a moment goes
/// to a pixel on the bed and comes back the same moment, at every zoom.
#[test]
fn a_moment_and_the_place_it_is_drawn_are_the_same_at_every_zoom() {
    let duration = 20.;
    // The fit, which is the only scale the content's own length picks: 20 s
    // across a 200 px bed is 10 px to the second, and a 5 s clip is a
    // quarter of the bed -- the answer the fractional view gave for it.
    let fit = test_view(Scale::default(), duration).fit();
    assert_eq!(fit.pps, 10.);
    assert_eq!(fit.width_px(5.), TEST_BED * 0.25);
    for t in [0., 5., 12.5, 20.] {
        assert_eq!(fit.px_at(t), (t / duration) as f32 * TEST_BED, "fit at {t}");
    }
    for scale in [
        fit,
        Scale {
            pps: 20.,
            start: 4.,
        },
        Scale {
            pps: 80.,
            start: 12.5,
        },
        Scale {
            pps: 375.,
            start: 19.,
        },
    ] {
        let scale = test_view(scale, duration).settled();
        for x in [0., 50., 100., TEST_BED] {
            let at = scale.time_at(x);
            assert!(
                (f64::from(scale.px_at(at)) - f64::from(x)).abs() < 1e-3,
                "{scale:?} round trip at {x}"
            );
        }
        // A stretch as long as what is on the bed is as wide as the bed.
        assert!(
            (f64::from(scale.width_px(test_view(scale, duration).span())) - f64::from(TEST_BED))
                .abs()
                < 1e-3,
            "{scale:?} spans the bed"
        );
    }
}

/// The rule that makes a zoom usable: whatever was under the anchor is
/// still under it afterwards -- the playhead for a key, the pointer for a
/// ctrl+wheel.
#[test]
fn a_zoom_leaves_the_anchor_where_it_was_on_screen() {
    let duration = 20.;
    let mut scale = test_view(Scale::default(), duration).settled();
    // The same three points along the bed the fractional view held: a half,
    // a quarter and nine tenths of 200 px.
    for anchor in [100f32, 50., 180.] {
        // Well past the zoom-in stop: the anchor holds at the clamp too,
        // which is where it used to slide (the clamp came after the offset).
        for _ in 0..30 {
            let at = scale.time_at(anchor);
            let view = test_view(scale, duration);
            let zoomed = view.zoomed(ZOOM_STEP, anchor);
            // Only where the anchor is not pinned by an edge of the
            // timeline: a view already against an end cannot slide further.
            let span = test_view(zoomed, duration).span();
            let pinned = zoomed.start <= 0. || zoomed.start + span >= duration;
            if !pinned {
                assert!(
                    (f64::from(zoomed.px_at(at)) - f64::from(anchor)).abs() < 1e-3,
                    "{at} moved: {scale:?} -> {zoomed:?}"
                );
            }
            assert!(zoomed.pps >= scale.pps, "and it did zoom in");
            scale = zoomed;
        }
    }
}

/// Both stops. In is [`ZOOM_MIN_FRAMES`] across the bed; out, on a timeline
/// far too short to be worth widening to, is a pixel to the second -- so a
/// short import can be zoomed out of, a long one zoomed into, and neither
/// can scroll off an end.
#[test]
fn zoom_stops_at_a_bedful_of_time_and_at_a_handful_of_frames() {
    let (duration, fps) = (20., 30.);
    let mut scale = Scale::default();
    for _ in 0..200 {
        scale = test_view(scale, duration).zoomed(1. / ZOOM_STEP, 100.);
    }
    assert!(
        (test_view(scale, duration).span() - f64::from(TEST_BED) / PPS_MIN).abs() < 1e-6,
        "widest is a pixel to the second, not the 20 s that happen to be on it"
    );
    for _ in 0..200 {
        scale = test_view(scale, duration).zoomed(ZOOM_STEP, 100.);
    }
    assert_eq!(
        (test_view(scale, duration).span() * fps).round(),
        ZOOM_MIN_FRAMES,
        "tightest is a handful of frames"
    );
    // Against the far end, the slice still ends at the last frame.
    let end = test_view(
        Scale {
            start: 1e6,
            ..scale
        },
        duration,
    );
    let end = (end.settled(), end.span());
    assert!((end.0.start + end.1 - duration).abs() < 1e-9);
    assert_eq!(
        test_view(
            Scale {
                start: -5.,
                ..scale
            },
            duration
        )
        .settled()
        .start,
        0.
    );
    // The dead zone the fractional view had: a timeline of a handful of
    // frames could not be zoomed at all, because its own length was the
    // floor *and* the ceiling. Both keys work on it now.
    let tiny = test_view(Scale::default(), 0.1);
    assert!(tiny.zoomed(ZOOM_STEP, 100.).pps > PPS_DEFAULT);
    assert!(tiny.zoomed(1. / ZOOM_STEP, 100.).pps < PPS_DEFAULT);
    // A timeline with no length at all divides by nothing and scrolls
    // nowhere; the fit of one keeps the scale it had.
    let empty = test_view(Scale::default(), 0.);
    assert_eq!(empty.settled(), Scale::default());
    assert_eq!(empty.fit(), Scale::default());
    assert_eq!(Scale::default().px_at(0.), 0.);
    // A bed that was never painted clamps nothing -- there is nothing to
    // clamp against, and a zoom must survive the frame before the probe.
    let unpainted = View {
        bed: 0.,
        ..test_view(
            Scale {
                pps: 4e6,
                start: 3.,
            },
            duration,
        )
    };
    assert_eq!(unpainted.settled().pps, 4e6);
    assert_eq!(unpainted.following(19.), unpainted.scale);
}

/// The bug a fixed far stop was: two two-and-a-half hour clips are five
/// hours of timeline, longer than any stop measured in hours, so the end of
/// the second one could not be brought on screen by any zoom. The stop is
/// the timeline's own length now, so it can.
#[test]
fn zooming_out_reaches_the_end_of_a_timeline_however_long_it_is() {
    let bed = 900.;
    let view = |scale: Scale, duration: f64| View {
        scale,
        bed,
        duration,
        fps: 30.,
    };
    // As far out as the keys go, from the scale a fresh project is drawn at.
    let out = |duration: f64| {
        let mut scale = Scale::default();
        for _ in 0..400 {
            scale = view(scale, duration).zoomed(1. / ZOOM_STEP, 0.);
        }
        scale
    };
    let five_hours = 2. * 2.5 * 3600.;
    let wide = out(five_hours);
    // The whole five hours is on the bed, the last frame drawn inside the
    // window rather than against its edge.
    assert_eq!(wide.start, 0.);
    let end = wide.px_at(five_hours);
    assert!(
        end < bed,
        "the end of the timeline is off the bed at {end} px"
    );
    assert!(end > bed * 0.9, "and not shrunk into a corner: {end} px");
    assert!(
        (view(wide, five_hours).span() - five_hours * ZOOM_OUT_MARGIN).abs() < 1e-6,
        "the far stop is the timeline plus its margin"
    );
    // Zooming back in still reaches the frame stop on a timeline that long:
    // the far stop moving does not drag the near one with it.
    let mut scale = wide;
    for _ in 0..400 {
        scale = view(scale, five_hours).zoomed(ZOOM_STEP, 0.);
    }
    assert_eq!(
        (view(scale, five_hours).span() * 30.).round(),
        ZOOM_MIN_FRAMES
    );
    // And a ten second project is not zoomed out to four hours of empty
    // bed: short of a pixel to the second its own length is not worth
    // widening to, and that is 900 s of bed, not 14400.
    assert!((view(out(10.), 10.).span() - f64::from(bed) / PPS_MIN).abs() < 1e-6);
    // Whatever the length, the resting scale is nobody's content: the width
    // invariant, which a far stop measured off the content would break.
    assert_eq!(view(Scale::default(), 10.).settled(), Scale::default());
    assert_eq!(
        view(Scale::default(), five_hours).settled(),
        Scale::default()
    );
    // Shrinking the timeline pulls a fully zoomed out view in with it --
    // what was showing all of the timeline still shows all of it.
    let shrunk = view(wide, 1800.).settled();
    assert!((view(shrunk, 1800.).span() - 1800. * ZOOM_OUT_MARGIN).abs() < 1e-6);
    assert!(shrunk.px_at(1800.) < bed);
    // Growing it does not: a scale the user zoomed to is still legal, and
    // the stop it stopped at is one press further out.
    assert_eq!(view(wide, 2. * five_hours).settled().pps, wide.pps);
    assert!(view(wide, 2. * five_hours).zoomed(1. / ZOOM_STEP, 0.).pps < wide.pps);
}

/// The bare wheel's own move: the view slides along the timeline, what is
/// drawn where slides with it, the scale never changes, and both ends stop
/// against the content rather than scrolling it off the bed.
#[test]
fn a_scroll_slides_the_view_without_zooming_it() {
    let duration = 60.;
    // Zoomed in far enough that there is somewhere to scroll to: 10 s of a
    // 60 s timeline on the bed.
    let start = test_view(
        Scale {
            pps: f64::from(TEST_BED) / 10.,
            start: 0.,
        },
        duration,
    )
    .settled();
    let notch = TEST_BED * SCROLL_NOTCH_SHARE;
    let on = test_view(start, duration).scrolled(notch);
    // A tenth of the bed later, and drawn a tenth of the bed to the left --
    // the two halves of one move.
    assert_eq!(on.pps, start.pps, "a scroll is not a zoom");
    assert!(
        (on.start - (start.start + 1.)).abs() < 1e-9,
        "one notch is a tenth of the 10 s on the bed: {on:?}"
    );
    assert!(
        (f64::from(start.px_at(20.) - on.px_at(20.)) - f64::from(notch)).abs() < 1e-3,
        "and what was drawn moved with it: {on:?}"
    );
    // Back the other way, from the same place, is the same distance.
    let back = test_view(on, duration).scrolled(-notch);
    assert!((back.start - start.start).abs() < 1e-9, "{back:?}");
    // Neither end can be scrolled off: the head stops at zero, and the tail
    // stops with the last frame on the bed -- the clamp is the timeline's
    // own length, not a number.
    let mut scale = start;
    for _ in 0..200 {
        scale = test_view(scale, duration).scrolled(notch);
    }
    assert_eq!(scale.pps, start.pps);
    assert!(
        (scale.start - (duration - 10.)).abs() < 1e-6,
        "the tail stops with the end on the bed: {scale:?}"
    );
    for _ in 0..200 {
        scale = test_view(scale, duration).scrolled(-notch);
    }
    assert_eq!(scale.start, 0., "and the head stops at the head: {scale:?}");
    // A view already showing all of it has nowhere to go.
    let whole = test_view(Scale::default(), duration).fit();
    assert_eq!(test_view(whole, duration).scrolled(notch), whole);
}

/// What makes a zoomed timeline follow the playing head: off the bed at
/// either end pulls the view onto it, and on the bed it never moves -- a
/// view that jumped every frame would be unreadable.
#[test]
fn the_view_follows_a_playhead_that_runs_off_the_bed() {
    let duration = 20.;
    // 5 s on a 200 px bed: 40 px to the second, starting at second 5.
    let view = test_view(
        Scale {
            pps: 40.,
            start: 5.,
        },
        duration,
    );
    let scale = view.settled();
    assert_eq!(view.span(), 5.);
    // Inside: untouched, whichever part of the slice it is in.
    for at in [5., 7.5, 10.] {
        assert_eq!(view.following(at), scale, "{at} is on the bed");
    }
    // Past the right edge, as playback does it: the head comes back on the
    // bed, and the scale is not changed by the scroll.
    let moved = view.following(12.);
    assert_eq!(moved.pps, scale.pps);
    assert!(moved.start > scale.start, "scrolled forward");
    assert!(
        moved.px_at(12.) > 0. && moved.px_at(12.) < TEST_BED,
        "and the playhead is on screen"
    );
    // A seek back behind the slice does the same the other way.
    let back = view.following(1.);
    assert!(back.start < scale.start);
    assert!(back.px_at(1.) >= 0.);
    // With the whole timeline on the bed there is nothing to follow.
    let whole = test_view(Scale::default(), duration);
    let fit = whole.fit();
    for at in [0., 10., 20.] {
        assert_eq!(
            test_view(fit, duration).following(at),
            fit,
            "the fit never scrolls"
        );
    }
}

/// The wheel during playback. A scroll away from the playing head used to
/// be undone by the follow on the very next frame -- with a second, longer
/// media on the timeline making it scrollable at all, one notch in five
/// reached the screen and the wheel looked dead. The hand keeps the view
/// now, and the follow takes it back by itself when the head runs into what
/// the hand is looking at.
#[test]
fn a_scroll_during_playback_wins_until_the_head_catches_up() {
    let duration = 80.;
    let mut scale = test_view(
        Scale {
            pps: 40.,
            start: 0.,
        },
        duration,
    )
    .settled();
    // The render's arbitration, in the two lines it is there: a panned view
    // is left where the hand put it, and the pan ends where the head is
    // back on the bed.
    let frame = |scale: Scale, panned: &mut bool, at: f64| {
        let scale = match *panned {
            true => test_view(scale, duration).settled(),
            false => test_view(scale, duration).following(at),
        };
        if *panned && test_view(scale, duration).shows(at) {
            *panned = false;
        }
        scale
    };
    // Playing at second one, five notches of the wheel: every one of them
    // moves the view, and none is taken back by the head being off the bed.
    let notch = f64::from(TEST_BED * SCROLL_NOTCH_SHARE);
    let mut panned = false;
    for n in 1..=5 {
        scale = test_view(scale, duration).scrolled(TEST_BED * SCROLL_NOTCH_SHARE);
        panned = true;
        scale = frame(scale, &mut panned, 1.);
        // ...and the pan is only *held* while the head is off the bed: the
        // first notches still show it, so the follow has the view back for
        // free and would not have moved anything anyway.
        assert_eq!(panned, scale.start > 1., "held while the head is off");
        assert!(
            (scale.start - notch * f64::from(n) / 40.).abs() < 1e-6,
            "notch {n} reached the screen: {scale:?}"
        );
    }
    // The head runs into it: the pan is given back with nothing pressed...
    let span = test_view(scale, duration).span();
    let start = frame(scale, &mut panned, scale.start + span / 2.).start;
    assert!(!panned, "the follow has the view back");
    assert_eq!(start, scale.start, "and it did not jump to take it");
    // ...and the next head that runs off the bed pulls the view again.
    let ran_off = frame(scale, &mut panned, start + span + 1.);
    assert!(
        ran_off.start > start,
        "the follow follows again: {ran_off:?}"
    );
    // A paused hand is untouched by any of this: with no follow asking,
    // `shows` is the only thing the pan is ever released by.
    assert!(test_view(scale, duration).shows(scale.start));
    assert!(!test_view(scale, duration).shows(scale.start + span + 0.5));
}

/// The bug this mapping exists for: the first import used to fill the whole
/// track whatever it was, because the bed *was* the timeline -- so a 5 s
/// clip was 100% of the lane, zooming out did nothing, and adding a second
/// clip silently halved the first one's box.
#[test]
fn a_clip_is_drawn_the_same_width_whatever_else_is_on_the_timeline() {
    let bed = 900.;
    let of = |duration: f64| View {
        scale: Scale::default(),
        bed,
        duration,
        fps: 30.,
    };
    // One 5 s import, then a second clip after it: 20 s of timeline where
    // there were 5, and the first box has not moved or narrowed.
    let (alone, joined) = (of(5.).settled(), of(20.).settled());
    assert_eq!(alone, joined);
    assert_eq!(alone.width_px(5.), joined.width_px(5.));
    assert_eq!(joined.px_at(5.), alone.width_px(5.));
    // And it does not fill the track: a short clip reads as short.
    assert!(
        alone.width_px(5.) < bed / 2.,
        "5 s at {} px/s is {} px of a {bed} px bed",
        alone.pps,
        alone.width_px(5.)
    );
    // Zooming out visibly shrinks it, however short the timeline is -- the
    // press that used to be a no-op.
    let out = of(5.).zoomed(1. / ZOOM_STEP, 0.);
    assert!(
        out.width_px(5.) < alone.width_px(5.),
        "{} px is not smaller than {} px",
        out.width_px(5.),
        alone.width_px(5.)
    );
    // The way back to "the whole timeline across the bed" is still one key.
    assert_eq!(of(5.).fit().width_px(5.), bed);
}

/// What the zoom button says: how much timeline is on the bed, in a unit
/// that tells two zooms apart.
#[test]
fn the_zoom_button_says_how_much_is_on_the_bed() {
    assert_eq!(span_label(4.5), "4.5s");
    assert_eq!(span_label(22.5), "22s");
    assert_eq!(span_label(90.), "1.5m");
    assert_eq!(span_label(3600.), "1.0h");
    // The span a five hour timeline is zoomed all the way out to.
    assert_eq!(span_label(5. * 3600. * 1.05), "5.2h");
    // Before the first paint there is no bed and so no answer to give.
    assert_eq!(span_label(0.), "—");
    assert_eq!(span_label(f64::NAN), "—");
    // A span under a second is a span: the tightest zoom is
    // `ZOOM_MIN_FRAMES` across the bed, which on 240 fps slow-motion is
    // 0.03s, and "0.0s" would be the pill saying nothing is on the bed.
    for fps in [60., 120., 240., 1000.] {
        let label = span_label(ZOOM_MIN_FRAMES / fps);
        assert_ne!(label, "0.0s", "{fps} fps");
        assert_ne!(label, "0.00s", "{fps} fps");
    }
    assert_eq!(span_label(ZOOM_MIN_FRAMES / 240.), "0.03s");
    // A frame of quiet at 60 fps, which the silence card says out loud.
    assert_eq!(secs_label(1. / 60.), "0.02s");
    assert_eq!(secs_label(4.5), "4.5s");
}
