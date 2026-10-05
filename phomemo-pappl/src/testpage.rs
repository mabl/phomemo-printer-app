//! The test page: PAPPL's `testpage_cb`.
//!
//! A grayscale PNG laid out for a 640-dot reference width and scaled to the
//! loaded media: a frame, a gray ramp, a checkerboard and a row of gray bars
//! to show the dithering, a centre cross and corner registration marks. Its
//! resolution is the printer's and it fits the page PAPPL rasterizes for
//! the media ([`page_width`]), so PAPPL draws it dot for dot.

use std::ffi::c_int;
use std::fs::File;
use std::io::Write;
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;

use png::{BitDepth, ColorType, Encoder, EncodingError, PixelDimensions, Unit};

use crate::models::{Model, PmModel};

/// The width the layout is drawn for.
const REFERENCE_WIDTH: usize = 640;

const BLACK: u8 = 0;
const WHITE: u8 = u8::MAX;

/// The test page's width in dots on `model` with media `width` x `length`
/// hundredths of a millimetre loaded (either 0 if unknown).
///
/// As wide as the page PAPPL rasterizes for the media - in dots as PAPPL
/// counts them, truncated - but no wider than the head on models that never
/// turn a page onto it, and narrow enough for the test page's 8:5 shape to
/// fit within a label's length.
#[must_use]
pub fn page_width(model: &Model, width: c_int, length: c_int) -> u16 {
    let dpi = c_int::from(model.info().dpi);
    let dots = |hundredths: c_int| hundredths.saturating_mul(dpi) / 2540;
    let head = c_int::from(model.info().max_width_px);
    let mut across = if width > 0 { dots(width) } else { head };
    if !model.has_sideways_media() {
        across = across.min(head);
    }
    if length > 0 {
        across = across.min(dots(length).saturating_mul(8) / 5);
    }
    u16::try_from(across.max(1)).unwrap_or(u16::MAX)
}

/// The test page `width` dots wide at `dpi`, as a PNG.
///
/// # Errors
///
/// Fails only if the PNG encoder does, which for an in-memory image of
/// valid dimensions it does not.
pub fn encode_png(width: u16, dpi: u16) -> Result<Vec<u8>, EncodingError> {
    let canvas = Canvas::test_page(usize::from(width.max(1)));
    let (width, height) = (canvas.width_u32(), canvas.height_u32());
    // Rounded up: PAPPL truncates the dots per inch it reads back
    // (`png_get_x_pixels_per_inch`), and 7992 per metre would give 202.
    let dots_per_metre = (u32::from(dpi) * 10_000).div_ceil(254);

    let mut out = Vec::new();
    let mut encoder = Encoder::new(&mut out, width, height);
    encoder.set_color(ColorType::Grayscale);
    encoder.set_depth(BitDepth::Eight);
    encoder.set_pixel_dims(Some(PixelDimensions {
        xppu: dots_per_metre,
        yppu: dots_per_metre,
        unit: Unit::Meter,
    }));
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&canvas.pixels)?;
    writer.finish()?;
    Ok(out)
}

/// Write the test page for `model` with media `width` x `length`
/// hundredths of a millimetre loaded to file descriptor `fd`, which stays
/// open. Returns false if `model` is not one of the table's views (it is
/// only compared against them) or the page cannot be written.
///
/// # Safety
///
/// `fd` must be an open file descriptor, writable, that nothing else uses
/// during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_write_testpage_png(
    fd: c_int,
    model: *const PmModel,
    width: c_int,
    length: c_int,
) -> bool {
    let Some(model) = Model::from_view(model) else {
        return false;
    };
    let width = page_width(model, width, length);
    let Ok(png) = encode_png(width, model.info().dpi) else {
        return false;
    };
    // SAFETY: `fd` is open and not in use elsewhere; `ManuallyDrop` leaves
    // closing it to the caller, who owns it.
    let mut file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    file.write_all(&png).is_ok()
}

/// An 8-bit grayscale image to draw on.
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    /// The test page, `width` dots wide.
    fn test_page(width: usize) -> Self {
        // Lengths in the 640-dot layout, scaled to `width`.
        let scale = |length: usize| (length * width / REFERENCE_WIDTH).max(1);
        let height = scale(400);
        let mut canvas = Self {
            width,
            height,
            pixels: vec![WHITE; width * height],
        };

        canvas.draw_frame(0, 0, width, height, scale(4));

        let left = scale(24);
        let content_width = width.saturating_sub(2 * left);

        let ramp_top = scale(24);
        let ramp_height = scale(48);
        canvas.draw_ramp(left, ramp_top, content_width, ramp_height, 16);
        canvas.draw_frame(left, ramp_top, content_width, ramp_height, 1);

        let checker_top = ramp_top + ramp_height + scale(24);
        let checker_height = scale(170);
        canvas.draw_checkerboard(left, checker_top, content_width, checker_height, scale(16));
        canvas.draw_frame(left, checker_top, content_width, checker_height, 1);

        let bars_top = checker_top + checker_height + scale(20);
        let bars_height = scale(56);
        canvas.draw_bars(
            left,
            bars_top,
            content_width,
            bars_height,
            &[0, 32, 64, 96, 128, 160, 192, 224, 255],
        );
        canvas.draw_frame(left, bars_top, content_width, bars_height, 1);

        let (centre_x, centre_y) = (width / 2, height / 2);
        let (arm_x, arm_y) = (scale(80), scale(60));
        canvas.fill(
            centre_x.saturating_sub(arm_x),
            centre_y,
            2 * arm_x + 1,
            1,
            BLACK,
        );
        canvas.fill(
            centre_x,
            centre_y.saturating_sub(arm_y),
            1,
            2 * arm_y + 1,
            BLACK,
        );

        let inset = scale(16);
        canvas.draw_registration_marks(
            inset,
            inset,
            width.saturating_sub(inset),
            height.saturating_sub(inset),
            scale(12),
        );
        canvas
    }

    fn width_u32(&self) -> u32 {
        u32::try_from(self.width).unwrap_or(u32::MAX)
    }

    fn height_u32(&self) -> u32 {
        u32::try_from(self.height).unwrap_or(u32::MAX)
    }

    /// Fill a rectangle, clipped to the canvas.
    fn fill(&mut self, x: usize, y: usize, width: usize, height: usize, value: u8) {
        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);
        if x >= x_end {
            return;
        }
        for row in y..y_end {
            let start = row * self.width;
            self.pixels[start + x..start + x_end].fill(value);
        }
    }

    /// A black frame `thickness` wide just inside a rectangle.
    fn draw_frame(&mut self, x: usize, y: usize, width: usize, height: usize, thickness: usize) {
        let right = (x + width).saturating_sub(thickness);
        let bottom = (y + height).saturating_sub(thickness);
        self.fill(x, y, width, thickness, BLACK);
        self.fill(x, bottom, width, thickness, BLACK);
        self.fill(x, y, thickness, height, BLACK);
        self.fill(right, y, thickness, height, BLACK);
    }

    /// `steps` gray steps from black on the left to white on the right.
    fn draw_ramp(&mut self, x: usize, y: usize, width: usize, height: usize, steps: usize) {
        let last = steps.saturating_sub(1).max(1);
        for step in 0..steps {
            let x0 = x + step * width / steps;
            let x1 = x + (step + 1) * width / steps;
            let value = u8::try_from(step * usize::from(WHITE) / last).unwrap_or(WHITE);
            self.fill(x0, y, x1 - x0, height, value);
        }
    }

    /// Black and white squares `cell` dots wide.
    fn draw_checkerboard(&mut self, x: usize, y: usize, width: usize, height: usize, cell: usize) {
        for row in 0..height.div_ceil(cell) {
            for col in 0..width.div_ceil(cell) {
                let value = if (row + col) % 2 == 0 { BLACK } else { WHITE };
                let cell_x = x + col * cell;
                let cell_y = y + row * cell;
                let cell_width = cell.min(x + width - cell_x);
                let cell_height = cell.min(y + height - cell_y);
                self.fill(cell_x, cell_y, cell_width, cell_height, value);
            }
        }
    }

    /// Equal-width bars of the given grays.
    fn draw_bars(&mut self, x: usize, y: usize, width: usize, height: usize, values: &[u8]) {
        for (index, &value) in values.iter().enumerate() {
            let x0 = x + index * width / values.len();
            let x1 = x + (index + 1) * width / values.len();
            self.fill(x0, y, x1 - x0, height, value);
        }
    }

    /// An L-shaped mark `size` dots long in each corner of a rectangle.
    fn draw_registration_marks(
        &mut self,
        left: usize,
        top: usize,
        right: usize,
        bottom: usize,
        size: usize,
    ) {
        if right <= left || bottom <= top {
            return;
        }
        let (far_x, far_y) = (right.saturating_sub(size), bottom.saturating_sub(size));
        let (edge_x, edge_y) = (right.saturating_sub(2), bottom.saturating_sub(2));
        for (x, y) in [(left, top), (far_x, top), (left, far_y), (far_x, far_y)] {
            let vertical_x = if x == left { left } else { edge_x };
            let horizontal_y = if y == top { top } else { edge_y };
            self.fill(x, horizontal_y, size, 2, BLACK);
            self.fill(vertical_x, y, 2, size, BLACK);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn decode(png: &[u8]) -> (png::OutputInfo, Option<PixelDimensions>, Vec<u8>) {
        let decoder = png::Decoder::new(Cursor::new(png));
        let mut reader = decoder.read_info().expect("PNG decodes");
        let dims = reader.info().pixel_dims;
        let size = reader.output_buffer_size().expect("frame fits in memory");
        let mut pixels = vec![0; size];
        let info = reader.next_frame(&mut pixels).expect("frame decodes");
        (info, dims, pixels)
    }

    #[test]
    fn page_width_follows_the_media() {
        let model = |name| Model::by_name(name).expect("known model");
        // 40 x 30 mm on an M220: as wide as PAPPL's 319-dot page.
        assert_eq!(page_width(model("M220"), 4000, 3000), 319);
        // A roll has no length to fit, and nothing loaded means the head.
        assert_eq!(page_width(model("M220"), 4000, 0), 319);
        assert_eq!(page_width(model("M220"), 0, 0), 576);
        // 50 x 30 mm on an M110: narrower than the 384-dot head, for the
        // 8:5 shape to fit 239 dots of length.
        assert_eq!(page_width(model("M110"), 5000, 3000), 382);
        assert_eq!(page_width(model("M110"), 5000, 0), 384);
        // 30 x 15 mm on a D30: the page is turned onto the head later, and
        // 119 dots of label length allow 190 dots of width.
        assert_eq!(page_width(model("D30"), 3000, 1500), 190);
    }

    #[test]
    fn page_fits_the_label_it_is_sized_for() {
        for (name, width, length) in [
            ("M220", 4000, 3000),
            ("M110", 5000, 3000),
            ("D30", 3000, 1500),
        ] {
            let model = Model::by_name(name).expect("known model");
            let (info, _, _) =
                decode(&encode_png(page_width(model, width, length), 203).expect("encodes"));
            // PAPPL's page for the label, in whole dots.
            let dots =
                |hundredths: c_int| u32::try_from(hundredths * 203 / 2540).expect("positive");
            assert!(info.width <= dots(width), "{name}");
            assert!(info.height <= dots(length), "{name}");
        }
    }

    #[test]
    fn page_keeps_its_shape() {
        for (width, height) in [(576, 360), (384, 240), (96, 60), (1, 1)] {
            let (info, _, _) = decode(&encode_png(width, 203).expect("encodes"));
            assert_eq!((info.width, info.height), (u32::from(width), height));
            assert_eq!(info.color_type, ColorType::Grayscale);
        }
    }

    #[test]
    fn page_carries_the_printer_resolution() {
        let (_, dims, _) = decode(&encode_png(576, 203).expect("encodes"));
        let dims = dims.expect("pHYs chunk");
        assert_eq!((dims.xppu, dims.yppu, dims.unit), (7993, 7993, Unit::Meter));
    }

    #[test]
    fn page_has_ink_and_paper() {
        let (_, _, pixels) = decode(&encode_png(96, 203).expect("encodes"));
        assert!(pixels.contains(&BLACK));
        assert!(pixels.contains(&WHITE));
    }

    #[test]
    fn ffi_refuses_unknown_models() {
        // SAFETY: the model is refused before the descriptor is touched.
        assert!(!unsafe { pm_write_testpage_png(-1, std::ptr::null(), 4000, 3000) });
    }
}
