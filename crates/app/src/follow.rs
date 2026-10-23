//! Live follow: whether the `.edith` the window has open was written on disk by
//! someone else (the agent, through `edith_mcp`) since the window last looked.
//! The decision is a pure function of two stamps and one flag, so it reads and
//! tests without a window; [`crate::Player`] supplies the three inputs.

use std::path::Path;
use std::time::SystemTime;

/// A project file as the window last saw it: when it was written and how long
/// it is. Both must match for a file to count as unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) mtime: SystemTime,
    pub(crate) len: u64,
}

/// What the window does about one look at the project file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// Nothing settled to read (missing, empty, or mid-write). Change nothing
    /// and look again on the next tick.
    Wait,
    /// The file is the one the window already accounts for.
    Idle,
    /// The window had no stamp for this file yet. Take the current one as the
    /// known state, silently.
    Adopt,
    /// Changed on disk and the window holds no unsaved edits: reload in place.
    Reload,
    /// Changed on disk under unsaved edits: ask once, and keep the question
    /// away from the window until the file changes again.
    Ask,
}

/// The whole rule. `known` is the stamp the window last accounted for (set on
/// open, on its own save, and when a change has been answered). `now` is the
/// file as it stands. `edited` says the window holds edits the file does not.
///
/// The window's own save needs no case here: [`crate::Player::save_project`]
/// records the stamp it just wrote as `known`, so its own write reads as `Idle`.
pub(crate) fn follow_decision(known: Option<Stamp>, now: Option<Stamp>, edited: bool) -> Follow {
    match (known, now) {
        (_, None) => Follow::Wait,
        (None, Some(_)) => Follow::Adopt,
        (Some(known), Some(now)) if known == now => Follow::Idle,
        (Some(_), Some(_)) if edited => Follow::Ask,
        (Some(_), Some(_)) => Follow::Reload,
    }
}

/// The stamp of `path` as it stands, or `None` when there is nothing settled to
/// read: no path, no file, a zero-length file, or a `<path>.part` beside it. The
/// engine writes `<path>.part` and renames it over `path`, so the file is only
/// final once that sibling is gone.
pub(crate) fn stamp(path: &Path) -> Option<Stamp> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    if Path::new(&part).exists() {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 {
        return None;
    }
    Some(Stamp {
        mtime: meta.modified().ok()?,
        len: meta.len(),
    })
}
