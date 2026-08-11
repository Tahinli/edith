//! Subtitles: the cues of a file beside the media and of a track inside it are
//! the same cues, a project keeps them across a save, and the formats that are
//! pictures rather than text are listed and skipped by name rather than
//! silently dropped.
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
        },
        Cue {
            start_us: 2_000_000,
            end_us: 3_250_000,
            text: "second line\nwith a break".into(),
        },
        Cue {
            start_us: 4_000_000,
            end_us: 4_750_000,
            text: "third line".into(),
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

/// A subtitle track this cannot read is a row that says why -- never an error
/// that takes the whole file down with it. The repro is a BluRay remux
/// carrying four PGS tracks: opening it lists four subtitles it cannot draw,
/// and the picture and the sound come up regardless.
///
/// Written here rather than muxed by the fixture script because ffmpeg cannot
/// encode text subtitles into pictures; the bytes below are the `Tracks`
/// element of such a file and nothing else.
#[test]
fn bitmap_subtitles_are_listed_and_skipped_by_name() {
    let dir = scratch("pgs");
    let file = dir.join("remux.mkv");
    std::fs::write(&file, pgs_matroska()).expect("write the hand-made mkv");

    let tracks = subtitle::of_matroska(&file).expect("the walk does not fail over them");
    assert_eq!(tracks.len(), 4, "every PGS track is listed");
    for (i, track) in tracks.iter().enumerate() {
        assert_eq!(track.track, Some(i as u64 + 1));
        assert_eq!(track.cues, Vec::new(), "no cues to have");
        let why = track.refused.clone().unwrap_or_default();
        assert!(
            why.contains("S_HDMV/PGS") && why.contains("pictures"),
            "the refusal names the codec: {why:?}"
        );
    }
    // The language is what a list shows, and `und` is what the spec says a
    // track that names none is.
    assert_eq!(tracks[0].label, "eng");
    assert_eq!(tracks[3].label, "und");
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

/// The `Tracks` element of a Matroska file carrying four PGS subtitle tracks
/// and nothing else -- enough for the walk, which stops at the first `Cluster`
/// and there is none.
fn pgs_matroska() -> Vec<u8> {
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
    let segment = element(&[0x16, 0x54, 0xAE, 0x6B], &tracks); // Tracks
    element(&[0x18, 0x53, 0x80, 0x67], &segment) // Segment
}
