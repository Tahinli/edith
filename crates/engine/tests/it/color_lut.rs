//! The `.cube` LUT path held against ffmpeg's: `lut3d=interp=tetrahedral` on a
//! testsrc2 frame in yuv420p and yuv420p10le, tagged Rec. 709 limited range, run
//! through edith's own grade ([`engine::color::apply_yuv`] and
//! [`engine::color::apply_yuv_wide`]) and through ffmpeg, for an identity LUT and
//! two analytic grades. Mean absolute error at most 1 code at 8 bits and 2 at 10;
//! the maximum is printed. On the samples in gamut before the LUT (R'G'B'
//! inside 0..1, where ffmpeg's clip and edith's kept excess agree) the maximum
//! is at most 1 code at 8 bits and 2 at 10. An identity LUT moves no sample by more than 1, and a
//! grade without a LUT is byte-identical. Without ffmpeg the ffmpeg parts say so
//! and pass.
//!
//! ```text
//! cargo test -p engine --release --test color_lut -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::color::{ColorParams, Look, apply_yuv, apply_yuv_wide};
use engine::cube::LutRef;

const W: usize = 320;
const H: usize = 240;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-color-lut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn write_cube(path: &Path, size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) {
    let mut s = format!("TITLE \"test\"\nLUT_3D_SIZE {size}\n");
    let e = (size - 1) as f32;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let o = f([r as f32 / e, g as f32 / e, b as f32 / e]);
                s.push_str(&format!("{:.6} {:.6} {:.6}\n", o[0], o[1], o[2]));
            }
        }
    }
    std::fs::write(path, s).expect("write cube");
}

const TAGS: [&str; 8] = ["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output().expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// One testsrc2 frame as raw planar YUV in `pix_fmt`.
fn source(dir: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = dir.join(format!("src-{pix_fmt}.yuv"));
    let size = format!("testsrc2=s={W}x{H}:r=1");
    let mut args = vec!["-f", "lavfi", "-i", &size, "-frames:v", "1", "-vf"];
    let vf = format!("scale=out_color_matrix=bt709:out_range=tv,format={pix_fmt}");
    args.push(&vf);
    args.extend(TAGS);
    args.extend(["-f", "rawvideo", out.to_str().expect("utf-8")]);
    ffmpeg(&args);
    std::fs::read(&out).expect("source frame")
}

/// ffmpeg's lut3d over a 4:2:0 frame `planes`: the chroma is doubled to 4:4:4 by
/// repetition (each chroma sample is its 2x2 block's, as edith's grade reads it),
/// ffmpeg converts Rec. 709 limited YUV to float RGB, runs the LUT and converts
/// back, and the chroma is brought back to 4:2:0 as the mean of each block (as
/// edith's grade writes it). So the two sides differ by the LUT and the matrix
/// arithmetic only, not by two different chroma resamplers.
fn ffmpeg_lut(dir: &Path, planes: &[u16], ten: bool, cube: &Path) -> Vec<u16> {
    let (pix_fmt, size) = if ten { ("yuv444p10le", 2) } else { ("yuv444p", 1) };
    let (y, u, v) = split(planes);
    let up = |c: &[u16]| -> Vec<u16> { (0..W * H).map(|i| c[(i / W / 2) * (W / 2) + (i % W) / 2]).collect() };
    let full = [y, up(&u), up(&v)].concat();
    let bytes: Vec<u8> = if ten { full.iter().flat_map(|s| s.to_le_bytes()).collect() } else { full.iter().map(|&s| s as u8).collect() };
    let (inp, out) = (dir.join(format!("in-{pix_fmt}.yuv")), dir.join(format!("out-{pix_fmt}.yuv")));
    std::fs::write(&inp, bytes).expect("write input");
    let s = format!("{W}x{H}");
    let vf = format!(
        "scale=in_color_matrix=bt709:in_range=tv:out_range=pc:flags=accurate_rnd,format=gbrpf32le,\
         lut3d=file={}:interp=tetrahedral,\
         scale=out_color_matrix=bt709:out_range=tv:in_range=pc:flags=accurate_rnd,format={pix_fmt}",
        cube.display()
    );
    let mut args = vec!["-f", "rawvideo", "-pix_fmt", pix_fmt, "-s", &s];
    args.extend(TAGS);
    args.extend(["-i", inp.to_str().expect("utf-8"), "-vf", &vf]);
    args.extend(TAGS);
    args.extend(["-f", "rawvideo", out.to_str().expect("utf-8")]);
    ffmpeg(&args);
    let raw = std::fs::read(&out).expect("ffmpeg output");
    let full: Vec<u16> = if size == 2 { raw.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect() } else { raw.into_iter().map(u16::from).collect() };
    let n = W * H;
    let down = |c: &[u16]| -> Vec<u16> {
        (0..n / 4)
            .map(|i| {
                let (cx, cy) = (i % (W / 2), i / (W / 2));
                let at = |dx: usize, dy: usize| u32::from(c[(cy * 2 + dy) * W + cx * 2 + dx]);
                ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u16
            })
            .collect()
    };
    [full[..n].to_vec(), down(&full[n..2 * n]), down(&full[2 * n..])].concat()
}

fn split<T: Copy>(raw: &[T]) -> (Vec<T>, Vec<T>, Vec<T>) {
    let (l, c) = (W * H, (W / 2) * (H / 2));
    (raw[..l].to_vec(), raw[l..l + c].to_vec(), raw[l + c..l + 2 * c].to_vec())
}

fn graded(lut: Option<LutRef>) -> ColorParams {
    ColorParams { look: Look { lut, ..Default::default() }, ..Default::default() }
}

/// (mean abs, max) over all three planes.
fn error(a: &[u16], b: &[u16]) -> (f64, u16) {
    let sum: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    (sum as f64 / a.len() as f64, a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0))
}

/// Which samples of a 4:2:0 frame `planes` (codes at `scale` times their 8-bit
/// values) are in gamut: a luma sample whose R'G'B' (Rec. 709, limited range) is
/// inside 0..1 before the LUT, and a chroma sample whose every luma sample is.
/// On those both tools grade the same value; outside it ffmpeg clips and edith
/// keeps the excess, so only these are held to the code.
fn in_gamut(planes: &[u16], scale: f32) -> Vec<bool> {
    let (y, u, v) = split(planes);
    let (kr, kb) = (0.2126f32, 0.0722f32);
    let luma: Vec<bool> = (0..W * H)
        .map(|i| {
            let c = (i / W / 2) * (W / 2) + (i % W) / 2;
            let l = (f32::from(y[i]) - 16.0 * scale) / (219.0 * scale);
            let cb = (f32::from(u[c]) - 128.0 * scale) / (224.0 * scale);
            let cr = (f32::from(v[c]) - 128.0 * scale) / (224.0 * scale);
            let r = l + 2.0 * (1.0 - kr) * cr;
            let b = l + 2.0 * (1.0 - kb) * cb;
            let g = (l - kr * r - kb * b) / (1.0 - kr - kb);
            [r, g, b].iter().all(|x| (0.0..=1.0).contains(x))
        })
        .collect();
    let chroma: Vec<bool> = (0..W * H / 4)
        .map(|i| {
            let (cx, cy) = (i % (W / 2), i / (W / 2));
            [(0, 0), (1, 0), (0, 1), (1, 1)].iter().all(|&(dx, dy)| luma[(cy * 2 + dy) * W + cx * 2 + dx])
        })
        .collect();
    [luma, chroma.clone(), chroma].concat()
}

/// The largest difference over the samples `mask` keeps, and how many it keeps.
fn masked_max(a: &[u16], b: &[u16], mask: &[bool]) -> (u16, usize) {
    let kept = a.iter().zip(b).zip(mask).filter(|(_, m)| **m);
    let n = mask.iter().filter(|m| **m).count();
    (kept.map(|((x, y), _)| x.abs_diff(*y)).max().unwrap_or(0), n)
}

type Grade = Box<dyn Fn([f32; 3]) -> [f32; 3]>;

fn grades() -> Vec<(&'static str, Grade)> {
    vec![
        ("identity", Box::new(|p| p)),
        ("warm", Box::new(|p: [f32; 3]| [(p[0] * 1.08 + 0.04).min(1.0), p[1] * 0.98 + 0.01, p[2] * 0.85])),
        (
            "s-curve and desaturate",
            Box::new(|p: [f32; 3]| {
                let s = |v: f32| v * v * (3.0 - 2.0 * v);
                let l = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                [0, 1, 2].map(|k| s(l + (p[k] - l) * 0.6))
            }),
        ),
    ]
}

#[test]
fn luts_match_ffmpeg_tetrahedral_at_8_and_10_bits() {
    if !have_ffmpeg() {
        println!("ffmpeg not found: the ffmpeg comparison is skipped");
        return;
    }
    let dir = scratch();
    let src8 = source(&dir, "yuv420p");
    let src10: Vec<u16> = source(&dir, "yuv420p10le").as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    let src8w: Vec<u16> = src8.iter().map(|&c| u16::from(c)).collect();
    let (gamut8, gamut10) = (in_gamut(&src8w, 1.0), in_gamut(&src10, 4.0));
    for (i, (name, f)) in grades().into_iter().enumerate() {
        let cube = dir.join(format!("grade{i}.cube"));
        write_cube(&cube, 33, f);
        let lut = LutRef::reload(&cube);
        assert!(lut.problem().is_none(), "{:?}", lut.problem());
        let params = graded(Some(lut));

        let (mut y, mut u, mut v) = split(&src8);
        apply_yuv(&params, &mut y, &mut u, &mut v, W, false);
        let ours: Vec<u16> = [y, u, v].concat().into_iter().map(u16::from).collect();
        let theirs = ffmpeg_lut(&dir, &src8w, false, &cube);
        let (mean, max) = error(&ours, &theirs);
        let (in_max, n) = masked_max(&ours, &theirs, &gamut8);
        println!("{name}, 8-bit: mean abs {mean:.3}, max {max}; in gamut ({n} samples) max {in_max}");
        assert!(mean <= 1.0, "{name} 8-bit mean abs {mean}");
        assert!(in_max <= 1, "{name} 8-bit in-gamut max {in_max}");

        let (mut y, mut u, mut v) = split(&src10);
        apply_yuv_wide(&params, &mut y, &mut u, &mut v, W, false);
        let ours = [y, u, v].concat();
        let theirs = ffmpeg_lut(&dir, &src10, true, &cube);
        let (mean, max) = error(&ours, &theirs);
        let (in_max, n) = masked_max(&ours, &theirs, &gamut10);
        println!("{name}, 10-bit: mean abs {mean:.3}, max {max}; in gamut ({n} samples) max {in_max}");
        assert!(mean <= 2.0, "{name} 10-bit mean abs {mean}");
        assert!(in_max <= 2, "{name} 10-bit in-gamut max {in_max}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An identity LUT is transparent to a code, at either depth, on a frame with
/// every luma code and a chroma sweep; and a grade without a LUT is no grade.
#[test]
fn an_identity_lut_moves_nothing_and_no_lut_is_byte_identical() {
    let dir = scratch().join("identity");
    std::fs::create_dir_all(&dir).expect("dir");
    let cube = dir.join("identity.cube");
    write_cube(&cube, 17, |p| p);
    let params = graded(Some(LutRef::reload(&cube)));
    // Every code is a pixel, chroma a slow sweep in-gamut-ish around grey.
    let y8: Vec<u8> = (0..W * H).map(|i| (16 + (i % 220)) as u8).collect();
    let u8_: Vec<u8> = (0..W * H / 4).map(|i| (100 + (i * 7) % 56) as u8).collect();
    let v8: Vec<u8> = (0..W * H / 4).map(|i| (100 + (i * 13) % 56) as u8).collect();
    let (mut y, mut u, mut v) = (y8.clone(), u8_.clone(), v8.clone());
    apply_yuv(&params, &mut y, &mut u, &mut v, W, false);
    let worst = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    assert!(worst(&y, &y8) <= 1 && worst(&u, &u8_) <= 1 && worst(&v, &v8) <= 1, "identity LUT moved a sample by more than 1");
    let wide = |s: &[u8]| s.iter().map(|&c| u16::from(c) * 4 + 2).collect::<Vec<u16>>();
    let (y10, u10, v10) = (wide(&y8), wide(&u8_), wide(&v8));
    let (mut y, mut u, mut v) = (y10.clone(), u10.clone(), v10.clone());
    apply_yuv_wide(&params, &mut y, &mut u, &mut v, W, false);
    let worst16 = |a: &[u16], b: &[u16]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    assert!(worst16(&y, &y10) <= 1 && worst16(&u, &u10) <= 1 && worst16(&v, &v10) <= 1, "10-bit identity LUT moved a sample by more than 1");

    // No LUT, every other look control neutral: the same bytes, both depths.
    let none = graded(None);
    let (mut y, mut u, mut v) = (y8.clone(), u8_.clone(), v8.clone());
    apply_yuv(&none, &mut y, &mut u, &mut v, W, false);
    assert_eq!((y, u, v), (y8, u8_, v8));
    let (mut y, mut u, mut v) = (y10.clone(), u10.clone(), v10.clone());
    apply_yuv_wide(&none, &mut y, &mut u, &mut v, W, false);
    assert_eq!((y, u, v), (y10, u10, v10));
    let _ = std::fs::remove_dir_all(&dir);
}
