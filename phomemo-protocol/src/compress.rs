//! LZO compression of raster data.
//!
//! The vendor apps compress raster data with miniLZO's LZO1X-1 in blocks of
//! [`BLOCK_SIZE`] input bytes, each compressed independently and prefixed
//! with its compressed length as three little-endian bytes
//! (`re/protocol/bitmap-encoding.md`, "Compression (miniLZO)"). This module
//! produces that format with `lzokay-native`, a pure-Rust LZO1X compressor
//! whose output miniLZO decompresses.

use std::error::Error;
use std::fmt;

use lzokay_native::Dict;

/// Uncompressed bytes per block (the vendor library's `allocSize`).
pub const BLOCK_SIZE: usize = 4096;

/// Raster data that could not be compressed.
#[derive(Debug)]
pub struct CompressError(Repr);

#[derive(Debug)]
enum Repr {
    Lzo(lzokay_native::Error),
    BlockTooLarge(usize),
}

impl fmt::Display for CompressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Lzo(_) => f.write_str("LZO compression failed"),
            Repr::BlockTooLarge(len) => {
                write!(
                    f,
                    "compressed block of {len} bytes overflows its 3-byte length"
                )
            }
        }
    }
}

impl Error for CompressError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.0 {
            Repr::Lzo(err) => Some(err),
            Repr::BlockTooLarge(_) => None,
        }
    }
}

/// Compress `data` into length-prefixed LZO blocks:
///
/// ```text
/// [len0 len1 len2] [compressed block 0] [len0 len1 len2] [compressed block 1] ...
/// ```
///
/// Empty input produces no blocks.
///
/// # Errors
///
/// Fails if the compressor fails, or a block's compressed length does not
/// fit in three bytes.
pub fn compress_blocks(data: &[u8]) -> Result<Vec<u8>, CompressError> {
    let mut dict = Dict::new();
    let mut out = Vec::with_capacity(data.len() / 2);
    for block in data.chunks(BLOCK_SIZE) {
        let compressed = lzokay_native::compress_with_dict(block, &mut dict)
            .map_err(|err| CompressError(Repr::Lzo(err)))?;
        let prefix = length_prefix(compressed.len())
            .ok_or(CompressError(Repr::BlockTooLarge(compressed.len())))?;
        out.extend_from_slice(&prefix);
        out.extend_from_slice(&compressed);
    }
    Ok(out)
}

/// `len` as three little-endian bytes, if it fits.
fn length_prefix(len: usize) -> Option<[u8; 3]> {
    match u32::try_from(len).ok()?.to_le_bytes() {
        [b0, b1, b2, 0] => Some([b0, b1, b2]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split compressed output into its blocks.
    fn blocks(mut compressed: &[u8]) -> Vec<&[u8]> {
        let mut blocks = Vec::new();
        while let [b0, b1, b2, rest @ ..] = compressed {
            let len = usize::from(*b0) | usize::from(*b1) << 8 | usize::from(*b2) << 16;
            assert!(len <= rest.len(), "block of {len} bytes truncated");
            let (block, tail) = rest.split_at(len);
            blocks.push(block);
            compressed = tail;
        }
        assert!(compressed.is_empty(), "trailing partial header");
        blocks
    }

    /// A deterministic raster-like test input: runs with some noise.
    fn sample(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| {
                if i % 97 < 60 {
                    0
                } else {
                    (i * 31 % 251).to_le_bytes()[0]
                }
            })
            .collect()
    }

    fn decompress(compressed: &[u8]) -> Vec<u8> {
        blocks(compressed)
            .into_iter()
            .flat_map(|block| {
                lzokay_native::decompress_all(block, Some(BLOCK_SIZE)).expect("valid LZO block")
            })
            .collect()
    }

    #[test]
    fn round_trips() {
        for len in [1, 200, BLOCK_SIZE - 1, BLOCK_SIZE, 2 * BLOCK_SIZE + 100] {
            let data = sample(len);
            let compressed = compress_blocks(&data).expect("compressible");
            assert_eq!(decompress(&compressed), data, "length {len}");
        }
    }

    #[test]
    fn one_block_per_4096_input_bytes() {
        let compressed = compress_blocks(&sample(2 * BLOCK_SIZE + 100)).expect("compressible");
        assert_eq!(blocks(&compressed).len(), 3);
    }

    #[test]
    fn blocks_are_compressed_independently() {
        // Reusing the dictionary must not make a block depend on the one
        // before it: each block equals a fresh compression of its input.
        let data = sample(3 * BLOCK_SIZE);
        let compressed = compress_blocks(&data).expect("compressible");
        for (block, input) in blocks(&compressed).into_iter().zip(data.chunks(BLOCK_SIZE)) {
            assert_eq!(block, lzokay_native::compress(input).expect("compressible"));
        }
    }

    #[test]
    fn empty_input_produces_no_blocks() {
        assert!(
            compress_blocks(&[])
                .expect("trivially compressible")
                .is_empty()
        );
    }

    #[test]
    fn length_prefix_is_three_little_endian_bytes() {
        assert_eq!(length_prefix(0x01_02_03), Some([0x03, 0x02, 0x01]));
        assert_eq!(length_prefix(0xff_ff_ff), Some([0xff, 0xff, 0xff]));
        assert_eq!(length_prefix(0x01_00_00_00), None);
    }
}
