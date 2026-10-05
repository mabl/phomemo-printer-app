//! The byte stream that prints a page.
//!
//! A page goes out in two parts, with a pause between them for the printer
//! to re-initialise after `ESC @`:
//!
//! ```text
//! Preamble   1F 11 24 mm                left margin
//!            [1B 4E 04 dd]              density, if set
//!            [1B 4E 0D ss]              speed, if set
//!            [1F 11 tt]                 media tracking, if set
//!            1B 40                      ESC @
//!                -- settle, ~100 ms --
//! Raster     1F 11 21 nn                copies
//!            [1F 11 35 01]              compression on, if compressed
//!            1D 76 30 00 wL wH hL hH    GS v 0: row width in bytes, row count
//!            <rows, or LZO blocks>
//!            [1F 11 35 00]              compression off, if compressed
//! ```
//!
//! The skeleton is Print Master's `M200Printer.printBitmapx` - margin, then
//! `ESC @`, copies and raster - which was validated on an M220
//! (`re/RE_RESULTS_BITMAP_GEOMETRY.md`, experiment 1 and Q5), with
//! compression wrapped around the raster as `M220CPrinter` does. Print
//! Master sends nothing else before `ESC @`: placing density, speed and
//! tracking there mirrors the driver this crate replaces, not a
//! vendor-validated order. The settle time is the validator's
//! (`src/phomemo/printing.py`, `settle_ms`). Nothing follows the raster:
//! the printer prints once it has every row and answers `1A 0F 0C`; the
//! CUPS filter's end-of-page and end-of-document commands make no
//! difference (same file, Q4).
//!
//! The margin belongs to the raster that follows it, so send a preamble
//! before every page, with the margin computed from that page's final
//! bitmap ([`LeftMargin::for_width`] of the head width and
//! [`MonoBitmap::stride`]), as Print Master does. A margin set once per
//! job is right only for pages whose bitmap has exactly the width it was
//! computed from. The margin is sent even when it is 0, as Print Master
//! (`setMargin(0)` for a full-width image) and phomemo-tools'
//! `rastertopd30.py` (`1F 11 24 00`) do - the validator sends it only when
//! positive - because it takes effect across the `ESC @` after it, so the
//! printer keeps it and a page without one could inherit an earlier one.
//!
//! A raster is always one `GS v 0` block, however tall: the vendor apps and
//! CUPS filter never split a page (`re/protocol/bitmap-encoding.md`, "In
//! practice, the entire page is sent as a single block"). Its width and
//! height are 16-bit fields, and a bitmap that exceeds them is rejected
//! rather than truncated.

use std::error::Error;
use std::fmt;
use std::num::NonZeroU8;

use crate::bitmap::MonoBitmap;
use crate::commands::{Command, Density, LeftMargin, Speed, raster_header};
#[cfg(feature = "compress")]
use crate::compress::{CompressError, compress_blocks};
use crate::media::MediaTracking;

/// A page that cannot be encoded.
#[derive(Debug)]
pub enum EncodeError {
    /// A row is wider than the header's 16-bit byte count.
    WidthTooLarge {
        /// Bytes per row.
        width_bytes: usize,
    },
    /// There are more rows than the header's 16-bit row count.
    HeightTooLarge {
        /// Rows in the bitmap.
        rows: usize,
    },
    /// The raster could not be compressed.
    #[cfg(feature = "compress")]
    Compress(CompressError),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WidthTooLarge { width_bytes } => {
                write!(
                    f,
                    "raster rows of {width_bytes} bytes exceed the 65535-byte limit"
                )
            }
            Self::HeightTooLarge { rows } => {
                write!(f, "raster of {rows} rows exceeds the 65535-row limit")
            }
            #[cfg(feature = "compress")]
            Self::Compress(_) => f.write_str("raster compression failed"),
        }
    }
}

impl Error for EncodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(feature = "compress")]
            Self::Compress(err) => Some(err),
            _ => None,
        }
    }
}

/// Everything sent before a page's raster: the margin, any settings, and the
/// `ESC @` that applies them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Preamble {
    /// Offset of the raster on the head; see [`LeftMargin::for_width`].
    pub left_margin: LeftMargin,
    /// Print density, or `None` to keep the printer's.
    pub density: Option<Density>,
    /// Print speed, or `None` to keep the printer's.
    pub speed: Option<Speed>,
    /// Media tracking, or `None` to keep the printer's.
    pub tracking: Option<MediaTracking>,
}

impl Preamble {
    /// The preamble's commands, in wire order.
    pub fn commands(self) -> impl Iterator<Item = Command> {
        [
            Some(Command::SetLeftMargin(self.left_margin)),
            self.density.map(Command::SetDensity),
            self.speed.map(Command::SetSpeed),
            self.tracking.map(Command::SetMediaTracking),
            Some(Command::Reset),
        ]
        .into_iter()
        .flatten()
    }

    /// Append the preamble's bytes to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        for command in self.commands() {
            command.encode_into(out);
        }
    }

    /// The preamble's bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }
}

/// A bitmap ready to send: its `GS v 0` header and its rows, either as they
/// are or LZO-compressed.
#[derive(Debug, Clone)]
pub struct Raster<'a> {
    header: [u8; 8],
    payload: Payload<'a>,
}

#[derive(Debug, Clone)]
enum Payload<'a> {
    Raw(&'a [u8]),
    #[cfg(feature = "compress")]
    Lzo(Vec<u8>),
}

impl<'a> Raster<'a> {
    /// Send `bitmap`'s rows as they are.
    ///
    /// # Errors
    ///
    /// Fails if the bitmap's width in bytes or its height exceeds 65535.
    pub fn uncompressed(bitmap: &'a MonoBitmap) -> Result<Self, EncodeError> {
        Ok(Self {
            header: header(bitmap)?,
            payload: Payload::Raw(bitmap.data()),
        })
    }

    /// Send `bitmap`'s rows LZO-compressed (see [`compress`](crate::compress)).
    ///
    /// # Errors
    ///
    /// Fails if the bitmap's width in bytes or its height exceeds 65535, or
    /// its rows cannot be compressed.
    #[cfg(feature = "compress")]
    pub fn compressed(bitmap: &MonoBitmap) -> Result<Self, EncodeError> {
        Ok(Self {
            header: header(bitmap)?,
            payload: Payload::Lzo(compress_blocks(bitmap.data()).map_err(EncodeError::Compress)?),
        })
    }

    /// Whether the rows are compressed.
    #[must_use]
    pub const fn is_compressed(&self) -> bool {
        match self.payload {
            Payload::Raw(_) => false,
            #[cfg(feature = "compress")]
            Payload::Lzo(_) => true,
        }
    }

    /// Bytes of row data sent after the header.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.payload().len()
    }

    const fn payload(&self) -> &[u8] {
        match &self.payload {
            Payload::Raw(rows) => rows,
            #[cfg(feature = "compress")]
            Payload::Lzo(blocks) => blocks.as_slice(),
        }
    }

    /// Append the commands that print this raster `copies` times to `out`.
    pub fn encode_into(&self, copies: NonZeroU8, out: &mut Vec<u8>) {
        out.reserve(EXTRA_BYTES + self.payload_len());
        Command::SetCopies(copies).encode_into(out);
        if self.is_compressed() {
            Command::SetCompression(true).encode_into(out);
        }
        out.extend_from_slice(&self.header);
        out.extend_from_slice(self.payload());
        if self.is_compressed() {
            Command::SetCompression(false).encode_into(out);
        }
    }

    /// The commands that print this raster `copies` times.
    #[must_use]
    pub fn encode(&self, copies: NonZeroU8) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(copies, &mut out);
        out
    }
}

/// Upper bound on what [`Raster::encode_into`] adds besides the payload:
/// copies, header and both compression toggles.
const EXTRA_BYTES: usize = 4 + 8 + 4 + 4;

fn header(bitmap: &MonoBitmap) -> Result<[u8; 8], EncodeError> {
    let width_bytes = bitmap.stride();
    let width =
        u16::try_from(width_bytes).map_err(|_| EncodeError::WidthTooLarge { width_bytes })?;
    let rows = bitmap.height();
    let height = u16::try_from(rows).map_err(|_| EncodeError::HeightTooLarge { rows })?;
    Ok(raster_header(width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: NonZeroU8 = NonZeroU8::MIN;

    /// A 9x2 bitmap: two bytes per row, the second holding one pixel.
    fn small_bitmap() -> MonoBitmap {
        MonoBitmap::new(9, 2, vec![0xf0, 0x80, 0x0f, 0x00]).expect("valid dimensions")
    }

    #[test]
    fn preamble_with_margin_only() {
        let preamble = Preamble {
            left_margin: LeftMargin::new(32),
            ..Preamble::default()
        };
        assert_eq!(preamble.encode(), [0x1f, 0x11, 0x24, 0x20, 0x1b, 0x40]);
    }

    #[test]
    fn preamble_sends_a_zero_margin_like_print_master() {
        // M200Printer.printBitmapx calls setMargin(0) for full-width images,
        // and phomemo-tools' D30 filter sends 1F 11 24 00.
        assert_eq!(
            Preamble::default().encode(),
            [0x1f, 0x11, 0x24, 0x00, 0x1b, 0x40]
        );
    }

    #[test]
    fn preamble_with_density() {
        let preamble = Preamble {
            density: Density::new(8),
            ..Preamble::default()
        };
        assert_eq!(
            preamble.encode(),
            [0x1f, 0x11, 0x24, 0x00, 0x1b, 0x4e, 0x04, 0x08, 0x1b, 0x40]
        );
    }

    #[test]
    fn preamble_with_speed() {
        let preamble = Preamble {
            speed: Speed::new(3),
            ..Preamble::default()
        };
        assert_eq!(
            preamble.encode(),
            [0x1f, 0x11, 0x24, 0x00, 0x1b, 0x4e, 0x0d, 0x03, 0x1b, 0x40]
        );
    }

    #[test]
    fn preamble_with_tracking() {
        let preamble = Preamble {
            tracking: Some(MediaTracking::Mark),
            ..Preamble::default()
        };
        assert_eq!(
            preamble.encode(),
            [0x1f, 0x11, 0x24, 0x00, 0x1f, 0x11, 0x26, 0x1b, 0x40]
        );
    }

    #[test]
    fn preamble_with_everything_keeps_wire_order() {
        let preamble = Preamble {
            left_margin: LeftMargin::new(32),
            density: Density::new(8),
            speed: Speed::new(1),
            tracking: Some(MediaTracking::Gap),
        };
        assert_eq!(
            preamble.encode(),
            [
                0x1f, 0x11, 0x24, 0x20, // left margin 32
                0x1b, 0x4e, 0x04, 0x08, // density 8
                0x1b, 0x4e, 0x0d, 0x02, // speed 1, sent as 2
                0x1f, 0x11, 0x0a, // gap
                0x1b, 0x40, // ESC @
            ]
        );
    }

    #[test]
    fn uncompressed_page() {
        let bitmap = small_bitmap();
        let raster = Raster::uncompressed(&bitmap).expect("fits");
        assert!(!raster.is_compressed());
        assert_eq!(raster.payload_len(), 4);
        assert_eq!(
            raster.encode(NonZeroU8::new(2).expect("non-zero")),
            [
                0x1f, 0x11, 0x21, 0x02, // copies 2
                0x1d, 0x76, 0x30, 0x00, 0x02, 0x00, 0x02, 0x00, // 2 bytes x 2 rows
                0xf0, 0x80, 0x0f, 0x00, // rows
            ]
        );
    }

    #[cfg(feature = "compress")]
    #[test]
    fn compressed_page() {
        let bitmap = MonoBitmap::new(16, 64, vec![0; 128]).expect("valid dimensions");
        let raster = Raster::compressed(&bitmap).expect("fits");
        assert!(raster.is_compressed());
        assert_eq!(
            raster.encode(ONE),
            [
                0x1f, 0x11, 0x21, 0x01, // copies 1
                0x1f, 0x11, 0x35, 0x01, // compression on
                0x1d, 0x76, 0x30, 0x00, 0x02, 0x00, 0x40, 0x00, // 2 bytes x 64 rows
                0x09, 0x00, 0x00, // one LZO block of 9 bytes:
                0x12, 0x00, 0x20, 0x5e, 0x00, 0x00, // 128 zeros
                0x11, 0x00, 0x00, // end of stream
                0x1f, 0x11, 0x35, 0x00, // compression off
            ]
        );
    }

    #[test]
    fn m220_label_matches_the_validated_sequence() {
        // `re/RE_RESULTS_BITMAP_GEOMETRY.md` Q5: a 40-byte-wide image on the
        // M220's 72-byte head, one copy.
        let bitmap = MonoBitmap::new(320, 1, vec![0xff; 40]).expect("valid dimensions");
        let margin = LeftMargin::for_width(72, bitmap.stride()).expect("fits in a byte");
        let mut stream = Preamble {
            left_margin: margin,
            ..Preamble::default()
        }
        .encode();
        Raster::uncompressed(&bitmap)
            .expect("fits")
            .encode_into(ONE, &mut stream);

        let mut expected = vec![
            0x1f, 0x11, 0x24, 0x20, // LEFT_MARGIN 72 - 40
            0x1b, 0x40, // INIT_PRINTER
            0x1f, 0x11, 0x21, 0x01, // PRINT_MULTI 1
            0x1d, 0x76, 0x30, 0x00, 0x28, 0x00, 0x01, 0x00, // PRINT_IMAGE 40 x 1
        ];
        expected.extend([0xff; 40]);
        assert_eq!(stream, expected);
    }

    #[test]
    fn preamble_commands_are_in_wire_order() {
        let preamble = Preamble {
            tracking: Some(MediaTracking::Continuous),
            ..Preamble::default()
        };
        assert_eq!(
            preamble.commands().collect::<Vec<_>>(),
            [
                Command::SetLeftMargin(LeftMargin::ZERO),
                Command::SetMediaTracking(MediaTracking::Continuous),
                Command::Reset,
            ]
        );
    }

    #[test]
    fn rejects_rows_too_wide_for_the_header() {
        let bitmap = MonoBitmap::new(65_536 * 8, 1, vec![0; 65_536]).expect("valid dimensions");
        assert!(matches!(
            Raster::uncompressed(&bitmap),
            Err(EncodeError::WidthTooLarge {
                width_bytes: 65_536
            })
        ));
        let widest = MonoBitmap::new(65_535 * 8, 1, vec![0; 65_535]).expect("valid dimensions");
        assert!(Raster::uncompressed(&widest).is_ok());
    }

    #[test]
    fn rejects_more_rows_than_the_header_counts() {
        let bitmap = MonoBitmap::new(1, 65_536, vec![0; 65_536]).expect("valid dimensions");
        assert!(matches!(
            Raster::uncompressed(&bitmap),
            Err(EncodeError::HeightTooLarge { rows: 65_536 })
        ));
        let tallest = MonoBitmap::new(1, 65_535, vec![0; 65_535]).expect("valid dimensions");
        assert!(Raster::uncompressed(&tallest).is_ok());
    }
}
