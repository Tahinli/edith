//! Every placement door holds each lane's clip to that lane's wall, so a clip
//! the save would refuse can never be placed, and an autosave never refuses.
//!
//! The asset is `test_23976fps.mp4`: 2.002 s of picture (61 frames on the 30
//! fps timeline) and 2.000 s of sound (60 frames). A clip of it on an audio lane
//! that ran 61 frames is the bug this file guards: the save checks each lane
//! against its own table, and the sound's is 60.
//!
//! ```text
//! cargo test -p engine --test placement_walls
//! ```

use std::path::PathBuf;

use engine::project::{Lane, LaneKind, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::PlaybackSession;
use engine::project::Clip;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// A 30 fps timeline with `test_23976fps.mp4` in its library, silent.
fn session() -> (PlaybackSession, usize) {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.set_gain(0.0);
    let idx = s.import(&asset("test_23976fps.mp4")).expect("import");
    assert_eq!(s.file_frames(&asset("test_23976fps.mp4")), 61, "picture is 61 frames");
    (s, idx)
}

/// A clip of `source` over `in_frame..out_frame`, as the MCP builds one.
fn clip(source: usize, in_frame: u32, out_frame: u32) -> Clip {
    Clip {
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
        in_frame,
        out_frame,
        source,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    }
}

fn a(n: usize) -> Lane {
    Lane::new(LaneKind::Audio, n)
}

fn v(n: usize) -> Lane {
    Lane::new(LaneKind::Video, n)
}

/// Every lane's clips are inside their walls (61 on picture, 60 on sound), and
/// the project saves and reopens with no clamp warning -- which is what the
/// save and the load both demand.
fn assert_holds(s: &PlaybackSession, label: &str) {
    for lane in s.lanes() {
        for (i, c) in s.lane_clips(lane).iter().enumerate() {
            let wall = match lane.kind {
                LaneKind::Audio => 60,
                _ => 61,
            };
            if c.source == s.sources().len() - 1 {
                assert!(
                    c.out_frame <= wall,
                    "{label}: {lane:?} clip {i} ends at {} past its wall {wall}",
                    c.out_frame
                );
            }
        }
    }
    let dir = Scratch::dir("placement_walls");
    let path = dir.join(format!("{}.edith", label.replace(|c: char| !c.is_alphanumeric(), "_")));
    s.save_project(&path)
        .unwrap_or_else(|e| panic!("{label}: save refused: {e}"));
    let reopened = PlaybackSession::open_project(&path).expect("reopen");
    assert!(
        reopened.load_warnings().is_empty(),
        "{label}: reopen clamped: {:?}",
        reopened.load_warnings()
    );
}

#[test]
fn place_stream_at_every_lane_holds_to_its_wall() {
    let file = asset("test_23976fps.mp4");
    for (label, onto) in [
        ("grouped", None),
        ("V1", Some(v(0))),
        ("V2", Some(v(1))),
        ("A1", Some(a(0))),
        ("A2", Some(a(1))),
    ] {
        let (mut s, _) = session();
        let end = s.timeline_duration();
        s.place_stream_at(end, &file, 0, onto)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_holds(&s, &format!("place_stream_at {label}"));
    }
}

#[test]
fn place_at_with_a_range_past_the_sound_holds_to_the_sound() {
    for lane in [v(0), v(1), a(0), a(1)] {
        let (mut s, src) = session();
        // A further lane is added first, as the MCP's `lane_add` does.
        let lane = match lane.ord {
            0 => lane,
            _ => s.add_lane(lane.kind),
        };
        let at = s.timeline_duration();
        // The whole picture, 61 frames: on a video lane that is the wall; on an
        // audio lane the sound's 60 must cut it back.
        assert!(s.place_at(lane, at, clip(src, 0, 61)), "{lane:?}: placed");
        let landed = s.lane_clips(lane).last().expect("a clip").frames();
        let want = match lane.kind {
            LaneKind::Audio => 60,
            _ => 61,
        };
        assert_eq!(landed, want, "{lane:?} holds to its own wall");
        assert_holds(&s, &format!("place_at {lane:?}"));
    }
}

#[test]
fn place_at_past_the_wall_entirely_is_refused_not_saved_wrong() {
    let (mut s, src) = session();
    let at = s.timeline_duration();
    // In-point at the sound's end: no sound frame is left for an audio lane.
    assert!(!s.place_at(a(0), at, clip(src, 60, 61)));
    assert_holds(&s, "refused sound");
}

#[test]
fn paste_and_paste_set_hold_each_item_to_its_lane() {
    let (mut s, src) = session();
    let at = s.timeline_duration();
    assert!(s.paste_at(at, clip(src, 0, 61)), "paste lands");
    assert_holds(&s, "paste_at");

    let (mut s, src) = session();
    let at = s.timeline_duration();
    assert!(s.paste_set_at(at, &[(v(0), clip(src, 0, 61)), (a(0), clip(src, 0, 61))]));
    assert_eq!(s.lane_clips(a(0)).last().expect("sound").frames(), 60);
    assert_eq!(s.lane_clips(v(0)).last().expect("picture").frames(), 61);
    assert_holds(&s, "paste_set_at");
}

#[test]
fn autosave_writes_a_project_that_runs_past_its_source() {
    let (mut s, src) = session();
    let at = s.timeline_duration();
    assert!(s.place_at(a(0), at, clip(src, 0, 61)));
    let dir = Scratch::dir("placement_walls_autosave");
    let path = dir.join("autosave.edith");
    s.save_autosave(&path).expect("an autosave is never refused");
    assert!(path.exists(), "the autosave wrote its file");
    // The reopen clamps with a warning rather than refusing, so the file opens.
    let reopened = PlaybackSession::open_project(&path).expect("the autosave reopens");
    assert!(!reopened.sources().is_empty());
}

/// A drop overwrites: a file let go over what is already there for its own length replaces it,
/// and nothing after it moves. The insert door, on the same frames, pushes the rest along.
#[test]
fn place_stream_overwrite_at_replaces_and_holds_the_timeline_length() {
    let file = asset("test_23976fps.mp4");
    let (mut s, _) = session();
    s.place_stream_at(0.0, &file, 0, None).expect("first placement");
    let before = s.timeline_duration();
    s.place_stream_overwrite_at(0.0, &file, 0, None)
        .expect("overwrite over the clip");
    assert_eq!(s.timeline_duration(), before, "an overwrite pushes nothing past the end");
    assert_holds(&s, "overwrite over a take");

    let (mut t, _) = session();
    t.place_stream_at(0.0, &file, 0, None).expect("first placement");
    let before = t.timeline_duration();
    t.place_stream_at(0.0, &file, 0, None).expect("insert over the clip");
    assert!(t.timeline_duration() > before, "an insert pushes the clip under it along");
}
