//! Geometry redaction, the same step as Dark Room's `redact.py`: fill each
//! rectangle solid black before the image leaves the tool.

use agent_desktop_core::{ImageBuffer, ImageFormat};

use super::probe::ScreenRect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PixelBox {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

/// Screen rectangles to image pixels, scaling by the capture's own size so
/// DPI differences between capture and window geometry cancel out. Each box
/// grows by 2 px to cover shadows and rounding.
pub(crate) fn map_to_image(boxes: &[ScreenRect], area: ScreenRect, width: u32, height: u32) -> Vec<PixelBox> {
    let aw = (area.right - area.left).max(1) as f64;
    let ah = (area.bottom - area.top).max(1) as f64;
    let sx = width as f64 / aw;
    let sy = height as f64 / ah;
    boxes
        .iter()
        .map(|b| {
            let clamp = |v: f64, max: u32| v.max(0.0).min(max as f64) as u32;
            PixelBox {
                x0: clamp(((b.left - area.left) as f64 * sx).floor() - 2.0, width),
                y0: clamp(((b.top - area.top) as f64 * sy).floor() - 2.0, height),
                x1: clamp(((b.right - area.left) as f64 * sx).ceil() + 2.0, width),
                y1: clamp(((b.bottom - area.top) as f64 * sy).ceil() + 2.0, height),
            }
        })
        .filter(|b| b.x1 > b.x0 && b.y1 > b.y0)
        .collect()
}

#[derive(Debug)]
pub(crate) struct RedactError;

pub(crate) fn black_out(image: ImageBuffer, boxes: &[PixelBox]) -> Result<ImageBuffer, RedactError> {
    if !matches!(image.format, ImageFormat::Png) {
        return Err(RedactError);
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(&image.data));
    let mut reader = decoder.read_info().map_err(|_| RedactError)?;
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or(RedactError)?];
    let info = reader.next_frame(&mut buf).map_err(|_| RedactError)?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err(RedactError);
    }
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err(RedactError),
    };
    let (w, h) = (info.width, info.height);
    let stride = info.line_size;
    for b in boxes {
        for y in b.y0.min(h)..b.y1.min(h) {
            let row = y as usize * stride;
            for x in b.x0.min(w)..b.x1.min(w) {
                let i = row + x as usize * channels;
                buf[i] = 0;
                buf[i + 1] = 0;
                buf[i + 2] = 0;
                if channels == 4 {
                    buf[i + 3] = 255;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(image.data.len());
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(info.color_type);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|_| RedactError)?;
        writer.write_image_data(&buf[..stride * h as usize]).map_err(|_| RedactError)?;
    }
    Ok(ImageBuffer { data: out, ..image })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_rgba(w: u32, h: u32, fill: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&vec![fill; (w * h * 4) as usize]).unwrap();
        drop(wr);
        out
    }

    fn pixel(data: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let mut r = png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
        let mut buf = vec![0u8; r.output_buffer_size().unwrap()];
        r.next_frame(&mut buf).unwrap();
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn maps_and_scales_screen_rects_into_the_image() {
        let area = ScreenRect { left: -100, top: 0, right: 100, bottom: 100 };
        let b = ScreenRect { left: 0, top: 10, right: 50, bottom: 20 };
        let m = map_to_image(&[b], area, 400, 200);
        assert_eq!(m, vec![PixelBox { x0: 198, y0: 18, x1: 302, y1: 42 }]);
    }

    #[test]
    fn blacks_out_only_inside_the_box() {
        let data = png_rgba(20, 10, 200);
        let img = ImageBuffer { data, format: ImageFormat::Png, width: 20, height: 10, scale_factor: 1.0 };
        let out = black_out(img, &[PixelBox { x0: 5, y0: 2, x1: 10, y1: 6 }]).unwrap();
        assert_eq!(pixel(&out.data, 20, 6, 3), [0, 0, 0, 255]);
        assert_eq!(pixel(&out.data, 20, 0, 0), [200, 200, 200, 200]);
        assert_eq!(pixel(&out.data, 20, 10, 6), [200, 200, 200, 200]);
    }

    #[test]
    fn non_png_input_is_refused() {
        let img = ImageBuffer { data: vec![1, 2, 3], format: ImageFormat::Jpg, width: 1, height: 1, scale_factor: 1.0 };
        assert!(black_out(img, &[]).is_err());
    }
}
