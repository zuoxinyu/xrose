//! Desktop pointer coordinates remain available while another window is active.
use super::platform::Pointer;
use gpui_kit::{DisplayId, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{ffi::c_void, ptr::NonNull};

unsafe extern "C" {
    fn xrose_win_begin(hwnd: isize) -> *mut c_void;
    fn xrose_win_input(
        session: *mut c_void,
        x: *mut f32,
        y: *mut f32,
        cx: *mut f32,
        cy: *mut f32,
        revision: *mut u32,
    ) -> u32;
    fn xrose_win_end(session: *mut c_void);
    fn xrose_win_cursor(session: *mut c_void, visible: bool);
}
pub(super) struct DesktopInput {
    session: NonNull<c_void>,
    scale: f32,
}
impl DesktopInput {
    pub(super) fn new(window: &Window, _display: DisplayId) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        let session = NonNull::new(unsafe { xrose_win_begin(handle.hwnd.get()) })?;
        Some(Self {
            session,
            scale: window.scale_factor(),
        })
    }
    pub(super) fn sample(&mut self) -> Pointer {
        let (mut x, mut y, mut cx, mut cy, mut revision) = (0., 0., 0., 0., 0);
        let flags = unsafe {
            xrose_win_input(
                self.session.as_ptr(),
                &mut x,
                &mut y,
                &mut cx,
                &mut cy,
                &mut revision,
            )
        };
        Pointer {
            position: [x / self.scale, y / self.scale],
            clicked: (flags & 1 != 0).then_some([cx / self.scale, cy / self.scale]),
            cancel: flags & 2 != 0,
            revision,
        }
    }
    pub(super) fn set_cursor_visible(&mut self, visible: bool) {
        unsafe { xrose_win_cursor(self.session.as_ptr(), visible) }
    }
}
impl Drop for DesktopInput {
    fn drop(&mut self) {
        unsafe { xrose_win_end(self.session.as_ptr()) }
    }
}
