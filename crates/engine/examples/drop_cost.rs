//! Scratch: how long a PlaybackSession takes to drop now that it joins its
//! decode worker. Worst case is what a UI scrub pays per abandoned session.
use std::time::Instant;

fn main() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/test_av.mp4");
    let mut worst = 0f64;
    for i in 0..20 {
        let session = engine::PlaybackSession::open(path).expect("open");
        let t = Instant::now();
        drop(session);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        eprintln!("run {i}: drop {ms:.1} ms");
    }
    eprintln!("worst drop: {worst:.1} ms");

    // The scrub path: every seek now joins the worker it replaces.
    let mut session = engine::PlaybackSession::open(path).expect("open");
    let mut worst_seek = 0f64;
    for i in 0..20 {
        let t = Instant::now();
        session.seek(f64::from(i) * 0.2);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst_seek = worst_seek.max(ms);
    }
    eprintln!("worst seek: {worst_seek:.1} ms");
}
