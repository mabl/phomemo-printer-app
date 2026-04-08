//! Bitmap encoding for Phomemo printers.
//!
//! Converts raster data into the wire formats the printer expects.

/// Pack an 8-bit grayscale raster line into 1-bit monochrome.
///
/// Each input byte is a grayscale pixel (0=black, 255=white).
/// Output is MSB-first packed: bit 7 of byte 0 = first pixel.
/// A pixel is "black" (bit=1) if its value is below `threshold`.
///
/// `width_px` is the number of pixels.  The output length is
/// `ceil(width_px / 8)` bytes.
#[must_use]
pub fn pack_mono_line(pixels: &[u8], width_px: usize, threshold: u8) -> Vec<u8> {
    let width_bytes = width_px.div_ceil(8);
    let mut out = vec![0u8; width_bytes];

    for (i, &px) in pixels.iter().take(width_px).enumerate() {
        if px < threshold {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }

    out
}

/// Pack a pre-thresholded 1-bit-per-pixel raster line.
///
/// Input: CUPS `BLACK_1` format — each byte contains 8 pixels,
/// MSB = leftmost.  `1` = black, `0` = white.
///
/// For CUPS `BLACK_1`, the input is already in the right format,
/// so this is effectively a copy/truncation to `width_bytes`.
#[must_use]
pub fn pack_mono_line_1bpp(line: &[u8], width_bytes: usize) -> Vec<u8> {
    let mut out = vec![0u8; width_bytes];
    let copy_len = line.len().min(width_bytes);
    out[..copy_len].copy_from_slice(&line[..copy_len]);
    out
}

/// Packed-mono orientation transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrientationTransform {
    /// Keep source orientation as-is.
    None,
    /// Rotate 90 degrees counter-clockwise.
    Rotate90Ccw,
    /// Rotate 90 degrees clockwise.
    Rotate90Cw,
    /// Rotate 180 degrees.
    Rotate180,
}

fn bit_is_set(data: &[u8], stride: usize, x: usize, y: usize) -> bool {
    let idx = y * stride + x / 8;
    idx < data.len() && (data[idx] & (0x80 >> (x % 8))) != 0
}

fn set_bit(data: &mut [u8], stride: usize, x: usize, y: usize) {
    let idx = y * stride + x / 8;
    if idx < data.len() {
        data[idx] |= 0x80 >> (x % 8);
    }
}

/// Transform a packed 1-bpp image using the requested orientation.
///
/// Input/output are MSB-first packed rows. `width_px` is the active pixel width,
/// while each row occupies `ceil(width_px / 8)` bytes.
#[must_use]
pub fn transform_packed_mono(
    data: &[u8],
    width_px: usize,
    height: usize,
    transform: OrientationTransform,
) -> (Vec<u8>, usize, usize) {
    let src_stride = width_px.div_ceil(8);
    if width_px == 0 || height == 0 {
        return (Vec::new(), width_px, height);
    }

    match transform {
        OrientationTransform::None => (data.to_vec(), width_px, height),
        OrientationTransform::Rotate180 => {
            let mut out = vec![0u8; src_stride * height];
            for y in 0..height {
                for x in 0..width_px {
                    if bit_is_set(data, src_stride, x, y) {
                        let nx = width_px - 1 - x;
                        let ny = height - 1 - y;
                        set_bit(&mut out, src_stride, nx, ny);
                    }
                }
            }
            (out, width_px, height)
        }
        OrientationTransform::Rotate90Cw => {
            let out_width_px = height;
            let out_height = width_px;
            let out_stride = out_width_px.div_ceil(8);
            let mut out = vec![0u8; out_stride * out_height];

            for y in 0..height {
                for x in 0..width_px {
                    if bit_is_set(data, src_stride, x, y) {
                        let nx = height - 1 - y;
                        let ny = x;
                        set_bit(&mut out, out_stride, nx, ny);
                    }
                }
            }

            (out, out_width_px, out_height)
        }
        OrientationTransform::Rotate90Ccw => {
            let out_width_px = height;
            let out_height = width_px;
            let out_stride = out_width_px.div_ceil(8);
            let mut out = vec![0u8; out_stride * out_height];

            for y in 0..height {
                for x in 0..width_px {
                    if bit_is_set(data, src_stride, x, y) {
                        let nx = y;
                        let ny = width_px - 1 - x;
                        set_bit(&mut out, out_stride, nx, ny);
                    }
                }
            }

            (out, out_width_px, out_height)
        }
    }
}

/// Rotate a 1-bpp page buffer 180 degrees in place.
///
/// This compatibility helper assumes rows are byte-aligned (`width_bytes * 8`
/// active pixels).
pub fn rotate_180(data: &mut [u8], width_bytes: usize) {
    if width_bytes == 0 || data.is_empty() {
        return;
    }
    let height = data.len() / width_bytes;
    let (rotated, _, _) = transform_packed_mono(
        data,
        width_bytes * 8,
        height,
        OrientationTransform::Rotate180,
    );
    if rotated.len() == data.len() {
        data.copy_from_slice(&rotated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_mono_all_black() {
        let pixels = vec![0u8; 16]; // 16 black pixels
        let packed = pack_mono_line(&pixels, 16, 128);
        assert_eq!(packed, vec![0xff, 0xff]);
    }

    #[test]
    fn test_pack_mono_all_white() {
        let pixels = vec![255u8; 16];
        let packed = pack_mono_line(&pixels, 16, 128);
        assert_eq!(packed, vec![0x00, 0x00]);
    }

    #[test]
    fn test_pack_mono_alternating() {
        // B W B W B W B W = 10101010 = 0xAA
        let pixels = vec![0, 255, 0, 255, 0, 255, 0, 255];
        let packed = pack_mono_line(&pixels, 8, 128);
        assert_eq!(packed, vec![0xaa]);
    }

    #[test]
    fn test_pack_mono_partial_byte() {
        // 3 black pixels → first byte = 11100000 = 0xE0
        let pixels = vec![0, 0, 0];
        let packed = pack_mono_line(&pixels, 3, 128);
        assert_eq!(packed, vec![0xe0]);
    }

    #[test]
    fn test_pack_1bpp_passthrough() {
        let line = vec![0xaa, 0x55, 0xff];
        let packed = pack_mono_line_1bpp(&line, 3);
        assert_eq!(packed, line);
    }

    #[test]
    fn test_pack_1bpp_truncation() {
        let line = vec![0xaa, 0x55, 0xff, 0x00];
        let packed = pack_mono_line_1bpp(&line, 2);
        assert_eq!(packed, vec![0xaa, 0x55]);
    }

    #[test]
    fn test_rotate_180_single_row() {
        // Row: [10101010] = 0xAA → reversed bytes → bit-reversed = [01010101] = 0x55
        let mut data = vec![0xAA];
        rotate_180(&mut data, 1);
        assert_eq!(data, vec![0x55]);
    }

    #[test]
    fn test_rotate_180_two_rows() {
        // Row 0: [0xFF, 0x00], Row 1: [0x00, 0xFF]
        // After: Row 0 becomes reversed Row 1, Row 1 becomes reversed Row 0
        // Byte reversal: [0xFF, 0x00, 0x00, 0xFF] → reversed → [0xFF, 0x00, 0x00, 0xFF]
        // Then bit-reverse each: [0xFF, 0x00, 0x00, 0xFF]
        let mut data = vec![0xFF, 0x00, 0x00, 0xFF];
        rotate_180(&mut data, 2);
        assert_eq!(data, vec![0xFF, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn test_rotate_180_asymmetric() {
        // Row 0: [0x80] = 10000000, Row 1: [0x01] = 00000001
        // reverse bytes: [0x01, 0x80]
        // bit-reverse: [0x80, 0x01]
        let mut data = vec![0x80, 0x01];
        rotate_180(&mut data, 1);
        assert_eq!(data, vec![0x80, 0x01]);
    }

    #[test]
    fn test_rotate_180_empty() {
        let mut data: Vec<u8> = vec![];
        rotate_180(&mut data, 1);
        assert!(data.is_empty());
    }

    #[test]
    fn transform_rotate_90_cw_simple() {
        // 3x2 source:
        // 100
        // 010
        let src = vec![0x80, 0x40];
        let (out, out_w, out_h) =
            transform_packed_mono(&src, 3, 2, OrientationTransform::Rotate90Cw);

        // 2x3 expected:
        // 01
        // 10
        // 00
        assert_eq!(out_w, 2);
        assert_eq!(out_h, 3);
        assert_eq!(out, vec![0x40, 0x80, 0x00]);
    }

    #[test]
    fn transform_rotate_90_ccw_simple() {
        // 3x2 source:
        // 100
        // 010
        let src = vec![0x80, 0x40];
        let (out, out_w, out_h) =
            transform_packed_mono(&src, 3, 2, OrientationTransform::Rotate90Ccw);

        // 2x3 expected:
        // 00
        // 01
        // 10
        assert_eq!(out_w, 2);
        assert_eq!(out_h, 3);
        assert_eq!(out, vec![0x00, 0x40, 0x80]);
    }

    #[test]
    fn transform_rotate_90_ccw_non_symmetric_pattern() {
        // 3x2 source:
        // 100
        // 001
        let src = vec![0x80, 0x20];
        let (out, out_w, out_h) =
            transform_packed_mono(&src, 3, 2, OrientationTransform::Rotate90Ccw);

        // 2x3 expected:
        // 01
        // 00
        // 10
        assert_eq!(out_w, 2);
        assert_eq!(out_h, 3);
        assert_eq!(out, vec![0x40, 0x00, 0x80]);
    }

    #[test]
    fn transform_rotate_180_non_byte_aligned_width() {
        // 3x2 source:
        // 100
        // 001
        let src = vec![0x80, 0x20];
        let (out, out_w, out_h) =
            transform_packed_mono(&src, 3, 2, OrientationTransform::Rotate180);

        // 3x2 expected:
        // 100
        // 001
        // (this specific pattern is 180-symmetric)
        assert_eq!(out_w, 3);
        assert_eq!(out_h, 2);
        assert_eq!(out, src);
    }
}
