//! miniLZO compression for Phomemo raster data.
//!
//! The printer accepts LZO1X-1 compressed pixel data in 4096-byte
//! blocks, each prefixed with a 3-byte little-endian length header.
//! The bitmap command header (8 bytes for mono) is sent uncompressed
//! before the compressed blocks.
//!
//! Wire format details are documented in the project protocol notes.

/// Input block size for LZO compression (matches reference app `allocSize`).
const BLOCK_SIZE: usize = 4096;

/// Compress raw pixel data into the Phomemo block format.
///
/// Each `BLOCK_SIZE` chunk of `data` is independently LZO1X-1
/// compressed, then prefixed with a 3-byte little-endian length:
///
/// ```text
/// [len_lo] [len_mid] [len_hi] [compressed_data...]
/// ```
///
/// Returns the concatenated compressed blocks.
///
/// # Errors
///
/// Returns `Err` if LZO compression fails on any block.
pub fn compress_blocks(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len());

    for chunk in data.chunks(BLOCK_SIZE) {
        let compressed =
            lzokay_native::compress(chunk).map_err(|e| format!("LZO compress failed: {e:?}"))?;

        let len = compressed.len();
        // 3-byte little-endian length header
        #[allow(clippy::cast_possible_truncation)]
        {
            out.push(len as u8);
            out.push((len >> 8) as u8);
            out.push((len >> 16) as u8);
        }
        out.extend_from_slice(&compressed);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_small_block() {
        // A single block smaller than 4096 bytes.
        let data = vec![0u8; 200];
        let result = compress_blocks(&data).unwrap();

        // Must start with a 3-byte length header.
        assert!(result.len() >= 3);
        let block_len =
            usize::from(result[0]) | (usize::from(result[1]) << 8) | (usize::from(result[2]) << 16);
        assert_eq!(result.len(), 3 + block_len);
    }

    #[test]
    fn compress_exact_block() {
        let data = vec![0xAA; BLOCK_SIZE];
        let result = compress_blocks(&data).unwrap();
        assert!(result.len() >= 3);
    }

    #[test]
    fn compress_multi_block() {
        // Two full blocks + a partial one.
        let data = vec![0x55; BLOCK_SIZE * 2 + 100];
        let result = compress_blocks(&data).unwrap();

        // Parse 3 block headers.
        let mut pos = 0;
        for _ in 0..3 {
            assert!(pos + 3 <= result.len(), "truncated at block header");
            let blen = usize::from(result[pos])
                | (usize::from(result[pos + 1]) << 8)
                | (usize::from(result[pos + 2]) << 16);
            pos += 3 + blen;
        }
        assert_eq!(pos, result.len());
    }

    #[test]
    fn compress_empty() {
        let result = compress_blocks(&[]).unwrap();
        assert!(result.is_empty());
    }
}
