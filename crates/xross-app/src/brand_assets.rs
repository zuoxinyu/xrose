//! Embedded brand assets merged with the GPUI Kit icon catalogue.
//!
//! GPUI resolves `img("logo-mark.png")` through the registered [`AssetSource`].
//! The Kit's icon set stays available for every component.

use std::borrow::Cow;
use std::sync::Arc;

use gpui_kit::{AssetSource, RenderImage, Result, SharedString};

const LOGO_MARK: &[u8] = include_bytes!("../assets/logo-mark.png");
const APP_ICON: &[u8] = include_bytes!("../assets/app-icon.png");
const APP_ICON_256: &[u8] = include_bytes!("../assets/app-icon-256.png");
const APP_ICON_128: &[u8] = include_bytes!("../assets/app-icon-128.png");
const APP_ICON_96: &[u8] = include_bytes!("../assets/app-icon-96.png");
const APP_ICON_64: &[u8] = include_bytes!("../assets/app-icon-64.png");

const BRAND_PATHS: [(&str, &[u8]); 6] = [
    ("logo-mark.png", LOGO_MARK),
    ("app-icon.png", APP_ICON),
    ("app-icon-256.png", APP_ICON_256),
    ("app-icon-128.png", APP_ICON_128),
    ("app-icon-96.png", APP_ICON_96),
    ("app-icon-64.png", APP_ICON_64),
];

#[derive(Clone, Copy, Debug, Default)]
pub struct BrandAssets;

impl AssetSource for BrandAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND_PATHS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = gpui_kit::assets::Assets.list(path)?;
        items.extend(
            BRAND_PATHS
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| name.starts_with(path))
                .map(|name| SharedString::from(name)),
        );
        Ok(items)
    }
}

/// Decodes an embedded PNG into a GPU-ready image up front, so logos appear on
/// the very first frame instead of waiting for the asynchronous asset loader.
pub fn render_png(bytes: &[u8]) -> Arc<RenderImage> {
    let decoded = image::load_from_memory(bytes).expect("embedded brand PNG must decode");
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    // GPUI stores image bytes as BGRA.
    let mut bgra = Vec::with_capacity((width * height * 4) as usize);
    for pixel in rgba.pixels() {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    let buffer = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(width, height, bgra)
        .expect("brand image dimensions must match its buffer");
    let frame = image::Frame::new(buffer);
    Arc::new(RenderImage::new(smallvec::smallvec![frame]))
}

pub fn logo_badge() -> Arc<RenderImage> {
    render_png(APP_ICON_128)
}

pub fn logo_hero() -> Arc<RenderImage> {
    render_png(APP_ICON)
}
