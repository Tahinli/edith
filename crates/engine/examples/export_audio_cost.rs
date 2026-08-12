//! What the *sound* of a copied export costs, on a window of a real film.
//!
//! The picture of an untouched Matroska timeline is copied block for block, so
//! everything an export of a feature film waits for is in the audio pass. This
//! runs that pass over the first `minutes` of a file and prints the engine's own
//! stage lines (`export audio: ...`), which is how the per-minute cost of the
//! mix, the Opus encode and the fidelity gate are told apart.
//!
//! ```text
//! cargo run --release -p engine --example export_audio_cost -- <film.mkv> [minutes] [out.mkv]
//! ```
//!
//! The output file is overwritten; name a scratch path, never a source.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::export::{ExportSettings, Format};
use engine::project::Lane;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let film = PathBuf::from(
        args.next()
            .expect("usage: export_audio_cost <film.mkv> [minutes] [out.mkv]"),
    );
    let minutes: f64 = args
        .next()
        .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(5.0);
    let out = args.next().map_or_else(
        || PathBuf::from(std::env::temp_dir()).join("ve_export_audio_cost.mkv"),
        PathBuf::from,
    );

    // On a sync point, or the picture is re-encoded and the measurement is of
    // the encoder rather than of the sound.
    let (_, demuxer) = engine::demux::Demuxer::open(&film).expect("open the film");
    let engine::demux::Demuxer::Mkv(mut source) = demuxer else {
        panic!("the film is not Matroska");
    };

    let mut session = PlaybackSession::open(&film).expect("open the film");
    let fps = session.meta().frame_rate;
    let at = ((minutes * 60.0 * fps) as usize..source.block_count())
        .find(|&i| source.is_sync(i))
        .expect("a sync point after the mark");
    if session.cut_at(at as f64 / fps) {
        // Everything after the cut goes; what is left is the window measured.
        while session.delete_clip(Lane::V1, 1) {}
    }
    println!(
        "{}: {:.1} min of timeline at {fps:.3} fps -> {}",
        film.display(),
        session.timeline_duration() / 60.0,
        out.display()
    );

    let settings = ExportSettings {
        format: Format::Hevc,
        ..Default::default()
    };
    let started = Instant::now();
    let handle = session.export_to_with(&out, &settings);
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(200));
    }
    let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "export: {:.1} s, {} MB ({})",
        started.elapsed().as_secs_f64(),
        bytes / 1_000_000,
        handle.encoders().unwrap_or_default()
    );
    if let Some(Err(e)) = handle.result() {
        println!("export failed: {e}");
    }
}
