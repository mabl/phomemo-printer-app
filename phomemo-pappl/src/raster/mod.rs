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

pub use self::host::{Host, Log};
pub use self::options::{DARKNESS_LEVELS, INCH_PER_SECOND, PrintOptions, RasterHeader, SPEED_MAX};
pub use self::page::MAX_ROWS;
use self::page::Page;
use crate::models::Model;
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

/// The driver's state for one job.
#[derive(Debug)]
pub struct Job {
    model: &'static Model,
    page: Option<Page>,
    sent: Sent,
    settle: Duration,
}

impl Job {
    /// A job printing on `model`.
    #[must_use]
    pub const fn new(model: &'static Model) -> Self {
        Self {
            model,
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
    /// # Errors
    ///
    /// Fails if the page is one the driver cannot print; see [`Page::new`].
    pub fn start_page(&mut self, host: &impl Host) -> Result<(), Error> {
        self.page = Some(Page::new(
            &host.options().raster,
            self.model.head_width_px(),
            self.model.has_sideways_media(),
            host,
        )?);
        Ok(())
    }

    /// What has been sent to the printer so far.
    #[must_use]
    pub const fn sent(&self) -> Sent {
        self.sent
    }

    /// Bytes in each line of the current page, if one has been started.
    #[must_use]
    pub fn line_len(&self) -> Option<usize> {
        self.page.as_ref().map(Page::line_len)
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
        self.page.as_mut().ok_or(Error::NoPage)?.push_line(line)
    }

    /// Finish the page and send it.
    ///
    /// # Errors
    ///
    /// Fails if no page was started, the page cannot be encoded, or the
    /// device does not take it.
    pub fn end_page(&mut self, host: &mut impl Host) -> Result<(), Error> {
        let page = self.page.take().ok_or(Error::NoPage)?;
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
            tracking: options.tracking(),
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
        if let Some(page) = self.page {
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
    use phomemo_protocol::media::MediaTracking;

    use super::*;
    use crate::pappl::{PM_CSPACE_K, PM_CSPACE_SW, PM_MEDIA_TRACKING_GAP};

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
        Job {
            settle: Duration::ZERO,
            ..Job::new(model(model_name))
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

    #[test]
    fn m220_label_byte_stream() {
        // A 40 x 30 mm gap label (320 x 240 dots) at the default darkness,
        // black on the left half of each line.
        let mut host = FakeHost {
            options: PrintOptions {
                media_tracking: PM_MEDIA_TRACKING_GAP,
                media_length: 3000,
                ..gray_options(320, 240, PM_CSPACE_SW)
            },
            ..FakeHost::default()
        };
        let mut job = job("M220");
        print_page(&mut job, &mut host, |_| {
            let mut line = vec![0; 160];
            line.resize(320, 255);
            line
        });
        job.end(&mut host).expect("job ends");

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
        assert_eq!(host.written, expected);
        // Preamble, raster, end of job.
        assert_eq!(host.flushes, 3);
        assert!(host.warnings().is_empty(), "{:?}", host.warnings());
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
