//! Still images as clips: imported into the library, placed on a video lane,
//! shown at the project's resolution, trimmed against the length a still is
//! held to, saved, reopened and exported.
//!
//! Every picture assertion is against the *file's own pixels* (read here with
//! the `image` crate) rather than against a constant, so a channel swap, a
//! vertical flip and a wrong colour matrix all fail rather than agreeing with
//! a hard-coded red.
//!
//! ```text
//! cargo test -p engine --test images -- --test-threads=1 --nocapture
//! ```

use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::{Edge, Lane, LaneKind};
use engine::scratch::Scratch;
use engine::{DecodeSession, ExportHandle, PlaybackSession};

/// test_av.mp4's rate, which every fixture here shares.
const FPS: f64 = 30.0;
/// [`playback::IMAGE_MAX_SECS`] at that rate: ten minutes.
const CAP: u32 = 18_000;
/// What a placed still is five seconds of.
const PLACED: u32 = 150;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Software everywhere, so the suite proves the path every machine has.
fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
}

/// The fixture's own pixel at `(x, y)` as RGB -- what the timeline has to show
/// there, whatever it scaled the picture to on the way.
fn source_rgb(x: u32, y: u32) -> [u8; 3] {
    let img = image::open(asset("test_still.png"))
        .expect("the still fixture (scripts/gen_fixtures.sh)")
        .to_rgb8();
    img.get_pixel(x, y).0
}

/// One BGRA pixel of a frame as RGB, so both sides of a comparison read alike.
fn frame_rgb(bgra: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * width + x) * 4) as usize;
    [bgra[at + 2], bgra[at + 1], bgra[at]]
}

/// BT.601 is lossy in both directions and the chroma is subsampled, so a
/// colour survives to within a few counts rather than exactly.
fn close(got: [u8; 3], want: [u8; 3], what: &str) {
    let off = got
        .iter()
        .zip(&want)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(off <= 6, "{what}: showed {got:?}, the file has {want:?}");
}

/// The frame the playhead is on, decoded: a paused seek still decodes, so this
/// is what the viewer would be showing.
fn frame_at(session: &mut PlaybackSession, secs: f64) -> engine::Frame {
    session.seek(secs);
    let target = (secs * FPS) as u32;
    let started = Instant::now();
    loop {
        if let Some(frame) = session.try_frame()
            && frame.index >= target
        {
            return frame;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no frame at {secs} s (timeline frame {target})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// An import is a library entry and nothing else -- the same promise every
/// other source makes, kept by the one with no length of its own.
#[test]
fn importing_a_still_fills_the_library_and_places_nothing() {
    pin_software();
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let before = session.clip_spans();

    let source = session.import(&asset("test_still.png")).expect("a picture");
    assert_eq!(source, 1, "a second source");
    assert_eq!(session.clip_spans(), before, "nothing was placed");
    // Ten minutes: the length a still is *held* to, which is what a trim is
    // walled by and what a reload recomputes.
    assert_eq!(session.file_frames(&asset("test_still.png")), CAP);

    // A file that is not a picture at all is refused at the same door, by name.
    let broken = Scratch::file("ve_not_a_png", "png");
    std::fs::write(&broken, b"this is not a PNG").expect("write the decoy");
    let refused = session.import(&broken).expect_err("not a picture");
    assert!(
        refused.to_string().contains("not_a_png"),
        "the refusal names the file: {refused}"
    );
    assert_eq!(session.sources().len(), 2, "a refusal registers nothing");
}

/// The picture itself: placed on a canvas twice its size, the still comes back
/// scaled to that canvas with its own colours, the right way up.
#[test]
fn a_placed_still_shows_its_own_pixels_on_the_canvas() {
    pin_software();
    let still = asset("test_still.png");
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    session.import(&still).expect("a picture");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &still, 0, Some(Lane::V1))
            .expect("a still joins any timeline")
    );

    // Placed for five seconds on the video lane, and on no other: a still is
    // silent, so nothing of it belongs on an audio lane.
    let placed = *session.lane_clips(Lane::V1).last().expect("the still clip");
    assert_eq!(placed.len(), PLACED, "five seconds at 30 fps");
    assert_eq!(
        session.lane_clips(Lane::A1).len(),
        1,
        "no audio clip for it"
    );

    // Half a second into the still, and the picture is the file's: the canvas
    // is 1280x720 and the image 640x360, so this is also the scaler's answer.
    let frame = frame_at(&mut session, end + 0.5);
    assert_eq!((frame.width, frame.height), session.resolution());
    let (w, h) = (frame.width, frame.height);
    close(
        frame_rgb(&frame.bgra, w, w / 2, h / 4),
        source_rgb(320, 90),
        "the top band",
    );
    close(
        frame_rgb(&frame.bgra, w, w / 2, h * 3 / 4),
        source_rgb(320, 270),
        "the bottom band",
    );

    // ...and the frame before the cut is still the video, which is what makes
    // this a cut rather than a still that swallowed the timeline.
    let video = frame_at(&mut session, end - 1.0 / FPS);
    assert_ne!(
        frame_rgb(&video.bgra, w, w / 2, h / 4),
        frame_rgb(&frame.bgra, w, w / 2, h / 4),
        "the video frame before the cut is not the still"
    );
}

/// A still has no length, so the wall is the one the engine gives it: a trim
/// may drag the tail out to ten minutes and not one frame further.
#[test]
fn a_still_trims_out_to_its_cap_and_no_further() {
    pin_software();
    let still = asset("test_still.png");
    let mut session = PlaybackSession::open(&still).expect("open the still itself");
    // Opened on its own: its own picture is the canvas, and the clip is the
    // same five seconds a placement makes.
    assert_eq!(session.resolution(), (640, 360));
    assert_eq!(session.lane_clips(Lane::V1)[0].len(), PLACED);
    assert!(session.lane_clips(Lane::A1).is_empty(), "silent");

    let clip = session.lane_clips(Lane::V1)[0];
    assert_eq!(
        session.trim_room(Lane::V1, 0, Edge::End),
        Some((clip.start + 1, clip.start + CAP)),
        "the tail may go out to the cap"
    );
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, clip.start + CAP));
    assert_eq!(session.lane_clips(Lane::V1)[0].len(), CAP);
    assert!(
        !session.trim_clip(Lane::V1, 0, Edge::End, clip.start + CAP + 1),
        "past the cap is refused"
    );
    // ...and back in, which is the half a five-second title card actually uses.
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, clip.start + 60));
    assert_eq!(session.lane_clips(Lane::V1)[0].len(), 60);

    // The *head* is the same story from the other side: a still has no first
    // frame to walk an in-point back to, so its left edge stretches out to the
    // same cap. Pulled in first, since a clip at frame 0 has nowhere to go.
    assert!(session.trim_clip(Lane::V1, 0, Edge::Start, 20));
    let clip = session.lane_clips(Lane::V1)[0];
    assert_eq!((clip.start, clip.len()), (20, 40));
    assert_eq!(
        session.trim_room(Lane::V1, 0, Edge::Start),
        Some((0, clip.end() - 1)),
        "out to frame 0: the cap is further off than the timeline reaches"
    );
    assert!(session.trim_clip(Lane::V1, 0, Edge::Start, 0), "dragged out");
    let clip = session.lane_clips(Lane::V1)[0];
    assert_eq!(
        (clip.start, clip.in_frame, clip.len()),
        (0, 0, 60),
        "longer by what the head took, and still read from the first frame"
    );
}

/// The *clipboard* door, which is the second way a clip picks its lanes and
/// the one that does not go through `place_stream_at`: a copied still pasted at
/// the playhead is a grouped take everywhere else, and a take of a picture with
/// no sound is one lane. Pasted onto `A1` as well it would be a PNG the audio
/// worker demuxes -- which silences the whole session, not just that clip --
/// and a save the engine's own loader then refuses.
///
/// Both consequences are asserted, not just the lane count: the failure this
/// guards against was invisible in the clip list.
#[test]
fn pasting_a_copied_still_never_reaches_the_audio_lane() {
    pin_software();
    let dir = Scratch::dir("ve_images_paste");
    let media = dir.join("test_av.mp4");
    let still = dir.join("test_still.png");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the video");
    std::fs::copy(asset("test_still.png"), &still).expect("copy the still");

    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    session.import(&still).expect("a picture");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &still, 0, Some(Lane::V1))
            .expect("a still joins any timeline")
    );
    let audio_clips = session.lane_clips(Lane::A1).len();

    // Copy the still clip and paste it at the playhead, exactly as the app's
    // ctrl+c / ctrl+v pair does (`Player::copy` takes `clip_at`, `Player::paste`
    // hands it to `paste_at`).
    let last = session.lane_clips(Lane::V1).len() - 1;
    let copied = session.clip_at(last).expect("the clipboard sees the still");
    session.seek(0.0);
    assert!(session.paste_at(0.0, copied), "the paste takes");

    assert_eq!(
        session.lane_clips(Lane::A1).len(),
        audio_clips,
        "a still is silent: the audio lane gains nothing from a paste"
    );
    assert!(
        session
            .lane_clips(Lane::A1)
            .iter()
            .all(|c| !engine::is_image(&session.sources()[c.source].path)),
        "no audio clip may play from a picture"
    );
    // The sound itself, through the one public door that reads the same play
    // list playback feeds from (`audio_segments_from` ->
    // `open_mixed_streams_eq`): a WAV export. With a PNG on the audio lane this
    // is the demux error that silences the session, not a file.
    let wav = dir.join("pasted.wav");
    let settings = ExportSettings {
        format: Format::Wav,
        ..ExportSettings::default()
    };
    wait(
        &session.export_to_with(&wav, &settings),
        Duration::from_secs(120),
    )
    .expect("the timeline's sound is still there");
    assert!(
        std::fs::metadata(&wav).expect("the wav exists").len() > 1_000,
        "a silent-by-failure export would be the header alone"
    );
    // ...and the save the engine writes is one the engine can open.
    let project = dir.join("pasted.edith");
    session.save_project(&project).expect("save");
    PlaybackSession::open_project(&project).expect("the engine's own save reopens");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Saved and reopened: the clip that comes back plays the same picture for the
/// same frames. The reload recomputes the still's length, so a clip trimmed out
/// to the cap has to still be inside it -- `open_project` refuses one that is
/// not, by name.
#[test]
fn a_still_survives_the_project_round_trip() {
    pin_software();
    let dir = Scratch::dir("ve_images");
    let media = dir.join("test_av.mp4");
    let still = dir.join("test_still.png");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the video");
    std::fs::copy(asset("test_still.png"), &still).expect("copy the still");

    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    session.import(&still).expect("a picture");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &still, 0, Some(Lane::V1))
            .expect("a still joins any timeline")
    );
    // Trimmed well past the five seconds it went down as, so the reload's own
    // length check has something to be wrong about.
    let clip = *session.lane_clips(Lane::V1).last().expect("the still clip");
    let idx = session.lane_clips(Lane::V1).len() - 1;
    assert!(session.trim_clip(Lane::V1, idx, Edge::End, clip.start + 900));
    let before: Vec<_> = session.lane_clips(Lane::V1).to_vec();

    let project = dir.join("stills.edith");
    session.save_project(&project).expect("save");
    let reopened = PlaybackSession::open_project(&project).expect("reopen");
    assert_eq!(reopened.lane_clips(Lane::V1), &before[..]);
    assert_eq!(reopened.file_frames(&still), CAP, "the cap came back");

    // The one thing a hand-written project could say that no edit can: a still
    // on an audio lane, which the audio worker would open a PNG for.
    let text = std::fs::read_to_string(&project).expect("read the project");
    let hacked = text.replace("V1", "A2");
    let bad = dir.join("hacked.edith");
    std::fs::write(&bad, &hacked).expect("write the hacked project");
    if hacked != text {
        let Err(refused) = PlaybackSession::open_project(&bad) else {
            panic!("a still cannot be heard: the audio lane took it");
        };
        assert!(
            refused.to_string().contains("still image"),
            "the refusal says what it is: {refused}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The save renumbers sources by first use, so a still at the head of `V1`
/// comes back as **source 0** however the session was scaffolded -- and source
/// 0 is what everything used to read the timeline off. A picture defines no
/// frame rate and has no sound, so the reload used to read this project as 30
/// fps (refusing the 25 fps footage still on `A1` by name) and open the output
/// device on a PNG (a timeline that plays silent although its clips have
/// sound). Nothing may assume what source 0 *is*: the rate comes from the first
/// source that has one, and the sound from the first that could have any.
#[test]
fn a_still_at_saved_source_zero_keeps_the_rate_and_the_sound() {
    pin_software();
    let dir = Scratch::dir("ve_still_first");
    let media = dir.join("test_25fps.mp4");
    let still = dir.join("test_still.png");
    std::fs::copy(asset("test_25fps.mp4"), &media).expect("copy the video");
    std::fs::copy(asset("test_still.png"), &still).expect("copy the still");

    // 25 fps, which no still is ever given: the rate alone says which source
    // the reload scaffolded from.
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    assert!((session.meta().frame_rate - 25.0).abs() < 0.01);
    session.import(&still).expect("a picture");
    // Over the head of V1, where it takes the whole take's picture: the sound
    // stays on A1, so the still is the first clip in save order and the video
    // is a source only the audio lane names.
    assert!(
        session
            .place_stream_at(0.0, &still, 0, Some(Lane::V1))
            .expect("a still joins any timeline")
    );
    assert!(session.lane_clips(Lane::V1).len() == 1 && session.lane_clips(Lane::A1).len() == 1);

    let project = dir.join("still_first.edith");
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).expect("read the project");
    let source_0 = text
        .lines()
        .find(|l| l.starts_with("source "))
        .expect("a source line");
    assert!(
        source_0.contains("test_still.png"),
        "the premise: the save put the still at index 0 -- {source_0}"
    );

    let reopened = PlaybackSession::open_project(&project).expect("a saved project always reopens");
    assert!(
        (reopened.meta().frame_rate - 25.0).abs() < 0.01,
        "the footage's rate, not the still's 30: {}",
        reopened.meta().frame_rate
    );
    // The same sound, at the same place, playing the same file -- by a *new*
    // index, which is the renumbering this whole test is about.
    let (sound, was) = (
        reopened.lane_clips(Lane::A1)[0],
        session.lane_clips(Lane::A1)[0],
    );
    assert_eq!(
        (sound.start, sound.in_frame, sound.out_frame),
        (was.start, was.in_frame, was.out_frame)
    );
    assert!(
        reopened.sources()[sound.source]
            .path
            .ends_with("test_25fps.mp4"),
        "the take's sound still names the video"
    );
    assert!(
        reopened.audio_disabled_reason().is_none(),
        "a still is not a reason a timeline is silent: {:?}",
        reopened.audio_disabled_reason()
    );
    // And the sound itself, through the very opener a seek builds its worker
    // with: source 0 is the PNG here, and one opened unconditionally is an
    // error that takes every lane's audio down with it.
    let sources: Vec<_> = reopened
        .sources()
        .iter()
        .map(|s| (s.path.clone(), s.audio_stream))
        .collect();
    let (meta, _rx) = engine::AudioSession::open_multi_streams(&sources, &[(Some(1), 0.0, 1.0)])
        .expect("the worker opens over a still at index 0")
        .expect("the take on A1 has sound");
    assert_eq!(meta.sample_rate, 44_100, "the take's own rate");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The export is what was watched: a timeline of video then still comes back
/// out of the mp4 as video then still, with the still's own colours in it.
#[test]
fn a_mixed_timeline_exports_the_still_at_the_cut() {
    pin_software();
    let still = asset("test_still.png");
    // The silent 640x360 fixture: no AAC track to copy and no scaling to do,
    // so what this measures is the still in the encoder and nothing else.
    let mut session = PlaybackSession::open(asset("test_mismatch.mp4")).expect("open the fixture");
    session.import(&still).expect("a picture");
    // One second of video, then one second of still: 60 frames to encode.
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, 30));
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &still, 0, Some(Lane::V1))
            .expect("a still joins any timeline")
    );
    assert!(session.trim_clip(Lane::V1, 1, Edge::End, 60));
    assert_eq!(session.lane_clips(Lane::V1).len(), 2);
    assert_eq!(session.resolution(), (640, 360));

    let out = Scratch::file("ve_images_export", "mp4");
    wait(&session.export_to(&out), Duration::from_secs(300)).expect("export");

    let (meta, frames) = DecodeSession::open(&out).expect("reopen the export");
    assert_eq!((meta.width, meta.height), (640, 360));
    let decoded: Vec<_> = frames.into_iter().collect();
    assert_eq!(decoded.len(), 60, "one second of video, one of still");
    // Well inside the still half, so no GOP smear from the cut reaches it.
    let frame = &decoded[50];
    close(
        frame_rgb(&frame.bgra, 640, 320, 90),
        source_rgb(320, 90),
        "the exported top band",
    );
    close(
        frame_rgb(&frame.bgra, 640, 320, 270),
        source_rgb(320, 270),
        "the exported bottom band",
    );
    // ...and the video half is still the video: the cut is in the file.
    assert_ne!(
        frame_rgb(&decoded[10].bgra, 640, 320, 90),
        frame_rgb(&frame.bgra, 640, 320, 90),
        "the video half is not the still"
    );
    let _ = std::fs::remove_file(&out);
}

/// Which lane a still may land on is the engine's answer, not a caller's: a
/// picture asked for on an audio lane goes on the video one instead, exactly as
/// a song asked for on a video lane goes on `A1`.
#[test]
fn a_still_asked_for_on_an_audio_lane_lands_on_the_video_one() {
    pin_software();
    let still = asset("test_still.png");
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    session.import(&still).expect("a picture");
    let a2 = session.add_lane(LaneKind::Audio);
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &still, 0, Some(a2))
            .expect("a still joins any timeline")
    );
    assert!(session.lane_clips(a2).is_empty(), "not on the audio lane");
    assert_eq!(
        session.lane_clips(Lane::V1).len(),
        2,
        "on the video lane instead"
    );
}
