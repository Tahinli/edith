//! Where a seek actually lands, in *content*: seeks a file to `start` and
//! writes the next `dur` seconds of the mixer's own samples as raw f32, for
//! correlating against an ffmpeg decode of the same window.
//!
//! `cargo run --release -p engine --example seek_dump -- <file> <start_secs> <dur_secs> <out.f32>`
//!
//! Its reason to exist is the same as `probe_fps`'s: the files this has to be
//! measured on are films, too big to keep as fixtures, and "the sound is out of
//! step after a scrub" is a number -- the offset between what came out here and
//! what ffmpeg decodes at the same second -- long before it is an opinion.

use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: seek_dump <file> <start> <dur> <out.f32>");
    let start: f64 = args.next().expect("missing start").parse().expect("start");
    let dur: f64 = args
        .next()
        .expect("missing duration")
        .parse()
        .expect("duration");
    let out = args.next().expect("missing output path");

    let (meta, rx) = engine::AudioSession::open_at(&path, start)
        .expect("open")
        .expect("no audio track");
    eprintln!("{} Hz, {} ch", meta.sample_rate, meta.channels);
    let want = (dur * f64::from(meta.sample_rate)) as usize * usize::from(meta.channels);
    let mut samples: Vec<f32> = Vec::with_capacity(want);
    for chunk in rx {
        if samples.is_empty() {
            eprintln!("first chunk at sample {}", chunk.start_sample);
        }
        samples.extend(chunk.samples);
        if samples.len() >= want {
            break;
        }
    }
    samples.truncate(want);
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).expect("create"));
    for s in &samples {
        w.write_all(&s.to_le_bytes()).expect("write");
    }
    w.flush().expect("flush");
    eprintln!(
        "wrote {} frames to {out}",
        samples.len() / usize::from(meta.channels)
    );
}
