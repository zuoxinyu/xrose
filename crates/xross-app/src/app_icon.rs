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
    use std::sync::Once;

    use cocoa::appkit::NSApplication;
    use cocoa::base::{id, nil, BOOL};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    static FILE_ICON_ONCE: Once = Once::new();

    fn load_icon_image() -> id {
        let bytes = include_bytes!("../assets/app-icon-256.png");
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
}
