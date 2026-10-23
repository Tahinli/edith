//! The last projects opened or saved, newest first, kept in `$XDG_STATE_HOME/edith/recent`
//! (one absolute path per line, at most [`MAX`]). The File menu's Open recent reads it, and
//! so will the welcome screen: [`list`] is the one API for both.

use std::path::{Path, PathBuf};

/// How many projects the list holds.
pub(crate) const MAX: usize = 10;

/// Why a listed project cannot be opened right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    /// The folder is there, the file is not: moved, renamed or deleted.
    Moved,
    /// The folder is gone too: an unplugged drive, an unmounted share.
    Folder,
}

impl Missing {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Missing::Moved => "File moved or deleted",
            Missing::Folder => "Folder not found",
        }
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recent {
    pub(crate) path: PathBuf,
    /// The file name without `.edith`.
    pub(crate) name: String,
    /// The folder it sits in, with the home folder written `~`.
    pub(crate) folder: String,
    /// `None` when the file is there.
    pub(crate) missing: Option<Missing>,
}

/// The recent projects, newest first, each checked against the disk.
pub(crate) fn list() -> Vec<Recent> {
    read(&file()).into_iter().map(|p| describe(p, home().as_deref())).collect()
}

/// Puts `project` at the top of the list (moving it there if it was already in it).
/// Best effort: a list that cannot be written costs the menu a row and nothing else.
pub(crate) fn push(project: &Path) {
    push_to(&file(), project);
}

/// Empties the list. The projects themselves are not touched.
pub(crate) fn clear() {
    let _ = std::fs::remove_file(file());
}

fn describe(path: PathBuf, home: Option<&Path>) -> Recent {
    let name = path.file_stem().map_or_else(|| "Untitled".to_string(), |s| s.to_string_lossy().into_owned());
    let dir = path.parent().unwrap_or(Path::new("/"));
    let folder = match home.and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    };
    let missing = match (path.is_file(), dir.is_dir()) {
        (true, _) => None,
        (false, true) => Some(Missing::Moved),
        (false, false) => Some(Missing::Folder),
    };
    Recent { path, name, folder, missing }
}

/// `folder` cut from the left to at most `max` characters, at a slash when it can: the end of a
/// path is the part that tells folders apart.
pub(crate) fn tail(folder: &str, max: usize) -> String {
    let n = folder.chars().count();
    if n <= max {
        return folder.to_string();
    }
    let cut: String = folder.chars().skip(n - (max - 1)).collect();
    match cut.find('/') {
        Some(at) if at + 1 < cut.len() => format!("…{}", &cut[at..]),
        _ => format!("…{cut}"),
    }
}

/// `folder` shortened to at most `max` characters by dropping the middle: the first folder
/// (`~`) stays, then `…`, then as many of the last folders as fit -- the two ends are the
/// parts that say where a folder is and which one it is.
pub(crate) fn middle(folder: &str, max: usize) -> String {
    if folder.chars().count() <= max {
        return folder.to_string();
    }
    let parts: Vec<&str> = folder.split('/').collect();
    let head = parts.first().copied().unwrap_or("");
    let mut tail: Vec<&str> = Vec::new();
    for part in parts.iter().skip(1).rev() {
        let mut next = tail.clone();
        next.insert(0, part);
        let text = format!("{head}/…/{}", next.join("/"));
        if text.chars().count() > max {
            break;
        }
        tail = next;
    }
    match tail.is_empty() {
        true => self::tail(folder, max),
        false => format!("{head}/…/{}", tail.join("/")),
    }
}

/// A path as the person sees it: the home folder written `~`, and cut from the left to fit.
pub(crate) fn short_path(path: &Path, max: usize) -> String {
    let shown = match home().as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    tail(&shown, max)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn file() -> PathBuf {
    // A test never touches the person's real list.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("edith-test-recent-{}", std::process::id()));
    }
    crate::Player::autosave_state_dir().map_or_else(|| PathBuf::from("recent"), |d| d.join("recent"))
}

fn read(file: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let p = PathBuf::from(line);
        if p.is_absolute() && !out.contains(&p) {
            out.push(p);
        }
    }
    out.truncate(MAX);
    out
}

fn push_to(file: &Path, project: &Path) {
    // The path as a person would find it; a project not yet on disk cannot be reopened.
    let Ok(path) = project.canonicalize() else {
        return;
    };
    if path.to_string_lossy().contains('\n') {
        return;
    }
    let mut all = read(file);
    all.retain(|p| p != &path);
    all.insert(0, path);
    all.truncate(MAX);
    let text: String = all.iter().map(|p| format!("{}\n", p.display())).collect();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Written whole beside the list and renamed over it: a crash leaves the old list.
    let part = file.with_extension("part");
    if std::fs::write(&part, text).is_ok() {
        let _ = std::fs::rename(&part, file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("edith-recent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_long_folder_loses_its_middle_and_keeps_both_ends() {
        let f = "~/Documents/Code/Rust/edith/projects/holiday";
        assert_eq!(middle(f, 80), f);
        let m = middle(f, 26);
        assert!(m.starts_with("~/…/") && m.ends_with("projects/holiday"), "{m}");
        assert!(m.chars().count() <= 26, "{m}");
        assert!(middle("/very/long/folder/name", 5).chars().count() <= 5);
    }

    #[test]
    fn newest_first_deduplicated_and_capped() {
        let d = dir("cap");
        let list = d.join("state/recent");
        let mut made = Vec::new();
        for n in 0..12 {
            let p = d.join(format!("p{n}.edith"));
            std::fs::write(&p, "x").unwrap();
            push_to(&list, &p);
            made.push(p.canonicalize().unwrap());
        }
        let got = read(&list);
        assert_eq!(got.len(), MAX);
        assert_eq!(got[0], made[11]);
        push_to(&list, &d.join("p5.edith"));
        let got = read(&list);
        assert_eq!((got[0].clone(), got.len()), (made[5].clone(), MAX));
        assert_eq!(got.iter().filter(|p| **p == made[5]).count(), 1);
        // A path that is not on disk is not listed.
        push_to(&list, &d.join("nothing.edith"));
        assert_eq!(read(&list), got);
    }

    #[test]
    fn long_folders_keep_their_end() {
        assert_eq!(tail("~/films", 20), "~/films");
        let t = tail("/home/me/Documents/Projects/summer/trip", 24);
        assert!(t.starts_with('…') && t.ends_with("summer/trip") && t.chars().count() <= 24, "{t}");
    }

    #[test]
    fn rows_say_why_a_project_is_missing_and_shorten_home() {
        let d = dir("rows");
        let home = d.join("home");
        std::fs::create_dir_all(home.join("films")).unwrap();
        let here = home.join("films/a.edith");
        std::fs::write(&here, "x").unwrap();
        let r = describe(here, Some(&home));
        assert_eq!((r.name.as_str(), r.folder.as_str(), r.missing), ("a", "~/films", None));
        let moved = describe(home.join("films/b.edith"), Some(&home));
        assert_eq!(moved.missing, Some(Missing::Moved));
        let gone = describe(d.join("usb/c.edith"), Some(&home));
        assert_eq!(gone.missing, Some(Missing::Folder));
        assert_eq!(describe(home.join("d.edith"), Some(&home)).folder, "~");
    }
}
