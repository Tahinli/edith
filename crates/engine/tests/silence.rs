//! The jumpcut, end to end through the front door: a real file scanned for its
//! silences, the regions that come back cut (or sped up) as **one** edit, and
//! the timeline that leaves.
//!
//! `silence.rs`'s own unit tests own the detection arithmetic. What is measured
//! here is what an editor would see: how long the timeline is afterwards, that
//! every lane moved together, that one undo press takes the whole batch back,
//! and that the refusals name what is in the way. Group consistency is checked
//! the way a save checks it -- the parts are handed back to
//! `Project::from_parts`, which is the door a `.edith` comes in through.

use std::path::PathBuf;

use engine::project::{Lane, LaneKind, Speed};
use engine::silence::{self, Settings};
use engine::{Clip, Project};

const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The A/V fixture as a fresh project: `V1` and `A1`, one grouped clip each
/// covering the whole file -- what opening it in the editor gives.
fn fixture() -> Project {
    let path = asset("test_av.mp4");
    let (meta, _) = engine::demux::Demuxer::open(&path).expect("open the fixture");
    Project::single(&path, meta.frame_count)
}

/// A link is one span on however many lanes, and a project whose links disagree
/// is one no save could load: this is that check, through the same door a load
/// takes.
fn loadable(project: &Project) {
    let (sources, lanes, eq, color) = project.without_orphan_sources();
    Project::from_parts(sources, lanes, eq, color).expect("the lanes still load");
}

fn spans(project: &Project) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    (
        project.lane_spans(Lane::V1),
        project.lane_spans(Lane::new(LaneKind::Audio, 0)),
    )
}

/// Five cuts, one undo press. The linked picture slides exactly as its sound
/// does, the timeline is shorter by exactly what was cut, and the press after
/// the undo reaches the edit that came *before* the jumpcut -- which is what a
/// loop over `ripple_delete` would have buried under five steps.
#[test]
fn a_batch_of_cuts_is_one_edit_and_moves_every_lane_alike() {
    let mut p = fixture();
    let was = p.timeline_frames();
    // An edit first, so the undo stack has something under the batch.
    assert!(p.split(60));
    let before = p.lane(Lane::V1).to_vec();
    let regions = [(10, 5), (30, 5), (50, 5), (70, 5), (90, 5)];
    assert!(p.cut_regions(&regions));

    let cut: u32 = regions.iter().map(|&(_, len)| len).sum();
    assert_eq!(p.timeline_frames(), was - cut);
    let (video, audio) = spans(&p);
    assert_eq!(video, audio, "the picture and its sound moved apart");
    loadable(&p);

    // One press takes the whole batch back...
    assert!(p.undo());
    assert_eq!(p.lane(Lane::V1), before, "one undo did not restore the lot");
    assert_eq!(p.timeline_frames(), was);
    // ...and the next one is the split, not another fifth of the jumpcut.
    assert!(p.undo());
    assert_eq!(p.lane(Lane::V1).len(), 1);
    assert!(!p.undo(), "the batch left extra history behind");
}

/// Speeding the silences up leaves **no holes**: `set_speed` on its own does
/// not ripple, and a gap where a silence shrank is the whole failure this
/// guards. Everything that was contiguous stays contiguous, both lanes shrink
/// alike, and it is one undo press.
#[test]
fn speeding_silences_up_closes_the_room_they_no_longer_need() {
    let mut p = fixture();
    let was = p.timeline_frames();
    let regions = [(30, 20), (80, 20)];
    p.speed_regions(&regions, Speed::MAX).expect("no lane laps");

    // 20 source frames at 4x occupy 5, so each region gives back 15.
    let shrunk = Speed::MAX.frames(20);
    let (video, audio) = spans(&p);
    assert_eq!(video, audio);
    assert_eq!(p.timeline_frames(), was - 2 * (20 - shrunk));
    for pair in video.windows(2) {
        assert_eq!(
            pair[0].0 + pair[0].1,
            pair[1].0,
            "a hole was left where a silence shrank: {video:?}"
        );
    }
    // The pieces that were the silences are the ones running fast, and only
    // those.
    let fast: Vec<u32> = p
        .lane(Lane::V1)
        .iter()
        .filter(|c| c.speed == Speed::MAX)
        .map(|c| c.start)
        .collect();
    assert_eq!(fast, vec![30, 30 + shrunk + (80 - 50)]);
    loadable(&p);

    // Absolute, not compounding: running it again over where those pieces sit
    // now leaves the timeline byte for byte as it was -- 4x set twice is 4x.
    let settled = p.lane(Lane::V1).to_vec();
    let again: Vec<(u32, u32)> = fast.iter().map(|&at| (at, shrunk)).collect();
    p.speed_regions(&again, Speed::MAX).expect("still no lap");
    assert_eq!(p.lane(Lane::V1), settled, "a second pass compounded");

    assert!(p.undo());
    assert!(p.undo());
    assert_eq!(p.lane(Lane::V1).len(), 1, "one press per apply, no more");
    assert!(!p.undo());
}

/// A clip on another lane that laps over the edge of a silence makes speed mode
/// refuse **by name**: the lanes would shrink by different amounts and the
/// ripple would pull them apart. The refusal costs no undo step and changes
/// nothing -- and cutting the same regions is still fine, which is why delete
/// mode needs no such rule.
#[test]
fn speed_mode_refuses_a_clip_that_laps_over_a_silence() {
    let mut p = fixture();
    let lane = p.add_lane(LaneKind::Video);
    assert_eq!(lane.label(), "V2");
    let broll = Clip {
        start: 0,
        in_frame: 0,
        out_frame: 20,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: Default::default(),
        speed: Speed::NORMAL,
    };
    assert!(p.place(lane, 40, broll));
    let before = spans(&p);

    let err = p
        .speed_regions(&[(30, 20)], Speed::MAX)
        .expect_err("a half-covered region must not be sped up")
        .to_string();
    assert!(err.contains("V2"), "{err}");
    assert!(err.contains("40"), "{err}");
    assert_eq!(spans(&p), before, "a refusal changed the timeline");

    // No undo step was spent on it: the press below is the `place` above.
    assert!(p.undo());
    assert!(p.lane(lane).is_empty());

    // The same regions cut instead: a ripple is uniform, so nothing laps.
    assert!(p.place(lane, 40, broll));
    assert!(p.cut_regions(&[(30, 20)]));
    loadable(&p);
}

/// The whole chain a person presses one key for: the fixture is scanned, its
/// silences come back as timeline frames, and cutting them shortens the
/// timeline by exactly what was found -- with the scan itself having changed
/// nothing at all.
///
/// `test_av.mp4`'s envelope is silent every second at t≈0.75, so the regions
/// land there ±1 frame.
#[test]
fn scanning_a_clip_and_cutting_what_it_finds() {
    let mut p = fixture();
    let was = p.timeline_frames();
    // Nothing to undo yet, and the scan below must not change that: a preview
    // is not an edit.
    assert!(!p.undo());

    let levels = silence::levels(asset("test_av.mp4"), 0)
        .expect("scan")
        .expect("the fixture has audio");
    let cfg = Settings {
        threshold_db: -40.,
        min_silence: 0.08,
        padding: 0.02,
        min_keep: 0.,
    };
    let audio = Lane::new(LaneKind::Audio, 0);
    let clip = p.lane(audio)[0];
    let regions = silence::timeline_regions(&clip, FPS, &silence::regions(&levels, cfg));
    assert_eq!(regions.len(), 5, "{regions:?}");
    for (second, &(start, len)) in regions.iter().enumerate() {
        // The dip of that second, in frames, sits inside the marked region.
        let dip = (second as f64 + 0.75) * FPS;
        assert!(
            (f64::from(start) - 1.0..f64::from(start + len) + 1.0).contains(&dip),
            "second {second}: [{start}, {}) is not the dip at {dip}",
            start + len
        );
    }
    assert!(!p.undo(), "the scan took an undo step");

    let cut: u32 = regions.iter().map(|&(_, len)| len).sum();
    assert!(p.cut_regions(&regions));
    assert_eq!(p.timeline_frames(), was - cut);
    let (video, audio_spans) = spans(&p);
    assert_eq!(video, audio_spans);
    loadable(&p);

    // What a jumpcut leaves is ordinary clips, so the file it saves to is the
    // same dialect it always was -- no wire change went with this feature.
    let dir = std::env::temp_dir().join(format!("edith-silence-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a place to save");
    let file = dir.join("cut.edith");
    let (sources, lanes, eq, color) = p.without_orphan_sources();
    engine::edith::save(&file, &sources, &lanes, &eq, &color, (1920, 1080), 0).expect("save");
    let text = std::fs::read_to_string(&file).expect("read it back");
    assert!(
        text.starts_with("edith 8"),
        "{:?}",
        &text[..16.min(text.len())]
    );
    let back = engine::edith::load(&file).expect("load");
    assert_eq!(back.lanes, lanes, "the cut timeline did not round-trip");
    std::fs::remove_dir_all(&dir).ok();

    assert!(p.undo());
    assert_eq!(p.timeline_frames(), was);
}
