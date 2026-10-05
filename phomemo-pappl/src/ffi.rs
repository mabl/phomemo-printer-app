//! C-ABI types and exported functions.

use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::num::NonZeroU8;

use phomemo_protocol::bitmap::{GrayImage, MonoBitmap, Rotation};
use phomemo_protocol::commands::{Density, LeftMargin, Speed};
use phomemo_protocol::dither::{self, Algorithm};
use phomemo_protocol::job::{Preamble, Raster};

use crate::testpage;

/// Vtable of PAPPL accessor functions passed from C into every Rust callback.
/// Each function pointer is optional so we can gracefully handle NULL.
#[repr(C)]
pub struct PhomemoOps {
    // I/O (through PAPPL device)
    pub write: Option<extern "C" fn(device: *mut c_void, data: *const u8, len: usize) -> isize>,
    pub read: Option<extern "C" fn(device: *mut c_void, buf: *mut u8, len: usize) -> isize>,
    pub flush: Option<extern "C" fn(device: *mut c_void)>,

    // Job control
    pub log: Option<extern "C" fn(job: *mut c_void, level: c_int, msg: *const c_char)>,
    pub is_canceled: Option<extern "C" fn(job: *mut c_void) -> bool>,

    // Raster geometry (from cups_page_header2_t via options)
    pub bytes_per_line: Option<extern "C" fn(options: *const c_void) -> c_uint>,
    pub width_px: Option<extern "C" fn(options: *const c_void) -> c_uint>,
    pub height_px: Option<extern "C" fn(options: *const c_void) -> c_uint>,

    // Driver config
    pub get_print_darkness: Option<extern "C" fn(options: *const c_void) -> c_int>,
    pub get_print_speed: Option<extern "C" fn(options: *const c_void) -> c_int>,
    pub get_media_tracking: Option<extern "C" fn(options: *const c_void) -> c_uint>,
    pub get_orientation: Option<extern "C" fn(options: *const c_void) -> c_int>,
    pub get_copies: Option<extern "C" fn(options: *const c_void) -> c_uint>,

    // Raster metadata (for dithering decisions)
    pub get_bits_per_pixel: Option<extern "C" fn(options: *const c_void) -> c_uint>,
    pub get_color_space: Option<extern "C" fn(options: *const c_void) -> c_uint>,
    pub get_content_optimize: Option<extern "C" fn(options: *const c_void) -> c_uint>,
    pub get_vendor_option:
        Option<extern "C" fn(options: *const c_void, name: *const c_char) -> *const c_char>,
    pub model_supports_compression: Option<extern "C" fn(job: *mut c_void) -> bool>,
}

/// Per-job driver context, heap-allocated and owned by the C side
/// via `papplJobSetData`.
pub struct DriverCtx {
    /// Print-head width in bytes (e.g. 72 for M220's 576 px head).
    /// Set once at allocation; raster rows are padded/clipped to this.
    head_width_bytes: usize,
    width_bytes: usize,
    /// Width in pixels (for grayscale: `width_px == width_bytes`).
    width_px: usize,
    height: usize,
    /// Bits per pixel of incoming raster (1 or 8).
    bits_per_pixel: u32,
    page: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

unsafe fn log_msg(ops: &PhomemoOps, job: *mut c_void, level: c_int, msg: &str) {
    if let Some(log_fn) = ops.log {
        if let Ok(c) = CString::new(msg) {
            log_fn(job, level, c.as_ptr());
        }
    }
}

unsafe fn write_all_fd(fd: c_int, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let wrote = libc::write(fd, data.as_ptr().cast(), data.len());
        if wrote < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }

        let Ok(wrote_len) = usize::try_from(wrote) else {
            return false;
        };
        if wrote_len == 0 {
            return false;
        }

        data = &data[wrote_len..];
    }

    true
}

#[no_mangle]
/// Write a generated PNG test page to an existing file descriptor.
///
/// # Safety
///
/// `fd` must be a valid writable file descriptor.
pub unsafe extern "C" fn pm_write_testpage_png(fd: c_int) -> bool {
    if fd < 0 {
        return false;
    }

    let Ok(data) = testpage::encode_test_page_png() else {
        return false;
    };

    write_all_fd(fd, &data)
}

// ---------------------------------------------------------------------------
// Dither algorithm resolution
// ---------------------------------------------------------------------------

/// PAPPL content-optimize bit flags.
const PAPPL_CONTENT_TEXT: u32 = 0x08;
const PAPPL_CONTENT_TEXT_AND_GRAPHIC: u32 = 0x10;

/// Resolve the dithering algorithm from vendor options and content hints.
///
/// Priority:
/// 1. Explicit `phomemo-dither` vendor option (e.g. "floyd-steinberg")
/// 2. `auto` mode based on `print-content-optimize`:
///    - text/text-and-graphic → Threshold (sharp edges)
///    - photo/graphic/auto → `FloydSteinberg`
/// 3. Default: `None` (caller picks `FloydSteinberg`)
unsafe fn resolve_dither_algorithm(ops: &PhomemoOps, options: *const c_void) -> Option<Algorithm> {
    // Check explicit vendor option.
    if let Some(get_vendor) = ops.get_vendor_option {
        let key = c"phomemo-dither";
        let val = get_vendor(options, key.as_ptr());
        if !val.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr(val).to_str() {
                if let Ok(algo) = s.parse() {
                    return Some(algo);
                }
                if s == "auto" {
                    // Fall through to content-based selection.
                } else {
                    // Unknown name — fall through to default.
                }
            }
        }
    }

    // Content-based auto selection.
    if let Some(f) = ops.get_content_optimize {
        let content = f(options);
        if content & (PAPPL_CONTENT_TEXT | PAPPL_CONTENT_TEXT_AND_GRAPHIC) != 0 {
            return Some(Algorithm::Threshold);
        }
    }

    None // caller defaults to FloydSteinberg
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompressionMode {
    Auto,
    On,
    Off,
}

const fn orientation_rotation(value: c_int) -> Rotation {
    match value {
        // 4 = landscape (90° counter-clockwise)
        4 => Rotation::CounterClockwise,
        // 5 = reverse-landscape (90° clockwise)
        5 => Rotation::Clockwise,
        // 6 = reverse-portrait (180°)
        6 => Rotation::HalfTurn,
        _ => Rotation::Identity,
    }
}

unsafe fn resolve_compression_mode(ops: &PhomemoOps, options: *const c_void) -> CompressionMode {
    if let Some(get_vendor) = ops.get_vendor_option {
        let key = c"phomemo-compression";
        let val = get_vendor(options, key.as_ptr());
        if !val.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr(val).to_str() {
                match s.trim().to_ascii_lowercase().as_str() {
                    "on" => return CompressionMode::On,
                    "off" => return CompressionMode::Off,
                    "auto" => return CompressionMode::Auto,
                    _ => {}
                }
            }
        }
    }
    CompressionMode::Auto
}

// ---------------------------------------------------------------------------
// Raster callback exports
// ---------------------------------------------------------------------------

/// Allocate a new driver context.
///
/// `head_width_bytes` is the physical print-head width in bytes
/// (e.g. 72 for a 576-pixel head).  Raster rows will be
/// padded or clipped to this width before transmission.
#[no_mangle]
pub extern "C" fn pm_ctx_new(head_width_bytes: c_uint) -> *mut DriverCtx {
    Box::into_raw(Box::new(DriverCtx {
        head_width_bytes: usize::try_from(head_width_bytes).unwrap_or(usize::MAX),
        width_bytes: 0,
        width_px: 0,
        height: 0,
        bits_per_pixel: 1,
        page: Vec::new(),
    }))
}

#[no_mangle]
///
/// # Safety
///
/// `ctx` must be null or a pointer previously returned by `pm_ctx_new`.
pub unsafe extern "C" fn pm_ctx_free(ctx: *mut DriverCtx) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

#[no_mangle]
///
/// # Safety
///
/// `ctx_ptr`, `ops`, and `options` must be valid pointers for the duration of
/// this call.
pub unsafe extern "C" fn pm_start_job(
    ctx_ptr: *mut DriverCtx,
    ops: *const PhomemoOps,
    job: *mut c_void,
    options: *const c_void,
    device: *mut c_void,
) -> bool {
    if ops.is_null() || ctx_ptr.is_null() || options.is_null() {
        return false;
    }
    let ops = &*ops;
    let ctx = &*ctx_ptr;
    let Some(write_fn) = ops.write else {
        log_msg(ops, job, 3, "pm_start_job: missing write callback");
        return false;
    };

    // Reference mobile-app sequence: LEFT_MARGIN → ESC @ → (settle) → ...
    let head_w = ctx.head_width_bytes;
    let Some(bpl_fn) = ops.bytes_per_line else {
        log_msg(ops, job, 3, "pm_start_job: missing bytes_per_line callback");
        return false;
    };
    let raster_w = usize::try_from(bpl_fn(options)).unwrap_or(usize::MAX);
    let Ok(left_margin) = LeftMargin::for_width(head_w, raster_w) else {
        log_msg(ops, job, 3, "pm_start_job: left margin exceeds one byte");
        return false;
    };

    // Density (1-15, from PAPPL darkness setting)
    let darkness = ops.get_print_darkness.map_or(0, |f| f(options));
    let density = if darkness > 0 {
        u8::try_from(darkness.clamp(1, 15))
            .ok()
            .and_then(Density::new)
    } else {
        None
    };

    // Speed (PAPPL speed in hundredths of mm/sec → Phomemo level 1-6)
    let speed = ops.get_print_speed.map_or(0, |f| f(options));
    let speed = if speed > 0 {
        u8::try_from((speed / 2540).clamp(1, 6))
            .ok()
            .and_then(Speed::new)
    } else {
        None
    };

    // Media tracking
    let tracking =
        crate::media_tracking_from_pappl(ops.get_media_tracking.map_or(0, |f| f(options)));

    let preamble = Preamble {
        left_margin,
        density,
        speed,
        tracking,
    }
    .encode();

    let wrote = write_fn(device, preamble.as_ptr(), preamble.len());
    if wrote < 0 {
        log_msg(ops, job, 3, "pm_start_job: preamble write failed");
        return false;
    }
    if let Some(flush_fn) = ops.flush {
        flush_fn(device);
    }

    // Brief settle after reset — the printer needs a moment to
    // re-initialize before accepting print commands.
    std::thread::sleep(std::time::Duration::from_millis(100));

    true
}

#[no_mangle]
///
/// # Safety
///
/// `ctx`, `ops`, and `options` must be valid pointers for the duration of this
/// call.
pub unsafe extern "C" fn pm_start_page(
    ctx: *mut DriverCtx,
    ops: *const PhomemoOps,
    _job: *mut c_void,
    options: *const c_void,
    _device: *mut c_void,
    _page: c_uint,
) -> bool {
    if ctx.is_null() || ops.is_null() || options.is_null() {
        return false;
    }
    let ctx = &mut *ctx;
    let ops = &*ops;

    let Some(bpl_fn) = ops.bytes_per_line else {
        return false;
    };
    ctx.width_bytes = usize::try_from(bpl_fn(options)).unwrap_or(usize::MAX);
    ctx.width_px = ops.width_px.map_or(ctx.width_bytes * 8, |f| {
        usize::try_from(f(options)).unwrap_or(usize::MAX)
    });
    ctx.bits_per_pixel = ops.get_bits_per_pixel.map_or(1, |f| f(options));
    ctx.height = 0;
    ctx.page.clear();

    true
}

#[no_mangle]
///
/// # Safety
///
/// `ctx`, `ops`, and `line` must be valid pointers. `line` must contain at
/// least `ctx.width_bytes` readable bytes.
pub unsafe extern "C" fn pm_write_line(
    ctx: *mut DriverCtx,
    ops: *const PhomemoOps,
    job: *mut c_void,
    _options: *const c_void,
    _device: *mut c_void,
    _y: c_uint,
    line: *const u8,
) -> bool {
    if ctx.is_null() || ops.is_null() || line.is_null() {
        return false;
    }
    let ctx = &mut *ctx;
    let ops = &*ops;

    let is_canceled = ops.is_canceled.is_some_and(|f| f(job));
    if is_canceled {
        return false;
    }

    let src = std::slice::from_raw_parts(line, ctx.width_bytes);
    ctx.page.extend_from_slice(src);
    ctx.height += 1;
    true
}

#[no_mangle]
///
/// # Safety
///
/// `ctx` and `ops` must be valid pointers. `device` and `options` must remain
/// valid for callback invocations during this call.
#[allow(clippy::too_many_lines)]
pub unsafe extern "C" fn pm_end_page(
    ctx: *mut DriverCtx,
    ops: *const PhomemoOps,
    job: *mut c_void,
    options: *const c_void,
    device: *mut c_void,
    _page: c_uint,
) -> bool {
    if ctx.is_null() || ops.is_null() {
        return false;
    }
    let ctx = &mut *ctx;
    let ops = &*ops;
    let Some(write_fn) = ops.write else {
        return false;
    };

    let head_w = ctx.head_width_bytes;
    let src_w = ctx.width_bytes;
    let bpp = ctx.bits_per_pixel;
    let width_px = ctx.width_px;

    // --- Determine dither algorithm ---
    let dither_algo = resolve_dither_algorithm(ops, options);

    // --- Convert to 1bpp if input is grayscale ---
    let bitmap = if bpp >= 8 {
        // Input is 8-bit grayscale: width_bytes == width_px.
        // Dither to 1bpp.
        let algo = dither_algo.unwrap_or(Algorithm::FloydSteinberg);
        let pixel_w = width_px.min(head_w * 8);
        // Collect grayscale pixels (one byte per pixel, clipped to head width).
        let mut gray_pixels = Vec::with_capacity(pixel_w * ctx.height);
        for row in 0..ctx.height {
            let row_start = row * src_w;
            let row_end = (row_start + pixel_w).min(ctx.page.len());
            gray_pixels.extend_from_slice(&ctx.page[row_start..row_end]);
            // Pad short rows
            if row_end - row_start < pixel_w {
                gray_pixels.resize(gray_pixels.len() + pixel_w - (row_end - row_start), 255);
            }
        }

        log_msg(
            ops,
            job,
            0,
            &format!("Dithering {pixel_w}x{} with {algo:?}", ctx.height),
        );

        match GrayImage::new(pixel_w, ctx.height, gray_pixels) {
            Ok(gray) => dither::dither(&gray, algo),
            Err(err) => {
                log_msg(ops, job, 3, &format!("pm_end_page: {err}"));
                return false;
            }
        }
    } else {
        // Input is already 1bpp packed: clip it to the head width.
        let bmp_w = src_w.min(head_w);
        let pixel_w = width_px.min(bmp_w * 8);
        let Some(src_px) = src_w.checked_mul(8) else {
            log_msg(ops, job, 3, "pm_end_page: raster width overflows");
            return false;
        };
        match MonoBitmap::new(src_px, ctx.height, std::mem::take(&mut ctx.page)) {
            Ok(bitmap) => bitmap.clip_width(pixel_w),
            Err(err) => {
                log_msg(ops, job, 3, &format!("pm_end_page: {err}"));
                return false;
            }
        }
    };

    // --- Orientation transform (on the packed 1bpp data) ---
    let orientation = ops.get_orientation.map_or(0, |f| f(options));
    let bitmap = bitmap.rotate(orientation_rotation(orientation));
    let uncompressed = match Raster::uncompressed(&bitmap) {
        Ok(raster) => raster,
        Err(err) => {
            log_msg(ops, job, 3, &format!("pm_end_page: {err}"));
            return false;
        }
    };

    let compression_mode = resolve_compression_mode(ops, options);
    let supports_compression = ops.model_supports_compression.is_none_or(|f| f(job));

    let raster = if compression_mode == CompressionMode::Off {
        uncompressed
    } else if !supports_compression {
        if compression_mode == CompressionMode::On {
            log_msg(
                ops,
                job,
                1,
                "Compression forced on but model does not support it; using raw bitmap",
            );
        }
        uncompressed
    } else {
        match Raster::compressed(&bitmap) {
            Ok(compressed)
                if compression_mode == CompressionMode::On
                    || compressed.payload_len() < uncompressed.payload_len() =>
            {
                // Log compression ratio (precision loss acceptable for log message).
                #[allow(clippy::cast_precision_loss)]
                let ratio = 100.0 * (compressed.payload_len() as f64)
                    / (uncompressed.payload_len().max(1) as f64);
                log_msg(
                    ops,
                    job,
                    0,
                    &format!(
                        "LZO: {}B -> {}B ({ratio:.0}%) [{compression_mode:?}]",
                        uncompressed.payload_len(),
                        compressed.payload_len(),
                    ),
                );
                compressed
            }
            Ok(_) => uncompressed,
            Err(err) => {
                log_msg(ops, job, 1, &format!("LZO compression failed: {err}"));
                uncompressed
            }
        }
    };

    // --- Assemble the wire packet: copies, then the raster ---
    let copies = ops.get_copies.map_or(1, |f| f(options));
    let copies = NonZeroU8::new(u8::try_from(copies).unwrap_or(u8::MAX)).unwrap_or(NonZeroU8::MIN);
    let out = raster.encode(copies);

    let wrote = write_fn(device, out.as_ptr(), out.len());
    if wrote < 0 {
        log_msg(ops, job, 3, "pm_end_page: device write failed");
        return false;
    }

    if let Some(flush_fn) = ops.flush {
        flush_fn(device);
    }

    true
}

#[no_mangle]
///
/// # Safety
///
/// `ops` must be a valid pointer and `device` must remain valid for callback
/// invocations during this call.
pub unsafe extern "C" fn pm_end_job(
    _ctx: *mut DriverCtx,
    ops: *const PhomemoOps,
    _job: *mut c_void,
    _options: *const c_void,
    device: *mut c_void,
) -> bool {
    let ops = &*ops;

    // No end-document command — the reference app flow doesn't send one.
    // The printer auto-completes and responds with 1A 0F 0C.
    // The completion read is handled on the C side (tp_rendjob)
    // with a temporarily increased SO_RCVTIMEO.

    if let Some(flush_fn) = ops.flush {
        flush_fn(device);
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_mapping_matches_ipp_values() {
        assert_eq!(orientation_rotation(0), Rotation::Identity);
        assert_eq!(orientation_rotation(3), Rotation::Identity);
        assert_eq!(orientation_rotation(4), Rotation::CounterClockwise);
        assert_eq!(orientation_rotation(5), Rotation::Clockwise);
        assert_eq!(orientation_rotation(6), Rotation::HalfTurn);
    }
}
