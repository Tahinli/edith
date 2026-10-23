//! What the demuxer reads a file's frame rate as, and what that means for the
//! frame the master clock asks for ten minutes in -- the arithmetic behind A/V
//! desync on NTSC-rate files, on a file too big to keep as a fixture.
//!
//! `cargo run --release -p engine --example probe_fps -- <file> [secs] [play] [play_secs]`
//!
//! With `play` it also seeks there, plays `play_secs` (two by default) against the
//! real audio device and reports the skew between the picture on screen and the
//! master clock -- the machine-checkable half of "lip sync holds".

use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("usage: probe_fps <file.mp4> [secs]"));
    let secs: f64 = args
        .next()
        .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(600.0);

    let (meta, _) = engine::demux::Demuxer::open(&path).expect("open");
    println!(
        "{}: {}x{} @ {:.9} fps, {} frames",
        path.display(),
        meta.width,
        meta.height,
        meta.frame_rate,
        meta.frame_count
    );
    println!("  ntsc 24000/1001 = {:.9}", 24_000.0 / 1001.0);
    println!(
        "  target frame at t={secs}s: {:.3} (mp4 0.14's flat 23.0 would say {:.3}, {:.1} frames out)",
        secs * meta.frame_rate,
        secs * 23.0,
        secs * (meta.frame_rate - 23.0)
    );
    // Straight from the container, to show what the demuxer's frame 0 skipped.
    // mp4 only: a Matroska file has no `moov` and reading one out of it is a
    // panic, which used to make this tool unusable on exactly the films in a
    // library that the demuxer most needs checking against.
    if !engine::demux::is_matroska(&path) {
        mp4_boxes(&path);
    }
    match engine::AudioSession::open(&path) {
        Ok(Some((audio, _))) => println!("  audio: {} Hz {} ch", audio.sample_rate, audio.channels),
        Ok(None) => println!(
            "  audio: none usable ({:?})",
            engine::AudioSession::unsupported(&path)
        ),
        Err(e) => println!("  audio: refused ({e})"),
    }

    // `play [secs]`: how long to play for, two seconds by default.
    if args.next().is_some_and(|a| a == *"play") {
        let seconds = args
            .next()
            .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
            .unwrap_or(2.0);
        play(&path, secs, meta.frame_rate, seconds);
    }
}

/// The edit list and sample counts of an mp4, per track.
fn mp4_boxes(path: &std::path::Path) {
    let file = std::fs::File::open(path).expect("open");
    let reader = ec_mp4::Mp4Demuxer::new(std::io::BufReader::new(file)).expect("read header");
    for track in reader.tracks() {
        let samples = track.stream.map_or(0, |s| reader.samples(s).len());
        println!(
            "  track {}: {:?}, {samples} samples, media_time {:?}, {} edit entries, rotation {:?}",
            track.track_id,
            track.media,
            track.media_time,
            track.edits.len(),
            track.rotation,
        );
    }
}

/// Seeks in, plays two seconds against the real device, and reports where the
/// picture stands against the master clock. The clock counts audio samples, so
/// `frame / fps - clock` *is* the A/V skew: a truncated frame rate shows up
/// here as tens of seconds of it, ten minutes in.
fn play(path: &std::path::Path, secs: f64, fps: f64, seconds: f64) {
    let mut session = engine::PlaybackSession::open(path).expect("open for playback");
    session.seek(secs);
    session.play();
    let mut last = None;
    let mut held: Option<u32> = None;
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        session.tick();
        // The app's own drop-when-behind rule (`Player::pump`): show every frame
        // already due and hold the first one that is not, or this would measure
        // how far the decoder runs ahead instead of what is on screen.
        let target = session.now() * fps;
        while let Some(index) = held.take().or_else(|| session.try_frame().map(|f| f.index)) {
            if f64::from(index) > target {
                held = Some(index);
                break;
            }
            last = Some(index);
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    let now = session.now();
    let Some(index) = last else {
        println!("  played from {secs}s: no frame arrived, clock at {now:.3}s");
        return;
    };
    let media = f64::from(index) / fps;
    println!(
        "  played from {secs}s: clock {now:.3}s, frame {index} = media {media:.3}s, skew {:+.3}s",
        media - now
    );
}
