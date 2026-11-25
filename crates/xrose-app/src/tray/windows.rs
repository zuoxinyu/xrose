//! Windows notification icon; clicks queue the shared GPUI flyout.

use super::motion::{Edge, Motion, Placement};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use gpui_kit::{Bounds, Window, point, px};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, ClientToScreen, CreateBitmap, CreateDIBSection,
    DIB_RGB_COLORS, DeleteObject, GetDC, HGDIOBJ, ReleaseDC,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateIconIndirect, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyWindow, FindWindowW, GetClientRect, GetCursorPos,
    GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, HICON, ICONINFO, IDC_ARROW,
    IsIconic, IsWindow, LoadCursorW, MSG, MSGFLT_ALLOW, PM_NOREMOVE, PeekMessageW, PostMessageW,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SW_HIDE, SW_RESTORE, SW_SHOW,
    SetForegroundWindow, ShowWindow, WM_APP, WM_CONTEXTMENU, WM_LBUTTONUP, WM_RBUTTONUP,
    WM_SETTINGCHANGE, WM_THEMECHANGED, WNDCLASSW, WS_OVERLAPPED,
};

use super::TrayCommand as Command;
use super::dashboard;
use crate::brand_assets::TrayGlyph;
use xrose_core::AppService;
use xrose_core::service::TrayStatus;

const WM_TRAY: u32 = WM_APP + 1;
const WM_POPUP: u32 = WM_APP + 2;
const WM_WAKEUP: u32 = WM_APP + 3;
const FLYOUT_TIMER: usize = 0x5852;
static FLYOUT: Mutex<Option<Flyout>> = Mutex::new(None);
struct Flyout {
    hwnd: usize,
    placement: Placement,
    motion: Option<Motion>,
    shown: Option<tokio::sync::oneshot::Sender<()>>,
}

static POPUP_OPS: Mutex<Vec<PopupOp>> = Mutex::new(Vec::new());

enum PopupOp {
    Hide(usize),
    Resize {
        hwnd: usize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Show {
        hwnd: usize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        work: [i32; 4],
        edge: Edge,
        shown: tokio::sync::oneshot::Sender<()>,
    },
}

fn post_popup(operation: PopupOp) {
    POPUP_OPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(operation);
    if let Some((owner, _, _)) = icon_snapshot() {
        unsafe {
            PostMessageW(owner as HWND, WM_POPUP, 0, 0);
        }
    }
}

const TRAY_UID: u32 = 1;
const CLASS_NAME: &str = "XroseTrayWindow";
static DARK_TASKBAR: AtomicBool = AtomicBool::new(true);
static NEEDS_READD: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
static MAIN_HWND: OnceLock<Mutex<usize>> = OnceLock::new();
static ICON: OnceLock<Mutex<IconState>> = OnceLock::new();

struct IconState {
    hwnd: usize,
    icon: isize,
    tip: String,
}

pub struct Backend {
    last_tip: String,
    glyph: TrayGlyph,
    added: bool,
    add_attempts: u32,
    reported_failure: bool,
    /// ticks (~200 ms) to wait before retrying `Shell_NotifyIconW`.
    retry_ticks: u32,
    retry_delay: u32,
}

impl Backend {
    pub fn new(service: &AppService) -> Option<Self> {
        // SAFETY: called on the GPUI main thread; all pointers refer to live
        // buffers for the duration of each call.
        unsafe {
            // The GPUI message loop has not pumped yet, so force the thread
            // message queue into existence before talking to the shell.
            let mut message: MSG = std::mem::zeroed();
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);

            let class = wide(CLASS_NAME);
            let instance = GetModuleHandleW(std::ptr::null());
            let window_class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            // A duplicate registration (second app instance) is harmless.
            RegisterClassW(&window_class);
            let title = wide(&window_title(service.paths()));
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                debug_log("CreateWindowExW failed");
                return None;
            }
            let taskbar_created = *TASKBAR_CREATED
                .get_or_init(|| RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
            // Allow standard-integrity Explorer and secondary launches to deliver
            // tray callbacks, taskbar recreation notices, and single-instance
            // wakeups even when the GUI process runs elevated.
            for msg in [WM_TRAY, WM_WAKEUP, taskbar_created] {
                if msg != 0 {
                    ChangeWindowMessageFilterEx(hwnd, msg, MSGFLT_ALLOW, std::ptr::null_mut());
                }
            }
            let status = service.tray_status();
            let white_ink = taskbar_is_dark();
            DARK_TASKBAR.store(white_ink, Ordering::Relaxed);
            let glyph = TrayGlyph::new(&status, white_ink);
            let icon = make_icon(glyph);
            if icon == 0 {
                debug_log("make_icon failed");
                DestroyWindow(hwnd);
                return None;
            }
            let tip = dashboard::tooltip(&status);
            ICON.get_or_init(|| {
                Mutex::new(IconState {
                    hwnd: hwnd as usize,
                    icon,
                    tip: tip.clone(),
                })
            });
            let mut backend = Self {
                last_tip: tip,
                glyph,
                added: false,
                add_attempts: 0,
                reported_failure: false,
                retry_ticks: 0,
                retry_delay: 25,
            };
            // Registering can fail while the taskbar is starting up (or when
            // the host runs at a lower integrity level than the shell); tick()
            // retries with backoff, so a false here only delays the tray.
            backend.try_add();
            if !backend.added {
                backend.retry_ticks = backend.retry_delay;
            }
            Some(backend)
        }
    }

    /// Registers the icon with the shell. Idempotent and retried from tick().
    fn try_add(&mut self) -> bool {
        if self.added {
            return true;
        }
        let Some((hwnd, icon, tip)) = icon_snapshot() else {
            return false;
        };
        let data = notify_data(hwnd as HWND, icon as HICON, &tip);
        // SAFETY: data is a fully initialized NOTIFYICONDATAW.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0 {
            self.added = true;
            READY.store(true, Ordering::SeqCst);
            debug_log("tray icon registered");
            return true;
        }
        let error = unsafe { GetLastError() };
        self.add_attempts += 1;
        if self.add_attempts <= 3 {
            debug_log(&format!(
                "Shell_NotifyIconW(NIM_ADD) failed (error {error}, cbSize {})",
                data.cbSize
            ));
        }
        false
    }

    pub fn tick(&mut self, service: &AppService, _cx: &gpui_kit::App) -> Option<Command> {
        if NEEDS_READD.swap(false, Ordering::Relaxed) {
            self.added = false;
            self.retry_ticks = 0;
            READY.store(false, Ordering::SeqCst);
        }
        if !self.added {
            if self.retry_ticks > 0 {
                self.retry_ticks -= 1;
            } else if !self.try_add() {
                self.retry_ticks = self.retry_delay;
                self.retry_delay = (self.retry_delay * 2).min(300);
            }
            if !self.added && self.add_attempts == 3 && !self.reported_failure {
                self.reported_failure = true;
                service.notify(
                    xrose_core::service::ToastLevel::Warning,
                    crate::i18n::tr(
                        service.settings().language,
                        "系统拒绝注册托盘图标：关闭窗口将直接退出（可稍后重试或检查任务栏是否正常）",
                        "System rejected tray icon registration: closing the window will exit (retry later or check the taskbar)",
                    )
                    .to_string(),
                );
            }
        }
        if self.added {
            let status = service.tray_status();
            self.refresh_tooltip(&status);
            self.refresh_icon(TrayGlyph::new(
                &status,
                DARK_TASKBAR.load(Ordering::Relaxed),
            ));
        }
        None
    }

    fn refresh_tooltip(&mut self, status: &TrayStatus) {
        let tip = dashboard::tooltip(status);
        if tip == self.last_tip {
            return;
        }
        if let Some((hwnd, icon, _)) = icon_snapshot() {
            let data = notify_data(hwnd as HWND, icon as HICON, &tip);
            // SAFETY: data is a fully initialized NOTIFYICONDATAW.
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &data);
            }
            if let Some(mut state) = icon_state() {
                state.tip = tip.clone();
            }
        }
        self.last_tip = tip;
    }

    fn refresh_icon(&mut self, glyph: TrayGlyph) {
        if self.glyph == glyph {
            return;
        }
        let Some((hwnd, old_icon, tip)) = icon_snapshot() else {
            return;
        };
        let icon = make_icon(glyph);
        if icon == 0 {
            return;
        }
        let data = notify_data(hwnd as HWND, icon as HICON, &tip);
        if let Some(mut state) = icon_state() {
            state.icon = icon;
        }
        // SAFETY: the shell copies the icon; we own both source handles.
        unsafe {
            if Shell_NotifyIconW(NIM_MODIFY, &data) == 0 {
                NEEDS_READD.store(true, Ordering::Relaxed);
            }
            if old_icon != 0 {
                DestroyIcon(old_icon as HICON);
            }
        }
        self.glyph = glyph;
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        READY.store(false, Ordering::SeqCst);
        let Some((hwnd, icon, tip)) = icon_snapshot() else {
            return;
        };
        let data = notify_data(hwnd as HWND, icon as HICON, &tip);
        // SAFETY: handles belong to this process and are no longer used.
        unsafe {
            if self.added {
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            if icon != 0 {
                DestroyIcon(icon as HICON);
            }
            DestroyWindow(hwnd as HWND);
        }
        if let Some(mut state) = icon_state() {
            state.icon = 0;
        }
    }
}

pub fn available() -> bool {
    READY.load(Ordering::SeqCst)
}

pub fn hide_main_window() {
    if let Some(hwnd) = main_hwnd() {
        // SAFETY: hwnd belongs to this process.
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

pub fn show_main_window() {
    if let Some(hwnd) = main_hwnd() {
        // SAFETY: hwnd belongs to this process.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
    }
}

pub fn setup_window() {}

pub fn wake_existing(paths: &xrose_core::paths::AppPaths) -> bool {
    let class = wide(CLASS_NAME);
    let title = wide(&window_title(paths));
    // SAFETY: class and title are NUL-terminated UTF-16 buffers.
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), title.as_ptr());
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != 0 {
            AllowSetForegroundWindow(pid);
        }
        PostMessageW(hwnd, WM_WAKEUP, 0, 0) != 0
    }
}

fn window_title(paths: &xrose_core::paths::AppPaths) -> String {
    format!("XroseTray:{}", paths.instance_id())
}

fn icon_state() -> Option<MutexGuard<'static, IconState>> {
    ICON.get()
        .map(|mutex| mutex.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Copies the icon state so Win32 calls never run while the lock is held
/// (a shell callback could otherwise re-enter `readd_icon`).
fn icon_snapshot() -> Option<(usize, isize, String)> {
    icon_state().map(|state| (state.hwnd, state.icon, state.tip.clone()))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn debug_log(message: &str) {
    if std::env::var_os("XROSE_DEBUG").is_some() {
        eprintln!("[tray] {message}");
    }
}

fn fill_tip(buffer: &mut [u16; 128], value: &str) {
    let encoded: Vec<u16> = value.encode_utf16().take(126).collect();
    buffer[..encoded.len()].copy_from_slice(&encoded);
    buffer[encoded.len()] = 0;
}

fn notify_data(hwnd: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is a plain C struct; all-zero is a valid value
    // (its union is a pair of integers).
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    fill_tip(&mut data.szTip, tip);
    data
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == windows_sys::Win32::UI::WindowsAndMessaging::WM_TIMER
            && wparam == FLYOUT_TIMER
        {
            advance_flyout(hwnd);
            return 0;
        }
        if message == WM_POPUP {
            // Native visibility/geometry messages can synchronously re-enter GPUI.
            // Run them from our message window, after the GPUI update has finished.
            let operations =
                std::mem::take(&mut *POPUP_OPS.lock().unwrap_or_else(|e| e.into_inner()));
            for operation in operations {
                match operation {
                    PopupOp::Hide(target) if IsWindow(target as HWND) != 0 => {
                        animate_hide(hwnd, target)
                    }
                    PopupOp::Resize {
                        hwnd: target,
                        x,
                        y,
                        width,
                        height,
                    } if IsWindow(target as HWND) != 0 => {
                        use windows_sys::Win32::UI::WindowsAndMessaging::{
                            SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
                        };
                        SetWindowPos(
                            target as HWND,
                            std::ptr::null_mut(),
                            x,
                            y,
                            width,
                            height,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        if let Some(flyout) = FLYOUT
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_mut()
                            .filter(|f| f.hwnd == target)
                        {
                            flyout.placement.origin = [x, y];
                            flyout.placement.size = [width, height];
                        }
                    }
                    PopupOp::Show {
                        hwnd: target,
                        x,
                        y,
                        width,
                        height,
                        work,
                        edge,
                        shown,
                    } if IsWindow(target as HWND) != 0 => {
                        animate_show(
                            hwnd,
                            target,
                            Placement {
                                origin: [x, y],
                                size: [width, height],
                                work,
                                edge,
                            },
                            shown,
                        );
                    }
                    _ => {}
                }
            }
            return 0;
        }
        if message == WM_WAKEUP {
            super::queue_command(Command::ShowWindow);
            return 0;
        }
        if message == WM_TRAY {
            let event = (lparam as usize & 0xFFFF) as u32;
            match event {
                WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU => {
                    super::queue_command(Command::TogglePopup)
                }
                _ => {}
            }
            return 0;
        }
        if matches!(message, WM_SETTINGCHANGE | WM_THEMECHANGED) {
            DARK_TASKBAR.store(taskbar_is_dark(), Ordering::Relaxed);
        }
        if TASKBAR_CREATED
            .get()
            .map(|registered| *registered == message)
            .unwrap_or(false)
        {
            DARK_TASKBAR.store(taskbar_is_dark(), Ordering::Relaxed);
            NEEDS_READD.store(true, Ordering::Relaxed);
            return 0;
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

/// The shell and applications can use different Windows color modes.
fn taskbar_is_dark() -> bool {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value::<u32, _>("SystemUsesLightTheme"))
        .unwrap_or(0)
        == 0
}

/// Builds a premultiplied BGRA HICON for the current notification area size.
fn make_icon(glyph: TrayGlyph) -> isize {
    // SAFETY: GetSystemMetrics has no preconditions.
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    let mut bgra = glyph.raster(size).into_raw();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    // SAFETY: every GDI object is created and released in this function.
    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = size as i32;
        info.bmiHeader.biHeight = -(size as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dc = GetDC(std::ptr::null_mut());
        let color = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        ReleaseDC(std::ptr::null_mut(), dc);
        if color.is_null() || bits.is_null() {
            return 0;
        }
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());
        let stride = (((size + 15) / 16) * 2) as usize;
        let mask_bits = vec![0u8; stride * size as usize];
        let mask = CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            mask_bits.as_ptr() as *const c_void,
        );
        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info);
        DeleteObject(mask as HGDIOBJ);
        DeleteObject(color as HGDIOBJ);
        icon as isize
    }
}

fn main_hwnd() -> Option<HWND> {
    let cache = MAIN_HWND.get_or_init(|| Mutex::new(0));
    let cached = *cache.lock().unwrap_or_else(|e| e.into_inner());
    if cached != 0 {
        let hwnd = cached as HWND;
        // SAFETY: the handle came from this process; IsWindow tolerates stale
        // handles.
        if unsafe { IsWindow(hwnd) } != 0 {
            return Some(hwnd);
        }
    }
    None
}

/// Capture the exact GPUI main window before any flyout can be created.
pub fn register_main_window(window: &Window) {
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(raw) = handle.as_raw() {
            *MAIN_HWND
                .get_or_init(|| Mutex::new(0))
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = raw.hwnd.get() as usize;
        }
    }
}

/// Return logical coordinates on the icon's monitor, respecting per-monitor DPI.
pub fn popup_anchor() -> Option<super::PopupAnchor> {
    let (hwnd, _, _) = icon_snapshot()?;
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd as HWND,
            uID: TRAY_UID,
            guidItem: std::mem::zeroed(),
        };
        let mut rect: RECT = std::mem::zeroed();
        let has_rect = Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0;
        let at = if has_rect {
            POINT {
                x: (rect.left + rect.right) / 2,
                y: (rect.top + rect.bottom) / 2,
            }
        } else {
            cursor
        };
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let mut dpi_x = 96;
        let mut dpi_y = 96;
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x.max(96) as f32 / 96.;
        let work = info.rcWork;
        Some(super::PopupAnchor {
            position: point(px(at.x as f32 / scale), px(at.y as f32 / scale)),
            work_area: Some(Bounds::from_corners(
                point(px(work.left as f32 / scale), px(work.top as f32 / scale)),
                point(
                    px(work.right as f32 / scale),
                    px(work.bottom as f32 / scale),
                ),
            )),
        })
    }
}

pub fn pointer_on_icon() -> bool {
    let Some((hwnd, _, _)) = icon_snapshot() else {
        return false;
    };
    unsafe {
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd as HWND,
            uID: TRAY_UID,
            guidItem: std::mem::zeroed(),
        };
        let mut rect: RECT = std::mem::zeroed();
        let mut cursor = POINT { x: 0, y: 0 };
        Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0
            && GetCursorPos(&mut cursor) != 0
            && cursor.x >= rect.left
            && cursor.x < rect.right
            && cursor.y >= rect.top
            && cursor.y < rect.bottom
    }
}

/// Hide rather than destroy: AccessKit providers may outlive an accessibility query.
pub fn hide_popup(window: &mut Window) {
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(raw) = handle.as_raw() {
            post_popup(PopupOp::Hide(raw.hwnd.get() as usize));
        }
    }
}

/// Hide the flyout immediately during application exit without waiting for animation.
pub fn hide_popup_immediately(window: &mut Window) {
    if let Ok(handle) = HasWindowHandle::window_handle(window)
        && let RawWindowHandle::Win32(raw) = handle.as_raw()
    {
        let hwnd = raw.hwnd.get() as HWND;
        if let Some((owner, _, _)) = icon_snapshot() {
            // SAFETY: owner is the tray message window owned by this process.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(owner as HWND, FLYOUT_TIMER);
            }
        }
        *FLYOUT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        POPUP_OPS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // SAFETY: hwnd belongs to this process.
        unsafe {
            if IsWindow(hwnd) != 0 {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

pub fn show_popup(
    window: &mut Window,
    bounds: Bounds<gpui_kit::Pixels>,
    shown: tokio::sync::oneshot::Sender<()>,
) {
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(raw) = handle.as_raw() {
            let hwnd = raw.hwnd.get() as HWND;
            unsafe {
                let mut cursor = POINT { x: 0, y: 0 };
                GetCursorPos(&mut cursor);
                // The tray icon's current monitor, rather than the old popup DPI.
                if let Some((owner, _, _)) = icon_snapshot() {
                    let identifier = NOTIFYICONIDENTIFIER {
                        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                        hWnd: owner as HWND,
                        uID: TRAY_UID,
                        guidItem: std::mem::zeroed(),
                    };
                    let mut icon: RECT = std::mem::zeroed();
                    if Shell_NotifyIconGetRect(&identifier, &mut icon) >= 0 {
                        cursor = POINT {
                            x: (icon.left + icon.right) / 2,
                            y: (icon.top + icon.bottom) / 2,
                        };
                    }
                }
                let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
                let (mut dx, mut dy) = (96, 96);
                GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
                let scale = dx.max(96) as f32 / 96.;
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(monitor, &mut info) == 0 {
                    return;
                }
                let rect_array = |r: RECT| [r.left, r.top, r.right, r.bottom];
                let work = rect_array(info.rcWork);
                let edge = Edge::detect(rect_array(info.rcMonitor), work, [cursor.x, cursor.y]);
                let mut outer: RECT = std::mem::zeroed();
                let mut client: RECT = std::mem::zeroed();
                let mut origin = POINT { x: 0, y: 0 };
                let ratio = scale / window.scale_factor();
                let (left, top, extra_width, extra_height) = if GetWindowRect(hwnd, &mut outer) != 0
                    && GetClientRect(hwnd, &mut client) != 0
                    && ClientToScreen(hwnd, &mut origin) != 0
                {
                    (
                        ((origin.x - outer.left) as f32 * ratio).round() as i32,
                        ((origin.y - outer.top) as f32 * ratio).round() as i32,
                        ((outer.right - outer.left - client.right) as f32 * ratio).round() as i32,
                        ((outer.bottom - outer.top - client.bottom) as f32 * ratio).round() as i32,
                    )
                } else {
                    (0, 0, 0, 0)
                };
                post_popup(PopupOp::Show {
                    hwnd: hwnd as usize,
                    x: (f32::from(bounds.origin.x) * scale).round() as i32 - left,
                    y: (f32::from(bounds.origin.y) * scale).round() as i32 - top,
                    width: (f32::from(bounds.size.width) * scale).round() as i32 + extra_width,
                    height: (f32::from(bounds.size.height) * scale).round() as i32 + extra_height,
                    work,
                    edge,
                    shown,
                });
            }
        }
    }
}

pub fn resize_popup(window: &mut Window, bounds: Bounds<gpui_kit::Pixels>) {
    if let Ok(handle) = HasWindowHandle::window_handle(window)
        && let RawWindowHandle::Win32(raw) = handle.as_raw()
    {
        let hwnd = raw.hwnd.get() as HWND;
        unsafe {
            let mut outer: RECT = std::mem::zeroed();
            let mut client: RECT = std::mem::zeroed();
            let mut origin = POINT { x: 0, y: 0 };
            let scale = window.scale_factor();
            let (left, top, extra_width, extra_height) = if GetWindowRect(hwnd, &mut outer) != 0
                && GetClientRect(hwnd, &mut client) != 0
                && ClientToScreen(hwnd, &mut origin) != 0
            {
                (
                    origin.x - outer.left,
                    origin.y - outer.top,
                    outer.right - outer.left - client.right,
                    outer.bottom - outer.top - client.bottom,
                )
            } else {
                (0, 0, 0, 0)
            };
            post_popup(PopupOp::Resize {
                hwnd: hwnd as usize,
                x: (f32::from(bounds.origin.x) * scale).round() as i32 - left,
                y: (f32::from(bounds.origin.y) * scale).round() as i32 - top,
                width: (f32::from(bounds.size.width) * scale).round() as i32 + extra_width,
                height: (f32::from(bounds.size.height) * scale).round() as i32 + extra_height,
            });
        }
    }
}

fn animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SPI_GETUIEFFECTS, SystemParametersInfoW,
    };
    let (mut client, mut effects) = (0_i32, 0_i32);
    let system_enabled = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut client as *mut _ as _, 0) != 0
            && SystemParametersInfoW(SPI_GETUIEFFECTS, 0, &mut effects as *mut _ as _, 0) != 0
            && client != 0
            && effects != 0
    };
    crate::ui::expand_motion::resolve_platform_animations(system_enabled)
}

unsafe fn animate_show(
    owner: HWND,
    hwnd: usize,
    placement: Placement,
    shown: tokio::sync::oneshot::Sender<()>,
) {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_NOTOPMOST, IsWindowVisible, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos,
        };
        let mut current: RECT = std::mem::zeroed();
        let from = if IsWindowVisible(hwnd as HWND) != 0
            && GetWindowRect(hwnd as HWND, &mut current) != 0
        {
            [current.left, current.top]
        } else {
            placement.hidden_origin()
        };
        let enabled = animations_enabled();
        let origin = if enabled { from } else { placement.origin };
        let motion = enabled.then(|| Motion::new(true, from, placement, Instant::now()));
        *FLYOUT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Flyout {
            hwnd,
            placement,
            motion,
            shown: Some(shown),
        });
        // Slide below the topmost taskbar rather than painting over its icons.
        SetWindowPos(
            hwnd as HWND,
            HWND_NOTOPMOST,
            origin[0],
            origin[1],
            placement.size[0],
            placement.size[1],
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        SetForegroundWindow(hwnd as HWND);
        debug_log(&format!(
            "flyout opening ({hwnd}, {:?}, animate={enabled})",
            placement.edge
        ));
        if !enabled || !start_flyout_timer(owner) {
            finish_flyout(owner, true);
        }
    }
}

unsafe fn animate_hide(owner: HWND, hwnd: usize) {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_NOTOPMOST, IsWindowVisible, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
        };
        let placement = {
            let controller = FLYOUT.lock().unwrap_or_else(|e| e.into_inner());
            let Some(flyout) = controller.as_ref().filter(|flyout| flyout.hwnd == hwnd) else {
                ShowWindow(hwnd as HWND, SW_HIDE);
                return;
            };
            if flyout.motion.is_some_and(|motion| !motion.showing) {
                return;
            }
            flyout.placement
        };
        if IsWindowVisible(hwnd as HWND) == 0 {
            return;
        }
        let mut current: RECT = std::mem::zeroed();
        let from = if GetWindowRect(hwnd as HWND, &mut current) != 0 {
            [current.left, current.top]
        } else {
            placement.origin
        };
        let enabled = animations_enabled();
        let motion = enabled.then(|| Motion::new(false, from, placement, Instant::now()));
        *FLYOUT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Flyout {
            hwnd,
            placement,
            motion,
            shown: None,
        });
        SetWindowPos(
            hwnd as HWND,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOMOVE,
        );
        debug_log(&format!("flyout closing ({hwnd}, animate={enabled})"));
        if !enabled || !start_flyout_timer(owner) {
            finish_flyout(owner, false);
        }
    }
}

unsafe fn start_flyout_timer(owner: HWND) -> bool {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(owner, FLYOUT_TIMER, 10, None) != 0
    }
}

unsafe fn advance_flyout(owner: HWND) {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
        };
        let frame = {
            let controller = FLYOUT.lock().unwrap_or_else(|e| e.into_inner());
            controller.as_ref().and_then(|flyout| {
                flyout.motion.map(|motion| {
                    let (position, done) = motion.sample(Instant::now());
                    (flyout.hwnd, position, done, motion.showing)
                })
            })
        };
        let Some((hwnd, position, done, showing)) = frame else {
            return;
        };
        if IsWindow(hwnd as HWND) == 0 {
            finish_flyout(owner, false);
            return;
        }
        if SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
            position[0],
            position[1],
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        ) == 0
            || done
        {
            finish_flyout(owner, showing);
        }
    }
}

unsafe fn finish_flyout(owner: HWND, showing: bool) {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_NOTOPMOST, HWND_TOPMOST, KillTimer, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos,
        };
        KillTimer(owner, FLYOUT_TIMER);
        let target = {
            let mut controller = FLYOUT.lock().unwrap_or_else(|e| e.into_inner());
            controller.as_mut().map(|flyout| {
                flyout.motion = None;
                (flyout.hwnd, flyout.placement.origin, flyout.shown.take())
            })
        };
        let Some((hwnd, origin, shown)) = target else {
            return;
        };
        if IsWindow(hwnd as HWND) == 0 {
            return;
        }
        if !showing {
            ShowWindow(hwnd as HWND, SW_HIDE);
        }
        let positioned = SetWindowPos(
            hwnd as HWND,
            if showing {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            },
            origin[0],
            origin[1],
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE,
        ) != 0;
        if positioned
            && showing
            && let Some(shown) = shown
        {
            let _ = shown.send(());
        }
        debug_log(&format!("flyout settled ({hwnd}, visible={showing})"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use xrose_core::paths::AppPaths;

    #[test]
    fn tray_window_title_is_scoped_to_profile_and_missing_instance_returns_false() {
        let first = AppPaths::from_root(Path::new(r"C:\fixture-a"));
        let second = AppPaths::from_root(Path::new(r"C:\fixture-b"));
        assert_ne!(window_title(&first), window_title(&second));
        assert!(!wake_existing(&first));
    }
}
