//! The model-specific capabilities `driver_cb` reports to PAPPL.
//!
//! `c/driver.c` copies a [`PmDriverDefaults`] into PAPPL's
//! `pappl_pr_driver_data_t` and adds what is the same for every model; the
//! callbacks stay in C because they take PAPPL's types.

use std::ffi::{c_char, c_int, c_ushort};
use std::ptr;

use crate::media;
use crate::models::{Model, PmModel};
use crate::pappl::{PM_MAX_MEDIA, tracking_flag};
use crate::raster::{DARKNESS_LEVELS, INCH_PER_SECOND, SPEED_MAX};

/// PAPPL's own default `printer-darkness-configured`
/// (`printer-driver.c`, `_papplPrinterInitDriverData`), which
/// [`PrintOptions::density`](crate::raster::PrintOptions::density) maps to
/// the middle density, 8.
const DARKNESS_CONFIGURED: c_int = 50;

/// What `driver_cb` reports for one model.
///
/// The strings belong to the model table and live as long as the process.
#[repr(C)]
#[derive(Debug)]
pub struct PmDriverDefaults {
    /// `make_and_model`, e.g. `Phomemo M220`.
    pub make_and_model: *const c_char,
    /// Resolution in dots per inch, both axes.
    pub dpi: c_int,
    /// Whether the model has a cutter.
    pub has_cutter: bool,
    /// `darkness_supported`: levels of `printer-darkness`.
    pub darkness_supported: c_int,
    /// `darkness_configured`: the printer's darkness, 0 to 100.
    pub darkness_configured: c_int,
    /// `darkness_default`: the job's default offset from it.
    pub darkness_default: c_int,
    /// `speed_supported`: `print-speed` range, hundredths of mm per second.
    pub speed_supported: [c_int; 2],
    /// `speed_default`: 0 leaves the speed to the printer.
    pub speed_default: c_int,
    /// `tracking_supported`: `pappl_media_tracking_t` bits.
    pub tracking_supported: c_ushort,
    /// Entries used in `media`.
    pub num_media: c_int,
    /// `media`: PWG names of the media sizes offered, then the bounds of
    /// the custom sizes ([`Model::custom_range_names`]).
    pub media: [*const c_char; PM_MAX_MEDIA],
    /// `media_default`.
    pub media_default: PmMediaDefault,
}

/// The fields of `media_default` that depend on the model.
#[repr(C)]
#[derive(Debug)]
pub struct PmMediaDefault {
    /// `size_name`, a PWG media name.
    pub size_name: *const c_char,
    /// `size_width` in hundredths of a millimetre.
    pub width: c_int,
    /// `size_length` in hundredths of a millimetre; 0 for a roll.
    pub length: c_int,
    /// `tracking`, a `pappl_media_tracking_t` bit.
    pub tracking: c_ushort,
    /// `type`, `labels` or `continuous`.
    pub media_type: *const c_char,
}

impl PmDriverDefaults {
    /// The defaults for `model`.
    #[must_use]
    pub fn new(model: &Model) -> Self {
        let mut media = [ptr::null(); PM_MAX_MEDIA];
        let mut num_media = 0;
        let names = model
            .media()
            .map(|(_, name)| name)
            .chain(model.custom_range_names());
        for (slot, name) in media.iter_mut().zip(names) {
            *slot = name.as_ptr();
            num_media += 1;
        }
        let (default, default_name) = model.default_media();
        let length = default.length_hundredths_mm();
        Self {
            make_and_model: model.make_and_model().as_ptr(),
            dpi: model.info().dpi.into(),
            has_cutter: model.info().has_cutter,
            darkness_supported: DARKNESS_LEVELS,
            darkness_configured: DARKNESS_CONFIGURED,
            darkness_default: 0,
            speed_supported: [INCH_PER_SECOND, SPEED_MAX],
            speed_default: 0,
            tracking_supported: model.tracking_supported(),
            num_media,
            media,
            media_default: PmMediaDefault {
                size_name: default_name.as_ptr(),
                width: default.width_hundredths_mm(),
                length,
                tracking: tracking_flag(default.tracking_default),
                media_type: media::media_type(length).as_ptr(),
            },
        }
    }
}

/// Fill `out` with the defaults for `model`.
///
/// Returns false, leaving `out` untouched, if `model` is not one of the
/// table's views (it is only compared against them, never dereferenced)
/// or `out` is NULL.
///
/// # Safety
///
/// `out` must be NULL or valid for writing a `PmDriverDefaults`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_driver_defaults(
    model: *const PmModel,
    out: *mut PmDriverDefaults,
) -> bool {
    let Some(model) = Model::from_view(model) else {
        return false;
    };
    if out.is_null() {
        return false;
    }
    // SAFETY: `out` is valid for writes; `write` does not read or drop
    // whatever was there.
    unsafe { out.write(PmDriverDefaults::new(model)) };
    true
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    use super::*;
    use crate::pappl::{
        PM_MEDIA_TRACKING_CONTINUOUS, PM_MEDIA_TRACKING_GAP, PM_MEDIA_TRACKING_MARK,
    };

    fn defaults(name: &str) -> PmDriverDefaults {
        PmDriverDefaults::new(Model::by_name(name).expect("known model"))
    }

    /// The string at `ptr`, which the model table owns.
    fn string(ptr: *const c_char) -> &'static str {
        // SAFETY: the defaults only point at strings the table owns.
        unsafe { CStr::from_ptr(ptr) }.to_str().expect("UTF-8")
    }

    #[test]
    fn m220_defaults() {
        let defaults = defaults("M220");
        assert_eq!(string(defaults.make_and_model), "Phomemo M220");
        assert_eq!(defaults.dpi, 203);
        assert!(!defaults.has_cutter);
        assert_eq!(
            (
                defaults.darkness_supported,
                defaults.darkness_configured,
                defaults.darkness_default
            ),
            (15, 50, 0)
        );
        assert_eq!(defaults.speed_supported, [2540, 6 * 2540]);
        assert_eq!(defaults.speed_default, 0);
        assert_eq!(
            defaults.tracking_supported,
            PM_MEDIA_TRACKING_CONTINUOUS | PM_MEDIA_TRACKING_GAP | PM_MEDIA_TRACKING_MARK
        );

        let media = defaults.media_default;
        assert_eq!(string(media.size_name), "om_40x30mm_40x30mm");
        assert_eq!((media.width, media.length), (4000, 3000));
        assert_eq!(media.tracking, PM_MEDIA_TRACKING_GAP);
        assert_eq!(string(media.media_type), "labels");
    }

    #[test]
    fn media_list_is_the_catalogs_and_the_custom_range() {
        for model in Model::all() {
            let defaults = PmDriverDefaults::new(model);
            let count = usize::try_from(defaults.num_media).expect("non-negative");
            let names: Vec<_> = defaults.media[..count]
                .iter()
                .map(|&name| string(name))
                .collect();
            let expected: Vec<_> = model
                .media()
                .map(|(preset, _)| preset.size_name.as_str())
                .chain(
                    model
                        .custom_range_names()
                        .map(|name| name.to_str().expect("UTF-8")),
                )
                .collect();
            assert_eq!(names, expected, "{}", model.name());
            assert!(names[count - 2].starts_with("roll_min_"));
            assert!(names[count - 1].starts_with("roll_max_"));
            assert!(defaults.media[count..].iter().all(|name| name.is_null()));
            assert!(names.contains(&string(defaults.media_default.size_name)));
        }
    }

    #[test]
    fn ffi_writes_the_defaults() {
        let model = Model::by_name("D30").expect("known model");
        let mut out = MaybeUninit::<PmDriverDefaults>::uninit();
        // SAFETY: `out` is valid for writes; a NULL model and output are
        // refused.
        unsafe {
            assert!(!pm_driver_defaults(ptr::null(), out.as_mut_ptr()));
            assert!(!pm_driver_defaults(model.view(), ptr::null_mut()));
            assert!(pm_driver_defaults(model.view(), out.as_mut_ptr()));
        }
        // SAFETY: `pm_driver_defaults` returned true, so it wrote `out`.
        let out = unsafe { out.assume_init() };
        assert_eq!(string(out.make_and_model), "Phomemo D30");
        assert_eq!(string(out.media_default.size_name), "om_30x15mm_30x15mm");
    }
}
