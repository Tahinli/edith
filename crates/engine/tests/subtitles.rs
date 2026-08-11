//! Subtitles: the cues of a file beside the media and of a track inside it are
//! the same cues, a project keeps them across a save, and a track that is
//! pictures rather than text is read as pictures rather than listed and
//! skipped.
//!
//! ```text
//! cargo test -p engine --release --test subtitles -- --nocapture
//! ```

use std::path::PathBuf;

use engine::subtitle::{self, Cue};

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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ve_subs_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::canonicalize(&dir).expect("canonical scratch dir")
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
/// a 4K remux and lives in a local folder, not in this repository.
#[test]
fn the_remux_that_reported_the_bug_reads_all_five_of_its_tracks() {
    let film = std::path::Path::new(
        "/path/to/a-real-4k-pgs-film.mkv",
    );
    if !film.exists() {
        eprintln!("skipped: {} is not here", film.display());
        return;
    }
    let tracks = subtitle::of_matroska(film).expect("the walk");
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
    assert!(text.starts_with("edith 10\n"), "{text}");
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
