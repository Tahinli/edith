//! The decode seat for a test, shared by the engine's test binaries.
//!
//! `VE_SW` is process-wide and the test harness runs a binary's tests on many
//! threads, so a test that pins it for its own seat must not share the process
//! with one that unpins it. Every test that pins a seat takes [`SEAT`] for as
//! long as its guard lives, and the guard leaves `VE_SW` as it found it: the
//! software seat's tests are then one at a time, and none of them can see a
//! hardware pin that another test is holding.
//!
//! A test that means "the software seat" takes [`software`]; one that means "the
//! plugin, if the machine has it" takes [`hardware`]. A test that takes neither
//! is one whose assertions hold on either seat.

use std::sync::{Mutex, MutexGuard};

static SEAT: Mutex<()> = Mutex::new(());

/// Holds the seat for the test's duration. Dropping it removes `VE_SW`.
#[must_use = "the seat holds only while its guard is alive"]
pub struct Seat {
    _lock: MutexGuard<'static, ()>,
}

fn lock() -> MutexGuard<'static, ()> {
    SEAT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The software seat: the plugin forced off for as long as the guard lives.
#[allow(dead_code)]
pub fn software() -> Seat {
    let guard = lock();
    engine::envcfg::set("VE_SW", "1");
    Seat { _lock: guard }
}

/// The plugin, as the machine has it: `VE_SW` unset for as long as the guard lives.
#[allow(dead_code)]
pub fn hardware() -> Seat {
    let guard = lock();
    engine::envcfg::unset("VE_SW");
    Seat { _lock: guard }
}

impl Drop for Seat {
    fn drop(&mut self) {
        engine::envcfg::unset("VE_SW");
    }
}
