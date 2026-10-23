//! Environment switches the engine reads, with a process-local override layer.
//!
//! Editing the real environment is `unsafe` in Rust 2024, so the tests that pin a
//! switch (`VE_SW`, `VE_HW_TEST_ABORT`, ...) record it here instead. Engine code
//! reads these keys through [`var`] / [`var_os`], which answer from the overrides
//! first and fall back to the real environment. A helper process the engine
//! starts gets the overrides through [`apply`], because it reads its own
//! environment and not ours.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::sync::RwLock;

/// `None` as a value means "unset": the key behaves as if it were absent, even
/// when the real environment has it.
static OVERRIDES: RwLock<BTreeMap<&'static str, Option<OsString>>> = RwLock::new(BTreeMap::new());

fn table() -> std::sync::RwLockReadGuard<'static, BTreeMap<&'static str, Option<OsString>>> {
    OVERRIDES.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The value of `key`: the override if one is recorded, else the real environment.
pub fn var_os(key: &str) -> Option<OsString> {
    match table().get(key) {
        Some(overridden) => overridden.clone(),
        None => std::env::var_os(key),
    }
}

/// [`var_os`] as text; `None` when the value is absent or not valid Unicode.
pub fn var(key: &str) -> Option<String> {
    var_os(key).and_then(|value| value.into_string().ok())
}

/// Records `key = value` for every engine read from here on.
#[doc(hidden)]
pub fn set(key: &'static str, value: impl AsRef<OsStr>) {
    OVERRIDES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, Some(value.as_ref().to_os_string()));
}

/// Records `key` as absent for every engine read from here on.
#[doc(hidden)]
pub fn unset(key: &'static str) {
    OVERRIDES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, None);
}

/// Passes every recorded override on to a helper process that is about to start,
/// so the helper sees the same switches as this process.
#[doc(hidden)]
pub fn apply(command: &mut Command) {
    for (key, value) in table().iter() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
}
