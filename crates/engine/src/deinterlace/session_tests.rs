//! The decode layer's field mode (`Deinterlace::Field`) and `Off` on real
//! interlaced H.264: frame numbers become field numbers, a mid-GOP seek equals a
//! sequential decode, reverse plays fields last to first, and a progressive file
//! ignores the setting. Fixtures are synthetic (`testsrc2`, x264 MBAFF) made with
//! ffmpeg; without ffmpeg and libx264 each test says so and passes.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::Frame as CoreFrame;
use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase, VideoFrame};
use ec_h264::H264Decoder;

use crate::color::ColorParams;
use crate::decode::{DecodeSession, Frame, FrameStream};
use crate::demux::Demuxer;
use crate::project::Deinterlace;
use crate::scale::Composer;
use crate::transform::TransformParams;

const FRAMES: u32 = 30;

fn have_x264() -> bool {
    let tool = |t: &str, a: &str| Command::new(t).arg(a).output().is_ok_and(|o| o.status.success());
    tool("ffmpeg", "-version")
        && Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libx264"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// 30 interlaced frames (60 source moments), keyint 10, B frames, top or bottom first.
fn make(dir: &Path, name: &str, tff: bool) -> PathBuf {
    let scan = if tff { "tff" } else { "bff" };
    let clip = dir.join(name);
    let params = format!("interlaced=1:{scan}=1:keyint=10:min-keyint=10:bframes=2");
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=50", "-frames:v", "30", "-vf", &format!("interlace=scan={scan}"),
        "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p", "-crf", "12", "-flags", "+ildct+ilme", "-x264-params", &params,
        clip.to_str().unwrap(),
    ]);
    clip
}

fn collect(stream: FrameStream) -> Vec<Frame> {
    let frames: Vec<Frame> = stream.frames.iter().collect();
    frames
}

fn range(path: &Path, start: u32, end: u32, mode: Deinterlace) -> Vec<Frame> {
    let (_, stream) = DecodeSession::open_worker_with(
        path,
        start,
        end,
        ColorParams::default(),
        TransformParams::default(),
        Composer::passthrough(),
        crate::tonemap::Preset::default(),
        mode,
    )
    .expect("open");
    collect(stream)
}

fn all(path: &Path, mode: Deinterlace) -> Vec<Frame> {
    range(path, 0, u32::MAX, mode)
}

fn woven(clip: &Path) -> Vec<VideoFrame> {
    let (_, mut demuxer) = Demuxer::open(clip).expect("open");
    demuxer.seek_to_sync_at_or_before(0);
    let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("decoder");
    let mut out = Vec::new();
    let take = |decoder: &mut H264Decoder, out: &mut Vec<VideoFrame>| {
        while let Ok(frame) = decoder.receive_frame() {
            if let CoreFrame::Video(pic) = frame {
                out.push(pic);
            }
        }
    };
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        decoder.send_packet(&Packet::new(0, TimeBase::new(1, 1), au)).expect("decode");
        take(&mut decoder, &mut out);
    }
    decoder.flush().expect("flush");
    take(&mut decoder, &mut out);
    out
}

#[test]
fn field_mode_numbers_fields_and_its_first_fields_are_frame_mode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-field");
    for (name, tff) in [("f-tff.mp4", true), ("f-bff.mkv", false)] {
        let clip = make(&dir, name, tff);
        let frame_mode = all(&clip, Deinterlace::Frame);
        let field_mode = all(&clip, Deinterlace::Field);
        assert_eq!(frame_mode.len(), FRAMES as usize, "{name}");
        assert_eq!(field_mode.len(), 2 * FRAMES as usize, "{name}: one picture per field");
        assert_eq!(
            field_mode.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..2 * FRAMES).collect::<Vec<_>>(),
            "{name}: field numbers"
        );
        // A 25p timeline shows field 2n: it is exactly frame mode's picture n.
        for n in 0..FRAMES as usize {
            assert!(field_mode[2 * n].bgra == frame_mode[n].bgra, "{name}: first field of frame {n} differs from Frame mode");
            assert!(field_mode[2 * n + 1].bgra != field_mode[2 * n].bgra, "{name}: field {} repeats", 2 * n + 1);
        }
        // Motion continuity: every field differs from the next.
        for w in field_mode.windows(2) {
            assert!(w[0].bgra != w[1].bgra, "{name}: field {} equals the next", w[0].index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn field_mode_mid_gop_seeks_and_ranges_equal_a_sequential_decode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-seek");
    for (name, tff) in [("s-tff.mp4", true), ("s-bff.mkv", false)] {
        let clip = make(&dir, name, tff);
        let sequential = all(&clip, Deinterlace::Field);
        assert_eq!(sequential.len(), 60);
        // Even and odd targets, on and off the GOP boundaries (a sync every 10 frames = 20 fields).
        for target in [0u32, 1, 2, 3, 6, 7, 19, 20, 21, 22, 23, 39, 40, 41, 57, 58, 59] {
            let seeked = range(&clip, target, target + 1, Deinterlace::Field);
            assert_eq!(seeked.len(), 1, "{name}: field {target}");
            assert_eq!(seeked[0].index, target, "{name}");
            assert!(seeked[0].bgra == sequential[target as usize].bgra, "{name}: seek to field {target} differs from sequential");
        }
        let part = range(&clip, 13, 31, Deinterlace::Field);
        assert_eq!(part.iter().map(|f| f.index).collect::<Vec<_>>(), (13..31).collect::<Vec<_>>(), "{name}");
        for f in &part {
            assert!(f.bgra == sequential[f.index as usize].bgra, "{name}: range field {} differs", f.index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn field_mode_reverse_plays_fields_in_reverse_temporal_order() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-rev");
    for (name, tff) in [("r-tff.mp4", true), ("r-bff.mkv", false)] {
        let clip = make(&dir, name, tff);
        let forward = all(&clip, Deinterlace::Field);
        // The whole clip, then a window that starts mid-frame on an odd field.
        for (top, count) in [(59u32, 60u32), (40, 25), (33, 9)] {
            let stream = DecodeSession::open_reversed_with(
                &clip,
                top,
                count,
                ColorParams::default(),
                TransformParams::default(),
                Composer::passthrough(),
                crate::tonemap::Preset::default(),
                Deinterlace::Field,
            );
            let back = collect(stream);
            assert_eq!(back.len(), count as usize, "{name}: top {top}");
            for (d, f) in back.iter().enumerate() {
                let want = &forward[top as usize - d];
                assert!(f.bgra == want.bgra, "{name}: reversed step {d} is not field {}", top as usize - d);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn off_passes_the_woven_pictures_and_a_progressive_file_ignores_the_setting() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-off");
    let clip = make(&dir, "o.mp4", true);
    let off = all(&clip, Deinterlace::Off);
    assert_eq!(off.len(), FRAMES as usize);
    let (meta, demuxer) = Demuxer::open(&clip).expect("open");
    let mut render = crate::decode::test_render(meta, &demuxer);
    for (i, pic) in woven(&clip).iter().enumerate() {
        let (y, u, v) = (&pic.planes[0], &pic.planes[1], &pic.planes[2]);
        let want = render.frame(i as u32, &y.data, &u.data, &v.data, pic.width, pic.height);
        assert!(off[i].bgra == want.bgra, "Off frame {i} is not the plain decode");
    }
    let frame = all(&clip, Deinterlace::Frame);
    assert!(off.iter().zip(&frame).any(|(a, b)| a.bgra != b.bgra), "Off must differ from the filtered Frame mode");

    // Progressive: every mode is the same bytes, and the count is not doubled.
    let prog = dir.join("p.mp4");
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25", "-frames:v", "30", "-c:v", "libx264", "-x264-params",
        "keyint=10:min-keyint=10:bframes=2", prog.to_str().unwrap(),
    ]);
    let base = all(&prog, Deinterlace::Frame);
    assert_eq!(base.len(), 30);
    for mode in [Deinterlace::Field, Deinterlace::Off] {
        let other = all(&prog, mode);
        assert_eq!(other.len(), 30, "{mode:?}");
        assert!(other.iter().zip(&base).all(|(a, b)| a.index == b.index && a.bgra == b.bgra), "{mode:?} changed a progressive file");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
