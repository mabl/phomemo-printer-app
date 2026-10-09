//! The C side of the raster driver.
//!
//! `c/driver.c`'s raster callbacks forward PAPPL's arguments here together
//! with [`PmOps`], the table of PAPPL functions the driver calls back. Each
//! call wraps them once in [`Ops`], which checks the pointers and reads the
//! print options the first time the driver asks for them - when a page
//! starts or ends, not for every line - and from there on the driver is the
//! safe Rust of [`Job`].

use std::cell::OnceCell;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort, c_void};
use std::io;
use std::ptr::{self, NonNull};
use std::slice;

use libc::ssize_t;

use super::{Error, Host, Job, JobContext, Log, PrintOptions, RasterHeader, Sent};
use crate::models::{MediaSize, Model, PmModel};
use crate::pappl::{LogLevel, PapplDevice, PapplJob, PapplPrOptions};

/// The PAPPL functions the driver calls back. Every entry is required.
#[repr(C)]
#[derive(Debug)]
pub struct PmOps {
    /// Write `bytes` bytes to the device: `papplDeviceWrite`.
    pub write: unsafe extern "C" fn(
        device: *mut PapplDevice,
        buffer: *const c_void,
        bytes: usize,
    ) -> ssize_t,
    /// Send buffered bytes to the device: `papplDeviceFlush`.
    pub flush: unsafe extern "C" fn(device: *mut PapplDevice),
    /// Whether the job has been canceled: `papplJobIsCanceled`.
    pub is_canceled: unsafe extern "C" fn(job: *mut PapplJob) -> bool,
    /// Log `message` for the job at `level`, a `pappl_loglevel_t`.
    pub log: unsafe extern "C" fn(job: *mut PapplJob, level: c_int, message: *const c_char),
    /// The fields of `options` the driver reads.
    pub options: unsafe extern "C" fn(options: *const PapplPrOptions) -> PmOptions,
}

/// The fields of a `pappl_pr_options_t` the driver reads.
#[repr(C)]
#[derive(Debug)]
pub struct PmOptions {
    /// `header.cupsWidth`.
    pub width: c_uint,
    /// `header.cupsHeight`.
    pub height: c_uint,
    /// `header.cupsBytesPerLine`.
    pub bytes_per_line: c_uint,
    /// `header.cupsBitsPerPixel`.
    pub bits_per_pixel: c_uint,
    /// `header.cupsColorSpace`.
    pub color_space: c_uint,
    /// `print_darkness`.
    pub print_darkness: c_int,
    /// `darkness_configured`.
    pub darkness_configured: c_int,
    /// `print_speed`.
    pub print_speed: c_int,
    /// `media.tracking`.
    pub media_tracking: c_ushort,
    /// `media.size_length`.
    pub media_length: c_int,
    /// `print_color_mode`.
    pub color_mode: c_uint,
    /// `print_content_optimize`.
    pub content_optimize: c_uint,
    /// The `phomemo-dither` vendor option, or NULL.
    pub dither: *const c_char,
    /// The `phomemo-compression` vendor option, or NULL.
    pub compression: *const c_char,
    /// `media.size_name`: never NULL from `c/driver.c`, it points into
    /// `options->media` and is valid during the callback only. NULL is
    /// read as no name.
    pub media_size_name: *const c_char,
    /// `media.size_width`.
    pub media_width: c_int,
    /// `header.cupsPageSize`, in points.
    pub cups_page_size: [f32; 2],
    /// `header.PageSize`, in points.
    pub page_size: [c_uint; 2],
    /// `header.HWResolution`, in dots per inch.
    pub resolution: [c_uint; 2],
    /// The `phomemo-overprint-vertical` vendor option, or NULL.
    pub overprint_vertical: *const c_char,
}

/// Bytes in [`PmJobContext::ready_size_name`]: `pappl_media_col_t`'s
/// `size_name`, which `c/driver.c` checks.
pub const PM_MEDIA_NAME_SIZE: usize = 64;

/// Bytes in a vendor option default in [`PmJobContext`], terminating NUL
/// included; a longer value is cut short.
pub const PM_VENDOR_VALUE_SIZE: usize = 32;

/// What the driver reads once per job, when it starts, copied by value:
/// the loaded media (`media_ready[0]`) and the printer's defaults for
/// vendor options, which PAPPL 1.4 does not apply to jobs itself.
///
/// The strings are copies, so nothing here points into PAPPL; each should
/// be NUL-terminated, but one that fills its array is read to its end.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PmJobContext {
    /// `media_ready[0].size_name`.
    pub ready_size_name: [c_char; PM_MEDIA_NAME_SIZE],
    /// `media_ready[0].size_width`.
    pub ready_width: c_int,
    /// `media_ready[0].size_length`.
    pub ready_length: c_int,
    /// `media_ready[0].tracking`.
    pub ready_tracking: c_ushort,
    /// The printer's `phomemo-overprint-vertical-default`; empty if none.
    pub overprint_vertical_default: [c_char; PM_VENDOR_VALUE_SIZE],
    /// The printer's `phomemo-dither-default`; empty if none.
    pub dither_default: [c_char; PM_VENDOR_VALUE_SIZE],
    /// The printer's `phomemo-compression-default`; empty if none.
    pub compression_default: [c_char; PM_VENDOR_VALUE_SIZE],
}

impl PmJobContext {
    /// The context as the driver keeps it.
    fn read(&self) -> JobContext {
        JobContext {
            ready_name: fixed_string(&self.ready_size_name).unwrap_or_default(),
            ready_size: MediaSize {
                width: self.ready_width,
                length: self.ready_length,
            },
            ready_tracking: self.ready_tracking,
            overprint_vertical_default: fixed_string(&self.overprint_vertical_default),
            dither_default: fixed_string(&self.dither_default),
            compression_default: fixed_string(&self.compression_default),
        }
    }
}

/// An owned copy of the string in `chars`: up to its first NUL, or all of
/// it if there is none; `None` if it is empty.
fn fixed_string(chars: &[c_char]) -> Option<String> {
    let bytes: Vec<u8> = chars
        .iter()
        .map(|&c| c.to_ne_bytes()[0])
        .take_while(|&byte| byte != 0)
        .collect();
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// The driver's state for one job, owned by C between callbacks
/// (`papplJobSetData`).
#[derive(Debug)]
pub struct PmJob(Job);

/// The PAPPL objects of one callback, as a [`Host`].
#[derive(Debug)]
pub struct Ops<'a> {
    vtable: &'a PmOps,
    job: NonNull<PapplJob>,
    device: NonNull<PapplDevice>,
    options: NonNull<PapplPrOptions>,
    /// The options, read on first use.
    snapshot: OnceCell<PrintOptions>,
}

impl Ops<'_> {
    /// Wrap a callback's PAPPL objects, or `None` if any pointer is NULL.
    ///
    /// # Safety
    ///
    /// `ops` must be NULL or point to a [`PmOps`] whose functions may be
    /// called with `job`, `options` and `device`, and each of those must be
    /// NULL or a PAPPL object that stays valid while the `Ops` exists.
    unsafe fn new(
        ops: *const PmOps,
        job: *mut PapplJob,
        options: *const PapplPrOptions,
        device: *mut PapplDevice,
    ) -> Option<Self> {
        // SAFETY: the caller passes NULL or a valid `PmOps`.
        let vtable = unsafe { ops.as_ref() }?;
        let job = NonNull::new(job)?;
        let device = NonNull::new(device)?;
        let options = NonNull::new(options.cast_mut())?;
        Some(Self {
            vtable,
            job,
            device,
            options,
            snapshot: OnceCell::new(),
        })
    }

    /// Read the print options the driver uses.
    fn read_options(&self) -> PrintOptions {
        // SAFETY: `options` is the callback's `pappl_pr_options_t`
        // (`Ops::new`).
        let raw = unsafe { (self.vtable.options)(self.options.as_ptr()) };
        // SAFETY: the snapshot's strings are NULL or NUL-terminated strings
        // owned by `options` - vendor option values and the media's name -
        // which outlives this call; they are copied here.
        let (dither, compression, overprint_vertical, media_name) = unsafe {
            (
                c_string(raw.dither),
                c_string(raw.compression),
                c_string(raw.overprint_vertical),
                c_string(raw.media_size_name),
            )
        };
        PrintOptions {
            raster: RasterHeader {
                width: to_usize(raw.width),
                height: to_usize(raw.height),
                bytes_per_line: to_usize(raw.bytes_per_line),
                bits_per_pixel: raw.bits_per_pixel,
                color_space: raw.color_space,
            },
            cups_page_size: raw.cups_page_size,
            page_size: raw.page_size,
            resolution: raw.resolution,
            media_name: media_name.unwrap_or_default(),
            media_width: raw.media_width,
            phomemo_overprint_vertical: overprint_vertical,
            print_darkness: raw.print_darkness,
            darkness_configured: raw.darkness_configured,
            print_speed: raw.print_speed,
            media_tracking: raw.media_tracking,
            media_length: raw.media_length,
            color_mode: raw.color_mode,
            content_optimize: raw.content_optimize,
            phomemo_dither: dither,
            phomemo_compression: compression,
        }
    }
}

impl Log for Ops<'_> {
    fn log(&self, level: LogLevel, message: &str) {
        // A NUL cannot cross into C; drop any rather than the message.
        let message = CString::new(message.replace('\0', "")).unwrap_or_default();
        // SAFETY: `job` is the callback's job (`Ops::new`), and `message` a
        // NUL-terminated string that outlives the call.
        unsafe { (self.vtable.log)(self.job.as_ptr(), level.raw(), message.as_ptr()) };
    }
}

impl io::Write for Ops<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `device` is the callback's device (`Ops::new`), and `buf`
        // is readable for `buf.len()` bytes.
        let written =
            unsafe { (self.vtable.write)(self.device.as_ptr(), buf.as_ptr().cast(), buf.len()) };
        usize::try_from(written).map_err(|_| io::Error::other("papplDeviceWrite failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        // SAFETY: `device` is the callback's device (`Ops::new`).
        unsafe { (self.vtable.flush)(self.device.as_ptr()) };
        Ok(())
    }
}

impl Host for Ops<'_> {
    fn options(&self) -> &PrintOptions {
        self.snapshot.get_or_init(|| self.read_options())
    }

    fn is_canceled(&self) -> bool {
        // SAFETY: `job` is the callback's job (`Ops::new`).
        unsafe { (self.vtable.is_canceled)(self.job.as_ptr()) }
    }
}

/// An owned copy of a C string, or `None` for NULL.
///
/// # Safety
///
/// `string` must be NULL or point to a NUL-terminated string.
unsafe fn c_string(string: *const c_char) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let string = unsafe { CStr::from_ptr(string) };
    Some(string.to_string_lossy().into_owned())
}

/// A raster dimension as a `usize`, saturating on targets where it does
/// not fit - which then fails as an oversized page.
fn to_usize(value: c_uint) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Run `step` on the job state of a callback, logging why it failed.
///
/// # Safety
///
/// `ctx` must be NULL or a live state from [`pm_job_start`], not otherwise
/// in use during the call; the rest as for [`Ops::new`].
unsafe fn run(
    ctx: *mut PmJob,
    ops: *const PmOps,
    job: *mut PapplJob,
    options: *const PapplPrOptions,
    device: *mut PapplDevice,
    step: impl FnOnce(&mut Job, &mut Ops<'_>) -> Result<(), Error>,
) -> bool {
    // SAFETY: forwarded from this function's contract.
    let Some(mut host) = (unsafe { Ops::new(ops, job, options, device) }) else {
        return false;
    };
    // SAFETY: `ctx` is NULL or a live, unaliased state.
    let Some(PmJob(state)) = (unsafe { ctx.as_mut() }) else {
        host.log(LogLevel::Error, "The raster job was not started.");
        return false;
    };
    let result = step(state, &mut host);
    report(&host, result)
}

/// Log a failed step; whether it succeeded.
fn report(log: &impl Log, result: Result<(), Error>) -> bool {
    match result {
        Ok(()) => true,
        Err(Error::Canceled) => {
            log.log(LogLevel::Info, "Stopping: the job was canceled.");
            false
        }
        Err(err) => {
            log.log(LogLevel::Error, &format!("Unable to print: {err}."));
            false
        }
    }
}

/// Start a raster job on `model`, for PAPPL's `rstartjob_cb`, with
/// `context`, which holds for all of the job's pages.
///
/// Returns the job's state, to pass to the other `pm_job_` functions and
/// finally to [`pm_job_end`], or NULL if the job cannot start. `model` is
/// only compared against the table's views, never dereferenced.
///
/// # Safety
///
/// `ops` must be NULL or point to a [`PmOps`] whose functions may be called
/// with `job`, `options` and `device`, each of which must be NULL or a
/// PAPPL object valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_job_start(
    model: *const PmModel,
    context: PmJobContext,
    ops: *const PmOps,
    job: *mut PapplJob,
    options: *const PapplPrOptions,
    device: *mut PapplDevice,
) -> *mut PmJob {
    // SAFETY: forwarded from this function's contract.
    let Some(host) = (unsafe { Ops::new(ops, job, options, device) }) else {
        return ptr::null_mut();
    };
    let Some(model) = Model::from_view(model) else {
        host.log(LogLevel::Error, "Unable to print: unknown printer model.");
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(PmJob(Job::new(model, context.read()))))
}

/// Start a page, for PAPPL's `rstartpage_cb`.
///
/// # Safety
///
/// `ctx` must be NULL or the live state from [`pm_job_start`]; the rest as
/// for [`pm_job_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_job_start_page(
    ctx: *mut PmJob,
    ops: *const PmOps,
    job: *mut PapplJob,
    options: *const PapplPrOptions,
    device: *mut PapplDevice,
) -> bool {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        run(ctx, ops, job, options, device, |state, host| {
            state.start_page(host)
        })
    }
}

/// Add the next line to the page, for PAPPL's `rwriteline_cb`.
///
/// # Safety
///
/// As for [`pm_job_start_page`]; `line` must be NULL or readable for
/// `cupsBytesPerLine` bytes of the header the page was started with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_job_write_line(
    ctx: *mut PmJob,
    ops: *const PmOps,
    job: *mut PapplJob,
    options: *const PapplPrOptions,
    device: *mut PapplDevice,
    line: *const u8,
) -> bool {
    let step = |state: &mut Job, host: &mut Ops<'_>| {
        let len = state.line_len().ok_or(Error::NoPage)?;
        let line = if line.is_null() {
            // An empty line, which the page refuses.
            &[]
        } else {
            // SAFETY: `line` is readable for the started page's line length.
            unsafe { slice::from_raw_parts(line, len) }
        };
        state.write_line(host, line)
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { run(ctx, ops, job, options, device, step) }
}

/// Finish the page and send it, for PAPPL's `rendpage_cb`.
///
/// # Safety
///
/// As for [`pm_job_start_page`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_job_end_page(
    ctx: *mut PmJob,
    ops: *const PmOps,
    job: *mut PapplJob,
    options: *const PapplPrOptions,
    device: *mut PapplDevice,
) -> bool {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        run(ctx, ops, job, options, device, |state, host| {
            state.end_page(host)
        })
    }
}

/// What a job has sent to the printer, which reports each page once
/// printed: what the Bluetooth backend waits for.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PmJobSent {
    /// Pages sent.
    pub pages: c_uint,
    /// The longest page's length on paper, in hundredths of a millimetre.
    pub longest_page: c_uint,
}

/// What the job has sent to the printer so far; nothing for NULL.
///
/// # Safety
///
/// `ctx` must be NULL or a live state from [`pm_job_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_job_sent(ctx: *const PmJob) -> PmJobSent {
    // SAFETY: the caller passes NULL or a live state.
    unsafe { ctx.as_ref() }.map_or_else(PmJobSent::default, |PmJob(state)| {
        let Sent {
            pages,
            longest_page,
        } = state.sent();
        PmJobSent {
            pages,
            longest_page,
        }
    })
}

/// Finish the job, for PAPPL's `rendjob_cb`, and free its state.
///
/// `ctx` is freed whatever the outcome and must not be used again.
///
/// # Safety
///
/// `ctx` must be NULL or the live state from [`pm_job_start`]; the rest as
/// for [`pm_job_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_job_end(
    ctx: *mut PmJob,
    ops: *const PmOps,
    job: *mut PapplJob,
    options: *const PapplPrOptions,
    device: *mut PapplDevice,
) -> bool {
    // SAFETY: `ctx` came from `Box::into_raw` in `pm_job_start`, and the
    // caller gives up its ownership here.
    let state = (!ctx.is_null()).then(|| unsafe { Box::from_raw(ctx) });
    // SAFETY: forwarded from this function's contract.
    let Some(mut host) = (unsafe { Ops::new(ops, job, options, device) }) else {
        return false;
    };
    // PAPPL ends a job a second time when ending it failed; by then the
    // state is gone and there is nothing left to do.
    let Some(state) = state else {
        host.log(LogLevel::Debug, "The raster job has already ended.");
        return false;
    };
    let result = state.0.end(&mut host);
    report(&host, result)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::pappl::{
        PM_CSPACE_SW, PM_LOGLEVEL_ERROR, PM_LOGLEVEL_INFO, PM_LOGLEVEL_WARN,
        PM_MEDIA_TRACKING_CONTINUOUS, PM_MEDIA_TRACKING_GAP,
    };

    thread_local! {
        static WRITTEN: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static LOGGED: RefCell<Vec<(c_int, String)>> = const { RefCell::new(Vec::new()) };
        static OPTIONS_READ: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "C" fn write(
        _: *mut PapplDevice,
        buffer: *const c_void,
        bytes: usize,
    ) -> ssize_t {
        // SAFETY: the driver passes a buffer readable for `bytes` bytes.
        let data = unsafe { slice::from_raw_parts(buffer.cast::<u8>(), bytes) };
        WRITTEN.with_borrow_mut(|written| written.extend_from_slice(data));
        ssize_t::try_from(bytes).expect("small write")
    }

    unsafe extern "C" fn flush(_: *mut PapplDevice) {}

    unsafe extern "C" fn is_canceled(_: *mut PapplJob) -> bool {
        false
    }

    unsafe extern "C" fn log(_: *mut PapplJob, level: c_int, message: *const c_char) {
        // SAFETY: the driver passes a NUL-terminated message.
        let message = unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned();
        LOGGED.with_borrow_mut(|logged| logged.push((level, message)));
    }

    /// A 16 x 2 dot gray label, with `phomemo-compression=off`.
    unsafe extern "C" fn options(_: *const PapplPrOptions) -> PmOptions {
        OPTIONS_READ.set(OPTIONS_READ.get() + 1);
        PmOptions {
            width: 16,
            height: 2,
            bytes_per_line: 16,
            bits_per_pixel: 8,
            color_space: PM_CSPACE_SW,
            print_darkness: 0,
            darkness_configured: 50,
            print_speed: 0,
            media_tracking: PM_MEDIA_TRACKING_GAP,
            media_length: 1000,
            color_mode: 0,
            content_optimize: 0,
            dither: ptr::null(),
            compression: c"off".as_ptr(),
            media_size_name: c"om_20x10mm_20x10mm".as_ptr(),
            media_width: 2000,
            cups_page_size: [0.0; 2],
            page_size: [57, 28],
            resolution: [203, 203],
            overprint_vertical: ptr::null(),
        }
    }

    /// A job context with nothing loaded.
    fn no_context() -> PmJobContext {
        PmJobContext {
            ready_size_name: [0; PM_MEDIA_NAME_SIZE],
            ready_width: 0,
            ready_length: 0,
            ready_tracking: 0,
            overprint_vertical_default: [0; PM_VENDOR_VALUE_SIZE],
            dither_default: [0; PM_VENDOR_VALUE_SIZE],
            compression_default: [0; PM_VENDOR_VALUE_SIZE],
        }
    }

    /// `string` in a C array of `N` characters, cut short to fit with no
    /// NUL if it is as long or longer.
    fn chars<const N: usize>(string: &str) -> [c_char; N] {
        let mut chars = [0; N];
        for (char, byte) in chars.iter_mut().zip(string.bytes()) {
            *char = c_char::from_ne_bytes([byte]);
        }
        chars
    }

    const OPS: PmOps = PmOps {
        write,
        flush,
        is_canceled,
        log,
        options,
    };

    /// Stand-ins for PAPPL's objects, which the fake callbacks never
    /// dereference.
    fn pappl_objects() -> (*mut PapplJob, *const PapplPrOptions, *mut PapplDevice) {
        (
            NonNull::dangling().as_ptr(),
            NonNull::dangling().as_ptr(),
            NonNull::dangling().as_ptr(),
        )
    }

    #[test]
    fn a_page_through_the_c_interface() {
        let model = Model::by_name("M220").expect("known model");
        let (job, options, device) = pappl_objects();
        let line = [0u8; 16];
        // SAFETY: the fake callbacks accept any pointers, `line` holds the
        // header's 16 bytes, and the state is used as `pm_job_start` returns
        // it and freed by `pm_job_end` only.
        unsafe {
            let ctx = pm_job_start(model.view(), no_context(), &OPS, job, options, device);
            assert!(!ctx.is_null());
            assert!(pm_job_start_page(ctx, &OPS, job, options, device));
            for _ in 0..2 {
                assert!(pm_job_write_line(
                    ctx,
                    &OPS,
                    job,
                    options,
                    device,
                    line.as_ptr()
                ));
            }
            assert!(pm_job_end_page(ctx, &OPS, job, options, device));
            assert!(pm_job_end(ctx, &OPS, job, options, device));
        }

        // Once to start the page and once to end it, not for every line.
        assert_eq!(OPTIONS_READ.take(), 2);
        let written = WRITTEN.take();
        assert_eq!(
            written,
            [
                0x1f, 0x11, 0x24, 70, // LEFT_MARGIN 72 - 2 bytes
                0x1b, 0x4e, 0x04, 8, // density 8
                0x1f, 0x11, 0x0a, // gap tracking
                0x1b, 0x40, // ESC @
                0x1f, 0x11, 0x21, 0x01, // one copy
                0x1d, 0x76, 0x30, 0x00, 2, 0, 2, 0, // 2 bytes x 2 rows
                0xff, 0xff, 0xff, 0xff, // all black
            ]
        );
        let errors: Vec<_> = LOGGED
            .take()
            .into_iter()
            .filter(|&(level, _)| level >= LogLevel::Warn.raw())
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn null_arguments_are_refused() {
        let model = Model::by_name("M220").expect("known model");
        let (job, options, device) = pappl_objects();
        // SAFETY: NULL is accepted for every pointer, and the fake
        // callbacks accept any others.
        unsafe {
            assert!(
                pm_job_start(
                    model.view(),
                    no_context(),
                    ptr::null(),
                    job,
                    options,
                    device
                )
                .is_null()
            );
            assert!(
                pm_job_start(model.view(), no_context(), &OPS, job, ptr::null(), device).is_null()
            );
            assert!(pm_job_start(ptr::null(), no_context(), &OPS, job, options, device).is_null());
            assert!(!pm_job_start_page(
                ptr::null_mut(),
                &OPS,
                job,
                options,
                device
            ));
            assert!(!pm_job_end_page(
                ptr::null_mut(),
                &OPS,
                job,
                options,
                device
            ));
            assert!(!pm_job_end(ptr::null_mut(), &OPS, job, options, device));
        }
        let logged = LOGGED.take();
        assert!(logged.iter().any(|&(level, _)| level == PM_LOGLEVEL_ERROR));
        OPTIONS_READ.take();
    }

    #[test]
    fn ending_an_ended_job_is_quiet() {
        let (job, options, device) = pappl_objects();
        // SAFETY: a NULL state is accepted, and the fake callbacks accept
        // any pointers.
        assert!(!unsafe { pm_job_end(ptr::null_mut(), &OPS, job, options, device) });
        let logged = LOGGED.take();
        assert!(logged.iter().all(|&(level, _)| level < PM_LOGLEVEL_WARN));
    }

    #[test]
    fn a_line_without_data_fails_the_page() {
        let model = Model::by_name("M220").expect("known model");
        let (job, options, device) = pappl_objects();
        // SAFETY: as in `a_page_through_the_c_interface`; a NULL line is
        // accepted.
        unsafe {
            let ctx = pm_job_start(model.view(), no_context(), &OPS, job, options, device);
            assert!(pm_job_start_page(ctx, &OPS, job, options, device));
            assert!(!pm_job_write_line(
                ctx,
                &OPS,
                job,
                options,
                device,
                ptr::null()
            ));
            assert!(pm_job_end(ctx, &OPS, job, options, device));
        }
        assert!(WRITTEN.take().is_empty());
        LOGGED.take();
        OPTIONS_READ.take();
    }

    /// A 44 x 34 mm overprint canvas, 352 x 272 dots, named only by its
    /// size (CUPS' driverless path), asking for `trailing`.
    unsafe extern "C" fn canvas_options(_: *const PapplPrOptions) -> PmOptions {
        PmOptions {
            width: 352,
            height: 272,
            bytes_per_line: 352,
            media_size_name: c"custom_44x34mm_44x34mm".as_ptr(),
            media_width: 4400,
            media_length: 3400,
            media_tracking: PM_MEDIA_TRACKING_CONTINUOUS,
            cups_page_size: [124.72, 96.38],
            page_size: [125, 96],
            overprint_vertical: c"trailing".as_ptr(),
            // SAFETY: `options` accepts any pointer.
            ..unsafe { options(ptr::null()) }
        }
    }

    /// The same page, without a media name or a usable vendor value.
    unsafe extern "C" fn unnamed_options(_: *const PapplPrOptions) -> PmOptions {
        PmOptions {
            media_size_name: ptr::null(),
            overprint_vertical: c"".as_ptr(),
            // SAFETY: `canvas_options` accepts any pointer.
            ..unsafe { canvas_options(ptr::null()) }
        }
    }

    const CANVAS_OPS: PmOps = PmOps {
        options: canvas_options,
        ..OPS
    };

    #[test]
    fn options_are_copied() {
        let (job, options, device) = pappl_objects();
        for (callback, name, vertical) in [
            (
                canvas_options as unsafe extern "C" fn(_) -> _,
                "custom_44x34mm_44x34mm",
                Some("trailing"),
            ),
            // An empty value is kept; `overprint_vertical` reads it as none.
            (unnamed_options, "", Some("")),
        ] {
            let ops = PmOps {
                options: callback,
                ..OPS
            };
            // SAFETY: the fake callbacks accept any pointers.
            let host = unsafe { Ops::new(&raw const ops, job, options, device) }.expect("non-NULL");
            let read = host.options();
            assert_eq!(read.media_name, name);
            assert_eq!(read.phomemo_overprint_vertical.as_deref(), vertical);
            assert_eq!(read.media_width, 4400);
            assert_eq!(read.media_length, 3400);
            // Copied exactly.
            let bits = |points: [f32; 2]| points.map(f32::to_bits);
            assert_eq!(bits(read.cups_page_size), bits([124.72, 96.38]));
            assert_eq!(read.page_size, [125, 96]);
            assert_eq!(read.resolution, [203, 203]);
            assert_eq!(read.page_points().map(bits), Some(bits([124.72, 96.38])));
        }
        OPTIONS_READ.take();
    }

    #[test]
    fn job_context_strings_are_copied_safely() {
        let context = PmJobContext {
            ready_size_name: chars("om_40x30mm_40x30mm"),
            ready_width: 4000,
            ready_length: 3000,
            ready_tracking: PM_MEDIA_TRACKING_GAP,
            overprint_vertical_default: chars("trailing"),
            dither_default: chars("floyd-steinberg"),
            compression_default: chars("off"),
        };
        assert_eq!(
            context.read(),
            JobContext {
                ready_name: "om_40x30mm_40x30mm".to_owned(),
                ready_size: MediaSize {
                    width: 4000,
                    length: 3000
                },
                ready_tracking: PM_MEDIA_TRACKING_GAP,
                overprint_vertical_default: Some("trailing".to_owned()),
                dither_default: Some("floyd-steinberg".to_owned()),
                compression_default: Some("off".to_owned()),
            }
        );
        // Empty strings are none.
        assert_eq!(no_context().read(), JobContext::default());
        // Arrays without a NUL are read to their end, and no further.
        let long = "x".repeat(100);
        let unterminated = PmJobContext {
            ready_size_name: chars(&long),
            overprint_vertical_default: chars(&long),
            dither_default: chars(&"d".repeat(100)),
            compression_default: chars(&"c".repeat(100)),
            ..no_context()
        };
        assert_eq!(
            unterminated.ready_size_name.last(),
            Some(&c_char::from_ne_bytes(*b"x"))
        );
        assert_eq!(
            unterminated.dither_default.last(),
            Some(&c_char::from_ne_bytes(*b"d"))
        );
        assert_eq!(
            unterminated.compression_default.last(),
            Some(&c_char::from_ne_bytes(*b"c"))
        );
        let read = unterminated.read();
        assert_eq!(read.ready_name, "x".repeat(PM_MEDIA_NAME_SIZE));
        assert_eq!(
            read.overprint_vertical_default,
            Some("x".repeat(PM_VENDOR_VALUE_SIZE))
        );
        assert_eq!(read.dither_default, Some("d".repeat(PM_VENDOR_VALUE_SIZE)));
        assert_eq!(
            read.compression_default,
            Some("c".repeat(PM_VENDOR_VALUE_SIZE))
        );
        // Bytes after a NUL are not part of the string.
        let mut after_nul = chars::<PM_VENDOR_VALUE_SIZE>("clip");
        after_nul[4] = 0;
        after_nul[5] = c_char::from_ne_bytes(*b"x");
        assert_eq!(fixed_string(&after_nul).as_deref(), Some("clip"));
        assert_eq!(fixed_string(&[]), None);
    }

    #[test]
    fn a_canvas_through_the_c_interface() {
        let model = Model::by_name("M220").expect("known model");
        let (job, options, device) = pappl_objects();
        let ops = &CANVAS_OPS;
        // 40 x 30 mm gap labels are loaded.
        let context = PmJobContext {
            ready_size_name: chars("om_40x30mm_40x30mm"),
            ready_width: 4000,
            ready_length: 3000,
            ready_tracking: PM_MEDIA_TRACKING_GAP,
            ..no_context()
        };
        let line = [255u8; 352];
        // SAFETY: as in `a_page_through_the_c_interface`, with 352-byte
        // lines.
        unsafe {
            let ctx = pm_job_start(model.view(), context, ops, job, options, device);
            assert!(!ctx.is_null());
            assert!(pm_job_start_page(ctx, ops, job, options, device));
            for _ in 0..272 {
                assert!(pm_job_write_line(
                    ctx,
                    ops,
                    job,
                    options,
                    device,
                    line.as_ptr()
                ));
            }
            assert!(pm_job_end_page(ctx, ops, job, options, device));
            assert!(pm_job_end(ctx, ops, job, options, device));
        }
        let written = WRITTEN.take();
        assert_eq!(
            written[..17],
            [
                0x1f, 0x11, 0x24, 30, // LEFT_MARGIN 72 - 42 bytes
                0x1b, 0x4e, 0x04, 8, // density 8
                0x1f, 0x11, 0x0a, // the loaded stock's gap tracking
                0x1b, 0x40, // ESC @
                0x1f, 0x11, 0x21, 0x01, // one copy
            ]
        );
        // The job's trailing policy: 42 bytes x 256 rows.
        assert_eq!(written[17..25], [0x1d, 0x76, 0x30, 0x00, 42, 0, 0, 1]);
        assert_eq!(written.len(), 25 + 42 * 256);
        let logged = LOGGED.take();
        assert!(
            logged
                .iter()
                .any(|(level, message)| *level == PM_LOGLEVEL_INFO
                    && message.contains("om_40x30mm-overprint-2mm_44x34mm")
                    && message.contains("trailing")),
            "{logged:?}"
        );
        OPTIONS_READ.take();
    }
}
