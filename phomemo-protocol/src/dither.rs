//! Grayscale-to-monochrome conversion.
//!
//! [`dither`] turns a [`GrayImage`] (luminance, 0 = black) into a
//! [`MonoBitmap`] (set bit = black dot). Every algorithm maps pure black (0)
//! to a black dot and pure white (255) to no dot, so solid areas and blank
//! paper come out exact whatever the algorithm.
//!
//! The algorithms come in two kinds:
//!
//! - **Ordered**: each pixel is compared against a threshold that depends
//!   only on its position. [`Algorithm::Threshold`] uses one threshold
//!   everywhere, [`Algorithm::Bayer8`] tiles an 8x8 matrix of them.
//! - **Error diffusion**: each pixel is rounded to black or white and the
//!   rounding error is pushed onto pixels not yet visited, weighted by a
//!   kernel. [`Algorithm::FloydSteinberg`] passes the whole error on,
//!   [`Algorithm::Atkinson`] only three quarters of it.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use crate::bitmap::{GrayImage, MonoBitmap};

/// A dithering algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    /// One threshold at mid-gray. Best for barcodes, QR codes and line art.
    Threshold,
    /// 8x8 Bayer ordered dither. Fast and deterministic; good for graphics.
    Bayer8,
    /// Floyd-Steinberg error diffusion. Best general-purpose choice for
    /// photos.
    FloydSteinberg,
    /// Atkinson error diffusion. Higher contrast: highlights and shadows
    /// lose texture, which suits thermal media.
    Atkinson,
}

impl Algorithm {
    /// Every algorithm.
    pub const ALL: [Self; 4] = [
        Self::Threshold,
        Self::Bayer8,
        Self::FloydSteinberg,
        Self::Atkinson,
    ];

    /// The name [`FromStr`] accepts and [`Display`](fmt::Display) prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Bayer8 => "bayer8",
            Self::FloydSteinberg => "floyd-steinberg",
            Self::Atkinson => "atkinson",
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A string that names no [`Algorithm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseAlgorithmError;

impl fmt::Display for ParseAlgorithmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown dithering algorithm")
    }
}

impl Error for ParseAlgorithmError {}

impl FromStr for Algorithm {
    type Err = ParseAlgorithmError;

    /// Parse an algorithm [`name`](Self::name), ignoring ASCII case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|algorithm| algorithm.name().eq_ignore_ascii_case(s))
            .ok_or(ParseAlgorithmError)
    }
}

/// Convert `image` to a bitmap of the same size with `algorithm`.
#[must_use]
pub fn dither(image: &GrayImage, algorithm: Algorithm) -> MonoBitmap {
    match algorithm {
        Algorithm::Threshold => ordered(image, &THRESHOLD),
        Algorithm::Bayer8 => ordered(image, &BAYER8),
        Algorithm::FloydSteinberg => diffuse(image, &FLOYD_STEINBERG),
        Algorithm::Atkinson => diffuse(image, &ATKINSON),
    }
}

/// A bitmap the size of `image` with the pixels `is_black` selects, which
/// is called once per pixel in raster order.
fn bitmap_where(image: &GrayImage, mut is_black: impl FnMut(usize, usize) -> bool) -> MonoBitmap {
    let mut bitmap = MonoBitmap::white(image.width(), image.height());
    for y in 0..image.height() {
        for x in 0..image.width() {
            if is_black(x, y) {
                bitmap.set_pixel(x, y, true);
            }
        }
    }
    bitmap
}

// ---------------------------------------------------------------------------
// Ordered dithering
// ---------------------------------------------------------------------------

/// A pixel is black iff it is darker than the threshold at its position.
///
/// No threshold is 0, so 0 is always black; and no `u8` exceeds 255, so 255
/// is always white.
fn ordered<const N: usize>(image: &GrayImage, thresholds: &[[u8; N]; N]) -> MonoBitmap {
    let width = image.width();
    let pixels = image.data();
    bitmap_where(image, |x, y| {
        pixels[y * width + x] < thresholds[y % N][x % N]
    })
}

/// [`Algorithm::Threshold`]: black below mid-gray, as the vendor apps do
/// (Print Master `XNvUtil.img2Nv`, `R < 128`; `re/protocol/bitmap-encoding.md`).
const THRESHOLD: [[u8; 1]; 1] = [[128]];

/// The 8x8 Bayer index matrix used by the vendor CUPS filter
/// (`re/protocol/bitmap-encoding.md`, "Bayer Dithering").
#[rustfmt::skip]
const BAYER8_INDEX: [[u8; 8]; 8] = [
    [ 0, 32,  8, 40,  2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44,  4, 36, 14, 46,  6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [ 3, 35, 11, 43,  1, 33,  9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47,  7, 39, 13, 45,  5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// [`Algorithm::Bayer8`] thresholds: index `i` becomes `4i + 1`.
///
/// The vendor filter prints white iff `4i < pixel`, i.e. black iff
/// `pixel < 4i + 1`. The thresholds span `1..=253`, so 0 is always black
/// and 255 always white, and a uniform gray `g` blackens `(256 - g) / 4` of
/// every 64 pixels.
const BAYER8: [[u8; 8]; 8] = {
    let mut thresholds = [[0; 8]; 8];
    let mut y = 0;
    while y < 8 {
        let mut x = 0;
        while x < 8 {
            thresholds[y][x] = BAYER8_INDEX[y][x] * 4 + 1;
            x += 1;
        }
        y += 1;
    }
    thresholds
};

// ---------------------------------------------------------------------------
// Error diffusion
// ---------------------------------------------------------------------------

/// An error-diffusion kernel: the neighbours of the current pixel that
/// receive `weight / divisor` of its rounding error.
struct Kernel {
    divisor: i16,
    /// `(dx, dy, weight)`: column offset, row offset, share of the error.
    taps: &'static [(isize, usize, i16)],
}

/// ```text
///         X   7
///     3   5   1     (/16)
/// ```
const FLOYD_STEINBERG: Kernel = Kernel {
    divisor: 16,
    taps: &[(1, 0, 7), (-1, 1, 3), (0, 1, 5), (1, 1, 1)],
};

/// ```text
///         X   1   1
///     1   1   1
///         1         (/8)
/// ```
const ATKINSON: Kernel = Kernel {
    divisor: 8,
    taps: &[
        (1, 0, 1),
        (2, 0, 1),
        (-1, 1, 1),
        (0, 1, 1),
        (1, 1, 1),
        (0, 2, 1),
    ],
};

/// Diffuse rounding error across the image in raster order.
///
/// A pixel rounds to black iff its value plus the error it received is
/// below 128, so the error it passes on is within `-127..=127`. Each kernel
/// distributes at most the whole error, so a pixel receives less than 128
/// in total: 0 stays black and 255 stays white.
fn diffuse(image: &GrayImage, kernel: &Kernel) -> MonoBitmap {
    let (width, height) = (image.width(), image.height());
    // Pixel values plus received error: -127..=382, well within i16.
    let mut values: Vec<i16> = image.data().iter().map(|&p| i16::from(p)).collect();

    bitmap_where(image, |x, y| {
        let value = values[y * width + x].clamp(0, 255);
        let black = value < 128;
        let error = if black { value } else { value - 255 };
        for &(dx, dy, weight) in kernel.taps {
            let Some(tx) = x.checked_add_signed(dx).filter(|&tx| tx < width) else {
                continue;
            };
            let ty = y + dy;
            if ty < height {
                values[ty * width + tx] += error * weight / kernel.divisor;
            }
        }
        black
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uniform(width: usize, height: usize, value: u8) -> GrayImage {
        GrayImage::new(width, height, vec![value; width * height]).expect("valid dimensions")
    }

    /// Deterministic pseudo-random noise (LCG), so tests are reproducible.
    fn noise(width: usize, height: usize, seed: u32) -> GrayImage {
        let mut state = seed;
        let data = (0..width * height)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state.to_be_bytes()[0]
            })
            .collect();
        GrayImage::new(width, height, data).expect("valid dimensions")
    }

    fn black_dots(bitmap: &MonoBitmap) -> u32 {
        bitmap.data().iter().map(|b| b.count_ones()).sum()
    }

    #[test]
    fn solid_black_and_white_are_exact_for_every_algorithm() {
        for algorithm in Algorithm::ALL {
            let black = dither(&uniform(19, 11, 0), algorithm);
            assert_eq!(black_dots(&black), 19 * 11, "{algorithm}: holes in black");
            let white = dither(&uniform(19, 11, 255), algorithm);
            assert_eq!(black_dots(&white), 0, "{algorithm}: dots in white");
        }
    }

    #[test]
    fn extremes_are_exact_inside_any_image() {
        // Embed pure black and pure white pixels in noise: whatever error
        // their neighbours diffuse onto them, they must not flip.
        for algorithm in Algorithm::ALL {
            let source = noise(64, 48, 7);
            let mut data = source.data().to_vec();
            for (i, pixel) in data.iter_mut().enumerate() {
                match i % 5 {
                    0 => *pixel = 0,
                    1 => *pixel = 255,
                    _ => {}
                }
            }
            let image = GrayImage::new(64, 48, data).expect("valid dimensions");
            let bitmap = dither(&image, algorithm);
            for (i, &pixel) in image.data().iter().enumerate() {
                let (x, y) = (i % 64, i / 64);
                match pixel {
                    0 => assert!(
                        bitmap.pixel(x, y),
                        "{algorithm}: black flipped at ({x}, {y})"
                    ),
                    255 => assert!(
                        !bitmap.pixel(x, y),
                        "{algorithm}: white flipped at ({x}, {y})"
                    ),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn output_matches_input_dimensions() {
        for algorithm in Algorithm::ALL {
            let bitmap = dither(&noise(21, 5, 1), algorithm);
            assert_eq!((bitmap.width_px(), bitmap.height()), (21, 5), "{algorithm}");
            assert_eq!(bitmap.data().len(), 3 * 5, "{algorithm}");
        }
    }

    #[test]
    fn empty_images() {
        for algorithm in Algorithm::ALL {
            let bitmap = dither(&uniform(0, 3, 0), algorithm);
            assert_eq!((bitmap.width_px(), bitmap.height()), (0, 3));
        }
    }

    #[test]
    fn threshold_splits_at_mid_gray() {
        let image = GrayImage::new(4, 1, vec![0, 127, 128, 255]).expect("valid dimensions");
        let bitmap = dither(&image, Algorithm::Threshold);
        assert_eq!(bitmap.data(), [0b1100_0000]);
    }

    #[test]
    fn bayer_thresholds_cover_every_level_once() {
        let mut thresholds: Vec<u8> = BAYER8.iter().flatten().copied().collect();
        thresholds.sort_unstable();
        let expected: Vec<u8> = (0..64).map(|i| i * 4 + 1).collect();
        assert_eq!(thresholds, expected);
    }

    #[test]
    fn bayer_renders_uniform_gray_in_proportion() {
        // Black iff g < 4i + 1, i.e. i > (g - 1) / 4: (256 - g) / 4 of 64.
        for (gray, expected) in [(1, 63), (64, 48), (128, 32), (192, 16), (252, 1), (253, 0)] {
            let bitmap = dither(&uniform(8, 8, gray), Algorithm::Bayer8);
            assert_eq!(black_dots(&bitmap), expected, "gray {gray}");
        }
    }

    #[test]
    fn floyd_steinberg_preserves_mean_tone() {
        // Full error diffusion keeps the average darkness: a 75 % white
        // field comes out about 25 % black.
        let bitmap = dither(&uniform(64, 64, 191), Algorithm::FloydSteinberg);
        let black = black_dots(&bitmap);
        assert!(
            black.abs_diff(64 * 64 / 4) < 64 * 64 / 50,
            "{black} black dots"
        );
    }

    #[test]
    fn atkinson_washes_out_highlights_and_fills_shadows() {
        // Atkinson drops a quarter of the error, so light grays render
        // lighter and dark grays darker than Floyd-Steinberg's faithful
        // reproduction.
        let count = |gray, algorithm| black_dots(&dither(&uniform(64, 64, gray), algorithm));

        let (fs, atkinson) = (
            count(224, Algorithm::FloydSteinberg),
            count(224, Algorithm::Atkinson),
        );
        assert!(
            atkinson * 2 < fs,
            "highlights: atkinson {atkinson}, fs {fs}"
        );

        let (fs, atkinson) = (
            count(32, Algorithm::FloydSteinberg),
            count(32, Algorithm::Atkinson),
        );
        assert!(
            atkinson > fs + fs / 20,
            "shadows: atkinson {atkinson}, fs {fs}"
        );
    }

    #[test]
    fn names_round_trip() {
        for algorithm in Algorithm::ALL {
            assert_eq!(algorithm.to_string().parse(), Ok(algorithm));
        }
        assert_eq!("Floyd-Steinberg".parse(), Ok(Algorithm::FloydSteinberg));
        assert_eq!("BAYER8".parse(), Ok(Algorithm::Bayer8));
    }

    #[test]
    fn unknown_names_are_rejected() {
        for name in ["", "auto", "fs", "floydsteinberg", "bayer"] {
            assert_eq!(
                name.parse::<Algorithm>(),
                Err(ParseAlgorithmError),
                "{name:?}"
            );
        }
    }
}
