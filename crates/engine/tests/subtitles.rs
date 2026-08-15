//! Subtitles: the cues of a file beside the media and of a track inside it are
//! the same cues, a project keeps them across a save, and a track that is
//! pictures rather than text is read as pictures rather than listed and
//! skipped.
//!
//! ```text
//! cargo test -p engine --release --test subtitles -- --nocapture
//! ```

use std::path::PathBuf;

use engine::project::{Edge, Lane, LaneKind, SubClip};
use engine::scratch::Scratch;
use engine::subtitle::{self, Cue, SubtitleTrack};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn scratch(name: &str) -> Scratch {
    Scratch::dir(&format!("ve_subs_{name}"))
}

/// The three cues both hand-written fixtures hold, with the markup resolved:
/// the italics of the `.ass` third line are not text, and the `\N` of its
/// second one is a line break like the `.srt`'s.
fn expected() -> Vec<Cue> {
    vec![
        Cue {
            start_us: 500_000,
            end_us: 1_500_000,
            text: "first line".into(),
            image: None,
        },
        Cue {
            start_us: 2_000_000,
            end_us: 3_250_000,
            text: "second line\nwith a break".into(),
            image: None,
        },
        Cue {
            start_us: 4_000_000,
            end_us: 4_750_000,
            text: "third line".into(),
            image: None,
        },
    ]
}

/// The whole of the parse half: four routes to the same three cues -- an
/// `.srt`, an `.ass` with override tags in it, and the two tracks of the
/// Matroska file both were muxed into, whose timing lives on the block and
/// whose text lives in it.
#[test]
fn a_file_beside_the_media_and_a_track_inside_it_read_the_same() {
    let srt = subtitle::open(&data("test_subs.srt"), None);
    assert_eq!(srt.refused, None, "the .srt parses");
    assert_eq!(srt.cues, expected());
    assert_eq!(srt.label, "test_subs.srt");

    let ass = subtitle::open(&data("test_subs.ass"), None);
    assert_eq!(ass.refused, None, "the .ass parses");
    assert_eq!(ass.cues, expected(), "the override tags are resolved");

    let embedded = subtitle::of_matroska(&asset("test_subs.mkv")).expect("the mkv is walked");
    let text: Vec<_> = embedded.iter().map(|t| (&t.label, &t.cues)).collect();
    assert_eq!(embedded.len(), 2, "two subtitle tracks: {text:?}");
    for track in &embedded {
        assert_eq!(track.refused, None, "{} parses", track.label);
        assert_eq!(
            track.cues,
            expected(),
            "{} holds the same cues",
            track.label
        );
    }
    // ...and each is named by what the file says about it, so a list has
    // something to show but the track number.
    assert_eq!(embedded[0].label, "eng");
    assert_eq!(embedded[1].label, "fra — Signs");
    assert_eq!(embedded[0].track, Some(2));
    assert_eq!(embedded[1].track, Some(3));

    // The one door a saved line comes back through reaches the same track.
    let one = subtitle::open(&asset("test_subs.mkv"), Some(3));
    assert_eq!(one.cues, expected());
    // A track number the file does not have is refused by name, not an error:
    // the project it was saved in still opens.
    let gone = subtitle::open(&asset("test_subs.mkv"), Some(9));
    assert!(
        gone.refused.as_deref().unwrap_or_default().contains("9"),
        "{:?}",
        gone.refused
    );
}

/// The whole of the bitmap half on a file small enough to read in one screen:
/// four PGS tracks, the first of them carrying one picture and the disc's own
/// "take it off again" after it. A row per track, cues on the one with blocks,
/// and the picture decodes to the pixels the display set describes.
///
/// Written here rather than muxed by the fixture script because ffmpeg cannot
/// encode text subtitles into pictures; the bytes below are that Matroska file.
#[test]
fn a_pgs_track_is_read_as_pictures_and_the_erase_after_one_ends_it() {
    let dir = scratch("pgs");
    let file = dir.join("remux.mkv");
    std::fs::write(&file, pgs_matroska(pgs_display_set())).expect("write the hand-made mkv");

    let tracks = subtitle::of_matroska(&file).expect("the walk does not fail over them");
    assert_eq!(tracks.len(), 4, "every PGS track is listed");
    for (i, track) in tracks.iter().enumerate() {
        assert_eq!(track.track, Some(i as u64 + 1));
        assert_eq!(track.refused, None, "a PGS track is read, not refused");
    }
    // The language is what a list shows, and `und` is what the spec says a
    // track that names none is.
    assert_eq!(tracks[0].label, "eng");
    assert_eq!(tracks[3].label, "und");
    // The three tracks with no blocks are tracks with no cues -- and still
    // tracks of pictures, because that is the codec and not a count: one whose
    // every display set is an erase would otherwise pass for text, and an
    // export would promise to write words it has none of.
    for track in &tracks[1..] {
        assert_eq!(track.cues, Vec::new());
        assert!(track.is_bitmap(), "a PGS track with no cue is still PGS");
    }

    // Two blocks, one cue: the second composes no object, which is the disc
    // clearing the screen, and that is where the first one ends. `BlockDuration`
    // says nothing here, exactly as it says zero on a real remux.
    let track = &tracks[0];
    assert!(track.is_bitmap(), "its cues are pictures");
    assert_eq!(track.cues.len(), 1, "the erase is not a cue of its own");
    let cue = &track.cues[0];
    assert_eq!((cue.start_us, cue.end_us), (500_000, 2_000_000));
    assert_eq!(cue.text, "", "a picture has no words");

    let image = cue.image.as_ref().expect("the cue is a picture");
    assert_eq!(
        (image.width, image.height),
        (8, 4),
        "the canvas is the disc's frame, not the object's box"
    );
    let rgba = image.rgba().expect("the display set decodes");
    assert_eq!(rgba.len(), 8 * 4 * 4);
    // A 2x2 white square at (1, 1) on an otherwise transparent canvas: opaque
    // where the object is, and nothing anywhere else.
    let at = |x: usize, y: usize| rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].to_vec();
    assert_eq!(at(1, 1), vec![235, 235, 235, 255]);
    assert_eq!(at(2, 2), vec![235, 235, 235, 255]);
    assert_eq!(at(0, 0), vec![0, 0, 0, 0]);
    assert_eq!(at(3, 1), vec![0, 0, 0, 0], "and no wider than it is");
    assert_eq!(
        rgba.chunks_exact(4).filter(|p| p[3] > 0).count(),
        4,
        "four painted pixels and no more"
    );
}

/// The muxer that packs a display set and the erase after it into one block:
/// the cue is still the picture. The sup handed to the decoder stops at the end
/// of the first set that composes something, and without that cut the erase is
/// the last word and the cue decodes to a canvas with nothing on it.
#[test]
fn a_block_holding_the_erase_after_the_picture_still_draws_the_picture() {
    let dir = scratch("pgs_packed");
    let file = dir.join("packed.mkv");
    let packed = [pgs_display_set(), pgs_erase()].concat();
    std::fs::write(&file, pgs_matroska(packed)).expect("write the hand-made mkv");

    let tracks = subtitle::of_matroska(&file).expect("the walk");
    let cue = &tracks[0].cues[0];
    let image = cue.image.as_ref().expect("the packed block is still a picture");
    assert_eq!((image.width, image.height), (8, 4));
    let rgba = image.rgba().expect("it decodes");
    assert_eq!(
        rgba.chunks_exact(4).filter(|p| p[3] > 0).count(),
        4,
        "the picture, not the erase written after it"
    );
}

/// The film that reported the bug: four PGS tracks beside one `S_TEXT/UTF8`
/// one, none of them refused any more. Skipped where the file is not -- it is
/// a 4K remux named by the local `real_library.toml`, not in this repository.
#[test]
fn the_remux_that_reported_the_bug_reads_all_five_of_its_tracks() {
    let Some(film) = engine::real_library::film("hevc_4k_pgs") else {
        return;
    };
    let tracks = subtitle::of_matroska(&film).expect("the walk");
    assert_eq!(tracks.len(), 5);
    assert!(
        tracks.iter().all(|t| t.refused.is_none()),
        "every row is one a click can pick: {:?}",
        tracks.iter().map(|t| &t.refused).collect::<Vec<_>>()
    );
    // One text track and four of pictures, which is what the remux carries.
    let bitmap: Vec<_> = tracks.iter().filter(|t| t.is_bitmap()).collect();
    assert_eq!(bitmap.len(), 4);
    for track in &bitmap {
        assert!(
            track.cues.len() > 100,
            "{} — {} cues",
            track.label,
            track.cues.len()
        );
        let cue = &track.cues[0];
        let image = cue.image.as_ref().expect("a picture cue");
        assert_eq!(
            (image.width, image.height),
            (1920, 1080),
            "the BluRay canvas"
        );
        assert!(cue.end_us > cue.start_us, "a cue that is up for a while");
        let rgba = image.rgba().expect("the first display set decodes");
        assert_eq!(rgba.len(), 1920 * 1080 * 4);
        assert!(
            rgba.chunks_exact(4).any(|p| p[3] > 0),
            "{} — the first cue paints something",
            track.label
        );
    }
}

/// A saved project comes back with its subtitles: the `.edith` names the file
/// and the track, and the cues are read out of them again on the way in. The
/// engine's own door for both halves -- import, save, open -- since that is
/// what a front-end will drive.
#[test]
fn a_project_keeps_its_subtitles_across_a_save() {
    let dir = scratch("save");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    // Beside the media, which is where a subtitle file lives: the project then
    // writes it relative, and the whole folder stays movable.
    let subs = dir.join("test_subs.srt");
    std::fs::copy(data("test_subs.srt"), &subs).expect("copy the subtitle fixture");

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0); // silent like the rest of the suites
    assert_eq!(session.subtitles().len(), 0, "nothing to start with");
    assert_eq!(
        session.import_subtitles(&subs).expect("the .srt imports"),
        1
    );
    // The same file twice is the same subtitles, not two rows of them.
    assert_eq!(session.import_subtitles(&subs).expect("no error"), 0);
    assert_eq!(session.subtitles()[0].cues, expected());

    let project = dir.join("cut.edith");
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).expect("read the project back");
    assert!(text.starts_with("edith 16\n"), "{text}");
    assert!(
        text.contains("\nsubtitle - test_subs.srt\n"),
        "the reference is written relative, and no cue is: {text}"
    );

    let back = engine::PlaybackSession::open_project(&project).expect("open the project");
    assert_eq!(back.subtitles().len(), 1);
    assert_eq!(back.subtitles()[0].cues, expected());
    assert_eq!(back.subtitles()[0].track, None);
    drop(back);

    // A subtitle file that has gone missing is listed, refused by name, and
    // written out again: the project still opens and the row is not lost.
    std::fs::remove_file(&subs).expect("take the subtitles away");
    let gone = engine::PlaybackSession::open_project(&project).expect("the project still opens");
    assert_eq!(gone.subtitles().len(), 1);
    assert!(gone.subtitles()[0].refused.is_some());
    let again = dir.join("cut2.edith");
    gone.save_project(&again).expect("save again");
    assert!(
        std::fs::read_to_string(&again)
            .expect("read")
            .contains("\nsubtitle - test_subs.srt\n"),
        "a re-save keeps the row"
    );
}

/// The two-door import: the parse runs with no session in hand -- which is what
/// lets a front-end run it off the render thread -- and the tracks it hands back
/// go on the timeline for the cost of a push. Same dedupe as the one-call door,
/// so the same file twice is still one set of rows, and what lands is saved and
/// loaded as a reference exactly like an `import_subtitles` row.
#[test]
fn parsed_tracks_can_be_handed_to_a_session_without_it_reading_the_file() {
    let dir = scratch("handoff");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    let mkv = dir.join("test_subs.mkv");
    std::fs::copy(asset("test_subs.mkv"), &mkv).expect("copy the subtitle fixture");

    // The door a background executor calls: no `&self`, so nothing about the
    // session is borrowed while the container is walked.
    let tracks =
        engine::PlaybackSession::parse_subtitles(&mkv).expect("the mkv is walked off-thread");
    assert_eq!(tracks.len(), 2, "both tracks of the fixture");
    // Owned data all the way down, so the parse may run on a background task
    // and its result be sent back: a track holding a handle would put the walk
    // back on the thread that draws.
    fn sendable<T: Send + 'static>(_: &T) {}
    sendable(&tracks);

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    assert_eq!(session.add_subtitle_tracks(tracks.clone()), 2, "both go on");
    // The same tracks again are the same subtitles: the (path, track) dedupe is
    // what a front-end's "already on the timeline" line falls out of.
    assert_eq!(session.add_subtitle_tracks(tracks), 0, "and no repeat rows");
    assert_eq!(session.subtitles().len(), 2);
    // And the one-call door agrees with it, over the very same file.
    assert_eq!(session.import_subtitles(&mkv).expect("no error"), 0);
    assert_eq!(session.subtitles()[0].cues, expected());

    let project = dir.join("cut.edith");
    session.save_project(&project).expect("save");
    let back = engine::PlaybackSession::open_project(&project).expect("open the project");
    assert_eq!(
        back.subtitles().iter().map(|t| t.track).collect::<Vec<_>>(),
        session
            .subtitles()
            .iter()
            .map(|t| t.track)
            .collect::<Vec<_>>(),
        "a handed-over row is a reference like any other"
    );
    assert_eq!(back.subtitles()[1].cues, expected());
}

/// Whatever an import added comes back off: three tracks go on, the middle one
/// is removed, and what is left is the other two *in the order they were added*
/// -- a save then writes exactly those two references and a load reads back
/// exactly them. The bad index is a refusal naming the index, not a silent
/// no-op, since a front-end holding a stale row has picked the wrong one.
#[test]
fn a_subtitle_row_can_be_removed_and_the_survivors_round_trip() {
    let dir = scratch("remove");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    // Three files rather than three tracks of one: `add_subtitles` dedupes by
    // (path, track), so three names are three rows.
    for name in ["a.srt", "b.srt", "c.srt"] {
        std::fs::copy(data("test_subs.srt"), dir.join(name)).expect("copy the subtitle fixture");
    }

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    for name in ["a.srt", "b.srt", "c.srt"] {
        assert_eq!(
            session
                .import_subtitles(&dir.join(name))
                .expect("the .srt imports"),
            1
        );
    }
    assert_eq!(session.subtitles().len(), 3);

    session.remove_subtitles(1).expect("the middle row goes");
    let names: Vec<String> = session
        .subtitles()
        .iter()
        .map(|t| {
            t.path
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["a.srt", "c.srt"], "the survivors keep their order");
    assert_eq!(session.subtitles()[0].cues, expected(), "and their cues");

    let project = dir.join("cut.edith");
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).expect("read the project back");
    assert!(
        text.contains("\nsubtitle - a.srt\n") && text.contains("\nsubtitle - c.srt\n"),
        "the survivors are written, by reference: {text}"
    );
    assert!(
        !text.contains("b.srt"),
        "and the removed one is not: {text}"
    );

    let back = engine::PlaybackSession::open_project(&project).expect("open the project");
    assert_eq!(
        back.subtitles()
            .iter()
            .map(|t| t.path.file_name().expect("a name").to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["a.srt", "c.srt"],
        "and a load is the same two, in the same order"
    );
    assert_eq!(back.subtitles()[1].cues, expected());

    // A row that is not there says so, naming the index a front-end passed.
    let why = session
        .remove_subtitles(9)
        .expect_err("there is no row 9")
        .to_string();
    assert!(why.contains('9') && why.contains("subtitle"), "{why}");
    assert_eq!(session.subtitles().len(), 2, "and nothing moved");
}

/// The whole point of the add-subtitles door: another release's subtitle
/// tracks go onto the timeline that is already open, and the file they came out
/// of joins *nothing* -- no source row, no lane, no clip. Nobody has to import a
/// second copy of a film, or break the edit they have, to get its subtitles.
///
/// And a file the walk found no subtitle track in is refused as that, in words,
/// rather than counted as zero: zero added is what a file whose tracks are here
/// already answers, and the two are opposite instructions to the person reading
/// them.
#[test]
fn subtitles_come_off_another_file_without_it_joining_the_timeline() {
    let media = asset("test_av.mp4");
    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    let sources: Vec<_> = session.sources().iter().map(|s| s.path.clone()).collect();
    let lanes = session.lanes();
    let clips: Vec<_> = lanes.iter().map(|&l| session.lane_clips(l).len()).collect();
    assert_eq!(sources.len(), 1, "one file is open");

    // A different file entirely, carrying two subtitle tracks.
    let other = asset("test_subs.mkv");
    assert_eq!(
        session.import_subtitles(&other).expect("its subtitles come"),
        2
    );
    assert_eq!(session.subtitles().len(), 2);
    for track in session.subtitles() {
        assert_eq!(track.path, other, "the rows name the file they came out of");
        assert!(!track.cues.is_empty(), "with their cues read");
    }
    // ...and the timeline is exactly the timeline it was.
    assert_eq!(
        session.sources().iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
        sources,
        "the file whose subtitles these are did not join the library"
    );
    assert_eq!(session.lanes(), lanes, "no lane was added");
    assert_eq!(
        session.lanes().iter().map(|&l| session.lane_clips(l).len()).collect::<Vec<_>>(),
        clips,
        "and nothing was cut, moved or dropped on one"
    );

    // The same file again is the same two tracks, not four.
    assert_eq!(session.import_subtitles(&other).expect("no error"), 0);
    assert_eq!(session.subtitles().len(), 2);

    // A container walked and carrying none says so, naming the file -- and the
    // claim is only made after the walk looked.
    let why = session
        .import_subtitles(&media)
        .expect_err("the fixture carries no subtitle track")
        .to_string();
    assert!(
        why.contains("no subtitle tracks in") && why.contains("test_av.mp4"),
        "{why}"
    );
    assert_eq!(session.subtitles().len(), 2, "and nothing changed over it");
}

/// A bitmap track survives a save the same way a text one does, and for the
/// same reason: a `.edith` holds the file and the track number, never a cue, so
/// what comes back is what the file still says. The pictures are read out of
/// the remux again on the way in -- thirty megabytes a track is exactly what a
/// project file is not for.
#[test]
fn a_project_keeps_a_bitmap_track_as_a_reference_and_reads_its_pictures_back() {
    let dir = scratch("save_pgs");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    let remux = dir.join("remux.mkv");
    std::fs::write(&remux, pgs_matroska(pgs_display_set())).expect("write the hand-made mkv");

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    assert_eq!(
        session.import_subtitles(&remux).expect("the remux imports"),
        4,
        "one row per PGS track"
    );
    assert!(session.subtitles()[0].is_bitmap());
    // An mkv export writes a *text* track and these cues are pictures, so the
    // card says so before the button rather than the file coming out short.
    // The same `is_bitmap` is what makes `export_subtitles` write none.
    let planned = session.planned_subtitles(engine::export::Format::Hevc, Some(0));
    assert!(
        planned.contains("pictures") && planned.contains("not written"),
        "{planned}"
    );

    let project = dir.join("cut.edith");
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).expect("read the project back");
    assert!(
        text.contains("\nsubtitle 1 remux.mkv\n"),
        "the track is a reference and no picture is written: {text}"
    );

    let back = engine::PlaybackSession::open_project(&project).expect("open the project");
    assert_eq!(back.subtitles().len(), 4);
    let track = &back.subtitles()[0];
    assert_eq!(track.refused, None);
    assert!(track.is_bitmap(), "and it comes back as pictures");
    assert_eq!(track.cues.len(), 1);
    let image = track.cues[0].image.as_ref().expect("the picture is back");
    assert_eq!((image.width, image.height), (8, 4));
    assert_eq!(image.rgba().expect("it decodes").len(), 8 * 4 * 4);

    // Each row is the track it was saved as, in the order it was saved in: what
    // a click picks is a place in this list, so a load that reordered the rows
    // would repoint the picked track without saying anything. A re-save writing
    // the same bytes is that order proved from both ends.
    assert_eq!(
        back.subtitles().iter().map(|t| t.track).collect::<Vec<_>>(),
        vec![Some(1), Some(2), Some(3), Some(4)]
    );
    let again = dir.join("cut2.edith");
    back.save_project(&again).expect("save again");
    assert_eq!(
        std::fs::read(&again).expect("read"),
        std::fs::read(&project).expect("read"),
        "a save, a load and a save again are the same file"
    );
}

/// Bytes this process has asked the kernel for so far, which is what a walk of
/// a Matroska file costs and a page cache cannot hide: `rchar` counts what
/// `read` handed back, warm or cold.
fn read_bytes() -> u64 {
    let io = std::fs::read_to_string("/proc/self/io").expect("Linux, like the rest of this suite");
    io.lines()
        .find_map(|l| l.strip_prefix("rchar: "))
        .expect("rchar")
        .trim()
        .parse()
        .expect("a number")
}

/// A project naming four tracks of one file walks that file *once*, and hands
/// the rows back in the order they were saved in.
///
/// The order is the invariant with teeth: a subtitle is picked by its place in
/// this list, so a reader that grouped the rows by file and handed them back
/// grouped would silently repoint what the user had picked. And the walk count
/// is the reason the grouping exists at all -- a saved film with thirty-five
/// tracks was thirty-five walks of a 25 GB file with the window stopped.
#[test]
fn a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order() {
    let dir = scratch("one_walk");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    let remux = dir.join("remux.mkv");
    std::fs::write(&remux, pgs_matroska(pgs_display_set())).expect("write the hand-made mkv");
    let subs = dir.join("test_subs.srt");
    std::fs::copy(data("test_subs.srt"), &subs).expect("copy the subtitle fixture");

    // One walk, for the measurement below to be compared against.
    let before = read_bytes();
    let all = subtitle::of_matroska(&remux).expect("the walk");
    let one_walk = read_bytes() - before;
    assert_eq!(all.len(), 4);
    assert!(one_walk > 0, "a walk reads something");

    // The rows a `.edith` holds, deliberately out of file order and with the
    // standalone file in the middle of them.
    let rows: Vec<(PathBuf, Option<u64>)> = vec![
        (remux.clone(), Some(3)),
        (remux.clone(), Some(1)),
        (subs.clone(), None),
        (remux.clone(), Some(4)),
        (remux.clone(), Some(2)),
        (remux.clone(), Some(9)), // gone: refused by name, still a row
    ];
    let before = read_bytes();
    let opened = subtitle::open_all(&rows);
    let cost = read_bytes() - before;
    assert_eq!(
        opened.iter().map(|t| (&t.path, t.track)).collect::<Vec<_>>(),
        rows.iter().map(|(p, n)| (p, *n)).collect::<Vec<_>>(),
        "every row comes back where it was saved"
    );
    assert_eq!(opened[1].label, "eng", "and it is that file's track 1");
    assert_eq!(opened[2].cues, expected(), "the standalone row is untouched");
    assert!(
        opened[5]
            .refused
            .as_deref()
            .unwrap_or_default()
            .contains("no subtitle track 9"),
        "{:?}",
        opened[5].refused
    );
    assert!(
        cost < one_walk * 2,
        "four rows of one file is one walk of it: {cost} bytes read against {one_walk} for a walk"
    );
}

/// A refusal a front-end shows at the moment of the import, rather than a row
/// nobody asked for: a file that is not a subtitle format at all.
#[test]
fn importing_something_that_is_not_a_subtitle_says_so() {
    let dir = scratch("refuse");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    let notes = dir.join("notes.txt");
    std::fs::write(&notes, "not a subtitle").expect("write");

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    let err = session
        .import_subtitles(&notes)
        .expect_err("a .txt is not a subtitle")
        .to_string();
    assert!(err.contains("txt"), "{err}");
    assert_eq!(session.subtitles().len(), 0, "and nothing was added");
}

/// The Matroska that is the subtitles alone: `.mks`, what a subtitle release
/// ships beside the film, imported by the door an `.mkv` is imported by.
///
/// The refusal it used to get -- *"mks" is not a subtitle format this reads* --
/// was a claim about a container this very binary parses: the bytes are a
/// Matroska's and the same walk reads them, so only the extension list said no.
#[test]
fn a_matroska_that_is_the_subtitles_alone_imports_like_any_other() {
    let dir = scratch("mks");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    let added = session
        .import_subtitles(&asset("test_subs.mks"))
        .expect("a .mks is a Matroska and imports as one");
    assert_eq!(added, 2, "both its tracks came in");
    let labels: Vec<&str> = session.subtitles().iter().map(|t| &*t.label).collect();
    assert_eq!(labels, ["eng", "fra — Signs"], "with their own names");
    for track in session.subtitles() {
        assert_eq!(track.refused, None, "{} parses", track.label);
        assert_eq!(track.cues, expected(), "{} holds the cues", track.label);
    }
}

/// A track that states its language the way a modern muxer states it --
/// `LanguageBCP47` and no legacy `Language` element at all -- keeps that
/// language, where it used to come in as `und`.
///
/// The shape of every English track of one real 4K web remux and all four
/// of another film's: they read as `und` while the file said `en`,
/// and the loss survived into the exported file's track list.
#[test]
fn a_track_that_states_its_language_the_modern_way_keeps_it() {
    // The fixture is what it claims to be: the modern element is in it and the
    // legacy one is not, so what is being read is the new element and not a
    // leftover of the old one.
    let bytes = std::fs::read(asset("test_subs_bcp47.mkv")).expect("the fixture is there");
    assert!(
        bytes.windows(3).any(|w| w == [0x22, 0xB5, 0x9D]),
        "the fixture states LanguageBCP47"
    );
    for legacy in [b"\x22\xb5\x9c\x83eng".as_slice(), b"\x22\xb5\x9c\x83fra"] {
        assert!(
            !bytes.windows(legacy.len()).any(|w| w == legacy),
            "and states no legacy Language element for its subtitle tracks"
        );
    }

    let tracks = subtitle::of_matroska(&asset("test_subs_bcp47.mkv")).expect("the mkv is walked");
    let languages: Vec<&str> = tracks.iter().map(|t| &*t.language).collect();
    assert_eq!(languages, ["eng", "fra"], "`en` is English, `fr` is French");
    // ...and the same file with the legacy element reads exactly the same, which
    // is the point: which element a muxer chose is not something a row shows.
    let legacy = subtitle::of_matroska(&asset("test_subs.mkv")).expect("the mkv is walked");
    let want: Vec<&str> = legacy.iter().map(|t| &*t.label).collect();
    let got: Vec<&str> = tracks.iter().map(|t| &*t.label).collect();
    assert_eq!(got, want, "the two files list the same two tracks");
}

/// The frame rate every placement test below counts in: at 30 fps a frame is
/// 33333 microseconds and a second is 30 frames exactly, so a window and a span
/// can be read against each other by eye.
const LANE_FPS: f64 = 30.0;
/// Never opened -- a `Project` is pure data ([`engine::Project::single`] reads
/// no file), so a path that is not there simply dedups by spelling.
const FILE: &str = "/nonexistent/subs.mp4";

/// A project with the three-cue `.srt` on its palette and one empty subtitle
/// lane: `V1`, `A1`, `S1`, which is the state a `+ S` press leaves.
fn with_subtitle_lane() -> (engine::Project, Lane) {
    let mut project = engine::Project::single(FILE, 150);
    assert!(
        project.add_subtitles(&subtitle::open(&data("test_subs.srt"), None)),
        "the palette takes the track"
    );
    let lane = project.add_lane(LaneKind::Subtitle);
    assert_eq!(lane, Lane::S1, "the first subtitle lane is S1");
    assert_eq!(lane.label(), "S1", "and is written S1");
    (project, lane)
}

/// A placement of the whole of palette track 0, five seconds of it.
fn whole(track: usize) -> SubClip {
    SubClip {
        start: 0,
        frames: 150,
        track,
        in_us: 0,
        out_us: 5_000_000,
        link: None,
    }
}

/// The lane half of the model: a caption is placed, trimmed at either end,
/// moved and taken back -- all of it through the machinery a clip already goes
/// through, so an undo is the lane snapshot every other edit pushes.
#[test]
fn a_caption_is_placed_trimmed_moved_and_undone_on_its_lane() {
    let (mut project, lane) = with_subtitle_lane();
    project.place_sub(lane, 30, whole(0)).expect("it goes down");
    assert_eq!(
        project.sub_lane(lane),
        [SubClip {
            start: 30,
            ..whole(0)
        }],
        "placed where it was asked for, window untouched"
    );

    // The tail in: fewer frames, and the window loses exactly the seconds the
    // frames did (90 frames of 30 fps is 3s, so [0, 2s) is what is left).
    project
        .trim_sub(lane, 0, Edge::End, 90, LANE_FPS)
        .expect("the tail comes in");
    assert_eq!(
        project.sub_lane(lane)[0],
        SubClip {
            start: 30,
            frames: 60,
            track: 0,
            in_us: 0,
            out_us: 2_000_000,
            link: None,
        }
    );

    // ...and the head, which moves the window's *start* with it: the words that
    // stay are the words that were there.
    project
        .trim_sub(lane, 0, Edge::Start, 45, LANE_FPS)
        .expect("the head comes in");
    assert_eq!(
        project.sub_lane(lane)[0],
        SubClip {
            start: 45,
            frames: 45,
            track: 0,
            in_us: 500_000,
            out_us: 2_000_000,
            link: None,
        }
    );

    // A move is `start` and nothing else -- the same words, later.
    project
        .move_sub(lane, 0, lane, 300)
        .expect("it slides down the lane");
    assert_eq!(
        project.sub_lane(lane)[0],
        SubClip {
            start: 300,
            frames: 45,
            track: 0,
            in_us: 500_000,
            out_us: 2_000_000,
            link: None,
        }
    );

    // ...and onto another subtitle track, which is the same call: a drag across
    // lanes is where a second caption row is *for*.
    let s2 = project.add_lane(LaneKind::Subtitle);
    project.move_sub(lane, 0, s2, 300).expect("it changes lane");
    assert!(project.sub_lane(lane).is_empty(), "off the first row");
    assert_eq!(project.sub_lane(s2).len(), 1, "and onto the second");
    assert!(project.undo() && project.undo(), "the move and the added lane");

    // Every one of the four was one undo step, and the snapshots are the lane
    // list's own: subtitles joined the history without a history of their own.
    for want in [
        SubClip {
            start: 45,
            frames: 45,
            track: 0,
            in_us: 500_000,
            out_us: 2_000_000,
            link: None,
        },
        SubClip {
            start: 30,
            frames: 60,
            track: 0,
            in_us: 0,
            out_us: 2_000_000,
            link: None,
        },
        SubClip {
            link: None,
            start: 30,
            ..whole(0)
        },
    ] {
        assert!(project.undo(), "there is a step to walk back");
        assert_eq!(project.sub_lane(lane)[0], want);
    }
    assert!(project.undo(), "and the placement itself");
    assert!(
        project.sub_lane(lane).is_empty(),
        "which leaves the lane as it was added"
    );
}

/// Two captions may not cover one frame of one lane, and every refusal says so
/// in words -- the one place this model *differs* from
/// [`engine::Project::place`], which overwrites what it lands on. A refusal
/// costs no undo step.
#[test]
fn two_captions_may_not_cover_one_frame_and_the_refusal_says_which() {
    let (mut project, lane) = with_subtitle_lane();
    project
        .place_sub(lane, 30, SubClip { frames: 60, ..whole(0) })
        .expect("the first goes down");
    let err = project
        .place_sub(lane, 60, SubClip { frames: 60, ..whole(0) })
        .expect_err("the second lands inside it");
    assert!(
        err.to_string().contains("already covers [30, 90)"),
        "the refusal names the one in the way: {err}"
    );

    project
        .place_sub(lane, 200, SubClip { frames: 60, ..whole(0) })
        .expect("clear of it, it goes down");
    let err = project
        .move_sub(lane, 0, lane, 150)
        .expect_err("and a drag onto it is refused too");
    assert!(
        err.to_string().contains("already covers [200, 260)"),
        "by the same words: {err}"
    );

    // Neither refusal moved anything, and neither cost a step: one undo takes
    // the *second placement* back, not a refusal.
    assert_eq!(
        project.sub_lane(lane).iter().map(|s| s.start).collect::<Vec<_>>(),
        [30, 200]
    );
    assert!(project.undo());
    assert_eq!(
        project.sub_lane(lane).iter().map(|s| s.start).collect::<Vec<_>>(),
        [30]
    );

    // The other refusals of the door, each in its own words.
    for (lane, at, sub, want) in [
        (Lane::V1, 0, whole(0), "not a subtitle track"),
        (Lane::new(LaneKind::Subtitle, 4), 0, whole(0), "there is no S5"),
        (Lane::S1, 0, SubClip { frames: 0, ..whole(0) }, "is empty"),
        (Lane::S1, 0, SubClip { track: 7, ..whole(0) }, "subtitle track 7 of 1"),
    ] {
        let err = project.place_sub(lane, at, sub).expect_err("refused");
        assert!(
            err.to_string().contains(want),
            "{want:?} is what it says, not {err:?}"
        );
    }
}

/// The mapping: what a lane *shows* is its placements' windows of their track,
/// cut to the window and shifted to where the placement sits -- the twin of
/// `export::timeline_cues` for a track that is placed rather than carried.
#[test]
fn a_lane_maps_to_cues_clipped_to_the_window_and_shifted_onto_the_timeline() {
    let (mut project, lane) = with_subtitle_lane();
    // Two seconds of the track, placed two seconds in: the first cue and no
    // more -- the second starts at exactly 2s, which the half-open window ends
    // at.
    project
        .place_sub(
            lane,
            60,
            SubClip {
                frames: 60,
                out_us: 2_000_000,
                ..whole(0)
            },
        )
        .expect("it goes down");
    let cues = project.sub_lane_cues(lane, LANE_FPS);
    assert_eq!(cues.len(), 1, "one cue is inside that window");
    assert_eq!(
        (cues[0].start_us, cues[0].end_us, &*cues[0].text),
        (2_500_000, 3_500_000, "first line"),
        "shifted by the two seconds the placement sits at"
    );

    // A window that starts mid-cue keeps the half that is inside it, exactly as
    // a cut clip keeps the frames that are.
    project.undo();
    project
        .place_sub(
            lane,
            0,
            SubClip {
                frames: 60,
                in_us: 1_000_000,
                out_us: 3_000_000,
                ..whole(0)
            },
        )
        .expect("it goes down");
    let cues = project.sub_lane_cues(lane, LANE_FPS);
    assert_eq!(
        cues.iter()
            .map(|c| (c.start_us, c.end_us, c.text.clone()))
            .collect::<Vec<_>>(),
        [
            (0, 500_000, "first line".to_string()),
            (1_000_000, 2_000_000, "second line\nwith a break".to_string()),
        ],
        "both cues clipped to the window and shifted to the placement"
    );

    // The palette row cannot be taken off under the placement that plays it --
    // the refusal names the lane and the frame it is at.
    let why = project
        .remove_subtitles(0)
        .expect_err("a placed row does not come off")
        .to_string();
    assert!(
        why.contains("S1 at frame 0") && why.contains("delete those clips first"),
        "the refusal names where it is placed: {why}"
    );

    // A placement whose track is gone all the same -- a load that put a shorter
    // palette back, which is the only door left -- shows nothing, never another
    // track's words.
    let project = project.with_subtitles(Vec::new());
    assert!(
        project.sub_lane_cues(lane, LANE_FPS).is_empty(),
        "a placement with no track left shows nothing"
    );
    assert!(
        project.sub_lane_cues(lane, 0.0).is_empty(),
        "and a rate that is not a rate maps nothing"
    );
}

/// A palette row taken off while *another* row is placed: the bug a
/// placement-that-names-an-index invites, and the rule
/// [`engine::Project::remove_source`] already follows -- the rows below the one
/// that went move down, so every clip that named one has to move with them or
/// it plays somebody else's words.
#[test]
fn a_removed_palette_row_walks_the_placements_below_it_down() {
    let mut project = engine::Project::single(FILE, 150);
    let base = subtitle::open(&data("test_subs.srt"), None);
    // Three rows of one file: `add_subtitles` dedupes by (path, track), so
    // three track numbers are three rows without three files.
    for (n, label) in [(0u64, "eng"), (1, "fra - Signs"), (2, "spa")] {
        assert!(
            project.add_subtitles(&SubtitleTrack {
                track: Some(n),
                label: label.into(),
                ..base.clone()
            }),
            "row {label} goes on the palette"
        );
    }
    let lane = project.add_lane(LaneKind::Subtitle);
    project
        .place_sub(lane, 30, whole(1))
        .expect("the middle row is placed");
    assert_eq!(
        project.subtitles()[project.sub_lane(lane)[0].track].label,
        "fra - Signs"
    );

    // The row above it comes off -- nothing plays it -- and the placement is
    // walked down with the palette, so it still says the same words.
    project.remove_subtitles(0).expect("an unplaced row comes off");
    assert_eq!(
        project.subtitles().iter().map(|t| &*t.label).collect::<Vec<_>>(),
        ["fra - Signs", "spa"],
        "the survivors keep their order"
    );
    assert_eq!(project.sub_lane(lane)[0].track, 0, "the placement moved down");
    assert_eq!(
        project.subtitles()[project.sub_lane(lane)[0].track].label,
        "fra - Signs",
        "and names the very track it named before"
    );

    // The history went with it: its snapshots hold the indexes as they were
    // before the reindex, so an undo into one would put the placement back on a
    // track that is no longer there.
    assert!(
        !project.undo(),
        "no step survives a reindex the snapshots do not know about"
    );

    // ...and now that it *is* placed, the same row does not come off at all:
    // the refusal names the lane, the frame, and the way out.
    let why = project
        .remove_subtitles(0)
        .expect_err("a placed row stays")
        .to_string();
    assert!(
        why.contains("fra - Signs") && why.contains("S1 at frame 30"),
        "the refusal names the row and where it plays: {why}"
    );
    assert_eq!(project.subtitles().len(), 2, "and nothing came off");
}

/// A drag that changes nothing is not a mistake to report: a caption picked up
/// and put back down, and an edge pulled past a wall it already stands against,
/// both come back `Ok` and cost no undo step -- a front-end shows every `Err`
/// as a notice, and neither of these two is worth one.
#[test]
fn a_drag_that_changes_nothing_is_ok_and_costs_no_step() {
    let (mut project, lane) = with_subtitle_lane();
    project
        .place_sub(lane, 30, SubClip { frames: 60, ..whole(0) })
        .expect("it goes down");
    let placed = project.sub_lane(lane)[0];

    project
        .move_sub(lane, 0, lane, 30)
        .expect("put back where it was picked up");
    assert_eq!(project.sub_lane(lane)[0], placed, "and nothing moved");

    let (_lo, hi) = project
        .trim_sub_room(lane, 0, Edge::End, LANE_FPS)
        .expect("the room the tail has");
    project
        .trim_sub(lane, 0, Edge::End, hi, LANE_FPS)
        .expect("out to the wall");
    let at_wall = project.sub_lane(lane)[0];
    project
        .trim_sub(lane, 0, Edge::End, hi + 5, LANE_FPS)
        .expect("and past it, which is the wall again");
    assert_eq!(project.sub_lane(lane)[0], at_wall, "the edge stayed put");

    // Two edits happened, so two steps exist: the trim, then the placement.
    assert!(project.undo(), "the trim walks back");
    assert_eq!(project.sub_lane(lane)[0], placed);
    assert!(project.undo(), "and the placement");
    assert!(
        project.sub_lane(lane).is_empty(),
        "the two no-ops left no step of their own"
    );
}

/// The razor and the ripple, which is the whole reason a caption is a lane and
/// not a setting: a cut through the picture cuts the words over it, and the
/// windows follow by proportion -- no frame rate anywhere in that arithmetic.
#[test]
fn a_caption_is_cut_and_rippled_with_the_picture_under_it() {
    let (mut project, lane) = with_subtitle_lane();
    project
        .place_sub(
            lane,
            0,
            SubClip {
                frames: 60,
                out_us: 2_000_000,
                ..whole(0)
            },
        )
        .expect("it goes down");

    // The razor: halves that add up to the whole, in frames and in microseconds.
    assert!(project.split(30), "the cut lands on the picture and the words");
    assert_eq!(
        project
            .sub_lane(lane)
            .iter()
            .map(|s| (s.start, s.frames, s.in_us, s.out_us))
            .collect::<Vec<_>>(),
        [(0, 30, 0, 1_000_000), (30, 30, 1_000_000, 2_000_000)]
    );
    // ...and its inverse puts the caption back exactly as it was.
    assert!(project.regroup(30), "the halves rejoin");
    assert_eq!(
        project.sub_lane(lane).iter().map(|s| (s.start, s.frames, s.in_us, s.out_us)).collect::<Vec<_>>(),
        [(0, 60, 0, 2_000_000)]
    );

    // A rippling delete through the middle of it: the hole closes, what was
    // inside it is gone from the words as well as from the picture, and the two
    // halves keep exactly the seconds they kept frames for.
    assert!(project.ripple_delete(20, 20), "the hole closes on every lane");
    assert_eq!(
        project
            .sub_lane(lane)
            .iter()
            .map(|s| (s.start, s.frames, s.in_us, s.out_us))
            .collect::<Vec<_>>(),
        [(0, 20, 0, 666_666), (20, 20, 1_333_333, 2_000_000)],
        "cut by proportion of the window, not by a frame rate"
    );
    assert!(project.undo(), "and the whole ripple is one step");
    assert_eq!(project.sub_lane(lane).len(), 1);
}

/// An EBML element: its id as it is written in the file, then an 8-byte
/// length, which is always a legal encoding of one.
fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    let mut len = (body.len() as u64).to_be_bytes();
    len[0] |= 0x01; // the 8-byte length marker
    out.extend_from_slice(&len);
    out.extend_from_slice(body);
    out
}

/// A Matroska file carrying four PGS subtitle tracks, the first of them with
/// `first` in a block at 500 ms and the erase that ends it at 2 s. No `Info`,
/// so the timestamp scale is the spec's default of a millisecond a tick.
fn pgs_matroska(first: Vec<u8>) -> Vec<u8> {
    let mut tracks = Vec::new();
    for (number, language) in [(1u8, "eng"), (2, "fra"), (3, "spa"), (4, "")] {
        let mut entry = Vec::new();
        entry.extend(element(&[0xD7], &[number])); // TrackNumber
        entry.extend(element(&[0x83], &[0x11])); // TrackType: subtitle
        entry.extend(element(&[0x86], b"S_HDMV/PGS")); // CodecID
        if !language.is_empty() {
            entry.extend(element(&[0x22, 0xB5, 0x9C], language.as_bytes()));
        }
        tracks.extend(element(&[0xAE], &entry)); // TrackEntry
    }
    // A `SimpleBlock` on track 1: the track number as a one-byte vint, the
    // timestamp relative to the cluster's, and no flags.
    let block = |at: i16, data: Vec<u8>| {
        let mut body = vec![0x81, (at >> 8) as u8, at as u8, 0x00];
        body.extend(data);
        element(&[0xA3], &body)
    };
    let mut cluster = element(&[0xE7], &[0x00]); // Timestamp: 0
    cluster.extend(block(500, first));
    cluster.extend(block(2000, pgs_erase()));

    let mut segment = element(&[0x16, 0x54, 0xAE, 0x6B], &tracks); // Tracks
    segment.extend(element(&[0x1F, 0x43, 0xB6, 0x75], &cluster)); // Cluster
    element(&[0x18, 0x53, 0x80, 0x67], &segment) // Segment
}

/// One PGS segment as a Matroska block holds it: the type, a big-endian size,
/// the body -- and none of the `PG` magic or timestamps a `.sup` file writes,
/// which the muxer takes off.
fn pgs_segment(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// A display set that paints a 2x2 white square at (1, 1) of an 8x4 canvas:
/// the composition, the palette it colours by, the run-length object itself,
/// and the end of the set.
fn pgs_display_set() -> Vec<u8> {
    // Canvas 8x4, a frame-rate byte, composition 0, epoch start, no palette
    // update, palette 0 -- and one object, id 0, at (1, 1) with no flags.
    #[rustfmt::skip]
    let pcs = [
        0, 8, 0, 4, 0x10, 0, 0, 0x80, 0, 0, 1,
        0, 0, 0, 0, 0, 1, 0, 1,
    ];
    // Palette 0, version 0, then colour 1: white at full opacity, in the
    // limited-range YCbCr a disc writes (index, Y, Cr, Cb, A). Colour 0 is the
    // transparent one every palette starts with.
    let pds = [0, 0, 1, 235, 128, 128, 255];
    // Two rows of "two pixels of colour 1, end of line", which is the shortest
    // run PGS can write and still be a picture.
    let rle = [0x00, 0x82, 0x01, 0x00, 0x00, 0x00, 0x82, 0x01, 0x00, 0x00];
    // Object 0, version 0, first *and* last fragment of the sequence; then the
    // length of what follows, and the object's own 2x2 box.
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend_from_slice(&[0, 0, (4 + rle.len()) as u8, 0, 2, 0, 2]);
    ods.extend_from_slice(&rle);

    let mut set = pgs_segment(0x16, &pcs);
    set.extend(pgs_segment(0x14, &pds));
    set.extend(pgs_segment(0x15, &ods));
    set.extend(pgs_segment(0x80, &[]));
    set
}

/// The display set that composes nothing: the same canvas with no object on
/// it, which is how a disc says the picture before it is over.
fn pgs_erase() -> Vec<u8> {
    let mut set = pgs_segment(0x16, &[0, 8, 0, 4, 0x10, 0, 1, 0, 0, 0, 0]);
    set.extend(pgs_segment(0x80, &[]));
    set
}

/// The v15 lines: a subtitle lane is written in its place among the others, a
/// caption on it is one `sub` line -- both clocks and the palette row it names
/// -- and an empty one is the bare line a video lane's is. The whole of it
/// comes back the placement it was, and a re-save is the same bytes, which is
/// what "the file is the timeline" means for words as much as for pictures.
#[test]
fn subtitle_lanes_and_their_captions_survive_a_save() {
    let dir = scratch("lanes_save");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    let subs = dir.join("test_subs.srt");
    std::fs::copy(data("test_subs.srt"), &subs).expect("copy the subtitle fixture");

    let mut session = engine::PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0); // silent like the rest of the suites
    assert_eq!(session.import_subtitles(&subs).expect("the .srt imports"), 1);

    let s1 = session.add_lane(LaneKind::Subtitle);
    let s2 = session.add_lane(LaneKind::Subtitle);
    let caption = SubClip {
        start: 0,
        frames: 45,
        track: 0,
        in_us: 500_000,
        out_us: 1_500_000,
        link: None,
    };
    session.place_sub(s1, 10, caption).expect("a caption goes down");

    let project = dir.join("cut.edith");
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).expect("read the project back");
    assert!(text.starts_with("edith 16\n"), "{text}");
    assert!(
        text.contains("\nsub 1 10 45 0 500000 1500000\n"),
        "the placement is one line: where it sits, how long, whose words, and \
         the window of them in microseconds: {text}"
    );
    assert!(
        text.contains("\nsub 2\n"),
        "and an empty subtitle lane says so on a line of its own: {text}"
    );

    let back = engine::PlaybackSession::open_project(&project).expect("open the project");
    assert_eq!(
        back.subtitle_lanes(),
        [s1, s2],
        "both lanes come back, in the order they were laid out"
    );
    assert_eq!(
        back.sub_lane(s1),
        [SubClip { start: 10, ..caption }],
        "and the caption is the very placement it was"
    );
    assert!(back.sub_lane(s2).is_empty(), "the empty one is still empty");
    assert_eq!(back.subtitles()[0].cues, expected(), "palette intact");

    // ...and saving what was loaded writes the file it was loaded from.
    let again = dir.join("cut2.edith");
    back.save_project(&again).expect("save again");
    assert_eq!(
        std::fs::read_to_string(&again).expect("read"),
        text,
        "a load and a re-save is the same file"
    );
}

/// The dialect before the lanes: a v14 project names its subtitle files and
/// places none of them -- there was no line to place one on -- so it loads with
/// its palette whole and no subtitle lane at all, and re-saves as v15 with the
/// same one `subtitle` line and nothing on any lane.
#[test]
fn a_version_14_project_keeps_its_palette_and_places_nothing() {
    let dir = scratch("v14");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    let srt = dir.join("test_subs.srt");
    std::fs::copy(data("test_subs.srt"), &srt).expect("copy the subtitle fixture");
    let path = dir.join("old.edith");
    std::fs::write(
        &path,
        "edith 14\nplayhead 0\nresolution 1280 720\nfps 30.0\nsource 0 test_av.mp4\n\
         subtitle - test_subs.srt\nvideo 1 0 0 30 0 0 - - fit 1000\n\
         audio 1 0 0 30 0 0 - - fit 1000\n",
    )
    .expect("write a v14 file");

    let loaded = engine::PlaybackSession::open_project(&path).expect("a v14 file still opens");
    assert_eq!(loaded.subtitles().len(), 1, "the palette row is there");
    assert_eq!(loaded.subtitles()[0].cues, expected(), "cues and all");
    assert!(
        loaded.subtitle_lanes().is_empty(),
        "and nothing of it is placed: that dialect had nowhere to place it"
    );

    let now = dir.join("new.edith");
    loaded.save_project(&now).expect("save");
    let text = std::fs::read_to_string(&now).expect("read back");
    assert!(text.starts_with("edith 16\n"), "{text}");
    assert!(
        text.contains("\nsubtitle - test_subs.srt\n"),
        "the row survives the migration: {text}"
    );
    assert!(
        !text.contains("\nsub "),
        "and no lane and no placement were invented for it: {text}"
    );
}

/// The palette bound, which is a clip's source bound for words: a caption names
/// a `subtitle` line by position, so one naming a row that is not there is
/// refused -- at the parser, which names the line, and at
/// [`engine::Project::with_subs`], which names the lane. Neither is a file this
/// editor wrote; both are files it may be handed.
#[test]
fn a_caption_naming_a_track_the_palette_does_not_have_is_refused() {
    let dir = scratch("badtrack");
    let media = dir.join("test_av.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the media fixture");
    std::fs::copy(data("test_subs.srt"), dir.join("test_subs.srt")).expect("copy the subtitles");
    let path = dir.join("bad.edith");
    std::fs::write(
        &path,
        "edith 16\nplayhead 0\nresolution 1280 720\nfps 30.0\nsource 0 test_av.mp4\n\
         subtitle - test_subs.srt\nvideo 1 0 0 30 0 0 - - fit 1000\n\
         audio 1 0 0 30 0 0 - - fit 1000\nsub 1 0 45 3 0 1000000\n",
    )
    .expect("write it");
    let err = engine::PlaybackSession::open_project(&path)
        .err()
        .expect("a caption may not name a row that is not there")
        .to_string();
    assert_eq!(err, "line 9: caption names subtitle track 3 of 1");

    // An empty placement is refused by the same door, at either clock.
    let empty = dir.join("empty.edith");
    std::fs::write(
        &empty,
        "edith 16\nplayhead 0\nresolution 1280 720\nfps 30.0\nsource 0 test_av.mp4\n\
         subtitle - test_subs.srt\nvideo 1 0 0 30 0 0 - - fit 1000\n\
         audio 1 0 0 30 0 0 - - fit 1000\nsub 1 0 45 0 1000000 1000000\n",
    )
    .expect("write it");
    let err = engine::PlaybackSession::open_project(&empty)
        .err()
        .expect("a window of nothing is not a caption")
        .to_string();
    assert_eq!(
        err,
        "line 9: caption at 0 is empty: 45 frames of [1000000, 1000000)"
    );

    // ...and the load's own door says the same thing about the same file, by
    // the lane rather than by the line: `from_parts` for words.
    let (project, _lane) = with_subtitle_lane();
    let beyond = SubClip { track: 3, ..whole(0) };
    let refused = project
        .clone()
        .with_subs(vec![Vec::new(), Vec::new(), vec![beyond]])
        .expect_err("the palette holds one row")
        .to_string();
    assert_eq!(refused, "S1 caption at 0 names subtitle track 3 of 1");

    // A caption on a lane that is not a subtitle lane is refused there too --
    // the media lanes hold pictures and sound, and never words.
    let wrong = project
        .with_subs(vec![vec![whole(0)], Vec::new(), Vec::new()])
        .expect_err("V1 is not a subtitle track")
        .to_string();
    assert!(
        wrong.starts_with("V1 is not a subtitle track and holds 1 caption(s)"),
        "{wrong}"
    );
}
