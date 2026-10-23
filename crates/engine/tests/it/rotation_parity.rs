//! The mp4 display matrix, read three ways that must agree: the engine's
//! reading (`engine::demux::Demuxer`), the container demuxer's own
//! (`ec_mp4`'s track rotation), and ffprobe's side data.
//!
//! Every fixture is generated here with ffmpeg, so nothing depends on a file
//! someone happened to keep in `assets/`. The expectations are the turns the
//! engine reported before its reading moved to `ec_mp4`, which
//! `tests/rotation.rs` already pins against ffmpeg's own autorotation.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;

use ec_core::Rotation as EcRotation;
use engine::Rotation;
use engine::scratch::Scratch;

fn run(tool: &str, args: &[&str]) -> String {
    let out = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run {tool}: {e}"));
    assert!(
        out.status.success(),
        "{tool} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One 320x180 H.264 mp4 of ffmpeg's test pattern, 10 frames, no rotation.
fn plain_source(name: &str) -> Scratch {
    let scratch = Scratch::file(name, "mp4");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=10",
            "-frames:v",
            "10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            scratch.to_str().expect("utf-8 scratch path"),
        ],
    );
    scratch
}

/// The same stream remuxed with `-display_rotation <degrees>`, the way
/// `scripts/gen_fixtures.sh` writes its `test_rotation*.mp4` files.
fn with_display_rotation(source: &Path, degrees: u32) -> Scratch {
    let out = Scratch::file(&format!("rotation_parity_{degrees}"), "mp4");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-y",
            "-display_rotation",
            &degrees.to_string(),
            "-i",
            source.to_str().expect("utf-8 source path"),
            "-c",
            "copy",
            out.to_str().expect("utf-8 output path"),
        ],
    );
    out
}

/// The rotation ffprobe's `side_data` states, in its own convention: the
/// counter-clockwise angle, as ffprobe prints it (`90` for a file that is
/// shown a quarter turn clockwise). Absent side data is no turn.
fn ffprobe_rotation(path: &Path) -> i32 {
    let text = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "side_data=rotation",
            "-of",
            "csv=p=0",
            path.to_str().expect("utf-8 path"),
        ],
    );
    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        None => 0,
        Some(line) => line
            .parse()
            .unwrap_or_else(|_| panic!("ffprobe rotation {line:?}")),
    }
}

/// ffprobe's counter-clockwise angle, turned into the clockwise quarter turn
/// the engine names.
fn clockwise_from_ffprobe(ccw: i32) -> Rotation {
    match (360 - ccw.rem_euclid(360)) % 360 {
        0 => Rotation::None,
        90 => Rotation::Cw90,
        180 => Rotation::Cw180,
        270 => Rotation::Cw270,
        other => panic!("ffprobe states a {other}-degree clockwise turn, not a quarter turn"),
    }
}

/// The rotation `ec_mp4` reports for the file's track, read from the container
/// by the demuxer alone.
fn ec_mp4_rotation(path: &Path) -> EcRotation {
    let file = BufReader::new(File::open(path).expect("open the fixture"));
    let demuxer = ec_mp4::Mp4Demuxer::new(file).expect("ec-mp4 opens the fixture");
    let tracks = demuxer.tracks();
    assert_eq!(tracks.len(), 1, "the fixture has one track");
    tracks[0].rotation
}

/// What the engine reports for `degrees` of `-display_rotation`, and what
/// `ec_mp4` reports for the same matrix. These are the values the engine's own
/// `tkhd` parse gave before the switch.
fn expected(degrees: u32) -> (Rotation, EcRotation) {
    match degrees {
        0 => (Rotation::None, EcRotation::None),
        90 => (Rotation::Cw270, EcRotation::Cw270),
        180 => (Rotation::Cw180, EcRotation::Cw180),
        270 => (Rotation::Cw90, EcRotation::Cw90),
        other => panic!("no expectation for {other} degrees"),
    }
}

#[test]
fn the_engine_matches_ec_mp4_and_ffprobe_for_every_quarter_turn() {
    let source = plain_source("rotation_parity_source");
    for degrees in [0, 90, 180, 270] {
        let file = with_display_rotation(&source, degrees);
        let (engine_turn, ec_turn) = expected(degrees);

        let (meta, _) = engine::demux::Demuxer::open(&file).expect("open the fixture");
        assert_eq!(
            meta.rotation, engine_turn,
            "-display_rotation {degrees}: the engine reads the turn it always read"
        );
        assert_eq!(
            ec_mp4_rotation(&file),
            ec_turn,
            "-display_rotation {degrees}: ec-mp4 reads the same matrix"
        );
        let ffprobe = ffprobe_rotation(&file);
        assert_eq!(
            clockwise_from_ffprobe(ffprobe),
            engine_turn,
            "-display_rotation {degrees}: ffprobe side_data says {ffprobe}"
        );
    }
}

/// A mirror is not a turn: `ec_mp4` carries its four terms verbatim, and the
/// engine still refuses it by name, with the same angle in the sentence as
/// before. The matrix is written into a copy of the unrotated fixture's `tkhd`
/// (`a = -1`, `d = 1`), which ffmpeg has no option to state.
#[test]
fn a_horizontal_mirror_is_refused_by_name_as_before() {
    let source = plain_source("rotation_parity_hflip_source");
    let file = with_display_rotation(&source, 0);
    write_matrix(&file, [-65536, 0, 0, 65536]);

    assert_eq!(
        ec_mp4_rotation(&file),
        EcRotation::Other {
            a: -65536,
            b: 0,
            c: 0,
            d: 65536
        },
        "ec-mp4 keeps the mirror's terms verbatim"
    );
    let error = match engine::demux::Demuxer::open(&file) {
        Ok(_) => panic!("a mirror is not a turn, and opening it must not succeed"),
        Err(error) => error,
    };
    assert!(
        engine::UnsupportedRotation::is_it(&error),
        "the refusal is the named one: {error}"
    );
    assert!(
        error.to_string().contains("180.0°"),
        "the refusal keeps the angle it had before: {error}"
    );
}

/// Overwrites the `tkhd` display matrix of a version-0 mp4 with the four linear
/// terms `[a, b, c, d]` in 16.16 fixed point. The FullBox payload starts after the
/// fourcc; the matrix sits at byte 40 of it, and `a`, `b`, `u`, `c`, `d` are
/// consecutive words there. ffmpeg has no option that writes an arbitrary matrix.
fn write_matrix(file: &Path, [a, b, c, d]: [i32; 4]) {
    let mut bytes = std::fs::read(file).expect("read the fixture");
    let tkhd = bytes
        .windows(4)
        .position(|window| window == b"tkhd")
        .expect("the fixture has a tkhd box");
    let payload = tkhd + 4;
    assert_eq!(bytes[payload], 0, "a version-0 tkhd, as ffmpeg writes it");
    let matrix = payload + 40;
    for (word, value) in [(0, a), (1, b), (3, c), (4, d)] {
        let at = matrix + word * 4;
        bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
    std::fs::write(file, &bytes).expect("write the patched fixture");
}

/// A matrix 0.3 degrees past a quarter turn is still that quarter turn: the
/// engine has always tolerated half a degree, and `ec-mp4` snaps the same way
/// (its track rotation is already quantised, so the engine takes it as it is).
/// Byte-patched into an ffmpeg-made file, `a = cos 90.3`, `b = sin 90.3`.
#[test]
fn a_matrix_within_half_a_degree_of_a_quarter_turn_snaps_to_it() {
    let source = plain_source("rotation_parity_snap_source");
    let file = with_display_rotation(&source, 0);
    let theta = 90.3f64.to_radians();
    let (a, b) = (
        (theta.cos() * 65536.0).round() as i32,
        (theta.sin() * 65536.0).round() as i32,
    );
    write_matrix(&file, [a, b, -b, a]);

    assert_eq!(
        ec_mp4_rotation(&file),
        EcRotation::Cw90,
        "ec-mp4 snaps a matrix within half a degree of a quarter turn"
    );
    let (meta, _) = engine::demux::Demuxer::open(&file).expect("a 90.3 degree turn opens");
    assert_eq!(
        meta.rotation,
        Rotation::Cw90,
        "snapped to the 90 degree turn"
    );
    assert_eq!(
        [meta.width, meta.height],
        [180, 320],
        "and shown as the quarter turn it snaps to"
    );
}
