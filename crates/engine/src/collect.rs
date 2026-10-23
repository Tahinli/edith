//! Collect a project's media: copy every source the project uses into one folder and point
//! the project at the copies, so the folder and the `.edith` can be moved or archived together.
//!
//! Three steps the window and the MCP server share: [`plan`] reads the sources and decides the
//! copies, [`copy_all`] makes them (cancellable, with progress), and
//! [`crate::PlaybackSession::relink_all`] points the project at them as one undo step.
//! Generated sources (solids, bars, titles' backdrops) are files nowhere and are skipped; a source
//! whose file is missing is skipped and named; one already inside the folder stays where it is.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::project::Source;

/// One file to copy, and every source (stream) of the project that plays from it.
#[derive(Clone, Debug)]
pub struct Item {
    pub indices: Vec<usize>,
    pub from: PathBuf,
    pub to: PathBuf,
    pub bytes: u64,
    /// The copy is already in the folder and is the same bytes: nothing to write.
    pub present: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub folder: PathBuf,
    pub items: Vec<Item>,
    /// Sources that are generated, not files.
    pub generated: usize,
    /// Files the project names that are not on disk.
    pub offline: Vec<PathBuf>,
    /// Sources already inside the folder.
    pub already: usize,
}

impl Plan {
    /// Bytes still to write.
    pub fn bytes(&self) -> u64 {
        self.items.iter().filter(|i| !i.present).map(|i| i.bytes).sum()
    }
}

/// What a finished copy did.
#[derive(Clone, Debug, Default)]
pub struct Done {
    /// `(source index, the copy)`, ready for [`crate::PlaybackSession::relink_all`].
    pub pairs: Vec<(usize, PathBuf)>,
    /// Files written now.
    pub files: usize,
    pub bytes: u64,
    created: Vec<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    Failed(String),
}

/// `<project dir>/<project stem>_media`.
pub fn default_folder(project: &Path) -> PathBuf {
    let stem = project.file_stem().map_or_else(|| "project".to_string(), |s| s.to_string_lossy().into_owned());
    project.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).join(format!("{stem}_media"))
}

/// Decides the copies of `sources` into `folder`. Reads file sizes; writes nothing.
pub fn plan(sources: &[Source], folder: &Path) -> Plan {
    let mut plan = Plan { folder: folder.to_path_buf(), ..Plan::default() };
    let real_folder = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    let mut taken: Vec<PathBuf> = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        if crate::generate::is_generated(&source.path) {
            plan.generated += 1;
            continue;
        }
        let Ok(meta) = std::fs::metadata(&source.path) else {
            if !plan.offline.contains(&source.path) {
                plan.offline.push(source.path.clone());
            }
            continue;
        };
        if source.path.parent() == Some(real_folder.as_path()) {
            plan.already += 1;
            continue;
        }
        // Two streams of one file are two sources and one copy.
        if let Some(item) = plan.items.iter_mut().find(|i| i.from == source.path) {
            item.indices.push(index);
            continue;
        }
        let (to, present) = destination(&source.path, folder, meta.len(), &taken);
        taken.push(to.clone());
        plan.items.push(Item { indices: vec![index], from: source.path.clone(), to, bytes: meta.len(), present });
    }
    plan
}

/// Where `from` lands: its own name in `folder`; if that name is taken by another file (or by
/// another source of this plan) `name-2.ext`, `name-3.ext`... The same bytes already there are
/// reused, so collecting twice copies nothing twice.
fn destination(from: &Path, folder: &Path, len: u64, taken: &[PathBuf]) -> (PathBuf, bool) {
    let name = from.file_name().map_or_else(|| "media".to_string(), |n| n.to_string_lossy().into_owned());
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (name[..dot].to_string(), name[dot..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let mut n = 1;
    loop {
        let candidate = if n == 1 { folder.join(&name) } else { folder.join(format!("{stem}-{n}{ext}")) };
        if !taken.contains(&candidate) {
            match std::fs::metadata(&candidate) {
                Err(_) => return (candidate, false),
                Ok(m) if m.len() == len && same_bytes(from, &candidate) => return (candidate, true),
                Ok(_) => {}
            }
        }
        n += 1;
    }
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let (Ok(na), Ok(nb)) = (read_full(&mut fa, &mut ba), read_full(&mut fb, &mut bb)) else {
            return false;
        };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// Makes the copies of `plan`. `progress(done, total)` counts bytes and is called as the copy
/// goes; `cancel` is looked at between chunks. A cancel or a failure removes what this call wrote
/// and returns [`Stop`]; the project is not touched either way.
pub fn copy_all(plan: &Plan, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<Done, Stop> {
    let total = plan.bytes();
    let mut done = Done::default();
    let mut written = 0u64;
    let fail = |done: &Done, stop: Stop| {
        for p in &done.created {
            let _ = std::fs::remove_file(p);
        }
        Err(stop)
    };
    if plan.items.iter().any(|i| !i.present)
        && let Err(e) = std::fs::create_dir_all(&plan.folder)
    {
        return Err(Stop::Failed(format!("cannot make {}: {e}", plan.folder.display())));
    }
    progress(0, total);
    for item in &plan.items {
        if !item.present {
            let mut name = item.to.file_name().unwrap_or_default().to_os_string();
            name.push(".edith-part");
            let part = item.to.with_file_name(name);
            let result = copy_one(&item.from, &part, cancel, &mut |n| {
                written += n;
                progress(written, total);
            });
            match result {
                Ok(()) => {}
                Err(stop) => {
                    let _ = std::fs::remove_file(&part);
                    return fail(&done, stop);
                }
            }
            if let Err(e) = std::fs::rename(&part, &item.to) {
                let _ = std::fs::remove_file(&part);
                return fail(&done, Stop::Failed(format!("cannot write {}: {e}", item.to.display())));
            }
            done.created.push(item.to.clone());
            done.files += 1;
            done.bytes += item.bytes;
        }
        done.pairs.extend(item.indices.iter().map(|&i| (i, item.to.clone())));
    }
    Ok(done)
}

fn copy_one(from: &Path, to: &Path, cancel: &AtomicBool, step: &mut dyn FnMut(u64)) -> Result<(), Stop> {
    let mut src = std::fs::File::open(from).map_err(|e| Stop::Failed(format!("cannot read {}: {e}", from.display())))?;
    let mut dst = std::fs::File::create(to).map_err(|e| Stop::Failed(format!("cannot write {}: {e}", to.display())))?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        let n = src.read(&mut buf).map_err(|e| Stop::Failed(format!("cannot read {}: {e}", from.display())))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).map_err(|e| Stop::Failed(format!("cannot write {}: {e}", to.display())))?;
        step(n as u64);
    }
    dst.sync_all().map_err(|e| Stop::Failed(format!("cannot write {}: {e}", to.display())))?;
    Ok(())
}

/// `1.4 GB`, `82 MB`, `3 KB`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [(&str, f64); 3] = [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    for (unit, size) in UNITS {
        if bytes as f64 >= size {
            let v = bytes as f64 / size;
            return if v >= 100. || unit == "KB" { format!("{v:.0} {unit}") } else { format!("{v:.1} {unit}") };
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn plans_skip_generated_and_offline_and_rename_clashes() {
        let dir = crate::scratch::Scratch::dir("ve_collect_plan");
        let (a, b) = (dir.join("one/clip.mp4"), dir.join("two/clip.mp4"));
        write(&a, b"aaaa");
        write(&b, b"bbbbbb");
        let gone = dir.join("gone.mp4");
        let solid = crate::generate::Gen::named("solid", 64, 64, 25.).unwrap().path();
        let sources = vec![Source::new(&a, 0), Source::new(&a, 1), Source::new(&b, 0), Source { path: gone.clone(), audio_stream: 0, ..Default::default() }, Source { path: solid, audio_stream: 0, ..Default::default() }];
        let folder = dir.join("media");
        let p = plan(&sources, &folder);
        assert_eq!((p.generated, p.offline.clone(), p.items.len()), (1, vec![gone], 2));
        assert_eq!(p.items[0].indices, vec![0, 1]);
        assert_eq!(p.items[0].to, folder.join("clip.mp4"));
        assert_eq!(p.items[1].to, folder.join("clip-2.mp4"));
        assert_eq!(p.bytes(), 10);

        let mut seen = 0;
        let done = copy_all(&p, &AtomicBool::new(false), &mut |d, t| {
            assert!(d <= t);
            seen = d;
        })
        .expect("copies");
        assert_eq!((done.files, done.bytes, seen), (2, 10, 10));
        assert_eq!(done.pairs.len(), 3);
        assert_eq!(std::fs::read(folder.join("clip-2.mp4")).unwrap(), b"bbbbbb");
        // Collecting again finds the same bytes there: nothing to write.
        let again = plan(&sources, &folder);
        assert!(again.items.iter().all(|i| i.present) && again.bytes() == 0);
        // And sources already in the folder stay.
        let moved = vec![Source::new(folder.join("clip.mp4"), 0)];
        assert_eq!(plan(&moved, &folder).already, 1);
    }

    #[test]
    fn a_cancel_leaves_nothing_behind() {
        let dir = crate::scratch::Scratch::dir("ve_collect_cancel");
        let a = dir.join("a.bin");
        write(&a, &vec![7u8; 3 << 20]);
        let p = plan(&[Source::new(&a, 0)], &dir.join("m"));
        let cancel = AtomicBool::new(false);
        let r = copy_all(&p, &cancel, &mut |d, _| {
            if d > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(r.err(), Some(Stop::Cancelled));
        assert_eq!(std::fs::read_dir(dir.join("m")).unwrap().count(), 0);
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(size_text(999), "999 B");
        assert_eq!(size_text(3_400), "3 KB");
        assert_eq!(size_text(82_000_000), "82.0 MB");
        assert_eq!(size_text(1_400_000_000), "1.4 GB");
    }

    #[test]
    fn collecting_relinks_a_session_in_one_undo_step() {
        let dir = crate::scratch::Scratch::dir("ve_collect_session");
        let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let mut s = crate::PlaybackSession::open(&media).expect("open test asset");
        let was = s.sources()[0].path.clone();
        let p = plan(s.sources(), &dir.join("m"));
        let done = copy_all(&p, &AtomicBool::new(false), &mut |_, _| {}).expect("copies");
        assert_eq!(s.relink_all(&done.pairs), 1);
        assert_eq!(s.sources()[0].path.parent().unwrap().file_name().unwrap(), "m");
        assert_eq!(s.relink_all(&done.pairs), 0, "already there: no second step");
        assert!(s.undo());
        assert_eq!(s.sources()[0].path, was);
    }
}
