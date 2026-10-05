//! The Bluetooth device backend's C exports, behind `c/device_bt.c`.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use phomemo_protocol::commands::{Command, Query};
use phomemo_protocol::responses::{BatteryStatus, Decoder, Response};

use super::status::{
    STATUS_QUERY_ATTEMPTS, StatusState, apply_status_response, finalize_status_state,
};
use super::{connmgr, discovery, rfcomm};
use crate::pappl::PM_PREASON_OFFLINE;

fn usize_to_isize_or_neg1(value: usize) -> isize {
    isize::try_from(value).unwrap_or(-1)
}

/// Opaque connection handle stored via papplDeviceSetData.
///
/// Owns a `ConnGuard` that keeps the connection-manager mutex locked
/// for the entire open→close lifecycle.  This guarantees the
/// `RfcommConnection` cannot be dropped while in use.
pub struct BtConnectionHandle {
    guard: connmgr::ConnGuard,
    /// Cached battery level (-1 = unknown, 0-100 = percent).
    battery_level: AtomicI32,
    /// Resolved model name (e.g. "M220"), or empty if unknown.
    model_name: CString,
}

/// Enumerate paired Phomemo Bluetooth devices.
///
/// Calls `cb` for each found device.  Returns `true` if any devices were found.
/// The C side passes `pappl_device_cb_t` directly — a non-nullable function pointer.
///
/// # Safety
///
/// `cb` must be a valid function pointer and `data` must remain valid for each callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_list(
    cb: extern "C" fn(*const c_char, *const c_char, *const c_char, *mut c_void) -> bool,
    data: *mut c_void,
    _err_cb: *const c_void,
    _err_data: *mut c_void,
) -> bool {
    let Ok(devices) = discovery::list_paired_phomemo_devices() else {
        return false;
    };

    let mut found = false;
    for dev in &devices {
        let info = CString::new(format!("Phomemo {}", dev.name)).unwrap_or_default();
        let uri = CString::new(discovery::make_uri(&dev.address)).unwrap_or_default();
        let id = CString::new(discovery::make_device_id(&dev.name)).unwrap_or_default();

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
///
/// # Safety
///
/// `uri` must be non-null and point to a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_open(
    uri: *const c_char,
    timeout_ms: c_uint,
) -> *mut BtConnectionHandle {
    if uri.is_null() {
        return ptr::null_mut();
    }

    // SAFETY: the caller passes a NUL-terminated string.
    let Ok(uri_str) = unsafe { CStr::from_ptr(uri) }.to_str() else {
        return ptr::null_mut();
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

    let Ok(mac) = rfcomm::parse_mac(&mac_colon) else {
        return ptr::null_mut();
    };

    // Honor caller timeout. `0` means "use default".
    // SO_RCVTIMEO for reads is set separately.
    let connect_timeout_ms = if timeout_ms == 0 {
        5000
    } else {
        timeout_ms.clamp(100, 30_000)
    };
    let timeout = Duration::from_millis(u64::from(connect_timeout_ms));

    let guard = match connmgr::acquire(mac, channel_hint, timeout) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[Device] BT connect to {mac_colon} failed: {e}");
            return ptr::null_mut();
        }
    };

    // Drain unsolicited data (battery push etc.)
    let drained = rfcomm::drain(guard.conn());
    let battery = AtomicI32::new(-1);
    if !drained.is_empty() {
        let mut decoder = Decoder::new();
        decoder.extend_from_slice(&drained);
        for resp in decoder.responses() {
            if let Response::Battery(BatteryStatus::Level(pct)) = resp {
                battery.store(i32::from(pct), Ordering::Relaxed);
            }
        }
    }

    // Resolve the model name from the MAC via BT device list.
    let model_name = discovery::resolve_model_name_from_mac(&mac_colon)
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
///
/// # Safety
///
/// `handle` must be either null or a pointer returned by `pm_bt_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_close(handle: *mut BtConnectionHandle) {
    if !handle.is_null() {
        // SAFETY: `handle` came from `Box::into_raw` in `pm_bt_open`, and
        // the caller gives up its ownership here.
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Write data to the BT connection with 1024-byte SPP chunking.
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open` and `data` must
/// point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_write(
    handle: *mut BtConnectionHandle,
    data: *const u8,
    len: usize,
) -> isize {
    if handle.is_null() || data.is_null() {
        return -1;
    }
    // SAFETY: `handle` is a live handle from `pm_bt_open`.
    let conn = unsafe { &*handle }.guard.conn();
    // SAFETY: `data` is readable for `len` bytes.
    let slice = unsafe { slice::from_raw_parts(data, len) };
    rfcomm::write_chunked(conn, slice).map_or(-1, usize_to_isize_or_neg1)
}

/// Read data from the BT connection.
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open` and `buf` must
/// point to `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_read(
    handle: *mut BtConnectionHandle,
    buf: *mut u8,
    len: usize,
) -> isize {
    if handle.is_null() || buf.is_null() {
        return -1;
    }
    // SAFETY: `handle` is a live handle from `pm_bt_open`.
    let conn = unsafe { &*handle }.guard.conn();
    // SAFETY: `buf` is writable for `len` bytes.
    let slice = unsafe { slice::from_raw_parts_mut(buf, len) };
    rfcomm::read(conn, slice).map_or(-1, usize_to_isize_or_neg1)
}

/// Read with a temporary `SO_RCVTIMEO` override (in milliseconds).
///
/// Used by `tp_rendjob` to wait longer for the print-completion
/// response without permanently changing the socket timeout.
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open` and `buf` must
/// point to `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_read_timeout(
    handle: *mut BtConnectionHandle,
    buf: *mut u8,
    len: usize,
    timeout_ms: c_uint,
) -> isize {
    if handle.is_null() || buf.is_null() {
        return -1;
    }
    // SAFETY: `handle` is a live handle from `pm_bt_open`.
    let conn = unsafe { &*handle }.guard.conn();
    let timeout = Duration::from_millis(u64::from(timeout_ms));
    let prev = rfcomm::set_recv_timeout(conn, timeout);

    // SAFETY: `buf` is writable for `len` bytes.
    let slice = unsafe { slice::from_raw_parts_mut(buf, len) };
    let result = rfcomm::read(conn, slice).map_or(-1, usize_to_isize_or_neg1);

    // Restore previous timeout
    if let Some(prev) = prev {
        rfcomm::set_recv_timeout(conn, prev);
    }

    result
}

/// Query device status. Returns `pappl_preason_t` bitfield.
///
/// Sends cover and paper queries, then reads with a bounded deadline
/// to collect all expected responses (handles fragmentation, batching,
/// and unsolicited traffic).
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_status(handle: *mut BtConnectionHandle) -> c_uint {
    if handle.is_null() {
        return PM_PREASON_OFFLINE;
    }
    // SAFETY: `handle` is a live handle from `pm_bt_open`.
    let h = unsafe { &*handle };
    let conn = h.guard.conn();

    // Collect responses with a bounded deadline.
    // We expect 3 mandatory responses (cover/paper/temp) but may receive
    // unsolicited battery/work-status packets and fragmented data.
    let read_timeout = Duration::from_millis(500);
    let prev_timeout = rfcomm::set_recv_timeout(conn, read_timeout);

    let query_packets = [Query::Cover, Query::Paper, Query::Temperature]
        .map(|query| Command::Query(query).encode());

    let mut best_partial: Option<StatusState> = None;
    let mut had_transport_error = false;

    for _attempt in 0..STATUS_QUERY_ATTEMPTS {
        let mut write_failed = false;
        for cmd in &query_packets {
            if rfcomm::write_chunked(conn, cmd).is_err() {
                write_failed = true;
                had_transport_error = true;
                break;
            }
        }
        if write_failed {
            continue;
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut decoder = Decoder::new();
        let mut status = StatusState::default();

        while Instant::now() < deadline && !status.is_complete() {
            let mut buf = [0u8; 256];
            match rfcomm::read(conn, &mut buf) {
                Ok(0) => break,
                Err(_) => {
                    had_transport_error = true;
                    break;
                }
                Ok(n) => decoder.extend_from_slice(&buf[..n]),
            }

            for response in decoder.responses() {
                if let Some(level) = apply_status_response(&mut status, &response) {
                    h.battery_level.store(level, Ordering::Relaxed);
                }
            }
        }

        if status.is_complete() {
            if let Some(prev) = prev_timeout {
                rfcomm::set_recv_timeout(conn, prev);
            }
            return status.reasons;
        }

        best_partial = match best_partial {
            Some(prev) if prev.completeness_score() >= status.completeness_score() => Some(prev),
            _ => Some(status),
        };
    }

    if let Some(prev) = prev_timeout {
        rfcomm::set_recv_timeout(conn, prev);
    }

    best_partial.map_or(PM_PREASON_OFFLINE, |status| {
        finalize_status_state(status, had_transport_error)
    })
}

/// Get cached battery level.  Returns 0-100, or -1 if unknown.
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_battery(handle: *mut BtConnectionHandle) -> c_int {
    if handle.is_null() {
        return -1;
    }
    // SAFETY: `handle` is a live handle from `pm_bt_open`.
    let h = unsafe { &*handle };
    h.battery_level.load(Ordering::Relaxed)
}

/// Get the resolved model name (e.g. "M220") for this connection.
///
/// Returns a pointer to a NUL-terminated string valid for the handle's
/// lifetime, or an empty string if the model is unknown.
///
/// # Safety
///
/// `handle` must be a valid pointer returned by `pm_bt_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_model_name(handle: *mut BtConnectionHandle) -> *const c_char {
    if handle.is_null() {
        return c"".as_ptr();
    }
    // SAFETY: `handle` is a live handle from `pm_bt_open`.
    let h = unsafe { &*handle };
    h.model_name.as_ptr()
}
