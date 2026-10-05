//! The PAPPL and CUPS definitions this crate shares with C.
//!
//! Rust cannot read C headers without bindgen, so the handful of PAPPL and
//! CUPS enumeration values the driver needs are copied here, each under its
//! C name with `PM_` in place of `PAPPL_` or `CUPS_`. cbindgen exports them,
//! and `c/driver.c` checks every one against the real header with
//! `_Static_assert`: a copy that drifts from PAPPL fails the C build instead
//! of misbehaving at run time.
//!
//! The PAPPL types the driver only ever handles by pointer are declared here
//! as opaque stand-ins, which cbindgen exports under PAPPL's own names
//! (`phomemo-pappl/cbindgen.toml`), so the C side passes its pointers without
//! casts.

use std::ffi::{c_int, c_uint};
use std::marker::{PhantomData, PhantomPinned};

use phomemo_protocol::media::MediaTracking;

/// `PAPPL_MAX_MEDIA`: the most media sizes a driver can list.
pub const PM_MAX_MEDIA: usize = 256;

/// `PAPPL_LOGLEVEL_DEBUG`.
pub const PM_LOGLEVEL_DEBUG: c_int = 0;
/// `PAPPL_LOGLEVEL_INFO`.
pub const PM_LOGLEVEL_INFO: c_int = 1;
/// `PAPPL_LOGLEVEL_WARN`.
pub const PM_LOGLEVEL_WARN: c_int = 2;
/// `PAPPL_LOGLEVEL_ERROR`.
pub const PM_LOGLEVEL_ERROR: c_int = 3;

/// `PAPPL_MEDIA_TRACKING_CONTINUOUS`.
pub const PM_MEDIA_TRACKING_CONTINUOUS: c_uint = 0x0001;
/// `PAPPL_MEDIA_TRACKING_GAP`.
pub const PM_MEDIA_TRACKING_GAP: c_uint = 0x0002;
/// `PAPPL_MEDIA_TRACKING_MARK`.
pub const PM_MEDIA_TRACKING_MARK: c_uint = 0x0004;

/// `PAPPL_PREASON_OTHER`.
pub const PM_PREASON_OTHER: c_uint = 0x0001;
/// `PAPPL_PREASON_COVER_OPEN`.
pub const PM_PREASON_COVER_OPEN: c_uint = 0x0002;
/// `PAPPL_PREASON_MARKER_SUPPLY_LOW`.
pub const PM_PREASON_MARKER_SUPPLY_LOW: c_uint = 0x0010;
/// `PAPPL_PREASON_MEDIA_EMPTY`.
pub const PM_PREASON_MEDIA_EMPTY: c_uint = 0x0080;
/// `PAPPL_PREASON_OFFLINE`.
pub const PM_PREASON_OFFLINE: c_uint = 0x0800;

/// `PAPPL_COLOR_MODE_BI_LEVEL`: black and white, thresholded.
pub const PM_COLOR_MODE_BI_LEVEL: c_uint = 0x0004;

/// `PAPPL_CONTENT_TEXT`.
pub const PM_CONTENT_TEXT: c_uint = 0x08;
/// `PAPPL_CONTENT_TEXT_AND_GRAPHIC`.
pub const PM_CONTENT_TEXT_AND_GRAPHIC: c_uint = 0x10;

/// `CUPS_CSPACE_W`: luminance, 0 is black.
pub const PM_CSPACE_W: c_uint = 0;
/// `CUPS_CSPACE_K`: black ink, 0 is no ink.
pub const PM_CSPACE_K: c_uint = 3;
/// `CUPS_CSPACE_SW`: sRGB-gamma luminance, 0 is black.
pub const PM_CSPACE_SW: c_uint = 18;

/// PAPPL's `pappl_job_t`, known to Rust only by pointer.
///
/// cbindgen:no-export
#[repr(C)]
pub struct PapplJob {
    _opaque: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// PAPPL's `pappl_device_t`, known to Rust only by pointer.
///
/// cbindgen:no-export
#[repr(C)]
pub struct PapplDevice {
    _opaque: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// PAPPL's `pappl_pr_options_t`, known to Rust only by pointer.
///
/// cbindgen:no-export
#[repr(C)]
pub struct PapplPrOptions {
    _opaque: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Severity of a job log message, as `pappl_loglevel_t` grades it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    /// Detail for debugging.
    Debug,
    /// Normal progress.
    Info,
    /// Something was ignored or worked around.
    Warn,
    /// The job cannot go on.
    Error,
}

impl LogLevel {
    /// The `pappl_loglevel_t` value.
    #[must_use]
    pub const fn raw(self) -> c_int {
        match self {
            Self::Debug => PM_LOGLEVEL_DEBUG,
            Self::Info => PM_LOGLEVEL_INFO,
            Self::Warn => PM_LOGLEVEL_WARN,
            Self::Error => PM_LOGLEVEL_ERROR,
        }
    }
}

/// The `pappl_media_tracking_t` bit for `tracking`.
///
/// PAPPL has no card mode; as far as PAPPL is concerned card stock is
/// gap-tracked.
#[must_use]
pub const fn tracking_flag(tracking: MediaTracking) -> c_uint {
    match tracking {
        MediaTracking::Continuous => PM_MEDIA_TRACKING_CONTINUOUS,
        MediaTracking::Gap | MediaTracking::Card => PM_MEDIA_TRACKING_GAP,
        MediaTracking::Mark => PM_MEDIA_TRACKING_MARK,
    }
}

/// The tracking mode a single `pappl_media_tracking_t` bit selects, or
/// `None` for anything else (no bit, several, or `web`).
#[must_use]
pub const fn tracking_from_flag(flag: c_uint) -> Option<MediaTracking> {
    match flag {
        PM_MEDIA_TRACKING_CONTINUOUS => Some(MediaTracking::Continuous),
        PM_MEDIA_TRACKING_GAP => Some(MediaTracking::Gap),
        PM_MEDIA_TRACKING_MARK => Some(MediaTracking::Mark),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_flags_round_trip() {
        for tracking in [
            MediaTracking::Continuous,
            MediaTracking::Gap,
            MediaTracking::Mark,
        ] {
            assert_eq!(tracking_from_flag(tracking_flag(tracking)), Some(tracking));
        }
    }

    #[test]
    fn card_stock_is_gap_tracked_for_pappl() {
        assert_eq!(tracking_flag(MediaTracking::Card), PM_MEDIA_TRACKING_GAP);
    }

    #[test]
    fn tracking_from_flag_rejects_combinations_and_web() {
        assert_eq!(tracking_from_flag(0), None);
        assert_eq!(
            tracking_from_flag(PM_MEDIA_TRACKING_GAP | PM_MEDIA_TRACKING_MARK),
            None
        );
        assert_eq!(tracking_from_flag(0x0008), None);
    }
}
