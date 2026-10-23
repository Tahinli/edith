//! Render preview: a stretch of the timeline pre-rendered once into a picture
//! file at preview size, so that playback over it reads frames instead of
//! compositing them -- the heavy sections (stacked effects, titles over video,
//! colour, speed, multicam) are the ones that drop frames live.
//!
//! The rules, and they are the whole contract:
//!
//!   - **Content-keyed, never stale.** A segment is named by a hash of everything
//!     that can change its pixels in its range: the clips that overlap it (and the
//!     neighbours either side, whose transitions reach into it), their sources'
//!     files, the titles and captions over it, the lanes' visibility, the tone and
//!     the frame rate. Any edit that changes those drops the segment by name: a
//!     changed hash is a file nothing looks for. Nothing is ever compared by time.
//!   - **Derived, never authoritative.** Deleting the render directory costs the
//!     time to render again and nothing else. Nothing in a project file names a
//!     segment; the directory is scanned and each file is checked against the
//!     hash of the timeline as it is now ([`segments`]).
//! - **Picture at preview size.** Rendered through [`crate::export`] over the
//!   range, at [`crate::proxy::size_for`] of the project, every frame a random
//!   access point. Playback scales it to the viewer like any other picture.
//!   Sound is not used from here; the live mix always plays.
//!
//! Files are named `{from}-{to}-{width}x{height}-{key:016x}.mp4` in the user's
//! cache under `edith/renders`. The `.part` while it is written is what an
//! export writes beside its output, and the rename is the finish.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::demux::{VideoMeta, fnv1a};
use crate::export::{ExportHandle, ExportSettings, Format};
use crate::project::{Clip, Project};

/// One rendered stretch that still matches the timeline: frames `from..to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub from: u32,
    pub to: u32,
    /// The picture size the frames are at.
    pub size: (u32, u32),
    pub path: PathBuf,
}

/// Where renders live, or `None` where the machine has no cache directory.
pub fn dir() -> Option<PathBuf> {
    crate::demux::cache_dir(
        crate::envcfg::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        "renders",
    )
}

/// What a file on disk says about itself, for the content key: its path, its
/// length and its modification time. A source that moved or changed is a
/// different key.
fn file_stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| (d.as_secs(), d.subsec_nanos()));
            format!("{path:?} {} {mtime:?}\n", meta.len())
        }
        Err(_) => format!("{path:?} missing\n"),
    }
}

/// The hash of everything that can change the pixels of `from..to` at `size`.
///
/// Each clip that overlaps the range is hashed whole (its effects, keys, colour,
/// transform, fades and transitions are all in its `Debug`), and so is the one
/// clip either side of the range in every lane: a transition out of the clip
/// before reaches into the range, and the clip after is what it blends into.
/// Captions and titles are hashed whole when they overlap, and so is the tone.
/// Over-keying is safe here -- an extra hash change only costs a re-render --
/// so the rule is kept simple rather than exact.
fn content_key(project: &Project, meta: &VideoMeta, from: u32, to: u32, size: (u32, u32)) -> u64 {
    let (near_from, near_to) = (from.saturating_sub(1), to.saturating_add(1));
    let mut text = format!(
        "{from} {to} {}x{} {:?} {:?}\n",
        size.0,
        size.1,
        meta.frame_rate,
        project.tone()
    );
    let sources = project.sources();
    for lane in project.lanes() {
        text.push_str(&format!("lane {lane:?} hidden {}\n", project.lane_hidden(lane)));
        let clips = project.lane(lane);
        for (i, clip) in clips.iter().enumerate() {
            let overlaps = clip.start < near_to && clip.end() > near_from;
            let next = clip.start >= to && !clips[..i].iter().any(|c| c.start >= to);
            let prev = clip.end() <= from && clips.get(i + 1).is_none_or(|c| c.end() <= from);
            if !(overlaps || next || prev) {
                continue;
            }
            text.push_str(&format!("{clip:?}\n"));
            if let Some(source) = sources.get(clip.source) {
                text.push_str(&file_stamp(&source.path));
            }
        }
    }
    for title in project.titles() {
        if title.start < near_to && title.start + title.frames > near_from {
            text.push_str(&format!("{title:?}\n"));
        }
    }
    text.push_str(&format!("{:?}\n", project.subtitles()));
    fnv1a(text.as_bytes())
}

fn name(from: u32, to: u32, size: (u32, u32), key: u64) -> String {
    format!("{from}-{to}-{}x{}-{key:016x}.mp4", size.0, size.1)
}

/// The inverse of [`name`]: `None` for anything else in the directory.
fn parse(name: &str) -> Option<(u32, u32, (u32, u32), u64)> {
    let mut parts = name.strip_suffix(".mp4")?.split('-');
    let from = parts.next()?.parse().ok()?;
    let to = parts.next()?.parse().ok()?;
    let (w, h) = parts.next()?.split_once('x')?;
    let key = u64::from_str_radix(parts.next()?, 16).ok()?;
    match parts.next() {
        None => Some((from, to, (w.parse().ok()?, h.parse().ok()?), key)),
        Some(_) => None,
    }
}

/// Every rendered segment that still matches `project` as it is now, in
/// timeline order. A file whose hash no longer matches is simply not returned;
/// it is left for [`delete_all`] to take.
pub fn segments(project: &Project, meta: &VideoMeta) -> Vec<Segment> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((from, to, size, key)) = parse(file) else {
            continue;
        };
        if to > from && content_key(project, meta, from, to, size) == key {
            found.push(Segment { from, to, size, path });
        }
    }
    found.sort_by_key(|s| s.from);
    found
}

/// The segment that holds timeline frame `frame`, if one matches now.
pub fn at(project: &Project, meta: &VideoMeta, frame: u32) -> Option<Segment> {
    segments(project, meta)
        .into_iter()
        .find(|s| s.from <= frame && frame < s.to)
}

/// A render being made, or one that was already there. Poll
/// [`progress`](Job::progress) while [`is_finished`](Job::is_finished) is false
/// and take the [`outcome`](Job::outcome) when it flips, as a proxy is polled.
pub struct Job {
    out: PathBuf,
    /// `None` for a segment that already exists: there is nothing to run.
    handle: Option<ExportHandle>,
}

impl Job {
    /// Fraction of the render written, `0.0..=1.0`. An existing segment is done.
    pub fn progress(&self) -> f32 {
        self.handle.as_ref().map_or(1.0, |h| h.progress().clamp(0.0, 1.0))
    }

    /// Asks the encoder to stop at its next frame and leave no file behind.
    pub fn cancel(&self) {
        if let Some(handle) = &self.handle {
            handle.cancel();
        }
    }

    /// Whether [`cancel`](Job::cancel) was asked of this render.
    pub fn is_cancelled(&self) -> bool {
        self.handle.as_ref().is_some_and(ExportHandle::is_cancelling)
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(ExportHandle::is_finished)
    }

    /// The segment's path once it is written, once. `None` while it is still
    /// being made.
    pub fn outcome(&self) -> Option<crate::Result<PathBuf>> {
        let Some(handle) = &self.handle else {
            return Some(Ok(self.out.clone()));
        };
        handle.result().map(|r| r.map(|()| self.out.clone()))
    }
}

/// Starts rendering `from..to` of `project` (clamped to its end), or hands back
/// the segment already there. The frames are the ones the project plays: the
/// render is an export of the range at [`crate::proxy::size_for`] the project.
pub fn start(project: Project, meta: VideoMeta, from: u32, to: u32) -> crate::Result<Job> {
    let to = to.min(project.media_frames());
    if from >= to {
        return Err("there is nothing between the in and out points to render".into());
    }
    let size = crate::proxy::size_for(&meta);
    let dir = dir().ok_or("no cache directory to keep a render in")?;
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(name(from, to, size, content_key(&project, &meta, from, to, size)));
    if out.is_file() {
        return Ok(Job { out, handle: None });
    }
    // What a killed render left behind, under the same name: the same range of
    // the same timeline, so it is nobody else's to keep.
    let mut part = out.clone().into_os_string();
    part.push(".part");
    let _ = std::fs::remove_file(PathBuf::from(part));
    let handle = crate::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Mp4,
            // Every frame its own random-access point: a seek into a render
            // lands on the frame asked for, the way it does in a proxy.
            intra_only: true,
            range: Some((from, to)),
            size: Some(size),
            ..Default::default()
        },
        None,
    );
    Ok(Job {
        out,
        handle: Some(handle),
    })
}

/// Deletes every render, finished or half written, and says how many files went.
/// Nothing else in the cache directory is ever named by this module.
pub fn delete_all() -> std::io::Result<usize> {
    let Some(dir) = dir() else {
        return Ok(0);
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| parse(n).is_some() || n.ends_with(".mp4.part"));
        if ours && path.is_file() {
            std::fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Whether a clip carries anything that a render would be paying for: an effect,
/// a colour or transform, a key, a fade or a transition, or a speed change.
/// Used by the bar that marks the stretches worth rendering.
pub fn clip_has_work(clip: &Clip) -> bool {
    !clip.effects.is_empty()
        || clip.color.is_some()
        || clip.transform.is_some()
        || !clip.color_keys.is_empty()
        || !clip.transform_keys.is_empty()
        || !clip.opacity_keys.is_empty()
        || !clip.speed_keys.is_empty()
        || clip.speed != crate::project::Speed::NORMAL
        || clip.fade_in > 0
        || clip.fade_out > 0
        || clip.transition_out > 0
        || clip.reversed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorParams;
    use crate::playback::PlaybackSession;
    use crate::project::Lane;
    use crate::scratch::Scratch;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn grade(brightness: f32) -> Option<ColorParams> {
        Some(ColorParams {
            brightness,
            ..Default::default()
        })
    }

    /// A render of frames 0..10 is what a seek into it plays from, and changing
    /// the grade on its clip drops it: the one test this module carries.
    #[test]
    fn a_render_serves_its_frames_until_an_effect_changes() {
        let dir = Scratch::dir("render-cache");
        crate::envcfg::set("XDG_CACHE_HOME", &dir);
        let mut session = PlaybackSession::open(asset("test_25fps.mp4")).expect("open the fixture");
        assert!(session.set_color(Lane::V1, 0, grade(0.2)));
        let (project, meta) = session.export_snapshot();
        let job = start(project, meta, 0, 10).expect("a render starts");
        for _ in 0..1200 {
            if job.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let rendered = job.outcome().expect("finished").expect("rendered");
        assert!(rendered.is_file(), "the render is on disk");

        let fps = session.meta().frame_rate;
        let mut served = false;
        session.seek(2.0 / fps);
        for _ in 0..400 {
            if session.try_frame().is_some() {
                served = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(served, "a frame inside the render arrives");
        assert!(session.playing_from_cache(), "the frame is read off the render");

        assert!(session.set_color(Lane::V1, 0, grade(-0.2)));
        let (project, meta) = session.export_snapshot();
        assert!(at(&project, &meta, 2).is_none(), "the changed grade drops the render");
        session.seek(2.0 / fps);
        std::thread::sleep(std::time::Duration::from_millis(50));
        session.try_frame();
        assert!(!session.playing_from_cache(), "and playback composes live again");
    }
}
