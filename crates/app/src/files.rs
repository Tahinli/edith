//! Paths in and out: the pickers, the URIs, the names.

use crate::*;

/// How a finished export announces itself. Written by `poll_export` and read
/// by the notice bar, which is what makes that one line clickable.
pub(crate) const EXPORT_DONE: &str = "EXPORT DONE → ";

/// The path as a URI the bus will take: percent-encoded, because an export
/// lands wherever its source lives and those directories have spaces in them.
/// Bytes, not chars -- a path is not required to be UTF-8.
pub(crate) fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

/// Shows a file in the desktop's file manager, selected: the freedesktop
/// interface every major one answers, asked for over the session bus. With no
/// file manager on the bus the folder itself is the next best thing, and with
/// neither there is nothing to say -- the notice the click retired was the
/// answer, and a machine without a desktop opener is not one a second notice
/// would help. Blocks on two child processes, so it is never called on the UI
/// thread.
pub(crate) fn show_in_file_manager(path: &std::path::Path) {
    // The URI must be absolute; an export path is only as absolute as the
    // source it was built from.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let shown = std::process::Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            "ass",
            "1",
            &file_uri(&path),
            "",
        ])
        .status()
        .is_ok_and(|s| s.success());
    if shown {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).status();
    }
}

/// Runs the first chooser that is installed. `Some(None)` is a cancelled
/// dialog; `None` is a machine with no chooser at all, and what still works
/// without one differs per dialog, so the caller words that refusal.
///
/// The desktop's own choosers, asked for by name because gpui 0.2 has no file
/// dialog of its own and none of these is worth a dependency.
pub(crate) fn run_picker(pickers: [(&str, Vec<String>); 2]) -> Option<Option<PathBuf>> {
    for (bin, args) in pickers {
        // Not installed: try the next one. Anything else (a cancel, a refusal)
        // is that chooser's answer and is taken as final.
        let Ok(out) = std::process::Command::new(bin).args(args).output() else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Some((!path.is_empty()).then(|| PathBuf::from(path)));
    }
    None
}

/// `title` is what the dialog calls itself: two buttons open this same chooser
/// for two different questions -- a file to import, a file to take subtitles out
/// of -- and a dialog titled "import" over the second one is the wrong question
/// answered. No extension filter on either: what can be read is the engine's
/// answer (`PlaybackSession::parse_subtitles` takes a container as readily as a
/// `.srt`), and a list of suffixes written here would hide a file edith would
/// have taken.
pub(crate) fn pick_file(title: &str) -> Result<Option<PathBuf>, &'static str> {
    run_picker([
        (
            "zenity",
            vec!["--file-selection".into(), format!("--title={title}")],
        ),
        ("kdialog", vec!["--getopenfilename".into()]),
    ])
    .ok_or(
        "NO FILE CHOOSER — install zenity or kdialog, or drag the file onto this window to import it",
    )
}

/// The save-side dialog, opened on where the export would land anyway: with no
/// chooser installed that default is still what gets written, so this refusal
/// costs the export nothing and says so.
pub(crate) fn pick_save(default: &std::path::Path) -> Result<Option<PathBuf>, &'static str> {
    let default = default.to_string_lossy().into_owned();
    run_picker([
        (
            "zenity",
            vec![
                "--file-selection".into(),
                // No `--confirm-overwrite`: zenity 4.2 lists it as deprecated
                // and does the confirming itself.
                "--save".into(),
                "--title=edith — export to".into(),
                format!("--filename={default}"),
            ],
        ),
        ("kdialog", vec!["--getsavefilename".into(), default]),
    ])
    .ok_or(
        "NO FILE CHOOSER — install zenity or kdialog to choose where; exporting beside the source",
    )
}

/// Where an export goes: the source path with `.export.mp4` for an extension,
/// so it lands beside the original and can never be the original.
pub(crate) fn export_path(source: impl Into<PathBuf>) -> PathBuf {
    let mut path = source.into();
    path.set_extension("export.mp4");
    path
}

/// Where a save goes when the timeline did not come from a project file: the
/// media path with `.edith` for an extension, beside it like an export. A
/// project loaded from disk keeps its own path instead, so saving it twice
/// writes the same file.
pub(crate) fn project_path(source: impl Into<PathBuf>) -> PathBuf {
    let mut path = source.into();
    path.set_extension("edith");
    path
}

/// Whether a dropped or named path is a project rather than media. Exactly the
/// lowercase extension `save_project` writes -- anything else goes to the
/// demuxer, which is the one that can say what it really is.
pub(crate) fn is_project(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e == "edith")
}

/// The tail of a path, for showing. A path that is all root has none, and reads
/// as itself.
pub(crate) fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}
