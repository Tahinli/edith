//! The video copy path: a Matroska timeline nobody has touched leaves as the
//! very coded blocks its source holds, and one that anybody *has* touched still
//! goes through the encoder.
//!
//! What is measured here is not "a file came out": it is that the bytes of the
//! file that came out are the source's own, that the pictures either side of a
//! cut still decode through this project's own decoder, and that every gate the
//! copy claims to have really refuses.
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test video_copy -- --test-threads=1
//! ```
//!
//! The plugin is needed only by the decode-back test (there is no software HEVC
//! decoder here); the copy itself decodes nothing and needs nothing. The
//! multi-cut tests build their own source with the `ffmpeg` CLI -- the fixture
//! suite's HEVC file is two groups of pictures long, which has no interior to
//! splice -- and say so and pass where it is not installed, exactly as
//! `tests/hevc_export.rs` does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};
use engine::hw::HwSession;
use engine::project::{Lane, Speed};
use engine::scratch::Scratch;
use engine::{DecodeSession, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// One coded block of a Matroska picture track, as the file holds it.
#[derive(PartialEq)]
struct Coded {
    bytes: Vec<u8>,
    key: bool,
    ts_ns: i64,
}

/// Every block of `path`'s picture track, in the file's own order.
fn blocks(path: &Path) -> Vec<Coded> {
    let (_, demuxer) = Demuxer::open(path).expect("open");
    let Demuxer::Mkv(mut demuxer) = demuxer else {
        panic!("{} is not Matroska", path.display());
    };
    (0..demuxer.block_count())
        .map(|i| {
            let block = demuxer
                .coded_block(i)
                .expect("read a block")
                .expect("a block at an index the count states");
            Coded {
                bytes: block.bytes.to_vec(),
                key: block.key,
                ts_ns: block.ts_ns,
            }
        })
        .collect()
}

/// Runs an export to completion and hands back the encoder line it published --
/// which is where a copy says it was one.
fn export(session: &PlaybackSession, out: &Path, format: Format) -> String {
    let settings = ExportSettings {
        format,
        ..Default::default()
    };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "the export did not finish in 600 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let encoders = handle.encoders().unwrap_or_default();
    handle
        .result()
        .expect("an outcome")
        .unwrap_or_else(|e| panic!("export failed: {e}"));
    encoders
}

/// A source of `gops` groups of pictures at 30 fps, keyed every 30 frames, with
/// B-frames and the open groups x265 writes by default -- which is the shape a
/// film off a disc has and the shape the copy path has to survive. `None` where
/// `ffmpeg` is not installed.
fn multi_gop(name: &str, gops: u32) -> Option<Scratch> {
    let out = Scratch::file(name, "mkv");
    let seconds = gops.to_string();
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size=320x240:rate=30:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error:keyint=30:min-keyint=30:scenecut=0",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
        ])
        .arg(&out)
        .status()
        .ok()?;
    ok.success().then_some(out)
}

/// An untouched Matroska timeline is written by copying: the file that comes out
/// holds its source's blocks, byte for byte, with the same sync points and the
/// same timing -- reordered B-frames included, which is the thing a frame
/// counter would silently get wrong.
#[test]
fn an_untouched_matroska_timeline_leaves_as_its_source_packets() {
    let source = asset("test_hevc.mkv");
    let session = PlaybackSession::open(&source).expect("open the fixture");
    let out = Scratch::file("ve_copy_untouched", "mkv");
    let line = export(&session, &out, Format::Hevc);
    assert!(
        line.starts_with("copy · "),
        "an untouched timeline was not copied: {line}"
    );
    let (before, after) = (blocks(&source), blocks(&out));
    assert_eq!(before.len(), after.len(), "block count");
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert_eq!(a.bytes, b.bytes, "block {i} is not the source's bytes");
        assert_eq!(a.key, b.key, "block {i} sync flag");
        // Millisecond ticks either side, so the comparison is the timestamp the
        // container can really state.
        assert_eq!(
            a.ts_ns / 1_000_000,
            b.ts_ns / 1_000_000,
            "block {i} is shown at another time than its source"
        );
    }
}

/// The same for AV1, whose blocks are whole temporal units rather than
/// length-prefixed NALs: one gate, two codecs, and the file says which.
#[test]
fn an_untouched_av1_timeline_leaves_as_its_source_packets() {
    let source = asset("test_av1.mkv");
    let session = PlaybackSession::open(&source).expect("open the fixture");
    let out = Scratch::file("ve_copy_av1", "mkv");
    let line = export(&session, &out, Format::Av1);
    assert!(line.starts_with("copy · "), "AV1 was not copied: {line}");
    let (before, after) = (blocks(&source), blocks(&out));
    assert_eq!(before.len(), after.len(), "block count");
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert_eq!(a.bytes, b.bytes, "block {i} is not the source's bytes");
    }
}

/// A cut placed on a sync point: the middle of the film is deleted and both
/// sides leave as copies. What is checked is the join -- the blocks either side
/// of it are the source's own, the timestamps rise a frame at a time across it
/// once they are put back in display order, and nothing but the leading
/// pictures the cut orphaned is missing.
#[test]
fn a_cut_on_a_keyframe_copies_both_sides() {
    let Some(source) = multi_gop("ve_copy_source", 6) else {
        println!("ffmpeg is not installed: skipping the multi-cut copy test");
        return;
    };
    let before = blocks(&source);
    let keys: Vec<usize> = (0..before.len()).filter(|&i| before[i].key).collect();
    assert!(keys.len() >= 4, "expected several groups, got {keys:?}");
    // Frames 60 and 120 in the block order the whole engine counts in.
    let (first, second) = (keys[2], keys[4]);
    let mut session = PlaybackSession::open(&source).expect("open the source");
    let fps = session.meta().frame_rate;
    assert!(session.cut_at(first as f64 / fps), "cut at the third group");
    assert!(
        session.cut_at(second as f64 / fps),
        "cut at the fifth group"
    );
    // One call: the picture and the sound of a take are one group, so
    // deleting the clip takes its lane partner with it.
    assert!(session.delete_clip(Lane::V1, 1), "drop the middle clip");

    let out = Scratch::file("ve_copy_cut", "mkv");
    let line = export(&session, &out, Format::Hevc);
    assert!(
        line.starts_with("copy · "),
        "a keyframe cut was not copied: {line}"
    );

    // What the file should hold: the blocks before the cut, then the blocks
    // after it minus the leading pictures the second region orphaned -- those
    // are shown before their own sync point and reference the group this cut
    // threw away.
    let after = blocks(&out);
    let mut want: Vec<&Coded> = before[..first].iter().collect();
    let origin = before[second].ts_ns;
    want.extend(before[second..].iter().filter(|b| b.ts_ns >= origin));
    assert_eq!(after.len(), want.len(), "copied block count");
    for (i, (a, b)) in want.iter().zip(&after).enumerate() {
        assert_eq!(a.bytes, b.bytes, "block {i} is not the source's bytes");
        assert_eq!(a.key, b.key, "block {i} sync flag");
    }
    // Put back in display order, the file is a frame grid with one hole: the
    // frames the second region's dropped leading pictures would have filled,
    // which is where the picture before the cut is held instead.
    let mut shown: Vec<i64> = after.iter().map(|b| b.ts_ns / 1_000_000).collect();
    shown.sort_unstable();
    let step = (1000.0 / fps).round() as i64;
    let holes = shown
        .windows(2)
        .filter(|w| (w[1] - w[0] - step).abs() > 1)
        .count();
    assert!(
        holes <= 1,
        "the copied file is not one run of frames: {holes} holes in {shown:?}"
    );
    assert_eq!(shown[0], 0, "the file starts at the timeline's own zero");
}

/// ...and that join really decodes: every picture around it comes back through
/// this project's own demuxer and decoder, in order, without an error and
/// without a black frame where the cut is.
#[test]
fn the_pictures_either_side_of_a_copied_cut_decode() {
    let Some(source) = multi_gop("ve_copy_decode_source", 6) else {
        println!("ffmpeg is not installed: skipping the copied-join decode test");
        return;
    };
    let before = blocks(&source);
    let keys: Vec<usize> = (0..before.len()).filter(|&i| before[i].key).collect();
    let (first, second) = (keys[2], keys[4]);
    let mut session = PlaybackSession::open(&source).expect("open the source");
    let fps = session.meta().frame_rate;
    assert!(session.cut_at(first as f64 / fps));
    assert!(session.cut_at(second as f64 / fps));
    assert!(session.delete_clip(Lane::V1, 1));
    let out = Scratch::file("ve_copy_decode", "mkv");
    let line = export(&session, &out, Format::Hevc);
    assert!(line.starts_with("copy · "), "not copied: {line}");

    let (meta, frames) = match DecodeSession::open(&out) {
        Ok(open) => open,
        Err(e) => {
            println!("no HEVC decoder here ({e}): skipping the decode-back");
            return;
        }
    };
    let mut seen = Vec::new();
    for frame in frames {
        let luma: u64 = frame
            .bgra
            .chunks_exact(4)
            .map(|px| u64::from(px[1]))
            .sum::<u64>()
            / (u64::from(frame.width) * u64::from(frame.height));
        seen.push((frame.index, luma));
    }
    assert!(
        seen.len() >= meta.frame_count as usize - 8,
        "{} pictures decoded of {}",
        seen.len(),
        meta.frame_count
    );
    for (i, (index, _)) in seen.iter().enumerate() {
        assert_eq!(*index as usize, i, "pictures came back out of order");
    }
    // The join is at the first region's length; a frame either side of it is a
    // real picture rather than the black a broken splice decodes to.
    let joint = first.min(seen.len() - 1);
    for at in [0, joint.saturating_sub(2), joint, seen.len() - 1] {
        assert!(
            seen[at].1 > 4,
            "picture {at} around the join is black ({} luma)",
            seen[at].1
        );
    }
}

/// Every gate the copy claims: a grade, a speed and a format whose container
/// this path does not write all put the export back through the encoder. A copy
/// that took any of these would write a file that is not what was watched.
#[test]
fn a_touched_timeline_is_still_encoded() {
    let source = asset("test_hevc.mkv");
    let out = Scratch::file("ve_copy_gate", "mkv");

    let mut graded = PlaybackSession::open(&source).expect("open");
    assert!(graded.set_color(
        Lane::V1,
        0,
        Some(engine::color::ColorParams {
            saturation: 1.4,
            ..Default::default()
        })
    ));
    let line = export(&graded, &out, Format::Hevc);
    assert!(
        !line.starts_with("copy"),
        "a graded timeline was copied: {line}"
    );

    let mut speeded = PlaybackSession::open(&source).expect("open");
    speeded
        .set_speed(Lane::V1, 0, Speed::from_permille(2000))
        .expect("speed the clip");
    let line = export(&speeded, &out, Format::Hevc);
    assert!(
        !line.starts_with("copy"),
        "a speeded timeline was copied: {line}"
    );

    // The same untouched timeline into an mp4: nothing here copies into one,
    // because a sample entry states its parameter sets once and the mp4 muxer
    // times its samples by a frame counter.
    let plain = PlaybackSession::open(&source).expect("open");
    let mp4 = Scratch::file("ve_copy_gate_mp4", "mp4");
    let line = export(&plain, &mp4, Format::HevcMp4);
    assert!(!line.starts_with("copy"), "an mp4 was copied: {line}");
}

/// A cut that lands between two sync points is the case this path refuses: the
/// head of the second region would need pictures that are not in the file, so
/// the whole export goes back through the encoder rather than writing a file
/// whose first second cannot be decoded.
#[test]
fn a_cut_off_the_keyframe_grid_falls_back_to_the_encoder() {
    let source = asset("test_hevc.mkv");
    let mut session = PlaybackSession::open(&source).expect("open");
    let fps = session.meta().frame_rate;
    // Block 40 of a file keyed every 30: inside the second group.
    assert!(session.cut_at(40.0 / fps), "cut mid-group");
    assert!(session.delete_clip(Lane::V1, 0), "drop the first clip");
    let out = Scratch::file("ve_copy_offgrid", "mkv");
    let line = export(&session, &out, Format::Hevc);
    assert!(
        !line.starts_with("copy"),
        "a cut off the sync grid was copied: {line}"
    );
}

/// The measurement this path exists for, on a real film rather than a fixture:
/// a 4K HDR x265 Matroska of two and a half hours, cut once in the middle, out
/// to Matroska. What it costs is the *edited* spans -- which for a copy is no
/// spans at all -- plus reading and writing the bytes, against a re-encode that
/// codes every one of its 167 thousand pictures.
///
/// `#[ignore]`d and named by environment, so no path off this machine is in the
/// repository and the suite runs without the file:
///
/// ```text
/// VE_FILM=/path/to/film.mkv VE_OUT=/somewhere/cut.mkv \
///   cargo test -p engine --release --test video_copy -- --ignored --nocapture \
///   a_real_film
/// ```
#[test]
#[ignore]
fn a_real_film_is_cut_by_copying_its_spans() {
    let Ok(film) = std::env::var("VE_FILM") else {
        println!("VE_FILM is not set: skipping the real-film copy measurement");
        return;
    };
    let film = PathBuf::from(film);
    let opened = Instant::now();
    let (meta, demuxer) = Demuxer::open(&film).expect("open the film");
    let Demuxer::Mkv(mut source) = demuxer else {
        panic!("the film is not Matroska");
    };
    println!(
        "film: {}x{} {:?} {:.4} fps, {} blocks, indexed in {:?}",
        meta.width,
        meta.height,
        meta.codec,
        meta.frame_rate,
        source.block_count(),
        opened.elapsed()
    );
    // The cut: five minutes taken out of the middle of the film, on the two
    // sync points nearest the half-hour marks -- which is what placing a cut on
    // a keyframe means, and what the copy needs of any cut.
    let mut sync_at = |from: u32| {
        (from as usize..source.block_count())
            .find(|&i| source.is_sync(i))
            .expect("a sync point after the mark")
    };
    let (first, second) = (
        sync_at((30.0 * 60.0 * meta.frame_rate) as u32),
        sync_at((35.0 * 60.0 * meta.frame_rate) as u32),
    );
    println!(
        "cutting blocks {first}..{second} out of {}",
        meta.frame_count
    );

    let mut session = PlaybackSession::open(&film).expect("open the film as a project");
    let fps = session.meta().frame_rate;
    assert!(session.cut_at(first as f64 / fps), "cut at the first mark");
    assert!(
        session.cut_at(second as f64 / fps),
        "cut at the second mark"
    );
    assert!(session.delete_clip(Lane::V1, 1), "drop the middle");

    let out = PathBuf::from(
        std::env::var("VE_OUT").unwrap_or_else(|_| "/tmp/ve_real_film_cut.mkv".into()),
    );
    let started = Instant::now();
    let line = export(&session, &out, Format::Hevc);
    let took = started.elapsed();
    let bytes = std::fs::metadata(&out).expect("the exported file").len();
    println!("exported {} MB in {:?} ({line})", bytes / 1_000_000, took);
    assert!(
        line.starts_with("copy · "),
        "the film was not copied: {line}"
    );

    // The bytes: every block around either side of the join, and a sample
    // across the rest -- a whole-file comparison is a second read of twelve
    // gigabytes and says nothing this does not.
    let mut written = blocks(&out);
    println!("{} blocks written of {}", written.len(), meta.frame_count);
    let origin = {
        let block = source.coded_block(second).expect("read").expect("a block");
        block.ts_ns
    };
    let mut want: Vec<usize> = (0..first).collect();
    want.extend((second..source.block_count()).filter(|&i| {
        let block = source.coded_block(i).expect("read").expect("a block");
        block.ts_ns >= origin
    }));
    assert_eq!(written.len(), want.len(), "copied block count");
    let near_join = first.saturating_sub(200)..(first + 200).min(want.len());
    for (out_index, &source_index) in want.iter().enumerate() {
        if !near_join.contains(&out_index) && out_index % 500 != 0 {
            continue;
        }
        let block = source
            .coded_block(source_index)
            .expect("read")
            .expect("a block");
        assert_eq!(
            block.bytes,
            written[out_index].bytes.as_slice(),
            "block {out_index} of the export is not source block {source_index}"
        );
        assert_eq!(
            block.key, written[out_index].key,
            "block {out_index} sync flag"
        );
    }
    // The join itself, in display order: one run of frames with at most the one
    // hole the orphaned leading pictures leave.
    written.sort_by_key(|b| b.ts_ns);
    let step = (1e9 / fps).round() as i64;
    let holes: Vec<i64> = written
        .windows(2)
        .map(|w| w[1].ts_ns - w[0].ts_ns)
        .filter(|d| (d - step).abs() > step / 2)
        .collect();
    println!("gaps in the shown order: {holes:?} ns (one frame is {step} ns)");
    assert!(holes.len() <= 1, "the copied film is not one run of frames");

    // ...and the join really decodes: two seconds either side of it, through
    // this project's own demuxer and decoder, in order and never black. The
    // start and the end of the film with them, so the file is exercised end to
    // end and not only where it was cut.
    let seconds = fps.round() as usize * 2;
    let decode_at = |path: &Path, at: u32| -> Option<Vec<u64>> {
        let mut decoder = HwSession::open_at(path, at)?;
        let mut lumas = Vec::new();
        while lumas.len() < seconds {
            let Some((y, _, _, w, h)) = decoder.next_frame().expect("decode") else {
                break;
            };
            lumas
                .push(y.iter().map(|&s| u64::from(s)).sum::<u64>() / (u64::from(w) * u64::from(h)));
        }
        Some(lumas)
    };
    // The tail is measured against the *source's* own tail rather than against
    // the count asked for: a decoder drains the last group of pictures the same
    // way in both files (45 of the last 48 on this film, measured), and a copy
    // is right when it behaves as its source does, not when it behaves better.
    let tail = decode_at(&film, meta.frame_count - seconds as u32).map(|l| l.len());
    for (what, at, least) in [
        ("the start", 0, seconds),
        ("the join", first as u32 - seconds as u32, seconds),
        (
            "the end",
            written.len() as u32 - seconds as u32,
            tail.unwrap_or(seconds),
        ),
    ] {
        let Some(lumas) = decode_at(&out, at) else {
            println!("no VA-API plugin here: skipping the decode of {what}");
            break;
        };
        println!(
            "{what} at frame {at}: {} pictures, luma {lumas:?}",
            lumas.len()
        );
        assert!(
            lumas.len() >= least,
            "{what} decoded {} pictures of the {least} its source gives",
            lumas.len()
        );
        // Limited-range black is 16, and this film opens and closes on it; what
        // a broken splice decodes to is 0, and what it never does is stay in
        // step with the source's own luma.
        assert!(
            lumas.iter().all(|&l| l >= 15),
            "a black picture around {what}: {lumas:?}"
        );
    }
}
