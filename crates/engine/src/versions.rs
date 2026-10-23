//! Versions of a project: named full copies kept beside it, in
//! `<project dir>/checkpoints/<project stem>/<name>.edith`. The MCP server's
//! `checkpoint` / `restore` / `checkpoints` and the window's File > Save version and
//! Versions share these functions, so a version made by one shows in the other.
//!
//! A version is a whole project file, so it is not limited by the undo history, it
//! survives a restart, and each one costs a save of the project.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::project::LaneKind;
use crate::PlaybackSession;

/// One version on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub name: String,
    pub saved: SystemTime,
    /// Clips placed in the version, captions included; 0 when the file would not read.
    pub clips: usize,
}

/// What a save of a version did.
#[derive(Clone, Copy, Debug)]
pub struct Saved {
    /// When the version of the same name this replaced was written, if there was one.
    pub replaced: Option<SystemTime>,
    pub clips: usize,
}

/// Where the versions of `project` are kept: beside it, so they outlive the process.
pub fn dir_beside(project: &Path) -> PathBuf {
    let stem = project.file_stem().map_or_else(|| "untitled".to_string(), |f| f.to_string_lossy().into_owned());
    let parent = project.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    parent.join("checkpoints").join(stem)
}

/// A version name: 1 to 40 letters, digits, `-` or `_`, so it is a safe file name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What `typed` becomes as a name: trimmed, anything but letters, digits and `_` turned into `-`
/// (so `Before intro` is `Before-intro`), cut to 40.
/// `None` when nothing is left.
pub fn clean_name(typed: &str) -> Option<String> {
    let name: String = typed
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .take(40)
        .collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

pub fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.edith"))
}

/// The clips placed in the session, captions included.
pub fn clip_count(s: &PlaybackSession) -> usize {
    s.lanes()
        .into_iter()
        .map(|l| if l.kind == LaneKind::Subtitle { s.sub_lane(l).len() } else { s.lane_clips(l).len() })
        .sum()
}

/// Saves the open project as version `name` in `dir` (made when missing); a version of the
/// same name is replaced. The project file itself is not touched.
pub fn save(s: &PlaybackSession, dir: &Path, name: &str) -> crate::Result<Saved> {
    if !valid_name(name) {
        return Err(format!("version name `{name}` is not usable: 1 to 40 letters, digits, - or _").into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let path = file(dir, name);
    let replaced = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    s.save_project(&path)?;
    Ok(Saved { replaced, clips: clip_count(s) })
}

/// Saves the open state as `before-restore-<name>` (a numeric suffix when taken, cut to the 40
/// characters a name may hold), so a restore never discards unsaved edits. Returns the name.
pub fn save_before_restore(s: &PlaybackSession, dir: &Path, name: &str) -> crate::Result<String> {
    let base = format!("before-restore-{name}");
    let cut = |n: usize| base[..base.len().min(n)].trim_end().to_string();
    let mut auto = cut(40);
    let mut n = 2;
    while file(dir, &auto).exists() {
        let suffix = format!("-{n}");
        auto = format!("{}{suffix}", cut(40 - suffix.len()));
        n += 1;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    s.save_project(&file(dir, &auto))?;
    Ok(auto)
}

/// The versions in `dir`, newest first. A missing folder is no versions.
pub fn list(dir: &Path) -> Vec<Version> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Version> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "edith"))
        .filter_map(|p| {
            let name = p.file_stem()?.to_string_lossy().into_owned();
            let saved = std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            let clips = crate::edith::load(&p)
                .map(|d| d.lanes.iter().map(|(_, c)| c.len()).sum::<usize>() + d.subs.iter().map(Vec::len).sum::<usize>())
                .unwrap_or(0);
            Some(Version { name, saved, clips })
        })
        .collect();
    found.sort_by(|a, b| b.saved.cmp(&a.saved).then_with(|| a.name.cmp(&b.name)));
    found
}

/// How long ago `t` was, as a short phrase: `40 s ago`, `3 min ago`.
pub fn age_text(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map_or(0, |d| d.as_secs());
    match secs {
        0..=59 => format!("{secs} s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_file_safe() {
        assert!(valid_name("before-intro-2_x"));
        for bad in ["", " a", "a b", "a/b", "a.b", &"x".repeat(41)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(clean_name("  my v1/final?  ").as_deref(), Some("my-v1-final-"));
        assert_eq!(clean_name("   "), None);
        assert_eq!(clean_name(&"y".repeat(60)).unwrap().len(), 40);
    }

    #[test]
    fn the_folder_sits_beside_the_project() {
        assert_eq!(dir_beside(Path::new("/a/b/film.edith")), Path::new("/a/b/checkpoints/film"));
        assert_eq!(dir_beside(Path::new("film.edith")), Path::new("./checkpoints/film"));
    }

    #[test]
    fn save_list_and_before_restore_round_trip() {
        let dir = crate::scratch::Scratch::dir("ve_versions");
        let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let s = PlaybackSession::open(media).expect("open test asset");
        let vdir = dir.join("checkpoints");
        let saved = save(&s, &vdir, "first-take").expect("saves");
        assert!(saved.replaced.is_none());
        let again = save(&s, &vdir, "first-take").expect("saves again");
        assert!(again.replaced.is_some());
        assert!(save(&s, &vdir, "no/slash").is_err());
        let auto = save_before_restore(&s, &vdir, "first-take").expect("auto");
        assert_eq!(auto, "before-restore-first-take");
        assert_eq!(save_before_restore(&s, &vdir, "first-take").unwrap(), "before-restore-first-take-2");
        let all = list(&vdir);
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|v| v.name == "first-take" && v.clips == saved.clips));
        assert!(list(&dir.join("nothing")).is_empty());
    }
}
