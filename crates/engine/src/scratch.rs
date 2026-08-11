//! Throwaway paths for the tests, and the one rule about them: everything a
//! test writes goes inside a directory that deletes itself.
//!
//! [`Scratch`] is that directory. It derefs to a path -- the directory itself,
//! or the one file inside it -- so a helper that used to hand back a `PathBuf`
//! hands back one of these instead and its call sites do not change. `Drop` is
//! what cleans up, which is the whole point: unwinding runs it, so a *failing*
//! test leaves nothing behind either. The suites used to litter `/tmp` with a
//! fresh `ve_*` directory per run and never take one back; on this machine that
//! reached 2907 of them.
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
    let dir = std::env::temp_dir().join(format!(
        "{name}_{}_{}",
        std::process::id(),
        NTH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::canonicalize(&dir).expect("canonical scratch dir")
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
    use super::Scratch;

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
