//! Subtitles leave with the file, whichever file: a Matroska export carries
//! every picked track as `S_TEXT/UTF8` and an mp4 carries it as `tx3g` timed
//! text, both timed on the *exported* timeline -- so a cut moves the cues with
//! the pictures they belong to in either container.
//!
//! Two witnesses for every cue: this project's own Matroska reader
//! (`demux::matroska_subtitles`, the way an exported file comes back into a
//! project) and, where the box has it, ffmpeg -- which is what "it plays
//! elsewhere" means and the only thing that can say this file is not merely
//! self-consistent.
//!
//! ```text
//! cargo test -p engine --release --test export_subs -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::Lane;
use engine::scratch::Scratch;
use engine::subtitle::{self, Cue, SubtitleTrack};
use engine::{ExportHandle, Project};

/// The media fixture: five seconds at 30 fps, and a picture this box can decode
/// with nothing installed.
const MEDIA: &str = "test_av.mp4";
const FRAMES: u32 = 150;
/// What a cue may be out by: the file's own tick, a millisecond. The mapping is
/// exact microseconds -- only the write rounds -- so nothing wider than this is
/// being allowed for.
const SLACK_US: i64 = 1_000;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str) -> Scratch {
    Scratch::file(&format!("ve_subs_export_{name}"), "mkv")
}

fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
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

/// The three cues the subtitle fixtures hold (`tests/subtitles.rs` pins them
/// against the real `.srt`, `.ass` and `.mkv`), on a timeline of the media they
/// are timed against.
///
/// The track says it came out of *this* media file, which is what an embedded
/// track is and what makes its cues the file's own clock rather than the
/// timeline's. Written out here rather than read out of `test_subs.mkv` because
/// that fixture's picture is AV1 and there is no software AV1 decoder to export
/// it through; the cues are the same three either way.
fn track() -> SubtitleTrack {
    SubtitleTrack {
        path: asset(MEDIA),
        track: Some(2),
        language: "eng".into(),
        name: String::new(),
        label: "eng".into(),
        bitmap: false,
        cues: vec![
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
        ],
        refused: None,
    }
}

fn project() -> Project {
    Project::single(asset(MEDIA), FRAMES).with_subtitles(vec![track()])
}

/// The two subtitle tracks `assets/test_subs.mkv` really carries -- `eng` as
/// `S_TEXT/UTF8` and `fra` (titled `Signs`) as `S_TEXT/ASS`, the same three cues
/// twice -- read back out of it the way an import reads them, and repointed at
/// the media this box can encode: that fixture's own picture is AV1 and there is
/// no software AV1 decoder to export it through.
fn both_tracks() -> Vec<SubtitleTrack> {
    let mut tracks = subtitle::of_matroska(&asset("test_subs.mkv")).expect("read the fixture");
    assert_eq!(
        tracks.len(),
        2,
        "the fixture carries two tracks: {tracks:?}"
    );
    for track in &mut tracks {
        track.path = asset(MEDIA);
    }
    tracks
}

/// Exports `project` as an HEVC Matroska carrying the subtitle tracks `picks`,
/// and hands back what our own reader finds in the file -- one track per pick,
/// in the order they were picked.
fn exported(name: &str, project: Project, picks: &[usize]) -> (Scratch, Vec<SubtitleTrack>) {
    pin_software();
    let out = out_path(name);
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Hevc,
            subtitles: picks.to_vec(),
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(300)).expect("the export finishes");
    let tracks = subtitle::of_matroska(&out).expect("read the export back");
    assert_eq!(
        tracks.len(),
        picks.len(),
        "exactly the picked tracks: {tracks:?}"
    );
    for track in &tracks {
        assert_eq!(track.refused, None, "the track reads back as text");
    }
    (out, tracks)
}

/// The same, as an **mp4**: the picked tracks leave as `tx3g` timed text, which
/// is what an mp4 says a subtitle with. Nothing here reads the file back with
/// this project's own reader -- there is no mp4 subtitle reader to read it with
/// -- so ffmpeg is the witness and the caller asks it.
fn exported_mp4(name: &str, project: Project, picks: &[usize]) -> Scratch {
    pin_software();
    let out = Scratch::file(&format!("ve_subs_export_{name}"), "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Mp4,
            subtitles: picks.to_vec(),
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(300)).expect("the export finishes");
    out
}

/// What ffmpeg makes of the same file's subtitle track, as an SRT on stdout:
/// a second implementation reading the bytes, which is the whole point of it.
/// `None` where there is no ffmpeg on the box -- the test is not about this
/// machine's packages.
fn ffmpeg_srt(path: &Path, which: usize) -> Option<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:s:{which}"), "-f", "srt", "-"])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "ffmpeg refused the exported subtitles: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What streams ffmpeg lists in the file, as it prints them -- `ffmpeg -i` with
/// no output, whose exit status is an error by design, so only the stream lines
/// are read. That listing carries each track's *language* beside its type
/// (`Stream #0:3(fra): Subtitle: subrip`), which is the second implementation's
/// word on what language the file says it is in. `None` where there is no
/// ffmpeg on the box.
fn ffmpeg_streams(path: &Path) -> Option<String> {
    let out = Command::new("ffmpeg").arg("-i").arg(path).output().ok()?;
    let listing = String::from_utf8_lossy(&out.stderr).into_owned();
    println!("ffmpeg sees:\n{listing}");
    Some(listing)
}

fn near(got: i64, want: i64, what: &str) {
    assert!(
        (got - want).abs() <= SLACK_US,
        "{what}: {got} us is not within a frame of {want} us"
    );
}

/// The whole timeline, untouched: the file comes back with the very cues the
/// source's track holds, at the times it holds them -- and ffmpeg reads the
/// same three lines out of it.
#[test]
fn an_mkv_export_carries_the_picked_subtitle_track() {
    let (out, tracks) = exported("plain", project(), &[0]);
    let cues = &tracks[0].cues;
    assert_eq!(cues.len(), 3, "three cues: {cues:?}");
    let want = [
        (500_000, 1_500_000, "first line"),
        (2_000_000, 3_250_000, "second line\nwith a break"),
        (4_000_000, 4_750_000, "third line"),
    ];
    for (cue, (start, end, text)) in cues.iter().zip(want) {
        near(cue.start_us, start, "cue start");
        near(cue.end_us, end, "cue end");
        assert_eq!(cue.text, text, "the text travels, line breaks and all");
    }
    // ...and the track is named the way it was named in the source, so the
    // subtitle menu of whatever plays this says `eng` too.
    let tracks = subtitle::of_matroska(&out).expect("read back");
    assert_eq!(tracks[0].label, "eng");
    let raw = engine::demux::matroska_subtitles(&out).expect("walk the tracks element");
    assert_eq!(raw[0].codec, "S_TEXT/UTF8");
    assert_eq!(raw[0].language, "eng", "the label went in as a language");

    match ffmpeg_srt(&out, 0) {
        Some(srt) => {
            println!("ffmpeg reads the exported track back as:\n{srt}");
            assert!(srt.contains("first line"), "{srt}");
            assert!(srt.contains("with a break"), "{srt}");
            assert!(srt.contains("00:00:00,500 --> 00:00:01,500"), "{srt}");
            assert!(srt.contains("00:00:04,000 --> 00:00:04,750"), "{srt}");
        }
        None => println!("no ffmpeg on this box: our own reader is the only witness"),
    }
    std::fs::remove_file(&out).unwrap();
}

/// A cut timeline: `[0.5s, 2.5s)` rippled out of it, which is one cue gone
/// entirely, one that straddles the far edge and keeps the half still there,
/// and one that simply moves back with the picture.
#[test]
fn a_cut_moves_the_cues_with_the_pictures() {
    let mut project = project();
    assert!(project.ripple_delete(15, 60), "cut 0.5s..2.5s out");
    assert_eq!(project.timeline_frames(), 90, "three seconds are left");
    assert_eq!(
        project.lane(Lane::V1).len(),
        2,
        "the hole closed, over two placements of the one source"
    );

    let (out, tracks) = exported("cut", project, &[0]);
    let cues = &tracks[0].cues;
    assert_eq!(
        cues.len(),
        2,
        "the cue inside the cut is gone, the other two are not: {cues:?}"
    );
    // 2.0s..3.25s of the source, of which 2.5s on survived, landing where the
    // cut left it: 0.5s..1.25s.
    near(
        cues[0].start_us,
        500_000,
        "the straddling cue starts at the cut",
    );
    near(
        cues[0].end_us,
        1_250_000,
        "...and keeps the rest of its length",
    );
    assert_eq!(cues[0].text, "second line\nwith a break");
    // 4.0s..4.75s of the source, two seconds back.
    near(
        cues[1].start_us,
        2_000_000,
        "the last cue moves back by the cut",
    );
    near(cues[1].end_us, 2_750_000, "...end with it");
    assert_eq!(cues[1].text, "third line");

    if let Some(srt) = ffmpeg_srt(&out, 0) {
        println!("ffmpeg reads the cut export back as:\n{srt}");
        assert!(
            !srt.contains("first line"),
            "the cut cue is not there: {srt}"
        );
        assert!(srt.contains("00:00:00,500 --> 00:00:01,250"), "{srt}");
        assert!(srt.contains("00:00:02,000 --> 00:00:02,750"), "{srt}");
        assert!(srt.contains("third line"), "{srt}");
    }
    std::fs::remove_file(&out).unwrap();
}

/// Nothing picked is nothing written -- the file every export of a timeline
/// without subtitles has always been.
#[test]
fn an_export_with_no_pick_carries_no_subtitle_track() {
    // `exported` asserts the file has no subtitle track at all; this is the
    // pick being absent rather than the timeline having nothing to pick.
    let (out, tracks) = exported("none", project(), &[]);
    assert!(tracks.is_empty());
    std::fs::remove_file(&out).unwrap();
}

/// As many tracks as the timeline holds, not one: both of a film's own subtitle
/// tracks go into the export, each numbered and named for itself, and a player
/// -- ffmpeg here -- offers the two of them.
#[test]
fn every_picked_track_travels_with_its_own_name() {
    let project = Project::single(asset(MEDIA), FRAMES).with_subtitles(both_tracks());
    let (out, tracks) = exported("both", project, &[0, 1]);
    // The labels the fixture states: a language on one, a language and a muxer's
    // title on the other, and neither is the other's.
    assert_eq!(tracks[0].label, "eng");
    assert_eq!(tracks[1].label, "fra — Signs");
    for track in &tracks {
        assert_eq!(track.cues.len(), 3, "three cues each: {track:?}");
        near(track.cues[0].start_us, 500_000, "cue start");
        near(track.cues[2].end_us, 4_750_000, "cue end");
        assert_eq!(
            track.cues[2].text, "third line",
            "the ASS markup is resolved to text, not carried into the file"
        );
    }

    // In the file itself: two text tracks, numbered from 3 up, and the second
    // one's number is what its blocks name -- the reason the cues above came
    // back apart rather than doubled onto one track.
    let raw = engine::demux::matroska_subtitles(&out).expect("walk the tracks element");
    assert_eq!(
        raw.iter().map(|t| t.number).collect::<Vec<_>>(),
        vec![3, 4],
        "the picture is 1, the sound 2, the text 3 and 4"
    );
    assert!(
        raw.iter().all(|t| t.codec == "S_TEXT/UTF8"),
        "both are written as text: {raw:?}"
    );
    assert_eq!(raw[0].language, "eng", "a three-letter label is a language");
    // The language and the title are two fields, not one string: a track that
    // says `fra` is what a player's "French" setting finds, and a track that
    // says only `Signs` is an *English* track to every reader, Matroska's
    // default language being `eng`.
    assert_eq!(raw[1].language, "fra", "the French track says so in the file");
    assert_eq!(raw[1].name, "Signs", "...and keeps its own title beside it");
    assert_eq!(raw[1].codec, "S_TEXT/UTF8");

    match ffmpeg_streams(&out) {
        Some(listing) => {
            let n = listing.matches("Subtitle:").count();
            assert_eq!(n, 2, "ffmpeg lists both subtitle streams");
            // The other implementation reads the same two languages -- this is
            // the assertion that used to come back `(eng)` for the French one.
            assert!(
                listing.contains("(eng): Subtitle") && listing.contains("(fra): Subtitle"),
                "ffmpeg sees one English and one French track: {listing}"
            );
            let second = ffmpeg_srt(&out, 1).expect("ffmpeg is here");
            println!("ffmpeg reads the second track back as:\n{second}");
            assert!(second.contains("third line"), "{second}");
            assert!(second.contains("00:00:00,500 --> 00:00:01,500"), "{second}");
        }
        None => println!("no ffmpeg on this box: our own reader is the only witness"),
    }
    std::fs::remove_file(&out).unwrap();
}

/// The mp4 half of [`every_picked_track_travels_with_its_own_name`]: the very
/// same two tracks into the container the export *defaults* to, where they used
/// to be dropped with a card saying an mp4 carried none. ffmpeg is the whole
/// witness here -- a second implementation reading the bytes -- and it reads two
/// `mov_text` streams, one English and one French, the French one still titled
/// `Signs`, with the three cues at the three times.
#[test]
fn an_mp4_export_carries_the_picked_tracks_as_timed_text() {
    let project = Project::single(asset(MEDIA), FRAMES).with_subtitles(both_tracks());
    let out = exported_mp4("mp4_both", project, &[0, 1]);
    let Some(listing) = ffmpeg_streams(&out) else {
        println!("no ffmpeg on this box: an mp4's tx3g has no other reader here");
        std::fs::remove_file(&out).unwrap();
        return;
    };
    assert_eq!(
        listing.matches("Subtitle: mov_text").count(),
        2,
        "two timed-text streams, not none and not one: {listing}"
    );
    // The language is the `mdhd` field of each track, which is what a player's
    // subtitle menu offers -- the mp4 spelling of the Matroska `Language` the
    // sibling test pins.
    assert!(
        listing.contains("(eng): Subtitle") && listing.contains("(fra): Subtitle"),
        "one English and one French: {listing}"
    );
    // ...and the title beside it, which an mp4 states in a `trak/udta/name` box
    // that `mp4 0.14` has no field for and this project patches in.
    assert!(
        listing.contains("name            : Signs"),
        "the French track keeps its own title: {listing}"
    );
    for which in 0..2 {
        let srt = ffmpeg_srt(&out, which).expect("ffmpeg is here");
        println!("ffmpeg reads mp4 track {which} back as:\n{srt}");
        assert!(srt.contains("00:00:00,500 --> 00:00:01,500"), "{srt}");
        assert!(srt.contains("first line"), "{srt}");
        // The line break inside a cue survives a sample that states its length
        // in bytes and nothing else.
        assert!(srt.contains("with a break"), "{srt}");
        assert!(srt.contains("00:00:04,000 --> 00:00:04,750"), "{srt}");
        assert!(srt.contains("third line"), "{srt}");
    }
    std::fs::remove_file(&out).unwrap();
}

/// The pair a container states, carried through as a *pair*, into both
/// containers: a track's language and its title are two fields at every step
/// from the demuxer to the muxer, and neither is derived from the other.
///
/// The two tracks here are the shapes that used to be lost. `fra` titled `Signs`
/// is the one every multi-language export declared English -- the two were
/// flattened into one label and split back apart by guessing, and a title that
/// was not three lowercase letters took the language down with it. `und` titled
/// `Commentary` is the same fault from the other side: a track that states its
/// language is undetermined said nothing at all in the file, and a Matroska
/// track with no `Language` *is* an English one by spec.
///
/// Read back with this project's own readers -- ffmpeg is no witness for `und`,
/// which both its demuxers drop from the listing rather than print.
#[test]
fn a_language_and_a_title_are_two_fields_in_both_containers() {
    let named = |language: &str, name: &str| SubtitleTrack {
        language: language.into(),
        name: name.into(),
        label: format!("{language} — {name}"),
        ..track()
    };
    let tracks = vec![named("fra", "Signs"), named("und", "Commentary")];
    let project = || Project::single(asset(MEDIA), FRAMES).with_subtitles(tracks.clone());

    let (out, read) = exported("pair", project(), &[0, 1]);
    let raw = engine::demux::matroska_subtitles(&out).expect("walk the tracks element");
    assert_eq!(
        raw.iter()
            .map(|t| (&t.language[..], &t.name[..]))
            .collect::<Vec<_>>(),
        vec![("fra", "Signs"), ("und", "Commentary")],
        "the Matroska keeps both fields of both tracks"
    );
    // ...and the label a row shows is built back out of them, not stored:
    // an undetermined language is no language to a reader.
    assert_eq!(read[0].label, "fra — Signs");
    assert_eq!(read[1].label, "Commentary");
    std::fs::remove_file(&out).unwrap();

    let out = exported_mp4("mp4_pair", project(), &[0, 1]);
    let read = subtitle::of_mp4(&out).expect("read the mp4 back");
    assert_eq!(
        read.iter()
            .map(|t| (&t.language[..], &t.name[..]))
            .collect::<Vec<_>>(),
        vec![("fra", "Signs"), ("und", "Commentary")],
        "and so does the mp4, in its `mdhd` and its `udta/name`"
    );
    for track in &read {
        assert_eq!(track.cues.len(), 3, "three cues each: {track:?}");
    }
    std::fs::remove_file(&out).unwrap();
}

/// The other mp4: an HEVC one, whose *video* sample entry is rewritten by hand
/// after the file is closed (`mux::patch_entry` turns the crate's `avc1` into
/// `hvc1`) -- the same rebuild of `moov` the subtitle track's title is patched
/// into. The two patches are one walk, so this is the file that says they do not
/// tread on each other: ffmpeg reads an HEVC picture *and* the timed text.
#[test]
fn an_hevc_mp4_keeps_its_rewritten_entry_and_its_text() {
    pin_software();
    let out = Scratch::file("ve_subs_export_hevc_mp4", "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = engine::export::start(
        Project::single(asset(MEDIA), FRAMES).with_subtitles(both_tracks()),
        meta,
        &out,
        &ExportSettings {
            format: Format::HevcMp4,
            subtitles: vec![1],
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(300)).expect("the export finishes");
    let Some(listing) = ffmpeg_streams(&out) else {
        println!("no ffmpeg on this box: an mp4's tx3g has no other reader here");
        std::fs::remove_file(&out).unwrap();
        return;
    };
    assert!(listing.contains("Video: hevc"), "the entry patch held: {listing}");
    assert!(
        listing.contains("(fra): Subtitle: mov_text"),
        "...and the text track beside it: {listing}"
    );
    let srt = ffmpeg_srt(&out, 0).expect("ffmpeg is here");
    assert!(srt.contains("00:00:00,500 --> 00:00:01,500"), "{srt}");
    assert!(srt.contains("third line"), "{srt}");
    std::fs::remove_file(&out).unwrap();
}

/// ...and the cut lands in an mp4 where it lands in a Matroska: the samples of a
/// timed-text track are timed by their own durations, so a cue that moved with
/// its pictures has to come out moved here too.
#[test]
fn a_cut_moves_the_mp4_cues_with_the_pictures() {
    let mut project = project();
    assert!(project.ripple_delete(15, 60), "cut 0.5s..2.5s out");
    let out = exported_mp4("mp4_cut", project, &[0]);
    let Some(srt) = ffmpeg_srt(&out, 0) else {
        println!("no ffmpeg on this box: an mp4's tx3g has no other reader here");
        std::fs::remove_file(&out).unwrap();
        return;
    };
    println!("ffmpeg reads the cut mp4 export back as:\n{srt}");
    assert!(!srt.contains("first line"), "the cut cue is not there: {srt}");
    assert!(srt.contains("00:00:00,500 --> 00:00:01,250"), "{srt}");
    assert!(srt.contains("00:00:02,000 --> 00:00:02,750"), "{srt}");
    assert!(srt.contains("third line"), "{srt}");
    std::fs::remove_file(&out).unwrap();
}

/// The ceiling itself, from underneath: a file carrying exactly
/// `MAX_SUB_TRACKS` tracks comes back whole. The last of them is track 126,
/// whose block byte is `0xFE` -- one under the all-ones `0xFF` that EBML spells
/// *unknown* with, which is what a 127th track's blocks would say and why its
/// cues would come back nowhere. Read back with our own reader, so the file is
/// one edith can re-import and not merely write.
#[test]
fn the_last_numberable_track_keeps_its_cues() {
    let full = engine::mux::MAX_SUB_TRACKS;
    let picks: Vec<usize> = (0..full).collect();
    let project =
        Project::single(asset(MEDIA), FRAMES).with_subtitles((0..full).map(|_| track()).collect());
    let (out, tracks) = exported("ceiling_at", project, &picks);
    let empty: Vec<usize> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.cues.is_empty())
        .map(|(i, _)| i)
        .collect();
    assert!(
        empty.is_empty(),
        "every one of the {full} tracks keeps its three cues; these came back with none: {empty:?}"
    );
    let raw = engine::demux::matroska_subtitles(&out).expect("walk the tracks element");
    assert_eq!(
        raw.iter().map(|t| t.number).collect::<Vec<_>>(),
        (3..3 + full as u64).collect::<Vec<_>>(),
        "numbered 3 up to the last one a block byte can name"
    );
    assert_eq!(raw.last().map(|t| t.number), Some(126));
    std::fs::remove_file(&out).unwrap();
}

/// The ceiling is said out loud: a Matroska block names its track in one byte,
/// so past `MAX_SUB_TRACKS` the export refuses by name instead of writing a byte
/// that means some other track -- and leaves no file behind.
#[test]
fn more_tracks_than_a_block_can_number_are_refused_by_name() {
    pin_software();
    let over = engine::mux::MAX_SUB_TRACKS + 1;
    let project =
        Project::single(asset(MEDIA), FRAMES).with_subtitles((0..over).map(|_| track()).collect());
    let out = out_path("ceiling");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Hevc,
            subtitles: (0..over).collect(),
            ..Default::default()
        },
    );
    let said = wait(&handle, Duration::from_secs(300))
        .expect_err("a file that cannot be numbered is not written")
        .to_string();
    assert!(
        said.contains(&format!("{over} subtitle tracks"))
            && said.contains(&format!("at most {}", engine::mux::MAX_SUB_TRACKS)),
        "the refusal says how many were asked for and how many fit: {said}"
    );
    assert!(!out.exists(), "and no half-written file is left: {said}");
}

/// A language stated the modern way (`LanguageBCP47`, no legacy element) leaves
/// with the file, in the element every reader looks at.
///
/// The loss used to survive the export: his 22-track Matroska listed
/// `2,subrip,English (UK) (SDH)` with an *empty* language field beside
/// neighbours that kept `ara` and `chi`, because the source stated `en` in an
/// element nothing here read. What is written is still the legacy element -- a
/// three-letter code is what Matroska's `Language` and an mp4's `mdhd` hold, and
/// ffmpeg 8.1.2 reads no `LanguageBCP47` at all.
#[test]
fn a_language_stated_the_modern_way_leaves_with_the_file() {
    let mut tracks =
        subtitle::of_matroska(&asset("test_subs_bcp47.mkv")).expect("read the fixture");
    assert_eq!(
        tracks.len(),
        2,
        "the fixture carries two tracks: {tracks:?}"
    );
    for track in &mut tracks {
        track.path = asset(MEDIA);
    }
    assert_eq!(
        tracks.iter().map(|t| &*t.language).collect::<Vec<_>>(),
        ["eng", "fra"],
        "`en` and `fr` are read as the codes an export writes"
    );

    let project = Project::single(asset(MEDIA), FRAMES).with_subtitles(tracks);
    let (out, back) = exported("bcp47", project, &[0, 1]);
    assert_eq!(
        back.iter().map(|t| &*t.language).collect::<Vec<_>>(),
        ["eng", "fra"],
        "and they are the codes the file states"
    );
    let raw = engine::demux::matroska_subtitles(&out).expect("walk the tracks element");
    assert_eq!(
        raw[1].name, "Signs",
        "the title travels beside the language"
    );
    if let Some(listing) = ffmpeg_streams(&out) {
        assert!(
            listing.contains("(eng): Subtitle") && listing.contains("(fra): Subtitle"),
            "a second implementation reads both languages: {listing}"
        );
    }
    std::fs::remove_file(&out).unwrap();
}

/// What the card says before the button is pressed, which is where an mp4's
/// refusal has to land: the file is not the place to find out.
#[test]
fn the_plan_says_what_each_container_does_with_them() {
    let project = project();
    let plan = |format| engine::export::planned_subtitles(&project, format, Some(0));
    assert_eq!(plan(Format::Hevc), "eng → embedded");
    assert_eq!(plan(Format::Av1), "eng → embedded");
    // An mp4 carries them too, as `tx3g` -- so the card says what travels
    // rather than which container to switch to. The refusal it used to print
    // ("mkv only — an mp4 carries none") was a claim about the container that
    // the container never made.
    assert_eq!(plan(Format::Mp4), "eng → embedded");
    assert_eq!(plan(Format::HevcMp4), "eng → embedded");
    assert_eq!(plan(Format::Av1Mp4), "eng → embedded");
    assert_eq!(plan(Format::Wav), "none — this format is the sound alone");
    // A track of *pictures* on a format that is the sound alone says what the
    // file cannot do rather than what the track cannot: the per-track reason
    // used to be pushed first, which left the sentence above unreachable for a
    // pick that was all bitmaps -- an MP3 with a PGS track picked read
    // "pictures; drawn, not written", a true sentence about the wrong thing.
    let mut pictures = track();
    pictures.bitmap = true;
    pictures.label = "PGS".into();
    let pgs = Project::single(asset(MEDIA), FRAMES).with_subtitles(vec![pictures]);
    assert_eq!(
        engine::export::planned_subtitles(&pgs, Format::Mp3, Some(0)),
        "none — this format is the sound alone"
    );
    // ...and on a container that carries text it is the *track* that cannot
    // travel, which is the other sentence and just as true: both are reachable.
    assert_eq!(
        engine::export::planned_subtitles(&pgs, Format::Hevc, Some(0)),
        "PGS — pictures; drawn, not written"
    );
    // Nothing picked is "none" whatever the format -- there is no track for a
    // container to have nowhere to put.
    assert_eq!(
        engine::export::planned_subtitles(&pgs, Format::Mp3, None),
        "none"
    );
    // No pick is "none" rather than a panic or a lie...
    assert_eq!(
        engine::export::planned_subtitles(&project, Format::Hevc, None),
        "none"
    );
    // ...and a pick the project has no row for is *named*, like every other
    // reason a pick does not travel: an index nothing answers is a caller's bug
    // and the card is where a bug is seen, not the finished file.
    assert_eq!(
        engine::export::planned_subtitles(&project, Format::Hevc, Some(9)),
        "#9 — no such track"
    );
    let bare = Project::single(asset(MEDIA), FRAMES);
    assert_eq!(
        engine::export::planned_subtitles(&bare, Format::Hevc, Some(0)),
        "#0 — no such track"
    );
    // ...and it costs its own row and not the others': the tracks around it
    // still travel, which is what made this one easy to miss.
    assert_eq!(
        engine::export::planned_subtitles(&project, Format::Hevc, [0, 99]),
        "eng → embedded; #99 — no such track"
    );

    // Two picks are two tracks, named: the count first, because that is what the
    // row is answering, and the names after it because that is what a menu will
    // show.
    let two = Project::single(asset(MEDIA), FRAMES).with_subtitles(both_tracks());
    assert_eq!(
        engine::export::planned_subtitles(&two, Format::Hevc, [0, 1]),
        "2 tracks → embedded (eng, fra — Signs)"
    );
    // ...and a pick that cannot travel costs its own row and not the other's:
    // the empty track is dropped by name, the real one still goes.
    let mut mixed = both_tracks();
    mixed[1].cues.clear();
    let mixed = Project::single(asset(MEDIA), FRAMES).with_subtitles(mixed);
    assert_eq!(
        engine::export::planned_subtitles(&mixed, Format::Hevc, [0, 1]),
        "eng → embedded; fra — Signs — no cues"
    );
}
