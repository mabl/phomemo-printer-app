//! Individual printer commands and their wire encoding.
//!
//! This module is the one place that knows the command bytes; [`job`]
//! sequences them into pages. Apart from `ESC @` and the raster header
//! `GS v 0`, every command belongs to one of two families, named here by
//! their leading bytes:
//!
//! - **`ESC N`** (`1B 4E nn ..`): printer settings. The vendor CUPS filter
//!   `rastertolabelmxxx` and phomemo-tools' `rastertopm110.py` set density
//!   and speed this way; Print Master uses the family for time sync and
//!   auto power-off, and sets density with `1F 11 02` instead.
//! - **`US DC1`** (`1F 11 nn ..`): everything else - media tracking, margin,
//!   copies, compression and status queries.
//!
//! The reverse-engineering notes call these the "Linux" and "APK" dialects
//! (`re/protocol/commands.md`:6-8), but the split is not by host: the CUPS
//! filter sends `1F 11 0A` for media tracking, and Print Master sends
//! `1B 4E 1C` for time sync.
//!
//! A command encodes to at most [`EncodedCommand::MAX_LEN`] bytes, returned
//! by value so that sending one never allocates.
//!
//! [`job`]: crate::job

use std::error::Error;
use std::fmt;
use std::num::NonZeroU8;
use std::ops::Deref;

use crate::media::MediaTracking;

const ESC: u8 = 0x1b;
const GS: u8 = 0x1d;
const US: u8 = 0x1f;
const DC1: u8 = 0x11;

/// `GS v 0` (`1D 76 30 00 wL wH hL hH`): a monochrome raster of `rows` rows
/// of `width_bytes` bytes follows (`re/protocol/bitmap-encoding.md`,
/// "Monochrome Bitmap").
pub(crate) const fn raster_header(width_bytes: u16, rows: u16) -> [u8; 8] {
    let [w0, w1] = width_bytes.to_le_bytes();
    let [h0, h1] = rows.to_le_bytes();
    [GS, b'v', b'0', 0x00, w0, w1, h0, h1]
}

/// Print density (heat), `1..=15`.
///
/// Source: vendor CUPS filter `bCmdDensity`, which sends the PPD value
/// directly for every model but the M02 (`re/protocol/commands.md`, "Print
/// Density: `1B 4E 04 XX`").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Density(u8);

impl Density {
    /// The lightest density.
    pub const MIN: Self = Self(1);
    /// The darkest density.
    pub const MAX: Self = Self(15);

    /// The density `level`, if it is within `1..=15`.
    #[must_use]
    pub const fn new(level: u8) -> Option<Self> {
        if level >= Self::MIN.0 && level <= Self::MAX.0 {
            Some(Self(level))
        } else {
            None
        }
    }

    /// The density level.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Print speed, `1..=6` (1 = slowest).
///
/// The vendor CUPS filter sends level 1 as 2 (`bCmdSpeed`;
/// `re/protocol/commands.md`, "Print Speed: `1B 4E 0D XX`"), and so does
/// [`Command::SetSpeed`]. Speed changes motor velocity only, not line
/// spacing (`re/RE_RESULTS_BITMAP_GEOMETRY.md`, Q1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Speed(u8);

impl Speed {
    /// The slowest speed.
    pub const MIN: Self = Self(1);
    /// The fastest speed.
    pub const MAX: Self = Self(6);

    /// The speed `level`, if it is within `1..=6`.
    #[must_use]
    pub const fn new(level: u8) -> Option<Self> {
        if level >= Self::MIN.0 && level <= Self::MAX.0 {
            Some(Self(level))
        } else {
            None
        }
    }

    /// The speed level.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The level put on the wire.
    const fn wire_level(self) -> u8 {
        if self.0 == 1 { 2 } else { self.0 }
    }
}

/// Horizontal offset of the raster on the print head, in bytes (8 dots).
///
/// The printer starts each raster row this far into the head. Print Master
/// right-aligns images this way, sending `head - image` bytes, and the
/// printer does not treat a full-head-width raster padded with white the
/// same (`re/RE_RESULTS_BITMAP_GEOMETRY.md`, Q2-Q3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeftMargin(u8);

impl LeftMargin {
    /// No offset: the raster starts at the head's first dot.
    pub const ZERO: Self = Self(0);

    /// An offset of `bytes` x 8 dots.
    #[must_use]
    pub const fn new(bytes: u8) -> Self {
        Self(bytes)
    }

    /// The offset in bytes.
    #[must_use]
    pub const fn bytes(self) -> u8 {
        self.0
    }

    /// The offset that right-aligns an image `image_width_bytes` wide on a
    /// head `head_width_bytes` wide, as Print Master's
    /// `M200Printer.printBitmapx` does: their difference, or zero for an
    /// image at least as wide as the head.
    ///
    /// # Errors
    ///
    /// Returns [`MarginTooLarge`] if the offset exceeds the command's single
    /// byte. No head in [`model`](crate::model) is that wide.
    pub fn for_width(
        head_width_bytes: usize,
        image_width_bytes: usize,
    ) -> Result<Self, MarginTooLarge> {
        let bytes = head_width_bytes.saturating_sub(image_width_bytes);
        u8::try_from(bytes)
            .map(Self)
            .map_err(|_| MarginTooLarge { bytes })
    }
}

/// A left margin wider than `LEFT_MARGIN`'s one byte can express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarginTooLarge {
    /// The margin that was needed, in bytes.
    pub bytes: usize,
}

impl fmt::Display for MarginTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "left margin of {} bytes exceeds the 255-byte limit",
            self.bytes
        )
    }
}

impl Error for MarginTooLarge {}

/// A status query.
///
/// The printer answers with the
/// [`Response`](crate::responses::Response) named on each variant, in no
/// particular order and possibly not at all if its firmware lacks the
/// query (`re/protocol/sequences.md`, "Connection Handshake").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Query {
    /// `1F 11 12`; answered by `Response::Cover`.
    Cover,
    /// `1F 11 11`; answered by `Response::Paper`, possibly among other
    /// status responses.
    Paper,
    /// `1F 11 13` (Print Master: overheat); answered by
    /// `Response::Temperature`.
    Temperature,
    /// `1F 11 07`; answered by `Response::FirmwareVersion`.
    FirmwareVersion,
    /// `1F 11 08`; answered by `Response::SerialNumber`.
    SerialNumber,
    /// `1F 11 09`; answered by `Response::AutoOff`.
    AutoOff,
    /// `1F 11 63`; answered by `Response::ChipType`.
    ChipType,
}

impl Query {
    const fn code(self) -> u8 {
        match self {
            Self::Cover => 0x12,
            Self::Paper => 0x11,
            Self::Temperature => 0x13,
            Self::FirmwareVersion => 0x07,
            Self::SerialNumber => 0x08,
            Self::AutoOff => 0x09,
            Self::ChipType => 0x63,
        }
    }
}

impl MediaTracking {
    /// The paper-type byte of `1F 11 nn`, as Print Master's
    /// `QuinPrinter.setPaperType` sends it (`re/protocol/commands.md`, "Paper
    /// Type / Tracking: `1F 11 XX`").
    const fn code(self) -> u8 {
        match self {
            Self::Continuous => 0x0b,
            Self::Gap => 0x0a,
            Self::Mark => 0x26,
            Self::Card => 0x4e,
        }
    }
}

/// A command to the printer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    /// `1B 40` (`ESC @`): re-initialise the printer, which then needs a
    /// moment to settle. Print Master sends the margin just before it, and
    /// the margin takes effect.
    Reset,
    /// `1B 4E 04 nn`: set the print density.
    SetDensity(Density),
    /// `1B 4E 0D nn`: set the print speed.
    SetSpeed(Speed),
    /// `1F 11 nn`: select how the printer finds the next label.
    SetMediaTracking(MediaTracking),
    /// `1F 11 24 nn`: offset the raster on the head.
    SetLeftMargin(LeftMargin),
    /// `1F 11 21 nn`: print the next raster this many times.
    SetCopies(NonZeroU8),
    /// `1F 11 35 01` / `1F 11 35 00`: announce that raster data is, or is no
    /// longer, LZO-compressed.
    SetCompression(bool),
    /// `1F 11 nn`: ask for a status value.
    Query(Query),
}

impl Command {
    /// The command's bytes.
    #[must_use]
    pub fn encode(self) -> EncodedCommand {
        match self {
            Self::Reset => EncodedCommand::new([ESC, b'@']),
            Self::SetDensity(density) => EncodedCommand::new([ESC, b'N', 0x04, density.get()]),
            Self::SetSpeed(speed) => EncodedCommand::new([ESC, b'N', 0x0d, speed.wire_level()]),
            Self::SetMediaTracking(tracking) => EncodedCommand::new([US, DC1, tracking.code()]),
            Self::SetLeftMargin(margin) => EncodedCommand::new([US, DC1, 0x24, margin.bytes()]),
            Self::SetCopies(copies) => EncodedCommand::new([US, DC1, 0x21, copies.get()]),
            Self::SetCompression(enabled) => {
                EncodedCommand::new([US, DC1, 0x35, u8::from(enabled)])
            }
            Self::Query(query) => EncodedCommand::new([US, DC1, query.code()]),
        }
    }

    /// Append the command's bytes to `out`.
    pub fn encode_into(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.encode());
    }
}

/// The bytes of one [`Command`], held inline. Dereferences to `[u8]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EncodedCommand {
    bytes: [u8; Self::MAX_LEN],
    len: usize,
}

impl EncodedCommand {
    /// The longest command, in bytes.
    pub const MAX_LEN: usize = 4;

    const fn new<const N: usize>(bytes: [u8; N]) -> Self {
        const { assert!(N <= Self::MAX_LEN) }
        let mut buffer = [0; Self::MAX_LEN];
        buffer.split_at_mut(N).0.copy_from_slice(&bytes);
        Self {
            bytes: buffer,
            len: N,
        }
    }
}

impl Deref for EncodedCommand {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl AsRef<[u8]> for EncodedCommand {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// Formats as hex bytes, e.g. `EncodedCommand(1B 40)`.
impl fmt::Debug for EncodedCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncodedCommand(")?;
        for (i, byte) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{byte:02X}")?;
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(command: Command) -> Vec<u8> {
        command.encode().to_vec()
    }

    #[test]
    fn reset() {
        assert_eq!(bytes(Command::Reset), [0x1b, 0x40]);
    }

    #[test]
    fn density() {
        let density = Density::new(8).expect("in range");
        assert_eq!(
            bytes(Command::SetDensity(density)),
            [0x1b, 0x4e, 0x04, 0x08]
        );
        assert_eq!(
            bytes(Command::SetDensity(Density::MAX)),
            [0x1b, 0x4e, 0x04, 0x0f]
        );
    }

    #[test]
    fn density_range() {
        assert_eq!(Density::new(0), None);
        assert_eq!(Density::new(1), Some(Density::MIN));
        assert_eq!(Density::new(15), Some(Density::MAX));
        assert_eq!(Density::new(16), None);
    }

    #[test]
    fn speed_level_one_is_sent_as_two() {
        let speed = |level| Command::SetSpeed(Speed::new(level).expect("in range"));
        assert_eq!(bytes(speed(1)), [0x1b, 0x4e, 0x0d, 0x02]);
        assert_eq!(bytes(speed(2)), [0x1b, 0x4e, 0x0d, 0x02]);
        assert_eq!(bytes(speed(6)), [0x1b, 0x4e, 0x0d, 0x06]);
    }

    #[test]
    fn speed_range() {
        assert_eq!(Speed::new(0), None);
        assert_eq!(Speed::new(1), Some(Speed::MIN));
        assert_eq!(Speed::new(6), Some(Speed::MAX));
        assert_eq!(Speed::new(7), None);
    }

    #[test]
    fn media_tracking() {
        let tracking = |t| bytes(Command::SetMediaTracking(t));
        assert_eq!(tracking(MediaTracking::Continuous), [0x1f, 0x11, 0x0b]);
        assert_eq!(tracking(MediaTracking::Gap), [0x1f, 0x11, 0x0a]);
        assert_eq!(tracking(MediaTracking::Mark), [0x1f, 0x11, 0x26]);
        assert_eq!(tracking(MediaTracking::Card), [0x1f, 0x11, 0x4e]);
    }

    #[test]
    fn left_margin() {
        let margin = Command::SetLeftMargin(LeftMargin::new(32));
        assert_eq!(bytes(margin), [0x1f, 0x11, 0x24, 0x20]);
    }

    #[test]
    fn left_margin_right_aligns_on_the_head() {
        // `re/RE_RESULTS_BITMAP_GEOMETRY.md`: a 40-byte image on the M220's
        // 72-byte head needs a margin of 32.
        assert_eq!(LeftMargin::for_width(72, 40), Ok(LeftMargin::new(32)));
        assert_eq!(LeftMargin::for_width(72, 72), Ok(LeftMargin::ZERO));
        assert_eq!(LeftMargin::for_width(72, 80), Ok(LeftMargin::ZERO));
        assert_eq!(LeftMargin::for_width(300, 45), Ok(LeftMargin::new(255)));
        assert_eq!(
            LeftMargin::for_width(300, 44),
            Err(MarginTooLarge { bytes: 256 })
        );
    }

    #[test]
    fn copies() {
        let copies = Command::SetCopies(NonZeroU8::new(3).expect("non-zero"));
        assert_eq!(bytes(copies), [0x1f, 0x11, 0x21, 0x03]);
    }

    #[test]
    fn compression() {
        assert_eq!(
            bytes(Command::SetCompression(true)),
            [0x1f, 0x11, 0x35, 0x01]
        );
        assert_eq!(
            bytes(Command::SetCompression(false)),
            [0x1f, 0x11, 0x35, 0x00]
        );
    }

    #[test]
    fn queries() {
        let query = |q| bytes(Command::Query(q));
        assert_eq!(query(Query::Cover), [0x1f, 0x11, 0x12]);
        assert_eq!(query(Query::Paper), [0x1f, 0x11, 0x11]);
        assert_eq!(query(Query::Temperature), [0x1f, 0x11, 0x13]);
        assert_eq!(query(Query::FirmwareVersion), [0x1f, 0x11, 0x07]);
        assert_eq!(query(Query::SerialNumber), [0x1f, 0x11, 0x08]);
        assert_eq!(query(Query::AutoOff), [0x1f, 0x11, 0x09]);
        assert_eq!(query(Query::ChipType), [0x1f, 0x11, 0x63]);
    }

    #[test]
    fn raster_header_is_little_endian() {
        assert_eq!(
            raster_header(72, 1000),
            [0x1d, 0x76, 0x30, 0x00, 0x48, 0x00, 0xe8, 0x03]
        );
    }

    #[test]
    fn encode_into_appends() {
        let mut out = vec![0xaa];
        Command::Reset.encode_into(&mut out);
        Command::SetCompression(true).encode_into(&mut out);
        assert_eq!(out, [0xaa, 0x1b, 0x40, 0x1f, 0x11, 0x35, 0x01]);
    }

    #[test]
    fn encoded_command_debug_is_hex() {
        let encoded = Command::SetLeftMargin(LeftMargin::new(0x2a)).encode();
        assert_eq!(format!("{encoded:?}"), "EncodedCommand(1F 11 24 2A)");
    }
}
