//! The chain a click on a PGS row actually runs, against a real BluRay remux:
//! the session lists the track, maps its cues onto the timeline and hands over
//! the picture of one -- which is what the window draws over the film.
//!
//! Gated on the film being there: it is a 4K remux in a local folder and no
//! fixture in this repository, so on a machine without it this says so and
//! passes. What it guards is what a hand-made display set cannot -- that a real
//! disc's blocks come whole out of the demuxer and decode, and that the map a
//! repaint asks for is cheap enough to ask sixty times a second.
//!
//! ```text
//! cargo test -p engine --release --test pgs_bitmaps -- --nocapture
//! ```

use std::path::Path;
use std::time::Instant;

/// A remux with `S_TEXT/UTF8` + 4x `S_HDMV/PGS`.
const FILM: &str = "/path/to/a-real-4k-pgs-film.mkv";

#[test]
fn the_session_maps_a_pgs_track_onto_the_timeline_and_draws_it() {
    let film = Path::new(FILM);
    if !film.exists() {
        eprintln!("skipped: {FILM} is not on this machine");
        return;
    }
    let opened = Instant::now();
    let mut session = engine::PlaybackSession::open(film).expect("the film opens");
    session.set_gain(0.0);
    let rows = session
        .import_subtitles(film)
        .expect("the film's own subtitle tracks");
    eprintln!("{rows} rows in {:?}", opened.elapsed());
    let pick = session
        .subtitles()
        .iter()
        .position(|t| t.is_bitmap())
        .expect("a bitmap row to pick");
    assert_eq!(
        session.subtitles()[pick].refused, None,
        "a row a click can pick"
    );

    let mapped = Instant::now();
    let cues = session.timeline_cues(pick);
    eprintln!("{} cues mapped in {:?}", cues.len(), mapped.elapsed());
    assert!(!cues.is_empty(), "no cue of this track lands on the timeline");
    // The map is asked once per repaint, so the *second* ask is the one that has
    // to be cheap -- the first paid for whatever the open did not.
    let warm = Instant::now();
    let again = session.timeline_cues(pick);
    let warm = warm.elapsed();
    eprintln!("mapped again in {warm:?}");
    assert_eq!(again, cues, "the same map, twice");
    assert!(warm.as_millis() < 16, "a repaint's worth of map: {warm:?}");

    // Every cue is on screen for a while and gone again, in order: the erase
    // blocks are what end them and are not cues themselves.
    for pair in cues.windows(2) {
        assert!(pair[0].end_us > pair[0].start_us, "{:?}", pair[0]);
        assert!(pair[1].start_us >= pair[0].start_us, "{pair:?}");
    }
    // ...and the first one's picture is the canvas a front-end lays over the
    // film: the disc's own frame, painted where the line is and nowhere else.
    let drawn = Instant::now();
    let image = cues[0].image.as_ref().expect("a picture cue");
    let rgba = image.rgba().expect("the first display set decodes");
    eprintln!(
        "cue 0 decoded to {}x{} in {:?}",
        image.width,
        image.height,
        drawn.elapsed()
    );
    assert_eq!(rgba.len(), (image.width * image.height * 4) as usize);
    let opaque = rgba.chunks_exact(4).filter(|px| px[3] > 0).count();
    assert!(opaque > 0, "the block decoded to an empty canvas");
    assert!(
        opaque < (image.width * image.height) as usize,
        "a subtitle that covers the whole frame is not a subtitle"
    );

    // A text row of the same film carries no picture, which is what keeps the
    // two kinds of row apart.
    let text = session
        .subtitles()
        .iter()
        .position(|t| !t.is_bitmap() && t.refused.is_none())
        .expect("the film's text track");
    let words = session.timeline_cues(text);
    assert!(!words.is_empty());
    assert!(words.iter().all(|c| c.image.is_none()));
}
