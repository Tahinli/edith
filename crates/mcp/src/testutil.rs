//! Helpers shared by the crate's tests.

use std::path::{Path, PathBuf};

/// A fresh scratch folder under the temp dir, removed when dropped, so a test that
/// panics does not leave it behind either.
pub struct TestDir(PathBuf);

impl TestDir {
    /// `<temp>/edith_mcp-test-<name>-<pid>`, emptied first.
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("edith_mcp-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}
