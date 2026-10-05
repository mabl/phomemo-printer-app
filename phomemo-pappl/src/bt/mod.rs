//! The Bluetooth SPP device backend: PAPPL's `btspp://` scheme.
//!
//! - `address`: Bluetooth addresses, RFCOMM channels and `btspp://` URIs.
//! - `rfcomm`: the RFCOMM socket.
//! - `link`: a printer's link - the socket and what the printer reported.
//! - `connmgr`: keeps each printer's link open across PAPPL's device
//!   sessions, and closes idle ones.
//! - `discovery`: paired printers, from `BlueZ` over D-Bus.
//! - `flight`: runs a query on a helper thread, one at a time.
//! - `status`: the printer's status from its answers to status queries.
//! - `completion`: waiting until the printer has printed a job.
//! - `ffi`: the `pm_bt_*` exports behind `c/device_bt.c`.

mod address;
mod completion;
mod connmgr;
mod discovery;
pub mod ffi;
mod flight;
mod link;
mod rfcomm;
mod status;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `mutex`, whether or not a thread panicked holding it: every
/// critical section in this backend leaves its state consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
