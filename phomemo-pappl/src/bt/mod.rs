//! Bluetooth SPP transport for Phomemo printers.
//!
//! - `rfcomm`: Raw RFCOMM socket connect/read/write via libc.
//! - `connmgr`: Persistent connection manager (reuses sockets across
//!   PAPPL device open/close cycles).
//! - `discovery`: `BlueZ` D-Bus device enumeration.
//! - `status`: Printer status from the status-query answers.
//! - `ffi`: The `pm_bt_*` exports behind `c/device_bt.c`.

#[expect(
    clippy::undocumented_unsafe_blocks,
    reason = "the transport internals predate the lint; the Bluetooth rework documents them"
)]
pub mod connmgr;
pub mod discovery;
pub mod ffi;
#[expect(
    clippy::undocumented_unsafe_blocks,
    reason = "the transport internals predate the lint; the Bluetooth rework documents them"
)]
pub mod rfcomm;
mod status;
