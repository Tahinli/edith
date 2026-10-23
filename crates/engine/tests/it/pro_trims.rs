//! Roll, slip and slide through a session, against a real file's length: the
//! source walls come from the media, a take moves together, each is one undo.
//!
//! ```text
//! cargo test -p engine --test pro_trims
//! ```

use std::path::PathBuf;

use engine::project::Lane;
use engine::PlaybackSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

/// `test_av.mp4` is 150 frames at 30 fps; cut at 1 s and 3 s: V1+A1 as 0..30, 30..90, 90..150.
fn cut_in_three() -> PlaybackSession {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.set_gain(0.0);
    assert!(s.cut_at(1.0) && s.cut_at(3.0));
    assert_eq!(s.lane_clips(Lane::V1).len(), 3);
    s
}

fn spans(s: &PlaybackSession, lane: Lane) -> Vec<(u32, u32, u32, u32)> {
    s.lane_clips(lane).iter().map(|c| (c.start, c.end(), c.in_frame, c.out_frame)).collect()
}

#[test]
fn roll_moves_a_cut_and_undo_puts_it_back() {
    let mut s = cut_in_three();
    let depth = s.undo_depth();
    assert_eq!(s.roll_room(Lane::V1, 0).map(|r| r.0 >= 1), Some(true));
    assert_eq!(s.roll_clip(Lane::V1, 0, 45), Some(45));
    for lane in [Lane::V1, Lane::A1] {
        assert_eq!(spans(&s, lane), [(0, 45, 0, 45), (45, 90, 45, 90), (90, 150, 90, 150)], "{lane:?}");
    }
    assert_eq!(s.undo_depth(), depth + 1);
    assert!(s.undo());
    assert_eq!(spans(&s, Lane::V1), [(0, 30, 0, 30), (30, 90, 30, 90), (90, 150, 90, 150)]);
    assert_eq!(s.roll_clip(Lane::V1, 2, 100), None, "the last clip has no cut behind it");
}

#[test]
fn slip_stops_at_the_file_and_slide_keeps_the_length() {
    let mut s = cut_in_three();
    assert_eq!(s.slip_room(Lane::V1, 1), Some((-30, 60)));
    assert_eq!(s.slip_clip(Lane::V1, 1, 1000), Some(60), "clamped to the file's last frame");
    assert_eq!(spans(&s, Lane::V1)[1], (30, 90, 90, 150));
    assert!(s.undo());
    assert_eq!(s.slide_clip(Lane::V1, 1, 40), Some(40));
    for lane in [Lane::V1, Lane::A1] {
        assert_eq!(spans(&s, lane), [(0, 40, 0, 40), (40, 100, 30, 90), (100, 150, 100, 150)], "{lane:?}");
    }
    assert_eq!(s.slide_clip(Lane::V1, 0, 5), None, "no clip before it");
    assert!(s.undo());
    assert_eq!(spans(&s, Lane::V1), [(0, 30, 0, 30), (30, 90, 30, 90), (90, 150, 90, 150)]);
}
