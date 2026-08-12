//! What one open of a Matroska file's *sound* costs -- the walk the `Cues` do
//! not answer, since Matroska indexes no audio samples at all.
//!
//! `cargo run --release -p engine --example audio_open_cost -- <file> [repeats]`
//!
//! Prints one line per open in milliseconds and the block count it found; the
//! first line is the cold one (no sidecar yet), the rest are what a reopen costs
//! once the walk has been written down.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("usage: audio_open_cost <file> [repeats]"));
    let repeats: usize = args
        .next()
        .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(3);
    for i in 0..repeats {
        let start = Instant::now();
        let audio = engine::demux::MkvAudio::open(&path).expect("open the sound track");
        let ms = start.elapsed().as_secs_f64() * 1e3;
        match audio {
            Some(audio) => println!("audio open {i}: {ms:.1} ms ({} blocks)", audio.blocks()),
            None => println!("audio open {i}: {ms:.1} ms (no AC-3 track)"),
        }
    }
}
