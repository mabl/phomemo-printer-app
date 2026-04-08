//! Per-model capability database for the Phomemo printer family.

use std::sync::OnceLock;

/// Hardware capabilities for a specific printer model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Model name as printed on the device (e.g. "M220").
    pub name: &'static str,
    /// Model series (e.g. "M200").
    pub series: &'static str,
    /// Print resolution in DPI.
    pub dpi: u16,
    /// Maximum print width in pixels.
    pub max_width_px: u16,
    /// Maximum print width in bytes (`max_width_px` / 8).
    pub max_width_bytes: u16,
    /// Whether the firmware accepts LZO-compressed raster data.
    pub supports_compression: bool,
    /// Feed distance after print (protocol-level, in firmware units).
    pub feed_distance: u8,
    /// Number of ESC d feed lines between pages.
    pub feed_lines: u8,
    /// Whether the device has a cutter.
    pub has_cutter: bool,
}

#[derive(Debug, Clone, Copy)]
struct CapabilityProfile {
    dpi: u16,
    max_width_px: u16,
    supports_compression: bool,
    feed_distance: u8,
    feed_lines: u8,
    has_cutter: bool,
}

const PROFILE_M200_SERIES: CapabilityProfile = CapabilityProfile {
    dpi: 203,
    max_width_px: 576,
    supports_compression: true,
    feed_distance: 17,
    feed_lines: 2,
    has_cutter: false,
};

const PROFILE_48MM_SERIES: CapabilityProfile = CapabilityProfile {
    dpi: 203,
    max_width_px: 384,
    supports_compression: true,
    feed_distance: 17,
    feed_lines: 2,
    has_cutter: false,
};

const PROFILE_LARGE_FORMAT: CapabilityProfile = CapabilityProfile {
    dpi: 203,
    max_width_px: 640,
    supports_compression: true,
    feed_distance: 17,
    feed_lines: 2,
    has_cutter: false,
};

const PROFILE_POCKET: CapabilityProfile = CapabilityProfile {
    dpi: 203,
    max_width_px: 128,
    supports_compression: true,
    feed_distance: 17,
    feed_lines: 2,
    has_cutter: false,
};

const MODEL_SEEDS: &[(&str, &str, CapabilityProfile)] = &[
    // M200 series
    ("M200", "M200", PROFILE_M200_SERIES),
    ("M200C", "M200", PROFILE_M200_SERIES),
    ("M206", "M200", PROFILE_M200_SERIES),
    ("M208", "M200", PROFILE_M200_SERIES),
    ("M209", "M200", PROFILE_M200_SERIES),
    ("M219", "M200", PROFILE_M200_SERIES),
    ("M220", "M200", PROFILE_M200_SERIES),
    ("M220C", "M200", PROFILE_M200_SERIES),
    ("M220S", "M200", PROFILE_M200_SERIES),
    ("M221", "M200", PROFILE_M200_SERIES),
    // 48mm families
    ("M100", "M150", PROFILE_48MM_SERIES),
    ("M102", "M120", PROFILE_48MM_SERIES),
    ("M105", "M110", PROFILE_48MM_SERIES),
    ("M108", "M110", PROFILE_48MM_SERIES),
    ("M109", "M110", PROFILE_48MM_SERIES),
    ("M110", "M110", PROFILE_48MM_SERIES),
    ("M110C", "M110", PROFILE_48MM_SERIES),
    ("M110S", "M110", PROFILE_48MM_SERIES),
    ("M120", "M120", PROFILE_48MM_SERIES),
    ("M120C", "M120", PROFILE_48MM_SERIES),
    ("M126", "M120", PROFILE_48MM_SERIES),
    ("M150", "M150", PROFILE_48MM_SERIES),
    // D/Q label maker families
    ("A30", "D30", PROFILE_48MM_SERIES),
    ("D10", "D30", PROFILE_48MM_SERIES),
    ("D20", "D30", PROFILE_48MM_SERIES),
    ("D30", "D30", PROFILE_48MM_SERIES),
    ("D31", "D30", PROFILE_48MM_SERIES),
    ("D32", "D30", PROFILE_48MM_SERIES),
    ("D35", "D30", PROFILE_48MM_SERIES),
    ("D50", "D50", PROFILE_48MM_SERIES),
    ("DM170", "D30", PROFILE_48MM_SERIES),
    ("Q30", "Q30", PROFILE_48MM_SERIES),
    // Pocket models
    ("M02", "M02", PROFILE_POCKET),
    ("M03", "M03", PROFILE_POCKET),
    ("M04", "M04", PROFILE_POCKET),
    ("T02", "T02", PROFILE_POCKET),
    // Wide/desktop families
    ("D1600", "P3100", PROFILE_LARGE_FORMAT),
    ("D480", "D480", PROFILE_LARGE_FORMAT),
    ("D680", "P780", PROFILE_LARGE_FORMAT),
    ("E600S", "E600S", PROFILE_LARGE_FORMAT),
    ("E8000", "E600S", PROFILE_LARGE_FORMAT),
    ("E9000", "E9000", PROFILE_LARGE_FORMAT),
    ("G100", "G100", PROFILE_LARGE_FORMAT),
    ("LM1600", "P3100", PROFILE_LARGE_FORMAT),
    ("LT12", "P12", PROFILE_LARGE_FORMAT),
    ("M08F", "M200", PROFILE_LARGE_FORMAT),
    ("M831", "M831", PROFILE_LARGE_FORMAT),
    ("M832", "M831", PROFILE_LARGE_FORMAT),
    ("M833", "M831", PROFILE_LARGE_FORMAT),
    ("M834", "M831", PROFILE_LARGE_FORMAT),
    ("M835", "M831", PROFILE_LARGE_FORMAT),
    ("M950", "P3100", PROFILE_LARGE_FORMAT),
    ("M960", "P3100", PROFILE_LARGE_FORMAT),
    ("P1000", "P1000", PROFILE_LARGE_FORMAT),
    ("P12", "P12", PROFILE_LARGE_FORMAT),
    ("P3100", "P3100", PROFILE_LARGE_FORMAT),
    ("P3200", "P3100", PROFILE_LARGE_FORMAT),
    ("P780", "P780", PROFILE_LARGE_FORMAT),
    ("S821", "S821", PROFILE_LARGE_FORMAT),
    ("S823", "S823", PROFILE_LARGE_FORMAT),
    ("TK", "TK", PROFILE_LARGE_FORMAT),
];

fn build_models() -> Vec<ModelInfo> {
    let mut models = MODEL_SEEDS
        .iter()
        .map(|(name, series, profile)| ModelInfo {
            name,
            series,
            dpi: profile.dpi,
            max_width_px: profile.max_width_px,
            max_width_bytes: profile.max_width_px / 8,
            supports_compression: profile.supports_compression,
            feed_distance: profile.feed_distance,
            feed_lines: profile.feed_lines,
            has_cutter: profile.has_cutter,
        })
        .collect::<Vec<_>>();

    models.sort_by_key(|m| m.name);
    models.dedup_by(|left, right| left.name.eq_ignore_ascii_case(right.name));
    models
}

fn models() -> &'static [ModelInfo] {
    static MODELS: OnceLock<Vec<ModelInfo>> = OnceLock::new();
    MODELS.get_or_init(build_models)
}

/// Lookup a model by name (case-insensitive).
#[must_use]
pub fn lookup(name: &str) -> Option<&'static ModelInfo> {
    models()
        .iter()
        .find(|model| model.name.eq_ignore_ascii_case(name))
}

/// All known models, for enumeration.
#[must_use]
pub fn all_models() -> &'static [ModelInfo] {
    models()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("m220").is_some());
        assert!(lookup("M220").is_some());
    }

    #[test]
    fn known_model_catalog_contains_multiple_families() {
        assert!(lookup("M220").is_some());
        assert!(lookup("M110").is_some());
        assert!(lookup("D30").is_some());
        assert!(lookup("P3100").is_some());
    }

    #[test]
    fn all_models_are_unique_by_name() {
        let all = all_models();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert!(
                    !all[i].name.eq_ignore_ascii_case(all[j].name),
                    "duplicate model name in catalog: {}",
                    all[i].name
                );
            }
        }
    }
}
