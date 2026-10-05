//! What PAPPL's media description means for the printer.
//!
//! PAPPL describes a continuous roll as media 0 mm long. The rules that
//! follow from that live here, once, for the raster driver and for the C
//! media page alike.

use std::ffi::{CStr, c_char, c_int, c_ushort};

use phomemo_protocol::media::MediaTracking;

use crate::models::{Model, PmModel};
use crate::pappl::{PM_MEDIA_TRACKING_MARK, tracking_flag, tracking_from_flag};

/// Whether media `length` hundredths of a millimetre long is a continuous
/// roll rather than labels.
#[must_use]
pub const fn is_roll(length: c_int) -> bool {
    length == 0
}

/// PAPPL's `media-type` keyword for media `length` long.
#[must_use]
pub const fn media_type(length: c_int) -> &'static CStr {
    if is_roll(length) {
        c"continuous"
    } else {
        c"labels"
    }
}

/// The tracking to send for a job on media `length` long whose
/// `media-tracking` is `flag`.
///
/// The web interface can leave gap tracking selected when a continuous roll
/// is loaded, so a roll is tracked as continuous unless black-mark tracking
/// was chosen explicitly. `None` means the job named no single mode the
/// printer knows, and the printer's own setting stands.
#[must_use]
pub const fn job_tracking(flag: c_ushort, length: c_int) -> Option<MediaTracking> {
    if is_roll(length) && flag != PM_MEDIA_TRACKING_MARK {
        Some(MediaTracking::Continuous)
    } else {
        tracking_from_flag(flag)
    }
}

/// The tracking to preselect for media `size_name`, `length` long, loaded
/// in `model`: the catalog's default for that size, or for a size the
/// catalog lacks, continuous for a roll and gap for labels.
#[must_use]
pub fn preferred_tracking(model: Option<&Model>, size_name: &str, length: c_int) -> MediaTracking {
    match model.and_then(|model| model.find_media(size_name)) {
        Some(preset) => preset.tracking_default,
        None if is_roll(length) => MediaTracking::Continuous,
        None => MediaTracking::Gap,
    }
}

/// The `pappl_media_tracking_t` bit to preselect for media `size_name`,
/// `length` hundredths of a millimetre long, loaded in `model`; see
/// [`preferred_tracking`]. Never 0.
///
/// `model` is only compared against the table's views, never dereferenced,
/// so NULL or an unknown pointer just means "no catalog".
///
/// # Safety
///
/// `size_name` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_media_tracking(
    model: *const PmModel,
    size_name: *const c_char,
    length: c_int,
) -> c_ushort {
    let size_name = if size_name.is_null() {
        c""
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        unsafe { CStr::from_ptr(size_name) }
    };
    let tracking = preferred_tracking(
        Model::from_view(model),
        &size_name.to_string_lossy(),
        length,
    );
    tracking_flag(tracking)
}

/// PAPPL's `media-type` keyword for media `length` hundredths of a
/// millimetre long; see [`media_type`]. The string is static.
#[unsafe(no_mangle)]
pub const extern "C" fn pm_media_type(length: c_int) -> *const c_char {
    media_type(length).as_ptr()
}

/// Whether `model` takes media `width` x `length` hundredths of a
/// millimetre; see [`Model::accepts_media`]. Any positive width is taken
/// when `model` is not one of the table's views, which are the only
/// pointers it is compared against, never dereferenced.
#[unsafe(no_mangle)]
pub extern "C" fn pm_media_fits(model: *const PmModel, width: c_int, length: c_int) -> bool {
    Model::from_view(model).map_or(width > 0, |model| model.accepts_media(width, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pappl::{PM_MEDIA_TRACKING_CONTINUOUS, PM_MEDIA_TRACKING_GAP};

    fn m220() -> &'static Model {
        Model::by_name("M220").expect("known model")
    }

    #[test]
    fn rolls_are_continuous_unless_mark_tracked() {
        assert_eq!(
            job_tracking(PM_MEDIA_TRACKING_GAP, 0),
            Some(MediaTracking::Continuous)
        );
        assert_eq!(job_tracking(0, 0), Some(MediaTracking::Continuous));
        assert_eq!(
            job_tracking(PM_MEDIA_TRACKING_MARK, 0),
            Some(MediaTracking::Mark)
        );
    }

    #[test]
    fn labels_keep_the_jobs_tracking() {
        assert_eq!(
            job_tracking(PM_MEDIA_TRACKING_GAP, 3000),
            Some(MediaTracking::Gap)
        );
        assert_eq!(
            job_tracking(PM_MEDIA_TRACKING_CONTINUOUS, 3000),
            Some(MediaTracking::Continuous)
        );
        assert_eq!(job_tracking(0, 3000), None);
    }

    #[test]
    fn catalog_sizes_use_their_default_tracking() {
        assert_eq!(
            preferred_tracking(Some(m220()), "om_40x30mm_40x30mm", 3000),
            MediaTracking::Gap
        );
        assert_eq!(
            preferred_tracking(Some(m220()), "om_40x0mm_40x0mm", 0),
            MediaTracking::Continuous
        );
    }

    #[test]
    fn other_sizes_are_tracked_by_length() {
        let custom = "custom_40x55mm_40x55mm";
        assert_eq!(
            preferred_tracking(Some(m220()), custom, 5500),
            MediaTracking::Gap
        );
        assert_eq!(
            preferred_tracking(None, custom, 0),
            MediaTracking::Continuous
        );
    }

    #[test]
    fn ffi_media_fits() {
        assert!(pm_media_fits(m220().view(), 7000, 3000));
        assert!(!pm_media_fits(m220().view(), 8000, 3000));
        assert!(pm_media_fits(m220().view(), 4000, 819_900));
        assert!(!pm_media_fits(m220().view(), 4000, 820_000));
        assert!(pm_media_fits(std::ptr::null(), 8000, 3000));
        assert!(!pm_media_fits(std::ptr::null(), 0, 3000));
    }

    #[test]
    fn media_types() {
        assert_eq!(media_type(0), c"continuous");
        assert_eq!(media_type(3000), c"labels");
        // SAFETY: pm_media_type returns a static C string.
        assert_eq!(unsafe { CStr::from_ptr(pm_media_type(0)) }, c"continuous");
    }

    #[test]
    fn ffi_never_answers_zero() {
        // SAFETY: a view from the table, NULL, and static C strings.
        unsafe {
            assert_eq!(
                pm_media_tracking(m220().view(), c"om_40x30mm_40x30mm".as_ptr(), 3000),
                PM_MEDIA_TRACKING_GAP
            );
            assert_eq!(
                pm_media_tracking(std::ptr::null(), std::ptr::null(), 0),
                PM_MEDIA_TRACKING_CONTINUOUS
            );
        }
    }
}
