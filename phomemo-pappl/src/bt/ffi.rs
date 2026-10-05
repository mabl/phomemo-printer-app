//! The Bluetooth device backend's C exports, behind `c/device_bt.c`.
//!
//! PAPPL keeps a [`PmBtConnection`] as the device's data between its
//! callbacks. Each export checks its pointers and hands over to the safe
//! code of the other `bt` modules; text for C - error messages, the device
//! ID - is written into a buffer the caller provides, as PAPPL's own
//! callbacks do.

use std::error::Error as StdError;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fmt;
use std::ptr;
use std::slice;
use std::time::Duration;

use libc::ssize_t;

use super::address::{BdAddr, BtUri, ParseUriError};
use super::completion::{self, page_timeout};
use super::connmgr::{AcquireError, Lease};
use super::discovery::{self, DISCOVERY_TIMEOUT};
use super::link::{self, Link};
use super::status::{self, ANSWER_TIMEOUT};
use crate::pappl::{PM_PREASON_OFFLINE, PapplDeverrorCb, PapplDeviceCb};

/// How long opening a device may wait for a printer someone else is using,
/// and for each connection attempt.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// An open `btspp://` device: a lease on its printer's link.
#[derive(Debug)]
pub struct PmBtConnection {
    link: Lease<Link>,
    address: BdAddr,
}

/// Why a device could not be opened.
#[derive(Debug)]
enum OpenError {
    /// NULL, or not UTF-8.
    NoUri,
    /// Not a `btspp://` URI.
    Uri(String, ParseUriError),
    /// The printer could not be leased.
    Connect(BdAddr, AcquireError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoUri => f.write_str("Missing device URI."),
            Self::Uri(uri, err) => write!(f, "Bad device URI '{uri}': {err}."),
            Self::Connect(address, err) => write!(f, "Unable to connect to {address}: {err}."),
        }
    }
}

impl StdError for OpenError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::NoUri => None,
            Self::Uri(_, err) => Some(err),
            Self::Connect(_, err) => Some(err),
        }
    }
}

impl PmBtConnection {
    fn open(uri: Option<&str>) -> Result<Self, OpenError> {
        let uri = uri.ok_or(OpenError::NoUri)?;
        let BtUri { address, channel } = uri
            .parse()
            .map_err(|err| OpenError::Uri(uri.to_owned(), err))?;
        let link = link::pool()
            .acquire(address, channel, OPEN_TIMEOUT)
            .map_err(|err| OpenError::Connect(address, err))?;
        Ok(Self { link, address })
    }
}

/// List the paired Phomemo printers, for PAPPL's `list_cb`.
///
/// Calls `cb` for each printer until it returns `true`, and returns
/// whether it did. A printer whose model its name does not tell is listed
/// with a device ID that names none. If `BlueZ` cannot be asked, `err_cb`
/// says why; finding no printers is no error.
///
/// # Safety
///
/// `cb` and `err_cb` must be NULL or functions that may be called with
/// `data` and `err_data` respectively.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_list(
    cb: PapplDeviceCb,
    data: *mut c_void,
    err_cb: PapplDeverrorCb,
    err_data: *mut c_void,
) -> bool {
    let Some(cb) = cb else {
        return false;
    };
    match discovery::printers(DISCOVERY_TIMEOUT) {
        Ok(printers) => printers.iter().any(|printer| {
            let info = c_string(&printer.info());
            let uri = c_string(&printer.uri().to_string());
            let id = discovery::device_id(printer.model());
            // SAFETY: `cb` may be called with `data`; the strings are
            // NUL-terminated and outlive the call.
            unsafe { cb(info.as_ptr(), uri.as_ptr(), id.as_ptr(), data) }
        }),
        Err(err) => {
            if let Some(err_cb) = err_cb {
                let message = c_string(&format!("Unable to list Bluetooth printers: {err}."));
                // SAFETY: `err_cb` may be called with `err_data`; the
                // message is NUL-terminated and outlives the call.
                unsafe { err_cb(message.as_ptr(), err_data) };
            }
            false
        }
    }
}

/// Open a `btspp://` device, for PAPPL's `open_cb`: lease its printer's
/// link, connecting if there is none.
///
/// Returns the connection, to pass to the other `pm_bt_` functions and
/// finally to [`pm_bt_close`], or NULL with the reason in `message`.
///
/// # Safety
///
/// `uri` must be NULL or a NUL-terminated string, and `message` NULL or
/// writable for `message_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_open(
    uri: *const c_char,
    message: *mut c_char,
    message_size: usize,
) -> *mut PmBtConnection {
    // SAFETY: the caller passes NULL or a NUL-terminated string.
    let uri = unsafe { c_str(uri) };
    match PmBtConnection::open(uri) {
        Ok(connection) => Box::into_raw(Box::new(connection)),
        Err(err) => {
            // SAFETY: the caller passes NULL or a buffer of `message_size`.
            unsafe { write_c_string(message, message_size, &err.to_string()) };
            ptr::null_mut()
        }
    }
}

/// Close a device, for PAPPL's `close_cb`. The link stays open for the
/// next session until it has been idle for a while.
///
/// # Safety
///
/// `connection` must be NULL or a connection from [`pm_bt_open`], which
/// must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_close(connection: *mut PmBtConnection) {
    if !connection.is_null() {
        // SAFETY: `connection` came from `Box::into_raw` in `pm_bt_open`,
        // and the caller gives up its ownership here.
        drop(unsafe { Box::from_raw(connection) });
    }
}

/// Read what the printer sends, for PAPPL's `read_cb`: the number of bytes
/// read, 0 once the printer has closed the link, or -1 on an error or if
/// nothing arrives within the read timeout.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`], and
/// `buffer` NULL or writable for `bytes` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_read(
    connection: *const PmBtConnection,
    buffer: *mut u8,
    bytes: usize,
) -> ssize_t {
    // SAFETY: the caller passes NULL or a live connection.
    let Some(connection) = (unsafe { connection.as_ref() }) else {
        return -1;
    };
    if buffer.is_null() {
        return -1;
    }
    // SAFETY: `buffer` is writable for `bytes` bytes.
    let buffer = unsafe { slice::from_raw_parts_mut(buffer, bytes) };
    connection.link.read(buffer).map_or(-1, to_ssize)
}

/// Send all of `bytes` bytes to the printer, for PAPPL's `write_cb`:
/// `bytes`, or -1 on an error.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`], and
/// `buffer` NULL or readable for `bytes` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_write(
    connection: *const PmBtConnection,
    buffer: *const u8,
    bytes: usize,
) -> ssize_t {
    // SAFETY: the caller passes NULL or a live connection.
    let Some(connection) = (unsafe { connection.as_ref() }) else {
        return -1;
    };
    if buffer.is_null() {
        return -1;
    }
    // SAFETY: `buffer` is readable for `bytes` bytes.
    let data = unsafe { slice::from_raw_parts(buffer, bytes) };
    connection
        .link
        .write_all(data)
        .map_or(-1, |()| to_ssize(bytes))
}

/// The printer's status, for PAPPL's `status_cb`, as `pappl_preason_t`
/// bits; offline for NULL.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_status(connection: *const PmBtConnection) -> c_uint {
    // SAFETY: the caller passes NULL or a live connection.
    unsafe { connection.as_ref() }.map_or(PM_PREASON_OFFLINE, |connection| {
        status::query(&connection.link, ANSWER_TIMEOUT)
    })
}

/// The battery level the printer last reported, in percent, or -1 if it
/// has not reported one or `connection` is NULL.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_battery(connection: *const PmBtConnection) -> c_int {
    // SAFETY: the caller passes NULL or a live connection.
    unsafe { connection.as_ref() }
        .and_then(|connection| connection.link.battery())
        .map_or(-1, c_int::from)
}

/// Write the printer's IEEE 1284 device ID into `buffer`, for PAPPL's
/// `id_cb`: with the model, if the printer's Bluetooth name tells it, and
/// without one otherwise. Returns whether it fit.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`], and
/// `buffer` NULL or writable for `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_device_id(
    connection: *const PmBtConnection,
    buffer: *mut c_char,
    size: usize,
) -> bool {
    // SAFETY: the caller passes NULL or a live connection.
    let Some(connection) = (unsafe { connection.as_ref() }) else {
        return false;
    };
    let model = discovery::model_at(connection.address, DISCOVERY_TIMEOUT);
    let id = discovery::device_id(model).to_string_lossy();
    // SAFETY: the caller passes NULL or a buffer of `size` bytes.
    unsafe { write_c_string(buffer, size, &id) }
}

/// Take off the input that has arrived unasked, such as a late report from
/// an earlier job, before a job sends its first page; whether the link is
/// still sound.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_discard_input(connection: *const PmBtConnection) -> bool {
    // SAFETY: the caller passes NULL or a live connection.
    unsafe { connection.as_ref() }.is_some_and(|connection| connection.link.discard_input().is_ok())
}

/// Wait until the printer has reported `pages` pages printed, so that the
/// device is not closed on data still in flight. Each page may take as
/// long as one `longest_page` hundredths of a millimetre long
/// ([`page_timeout`]). Describes the outcome in `message` either way;
/// whether every page printed.
///
/// # Safety
///
/// `connection` must be NULL or a live connection from [`pm_bt_open`], and
/// `message` NULL or writable for `message_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_bt_wait_printed(
    connection: *const PmBtConnection,
    pages: c_uint,
    longest_page: c_uint,
    message: *mut c_char,
    message_size: usize,
) -> bool {
    let timeout = page_timeout(longest_page);
    // SAFETY: the caller passes NULL or a live connection.
    let result = unsafe { connection.as_ref() }
        .map(|connection| completion::wait_printed(&connection.link, pages, timeout));
    let (printed, text) = match result {
        Some(Ok(())) => (
            true,
            format!("The printer reported {pages} page(s) printed."),
        ),
        Some(Err(err)) => (false, format!("Unable to confirm the print: {err}.")),
        None => (
            false,
            "Unable to confirm the print: no connection.".to_owned(),
        ),
    };
    // SAFETY: the caller passes NULL or a buffer of `message_size` bytes.
    unsafe { write_c_string(message, message_size, &text) };
    printed
}

/// `text` as a C string, without any NUL it contains.
fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
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

/// Copy `text` into the `size`-byte buffer at `buffer`, NUL-terminated and
/// cut at a character boundary if it does not fit; whether all of it fit.
///
/// # Safety
///
/// `buffer` must be NULL or writable for `size` bytes.
unsafe fn write_c_string(buffer: *mut c_char, size: usize, text: &str) -> bool {
    let Some(capacity) = size.checked_sub(1).filter(|_| !buffer.is_null()) else {
        return false;
    };
    let mut len = text.len().min(capacity);
    while !text.is_char_boundary(len) {
        len -= 1;
    }
    // SAFETY: the caller passes a buffer writable for `size` bytes.
    let out = unsafe { slice::from_raw_parts_mut(buffer.cast::<u8>(), size) };
    out[..len].copy_from_slice(&text.as_bytes()[..len]);
    out[len] = 0;
    len == text.len()
}

/// A byte count as `ssize_t`; one that came from a slice always fits.
fn to_ssize(bytes: usize) -> ssize_t {
    ssize_t::try_from(bytes).unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(size: usize, text: &str) -> (bool, Vec<u8>) {
        let mut buffer = vec![0x7f_u8; size];
        // SAFETY: `buffer` is writable for `size` bytes.
        let fit = unsafe { write_c_string(buffer.as_mut_ptr().cast(), size, text) };
        (fit, buffer)
    }

    #[test]
    fn strings_are_copied_nul_terminated() {
        assert_eq!(written(4, "abc"), (true, b"abc\0".to_vec()));
        assert_eq!(written(5, "abc"), (true, b"abc\0\x7f".to_vec()));
    }

    #[test]
    fn strings_are_cut_to_fit_at_a_character_boundary() {
        assert_eq!(written(3, "abc"), (false, b"ab\0".to_vec()));
        // "ä" is two bytes: it does not fit in the one byte left.
        assert_eq!(written(3, "aä"), (false, b"a\0\x7f".to_vec()));
        assert_eq!(written(1, "abc"), (false, b"\0".to_vec()));
    }

    #[test]
    fn nothing_is_written_without_a_buffer() {
        // SAFETY: NULL and an empty buffer are never written to.
        unsafe {
            assert!(!write_c_string(ptr::null_mut(), 16, "abc"));
            assert!(!write_c_string(ptr::NonNull::dangling().as_ptr(), 0, "abc"));
        }
    }

    #[test]
    fn a_bad_uri_is_reported_without_connecting() {
        let mut message = [0 as c_char; 128];
        // SAFETY: a static C string and a buffer of its size.
        let connection =
            unsafe { pm_bt_open(c"usb://x".as_ptr(), message.as_mut_ptr(), message.len()) };
        assert!(connection.is_null());
        // SAFETY: `pm_bt_open` NUL-terminated the message.
        let message = unsafe { CStr::from_ptr(message.as_ptr()) };
        assert_eq!(
            message.to_str(),
            Ok("Bad device URI 'usb://x': not a btspp:// URI.")
        );
    }

    #[test]
    fn null_connections_are_refused() {
        let mut buffer = [0_u8; 8];
        let mut message = [0 as c_char; 64];
        // SAFETY: NULL connections are allowed; the buffers are valid.
        unsafe {
            assert_eq!(
                pm_bt_read(ptr::null(), buffer.as_mut_ptr(), buffer.len()),
                -1
            );
            assert_eq!(pm_bt_write(ptr::null(), buffer.as_ptr(), buffer.len()), -1);
            assert_eq!(pm_bt_status(ptr::null()), PM_PREASON_OFFLINE);
            assert_eq!(pm_bt_battery(ptr::null()), -1);
            assert!(!pm_bt_device_id(
                ptr::null(),
                message.as_mut_ptr(),
                message.len()
            ));
            assert!(!pm_bt_discard_input(ptr::null()));
            assert!(!pm_bt_wait_printed(
                ptr::null(),
                1,
                3000,
                message.as_mut_ptr(),
                message.len()
            ));
            pm_bt_close(ptr::null_mut());
        }
    }
}
