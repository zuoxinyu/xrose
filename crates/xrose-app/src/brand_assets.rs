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
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrayGlyph {
    rose: Option<xrose_core::service::TrayIndicator>,
    white_ink: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
impl TrayGlyph {
    pub(crate) fn new(status: &xrose_core::service::TrayStatus, white_ink: bool) -> Self {
        use xrose_core::service::TrayIndicator::Direct;
        let rose =
            (status.show_indicator && status.indicator != Direct).then_some(status.indicator);
        Self { rose, white_ink }
    }

    pub(crate) fn png(self) -> &'static [u8] {
        use xrose_core::service::TrayIndicator::*;
        macro_rules! asset {
            ($name:literal) => {
                if self.white_ink {
                    include_bytes!(concat!("../assets/tray/white-", $name, ".png"))
                } else {
                    include_bytes!(concat!("../assets/tray/black-", $name, ".png"))
                }
            };
        }
        match self.rose {
            None | Some(Direct) => asset!("plain"),
            Some(Idle) => asset!("idle"),
            Some(Proxy) => asset!("proxy"),
            Some(MeshOnly) => asset!("mesh"),
            Some(MeshProxy) => asset!("mesh-proxy"),
            Some(Unavailable) => asset!("unavailable"),
        }
    }

    /// Resample with associated alpha so transparent pixels cannot darken the thin strokes.
    #[cfg(any(target_os = "linux", windows, test))]
    pub(crate) fn raster(self, size: u32) -> image::RgbaImage {
        let mut rgba = image::load_from_memory(self.png())
            .expect("embedded tray PNG")
            .to_rgba8();
        for p in rgba.pixels_mut() {
            let a = u16::from(p[3]);
            for v in &mut p.0[..3] {
                *v = ((u16::from(*v) * a + 127) / 255) as u8;
            }
        }
        image::imageops::resize(&rgba, size, size, image::imageops::FilterType::Triangle)
    }
}

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
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = gpui_kit::assets::AllAssets.list(path)?;
        items.extend(
            BRAND_PATHS
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| name.starts_with(path))
                .map(SharedString::from),
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

#[cfg(test)]
mod tests {
    use super::*;
    use xrose_core::service::TrayIndicator::*;

    #[test]
    fn only_the_rose_changes_at_tray_size_on_either_background() {
        for white_ink in [false, true] {
            let plain = TrayGlyph {
                rose: None,
                white_ink,
            }
            .raster(22);
            for state in [Idle, Proxy, MeshOnly, MeshProxy, Unavailable] {
                let icon = TrayGlyph {
                    rose: Some(state),
                    white_ink,
                }
                .raster(22);
                let mut changed = 0;
                let mut blue = false;
                let mut purple = false;
                let mut red = false;
                for (x, y, p) in icon.enumerate_pixels() {
                    let mono = plain.get_pixel(x, y);
                    assert_eq!(p[3], mono[3]);
                    let ink = if white_ink { mono[3] } else { 0 };
                    assert_eq!(mono.0, [ink, ink, ink, mono[3]]);
                    // Include the one-pixel antialiasing fringe after resampling.
                    if !(5..17).contains(&x) || !(3..15).contains(&y) {
                        assert_eq!(p, mono, "{state:?} at ({x}, {y})");
                    }
                    if p != mono && p[3] > 60 {
                        changed += 1;
                        blue |= p[1] > p[0] && p[2] > p[1];
                        purple |= p[2] > p[0] && p[0] > p[1];
                        red |= p[0] > p[1] && p[0] > p[2];
                    }
                }
                assert!(changed > 0);
                match state {
                    Idle => assert!(!purple && !red),
                    Proxy => assert!(blue && !purple && !red),
                    MeshOnly => assert!(purple && !blue && !red),
                    MeshProxy => assert!(blue && purple && !red),
                    Unavailable => assert!(red && !blue && !purple),
                    Direct => unreachable!(),
                }
            }
        }
    }
}
