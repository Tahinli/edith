//! The check every test that needs a hardware seat runs first.
//!
//! The seats open `hw-child` beside the executable, and nothing builds it for
//! `cargo test -p engine`. Without it every hardware call answers "no" and the
//! test fails with a vague "no plugin available", or quietly takes the software
//! path. So on a machine that has a render node, a missing child is a panic
//! that names the build command. On a machine without one, the tests keep their
//! old behaviour and skip.
//!
//! Included by path (`#[path = "support/hwchild.rs"] mod hwchild;`); not a test
//! target of its own.
#![allow(dead_code)]

/// Panics when this machine has a render node but no `hw-child` can be found.
pub fn require() {
    if engine::hwproc::child_path().is_some() || !engine::hwproc::render_node_present() {
        return;
    }
    panic!(
        "hw-child not built: run `{}` (or set VE_HW_CHILD_BIN)",
        engine::hwproc::BUILD_CHILD
    );
}
