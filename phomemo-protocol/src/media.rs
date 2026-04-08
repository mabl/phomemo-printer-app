//! Media catalog and paper tracking definitions for Phomemo printers.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

/// Media tracking modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tracking {
    Continuous,
    Gap,
    Mark,
    Card,
}

impl Tracking {
    /// PAPPL tracking bit value.
    #[must_use]
    pub const fn pappl_flag(self) -> u32 {
        match self {
            Self::Continuous => 0x0001,
            Self::Gap | Self::Card => 0x0002,
            Self::Mark => 0x0004,
        }
    }
}

/// One normalized media preset.
#[derive(Debug, Clone)]
pub struct MediaPreset {
    pub size_name: String,
    pub width_mm: f32,
    pub length_mm: f32,
    pub tracking_default: Tracking,
    pub tracking_supported: Vec<Tracking>,
}

impl MediaPreset {
    /// Width in hundredths of millimeters.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn width_hundredths_mm(&self) -> i32 {
        (self.width_mm * 100.0).round() as i32
    }

    /// Length in hundredths of millimeters.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn length_hundredths_mm(&self) -> i32 {
        (self.length_mm * 100.0).round() as i32
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogRaw {
    paper_pools: Vec<PaperPoolRaw>,
    printer_types: Vec<PrinterTypeRaw>,
}

#[derive(Debug, Clone, Deserialize)]
struct PaperPoolRaw {
    pool: String,
    default_size_name: String,
    media: Vec<MediaPresetRaw>,
}

#[derive(Debug, Clone, Deserialize)]
struct MediaPresetRaw {
    size_name: String,
    width_mm: f32,
    length_mm: f32,
    tracking_default: Tracking,
    #[serde(default)]
    tracking_supported: Vec<Tracking>,
}

#[derive(Debug, Clone, Deserialize)]
struct PrinterTypeRaw {
    #[serde(rename = "type")]
    printer_type: String,
    pool: String,
}

struct RuntimeCatalog {
    pools: BTreeMap<String, PaperPoolRaw>,
    printer_type_to_pool: BTreeMap<String, String>,
}

impl RuntimeCatalog {
    fn load() -> Self {
        let raw: CatalogRaw = serde_json::from_str(include_str!("../data/media_catalog.json"))
            .unwrap_or_else(|err| {
                panic!("failed to parse media catalog JSON: {err}");
            });

        let mut pools = BTreeMap::new();
        for pool in raw.paper_pools {
            pools.insert(normalize_key(&pool.pool), pool);
        }

        let mut printer_type_to_pool = BTreeMap::new();
        for printer in raw.printer_types {
            printer_type_to_pool.insert(
                normalize_key(&printer.printer_type),
                normalize_key(&printer.pool),
            );
        }

        Self {
            pools,
            printer_type_to_pool,
        }
    }

    fn resolve_pool_key(&self, printer_type_or_series: &str) -> Option<String> {
        let key = normalize_key(printer_type_or_series);
        if let Some(pool) = self.printer_type_to_pool.get(&key) {
            return Some(pool.clone());
        }
        if self.pools.contains_key(&key) {
            return Some(key);
        }
        None
    }

    fn media_for(&self, printer_type_or_series: &str) -> Vec<MediaPreset> {
        let Some(pool_key) = self.resolve_pool_key(printer_type_or_series) else {
            return Vec::new();
        };
        let Some(pool) = self.pools.get(&pool_key) else {
            return Vec::new();
        };

        pool.media
            .iter()
            .map(|m| MediaPreset {
                size_name: m.size_name.clone(),
                width_mm: m.width_mm,
                length_mm: m.length_mm,
                tracking_default: m.tracking_default,
                tracking_supported: m.tracking_supported.clone(),
            })
            .collect()
    }

    fn default_media_for(&self, printer_type_or_series: &str) -> Option<MediaPreset> {
        let pool_key = self.resolve_pool_key(printer_type_or_series)?;
        let pool = self.pools.get(&pool_key)?;
        pool.media
            .iter()
            .find(|m| m.size_name == pool.default_size_name)
            .map(|m| MediaPreset {
                size_name: m.size_name.clone(),
                width_mm: m.width_mm,
                length_mm: m.length_mm,
                tracking_default: m.tracking_default,
                tracking_supported: m.tracking_supported.clone(),
            })
    }

    fn tracking_for_size(&self, printer_type_or_series: &str, size_name: &str) -> Option<Tracking> {
        let pool_key = self.resolve_pool_key(printer_type_or_series)?;
        let pool = self.pools.get(&pool_key)?;
        pool.media
            .iter()
            .find(|m| m.size_name.eq_ignore_ascii_case(size_name))
            .map(|m| m.tracking_default)
    }

    fn tracking_mask(&self, printer_type_or_series: &str) -> u32 {
        self.media_for(printer_type_or_series)
            .iter()
            .flat_map(|m| m.tracking_supported.iter().copied())
            .fold(0_u32, |acc, mode| acc | mode.pappl_flag())
    }
}

fn normalize_key(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn catalog() -> &'static RuntimeCatalog {
    static CATALOG: OnceLock<RuntimeCatalog> = OnceLock::new();
    CATALOG.get_or_init(RuntimeCatalog::load)
}

/// Lookup all media presets for a printer type (or canonical series/pool).
#[must_use]
pub fn media_for_printer_type(printer_type_or_series: &str) -> Vec<MediaPreset> {
    catalog().media_for(printer_type_or_series)
}

/// Lookup the default media preset for a printer type.
#[must_use]
pub fn default_media_for_printer_type(printer_type_or_series: &str) -> Option<MediaPreset> {
    catalog().default_media_for(printer_type_or_series)
}

/// Preferred tracking mode for a selected size name.
#[must_use]
pub fn tracking_for_size_name(printer_type_or_series: &str, size_name: &str) -> Option<Tracking> {
    catalog().tracking_for_size(printer_type_or_series, size_name)
}

/// Supported tracking bitmask for all media of a printer type.
#[must_use]
pub fn tracking_supported_mask(printer_type_or_series: &str) -> u32 {
    catalog().tracking_mask(printer_type_or_series)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m200_family_has_expected_default() {
        let default = default_media_for_printer_type("M220");
        assert!(default.is_some());
        let default = default.unwrap_or(MediaPreset {
            size_name: String::new(),
            width_mm: 0.0,
            length_mm: 0.0,
            tracking_default: Tracking::Gap,
            tracking_supported: Vec::new(),
        });

        assert_eq!(default.size_name, "om_40x30mm_40x30mm");
        assert_eq!(default.tracking_default, Tracking::Gap);
    }

    #[test]
    fn local_paper_coercion_data_is_present() {
        let media = media_for_printer_type("M110");
        assert!(!media.is_empty());
        assert!(media
            .iter()
            .any(|m| m.size_name == "om_20x20mm_20x20mm" && m.tracking_default == Tracking::Gap));
    }

    #[test]
    fn synthesized_continuous_roll_is_present() {
        let media = media_for_printer_type("M220");
        assert!(media.iter().any(|m| {
            m.size_name == "om_40x0mm_40x0mm"
                && m.length_mm == 0.0
                && m.tracking_default == Tracking::Continuous
        }));
    }

    #[test]
    fn tracking_lookup_works() {
        let tracking = tracking_for_size_name("M200", "om_40x30mm_40x30mm");
        assert_eq!(tracking, Some(Tracking::Gap));

        let unknown = tracking_for_size_name("M200", "om_999x999mm_999x999mm");
        assert!(unknown.is_none());
    }
}
