//! The printer models this application drives, one PAPPL driver each.
//!
//! The table is built once from [`phomemo_protocol::model`] and the media
//! catalog, and owns every string C is handed: C holds [`PmModel`] views
//! into it (PAPPL's driver table, and each printer's driver `extension`),
//! and Rust maps a view back to its [`Model`] by address rather than by
//! reading strings back through C pointers.

use std::ffi::{CStr, CString, c_char, c_int, c_uint};
use std::ptr;
use std::sync::LazyLock;

use phomemo_protocol::media::{MediaPreset, PaperPool, paper_pool};
use phomemo_protocol::model::{self, ModelInfo};

use crate::media;
use crate::pappl::tracking_flag;

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
    /// Keeps the string `view.name` points to.
    _name: CString,
    view: PmModel,
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
        Some(Self {
            info,
            pool,
            default_media,
            driver_name,
            device_id,
            product,
            media_names,
            _name: name,
            view,
        })
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
    /// 48 mm head, 15 mm on a D30's 12 mm head) and are cropped to it.
    pub fn accepts_media(&self, width: c_int, length: c_int) -> bool {
        let widest = self
            .pool
            .media
            .iter()
            .filter_map(|preset| {
                self.across_head(preset.width_hundredths_mm(), preset.length_hundredths_mm())
            })
            .fold(self.head_width_hundredths_mm(), c_int::max);
        width > 0
            && self
                .across_head(width, length)
                .is_none_or(|across| across <= widest)
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

    /// The catalog's default media size, with its PWG name.
    pub fn default_media(&self) -> (&'static MediaPreset, &CStr) {
        // `new` found the index in `pool.media`, which `media_names`
        // parallels.
        let index = self.default_media;
        (&self.pool.media[index], &self.media_names[index])
    }

    /// Every media-tracking mode the model's media come in, as
    /// `pappl_media_tracking_t` bits.
    pub fn tracking_supported(&self) -> c_uint {
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
    fn default_media_comes_with_its_name() {
        let (preset, name) = model("M220").default_media();
        assert_eq!(preset.size_name, "om_40x30mm_40x30mm");
        assert_eq!(name, c"om_40x30mm_40x30mm");
    }

    #[test]
    fn media_fit_in_pappl() {
        for model in Model::all() {
            assert!(
                model.media().count() <= crate::pappl::PM_MAX_MEDIA,
                "{}",
                model.name()
            );
        }
    }
}
