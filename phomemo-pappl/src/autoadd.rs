//! Which driver a discovered device gets: PAPPL's `autoadd_cb`.

mod ieee1284;

use std::ffi::{CStr, c_char};
use std::ptr;

use crate::models::Model;

/// The model for a device with IEEE 1284 `device_id` and description
/// `device_info`, in order of confidence:
///
/// 1. the device ID's `MDL` (or `MODEL`) is a model name;
/// 2. the device ID matches a model's ID, field by field
///    ([`ieee1284::match_score`]), which accepts a model among a
///    comma-separated `MDL` list;
/// 3. failing both, a model name is a whole word of `device_info`, such as
///    `M220` in `Phomemo M220` or `M220-A4B3` - so `Phomemo M200C` is an
///    M200C, not an M200, and an `M110C`, which this application does not
///    drive, is nothing. If several are, the longest wins.
///
/// The Bluetooth backend reports an ID without `MDL` when it cannot tell
/// the model, so the description is consulted whenever the ID names no
/// known model, not only when there is no ID.
#[must_use]
pub fn match_model(device_info: Option<&str>, device_id: Option<&str>) -> Option<&'static Model> {
    device_id
        .and_then(|id| by_model_field(id).or_else(|| by_device_id(id)))
        .or_else(|| device_info.and_then(by_description))
}

fn by_model_field(device_id: &str) -> Option<&'static Model> {
    ieee1284::extract_field(device_id, "MDL")
        .or_else(|| ieee1284::extract_field(device_id, "MODEL"))
        .and_then(Model::by_name)
}

fn by_device_id(device_id: &str) -> Option<&'static Model> {
    // `max_by_key` keeps the last of equal scores, so reversing the table
    // lets the first model in it win a tie.
    Model::all()
        .iter()
        .rev()
        .filter_map(|model| {
            let score = ieee1284::match_score(device_id, model.device_id().to_str().ok()?);
            (score > 0).then_some((model, score))
        })
        .max_by_key(|&(_, score)| score)
        .map(|(model, _)| model)
}

fn by_description(device_info: &str) -> Option<&'static Model> {
    device_info
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter_map(Model::by_name)
        .max_by_key(|model| model.name().len())
}

/// The PAPPL driver name for a device, or NULL if no model matches; see
/// [`match_model`]. The name lives as long as the process.
///
/// # Safety
///
/// `device_info` and `device_id` must each be NULL or point to a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_autoadd_match(
    device_info: *const c_char,
    device_id: *const c_char,
) -> *const c_char {
    // SAFETY: the caller passes NULL or NUL-terminated strings.
    let (device_info, device_id) = unsafe { (c_str(device_info), c_str(device_id)) };
    match_model(device_info, device_id).map_or(ptr::null(), |model| model.driver_name().as_ptr())
}

/// `string` as UTF-8, or `None` for NULL or anything else.
///
/// # Safety
///
/// `string` must be NULL or point to a NUL-terminated string that outlives
/// `'a`.
unsafe fn c_str<'a>(string: *const c_char) -> Option<&'a str> {
    if string.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { CStr::from_ptr(string) }.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(device_info: Option<&str>, device_id: Option<&str>) -> Option<&'static str> {
        match_model(device_info, device_id).map(Model::name)
    }

    #[test]
    fn model_field_matches_ignoring_case() {
        assert_eq!(
            matched(None, Some("MFG:Phomemo;MDL:m220;CMD:PHOMEMO;")),
            Some("M220")
        );
        assert_eq!(matched(None, Some("MFG:Phomemo;MODEL:D30;")), Some("D30"));
    }

    #[test]
    fn device_id_matches_a_model_in_a_list() {
        assert_eq!(
            matched(None, Some("MFG:Phomemo;MDL:M220,M220S;CMD:PHOMEMO,ESC;")),
            Some("M220")
        );
    }

    #[test]
    fn description_matches_whole_words() {
        assert_eq!(matched(Some("Phomemo M200C"), None), Some("M200C"));
        assert_eq!(matched(Some("Phomemo M200"), None), Some("M200"));
        assert_eq!(matched(Some("M220-A4B3"), None), Some("M220"));
        assert_eq!(matched(Some("phomemo d30"), None), Some("D30"));
        assert_eq!(matched(Some("Phomemo M110C"), None), None);
        assert_eq!(matched(Some("Phomemo M2200"), None), None);
        assert_eq!(matched(Some(""), None), None);
    }

    #[test]
    fn description_is_consulted_when_the_id_names_no_model() {
        let anonymous = Some("MFG:Phomemo;CMD:PHOMEMO;");
        assert_eq!(matched(Some("Phomemo M220"), anonymous), Some("M220"));
    }

    #[test]
    fn device_id_beats_description() {
        assert_eq!(
            matched(Some("Phomemo M220"), Some("MFG:Phomemo;MDL:D30;")),
            Some("D30")
        );
    }

    #[test]
    fn nothing_matches_nothing() {
        assert_eq!(matched(None, None), None);
        assert_eq!(
            matched(Some("HP LaserJet"), Some("MFG:HP;MDL:LaserJet;")),
            None
        );
    }

    #[test]
    fn ffi_returns_the_driver_name() {
        // SAFETY: static C strings and NULL.
        unsafe {
            let driver = pm_autoadd_match(c"Phomemo M200C".as_ptr(), ptr::null());
            assert_eq!(CStr::from_ptr(driver), c"phomemo_m200c");
            assert!(pm_autoadd_match(ptr::null(), ptr::null()).is_null());
        }
    }
}
