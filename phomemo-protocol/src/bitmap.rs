//! Raster images: packed 1-bit [`MonoBitmap`]s, as the printer receives
//! them, and 8-bit [`GrayImage`]s, as [`dither`](crate::dither) consumes them.

use std::error::Error;
use std::fmt;
use std::slice::ChunksExact;

/// Pixel data whose length does not match the image dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimensionError {
    /// Bytes the dimensions call for, or `None` if that overflows `usize`.
    pub expected: Option<usize>,
    /// Bytes supplied.
    pub actual: usize,
}

impl fmt::Display for DimensionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.expected {
            Some(expected) => write!(
                f,
                "image data is {} bytes but its dimensions need {expected}",
                self.actual
            ),
            None => f.write_str("image dimensions overflow the address space"),
        }
    }
}

impl Error for DimensionError {}

/// Check that `data_len` bytes are exactly `stride * height`.
const fn check_len(stride: usize, height: usize, data_len: usize) -> Result<(), DimensionError> {
    match stride.checked_mul(height) {
        Some(expected) if expected == data_len => Ok(()),
        expected => Err(DimensionError {
            expected,
            actual: data_len,
        }),
    }
}

/// A quarter-turn rotation applied to a [`MonoBitmap`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// Leave the image as it is.
    #[default]
    Identity,
    /// Rotate a quarter turn clockwise.
    Clockwise,
    /// Rotate half a turn.
    HalfTurn,
    /// Rotate a quarter turn counter-clockwise.
    CounterClockwise,
}

impl Rotation {
    /// Dimensions of a `width` x `height` image after rotation.
    const fn dimensions(self, width: usize, height: usize) -> (usize, usize) {
        match self {
            Self::Identity | Self::HalfTurn => (width, height),
            Self::Clockwise | Self::CounterClockwise => (height, width),
        }
    }

    /// Where pixel `(x, y)` of a `width` x `height` image lands.
    const fn destination(self, x: usize, y: usize, width: usize, height: usize) -> (usize, usize) {
        match self {
            Self::Identity => (x, y),
            Self::Clockwise => (height - 1 - y, x),
            Self::HalfTurn => (width - 1 - x, height - 1 - y),
            Self::CounterClockwise => (y, width - 1 - x),
        }
    }
}

/// A 1-bit-per-pixel image in the printer's raster format.
///
/// Rows are packed MSB first (bit 7 of a row's first byte is its leftmost
/// pixel), a set bit is a black dot, and each row occupies
/// [`stride`](Self::stride) `= ceil(width_px / 8)` bytes with no padding
/// between rows. The bits past `width_px` in a row's last byte are always
/// zero: the printer prints whole bytes, so they would otherwise come out as
/// dots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoBitmap {
    width_px: usize,
    height: usize,
    data: Vec<u8>,
}

impl MonoBitmap {
    /// Wrap packed rows of `ceil(width_px / 8)` bytes each.
    ///
    /// Bits past `width_px` in each row's last byte are not pixels and are
    /// cleared.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionError`] unless `data` holds exactly `height` rows.
    pub fn new(width_px: usize, height: usize, data: Vec<u8>) -> Result<Self, DimensionError> {
        check_len(width_px.div_ceil(8), height, data.len())?;
        let mut bitmap = Self {
            width_px,
            height,
            data,
        };
        bitmap.clear_padding();
        Ok(bitmap)
    }

    /// An all-white bitmap.
    ///
    /// Only called with the dimensions of an existing image, whose size
    /// bounds the allocation.
    pub(crate) fn white(width_px: usize, height: usize) -> Self {
        Self {
            width_px,
            height,
            data: vec![0; width_px.div_ceil(8) * height],
        }
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width_px(&self) -> usize {
        self.width_px
    }

    /// Height in rows.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Bytes per row: `ceil(width_px / 8)`.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.width_px.div_ceil(8)
    }

    /// The packed rows, `stride() * height()` bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Iterate over the packed rows, top to bottom.
    ///
    /// A zero-width bitmap holds no bytes, so it yields no rows at all.
    pub fn rows(&self) -> ChunksExact<'_, u8> {
        self.data.chunks_exact(self.stride().max(1))
    }

    /// Byte index and bit mask of pixel `(x, y)`.
    fn locate(&self, x: usize, y: usize) -> (usize, u8) {
        assert!(
            x < self.width_px && y < self.height,
            "pixel ({x}, {y}) outside a {}x{} bitmap",
            self.width_px,
            self.height
        );
        (y * self.stride() + x / 8, 0x80 >> (x % 8))
    }

    /// Whether pixel `(x, y)` is black.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` lies outside the bitmap.
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        let (index, mask) = self.locate(x, y);
        self.data[index] & mask != 0
    }

    /// Make pixel `(x, y)` black or white.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` lies outside the bitmap.
    pub fn set_pixel(&mut self, x: usize, y: usize, black: bool) {
        let (index, mask) = self.locate(x, y);
        if black {
            self.data[index] |= mask;
        } else {
            self.data[index] &= !mask;
        }
    }

    /// Mask of the bits in a row's last byte that hold pixels.
    const fn last_byte_mask(width_px: usize) -> u8 {
        match width_px % 8 {
            0 => 0xff,
            used => !(0xff >> used),
        }
    }

    fn clear_padding(&mut self) {
        let stride = self.stride();
        let mask = Self::last_byte_mask(self.width_px);
        if stride == 0 || mask == 0xff {
            return;
        }
        for row in self.data.chunks_exact_mut(stride) {
            row[stride - 1] &= mask;
        }
    }

    /// Rotate the image.
    #[must_use]
    pub fn rotate(self, rotation: Rotation) -> Self {
        let (width, height) = (self.width_px, self.height);
        match rotation {
            Rotation::Identity => return self,
            // Byte-aligned rows turn half way by reversing the whole buffer
            // and the bit order within each byte.
            Rotation::HalfTurn if width % 8 == 0 => {
                let mut data = self.data;
                data.reverse();
                for byte in &mut data {
                    *byte = byte.reverse_bits();
                }
                return Self { data, ..self };
            }
            _ => {}
        }

        let (rotated_width, rotated_height) = rotation.dimensions(width, height);
        let mut rotated = Self::white(rotated_width, rotated_height);
        for y in 0..height {
            for x in 0..width {
                if self.pixel(x, y) {
                    let (rx, ry) = rotation.destination(x, y, width, height);
                    rotated.set_pixel(rx, ry, true);
                }
            }
        }
        rotated
    }

    /// Crop the image to at most `max_width_px` columns, keeping its left
    /// edge. An image already that narrow is returned unchanged.
    ///
    /// To fit an image to the print head, [`rotate`](Self::rotate) it into
    /// the head's orientation first - labels for the D30 class are laid out
    /// along the feed, wider than the head - then clip to the head width.
    #[must_use]
    pub fn clip_width(self, max_width_px: usize) -> Self {
        if self.width_px <= max_width_px {
            return self;
        }
        let stride = max_width_px.div_ceil(8);
        let mut data = Vec::with_capacity(stride * self.height);
        for row in self.rows() {
            data.extend_from_slice(&row[..stride]);
        }
        let mut clipped = Self {
            width_px: max_width_px,
            height: self.height,
            data,
        };
        clipped.clear_padding();
        clipped
    }
}

/// An 8-bit grayscale image: one byte of luminance per pixel, row-major,
/// where 0 is black and 255 is white.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl GrayImage {
    /// Wrap `width * height` luminance bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionError`] unless `data` holds exactly `height` rows
    /// of `width` pixels.
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Result<Self, DimensionError> {
        check_len(width, height, data.len())?;
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Height in rows.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// The pixels, row-major.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Swap black and white, turning ink coverage (0 = no ink, as in a
    /// CUPS/PWG `black` raster) into luminance or back.
    pub fn invert(&mut self) {
        for pixel in &mut self.data {
            *pixel = !*pixel;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bitmap from rows of `#` (black) and `.` (white).
    fn bitmap(rows: &[&str]) -> MonoBitmap {
        let width = rows.first().map_or(0, |row| row.len());
        let mut bitmap = MonoBitmap::white(width, rows.len());
        for (y, row) in rows.iter().enumerate() {
            assert_eq!(row.len(), width, "ragged test bitmap");
            for (x, cell) in row.bytes().enumerate() {
                bitmap.set_pixel(x, y, cell == b'#');
            }
        }
        bitmap
    }

    /// A deterministic, asymmetric test pattern.
    fn pattern(width: usize, height: usize) -> MonoBitmap {
        let mut bitmap = MonoBitmap::white(width, height);
        for y in 0..height {
            for x in 0..width {
                bitmap.set_pixel(x, y, (x * 7 + y * 3 + x * y) % 5 < 2);
            }
        }
        bitmap
    }

    #[test]
    fn new_validates_length() {
        assert_eq!(
            MonoBitmap::new(9, 2, vec![0; 3]),
            Err(DimensionError {
                expected: Some(4),
                actual: 3
            })
        );
        assert_eq!(
            MonoBitmap::new(usize::MAX, 9, Vec::new()),
            Err(DimensionError {
                expected: None,
                actual: 0
            })
        );
    }

    #[test]
    fn new_clears_padding_bits() {
        let bitmap = MonoBitmap::new(3, 2, vec![0xff, 0x5f]).expect("valid dimensions");
        assert_eq!(bitmap.data(), [0xe0, 0x40]);
    }

    #[test]
    fn pixel_access_is_msb_first() {
        let bitmap = MonoBitmap::new(10, 1, vec![0x80, 0x40]).expect("valid dimensions");
        let black: Vec<usize> = (0..10).filter(|&x| bitmap.pixel(x, 0)).collect();
        assert_eq!(black, [0, 9]);
    }

    #[test]
    #[should_panic(expected = "outside a 3x1 bitmap")]
    fn pixel_access_rejects_padding_bits() {
        let bitmap = MonoBitmap::new(3, 1, vec![0]).expect("valid dimensions");
        let _ = bitmap.pixel(3, 0);
    }

    #[test]
    fn rotate_clockwise() {
        let source = bitmap(&["#..", ".##"]);
        assert_eq!(
            source.rotate(Rotation::Clockwise),
            bitmap(&[".#", "#.", "#."])
        );
    }

    #[test]
    fn rotate_counter_clockwise() {
        let source = bitmap(&["#..", ".##"]);
        assert_eq!(
            source.rotate(Rotation::CounterClockwise),
            bitmap(&[".#", ".#", "#."])
        );
    }

    #[test]
    fn rotate_half_turn() {
        let source = bitmap(&["#..", ".##"]);
        assert_eq!(source.rotate(Rotation::HalfTurn), bitmap(&["##.", "..#"]));
    }

    #[test]
    fn half_turn_fast_path_matches_two_quarter_turns() {
        for (width, height) in [(16, 5), (8, 1), (24, 3)] {
            let source = pattern(width, height);
            let twice = source
                .clone()
                .rotate(Rotation::Clockwise)
                .rotate(Rotation::Clockwise);
            assert_eq!(source.rotate(Rotation::HalfTurn), twice, "{width}x{height}");
        }
    }

    #[test]
    fn quarter_turns_compose_to_identity() {
        let source = pattern(13, 7);
        let round_trip = source
            .clone()
            .rotate(Rotation::Clockwise)
            .rotate(Rotation::CounterClockwise);
        assert_eq!(round_trip, source);
        let full_turn = (0..4).fold(source.clone(), |b, _| b.rotate(Rotation::Clockwise));
        assert_eq!(full_turn, source);
    }

    #[test]
    fn rotation_handles_empty_bitmaps() {
        let empty = MonoBitmap::new(0, 4, Vec::new()).expect("valid dimensions");
        let rotated = empty.rotate(Rotation::Clockwise);
        assert_eq!((rotated.width_px(), rotated.height()), (4, 0));
    }

    #[test]
    fn clip_width_crops_and_clears_cut_bits() {
        let source = MonoBitmap::new(16, 2, vec![0xff, 0xff, 0x0f, 0xf0]).expect("valid");
        let clipped = source.clip_width(10);
        assert_eq!(clipped.width_px(), 10);
        assert_eq!(clipped.data(), [0xff, 0xc0, 0x0f, 0xc0]);
    }

    #[test]
    fn clip_width_keeps_narrower_images() {
        let source = pattern(9, 3);
        assert_eq!(source.clone().clip_width(9), source);
        assert_eq!(source.clone().clip_width(100), source);
    }

    #[test]
    fn clip_width_to_zero() {
        let clipped = pattern(9, 3).clip_width(0);
        assert_eq!((clipped.width_px(), clipped.height()), (0, 3));
        assert!(clipped.data().is_empty());
    }

    #[test]
    fn gray_image_validates_length() {
        assert_eq!(
            GrayImage::new(3, 2, vec![0; 5]),
            Err(DimensionError {
                expected: Some(6),
                actual: 5
            })
        );
    }

    #[test]
    fn gray_image_invert() {
        let mut image = GrayImage::new(3, 1, vec![0, 100, 255]).expect("valid dimensions");
        image.invert();
        assert_eq!(image.data(), [255, 155, 0]);
    }
}
