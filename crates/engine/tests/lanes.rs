//! N lanes, composited: what a timeline with a `V2` over its `V1` and an `A2`
//! over its `A1` plays, and that an export of it is the same thing.
//!
//! The fixtures are the two video assets, which differ picture for picture, so
//! "which lane is showing" is answered by comparing the decoded frame against
//! each source rather than by trusting the edit list.
//!
//! ```text
//! cargo test -p engine --release --test lanes -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::{Clip, Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::{DecodeSession, ExportHandle, PlaybackSession, Project};

const FPS: f64 = 30.0;
/// `test_av.mp4` is 150 frames, `test_av2.mp4` 120; the top lane covers the
/// middle third of the first.
const TOTAL: u32 = 150;
const TOP_IN: u32 = 50;
const TOP_OUT: u32 = 100;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Software on both halves, so this suite proves the composite on any machine
/// rather than whatever the local VA-API driver does.
fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
}

/// Its own directory, gone with the value: two suites at once must not write
/// each other's files, and nothing may outlive the test that wrote it.
fn out_path(name: &str, ext: &str) -> Scratch {
    Scratch::file(&format!("ve_lanes_{name}"), ext)
}

/// A hand-written v4 project: `V1` the whole of `test_av`, `V2` the middle
/// third playing `test_av2` from its own frame 0, `A1` the whole of `test_av`.
/// Exactly the file a user would write by hand, parsed by the real loader.
///
/// `name` is the caller's own, because [`out_path`] is unique per *run* and not
/// per test: two tests sharing one name run in parallel over one file, and the
/// first to finish deletes it out from under the second -- which then fails on
/// a file that is simply not there any more.
fn three_lane_file(name: &str) -> Scratch {
    let path = out_path(name, "edith");
    // `source <audio stream> <path>`: the number is which audio track of the
    // file plays, not the source index -- both of these are on their first.
    let text = format!(
        "edith 4\nplayhead 0\nsource 0 {}\nsource 0 {}\n\
         video 1 0 0 {TOTAL} 0 -\naudio 1 0 0 {TOTAL} 0 -\n\
         video 2 {TOP_IN} 0 {} 1 -\n",
        asset("test_av.mp4").display(),
        asset("test_av2.mp4").display(),
        TOP_OUT - TOP_IN,
    );
    std::fs::write(&path, text).expect("write the project file");
    path
}

/// One frame of `path` by absolute index, BGRA.
fn frame_at(path: &Path, index: u32) -> Vec<u8> {
    let (_, frames, _) = DecodeSession::open_at(path, index).expect("open source");
    frames.recv().expect("frame present").bgra
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "frame sizes differ");
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.len() as f64
}

/// Which of the two sources `frame` came from, by picture: the near one wins,
/// and the assertion is that the other is nowhere near.
fn assert_from(frame: &[u8], source: &str, source_frame: u32, timeline: u32) {
    let other = match source {
        "test_av.mp4" => "test_av2.mp4",
        _ => "test_av.mp4",
    };
    let mine = mean_abs_diff(frame, &frame_at(&asset(source), source_frame));
    let theirs = mean_abs_diff(frame, &frame_at(&asset(other), source_frame));
    println!("timeline {timeline}: {source} {source_frame} diff {mine:.2}, {other} {theirs:.2}");
    assert!(
        mine < 6.0,
        "timeline frame {timeline} drifted by {mine:.2} from {source} frame {source_frame}"
    );
    assert!(
        theirs > 4.0 * mine.max(0.5),
        "the two sources are too alike at frame {source_frame} for this test to mean anything"
    );
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// Plays the session up to `timeline` and hands back the frame shown there.
/// Times it, because a lane change costs exactly one decoder reopen -- the
/// composite is one span list, so N video lanes never mean N decoders.
fn frame_of(session: &mut PlaybackSession, timeline: u32) -> Vec<u8> {
    let started = Instant::now();
    session.seek(f64::from(timeline) / FPS);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        session.tick();
        if let Some(frame) = session.try_frame() {
            assert_eq!(frame.index, timeline, "a seek shows the frame it landed on");
            println!(
                "frame {timeline} ready {:.0} ms after the seek",
                started.elapsed().as_secs_f64() * 1000.0
            );
            return frame.bgra;
        }
        assert!(Instant::now() < deadline, "no frame at timeline {timeline}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The slice, from the front door: a hand-written project with a second video
/// lane opens and *plays* the top lane where it has a clip, the one under it
/// everywhere else.
#[test]
fn playback_shows_the_topmost_video_lane() {
    pin_software();
    let project = three_lane_file("playback");
    let mut session = PlaybackSession::open_project(&project).expect("open the project");
    session.set_gain(0.0);
    assert_eq!(session.timeline_duration(), f64::from(TOTAL) / FPS);

    // Before the top lane starts, inside it, and after it ends.
    let head = frame_of(&mut session, 10);
    assert_from(&head, "test_av.mp4", 10, 10);
    let over = frame_of(&mut session, TOP_IN + 10);
    assert_from(&over, "test_av2.mp4", 10, TOP_IN + 10);
    let tail = frame_of(&mut session, TOP_OUT + 10);
    assert_from(&tail, "test_av.mp4", TOP_OUT + 10, TOP_OUT + 10);

    std::fs::remove_file(&project).unwrap();
}

/// Export is playback: the same project written out and decoded back shows the
/// same lane at the same frame.
#[test]
fn export_writes_the_same_composite() {
    pin_software();
    let file = three_lane_file("export");
    let session = PlaybackSession::open_project(&file).expect("open the project");
    let out = out_path("composite", "mp4");

    let started = Instant::now();
    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(300)).expect("export");
    println!(
        "composite export: {TOTAL} frames in {:.2} s",
        started.elapsed().as_secs_f64()
    );

    let (meta, _) = engine::demux::Demuxer::open(&out).expect("reopen the export");
    assert_eq!(meta.frame_count, TOTAL, "timeline frames written");
    let (_, frames) = DecodeSession::open(&out).expect("decode the export");
    let frames: Vec<Vec<u8>> = frames.into_iter().map(|f| f.bgra).collect();
    assert_eq!(frames.len() as u32, TOTAL);
    assert_from(&frames[10], "test_av.mp4", 10, 10);
    assert_from(
        &frames[(TOP_IN + 10) as usize],
        "test_av2.mp4",
        10,
        TOP_IN + 10,
    );
    assert_from(
        &frames[(TOP_OUT + 10) as usize],
        "test_av.mp4",
        TOP_OUT + 10,
        TOP_OUT + 10,
    );

    std::fs::remove_file(&file).unwrap();
    std::fs::remove_file(&out).unwrap();
}

/// Every sample of `path`'s first audio stream, interleaved -- what the export
/// wrote, read back through the engine's own decoder.
fn decoded_audio(path: &Path) -> Vec<f32> {
    let sources = [(path.to_path_buf(), 0)];
    let (_, chunks) =
        engine::AudioSession::open_multi_streams(&sources, &[(Some(0), 0.0, f64::INFINITY)])
            .expect("open the audio")
            .expect("there is an audio track");
    chunks.into_iter().flat_map(|c| c.samples).collect()
}

fn rms(samples: &[f32]) -> f64 {
    assert!(!samples.is_empty(), "no samples to measure");
    (samples
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

/// The sound need not be on `A1`: a project that leaves `A1` empty and places
/// everything on `A2` is still *one* audio lane, so the mp4 export copies it
/// rather than refusing -- and rather than writing a file with no audio track
/// at all, which is what pinning the copy to `A1` did (verifier's repro: the
/// export succeeded, ffprobe showed video only, and nobody would notice until
/// they played it).
#[test]
fn the_mp4_copy_follows_the_lane_that_holds_the_sound() {
    pin_software();
    let source = asset("test_av.mp4");
    let clip = Clip {
        start: 0,
        in_frame: 0,
        out_frame: TOTAL,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let project = Project::from_parts(
        vec![Source::new(&source, 0)],
        vec![
            (LaneKind::Video, vec![clip]),
            (LaneKind::Audio, Vec::new()),
            (LaneKind::Audio, vec![clip]),
        ],
        vec![],
        vec![],
    )
    .expect("valid parts");
    assert_eq!(
        project.audio_segments_from(0, FPS).len(),
        1,
        "one lane holds sound, so this is not a mix"
    );

    let out = out_path("a2_only", "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&source).unwrap();
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(300)).expect("export the A2-only timeline");

    let written = decoded_audio(&out);
    let played = decoded_audio(&source);
    println!(
        "A2-only export: {} samples rms {:.4}, source {} samples rms {:.4}",
        written.len(),
        rms(&written),
        played.len(),
        rms(&played)
    );
    assert!(rms(&written) > 0.001, "the export has to carry the sound");
    assert!(
        (rms(&written) / rms(&played) - 1.0).abs() < 0.05,
        "the exported audio is not the lane's content"
    );
    std::fs::remove_file(&out).unwrap();
}

/// `A1` and `A2` playing the *same* source range at the same place: over that
/// stretch the mix is that audio twice over, so it measures at exactly double
/// the level the one-lane export of the same timeline has there -- and outside
/// it, at the same level. The comparison is against the same window of the same
/// content, because the fixture's own level moves and comparing two *stretches*
/// would measure the fixture rather than the mixer.
#[test]
fn two_audio_lanes_are_summed() {
    pin_software();
    let one_lane = Project::single(asset("test_av.mp4"), TOTAL);
    let mut project = one_lane.clone();
    let doubled = Clip {
        start: TOP_IN,
        in_frame: TOP_IN,
        out_frame: TOP_OUT,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let a2 = project.add_lane(LaneKind::Audio);
    assert!(project.place(a2, TOP_IN, doubled), "place the second lane");
    let (sources, lanes, eq, color) = project.without_orphan_sources();
    assert_eq!(lanes.len(), 3, "V1, A1 and the new A2");
    let project = Project::from_parts(sources, lanes, eq, color).expect("valid parts");

    let (meta, _) = engine::demux::Demuxer::open(&asset("test_av.mp4")).unwrap();
    let wav = |name: &str, project: Project| {
        let out = out_path(name, "wav");
        let settings = ExportSettings {
            format: Format::Wav,
            ..Default::default()
        };
        let handle = engine::export::start(project, meta, &out, &settings);
        wait(&handle, Duration::from_secs(120)).expect("wav export");
        let mut reader = hound::WavReader::open(&out).expect("reopen the wav");
        let spec = reader.spec();
        let samples: Vec<f64> = reader
            .samples::<i16>()
            .map(|s| f64::from(s.expect("sample")))
            .collect();
        std::fs::remove_file(&out).unwrap();
        (spec, samples)
    };
    let (spec, plain) = wav("one_lane", one_lane);
    let (_, mixed) = wav("mixed", project.clone());
    assert_eq!(plain.len(), mixed.len(), "both are the timeline's length");

    let rms = |samples: &[f64], from: u32, to: u32| {
        let per = f64::from(spec.sample_rate) / FPS * f64::from(spec.channels);
        let (a, b) = (
            (f64::from(from) * per) as usize,
            (f64::from(to) * per) as usize,
        );
        let window = &samples[a..b.min(samples.len())];
        (window.iter().map(|s| s * s).sum::<f64>() / window.len() as f64).sqrt()
    };
    // A margin inside each region: a segment boundary lands on a packet edge,
    // and this measures the middle of a stretch, not the seam.
    let (over_one, over_two) = (
        rms(&plain, TOP_IN + 5, TOP_OUT - 5),
        rms(&mixed, TOP_IN + 5, TOP_OUT - 5),
    );
    let (out_one, out_two) = (
        rms(&plain, TOP_OUT + 5, TOTAL - 5),
        rms(&mixed, TOP_OUT + 5, TOTAL - 5),
    );
    println!("rms under A2: one lane {over_one:.1}, mixed {over_two:.1}");
    println!("rms past A2:  one lane {out_one:.1}, mixed {out_two:.1}");
    assert!(over_one > 100.0, "the fixture has to be audible to measure");
    assert!(
        (over_two / over_one - 2.0).abs() < 0.05,
        "the overlap is {:.3}x one lane, not 2x",
        over_two / over_one
    );
    assert!(
        (out_two / out_one - 1.0).abs() < 0.05,
        "past the second lane the level is back to one lane's ({:.3}x)",
        out_two / out_one
    );

    // ...and the mp4 of the same timeline carries that very mix. It used to be
    // a refusal by name -- the mp4 path *copies* AAC packets and a mix is not a
    // copy -- and a refusal is what a file missing half its sound deserves;
    // this decodes both lanes and encodes the sum instead (`export::copy_audio`
    // routes to `encode_audio`), which is the same mix the WAV above measured.
    let out = out_path("mixed", "mp4");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(180)).expect("an mp4 of two audio lanes");
    let (audio, chunks) = engine::AudioSession::open(&out)
        .expect("reopen the export")
        .expect("it has an audio track");
    let coded: Vec<f64> = chunks
        .into_iter()
        .flat_map(|c| c.samples)
        .map(|s| f64::from(s) * f64::from(i16::MAX))
        .collect();
    let per = f64::from(audio.sample_rate) / FPS * f64::from(audio.channels);
    let window = |from: u32, to: u32| {
        let (a, b) = (
            (f64::from(from) * per) as usize,
            (f64::from(to) * per) as usize,
        );
        let w = &coded[a.min(coded.len())..b.min(coded.len())];
        (w.iter().map(|s| s * s).sum::<f64>() / w.len().max(1) as f64).sqrt()
    };
    let (coded_over, coded_out) = (
        window(TOP_IN + 5, TOP_OUT - 5),
        window(TOP_OUT + 5, TOTAL - 5),
    );
    println!("mp4 rms: under A2 {coded_over:.1}, past it {coded_out:.1}");
    // Against the WAV of the same timeline, not against a figure: one lossy
    // generation moves the level by a fraction of a dB and nothing here is
    // measuring the encoder.
    assert!(
        (coded_over / over_two - 1.0).abs() < 0.1,
        "the mp4's overlap is {coded_over:.1} against the mix's {over_two:.1}"
    );
    assert!(
        (coded_out / out_two - 1.0).abs() < 0.1,
        "past A2 the mp4 is {coded_out:.1} against the mix's {out_two:.1}"
    );
    std::fs::remove_file(&out).unwrap();
}

/// A project with one lane of each kind is what it always was: the composite of
/// a single video lane is that lane, and its audio is one un-mixed stream.
#[test]
fn one_lane_projects_are_untouched() {
    let project = Project::single(asset("test_av.mp4"), TOTAL);
    assert_eq!(
        project.composite_spans_from(0),
        project.spans_from(engine::project::Lane::V1, 0)
    );
    let lists = project.audio_segments_from(0, FPS);
    assert_eq!(lists, vec![project.segments_from(0, FPS)]);
    assert_eq!(project.sources(), [Source::new(asset("test_av.mp4"), 0)]);
}

/// The burden a third lane kind carries: a subtitle lane is a lane like the
/// others everywhere a *lane* is the subject -- it is added, labelled, ordered,
/// snapshotted and removed by one code path -- and every path that is about
/// media refuses it, by the bounds check it already had or by name.
///
/// One test rather than a dozen because the claim is one claim: nothing about
/// a caption was threaded through the media machinery.
#[test]
fn a_subtitle_lane_is_a_peer_and_every_media_path_refuses_it() {
    use engine::project::{Edge, SubClip};

    let mut project = Project::single(asset("test_av.mp4"), TOTAL);
    let subs = engine::subtitle::open(&asset("test_subs.srt"), None);
    assert!(project.add_subtitles(&subs), "the palette takes the track");
    let s1 = project.add_lane(LaneKind::Subtitle);
    assert_eq!(project.lanes(), [Lane::V1, Lane::A1, s1], "a peer in the stack");
    assert_eq!(s1.label(), "S1");

    let caption = SubClip {
        start: 0,
        frames: 60,
        track: 0,
        in_us: 0,
        out_us: 2_000_000,
    };
    project.place_sub(s1, 0, caption).expect("a caption goes down");

    // Nothing about the *picture* or the *mix* learned about it.
    assert_eq!(
        project.composite_spans_from(0),
        project.spans_from(Lane::V1, 0),
        "the composite is still V1 alone: a subtitle lane is not a video lane"
    );
    assert_eq!(project.composite_clip_at(0), Some((Lane::V1, 0)));
    assert_eq!(project.audio_lanes(), [Lane::A1], "and the mix is A1 alone");
    assert_eq!(project.audio_gains().len(), 1);
    assert_eq!(
        project.lane_gains().len(),
        2,
        "a save's gain list holds the two media lanes, in step with its lane list"
    );

    // The clip-shaped calls: the lane holds no `Clip`, so each refuses by the
    // check it always made, and none of them costs an undo step.
    assert!(project.lane(s1).is_empty(), "there is no clip on a subtitle lane");
    assert!(!project.set_lane_gain_db(s1, -6.0), "words have no loudness");
    assert_eq!(project.lane_gain_db(s1), 0.0);
    assert!(!project.set_eq(s1, 0, None), "and no equalizer");
    assert!(!project.set_color(s1, 0, None), "and no colour grade");
    assert!(!project.set_fit(s1, 0, engine::scale::FitPolicy::Fill), "and no fit");
    let err = project
        .set_speed(s1, 0, Speed::from_permille(2000))
        .expect_err("and no speed");
    assert!(err.to_string().contains("there is no clip 0 on S1"), "{err}");
    assert!(!project.trim(s1, 0, Edge::End, 10, &[TOTAL]), "nothing to trim");
    assert!(!project.lift(s1, 0), "nothing to lift");
    assert!(!project.delete_in(s1, 0), "nothing to delete");

    // ...and the three doors that could still have put a picture on one.
    let clip = *project.lane(Lane::V1).first().expect("V1 holds the film");
    assert!(!project.place(s1, 0, clip), "a picture may not be placed on it");
    assert!(!project.place_take(s1, 0, clip), "nor a whole take");
    assert!(!project.move_clip(Lane::V1, 0, s1, 0), "nor dragged onto it");
    project.append_clip(0, 30);
    assert!(
        project.lane(s1).is_empty(),
        "an import lands on the media lanes and skips this one"
    );
    assert!(project.undo(), "the import is one step");
    let refused = Project::from_parts(
        vec![Source::new(asset("test_av.mp4"), 0)],
        vec![(LaneKind::Subtitle, vec![clip])],
        Vec::new(),
        Vec::new(),
    )
    .expect_err("a file may not name one either");
    assert!(
        refused.to_string().contains("S1 is a subtitle track"),
        "the refusal says which lane and why: {refused}"
    );

    // The lane's own life cycle, which is the machinery it *does* use.
    let held = project.remove_lane(s1).expect_err("it still holds a caption");
    assert!(held.to_string().contains("still holds 1 subtitle(s)"), "{held}");
    assert!(project.lift_sub(s1, 0), "the caption comes off");
    project.remove_lane(s1).expect("and then so does the lane");
    assert_eq!(project.lanes(), [Lane::V1, Lane::A1], "the last one may go");
    assert!(project.undo() && project.undo(), "both steps walk back");
    assert_eq!(project.sub_lane(s1), [caption], "caption and lane restored");
}

/// A rippling paste opens its room on the subtitle lanes too -- the desync a
/// lane model exists to prevent: a caption that stayed where it was while the
/// picture under it moved on says the wrong words over the wrong frames.
#[test]
fn a_paste_opens_its_room_under_the_captions_too() {
    use engine::project::SubClip;

    let mut project = Project::single(asset("test_av.mp4"), TOTAL);
    project.add_subtitles(&engine::subtitle::open(&asset("test_subs.srt"), None));
    let s1 = project.add_lane(LaneKind::Subtitle);
    project
        .place_sub(
            s1,
            0,
            SubClip {
                start: 0,
                frames: 60,
                track: 0,
                in_us: 0,
                out_us: 2_000_000,
            },
        )
        .expect("a caption over the first two seconds");

    let clip = *project.lane(Lane::V1).first().expect("V1 holds the film");
    let pasted = Clip {
        out_frame: 30,
        ..clip
    };
    assert!(project.paste(20, pasted), "thirty frames go in at frame 20");
    assert_eq!(
        project
            .sub_lane(s1)
            .iter()
            .map(|s| (s.start, s.frames, s.in_us, s.out_us))
            .collect::<Vec<_>>(),
        [(0, 20, 0, 666_666), (50, 40, 666_666, 2_000_000)],
        "the caption is split at the insert and its tail moved on with the picture"
    );
    assert!(project.undo(), "and the paste is one step for the lot");
    assert_eq!(project.sub_lane(s1).len(), 1);
}
