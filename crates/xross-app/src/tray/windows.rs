//! Win32 tray backend.
//!
//! A hidden top-level window owns the `Shell_NotifyIcon` callback and the
//! native popup menu. Menu commands are queued and executed by
//! [`super::TrayManager::tick`] on the GPUI main thread, never from the window
//! procedure itself.
//!
//! The menu mirrors the macOS status-item dashboard: app/core status, live
//! traffic, the first enabled nodes with latency and a checked marker, then
//! system proxy / core toggle / latency test / show window / quit.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, EnumWindows, GetClassNameW, GetCursorPos, GetSystemMetrics,
    GetWindowThreadProcessId, IsIconic, IsWindow, LoadCursorW, PeekMessageW, PostMessageW,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, ShowWindow, TrackPopupMenu, HICON,
    ICONINFO, IDC_ARROW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PM_NOREMOVE,
    SM_CXSMICON, SW_HIDE, SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON,
    WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSW, WS_OVERLAPPED,
};

use super::dashboard::{self, Dashboard, TrayAction, TrayCommand};
use super::TrayCommand as Command;
use xross_core::util::format_speed;
use xross_core::AppService;

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;
const CLASS_NAME: &str = "XrossTrayWindow";
const ID_PROXY: u32 = 10;
const ID_TOGGLE_CORE: u32 = 11;
const ID_TEST_ALL: u32 = 12;
const ID_SHOW: u32 = 13;
const ID_QUIT: u32 = 14;

static READY: AtomicBool = AtomicBool::new(false);
static PENDING: OnceLock<Mutex<Vec<TrayAction>>> = OnceLock::new();
static MENU_ACTIONS: OnceLock<Mutex<Vec<(u32, TrayAction)>>> = OnceLock::new();
static DASHBOARD: OnceLock<Mutex<Option<Dashboard>>> = OnceLock::new();
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
    last_running: Option<bool>,
    added: bool,
    add_attempts: u32,
    reported_failure: bool,
    /// ticks (~200 ms) to wait before retrying `Shell_NotifyIconW`.
    retry_ticks: u32,
    retry_delay: u32,
}

impl Backend {
    pub fn new(_service: &AppService) -> Option<Self> {
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
            let title = wide("XrossTray");
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
            let _ = TASKBAR_CREATED.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
            let icon = make_icon(false);
            if icon == 0 {
                debug_log("make_icon failed");
                DestroyWindow(hwnd);
                return None;
            }
            let tip = "◈ Xross".to_string();
            ICON.get_or_init(|| {
                Mutex::new(IconState {
                    hwnd: hwnd as usize,
                    icon,
                    tip: tip.clone(),
                })
            });
            let mut backend = Self {
                last_tip: tip,
                last_running: Some(false),
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

    pub fn tick(&mut self, service: &AppService) -> Option<Command> {
        if !self.added {
            if self.retry_ticks > 0 {
                self.retry_ticks -= 1;
            } else if !self.try_add() {
                self.retry_ticks = self.retry_delay;
                self.retry_delay = (self.retry_delay * 2).min(300);
            }
            if !self.added && self.add_attempts == 3 && !self.reported_failure {
                self.reported_failure = true;
                service.toast(
                    xross_core::service::ToastLevel::Warning,
                    "系统拒绝注册托盘图标：关闭窗口将直接退出（可稍后重试或检查任务栏是否正常）",
                );
            }
        }
        let dashboard = dashboard::build(service);
        *DASHBOARD
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(dashboard.clone());
        if self.added {
            self.refresh_tooltip(&dashboard);
            self.refresh_icon(&dashboard);
        }
        let pending: Vec<TrayAction> = PENDING
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        let mut command = None;
        for action in pending {
            if let Some(next) = dashboard::dispatch(action, service) {
                command = Some(match next {
                    TrayCommand::ShowWindow => Command::ShowWindow,
                    TrayCommand::Quit => Command::Quit,
                });
            }
        }
        command
    }

    fn refresh_tooltip(&mut self, dashboard: &Dashboard) {
        let tip = if dashboard.running {
            format!(
                "◈ Xross · ↓{} ↑{}",
                format_speed(dashboard.down_speed),
                format_speed(dashboard.up_speed)
            )
        } else {
            "◇ Xross · 已停止".to_string()
        };
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

    fn refresh_icon(&mut self, dashboard: &Dashboard) {
        if self.last_running == Some(dashboard.running) {
            return;
        }
        let icon = make_icon(dashboard.running);
        if icon == 0 {
            return;
        }
        if let Some((hwnd, old_icon, tip)) = icon_snapshot() {
            let data = notify_data(hwnd as HWND, icon as HICON, &tip);
            if let Some(mut state) = icon_state() {
                state.icon = icon;
            }
            // SAFETY: data is a fully initialized NOTIFYICONDATAW; the old
            // icon is no longer referenced by the shell after NIM_MODIFY.
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &data);
                if old_icon != 0 {
                    DestroyIcon(old_icon as HICON);
                }
            }
        }
        self.last_running = Some(dashboard.running);
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

pub fn main_window_minimized() -> bool {
    main_hwnd()
        .map(|hwnd| {
            // SAFETY: hwnd belongs to this process.
            unsafe { IsIconic(hwnd) != 0 }
        })
        .unwrap_or(false)
}

pub fn setup_window() {}

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
    if std::env::var_os("XROSS_DEBUG").is_some() {
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

fn queue(action: TrayAction) {
    PENDING
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(action);
}

fn menu_action(id: u32) -> Option<TrayAction> {
    MENU_ACTIONS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(action_id, _)| *action_id == id)
        .map(|(_, action)| action.clone())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_TRAY {
        let event = (lparam as usize & 0xFFFF) as u32;
        match event {
            WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU => popup_menu(hwnd),
            WM_LBUTTONDBLCLK => queue(TrayAction::ShowWindow),
            _ => {}
        }
        return 0;
    }
    if message == WM_COMMAND {
        let id = (wparam & 0xFFFF) as u32;
        if let Some(action) = menu_action(id) {
            queue(action);
        }
        return 0;
    }
    if TASKBAR_CREATED
        .get()
        .map(|registered| *registered == message)
        .unwrap_or(false)
    {
        readd_icon();
        return 0;
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

unsafe fn readd_icon() {
    if let Some((hwnd, icon, tip)) = icon_snapshot() {
        let data = notify_data(hwnd as HWND, icon as HICON, &tip);
        Shell_NotifyIconW(NIM_ADD, &data);
    }
}

unsafe fn popup_menu(hwnd: HWND) {
    let dashboard = DASHBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(dashboard) = dashboard else { return };
    let (menu, actions) = build_menu(&dashboard);
    *MENU_ACTIONS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = actions;
    let mut point = POINT { x: 0, y: 0 };
    GetCursorPos(&mut point);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_BOTTOMALIGN,
        point.x,
        point.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);
    PostMessageW(hwnd, WM_NULL, 0, 0);
}

unsafe fn append_item(menu: *mut c_void, label: &str, flags: u32, id: u32) {
    if flags & MF_SEPARATOR != 0 {
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        return;
    }
    let text = wide(label);
    AppendMenuW(menu, flags, id as usize, text.as_ptr());
}

unsafe fn build_menu(dashboard: &Dashboard) -> (*mut c_void, Vec<(u32, TrayAction)>) {
    let menu = CreatePopupMenu();
    let mut actions = Vec::new();
    append_item(menu, &dashboard.status_line, MF_STRING | MF_GRAYED, 0);
    if let Some(line) = &dashboard.traffic_line {
        append_item(menu, line, MF_STRING | MF_GRAYED, 0);
    }
    append_item(menu, "", MF_SEPARATOR, 0);
    if !dashboard.nodes.is_empty() {
        let header = format!(
            "{} [{}]:",
            crate::i18n::tr(dashboard.language, "出站节点", "Outbound nodes"),
            dashboard.group_name
        );
        append_item(menu, &header, MF_STRING | MF_GRAYED, 0);
        for (index, node) in dashboard.nodes.iter().enumerate() {
            let id = 1000 + index as u32;
            let flags = MF_STRING | if node.active { MF_CHECKED } else { 0 };
            append_item(menu, &node.label, flags, id);
            actions.push((
                id,
                TrayAction::SelectNode {
                    group_id: dashboard
                        .group_id
                        .clone()
                        .unwrap_or_else(|| "proxy".to_string()),
                    node_id: node.id.clone(),
                },
            ));
        }
        append_item(menu, "", MF_SEPARATOR, 0);
    }
    let proxy_label = format!(
        "{} {}",
        if dashboard.system_proxy { "✓" } else { "○" },
        crate::i18n::tr(dashboard.language, "系统代理", "System proxy")
    );
    append_item(
        menu,
        &proxy_label,
        MF_STRING
            | if dashboard.system_proxy {
                MF_CHECKED
            } else {
                0
            },
        ID_PROXY,
    );
    actions.push((ID_PROXY, TrayAction::ToggleSystemProxy));
    let core_label = crate::i18n::tr(
        dashboard.language,
        if dashboard.running {
            "⏹ 停止 Core"
        } else {
            "▶ 启动 Core"
        },
        if dashboard.running {
            "⏹ Stop core"
        } else {
            "▶ Start core"
        },
    )
    .to_string();
    append_item(menu, &core_label, MF_STRING, ID_TOGGLE_CORE);
    actions.push((ID_TOGGLE_CORE, TrayAction::ToggleCore));
    let test_label = crate::i18n::tr(dashboard.language, "⚡ 全部节点测速", "⚡ Test all nodes");
    append_item(menu, &test_label, MF_STRING, ID_TEST_ALL);
    actions.push((ID_TEST_ALL, TrayAction::TestAll));
    append_item(menu, "", MF_SEPARATOR, 0);
    let show_label = crate::i18n::tr(dashboard.language, "🪟 显示主窗口", "🪟 Show window");
    append_item(menu, &show_label, MF_STRING, ID_SHOW);
    actions.push((ID_SHOW, TrayAction::ShowWindow));
    let quit_label = crate::i18n::tr(dashboard.language, "✕ 退出 Xross", "✕ Quit Xross");
    append_item(menu, &quit_label, MF_STRING, ID_QUIT);
    actions.push((ID_QUIT, TrayAction::Quit));
    (menu, actions)
}

/// Builds a small HICON from the bundled PNG, with a status dot in the
/// bottom-right corner (green = core running, gray = stopped).
fn make_icon(running: bool) -> isize {
    // SAFETY: GetSystemMetrics has no preconditions.
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    let Ok(image) = image::load_from_memory(include_bytes!("../../assets/app-icon-256.png")) else {
        return 0;
    };
    let resized = image
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let mut pixels = resized.into_raw();
    draw_status_dot(&mut pixels, size as usize, running);
    let mut bgra = Vec::with_capacity(pixels.len());
    for pixel in pixels.chunks_exact(4) {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
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

fn draw_status_dot(pixels: &mut [u8], size: usize, running: bool) {
    let radius = (size / 5).max(3) as i32;
    let center = (size as i32 - radius - 1, size as i32 - radius - 1);
    let color = if running {
        [52u8, 199, 89, 255]
    } else {
        [150, 150, 150, 255]
    };
    for y in 0..size as i32 {
        for x in 0..size as i32 {
            let dx = x - center.0;
            let dy = y - center.1;
            if dx * dx + dy * dy <= radius * radius {
                let index = (y as usize * size + x as usize) * 4;
                pixels[index..index + 4].copy_from_slice(&color);
            }
        }
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
    let found = find_main_hwnd()?;
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = found as usize;
    Some(found)
}

fn find_main_hwnd() -> Option<HWND> {
    struct Search {
        pid: u32,
        found: HWND,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        let search = &mut *(lparam as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != search.pid {
            return 1;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        if len <= 0 {
            return 1;
        }
        if String::from_utf16_lossy(&class[..len as usize]) == "Zed::Window" {
            search.found = hwnd;
            return 0;
        }
        1
    }

    let mut search = Search {
        pid: std::process::id(),
        found: std::ptr::null_mut(),
    };
    // SAFETY: the callback only writes into `search`, which outlives the call.
    unsafe {
        EnumWindows(Some(enum_proc), &mut search as *mut Search as LPARAM);
    }
    if search.found.is_null() {
        None
    } else {
        Some(search.found)
    }
}
