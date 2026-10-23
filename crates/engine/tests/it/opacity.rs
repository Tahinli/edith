//! Clip opacity: how much of a picture shows over what is beneath it.
//!
//! The blend is on the stored Y'CbCr code values, as a dissolve blends them:
//! gamma-encoded video levels, not linear light. So 50% white over black is
//! the midpoint of the two luma codes (limited range, black 16, white 235),
//! which is 126 -- mid grey as the eye reads it.

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::project::{Lane, LaneKind};

/// The second video lane, the one a layer sits on.
const TOP: Lane = Lane::new(LaneKind::Video, 1);
use engine::scale::{Composer, FitPolicy};
use engine::scratch::Scratch;
use engine::transform::TransformParams;

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn open(path: impl AsRef<Path>) -> PlaybackSession {
    engine::ao::set_silent_output();
    PlaybackSession::open(path).expect("open")
}

fn with_opacity(opacity: f32) -> TransformParams {
    TransformParams {
        opacity,
        ..TransformParams::default()
    }
}

#[test]
fn white_at_half_opacity_over_black_is_mid_grey() {
    // A 64x64 white picture (luma 235, neutral chroma) on a black canvas.
    let (w, h) = (64u32, 64u32);
    let y = vec![235u8; (w * h) as usize];
    let c = vec![128u8; ((w / 2) * (h / 2)) as usize];
    let mut composer = Composer::new(w, h, FitPolicy::Stretch);
    let (out_y, out_u, _, _, _) = composer.place_transformed(&y, &c, &c, w, h, &with_opacity(0.5));
    // 16 + (235 - 16) / 2 = 125.5, rounded up to 126.
    assert!(out_y.iter().all(|&v| v == 126), "luma {:?}", &out_y[..4]);
    assert!(out_u.iter().all(|&v| v == 128), "chroma stays neutral");

    // Opaque and transparent are the two exact ends.
    let (full, ..) = composer.place_transformed(&y, &c, &c, w, h, &with_opacity(1.0));
    assert!(full.iter().all(|&v| v == 235));
    let (none, ..) = composer.place_transformed(&y, &c, &c, w, h, &with_opacity(0.0));
    assert!(none.iter().all(|&v| v == 16), "0 is the black canvas");
}

#[test]
fn a_single_clip_at_half_opacity_is_half_as_bright() {
    let mut session = open(asset("test_av.mp4"));
    // At the project's own size: a box resize averages clipped super-white
    // samples, which is not linear, and would blur the check below.
    let full = session.compose_at(10, 1280, 720).expect("opaque frame");
    assert!(session.set_transform(Lane::V1, 0, Some(with_opacity(0.5))));
    let half = session.compose_at(10, 1280, 720).expect("half frame");
    // Per colour channel (the alpha byte is skipped), the half frame is half the
    // opaque one: the fade is linear in code values and RGB is linear in them
    // too. Where a channel would clip in the opaque picture the halving is not
    // exact, so only the unclipped channels are compared.
    let mut worst: f64 = 0.0;
    let mut compared = 0usize;
    for (px_full, px_half) in full.bgra.chunks(4).zip(half.bgra.chunks(4)) {
        for c in 0..3 {
            let (f, h) = (f64::from(px_full[c]), f64::from(px_half[c]));
            if (32.0..=223.0).contains(&f) {
                compared += 1;
                worst = worst.max((h - f / 2.0).abs());
            }
        }
    }
    assert!(
        compared > full.bgra.len() / 16,
        "too few unclipped channels: {compared}"
    );
    assert!(
        worst <= 2.5,
        "half strays {worst} from half of opaque over {compared}"
    );
}

#[test]
fn a_layer_at_half_opacity_is_the_mean_of_the_two_layers() {
    let dir = Scratch::dir("ve_opacity_layers");
    let project = dir.join("layers.edith");
    // Bottom: test_av.mp4 for the whole timeline. Top: test_av2.mp4 from frame 20
    // for 60 frames, which is the second lane the playback tests use too.
    let text = format!(
        "edith 4\nplayhead 0\nsource 0 {}\nsource 0 {}\n\
         video 1 0 0 150 0 -\naudio 1 0 0 150 0 -\n\
         video 2 20 0 60 1 -\n",
        asset("test_av.mp4").display(),
        asset("test_av2.mp4").display(),
    );
    std::fs::write(&project, text).expect("write the project");
    let mut session = PlaybackSession::open_project(&project).expect("open the layers");
    let at = 30; // inside the top layer

    let (w, h) = (1280, 720);
    assert!(session.set_transform(TOP, 0, Some(with_opacity(0.0))));
    let bottom = session.compose_at(at, w, h).expect("bottom only").bgra;
    assert!(session.set_transform(TOP, 0, Some(with_opacity(1.0))));
    let top = session.compose_at(at, w, h).expect("top only").bgra;
    assert!(session.set_transform(TOP, 0, Some(with_opacity(0.5))));
    let mix = session.compose_at(at, w, h).expect("half").bgra;

    // Per byte, the half-opacity frame is the midpoint of the two layers, to
    // within rounding -- but only where neither layer clips. The blend is in
    // code values and the conversion clamps to 0..=255, so a super-white
    // source byte (255 here) would otherwise pull the midpoint off.
    let mut worst: f64 = 0.0;
    let mut compared = 0usize;
    for ((&m, &b), &t) in mix.iter().zip(&bottom).zip(&top) {
        if (20..=235).contains(&b) && (20..=235).contains(&t) {
            compared += 1;
            worst = worst.max((f64::from(m) - (f64::from(b) + f64::from(t)) / 2.0).abs());
        }
    }
    assert!(
        compared > bottom.len() / 10,
        "too few unclipped bytes: {compared}"
    );
    assert!(worst < 4.0, "half opacity strays {worst} from the midpoint");
    let spread = bottom
        .iter()
        .zip(&top)
        .map(|(&b, &t)| (f64::from(b) - f64::from(t)).abs())
        .sum::<f64>()
        / bottom.len() as f64;
    assert!(spread > 5.0, "the two layers should differ");
    std::fs::remove_file(&project).ok();
}

#[test]
fn opacity_survives_a_save_and_a_reopen_and_default_writes_nothing_new() {
    let dir = Scratch::dir("ve_opacity_save");
    let path = dir.join("half.edith");
    let mut session = open(asset("test_av.mp4"));
    assert!(session.set_transform(Lane::V1, 0, Some(with_opacity(0.25))));
    session.save_project(&path).expect("save");
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.starts_with(b"edith 31\n"),
        "a layer that is not opaque needs v31"
    );

    let loaded = PlaybackSession::open_project(&path).expect("reopen");
    let back = loaded
        .transform_of(Lane::V1, 0)
        .expect("the transform is kept");
    assert_eq!(back.opacity.to_bits(), 0.25f32.to_bits());

    // A clip at the default opacity writes the v30 bytes it always did, and
    // never a ninth field.
    let plain_path = dir.join("plain.edith");
    let mut plain = open(asset("test_av.mp4"));
    assert!(plain.set_transform(Lane::V1, 0, Some(with_opacity(1.0))));
    plain.save_project(&plain_path).expect("save plain");
    let plain_bytes = std::fs::read(&plain_path).unwrap();
    assert!(plain_bytes.starts_with(b"edith 30\n"));
    assert!(!String::from_utf8_lossy(&plain_bytes).contains("transform 0:0:1:0:0:0:0:0:"));
}

#[test]
fn an_opacity_outside_zero_to_one_is_refused_on_load() {
    let dir = Scratch::dir("ve_opacity_range");
    let path = dir.join("bad.edith");
    let text = "edith 31\nplayhead 0\nresolution 1280 720\nsource 0 X\n\
                transform 0:0:1:0:0:0:0:0:1.5\n";
    std::fs::write(&path, text).unwrap();
    let err = engine::edith::load(&path)
        .expect_err("refused")
        .to_string();
    assert!(err.contains("opacity"), "{err}");

    // A ninth field is a v31 thing; a v30 file with one is refused as such.
    let old = text.replace("edith 31", "edith 30").replace(":1.5", ":0.5");
    std::fs::write(&path, old).unwrap();
    let err = engine::edith::load(&path)
        .expect_err("refused")
        .to_string();
    assert!(err.contains("transform wants 8 fields"), "{err}");
}
