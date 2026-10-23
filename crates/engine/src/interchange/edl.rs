//! CMX3600 EDL, the classic cut list, written for any editor that reads one.
//! Video is V1 only, audio is A1 (`A`) and A2 (`A2`), a dissolve is `D` with
//! its length, and every event is followed by its `FROM CLIP NAME`. Markers
//! are `LOC` comments. Timecode counts whole frames at the rounded rate and
//! the record starts at 01:00:00:00. Speed changes and anything that is not a
//! cut or a dissolve are not written.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::{Item, Timeline};
use crate::project::LaneKind;

/// `HH:MM:SS:FF` for `frames` at `nf` whole frames a second.
fn timecode(frames: u32, nf: u32) -> String {
    let f = frames % nf;
    let s = frames / nf;
    format!("{:02}:{:02}:{:02}:{:02}", s / 3600 % 100, s / 60 % 60, s % 60, f)
}

/// A reel name: the file's stem in capitals and digits, eight at most, unique in the list.
fn reel_for(reels: &mut Vec<(PathBuf, String)>, path: &Path) -> String {
    if let Some((_, r)) = reels.iter().find(|(p, _)| p == path) {
        return r.clone();
    }
    let stem: String = path
        .file_stem()
        .map(|s| {
            s.to_string_lossy()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .take(8)
                .collect()
        })
        .unwrap_or_default();
    let base = if stem.is_empty() { "BL".to_string() } else { stem };
    let mut name = base.clone();
    let mut n = 1;
    while reels.iter().any(|(_, r)| *r == name) {
        name = format!("{}{n:02}", base.chars().take(6).collect::<String>());
        n += 1;
    }
    reels.push((path.to_path_buf(), name.clone()));
    name
}

/// The CMX3600 EDL for `t`.
pub fn write(t: &Timeline) -> String {
    let nf = t.fps.round().max(1.0) as u32;
    let base = 3600 * nf;
    let rec = |frame: u32| timecode(frame + base, nf);
    let mut reels: Vec<(PathBuf, String)> = Vec::new();
    // (record start, track order, event line, clip name) for every event, sorted below.
    let mut events: Vec<(u32, usize, String, String)> = Vec::new();

    for (order, track) in t.tracks.iter().enumerate() {
        let chan = match (track.kind, track.ord) {
            (LaneKind::Audio, 0) => "A".to_string(),
            (LaneKind::Audio, 1) => "A2".to_string(),
            (LaneKind::Audio, _) => continue,
            (_, 0) => "V".to_string(),
            _ => continue,
        };
        let mut pending: Option<u32> = None;
        for item in &track.items {
            let c = match item {
                Item::Gap(_) => {
                    pending = None;
                    continue;
                }
                Item::Clip(c) => c,
            };
            let reel = reel_for(&mut reels, &c.path);
            let edit = match pending.take() {
                Some(n) => format!("D    {n:03}"),
                None => "C".to_string(),
            };
            let line = format!(
                "{:<8} {:<5} {:<8} {} {} {} {}",
                reel,
                chan,
                edit,
                timecode(c.in_frame, nf),
                timecode(c.out_frame, nf),
                rec(c.start),
                rec(c.start + c.frames),
            );
            events.push((c.start, order, line, c.file_name()));
            pending = c.transition.map(|(_, n)| n);
        }
    }
    events.sort_by_key(|e| (e.0, e.1));

    let mut out = format!("TITLE: {}\nFCM: NON-DROP FRAME\n\n", t.name);
    for (i, (_, _, line, name)) in events.iter().enumerate() {
        let _ = writeln!(out, "{:03}  {line}", i + 1);
        let _ = writeln!(out, "* FROM CLIP NAME:  {name}");
    }
    for m in &t.markers {
        let _ = writeln!(out, "* LOC: {} {:<6} {}", rec(m.frame), m.color.name().to_uppercase(), m.name);
    }
    out
}
