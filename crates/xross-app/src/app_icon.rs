//! Application (taskbar/Dock) icon installation.
//!
//! - **Windows**: the icon is embedded into the executable by `build.rs` as
//!   resource ID 1. GPUI's Windows platform loads that exact resource for the
//!   window class, which is what Explorer and the taskbar display; no runtime
//!   call is needed.
//! - **macOS**: `WindowOptions.icon` only affects the window, not the Dock.
//!   A development build has no app bundle/`.icns`, so set
//!   `NSApplication.applicationIconImage` explicitly from the bundled PNG.
//! - **Linux**: GPUI receives the same `WindowOptions.icon` and publishes it as
//!   the X11/Wayland window icon; installed launchers use the bundled
//!   `assets/icons/xross.png` through the `.desktop` file.

/// Installs the platform application icon. Call once during startup, after the
/// GPUI application has been initialized.
pub fn install() {
    #[cfg(target_os = "macos")]
    macos::set_dock_icon();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::sync::Once;

    use cocoa::appkit::NSApplication;
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    static INSTALL_ONCE: Once = Once::new();

    pub fn set_dock_icon() {
        INSTALL_ONCE.call_once(|| {
            let bytes = include_bytes!("../assets/app-icon-256.png");
            // SAFETY: NSData/NSImage are external objects; the byte buffer is
            // copied by NSData before this function returns. The `alloc`ed
            // NSImage is released after being handed to NSApplication (which
            // retains it).
            unsafe {
                let data: id = msg_send![
                    class!(NSData),
                    dataWithBytes: bytes.as_ptr() as *const c_void
                    length: bytes.len()
                ];
                if data == nil {
                    return;
                }
                let image: id = msg_send![class!(NSImage), alloc];
                let image: id = msg_send![image, initWithData: data];
                if image == nil {
                    return;
                }
                let app = NSApplication::sharedApplication(nil);
                let _: () = msg_send![app, setApplicationIconImage: image];
                let _: () = msg_send![image, release];
            }
        });
    }
}
