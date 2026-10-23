//! DVD (VobSub) and DVB bitmap subtitles imported the way PGS is: the tracks come out of
//! a Matroska file, a transport stream and an `.idx`/`.sub` pair as picture cues with
//! the container's timing, and a cue draws the pixels the encoder was given.
//!
//! The fixtures are generated here, at test time, from the synthetic source of
//! `edith_codecs/scripts/gen-bitmap-sub-fixtures.py` (four cues, 0.5-1.5, 2.0-3.2,
//! 3.5-4.5 and 5.0-6.0 s on 720x576) through ffmpeg's `dvdsub` and `dvbsub` encoders; no
//! media is checked in. Without ffmpeg, python3 with PIL, or the codec checkout, a test
//! prints a `SKIP` line and passes -- unless `EC_REQUIRE_FIXTURES=1`, which fails it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use engine::PlaybackSession;
use engine::subtitle::SubtitleTrack;

const STARTS_MS: [i64; 4] = [500, 2000, 3500, 5000];
const ENDS_MS: [i64; 4] = [1500, 3200, 4500, 6000];

fn skip(why: &str) -> Option<PathBuf> {
    if std::env::var("EC_REQUIRE_FIXTURES").is_ok_and(|v| v == "1") {
        panic!("EC_REQUIRE_FIXTURES=1 but the bitmap subtitle fixtures cannot be made: {why}");
    }
    eprintln!("SKIP bitmap_subs: {why}; this test did not run");
    None
}

/// The directory of generated fixtures: `dvd.mkv`, `dvd.idx`, `dvd.sub`, `dvb.ts`, `dvb.mkv`.
fn fixtures() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(make).clone()
}

fn make() -> Option<PathBuf> {
    let codecs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../edith_codecs");
    let script = codecs.join("scripts/gen-bitmap-sub-fixtures.py");
    if !script.exists() {
        return skip("edith_codecs/scripts/gen-bitmap-sub-fixtures.py is not there");
    }
    let dir = std::env::temp_dir().join(format!("edith-bitmap-subs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let sup = dir.join("src.sup");
    let ok = |c: &mut Command| c.status().is_ok_and(|s| s.success());
    // `EC_PYTHON` names an interpreter with PIL when the system python3 has none.
    let python = std::env::var("EC_PYTHON").unwrap_or_else(|_| "python3".into());
    if !ok(Command::new(python).arg(&script).arg(&sup)) {
        return skip("python3 with PIL could not run the source generator");
    }
    let ff = |args: &[&str], out: &Path| {
        ok(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-fix_sub_duration", "-f", "sup", "-i"])
            .arg(&sup)
            .args(args)
            .arg(out))
    };
    let (dvd_mkv, dvd_sub, dvb_mkv, dvb_ts) = (
        dir.join("dvd.mkv"),
        dir.join("dvd.sub"),
        dir.join("dvb.mkv"),
        dir.join("dvb.ts"),
    );
    let made = ff(&["-c:s", "dvdsub", "-map", "0"], &dvd_mkv)
        && ff(&["-c:s", "dvdsub", "-map", "0", "-target", "pal-dvd"], &dvd_sub)
        && ff(&["-c:s", "dvbsub", "-map", "0"], &dvb_mkv)
        && ok(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-fix_sub_duration", "-f", "sup", "-i"])
            .arg(&sup)
            .args(["-f", "lavfi", "-i", "color=c=black:s=64x64:r=5:d=7"])
            .args(["-c:v", "mpeg2video", "-b:v", "50k", "-c:s", "dvbsub", "-map", "0", "-map", "1"])
            .arg(&dvb_ts));
    if !made {
        return skip("ffmpeg could not encode the dvdsub/dvbsub fixtures");
    }
    // The palette of ffmpeg's dvdsub encoder is the DVD default table, which a missing
    // `palette:` line selects; the idx only has to name the canvas.
    std::fs::write(dir.join("dvd.idx"), "size: 720x576\nid: en, index: 0\n").ok()?;
    Some(dir)
}

fn within(got_us: i64, want_ms: i64, tol_ms: i64, what: &str) {
    let d = (got_us / 1000 - want_ms).abs();
    assert!(d <= tol_ms, "{what}: {} ms, wanted {want_ms} ms (+-{tol_ms})", got_us / 1000);
}

/// The first and only bitmap track of `path`, checked cue by cue. The muxer that wrote
/// the file may have moved every cue by one constant offset (ffmpeg's Matroska and TS
/// muxers start a track at zero), so the times are compared after taking the first
/// cue's offset off; the spacing and the pictures are what must match.
fn check(track: &SubtitleTrack) {
    assert!(track.is_bitmap(), "a DVD/DVB track is pictures");
    assert_eq!(track.refused, None);
    assert_eq!(track.cues.len(), 4, "four cues");
    let shift = track.cues[0].start_us - STARTS_MS[0] * 1000;
    for (i, cue) in track.cues.iter().enumerate() {
        within(cue.start_us - shift, STARTS_MS[i], 40, &format!("cue {i} start"));
        within(cue.end_us - shift, ENDS_MS[i], 100, &format!("cue {i} end"));
        let image = cue.image.as_ref().expect("a picture");
        assert_eq!((image.width, image.height), (720, 576));
        let rgba = image.rgba().expect("it decodes");
        assert_eq!(rgba.len(), 720 * 576 * 4);
        let painted = rgba.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(painted > 200, "cue {i} paints text, got {painted} pixels");
        assert!(painted < 720 * 576 / 2, "and leaves most of the canvas clear");
    }
}

fn the_only_track(path: &Path) -> SubtitleTrack {
    let mut tracks = PlaybackSession::parse_subtitles(path).expect("the file's subtitles");
    assert_eq!(tracks.len(), 1, "one subtitle track in {}", path.display());
    tracks.remove(0)
}

#[test]
fn a_matroska_vobsub_track_imports_and_draws() {
    let Some(dir) = fixtures() else { return };
    check(&the_only_track(&dir.join("dvd.mkv")));
}

#[test]
fn a_matroska_dvb_track_imports_and_draws() {
    let Some(dir) = fixtures() else { return };
    check(&the_only_track(&dir.join("dvb.mkv")));
}

#[test]
fn a_transport_stream_dvb_track_imports_and_draws() {
    let Some(dir) = fixtures() else { return };
    let ts = the_only_track(&dir.join("dvb.ts"));
    check(&ts);
    // The probe lists it as a subtitle of its own kind.
    let facts = engine::demux::probe_media(&dir.join("dvb.ts"));
    assert!(
        facts.streams.iter().any(|s| s.codec_id.as_deref() == Some("dvb_subtitle")),
        "the probe names the stream dvb_subtitle"
    );
}

#[test]
fn an_idx_sub_pair_imports_from_either_file() {
    let Some(dir) = fixtures() else { return };
    for name in ["dvd.idx", "dvd.sub"] {
        let track = the_only_track(&dir.join(name));
        // A pair's cues carry the muxer's offset: compare the spacing, not the origin.
        assert!(track.is_bitmap());
        assert_eq!(track.cues.len(), 4);
        let shift = track.cues[0].start_us - STARTS_MS[0] * 1000;
        for (i, cue) in track.cues.iter().enumerate() {
            within(cue.start_us - shift, STARTS_MS[i], 40, "pair cue start");
            assert!(cue.image.as_ref().unwrap().rgba().unwrap().chunks(4).any(|p| p[3] > 0));
        }
    }
}
