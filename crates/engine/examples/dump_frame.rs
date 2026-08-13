//! Dump decoded frames as PPM for visual inspection.
//! Usage: cargo run -p engine --release --example dump_frame -- <video.mp4> <frame_index> <out.ppm> [seek]
//!
//! With `seek` the frame is reached the way a scrub reaches it -- restart at the
//! random access point at or before it -- instead of by decoding the file from
//! its start. The two must be the same picture, byte for byte, which is what
//! makes a `cmp` of the two dumps a check that seeking into an open GOP decodes
//! the frame it claims and not a smear of missing references.

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

fn main() -> engine::Result<()> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: dump_frame <video.mp4> <frame_index> <out.ppm> [seek]")?;
    let target: u32 = args.next().ok_or("missing frame index")?.parse()?;
    let out = args.next().ok_or("missing output path")?;
    let seek = args.next().is_some_and(|a| a == "seek");

    let (meta, rx) = engine::DecodeSession::open(&path)?;
    eprintln!(
        "{}x{} @ {:.2} fps",
        meta.width, meta.height, meta.frame_rate
    );
    if seek {
        drop(rx);
        let mut session = engine::PlaybackSession::open(&path)?;
        session.seek(f64::from(target) / meta.frame_rate);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            session.tick();
            if let Some(frame) = session.try_frame() {
                write_ppm(&out, frame.width, frame.height, &frame.bgra)?;
                eprintln!("wrote frame {} (asked {target}) to {out}", frame.index);
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        return Err(format!("no frame arrived within 60 s of seeking to {target}").into());
    }

    for frame in rx {
        if frame.index == target {
            write_ppm(&out, frame.width, frame.height, &frame.bgra)?;
            eprintln!("wrote frame {target} to {out}");
            return Ok(());
        }
    }
    Err(format!("stream ended before frame {target}").into())
}

fn write_ppm(out: &str, width: u32, height: u32, bgra: &[u8]) -> engine::Result<()> {
    let mut w = BufWriter::new(File::create(out)?);
    writeln!(w, "P6\n{width} {height}\n255")?;
    for px in bgra.chunks_exact(4) {
        w.write_all(&[px[2], px[1], px[0]])?; // BGRA -> RGB
    }
    Ok(())
}
