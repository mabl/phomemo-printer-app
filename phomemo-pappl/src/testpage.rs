const TEST_PAGE_WIDTH: u32 = 640;
const TEST_PAGE_HEIGHT: u32 = 400;

pub fn encode_test_page_png() -> Result<Vec<u8>, png::EncodingError> {
    let mut image = GrayImage::new(TEST_PAGE_WIDTH, TEST_PAGE_HEIGHT)
        .expect("test page dimensions must be valid");

    draw_test_pattern(&mut image);

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, TEST_PAGE_WIDTH, TEST_PAGE_HEIGHT);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&image.data)?;
    }

    Ok(out)
}

fn draw_test_pattern(image: &mut GrayImage) {
    let width = image.width;
    let height = image.height;

    image.fill_rect(0, 0, width, height, u8::MAX);
    image.draw_border(0, 0, width, height, 4, 0);

    let left = 24;
    let top = 24;
    let content_width = width.saturating_sub(left * 2);

    draw_grayscale_ramp(image, left, top, content_width, 48, 16);
    image.draw_border(left, top, content_width, 48, 1, 0);

    let checker_top = top + 72;
    let checker_height = 170;
    draw_checkerboard(
        image,
        left,
        checker_top,
        content_width,
        checker_height,
        16,
        (0, u8::MAX),
    );
    image.draw_border(left, checker_top, content_width, checker_height, 1, 0);

    let bars_top = checker_top + checker_height + 20;
    let bars_height = 56;
    draw_vertical_bars(
        image,
        left,
        bars_top,
        content_width,
        bars_height,
        &[0, 32, 64, 96, 128, 160, 192, 224, 255],
    );
    image.draw_border(left, bars_top, content_width, bars_height, 1, 0);

    let center_x = width / 2;
    let center_y = height / 2;
    image.fill_rect(center_x.saturating_sub(80), center_y, 161, 1, 0);
    image.fill_rect(center_x, center_y.saturating_sub(60), 1, 121, 0);

    draw_registration_marks(
        image,
        16,
        16,
        width.saturating_sub(16),
        height.saturating_sub(16),
    );
}

fn draw_grayscale_ramp(
    image: &mut GrayImage,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    segments: usize,
) {
    if segments == 0 {
        return;
    }

    for segment in 0..segments {
        let x0 = x + (segment * width) / segments;
        let x1 = x + ((segment + 1) * width) / segments;
        let denominator = segments.saturating_sub(1).max(1);
        let numerator = denominator.saturating_sub(segment) * usize::from(u8::MAX);
        let value = u8::try_from(numerator / denominator).unwrap_or(u8::MAX);
        image.fill_rect(x0, y, x1.saturating_sub(x0), height, value);
    }
}

fn draw_checkerboard(
    image: &mut GrayImage,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    cell_size: usize,
    colors: (u8, u8),
) {
    if cell_size == 0 {
        return;
    }

    let cols = width.div_ceil(cell_size);
    let rows = height.div_ceil(cell_size);

    for row in 0..rows {
        for col in 0..cols {
            let value = if (row + col) % 2 == 0 {
                colors.0
            } else {
                colors.1
            };
            image.fill_rect(
                x + col * cell_size,
                y + row * cell_size,
                cell_size,
                cell_size,
                value,
            );
        }
    }
}

fn draw_vertical_bars(
    image: &mut GrayImage,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    values: &[u8],
) {
    if values.is_empty() {
        return;
    }

    for (idx, value) in values.iter().enumerate() {
        let x0 = x + (idx * width) / values.len();
        let x1 = x + ((idx + 1) * width) / values.len();
        image.fill_rect(x0, y, x1.saturating_sub(x0), height, *value);
    }
}

fn draw_registration_marks(
    image: &mut GrayImage,
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
) {
    if right <= left || bottom <= top {
        return;
    }

    let mark = 12;
    image.fill_rect(left, top, mark, 2, 0);
    image.fill_rect(left, top, 2, mark, 0);

    image.fill_rect(right.saturating_sub(mark), top, mark, 2, 0);
    image.fill_rect(right.saturating_sub(2), top, 2, mark, 0);

    image.fill_rect(left, bottom.saturating_sub(2), mark, 2, 0);
    image.fill_rect(left, bottom.saturating_sub(mark), 2, mark, 0);

    image.fill_rect(
        right.saturating_sub(mark),
        bottom.saturating_sub(2),
        mark,
        2,
        0,
    );
    image.fill_rect(
        right.saturating_sub(2),
        bottom.saturating_sub(mark),
        2,
        mark,
        0,
    );
}

struct GrayImage {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl GrayImage {
    fn new(width: u32, height: u32) -> Option<Self> {
        let width = usize::try_from(width).ok()?;
        let height = usize::try_from(height).ok()?;
        let len = width.checked_mul(height)?;
        Some(Self {
            width,
            height,
            data: vec![u8::MAX; len],
        })
    }

    fn draw_border(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        thickness: usize,
        value: u8,
    ) {
        if thickness == 0 || width == 0 || height == 0 {
            return;
        }

        self.fill_rect(x, y, width, thickness, value);
        self.fill_rect(
            x,
            y + height.saturating_sub(thickness),
            width,
            thickness,
            value,
        );
        self.fill_rect(x, y, thickness, height, value);
        self.fill_rect(
            x + width.saturating_sub(thickness),
            y,
            thickness,
            height,
            value,
        );
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, value: u8) {
        if x >= self.width || y >= self.height || width == 0 || height == 0 {
            return;
        }

        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);

        for yy in y..y_end {
            let row_start = yy * self.width;
            let start = row_start + x;
            let end = row_start + x_end;
            self.data[start..end].fill(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_test_page_png, TEST_PAGE_HEIGHT, TEST_PAGE_WIDTH};
    use std::io::Cursor;

    #[test]
    fn encodes_png_signature() {
        let png = encode_test_page_png().expect("test page PNG must encode");
        assert!(png.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]));
    }

    #[test]
    fn decodes_expected_dimensions() {
        let png = encode_test_page_png().expect("test page PNG must encode");
        let decoder = png::Decoder::new(Cursor::new(png));
        let mut reader = decoder.read_info().expect("PNG must decode");
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("PNG frame must decode");

        assert_eq!(info.width, TEST_PAGE_WIDTH);
        assert_eq!(info.height, TEST_PAGE_HEIGHT);
    }
}
