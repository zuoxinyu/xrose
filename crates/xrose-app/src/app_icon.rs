//! Application (taskbar/Dock) icon installation.
//!
//! - **Windows**: the icon is embedded into the executable by `build.rs` as
//!   resource ID 1. GPUI's Windows platform loads that exact resource for the
//!   window class, which is what Explorer and the taskbar display; no runtime
//!   call is needed.
//! - **macOS**: `WindowOptions.icon` only affects the window, not the Dock.
//!   Set `NSApplication.applicationIconImage` explicitly from the bundled PNG;
//!   packaged applications use their `.icns` for Finder and notifications.
//! - **Linux**: GPUI receives the same `WindowOptions.icon` and publishes it as
//!   the X11/Wayland window icon; installed launchers use the bundled
//!   `assets/icons/xrose.png` through the `.desktop` file.

/// Sets the custom icon on the executable file before `NSApplication` registers
/// with LaunchServices / Stage Manager. Must not touch `NSApplication` so GPUI
/// can still register `GPUIApplication` as the shared application instance.
pub fn preinstall_executable_icon() {
    #[cfg(target_os = "macos")]
    macos::preinstall_executable_icon();
}

/// Installs the platform application icon. Call once during startup, after the
/// GPUI application has been initialized.
pub fn install() {
    #[cfg(target_os = "macos")]
    macos::set_dock_icon();
}

/// Sets the window thumbnail/miniature icon for Stage Manager and WindowServer.
#[cfg(target_os = "macos")]
pub fn setup_window_icon(window: cocoa::base::id) {
    macos::setup_window_icon(window);
}

#[cfg(not(target_os = "macos"))]
pub fn setup_window_icon() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::sync::{Once, OnceLock};

    use cocoa::appkit::NSApplication;
    use cocoa::base::{BOOL, id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    static FILE_ICON_ONCE: Once = Once::new();
    static ICON_PNG_BYTES: OnceLock<Vec<u8>> = OnceLock::new();

    /// Signed distance to a rounded rectangle centered at the origin with
    /// half-extents `(hx, hy)` and corner radius `r`.
    fn sd_rounded_box(px: f32, py: f32, hx: f32, hy: f32, r: f32) -> f32 {
        let qx = px.abs() - (hx - r);
        let qy = py.abs() - (hy - r);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        let inside = qx.max(qy).min(0.0);
        outside + inside - r
    }

    /// Synthesizes a 512x512 macOS squircle application icon with a white
    /// rounded-rectangle background, soft drop shadow, and centered Xrose mark.
    pub(crate) fn build_macos_icon_png() -> Vec<u8> {
        let src_bytes = include_bytes!("../assets/app-icon.png");
        let Ok(src_img) = image::load_from_memory(src_bytes) else {
            return src_bytes.to_vec();
        };

        const SIZE: u32 = 512;
        const CENTER: f32 = 256.0;
        const HALF: f32 = 206.0; // 412x412 squircle on 512x512 canvas (Apple grid)
        const RADIUS: f32 = 92.0;

        let logo_size: u32 = 332;
        let logo_offset: u32 = (SIZE - logo_size) / 2;
        let logo = src_img
            .resize_exact(logo_size, logo_size, image::imageops::FilterType::Lanczos3)
            .to_rgba8();

        let mut canvas = image::RgbaImage::new(SIZE, SIZE);

        for y in 0..SIZE {
            for x in 0..SIZE {
                let fx = x as f32 + 0.5 - CENTER;
                let fy = y as f32 + 0.5 - CENTER;

                let d_card = sd_rounded_box(fx, fy, HALF, HALF, RADIUS);
                // Smooth 1px anti-aliased coverage for the squircle body.
                let card_cov = (0.5 - d_card).clamp(0.0, 1.0);

                // Drop shadow centered slightly below the card.
                let d_shadow = sd_rounded_box(fx, fy - 7.0, HALF - 2.0, HALF - 2.0, RADIUS);
                let shadow_t = (d_shadow.max(0.0) / 18.0).clamp(0.0, 1.0);
                let shadow_alpha = 0.22 * (1.0 - shadow_t).powi(3);

                // Predominantly white card fill with a subtle vertical luminance gradient
                // and a crisp 1px outer rim.
                let v_t = ((fy + HALF) / (2.0 * HALF)).clamp(0.0, 1.0);
                let mut card_r = 255.0 - 7.0 * v_t;
                let mut card_g = 255.0 - 5.0 * v_t;
                let mut card_b = 255.0 - 2.0 * v_t;

                // Subtle border rim along the outer 1.5px of the card.
                if d_card > -1.8 && d_card < 0.5 {
                    let rim_t = (1.0 - (d_card + 0.65).abs() / 1.15).clamp(0.0, 1.0);
                    let rim_shade = if fy < 0.0 { 255.0 } else { 226.0 };
                    card_r = card_r * (1.0 - 0.35 * rim_t) + rim_shade * (0.35 * rim_t);
                    card_g = card_g * (1.0 - 0.35 * rim_t)
                        + (rim_shade + 2.0).min(255.0) * (0.35 * rim_t);
                    card_b = card_b * (1.0 - 0.35 * rim_t)
                        + (rim_shade + 6.0).min(255.0) * (0.35 * rim_t);
                }

                // Composite foreground logo onto the white card.
                if x >= logo_offset
                    && x < logo_offset + logo_size
                    && y >= logo_offset
                    && y < logo_offset + logo_size
                {
                    let lp = logo.get_pixel(x - logo_offset, y - logo_offset);
                    let la = ((lp[3] as f32 / 255.0) * 1.25).clamp(0.0, 1.0);
                    if la > 0.0 {
                        // Slightly deepen the logo colors for crisp contrast on white.
                        let lr = (lp[0] as f32) * 0.88;
                        let lg = (lp[1] as f32) * 0.86;
                        let lb = (lp[2] as f32) * 0.94;
                        card_r = lr * la + card_r * (1.0 - la);
                        card_g = lg * la + card_g * (1.0 - la);
                        card_b = lb * la + card_b * (1.0 - la);
                    }
                }

                // Blend card over drop shadow in premultiplied alpha.
                let out_a = card_cov + shadow_alpha * (1.0 - card_cov);
                if out_a > 1e-4 {
                    let out_r = (card_r * card_cov) / out_a;
                    let out_g = (card_g * card_cov) / out_a;
                    let out_b = (card_b * card_cov) / out_a;
                    canvas.put_pixel(
                        x,
                        y,
                        image::Rgba([
                            out_r.round().clamp(0.0, 255.0) as u8,
                            out_g.round().clamp(0.0, 255.0) as u8,
                            out_b.round().clamp(0.0, 255.0) as u8,
                            (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
                        ]),
                    );
                }
            }
        }

        let mut out = Vec::new();
        if canvas
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .is_ok()
        {
            out
        } else {
            src_bytes.to_vec()
        }
    }

    fn load_icon_image() -> id {
        let bytes = ICON_PNG_BYTES.get_or_init(build_macos_icon_png);
        unsafe {
            let data: id = msg_send![
                class!(NSData),
                dataWithBytes: bytes.as_ptr() as *const c_void
                length: bytes.len()
            ];
            if data == nil {
                return nil;
            }
            let image: id = msg_send![class!(NSImage), alloc];
            msg_send![image, initWithData: data]
        }
    }

    unsafe fn set_executable_file_icon(image: id) {
        FILE_ICON_ONCE.call_once(|| unsafe {
            let bundle: id = msg_send![class!(NSBundle), mainBundle];
            let identifier: id = msg_send![bundle, bundleIdentifier];
            // Finder custom icons add resource forks to the executable and invalidate
            // signed bundles. Bundled applications declare their icon in Info.plist.
            if identifier != nil {
                return;
            }
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            let Some(exe_str) = exe.to_str() else {
                return;
            };
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace == nil {
                return;
            }
            let path_ns = NSString::alloc(nil).init_str(exe_str);
            if path_ns != nil {
                let _: BOOL = msg_send![
                    workspace,
                    setIcon: image
                    forFile: path_ns
                    options: 0usize
                ];
                let _: () = msg_send![path_ns, release];
            }
        });
    }

    pub fn preinstall_executable_icon() {
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let image = load_icon_image();
            if image != nil {
                set_executable_file_icon(image);
                let _: () = msg_send![image, release];
            }
            let _: () = msg_send![pool, drain];
        }
    }

    pub fn set_dock_icon() {
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let image = load_icon_image();
            if image != nil {
                set_executable_file_icon(image);
                let app = NSApplication::sharedApplication(nil);
                let _: () = msg_send![app, setApplicationIconImage: image];
                let tile: id = msg_send![app, dockTile];
                if tile != nil {
                    let _: () = msg_send![tile, display];
                }
                let _: () = msg_send![image, release];
            }
            let _: () = msg_send![pool, drain];
        }
    }

    pub fn setup_window_icon(window: id) {
        if window == nil {
            return;
        }
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let image = load_icon_image();
            if image != nil {
                set_executable_file_icon(image);
                let _: () = msg_send![window, setMiniwindowImage: image];
                let app = NSApplication::sharedApplication(nil);
                let _: () = msg_send![app, setApplicationIconImage: image];
                let tile: id = msg_send![app, dockTile];
                if tile != nil {
                    let _: () = msg_send![tile, display];
                }
                let _: () = msg_send![image, release];
            }
            let _: () = msg_send![pool, drain];
        }
    }

    #[cfg(test)]
    mod tests {
        use super::build_macos_icon_png;

        #[test]
        fn macos_squircle_icon_has_white_card_and_transparent_corners() {
            let png = build_macos_icon_png();
            let decoded = image::load_from_memory(&png).expect("valid PNG").to_rgba8();
            assert_eq!(decoded.dimensions(), (512, 512));
            // Outer corner outside the 412x412 squircle is transparent.
            assert_eq!(decoded.get_pixel(4, 4)[3], 0);
            // Card interior near top-middle (above the X mark) is opaque white.
            let card_px = decoded.get_pixel(256, 75);
            assert_eq!(card_px[3], 255);
            assert!(card_px[0] >= 245 && card_px[1] >= 245 && card_px[2] >= 245);
        }
    }
}
