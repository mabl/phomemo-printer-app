//! IEEE 1284 device-ID parsing and scored matching.
//!
//! Device IDs are semicolon-delimited `KEY:VALUE` pairs, e.g.:
//!
//! ```text
//! MFG:Phomemo;MDL:M220;CMD:PHOMEMO;
//! ```
//!
//! Values may be comma-delimited lists (especially `CMD`).

/// Extract the value for `key` from an IEEE 1284 device-ID string.
///
/// Returns `None` if the key is not present.
pub fn extract_field<'a>(device_id: &'a str, key: &str) -> Option<&'a str> {
    for field in device_id.split(';') {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        if let Some((k, v)) = field.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Score a device's IEEE 1284 ID against a driver's device-ID template.
///
/// For each `KEY:VALUE` pair in `driver_id`, the corresponding field
/// in `device_id` is looked up:
///
/// - **Missing field in device** → score 0 (immediate, no match).
/// - **Exact case-insensitive match** → +2 points.
/// - **Comma-delimited partial match** (the driver value appears as a
///   complete token in the device's comma-separated list) → +1 point.
/// - **No match** → score 0 (immediate).
///
/// Returns the accumulated score, or 0 if any required field fails.
pub fn match_score(device_id: &str, driver_id: &str) -> u32 {
    let mut score: u32 = 0;

    for field in driver_id.split(';') {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }

        let Some((key, driver_val)) = field.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let driver_val = driver_val.trim();
        if driver_val.is_empty() {
            continue;
        }

        let Some(device_val) = extract_field(device_id, key) else {
            // Required field missing in device → no match.
            return 0;
        };

        if device_val.eq_ignore_ascii_case(driver_val) {
            // Full exact match.
            score += 2;
        } else if is_token_in_list(device_val, driver_val) {
            // Partial comma-delimited match.
            score += 1;
        } else {
            // No match on this field → disqualified.
            return 0;
        }
    }

    score
}

/// Check if `needle` appears as a complete comma-delimited token
/// within `haystack` (case-insensitive).
fn is_token_in_list(haystack: &str, needle: &str) -> bool {
    haystack
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_basic() {
        let id = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        assert_eq!(extract_field(id, "MFG"), Some("Phomemo"));
        assert_eq!(extract_field(id, "MDL"), Some("M220"));
        assert_eq!(extract_field(id, "CMD"), Some("PHOMEMO"));
        assert_eq!(extract_field(id, "SN"), None);
    }

    #[test]
    fn extract_case_insensitive_key() {
        let id = "mfg:Phomemo;mdl:M220;";
        assert_eq!(extract_field(id, "MFG"), Some("Phomemo"));
        assert_eq!(extract_field(id, "MDL"), Some("M220"));
    }

    #[test]
    fn extract_model_alias() {
        let id = "MFG:Phomemo;MODEL:M200;";
        assert_eq!(extract_field(id, "MODEL"), Some("M200"));
        assert_eq!(extract_field(id, "MDL"), None);
    }

    #[test]
    fn score_exact_match() {
        let device = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        let driver = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        assert_eq!(match_score(device, driver), 6); // 3 fields * 2
    }

    #[test]
    fn score_partial_cmd_match() {
        let device = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO,PCL;";
        let driver = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        // MFG=2, MDL=2, CMD=1 (partial)
        assert_eq!(match_score(device, driver), 5);
    }

    #[test]
    fn score_missing_field() {
        let device = "MFG:Phomemo;CMD:PHOMEMO;";
        let driver = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        // MDL missing in device → 0
        assert_eq!(match_score(device, driver), 0);
    }

    #[test]
    fn score_wrong_value() {
        let device = "MFG:Phomemo;MDL:M200;CMD:PHOMEMO;";
        let driver = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        // MDL mismatch → 0
        assert_eq!(match_score(device, driver), 0);
    }

    #[test]
    fn score_empty_driver() {
        assert_eq!(match_score("MFG:Phomemo;", ""), 0);
    }

    #[test]
    fn score_best_of_two_drivers() {
        let device = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        let m220 = "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;";
        let m200 = "MFG:Phomemo;MDL:M200;CMD:PHOMEMO;";
        assert!(match_score(device, m220) > match_score(device, m200));
    }
}
