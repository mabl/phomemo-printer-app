//! Phomemo TX command encoding.
//!
//! Each function returns a `Vec<u8>` containing the wire-format command
//! bytes ready to be sent to the printer.  Two command dialects exist:
//!
//! - **Linux dialect** (`1B 4E` prefix): used by the vendor CUPS filter.
//! - **Mobile dialect** (`1F 11` prefix): used by companion mobile apps,
//!   with additional controls.
//!
//! This module provides both where applicable.

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// ESC @ — Reset the printer to default state.
#[must_use]
pub fn reset() -> Vec<u8> {
    vec![0x1b, 0x40]
}

// ---------------------------------------------------------------------------
// Configuration — Linux dialect (1B 4E)
// ---------------------------------------------------------------------------

/// Set print density (1..=15).  Linux dialect: `1B 4E 04 XX`.
#[must_use]
pub fn set_density(level: u8) -> Vec<u8> {
    let level = level.clamp(1, 15);
    vec![0x1b, 0x4e, 0x04, level]
}

/// Set print speed (1..=6).  Linux dialect: `1B 4E 0D XX`.
///
/// Per the vendor driver, PPD speed value 1 is remapped to 2.
#[must_use]
pub fn set_speed(level: u8) -> Vec<u8> {
    let level = level.clamp(1, 6);
    let level = if level == 1 { 2 } else { level };
    vec![0x1b, 0x4e, 0x0d, level]
}

// ---------------------------------------------------------------------------
// Configuration — mobile dialect (1F 11)
// ---------------------------------------------------------------------------

/// Set print density (mobile dialect): `1F 11 02 XX`.
#[must_use]
pub fn set_density_apk(level: u8) -> Vec<u8> {
    vec![0x1f, 0x11, 0x02, level]
}

/// Set speed / density coefficient (mobile dialect): `1F 11 37 XX`.
///
/// Values: 100 (0x64) = normal, 150 (0x96) = slower for higher quality.
#[must_use]
pub fn set_speed_coefficient(value: u8) -> Vec<u8> {
    vec![0x1f, 0x11, 0x37, value]
}

/// Set paper type (mobile dialect): `1F 11 00 XX`.
#[must_use]
pub fn set_paper_type(paper_type: PaperTypeCode) -> Vec<u8> {
    vec![0x1f, 0x11, 0x00, paper_type as u8]
}

/// Paper type protocol values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PaperTypeCode {
    /// Gap between labels.
    Gap = 0x0a,
    /// Continuous / default, no gap/mark detection.
    Continuous = 0x0b,
    /// Black mark (`BLine`) detection.
    Mark = 0x26,
    /// Named-format sheet mode (Card, A4, LTR, etc.).
    NamedFormat = 0x38,
    /// Card paper mode.
    Card = 0x4e,
}

/// Set media tracking mode.  Linux 3-byte variant.
///
/// - Continuous: `1F 11 0B`
/// - Gap:        `1F 11 0A`
/// - Black mark: `1F 11 26`
#[must_use]
pub fn set_media_tracking(tracking: MediaTracking) -> Vec<u8> {
    match tracking {
        MediaTracking::Continuous => vec![0x1f, 0x11, 0x0b],
        MediaTracking::Gap => vec![0x1f, 0x11, 0x0a],
        MediaTracking::Mark => vec![0x1f, 0x11, 0x26],
    }
}

/// Media tracking modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTracking {
    Continuous,
    Gap,
    Mark,
}

/// Enable or disable LZO compression mode.
///
/// - Enable:  `1F 11 35 01`
/// - Disable: `1F 11 35 00`
#[must_use]
pub fn set_compression(enabled: bool) -> Vec<u8> {
    vec![0x1f, 0x11, 0x35, u8::from(enabled)]
}

/// Set copy count: `1F 11 21 NN`.
#[must_use]
pub fn set_copies(count: u8) -> Vec<u8> {
    vec![0x1f, 0x11, 0x21, count.max(1)]
}

/// Set left margin: `1F 11 24 XX`.
///
/// For M220: margin = `MAX_PRINT_WIDTH(72)` - `bitmap_width_bytes`.
#[must_use]
pub fn set_left_margin(margin_bytes: u8) -> Vec<u8> {
    vec![0x1f, 0x11, 0x24, margin_bytes]
}

/// After-print feed: `1B 64 XX`.
#[must_use]
pub fn feed(amount: u8) -> Vec<u8> {
    vec![0x1b, 0x64, amount]
}

/// Heartbeat keep-alive: `1A 18 01`.
#[must_use]
pub fn heartbeat() -> Vec<u8> {
    vec![0x1a, 0x18, 0x01]
}

// ---------------------------------------------------------------------------
// Page control
// ---------------------------------------------------------------------------

/// End-of-page marker: `1F F0 05 00`.
#[must_use]
pub fn end_page() -> Vec<u8> {
    vec![0x1f, 0xf0, 0x05, 0x00]
}

/// End-of-document marker: `1F F0 03 00`.
#[must_use]
pub fn end_document() -> Vec<u8> {
    vec![0x1f, 0xf0, 0x03, 0x00]
}

// ---------------------------------------------------------------------------
// Bitmap headers
// ---------------------------------------------------------------------------

/// Monochrome bitmap header: `1D 76 30 00 WL WH HL HH`.
///
/// `width_bytes` is the row width in bytes (`pixel_width` / 8).
/// `height` is the number of raster rows.
/// Both are little-endian 16-bit.
#[must_use]
pub fn bitmap_header_mono(width_bytes: u16, height: u16) -> Vec<u8> {
    vec![
        0x1d,
        0x76,
        0x30,
        0x00,
        (width_bytes & 0xff) as u8,
        (width_bytes >> 8) as u8,
        (height & 0xff) as u8,
        (height >> 8) as u8,
    ]
}

/// Grayscale (16-level, 4-bit) bitmap header: `1D BB 30 WL WH HL HH`.
///
/// **Note:** This header is 7 bytes (no 0x00 after 0x30), unlike the
/// mono header which is 8 bytes.
#[must_use]
pub fn bitmap_header_grayscale(width_bytes: u16, height: u16) -> Vec<u8> {
    vec![
        0x1d,
        0xbb,
        0x30,
        (width_bytes & 0xff) as u8,
        (width_bytes >> 8) as u8,
        (height & 0xff) as u8,
        (height >> 8) as u8,
    ]
}

// ---------------------------------------------------------------------------
// Status queries (3-byte: 1F 11 XX)
// ---------------------------------------------------------------------------

/// Query cover status: `1F 11 12`.  Response: `1A 05 XX`.
#[must_use]
pub fn query_cover() -> Vec<u8> {
    vec![0x1f, 0x11, 0x12]
}

/// Query paper status: `1F 11 11`.  Response: `1A 06 XX`.
#[must_use]
pub fn query_paper() -> Vec<u8> {
    vec![0x1f, 0x11, 0x11]
}

/// Query firmware version: `1F 11 07`.  Response: `1A 07 v1 v2 v3`.
#[must_use]
pub fn query_firmware_version() -> Vec<u8> {
    vec![0x1f, 0x11, 0x07]
}

/// Query serial number: `1F 11 08`.  Response: `1A 08` + 15 ASCII bytes.
#[must_use]
pub fn query_serial_number() -> Vec<u8> {
    vec![0x1f, 0x11, 0x08]
}

/// Query auto-off timer: `1F 11 09`.  Response: `1A 09 XX` (XX*5 = minutes).
#[must_use]
pub fn query_auto_off() -> Vec<u8> {
    vec![0x1f, 0x11, 0x09]
}

/// Query overheat status: `1F 11 13`.  Response: `1A 03 XX`.
#[must_use]
pub fn query_overheat() -> Vec<u8> {
    vec![0x1f, 0x11, 0x13]
}

/// Query chip info: `1F 11 63`.  Response: `1A 17 XX`.
#[must_use]
pub fn query_chip_info() -> Vec<u8> {
    vec![0x1f, 0x11, 0x63]
}

/// Query capabilities/features: `1F 11 38`.  Response: `1A 3B D0..D4`.
#[must_use]
pub fn query_capabilities() -> Vec<u8> {
    vec![0x1f, 0x11, 0x38]
}

/// Print test page (built-in): `1F 11 27`.
#[must_use]
pub fn print_test_page() -> Vec<u8> {
    vec![0x1f, 0x11, 0x27]
}

// ---------------------------------------------------------------------------
// Print sequence builder
// ---------------------------------------------------------------------------

/// Configuration for a single print page.
#[derive(Debug, Clone)]
pub struct PageConfig {
    /// Print density 1-15.  0 = use printer default.
    pub density: u8,
    /// Print speed 1-6.  0 = use printer default.
    pub speed: u8,
    /// Media tracking mode.  `None` = use printer default.
    pub tracking: Option<MediaTracking>,
    /// Whether LZO compression is enabled.
    pub compression: bool,
    /// Number of copies.
    pub copies: u8,
}

impl Default for PageConfig {
    fn default() -> Self {
        Self {
            density: 0,
            speed: 0,
            tracking: None,
            compression: false,
            copies: 1,
        }
    }
}

/// Build the complete byte sequence for a print job following the Linux
/// CUPS driver pattern:
///
/// 1. Media tracking (if not default)
/// 2. Density (if not default)
/// 3. Speed (if not default)
/// 4. Bitmap header + pixel data
/// 5. End page
///
/// The caller is responsible for sending end-document after the last page.
#[must_use]
pub fn build_page(config: &PageConfig, width_bytes: u16, raster_data: &[u8]) -> Vec<u8> {
    let height = if width_bytes > 0 {
        let rows = raster_data.len() / usize::from(width_bytes);
        u16::try_from(rows).unwrap_or(u16::MAX)
    } else {
        0
    };

    let mut out = Vec::with_capacity(32 + raster_data.len());

    // 1. Media tracking
    if let Some(tracking) = config.tracking {
        out.extend_from_slice(&set_media_tracking(tracking));
    }

    // 2. Density
    if config.density > 0 {
        out.extend_from_slice(&set_density(config.density));
    }

    // 3. Speed
    if config.speed > 0 {
        out.extend_from_slice(&set_speed(config.speed));
    }

    // 4. Bitmap header + pixel data
    out.extend_from_slice(&bitmap_header_mono(width_bytes, height));
    out.extend_from_slice(raster_data);

    // 5. End page
    out.extend_from_slice(&end_page());

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reset() {
        assert_eq!(reset(), vec![0x1b, 0x40]);
    }

    #[test]
    fn test_density_clamping() {
        assert_eq!(set_density(0)[3], 1);
        assert_eq!(set_density(8)[3], 8);
        assert_eq!(set_density(20)[3], 15);
    }

    #[test]
    fn test_speed_level_1_remaps_to_2() {
        assert_eq!(set_speed(1)[3], 2);
        assert_eq!(set_speed(0)[3], 2);
    }

    #[test]
    fn test_bitmap_header_endianness() {
        let hdr = bitmap_header_mono(72, 100);
        assert_eq!(hdr, vec![0x1d, 0x76, 0x30, 0x00, 72, 0, 100, 0]);
    }

    #[test]
    fn test_bitmap_header_large_height() {
        let hdr = bitmap_header_mono(72, 1000);
        assert_eq!(hdr[6], 0xe8);
        assert_eq!(hdr[7], 0x03);
    }

    #[test]
    fn test_grayscale_header_is_7_bytes() {
        let hdr = bitmap_header_grayscale(36, 100);
        assert_eq!(hdr.len(), 7);
        assert_eq!(hdr[0..3], [0x1d, 0xbb, 0x30]);
    }

    #[test]
    fn test_media_tracking_bytes() {
        assert_eq!(
            set_media_tracking(MediaTracking::Continuous),
            vec![0x1f, 0x11, 0x0b]
        );
        assert_eq!(
            set_media_tracking(MediaTracking::Gap),
            vec![0x1f, 0x11, 0x0a]
        );
        assert_eq!(
            set_media_tracking(MediaTracking::Mark),
            vec![0x1f, 0x11, 0x26]
        );
    }

    #[test]
    fn test_compression_toggle() {
        assert_eq!(set_compression(true), vec![0x1f, 0x11, 0x35, 0x01]);
        assert_eq!(set_compression(false), vec![0x1f, 0x11, 0x35, 0x00]);
    }

    #[test]
    fn test_build_page_linux_sequence() {
        // Simulate the Linux CUPS driver sequence for a 2-row, 72-byte-wide page
        let raster = vec![0xff; 72 * 2]; // 2 rows of all-black
        let config = PageConfig {
            density: 8,
            speed: 3,
            tracking: Some(MediaTracking::Gap),
            ..PageConfig::default()
        };

        let out = build_page(&config, 72, &raster);

        // Should contain: tracking + density + speed + bitmap header + data + end_page
        assert!(out.starts_with(&[0x1f, 0x11, 0x0a])); // gap tracking
        assert!(out.ends_with(&[0x1f, 0xf0, 0x05, 0x00])); // end page

        // Bitmap header should be present with width=72, height=2
        let bmp_hdr = bitmap_header_mono(72, 2);
        assert!(out.windows(bmp_hdr.len()).any(|w| w == bmp_hdr));
    }

    #[test]
    fn test_build_page_defaults_no_config() {
        let raster = vec![0x00; 72];
        let config = PageConfig::default();
        let out = build_page(&config, 72, &raster);

        // No tracking/density/speed commands — should start with bitmap header
        assert!(out.starts_with(&[0x1d, 0x76, 0x30, 0x00]));
    }

    #[test]
    fn test_paper_type_code_values() {
        assert_eq!(PaperTypeCode::Gap as u8, 0x0a);
        assert_eq!(PaperTypeCode::Continuous as u8, 0x0b);
        assert_eq!(PaperTypeCode::Mark as u8, 0x26);
    }

    #[test]
    fn test_query_commands_are_3_bytes() {
        assert_eq!(query_cover().len(), 3);
        assert_eq!(query_paper().len(), 3);
        assert_eq!(query_firmware_version().len(), 3);
        assert_eq!(query_serial_number().len(), 3);
        assert_eq!(query_auto_off().len(), 3);
        assert_eq!(query_overheat().len(), 3);
        assert_eq!(query_chip_info().len(), 3);
        assert_eq!(query_capabilities().len(), 3);
    }
}
