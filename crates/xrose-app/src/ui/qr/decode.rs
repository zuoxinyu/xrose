//! Bounded, in-memory decoding shared by screenshots, drops and the clipboard.

use image::{GrayImage, ImageReader};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
};

pub(super) const MAX_BYTES: usize = 32 << 20;
pub(super) const MAX_PIXELS: u64 = 36_000_000;
pub(super) const MAX_TEXT: usize = 4 << 20;

pub(super) enum Input {
    Text(String),
    Image(Vec<u8>),
    File(PathBuf),
}

#[derive(Clone)]
pub(super) struct Code {
    pub text: String,
    /// Left, top, right, bottom as fractions of the original image dimensions.
    pub rect: [f32; 4],
}

fn scan(gray: &GrayImage) -> Vec<Code> {
    let (w, h) = gray.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return vec![];
    }
    let mut scanner = quircs::Quirc::default();
    scanner
        .identify(w as usize, h as usize, gray.as_raw())
        .flatten()
        .filter_map(|code| {
            let text = String::from_utf8(code.decode().ok()?.payload).ok()?;
            if text.trim().is_empty() {
                return None;
            }
            let xs = || code.corners.iter().map(|p| p.x);
            let ys = || code.corners.iter().map(|p| p.y);
            Some(Code {
                text,
                rect: [
                    xs().min()? as f32 / w as f32,
                    ys().min()? as f32 / h as f32,
                    xs().max()? as f32 / w as f32,
                    ys().max()? as f32 / h as f32,
                ],
            })
        })
        .collect()
}

/// The live finder samples a crop and bounds both luma allocation and QR work.
pub(super) fn screen_codes(image: &image::RgbaImage) -> Vec<Code> {
    let (w, h) = image.dimensions();
    let step = w.max(h).div_ceil(1024).max(1);
    let gray = GrayImage::from_fn(w.div_ceil(step), h.div_ceil(step), |x, y| {
        let [r, g, b, _] = image.get_pixel(x * step, y * step).0;
        image::Luma([((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8])
    });
    scan(&gray)
}

/// A small whole-frame pass finds large codes quickly. Overlapping full-scale
/// tiles preserve tiny codes and bound the decoder's histogram and working set.
pub(super) fn identify(
    gray: &GrayImage,
    mut preview: impl FnMut(&[Code]),
    cancelled: impl Fn() -> bool,
) -> Vec<Code> {
    const TILE: u32 = 1536;
    const STEP: u32 = 1152;
    let (w, h) = gray.dimensions();
    if u64::from(w) * u64::from(h) > MAX_PIXELS || cancelled() {
        return vec![];
    }
    if w.max(h) <= TILE {
        return scan(gray);
    }
    let factor = w.max(h).div_ceil(TILE);
    let thumb = GrayImage::from_fn(w.div_ceil(factor), h.div_ceil(factor), |x, y| {
        *gray.get_pixel(x * factor, y * factor)
    });
    let mut codes = scan(&thumb);
    if !codes.is_empty() {
        preview(&codes);
    }
    let starts = |n: u32| {
        (0..=n.saturating_sub(TILE).div_ceil(STEP))
            .map(move |i| (i * STEP).min(n.saturating_sub(TILE)))
    };
    for y in starts(h) {
        for x in starts(w) {
            if cancelled() {
                return vec![];
            }
            let (tw, th) = (w.min(TILE), h.min(TILE));
            let tile = image::imageops::crop_imm(gray, x, y, tw, th).to_image();
            for mut code in scan(&tile) {
                let [l, t, r, b] = code.rect;
                code.rect = [
                    (x as f32 + l * tw as f32) / w as f32,
                    (y as f32 + t * th as f32) / h as f32,
                    (x as f32 + r * tw as f32) / w as f32,
                    (y as f32 + b * th as f32) / h as f32,
                ];
                if !codes.iter().any(|other| {
                    let [ol, ot, or, ob] = other.rect;
                    other.text == code.text
                        && (ol + or - code.rect[0] - code.rect[2]).abs() < (or - ol)
                        && (ot + ob - code.rect[1] - code.rect[3]).abs() < (ob - ot)
                }) {
                    codes.push(code);
                }
            }
        }
    }
    codes.sort_by(|a, b| {
        a.rect[1]
            .total_cmp(&b.rect[1])
            .then(a.rect[0].total_cmp(&b.rect[0]))
    });
    codes
}

#[derive(Clone, Copy)]
pub(super) enum Issue {
    NoCode,
    Unsupported,
    TooLarge,
    Read,
}

impl Issue {
    pub fn message(self) -> (&'static str, &'static str) {
        match self {
            Self::NoCode => (
                "图片中未识别到二维码，请使用清晰、完整的图片",
                "No QR code found. Use a clear image of the whole code.",
            ),
            Self::Unsupported => (
                "请粘贴链接、二维码图片或拖入文件",
                "Paste a link or QR image, or drop a file.",
            ),
            Self::TooLarge => (
                "内容过大，请裁剪图片或分批导入",
                "The content is too large. Crop the image or import in batches.",
            ),
            Self::Read => (
                "无法读取文件，请确认文件可用",
                "The file could not be read. Check that it is available.",
            ),
        }
    }
}

pub(super) fn extract(input: Input) -> Result<Vec<String>, Issue> {
    fn text(s: String) -> Result<Vec<String>, Issue> {
        if s.len() > MAX_TEXT {
            Err(Issue::TooLarge)
        } else if s.trim().is_empty() {
            Err(Issue::Unsupported)
        } else {
            Ok(vec![s])
        }
    }
    let bytes = match input {
        Input::Text(s) => return text(s),
        Input::Image(bytes) => bytes,
        Input::File(path) => {
            let meta = path.metadata().map_err(|_| Issue::Read)?;
            if !meta.is_file() {
                return Err(Issue::Unsupported);
            }
            if meta.len() > MAX_BYTES as u64 {
                return Err(Issue::TooLarge);
            }
            let mut bytes = Vec::new();
            File::open(&path)
                .map_err(|_| Issue::Read)?
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| Issue::Read)?;
            if bytes.len() <= MAX_TEXT && image::guess_format(&bytes).is_err() {
                return text(String::from_utf8(bytes).map_err(|_| Issue::Unsupported)?);
            }
            bytes
        }
    };
    if bytes.len() > MAX_BYTES {
        return Err(Issue::TooLarge);
    }
    let format = image::guess_format(&bytes).map_err(|_| Issue::Unsupported)?;
    let reader = || ImageReader::with_format(Cursor::new(&bytes), format);
    let (w, h) = reader().into_dimensions().map_err(|_| Issue::Unsupported)?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(Issue::TooLarge);
    }
    let gray = reader()
        .decode()
        .map_err(|_| Issue::Unsupported)?
        .into_luma8();
    let codes = identify(&gray, |_| {}, || false);
    if codes.is_empty() {
        Err(Issue::NoCode)
    } else {
        Ok(codes.into_iter().map(|c| c.text).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, Luma};
    use qrcode::{Color, QrCode};
    fn draw(image: &mut GrayImage, text: &str, x: u32, y: u32) {
        let qr = QrCode::new(text).unwrap();
        let w = qr.width();
        for j in 0..w {
            for i in 0..w {
                if qr[(i, j)] == Color::Dark {
                    for yy in 0..4 {
                        for xx in 0..4 {
                            image.put_pixel(
                                x + i as u32 * 4 + xx,
                                y + j as u32 * 4 + yy,
                                Luma([0]),
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn decodes_multiple_codes_and_locates_them_at_display_scale() {
        let mut img = GrayImage::from_pixel(640, 340, Luma([255]));
        let a = "socks://node.example:1080#Demo";
        let b = "https://feed.example/test";
        draw(&mut img, a, 32, 64);
        draw(&mut img, b, 390, 140);
        let codes = identify(&img, |_| {}, || false);
        assert_eq!(codes.len(), 2);
        let left = codes.iter().find(|c| c.text == a).unwrap();
        let right = codes.iter().find(|c| c.text == b).unwrap();
        assert!((left.rect[0] - 32. / 640.).abs() < 0.01);
        assert!((right.rect[1] - 140. / 340.).abs() < 0.01);
        let (index, _) = super::super::finder::nearest(&codes, [224., 98.], [320., 170.]).unwrap();
        assert_eq!(codes[index].text, b);
        assert!(super::super::finder::nearest(&codes, [318., 1.], [320., 170.]).is_none());
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(img)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        assert!(extract(Input::Image(bytes.into_inner())).is_ok_and(|v| v.len() == 2));
    }
    #[test]
    fn large_frame_keeps_separate_copies_without_double_counting_overlapping_tiles() {
        let mut img = GrayImage::from_pixel(2304, 540, Luma([255]));
        let payload = "socks://tile.example:1080";
        draw(&mut img, payload, 1160, 30);
        draw(&mut img, payload, 1850, 270);
        let codes = identify(&img, |_| {}, || false);
        assert_eq!(codes.len(), 2);
        assert!((codes[1].rect[0] - 1850. / 2304.).abs() < 0.005);
    }

    #[test]
    fn blank_and_corrupt_images_do_not_produce_payloads() {
        let mut png = Cursor::new(Vec::new());
        GrayImage::new(80, 80)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        assert!(matches!(
            extract(Input::Image(png.into_inner())),
            Err(Issue::NoCode)
        ));
        assert!(matches!(
            extract(Input::Image(vec![0, 1, 2])),
            Err(Issue::Unsupported)
        ));
    }
}
