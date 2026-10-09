//! The raster driver: PAPPL's raster callbacks, turning pages into the
//! printer's byte stream.
//!
//! PAPPL calls the driver once to start a job, then for each page once to
//! start it, once per line and once to end it, and finally once to end the
//! job. [`Job`] keeps the state in between; [`ffi`] adapts the C callbacks
//! to it.
//!
//! # What goes out
//!
//! Nothing is sent until a page ends, because the left margin belongs to
//! the bitmap that follows it, whose width depends on how the page was
//! turned and cropped for the head ([`page::Layout`]). Each page then goes
//! out as [`job`](phomemo_protocol::job) describes: its own preamble - left
//! margin, density, speed if one was asked for, media tracking, `ESC @` -
//! then, after a pause for the printer to re-initialise, the raster. This
//! is Print Master's per-bitmap sequence (`M200Printer.printBitmapx` and
//! `D30Printer.printNormal` send `LEFT_MARGIN` and `ESC @` before every
//! bitmap), with the job's settings in the preamble rather than once per
//! job, so every page is complete in itself. The pause is the validator's
//! 100 ms settle, taken after every preamble.
//!
//! # Overprint canvases
//!
//! A page that is an overprint design ([`crate::overprint`]) - by its media
//! name or size or its page size, with the design's stock loaded - is laid
//! out by its profile instead: the label's left edge lands where an
//! ordinary label's first column does, the right bleed is dropped, the rows
//! sent follow `phomemo-overprint-vertical` (the job's, else the printer's
//! default from [`JobContext`], else `clip`), and it is tracked as the
//! loaded stock is. Every other page is sent exactly as without profiles.
//!
//! # What PAPPL already did
//!
//! Copies are never the driver's to print: every raster asks for one.
//! `papplJobFilterImage` (`job-filter.c`) prints an image once per copy.
//! Raster pages arrive already rendered: PAPPL advertises
//! `copies-supported` 1-1 for `image/pwg-raster` and `image/urf` requests
//! (`printer-ipp.c`, `_papplPrinterCopyAttributesNoLock`: "no copy support
//! for streaming raster formats"), and CUPS' driverless queues render the
//! copies themselves (`cupsManualCopies`). PAPPL still accepts `copies` up
//! to 999 on any job, but its raster path never repeats a page, so a raster
//! client that sends `copies` > 1 gets one copy - as with lprint. PAPPL
//! also counts the impressions.
//!
//! Nor is orientation: `papplJobFilterImage` rotates images and resets
//! `orientation-requested` to portrait ("Don't rotate in the driver"), and
//! raster clients send pages already laid out. The only turn the driver
//! makes is the one the head needs ([`page::Layout`]).

pub mod ffi;
mod host;
mod options;
mod page;

use std::error::Error as StdError;
use std::ffi::c_uint;
use std::fmt;
use std::io;
use std::num::NonZeroU8;
use std::thread;
use std::time::Duration;

use phomemo_protocol::bitmap::DimensionError;
use phomemo_protocol::commands::{LeftMargin, MarginTooLarge};
use phomemo_protocol::job::{EncodeError, Preamble, Raster};
use phomemo_protocol::media::MediaTracking;

pub use self::host::{Host, Log};
pub use self::options::{
    DARKNESS_LEVELS, INCH_PER_SECOND, JobContext, PrintOptions, RasterHeader, SPEED_MAX,
};
pub use self::page::MAX_ROWS;
use self::page::Page;
use crate::models::Model;
use crate::overprint::{self, Geometry, Note, OverprintProfile, Resolution, VerticalPolicy};
use crate::pappl::LogLevel;

/// How long the printer needs after `ESC @` before it takes the raster
/// (`src/phomemo/printing.py`, `settle_ms`).
const SETTLE: Duration = Duration::from_millis(100);

/// Why a raster callback failed.
#[derive(Debug)]
pub enum Error {
    /// A page callback came without a started page.
    NoPage,
    /// The job was canceled.
    Canceled,
    /// A line is not as long as the raster header says.
    LineLength {
        /// `cupsBytesPerLine`.
        expected: usize,
        /// Bytes in the line.
        actual: usize,
    },
    /// The page's pixels are in a format the driver does not read.
    UnsupportedRaster {
        /// `cupsBitsPerPixel`.
        bits_per_pixel: c_uint,
        /// `cupsColorSpace`.
        color_space: c_uint,
    },
    /// The raster header's dimensions do not add up.
    BadGeometry(RasterHeader),
    /// The page has no length: PAPPL rasterizes images onto the media size,
    /// and a continuous roll is 0 mm long.
    NoLength,
    /// The page has more lines than a raster can hold, or than its header
    /// announced.
    TooManyLines {
        /// Lines in the page.
        lines: usize,
    },
    /// The page's memory could not be reserved.
    OutOfMemory {
        /// Bytes needed.
        bytes: usize,
    },
    /// The lines did not make an image.
    Image(DimensionError),
    /// The bitmap is too narrow for the left margin to reach.
    Margin(MarginTooLarge),
    /// The bitmap does not fit in a raster.
    Encode(EncodeError),
    /// The device did not take the page.
    Io(io::Error),
    /// The page is an overprint design that cannot be printed on the
    /// loaded media ([`overprint::Resolution::Error`]).
    Overprint(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPage => f.write_str("no page has been started"),
            Self::Canceled => f.write_str("the job was canceled"),
            Self::LineLength { expected, actual } => {
                write!(f, "raster line of {actual} bytes instead of {expected}")
            }
            Self::UnsupportedRaster {
                bits_per_pixel,
                color_space,
            } => write!(
                f,
                "unsupported raster: {bits_per_pixel} bits per pixel in color space {color_space}"
            ),
            Self::BadGeometry(header) => write!(
                f,
                "inconsistent raster header: {}x{} pixels, {} bits per pixel, {} bytes per line",
                header.width, header.height, header.bits_per_pixel, header.bytes_per_line
            ),
            Self::NoLength => f.write_str(
                "the page has no length; continuous roll media needs a job media size with a length",
            ),
            Self::TooManyLines { lines } => write!(f, "page of {lines} lines is too long"),
            Self::OutOfMemory { bytes } => write!(f, "cannot allocate {bytes} bytes for the page"),
            Self::Image(err) => err.fmt(f),
            Self::Margin(err) => err.fmt(f),
            Self::Encode(err) => err.fmt(f),
            Self::Io(err) => write!(f, "device write failed: {err}"),
            // A sentence of its own; the caller adds the full stop.
            Self::Overprint(message) => f.write_str(message.trim_end_matches('.')),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Image(err) => Some(err),
            Self::Margin(err) => Some(err),
            Self::Encode(err) => Some(err),
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<DimensionError> for Error {
    fn from(err: DimensionError) -> Self {
        Self::Image(err)
    }
}

impl From<MarginTooLarge> for Error {
    fn from(err: MarginTooLarge) -> Self {
        Self::Margin(err)
    }
}

impl From<EncodeError> for Error {
    fn from(err: EncodeError) -> Self {
        Self::Encode(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// What a job has sent to the printer, which reports each page once it
/// has printed it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    /// Pages sent.
    pub pages: u32,
    /// The longest page's length on paper, in hundredths of a millimetre.
    pub longest_page: u32,
}

impl Sent {
    /// Count a page of `rows` rows at `dpi`.
    fn add(&mut self, rows: usize, dpi: u16) {
        let length = u64::try_from(rows)
            .unwrap_or(u64::MAX)
            .saturating_mul(2540)
            .div_ceil(u64::from(dpi.max(1)));
        self.pages = self.pages.saturating_add(1);
        self.longest_page = self
            .longest_page
            .max(u32::try_from(length).unwrap_or(u32::MAX));
    }
}

/// How many dots an overprint canvas's raster may be narrower or wider
/// than the canvas at the model's resolution and still be printed 1:1:
/// rasterizers round (CUPS, 352 dots for 44 mm at 203 dpi) or truncate
/// (PAPPL, 351), and a client's page size may be off by a few hundredths.
///
/// cbindgen:ignore
const RASTER_WIDTH_SLACK: usize = 4;

/// Why a page that resolved to `profile` was evidently not rasterized for
/// it at `dpi` and 100 %, if it was not: its `HWResolution` is set and is
/// not `dpi` in both axes, or its width is more than
/// [`RASTER_WIDTH_SLACK`] dots off the canvas's width.
fn raster_mismatch(profile: &OverprintProfile, dpi: u16, options: &PrintOptions) -> Option<String> {
    let [x, y] = options.resolution;
    let dpi_value = c_uint::from(dpi);
    if (x, y) != (0, 0) && (x, y) != (dpi_value, dpi_value) {
        return Some(format!("it was rasterized at {x}x{y} dpi"));
    }
    let width = options.raster.width;
    match profile.canvas_width_dots(dpi) {
        Some(canvas) if width.abs_diff(canvas) <= RASTER_WIDTH_SLACK => None,
        Some(canvas) => Some(format!(
            "its raster is {width} dots wide rather than the canvas's {canvas}"
        )),
        None => Some("the canvas's width in dots is out of range".to_owned()),
    }
}

/// Log each of `notes` at its own level.
fn log_notes(log: &impl Log, notes: &[Note]) {
    for note in notes {
        log.log(note.level, &note.text);
    }
}

/// The driver's state for one job.
#[derive(Debug)]
pub struct Job {
    model: &'static Model,
    context: JobContext,
    page: Option<Started>,
    sent: Sent,
    settle: Duration,
}

/// A page that has been started.
#[derive(Debug)]
struct Started {
    page: Page,
    /// Whether it is an overprint canvas, laid out by its profile.
    overprint: bool,
}

impl Job {
    /// A job printing on `model`, with what was read when it started.
    #[must_use]
    pub const fn new(model: &'static Model, context: JobContext) -> Self {
        Self {
            model,
            context,
            page: None,
            sent: Sent {
                pages: 0,
                longest_page: 0,
            },
            settle: SETTLE,
        }
    }

    /// Start a page with the geometry of the current options.
    ///
    /// A page that is an overprint design ([`overprint::resolve`]) is laid
    /// out by its profile's [`Geometry`]; any other page as it always was.
    ///
    /// # Errors
    ///
    /// Fails if the page is one the driver cannot print; see [`Page::new`]
    /// - or an overprint design whose stock is not loaded.
    pub fn start_page(&mut self, host: &impl Host) -> Result<(), Error> {
        self.page = None;
        let options = host.options();
        let started = match self.overprint_page(host)? {
            Some(page) => Started {
                page,
                overprint: true,
            },
            None => Started {
                page: Page::new(
                    &options.raster,
                    self.model.head_width_px(),
                    self.model.has_sideways_media(),
                    host,
                )?,
                overprint: false,
            },
        };
        self.page = Some(started);
        Ok(())
    }

    /// The current page as an overprint canvas, or `None` to print it as an
    /// ordinary page; logs how the page was resolved.
    ///
    /// A page that resolves to a profile is still printed as an ordinary
    /// page, with a warning, if it was not rasterized at the model's
    /// resolution and 100 % - its `HWResolution` is another, or its width
    /// is more than [`RASTER_WIDTH_SLACK`] dots off the canvas's - or if its
    /// raster does not reach the label.
    fn overprint_page(&self, host: &impl Host) -> Result<Option<Page>, Error> {
        let options = host.options();
        let resolution = overprint::resolve(
            self.model,
            &options.media(),
            options.page_points(),
            &self.context.ready(),
        );
        let (profile, rule, notes) = match resolution {
            Resolution::Ordinary { notes } => {
                log_notes(host, &notes);
                return Ok(None);
            }
            Resolution::Error(message) => return Err(Error::Overprint(message)),
            Resolution::Profile {
                profile,
                rule,
                notes,
            } => (profile, rule, notes),
        };
        log_notes(host, &notes);
        // A raster the driver cannot read fails as it would anyway, before
        // anything is said about its layout.
        let raster = &options.raster;
        page::validate_header(raster)?;

        let canvas = profile.canvas_name();
        let dpi = self.model.info().dpi;
        let ordinary = |why: &str| {
            host.log(
                LogLevel::Warn,
                &format!(
                    "The page is the overprint design {canvas}, but {why}; printing it as an ordinary page."
                ),
            );
            Ok(None)
        };
        if let Some(why) = raster_mismatch(profile, dpi, options) {
            return ordinary(&format!(
                "{why}; an overprint design must be rasterized at {dpi} dpi at 100 %"
            ));
        }
        let no_label = format!(
            "its {}x{} dot raster does not reach the label",
            raster.width, raster.height
        );
        let geometry = |policy| {
            Geometry::new(
                profile,
                dpi,
                self.model.head_width_bytes(),
                raster.width,
                raster.height,
                policy,
            )
        };
        // Whether a geometry exists does not depend on the policy, so the
        // policy - which may warn - is read only once it does.
        let Some(clip) = geometry(VerticalPolicy::Clip) else {
            return ordinary(&no_label);
        };
        let policy =
            options.overprint_vertical(self.context.overprint_vertical_default.as_deref(), host);
        let Some(geometry) = (match policy {
            VerticalPolicy::Clip => Some(clip),
            VerticalPolicy::Trailing => geometry(policy),
        }) else {
            return ordinary(&no_label);
        };
        let page = Page::overprint(raster, &geometry)?;
        host.log(
            LogLevel::Info,
            &format!(
                "Printing the overprint design {canvas} ({}, matched by {}) with vertical policy {}: canvas columns {}-{} and rows {}-{}, {} bytes from the head's start.",
                profile.label,
                rule.describe(),
                policy.name(),
                geometry.source_columns.start,
                geometry.source_columns.end.saturating_sub(1),
                geometry.rows.start,
                geometry.rows.end.saturating_sub(1),
                geometry.margin,
            ),
        );
        Ok(Some(page))
    }

    /// What has been sent to the printer so far.
    #[must_use]
    pub const fn sent(&self) -> Sent {
        self.sent
    }

    /// Bytes in each line of the current page, if one has been started.
    #[must_use]
    pub fn line_len(&self) -> Option<usize> {
        self.page.as_ref().map(|started| started.page.line_len())
    }

    /// Add the next line, [`line_len`](Self::line_len) bytes, to the page.
    ///
    /// # Errors
    ///
    /// Fails if the job was canceled, no page was started, or the line
    /// does not fit the page.
    pub fn write_line(&mut self, host: &impl Host, line: &[u8]) -> Result<(), Error> {
        if host.is_canceled() {
            return Err(Error::Canceled);
        }
        self.page
            .as_mut()
            .ok_or(Error::NoPage)?
            .page
            .push_line(line)
    }

    /// Finish the page and send it.
    ///
    /// An overprint canvas is tracked as the loaded stock is, whatever the
    /// job says (plan, D6) - or, if PAPPL names no single mode for it, as
    /// the job says, else by gap; its bitmap ends at the head's last dot,
    /// so the left margin is its geometry's.
    ///
    /// # Errors
    ///
    /// Fails if no page was started, the page cannot be encoded, or the
    /// device does not take it.
    pub fn end_page(&mut self, host: &mut impl Host) -> Result<(), Error> {
        let Started { page, overprint } = self.page.take().ok_or(Error::NoPage)?;
        // PAPPL ends a page whose lines stopped coming because the job was
        // canceled; it must not be printed.
        if host.is_canceled() {
            return Err(Error::Canceled);
        }
        let options = host.options();
        let compression = options.compression(host);
        let bitmap = page.rasterize(options.dither(host), host)?;
        let preamble = Preamble {
            left_margin: LeftMargin::for_width(self.model.head_width_bytes(), bitmap.stride())?,
            density: Some(options.density()),
            speed: options.speed(),
            tracking: if overprint {
                // The loaded stock's, if PAPPL names one mode; else the
                // job's; else gap, as a profile's stock is labels.
                self.context
                    .ready_tracking()
                    .or_else(|| options.tracking())
                    .or(Some(MediaTracking::Gap))
            } else {
                options.tracking()
            },
        };
        let raster = page::encode_raster(
            &bitmap,
            compression,
            self.model.info().supports_compression,
            host,
        )?;
        host.log(
            LogLevel::Debug,
            &format!(
                "Sending a {}x{} bitmap: {preamble:?}, {} raster.",
                bitmap.width_px(),
                bitmap.height(),
                if raster.is_compressed() {
                    "compressed"
                } else {
                    "uncompressed"
                },
            ),
        );
        self.send(host, preamble, &raster)?;
        self.sent.add(bitmap.height(), self.model.info().dpi);
        Ok(())
    }

    /// Send a page: its preamble, then, once the printer has settled, its
    /// raster.
    fn send(
        &self,
        host: &mut impl Host,
        preamble: Preamble,
        raster: &Raster<'_>,
    ) -> io::Result<()> {
        host.write_all(&preamble.encode())?;
        host.flush()?;
        thread::sleep(self.settle);
        host.write_all(&raster.encode(NonZeroU8::MIN))?;
        host.flush()?;
        Ok(())
    }

    /// Finish the job, discarding a page that was started but not ended -
    /// as when PAPPL aborts a job in the middle of a page.
    ///
    /// There is no end-of-job command: the printer prints each raster once
    /// it has every row (`re/RE_RESULTS_BITMAP_GEOMETRY.md`, Q4-Q5).
    ///
    /// # Errors
    ///
    /// Fails if the device cannot be flushed.
    pub fn end(self, host: &mut impl Host) -> Result<(), Error> {
        if let Some(Started { page, .. }) = self.page {
            host.log(
                LogLevel::Info,
                &format!("Discarding an unfinished page of {} lines.", page.lines()),
            );
        }
        host.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use phomemo_protocol::commands::{Density, Speed};

    use super::*;
    use crate::models::MediaSize;
    use crate::pappl::{
        PM_CSPACE_K, PM_CSPACE_SW, PM_MEDIA_TRACKING_CONTINUOUS, PM_MEDIA_TRACKING_GAP,
    };

    /// A host that keeps what the driver writes.
    #[derive(Default)]
    struct FakeHost {
        options: PrintOptions,
        canceled: bool,
        written: Vec<u8>,
        flushes: usize,
        log: RefCell<Vec<(LogLevel, String)>>,
    }

    impl Log for FakeHost {
        fn log(&self, level: LogLevel, message: &str) {
            self.log.borrow_mut().push((level, message.to_owned()));
        }
    }

    impl io::Write for FakeHost {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    impl Host for FakeHost {
        fn options(&self) -> &PrintOptions {
            &self.options
        }

        fn is_canceled(&self) -> bool {
            self.canceled
        }
    }

    impl FakeHost {
        fn warnings(&self) -> Vec<String> {
            self.log
                .borrow()
                .iter()
                .filter(|(level, _)| *level >= LogLevel::Warn)
                .map(|(_, message)| message.clone())
                .collect()
        }
    }

    fn model(name: &str) -> &'static Model {
        Model::by_name(name).expect("known model")
    }

    fn job(model_name: &str) -> Job {
        job_with(model_name, JobContext::default())
    }

    fn job_with(model_name: &str, context: JobContext) -> Job {
        Job {
            settle: Duration::ZERO,
            ..Job::new(model(model_name), context)
        }
    }

    /// A gray page of `width` x `height` pixels.
    fn gray_options(width: usize, height: usize, color_space: c_uint) -> PrintOptions {
        PrintOptions {
            raster: RasterHeader {
                width,
                height,
                bytes_per_line: width,
                bits_per_pixel: 8,
                color_space,
            },
            darkness_configured: 50,
            phomemo_compression: Some("off".to_owned()),
            ..PrintOptions::default()
        }
    }

    /// Print one page whose lines are `line(y)`.
    fn print_page(job: &mut Job, host: &mut FakeHost, line: impl Fn(usize) -> Vec<u8>) {
        job.start_page(host).expect("page starts");
        for y in 0..host.options.raster.height {
            job.write_line(host, &line(y)).expect("line fits");
        }
        job.end_page(host).expect("page ends");
    }

    /// A 40 x 30 mm gap label (320 x 240 dots) at the default darkness,
    /// black on the left half of each line.
    fn print_m220_label(job: &mut Job, options: PrintOptions) -> FakeHost {
        let mut host = FakeHost {
            options: PrintOptions {
                media_tracking: PM_MEDIA_TRACKING_GAP,
                media_length: 3000,
                ..options
            },
            ..FakeHost::default()
        };
        print_page(job, &mut host, |_| {
            let mut line = vec![0; 160];
            line.resize(320, 255);
            line
        });
        host
    }

    /// What [`print_m220_label`] sends: computed by hand from the protocol,
    /// as the driver sent it before overprint profiles.
    fn m220_label_bytes() -> Vec<u8> {
        let mut expected = vec![
            0x1f, 0x11, 0x24, 32, // LEFT_MARGIN 72 - 40 bytes
            0x1b, 0x4e, 0x04, 8, // density 8
            0x1f, 0x11, 0x0a, // gap tracking
            0x1b, 0x40, // ESC @
            0x1f, 0x11, 0x21, 0x01, // one copy
            0x1d, 0x76, 0x30, 0x00, 40, 0, 240, 0, // 40 bytes x 240 rows
        ];
        for _ in 0..240 {
            expected.extend([0xff; 20]);
            expected.extend([0x00; 20]);
        }
        expected
    }

    #[test]
    fn m220_label_byte_stream() {
        let mut job = job("M220");
        let mut host = print_m220_label(&mut job, gray_options(320, 240, PM_CSPACE_SW));
        job.end(&mut host).expect("job ends");
        assert_eq!(host.written, m220_label_bytes());
        // Preamble, raster, end of job.
        assert_eq!(host.flushes, 3);
        assert!(host.warnings().is_empty(), "{:?}", host.warnings());
    }

    /// An ordinary label is sent exactly as before, whatever is loaded and
    /// whatever the job says about media and page sizes and policies.
    #[test]
    fn ordinary_labels_are_unchanged_by_overprint() {
        let contexts = [
            JobContext::default(),
            stock_context(),
            JobContext {
                ready_tracking: PM_MEDIA_TRACKING_CONTINUOUS,
                overprint_vertical_default: Some("bogus".to_owned()),
                ..stock_context()
            },
        ];
        for context in contexts {
            for options in [
                gray_options(320, 240, PM_CSPACE_SW),
                PrintOptions {
                    media_name: STOCK.to_owned(),
                    media_width: 4000,
                    cups_page_size: [113.39, 85.04],
                    page_size: [113, 85],
                    resolution: [203, 203],
                    phomemo_overprint_vertical: Some("trailing".to_owned()),
                    ..gray_options(320, 240, PM_CSPACE_SW)
                },
            ] {
                let mut job = job_with("M220", context.clone());
                let host = print_m220_label(&mut job, options);
                assert_eq!(host.written, m220_label_bytes(), "{context:?}");
                assert!(host.warnings().is_empty(), "{:?}", host.warnings());
            }
        }
    }

    // Overprint canvases.

    const STOCK: &str = "om_40x30mm_40x30mm";
    const CANVAS: &str = "om_40x30mm-overprint-2mm_44x34mm";

    /// 40 x 30 mm gap labels loaded.
    fn stock_context() -> JobContext {
        JobContext {
            ready_name: STOCK.to_owned(),
            ready_size: MediaSize {
                width: 4000,
                length: 3000,
            },
            ready_tracking: PM_MEDIA_TRACKING_GAP,
            overprint_vertical_default: None,
        }
    }

    /// A `width` x `height` canvas page, named only by its media size
    /// (CUPS' driverless path), with no `cupsPageSize` or `PageSize`, so its
    /// page size is its pixels at 203 dpi.
    fn canvas_options(width: usize, height: usize, color_space: c_uint) -> PrintOptions {
        PrintOptions {
            media_name: "custom_44x34mm_44x34mm".to_owned(),
            media_width: 4400,
            media_length: 3400,
            media_tracking: PM_MEDIA_TRACKING_GAP,
            resolution: [203, 203],
            phomemo_dither: Some("threshold".to_owned()),
            ..gray_options(width, height, color_space)
        }
    }

    /// A page as the printer receives it.
    #[derive(Debug)]
    struct SentPage {
        /// Everything before the copies command.
        preamble: Vec<u8>,
        /// The bitmap's width in bytes.
        width_bytes: usize,
        rows: usize,
        data: Vec<u8>,
    }

    impl SentPage {
        /// Decode the one uncompressed page in `written`.
        fn decode(written: &[u8]) -> Self {
            let copies = [0x1f, 0x11, 0x21, 0x01];
            let at = written
                .windows(copies.len())
                .position(|window| window == copies)
                .expect("a copies command");
            let header = &written[at + 4..at + 12];
            assert_eq!(header[..4], [0x1d, 0x76, 0x30, 0x00], "GS v 0");
            let width_bytes = usize::from(u16::from_le_bytes([header[4], header[5]]));
            let rows = usize::from(u16::from_le_bytes([header[6], header[7]]));
            let data = written[at + 12..].to_vec();
            assert_eq!(data.len(), width_bytes * rows, "nothing after the rows");
            Self {
                preamble: written[..at].to_vec(),
                width_bytes,
                rows,
                data,
            }
        }

        /// The left margin, in bytes.
        fn margin(&self) -> u8 {
            assert_eq!(self.preamble[..3], [0x1f, 0x11, 0x24], "LEFT_MARGIN first");
            self.preamble[3]
        }

        /// The black dots, as (bitmap column, row).
        fn black(&self) -> Vec<(usize, usize)> {
            (0..self.rows)
                .flat_map(|y| (0..self.width_bytes * 8).map(move |x| (x, y)))
                .filter(|&(x, y)| self.data[y * self.width_bytes + x / 8] & (0x80 >> (x % 8)) != 0)
                .collect()
        }

        /// The head dot bitmap column `x` lands on.
        fn head_dot(&self, x: usize) -> usize {
            usize::from(self.margin()) * 8 + x
        }
    }

    /// Print a `width` x `height` canvas in `color_space` with dark dots at
    /// `marks` (canvas column, row), with `context` and `options` adjusted.
    fn print_canvas(
        context: JobContext,
        options: PrintOptions,
        marks: &[(usize, usize)],
    ) -> (SentPage, FakeHost) {
        let (white, ink) = if options.raster.color_space == PM_CSPACE_K {
            (0, 255)
        } else {
            (255, 0)
        };
        let width = options.raster.width;
        let mut host = FakeHost {
            options,
            ..FakeHost::default()
        };
        let mut job = job_with("M220", context);
        print_page(&mut job, &mut host, |y| {
            let mut line = vec![white; width];
            for &(mark_x, mark_y) in marks {
                if mark_y == y {
                    line[mark_x] = ink;
                }
            }
            line
        });
        (SentPage::decode(&host.written), host)
    }

    /// The label's corners, as canvas (column, row).
    const CORNERS: [(usize, usize); 4] = [(16, 16), (335, 16), (16, 255), (335, 255)];

    #[test]
    fn canvases_are_anchored_to_the_label() {
        for (width, height, color_space) in [(352, 272, PM_CSPACE_SW), (351, 271, PM_CSPACE_K)] {
            let case = format!("{width}x{height} in color space {color_space}");
            let mut marks = CORNERS.to_vec();
            marks.extend([
                // Left bleed.
                (0, 100),
                (15, 100),
                // Right bleed.
                (336, 100),
                (width - 1, 100),
                // Top bleed.
                (100, 0),
                (100, 15),
                // Bottom bleed.
                (100, 256),
                (100, height - 1),
            ]);
            let (page, host) = print_canvas(
                stock_context(),
                canvas_options(width, height, color_space),
                &marks,
            );
            assert_eq!(page.margin(), 30, "{case}");
            assert_eq!(page.width_bytes, 42, "{case}");
            assert_eq!(page.rows, 240, "{case}");
            // Output column o is canvas column o, output row r canvas row
            // r + 16; the right, top and bottom bleed are not sent.
            let mut expected = vec![(0, 84), (15, 84), (16, 0), (16, 239), (335, 0), (335, 239)];
            expected.sort_by_key(|&(x, y)| (y, x));
            assert_eq!(page.black(), expected, "{case}");
            // The label's first column lands where an ordinary label's does;
            // the last kept column on the head's last dot.
            assert_eq!(page.head_dot(16), 256, "{case}");
            assert_eq!(page.head_dot(335), 575, "{case}");
            // The margin is the geometry's.
            let geometry = Geometry::new(
                model("M220")
                    .overprint_profiles()
                    .next()
                    .expect("a profile"),
                203,
                72,
                width,
                height,
                VerticalPolicy::Clip,
            )
            .expect("a canvas");
            assert_eq!(usize::from(page.margin()), geometry.margin);
            assert!(host.warnings().is_empty(), "{:?}", host.warnings());
            assert!(
                host.log
                    .borrow()
                    .iter()
                    .any(|(level, message)| *level == LogLevel::Info
                        && message.contains(CANVAS)
                        && message.contains("clip")),
                "{case}"
            );
        }
    }

    #[test]
    fn trailing_sends_the_bottom_bleed() {
        for (width, height, color_space) in [(352, 272, PM_CSPACE_SW), (351, 271, PM_CSPACE_K)] {
            let mut marks = CORNERS.to_vec();
            marks.extend([(100, 15), (100, 256), (100, height - 1)]);
            let (page, _) = print_canvas(
                stock_context(),
                PrintOptions {
                    phomemo_overprint_vertical: Some("trailing".to_owned()),
                    ..canvas_options(width, height, color_space)
                },
                &marks,
            );
            assert_eq!(page.margin(), 30);
            assert_eq!(page.width_bytes, 42);
            assert_eq!(page.rows, height - 16);
            let mut expected = vec![
                (16, 0),
                (335, 0),
                (16, 239),
                (335, 239),
                (100, 240),
                (100, height - 17),
            ];
            expected.sort_by_key(|&(x, y)| (y, x));
            assert_eq!(page.black(), expected, "{width}x{height}");
        }
    }

    #[test]
    fn the_printer_default_policy_applies() {
        let (page, _) = print_canvas(
            JobContext {
                overprint_vertical_default: Some("trailing".to_owned()),
                ..stock_context()
            },
            canvas_options(352, 272, PM_CSPACE_SW),
            &[],
        );
        assert_eq!(page.rows, 256);
        // The job's own value wins.
        let (page, _) = print_canvas(
            JobContext {
                overprint_vertical_default: Some("trailing".to_owned()),
                ..stock_context()
            },
            PrintOptions {
                phomemo_overprint_vertical: Some("clip".to_owned()),
                ..canvas_options(352, 272, PM_CSPACE_SW)
            },
            &[],
        );
        assert_eq!(page.rows, 240);
    }

    #[test]
    fn ordinary_labels_start_on_the_same_head_dot() {
        for width in [319, 320] {
            let (page, host) = print_canvas(
                stock_context(),
                PrintOptions {
                    media_name: STOCK.to_owned(),
                    media_width: 4000,
                    media_length: 3000,
                    ..canvas_options(width, 240, PM_CSPACE_SW)
                },
                &[(0, 0), (width - 1, 239)],
            );
            assert_eq!(page.margin(), 32, "{width}");
            assert_eq!(page.width_bytes, 40, "{width}");
            assert_eq!(page.rows, 240, "{width}");
            assert_eq!(page.black(), [(0, 0), (width - 1, 239)], "{width}");
            assert_eq!(page.head_dot(0), 256, "{width}");
            assert!(host.warnings().is_empty(), "{:?}", host.warnings());
        }
    }

    #[test]
    fn canvases_are_tracked_as_the_loaded_stock() {
        // The job asks for continuous tracking; the gap labels loaded win.
        let (page, _) = print_canvas(
            stock_context(),
            PrintOptions {
                media_tracking: PM_MEDIA_TRACKING_CONTINUOUS,
                ..canvas_options(352, 272, PM_CSPACE_SW)
            },
            &[],
        );
        let expected = Preamble {
            left_margin: LeftMargin::for_width(72, 42).expect("fits"),
            density: Some(Density::new(8).expect("a density")),
            speed: None,
            tracking: Some(MediaTracking::Gap),
        }
        .encode();
        assert_eq!(page.preamble, expected);
        assert_eq!(page.preamble[8..11], [0x1f, 0x11, 0x0a]);

        // An ordinary page keeps the job's tracking.
        let (page, _) = print_canvas(
            stock_context(),
            PrintOptions {
                media_tracking: PM_MEDIA_TRACKING_CONTINUOUS,
                ..canvas_options(320, 240, PM_CSPACE_SW)
            },
            &[],
        );
        assert_eq!(page.preamble[8..11], [0x1f, 0x11, 0x0b]);
    }

    #[test]
    fn a_canvas_without_its_stock_fails_the_page() {
        let mut job = job_with(
            "M220",
            JobContext {
                ready_name: "om_50x30mm_50x30mm".to_owned(),
                ready_size: MediaSize {
                    width: 5000,
                    length: 3000,
                },
                ..stock_context()
            },
        );
        let host = FakeHost {
            options: PrintOptions {
                media_name: CANVAS.to_owned(),
                ..canvas_options(352, 272, PM_CSPACE_SW)
            },
            ..FakeHost::default()
        };
        let Err(err @ Error::Overprint(_)) = job.start_page(&host) else {
            panic!("the page must fail");
        };
        let message = err.to_string();
        assert!(message.contains(CANVAS), "{message}");
        assert!(message.contains("om_50x30mm_50x30mm"), "{message}");
        assert!(!message.ends_with('.'), "{message}");
        assert!(job.line_len().is_none());
        assert!(matches!(
            job.end_page(&mut FakeHost::default()),
            Err(Error::NoPage)
        ));
    }

    #[test]
    fn a_canvas_too_short_for_the_label_is_ordinary() {
        // Named and sized as the canvas, but only 10 rows: none of them
        // reaches the label.
        let (page, host) = print_canvas(
            stock_context(),
            PrintOptions {
                media_name: CANVAS.to_owned(),
                cups_page_size: [124.72, 96.38],
                // Not read, so not warned about, when there is no geometry.
                phomemo_overprint_vertical: Some("bogus".to_owned()),
                ..canvas_options(352, 10, PM_CSPACE_SW)
            },
            &[(0, 0)],
        );
        assert_eq!(host.warnings().len(), 1, "{:?}", host.warnings());
        assert!(host.warnings()[0].contains(CANVAS));
        assert!(host.warnings()[0].contains("does not reach the label"));
        assert!(infos(&host).is_empty(), "{:?}", infos(&host));
        // 352 dots in 44 bytes, from the head's 28th byte, all 10 rows.
        assert_eq!(page.margin(), 28);
        assert_eq!((page.width_bytes, page.rows), (44, 10));
        assert_eq!(page.black(), [(0, 0)]);
    }

    /// The info messages logged.
    fn infos(host: &FakeHost) -> Vec<String> {
        host.log
            .borrow()
            .iter()
            .filter(|(level, _)| *level == LogLevel::Info)
            .map(|(_, message)| message.clone())
            .collect()
    }

    #[test]
    fn a_canvas_named_by_its_name_prints() {
        let (page, host) = print_canvas(
            stock_context(),
            PrintOptions {
                media_name: CANVAS.to_owned(),
                cups_page_size: [124.72, 96.38],
                ..canvas_options(352, 272, PM_CSPACE_SW)
            },
            &CORNERS,
        );
        assert_eq!(page.margin(), 30);
        assert_eq!((page.width_bytes, page.rows), (42, 240));
        assert_eq!(page.black(), [(16, 0), (335, 0), (16, 239), (335, 239)]);
        let infos = infos(&host);
        assert_eq!(infos.len(), 1, "{infos:?}");
        assert!(infos[0].contains("matched by its media name"), "{infos:?}");
        assert!(host.warnings().is_empty(), "{:?}", host.warnings());
    }

    #[test]
    fn k_canvases_keep_white_white_and_ink_ink() {
        // No ink anywhere: nothing printed, edge columns included.
        let (blank, _) = print_canvas(stock_context(), canvas_options(351, 271, PM_CSPACE_K), &[]);
        assert_eq!(
            (blank.margin(), blank.width_bytes, blank.rows),
            (30, 42, 240)
        );
        assert!(blank.data.iter().all(|&byte| byte == 0));
        // Ink everywhere: every dot of the bitmap, edge columns included.
        let mut host = FakeHost {
            options: canvas_options(351, 271, PM_CSPACE_K),
            ..FakeHost::default()
        };
        let mut job = job_with("M220", stock_context());
        print_page(&mut job, &mut host, |_| vec![255; 351]);
        let ink = SentPage::decode(&host.written);
        assert_eq!((ink.margin(), ink.width_bytes, ink.rows), (30, 42, 240));
        assert!(ink.data.iter().all(|&byte| byte == 0xff));
    }

    /// The single warning of a canvas printed as an ordinary page.
    fn ordinary_canvas(options: PrintOptions) -> (SentPage, String) {
        let (page, host) = print_canvas(stock_context(), options, &[]);
        let warnings = host.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(infos(&host).is_empty(), "{:?}", infos(&host));
        (page, warnings[0].clone())
    }

    #[test]
    fn a_canvas_at_another_resolution_is_ordinary() {
        // 44 x 34 mm at 300 dpi, said to be the canvas by its page size.
        let (page, warning) = ordinary_canvas(PrintOptions {
            cups_page_size: [124.72, 96.38],
            resolution: [300, 300],
            ..canvas_options(520, 401, PM_CSPACE_SW)
        });
        assert!(warning.contains("rasterized at 300x300 dpi"), "{warning}");
        assert!(
            warning.contains("must be rasterized at 203 dpi at 100 %"),
            "{warning}"
        );
        // Printed as it is: 520 dots in 65 bytes.
        assert_eq!((page.margin(), page.width_bytes), (7, 65));
        // One axis off is enough.
        let (_, warning) = ordinary_canvas(PrintOptions {
            cups_page_size: [124.72, 96.38],
            resolution: [203, 406],
            ..canvas_options(352, 544, PM_CSPACE_SW)
        });
        assert!(warning.contains("203x406 dpi"), "{warning}");
    }

    #[test]
    fn a_scaled_canvas_is_ordinary() {
        // Shrunk to fit a 40 x 30 mm page, but still named the canvas.
        let (page, warning) = ordinary_canvas(PrintOptions {
            cups_page_size: [124.72, 96.38],
            ..canvas_options(320, 240, PM_CSPACE_SW)
        });
        assert!(
            warning.contains("320 dots wide rather than the canvas's 352"),
            "{warning}"
        );
        assert_eq!((page.margin(), page.width_bytes, page.rows), (32, 40, 240));
        // Four dots either side of 352 are the canvas; five are not.
        for width in [348, 356] {
            let (page, host) = print_canvas(
                stock_context(),
                PrintOptions {
                    cups_page_size: [124.72, 96.38],
                    ..canvas_options(width, 272, PM_CSPACE_SW)
                },
                &[],
            );
            assert_eq!(page.margin(), 30, "{width}");
            assert!(host.warnings().is_empty(), "{:?}", host.warnings());
        }
        for width in [347, 357] {
            let (page, _) = ordinary_canvas(PrintOptions {
                cups_page_size: [124.72, 96.38],
                ..canvas_options(width, 272, PM_CSPACE_SW)
            });
            assert_ne!(page.margin(), 30, "{width}");
        }
    }

    #[test]
    fn an_unknown_resolution_is_no_mismatch() {
        // No HWResolution: the page size comes from cupsPageSize.
        let (page, host) = print_canvas(
            stock_context(),
            PrintOptions {
                cups_page_size: [124.72, 96.38],
                resolution: [0, 0],
                ..canvas_options(352, 272, PM_CSPACE_SW)
            },
            &[],
        );
        assert_eq!((page.margin(), page.rows), (30, 240));
        assert!(host.warnings().is_empty(), "{:?}", host.warnings());
    }

    #[test]
    fn an_unreadable_canvas_fails_before_anything_is_logged() {
        let mut job = job_with("M220", stock_context());
        let mut options = canvas_options(352, 272, PM_CSPACE_SW);
        options.raster.bits_per_pixel = 16;
        let host = FakeHost {
            options,
            ..FakeHost::default()
        };
        assert!(matches!(
            job.start_page(&host),
            Err(Error::UnsupportedRaster { .. })
        ));
        assert!(host.warnings().is_empty(), "{:?}", host.warnings());
        assert!(infos(&host).is_empty(), "{:?}", infos(&host));
    }

    #[test]
    fn canvas_tracking_falls_back_to_the_job_then_gap() {
        // PAPPL names no single mode for the loaded stock.
        for ready_tracking in [0, PM_MEDIA_TRACKING_GAP | PM_MEDIA_TRACKING_CONTINUOUS] {
            let context = JobContext {
                ready_tracking,
                ..stock_context()
            };
            let (page, _) = print_canvas(
                context.clone(),
                PrintOptions {
                    media_tracking: PM_MEDIA_TRACKING_CONTINUOUS,
                    ..canvas_options(352, 272, PM_CSPACE_SW)
                },
                &[],
            );
            assert_eq!(page.preamble[8..11], [0x1f, 0x11, 0x0b], "the job's");
            let (page, _) = print_canvas(
                context,
                PrintOptions {
                    media_tracking: 0,
                    ..canvas_options(352, 272, PM_CSPACE_SW)
                },
                &[],
            );
            assert_eq!(page.preamble[8..11], [0x1f, 0x11, 0x0a], "gap");
        }
    }

    #[test]
    fn resolution_notes_are_logged() {
        // The job's media is the ready stock (no media-col), its page the
        // canvas: rule 3, noted at info level.
        let (page, host) = print_canvas(
            stock_context(),
            PrintOptions {
                media_name: STOCK.to_owned(),
                media_width: 4000,
                media_length: 3000,
                page_size: [124, 96],
                ..canvas_options(352, 272, PM_CSPACE_SW)
            },
            &[],
        );
        assert_eq!(page.margin(), 30);
        let infos: Vec<_> = host
            .log
            .borrow()
            .iter()
            .filter(|(level, _)| *level == LogLevel::Info)
            .map(|(_, message)| message.clone())
            .collect();
        assert_eq!(infos.len(), 2, "{infos:?}");
        assert!(infos[0].contains("names no media"), "{infos:?}");
        assert!(infos[1].contains("its page size"), "{infos:?}");
    }

    #[test]
    fn margin_follows_each_pages_width() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: gray_options(576, 1, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![255; 576]);
        assert_eq!(host.written[..4], [0x1f, 0x11, 0x24, 0]);

        host.written.clear();
        host.options = gray_options(100, 1, PM_CSPACE_SW);
        print_page(&mut job, &mut host, |_| vec![255; 100]);
        // 100 dots pack into 13 bytes: 72 - 13.
        assert_eq!(host.written[..4], [0x1f, 0x11, 0x24, 59]);
    }

    #[test]
    fn sent_pages_are_counted_with_the_longest() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: gray_options(8, 240, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        assert_eq!(job.sent(), Sent::default());
        print_page(&mut job, &mut host, |_| vec![0; 8]);
        host.options = gray_options(8, 80, PM_CSPACE_SW);
        print_page(&mut job, &mut host, |_| vec![0; 8]);
        // 240 rows at 203 dpi: 30.03 mm, rounded up.
        let sent = Sent {
            pages: 2,
            longest_page: 3003,
        };
        assert_eq!(job.sent(), sent);

        // A canceled page is not sent.
        job.start_page(&host).expect("page starts");
        host.canceled = true;
        assert!(job.end_page(&mut host).is_err());
        assert_eq!(job.sent(), sent);
    }

    #[test]
    fn wider_pages_are_cropped_to_the_head() {
        let mut job = job("M110");
        let mut host = FakeHost {
            options: gray_options(400, 2, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![0; 400]);
        assert_eq!(host.warnings().len(), 1, "cropping is logged");
        let raster_header = [0x1d, 0x76, 0x30, 0x00, 48, 0, 2, 0];
        assert!(
            host.written
                .windows(raster_header.len())
                .any(|window| window == raster_header)
        );
        assert_eq!(host.written[..4], [0x1f, 0x11, 0x24, 0]);
    }

    #[test]
    fn d30_labels_are_turned_onto_the_head() {
        // 30 x 15 mm: 240 x 120 dots on a 96-dot head.
        let mut job = job("D30");
        let mut host = FakeHost {
            options: gray_options(240, 120, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![0; 240]);
        let raster_header = [0x1d, 0x76, 0x30, 0x00, 12, 0, 240, 0];
        assert!(
            host.written
                .windows(raster_header.len())
                .any(|window| window == raster_header)
        );
    }

    #[test]
    fn k_full_scale_is_ink() {
        // A `black_8` page at full scale is all ink, so all black.
        let mut job = job("M220");
        let mut host = FakeHost {
            options: gray_options(8, 1, PM_CSPACE_K),
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![255; 8]);
        assert_eq!(host.written.last(), Some(&0xff));
    }

    #[test]
    fn settings_reach_the_preamble() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: PrintOptions {
                print_darkness: 50,
                print_speed: 2 * INCH_PER_SECOND,
                ..gray_options(576, 1, PM_CSPACE_SW)
            },
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![255; 576]);
        let expected = Preamble {
            left_margin: LeftMargin::ZERO,
            density: Some(Density::MAX),
            speed: Speed::new(2),
            tracking: Some(MediaTracking::Continuous),
        }
        .encode();
        assert_eq!(host.written[..expected.len()], expected);
    }

    #[test]
    fn copies_are_left_to_pappl() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: gray_options(8, 1, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![0; 8]);
        let copies = [0x1f, 0x11, 0x21];
        let position = host
            .written
            .windows(3)
            .position(|window| window == copies)
            .expect("copies command");
        assert_eq!(host.written[position + 3], 1);
    }

    #[test]
    fn cancel_stops_the_page() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: gray_options(8, 2, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        job.start_page(&host).expect("page starts");
        job.write_line(&host, &[0; 8]).expect("line fits");
        host.canceled = true;
        assert!(matches!(
            job.write_line(&host, &[0; 8]),
            Err(Error::Canceled)
        ));
        // PAPPL still ends the page; nothing may be printed.
        assert!(matches!(job.end_page(&mut host), Err(Error::Canceled)));
        assert!(host.written.is_empty());
    }

    #[test]
    fn ending_the_job_discards_an_unfinished_page() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: gray_options(8, 2, PM_CSPACE_SW),
            ..FakeHost::default()
        };
        job.start_page(&host).expect("page starts");
        job.end(&mut host).expect("job ends");
        assert!(host.written.is_empty());
        assert_eq!(host.flushes, 1);
        assert_eq!(
            host.log.borrow().last().map(|(level, _)| *level),
            Some(LogLevel::Info)
        );
    }

    #[test]
    fn page_callbacks_need_a_started_page() {
        let mut job = job("M220");
        let mut host = FakeHost::default();
        assert!(job.line_len().is_none());
        assert!(matches!(job.write_line(&host, &[]), Err(Error::NoPage)));
        assert!(matches!(job.end_page(&mut host), Err(Error::NoPage)));
    }

    #[test]
    fn compression_is_used_when_it_helps() {
        let mut job = job("M220");
        let mut host = FakeHost {
            options: PrintOptions {
                phomemo_compression: None,
                ..gray_options(576, 64, PM_CSPACE_SW)
            },
            ..FakeHost::default()
        };
        print_page(&mut job, &mut host, |_| vec![255; 576]);
        let compression_on = [0x1f, 0x11, 0x35, 0x01];
        assert!(
            host.written
                .windows(4)
                .any(|window| window == compression_on)
        );
        assert_eq!(
            host.written[host.written.len() - 4..],
            [0x1f, 0x11, 0x35, 0x00]
        );
    }
}
