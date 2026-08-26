//! Standalone audio files as sources: every container we claim to read decodes
//! to the same tone, a song joins a timeline of video on the audio lane, a song
//! *is* a timeline on its own (canvas scaffolded, picture black, sound exported)
//! and the two things it cannot do -- play on the video lane, be copied into an
//! mp4 -- are refusals that say so.
//!
//! ```text
//! cargo test -p engine --release --test audio_files
//! ```

use std::path::{Path, PathBuf};

use engine::export::{ExportSettings, Format};
use engine::project::{Lane, LaneKind};
use engine::scratch::Scratch;
use engine::{AudioSession, Clip, PlaybackSession, Project};

/// The fixtures `scripts/gen_fixtures.sh` writes: 3 s of 440 Hz left / 880 Hz
/// right at 44.1k stereo, under a 1 Hz volume pulse -- the same tone
/// `test_av.mp4` carries, so a song and a video clip share one timeline.
const CONTAINERS: [&str; 6] = [
    "test_tone.mp3",
    "test_tone.wav",
    "test_tone.flac",
    "test_tone.ogg",
    "test_tone.m4a",
    "test_tone.aac",
];

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The two acts a file on the timeline now takes, in the order a user does
/// them: taken into the library (`PlaybackSession::import` places nothing),
/// then dragged onto the end of the timeline.
fn import_and_place(session: &mut PlaybackSession, path: &Path) {
    session.import(path).expect("the file joins this timeline");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, path, 0, None)
            .expect("a file just imported is on this timeline"),
        "the drag onto the end of the timeline"
    );
}

/// The message of a call that had to be refused. Not `expect_err`: that wants
/// `Debug` on the success side, and neither a session nor a packet has it.
fn refusal<T>(result: engine::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e.to_string(),
    }
}

fn open(path: impl AsRef<Path>) -> PlaybackSession {
    let session = PlaybackSession::open(path).expect("open");
    session.set_gain(0.0);
    session
}

/// Every frame the file decodes to, interleaved, plus the meta it declares.
fn decode(path: &Path) -> (u32, u16, Vec<f32>) {
    let (meta, rx) = AudioSession::open(path)
        .expect("open")
        .expect("the fixture has audio");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend(chunk.samples);
    }
    (meta.sample_rate, meta.channels, samples)
}

#[test]
fn every_container_decodes_the_same_three_second_tone() {
    for name in CONTAINERS {
        let (rate, channels, samples) = decode(&asset(name));
        assert_eq!((rate, channels), (44100, 2), "{name}");
        let secs = samples.len() as f64 / f64::from(rate) / f64::from(channels);
        // Lossy formats pad: a codec frame is 1152 (mp3) or 1024 (aac) samples,
        // so a tenth of a second is generous and still catches a decoder that
        // stops early or doubles the stream.
        assert!(
            (secs - 3.0).abs() < 0.1,
            "{name}: {secs:.3} s of audio, want 3.0"
        );
        // The fixture's envelope is 0.5 + 0.5*sin(2*PI*t): full scale at t=0.25
        // of every second, silence at t=0.75. Sample it there rather than
        // trusting a length -- a channel-swapped or mis-scaled decode still has
        // the right number of samples.
        for second in 0..3 {
            let at = |t: f64| {
                let frame = ((f64::from(second) + t) * f64::from(rate)) as usize;
                let block = &samples[frame * usize::from(channels)..][..usize::from(channels) * 64];
                block.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
            };
            // Depth as a ratio, not an absolute: the fixture's sines sit around
            // an eighth of full scale and that is the encoder's business
            // (ledger:873), while a dip 20x below the peak is the pulse.
            let (loud, quiet) = (at(0.25), at(0.75));
            assert!(loud > 0.05, "{name} second {second}: peak only {loud}");
            assert!(
                quiet < 0.05 * loud,
                "{name} second {second}: dip {quiet} against peak {loud}"
            );
        }
    }
}

/// The length a placement is built from, which is the only frame count a file
/// with no picture has.
#[test]
fn duration_comes_from_the_header_or_the_decode() {
    for name in CONTAINERS {
        let secs = AudioSession::duration_secs(asset(name))
            .expect("probe")
            .expect("the fixture has audio");
        assert!(
            (secs - 3.0).abs() < 0.1,
            "{name}: duration reads {secs:.3} s"
        );
    }
    assert_eq!(
        AudioSession::duration_secs(asset("test_baseline.mp4")).expect("probe"),
        None,
        "a silent video has no audio duration"
    );
}

#[test]
fn a_song_joins_a_video_timeline_on_the_audio_lane() {
    let mut session = open(asset("test_av.mp4"));
    let before = session.timeline_duration();
    // The import alone is a library row: the lanes are untouched until it is
    // dragged onto one.
    session
        .import(&asset("test_tone.mp3"))
        .expect("import the song");
    assert_eq!(session.sources().len(), 2, "the library grew");
    assert_eq!(session.lane_clips(Lane::A1).len(), 1, "an import placed it");
    assert_eq!(session.timeline_duration(), before);
    assert_eq!(session.file_frames(&asset("test_tone.mp3")), 90);
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &asset("test_tone.mp3"), 0, None)
            .expect("a song just imported is on this timeline")
    );

    // 3 s at 30 fps, placed at the end: the audio lane runs 90 frames past the
    // video.
    assert!(
        (session.timeline_duration() - (before + 3.0)).abs() < 0.05,
        "{} s of timeline, want {}",
        session.timeline_duration(),
        before + 3.0
    );
    assert_eq!(session.sources().len(), 2);
    // The video lane never hears about it: one clip, the one the file opened
    // with. The audio lane has two.
    assert_eq!(session.lane_clips(Lane::V1).len(), 1, "video lane");
    assert_eq!(session.lane_clips(Lane::A1).len(), 2, "audio lane");
    let song = session.lane_clips(Lane::A1)[1];
    assert_eq!((song.source, song.in_frame, song.len()), (1, 0, 90));

    // And it plays: the segment list past the join names the song, so the
    // worker opens it rather than running silent.
    session.seek(before + 1.0);
    assert!(session.lane_spans_by_source(Lane::A1).len() >= 2);
}

/// A rate one output device cannot have two of is **converted**, not refused:
/// the 48 kHz twin of the tone joins a 44.1 kHz timeline, is placed for the
/// seconds it lasts, and comes out at the pitch it was recorded at.
///
/// The pitch is the whole claim. Reading a 48 kHz file's frames one for one onto
/// a 44.1 kHz timeline plays it 8.8% slow and 440 Hz becomes 404 -- which is
/// what "it imported" would look like from a length check alone.
#[test]
fn a_rate_the_device_cannot_mix_is_converted_at_the_door() {
    let mut session = open(asset("test_av.mp4"));
    let before = session.timeline_duration();
    import_and_place(&mut session, &asset("test_tone_48k.wav"));
    assert_eq!(session.sources().len(), 2, "the 48k file is in the library");
    assert_eq!(session.lane_clips(Lane::A1).len(), 2, "and on the lane");
    // 3 s of tone placed after the video, at the timeline's own rate.
    assert!(
        (session.timeline_duration() - (before + 3.0)).abs() < 0.05,
        "{} s of timeline, want {}",
        session.timeline_duration(),
        before + 3.0
    );

    // ...and the samples themselves, straight out of the worker the session
    // feeds from: one second of the 48k file, resampled onto a 44.1k timeline.
    let sources = [(asset("test_av.mp4"), 0), (asset("test_tone_48k.wav"), 0)];
    let (meta, rx) =
        AudioSession::open_multi_streams(&sources, &[(Some(1), 0.25, 1.25)]).expect("open").expect("the timeline has sound");
    assert_eq!(
        (meta.sample_rate, meta.channels),
        (44100, 2),
        "the timeline's rate, not the file's"
    );
    let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let frames = samples.len() / 2;
    assert!(
        (frames as i64 - 44100).abs() < 64,
        "one second of timeline is {frames} frames of 44100"
    );
    // 440 Hz left, 880 Hz right -- the fixture's own tones, unmoved. Zero
    // crossings date them without an FFT: a file read at 1:1 instead of 48:44.1
    // would come out at 404 and 808, an 8% miss this 2% band rejects.
    for (name, channel, hz) in [("left", 0, 440.0), ("right", 1, 880.0)] {
        let side: Vec<f32> = samples[channel..].iter().step_by(2).copied().collect();
        let secs = side.len() as f64 / 44100.0;
        let crossings = side.windows(2).filter(|p| (p[0] < 0.0) != (p[1] < 0.0)).count() as f64;
        let got = crossings / 2.0 / secs;
        assert!(
            (got - hz).abs() <= 0.02 * hz,
            "{name}: {got:.1} Hz out of a {hz} Hz tone -- the resample moved the pitch"
        );
    }
}

/// A song *is* a timeline: it scaffolds the canvas a video would have defined,
/// its own length is the timeline's, the video lane starts empty and the picture
/// is the black of an uncovered canvas.
#[test]
fn a_song_is_a_timeline_of_its_own() {
    let mut session = open(asset("test_tone.mp3"));
    let meta = *session.meta();
    assert_eq!(
        (meta.width, meta.height),
        (1920, 1080),
        "the default canvas"
    );
    assert_eq!(meta.frame_rate, 30.0);
    // 3 s of tone at 30 fps, rounded up: the only frame count a file with no
    // picture has, and it is the timeline's length too.
    assert_eq!(meta.frame_count, 90);
    assert!((session.timeline_duration() - 3.0).abs() < 0.05);
    assert!(!session.is_empty(), "a song is not an empty timeline");
    assert!(
        session.lane_clips(Lane::V1).is_empty(),
        "no picture anywhere"
    );
    assert_eq!(session.lane_clips(Lane::A1).len(), 1);
    let clip = session.lane_clips(Lane::A1)[0];
    assert_eq!((clip.source, clip.in_frame, clip.len()), (0, 0, 90));
    assert_eq!(session.sources().len(), 1);

    // And it shows black rather than nothing: the whole timeline is a gap, so
    // the black worker feeds it at the canvas size.
    session.seek(1.0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = session.try_frame() {
            break frame;
        }
        assert!(std::time::Instant::now() < deadline, "no frame at 1 s");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!((frame.width, frame.height), (1920, 1080));
    assert_eq!(frame.index, 30, "the gap is indexed in timeline frames");
    assert!(
        frame.bgra.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]),
        "the picture over a song is black"
    );
}

/// A file that is neither a picture nor a sound this engine can play is not a
/// timeline: refused at the door, in the words of whatever refused it.
#[test]
fn a_file_with_no_picture_and_no_sound_is_still_refused() {
    let dir = Scratch::dir("ve_audio_bad");
    let fake = dir.join("not_really.mp3");
    std::fs::write(&fake, b"this is not an mp3").expect("write");
    let e = refusal(PlaybackSession::open(&fake));
    assert!(e.starts_with(&fake.display().to_string()), "{e}");
}

/// The whole audio-only round trip a user makes: open a song, place a second
/// one, save, reopen, export the sound -- and be told, by name, that the one
/// format that needs a picture cannot have one.
#[test]
fn an_audio_only_project_saves_reloads_and_exports_its_sound() {
    let dir = Scratch::dir("ve_audio_only");

    let mut session = open(asset("test_tone.mp3"));
    // A second song joins it, the same way one joins a timeline of video: no
    // picture is needed for the import to have something to hold it to.
    import_and_place(&mut session, &asset("test_tone.flac"));
    assert_eq!(session.lane_clips(Lane::A1).len(), 2);
    assert!(session.lane_clips(Lane::V1).is_empty());
    assert!((session.timeline_duration() - 6.0).abs() < 0.1);

    // mp4 is refused by name, before a byte is written; WAV writes the sound.
    let mp4 = dir.join("song.mp4");
    let e = wait(&session.export_to_with(
        &mp4,
        &ExportSettings {
            format: Format::Mp4,
            ..ExportSettings::default()
        },
    ))
    .expect_err("an audio-only timeline is not an mp4");
    assert_eq!(
        e.to_string(),
        "the timeline has no picture: an mp4 would be black. \
         Export WAV, FLAC or MP3, which are the sound itself"
    );
    assert!(!mp4.exists(), "the refusal wrote a file anyway");

    let wav = dir.join("song.wav");
    wait(&session.export_to_with(
        &wav,
        &ExportSettings {
            format: Format::Wav,
            ..ExportSettings::default()
        },
    ))
    .expect("wav export of an audio-only timeline");
    let (rate, channels, samples) = decode(&wav);
    assert_eq!((rate, channels), (44100, 2));
    let secs = samples.len() as f64 / f64::from(rate) / f64::from(channels);
    assert!((secs - 6.0).abs() < 0.1, "{secs:.3} s of exported audio");
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    assert!(peak > 0.05, "the export is silent: peak {peak}");

    // Saved and reopened: an .edith naming nothing but songs loads.
    let project = dir.join("songs.edith");
    let before = (
        session.lane_spans_by_source(Lane::A1),
        session.sources().to_vec(),
        session.timeline_duration(),
    );
    session.save_project(&project).expect("save");
    drop(session);
    let reloaded = PlaybackSession::open_project(&project).expect("reload an audio-only project");
    reloaded.set_gain(0.0);
    assert_eq!(
        (
            reloaded.lane_spans_by_source(Lane::A1),
            reloaded.sources().to_vec(),
            reloaded.timeline_duration()
        ),
        before
    );
    assert_eq!(reloaded.meta().frame_rate, 30.0, "the canvas came back");
    assert_eq!(
        (reloaded.meta().width, reloaded.meta().height),
        (1920, 1080)
    );
    assert!(reloaded.lane_clips(Lane::V1).is_empty());
    drop(reloaded);
    let _ = std::fs::remove_dir_all(&dir);
}

/// DEBT #45: the device the timeline plays through used to be opened once, on
/// the first non-image source ([`audio_source_of`] in `playback.rs`) -- with
/// no check that the source it landed on has a usable audio track at all. A
/// timeline whose *first* source is silent (`test_baseline.mp4` has no audio
/// track) therefore never reopened on a later source that does, and stayed
/// silent for the whole session however many other sources had sound.
///
/// No PipeWire daemon lives in this sandbox (`AoSession::probe()` is false
/// here), so the device itself cannot be tapped for samples -- this reaches
/// past it instead, onto the one signal `open_audio` still hands back with no
/// device at all: [`PlaybackSession::audio_disabled_reason`]. `test_dts.mkv`'s
/// track has a header (so it is chosen over a source with none) but no
/// decoder (so opening it, unlike the silent baseline, is a reason worth
/// giving) -- the old code never got there, and reports nothing.
#[test]
fn a_silent_first_source_does_not_stop_the_scan_for_one_with_sound() {
    let dir = Scratch::dir("ve_silent_first_source");
    let path0 = asset("test_baseline.mp4");
    let path1 = asset("test_dts.mkv");
    let (meta0, _) = engine::demux::Demuxer::open(&path0).expect("open the baseline fixture");
    let (meta1, _) = engine::demux::Demuxer::open(&path1).expect("open the dts fixture");

    // `Project`, not `PlaybackSession::import`: the session-level import gate
    // would itself refuse a real track landing on a silent timeline
    // (`audio_matches_probed`, a different corner of the same debt) before
    // this test ever reaches `open_project`.
    let mut project = Project::single(&path0, meta0.frame_count);
    let source1 = project.import(&path1, 0);
    let lane2 = project.add_lane(LaneKind::Video);
    let clip = Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start: 0,
        in_frame: 0,
        out_frame: meta1.frame_count,
        source: source1,
        link: None,
        eq: None,
        color: None,
        fit: Default::default(),
        speed: engine::Speed::NORMAL,
        transform: None,
    };
    assert!(project.place(lane2, 0, clip), "the second source lands");

    let (sources, lanes, eq, color, transform) = project.without_orphan_sources();
    let file = dir.join("silent_first.edith");
    engine::edith::save(
        &file,
        &sources,
        &lanes,
        &project.lane_gains(),
        &project.lane_subs(),
        project.subtitles(),
        &eq,
        &color,
        &transform,
        (meta0.width, meta0.height),
        None,
        project.tone(),
        false,
        true,
        engine::export::EncoderSeat::default(),
        project.limiter(),
        None,
        0,
    )
    .expect("save");

    let reloaded = PlaybackSession::open_project(&file).expect("reopen");
    assert!(
        reloaded
            .audio_disabled_reason()
            .is_some_and(|r| r.contains("A_DTS")),
        "the scan for a source with sound never reached test_dts.mkv: {:?}",
        reloaded.audio_disabled_reason()
    );
    drop(reloaded);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A *song* as the first import into an empty window: `open_library` takes the
/// audio-only fork of `open` (there is no picture to scaffold a canvas from) and
/// still leaves the timeline empty. The row is there at the song's own length,
/// the canvas and the clock a song would have set are set, and the drag that
/// follows is what makes it play.
#[test]
fn the_first_import_of_a_song_opens_a_library_over_an_empty_timeline() {
    let mp3 = asset("test_tone.mp3");
    let mut session = PlaybackSession::open_library(&mp3).expect("open into the library");
    session.set_gain(0.0);
    assert!(session.is_empty(), "the timeline must start empty");
    assert_eq!(session.timeline_duration(), 0.0);
    assert!(session.lane_clips(Lane::A1).is_empty(), "nothing placed");
    assert!(session.lane_clips(Lane::V1).is_empty(), "no picture either");
    // The canvas a song scaffolds, and the clock that goes with it -- a session
    // with an empty timeline is still this file's session.
    let meta = *session.meta();
    assert_eq!((meta.width, meta.height), (1920, 1080), "the song canvas");
    assert_eq!(meta.frame_rate, 30.0);
    assert_eq!(session.sources().len(), 1, "the library row");
    assert_eq!(session.file_frames(&mp3), 90, "3 s at 30 fps, never placed");
    assert!(!session.undo(), "opening a library is not an undo step");
    // A file this session never took in has no length and cannot be placed.
    assert_eq!(session.file_frames(&asset("test_tone.flac")), 0);
    assert_eq!(
        refusal(session.place_stream_at(0.0, &asset("test_tone.flac"), 0, None)),
        format!(
            "{} is not on this timeline",
            asset("test_tone.flac").display()
        )
    );

    // The drag: onto the audio lane and nowhere else, at the song's own length.
    assert!(
        session
            .place_stream_at(0.0, &mp3, 0, None)
            .expect("its own file is on this timeline")
    );
    assert!(!session.is_empty());
    assert!((session.timeline_duration() - 3.0).abs() < 0.05);
    assert_eq!(session.lane_clips(Lane::A1).len(), 1);
    assert!(session.lane_clips(Lane::V1).is_empty(), "still no picture");
    let clip = session.lane_clips(Lane::A1)[0];
    assert_eq!((clip.source, clip.in_frame, clip.len()), (0, 0, 90));
    // And it plays: the segment list names the song, so the worker opens it
    // rather than running silent.
    session.seek(1.0);
    assert!(!session.lane_spans_by_source(Lane::A1).is_empty());
    // One `z` takes the drag back to the empty timeline, row and all.
    assert!(session.undo(), "undo the drag");
    assert!(session.is_empty());
    assert_eq!(session.sources().len(), 1, "the row survives the undo");
}

/// The other half of the door: a *video* joining a timeline a song scaffolded.
/// It is held to the canvas the song set (H.264, 30 fps), so a matching file
/// brings its picture in -- and the timeline stops being audio-only, mp4 export
/// included.
#[test]
fn a_video_joins_a_timeline_a_song_started() {
    let mut session = open(asset("test_tone.mp3"));
    import_and_place(&mut session, &asset("test_av.mp4"));
    assert_eq!(session.lane_clips(Lane::V1).len(), 1, "a picture at last");
    assert_eq!(session.lane_clips(Lane::A1).len(), 2);
    // A sample rate the device cannot have two of is no refusal either: it is
    // resampled onto the timeline's, as a frame rate is conformed to the canvas.
    session
        .import(&asset("test_tone_48k.wav"))
        .expect("48 kHz joins a 44.1 kHz timeline");
    assert_eq!(session.sources().len(), 3);
    // ...and a *frame* rate of its own is not a refusal either: the canvas a song
    // scaffolds runs at 30 fps and a 25 fps picture joins it at the length it
    // lasts in seconds (50 frames of file, 60 of timeline).
    session
        .import(&asset("test_25fps.mp4"))
        .expect("25 fps joins the 30 fps canvas a song scaffolded");
    assert_eq!(session.file_frames(&asset("test_25fps.mp4")), 60);
}

/// Blocks until the export settles, whichever way it settled.
fn wait(handle: &engine::ExportHandle) -> engine::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if let Some(result) = handle.result() {
            return result;
        }
        assert!(std::time::Instant::now() < deadline, "export never settled");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The *copy* path names what it cannot copy, by file and by format. That is a
/// fallback's reason rather than a user's refusal now -- `export::copy_audio`
/// takes this `Err` as "not copyable" and decodes and re-encodes the timeline
/// instead (`tests/audio_export.rs` exports exactly this shape as an mp4 with
/// sound in it) -- but the words are what a caller with no encoder shows, so
/// they are still asserted here.
#[test]
fn an_export_refuses_audio_it_cannot_copy() {
    let sources = vec![asset("test_av.mp4"), asset("test_tone.mp3")];
    let segs = [(Some(0), 0.0, 1.0), (Some(1), 0.0, 1.0)];
    assert_eq!(
        refusal(AudioSession::copy_multi_segments(&sources, &segs)),
        "a packet copy needs AAC in an mp4; test_tone.mp3 is mp3"
    );
    // A timeline of nothing but mp4 still exports, which is the invariant this
    // refusal must not have cost.
    assert!(
        AudioSession::copy_multi_segments(&sources[..1], &segs[..1])
            .expect("mp4 audio still copies")
            .is_some()
    );
}

#[test]
fn a_song_survives_a_save_and_a_reload() {
    let dir = Scratch::dir("ve_audio_files");
    let copy = |name: &str| {
        let to = dir.join(name);
        std::fs::copy(asset(name), &to).expect("copy the fixture");
        to
    };

    let mut session = open(copy("test_av.mp4"));
    import_and_place(&mut session, &copy("test_tone.flac"));
    // And one placed at the playhead, the way a library row dropped on the
    // audio lane arrives -- so the reload has both doors to restore.
    let song = session.lane_clips(Lane::A1)[1];
    assert!(session.place_at(
        Lane::A1,
        1.0,
        Clip {
            fade_in: 0,
            fade_out: 0,
            in_frame: 0,
            out_frame: 30,
            ..song
        }
    ));
    let before = (
        session.lane_spans_by_source(Lane::A1),
        session.sources().to_vec(),
        session.timeline_duration(),
    );

    let project = dir.join("song.edith");
    session.save_project(&project).expect("save");
    drop(session);
    let reloaded = PlaybackSession::open_project(&project).expect("reload");
    reloaded.set_gain(0.0);
    assert_eq!(
        (
            reloaded.lane_spans_by_source(Lane::A1),
            reloaded.sources().to_vec(),
            reloaded.timeline_duration()
        ),
        before
    );

    // The one thing only a hand-edited project can ask for: a file with no
    // picture on the video lane. Written by hand, because no door in the engine
    // will produce it.
    let broken = dir.join("broken.edith");
    std::fs::write(
        &broken,
        "edith 2\nplayhead 0\nsource test_av.mp4\nsource test_tone.flac\n\
         video 0 0 90 1 -\naudio 0 0 90 1 -\n",
    )
    .expect("write");
    let e = refusal(PlaybackSession::open_project(&broken));
    assert!(
        e.ends_with("has no picture: it can only play on an audio lane"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ...and the door the editor itself had onto that same broken project: a copy
/// of the song pasted from the clipboard used to land on `V1` as well as `A1`,
/// where it decodes to nothing -- and the save it wrote was refused on the way
/// back in. The paste is the plain UI path, so this is data loss with no
/// hand-editing anywhere in it.
#[test]
fn a_pasted_song_never_lands_on_the_video_lane() {
    let dir = Scratch::dir("ve_audio_paste");
    let copy = |name: &str| {
        let to = dir.join(name);
        std::fs::copy(asset(name), &to).expect("copy the fixture");
        to
    };

    let mut session = open(copy("test_av.mp4"));
    import_and_place(&mut session, &copy("test_tone.flac"));
    let song = session.lane_clips(Lane::A1)[1];
    let video_before = session.lane_clips(Lane::V1).to_vec();
    assert!(session.paste_at(0.0, song), "paste the copied song");

    // The video lane got room and nothing else: the same clips, pushed along.
    let video_after = session.lane_clips(Lane::V1).to_vec();
    assert_eq!(video_after.len(), video_before.len(), "no clip on V1");
    for (after, before) in video_after.iter().zip(&video_before) {
        assert_eq!(after.source, before.source);
        assert_eq!(after.start, before.start + song.len());
    }
    // The audio lane is the one that gained it, at the playhead.
    assert_eq!(session.lane_clips(Lane::A1).len(), 3);
    assert_eq!(session.lane_clips(Lane::A1)[0].source, song.source);

    // And the save that follows reopens -- which is what the video-lane clip
    // used to cost ("file contains a box with a larger size than it").
    let project = dir.join("pasted.edith");
    session.save_project(&project).expect("save");
    drop(session);
    let reloaded = PlaybackSession::open_project(&project).expect("the save reopens");
    reloaded.set_gain(0.0);
    assert_eq!(reloaded.lane_clips(Lane::A1).len(), 3);
    assert_eq!(reloaded.lane_clips(Lane::V1).len(), video_after.len());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A song muxed into an `.mp4` -- the extension names a film, the header names
/// no picture. `open` falls to the audio-only door on the demuxer's own
/// `NoVideoTrack` rather than refusing "no H.264, HEVC, VP9 or AV1 video track
/// in file", exactly as it would have if the file had been named `.mp3`.
#[test]
fn an_audio_only_file_wearing_an_mp4_extension_still_opens() {
    let session = open(asset("test_audio_only.mp4"));
    assert!(session.lane_clips(Lane::V1).is_empty(), "no picture anywhere");
    assert_eq!(session.lane_clips(Lane::A1).len(), 1);
    assert!((session.timeline_duration() - 3.0).abs() < 0.1);
}

/// The same fallback in [`PlaybackSession::open_project`]'s scaffold branch: a
/// project whose first source is one of these files reloads instead of failing
/// the open a save never had trouble with the first time.
#[test]
fn a_project_scaffolded_from_an_audio_only_mp4_reloads() {
    let dir = Scratch::dir("ve_audio_only_mp4");
    let mp4 = asset("test_audio_only.mp4");
    let session = open(&mp4);
    let project = dir.join("song_mp4.edith");
    session.save_project(&project).expect("save");
    drop(session);
    let reloaded =
        PlaybackSession::open_project(&project).expect("reload a project scaffolded from it");
    reloaded.set_gain(0.0);
    assert!(reloaded.lane_clips(Lane::V1).is_empty());
    assert_eq!(reloaded.lane_clips(Lane::A1).len(), 1);
    assert!((reloaded.timeline_duration() - 3.0).abs() < 0.1);
    drop(reloaded);
    let _ = std::fs::remove_dir_all(&dir);
}
