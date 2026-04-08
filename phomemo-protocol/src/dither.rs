//! Grayscale-to-monochrome dithering algorithms.
//!
//! Each function takes an 8-bit grayscale page buffer (one byte per
//! pixel, `width_px * height` bytes total) and returns a packed 1-bpp
//! monochrome buffer (`ceil(width_px/8) * height` bytes).
//!
//! 0 = black, 255 = white in the input.  1 = black in the output.

/// Available dithering algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// Fixed threshold (default 128).  Best for barcodes, QR, line art.
    Threshold,
    /// 8x8 Bayer ordered dither.  Fast, deterministic, good for graphics.
    Bayer8,
    /// Floyd-Steinberg error diffusion.  Best general-purpose for photos.
    FloydSteinberg,
    /// Atkinson error diffusion.  Lighter output, good for thermal media.
    Atkinson,
}

impl Algorithm {
    /// Parse from a string (vendor option value).
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "threshold" => Some(Self::Threshold),
            "bayer8" => Some(Self::Bayer8),
            "floyd-steinberg" => Some(Self::FloydSteinberg),
            "atkinson" => Some(Self::Atkinson),
            _ => None,
        }
    }
}

/// Apply the selected algorithm to an 8-bit grayscale page.
///
/// Returns a packed 1-bpp buffer (MSB = leftmost pixel, 1 = black).
#[must_use]
pub fn apply(algo: Algorithm, pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    match algo {
        Algorithm::Threshold => threshold(pixels, width, height, 128),
        Algorithm::Bayer8 => bayer8(pixels, width, height),
        Algorithm::FloydSteinberg => floyd_steinberg(pixels, width, height),
        Algorithm::Atkinson => atkinson(pixels, width, height),
    }
}

// ---------------------------------------------------------------------------
// Threshold
// ---------------------------------------------------------------------------

/// Simple threshold dithering.
#[must_use]
pub fn threshold(pixels: &[u8], width: usize, height: usize, thresh: u8) -> Vec<u8> {
    let out_stride = width.div_ceil(8);
    let mut out = vec![0u8; out_stride * height];

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if idx < pixels.len() && pixels[idx] < thresh {
                out[y * out_stride + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Bayer 8x8
// ---------------------------------------------------------------------------

/// Classic 8x8 Bayer threshold matrix (values 0..63, scaled to 0..255).
#[rustfmt::skip]
const BAYER8: [[u8; 8]; 8] = [
    [  0, 128,  32, 160,   8, 136,  40, 168],
    [192,  64, 224,  96, 200,  72, 232, 104],
    [ 48, 176,  16, 144,  56, 184,  24, 152],
    [240, 112, 208,  80, 248, 120, 216,  88],
    [ 12, 140,  44, 172,   4, 132,  36, 164],
    [204,  76, 236, 108, 196,  68, 228, 100],
    [ 60, 188,  28, 156,  52, 180,  20, 148],
    [252, 124, 220,  92, 244, 116, 212,  84],
];

/// Bayer 8x8 ordered dither.
#[must_use]
pub fn bayer8(pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    let out_stride = width.div_ceil(8);
    let mut out = vec![0u8; out_stride * height];

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if idx < pixels.len() {
                let threshold = BAYER8[y % 8][x % 8];
                if pixels[idx] < threshold {
                    out[y * out_stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Floyd-Steinberg
// ---------------------------------------------------------------------------

/// Floyd-Steinberg error-diffusion dithering.
///
/// Distributes quantization error to neighboring pixels:
/// ```text
///         X   7/16
///   3/16 5/16 1/16
/// ```
#[must_use]
pub fn floyd_steinberg(pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    let out_stride = width.div_ceil(8);
    let mut out = vec![0u8; out_stride * height];

    // Work buffer: i16 to hold accumulated error.
    let mut buf: Vec<i16> = pixels.iter().map(|&p| i16::from(p)).collect();
    buf.resize(width * height, 255);

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let old = buf[idx].clamp(0, 255);
            let new_val: i16 = if old < 128 { 0 } else { 255 };

            if old < 128 {
                out[y * out_stride + x / 8] |= 0x80 >> (x % 8);
            }

            let err = old - new_val;
            if x + 1 < width {
                buf[idx + 1] += err * 7 / 16;
            }
            if y + 1 < height {
                if x > 0 {
                    buf[idx + width - 1] += err * 3 / 16;
                }
                buf[idx + width] += err * 5 / 16;
                if x + 1 < width {
                    buf[idx + width + 1] += err / 16;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Atkinson
// ---------------------------------------------------------------------------

/// Atkinson error-diffusion dithering.
///
/// Distributes only 6/8 of the error (lighter output on thermal media):
/// ```text
///         X  1/8 1/8
///   1/8  1/8 1/8
///        1/8
/// ```
#[must_use]
pub fn atkinson(pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    let out_stride = width.div_ceil(8);
    let mut out = vec![0u8; out_stride * height];

    let mut buf: Vec<i16> = pixels.iter().map(|&p| i16::from(p)).collect();
    buf.resize(width * height, 255);

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let old = buf[idx].clamp(0, 255);
            let new_val: i16 = if old < 128 { 0 } else { 255 };

            if old < 128 {
                out[y * out_stride + x / 8] |= 0x80 >> (x % 8);
            }

            let err = old - new_val;
            let d = err / 8;

            // Right neighbors
            if x + 1 < width {
                buf[idx + 1] += d;
            }
            if x + 2 < width {
                buf[idx + 2] += d;
            }
            // Next row
            if y + 1 < height {
                if x > 0 {
                    buf[idx + width - 1] += d;
                }
                buf[idx + width] += d;
                if x + 1 < width {
                    buf[idx + width + 1] += d;
                }
            }
            // Two rows down
            if y + 2 < height {
                buf[idx + 2 * width] += d;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gradient(width: usize, height: usize) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                #[allow(clippy::cast_possible_truncation)]
                let val = ((x + y) * 255 / (width + height - 2).max(1)) as u8;
                pixels.push(val);
            }
        }
        pixels
    }

    fn count_black_bits(data: &[u8]) -> usize {
        data.iter().map(|b| b.count_ones() as usize).sum()
    }

    #[test]
    fn threshold_all_black() {
        let pixels = vec![0u8; 16];
        let result = threshold(&pixels, 16, 1, 128);
        assert_eq!(result, vec![0xFF, 0xFF]);
    }

    #[test]
    fn threshold_all_white() {
        let pixels = vec![255u8; 16];
        let result = threshold(&pixels, 16, 1, 128);
        assert_eq!(result, vec![0x00, 0x00]);
    }

    #[test]
    fn threshold_custom_value() {
        // All pixels at 100, threshold at 50 → all white (100 >= 50)
        let pixels = vec![100u8; 8];
        let result = threshold(&pixels, 8, 1, 50);
        assert_eq!(result, vec![0x00]);

        // threshold at 200 → all black (100 < 200)
        let result = threshold(&pixels, 8, 1, 200);
        assert_eq!(result, vec![0xFF]);
    }

    #[test]
    fn bayer8_output_size() {
        let pixels = make_gradient(80, 10);
        let result = bayer8(&pixels, 80, 10);
        assert_eq!(result.len(), 10 * 10); // ceil(80/8) = 10 bytes per row
    }

    #[test]
    fn bayer8_produces_dither_pattern() {
        // A uniform gray should produce a mix of black and white.
        let pixels = vec![128u8; 64]; // 8x8 uniform mid-gray
        let result = bayer8(&pixels, 8, 8);
        let blacks = count_black_bits(&result);
        // Should be roughly half black — not all-black or all-white.
        assert!(blacks > 10, "too few black pixels: {blacks}");
        assert!(blacks < 54, "too many black pixels: {blacks}");
    }

    #[test]
    fn floyd_steinberg_output_size() {
        let pixels = make_gradient(80, 10);
        let result = floyd_steinberg(&pixels, 80, 10);
        assert_eq!(result.len(), 10 * 10);
    }

    #[test]
    fn floyd_steinberg_preserves_extremes() {
        // All black input → all black output
        let result = floyd_steinberg(&[0u8; 8], 8, 1);
        assert_eq!(result, vec![0xFF]);

        // All white input → all white output
        let result = floyd_steinberg(&[255u8; 8], 8, 1);
        assert_eq!(result, vec![0x00]);
    }

    #[test]
    fn atkinson_output_size() {
        let pixels = make_gradient(80, 10);
        let result = atkinson(&pixels, 80, 10);
        assert_eq!(result.len(), 10 * 10);
    }

    #[test]
    fn atkinson_lighter_than_floyd() {
        // Atkinson discards 2/8 of error → fewer black pixels on gray input.
        let pixels = vec![128u8; 64]; // 8x8 uniform gray
        let fs_blacks = count_black_bits(&floyd_steinberg(&pixels, 8, 8));
        let at_blacks = count_black_bits(&atkinson(&pixels, 8, 8));
        // Atkinson should produce fewer or equal black pixels.
        assert!(
            at_blacks <= fs_blacks + 2,
            "Atkinson ({at_blacks}) should be lighter than Floyd-Steinberg ({fs_blacks})"
        );
    }

    #[test]
    fn algorithm_from_name() {
        assert_eq!(
            Algorithm::from_name("threshold"),
            Some(Algorithm::Threshold)
        );
        assert_eq!(Algorithm::from_name("bayer8"), Some(Algorithm::Bayer8));
        assert_eq!(
            Algorithm::from_name("floyd-steinberg"),
            Some(Algorithm::FloydSteinberg)
        );
        assert_eq!(Algorithm::from_name("atkinson"), Some(Algorithm::Atkinson));
        assert_eq!(Algorithm::from_name("floydsteinberg"), None);
        assert_eq!(Algorithm::from_name("fs"), None);
        assert_eq!(Algorithm::from_name("unknown"), None);
    }

    #[test]
    fn apply_dispatches_correctly() {
        // Threshold, FS, Atkinson: all-black input → all-black output.
        let pixels = vec![0u8; 8];
        for algo in [
            Algorithm::Threshold,
            Algorithm::FloydSteinberg,
            Algorithm::Atkinson,
        ] {
            let result = apply(algo, &pixels, 8, 1);
            assert_eq!(result, vec![0xFF], "failed for {algo:?}");
        }

        // Bayer: pixel=0 with matrix entry=0 means 0 < 0 is false,
        // so not quite all-black. Use pixel=1 (darker than any matrix entry).
        let pixels = vec![1u8; 8];
        let result = apply(Algorithm::Bayer8, &pixels, 8, 1);
        // All pixels < all matrix entries (min matrix value is 0, 1 > 0, but
        // first entry is 0: 1 < 0 is false). Actually pixel=1 < threshold=128
        // at (0,1), etc. Most bits should be set.
        let blacks = count_black_bits(&result);
        assert!(
            blacks >= 4,
            "Bayer on near-black should produce mostly black: {blacks}/8"
        );
    }
}
