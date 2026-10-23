//! Keyed transforms: a clip's placement animated over its own source frames.
//! Headless (`VE_NO_AO`), through the same session API the preview and the
//! MCP use. Run with `cargo test --release -p engine --test transform_keys`.

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::project::{Clip, Edge, Lane, LaneKind, Speed};
use engine::transform::{KeyEase, TransformKey, TransformParams};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn open(path: impl AsRef<Path>) -> PlaybackSession {
    engine::ao::set_silent_output();
    PlaybackSession::open(path).expect("open")
}

/// `test_av.mp4` is 1280x720, 30 fps, 150 frames.
const W: u32 = 640;
const H: u32 = 360;

fn scaled(scale: f32) -> TransformParams {
    TransformParams {
        scale,
        ..TransformParams::default()
    }
}

fn key(at: u32, scale: f32, ease: KeyEase) -> TransformKey {
    TransformKey {
        at,
        params: scaled(scale),
        ease,
    }
}

/// The largest per-channel difference between two composed frames.
fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| x.abs_diff(y))
        .max()
        .unwrap_or(0)
}

#[test]
fn a_keyed_clip_composes_its_interpolated_placement_at_each_frame() {
    // Keys 1.0 -> 1.2 over source frames 0..100: frame 50 is scale 1.1, the
    // same picture a clip set to 1.1 outright shows there.
    let mut keyed = open(asset("test_av.mp4"));
    assert!(keyed.set_transform_keys(
        Lane::V1,
        0,
        vec![key(0, 1.0, KeyEase::Linear), key(100, 1.2, KeyEase::Linear)],
    ));
    let mut fixed = open(asset("test_av.mp4"));
    assert!(fixed.set_transform(Lane::V1, 0, Some(scaled(1.1))));

    let mid_keyed = keyed.compose_at(50, W, H).expect("keyed midpoint");
    let mid_fixed = fixed.compose_at(50, W, H).expect("fixed 1.1");
    let diff = max_diff(&mid_keyed.bgra, &mid_fixed.bgra);
    assert!(diff <= 4, "midpoint differs from scale 1.1 by {diff}");

    // Exact at a key: frame 0 is the 1.0 key, and frame 100 the 1.2 one.
    let plain = open(asset("test_av.mp4"));
    let start_keyed = keyed.compose_at(0, W, H).expect("start");
    let start_plain = plain.compose_at(0, W, H).expect("start plain");
    assert!(max_diff(&start_keyed.bgra, &start_plain.bgra) <= 1, "frame 0 is the 1.0 key");
    assert!(
        max_diff(
            &keyed.compose_at(100, W, H).expect("end").bgra,
            &fixed_at(1.2).compose_at(100, W, H).expect("1.2").bgra,
        ) <= 1,
        "frame 100 is the 1.2 key"
    );
    // And the picture actually moves between the two keys.
    assert_ne!(mid_keyed.bgra, start_keyed.bgra, "the zoom changes the frame");
}

fn fixed_at(scale: f32) -> PlaybackSession {
    let mut s = open(asset("test_av.mp4"));
    assert!(s.set_transform(Lane::V1, 0, Some(scaled(scale))));
    s
}

#[test]
fn keys_are_divided_by_a_split_and_the_picture_does_not_change() {
    let mut s = open(asset("test_av.mp4"));
    // Linear, so the split is exact: a linear segment cut in two is the same
    // line. An eased segment is re-fitted to each half (its cut key is an
    // endpoint of both), which moves the picture by a little inside it.
    assert!(s.set_transform_keys(
        Lane::V1,
        0,
        vec![key(0, 1.0, KeyEase::Linear), key(120, 1.4, KeyEase::Linear)],
    ));
    let before_30 = s.compose_at(30, W, H).expect("30 before").bgra;
    let before_90 = s.compose_at(90, W, H).expect("90 before").bgra;

    assert!(s.cut_at(2.0), "a cut at frame 60 on V1");
    let head = s.transform_keys_of(Lane::V1, 0).to_vec();
    let tail = s.transform_keys_of(Lane::V1, 1).to_vec();
    assert_eq!(head.last().map(|k| k.at), Some(60), "the head ends on the cut");
    assert_eq!(tail.first().map(|k| k.at), Some(60), "the tail starts on the cut");
    assert_eq!(tail.last().map(|k| k.at), Some(120), "the original last key stays");
    assert_eq!(head[0].at, 0, "the first key stays on the head");
    assert_eq!(
        head.last().unwrap().params.scale,
        tail.first().unwrap().params.scale,
        "the cut key is the same placement on both halves"
    );

    assert!(max_diff(&s.compose_at(30, W, H).expect("30 after").bgra, &before_30) <= 1);
    assert!(max_diff(&s.compose_at(90, W, H).expect("90 after").bgra, &before_90) <= 1);
}

#[test]
fn a_tail_trim_drops_the_keys_past_the_end_and_undo_restores_them() {
    let mut s = open(asset("test_av.mp4"));
    let keys = vec![key(0, 1.0, KeyEase::Linear), key(100, 1.2, KeyEase::Linear)];
    assert!(s.set_transform_keys(Lane::V1, 0, keys.clone()));
    let before_40 = s.compose_at(40, W, H).expect("40 before").bgra;
    assert!(s.trim_clip(Lane::V1, 0, Edge::End, 80), "trim the tail to frame 80");

    // The key at 100 is past the new tail, so it goes; the animation at frame
    // 80 is the 1.16 it was there, kept as a key on the edge.
    let now = s.transform_keys_of(Lane::V1, 0).to_vec();
    assert_eq!(now.iter().map(|k| k.at).collect::<Vec<_>>(), vec![0, 80]);
    assert!((now[1].params.scale - 1.16).abs() < 1e-5, "{}", now[1].params.scale);
    assert!(max_diff(&s.compose_at(40, W, H).expect("40 after").bgra, &before_40) <= 1);

    assert!(s.undo(), "the trim is one undo step");
    assert_eq!(s.transform_keys_of(Lane::V1, 0), keys.as_slice(), "undo restores the lost key");
    assert!(s.undo(), "and the keys another");
    assert!(s.transform_keys_of(Lane::V1, 0).is_empty(), "undo clears the keys");
}

#[test]
fn a_head_trim_keeps_the_keys_on_their_footage() {
    let mut s = open(asset("test_av.mp4"));
    assert!(s.set_transform_keys(
        Lane::V1,
        0,
        vec![key(0, 1.0, KeyEase::Linear), key(100, 1.2, KeyEase::Linear)],
    ));
    let before_60 = s.compose_at(60, W, H).expect("60 before").bgra;
    assert!(s.trim_clip(Lane::V1, 0, Edge::Start, 30), "trim the head to frame 30");

    // Source 0 is cut off; the key at source 100 is still on that frame, and a
    // key on the new in-point holds the value the animation had there.
    let now = s.transform_keys_of(Lane::V1, 0).to_vec();
    assert_eq!(now.iter().map(|k| k.at).collect::<Vec<_>>(), vec![30, 100]);
    assert!((now[0].params.scale - 1.06).abs() < 1e-5, "{}", now[0].params.scale);
    assert_eq!(now[1].params.scale, 1.2);
    // Timeline 60 plays source 60 both before and after, so it is the same picture.
    assert!(max_diff(&s.compose_at(60, W, H).expect("60 after").bgra, &before_60) <= 1);
}

#[test]
fn a_still_keeps_its_keys_at_the_same_timeline_times_after_a_head_trim() {
    let mut s = open(asset("test_av.mp4"));
    let still = s.import(&asset("test_still.png")).expect("import the still");
    // A 5 s still placed at 1 s (frame 30), keyed at source 0, 60 and 150:
    // timeline 1 s, 3 s and 6 s.
    let clip = Clip {
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
        start: 0,
        in_frame: 0,
        out_frame: 150,
        source: still,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: engine::scale::FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    assert!(s.place_at(Lane::V1, 1.0, clip), "place the still at 1 s");
    let idx = s.lane_clips(Lane::V1).len() - 1;
    assert!(s.set_transform_keys(
        Lane::V1,
        idx,
        vec![
            key(0, 1.0, KeyEase::Linear),
            key(60, 1.4, KeyEase::Linear),
            key(150, 0.6, KeyEase::Linear),
        ],
    ));
    let start = s.lane_clips(Lane::V1)[idx].start;
    assert_eq!(start, 30, "the still starts at 1 s");
    let frame_before = s.compose_at(135, W, H).expect("4.5 s before").bgra;

    // Trim the head to 2 s: the still now starts at 60 with its in-point at 0.
    assert!(s.trim_clip(Lane::V1, idx, Edge::Start, 60), "trim the head to 2 s");
    let start = s.lane_clips(Lane::V1)[idx].start;
    assert_eq!(start, 60);
    let keys = s.transform_keys_of(Lane::V1, idx).to_vec();
    let timeline: Vec<u32> = keys.iter().map(|k| start + k.at - s.lane_clips(Lane::V1)[idx].in_frame).collect();
    assert_eq!(timeline, vec![60, 90, 180], "the 3 s and 6 s keys stay on the timeline");
    assert!(max_diff(&s.compose_at(135, W, H).expect("4.5 s after").bgra, &frame_before) <= 1, "4.5 s is unchanged");
}

#[test]
fn keys_are_refused_on_audio_and_when_unsorted_or_doubled() {
    let mut s = open(asset("test_av.mp4"));
    let a1 = Lane::new(LaneKind::Audio, 0);
    assert!(!s.set_transform_keys(a1, 0, vec![key(0, 1.0, KeyEase::Linear)]), "audio");
    assert!(!s.set_transform_keys(
        Lane::V1,
        0,
        vec![key(10, 1.0, KeyEase::Linear), key(10, 1.2, KeyEase::Linear)],
    ), "two keys on one frame");
    assert!(s.set_transform_keys(
        Lane::V1,
        0,
        vec![key(50, 1.2, KeyEase::Linear), key(10, 1.0, KeyEase::Linear)],
    ), "unsorted input is sorted");
    assert_eq!(s.transform_keys_of(Lane::V1, 0)[0].at, 10);
}


#[test]
fn keys_survive_a_save_and_reopen_and_older_files_stay_v31() {
    let dir = std::env::temp_dir().join(format!("transform_keys_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("keys.edith");

    let mut s = open(asset("test_av.mp4"));
    let keys = vec![
        key(0, 1.0, KeyEase::Ease),
        TransformKey {
            at: 100,
            params: TransformParams {
                pos_x: 0.1,
                opacity: 0.5,
                ..scaled(1.2)
            },
            ease: KeyEase::Linear,
        },
    ];
    assert!(s.set_transform_keys(Lane::V1, 0, keys.clone()));
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with("edith 32\n"), "keyed projects are v32:\n{text}");
    assert!(text.contains("tkeys video 1 0 "), "one tkeys line:\n{text}");

    let back = PlaybackSession::open_project(&path).expect("reopen");
    assert_eq!(back.transform_keys_of(Lane::V1, 0), keys.as_slice());

    // Without keys the project is the dialect it always was.
    let plain = open(asset("test_av.mp4"));
    plain.save_project(&path).expect("save plain");
    let text = std::fs::read_to_string(&path).expect("read plain");
    assert!(text.starts_with("edith 30\n"), "no keys, no opacity: v30:\n{text}");
    assert!(!text.contains("tkeys"));
    let _ = std::fs::remove_dir_all(&dir);
}
