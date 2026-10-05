//! What the driver needs from PAPPL while it prints.
//!
//! The driver is written against [`Host`] rather than PAPPL itself, so it is
//! plain safe Rust: [`ffi::Ops`](super::ffi::Ops) implements it over the C
//! callbacks, and the tests implement it over a byte buffer.

use std::io::Write;

use super::options::PrintOptions;
use crate::pappl::LogLevel;

/// Somewhere to report what the driver did and why.
pub trait Log {
    /// Add `message` to the job's log at `level`.
    fn log(&self, level: LogLevel, message: &str);
}

/// The PAPPL side of one raster callback: the job's log, its options, and
/// the device, which takes the page's bytes as an [`io::Write`](Write).
pub trait Host: Log + Write {
    /// The print options of the callback in progress.
    fn options(&self) -> &PrintOptions;

    /// Whether the job has been canceled.
    fn is_canceled(&self) -> bool;
}
