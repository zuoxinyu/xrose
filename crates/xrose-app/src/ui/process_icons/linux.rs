//! Resolve desktop application icons using the user's freedesktop icon theme.
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::PIXELS;

struct DesktopIcons {
    theme: String,
    names: HashMap<String, String>,
}

impl DesktopIcons {
    fn discover() -> Self {
        let mut names = HashMap::new();
        for entry in freedesktop_desktop_entry::desktop_entries(&[]) {
            let Some(icon) = entry.icon() else { continue };
            let command = entry.parse_exec().ok();
            let executable = entry
                .try_exec()
                .or_else(|| command.as_ref()?.first().map(String::as_str));
            let basename = executable.and_then(|exe| Path::new(exe).file_name()?.to_str());
            for key in [
                Some(entry.id()),
                entry.startup_wm_class(),
                executable,
                basename,
            ]
            .into_iter()
            .flatten()
            {
                if !matches!(key, "" | "env" | "sh" | "bash" | "flatpak") {
                    names
                        .entry(key.to_owned())
                        .or_insert_with(|| icon.to_owned());
                }
            }
        }
        let kde = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .any(|de| de.eq_ignore_ascii_case("KDE"));
        let theme = if kde {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")));
            config
                .and_then(|dir| std::fs::read_to_string(dir.join("kdeglobals")).ok())
                .and_then(|text| {
                    text.lines()
                        .skip_while(|s| s.trim() != "[Icons]")
                        .skip(1)
                        .take_while(|s| !s.starts_with('['))
                        .find_map(|s| s.strip_prefix("Theme="))
                        .map(|s| s.trim().to_owned())
                })
                .unwrap_or_else(|| "breeze".into())
        } else {
            freedesktop_icons::default_theme_gtk().unwrap_or_else(|| "Adwaita".into())
        };
        Self { names, theme }
    }
}

pub(super) fn load(key: &str) -> Option<image::RgbaImage> {
    static DESKTOP: OnceLock<DesktopIcons> = OnceLock::new();
    let desktop = DESKTOP.get_or_init(DesktopIcons::discover);
    let basename = Path::new(key)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let icon = desktop
        .names
        .get(key)
        .or_else(|| desktop.names.get(basename));
    let names: &[&str] = if key.is_empty() {
        &["application-x-executable", "application-default-icon"]
    } else {
        &[icon.map(String::as_str).unwrap_or(basename), basename]
    };
    for &name in names {
        let path = if Path::new(name).is_absolute() {
            Some(PathBuf::from(name))
        } else if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            None
        } else {
            freedesktop_icons::lookup(name)
                .with_size(PIXELS as u16)
                .with_theme(&desktop.theme)
                .find()
        };
        if let Some(pixels) = path.as_deref().and_then(decode) {
            return Some(pixels);
        }
    }
    None
}

fn decode(path: &Path) -> Option<image::RgbaImage> {
    const MAX_BYTES: u64 = 4 * 1024 * 1024;
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut data = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut data).ok()?;
    if data.len() as u64 > MAX_BYTES {
        return None;
    }
    let mut pixels = if path.extension()?.eq_ignore_ascii_case("svg") {
        let tree = resvg::usvg::Tree::from_data(&data, &Default::default()).ok()?;
        let size = tree.size();
        let scale = PIXELS as f32 / size.width().max(size.height());
        let mut pixmap = resvg::tiny_skia::Pixmap::new(PIXELS, PIXELS)?;
        let transform = resvg::tiny_skia::Transform::from_row(
            scale,
            0.,
            0.,
            scale,
            (PIXELS as f32 - scale * size.width()) / 2.,
            (PIXELS as f32 - scale * size.height()) / 2.,
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        image::RgbaImage::from_fn(PIXELS, PIXELS, |x, y| {
            let c = pixmap.pixels()[(y * PIXELS + x) as usize].demultiply();
            image::Rgba([c.red(), c.green(), c.blue(), c.alpha()])
        })
    } else {
        let mut reader = image::ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(2048);
        limits.max_image_height = Some(2048);
        reader.limits(limits);
        reader.decode().ok()?.thumbnail(PIXELS, PIXELS).into_rgba8()
    };
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(pixels)
}
