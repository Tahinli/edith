//! Standalone audio files as sources: every container we claim to read decodes
//! to the same tone, a song joins a timeline of video on the audio lane, and
//! the three things it cannot do -- start a timeline, play on the video lane,
//! be copied into an mp4 -- are refusals that say so.
//!
//! ```text
//! cargo test -p engine --release --test audio_files
//! ```

use std::path::{Path, PathBuf};

use engine::project::Lane;
use engine::{AudioSession, Clip, PlaybackSession};

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
    session
        .import(&asset("test_tone.mp3"))
        .expect("import the song");

    // 3 s at 30 fps, appended: the audio lane runs 90 frames past the video.
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

#[test]
fn a_rate_the_device_cannot_mix_is_refused_at_the_door() {
    let mut session = open(asset("test_av.mp4"));
    assert_eq!(
        refusal(session.import(&asset("test_tone_48k.wav"))),
        "audio 48000 Hz 2 ch does not match the timeline's 44100 Hz 2 ch"
    );
    // Refused means unchanged: no source, no clip, no length.
    assert_eq!(session.sources().len(), 1);
    assert_eq!(session.lane_clips(Lane::A1).len(), 1);
}

#[test]
fn a_song_cannot_be_a_timeline_of_its_own() {
    let e = refusal(PlaybackSession::open(asset("test_tone.mp3")));
    assert!(
        e.ends_with("has no picture: open a video first, then import it onto the audio lane"),
        "{e}"
    );
}

/// The export copies AAC packets and has no encoder, so a timeline carrying an
/// mp3 cannot become an mp4 -- named, with its format, rather than exported
/// silent or exported wrong.
#[test]
fn an_export_refuses_audio_it_cannot_copy() {
    let sources = vec![asset("test_av.mp4"), asset("test_tone.mp3")];
    let segs = [(Some(0), 0.0, 1.0), (Some(1), 0.0, 1.0)];
    assert_eq!(
        refusal(AudioSession::copy_multi_segments(&sources, &segs)),
        "export needs AAC audio today; test_tone.mp3 is mp3"
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
    let dir = std::env::temp_dir().join(format!("ve_audio_files_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let copy = |name: &str| {
        let to = dir.join(name);
        std::fs::copy(asset(name), &to).expect("copy the fixture");
        to
    };

    let mut session = open(copy("test_av.mp4"));
    session.import(&copy("test_tone.flac")).expect("import");
    // And one placed at the playhead, the way a library row dropped on the
    // audio lane arrives -- so the reload has both doors to restore.
    let song = session.lane_clips(Lane::A1)[1];
    assert!(session.place_at(
        Lane::A1,
        1.0,
        Clip {
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
    let dir = std::env::temp_dir().join(format!("ve_audio_paste_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let copy = |name: &str| {
        let to = dir.join(name);
        std::fs::copy(asset(name), &to).expect("copy the fixture");
        to
    };

    let mut session = open(copy("test_av.mp4"));
    session.import(&copy("test_tone.flac")).expect("import");
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
