//! Dump decoded frames as PPM for visual inspection.
//! Usage: cargo run -p engine --release --example dump_frame -- <video.mp4> <frame_index> <out.ppm>

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

fn main() -> engine::Result<()> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: dump_frame <video.mp4> <frame_index> <out.ppm>")?;
    let target: u32 = args.next().ok_or("missing frame index")?.parse()?;
    let out = args.next().ok_or("missing output path")?;

    let (meta, rx) = engine::DecodeSession::open(&path)?;
    eprintln!(
        "{}x{} @ {:.2} fps",
        meta.width, meta.height, meta.frame_rate
    );

    for frame in rx {
        if frame.index == target {
            let mut w = BufWriter::new(File::create(&out)?);
            writeln!(w, "P6\n{} {}\n255", frame.width, frame.height)?;
            for px in frame.bgra.chunks_exact(4) {
                w.write_all(&[px[2], px[1], px[0]])?; // BGRA -> RGB
            }
            eprintln!("wrote frame {target} to {out}");
            return Ok(());
        }
    }
    Err(format!("stream ended before frame {target}").into())
}
