//! Per-model capabilities of the Phomemo printer family.
//!
//! A model is listed only if there is positive evidence that it speaks this
//! crate's command set (`ESC N` / `US DC1` settings, `ESC @`, `GS v 0`
//! raster), and its head width is the one the vendor code aligns rasters
//! against - so a margin computed from it always fits the one-byte
//! `LEFT_MARGIN`. Models whose vendor drivers use another command set
//! (`rastertoM08F`, `rastertoD480`, `ESC a` justification) or that the
//! vendor app drives without any alignment (the reverse-scanned P/D tape
//! printers, the E series, M110C, M120C, M221) are deliberately absent.
//!
//! Sources, cited per row (Print Master = `Print_Master_5.17.1` APK,
//! `com.project.aimotech.printer`; QY = vendor CUPS driver 1.8.0):
//!
//! - **PM-M200**: Print Master `M200Printer.printBitmapx` sends
//!   `LEFT_MARGIN(72 - width)` and `ESC @` (`MAX_PRINT_WIDTH = 72` bytes);
//!   subclasses inherit it. QY PPDs: `rastertolabelmxxx`, 203 dpi; phomemo-tools
//!   `rastertopm110.py` drives the M220 with `ESC N` and `US DC1` settings.
//!   The M220 was validated on hardware (`re/RE_RESULTS_BITMAP_GEOMETRY.md`).
//! - **PM-M120**: Print Master `M120Printer.fillWhitePaddingWithMSeries`
//!   sends `LEFT_MARGIN(48 - width)`; QY `M102.ppd`/`M120.ppd`:
//!   `rastertolabelmxxx`, 203 dpi.
//! - **PM-M110**: Print Master `M110Printer.fillWhitePaddingWithMSeries`
//!   right-aligns by padding rows to 48 bytes instead of sending a margin,
//!   so `LEFT_MARGIN` itself is *unverified* on these models; QY PPDs:
//!   `rastertolabelmxxx`, 203 dpi, clamped to 48 bytes.
//! - **PM-D30**: Print Master `D30Printer.printNormal` / `A30Printer.printNormal`
//!   send `LEFT_MARGIN(12 - width)` and `ESC @` (`MAX_PRINT_WIDTH = 12`
//!   bytes, a static that subclasses share); phomemo-tools `rastertopd30.py`
//!   sends `1F 11 24 00`, `ESC @`, `GS v 0`. Resolution from
//!   `getBitmapScaleSize()`: 1.0 (203 dpi), or 0.8866995 = 180/203 for
//!   `D50Printer`.

/// Hardware capabilities of one printer model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelInfo {
    /// Model name as printed on the device, e.g. `M220`.
    pub name: &'static str,
    /// Model series, e.g. `M200`; also a media pool name where one exists.
    pub series: &'static str,
    /// Print resolution in dots per inch, both axes.
    pub dpi: u16,
    /// Print-head width in dots.
    pub max_width_px: u16,
    /// Whether the firmware accepts LZO-compressed raster data.
    pub supports_compression: bool,
    /// Whether the device has a cutter.
    pub has_cutter: bool,
}

impl ModelInfo {
    /// Print-head width in bytes, as raster rows are packed.
    #[must_use]
    pub const fn max_width_bytes(&self) -> u16 {
        self.max_width_px.div_ceil(8)
    }
}

/// A row of the table.
///
/// `supports_compression` is carried over from earlier revisions: LZO is
/// verified only for the M200 series (Print Master `M220CPrinter`, and the
/// M220 hardware). No listed model has a cutter.
const fn model(name: &'static str, series: &'static str, dpi: u16, max_width_px: u16) -> ModelInfo {
    ModelInfo {
        name,
        series,
        dpi,
        max_width_px,
        supports_compression: true,
        has_cutter: false,
    }
}

#[rustfmt::skip]
static MODELS: [ModelInfo; 29] = [
    model("A30",   "D30",  203,  96), // PM-D30 (A30Printer)
    model("D10",   "D30",  203,  96), // PM-D30
    model("D20",   "D30",  203,  96), // PM-D30
    model("D30",   "D30",  203,  96), // PM-D30
    model("D31",   "D30",  203,  96), // PM-D30 (via Q30Printer)
    model("D32",   "D30",  203,  96), // PM-D30 (via Q30Printer)
    model("D35",   "D30",  203,  96), // PM-D30
    model("D50",   "D50",  180,  96), // PM-D30 (D50Printer: 180 dpi)
    model("DM170", "D30",  203,  96), // PM-D30 (via A30Printer)
    model("M100",  "M150", 203, 384), // PM-M110 (via M150Printer); QY M100.ppd
    model("M102",  "M120", 203, 384), // PM-M120; QY M102.ppd
    model("M105",  "M110", 203, 384), // PM-M110 (via M108Printer); QY M105.ppd
    model("M108",  "M110", 203, 384), // PM-M110; QY M108.ppd
    model("M109",  "M110", 203, 384), // PM-M110 (via M108Printer); QY M109.ppd
    model("M110",  "M110", 203, 384), // PM-M110; QY M110.ppd; phomemo-tools
    model("M110S", "M110", 203, 384), // PM-M110 (M110sPrinter); QY M110S.ppd
    model("M120",  "M120", 203, 384), // PM-M120; QY M120.ppd; phomemo-tools
    model("M126",  "M120", 203, 384), // PM-M120 (extends M120Printer)
    model("M150",  "M150", 203, 384), // PM-M110 (extends M110Printer); QY M150.ppd
    model("M200",  "M200", 203, 576), // PM-M200; QY M200.ppd
    model("M200C", "M200", 203, 576), // PM-M200 (M200CPrinter)
    model("M206",  "M200", 203, 576), // PM-M200 (M200Printer, SN Q017)
    model("M208",  "M200", 203, 576), // PM-M200; QY M208.ppd
    model("M209",  "M200", 203, 576), // PM-M200 (M209Printer); QY M209.ppd
    model("M219",  "M200", 203, 576), // PM-M200; QY M219.ppd
    model("M220",  "M200", 203, 576), // PM-M200; QY M220.ppd; validated on hardware
    model("M220C", "M200", 203, 576), // PM-M200 (M220CPrinter)
    model("M220S", "M200", 203, 576), // PM-M200 (M220SPrinter)
    model("Q30",   "Q30",  203,  96), // PM-D30 (Q30Printer)
];

/// Every known model, sorted by name.
#[must_use]
pub const fn all() -> &'static [ModelInfo] {
    &MODELS
}

/// The model called `name`, ignoring ASCII case.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static ModelInfo> {
    MODELS
        .iter()
        .find(|model| model.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::LeftMargin;

    #[test]
    fn table_is_sorted_and_unique_ignoring_case() {
        for (a, b) in MODELS.iter().zip(&MODELS[1..]) {
            assert!(
                a.name.to_ascii_uppercase() < b.name.to_ascii_uppercase(),
                "{} must sort strictly before {}",
                a.name,
                b.name
            );
        }
    }

    #[test]
    fn heads_are_plausible() {
        for model in all() {
            assert!(
                [180, 203].contains(&model.dpi),
                "{}: {} dpi",
                model.name,
                model.dpi
            );
            assert!(model.max_width_px > 0, "{}: no head width", model.name);
        }
    }

    #[test]
    fn every_margin_fits_in_a_byte() {
        // The widest margin is a whole head, for an empty image.
        for model in all() {
            let head = usize::from(model.max_width_bytes());
            assert!(LeftMargin::for_width(head, 0).is_ok(), "{}", model.name);
        }
    }

    #[test]
    fn lookup_ignores_case() {
        assert_eq!(lookup("m220"), lookup("M220"));
        assert_eq!(lookup("M220").map(|m| m.name), Some("M220"));
        assert_eq!(lookup("M9999"), None);
    }

    #[test]
    fn m220_matches_validated_hardware() {
        let m220 = lookup("M220").expect("known model");
        assert_eq!(
            (m220.dpi, m220.max_width_px, m220.max_width_bytes()),
            (203, 576, 72)
        );
        assert_eq!(m220.series, "M200");
    }

    #[test]
    fn d30_class_heads_are_twelve_bytes() {
        for name in ["D30", "D50", "Q30", "A30"] {
            assert_eq!(
                lookup(name).map(ModelInfo::max_width_bytes),
                Some(12),
                "{name}"
            );
        }
    }
}
