//! Native desktop capture and external drag payloads.
#[cfg(target_os = "macos")]
use super::decode::Input;
use gpui_kit::*;
use image::RgbaImage;

pub(super) type CaptureError = (&'static str, &'static str);
const UNAVAILABLE: CaptureError = (
    "无法截取当前屏幕。请检查屏幕共享权限后重试",
    "Could not capture this display. Check screen sharing permissions and try again.",
);

#[derive(Default)]
pub(super) struct Pointer {
    pub position: [f32; 2],
    pub clicked: Option<[f32; 2]>,
    pub cancel: bool,
    pub revision: u32,
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        cell::{Cell, RefCell},
        ffi::c_void,
    };
    use tokio::sync::mpsc;
    thread_local! {
        static ENABLED: Cell<bool> = const { Cell::new(false) };
        static SENDER: RefCell<Option<mpsc::Sender<Input>>> = const { RefCell::new(None) };
    }
    unsafe extern "C" {
        fn xrose_capture(
            id: u32,
            region: *const f32,
            ctx: *mut c_void,
            receive: extern "C" fn(*mut c_void, u32, u32, *const u8),
        ) -> i32;
        fn xrose_install_drop(view: *mut c_void);
        fn xrose_begin_finder(view: *mut c_void, display: u32) -> *mut c_void;
        fn xrose_finder_input(
            session: *mut c_void,
            x: *mut f32,
            y: *mut f32,
            cx: *mut f32,
            cy: *mut f32,
            revision: *mut u32,
        ) -> u32;
        fn xrose_end_finder(session: *mut c_void);
        fn xrose_finder_cursor(session: *mut c_void, visible: bool);
        fn xrose_main_visible(view: *mut c_void, show: bool);
        fn xrose_paste_image(
            limit: usize,
            ctx: *mut c_void,
            receive: extern "C" fn(*mut c_void, *const u8, usize),
        ) -> i32;
    }
    fn view(window: &Window) -> Option<*mut c_void> {
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
            _ => None,
        }
    }
    pub(super) fn capture(id: DisplayId, region: [f32; 4]) -> Result<RgbaImage, CaptureError> {
        extern "C" fn receive(ctx: *mut c_void, w: u32, h: u32, data: *const u8) {
            // The native bitmap is packed RGBA, bounded and alive for this callback.
            unsafe {
                *ctx.cast::<Option<RgbaImage>>() = RgbaImage::from_raw(
                    w,
                    h,
                    std::slice::from_raw_parts(data, w as usize * h as usize * 4).to_vec(),
                );
            }
        }
        let mut pixels: Option<RgbaImage> = None;
        match unsafe {
            xrose_capture(
                u64::from(id) as u32,
                region.as_ptr(),
                (&raw mut pixels).cast(),
                receive,
            )
        } {
            0 => pixels.ok_or(UNAVAILABLE),
            1 => Err((
                "请在系统设置的「隐私与安全性 → 屏幕与系统录音」中允许 Xrose 录制屏幕，然后重试",
                "Allow Xrose in System Settings → Privacy & Security → Screen Recording, then try again.",
            )),
            _ => Err(UNAVAILABLE),
        }
    }
    pub(in super::super) fn paste_image() -> Result<Option<Input>, super::super::decode::Issue> {
        extern "C" fn receive(ctx: *mut c_void, bytes: *const u8, len: usize) {
            // The pasteboard retains the immutable NSData through this callback.
            unsafe {
                *ctx.cast::<Option<Input>>() = Some(Input::Image(
                    std::slice::from_raw_parts(bytes, len).to_vec(),
                ));
            }
        }
        let mut image: Option<Input> = None;
        let status = unsafe {
            xrose_paste_image(
                super::super::decode::MAX_BYTES,
                (&raw mut image).cast(),
                receive,
            )
        };
        if status < 0 {
            Err(super::super::decode::Issue::TooLarge)
        } else {
            Ok(image)
        }
    }
    pub(in super::super) struct DesktopInput(std::ptr::NonNull<c_void>);
    impl DesktopInput {
        pub(in super::super) fn new(window: &Window, display: DisplayId) -> Option<Self> {
            std::ptr::NonNull::new(unsafe {
                xrose_begin_finder(view(window)?, u64::from(display) as u32)
            })
            .map(Self)
        }
        pub(in super::super) fn sample(&mut self) -> Pointer {
            let (mut x, mut y, mut cx, mut cy, mut revision) = (0., 0., 0., 0., 0);
            let flags = unsafe {
                xrose_finder_input(
                    self.0.as_ptr(),
                    &mut x,
                    &mut y,
                    &mut cx,
                    &mut cy,
                    &mut revision,
                )
            };
            Pointer {
                position: [x, y],
                clicked: (flags & 1 != 0).then_some([cx, cy]),
                cancel: flags & 2 != 0,
                revision,
            }
        }
        pub(in super::super) fn set_cursor_visible(&mut self, visible: bool) {
            unsafe { xrose_finder_cursor(self.0.as_ptr(), visible) }
        }
    }
    impl Drop for DesktopInput {
        fn drop(&mut self) {
            unsafe { xrose_end_finder(self.0.as_ptr()) }
        }
    }
    pub(in super::super) fn main_visible(window: &Window, show: bool) {
        if let Some(v) = view(window) {
            unsafe {
                xrose_main_visible(v, show);
            }
        }
    }
    pub(in super::super) fn install(window: &Window) -> mpsc::Receiver<Input> {
        let (tx, rx) = mpsc::channel(4);
        SENDER.set(Some(tx));
        if let Some(v) = view(window) {
            unsafe {
                xrose_install_drop(v);
            }
        }
        rx
    }
    pub(in super::super) fn enable_drop(enabled: bool) {
        ENABLED.set(enabled);
    }
    #[unsafe(no_mangle)]
    extern "C" fn xrose_accepts_drop() -> bool {
        ENABLED.get()
    }
    #[unsafe(no_mangle)]
    extern "C" fn xrose_receive_drop(image: bool, data: *const u8, len: usize) -> bool {
        if !ENABLED.get() || data.is_null() || len == 0 || len > super::super::decode::MAX_BYTES {
            return false;
        }
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        let input = if image {
            Input::Image(bytes.to_vec())
        } else if let Ok(s) = std::str::from_utf8(bytes) {
            Input::Text(s.to_owned())
        } else {
            return false;
        };
        SENDER.with_borrow(|tx| tx.as_ref().is_some_and(|tx| tx.try_send(input).is_ok()))
    }
}
#[cfg(target_os = "macos")]
pub(super) use mac::{DesktopInput, enable_drop, install, main_visible, paste_image};

pub(super) fn capture(
    id: DisplayId,
    center: (i32, i32),
    region: [f32; 4],
) -> Result<RgbaImage, CaptureError> {
    #[cfg(target_os = "macos")]
    {
        let _ = center;
        mac::capture(id, region)
    }
    #[cfg(windows)]
    {
        let _ = center;
        let monitor = xcap::Monitor::all()
            .map_err(|_| UNAVAILABLE)?
            .into_iter()
            .find(|m| m.id().ok() == Some(u64::from(id) as u32))
            .ok_or(UNAVAILABLE)?;
        capture_region(&monitor, region)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = id;
        capture_region(
            &xcap::Monitor::from_point(center.0, center.1).map_err(|_| UNAVAILABLE)?,
            region,
        )
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = (id, center, region);
        Err(UNAVAILABLE)
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn capture_region(monitor: &xcap::Monitor, region: [f32; 4]) -> Result<RgbaImage, CaptureError> {
    let width = monitor.width().map_err(|_| UNAVAILABLE)?;
    let height = monitor.height().map_err(|_| UNAVAILABLE)?;
    let [l, t, w, h] = region;
    let x = (l * width as f32).floor() as u32;
    let y = (t * height as f32).floor() as u32;
    let w = ((w * width as f32).ceil() as u32).min(width.saturating_sub(x));
    let h = ((h * height as f32).ceil() as u32).min(height.saturating_sub(y));
    if w == 0 || h == 0 {
        return Err(UNAVAILABLE);
    }
    // Wayland portals capture in desktop coordinates; crop the selected monitor's image locally.
    #[cfg(target_os = "linux")]
    if uses_portal() {
        let desktop = monitor.capture_image().map_err(|_| UNAVAILABLE)?;
        return Ok(image::imageops::crop_imm(&desktop, x, y, w, h).to_image());
    }
    monitor.capture_region(x, y, w, h).map_err(|_| UNAVAILABLE)
}

#[cfg(target_os = "linux")]
pub(super) use super::linux::DesktopInput;
#[cfg(windows)]
pub(super) use super::windows::DesktopInput;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(super) struct DesktopInput;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl DesktopInput {
    pub(super) fn new(_: &Window, _: DisplayId) -> Option<Self> {
        None
    }
    pub(super) fn sample(&mut self) -> Pointer {
        Pointer::default()
    }
    pub(super) fn set_cursor_visible(&mut self, _: bool) {}
}

pub(super) fn uses_portal() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some()
}
