//! PAPPL FFI bridge and Bluetooth transport for Phomemo printers.
//!
//! This crate produces `libphomemo_pappl.a` — a static library linked into
//! the C PAPPL shell. It exports:
//!
//! - Raster callback implementations (start/end job/page, write line)
//! - Bluetooth device backend functions (list, open, close, read, write)

mod bt;
mod ffi;
mod testpage;

pub use ffi::*;

// ---------------------------------------------------------------------------
// Model info FFI
// ---------------------------------------------------------------------------

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::sync::OnceLock;

const PAPPL_MEDIA_TRACKING_CONTINUOUS: c_uint = 0x0001;
const PAPPL_MEDIA_TRACKING_GAP: c_uint = 0x0002;
const PAPPL_MEDIA_TRACKING_MARK: c_uint = 0x0004;

fn usize_to_isize_or_neg1(value: usize) -> isize {
    isize::try_from(value).unwrap_or(-1)
}

/// Per-model hardware capabilities, exported to C via cbindgen.
///
/// All string pointers are static (valid for the process lifetime).
#[repr(C)]
pub struct ModelInfoC {
    /// Model name (e.g. "M220").
    pub name: *const c_char,
    /// PAPPL driver name (e.g. `phomemo_m220`).
    pub driver_name: *const c_char,
    /// IEEE 1284 device ID fragment (e.g. "MFG:Phomemo;MDL:M220;CMD:PHOMEMO;").
    pub device_id: *const c_char,
    /// Print resolution in DPI.
    pub dpi: u16,
    /// Print-head width in bytes (e.g. 72 for 576 px).
    pub max_width_bytes: u16,
    /// Print-head width in pixels.
    pub max_width_px: u16,
    /// Whether the device has a cutter.
    pub has_cutter: bool,
    /// Whether the firmware accepts LZO-compressed raster data.
    pub supports_compression: bool,
}

/// Build the static model table once. Each model gets its strings
/// leaked into `'static` `CStrings` so the pointers are always valid.
fn build_model_table() -> Vec<ModelInfoC> {
    phomemo_protocol::model::all_models()
        .iter()
        .map(|m| {
            // Leak CStrings so pointers live forever (process lifetime).
            let name = CString::new(m.name).unwrap_or_default();
            let driver_name = m.name.to_lowercase();
            let driver = CString::new(format!("phomemo_{driver_name}")).unwrap_or_default();
            let devid = CString::new(format!(
                "MFG:Phomemo;MDL:{name};CMD:PHOMEMO;",
                name = m.name
            ))
            .unwrap_or_default();

            ModelInfoC {
                name: name.into_raw().cast_const(),
                driver_name: driver.into_raw().cast_const(),
                device_id: devid.into_raw().cast_const(),
                dpi: m.dpi,
                max_width_bytes: m.max_width_bytes,
                max_width_px: m.max_width_px,
                has_cutter: m.has_cutter,
                supports_compression: m.supports_compression,
            }
        })
        .collect()
}

// SAFETY: ModelInfoC contains *const c_char pointers that are leaked
// CStrings (valid for the process lifetime, never mutated).
unsafe impl Send for ModelInfoC {}
unsafe impl Sync for ModelInfoC {}

/// Lazily-initialized static model table.
fn model_table() -> &'static [ModelInfoC] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<ModelInfoC>> = OnceLock::new();
    TABLE.get_or_init(build_model_table)
}

/// Number of known printer models.
#[no_mangle]
pub extern "C" fn pm_model_count() -> c_uint {
    c_uint::try_from(model_table().len()).unwrap_or(c_uint::MAX)
}

/// Get the model info at the given index.
///
/// Returns NULL if `idx >= pm_model_count()`.
#[no_mangle]
pub extern "C" fn pm_model_get(idx: c_uint) -> *const ModelInfoC {
    let Ok(idx_usize) = usize::try_from(idx) else {
        return std::ptr::null();
    };

    model_table()
        .get(idx_usize)
        .map_or(std::ptr::null(), std::ptr::from_ref)
}

/// Look up a model by its PAPPL driver name (e.g. `phomemo_m220`).
///
/// Returns NULL if no match is found.
#[no_mangle]
///
/// # Safety
///
/// `driver_name` must be non-null and point to a valid NUL-terminated C string.
pub unsafe extern "C" fn pm_model_lookup(driver_name: *const c_char) -> *const ModelInfoC {
    if driver_name.is_null() {
        return std::ptr::null();
    }

    let Ok(name) = CStr::from_ptr(driver_name).to_str() else {
        return std::ptr::null();
    };

    model_table()
        .iter()
        .find(|m| {
            let dn = unsafe { CStr::from_ptr(m.driver_name) };
            dn.to_str().map(|s| s == name).unwrap_or(false)
        })
        .map_or(std::ptr::null(), std::ptr::from_ref)
}

// ---------------------------------------------------------------------------
// Driver defaults — data-driven capability struct for tp_driver_cb
// ---------------------------------------------------------------------------

/// Data-driven driver capabilities exported to C.
///
/// The C `tp_driver_cb` copies these values into PAPPL's
/// `pappl_pr_driver_data_t` and then sets the callback function
/// pointers (which must remain in C because they reference PAPPL
/// opaque types).
///
/// All string fields are `*const c_char` with `'static` lifetime
/// (leaked `CStrings` from the model table).
#[repr(C)]
pub struct DriverDefaultsC {
    /// "Phomemo M220" etc.
    pub make_and_model: [c_char; 128],
    /// DPI (both axes).
    pub dpi: c_int,
    /// Darkness range (0 = unsupported).
    pub darkness_supported: c_int,
    pub darkness_default: c_int,
    /// Speed range in hundredths of mm/sec.
    pub speed_min: c_int,
    pub speed_max: c_int,
    pub speed_default: c_int,
    /// Has cutter.
    pub has_cutter: bool,
    /// Union of all supported tracking modes (PAPPL bit values).
    pub tracking_supported: c_uint,
    /// Media: PWG names for supported sizes (NUL-terminated array).
    pub num_media: c_int,
    pub media_names: [*const c_char; 64],
    /// Preferred tracking mode for each media slot (PAPPL bit values).
    pub media_tracking: [c_uint; 64],
    /// Default media size in hundredths of mm.
    pub default_width: c_int,
    pub default_length: c_int,
    /// Default media PWG size name.
    pub default_size_name: [c_char; 64],
    /// Default media tracking (PAPPL bit value).
    pub default_tracking: c_uint,
}

struct ModelMedia {
    names: Vec<CString>,
    tracking: Vec<c_uint>,
    default_width: c_int,
    default_length: c_int,
    default_size_name: String,
    default_tracking: c_uint,
}

fn head_width_hundredths_mm(max_width_px: u16, dpi: u16) -> u32 {
    let px = u32::from(max_width_px);
    let dpi = u32::from(dpi.max(1));
    (px * 2540 + (dpi / 2)) / dpi
}

fn fallback_media_for_model(max_width_px: u16, dpi: u16) -> phomemo_protocol::media::MediaPreset {
    let width_hundredths = head_width_hundredths_mm(max_width_px, dpi).max(100);
    #[allow(clippy::cast_precision_loss)]
    let width_mm = width_hundredths as f32 / 100.0;
    let width_mm_label = (width_hundredths + 50) / 100;

    phomemo_protocol::media::MediaPreset {
        size_name: format!("om_{width_mm_label}x0mm_{width_mm_label}x0mm"),
        width_mm,
        length_mm: 0.0,
        tracking_default: phomemo_protocol::media::Tracking::Continuous,
        tracking_supported: vec![
            phomemo_protocol::media::Tracking::Continuous,
            phomemo_protocol::media::Tracking::Gap,
            phomemo_protocol::media::Tracking::Mark,
        ],
    }
}

fn media_for_model(model_name: &str) -> Vec<phomemo_protocol::media::MediaPreset> {
    let media = phomemo_protocol::media::media_for_printer_type(model_name);
    if !media.is_empty() {
        return media;
    }

    phomemo_protocol::model::lookup(model_name).map_or_else(Vec::new, |model| {
        phomemo_protocol::media::media_for_printer_type(model.series)
    })
}

fn default_media_for_model(model_name: &str) -> Option<phomemo_protocol::media::MediaPreset> {
    phomemo_protocol::media::default_media_for_printer_type(model_name).or_else(|| {
        phomemo_protocol::model::lookup(model_name)
            .and_then(|model| phomemo_protocol::media::default_media_for_printer_type(model.series))
    })
}

fn tracking_mask_for_model(model_name: &str) -> c_uint {
    let mask = phomemo_protocol::media::tracking_supported_mask(model_name);
    if mask != 0 {
        return mask;
    }

    phomemo_protocol::model::lookup(model_name).map_or(0, |model| {
        phomemo_protocol::media::tracking_supported_mask(model.series)
    })
}

fn build_model_media_table() -> std::collections::BTreeMap<String, ModelMedia> {
    let mut map = std::collections::BTreeMap::new();

    for model in model_table() {
        let model_name = unsafe { CStr::from_ptr(model.name) }
            .to_str()
            .unwrap_or_default()
            .to_string();
        let fallback = fallback_media_for_model(model.max_width_px, model.dpi);

        let mut media = media_for_model(&model_name);
        if media.is_empty() {
            media.push(fallback.clone());
        }

        let mut names = Vec::new();
        let mut tracking = Vec::new();
        for entry in &media {
            if let Ok(c_name) = CString::new(entry.size_name.as_str()) {
                names.push(c_name);
                tracking.push(entry.tracking_default.pappl_flag());
            }
        }

        if names.is_empty() {
            if let Ok(c_name) = CString::new(fallback.size_name.as_str()) {
                names.push(c_name);
                tracking.push(fallback.tracking_default.pappl_flag());
                media.push(fallback.clone());
            }
        }

        let default = default_media_for_model(&model_name)
            .or_else(|| media.first().cloned())
            .unwrap_or(fallback);

        map.insert(
            model_name,
            ModelMedia {
                names,
                tracking,
                default_width: default.width_hundredths_mm(),
                default_length: default.length_hundredths_mm(),
                default_size_name: default.size_name,
                default_tracking: default.tracking_default.pappl_flag(),
            },
        );
    }

    map
}

fn model_media_table() -> &'static std::collections::BTreeMap<String, ModelMedia> {
    static TABLE: OnceLock<std::collections::BTreeMap<String, ModelMedia>> = OnceLock::new();
    TABLE.get_or_init(build_model_media_table)
}

/// Fill a `DriverDefaultsC` for the given driver name.
///
/// Returns `true` on success, `false` if the driver is unknown.
#[no_mangle]
///
/// # Safety
///
/// `driver_name` must be a valid NUL-terminated C string.
/// `out` must point to a valid `DriverDefaultsC`.
pub unsafe extern "C" fn pm_driver_defaults(
    driver_name: *const c_char,
    out: *mut DriverDefaultsC,
) -> bool {
    if driver_name.is_null() || out.is_null() {
        return false;
    }
    let Ok(name) = CStr::from_ptr(driver_name).to_str() else {
        return false;
    };

    // Find the model.
    let model = model_table().iter().find(|m| {
        let dn = unsafe { CStr::from_ptr(m.driver_name) };
        dn.to_str().is_ok_and(|s| s == name)
    });
    let Some(model) = model else {
        return false;
    };

    let d = &mut *out;

    // make_and_model
    let mam = format!(
        "Phomemo {}",
        CStr::from_ptr(model.name).to_str().unwrap_or("?")
    );
    fill_c_str(&mut d.make_and_model, &mam);

    // Resolution
    d.dpi = c_int::from(model.dpi);

    // Darkness
    d.darkness_supported = 15;
    d.darkness_default = 8;

    // Speed (hundredths of mm/sec)
    d.speed_min = 2540;
    d.speed_max = 2540 * 6;
    d.speed_default = 2540 * 3;

    // Cutter
    d.has_cutter = model.has_cutter;

    let model_name = CStr::from_ptr(model.name)
        .to_str()
        .unwrap_or_default()
        .to_string();
    let media_table = model_media_table();
    let media = media_table.get(&model_name);
    d.tracking_supported = tracking_mask_for_model(&model_name);
    if d.tracking_supported == 0 {
        d.tracking_supported =
            PAPPL_MEDIA_TRACKING_CONTINUOUS | PAPPL_MEDIA_TRACKING_GAP | PAPPL_MEDIA_TRACKING_MARK;
    }

    // Media names and per-size tracking
    let media_count = media
        .map_or(0, |m| m.names.len())
        .min(d.media_names.len())
        .min(d.media_tracking.len());

    d.num_media = c_int::try_from(media_count).unwrap_or(0);
    if let Some(m) = media {
        for (i, name) in m.names.iter().enumerate().take(media_count) {
            d.media_names[i] = name.as_ptr();
            d.media_tracking[i] = m
                .tracking
                .get(i)
                .copied()
                .unwrap_or(PAPPL_MEDIA_TRACKING_GAP);
        }
    }

    for i in media_count..d.media_names.len() {
        d.media_names[i] = std::ptr::null();
        d.media_tracking[i] = 0;
    }

    // Default media and tracking
    if let Some(m) = media {
        d.default_width = m.default_width;
        d.default_length = m.default_length;
        d.default_tracking = m.default_tracking;
        fill_c_str(&mut d.default_size_name, &m.default_size_name);
    } else {
        let fallback = fallback_media_for_model(model.max_width_px, model.dpi);
        d.default_width = fallback.width_hundredths_mm();
        d.default_length = fallback.length_hundredths_mm();
        d.default_tracking = fallback.tracking_default.pappl_flag();
        fill_c_str(&mut d.default_size_name, &fallback.size_name);
    }

    true
}

/// Copy a Rust string into a fixed C char buffer.
fn fill_c_str<const N: usize>(buf: &mut [c_char; N], s: &str) {
    buf.fill(0);
    if N == 0 {
        return;
    }

    let bytes = s.as_bytes();
    let copy_len = bytes.len().min(N - 1);
    for (i, &b) in bytes.iter().enumerate().take(copy_len) {
        buf[i] = b.cast_signed();
    }
}

/// Resolve preferred media tracking for a given size name.
///
/// Returns PAPPL media-tracking bit values:
/// - `0x0001` continuous
/// - `0x0002` gap
/// - `0x0004` mark
/// - `0` unknown
#[no_mangle]
///
/// # Safety
///
/// `driver_name` and `size_name` must be valid NUL-terminated C strings.
pub unsafe extern "C" fn pm_media_tracking_for_size(
    driver_name: *const c_char,
    size_name: *const c_char,
) -> c_uint {
    if driver_name.is_null() || size_name.is_null() {
        return 0;
    }

    let Ok(driver) = CStr::from_ptr(driver_name).to_str() else {
        return 0;
    };
    let Ok(size) = CStr::from_ptr(size_name).to_str() else {
        return 0;
    };

    let model = model_table().iter().find(|m| {
        let dn = unsafe { CStr::from_ptr(m.driver_name) };
        dn.to_str().is_ok_and(|s| s == driver)
    });
    let Some(model) = model else {
        return 0;
    };

    let model_name = CStr::from_ptr(model.name).to_str().unwrap_or_default();
    phomemo_protocol::media::tracking_for_size_name(model_name, size)
        .map_or(0, phomemo_protocol::media::Tracking::pappl_flag)
}

// ---------------------------------------------------------------------------
// IEEE 1284 device-ID matching (autoadd)
// ---------------------------------------------------------------------------

mod ieee1284;

/// Determine the best driver for a device given its IEEE 1284 ID and
/// human-readable info string.
///
/// Returns the `driver_name` pointer from the model table (valid for
/// the process lifetime), or NULL if no driver matches.
///
/// Matching is three-phase:
/// 1. Exact MDL/MODEL field match (highest priority).
/// 2. Scored key/value matching of the full device ID against each
///    driver's device-ID template (2 pts exact, 1 pt partial).
/// 3. Case-insensitive substring of model name in `device_info`,
///    only when IEEE 1284 data is absent.
#[no_mangle]
///
/// # Safety
///
/// Both `device_id` and `device_info` must be either null or valid
/// NUL-terminated C strings.
pub unsafe extern "C" fn pm_autoadd_match(
    device_info: *const c_char,
    device_id: *const c_char,
) -> *const c_char {
    let dev_id = if device_id.is_null() {
        None
    } else {
        CStr::from_ptr(device_id).to_str().ok()
    };

    let dev_info = if device_info.is_null() {
        None
    } else {
        CStr::from_ptr(device_info).to_str().ok()
    };

    let table = model_table();

    // Helper: read a model's name as &str.
    let model_name = |m: &ModelInfoC| -> &str {
        unsafe { CStr::from_ptr(m.name) }
            .to_str()
            .unwrap_or_default()
    };

    // Helper: read a model's device_id template as &str.
    let driver_devid = |m: &ModelInfoC| -> &str {
        unsafe { CStr::from_ptr(m.device_id) }
            .to_str()
            .unwrap_or_default()
    };

    if let Some(id) = dev_id {
        // Phase 1: exact MDL match.
        if let Some(mdl) =
            ieee1284::extract_field(id, "MDL").or_else(|| ieee1284::extract_field(id, "MODEL"))
        {
            for m in table {
                if mdl.eq_ignore_ascii_case(model_name(m)) {
                    return m.driver_name;
                }
            }
        }

        // Phase 2: scored matching.
        let mut best_score: u32 = 0;
        let mut best: *const c_char = std::ptr::null();

        for m in table {
            let score = ieee1284::match_score(id, driver_devid(m));
            if score > best_score {
                best_score = score;
                best = m.driver_name;
            }
        }
        if !best.is_null() {
            return best;
        }
    }

    // Phase 3: fallback substring in device_info (only when no ID).
    if dev_id.is_none_or(str::is_empty) {
        if let Some(info) = dev_info {
            let info_upper = info.to_uppercase();
            for m in table {
                let name = model_name(m);
                if !name.is_empty() && info_upper.contains(&name.to_uppercase()) {
                    return m.driver_name;
                }
            }
        }
    }

    std::ptr::null()
}

// ---------------------------------------------------------------------------
// BT device backend FFI exports
// ---------------------------------------------------------------------------

// PAPPL printer-reason bit flags (from pappl/printer.h pappl_preason_t).
const PAPPL_PREASON_OTHER: c_uint = 0x0001;
const PAPPL_PREASON_COVER_OPEN: c_uint = 0x0002;
const PAPPL_PREASON_MARKER_SUPPLY_LOW: c_uint = 0x0010; // low battery
const PAPPL_PREASON_MEDIA_EMPTY: c_uint = 0x0080;
const PAPPL_PREASON_OFFLINE: c_uint = 0x0800;

const STATUS_QUERY_ATTEMPTS: usize = 2;
const STATUS_SEEN_COVER: u8 = 0x01;
const STATUS_SEEN_PAPER: u8 = 0x02;
const STATUS_SEEN_TEMP: u8 = 0x04;

#[derive(Debug, Default, Clone, Copy)]
struct StatusState {
    reasons: c_uint,
    mandatory_seen: u8,
    saw_response: bool,
}

impl StatusState {
    const fn mark_cover(&mut self) {
        self.mandatory_seen |= STATUS_SEEN_COVER;
    }

    const fn mark_paper(&mut self) {
        self.mandatory_seen |= STATUS_SEEN_PAPER;
    }

    const fn mark_temperature(&mut self) {
        self.mandatory_seen |= STATUS_SEEN_TEMP;
    }

    const fn completeness_score(self) -> u8 {
        let mut score = 0;
        if self.mandatory_seen & STATUS_SEEN_COVER != 0 {
            score += 1;
        }
        if self.mandatory_seen & STATUS_SEEN_PAPER != 0 {
            score += 1;
        }
        if self.mandatory_seen & STATUS_SEEN_TEMP != 0 {
            score += 1;
        }
        score
    }

    const fn is_complete(self) -> bool {
        self.mandatory_seen == (STATUS_SEEN_COVER | STATUS_SEEN_PAPER | STATUS_SEEN_TEMP)
    }
}

fn apply_status_response(
    state: &mut StatusState,
    response: &phomemo_protocol::responses::Response,
) -> Option<i32> {
    use phomemo_protocol::responses::{BatteryStatus, Response};

    state.saw_response = true;
    match response {
        Response::Cover { closed } => {
            state.mark_cover();
            if !closed {
                state.reasons |= PAPPL_PREASON_COVER_OPEN;
            }
            None
        }
        Response::Paper { present } => {
            state.mark_paper();
            if !present {
                state.reasons |= PAPPL_PREASON_MEDIA_EMPTY;
            }
            None
        }
        Response::Temperature { overheated } => {
            state.mark_temperature();
            if *overheated {
                state.reasons |= PAPPL_PREASON_OTHER;
            }
            None
        }
        Response::Battery(BatteryStatus::Level(pct)) => {
            if *pct <= 10 {
                state.reasons |= PAPPL_PREASON_MARKER_SUPPLY_LOW;
            }
            Some(i32::from(*pct))
        }
        Response::Battery(BatteryStatus::LowAlarm(_)) => {
            state.reasons |= PAPPL_PREASON_MARKER_SUPPLY_LOW;
            None
        }
        Response::WorkStatus { idle } => {
            if !idle {
                state.reasons |= PAPPL_PREASON_OTHER;
            }
            None
        }
        Response::PrintBusy { busy } => {
            if *busy {
                state.reasons |= PAPPL_PREASON_OTHER;
            }
            None
        }
        Response::PrintCancel => {
            state.reasons |= PAPPL_PREASON_OTHER;
            None
        }
        _ => None,
    }
}

const fn finalize_status_state(state: StatusState, had_transport_error: bool) -> c_uint {
    if state.is_complete() {
        return state.reasons;
    }

    if !state.saw_response || had_transport_error {
        return state.reasons | PAPPL_PREASON_OFFLINE;
    }

    // Partial status can happen during transient BT timing jitter.
    // Keep printer online but flag state as degraded.
    state.reasons | PAPPL_PREASON_OTHER
}

/// Opaque connection handle stored via papplDeviceSetData.
///
/// Owns a `ConnGuard` that keeps the connection-manager mutex locked
/// for the entire open→close lifecycle.  This guarantees the
/// `RfcommConnection` cannot be dropped while in use.
pub struct BtConnectionHandle {
    guard: bt::connmgr::ConnGuard,
    /// Cached battery level (-1 = unknown, 0-100 = percent).
    battery_level: std::sync::atomic::AtomicI32,
    /// Resolved model name (e.g. "M220"), or empty if unknown.
    model_name: CString,
}

/// Enumerate paired Phomemo Bluetooth devices.
///
/// Calls `cb` for each found device.  Returns `true` if any devices were found.
/// The C side passes `pappl_device_cb_t` directly — a non-nullable function pointer.
#[no_mangle]
///
/// # Safety
///
/// `cb` must be a valid function pointer and `data` must remain valid for each callback.
pub unsafe extern "C" fn pm_bt_list(
    cb: extern "C" fn(*const c_char, *const c_char, *const c_char, *mut c_void) -> bool,
    data: *mut c_void,
    _err_cb: *const c_void,
    _err_data: *mut c_void,
) -> bool {
    let Ok(devices) = bt::discovery::list_paired_phomemo_devices() else {
        return false;
    };

    let mut found = false;
    for dev in &devices {
        let info = CString::new(format!("Phomemo {}", dev.name)).unwrap_or_default();
        let uri = CString::new(bt::discovery::make_uri(&dev.address)).unwrap_or_default();
        let id = CString::new(bt::discovery::make_device_id(&dev.name)).unwrap_or_default();

        found = true;
        // cb returns true to stop iteration, false to continue
        if cb(info.as_ptr(), uri.as_ptr(), id.as_ptr(), data) {
            break;
        }
    }

    found
}

/// Open a btspp:// connection (or reuse a persistent one).
///
/// Uses the connection manager to keep the RFCOMM socket alive across
/// PAPPL's rapid open/close cycles.
#[no_mangle]
///
/// # Safety
///
/// `uri` must be non-null and point to a valid NUL-terminated C string.
pub unsafe extern "C" fn pm_bt_open(
    uri: *const c_char,
    timeout_ms: c_uint,
) -> *mut BtConnectionHandle {
    if uri.is_null() {
        return std::ptr::null_mut();
    }

    let Ok(uri_str) = CStr::from_ptr(uri).to_str() else {
        return std::ptr::null_mut();
    };

    let raw_uri = uri_str.strip_prefix("btspp://").unwrap_or(uri_str);
    let (addr_part, query_part) = raw_uri
        .split_once('?')
        .map_or((raw_uri, ""), |(addr, query)| (addr, query));
    let mac_token = addr_part.split('/').next().unwrap_or(addr_part);
    let mac_colon = mac_token.replace('-', ":");

    let mut channel_hint: Option<u8> = None;
    for param in query_part.split('&') {
        let Some(value) = param.strip_prefix("channel=") else {
            continue;
        };
        if let Ok(channel) = value.parse::<u8>() {
            if (1..=30).contains(&channel) {
                channel_hint = Some(channel);
                break;
            }
        }
    }

    let Ok(mac) = bt::rfcomm::parse_mac(&mac_colon) else {
        return std::ptr::null_mut();
    };

    // Honor caller timeout. `0` means "use default".
    // SO_RCVTIMEO for reads is set separately.
    let connect_timeout_ms = if timeout_ms == 0 {
        5000
    } else {
        timeout_ms.clamp(100, 30_000)
    };
    let timeout = std::time::Duration::from_millis(u64::from(connect_timeout_ms));

    let guard = match bt::connmgr::acquire(mac, channel_hint, timeout) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[Device] BT connect to {mac_colon} failed: {e}");
            return std::ptr::null_mut();
        }
    };

    // Drain unsolicited data (battery push etc.)
    let drained = bt::rfcomm::drain(guard.conn());
    let battery = std::sync::atomic::AtomicI32::new(-1);
    if !drained.is_empty() {
        for resp in phomemo_protocol::responses::parse_all(&drained) {
            if let phomemo_protocol::responses::Response::Battery(
                phomemo_protocol::responses::BatteryStatus::Level(pct),
            ) = resp
            {
                battery.store(i32::from(pct), std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    // Resolve the model name from the MAC via BT device list.
    let model_name = bt::discovery::resolve_model_name_from_mac(&mac_colon)
        .and_then(|n| CString::new(n).ok())
        .unwrap_or_default();

    Box::into_raw(Box::new(BtConnectionHandle {
        guard,
        battery_level: battery,
        model_name,
    }))
}

/// Release the BT connection handle.
///
/// Drops the `ConnGuard`, unlocking the connection-manager mutex.
/// The persistent socket stays alive for reuse by subsequent opens
/// until the idle timeout expires.
#[no_mangle]
///
/// # Safety
///
/// `handle` must be either null or a pointer returned by `pm_bt_open`.
pub unsafe extern "C" fn pm_bt_close(handle: *mut BtConnectionHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Write data to the BT connection with 1024-byte SPP chunking.
#[no_mangle]
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open` and `data` must
/// point to `len` readable bytes.
pub unsafe extern "C" fn pm_bt_write(
    handle: *mut BtConnectionHandle,
    data: *const u8,
    len: usize,
) -> isize {
    if handle.is_null() || data.is_null() {
        return -1;
    }
    let conn = (&*handle).guard.conn();
    let slice = std::slice::from_raw_parts(data, len);
    bt::rfcomm::write_chunked(conn, slice).map_or(-1, usize_to_isize_or_neg1)
}

/// Read data from the BT connection.
#[no_mangle]
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open` and `buf` must
/// point to `len` writable bytes.
pub unsafe extern "C" fn pm_bt_read(
    handle: *mut BtConnectionHandle,
    buf: *mut u8,
    len: usize,
) -> isize {
    if handle.is_null() || buf.is_null() {
        return -1;
    }
    let conn = (&*handle).guard.conn();
    let slice = std::slice::from_raw_parts_mut(buf, len);
    bt::rfcomm::read(conn, slice).map_or(-1, usize_to_isize_or_neg1)
}

/// Read with a temporary `SO_RCVTIMEO` override (in milliseconds).
///
/// Used by `pm_end_job` to wait longer for the print-completion
/// response without permanently changing the socket timeout.
#[no_mangle]
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open` and `buf` must
/// point to `len` writable bytes.
pub unsafe extern "C" fn pm_bt_read_timeout(
    handle: *mut BtConnectionHandle,
    buf: *mut u8,
    len: usize,
    timeout_ms: c_uint,
) -> isize {
    if handle.is_null() || buf.is_null() {
        return -1;
    }
    let conn = (&*handle).guard.conn();
    let timeout = std::time::Duration::from_millis(u64::from(timeout_ms));
    let prev = bt::rfcomm::set_recv_timeout(conn, timeout);

    let slice = std::slice::from_raw_parts_mut(buf, len);
    let result = bt::rfcomm::read(conn, slice).map_or(-1, usize_to_isize_or_neg1);

    // Restore previous timeout
    if let Some(prev) = prev {
        bt::rfcomm::set_recv_timeout(conn, prev);
    }

    result
}

/// Query device status. Returns `pappl_preason_t` bitfield.
///
/// Sends cover and paper queries, then reads with a bounded deadline
/// to collect all expected responses (handles fragmentation, batching,
/// and unsolicited traffic).
#[no_mangle]
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open`.
pub unsafe extern "C" fn pm_bt_status(handle: *mut BtConnectionHandle) -> c_uint {
    if handle.is_null() {
        return PAPPL_PREASON_OFFLINE;
    }
    let h = &*handle;
    let conn = h.guard.conn();

    // Collect responses with a bounded deadline.
    // We expect 3 mandatory responses (cover/paper/temp) but may receive
    // unsolicited battery/work-status packets and fragmented data.
    let read_timeout = std::time::Duration::from_millis(500);
    let prev_timeout = bt::rfcomm::set_recv_timeout(conn, read_timeout);

    let query_packets: [Vec<u8>; 3] = [
        phomemo_protocol::commands::query_cover(),
        phomemo_protocol::commands::query_paper(),
        phomemo_protocol::commands::query_overheat(),
    ];

    let mut best_partial: Option<StatusState> = None;
    let mut had_transport_error = false;

    for _attempt in 0..STATUS_QUERY_ATTEMPTS {
        let mut write_failed = false;
        for cmd in &query_packets {
            if bt::rfcomm::write_chunked(conn, cmd).is_err() {
                write_failed = true;
                had_transport_error = true;
                break;
            }
        }
        if write_failed {
            continue;
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut collected = Vec::new();
        let mut status = StatusState::default();

        while std::time::Instant::now() < deadline && !status.is_complete() {
            let mut buf = [0u8; 256];
            match bt::rfcomm::read(conn, &mut buf) {
                Ok(0) => break,
                Err(_) => {
                    had_transport_error = true;
                    break;
                }
                Ok(n) => collected.extend_from_slice(&buf[..n]),
            }

            let (responses, consumed) = phomemo_protocol::responses::parse_all_resilient(&collected);
            if consumed > 0 {
                collected.drain(..consumed);
            }

            for response in responses {
                if let Some(level) = apply_status_response(&mut status, &response) {
                    h.battery_level
                        .store(level, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        if status.is_complete() {
            if let Some(prev) = prev_timeout {
                bt::rfcomm::set_recv_timeout(conn, prev);
            }
            return status.reasons;
        }

        best_partial = match best_partial {
            Some(prev) if prev.completeness_score() >= status.completeness_score() => Some(prev),
            _ => Some(status),
        };
    }

    if let Some(prev) = prev_timeout {
        bt::rfcomm::set_recv_timeout(conn, prev);
    }

    best_partial.map_or(PAPPL_PREASON_OFFLINE, |status| {
        finalize_status_state(status, had_transport_error)
    })
}

/// Get cached battery level.  Returns 0-100, or -1 if unknown.
#[no_mangle]
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open`.
pub unsafe extern "C" fn pm_bt_battery(handle: *mut BtConnectionHandle) -> c_int {
    if handle.is_null() {
        return -1;
    }
    let h = &*handle;
    h.battery_level.load(std::sync::atomic::Ordering::Relaxed)
}

/// Get the resolved model name (e.g. "M220") for this connection.
///
/// Returns a pointer to a NUL-terminated string valid for the handle's
/// lifetime, or an empty string if the model is unknown.
#[no_mangle]
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open`.
pub unsafe extern "C" fn pm_bt_model_name(handle: *mut BtConnectionHandle) -> *const c_char {
    if handle.is_null() {
        return c"".as_ptr();
    }
    let h = &*handle;
    h.model_name.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_char;

    fn c_array_to_string(buf: &[c_char]) -> String {
        let bytes = buf
            .iter()
            .take_while(|&&ch| ch != 0)
            .map(|&ch| ch.cast_unsigned())
            .collect::<Vec<_>>();
        String::from_utf8(bytes).expect("buffer must contain utf-8 test data")
    }

    #[test]
    fn model_lookup_rejects_null_pointer() {
        let ptr = unsafe { pm_model_lookup(std::ptr::null()) };
        assert!(ptr.is_null());
    }

    #[test]
    fn fallback_media_uses_head_width() {
        let media = fallback_media_for_model(576, 203);
        assert_eq!(media.size_name, "om_72x0mm_72x0mm");
        assert_eq!(media.width_hundredths_mm(), 7207);
        assert_eq!(media.length_hundredths_mm(), 0);
        assert_eq!(
            media.tracking_default,
            phomemo_protocol::media::Tracking::Continuous
        );
    }

    #[test]
    fn fill_c_str_clears_previous_content() {
        let mut buf = [0 as c_char; 8];
        fill_c_str(&mut buf, "ABCDEFG");
        fill_c_str(&mut buf, "X");

        assert_eq!(c_array_to_string(&buf), "X");
        assert_eq!(buf[2], 0);
    }

    #[test]
    fn status_finalize_marks_offline_when_no_data() {
        let state = StatusState::default();
        let reasons = finalize_status_state(state, false);
        assert_eq!(reasons & PAPPL_PREASON_OFFLINE, PAPPL_PREASON_OFFLINE);
    }

    #[test]
    fn status_finalize_keeps_partial_online_without_transport_error() {
        let state = StatusState {
            mandatory_seen: STATUS_SEEN_COVER,
            saw_response: true,
            ..StatusState::default()
        };
        let reasons = finalize_status_state(state, false);
        assert_eq!(reasons & PAPPL_PREASON_OFFLINE, 0);
        assert_eq!(reasons & PAPPL_PREASON_OTHER, PAPPL_PREASON_OTHER);
    }

    #[test]
    fn status_apply_response_maps_core_reasons() {
        use phomemo_protocol::responses::Response;

        let mut state = StatusState::default();
        let battery = apply_status_response(
            &mut state,
            &Response::Battery(phomemo_protocol::responses::BatteryStatus::Level(7)),
        );
        assert_eq!(battery, Some(7));
        assert_eq!(
            state.reasons & PAPPL_PREASON_MARKER_SUPPLY_LOW,
            PAPPL_PREASON_MARKER_SUPPLY_LOW
        );

        let _ = apply_status_response(&mut state, &Response::Cover { closed: false });
        let _ = apply_status_response(&mut state, &Response::Paper { present: false });
        let _ = apply_status_response(&mut state, &Response::Temperature { overheated: true });

        assert!(state.is_complete());
        assert_eq!(state.reasons & PAPPL_PREASON_COVER_OPEN, PAPPL_PREASON_COVER_OPEN);
        assert_eq!(state.reasons & PAPPL_PREASON_MEDIA_EMPTY, PAPPL_PREASON_MEDIA_EMPTY);
        assert_eq!(state.reasons & PAPPL_PREASON_OTHER, PAPPL_PREASON_OTHER);
    }
}
