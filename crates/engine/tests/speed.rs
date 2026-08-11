//! Per-clip speed where it has to be true: in the pictures a preview shows, in
//! the samples the device is fed, in the file an export writes, and in the
//! project file the whole lot survives in.
//!
//! Everything goes through the front door. A `Project` is edited with
//! `set_speed` and then read exactly as `PlaybackSession::seek` reads it --
//! `audio_segments_from` + `audio_eqs_from` + `audio_speeds_from` handed to the
//! same opener playback hands them to, and `composite_span_at` handed to the
//! same decoder. Nothing calls the resampler directly; `project.rs`'s own unit
//! tests own the placement arithmetic, and what is measured here is the wiring.
//!
//! The fixture is `test_speed_sync.mp4` (`gen_fixtures.sh`): black with one white
//! flash from t=1.0 to t=1.1, silent with a 1 kHz beep over exactly that
//! stretch. A re-timed clip that drifts puts the beep where the flash is not,
//! which is the whole reason that file exists.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::{Lane, Speed};
use engine::{AudioSession, Clip, DecodeSession, ExportHandle, Project};

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The sync fixture as a fresh project: one video clip and one audio clip,
/// grouped, covering the whole file -- what opening it in the editor gives.
fn sync_project() -> (Project, engine::VideoMeta) {
    let path = asset("test_speed_sync.mp4");
    let (meta, _) = engine::demux::Demuxer::open(&path).expect("open the sync fixture");
    (Project::single(&path, meta.frame_count), meta)
}

/// What the device would be fed, drained to the end: the exact three lists
/// `PlaybackSession::seek` builds, handed to the exact opener it hands them to.
fn play(project: &Project) -> Vec<f32> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, FPS);
    let eqs = project.audio_eqs_from(0, FPS);
    let speeds = project.audio_speeds_from(0, FPS);
    let (meta, chunks) = AudioSession::open_mixed_streams_speed(&sources, &segs, &eqs, &speeds)
        .expect("open the timeline's audio")
        .expect("the timeline has audio");
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));
    let mut samples = Vec::new();
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    samples
}

/// When the beep starts, in timeline seconds: the first sample above a tenth of
/// full scale, which the silence around it is nowhere near.
fn beep_at(samples: &[f32]) -> f64 {
    let at = samples
        .iter()
        .position(|s| s.abs() > 0.1)
        .expect("the fixture beeps");
    at as f64 / 2.0 / f64::from(RATE)
}

/// When the flash shows, in timeline seconds: the first frame the composite maps
/// to a white picture, walked exactly as `PlaybackSession::try_frame` walks it
/// -- the decoder is opened over the span's *source* range and its frame numbers
/// come back through the span's own rate.
fn flash_at(project: &Project) -> f64 {
    let span = project.composite_span_at(0).expect("a span at frame 0");
    let (source, in_frame) = span.from.expect("the span plays a clip");
    let path = &project.sources()[source].path;
    let (_, frames, _) = DecodeSession::open_range(path, in_frame, in_frame + span.source_len())
        .expect("open the fixture's picture");
    for frame in frames {
        // Mid-picture, so a stray edge pixel cannot answer for the frame.
        let middle = frame.bgra.len() / 2 / 4 * 4;
        if frame.bgra[middle] > 200 {
            let timeline = span.start + span.speed.timeline_at(frame.index - in_frame);
            return f64::from(timeline) / FPS;
        }
    }
    panic!("the fixture flashes");
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < limit, "export did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

fn out_path(name: &str, ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("edith-speed-{name}-{}.{ext}", std::process::id()))
}

/// The headline: a clip at 2x is half as long on the timeline, plays the same
/// source range, and its box is the one thing that moved.
#[test]
fn a_clip_at_2x_takes_half_the_timeline_and_the_same_source() {
    let (mut project, _) = sync_project();
    let before = project.clips()[0];
    project
        .set_speed(Lane::V1, 0, Speed::from_permille(2000))
        .expect("nothing is in the way of a clip that got shorter");
    let after = project.clips()[0];
    assert_eq!(
        (after.in_frame, after.out_frame),
        (before.in_frame, before.out_frame),
        "the source range a trim and a split address is untouched"
    );
    assert_eq!(
        after.frames(),
        before.frames().div_ceil(2),
        "half the timeline frames, rounded"
    );
    assert_eq!(project.timeline_frames(), after.frames());
    // ...and its sound with it, on the other lane: one rate for the group.
    assert_eq!(project.speed_of(Lane::A1, 0).permille(), 2000);
    assert_eq!(project.lane(Lane::A1)[0].end(), after.end());
}

/// The invariant a footprint may never break, over the whole legal range: a
/// clip always occupies at least one frame, whatever it is divided by.
#[test]
fn no_rate_ever_makes_a_clip_occupy_nothing() {
    for permille in [250, 333, 500, 999, 1000, 1001, 1500, 2000, 3999, 4000] {
        let speed = Speed::from_permille(permille);
        for len in [1, 2, 3, 7, 30, 999] {
            assert!(
                speed.frames(len) >= 1,
                "{len} source frames at {speed} occupies nothing"
            );
        }
    }
}

/// Slowing a clip down makes it wider, and a lane may not overlap itself: the
/// refusal *names* the clip in the way, and costs no undo step.
#[test]
fn a_slow_down_into_the_next_clip_is_refused_by_name() {
    let (mut project, meta) = sync_project();
    let half = meta.frame_count / 2;
    assert!(project.split(half), "cut the take in two");
    let before = project.clips().to_vec();
    let text = project
        .set_speed(Lane::V1, 0, Speed::from_permille(500))
        .expect_err("the first half cannot double in length where it sits")
        .to_string();
    assert!(text.contains(&half.to_string()), "{text}");
    assert!(text.contains("0.50x"), "{text}");
    assert_eq!(project.clips(), before, "nothing moved");
    // ...and the refusal cost no undo step of its own: the one press left is
    // the split's, which puts the take back together.
    assert!(project.undo(), "the split is still the last thing undone");
    assert_eq!(project.clips().len(), 1, "the take is whole again");
    assert!(!project.undo(), "and nothing else was ever pushed");
}

/// One rate change is one undo press, for the whole linked group -- the picture
/// and the sound come back together.
#[test]
fn one_speed_change_is_one_undo_for_the_whole_group() {
    let (mut project, _) = sync_project();
    let (video, audio) = (project.clips().to_vec(), project.lane(Lane::A1).to_vec());
    project
        .set_speed(Lane::A1, 0, Speed::from_permille(2000))
        .expect("room enough");
    // A drag is one snapshot and then live writes, which is what keeps a
    // gesture across the card's bar one undo and not forty.
    for permille in [1500, 1750, 4000] {
        project
            .set_speed_live(Lane::A1, 0, Speed::from_permille(permille))
            .expect("room enough");
    }
    assert!(project.undo(), "one press");
    assert_eq!(project.clips(), video, "the picture is back");
    assert_eq!(project.lane(Lane::A1), audio, "and its sound with it");
    assert!(!project.undo(), "there was only ever one step");
}

/// A rate follows the clip through a split and through the clipboard, exactly
/// as an equalizer does: both halves keep it, and so does a paste.
#[test]
fn speed_survives_a_split_and_a_paste() {
    let (mut project, meta) = sync_project();
    project
        .set_speed(Lane::V1, 0, Speed::from_permille(2000))
        .expect("room enough");
    let whole = project.clips()[0].frames();
    assert!(project.split(whole / 2), "a 2x clip cuts on any frame");
    let halves = project.clips().to_vec();
    assert_eq!(halves.len(), 2);
    assert!(
        halves.iter().all(|c| c.speed.permille() == 2000),
        "both halves play at the rate the whole did"
    );
    assert_eq!(
        halves[0].frames() + halves[1].frames(),
        whole,
        "and they still add up to the clip that was cut"
    );
    assert_eq!(halves[0].out_frame, halves[1].in_frame, "in source frames");
    assert_eq!(halves[1].end(), whole, "the lane is unchanged in length");

    let copied = halves[0];
    assert!(project.paste(whole, copied), "paste it at the end");
    let pasted = *project.clips().last().expect("something was pasted");
    assert_eq!(pasted.speed, copied.speed, "a copy carries the rate");
    assert_eq!(
        pasted.frames(),
        copied.frames(),
        "and therefore the room it takes"
    );
    let _ = meta;
}

/// The one the anti-list is about: at 2x the beep still lands on the flash. The
/// picture comes off the decoder through the span's own rate and the sound comes
/// out of the resampler in the worker -- two independent paths, measured against
/// each other.
#[test]
fn the_beep_stays_on_the_flash_at_every_rate() {
    // Where the fixture's own mark sits, measured rather than assumed: what the
    // rates below are checked against is this, divided.
    let (base, _) = sync_project();
    let (mark, mark_beep) = (flash_at(&base), beep_at(&play(&base)));
    println!("real time: flash at {mark:.3}s, beep at {mark_beep:.3}s");
    assert!(
        (mark - mark_beep).abs() <= 1.0 / FPS && mark > 2.0 / FPS,
        "the fixture's flash and beep are its whole point: {mark:.3}s / {mark_beep:.3}s"
    );
    for permille in [2000, 500, 1500] {
        let (mut project, _) = sync_project();
        let speed = Speed::from_permille(permille);
        project
            .set_speed(Lane::V1, 0, speed)
            .expect("one clip on the timeline has room for any rate");
        let flash = flash_at(&project);
        let beep = beep_at(&play(&project));
        let apart = (flash - beep).abs();
        println!("{speed}: flash at {flash:.3}s, beep at {beep:.3}s, {apart:.3}s apart");
        assert!(
            apart <= 1.0 / FPS,
            "{speed}: the beep is {apart:.3}s off the flash, which is more than a frame"
        );
        // ...and both of them moved: at 2x the mark is at half its own time, and
        // a test that measured two things standing still would pass forever.
        let expected = mark / speed.as_f64();
        assert!(
            (flash - expected).abs() <= 2.0 / FPS,
            "{speed}: the flash should be around {expected:.3}s, not {flash:.3}s"
        );
    }
}

/// A WAV of a speeded timeline is as long as the timeline says, and it is the
/// sound that was heard -- the beep in the file sits where the preview put it.
#[test]
fn a_wav_export_of_a_2x_timeline_matches_the_preview() {
    let (mut project, meta) = sync_project();
    project
        .set_speed(Lane::V1, 0, Speed::from_permille(2000))
        .expect("room enough");
    let frames = project.timeline_frames();
    let heard = beep_at(&play(&project));
    let out = out_path("wav", "wav");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Wav,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(120)).expect("wav export");
    let mut reader = hound::WavReader::open(&out).expect("read the export back");
    let spec = reader.spec();
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| f32::from(s.expect("a sample")) / f32::from(i16::MAX))
        .collect();
    let written = samples.len() / usize::from(spec.channels);
    let expected = (f64::from(frames) / FPS * f64::from(RATE)).round() as usize;
    assert!(
        written.abs_diff(expected) <= spec.sample_rate as usize / 30,
        "the file is {written} frames and the timeline is {expected}"
    );
    let in_file = beep_at(&samples);
    assert!(
        (in_file - heard).abs() <= 1.0 / FPS,
        "the beep is at {in_file:.3}s in the file and was at {heard:.3}s in the preview"
    );
    std::fs::remove_file(&out).ok();
}

/// The other half of the same rule, and it used to be a refusal: an mp4 *copies*
/// AAC packets and a packet carries no rate, so a speeded clip on the lane being
/// copied could only have come out at 1.00x under a re-timed picture. It is
/// decoded, resampled and encoded again now (`export::copy_audio` routes a
/// speeded lane to `encode_audio`), so the file carries the sound that was
/// heard -- measured the way the WAV above is, by where the beep lands.
#[test]
fn an_mp4_export_resamples_a_speeded_sound_clip() {
    let (mut project, meta) = sync_project();
    project
        .set_speed(Lane::A1, 0, Speed::from_permille(2000))
        .expect("room enough");
    let heard = beep_at(&play(&project));
    let out = out_path("speeded", "mp4");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(180)).expect("an mp4 of a speeded sound clip");
    let (audio, chunks) = AudioSession::open(&out)
        .expect("reopen the export")
        .expect("it has an audio track");
    assert_eq!(audio.sample_rate, RATE);
    let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
    let in_file = beep_at(&samples);
    assert!(
        (in_file - heard).abs() <= 1.0 / FPS,
        "the beep is at {in_file:.3}s in the file and was at {heard:.3}s in the preview"
    );
    std::fs::remove_file(&out).ok();
}

/// The half that honours, and the shape that catches a walk that only works at
/// one rate: **2x then 1x on the same lane**. Each span picks its own source
/// frames, so the mark inside the fast clip lands at half its source offset and
/// the mark inside the slow one lands where it always did -- one coded picture
/// per timeline frame throughout.
///
/// The sound stays at 1.00x, which is what leaves the packet copy something it
/// can carry (the refusal above is the other half of the same rule). Software
/// encode, so this needs no plugin.
#[test]
fn an_mp4_export_honours_a_speeded_picture_across_a_rate_change() {
    unsafe { std::env::set_var("VE_SW", "1") };
    let (mut project, meta) = sync_project();
    // Only the picture: detach it from its sound first, or the rate would land
    // on the copied lane and be refused.
    assert!(
        project.ungroup(Lane::V1, 0),
        "take the picture off the take"
    );
    project
        .set_speed(Lane::V1, 0, Speed::from_permille(2000))
        .expect("room enough");
    assert_eq!(project.speed_of(Lane::A1, 0), Speed::NORMAL, "sound is 1x");
    let fast = project.lane(Lane::V1)[0].frames();
    assert_eq!(fast, 45, "90 source frames at 2x is 45 on the timeline");
    // ...and the same take again behind it, at real time.
    let whole = project.lane(Lane::V1)[0];
    assert!(
        project.place(
            Lane::V1,
            fast,
            Clip {
                speed: Speed::NORMAL,
                ..whole
            }
        ),
        "a second, unspeeded clip of the same source"
    );
    let total = project.timeline_frames();
    assert_eq!(total, fast + 90, "45 fast frames then 90 slow ones");

    let out = out_path("honoured", "mp4");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(300)).expect("an mp4 export of a speeded picture");
    let (_, frames) = DecodeSession::open(&out).expect("reopen the export");
    let frames: Vec<_> = frames.into_iter().collect();
    assert_eq!(
        frames.len() as u32,
        total,
        "one coded picture per timeline frame"
    );
    let bright: Vec<u32> = frames
        .iter()
        .filter(|frame| {
            frame.bgra.iter().map(|&b| u64::from(b)).sum::<u64>() / frame.bgra.len() as u64 > 128
        })
        .map(|frame| frame.index)
        .collect();
    // The fixture flashes for four source frames, 30..=33. At 2x the walk takes
    // every second one, so the flash is two timeline frames wide and starts at
    // 15; in the clip behind it, at real time, it is its full four frames and
    // starts at 45 + 30. Exactly that -- a walk that decoded per timeline frame
    // instead of per source frame would smear the mark or lose it.
    assert_eq!(
        bright,
        vec![15, 16, 75, 76, 77, 78],
        "the flash halved at 2x, then whole at 1.00x"
    );
    std::fs::remove_file(&out).ok();
}

/// AV1 honours a speeded picture with no detaching and no refusal -- and its
/// sound, which it now carries, is resampled exactly as the mp4's is.
#[test]
fn an_av1_export_honours_a_speeded_take_whole() {
    let (mut project, meta) = sync_project();
    project
        .set_speed(Lane::V1, 0, Speed::from_permille(2000))
        .expect("room enough");
    let total = project.timeline_frames();
    let out = out_path("av1", "mkv");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Av1,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(600)).expect("an AV1 export of a speeded take");
    assert!(out.exists(), "AV1 never refuses a rate");
    assert!(
        std::fs::metadata(&out).expect("the export").len() > 0,
        "and it wrote something"
    );
    // The timeline halved with the take, so the file is the shorter one.
    assert_eq!(total, 45, "the whole take at 2x");
    std::fs::remove_file(&out).ok();
}
