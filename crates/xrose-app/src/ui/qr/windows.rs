//! Desktop pointer coordinates remain available while another window is active.
use super::platform::{Pointer, SelectionSource};
use gpui_kit::{DisplayId, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{ffi::c_void, ptr::NonNull};

unsafe extern "C" {
    fn xrose_win_begin(hwnd: isize, monitor: usize) -> *mut c_void;
    fn xrose_win_input(
        session: *mut c_void,
        x: *mut f32,
        y: *mut f32,
        cx: *mut f32,
        cy: *mut f32,
        revision: *mut u32,
        zoom: *mut f32,
    ) -> u32;
    fn xrose_win_import_sound(success: bool) -> bool;
    fn xrose_win_end(session: *mut c_void);
    fn xrose_win_set_hit(session: *mut c_void, hit: bool);
    fn xrose_win_capture(session: *mut c_void, exclude: bool);
}
pub(super) fn play_import_sound(success: bool) -> bool {
    unsafe { xrose_win_import_sound(success) }
}

/// XCap's fallback lookup uses device pixels, not GPUI's per-monitor logical coordinates.
pub(super) fn monitor_center(display: DisplayId) -> Option<(i32, i32)> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (unsafe { GetMonitorInfoW(u64::from(display) as usize as HMONITOR, &mut info) } != 0).then(
        || {
            let b = info.rcMonitor;
            (
                b.left + (b.right - b.left) / 2,
                b.top + (b.bottom - b.top) / 2,
            )
        },
    )
}

pub(super) struct DesktopInput {
    session: NonNull<c_void>,
    scale: f32,
}
impl DesktopInput {
    pub(super) fn new(window: &Window, display: DisplayId) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        let session = NonNull::new(unsafe {
            xrose_win_begin(handle.hwnd.get(), u64::from(display) as usize)
        })?;
        Some(Self {
            session,
            scale: window.scale_factor(),
        })
    }
    pub(super) fn sample(&mut self) -> Pointer {
        let (mut x, mut y, mut cx, mut cy, mut revision) = (0., 0., 0., 0., 0);
        let mut zoom = 0.;
        let flags = unsafe {
            xrose_win_input(
                self.session.as_ptr(),
                &mut x,
                &mut y,
                &mut cx,
                &mut cy,
                &mut revision,
                &mut zoom,
            )
        };
        Pointer {
            position: [x / self.scale, y / self.scale],
            selection: SelectionSource::from_native(flags)
                .map(|source| ([cx / self.scale, cy / self.scale], source)),
            pressed: flags & 4 != 0,
            zoom_delta: zoom,
            cancel: flags & 2 != 0,
            revision,
        }
    }
    pub(super) fn set_hit(&self, hit: bool) {
        unsafe { xrose_win_set_hit(self.session.as_ptr(), hit) }
    }
    pub(super) fn exclude_from_capture(&self, exclude: bool) {
        unsafe { xrose_win_capture(self.session.as_ptr(), exclude) }
    }
}
impl Drop for DesktopInput {
    fn drop(&mut self) {
        unsafe { xrose_win_end(self.session.as_ptr()) }
    }
}
