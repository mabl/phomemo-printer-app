//! PAPPL driver and Bluetooth transport for Phomemo printers.
//!
//! This crate builds `libphomemo_pappl.a`, the static library the C PAPPL
//! application in `c/` links against; cbindgen generates its header,
//! `generated/phomemo_pappl.h`. The C side keeps what has to touch PAPPL's
//! types - registering callbacks, reading fields out of PAPPL's structs -
//! and hands Rust plain data and a table of PAPPL functions; the decisions
//! are made here:
//!
//! - `models`: the printer models, and the views of them C holds.
//! - `defaults`: the capabilities each model reports to PAPPL.
//! - `media`: what PAPPL's media description means for the printer.
//! - `overprint`: design canvases larger than the label, and where they
//!   reach the head.
//! - `strings`: the English strings catalog PAPPL shows names from.
//! - `autoadd`: which driver a discovered device gets.
//! - `raster`: the raster driver, from PAPPL's pages to the printer's
//!   byte stream.
//! - `testpage`: the test page.
//! - `bt`: the Bluetooth device backend.
//! - `pappl`: the PAPPL and CUPS values mirrored from their headers, each
//!   checked against them when the C side compiles.
//!
//! Everything C sees is named with a `pm_`, `Pm` or `PM_` prefix in Rust as
//! well, so a name in C code finds its definition here.

// Crate attributes rather than `[lints]`, which Cargo does not allow
// alongside `lints.workspace = true`. `make lint` turns warnings into
// errors.
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod autoadd;
mod bt;
mod defaults;
mod media;
mod models;
mod overprint;
mod pappl;
mod raster;
mod strings;
mod testpage;

// The types and constants of the C interface. Re-exporting them makes them
// the crate's public API, so fields only C reads are not dead code.
pub use crate::bt::ffi::PmBtConnection;
pub use crate::defaults::{PmDriverDefaults, PmMediaDefault};
pub use crate::models::PmModel;
pub use crate::overprint::PmOverprintInfo;
pub use crate::pappl::*;
pub use crate::raster::ffi::{
    PM_MEDIA_NAME_SIZE, PM_VENDOR_VALUE_SIZE, PmJob, PmJobContext, PmJobSent, PmOps, PmOptions,
};
