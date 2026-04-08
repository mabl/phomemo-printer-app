//! Bluetooth SPP transport for Phomemo printers.
//!
//! - `rfcomm`: Raw RFCOMM socket connect/read/write via libc.
//! - `connmgr`: Persistent connection manager (reuses sockets across
//!   PAPPL device open/close cycles).
//! - `discovery`: `BlueZ` D-Bus device enumeration.

pub mod connmgr;
pub mod discovery;
pub mod rfcomm;
