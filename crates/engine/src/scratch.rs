//! Throwaway paths for the tests, and the one rule about them: everything a
//! test writes goes inside a directory that deletes itself.
//!
//! [`Scratch`] is that directory. It derefs to a path -- the directory itself,
//! or the one file inside it -- so a helper that used to hand back a `PathBuf`
//! hands back one of these instead and its call sites do not change. `Drop` is
//! what cleans up, which is the whole point: unwinding runs it, so a *failing*
//! test leaves nothing behind either. The suites used to litter `/tmp` with a
//! fresh `ve_*` directory per run and never take one back; on this machine that
//! reached 2907 of them. A test the kernel *aborts* never unwinds and so never
//! drops anything; [`reap_orphans`] takes those on the next run instead.
//!
//! Nothing outside a test has any use for this. It lives in the library rather
//! than in a test file because the integration suites, the unit tests here and
//! the app's own tests all need the same guard, and none of them can see each
//! other's modules.

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A temporary directory and the path a test uses inside it, gone when the
/// value is.
#[derive(Debug)]
pub struct Scratch {
    dir: PathBuf,
    path: PathBuf,
}

/// A fresh, empty directory under the system temporary one. `name` leads it so
/// it is recognisable while a test is stopped in it; the process id and a
/// counter follow, because two suites (or two tests of one suite, in parallel)
/// must not write each other's files. Canonical, because saved projects hold
/// paths relative to their own directory and `Project` canonicalises what it is
/// handed.
fn fresh(name: &str) -> PathBuf {
    static NTH: AtomicU64 = AtomicU64::new(0);
    static REAP: std::sync::Once = std::sync::Once::new();
    REAP.call_once(reap_orphans);
    let dir = std::env::temp_dir().join(format!(
        "{name}_{}_{}",
        std::process::id(),
        NTH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::canonicalize(&dir).expect("canonical scratch dir")
}

/// The one leak `Drop` cannot reach: a test the kernel *aborts* -- the AV1
/// hardware encoder does it whenever the driver's ring hangs -- never unwinds,
/// so its directory outlives it. The name carries the process id that made it,
/// so a later run can tell an orphan from a directory a live sibling suite is
/// still writing into, and takes only the orphans. Once per process, before the
/// first scratch directory of it: the suites are what fills `/tmp`, and they all
/// come through here.
fn reap_orphans() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    if !Path::new("/proc").is_dir() {
        return; // No way to ask whether the owner is still running: take nothing.
    }
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `ve_<what>_<pid>_<nth>`, and nothing else, ever.
        let mut tail = name.rsplitn(3, '_');
        let (Some(nth), Some(pid), Some(head)) = (tail.next(), tail.next(), tail.next()) else {
            continue;
        };
        if !head.starts_with("ve_") || nth.parse::<u64>().is_err() {
            continue;
        }
        let Ok(pid) = pid.parse::<u64>() else { continue };
        if Path::new("/proc").join(pid.to_string()).exists() {
            continue; // Still running: its own `Drop` will do this.
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

impl Scratch {
    /// A directory to litter: the value *is* the directory.
    pub fn dir(name: &str) -> Self {
        let dir = fresh(name);
        Self {
            path: dir.clone(),
            dir,
        }
    }

    /// A path for the one file a test writes, `name.ext`, alone in a directory
    /// of its own. Nothing creates it -- what is under test is what writes it,
    /// and several of these tests are about a file that must *not* appear.
    pub fn file(name: &str, ext: &str) -> Self {
        let dir = fresh(name);
        let path = dir.join(format!("{name}.{ext}"));
        Self { dir, path }
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// So one of these can be handed to a `Command` as an argument, which is how
/// the suites that check our output with ffmpeg name the file.
impl AsRef<OsStr> for Scratch {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::{Scratch, reap_orphans};

    /// What an aborted test leaves goes on the next run's account: its
    /// directory is taken, a live process's is not, and nothing that is not
    /// ours is touched whatever it is named.
    #[test]
    fn the_next_run_takes_what_an_aborted_one_left() {
        let tmp = std::env::temp_dir();
        // No process has this id: `pid_max` is an exclusive bound everywhere.
        let orphan = tmp.join("ve_scratch_orphan_4294967295_0");
        let mine = tmp.join(format!("ve_scratch_live_{}_9999", std::process::id()));
        let theirs = tmp.join("someone_elses_4294967295_0");
        for dir in [&orphan, &mine, &theirs] {
            std::fs::create_dir_all(dir).expect("a directory to reap");
        }

        reap_orphans();

        assert!(!orphan.exists(), "the aborted run's directory is gone");
        assert!(mine.is_dir(), "a running suite's own directory is left alone");
        assert!(theirs.is_dir(), "and so is everything that is not ours");
        for dir in [&mine, &theirs] {
            std::fs::remove_dir_all(dir).expect("cleanup");
        }
    }

    /// The guard's whole job, both shapes: what it hands back is usable, and
    /// what it made is gone once it is -- including when a test panicked, which
    /// is the case that leaked.
    #[test]
    fn a_scratch_takes_its_directory_with_it() {
        let (dir, file) = (Scratch::dir("ve_scratch_dir"), Scratch::file("ve_scratch", "mp4"));
        assert!(dir.is_dir(), "a directory to litter, ready to use");
        std::fs::write(dir.join("a.txt"), b"litter").expect("write into it");
        assert!(!file.exists(), "the file itself is the test's to write");
        assert!(file.parent().expect("inside a directory").is_dir());
        assert_eq!(file.extension().and_then(|e| e.to_str()), Some("mp4"));

        let (left, right) = (dir.to_path_buf(), file.parent().unwrap().to_path_buf());
        assert_ne!(left, right, "one directory each, never shared");
        drop((dir, file));
        assert!(!left.exists(), "the directory went with the guard");
        assert!(!right.exists(), "and so did the file's");

        // Unwinding runs `Drop`, so a failing test cleans up like a passing one.
        // The panic below is this test's own and is meant to reach stderr.
        let seen = std::sync::Mutex::new(None);
        let _ = std::panic::catch_unwind(|| {
            let dir = Scratch::dir("ve_scratch_panic");
            std::fs::write(dir.join("a.txt"), b"litter").expect("write into it");
            *seen.lock().expect("the lock") = Some(dir.to_path_buf());
            panic!("a test failing with a scratch directory in hand");
        });
        let path = seen.lock().expect("the lock").clone().expect("it ran");
        assert!(!path.exists(), "a panicking test leaves nothing behind");
    }
}
