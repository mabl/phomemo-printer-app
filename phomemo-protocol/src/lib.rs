//! Phomemo printer protocol encoder/decoder.
//!
//! Pure Rust, no unsafe code and no FFI. Covers command encoding, the page
//! byte stream, response decoding, bitmap handling and dithering, the media
//! catalog, and the per-model capability table for the Phomemo/QY printer
//! family.
//!
//! The crate is transport- and framework-agnostic: it produces and consumes
//! byte slices and plain data, and knows nothing about the print system that
//! drives it.
//!
//! - [`job`] encodes the complete byte stream for a page.
//! - [`commands`] encodes the individual commands it is made of, plus status
//!   queries.
//! - [`responses`] decodes what the printer sends back, incrementally.
//! - [`bitmap`] and [`dither`] turn raster data into the 1-bit images the
//!   printer accepts.
//! - [`media`] and [`model`] describe the media and printers.
// These are crate attributes rather than `[lints.rust]` in Cargo.toml
// because Cargo rejects crate-local lints alongside `lints.workspace = true`,
// and a workspace-wide `missing_docs` would also cover phomemo-pappl. `make
// lint` (`-D warnings`) turns the warning into an error.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bitmap;
pub mod commands;
#[cfg(feature = "compress")]
pub mod compress;
pub mod dither;
pub mod job;
pub mod media;
pub mod model;
pub mod responses;
