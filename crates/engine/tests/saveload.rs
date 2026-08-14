//! Saving a project and opening it again: the timeline that comes back is the
//! one that went in, a folder of media plus its `.edith` can be moved
//! anywhere, and every way the files on disk can have changed underneath is a
//! refusal that says which file and why.
//!
//! ```text
//! cargo test -p engine --release --test saveload -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::project::{Lane, Source};
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// A fresh directory to litter, deleted when the test that holds it ends.
fn scratch(name: &str) -> Scratch {
    Scratch::dir(&format!("ve_saveload_{name}"))
}

fn copy_in(dir: &Path, name: &str) -> PathBuf {
    let to = dir.join(name);
    std::fs::copy(asset(name), &to).expect("copy the fixture");
    to
}

/// The whole of what a picked audio stream has to survive: a file's second
/// language goes on the timeline beside its first, the project says so on
/// disk, and reopening it plays the same two streams. This is the file-level
/// half of what a library row does -- `place_stream_at` is the one door that
/// row goes through.
#[test]
fn a_picked_audio_stream_lands_saves_and_comes_back() {
    let dir = scratch("streams");
    let media = copy_in(&dir, "test_multilang.mp4");
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0); // silent like the rest of the suites
    let end = session.timeline_duration();

    // Stream 1 is the French track, same rate and layout as stream 0, so it
    // can join this timeline; it lands as a second source of the same file.
    assert!(
        session
            .place_stream_at(end, &media, 1, None)
            .expect("stream 1 matches the timeline")
    );
    assert_eq!(
        session
            .sources()
            .iter()
            .map(|s| s.audio_stream)
            .collect::<Vec<_>>(),
        [0, 1],
        "the same file twice, on two streams"
    );
    assert_eq!(session.timeline_duration(), end * 2.0);

    let path = dir.join("langs.edith");
    session.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert!(
        text.contains("source 0 test_multilang.mp4")
            && text.contains("source 1 test_multilang.mp4"),
        "the streams did not reach the file:\n{text}"
    );

    let loaded = PlaybackSession::open_project(&path).expect("reopen the project");
    assert_eq!(loaded.sources(), session.sources());
    assert_eq!(loaded.timeline_duration(), session.timeline_duration());

    // And the refusals, in the engine's own words: a stream that cannot share
    // one output device with the timeline, and a file that is not on it.
    let other = copy_in(&dir, "test_multiaudio.mp4");
    let mut session = PlaybackSession::open(&other).expect("open the fixture");
    session.set_gain(0.0);
    // Its rate would be conformed now; its *layout* is what one output device
    // cannot have two of, and the refusal says which.
    let err = session
        .place_stream_at(0.0, &other, 1, None)
        .expect_err("a mono stream cannot join a stereo timeline")
        .to_string();
    assert_eq!(err, "audio 1 ch does not match the timeline's 2 ch");
    assert!(
        session
            .place_stream_at(0.0, &media, 1, None)
            .expect_err("a file that is not a source")
            .to_string()
            .contains("not on this timeline")
    );
    assert_eq!(session.sources().len(), 1, "a refusal added a source");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// Both stand-in switches through the front door: what the picture is cut on
/// and whether an import makes one at all are two settings, saved and reopened
/// as the pair they are -- and a session that was never told keeps making them,
/// which is what every project did before there was a switch.
#[test]
fn both_proxy_switches_survive_a_save_and_a_reopen() {
    let dir = scratch("proxyswitches");
    let media = copy_in(&dir, "test_av.mp4");
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    assert!(session.auto_proxies(), "a fresh project makes them by itself");
    assert!(!session.proxies(), "and is cut on the film itself");

    session.set_proxies(true);
    session.set_auto_proxies(false);
    let path = dir.join("switches.edith");
    session.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert!(text.contains("proxy on\nautoproxy off\n"), "{text}");

    let loaded = PlaybackSession::open_project(&path).expect("reopen");
    assert!(loaded.proxies(), "cut on the stand-ins came back");
    assert!(!loaded.auto_proxies(), "and so did making none of them");

    // ...and the default is the line left out, which is what a v12 file is.
    session.set_auto_proxies(true);
    session.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert!(!text.contains("autoproxy"), "on is written by saying nothing");
    assert!(
        PlaybackSession::open_project(&path)
            .expect("reopen")
            .auto_proxies()
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// The export card's encoder pick through the same front door: which seat a
/// delivery is written on is the project's, so it survives a save and a reopen
/// -- and a project nobody picked for exports on whatever this machine has,
/// which is what every project did before the row existed.
#[test]
fn the_encoder_seat_survives_a_save_and_a_reopen() {
    use engine::export::EncoderSeat;

    let dir = scratch("encoderseat");
    let media = copy_in(&dir, "test_av.mp4");
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    assert_eq!(
        session.encoder_seat(),
        EncoderSeat::Auto,
        "a fresh project takes the seat this machine has"
    );

    let path = dir.join("seat.edith");
    for seat in [EncoderSeat::Software, EncoderSeat::Hardware] {
        session.set_encoder_seat(seat);
        session.save_project(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains(&format!("encoder {}\n", seat.name())), "{text}");
        assert_eq!(
            PlaybackSession::open_project(&path)
                .expect("reopen")
                .encoder_seat(),
            seat
        );
    }

    // ...and the default is the line left out, which is what a v13 file is.
    session.set_encoder_seat(EncoderSeat::Auto);
    session.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert!(!text.contains("encoder"), "auto is written by saying nothing");
    assert_eq!(
        PlaybackSession::open_project(&path)
            .expect("reopen")
            .encoder_seat(),
        EncoderSeat::Auto
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// Why a trim's tail stops where the file ends: a clip reaching past the last
/// frame of its source is a project that will not open again (`open_project`
/// refuses it by name). Pulled in on both ends and then dragged out as far as
/// the pointer can ask for, the save still comes back as itself.
#[test]
fn a_trimmed_timeline_saves_and_opens_again() {
    use engine::project::Edge;

    let dir = scratch("trim");
    let media = copy_in(&dir, "test_av.mp4");
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    let whole = session.clip_at(0).expect("one clip");

    assert!(session.trim_clip(Lane::V1, 0, Edge::End, whole.end() / 2));
    assert!(session.trim_clip(Lane::V1, 0, Edge::Start, 5));
    let path = dir.join("trimmed.edith");
    session.save_project(&path).expect("save");
    let loaded = PlaybackSession::open_project(&path).expect("a trimmed project opens");
    assert_eq!(
        loaded
            .clip_at(0)
            .map(|c| (c.start, c.in_frame, c.out_frame)),
        Some((5, 5, whole.end() / 2)),
        "the trimmed range is what came back"
    );

    // A hand that keeps pulling: the tail stops at the file's own last frame,
    // and the save of *that* opens too.
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, u32::MAX));
    assert_eq!(
        session.clip_at(0).expect("still one clip").out_frame,
        whole.out_frame,
        "as far as the file goes and not one frame further"
    );
    session.save_project(&path).expect("save");
    PlaybackSession::open_project(&path).expect("and it still opens");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// Two files, four clips, one of them deleted, playhead at 3 s -- a timeline
/// with something to lose in every field.
fn edited(dir: &Path) -> PlaybackSession {
    let mut session = PlaybackSession::open(copy_in(dir, "test_av.mp4")).expect("open");
    // Imported into the library, then dragged onto the end -- the two acts a
    // second file on the timeline takes.
    let second = copy_in(dir, "test_av2.mp4");
    session.import(&second).expect("import the second file");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &second, 0, None)
            .expect("a file just imported is on this timeline"),
        "drag it onto the end"
    );
    assert!(session.cut_at(2.0), "cut inside the first file");
    assert!(session.cut_at(6.5), "cut inside the second file");
    assert!(
        session.delete_clip(Lane::V1, 1),
        "drop the second half of the first"
    );
    session.seek(3.0);
    assert_eq!(session.timeline_duration(), 6.0);
    session
}

/// What has to survive a round trip, all of it public API.
fn shape(session: &PlaybackSession) -> (Vec<(f64, f64, usize)>, Vec<Source>, f64) {
    (
        session.clip_spans_by_source(),
        session.sources().to_vec(),
        session.timeline_duration(),
    )
}

/// The version-1 promise: a file written before the lanes existed still opens,
/// as one grouped video+audio pair per clip laid end to end, and saving it
/// again writes version 7 -- which reopens as the same timeline.
#[test]
fn a_version_1_project_loads_fully_grouped_and_saves_as_version_7() {
    let dir = scratch("v1");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("old.edith");
    // 60 frames of picture from two clips: [0,30) then [60,90) of the source.
    std::fs::write(
        &path,
        "edith 1\nplayhead 45\nsource test_av.mp4\nclip 0 30 0\nclip 60 90 0\n",
    )
    .expect("write a v1 file");

    let loaded = PlaybackSession::open_project(&path).expect("a v1 file still opens");
    let fps = loaded.meta().frame_rate;
    assert_eq!(
        loaded.clip_spans_by_source(),
        vec![(0.0, 30.0 / fps, 0), (30.0 / fps, 30.0 / fps, 0)],
        "v1 clips queue up, the second starting where the first ended"
    );
    assert_eq!(
        loaded.lane_spans_by_source(engine::project::Lane::A1),
        loaded.clip_spans_by_source(),
        "both lanes, fully grouped: a v1 timeline had no holes"
    );
    assert!(
        (loaded.now() - 45.0 / fps).abs() < 1.0 / fps,
        "playhead kept"
    );

    // Saved again it is v7, and v7 round-trips to the same timeline.
    let v2 = dir.join("new.edith");
    loaded.save_project(&v2).expect("save");
    let text = std::fs::read_to_string(&v2).expect("read back");
    assert!(text.starts_with("edith 14\n"), "{text}");
    assert!(
        text.contains("\nresolution 1280 720\n"),
        "a project with no resolution of its own is saved at source 0's: {text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.starts_with("video ")).count(),
        2,
        "{text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.starts_with("audio ")).count(),
        2,
        "{text}"
    );
    let again = PlaybackSession::open_project(&v2).expect("open the v2 file");
    assert_eq!(shape(&again), shape(&loaded));
    // ...and re-saving it is byte-identical, so a round trip is stable.
    let third = dir.join("third.edith");
    again.save_project(&third).expect("save");
    assert_eq!(std::fs::read(&third).expect("read"), text.as_bytes());

    // And the same promise one version on: a v2 file names no audio stream and
    // opens on stream 0, which is the only one that dialect could play.
    let v2 = dir.join("v2.edith");
    std::fs::write(
        &v2,
        "edith 2\nplayhead 0\nsource test_av.mp4\nvideo 0 0 30 0 0\naudio 0 0 30 0 0\n",
    )
    .expect("write a v2 file");
    let loaded = PlaybackSession::open_project(&v2).expect("a v2 file still opens");
    assert_eq!(loaded.sources().len(), 1);
    assert_eq!(loaded.sources()[0].audio_stream, 0);
    loaded.save_project(&v2).expect("save");
    assert!(
        std::fs::read_to_string(&v2)
            .expect("read back")
            .contains("source 0 test_av.mp4"),
        "a re-saved v2 project says the stream it always meant"
    );
}

/// What version 4 is for: a project file holds any number of lanes, in the
/// order they are displayed in, and one holding nothing is still a lane. The
/// timeline is built by hand because the format is what this checks -- no UI
/// adds a lane yet -- and the file it writes back has to be the one it read.
#[test]
fn a_version_4_project_holds_more_than_two_lanes() {
    use engine::project::{Lane, LaneKind};

    let dir = scratch("v4_lanes");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("many.edith");
    // V1, A1, V2, A2 in display order: a take grouped across V1 and A2 (link 3,
    // one span on two lanes that are not a pair), a lone clip on V2, and an
    // empty A1 that has only its own line to say it is there.
    let text = "edith 4\nplayhead 0\nsource 0 test_av.mp4\n\
                video 1 0 0 30 0 3\naudio 1\nvideo 2 40 0 20 0 -\naudio 2 0 0 30 0 3\n";
    std::fs::write(&path, text).expect("write a v4 file");

    let loaded = PlaybackSession::open_project(&path).expect("a four-lane project opens");
    let v2 = Lane::new(LaneKind::Video, 1);
    let a2 = Lane::new(LaneKind::Audio, 1);
    assert_eq!(loaded.lane_clips(Lane::V1).len(), 1);
    assert!(loaded.lane_clips(Lane::A1).is_empty(), "A1 holds nothing");
    assert_eq!(
        loaded
            .lane_clips(v2)
            .iter()
            .map(|c| (c.start, c.end()))
            .collect::<Vec<_>>(),
        vec![(40, 60)],
        "V2 is a lane of its own, gap and all"
    );
    assert_eq!(
        loaded.lane_clips(a2)[0].link,
        loaded.lane_clips(Lane::V1)[0].link,
        "the group spans V1 and A2, which are not a pair"
    );
    assert_eq!(loaded.timeline_duration(), 60.0 / loaded.meta().frame_rate);

    // ...and saving it writes the v7 twin of what it read: the lane list, its
    // order and the empty lane all survive, each clip now saying it plays flat,
    // ungraded and fitted, under a project resolution taken from the media.
    let again = dir.join("again.edith");
    loaded.save_project(&again).expect("save");
    assert_eq!(
        std::fs::read_to_string(&again).expect("read back"),
        "edith 14\nplayhead 0\nresolution 1280 720\nfps 30.0\nsource 0 test_av.mp4\n\
         video 1 0 0 30 0 3 - - fit 1000\naudio 1\n\
         video 2 40 0 20 0 - - - fit 1000\naudio 2 0 0 30 0 3 - - fit 1000\n",
        "a four-lane project is written as it was read, three versions on"
    );
}

/// What version 5 is for: a clip carries equalizer settings, the file names
/// them once for however many clips share them, and the whole thing survives
/// the engine's own door -- open, save, every curve and index of it intact one
/// version on.
#[test]
fn a_version_5_project_carries_per_clip_equalizers() {
    let dir = scratch("v5_eq");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("eq.edith");
    // Two clips on one curve, one on another, one flat.
    let text = "edith 5\nplayhead 0\nsource 0 test_av.mp4\n\
                eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk\n\
                eq 12000.0:6.25:0.5:hs\n\
                video 1 0 0 30 0 - 0\nvideo 1 30 30 60 0 - 1\n\
                audio 1 0 0 30 0 - 0\naudio 1 30 30 60 0 - -\n";
    std::fs::write(&path, text).expect("write a v5 file");

    let loaded = PlaybackSession::open_project(&path).expect("a v5 project opens");
    assert_eq!(loaded.lane_clips(engine::project::Lane::V1).len(), 2);
    let again = dir.join("again.edith");
    loaded.save_project(&again).expect("save");
    assert_eq!(
        std::fs::read_to_string(&again).expect("read back"),
        "edith 14\nplayhead 0\nresolution 1280 720\nfps 30.0\nsource 0 test_av.mp4\n\
         eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk\n\
         eq 12000.0:6.25:0.5:hs\n\
         video 1 0 0 30 0 - 0 - fit 1000\nvideo 1 30 30 60 0 - 1 - fit 1000\n\
         audio 1 0 0 30 0 - 0 - fit 1000\naudio 1 30 30 60 0 - - - fit 1000\n",
        "the equalizer table and every clip's index survive a round trip"
    );
}

/// What version 6 is for, the same claim one version on: a clip carries a
/// colour grade beside its equalizer and the file names each once for however
/// many clips share it. A format bump necessarily rewrites the file, so what is
/// asserted here is the v7 twin of what was read; the byte-identity claim lives
/// in `a_version_7_project_carries_a_resolution_and_fit_policies` below.
#[test]
fn a_version_6_project_carries_per_clip_colours() {
    let dir = scratch("v6_color");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("color.edith");
    // Two clips on one grade, one on another, one ungraded -- and an equalizer
    // on a clip that is graded too, so the two tables are seen not to collide.
    let text = "edith 6\nplayhead 0\nsource 0 test_av.mp4\n\
                eq 80.0:-3.0:0.707:ls\n\
                color 0.1:1.2:0.9:-0.3\n\
                color -0.25:1.0:0.0:0.5\n\
                video 1 0 0 30 0 - 0 0\nvideo 1 30 30 60 0 - - 1\n\
                audio 1 0 0 30 0 - - 0\naudio 1 30 30 60 0 - - -\n";
    std::fs::write(&path, text).expect("write a v6 file");

    let loaded = PlaybackSession::open_project(&path).expect("a v6 project opens");
    assert_eq!(loaded.lane_clips(engine::project::Lane::V1).len(), 2);
    let again = dir.join("again.edith");
    loaded.save_project(&again).expect("save");
    assert_eq!(
        std::fs::read_to_string(&again).expect("read back"),
        "edith 14\nplayhead 0\nresolution 1280 720\nfps 30.0\nsource 0 test_av.mp4\n\
         eq 80.0:-3.0:0.707:ls\n\
         color 0.1:1.2:0.9:-0.3\n\
         color -0.25:1.0:0.0:0.5\n\
         video 1 0 0 30 0 - 0 0 fit 1000\nvideo 1 30 30 60 0 - - 1 fit 1000\n\
         audio 1 0 0 30 0 - - 0 fit 1000\naudio 1 30 30 60 0 - - - fit 1000\n",
        "the colour table and every clip's index survive a round trip"
    );
    // A dialect that could not say either one means the defaults, and those are
    // what such a project always was: the media's own size, nothing letterboxed.
    assert_eq!(loaded.resolution(), (1280, 720));
    assert_eq!(
        loaded.fit_of(engine::project::Lane::V1, 0),
        engine::scale::FitPolicy::Fit
    );
}

/// What version 7 is for: the project's picture size is its own, not source 0's,
/// and each clip says how it meets it. Both survive the engine's door byte for
/// byte -- and a project whose resolution is nobody's media size is exactly the
/// case that could not be written down before.
#[test]
fn a_version_7_project_carries_a_resolution_and_fit_policies() {
    let dir = scratch("v7_resolution");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("mixed.edith");
    let text = "edith 7\nplayhead 0\nresolution 960 720\nsource 0 test_av.mp4\n\
                video 1 0 0 30 0 - - - fill\nvideo 1 30 30 60 0 - - - center\n\
                audio 1 0 0 60 0 - - - fit\n";
    std::fs::write(&path, text).expect("write a v7 file");

    let loaded = PlaybackSession::open_project(&path).expect("a v7 project opens");
    assert_eq!(
        loaded.resolution(),
        (960, 720),
        "the project's own size, which is not the media's 1280x720"
    );
    assert_eq!(
        loaded.native_resolution(),
        (1280, 720),
        "...and the media's own size is still known"
    );
    let v1 = engine::project::Lane::V1;
    assert_eq!(loaded.fit_of(v1, 0), engine::scale::FitPolicy::Fill);
    assert_eq!(loaded.fit_of(v1, 1), engine::scale::FitPolicy::Center);

    let again = dir.join("again.edith");
    loaded.save_project(&again).expect("save");
    assert_eq!(
        std::fs::read_to_string(&again).expect("read back"),
        "edith 14\nplayhead 0\nresolution 960 720\nfps 30.0\nsource 0 test_av.mp4\n\
         video 1 0 0 30 0 - - - fill 1000\nvideo 1 30 30 60 0 - - - center 1000\n\
         audio 1 0 0 60 0 - - - fit 1000\n",
        "a v7 project is written back as the v8 it now is: the same clips, each \
         saying it plays at real time, which is what it always played at"
    );

    // The refusals the new grammar adds, each naming its line.
    for (bad, want) in [
        (
            "edith 7\nresolution 0 720\nsource 0 test_av.mp4\nvideo 1 0 0 30 0 - - - fit\n",
            "0x720 is not a picture",
        ),
        (
            "edith 7\nresolution 960 720\nresolution 960 720\nsource 0 test_av.mp4\n\
          video 1 0 0 30 0 - - - fit\n",
            "resolution belongs once",
        ),
        // Past 8K is refused by the same bound the keystroke has: unbounded,
        // this line reached `open_black` and panicked on a capacity overflow.
        (
            "edith 7\nresolution 4294967295 4294967295\nsource 0 test_av.mp4\n\
          video 1 0 0 30 0 - - - fit\n",
            "4294967295x4294967295 is not a picture",
        ),
        (
            "edith 7\nresolution 7681 1080\nsource 0 test_av.mp4\n\
          video 1 0 0 30 0 - - - fit\n",
            "7681x1080 is not a picture",
        ),
        (
            "edith 7\nresolution 960 720\nsource 0 test_av.mp4\nvideo 1 0 0 30 0 - - - squish\n",
            "not a fit policy",
        ),
    ] {
        let path = dir.join("bad.edith");
        std::fs::write(&path, bad).expect("write");
        let err = PlaybackSession::open_project(&path)
            .err()
            .expect("a malformed v7 file must be refused")
            .to_string();
        assert!(err.contains(want), "wanted {want:?}, got {err:?}");
    }
}

#[test]
fn a_saved_project_reopens_as_the_same_timeline() {
    let dir = scratch("round_trip");
    let saved = edited(&dir);
    let path = dir.join("edit.edith");
    saved.save_project(&path).expect("save");

    let mut loaded = PlaybackSession::open_project(&path).expect("open the project");
    assert_eq!(shape(&loaded), shape(&saved), "clips, sources and duration");
    assert!(
        (loaded.now() - saved.now()).abs() < 1.0 / loaded.meta().frame_rate,
        "playhead landed at {:.3}s, saved at {:.3}s",
        loaded.now(),
        saved.now()
    );
    assert!(!loaded.is_playing(), "a loaded project starts paused");
    assert!(!loaded.undo(), "history is not saved");

    // Saving what was loaded reproduces the file byte for byte -- the format
    // has no state the round trip drops.
    let again = dir.join("again.edith");
    loaded
        .save_project(&again)
        .expect("save the loaded project");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&again).unwrap(),
        "save -> load -> save is not a fixed point"
    );
    assert!(!path.with_extension("edith.part").exists(), "left a .part");
}

#[test]
fn a_folder_of_media_and_its_project_can_be_moved() {
    let from = scratch("relocate_from");
    let saved = edited(&from);
    saved.save_project(&from.join("edit.edith")).expect("save");
    drop(saved);

    let to = scratch("relocate_to");
    for name in ["test_av.mp4", "test_av2.mp4", "edit.edith"] {
        std::fs::copy(from.join(name), to.join(name)).expect("copy");
    }
    // The originals go away entirely: nothing absolute can be resolving here.
    std::fs::remove_dir_all(&from).expect("remove the original folder");

    let moved = PlaybackSession::open_project(&to.join("edit.edith")).expect("open the copy");
    assert_eq!(moved.timeline_duration(), 6.0);
    assert!(
        moved.sources().iter().all(|s| s.path.starts_with(&to)),
        "sources did not follow the folder: {:?}",
        moved.sources()
    );
}

#[test]
fn orphan_sources_are_not_written() {
    let dir = scratch("orphans");
    let mut session = PlaybackSession::open(copy_in(&dir, "test_av.mp4")).expect("open");
    // An import that was never dragged onto a lane *is* the orphan: a library
    // row with no clip naming it. (So is one whose clip was undone -- the same
    // entry, reached the other way.)
    session
        .import(&copy_in(&dir, "test_av2.mp4"))
        .expect("import");
    assert_eq!(
        session.sources().len(),
        2,
        "the orphan entry stays in-session"
    );

    let path = dir.join("orphan.edith");
    session.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("source ")).count(),
        1,
        "the orphan reached the file:\n{text}"
    );

    // And the load survives the orphan's file disappearing, which is the whole
    // point of pruning it.
    std::fs::remove_file(dir.join("test_av2.mp4")).expect("unlink the orphan");
    let loaded = PlaybackSession::open_project(&path).expect("open the project");
    assert_eq!(loaded.sources().len(), 1);
    assert_eq!(loaded.timeline_duration(), 5.0);
}

#[test]
fn a_source_that_vanished_is_refused_by_name() {
    let dir = scratch("missing");
    let session = edited(&dir);
    let path = dir.join("edit.edith");
    session.save_project(&path).expect("save");
    drop(session);

    let gone = dir.join("test_av2.mp4");
    std::fs::remove_file(&gone).expect("unlink");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a project whose source is gone must not open")
        .to_string();
    assert!(
        err.contains("test_av2.mp4"),
        "the refusal must name the file: {err}"
    );

    // The first source is no different -- it is simply refused earlier.
    std::fs::remove_file(dir.join("test_av.mp4")).expect("unlink");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("no first source, no session")
        .to_string();
    assert!(err.contains("test_av.mp4"), "{err}");
}

#[test]
fn a_source_that_shrank_is_refused_by_clip() {
    let dir = scratch("shrunk");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("shrunk.edith");
    // Hand-written: a clip that ran to frame 10000 of a 150-frame file, which
    // is what re-encoding a source shorter leaves behind.
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource test_av.mp4\nclip 0 30 0\nclip 30 10000 0\n",
    )
    .expect("write");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a clip past the end of its file must not open")
        .to_string();
    assert!(
        err.contains("clip 1") && err.contains("test_av.mp4") && err.contains("150"),
        "the refusal must name the clip, the file and its length: {err}"
    );

    // One frame short of the end is still inside the file.
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource test_av.mp4\nclip 0 150 0\n",
    )
    .expect("write");
    let whole = PlaybackSession::open_project(&path).expect("the exact length is legal");
    assert_eq!(whole.timeline_duration(), 5.0);
}

/// A refusal must *return*. Opening a project starts decoding source 0 before
/// it has validated anything, so by the time a later source is refused the
/// worker is several frames in -- and with a two-frame channel that nobody is
/// draining, it is parked in `send`. Tearing that down in the wrong order joins
/// a thread that is waiting for a receiver the same scope still holds, and the
/// app hangs on a bad project file instead of showing the error.
///
/// The many sources are what make the park a certainty rather than a race: each
/// one is opened and probed before the clip check that refuses, which is far
/// longer than the few frames it takes to fill the channel.
#[test]
fn a_refused_project_does_not_hang_on_its_own_decoder() {
    let dir = scratch("refusal_hang");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("refused.edith");
    let mut text = String::from("edith 1\nplayhead 0\n");
    for _ in 0..1024 {
        text.push_str("source test_av.mp4\n");
    }
    // Legal until the last line, which runs off the end of a 150-frame file.
    text.push_str("clip 0 150 0\nclip 0 10000 0\n");
    std::fs::write(&path, &text).expect("write");

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let probe = path.clone();
    std::thread::spawn(move || {
        let err = PlaybackSession::open_project(&probe)
            .err()
            .map(|e| e.to_string());
        let _ = tx.send(err);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Some(err)) => assert!(err.contains("150"), "refused, but not by name: {err}"),
        Ok(None) => panic!("a clip past the end of its file must not open"),
        Err(_) => panic!(
            "open_project hung on a refusal: its own decode worker is parked in \
             send and the teardown is waiting for it"
        ),
    }
}

#[test]
fn a_project_that_names_itself_is_refused() {
    let dir = scratch("self_reference");
    let path = dir.join("ouroboros.edith");
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource ouroboros.edith\nclip 0 30 0\n",
    )
    .expect("write");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a project is not a video")
        .to_string();
    assert!(err.contains("ouroboros.edith"), "{err}");
}

#[test]
fn malformed_files_are_numbered_errors_and_never_panics() {
    let dir = scratch("malformed");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("bad.edith");
    let good = "edith 1\nplayhead 0\nsource test_av.mp4\nclip 0 30 0\n";

    for (text, want) in [
        (
            "edith 15\nsource 0 test_av.mp4\nvideo 1 0 0 30 0 - - - fit\n",
            "line 1",
        ),
        // An eq index the table does not hold, and a band shape there is none.
        (
            "edith 5\nsource 0 test_av.mp4\nvideo 1 0 0 30 0 - 0\n",
            "line 3",
        ),
        (
            "edith 5\nsource 0 test_av.mp4\neq 80.0:0.0:0.707:band\nvideo 1 0 0 30 0 - 0\n",
            "line 3",
        ),
        // A lane number that skips one of its kind: V2 was never declared.
        (
            "edith 4\nsource 0 test_av.mp4\nvideo 2 0 0 30 0 -\n",
            "line 3",
        ),
        // A v2 source line in a v3 file: the stream field is not optional.
        ("edith 3\nsource test_av.mp4\nvideo 0 0 30 0 -\n", "line 2"),
        ("not a project at all\n", "line 1"),
        // Dialects do not mix: lane lines are v2's, `clip` is v1's.
        ("edith 2\nsource test_av.mp4\nclip 0 30 0\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nvideo 0 0 30 0 -\n", "line 3"),
        ("edith 2\nsource test_av.mp4\nvideo 0 0 30 0\n", "line 3"),
        (
            "edith 2\nsource test_av.mp4\nvideo 0 0 30 0 -\nvideo 10 0 30 0 -\n",
            "line 4",
        ),
        ("edith 1\nsource test_av.mp4\nclip 0 30\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nclip 0 30 4\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nclip 30 30 0\n", "line 3"),
        ("edith 1\nsource test_av.mp4\n\nclip 0 30 0\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nwhat 1\n", "line 3"),
        // Crafted numbers: `start + len` used to wrap past the top of `u32`,
        // and a wrapped end walks straight through the overlap check.
        (
            "edith 2\nsource test_av.mp4\nvideo 4294967290 0 30 0 -\n",
            "line 3",
        ),
    ] {
        std::fs::write(&path, text).expect("write");
        let err = PlaybackSession::open_project(&path)
            .err()
            .unwrap_or_else(|| panic!("accepted {text:?}"))
            .to_string();
        assert!(err.starts_with(want), "{text:?} -> {err}");
    }

    // Every truncation of a good file: some are valid shorter projects, none
    // may panic or be reported as anything but an error.
    for cut in 0..good.len() {
        std::fs::write(&path, &good[..cut]).expect("write");
        let _ = PlaybackSession::open_project(&path);
    }
    // ...and a file that is not there at all.
    std::fs::remove_file(&path).expect("unlink");
    assert!(PlaybackSession::open_project(&path).is_err());
}

#[test]
fn a_source_that_no_longer_matches_the_timeline_is_refused_in_import_words() {
    let dir = scratch("mismatch");
    copy_in(&dir, "test_av.mp4");
    std::fs::copy(asset("test_ac3.mp4"), dir.join("test_av2.mp4")).expect("substitute");
    let path = dir.join("swapped.edith");
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource test_av.mp4\nsource test_av2.mp4\nclip 0 30 0\nclip 0 30 1\n",
    )
    .expect("write");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a source that stopped matching must not open")
        .to_string();
    // The suffix is `import`'s own refusal, word for word. The substitute is
    // mono, which is the shape of what is left: one output device carries one
    // layout. A resolution of its own is placed on the project canvas, a frame
    // rate of its own is read through `Rate`, a *sample* rate of its own is
    // resampled at the decoder's door, a codec of its own opens its own decoder
    // and a file with no sound plays silence -- none of those is a refusal any
    // more.
    assert_eq!(
        err,
        format!(
            "source {}: audio 1 ch does not match the timeline's 2 ch",
            dir.join("test_av2.mp4").display()
        )
    );
}

/// The mix a project was left at comes back with it: a track's fader, the
/// master limiter and the rate the timeline was cut at. A fader that vanished
/// on a reload would be a setting nobody could keep.
#[test]
fn the_mix_survives_a_save_and_an_open() {
    use engine::project::{Lane, LaneKind};
    let dir = scratch("mix");
    let source = copy_in(&dir, "test_av.mp4");
    let mut saved = PlaybackSession::open(&source).expect("open the fixture");
    let a2 = saved.add_lane(LaneKind::Audio);
    assert!(saved.set_lane_gain_db(Lane::A1, -6.0));
    assert!(saved.set_lane_gain_db(a2, 3.0));
    let limiter = engine::limiter::Limiter {
        ceiling_db: -2.0,
        on: true,
    };
    assert!(saved.set_limiter(limiter));
    let path = dir.join("mix.edith");
    saved.save_project(&path).expect("save");

    let mut loaded = PlaybackSession::open_project(&path).expect("open the project");
    assert_eq!(loaded.lane_gain_db(Lane::A1), -6.0, "A1's fader");
    assert_eq!(loaded.lane_gain_db(a2), 3.0, "A2's own, not A1's");
    assert_eq!(loaded.limiter(), limiter);
    assert!(!loaded.undo(), "a loaded mix is not an undo step");
    // ...and saving it again is the same bytes: the format holds all of it.
    let again = dir.join("again.edith");
    loaded
        .save_project(&again)
        .expect("save the loaded project");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&again).unwrap(),
        "save -> load -> save is not a fixed point"
    );
}

/// The v11 bump's own round trip: the HDR rendition a project was left in comes
/// back with it, and a file written before there were any is the reference one.
/// A picked look that vanished on a reload would be a setting nobody could keep
/// -- the mix test's claim, for the picture.
#[test]
fn the_tone_map_preset_survives_a_save_and_an_open() {
    use engine::tonemap::Preset;
    let dir = scratch("tone");
    let source = copy_in(&dir, "test_av.mp4");
    let mut saved = PlaybackSession::open(&source).expect("open the fixture");
    assert_eq!(saved.tone(), Preset::Reference, "nobody has picked yet");
    assert!(saved.set_tone(Preset::Vivid));
    let path = dir.join("tone.edith");
    saved.save_project(&path).expect("save");

    let mut loaded = PlaybackSession::open_project(&path).expect("open the project");
    assert_eq!(loaded.tone(), Preset::Vivid);
    assert!(!loaded.undo(), "a loaded rendition is not an undo step");
    // ...and saving it again is the same bytes, as the mix is.
    let again = dir.join("again.edith");
    loaded
        .save_project(&again)
        .expect("save the loaded project");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&again).unwrap(),
        "save -> load -> save is not a fixed point"
    );

    // A project written before v11 says nothing about a rendition, and means the
    // published one -- the picture it was always shown in.
    let old = dir.join("old.edith");
    std::fs::write(
        &old,
        "edith 10\nplayhead 0\nresolution 1920 1080\nsource 0 test_av.mp4\n\
         video 1 0 0 24 0 - - - fit 1000\naudio 1 0 0 24 0 - - - fit 1000\n",
    )
    .expect("write a v10 project");
    let older = PlaybackSession::open_project(&old).expect("open the v10 project");
    assert_eq!(older.tone(), Preset::Reference);
}

/// The other half of the v9 bump, and the debt it pays: a timeline holding
/// nothing but a song has no file to take a frame rate from, so it used to come
/// back at the canvas rate whatever it was cut at. The written `fps` is now
/// preferred over that inference -- and a project whose scaffold *does* have a
/// picture still takes the rate from the file, which is what every clip on it
/// was conformed to.
#[test]
fn a_song_only_project_comes_back_at_the_rate_it_names() {
    let dir = scratch("song_fps");
    copy_in(&dir, "test_tone.mp3");
    let path = dir.join("song.edith");
    std::fs::write(
        &path,
        "edith 10\nplayhead 0\nresolution 1920 1080\nfps 24.0\nsource 0 test_tone.mp3\n\
         video 1\naudio 1 0 0 24 0 - - - fit 1000\n",
    )
    .expect("write a v9 song project");
    let loaded = PlaybackSession::open_project(&path).expect("a song-only project opens");
    assert_eq!(loaded.meta().frame_rate, 24.0, "the rate the file names");
    assert_eq!(loaded.timeline_duration(), 1.0, "one second at 24 fps");

    // The same file without the line -- which is every dialect before v9 --
    // still comes back at the canvas rate, so an old project is unchanged.
    let old = dir.join("song8.edith");
    std::fs::write(
        &old,
        "edith 8\nplayhead 0\nresolution 1920 1080\nsource 0 test_tone.mp3\n\
         video 1\naudio 1 0 0 24 0 - - - fit 1000\n",
    )
    .expect("write a v8 song project");
    let loaded = PlaybackSession::open_project(&old).expect("a v8 song project opens");
    assert_eq!(loaded.meta().frame_rate, 30.0, "the canvas rate, as before");
}
