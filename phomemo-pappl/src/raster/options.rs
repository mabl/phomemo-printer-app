//! The print options a page comes with, and what they mean for the printer.
//!
//! [`PrintOptions`] holds the fields of PAPPL's `pappl_pr_options_t` the
//! driver reads, as plain Rust values; its methods turn them into printer
//! settings. Values the driver does not understand are logged and replaced
//! by the default rather than failing the job.

use std::ffi::{c_int, c_uint, c_ushort};
use std::fmt;
use std::str::FromStr;

use phomemo_protocol::commands::{Density, Speed};
use phomemo_protocol::dither::Algorithm;
use phomemo_protocol::media::MediaTracking;

use super::host::Log;
use crate::media;
use crate::models::MediaSize;
use crate::overprint::{self, NamedMedia, VerticalPolicy};
use crate::pappl::{
    LogLevel, PM_COLOR_MODE_BI_LEVEL, PM_CONTENT_TEXT, PM_CONTENT_TEXT_AND_GRAPHIC,
};

/// Darkness levels offered to PAPPL (`darkness_supported`): one per
/// [`Density`].
///
/// cbindgen:ignore
pub const DARKNESS_LEVELS: c_int = 15;

/// One inch per second in `print-speed` units, hundredths of a millimetre
/// per second.
///
/// cbindgen:ignore
pub const INCH_PER_SECOND: c_int = 2540;

/// The fastest `print-speed` offered: one inch per second per [`Speed`]
/// level.
///
/// cbindgen:ignore
pub const SPEED_MAX: c_int = 6 * INCH_PER_SECOND;

/// The page geometry and pixel encoding, from the CUPS raster header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RasterHeader {
    /// `cupsWidth`: pixels per line.
    pub width: usize,
    /// `cupsHeight`: lines.
    pub height: usize,
    /// `cupsBytesPerLine`.
    pub bytes_per_line: usize,
    /// `cupsBitsPerPixel`.
    pub bits_per_pixel: c_uint,
    /// `cupsColorSpace`, a `cups_cspace_t`.
    pub color_space: c_uint,
}

/// What the driver reads once per job, when PAPPL starts it
/// (`rstartjob_cb`), and keeps for all of its pages: the loaded media and
/// the printer's defaults for vendor options (plan, D6 and D7).
///
/// The loaded media is `media_ready[0]` as it was when the job started; a
/// change on the Media Setup page while the job prints applies to the next
/// job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobContext {
    /// The loaded ("ready") media's PWG size name; empty if none.
    pub ready_name: String,
    /// The loaded media's size in hundredths of a millimetre; length 0 for
    /// a roll.
    pub ready_size: MediaSize,
    /// The loaded media's `media-tracking`, a `pappl_media_tracking_t` bit.
    pub ready_tracking: c_ushort,
    /// The printer's `phomemo-overprint-vertical-default`, if it has one.
    pub overprint_vertical_default: Option<String>,
}

impl JobContext {
    /// The loaded media.
    #[must_use]
    pub fn ready(&self) -> NamedMedia<'_> {
        NamedMedia {
            name: &self.ready_name,
            size: self.ready_size,
        }
    }

    /// The media tracking to print with on the loaded media; see
    /// [`media::job_tracking`].
    #[must_use]
    pub const fn ready_tracking(&self) -> Option<MediaTracking> {
        media::job_tracking(self.ready_tracking, self.ready_size.length)
    }
}

/// The print options the driver reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrintOptions {
    /// The page's raster header.
    pub raster: RasterHeader,
    /// The header's `cupsPageSize`, in points; 0 x 0 when it has none.
    pub cups_page_size: [f32; 2],
    /// The header's `PageSize`, in points.
    pub page_size: [c_uint; 2],
    /// The header's `HWResolution`, in dots per inch.
    pub resolution: [c_uint; 2],
    /// The job media's PWG size name; empty if none.
    pub media_name: String,
    /// The job media's width in hundredths of a millimetre.
    pub media_width: c_int,
    /// `print-darkness`: the job's offset from the printer's darkness,
    /// -100 to 100.
    pub print_darkness: c_int,
    /// `printer-darkness-configured`: the printer's darkness, 0 to 100.
    pub darkness_configured: c_int,
    /// `print-speed` in hundredths of a millimetre per second; 0 leaves the
    /// speed to the printer.
    pub print_speed: c_int,
    /// The media's `media-tracking`, a `pappl_media_tracking_t` bit.
    pub media_tracking: c_ushort,
    /// The media's length in hundredths of a millimetre; 0 for a roll.
    pub media_length: c_int,
    /// `print-color-mode`, a `pappl_color_mode_t` bit.
    pub color_mode: c_uint,
    /// `print-content-optimize`, a `pappl_content_t` bit.
    pub content_optimize: c_uint,
    /// The `phomemo-dither` vendor option, if set.
    pub phomemo_dither: Option<String>,
    /// The `phomemo-compression` vendor option, if set.
    pub phomemo_compression: Option<String>,
    /// The `phomemo-overprint-vertical` vendor option, if set.
    pub phomemo_overprint_vertical: Option<String>,
}

impl PrintOptions {
    /// The job's media.
    #[must_use]
    pub fn media(&self) -> NamedMedia<'_> {
        NamedMedia {
            name: &self.media_name,
            size: MediaSize {
                width: self.media_width,
                length: self.media_length,
            },
        }
    }

    /// The page's size in points, from its raster header; see
    /// [`overprint::page_points`].
    #[must_use]
    pub fn page_points(&self) -> Option<[f32; 2]> {
        let pixels = [self.raster.width, self.raster.height]
            .map(|pixels| c_uint::try_from(pixels).unwrap_or(c_uint::MAX));
        overprint::page_points(self.cups_page_size, self.page_size, pixels, self.resolution)
    }

    /// Which rows of an overprint canvas to send: the job's
    /// `phomemo-overprint-vertical`, else the printer's default for it
    /// (`printer_default`), else [`VerticalPolicy::Clip`]. An empty value
    /// counts as none; an unknown one is logged and taken as `clip`.
    ///
    /// The printer's default is passed in because PAPPL 1.4 looks vendor
    /// defaults up only in the job's own attributes (plan, D7).
    pub fn overprint_vertical(
        &self,
        printer_default: Option<&str>,
        log: &impl Log,
    ) -> VerticalPolicy {
        let value = [self.phomemo_overprint_vertical.as_deref(), printer_default]
            .into_iter()
            .flatten()
            .find(|value| !value.is_empty());
        vendor_option("phomemo-overprint-vertical", value, log).unwrap_or_default()
    }

    /// The density to print with.
    ///
    /// As in lprint, the job's `print-darkness` is an offset from the
    /// printer's `printer-darkness-configured`; their sum, clamped to
    /// 0-100 %, is scaled onto densities 1-15 with the rounding PAPPL's web
    /// interface uses to lay out its darkness choices, so its n-th choice
    /// selects density n (`printer-webif.c`, `_papplPrinterWebDefaults`).
    #[must_use]
    pub fn density(&self) -> Density {
        let percent = self
            .darkness_configured
            .saturating_add(self.print_darkness)
            .clamp(0, 100);
        let step = ((DARKNESS_LEVELS - 1) * percent + 50) / 100;
        u8::try_from(step + 1)
            .ok()
            .and_then(Density::new)
            .unwrap_or(Density::MAX)
    }

    /// The speed to print with, or `None` to keep the printer's.
    ///
    /// PAPPL offers `print-speed` in whole inches per second, as lprint's
    /// drivers do; the nearest one, from 1 to 6, is taken as that speed
    /// level. The levels' real speeds are not documented, so the inch
    /// labels are nominal. 0 - "Auto" in PAPPL's web interface, and what
    /// the vendor CUPS filter does by default - sends no speed at all.
    #[must_use]
    pub fn speed(&self) -> Option<Speed> {
        if self.print_speed <= 0 {
            return None;
        }
        let level = self.print_speed.saturating_add(INCH_PER_SECOND / 2) / INCH_PER_SECOND;
        let level = level.clamp(Speed::MIN.get().into(), Speed::MAX.get().into());
        u8::try_from(level).ok().and_then(Speed::new)
    }

    /// The media tracking to print with; see [`media::job_tracking`].
    #[must_use]
    pub const fn tracking(&self) -> Option<MediaTracking> {
        media::job_tracking(self.media_tracking, self.media_length)
    }

    /// The dithering algorithm for grayscale pages.
    ///
    /// `phomemo-dither` names one, or `auto` (the default) chooses:
    /// thresholding for `bi-level` color mode, which IPP defines as
    /// thresholded black and white, and for text; Floyd-Steinberg for
    /// everything else.
    pub fn dither(&self, log: &impl Log) -> Algorithm {
        let choice = vendor_option("phomemo-dither", self.phomemo_dither.as_deref(), log);
        match choice.unwrap_or_default() {
            DitherChoice::Algorithm(algorithm) => algorithm,
            DitherChoice::Auto
                if self.color_mode == PM_COLOR_MODE_BI_LEVEL
                    || matches!(
                        self.content_optimize,
                        PM_CONTENT_TEXT | PM_CONTENT_TEXT_AND_GRAPHIC
                    ) =>
            {
                Algorithm::Threshold
            }
            DitherChoice::Auto => Algorithm::FloydSteinberg,
        }
    }

    /// Whether to compress the raster, from `phomemo-compression`.
    pub fn compression(&self, log: &impl Log) -> Compression {
        vendor_option(
            "phomemo-compression",
            self.phomemo_compression.as_deref(),
            log,
        )
        .unwrap_or_default()
    }
}

/// Parse vendor option `name`'s `value`, logging one that does not parse.
fn vendor_option<T: FromStr>(name: &str, value: Option<&str>, log: &impl Log) -> Option<T> {
    let value = value?;
    let parsed = value.parse().ok();
    if parsed.is_none() {
        log.log(
            LogLevel::Warn,
            &format!("Ignoring unknown {name} value \"{value}\"."),
        );
    }
    parsed
}

/// A vendor option value that names nothing the driver knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownValue;

impl fmt::Display for UnknownValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown value")
    }
}

impl std::error::Error for UnknownValue {}

/// A `phomemo-dither` value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum DitherChoice {
    /// Choose from the job's color mode and content.
    #[default]
    Auto,
    /// Always this algorithm.
    Algorithm(Algorithm),
}

impl FromStr for DitherChoice {
    type Err = UnknownValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("auto") {
            Ok(Self::Auto)
        } else {
            s.parse().map(Self::Algorithm).map_err(|_| UnknownValue)
        }
    }
}

/// A `phomemo-compression` value: whether to LZO-compress the raster.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Compression {
    /// Compress when the model supports it and it makes the raster smaller.
    #[default]
    Auto,
    /// Compress whenever the model supports it.
    On,
    /// Never compress.
    Off,
}

impl FromStr for Compression {
    type Err = UnknownValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Self::Auto, Self::On, Self::Off]
            .into_iter()
            .find(|mode| mode.name().eq_ignore_ascii_case(s))
            .ok_or(UnknownValue)
    }
}

impl Compression {
    /// The option value.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::pappl::{PM_MEDIA_TRACKING_GAP, PM_MEDIA_TRACKING_MARK};

    /// Collects log messages.
    #[derive(Default)]
    struct Messages(RefCell<Vec<(LogLevel, String)>>);

    impl Log for Messages {
        fn log(&self, level: LogLevel, message: &str) {
            self.0.borrow_mut().push((level, message.to_owned()));
        }
    }

    fn darkness(configured: c_int, offset: c_int) -> u8 {
        PrintOptions {
            darkness_configured: configured,
            print_darkness: offset,
            ..PrintOptions::default()
        }
        .density()
        .get()
    }

    fn speed(print_speed: c_int) -> Option<u8> {
        PrintOptions {
            print_speed,
            ..PrintOptions::default()
        }
        .speed()
        .map(Speed::get)
    }

    #[test]
    fn darkness_levels_match_the_protocol() {
        assert_eq!(DARKNESS_LEVELS, c_int::from(Density::MAX.get()));
        assert_eq!(SPEED_MAX, c_int::from(Speed::MAX.get()) * INCH_PER_SECOND);
    }

    #[test]
    fn pappl_default_darkness_is_the_middle_density() {
        // PAPPL's default printer-darkness-configured, no job offset.
        assert_eq!(darkness(50, 0), 8);
    }

    #[test]
    fn darkness_spans_every_density() {
        assert_eq!(darkness(0, 0), 1);
        assert_eq!(darkness(100, 0), 15);
    }

    #[test]
    fn job_darkness_offsets_the_printer_darkness() {
        assert_eq!(darkness(50, 50), 15);
        assert_eq!(darkness(50, -50), 1);
        assert_eq!(darkness(50, 100), 15);
        assert_eq!(darkness(0, -100), 1);
        assert_eq!(darkness(c_int::MAX, c_int::MAX), 15);
    }

    #[test]
    fn every_web_interface_choice_selects_its_density() {
        // printer-webif.c offers `100 * i / (darkness_supported - 1)` percent.
        for choice in 0..DARKNESS_LEVELS {
            let percent = 100 * choice / (DARKNESS_LEVELS - 1);
            assert_eq!(c_int::from(darkness(percent, 0)), choice + 1, "{percent}%");
        }
    }

    #[test]
    fn speed_zero_keeps_the_printers() {
        assert_eq!(speed(0), None);
        assert_eq!(speed(-1), None);
    }

    #[test]
    fn speed_is_the_nearest_inch_per_second() {
        assert_eq!(speed(INCH_PER_SECOND), Some(1));
        assert_eq!(speed(3 * INCH_PER_SECOND), Some(3));
        assert_eq!(speed(SPEED_MAX), Some(6));
        assert_eq!(speed(2 * INCH_PER_SECOND - 100), Some(2));
        assert_eq!(speed(2 * INCH_PER_SECOND + 1000), Some(2));
    }

    #[test]
    fn speed_is_clamped_to_the_levels() {
        assert_eq!(speed(1), Some(1));
        assert_eq!(speed(10 * INCH_PER_SECOND), Some(6));
        assert_eq!(speed(c_int::MAX), Some(6));
    }

    #[test]
    fn tracking_follows_the_media() {
        let options = PrintOptions {
            media_tracking: PM_MEDIA_TRACKING_GAP,
            media_length: 3000,
            ..PrintOptions::default()
        };
        assert_eq!(options.tracking(), Some(MediaTracking::Gap));
        let roll = PrintOptions {
            media_length: 0,
            ..options
        };
        assert_eq!(roll.tracking(), Some(MediaTracking::Continuous));
        let marked_roll = PrintOptions {
            media_tracking: PM_MEDIA_TRACKING_MARK,
            ..roll
        };
        assert_eq!(marked_roll.tracking(), Some(MediaTracking::Mark));
    }

    fn dither(value: Option<&str>, color_mode: c_uint, content: c_uint) -> (Algorithm, Messages) {
        let messages = Messages::default();
        let options = PrintOptions {
            phomemo_dither: value.map(str::to_owned),
            color_mode,
            content_optimize: content,
            ..PrintOptions::default()
        };
        (options.dither(&messages), messages)
    }

    #[test]
    fn named_dither_wins() {
        let (algorithm, messages) = dither(Some("Atkinson"), 0, PM_CONTENT_TEXT);
        assert_eq!(algorithm, Algorithm::Atkinson);
        assert!(messages.0.borrow().is_empty());
    }

    #[test]
    fn auto_dither_thresholds_text_and_bi_level() {
        assert_eq!(dither(None, 0, PM_CONTENT_TEXT).0, Algorithm::Threshold);
        assert_eq!(
            dither(Some("auto"), 0, PM_CONTENT_TEXT_AND_GRAPHIC).0,
            Algorithm::Threshold
        );
        assert_eq!(
            dither(None, PM_COLOR_MODE_BI_LEVEL, 0).0,
            Algorithm::Threshold
        );
        assert_eq!(dither(None, 0, 0).0, Algorithm::FloydSteinberg);
    }

    #[test]
    fn unknown_dither_is_logged_and_treated_as_auto() {
        let (algorithm, messages) = dither(Some("sierra"), 0, 0);
        assert_eq!(algorithm, Algorithm::FloydSteinberg);
        assert_eq!(
            *messages.0.borrow(),
            [(
                LogLevel::Warn,
                "Ignoring unknown phomemo-dither value \"sierra\".".to_owned()
            )]
        );
    }

    fn compression(value: Option<&str>) -> (Compression, Messages) {
        let messages = Messages::default();
        let options = PrintOptions {
            phomemo_compression: value.map(str::to_owned),
            ..PrintOptions::default()
        };
        (options.compression(&messages), messages)
    }

    #[test]
    fn compression_values() {
        assert_eq!(compression(None).0, Compression::Auto);
        assert_eq!(compression(Some("auto")).0, Compression::Auto);
        assert_eq!(compression(Some("ON")).0, Compression::On);
        assert_eq!(compression(Some("off")).0, Compression::Off);
    }

    fn vertical(job: Option<&str>, printer: Option<&str>) -> (VerticalPolicy, Vec<LogLevel>) {
        let messages = Messages::default();
        let options = PrintOptions {
            phomemo_overprint_vertical: job.map(str::to_owned),
            ..PrintOptions::default()
        };
        let policy = options.overprint_vertical(printer, &messages);
        let levels = messages
            .0
            .borrow()
            .iter()
            .map(|(level, _)| *level)
            .collect();
        (policy, levels)
    }

    #[test]
    fn overprint_vertical_falls_back_in_order() {
        use VerticalPolicy::{Clip, Trailing};
        // The job's value, then the printer's default, then clip.
        assert_eq!(vertical(Some("trailing"), Some("clip")), (Trailing, vec![]));
        assert_eq!(vertical(Some("clip"), Some("trailing")), (Clip, vec![]));
        assert_eq!(vertical(None, Some("trailing")), (Trailing, vec![]));
        assert_eq!(vertical(None, Some("TRAILING")), (Trailing, vec![]));
        assert_eq!(vertical(None, None), (Clip, vec![]));
        // An empty value is none.
        assert_eq!(vertical(Some(""), Some("trailing")), (Trailing, vec![]));
        assert_eq!(vertical(Some(""), Some("")), (Clip, vec![]));
    }

    #[test]
    fn unknown_overprint_vertical_is_logged_and_treated_as_clip() {
        use VerticalPolicy::Clip;
        assert_eq!(
            vertical(Some("full"), Some("trailing")),
            (Clip, vec![LogLevel::Warn])
        );
        assert_eq!(vertical(None, Some("bogus")), (Clip, vec![LogLevel::Warn]));
        let messages = Messages::default();
        PrintOptions::default().overprint_vertical(Some("full"), &messages);
        assert_eq!(
            messages.0.borrow()[0].1,
            "Ignoring unknown phomemo-overprint-vertical value \"full\"."
        );
    }

    #[test]
    fn page_points_come_from_the_header() {
        let mut options = PrintOptions {
            raster: RasterHeader {
                width: 352,
                height: 272,
                ..RasterHeader::default()
            },
            ..PrintOptions::default()
        };
        // No resolution, no page size: unknown.
        assert_eq!(options.page_points(), None);
        options.resolution = [203, 203];
        let [width, height] = options.page_points().expect("pixels at 203 dpi");
        // 352 and 272 dots at 203 dpi: 124.85 x 96.47 points.
        assert!((width - 124.847).abs() < 0.001, "{width}");
        assert!((height - 96.473).abs() < 0.001, "{height}");
        options.page_size = [124, 96];
        assert_eq!(options.page_points(), Some([124.0, 96.0]));
        options.cups_page_size = [124.72, 96.38];
        assert_eq!(options.page_points(), Some([124.72, 96.38]));
    }

    #[test]
    fn job_and_ready_media() {
        let options = PrintOptions {
            media_name: "custom_44x34mm_44x34mm".to_owned(),
            media_width: 4400,
            media_length: 3400,
            ..PrintOptions::default()
        };
        assert_eq!(
            options.media(),
            NamedMedia {
                name: "custom_44x34mm_44x34mm",
                size: MediaSize {
                    width: 4400,
                    length: 3400
                },
            }
        );
        let context = JobContext {
            ready_name: "om_40x30mm_40x30mm".to_owned(),
            ready_size: MediaSize {
                width: 4000,
                length: 3000,
            },
            ready_tracking: PM_MEDIA_TRACKING_GAP,
            overprint_vertical_default: None,
        };
        assert_eq!(context.ready().name, "om_40x30mm_40x30mm");
        assert_eq!(context.ready_tracking(), Some(MediaTracking::Gap));
        // Tracked as a roll when a roll is loaded.
        let roll = JobContext {
            ready_size: MediaSize {
                width: 4000,
                length: 0,
            },
            ..context
        };
        assert_eq!(roll.ready_tracking(), Some(MediaTracking::Continuous));
    }

    #[test]
    fn unknown_compression_is_logged_and_treated_as_auto() {
        let (mode, messages) = compression(Some("max"));
        assert_eq!(mode, Compression::Auto);
        assert_eq!(messages.0.borrow().len(), 1);
        assert_eq!(messages.0.borrow()[0].0, LogLevel::Warn);
    }
}
