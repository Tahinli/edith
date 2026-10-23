//! The ffmpeg and ffprobe references the real-library witnesses compare against,
//! and the MD5 ffprobe prints for a packet's payload. Included by path from the
//! `real_library_*` tests; not a test target of its own.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

/// True when `tool -version` runs. A tool that is absent skips its witness.
pub fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One stream as ffprobe states it.
#[derive(Debug, Clone)]
pub struct FfStream {
    pub index: u32,
    pub kind: String,
    pub tag: String,
    pub time_base: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub start_time: Option<f64>,
    pub duration_ts: Option<i64>,
}

/// One packet row of `ffprobe -show_packets -show_data_hash md5`.
#[derive(Debug, Clone)]
pub struct FfPacket {
    pub stream: u32,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub duration: Option<i64>,
    pub size: usize,
    pub key: bool,
    pub md5: String,
}

fn opt<T: std::str::FromStr>(field: &str) -> Option<T> {
    field.trim().parse().ok()
}

/// Every stream of the file, in ffprobe's index order. Read as `key=value`
/// lines, a new stream starting at each `index=` line, because ffprobe's CSV
/// drops the fields a stream does not have and so cannot be read by column.
pub fn ffprobe_streams(path: &Path) -> Vec<FfStream> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries"])
        .arg("stream=index,codec_type,codec_tag_string,time_base,width,height,sample_rate,channels,start_time,duration_ts")
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed on {}", path.display());
    let mut streams: Vec<HashMap<String, String>> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key == "index" {
            streams.push(HashMap::new());
        }
        if let Some(cur) = streams.last_mut() {
            cur.insert(key.to_string(), value.to_string());
        }
    }
    streams
        .iter()
        .map(|f| {
            let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("N/A");
            FfStream {
                index: opt(get("index")).expect("stream index"),
                kind: get("codec_type").to_string(),
                tag: get("codec_tag_string").to_string(),
                time_base: get("time_base").to_string(),
                width: opt(get("width")),
                height: opt(get("height")),
                sample_rate: opt(get("sample_rate")),
                channels: opt(get("channels")),
                start_time: opt(get("start_time")),
                duration_ts: opt(get("duration_ts")),
            }
        })
        .collect()
}

/// Every packet of the file, in ffprobe's interleaved (decode-time) order.
pub fn ffprobe_packets(path: &Path) -> Vec<FfPacket> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_packets", "-show_data_hash", "md5"])
        .args(["-show_entries"])
        .arg("packet=stream_index,pts,dts,duration,size,flags,data_hash")
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed on {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            FfPacket {
                stream: opt(f[0]).expect("stream index"),
                pts: opt(f[1]),
                dts: opt(f[2]),
                duration: opt(f[3]),
                size: opt(f[4]).expect("size"),
                key: f[5].starts_with('K'),
                md5: f[6].trim_start_matches("MD5:").to_string(),
            }
        })
        .collect()
}

/// A child process whose stdout is read in exact pieces, stderr silenced.
pub fn ffmpeg_pipe(args: &[&str], path: &Path) -> (Child, BufReader<ChildStdout>) {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("run ffmpeg");
    let stdout = child.stdout.take().expect("piped stdout");
    (child, BufReader::with_capacity(1 << 20, stdout))
}

/// Fills `buf` from `r`, returning how many bytes arrived (short only at EOF).
pub fn fill(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => panic!("reading ffmpeg output: {e}"),
        }
    }
    got
}

/// RFC 1321 MD5 as lowercase hex, the digest ffprobe's `data_hash` prints.
pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let k: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);
    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for block in msg.as_chunks::<64>().0 {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(block[4 * i..4 * i + 4].try_into().unwrap())
        });
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[(i / 16) * 4 + i % 4]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn md5_matches_the_rfc_vectors() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(md5_hex(&[b'a'; 1000]), "cabe45dcc9ae5b66ba86600cca6b8ba8");
}
