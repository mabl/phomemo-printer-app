//! One page, from PAPPL's raster lines to the bitmap and raster sent to the
//! printer.
//!
//! [`Layout`] decides up front which part of the page can reach the head
//! and whether the page is turned; [`Page`] keeps only that part of each
//! line as it arrives, so the memory a page takes is bounded by the head
//! rather than by whatever size the job declares, and
//! [`rasterize`](Page::rasterize)s it. [`encode_raster`] then chooses
//! whether to compress the bitmap.

use std::ffi::c_uint;
use std::ops::Range;

use phomemo_protocol::bitmap::{GrayImage, MonoBitmap, Rotation};
use phomemo_protocol::dither::{self, Algorithm};
use phomemo_protocol::job::{EncodeError, Raster};

use super::Error;
use super::host::Log;
use super::options::{Compression, RasterHeader};
use crate::pappl::{LogLevel, PM_CSPACE_K, PM_CSPACE_SW, PM_CSPACE_W};

/// The most rows a raster can have: its header counts them in 16 bits.
const MAX_ROWS: usize = u16::MAX as usize;

/// The quarter turn that lays a page wider than the head along the feed.
///
/// Counter-clockwise, as phomemo-tools' `rastertopd30.py` turns every D30
/// page (`Image.ROTATE_90`). Print Master turns D30 labels by its
/// template's print direction and then sends the rows in reverse
/// (`XNvUtil.img2Nv(bitmap, 128, true)` scans bottom-up and right-to-left),
/// which is a further half turn; a template direction of 90 degrees comes
/// out as this same turn.
const SIDEWAYS: Rotation = Rotation::CounterClockwise;

/// Which way a page's pixel values run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    /// Larger values are darker: CUPS `K`, the PWG `black_8` type.
    Ink,
    /// Larger values are lighter: CUPS `W` and `SW`, the PWG `sgray_8` type.
    Light,
}

impl Polarity {
    /// The polarity of 8-bit pixels in CUPS color space `color_space`, if
    /// the driver reads it.
    ///
    /// `K` puts black at full scale and the luminance spaces at 0. PAPPL
    /// works the same way: it pads `K` lines with 0 and other lines with
    /// 255 (`job-process.c`, `_papplJobProcessRaster`), and inverts images
    /// it rasterizes to `K` (`job-filter.c`, `papplJobFilterImage`). Only
    /// 8-bit rasters reach the driver: `tp_driver_cb` offers no 1-bit type.
    const fn new(bits_per_pixel: c_uint, color_space: c_uint) -> Option<Self> {
        match (bits_per_pixel, color_space) {
            (8, PM_CSPACE_K) => Some(Self::Ink),
            (8, PM_CSPACE_W | PM_CSPACE_SW) => Some(Self::Light),
            _ => None,
        }
    }
}

/// Which part of a page reaches the head, and whether the page is turned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Whether the page is turned a quarter turn, by [`SIDEWAYS`].
    pub turn: bool,
    /// The pixels of each line that are kept.
    pub columns: Range<usize>,
    /// The lines that are kept.
    pub rows: Range<usize>,
}

impl Layout {
    /// The layout of a `width` x `height` page on a head `head` dots wide.
    ///
    /// A page is turned only on a model whose media is all wider than its
    /// head ([`Model::has_sideways_media`](crate::models::Model::has_sideways_media)),
    /// and only if it is wider than the head and turning makes it narrower:
    /// on other models a page a little wider than the head (50 mm labels on
    /// a 48 mm head) is media that runs across the head.
    ///
    /// Whatever then runs across the head beyond its width is cropped
    /// evenly from both sides. A bitmap narrower than the head is aligned
    /// by the left margin with the far end of the head, where the M220's
    /// paper guide holds a 40 mm label (`re/RE_RESULTS_BITMAP_GEOMETRY.md`,
    /// Q3), but no source shows on which side media wider than the head
    /// overhangs it: Print Master sends such a bitmap whole with no margin
    /// (`M200Printer.printBitmapx`, `D30Printer.printNormal`), and
    /// `M110Printer.fillWhitePaddingWithMSeries` even pads near-full-width
    /// bitmaps on the other side. Cropping both sides evenly misplaces the
    /// content by at most half the overhang either way: 1 mm for 50 mm
    /// labels on an M110, 1.5 mm for 15 mm labels on a D30.
    #[must_use]
    pub fn new(width: usize, height: usize, head: usize, sideways_media: bool) -> Self {
        let turn = sideways_media && width > head && height < width;
        if turn {
            Self {
                turn,
                columns: 0..width,
                rows: centred(height, head),
            }
        } else {
            Self {
                turn,
                columns: centred(width, head),
                rows: 0..height,
            }
        }
    }

    /// Dots across the head: the bitmap's width.
    #[must_use]
    pub fn across(&self) -> usize {
        if self.turn {
            self.rows.len()
        } else {
            self.columns.len()
        }
    }

    /// Rows along the feed: the bitmap's height.
    #[must_use]
    pub fn along(&self) -> usize {
        if self.turn {
            self.columns.len()
        } else {
            self.rows.len()
        }
    }
}

/// The middle `max` of `len` positions, or all of them.
fn centred(len: usize, max: usize) -> Range<usize> {
    let keep = len.min(max);
    let start = (len - keep) / 2;
    start..start + keep
}

/// The lines of a page received so far, cropped to its [`Layout`].
#[derive(Debug)]
pub struct Page {
    polarity: Polarity,
    line_len: usize,
    lines_expected: usize,
    lines_received: usize,
    layout: Layout,
    data: Vec<u8>,
}

impl Page {
    /// An empty page described by `header`, for a head `head` dots wide,
    /// logging a warning if part of it cannot be printed.
    ///
    /// # Errors
    ///
    /// Fails if the pixel format is one the driver does not read, the
    /// geometry is inconsistent or longer than a raster can hold, or the
    /// page's memory cannot be reserved.
    pub fn new(
        header: &RasterHeader,
        head: usize,
        sideways_media: bool,
        log: &impl Log,
    ) -> Result<Self, Error> {
        let polarity = Polarity::new(header.bits_per_pixel, header.color_space).ok_or(
            Error::UnsupportedRaster {
                bits_per_pixel: header.bits_per_pixel,
                color_space: header.color_space,
            },
        )?;
        if header.height == 0 {
            return Err(Error::NoLength);
        }
        if header.width == 0 || header.bytes_per_line != header.width {
            return Err(Error::BadGeometry(*header));
        }
        let layout = Layout::new(header.width, header.height, head, sideways_media);
        if layout.along() > MAX_ROWS {
            return Err(Error::TooManyLines {
                lines: layout.along(),
            });
        }
        // At most the head's width times a raster's rows.
        let size = layout.columns.len() * layout.rows.len();
        let mut data = Vec::new();
        data.try_reserve_exact(size)
            .map_err(|_| Error::OutOfMemory { bytes: size })?;

        let across = if layout.turn {
            header.height
        } else {
            header.width
        };
        if layout.across() < across {
            log.log(
                LogLevel::Warn,
                &format!(
                    "The page is {across} dots across the head, which prints {}; cropping the rest evenly from both sides.",
                    layout.across(),
                ),
            );
        }
        Ok(Self {
            polarity,
            line_len: header.bytes_per_line,
            lines_expected: header.height,
            lines_received: 0,
            layout,
            data,
        })
    }

    /// Bytes in each line.
    pub const fn line_len(&self) -> usize {
        self.line_len
    }

    /// Lines received so far.
    pub const fn lines(&self) -> usize {
        self.lines_received
    }

    /// Take the next line, [`line_len`](Self::line_len) bytes, keeping the
    /// part of it the layout keeps.
    ///
    /// # Errors
    ///
    /// Fails if the line is not [`line_len`](Self::line_len) bytes, or the
    /// page already has every line its header announced.
    pub fn push_line(&mut self, line: &[u8]) -> Result<(), Error> {
        if line.len() != self.line_len {
            return Err(Error::LineLength {
                expected: self.line_len,
                actual: line.len(),
            });
        }
        if self.lines_received == self.lines_expected {
            return Err(Error::TooManyLines {
                lines: self.lines_expected + 1,
            });
        }
        if self.layout.rows.contains(&self.lines_received) {
            self.data
                .extend_from_slice(&line[self.layout.columns.clone()]);
        }
        self.lines_received += 1;
        Ok(())
    }

    /// The kept lines as the bitmap to print, dithered with `algorithm` and
    /// turned as the layout says: at most the head's width across.
    ///
    /// # Errors
    ///
    /// Never in practice: the buffer holds whole cropped lines.
    pub fn rasterize(self, algorithm: Algorithm, log: &impl Log) -> Result<MonoBitmap, Error> {
        let width = self.layout.columns.len();
        let lines = self.data.len() / width;
        let mut image = GrayImage::new(width, lines, self.data)?;
        if self.polarity == Polarity::Ink {
            image.invert();
        }
        log.log(
            LogLevel::Debug,
            &format!("Dithering {width}x{lines} pixels with {algorithm}."),
        );
        let bitmap = dither::dither(&image, algorithm);
        Ok(if self.layout.turn {
            bitmap.rotate(SIDEWAYS)
        } else {
            bitmap
        })
    }
}

/// The raster to send for `bitmap`.
///
/// Compression is used only on models whose firmware accepts it: always
/// under [`Compression::On`], and under [`Compression::Auto`] when it makes
/// the raster smaller. Forcing it on a model without it, or LZO failing,
/// is logged and the raster goes uncompressed.
///
/// # Errors
///
/// Fails if the bitmap is too large for the raster header.
pub fn encode_raster<'a>(
    bitmap: &'a MonoBitmap,
    mode: Compression,
    model_supports_compression: bool,
    log: &impl Log,
) -> Result<Raster<'a>, EncodeError> {
    let uncompressed = Raster::uncompressed(bitmap)?;
    if mode == Compression::Off {
        return Ok(uncompressed);
    }
    if !model_supports_compression {
        if mode == Compression::On {
            log.log(
                LogLevel::Warn,
                "Compression is forced on, but this model does not support it; sending the raster uncompressed.",
            );
        }
        return Ok(uncompressed);
    }
    let compressed = match Raster::compressed(bitmap) {
        Ok(compressed) => compressed,
        Err(err) => {
            log.log(
                LogLevel::Warn,
                &format!("LZO compression failed ({err}); sending the raster uncompressed."),
            );
            return Ok(uncompressed);
        }
    };
    let (raw_len, lzo_len) = (uncompressed.payload_len(), compressed.payload_len());
    log.log(
        LogLevel::Debug,
        &format!("LZO compresses the raster from {raw_len} to {lzo_len} bytes."),
    );
    Ok(if mode == Compression::On || lzo_len < raw_len {
        compressed
    } else {
        uncompressed
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Messages(RefCell<Vec<(LogLevel, String)>>);

    impl Log for Messages {
        fn log(&self, level: LogLevel, message: &str) {
            self.0.borrow_mut().push((level, message.to_owned()));
        }
    }

    impl Messages {
        fn levels(&self) -> Vec<LogLevel> {
            self.0.borrow().iter().map(|(level, _)| *level).collect()
        }
    }

    fn header(width: usize, height: usize, color_space: c_uint) -> RasterHeader {
        RasterHeader {
            width,
            height,
            bytes_per_line: width,
            bits_per_pixel: 8,
            color_space,
        }
    }

    /// A page on a 576-dot head that runs across it.
    fn page(header: &RasterHeader, lines: &[&[u8]]) -> Page {
        let mut page = Page::new(header, 576, false, &Messages::default()).expect("valid header");
        for line in lines {
            page.push_line(line).expect("room for the line");
        }
        page
    }

    fn rasterize(page: Page) -> MonoBitmap {
        page.rasterize(Algorithm::Threshold, &Messages::default())
            .expect("whole lines")
    }

    #[test]
    fn luminance_puts_black_at_zero() {
        for color_space in [PM_CSPACE_SW, PM_CSPACE_W] {
            let bitmap = rasterize(page(&header(4, 1, color_space), &[&[0, 255, 0, 255]]));
            assert_eq!(bitmap.data(), [0b1010_0000], "color space {color_space}");
        }
    }

    #[test]
    fn k_puts_ink_at_full_scale() {
        let bitmap = rasterize(page(&header(4, 1, PM_CSPACE_K), &[&[255, 0, 255, 0]]));
        assert_eq!(bitmap.data(), [0b1010_0000]);
    }

    #[test]
    fn unsupported_formats_are_refused() {
        for (bits, color_space) in [(8, 1), (24, 19), (16, PM_CSPACE_SW), (1, PM_CSPACE_K)] {
            let header = RasterHeader {
                bits_per_pixel: bits,
                ..header(8, 1, color_space)
            };
            assert!(matches!(
                Page::new(&header, 576, false, &Messages::default()),
                Err(Error::UnsupportedRaster { .. })
            ));
        }
    }

    #[test]
    fn inconsistent_geometry_is_refused() {
        let bad = RasterHeader {
            bytes_per_line: 3,
            ..header(4, 1, PM_CSPACE_SW)
        };
        for header in [bad, header(0, 1, PM_CSPACE_SW)] {
            assert!(matches!(
                Page::new(&header, 576, false, &Messages::default()),
                Err(Error::BadGeometry(_))
            ));
        }
    }

    #[test]
    fn a_page_without_length_is_refused() {
        assert!(matches!(
            Page::new(
                &header(320, 0, PM_CSPACE_SW),
                576,
                false,
                &Messages::default()
            ),
            Err(Error::NoLength)
        ));
    }

    #[test]
    fn huge_pages_allocate_only_what_reaches_the_head() {
        // A page as wide and as long as a raster allows.
        let page = Page::new(
            &header(usize::MAX, MAX_ROWS, PM_CSPACE_SW),
            576,
            false,
            &Messages::default(),
        )
        .expect("cropped to the head");
        assert!(page.data.capacity() <= 576 * MAX_ROWS);

        // Turned, the page's width becomes the raster's rows.
        let turned = Page::new(
            &header(MAX_ROWS, 1000, PM_CSPACE_SW),
            96,
            true,
            &Messages::default(),
        )
        .expect("cropped to the head");
        assert!(turned.data.capacity() <= 96 * MAX_ROWS);
    }

    #[test]
    fn pages_longer_than_a_raster_are_refused() {
        assert!(matches!(
            Page::new(
                &header(8, MAX_ROWS + 1, PM_CSPACE_SW),
                576,
                false,
                &Messages::default()
            ),
            Err(Error::TooManyLines { .. })
        ));
        assert!(matches!(
            Page::new(
                &header(MAX_ROWS + 1, 100, PM_CSPACE_SW),
                96,
                true,
                &Messages::default()
            ),
            Err(Error::TooManyLines { .. })
        ));
    }

    #[test]
    fn lines_are_bounded_by_the_header() {
        let mut page = page(&header(1, 1, PM_CSPACE_SW), &[&[0]]);
        assert!(matches!(
            page.push_line(&[0]),
            Err(Error::TooManyLines { lines: 2 })
        ));
    }

    #[test]
    fn lines_must_have_the_header_length() {
        let mut page = page(&header(2, 2, PM_CSPACE_SW), &[]);
        assert!(matches!(
            page.push_line(&[0]),
            Err(Error::LineLength {
                expected: 2,
                actual: 1
            })
        ));
        assert_eq!(page.lines(), 0);
    }

    #[test]
    fn short_pages_keep_the_lines_received() {
        let bitmap = rasterize(page(&header(8, 5, PM_CSPACE_SW), &[&[0; 8], &[0; 8]]));
        assert_eq!((bitmap.width_px(), bitmap.height()), (8, 2));
    }

    fn layout(
        width: usize,
        height: usize,
        head: usize,
        sideways: bool,
    ) -> (bool, Range<usize>, Range<usize>) {
        let layout = Layout::new(width, height, head, sideways);
        (layout.turn, layout.columns, layout.rows)
    }

    #[test]
    fn pages_that_fit_are_left_alone() {
        assert_eq!(layout(320, 240, 576, false), (false, 0..320, 0..240));
        assert_eq!(layout(96, 240, 96, true), (false, 0..96, 0..240));
    }

    #[test]
    fn wide_pages_on_other_media_are_cropped_evenly() {
        // M110: a 50 mm label on a 48 mm head runs across the head.
        assert_eq!(layout(400, 240, 384, false), (false, 8..392, 0..240));
        assert_eq!(layout(401, 240, 384, false), (false, 8..392, 0..240));
    }

    #[test]
    fn wide_pages_on_sideways_media_are_turned_then_cropped_evenly() {
        // D30 default: a 30 x 15 mm label on a 12 mm head.
        assert_eq!(layout(240, 120, 96, true), (true, 0..240, 12..108));
        // 40 x 12 mm fits exactly once turned.
        assert_eq!(layout(320, 96, 96, true), (true, 0..320, 0..96));
    }

    #[test]
    fn portrait_pages_are_never_turned() {
        assert_eq!(layout(120, 240, 96, true), (false, 12..108, 0..240));
    }

    #[test]
    fn cropping_keeps_the_middle_and_warns() {
        let messages = Messages::default();
        let mut page = Page::new(&header(6, 1, PM_CSPACE_SW), 4, false, &messages).expect("valid");
        page.push_line(&[0, 255, 0, 0, 255, 0]).expect("fits");
        let bitmap = rasterize(page);
        assert_eq!((bitmap.width_px(), bitmap.data()), (4, &[0b0110_0000][..]));
        assert_eq!(messages.levels(), [LogLevel::Warn]);
        assert_eq!(
            messages.0.borrow()[0].1,
            "The page is 6 dots across the head, which prints 4; cropping the rest evenly from both sides."
        );
    }

    #[test]
    fn pages_that_fit_do_not_warn() {
        let messages = Messages::default();
        Page::new(&header(320, 240, PM_CSPACE_SW), 576, false, &messages).expect("valid");
        assert!(messages.levels().is_empty());
    }

    #[test]
    fn turning_is_counter_clockwise_and_keeps_the_middle_rows() {
        // A 16 x 12 page on an 8-dot head: rows 2..10 survive, turned.
        let messages = Messages::default();
        let mut page = Page::new(&header(16, 12, PM_CSPACE_SW), 8, true, &messages).expect("valid");
        for y in 0..12 {
            // Only row 2's first pixel is black.
            let mut line = [255; 16];
            if y == 2 {
                line[0] = 0;
            }
            page.push_line(&line).expect("fits");
        }
        let bitmap = rasterize(page);
        assert_eq!((bitmap.width_px(), bitmap.height()), (8, 16));
        // Top-left of the kept part ends up bottom-left.
        assert!(bitmap.pixel(0, 15));
        assert_eq!(messages.levels(), [LogLevel::Warn]);
    }

    fn compressible() -> MonoBitmap {
        MonoBitmap::new(64, 64, vec![0; 8 * 64]).expect("valid")
    }

    fn incompressible() -> MonoBitmap {
        let data = (0..64u32)
            .map(|i| u8::try_from(i.wrapping_mul(2_654_435_761) >> 24).expect("one byte"))
            .collect();
        MonoBitmap::new(64, 8, data).expect("valid")
    }

    fn encode(bitmap: &MonoBitmap, mode: Compression, supported: bool) -> (bool, Vec<LogLevel>) {
        let messages = Messages::default();
        let raster = encode_raster(bitmap, mode, supported, &messages).expect("fits");
        (raster.is_compressed(), messages.levels())
    }

    #[test]
    fn auto_compresses_only_when_smaller() {
        assert!(encode(&compressible(), Compression::Auto, true).0);
        assert!(!encode(&incompressible(), Compression::Auto, true).0);
    }

    #[test]
    fn on_compresses_even_when_larger() {
        assert!(encode(&incompressible(), Compression::On, true).0);
    }

    #[test]
    fn off_never_compresses() {
        assert_eq!(
            encode(&compressible(), Compression::Off, true),
            (false, vec![])
        );
    }

    #[test]
    fn unsupported_models_never_compress() {
        assert_eq!(
            encode(&compressible(), Compression::Auto, false),
            (false, vec![])
        );
        assert_eq!(
            encode(&compressible(), Compression::On, false),
            (false, vec![LogLevel::Warn])
        );
    }
}
