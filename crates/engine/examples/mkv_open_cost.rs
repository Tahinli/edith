//! What one open of a video file costs -- wall time and bytes read through the
//! read syscalls (`/proc/self/io`'s `rchar`) -- and what it answers: the frame
//! count, and where a seek to a given second lands.
//!
//! `cargo run --release -p engine --example mkv_open_cost -- <file> [secs ...] [all]`
//!
//! The point of the byte count: a Matroska open that walks every cluster reads
//! the whole segment's element headers, which on a multi-gigabyte film is
//! hundreds of megabytes of `pread` and seconds of wall clock. One that reads
//! the header, the `SeekHead` and the `Cues` reads kilobytes. Same tool on both
//! builds, so the two are comparable line for line.
//!
//! `all` walks every access unit of the file and prints a rolling hash of their
//! bytes with the count: two builds that disagree by one block anywhere disagree
//! on that number, which is what makes "the lazy index is the same index" a
//! check rather than a claim. The per-second lines do the same for a seek --
//! landing index and the hash of the access unit handed back there.

use std::path::PathBuf;
use std::time::Instant;

/// Bytes this process has read through `read`/`pread` so far, page cache or
/// disk -- what the walk really costs the kernel, independent of what was warm.
fn rchar() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .ok()
        .and_then(|io| {
            io.lines()
                .find_map(|l| l.strip_prefix("rchar: ")?.trim().parse().ok())
        })
        .unwrap_or(0)
}

/// FNV-1a over the bytes, so a run of access units collapses to one number a
/// diff can compare.
fn fnv(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .expect("usage: mkv_open_cost <file> [secs ...] [all]"),
    );
    let rest: Vec<String> = args.collect();
    let walk_all = rest.iter().any(|a| a == "all");
    let seeks: Vec<f64> = rest.iter().filter_map(|a| a.parse().ok()).collect();

    let before = rchar();
    let started = Instant::now();
    let (meta, mut demuxer) = engine::demux::Demuxer::open(&path).expect("open");
    let open_ms = started.elapsed().as_secs_f64() * 1e3;
    let open_bytes = rchar() - before;
    println!("file {}", path.display());
    println!("open {open_ms:.1} ms, {open_bytes} bytes read");
    println!(
        "meta {}x{} @ {:.6} fps, {} frames",
        meta.width, meta.height, meta.frame_rate, meta.frame_count
    );

    for secs in seeks {
        let frame = (secs * meta.frame_rate).round() as u32;
        let before = rchar();
        let started = Instant::now();
        let landed = demuxer.seek_to_sync_at_or_before(frame);
        let au = demuxer.next_access_unit().expect("read after seek");
        let ms = started.elapsed().as_secs_f64() * 1e3;
        let bytes = rchar() - before;
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let len = au.as_ref().map_or(0, |au| au.len());
        if let Some(au) = &au {
            fnv(&mut hash, au);
        }
        println!(
            "seek {secs}s -> frame {frame}: landed {landed}, au {len} bytes, hash {hash:016x}, \
             {ms:.1} ms, {bytes} bytes read"
        );
    }

    if walk_all {
        let before = rchar();
        let started = Instant::now();
        demuxer.seek_to_sync_at_or_before(0);
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let (mut count, mut bytes) = (0u64, 0u64);
        while let Some(au) = demuxer.next_access_unit().expect("read") {
            fnv(&mut hash, &au);
            bytes += au.len() as u64;
            count += 1;
        }
        println!(
            "walk {count} access units, {bytes} bytes, hash {hash:016x}, {:.1} ms, {} bytes read",
            started.elapsed().as_secs_f64() * 1e3,
            rchar() - before
        );
    }
}
