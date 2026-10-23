//! `PlaybackSession::compose_at`: the composed picture at an exact timeline
//! frame, headless. Silent (`VE_NO_AO`), no clock, no UI.
//!
//! ```text
//! cargo test -p engine --release --test compose_at -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::PlaybackSession;
use engine::decode::{Frame, to_png, to_png_bytes};
use engine::project::{Lane, LaneKind, Title};
use engine::text::{Place, TextStyle};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Opens a session silently. `VE_NO_AO` keeps the audio device out of the test.
fn open(path: impl AsRef<Path>) -> PlaybackSession {
    engine::ao::set_silent_output();
    PlaybackSession::open(path).expect("open")
}

/// `test_av.mp4` is 1280x720, 30 fps, 150 frames.
const W: u32 = 640;
const H: u32 = 360;

#[test]
fn size_follows_the_project_aspect_inside_the_box() {
    let session = open(asset("test_av.mp4"));
    let f = session.compose_at(0, W, H).expect("frame 0");
    assert_eq!((f.width, f.height), (640, 360));
    assert_eq!(f.index, 0);
    assert_eq!(f.bgra.len(), 640 * 360 * 4);

    // A 16:9 project asked for a square box comes back 16:9, fitted.
    let g = session.compose_at(0, 500, 500).expect("square box");
    assert_eq!((g.width, g.height), (500, 281));
}

#[test]
fn frames_differ_and_repeat_byte_identically() {
    let session = open(asset("test_av.mp4"));
    let a = session.compose_at(0, W, H).expect("frame 0");
    let b = session.compose_at(60, W, H).expect("frame 60");
    assert_ne!(a.bgra, b.bgra, "frame 0 and frame 60 should differ");

    let b2 = session.compose_at(60, W, H).expect("frame 60 again");
    assert!(b.bgra == b2.bgra, "two calls at the same frame must match");
    assert_eq!(b2.index, 60);
}

#[test]
fn a_gap_is_black() {
    let mut session = open(asset("test_av.mp4"));
    // Moving the only video clip to frame 100 leaves frames 0..100 a gap.
    assert!(session.move_clip_to(Lane::V1, 0, Lane::V1, 100));

    let gap = session.compose_at(60, W, H).expect("gap frame");
    assert_eq!(gap.bgra.len(), 640 * 360 * 4);
    let worst = gap
        .bgra
        .as_chunks::<4>().0.iter()
        .map(|px| px[0].max(px[1]).max(px[2]))
        .max()
        .unwrap_or(0);
    assert!(worst <= 2, "gap frame is not black (max channel {worst})");

    // The clip itself still composes after the gap.
    let clip = session.compose_at(120, W, H).expect("clip frame");
    let clip_max = clip
        .bgra
        .as_chunks::<4>().0.iter()
        .map(|px| px[0].max(px[1]).max(px[2]))
        .max()
        .unwrap_or(0);
    assert!(clip_max > 16, "frame after the gap is black");
}

#[test]
fn past_the_end_is_an_error() {
    let mut session = open(asset("test_av.mp4"));
    assert!(session.compose_at(150, W, H).is_err(), "frame 150 of 150");
    assert!(session.move_clip_to(Lane::V1, 0, Lane::V1, 100));
    // 100 + 150 frames of timeline now; 250 is the first past the end.
    assert!(session.compose_at(249, W, H).is_ok());
    assert!(session.compose_at(250, W, H).is_err());
}

#[test]
fn png_roundtrip_matches_the_pixels() {
    let session = open(asset("test_av.mp4"));
    let f = session.compose_at(60, W, H).expect("frame 60");
    let bytes = to_png_bytes(&f).expect("encode");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

    let decoded = ec_image::decode(&bytes).expect("decode png");
    assert_eq!((decoded.width, decoded.height), (f.width, f.height));
    let expected: Vec<u8> = f
        .bgra
        .as_chunks::<4>().0.iter()
        .flat_map(|px| [px[2], px[1], px[0], px[3]])
        .collect();
    assert!(decoded.to_rgba8() == expected, "png pixels differ");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("compose_at_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("frame60.png");
    to_png(&f, &path).expect("write");
    assert_eq!(std::fs::read(&path).expect("read back"), bytes);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timing() {
    let session = open(asset("test_av.mp4"));
    let _ = session.compose_at(60, W, H).expect("warm");
    let n = 10u32;
    let t = Instant::now();
    for i in 0..n {
        let _: Frame = session.compose_at(i * 10, W, H).expect("frame");
    }
    let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    eprintln!("compose_at 640x360: {per:.1} ms per frame (mean of {n}, varied frames)");
}

/// A title card or caption at `start` for `frames`, set with the default look
/// and no box, so the only pixels it changes are its own letters.
fn title(start: u32, frames: u32, place: Place) -> Title {
    Title {
        start,
        frames,
        text: "Episode 17".into(),
        style: TextStyle {
            place,
            boxed: false,
            ..TextStyle::default()
        },
        fade_in: 0,
        fade_out: 0,
        in_anim: Default::default(),
        out_anim: Default::default(),
        anim_frames: 0,
    }
}

/// The rows in which any byte of `a` and `b` differs: the rows a title drew in.
fn changed_rows(a: &[u8], b: &[u8], w: u32, h: u32) -> Vec<u32> {
    let row = w as usize * 4;
    (0..h)
        .filter(|&y| {
            let s = y as usize * row;
            a[s..s + row] != b[s..s + row]
        })
        .collect()
}

/// The brightest channel summed over the whole frame: how much of it is lit.
fn light(f: &Frame) -> u64 {
    f.bgra
        .as_chunks::<4>().0.iter()
        .map(|px| u64::from(px[0].max(px[1]).max(px[2])))
        .sum()
}

#[test]
fn a_title_draws_in_its_centre_band_and_its_frames_only() {
    let mut session = open(asset("test_av.mp4"));
    let before_in = session.compose_at(60, W, H).expect("frame 60");
    let before_out = session.compose_at(70, W, H).expect("frame 70");
    session
        .add_title(title(50, 20, Place::Centre))
        .expect("a title is added");
    let after_in = session.compose_at(60, W, H).expect("frame 60 with the title");
    let after_out = session.compose_at(70, W, H).expect("frame 70, after the title");

    let (w, h) = (after_in.width, after_in.height);
    let rows = changed_rows(&before_in.bgra, &after_in.bgra, w, h);
    assert!(!rows.is_empty(), "no title pixel inside its frames");
    assert!(
        rows.iter().all(|&y| y >= h * 3 / 10 && y < h * 7 / 10),
        "the centred title drew outside the middle band: {rows:?}"
    );
    assert!(
        changed_rows(&before_out.bgra, &after_out.bgra, w, h).is_empty(),
        "the frame after the title's last changed"
    );
}

#[test]
fn a_title_past_the_last_clip_is_text_on_black() {
    let mut session = open(asset("test_av.mp4"));
    // test_av.mp4 runs 150 frames; the title runs the timeline on to 180.
    session
        .add_title(title(150, 30, Place::Centre))
        .expect("a title past the clip is added");
    let f = session
        .compose_at(160, W, H)
        .expect("a frame of the title over black composes");
    let total = (f.width * f.height) as usize;
    let lit = f
        .bgra
        .as_chunks::<4>().0.iter()
        .filter(|px| px[0].max(px[1]).max(px[2]) > 2)
        .count();
    let white = f
        .bgra
        .as_chunks::<4>().0.iter()
        .filter(|px| px[0].max(px[1]).max(px[2]) > 200)
        .count();
    assert!(white > 100, "no white text over the black ({white} pixels)");
    assert!(lit * 10 < total, "the frame is not black around the text ({lit} of {total})");
    assert!(
        session.compose_at(180, W, H).is_err(),
        "frame 180 is past the title and the timeline"
    );
}

#[test]
fn a_fade_in_starts_fainter_than_its_middle() {
    let mut session = open(asset("test_av.mp4"));
    session
        .add_title(Title {
            fade_in: 10,
            ..title(150, 30, Place::Centre)
        })
        .expect("a title with a fade-in is added");
    let first = light(&session.compose_at(150, W, H).expect("the first frame"));
    let mid = light(&session.compose_at(155, W, H).expect("a frame mid-fade"));
    let full = light(&session.compose_at(160, W, H).expect("a frame past the fade"));
    assert!(first > 0, "the first frame shows nothing at all");
    assert!(first < mid, "the first frame ({first}) is not fainter than mid-fade ({mid})");
    assert!(mid < full, "mid-fade ({mid}) is not fainter than full ({full})");
}

/// A title's colour and side reach the composed frame: yellow letters, against the left margin.
#[test]
fn a_title_look_reaches_the_composed_frame() {
    use engine::text::{HPlace, Weight};
    let mut session = open(asset("test_av.mp4"));
    session
        .add_title(Title {
            style: TextStyle {
                ink: [255, 204, 0],
                hplace: HPlace::Left,
                weight: Weight::Bold,
                ..title(150, 30, Place::Centre).style
            },
            ..title(150, 30, Place::Centre)
        })
        .expect("a styled title is added");
    let f = session.compose_at(155, W, H).expect("a frame over the title");
    let px = f.bgra.as_chunks::<4>().0;
    let lit: Vec<(usize, &[u8; 4])> = px.iter().enumerate().filter(|(_, p)| p[0] > 0 || p[1] > 0 || p[2] > 0).collect();
    assert!(lit.len() > 100, "no text drawn");
    let brightest = lit.iter().max_by_key(|(_, p)| u32::from(p[2])).unwrap().1;
    assert!(brightest[2] > 240 && (190..=215).contains(&brightest[1]) && brightest[0] < 20, "yellow: {brightest:?}");
    assert!(lit.iter().all(|(i, _)| (*i % W as usize) < W as usize / 2), "the block sits in the left half");
}

/// The mean green channel over the whole frame: a black frame is near zero,
/// and a picture scaled by `k` toward black has a mean of about `k` times its
/// own.
fn mean_green(f: &Frame) -> f64 {
    let n = (f.bgra.len() / 4) as f64;
    f.bgra.as_chunks::<4>().0.iter().map(|px| f64::from(px[1])).sum::<f64>() / n
}

#[test]
fn a_picture_fade_comes_from_black_and_goes_to_it() {
    let mut session = open(asset("test_av.mp4"));
    // test_av.mp4 is one clip of 150 frames at 30 fps, so the clip's own
    // frames are the timeline's. Measure it plain first.
    let plain = |s: &PlaybackSession, at: u32| s.compose_at(at, W, H).expect("frame");
    let base: Vec<Frame> = [0, 5, 10, 140, 145, 149]
        .into_iter()
        .map(|at| plain(&session, at))
        .collect();
    assert!(session.set_fade_in(Lane::V1, 0, 10));
    assert!(session.set_fade_out(Lane::V1, 0, 10));

    // The fade-in: black on its first frame, half of the picture half way up
    // the ramp, and the picture itself from the frame the ramp ends on.
    let (first, half, done) = (plain(&session, 0), plain(&session, 5), plain(&session, 10));
    assert!(mean_green(&first) <= 2.0, "the first frame is not black: {}", mean_green(&first));
    let (b5, b10) = (mean_green(&base[1]), mean_green(&base[2]));
    let m5 = mean_green(&half);
    assert!(
        (m5 - 0.5 * b5).abs() <= 2.0 + 0.03 * b5,
        "mid-fade ({m5:.1}) is not about half the picture ({b5:.1})"
    );
    assert!(done.bgra == base[2].bgra, "the frame the fade-in ends on is not the picture");
    assert!(mean_green(&done) > b10 * 0.99, "the picture is still fading");

    // The fade-out: the picture until its last ten frames, half way down the
    // ramp at frame 145, and black on the last frame of the clip.
    let (out_mid, last) = (plain(&session, 145), plain(&session, 149));
    let (b145, b149) = (mean_green(&base[4]), mean_green(&base[5]));
    let m145 = mean_green(&out_mid);
    assert!(
        (m145 - 0.5 * b145).abs() <= 2.0 + 0.03 * b145,
        "mid-fade-out ({m145:.1}) is not about half the picture ({b145:.1})"
    );
    assert!(mean_green(&last) < 0.2 * b149, "the last frame is not near black");
    assert!(
        plain(&session, 140).bgra == base[3].bgra,
        "a frame before the fade-out's ramp is not the picture"
    );
}

/// The BGR of pixel `i` of a BGRA frame.
fn px(f: &Frame, i: usize) -> &[u8] {
    &f.bgra[i * 4..i * 4 + 3]
}

/// A picture-in-picture: V2 is a smaller, moved picture over V1. Outside the
/// picture V1 must show through (it is the same pixels V1 makes alone); inside
/// it the V2 picture must be what V2 makes alone. Both are exact -- the layers
/// are composed with the same placement a single clip gets -- so the check is
/// per pixel, not a mean.
#[test]
fn a_transformed_clip_on_top_lets_the_clip_beneath_show_outside_it() {
    let mut session = open(asset("test_av.mp4"));
    let v2 = session.add_lane(LaneKind::Video);
    // The same file on V2 from frame 0: the source is already on the timeline,
    // and the PiP is a scaled picture, so it differs from V1 wherever it lands.
    session
        .place_stream_at(0.0, &asset("test_av.mp4"), 0, Some(v2))
        .expect("place the picture on V2");
    let pip = engine::transform::TransformParams {
        pos_x: 0.3,
        pos_y: 0.3,
        scale: 0.3,
        ..Default::default()
    };
    assert!(session.set_transform(v2, 0, Some(pip)));

    let at = 20;
    let layered = session.compose_at(at, W, H).expect("layered frame");
    assert!(session.set_lane_hidden(v2, true));
    let v1_alone = session.compose_at(at, W, H).expect("V1 alone");
    assert!(session.set_lane_hidden(v2, false));
    assert!(session.set_lane_hidden(Lane::V1, true));
    let v2_alone = session.compose_at(at, W, H).expect("V2 alone");
    assert!(session.set_lane_hidden(Lane::V1, false));

    let (mut inside, mut outside) = (0usize, 0usize);
    for i in 0..(W * H) as usize {
        // V2 alone is black outside its picture; a picture pixel is at least
        // this bright in one channel (the test clip is not black there).
        let is_pip = px(&v2_alone, i).iter().any(|&c| c > 6);
        let want = if is_pip { &v2_alone } else { &v1_alone };
        if is_pip {
            inside += 1;
        } else {
            outside += 1;
        }
        assert_eq!(
            px(&layered, i),
            px(want, i),
            "pixel {i} ({}, {}) is not the {} picture",
            i % W as usize,
            i / W as usize,
            if is_pip { "V2" } else { "V1" }
        );
    }
    // The picture is a real share of the frame, and V1 shows around it.
    assert!(inside > (W * H) as usize / 20, "the PiP covers only {inside} pixels");
    assert!(outside > (W * H) as usize / 2, "V1 shows on only {outside} pixels");
}

/// The same picture-in-picture as the frame test above, played: the preview's
/// first picture after a seek is the picture `compose_at` composes there, so the
/// layered preview and the export agree -- not only the frame an agent asks for.
/// The preview's picture at timeline `at`, played: seek there and take the frame
/// the preview delivers for it, at the viewer's size.
fn played_at(session: &mut PlaybackSession, at: u32) -> Frame {
    session.seek(f64::from(at) / 30.0);
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(frame) = session.try_frame()
            && frame.index == at
        {
            return frame;
        }
        assert!(Instant::now() < deadline, "no picture at frame {at} after the seek");
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

/// The picture-in-picture of the frame test above, played. The preview composes
/// at the viewer's size on its own path, so it is compared with the same
/// preview's V1 alone and V2 alone -- like with like, at one size -- not with
/// `compose_at`, which resizes a project-size frame.
#[test]
fn a_layered_picture_plays_as_it_is_layered() {
    let mut session = open(asset("test_av.mp4"));
    let v2 = session.add_lane(LaneKind::Video);
    session
        .place_stream_at(0.0, &asset("test_av.mp4"), 0, Some(v2))
        .expect("place the picture on V2");
    let pip = engine::transform::TransformParams {
        pos_x: 0.3,
        pos_y: 0.3,
        scale: 0.3,
        ..Default::default()
    };
    assert!(session.set_transform(v2, 0, Some(pip)));
    assert!(session.set_view_size_exact(W, H));

    let at = 30;
    let layered = played_at(&mut session, at);
    assert!(session.set_lane_hidden(v2, true));
    let v1_alone = played_at(&mut session, at);
    assert!(session.set_lane_hidden(v2, false));
    assert!(session.set_lane_hidden(Lane::V1, true));
    let v2_alone = played_at(&mut session, at);
    assert!(session.set_lane_hidden(Lane::V1, false));
    assert_eq!((layered.width, layered.height), (W, H));

    let n = (W * H) as usize;
    let (mut inside, mut outside_worst, mut inside_worst) = (0usize, 0u8, 0u8);
    for i in 0..n {
        let a = &layered.bgra[i * 4..i * 4 + 3];
        let is_pip = v2_alone.bgra[i * 4..i * 4 + 3].iter().any(|&c| c > 6);
        let (want, worst) = match is_pip {
            true => {
                inside += 1;
                (&v2_alone.bgra, &mut inside_worst)
            }
            false => (&v1_alone.bgra, &mut outside_worst),
        };
        let d = (0..3).map(|c| a[c].abs_diff(want[i * 4 + c])).max().unwrap_or(0);
        *worst = (*worst).max(d);
    }
    eprintln!("played: outside the PiP worst {outside_worst}, inside worst {inside_worst}");
    assert!(inside > n / 20 && n - inside > n / 2, "the PiP covers {inside} of {n} pixels");
    assert!(outside_worst <= 2, "outside the PiP the preview is not V1: worst {outside_worst}");
    assert!(inside_worst <= 2, "inside the PiP the preview is not V2: worst {inside_worst}");
}

/// A dissolve on V1 with a V2 overlay across it. V1 is cut in two at frame 60
/// (A is 0..60, B is 60..150, both the test file) and A dissolves into B over
/// the last 10 frames. Mid-transition, outside the overlay, the picture must be
/// the blend of A and B -- the same picture a dissolve with nothing over it
/// composes -- and not A alone; inside the overlay it must be V2 alone.
#[test]
fn a_dissolve_under_a_layer_blends_the_clips_beneath_it() {
    let mut session = open(asset("test_av.mp4"));
    assert!(session.cut_at(2.0), "cut V1 at frame 60");
    assert!(session.set_transition_out(Lane::V1, 0, 10));
    let v2 = session.add_lane(LaneKind::Video);
    session
        .place_stream_at(0.0, &asset("test_av.mp4"), 0, Some(v2))
        .expect("place the picture on V2");
    let pip = engine::transform::TransformParams {
        pos_x: 0.3,
        pos_y: 0.3,
        scale: 0.3,
        ..Default::default()
    };
    assert!(session.set_transform(v2, 0, Some(pip)));

    // Frame 55 is the window's sixth frame of ten: A and B both show.
    let at = 55;
    let layered = session.compose_at(at, W, H).expect("layered dissolve");
    // The same dissolve with nothing over it: the single-clip blend.
    assert!(session.set_lane_hidden(v2, true));
    let blend = session.compose_at(at, W, H).expect("V1 dissolve alone");
    assert!(session.set_lane_hidden(v2, false));
    // A alone: the same frame with the dissolve taken away.
    assert!(session.set_transition_out(Lane::V1, 0, 0));
    assert!(session.set_lane_hidden(v2, true));
    let a_alone = session.compose_at(at, W, H).expect("A alone");
    assert!(session.set_lane_hidden(v2, false));
    assert!(session.set_transition_out(Lane::V1, 0, 10));
    assert!(session.set_lane_hidden(Lane::V1, true));
    let v2_alone = session.compose_at(at, W, H).expect("V2 alone");
    assert!(session.set_lane_hidden(Lane::V1, false));

    let (mut inside, mut outside, mut blended_apart) = (0usize, 0usize, 0usize);
    for i in 0..(W * H) as usize {
        let is_pip = px(&v2_alone, i).iter().any(|&c| c > 6);
        if is_pip {
            inside += 1;
            assert_eq!(px(&layered, i), px(&v2_alone, i), "pixel {i} is not the V2 picture");
        } else {
            outside += 1;
            assert_eq!(
                px(&layered, i),
                px(&blend, i),
                "pixel {i} ({}, {}) is not the A/B blend",
                i % W as usize,
                i / W as usize
            );
            if px(&a_alone, i) != px(&blend, i) {
                blended_apart += 1;
            }
        }
    }
    // The check has to be able to tell a blend from A. The test clip is mostly
    // still, so the blend departs from A on a few thousand pixels, not most.
    assert!(inside > (W * H) as usize / 20, "the PiP covers only {inside} pixels");
    assert!(outside > (W * H) as usize / 2, "V1 shows on only {outside} pixels");
    assert!(blended_apart > 1000, "the blend is A itself: only {blended_apart} pixels differ");
}
