//! The printer models this application drives, one PAPPL driver each.
//!
//! The table is built once from [`phomemo_protocol::model`] and the media
//! catalog, and owns every string C is handed: C holds [`PmModel`] views
//! into it (PAPPL's driver table, and each printer's driver `extension`),
//! and Rust maps a view back to its [`Model`] by address rather than by
//! reading strings back through C pointers.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort};
use std::ptr;
use std::sync::LazyLock;

use phomemo_protocol::media::{MediaPreset, PaperPool, paper_pool};
use phomemo_protocol::model::{self, ModelInfo};

use crate::media;
use crate::overprint::OverprintProfile;
use crate::pappl::tracking_flag;
use crate::raster::MAX_ROWS;

/// A printer model and its media.
#[derive(Debug)]
pub struct Model {
    info: &'static ModelInfo,
    pool: &'static PaperPool,
    /// Index of the catalog's default preset in `pool.media`.
    default_media: usize,
    driver_name: CString,
    device_id: CString,
    /// `Phomemo <name>`.
    product: CString,
    /// One per preset in `pool.media`, in the same order.
    media_names: Vec<CString>,
    /// The PWG names of [`Model::custom_range`]'s bounds.
    custom_range_names: [CString; 2],
    /// Keeps the string `view.name` points to.
    _name: CString,
    view: PmModel,
}

/// A media size in hundredths of a millimetre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSize {
    /// Across the media.
    pub width: c_int,
    /// Along the media; 0 for a roll.
    pub length: c_int,
}

/// The custom media sizes a model takes, as PAPPL describes them: the
/// `roll_min_` and `roll_max_` entries of a driver's media list, from which
/// PAPPL derives `media-size-supported`'s range and checks ready media
/// (`printer-driver.c`, `make_attrs` and `validate_ready`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomRange {
    /// The narrowest and shortest size.
    pub min: MediaSize,
    /// The widest and longest size.
    pub max: MediaSize,
}

impl CustomRange {
    /// PAPPL's names for the bounds: `roll_min_<w>x<l>mm` and
    /// `roll_max_<w>x<l>mm`.
    fn pwg_names(&self) -> Option<[CString; 2]> {
        let name = |bound: &str, size: MediaSize| {
            CString::new(format!(
                "roll_{bound}_{}x{}mm",
                millimetres(size.width),
                millimetres(size.length)
            ))
            .ok()
        };
        Some([name("min", self.min)?, name("max", self.max)?])
    }
}

/// `hundredths` of a millimetre in millimetres, without trailing zeros, as
/// PWG size names spell them.
pub fn millimetres(hundredths: c_int) -> String {
    let (whole, fraction) = (hundredths / 100, hundredths % 100);
    match fraction {
        0 => whole.to_string(),
        _ if fraction % 10 == 0 => format!("{whole}.{}", fraction / 10),
        _ => format!("{whole}.{fraction:02}"),
    }
}

/// A model as C sees it: the strings PAPPL's driver table needs.
///
/// Every pointer is to a NUL-terminated string owned by the model table,
/// which lives as long as the process.
#[repr(C)]
#[derive(Debug)]
pub struct PmModel {
    /// Model name, e.g. `M220`.
    pub name: *const c_char,
    /// PAPPL driver name, e.g. `phomemo_m220`.
    pub driver_name: *const c_char,
    /// IEEE 1284 device ID, e.g. `MFG:Phomemo;MDL:M220;CMD:PHOMEMO;`.
    pub device_id: *const c_char,
}

// SAFETY: `PmModel` has no interior mutability; sharing or sending one only
// copies pointer values, and every dereference of them is a separate unsafe
// operation with its own obligations. The table's views point at strings it
// owns, which are never mutated or freed once built.
unsafe impl Send for PmModel {}
// SAFETY: as for `Send`.
unsafe impl Sync for PmModel {}

static MODELS: LazyLock<Vec<Model>> =
    LazyLock::new(|| model::all().iter().filter_map(Model::new).collect());

impl Model {
    /// The row for `info`, or `None` if the catalog has no media for it -
    /// a model PAPPL could not offer a single media size for.
    fn new(info: &'static ModelInfo) -> Option<Self> {
        let pool = paper_pool(info.name).or_else(|| paper_pool(info.series))?;
        let default = pool.default_media()?;
        let default_media = pool
            .media
            .iter()
            .position(|preset| ptr::eq(preset, default))?;
        let name = CString::new(info.name).ok()?;
        let driver_name =
            CString::new(format!("phomemo_{}", info.name.to_ascii_lowercase())).ok()?;
        let device_id = CString::new(format!("MFG:Phomemo;MDL:{};CMD:PHOMEMO;", info.name)).ok()?;
        let product = CString::new(format!("Phomemo {}", info.name)).ok()?;
        let media_names = pool
            .media
            .iter()
            .map(|preset| CString::new(preset.size_name.as_str()))
            .collect::<Result<_, _>>()
            .ok()?;
        // The views point into the strings' heap buffers, which stay put
        // when the strings themselves move into the table.
        let view = PmModel {
            name: name.as_ptr(),
            driver_name: driver_name.as_ptr(),
            device_id: device_id.as_ptr(),
        };
        let mut model = Self {
            info,
            pool,
            default_media,
            driver_name,
            device_id,
            product,
            media_names,
            custom_range_names: Default::default(),
            _name: name,
            view,
        };
        // The range follows from the model's other properties.
        model.custom_range_names = model.custom_range()?.pwg_names()?;
        Some(model)
    }

    /// Every model, in the order of [`model::all`].
    pub fn all() -> &'static [Self] {
        &MODELS
    }

    /// The model whose PAPPL driver name is `driver_name`.
    pub fn by_driver_name(driver_name: &CStr) -> Option<&'static Self> {
        Self::all()
            .iter()
            .find(|model| model.driver_name.as_c_str() == driver_name)
    }

    /// The model called `name`, ignoring ASCII case.
    pub fn by_name(name: &str) -> Option<&'static Self> {
        Self::all()
            .iter()
            .find(|model| model.name().eq_ignore_ascii_case(name))
    }

    /// The model `view` is the view of: null, or any pointer that is not
    /// one of the table's views, gives `None`.
    pub fn from_view(view: *const PmModel) -> Option<&'static Self> {
        Self::all()
            .iter()
            .find(|model| ptr::eq(&raw const model.view, view))
    }

    /// The protocol crate's description of the hardware.
    pub const fn info(&self) -> &'static ModelInfo {
        self.info
    }

    /// Model name, e.g. `M220`.
    pub const fn name(&self) -> &'static str {
        self.info.name
    }

    /// PAPPL driver name, e.g. `phomemo_m220`.
    pub fn driver_name(&self) -> &CStr {
        &self.driver_name
    }

    /// IEEE 1284 device ID, e.g. `MFG:Phomemo;MDL:M220;CMD:PHOMEMO;`.
    pub fn device_id(&self) -> &CStr {
        &self.device_id
    }

    /// `Phomemo <name>`, PAPPL's make and model.
    pub fn make_and_model(&self) -> &CStr {
        &self.product
    }

    /// The view C holds.
    pub const fn view(&self) -> &PmModel {
        &self.view
    }

    /// Print-head width in dots.
    pub fn head_width_px(&self) -> usize {
        usize::from(self.info.max_width_px)
    }

    /// Print-head width in bytes, as raster rows are packed.
    pub fn head_width_bytes(&self) -> usize {
        usize::from(self.info.max_width_bytes())
    }

    /// Whether every label the model takes is wider than its head.
    ///
    /// That is the D30 class: a 12 mm head printing 25-50 mm labels, whose
    /// layouts run along the feed, so a page is printed turned a quarter
    /// turn (phomemo-tools' `rastertopd30.py` turns every page;
    /// Print Master's D30 templates carry a print direction).
    pub fn has_sideways_media(&self) -> bool {
        let head = self.head_width_hundredths_mm();
        self.pool
            .media
            .iter()
            .all(|preset| preset.width_hundredths_mm() > head)
    }

    /// Print-head width in hundredths of a millimetre, rounded to nearest.
    pub fn head_width_hundredths_mm(&self) -> c_int {
        let dots = c_int::from(self.info.max_width_px);
        let dpi = c_int::from(self.info.dpi.max(1));
        (dots * 2540 + dpi / 2) / dpi
    }

    /// How far media `width` x `length` hundredths of a millimetre reaches
    /// across the head: its width, or on a model with sideways media its
    /// shorter side. `None` for a roll on such a model, whose width runs
    /// along the feed and whose extent across the head is not known.
    fn across_head(&self, width: c_int, length: c_int) -> Option<c_int> {
        if !self.has_sideways_media() {
            Some(width)
        } else if media::is_roll(length) {
            None
        } else {
            Some(width.min(length))
        }
    }

    /// Whether the model takes media `width` x `length` hundredths of a
    /// millimetre: whether it reaches across the head no further than the
    /// head itself, or than media in the catalog for the model does -
    /// labels may be a little wider than the head (50 mm on an M110's
    /// 48 mm head, 15 mm on a D30's 12 mm head) and are cropped to it -
    /// and whether it runs along the feed no further than one page can
    /// ([`Self::longest_page_hundredths_mm`]).
    pub fn accepts_media(&self, width: c_int, length: c_int) -> bool {
        width > 0
            && length >= 0
            && self
                .across_head(width, length)
                .is_none_or(|across| across <= self.widest_across_head())
            && self
                .along_feed(width, length)
                .is_none_or(|along| along <= self.longest_page_hundredths_mm())
    }

    /// How far the widest media the model takes reaches across the head,
    /// in hundredths of a millimetre: the head's width, or the catalog's
    /// widest media if that is wider.
    fn widest_across_head(&self) -> c_int {
        self.pool
            .media
            .iter()
            .filter_map(|preset| {
                self.across_head(preset.width_hundredths_mm(), preset.length_hundredths_mm())
            })
            .fold(self.head_width_hundredths_mm(), c_int::max)
    }

    /// How far media `width` x `length` hundredths of a millimetre runs
    /// along the feed: its length, or on a model with sideways media its
    /// longer side, which a roll's width is. `None` for a roll on other
    /// models, whose pages are as long as the job makes them.
    fn along_feed(&self, width: c_int, length: c_int) -> Option<c_int> {
        if self.has_sideways_media() {
            Some(width.max(length))
        } else if media::is_roll(length) {
            None
        } else {
            Some(length)
        }
    }

    /// The longest page the driver prints, in hundredths of a millimetre:
    /// as many rows as one raster can carry ([`MAX_ROWS`]), in whole
    /// millimetres - 8199 mm at 203 dpi, 5548 mm at 300 dpi. A longer page
    /// is refused, so this is also the longest custom media.
    pub fn longest_page_hundredths_mm(&self) -> c_int {
        let dpi = c_int::from(self.info.dpi.max(1));
        c_int::from(MAX_ROWS) * 2540 / dpi / 100 * 100
    }

    /// The custom sizes the model takes: from the catalog's narrowest media
    /// and shortest label up to the widest media across the head and the
    /// longest page along the feed. On a model with sideways media a
    /// label's length runs across the head and its width along the feed.
    /// Every size in the range is one [`Self::accepts_media`] accepts;
    /// `None` if the catalog has no labels.
    pub fn custom_range(&self) -> Option<CustomRange> {
        let narrowest = self
            .pool
            .media
            .iter()
            .map(MediaPreset::width_hundredths_mm)
            .min()?;
        let shortest = self
            .pool
            .media
            .iter()
            .map(MediaPreset::length_hundredths_mm)
            .filter(|&length| !media::is_roll(length))
            .min()?;
        let (across, along) = (self.widest_across_head(), self.longest_page_hundredths_mm());
        let max = if self.has_sideways_media() {
            MediaSize {
                width: along,
                length: across,
            }
        } else {
            MediaSize {
                width: across,
                length: along,
            }
        };
        Some(CustomRange {
            min: MediaSize {
                width: narrowest,
                length: shortest,
            },
            max,
        })
    }

    /// The PWG names of [`Self::custom_range`]'s bounds, for the driver's
    /// media list.
    pub fn custom_range_names(&self) -> impl Iterator<Item = &CStr> {
        self.custom_range_names.iter().map(CString::as_c_str)
    }

    /// The media sizes to offer, with their PWG names as C strings.
    pub fn media(&self) -> impl Iterator<Item = (&'static MediaPreset, &CStr)> {
        self.pool
            .media
            .iter()
            .zip(self.media_names.iter().map(CString::as_c_str))
    }

    /// The preset named `size_name`, if the catalog has it for this model.
    pub fn find_media(&self, size_name: &str) -> Option<&'static MediaPreset> {
        self.pool.find(size_name)
    }

    /// The overprint profiles for this model (`docs/overprint-plan.md`):
    /// those named for it, if its media is not sideways, the profile's
    /// stock is in its catalog at the profile's size, and it takes the
    /// canvas ([`Self::accepts_media`]).
    pub fn overprint_profiles(&self) -> impl Iterator<Item = &'static OverprintProfile> {
        let model = self.name();
        let usable = !self.has_sideways_media();
        OverprintProfile::all().iter().filter(move |profile| {
            let canvas = profile.canvas();
            usable
                && profile.model.eq_ignore_ascii_case(model)
                && self.find_media(profile.stock_name).is_some_and(|preset| {
                    preset.width_hundredths_mm() == profile.stock.width
                        && preset.length_hundredths_mm() == profile.stock.length
                })
                && self.accepts_media(canvas.width, canvas.length)
        })
    }

    /// The catalog's default media size, with its PWG name.
    pub fn default_media(&self) -> (&'static MediaPreset, &CStr) {
        // `new` found the index in `pool.media`, which `media_names`
        // parallels.
        let index = self.default_media;
        (&self.pool.media[index], &self.media_names[index])
    }

    /// Every media-tracking mode the model's media come in, as
    /// `pappl_media_tracking_t` bits.
    pub fn tracking_supported(&self) -> c_ushort {
        self.pool
            .media
            .iter()
            .flat_map(|preset| &preset.tracking_supported)
            .fold(0, |mask, &tracking| mask | tracking_flag(tracking))
    }
}

/// Number of known printer models.
#[unsafe(no_mangle)]
pub extern "C" fn pm_model_count() -> c_uint {
    c_uint::try_from(Model::all().len()).unwrap_or(c_uint::MAX)
}

/// The model at `index`, or NULL if `index >= pm_model_count()`.
#[unsafe(no_mangle)]
pub extern "C" fn pm_model_get(index: c_uint) -> *const PmModel {
    usize::try_from(index)
        .ok()
        .and_then(|index| Model::all().get(index))
        .map_or(ptr::null(), |model| model.view())
}

/// The model whose PAPPL driver name is `driver_name`, or NULL.
///
/// # Safety
///
/// `driver_name` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_model_lookup(driver_name: *const c_char) -> *const PmModel {
    if driver_name.is_null() {
        return ptr::null();
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let driver_name = unsafe { CStr::from_ptr(driver_name) };
    Model::by_driver_name(driver_name).map_or(ptr::null(), |model| model.view())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> &'static Model {
        Model::by_name(name).unwrap_or_else(|| panic!("{name} is in the table"))
    }

    #[test]
    fn every_protocol_model_has_media() {
        let names: Vec<_> = Model::all().iter().map(Model::name).collect();
        let expected: Vec<_> = model::all().iter().map(|info| info.name).collect();
        assert_eq!(names, expected);
    }

    /// `docs/models.md` lists the models one line per head width:
    /// `- <dots>-dot head ...: <name>, <name>, ...`.
    #[test]
    fn the_model_list_names_every_model_by_head_width() {
        let list = include_str!("../../docs/models.md");
        let mut listed = Vec::new();
        for line in list.lines().filter_map(|line| line.strip_prefix("- ")) {
            let (head, names) = line.split_once(':').expect("`head: names`");
            let dots: usize = head
                .split_once("-dot")
                .and_then(|(dots, _)| dots.parse().ok())
                .expect("`<dots>-dot head`");
            for name in names.split(',').map(str::trim) {
                assert_eq!(model(name).head_width_px(), dots, "{name}");
                listed.push(name);
            }
        }
        listed.sort_unstable();
        let mut expected: Vec<_> = Model::all().iter().map(Model::name).collect();
        expected.sort_unstable();
        assert_eq!(listed, expected);
    }

    #[test]
    fn views_map_back_to_their_model() {
        for model in Model::all() {
            let found = Model::from_view(model.view()).expect("own view");
            assert!(ptr::eq(found, model));
        }
        assert!(Model::from_view(ptr::null()).is_none());
        let stranger = PmModel {
            name: ptr::null(),
            driver_name: ptr::null(),
            device_id: ptr::null(),
        };
        assert!(Model::from_view(&raw const stranger).is_none());
    }

    #[test]
    fn view_strings_match_the_model() {
        let m220 = model("M220");
        let view = m220.view();
        // SAFETY: the view's pointers are to strings the table owns.
        let (name, driver, device_id) = unsafe {
            (
                CStr::from_ptr(view.name),
                CStr::from_ptr(view.driver_name),
                CStr::from_ptr(view.device_id),
            )
        };
        assert_eq!(name, c"M220");
        assert_eq!(driver, c"phomemo_m220");
        assert_eq!(device_id, c"MFG:Phomemo;MDL:M220;CMD:PHOMEMO;");
        assert_eq!(m220.make_and_model(), c"Phomemo M220");
    }

    #[test]
    fn lookup_by_driver_name() {
        assert_eq!(
            Model::by_driver_name(c"phomemo_m200c").map(Model::name),
            Some("M200C")
        );
        assert!(Model::by_driver_name(c"phomemo_m9999").is_none());
        // SAFETY: a static C string and NULL are both valid arguments.
        unsafe {
            assert!(ptr::eq(
                pm_model_lookup(c"phomemo_d30".as_ptr()),
                model("D30").view()
            ));
            assert!(pm_model_lookup(ptr::null()).is_null());
        }
    }

    #[test]
    fn model_index_bounds() {
        let count = pm_model_count();
        assert!(!pm_model_get(0).is_null());
        assert!(!pm_model_get(count - 1).is_null());
        assert!(pm_model_get(count).is_null());
    }

    #[test]
    fn head_widths() {
        assert_eq!(model("M220").head_width_hundredths_mm(), 7207);
        assert_eq!(model("M110").head_width_hundredths_mm(), 4805);
        assert_eq!(model("D30").head_width_hundredths_mm(), 1201);
        assert_eq!(model("D50").head_width_hundredths_mm(), 1355);
    }

    #[test]
    fn custom_media_across_the_head() {
        let m220 = model("M220");
        assert!(m220.accepts_media(7207, 3000));
        assert!(!m220.accepts_media(7208, 3000));
        assert!(m220.accepts_media(4000, 0));
        assert!(!m220.accepts_media(0, 3000));
        // The catalog's 50 mm labels on the M110's 48 mm head.
        assert!(model("M110").accepts_media(5000, 3000));
        assert!(!model("M110").accepts_media(5001, 3000));
    }

    #[test]
    fn custom_sideways_media_is_measured_by_its_shorter_side() {
        let d30 = model("D30");
        // The catalog's 15 mm labels on the D30's 12 mm head, either way up.
        assert!(d30.accepts_media(4000, 1500));
        assert!(d30.accepts_media(1500, 4000));
        assert!(!d30.accepts_media(4000, 1600));
        assert!(d30.accepts_media(4000, 0));
    }

    #[test]
    fn only_the_d30_class_has_sideways_media() {
        for model in Model::all() {
            assert_eq!(
                model.has_sideways_media(),
                model.info().max_width_bytes() == 12,
                "{}",
                model.name()
            );
        }
    }

    #[test]
    fn custom_media_runs_no_further_than_a_page() {
        let m220 = model("M220");
        assert_eq!(m220.longest_page_hundredths_mm(), 819_900);
        assert!(m220.accepts_media(4000, 819_900));
        assert!(!m220.accepts_media(4000, 820_000));
        assert!(!m220.accepts_media(4000, -1));
        // Turned, a D30 label's width runs along the feed.
        let d30 = model("D30");
        assert!(d30.accepts_media(819_900, 1500));
        assert!(!d30.accepts_media(820_000, 1500));
    }

    #[test]
    fn m220_custom_range() {
        let m220 = model("M220");
        let range = m220.custom_range().expect("the catalog has labels");
        assert_eq!(
            range.min,
            MediaSize {
                width: 2000,
                length: 1000
            }
        );
        assert_eq!(
            range.max,
            MediaSize {
                width: 7207,
                length: 819_900
            }
        );
        let names: Vec<_> = m220.custom_range_names().collect();
        assert_eq!(names, [c"roll_min_20x10mm", c"roll_max_72.07x8199mm"]);
    }

    #[test]
    fn sideways_custom_media_is_long_across_the_feed() {
        let range = model("D30").custom_range().expect("the catalog has labels");
        assert_eq!(
            range.min,
            MediaSize {
                width: 2500,
                length: 1200
            }
        );
        assert_eq!(
            range.max,
            MediaSize {
                width: 819_900,
                length: 1500
            }
        );
    }

    #[test]
    fn the_custom_range_is_what_the_model_accepts() {
        for model in Model::all() {
            let CustomRange { min, max } = model.custom_range().expect("the catalog has labels");
            for (width, length) in [
                (min.width, min.length),
                (min.width, max.length),
                (max.width, min.length),
                (max.width, max.length),
            ] {
                assert!(
                    model.accepts_media(width, length),
                    "{} {width}x{length}",
                    model.name()
                );
            }
            assert!(
                !model.accepts_media(max.width + 1, min.length),
                "{}",
                model.name()
            );
            assert!(
                !model.accepts_media(min.width, max.length + 1),
                "{}",
                model.name()
            );
        }
    }

    #[test]
    fn millimetres_without_trailing_zeros() {
        assert_eq!(millimetres(2000), "20");
        assert_eq!(millimetres(7210), "72.1");
        assert_eq!(millimetres(7207), "72.07");
    }

    #[test]
    fn default_media_comes_with_its_name() {
        let (preset, name) = model("M220").default_media();
        assert_eq!(preset.size_name, "om_40x30mm_40x30mm");
        assert_eq!(name, c"om_40x30mm_40x30mm");
    }

    #[test]
    fn media_fit_in_pappl() {
        for model in Model::all() {
            assert!(
                model.media().count() + model.custom_range_names().count()
                    <= crate::pappl::PM_MAX_MEDIA,
                "{}",
                model.name()
            );
        }
    }
}
