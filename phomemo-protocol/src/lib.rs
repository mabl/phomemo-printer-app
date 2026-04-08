//! Phomemo printer protocol encoder/decoder.
//!
//! Pure Rust, zero unsafe, zero FFI. Covers command encoding, response
//! parsing, bitmap packing, media definitions, and per-model capability
//! databases for the Phomemo/QY printer family.
//!
//! This crate is intentionally transport- and framework-agnostic: it
//! produces and consumes byte slices, nothing else.

pub mod bitmap;
pub mod commands;
#[cfg(feature = "compress")]
pub mod compress;
pub mod dither;
pub mod media;
pub mod model;
pub mod responses;
