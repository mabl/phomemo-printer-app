//! Overprint label profiles: design canvases larger than the physical label.
//!
//! A profile lets a design reach past a label's edges (bleed), so that the
//! label is covered even when it sits slightly off position. The design is
//! laid out on a *canvas* - the stock plus the bleed on every side - and
//! printed 1:1, anchored by the label's left edge rather than by the canvas
//! (`docs/overprint-plan.md`, sections 1 to 3).
//!
//! This module holds the pure parts of that:
//!
//! - [`OverprintProfile`]: the hand-written table of profiles, separate from
//!   the media catalog, which lists physical stock only (plan, D1). A
//!   canvas has a real, self-describing PWG name whose dimension part is
//!   the canvas size (D2), so every client sees the canvas as a page.
//! - [`resolve`]: whether a job uses a profile, from its media name and
//!   size, its raster header's page size and the loaded media (D3), which
//!   survives CUPS' driverless path dropping the canvas name.
//! - [`Geometry`]: which canvas columns and rows reach which head dots, in
//!   integer dots (plan, section 2): the label's left edge lands on the
//!   head dot an ordinary job's first column does, and the right bleed is
//!   clipped (D5).
//! - [`VerticalPolicy`]: which canvas rows are sent along the feed.
//! - [`Canvas`] and the `pm_overprint_*` functions: a model's canvases as
//!   `c/driver.c` advertises them and `c/media.c` shows them.

use std::ffi::{CStr, CString, c_char, c_int, c_uint};
use std::fmt::{self, Write as _};
use std::ops::Range;
use std::ptr;
use std::str::FromStr;

use crate::media;
use crate::models::{MediaSize, Model, PmModel, millimetres};
use crate::pappl::LogLevel;

/// How far two sizes may differ, in hundredths of a millimetre per axis,
/// and still be the same size: CUPS' own `_PWG_EPSILON`, 0.5 mm.
pub const SIZE_TOLERANCE: u32 = 50;

/// Bleed on each side of the stock, in hundredths of a millimetre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bleed {
    /// At the start of each row, across the head.
    pub left: c_int,
    /// Before the label, along the feed.
    pub top: c_int,
    /// At the end of each row, across the head.
    pub right: c_int,
    /// After the label, along the feed.
    pub bottom: c_int,
}

impl Bleed {
    /// The same bleed `hundredths` of a millimetre on every side.
    #[must_use]
    pub const fn uniform(hundredths: c_int) -> Self {
        Self {
            left: hundredths,
            top: hundredths,
            right: hundredths,
            bottom: hundredths,
        }
    }

    /// The bleed, if it is the same on every side.
    #[must_use]
    pub const fn as_uniform(&self) -> Option<c_int> {
        if self.left == self.top && self.left == self.right && self.left == self.bottom {
            Some(self.left)
        } else {
            None
        }
    }
}

/// A design canvas for one model and one physical stock.
#[derive(Debug, PartialEq, Eq)]
pub struct OverprintProfile {
    /// The model the profile is for, e.g. `M220`.
    pub model: &'static str,
    /// The stock's PWG name in the media catalog, e.g. `om_40x30mm_40x30mm`.
    pub stock_name: &'static str,
    /// The physical stock, in hundredths of a millimetre.
    pub stock: MediaSize,
    /// The bleed around the stock in the design.
    pub bleed: Bleed,
    /// Human-readable name, e.g. `40 x 30 mm + 2 mm overprint`.
    pub label: &'static str,
}

/// Every profile; [`Model::overprint_profiles`] picks a model's.
static PROFILES: [OverprintProfile; 1] = [OverprintProfile {
    model: "M220",
    stock_name: "om_40x30mm_40x30mm",
    stock: MediaSize {
        width: 4000,
        length: 3000,
    },
    bleed: Bleed::uniform(200),
    label: "40 x 30 mm + 2 mm overprint",
}];

impl OverprintProfile {
    /// Every profile, for every model.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &PROFILES
    }

    /// The canvas: the stock and the bleed on every side, in hundredths of
    /// a millimetre.
    #[must_use]
    pub const fn canvas(&self) -> MediaSize {
        MediaSize {
            width: self
                .stock
                .width
                .saturating_add(self.bleed.left)
                .saturating_add(self.bleed.right),
            length: self
                .stock
                .length
                .saturating_add(self.bleed.top)
                .saturating_add(self.bleed.bottom),
        }
    }

    /// The canvas's PWG name (plan, D2):
    /// `om_<stock w>x<stock l>mm-overprint-<bleed>mm_<canvas w>x<canvas l>mm`,
    /// e.g. `om_40x30mm-overprint-2mm_44x34mm`. A bleed that differs by
    /// side is named `<left>-<top>-<right>-<bottom>mm`.
    ///
    /// The dimension part after the last `_` is the canvas size, so every
    /// client that reads the name sees the canvas; the name part contains
    /// no `_`.
    #[must_use]
    pub fn canvas_name(&self) -> String {
        let bleed = self.bleed.as_uniform().map_or_else(
            || {
                format!(
                    "{}-{}-{}-{}",
                    millimetres(self.bleed.left),
                    millimetres(self.bleed.top),
                    millimetres(self.bleed.right),
                    millimetres(self.bleed.bottom)
                )
            },
            millimetres,
        );
        let canvas = self.canvas();
        format!(
            "om_{}x{}mm-overprint-{bleed}mm_{}x{}mm",
            millimetres(self.stock.width),
            millimetres(self.stock.length),
            millimetres(canvas.width),
            millimetres(canvas.length)
        )
    }

    /// The canvas's width in dots at `dpi`, rounded to nearest as
    /// [`Geometry`] rounds the bleed: what a raster of the canvas at `dpi`
    /// and 100 % is wide, give or take a dot. `None` if it does not fit.
    #[must_use]
    pub fn canvas_width_dots(&self, dpi: u16) -> Option<usize> {
        usize::try_from((i64::from(self.canvas().width) * i64::from(dpi) + 1270) / 2540).ok()
    }

    /// Whether `size_name` is this profile's canvas name, ignoring ASCII
    /// case and surrounding whitespace.
    #[must_use]
    pub fn is_canvas_name(&self, size_name: &str) -> bool {
        size_name.trim().eq_ignore_ascii_case(&self.canvas_name())
    }

    /// Where the label sits on the canvas, for the media page: `design on
    /// 44 x 34 mm; the label is 2 mm from the left and 2 mm from the top;
    /// the right 2 mm is not printed`. The right bleed is never printed
    /// ([`Geometry`] ends the bitmap with the label, D5).
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = format!(
            "design on {}; the label is {} mm from the left and {} mm from the top",
            describe(self.canvas()),
            millimetres(self.bleed.left),
            millimetres(self.bleed.top),
        );
        if self.bleed.right > 0 {
            let _ = write!(
                summary,
                "; the right {} mm is not printed",
                millimetres(self.bleed.right)
            );
        }
        summary
    }
}

/// A profile as a model advertises it, with the C strings `c/driver.c` and
/// `c/media.c` are handed, which the model table owns for the process's
/// lifetime ([`Model::canvases`]).
#[derive(Debug)]
pub struct Canvas {
    profile: &'static OverprintProfile,
    name: CString,
    label: CString,
    stock_name: CString,
    summary: CString,
}

impl Canvas {
    /// The canvas of `profile`; `None` if a string holds a NUL.
    #[must_use]
    pub fn new(profile: &'static OverprintProfile) -> Option<Self> {
        Some(Self {
            profile,
            name: CString::new(profile.canvas_name()).ok()?,
            label: CString::new(profile.label).ok()?,
            stock_name: CString::new(profile.stock_name).ok()?,
            summary: CString::new(profile.summary()).ok()?,
        })
    }

    /// The profile.
    #[must_use]
    pub const fn profile(&self) -> &'static OverprintProfile {
        self.profile
    }

    /// The canvas's PWG name ([`OverprintProfile::canvas_name`]).
    #[must_use]
    pub fn name(&self) -> &CStr {
        &self.name
    }

    /// The canvas as C sees it.
    #[must_use]
    pub fn info(&self) -> PmOverprintInfo {
        let OverprintProfile { stock, bleed, .. } = self.profile;
        let canvas = self.profile.canvas();
        PmOverprintInfo {
            canvas_name: self.name.as_ptr(),
            label: self.label.as_ptr(),
            stock_name: self.stock_name.as_ptr(),
            summary: self.summary.as_ptr(),
            canvas_width: canvas.width,
            canvas_length: canvas.length,
            stock_width: stock.width,
            stock_length: stock.length,
            bleed_left: bleed.left,
            bleed_top: bleed.top,
            bleed_right: bleed.right,
            bleed_bottom: bleed.bottom,
        }
    }
}

/// One of a model's overprint canvases, for `c/media.c`.
///
/// Every pointer is to a NUL-terminated string owned by the model table,
/// which lives as long as the process; sizes are in hundredths of a
/// millimetre.
#[repr(C)]
#[derive(Debug)]
pub struct PmOverprintInfo {
    /// The canvas's PWG name, e.g. `om_40x30mm-overprint-2mm_44x34mm`.
    pub canvas_name: *const c_char,
    /// Human-readable name, e.g. `40 x 30 mm + 2 mm overprint`.
    pub label: *const c_char,
    /// The stock's PWG name, e.g. `om_40x30mm_40x30mm`.
    pub stock_name: *const c_char,
    /// Where the label sits on the canvas ([`OverprintProfile::summary`]).
    pub summary: *const c_char,
    /// The canvas's width.
    pub canvas_width: c_int,
    /// The canvas's length.
    pub canvas_length: c_int,
    /// The stock's width.
    pub stock_width: c_int,
    /// The stock's length.
    pub stock_length: c_int,
    /// Bleed at the start of each row.
    pub bleed_left: c_int,
    /// Bleed before the label.
    pub bleed_top: c_int,
    /// Bleed at the end of each row, which is not printed.
    pub bleed_right: c_int,
    /// Bleed after the label.
    pub bleed_bottom: c_int,
}

/// `model`'s canvas at `index`, if `model` is one of the table's views
/// (only compared against them, never dereferenced).
fn model_canvas(model: *const PmModel, index: c_uint) -> Option<&'static Canvas> {
    Model::from_view(model)?
        .canvases()
        .get(usize::try_from(index).ok()?)
}

/// Number of overprint canvases `model` offers; 0 if `model` is not one
/// of the table's views, which are the only pointers it is compared
/// against, never dereferenced.
#[unsafe(no_mangle)]
pub extern "C" fn pm_overprint_count(model: *const PmModel) -> c_uint {
    Model::from_view(model).map_or(0, |model| {
        c_uint::try_from(model.canvases().len()).unwrap_or(c_uint::MAX)
    })
}

/// Fill `out` with `model`'s canvas at `index`, in the order of the media
/// list. Returns false, leaving `out` untouched, if `model` is not one of
/// the table's views, `index >= pm_overprint_count(model)`, or `out` is
/// NULL.
///
/// # Safety
///
/// `out` must be NULL or valid for writing a `PmOverprintInfo`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_overprint_get(
    model: *const PmModel,
    index: c_uint,
    out: *mut PmOverprintInfo,
) -> bool {
    let Some(canvas) = model_canvas(model, index) else {
        return false;
    };
    if out.is_null() {
        return false;
    }
    // SAFETY: `out` is valid for writes; `write` does not read or drop
    // whatever was there.
    unsafe { out.write(canvas.info()) };
    true
}

/// The index of `model`'s canvas named `size_name`, ignoring ASCII case
/// ([`OverprintProfile::is_canvas_name`]), if there is one.
///
/// # Safety
///
/// `size_name` must be NULL or point to a NUL-terminated string.
unsafe fn find_canvas(model: *const PmModel, size_name: *const c_char) -> Option<usize> {
    if size_name.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let size_name = unsafe { CStr::from_ptr(size_name) }.to_string_lossy();
    Model::from_view(model)?
        .canvases()
        .iter()
        .position(|canvas| canvas.profile().is_canvas_name(&size_name))
}

/// Whether `size_name` is one of `model`'s canvas names, ignoring ASCII
/// case: a design size rather than media to load (plan, D1). False for
/// NULL, or if `model` is not one of the table's views.
///
/// # Safety
///
/// `size_name` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_overprint_is_canvas(
    model: *const PmModel,
    size_name: *const c_char,
) -> bool {
    // SAFETY: the caller's obligation is passed on.
    unsafe { find_canvas(model, size_name) }.is_some()
}

/// The index of `model`'s canvas named `size_name`, ignoring ASCII case,
/// for [`pm_overprint_get`]; -1 if [`pm_overprint_is_canvas`] is false.
///
/// # Safety
///
/// `size_name` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_overprint_find(
    model: *const PmModel,
    size_name: *const c_char,
) -> c_int {
    // SAFETY: the caller's obligation is passed on.
    unsafe { find_canvas(model, size_name) }
        .and_then(|index| c_int::try_from(index).ok())
        .unwrap_or(-1)
}

/// Whether the stock of `model`'s canvas at `index` is loaded, as a job's
/// resolution decides it (D3): the ready media `ready_name`,
/// `ready_width` x `ready_length` hundredths of a millimetre, is a label
/// of the stock's size, or is the canvas itself. False if there is no such
/// canvas.
///
/// # Safety
///
/// `ready_name` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_overprint_stock_loaded(
    model: *const PmModel,
    index: c_uint,
    ready_name: *const c_char,
    ready_width: c_int,
    ready_length: c_int,
) -> bool {
    let Some(canvas) = model_canvas(model, index) else {
        return false;
    };
    let ready_name = if ready_name.is_null() {
        c""
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        unsafe { CStr::from_ptr(ready_name) }
    };
    let ready = NamedMedia {
        name: &ready_name.to_string_lossy(),
        size: MediaSize {
            width: ready_width,
            length: ready_length,
        },
    };
    stock_loaded(canvas.profile(), &canvas.profile().canvas_name(), &ready).is_some()
}

/// The `phomemo-overprint-vertical` keyword at `index` of
/// `-supported`, or NULL past the last. The strings are static.
#[unsafe(no_mangle)]
pub extern "C" fn pm_overprint_vertical_keyword(index: c_uint) -> *const c_char {
    usize::try_from(index)
        .ok()
        .and_then(|index| VerticalPolicy::ALL.get(index))
        .map_or(ptr::null(), |policy| policy.keyword().as_ptr())
}

/// The label of the policy a job without a `phomemo-overprint-vertical` of
/// its own gets when the printer's default is `value`
/// ([`VerticalPolicy::resolve_default`]: case-insensitive; NULL, empty or
/// unknown is the default policy). The string is static.
///
/// # Safety
///
/// `value` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_overprint_vertical_label(value: *const c_char) -> *const c_char {
    let value = if value.is_null() {
        None
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        Some(unsafe { CStr::from_ptr(value) }.to_string_lossy())
    };
    VerticalPolicy::resolve_default(value.as_deref())
        .c_label()
        .as_ptr()
}

/// The `phomemo-overprint-vertical-default` keyword, `clip`. The string is
/// static.
#[unsafe(no_mangle)]
pub extern "C" fn pm_overprint_vertical_default() -> *const c_char {
    VerticalPolicy::default().keyword().as_ptr()
}

/// A named media size: a job's media, or the loaded ("ready") media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedMedia<'a> {
    /// PWG size name, e.g. `custom_44x34mm_44x34mm`.
    pub name: &'a str,
    /// Size in hundredths of a millimetre; length 0 for a roll.
    pub size: MediaSize,
}

/// Which of D3's rules made a job a profile job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// 1: the job's media name is the canvas name.
    Name,
    /// 2: the job's media size is the canvas size.
    MediaSize,
    /// 3: the raster header's page size is the canvas size.
    PageSize,
}

impl Rule {
    /// What matched, for the job's log: `its media name`, ...
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Name => "its media name",
            Self::MediaSize => "its media size",
            Self::PageSize => "its page size",
        }
    }
}

/// A message about a resolution, for the job's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// How severe it is.
    pub level: LogLevel,
    /// The message.
    pub text: String,
}

impl Note {
    const fn warn(text: String) -> Self {
        Self {
            level: LogLevel::Warn,
            text,
        }
    }

    const fn info(text: String) -> Self {
        Self {
            level: LogLevel::Info,
            text,
        }
    }

    const fn debug(text: String) -> Self {
        Self {
            level: LogLevel::Debug,
            text,
        }
    }
}

/// How a job is printed.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// As an ordinary job, exactly as without profiles.
    Ordinary {
        /// Why a near miss is not a profile job, if there was one.
        notes: Vec<Note>,
    },
    /// On `profile`'s canvas.
    Profile {
        /// The profile.
        profile: &'static OverprintProfile,
        /// The rule that matched.
        rule: Rule,
        /// What was worked around.
        notes: Vec<Note>,
    },
    /// Not at all: the job names a canvas whose stock is not loaded, and
    /// there is no correct geometry for other stock.
    Error(String),
}

/// How a job on `model` is printed (plan, D3).
///
/// `job` is the job's media, `page_points` its raster header's page size
/// in points (1/72 inch), if known ([`page_points`] finds it), and `ready`
/// the loaded media. Only `model`'s own profiles
/// ([`Model::overprint_profiles`]) are considered. A job uses a profile
/// when, with the profile's stock loaded (see below):
///
/// 1. its media name is the canvas name, ignoring ASCII case - and with
///    other stock loaded it fails instead;
/// 2. its media size is within [`SIZE_TOLERANCE`] of the canvas in both
///    axes; or
/// 3. its page size is within [`SIZE_TOLERANCE`] of the canvas.
///
/// The stock is loaded when `ready` is a label (not a roll) within
/// [`SIZE_TOLERANCE`] of the stock, whatever its name, or is the canvas
/// itself, which PAPPL's own media page can load (D1; a warning).
///
/// A known page size that is not the canvas's makes the job ordinary,
/// whatever its media says, before the stock is looked at: the raster is
/// what gets printed (a warning if the media named the canvas, or measured
/// it with the stock loaded; otherwise a debug note). A canvas-sized page whose job media is another size uses the
/// profile by rule 3; that is noted at info level when the job's media is
/// the ready media - it named none and inherited it, which is how CUPS'
/// driverless path sends a page size without `media-col` - and as a
/// warning when the job named another size. A page or media turned a
/// quarter turn does not match (a debug note).
#[must_use]
pub fn resolve(
    model: &Model,
    job: &NamedMedia<'_>,
    page_points: Option<[f32; 2]>,
    ready: &NamedMedia<'_>,
) -> Resolution {
    let page = page_points.and_then(page_size);
    let mut notes = Vec::new();
    for profile in model.overprint_profiles() {
        match resolve_profile(profile, job, page, ready) {
            Resolution::Ordinary {
                notes: mut profile_notes,
            } => notes.append(&mut profile_notes),
            resolution => return resolution,
        }
    }
    Resolution::Ordinary { notes }
}

/// A raster page's size in points (1/72 inch), from its header: the
/// `cupsPageSize` if both values are positive and finite, else the integer
/// `PageSize` if both are positive, else the page's pixels
/// (`cupsWidth`, `cupsHeight`) at its `HWResolution`; `None` if none of
/// them is usable.
///
/// PWG raster carries no `cupsPageSize`: libcups reads it as 0 x 0 and
/// sets only the integer `PageSize` (`image/pwg-raster`: 124 x 96 for a
/// 44 x 34 mm page), while `image/urf` and CUPS raster set
/// `cupsPageSize` (124.49 x 96.12 and 124.72 x 96.38).
#[must_use]
pub fn page_points(
    cups_page_size: [f32; 2],
    page_size: [u32; 2],
    pixels: [u32; 2],
    resolution: [u32; 2],
) -> Option<[f32; 2]> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "page sizes in points are far inside f32's range; precision beyond 1/100 mm is irrelevant"
    )]
    let narrow = |points: f64| points as f32;
    if cups_page_size
        .iter()
        .all(|&points| points.is_finite() && points > 0.0)
    {
        Some(cups_page_size)
    } else if page_size.iter().all(|&points| points > 0) {
        Some(page_size.map(|points| narrow(f64::from(points))))
    } else if pixels.iter().chain(&resolution).all(|&value| value > 0) {
        Some(
            [0, 1].map(|axis| narrow(f64::from(pixels[axis]) / f64::from(resolution[axis]) * 72.0)),
        )
    } else {
        None
    }
}

/// [`resolve`] for one profile, with the page size in hundredths of a
/// millimetre.
fn resolve_profile(
    profile: &'static OverprintProfile,
    job: &NamedMedia<'_>,
    page: Option<MediaSize>,
    ready: &NamedMedia<'_>,
) -> Resolution {
    let canvas = profile.canvas();
    let canvas_name = profile.canvas_name();
    let loaded = stock_loaded(profile, &canvas_name, ready);
    let named = profile.is_canvas_name(job.name);
    let job_is_canvas = same_size(job.size, canvas);

    // A known page that is not the canvas is printed as it is.
    if let Some(page) = page.filter(|&page| !same_size(page, canvas)) {
        let mut notes = Vec::new();
        if named || job_is_canvas {
            let what = if named { "is" } else { "is the size of" };
            let text = format!(
                "The job's media {} ({}) {what} the overprint canvas {canvas_name}, but its page is {}; following the page and printing an ordinary label.",
                job.name,
                describe(job.size),
                describe(page),
            );
            notes.push(if named || loaded.is_some() {
                Note::warn(text)
            } else {
                Note::debug(text)
            });
        }
        notes.extend(turned(profile, job.size, Some(page)));
        return Resolution::Ordinary { notes };
    }

    // The page is the canvas, or not known.
    let rule = if named {
        Rule::Name
    } else if job_is_canvas {
        Rule::MediaSize
    } else if page.is_some() {
        Rule::PageSize
    } else {
        return Resolution::Ordinary {
            notes: turned(profile, job.size, page).into_iter().collect(),
        };
    };
    let Some(mut notes) = loaded else {
        let ready = if ready.name.trim().is_empty() || ready.size.width <= 0 {
            "no media is loaded".to_owned()
        } else {
            format!("{} ({}) is loaded", ready.name, describe(ready.size))
        };
        if rule == Rule::Name {
            return Resolution::Error(format!(
                "The job's media {canvas_name} is a design for {} labels ({}), but {ready}; load {} to print it.",
                describe(profile.stock),
                profile.stock_name,
                profile.stock_name,
            ));
        }
        return Resolution::Ordinary {
            notes: vec![Note::debug(format!(
                "The job is the size of {canvas_name}, but {ready} rather than {}; printing an ordinary label.",
                profile.stock_name,
            ))],
        };
    };
    if rule == Rule::PageSize {
        let inherited =
            job.name.trim().eq_ignore_ascii_case(ready.name.trim()) && job.size == ready.size;
        notes.push(if inherited {
            Note::info(format!(
                "The job names no media of its own, but its page is {canvas_name}'s size; printing it as that design."
            ))
        } else {
            Note::warn(format!(
                "The job's media is {} ({}), but its page is {canvas_name}'s size; following the page.",
                job.name,
                describe(job.size),
            ))
        });
    }
    Resolution::Profile {
        profile,
        rule,
        notes,
    }
}

/// Whether `profile`'s stock is loaded, with the notes that go with it:
/// `ready` is a label of the stock's size, or the canvas itself (D1).
fn stock_loaded(
    profile: &OverprintProfile,
    canvas_name: &str,
    ready: &NamedMedia<'_>,
) -> Option<Vec<Note>> {
    if !media::is_roll(ready.size.length) && same_size(ready.size, profile.stock) {
        Some(Vec::new())
    } else if profile.is_canvas_name(ready.name) {
        Some(vec![Note::warn(format!(
            "The loaded media is the overprint canvas {canvas_name}; taking it as {}, which should be loaded instead.",
            profile.stock_name,
        ))])
    } else {
        None
    }
}

/// A debug note if the job's media or page is the canvas turned a quarter
/// turn, which is not the canvas.
fn turned(profile: &OverprintProfile, job: MediaSize, page: Option<MediaSize>) -> Option<Note> {
    let canvas = profile.canvas();
    let turned = MediaSize {
        width: canvas.length,
        length: canvas.width,
    };
    let size = [Some(job), page]
        .into_iter()
        .flatten()
        .find(|&size| same_size(size, turned))?;
    Some(Note::debug(format!(
        "The job is {}, {} turned a quarter turn, which is not an overprint design; printing an ordinary label.",
        describe(size),
        profile.canvas_name(),
    )))
}

/// Whether `a` and `b` are the same size within [`SIZE_TOLERANCE`].
const fn same_size(a: MediaSize, b: MediaSize) -> bool {
    a.width.abs_diff(b.width) <= SIZE_TOLERANCE && a.length.abs_diff(b.length) <= SIZE_TOLERANCE
}

/// A page size in points (1/72 inch) in hundredths of a millimetre,
/// rounded; `None` unless both are positive and finite.
fn page_size(points: [f32; 2]) -> Option<MediaSize> {
    let hundredths = |points: f32| {
        let value = (f64::from(points) * 2540.0 / 72.0).round();
        if value.is_finite() && value > 0.0 && value <= f64::from(c_int::MAX) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "checked to be a whole number in c_int's range"
            )]
            let hundredths = value as c_int;
            Some(hundredths)
        } else {
            None
        }
    };
    Some(MediaSize {
        width: hundredths(points[0])?,
        length: hundredths(points[1])?,
    })
}

/// `size` for a message: `44 x 34 mm`, or `a 40 mm roll`.
fn describe(size: MediaSize) -> String {
    if media::is_roll(size.length) {
        format!("a {} mm roll", millimetres(size.width))
    } else {
        format!(
            "{} x {} mm",
            millimetres(size.width),
            millimetres(size.length)
        )
    }
}

/// Which canvas rows are sent along the feed: `phomemo-overprint-vertical`
/// (plan, section 2.2 and D7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerticalPolicy {
    /// The label's rows only: as long as an ordinary job.
    #[default]
    Clip,
    /// The label's rows and the bottom bleed after them, which prints into
    /// the gap. Not yet validated on hardware.
    Trailing,
}

impl VerticalPolicy {
    /// Every policy offered, in the order `-supported` lists them.
    pub const ALL: [Self; 2] = [Self::Clip, Self::Trailing];

    /// The option keyword.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Trailing => "trailing",
        }
    }

    /// The option keyword as a C string, for `c/driver.c`'s `-supported`
    /// and `-default`; the same as [`Self::name`].
    #[must_use]
    pub const fn keyword(self) -> &'static CStr {
        match self {
            Self::Clip => c"clip",
            Self::Trailing => c"trailing",
        }
    }

    /// Human-readable name, for the strings catalog and the media page.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.c_label().to_str().unwrap_or_default()
    }

    /// [`Self::label`] as a C string.
    #[must_use]
    pub const fn c_label(self) -> &'static CStr {
        match self {
            Self::Clip => c"Label only (top and bottom bleed not printed)",
            Self::Trailing => c"Bottom bleed into the gap (experimental)",
        }
    }

    /// The policy a job with no value of its own gets for the printer
    /// default `value`, as [`PrintOptions::overprint_vertical`] decides it:
    /// parsed ignoring ASCII case, the default for none, an empty value or
    /// an unknown one.
    ///
    /// [`PrintOptions::overprint_vertical`]: crate::raster::PrintOptions::overprint_vertical
    #[must_use]
    pub fn resolve_default(value: Option<&str>) -> Self {
        value
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse().ok())
            .unwrap_or_default()
    }
}

impl FromStr for VerticalPolicy {
    type Err = UnknownPolicy;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|policy| policy.name().eq_ignore_ascii_case(s))
            .ok_or(UnknownPolicy)
    }
}

/// A `phomemo-overprint-vertical` value that names no policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownPolicy;

impl fmt::Display for UnknownPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown overprint vertical policy")
    }
}

impl std::error::Error for UnknownPolicy {}

/// Where a profile's canvas reaches the head (plan, section 2).
///
/// Each row of the bitmap sent is [`out_width`](Self::out_width) dots:
/// [`pad_left`](Self::pad_left) white dots, the canvas columns
/// [`source_columns`](Self::source_columns), then
/// [`pad_right`](Self::pad_right) white dots; the bitmap is placed
/// [`margin`](Self::margin) bytes from the start of the head. The canvas
/// rows sent are [`rows`](Self::rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    /// The left margin in bytes: `head_bytes - stock_bytes - b`.
    pub margin: usize,
    /// The bitmap's width in dots: `(stock_bytes + b) * 8`.
    pub out_width: usize,
    /// White dots before the canvas's columns.
    pub pad_left: usize,
    /// The canvas columns kept, in order.
    pub source_columns: Range<usize>,
    /// White dots after the canvas's columns.
    pub pad_right: usize,
    /// The canvas rows sent.
    pub rows: Range<usize>,
}

impl Geometry {
    /// The geometry of a `width` x `height` dot raster on `profile`'s
    /// canvas, for a head `head_bytes` wide at `dpi`, sending the rows
    /// `policy` says.
    ///
    /// Across the head, the label's left edge, canvas column
    /// `x0 = dots_round(bleed.left)`, lands on head dot
    /// `(head_bytes - stock_bytes) * 8`, where an ordinary job's first
    /// column does: `b = ceil(x0 / 8)` bytes of left bleed are kept (fewer
    /// if the head has no room for them), and output column `o` is canvas
    /// column `o - 8 * b + x0`. The bitmap ends at the head's last dot, so
    /// columns past it - the right bleed, `bleed.right`, which is not used
    /// here - are dropped; output columns before or past the raster are
    /// white.
    ///
    /// Along the feed, the label's rows are `y0 = dots_round(bleed.top)` up
    /// to `y1 = dots_round(bleed.top + stock.length)`. [`VerticalPolicy::Clip`]
    /// sends `y0 .. min(y1, height)`; [`VerticalPolicy::Trailing`] adds the
    /// bottom bleed, `y0 .. min(dots_round(bleed.top + stock.length +
    /// bleed.bottom), height)`, so a page longer than the canvas sends no
    /// more than the canvas's rows.
    ///
    /// `None` - print an ordinary page - exactly when:
    ///
    /// - a stock dimension is not positive or a bleed is negative;
    /// - the stock is less than one dot wide at `dpi`, or wider than the
    ///   head (`ceil(dots_floor(stock.width) / 8) > head_bytes`);
    /// - the raster holds no label column (`width <= x0`) or no label row
    ///   (`height <= y0`); or
    /// - a value overflows (`head_bytes * 8`, or a dimension in dots).
    #[must_use]
    pub fn new(
        profile: &OverprintProfile,
        dpi: u16,
        head_bytes: usize,
        width: usize,
        height: usize,
        policy: VerticalPolicy,
    ) -> Option<Self> {
        let OverprintProfile { stock, bleed, .. } = profile;
        if stock.width <= 0
            || stock.length <= 0
            || [bleed.left, bleed.top, bleed.right, bleed.bottom]
                .iter()
                .any(|&side| side < 0)
        {
            return None;
        }
        // Every head dot fits in a usize, so `margin * 8` and every dot
        // below do too: margin * 8 + out_width == head_bytes * 8.
        head_bytes.checked_mul(8)?;
        let dpi = i64::from(dpi);
        let dots_floor = |h: c_int| usize::try_from(i64::from(h) * dpi / 2540).ok();
        let dots_round = |h: c_int| usize::try_from((i64::from(h) * dpi + 1270) / 2540).ok();

        let stock_dots = dots_floor(stock.width)?;
        let stock_bytes = stock_dots.div_ceil(8);
        let x0 = dots_round(bleed.left)?;
        if stock_dots == 0 || width <= x0 {
            return None;
        }
        let spare = head_bytes.checked_sub(stock_bytes)?;
        let b = x0.div_ceil(8).min(spare);
        let margin = spare - b;
        let out_width = (stock_bytes + b) * 8;
        // Output column o is canvas column o + x0 - 8 * b.
        let (pad_left, start) = (8 * b)
            .checked_sub(x0)
            .map_or_else(|| (0, x0 - 8 * b), |pad| (pad, 0));
        let end = start
            .saturating_add(out_width.saturating_sub(pad_left))
            .min(width);
        let source_columns = start..end.max(start);
        let pad_right = out_width
            .saturating_sub(pad_left)
            .saturating_sub(source_columns.len());

        let label_end = bleed.top.checked_add(stock.length)?;
        let y0 = dots_round(bleed.top)?;
        let y1 = dots_round(label_end)?.min(height);
        if y0 >= y1 {
            return None;
        }
        let rows = match policy {
            VerticalPolicy::Clip => y0..y1,
            VerticalPolicy::Trailing => {
                y0..dots_round(label_end.checked_add(bleed.bottom)?)?.min(height)
            }
        };
        Some(Self {
            margin,
            out_width,
            pad_left,
            source_columns,
            pad_right,
            rows,
        })
    }

    /// The canvas column output column `output` shows, or `None` where the
    /// output is white.
    #[cfg(test)]
    #[must_use]
    pub fn canvas_column(&self, output: usize) -> Option<usize> {
        let offset = output.checked_sub(self.pad_left)?;
        (offset < self.source_columns.len()).then(|| self.source_columns.start + offset)
    }

    /// The head dot canvas column `column` lands on, or `None` if it is
    /// not printed.
    #[cfg(test)]
    #[must_use]
    pub fn head_dot(&self, column: usize) -> Option<usize> {
        self.source_columns
            .contains(&column)
            .then(|| self.margin * 8 + self.pad_left + (column - self.source_columns.start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> &'static Model {
        Model::by_name(name).unwrap_or_else(|| panic!("{name} is in the table"))
    }

    fn m220_profile() -> &'static OverprintProfile {
        model("M220")
            .overprint_profiles()
            .next()
            .expect("the M220 has a profile")
    }

    /// The M220 profile with another left bleed.
    fn with_left_bleed(left: c_int) -> OverprintProfile {
        OverprintProfile {
            bleed: Bleed {
                left,
                ..m220_profile().bleed
            },
            ..*m220_profile()
        }
    }

    fn m220_geometry(width: usize, height: usize, policy: VerticalPolicy) -> Option<Geometry> {
        Geometry::new(m220_profile(), 203, 72, width, height, policy)
    }

    fn size(width: c_int, length: c_int) -> MediaSize {
        MediaSize { width, length }
    }

    #[test]
    fn the_m220_profile() {
        let profile = m220_profile();
        assert_eq!(profile.model, "M220");
        assert_eq!(profile.stock_name, "om_40x30mm_40x30mm");
        assert_eq!(profile.stock, size(4000, 3000));
        assert_eq!(profile.bleed, Bleed::uniform(200));
        assert_eq!(profile.label, "40 x 30 mm + 2 mm overprint");
        assert_eq!(profile.canvas(), size(4400, 3400));
        assert_eq!(profile.canvas_name(), "om_40x30mm-overprint-2mm_44x34mm");
    }

    /// D2: the name is `<prefix>_<name>_<dimensions>` and its dimensions are
    /// the canvas, as `pwgMediaForPWG` reads them.
    #[test]
    fn canvas_names_parse_back_to_the_canvas() {
        for profile in OverprintProfile::all() {
            let name = profile.canvas_name();
            assert_eq!(name.matches('_').count(), 2, "{name}");
            let (_, dimensions) = name.rsplit_once('_').expect("two underscores");
            let (width, length) = dimensions
                .strip_suffix("mm")
                .and_then(|dimensions| dimensions.split_once('x'))
                .expect("<w>x<l>mm");
            let hundredths = |mm: &str| {
                let mm: f64 = mm.parse().expect("a number");
                #[expect(clippy::cast_possible_truncation, reason = "a small test value")]
                let hundredths = (mm * 100.0).round() as c_int;
                hundredths
            };
            assert_eq!(
                size(hundredths(width), hundredths(length)),
                profile.canvas(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_table_is_sane() {
        for profile in OverprintProfile::all() {
            let Bleed {
                left,
                top,
                right,
                bottom,
            } = profile.bleed;
            assert!([left, top, right, bottom].iter().all(|&side| side >= 0));
            assert!(profile.stock.width > 0 && profile.stock.length > 0);
            let name = profile.canvas_name();
            let segment = name.split('_').nth(1).expect("a name segment");
            assert!(
                segment
                    .bytes()
                    .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-')),
                "{name}"
            );
        }
    }

    #[test]
    fn uneven_bleed_is_named_side_by_side() {
        let profile = OverprintProfile {
            bleed: Bleed {
                left: 150,
                top: 200,
                right: 0,
                bottom: 250,
            },
            ..*m220_profile()
        };
        assert_eq!(
            profile.canvas_name(),
            "om_40x30mm-overprint-1.5-2-0-2.5mm_41.5x34.5mm"
        );
    }

    #[test]
    fn canvas_width_in_dots() {
        // 44 mm: 351.65 dots at 203 dpi, 519.69 at 300.
        assert_eq!(m220_profile().canvas_width_dots(203), Some(352));
        assert_eq!(m220_profile().canvas_width_dots(300), Some(520));
        assert_eq!(m220_profile().canvas_width_dots(0), Some(0));
    }

    #[test]
    fn canvas_names_ignore_case() {
        assert!(m220_profile().is_canvas_name("OM_40X30MM-OVERPRINT-2MM_44X34MM"));
        assert!(!m220_profile().is_canvas_name("custom_44x34mm_44x34mm"));
    }

    #[test]
    fn only_the_m220_has_profiles() {
        let m220 = model("M220");
        assert_eq!(m220.overprint_profiles().count(), 1);
        for profile in m220.overprint_profiles() {
            let canvas = profile.canvas();
            assert!(m220.accepts_media(canvas.width, canvas.length));
            assert!(m220.find_media(profile.stock_name).is_some());
        }
        for name in ["D30", "M110", "M200"] {
            assert_eq!(model(name).overprint_profiles().count(), 0, "{name}");
        }
    }

    #[test]
    fn m220_geometry_across_the_head() {
        let geometry = m220_geometry(352, 272, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(geometry.margin, 30);
        assert_eq!(geometry.out_width, 336);
        assert_eq!(geometry.pad_left, 0);
        assert_eq!(geometry.source_columns, 0..336);
        assert_eq!(geometry.pad_right, 0);
        // Output column o is canvas column o.
        assert_eq!(geometry.canvas_column(0), Some(0));
        assert_eq!(geometry.canvas_column(335), Some(335));
        assert_eq!(geometry.canvas_column(336), None);
        // The label's first column lands where an ordinary job's does, and
        // the last kept column on the head's last dot.
        assert_eq!(geometry.head_dot(16), Some(256));
        assert_eq!(geometry.head_dot(335), Some(575));
        assert_eq!(geometry.head_dot(336), None);
    }

    #[test]
    fn rasters_351_and_352_wide_cover_column_335() {
        for width in [351, 352] {
            for height in [271, 272] {
                let geometry =
                    m220_geometry(width, height, VerticalPolicy::Clip).expect("a canvas");
                assert_eq!(geometry.source_columns, 0..336, "{width}x{height}");
                assert_eq!((geometry.pad_left, geometry.pad_right), (0, 0));
                assert_eq!(geometry.head_dot(16), Some(256));
                assert_eq!(geometry.rows, 16..256);
            }
        }
    }

    #[test]
    fn m220_rows_per_policy() {
        let clip = m220_geometry(352, 272, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(clip.rows, 16..256);
        assert_eq!(clip.rows.len(), 240);
        for height in [271, 272] {
            let trailing = m220_geometry(352, height, VerticalPolicy::Trailing).expect("a canvas");
            assert_eq!(trailing.rows, 16..height);
        }
        // A page longer than the canvas sends no more than its rows: 34 mm
        // is 272 rows.
        let long = m220_geometry(352, 10_000, VerticalPolicy::Trailing).expect("a canvas");
        assert_eq!(long.rows, 16..272);
        let clip = m220_geometry(352, 10_000, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(clip.rows, 16..256);
    }

    /// The bitmap always fills the head from the margin to its last dot.
    #[test]
    fn geometry_is_consistent_across_sizes() {
        for (dpi, head_bytes) in [(203, 72), (203, 48), (300, 72), (300, 50), (203, 41)] {
            for left in [0, 100, 190, 200, 350] {
                let profile = with_left_bleed(left);
                for width in [1, 15, 17, 200, 319, 351, 352, 353, 600, 10_000] {
                    for height in [1, 16, 17, 271, 272, 400] {
                        for policy in [VerticalPolicy::Clip, VerticalPolicy::Trailing] {
                            let Some(geometry) =
                                Geometry::new(&profile, dpi, head_bytes, width, height, policy)
                            else {
                                continue;
                            };
                            let case = format!(
                                "{dpi} dpi, {head_bytes} bytes, left {left}, {width}x{height}, {policy:?}"
                            );
                            assert_eq!(
                                geometry.pad_left
                                    + geometry.source_columns.len()
                                    + geometry.pad_right,
                                geometry.out_width,
                                "{case}"
                            );
                            assert_eq!(geometry.out_width % 8, 0, "{case}");
                            assert_eq!(
                                geometry.margin,
                                head_bytes - geometry.out_width / 8,
                                "{case}"
                            );
                            assert!(geometry.source_columns.end <= width, "{case}");
                            assert!(geometry.rows.end <= height, "{case}");
                            assert!(!geometry.rows.is_empty(), "{case}");
                            let last = geometry.out_width - 1;
                            if let Some(column) = geometry.canvas_column(last) {
                                assert_eq!(
                                    geometry.head_dot(column),
                                    Some(head_bytes * 8 - 1),
                                    "{case}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn odd_x0_pads_one_white_column() {
        // 1.9 mm is 15.18 dots, rounded to 15.
        let profile = with_left_bleed(190);
        let geometry =
            Geometry::new(&profile, 203, 72, 352, 272, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(geometry.margin, 30);
        assert_eq!(geometry.out_width, 336);
        assert_eq!(geometry.pad_left, 1);
        assert_eq!(geometry.source_columns, 0..335);
        assert_eq!(geometry.pad_right, 0);
        assert_eq!(geometry.canvas_column(0), None);
        assert_eq!(geometry.canvas_column(1), Some(0));
        assert_eq!(geometry.canvas_column(335), Some(334));
        // The label's first column, canvas column 15, is still head dot 256.
        assert_eq!(geometry.head_dot(15), Some(256));
    }

    #[test]
    fn narrow_rasters_are_padded_white() {
        let geometry = m220_geometry(200, 272, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(geometry.source_columns, 0..200);
        assert_eq!(geometry.pad_right, 136);
        assert_eq!(geometry.canvas_column(200), None);
        assert_eq!(geometry.head_dot(16), Some(256));
        let small = m220_geometry(200, 100, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(
            (small.source_columns, small.pad_right, small.rows),
            (0..200, 136, 16..100)
        );
    }

    #[test]
    fn short_rasters_keep_what_they_have() {
        let geometry = m220_geometry(352, 100, VerticalPolicy::Clip).expect("a canvas");
        assert_eq!(geometry.rows, 16..100);
        let trailing = m220_geometry(352, 100, VerticalPolicy::Trailing).expect("a canvas");
        assert_eq!(trailing.rows, 16..100);
    }

    #[test]
    fn rasters_without_a_label_are_ordinary() {
        // No label row: the raster ends before y0 or at it.
        assert_eq!(m220_geometry(352, 10, VerticalPolicy::Clip), None);
        assert_eq!(m220_geometry(352, 16, VerticalPolicy::Trailing), None);
        // No label column.
        assert_eq!(m220_geometry(16, 272, VerticalPolicy::Clip), None);
        assert_eq!(m220_geometry(0, 0, VerticalPolicy::Clip), None);
        // Room for the first label column only.
        let one = m220_geometry(17, 17, VerticalPolicy::Clip).expect("one label dot");
        assert_eq!((one.source_columns, one.rows), (0..17, 16..17));
    }

    #[test]
    fn stock_wider_than_the_head_is_ordinary() {
        assert_eq!(
            Geometry::new(m220_profile(), 203, 39, 352, 272, VerticalPolicy::Clip),
            None
        );
    }

    #[test]
    fn left_bleed_shrinks_to_fit_the_head() {
        // 40 bytes of stock on a 41-byte head: one byte of left bleed, so
        // output column 0 is canvas column 8.
        let geometry = Geometry::new(m220_profile(), 203, 41, 352, 272, VerticalPolicy::Clip)
            .expect("a canvas");
        assert_eq!(geometry.margin, 0);
        assert_eq!(geometry.out_width, 328);
        assert_eq!(geometry.source_columns, 8..336);
        assert_eq!(geometry.canvas_column(0), Some(8));
        assert_eq!(geometry.head_dot(16), Some(8));
    }

    #[test]
    fn extreme_inputs_do_not_panic() {
        let huge = OverprintProfile {
            stock: size(c_int::MAX, c_int::MAX),
            bleed: Bleed::uniform(c_int::MAX),
            ..*m220_profile()
        };
        assert_eq!(huge.canvas(), size(c_int::MAX, c_int::MAX));
        let _ = huge.canvas_name();
        let wide = OverprintProfile {
            stock: size(c_int::MAX / 2, 3000),
            ..*m220_profile()
        };
        for policy in [VerticalPolicy::Clip, VerticalPolicy::Trailing] {
            for geometry in [
                Geometry::new(&huge, u16::MAX, usize::MAX, usize::MAX, usize::MAX, policy),
                Geometry::new(&wide, u16::MAX, usize::MAX / 8, usize::MAX, 272, policy),
                Geometry::new(&wide, u16::MAX, usize::MAX / 4, usize::MAX, 272, policy),
                Geometry::new(m220_profile(), u16::MAX, 72, usize::MAX, usize::MAX, policy),
                Geometry::new(m220_profile(), 203, usize::MAX / 8, usize::MAX, 272, policy),
                Geometry::new(m220_profile(), 203, usize::MAX, usize::MAX, 272, policy),
                Geometry::new(m220_profile(), 0, 72, 352, 272, policy),
            ]
            .into_iter()
            .flatten()
            {
                for index in [0, 1, 16, 335, usize::MAX - 1, usize::MAX] {
                    let _ = geometry.canvas_column(index);
                    let _ = geometry.head_dot(index);
                }
                let _ = geometry.canvas_column(geometry.out_width - 1);
                let _ = geometry.head_dot(geometry.source_columns.end - 1);
            }
        }
        // A head too wide to count in dots.
        assert_eq!(
            Geometry::new(
                m220_profile(),
                203,
                usize::MAX,
                352,
                272,
                VerticalPolicy::Clip
            ),
            None
        );
        let negative = OverprintProfile {
            bleed: Bleed::uniform(-200),
            ..*m220_profile()
        };
        assert_eq!(
            Geometry::new(&negative, 203, 72, 352, 272, VerticalPolicy::Clip),
            None
        );
    }

    #[test]
    fn vertical_policies() {
        assert_eq!(VerticalPolicy::default(), VerticalPolicy::Clip);
        assert_eq!("clip".parse(), Ok(VerticalPolicy::Clip));
        assert_eq!("TRAILING".parse(), Ok(VerticalPolicy::Trailing));
        assert_eq!("full".parse::<VerticalPolicy>(), Err(UnknownPolicy));
        assert_eq!("".parse::<VerticalPolicy>(), Err(UnknownPolicy));
        assert_eq!(VerticalPolicy::Trailing.name(), "trailing");
    }

    // Resolution.

    const CANVAS: &str = "om_40x30mm-overprint-2mm_44x34mm";
    const STOCK: &str = "om_40x30mm_40x30mm";

    fn stock() -> NamedMedia<'static> {
        NamedMedia {
            name: STOCK,
            size: size(4000, 3000),
        }
    }

    fn media(name: &'static str, width: c_int, length: c_int) -> NamedMedia<'static> {
        NamedMedia {
            name,
            size: size(width, length),
        }
    }

    /// `width` x `length` hundredths of a millimetre in points.
    #[expect(clippy::cast_possible_truncation, reason = "small test values")]
    fn points(width: c_int, length: c_int) -> [f32; 2] {
        let points = |h: c_int| (f64::from(h) * 72.0 / 2540.0) as f32;
        [points(width), points(length)]
    }

    fn m220_resolve(
        job: &NamedMedia<'_>,
        page: Option<[f32; 2]>,
        ready: &NamedMedia<'_>,
    ) -> Resolution {
        resolve(model("M220"), job, page, ready)
    }

    /// The rule and note levels of a profile resolution.
    fn profile_rule(resolution: &Resolution) -> Option<(Rule, Vec<LogLevel>)> {
        match resolution {
            Resolution::Profile {
                profile,
                rule,
                notes,
            } => {
                assert!(std::ptr::eq(*profile, m220_profile()));
                Some((*rule, notes.iter().map(|note| note.level).collect()))
            }
            _ => None,
        }
    }

    /// The note levels of an ordinary resolution.
    fn ordinary(resolution: &Resolution) -> Option<Vec<LogLevel>> {
        match resolution {
            Resolution::Ordinary { notes } => Some(notes.iter().map(|note| note.level).collect()),
            _ => None,
        }
    }

    #[test]
    fn rule_1_the_canvas_name() {
        let job = media(CANVAS, 4400, 3400);
        assert_eq!(
            profile_rule(&m220_resolve(&job, None, &stock())),
            Some((Rule::Name, vec![]))
        );
        assert_eq!(
            profile_rule(&m220_resolve(&job, Some(points(4400, 3400)), &stock())),
            Some((Rule::Name, vec![]))
        );
        let shouted = media("OM_40X30MM-OVERPRINT-2MM_44X34MM", 4400, 3400);
        assert_eq!(
            profile_rule(&m220_resolve(&shouted, None, &stock())),
            Some((Rule::Name, vec![]))
        );
    }

    #[test]
    fn rule_1_yields_to_another_page_size() {
        let job = media(CANVAS, 4400, 3400);
        assert_eq!(
            ordinary(&m220_resolve(&job, Some(points(4000, 3000)), &stock())),
            Some(vec![LogLevel::Warn])
        );
        // Before the stock is looked at: other stock is no error then.
        let other = media("om_50x30mm_50x30mm", 5000, 3000);
        assert_eq!(
            ordinary(&m220_resolve(&job, Some(points(4000, 3000)), &other)),
            Some(vec![LogLevel::Warn])
        );
    }

    #[test]
    fn rule_1_without_the_stock_fails() {
        let job = media(CANVAS, 4400, 3400);
        for ready in [
            media("om_50x30mm_50x30mm", 5000, 3000),
            media("om_40x0mm_40x0mm", 4000, 0),
        ] {
            let Resolution::Error(message) = m220_resolve(&job, None, &ready) else {
                panic!("{} loaded", ready.name);
            };
            assert!(message.contains(CANVAS), "{message}");
            assert!(message.contains(ready.name), "{message}");
            assert!(message.contains(STOCK), "{message}");
        }
        // With a canvas-sized page too.
        let other = media("om_50x30mm_50x30mm", 5000, 3000);
        assert!(matches!(
            m220_resolve(&job, Some(points(4400, 3400)), &other),
            Resolution::Error(_)
        ));
        for empty in [media("", 0, 0), media("om_40x0mm_40x0mm", 0, 0)] {
            let Resolution::Error(message) = m220_resolve(&job, None, &empty) else {
                panic!("nothing loaded");
            };
            assert!(message.contains("no media is loaded"), "{message}");
        }
    }

    #[test]
    fn rule_2_the_canvas_size() {
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        assert_eq!(
            profile_rule(&m220_resolve(&job, None, &stock())),
            Some((Rule::MediaSize, vec![]))
        );
        // With a page size that agrees.
        assert_eq!(
            profile_rule(&m220_resolve(&job, Some(points(4400, 3400)), &stock())),
            Some((Rule::MediaSize, vec![]))
        );
    }

    #[test]
    fn rule_2_tolerance() {
        for (width, length) in [(4450, 3400), (4350, 3350), (4400, 3450)] {
            let job = media("custom", width, length);
            assert_eq!(
                profile_rule(&m220_resolve(&job, None, &stock())),
                Some((Rule::MediaSize, vec![])),
                "{width}x{length}"
            );
        }
        for (width, length) in [(4451, 3400), (4349, 3400), (4400, 3451), (4400, 3349)] {
            let job = media("custom", width, length);
            assert_eq!(
                ordinary(&m220_resolve(&job, None, &stock())),
                Some(vec![]),
                "{width}x{length}"
            );
        }
    }

    #[test]
    fn rule_2_needs_the_stock() {
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        let other = media("om_50x30mm_50x30mm", 5000, 3000);
        assert_eq!(
            ordinary(&m220_resolve(&job, None, &other)),
            Some(vec![LogLevel::Debug])
        );
        // The same note when another page size decides anyway.
        assert_eq!(
            ordinary(&m220_resolve(&job, Some(points(4000, 3000)), &other)),
            Some(vec![LogLevel::Debug])
        );
    }

    #[test]
    fn rule_3_the_page_size() {
        // CUPS' driverless path without media-col: the job's media is the
        // ready stock, the page the canvas.
        assert_eq!(
            profile_rule(&m220_resolve(&stock(), Some(points(4400, 3400)), &stock())),
            Some((Rule::PageSize, vec![LogLevel::Info]))
        );
        // A job that named another size explicitly.
        let named = media("om_50x30mm_50x30mm", 5000, 3000);
        assert_eq!(
            profile_rule(&m220_resolve(&named, Some(points(4400, 3400)), &stock())),
            Some((Rule::PageSize, vec![LogLevel::Warn]))
        );
        let job = media("custom_43.99x34.01mm_43.99x34.01mm", 4399, 3401);
        assert_eq!(
            profile_rule(&m220_resolve(&job, Some(points(4399, 3401)), &stock())),
            Some((Rule::MediaSize, vec![]))
        );
    }

    #[test]
    fn rule_3_tolerance() {
        for (width, length) in [(4450, 3400), (4350, 3450)] {
            assert!(
                profile_rule(&m220_resolve(
                    &stock(),
                    Some(points(width, length)),
                    &stock()
                ))
                .is_some(),
                "{width}x{length}"
            );
        }
        for (width, length) in [(4451, 3400), (4400, 3349)] {
            assert_eq!(
                ordinary(&m220_resolve(
                    &stock(),
                    Some(points(width, length)),
                    &stock()
                )),
                Some(vec![]),
                "{width}x{length}"
            );
        }
    }

    #[test]
    fn rule_3_needs_the_stock() {
        let other = media("om_50x30mm_50x30mm", 5000, 3000);
        assert_eq!(
            ordinary(&m220_resolve(&other, Some(points(4400, 3400)), &other)),
            Some(vec![LogLevel::Debug])
        );
    }

    #[test]
    fn the_page_size_wins_over_the_media_size() {
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        assert_eq!(
            ordinary(&m220_resolve(&job, Some(points(4000, 3000)), &stock())),
            Some(vec![LogLevel::Warn])
        );
    }

    #[test]
    fn unusable_page_sizes_are_ignored() {
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        for page in [[0.0, 0.0], [f32::NAN, 96.0], [-124.7, 96.4], [f32::MAX; 2]] {
            assert_eq!(
                profile_rule(&m220_resolve(&job, Some(page), &stock())),
                Some((Rule::MediaSize, vec![])),
                "{page:?}"
            );
        }
    }

    #[test]
    fn a_ready_canvas_is_its_stock() {
        let ready = media(CANVAS, 4400, 3400);
        // Without media-col, the job's media is the ready media.
        assert_eq!(
            profile_rule(&m220_resolve(&ready, None, &ready)),
            Some((Rule::Name, vec![LogLevel::Warn]))
        );
        // A 40 x 30 mm page is printed as it is.
        assert_eq!(
            ordinary(&m220_resolve(&ready, Some(points(4000, 3000)), &ready)),
            Some(vec![LogLevel::Warn])
        );
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        assert_eq!(
            profile_rule(&m220_resolve(&job, None, &ready)),
            Some((Rule::MediaSize, vec![LogLevel::Warn]))
        );
    }

    #[test]
    fn custom_named_stock_is_loaded() {
        let ready = media("custom_40x30mm_40x30mm", 4000, 3000);
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        assert_eq!(
            profile_rule(&m220_resolve(&job, None, &ready)),
            Some((Rule::MediaSize, vec![]))
        );
        let near = media("custom_40.5x29.5mm_40.5x29.5mm", 4050, 2950);
        assert!(profile_rule(&m220_resolve(&job, None, &near)).is_some());
        let far = media("custom_40.51x30mm_40.51x30mm", 4051, 3000);
        assert!(ordinary(&m220_resolve(&job, None, &far)).is_some());
    }

    #[test]
    fn a_roll_is_not_the_stock() {
        let roll = media("om_40x0mm_40x0mm", 4000, 0);
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        assert!(ordinary(&m220_resolve(&job, None, &roll)).is_some());
        assert!(ordinary(&m220_resolve(&roll, Some(points(4400, 3400)), &roll)).is_some());
    }

    #[test]
    fn a_turned_canvas_is_ordinary() {
        let job = media("custom_34x44mm_34x44mm", 3400, 4400);
        assert_eq!(
            ordinary(&m220_resolve(&job, None, &stock())),
            Some(vec![LogLevel::Debug])
        );
        assert_eq!(
            ordinary(&m220_resolve(&stock(), Some(points(3400, 4400)), &stock())),
            Some(vec![LogLevel::Debug])
        );
    }

    #[test]
    fn ordinary_jobs_stay_ordinary() {
        assert_eq!(
            ordinary(&m220_resolve(&stock(), None, &stock())),
            Some(vec![])
        );
        assert_eq!(
            ordinary(&m220_resolve(&stock(), Some(points(4000, 3000)), &stock())),
            Some(vec![])
        );
        let roll = media("om_40x0mm_40x0mm", 4000, 0);
        assert_eq!(
            ordinary(&m220_resolve(&roll, Some(points(4000, 5000)), &roll)),
            Some(vec![])
        );
    }

    /// A page size in hundredths of a millimetre, from points.
    fn hundredths(points: Option<[f32; 2]>) -> Option<MediaSize> {
        points.and_then(page_size)
    }

    #[test]
    fn page_points_fall_back_in_order() {
        let none = [0, 0];
        // CUPS raster and URF set cupsPageSize.
        for cups in [[124.72, 96.38], [124.49, 96.12]] {
            assert_eq!(
                page_points(cups, [124, 96], [352, 272], [203, 203]),
                Some(cups)
            );
        }
        assert_eq!(
            hundredths(page_points([124.72, 96.38], none, none, none)),
            Some(size(4400, 3400))
        );
        assert_eq!(
            hundredths(page_points([124.49, 96.12], none, none, none)),
            Some(size(4392, 3391))
        );
        // PWG raster: cupsPageSize 0 x 0, integer PageSize.
        assert_eq!(
            page_points([0.0, 0.0], [124, 96], [352, 272], [203, 203]),
            Some([124.0, 96.0])
        );
        assert_eq!(
            hundredths(page_points([0.0, 0.0], [124, 96], none, none)),
            Some(size(4374, 3387))
        );
        // Neither: the pixels at the resolution.
        assert_eq!(
            hundredths(page_points([0.0, 0.0], none, [352, 272], [203, 203])),
            Some(size(4404, 3403))
        );
        assert_eq!(
            hundredths(page_points(
                [f32::NAN, 96.0],
                [124, 0],
                [520, 401],
                [300, 300]
            )),
            Some(size(4403, 3395))
        );
        // Nothing usable.
        assert_eq!(page_points([0.0, 0.0], none, none, none), None);
        assert_eq!(
            page_points([-1.0, 96.0], [0, 96], [352, 272], [203, 0]),
            None
        );
    }

    #[test]
    fn every_raster_format_resolves() {
        let none = [0, 0];
        for page in [
            page_points([124.72, 96.38], none, none, none),
            page_points([124.49, 96.12], none, none, none),
            page_points([0.0, 0.0], [124, 96], none, none),
            page_points([0.0, 0.0], none, [352, 272], [203, 203]),
        ] {
            assert_eq!(
                profile_rule(&m220_resolve(&stock(), page, &stock())),
                Some((Rule::PageSize, vec![LogLevel::Info])),
                "{page:?}"
            );
        }
    }

    /// The cases the plan names, with literal values.
    #[test]
    fn the_briefs_cases() {
        let job = media("custom_44x34mm_44x34mm", 4400, 3400);
        let ready = media("om_40x30mm_40x30mm", 4000, 3000);
        assert_eq!(
            profile_rule(&m220_resolve(&job, Some([124.72, 96.38]), &ready)),
            Some((Rule::MediaSize, vec![]))
        );
        assert_eq!(
            profile_rule(&m220_resolve(&ready, Some([124.0, 96.0]), &ready)),
            Some((Rule::PageSize, vec![LogLevel::Info]))
        );
    }

    #[test]
    fn policy_keywords_match_their_names() {
        for policy in VerticalPolicy::ALL {
            assert_eq!(policy.keyword().to_str(), Ok(policy.name()));
            assert_eq!(policy.name().parse(), Ok(policy));
        }
        let keywords: Vec<_> = (0..=c_uint::try_from(VerticalPolicy::ALL.len()).expect("small"))
            .map(|index| pm_overprint_vertical_keyword(index))
            .take_while(|keyword| !keyword.is_null())
            // SAFETY: non-NULL results are static C strings.
            .map(|keyword| unsafe { CStr::from_ptr(keyword) })
            .collect();
        assert_eq!(keywords, [c"clip", c"trailing"]);
        assert!(pm_overprint_vertical_keyword(c_uint::MAX).is_null());
        // SAFETY: pm_overprint_vertical_default returns a static C string.
        let default = unsafe { CStr::from_ptr(pm_overprint_vertical_default()) };
        assert_eq!(default, c"clip");
    }

    #[test]
    fn policy_labels_resolve_as_jobs_do() {
        /// A log that drops its messages.
        struct Quiet;
        impl crate::raster::Log for Quiet {
            fn log(&self, _: LogLevel, _: &str) {}
        }

        let label = |value: Option<&CStr>| {
            // SAFETY: a static C string or NULL; the result is static.
            unsafe {
                CStr::from_ptr(pm_overprint_vertical_label(
                    value.map_or(ptr::null(), CStr::as_ptr),
                ))
            }
        };
        let clip = c"Label only (top and bottom bleed not printed)";
        let trailing = c"Bottom bleed into the gap (experimental)";
        assert_eq!(label(Some(c"trailing")), trailing);
        assert_eq!(label(Some(c"TRAILING")), trailing);
        assert_eq!(label(Some(c"clip")), clip);
        for value in [None, Some(c""), Some(c"full"), Some(c"bogus")] {
            assert_eq!(label(value), clip, "{value:?}");
        }
        for policy in VerticalPolicy::ALL {
            assert_eq!(policy.c_label().to_str(), Ok(policy.label()));
        }
        // The same decision as a job's without a value of its own
        // (raster/options.rs).
        for value in [None, Some(""), Some("full"), Some("Trailing"), Some("clip")] {
            assert_eq!(
                VerticalPolicy::resolve_default(value),
                crate::raster::PrintOptions::default().overprint_vertical(value, &Quiet),
                "{value:?}"
            );
        }
    }

    #[test]
    fn the_m220_summary() {
        assert_eq!(
            m220_profile().summary(),
            "design on 44 x 34 mm; the label is 2 mm from the left and 2 mm from the top; the right 2 mm is not printed"
        );
        let no_right = OverprintProfile {
            bleed: Bleed {
                right: 0,
                ..m220_profile().bleed
            },
            ..*m220_profile()
        };
        assert_eq!(
            no_right.summary(),
            "design on 42 x 34 mm; the label is 2 mm from the left and 2 mm from the top"
        );
    }

    /// The string at `ptr`, which the model table owns.
    fn string(ptr: *const c_char) -> &'static str {
        // SAFETY: the info only points at strings the table owns.
        unsafe { CStr::from_ptr(ptr) }.to_str().expect("UTF-8")
    }

    #[test]
    fn ffi_lists_the_canvases() {
        let m220 = model("M220").view();
        assert_eq!(pm_overprint_count(m220), 1);
        assert_eq!(pm_overprint_count(model("D30").view()), 0);
        assert_eq!(pm_overprint_count(ptr::null()), 0);

        let mut info = std::mem::MaybeUninit::<PmOverprintInfo>::uninit();
        // SAFETY: `info` is valid for writes; bad models, indices and a
        // NULL output are refused.
        unsafe {
            assert!(!pm_overprint_get(ptr::null(), 0, info.as_mut_ptr()));
            assert!(!pm_overprint_get(m220, 1, info.as_mut_ptr()));
            assert!(!pm_overprint_get(m220, 0, ptr::null_mut()));
            assert!(pm_overprint_get(m220, 0, info.as_mut_ptr()));
        }
        // SAFETY: pm_overprint_get returned true, so it wrote `info`.
        let info = unsafe { info.assume_init() };
        assert_eq!(string(info.canvas_name), CANVAS);
        assert_eq!(string(info.label), "40 x 30 mm + 2 mm overprint");
        assert_eq!(string(info.stock_name), STOCK);
        assert_eq!(string(info.summary), m220_profile().summary());
        assert_eq!((info.canvas_width, info.canvas_length), (4400, 3400));
        assert_eq!((info.stock_width, info.stock_length), (4000, 3000));
        assert_eq!(
            [
                info.bleed_left,
                info.bleed_top,
                info.bleed_right,
                info.bleed_bottom
            ],
            [200; 4]
        );
    }

    #[test]
    fn ffi_recognizes_canvases() {
        let m220 = model("M220").view();
        // SAFETY: static C strings and NULL.
        unsafe {
            assert!(pm_overprint_is_canvas(
                m220,
                c"om_40x30mm-overprint-2mm_44x34mm".as_ptr()
            ));
            assert!(pm_overprint_is_canvas(
                m220,
                c"OM_40X30MM-OVERPRINT-2MM_44X34MM".as_ptr()
            ));
            assert!(!pm_overprint_is_canvas(
                m220,
                c"om_40x30mm_40x30mm".as_ptr()
            ));
            assert!(!pm_overprint_is_canvas(
                m220,
                c"custom_44x34mm_44x34mm".as_ptr()
            ));
            assert!(!pm_overprint_is_canvas(m220, ptr::null()));
            assert!(!pm_overprint_is_canvas(
                model("M110").view(),
                c"om_40x30mm-overprint-2mm_44x34mm".as_ptr()
            ));
            assert!(!pm_overprint_is_canvas(
                ptr::null(),
                c"om_40x30mm-overprint-2mm_44x34mm".as_ptr()
            ));
            assert_eq!(
                pm_overprint_find(m220, c"OM_40x30mm-overprint-2mm_44x34mm".as_ptr()),
                0
            );
            assert_eq!(pm_overprint_find(m220, c"om_40x30mm_40x30mm".as_ptr()), -1);
            assert_eq!(pm_overprint_find(m220, ptr::null()), -1);
        }
    }

    #[test]
    fn ffi_stock_loaded() {
        let m220 = model("M220").view();
        // SAFETY: static C strings and NULL.
        unsafe {
            assert!(pm_overprint_stock_loaded(
                m220,
                0,
                c"om_40x30mm_40x30mm".as_ptr(),
                4000,
                3000
            ));
            assert!(pm_overprint_stock_loaded(
                m220,
                0,
                c"custom_40x30mm_40x30mm".as_ptr(),
                4050,
                2950
            ));
            assert!(pm_overprint_stock_loaded(
                m220,
                0,
                c"om_40x30mm-overprint-2mm_44x34mm".as_ptr(),
                4400,
                3400
            ));
            assert!(!pm_overprint_stock_loaded(
                m220,
                0,
                c"om_50x30mm_50x30mm".as_ptr(),
                5000,
                3000
            ));
            assert!(!pm_overprint_stock_loaded(
                m220,
                0,
                c"om_40x0mm_40x0mm".as_ptr(),
                4000,
                0
            ));
            assert!(pm_overprint_stock_loaded(m220, 0, ptr::null(), 4000, 3000));
            assert!(!pm_overprint_stock_loaded(
                m220,
                1,
                c"om_40x30mm_40x30mm".as_ptr(),
                4000,
                3000
            ));
            assert!(!pm_overprint_stock_loaded(
                ptr::null(),
                0,
                c"om_40x30mm_40x30mm".as_ptr(),
                4000,
                3000
            ));
        }
    }

    #[test]
    fn other_models_have_no_profiles() {
        let canvas = media(CANVAS, 4400, 3400);
        for name in ["M110", "D30", "M200"] {
            let resolution = resolve(model(name), &canvas, Some(points(4400, 3400)), &stock());
            assert_eq!(ordinary(&resolution), Some(vec![]), "{name}");
        }
    }
}
