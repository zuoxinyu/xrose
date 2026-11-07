//! AppKit status item; the dashboard itself is the shared GPUI flyout.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Once;

use cocoa::appkit::{NSApplication, NSStatusBar, NSVariableStatusItemLength};
use cocoa::base::{id, nil, BOOL, YES};
use cocoa::foundation::{NSAutoreleasePool, NSPoint, NSRect, NSSize, NSString};
use gpui_kit::{point, px, Window};
use objc::declare::ClassDecl;
use objc::runtime::{Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use xrose_core::AppService;

static POPUP_TARGET: AtomicUsize = AtomicUsize::new(0);
static POPUP_OPS: parking_lot::Mutex<Vec<PopupOp>> = parking_lot::Mutex::new(Vec::new());

enum PopupOp {
    Hide(usize),
    Show(usize, gpui_kit::Bounds<gpui_kit::Pixels>),
}

unsafe fn string(value: &str) -> id {
    unsafe {
        let value = NSString::alloc(nil).init_str(value);
        msg_send![value, autorelease]
    }
}

fn post_popup(operation: PopupOp) {
    POPUP_OPS.lock().push(operation);
    let target = POPUP_TARGET.load(Ordering::Relaxed) as id;
    if target != nil {
        unsafe {
            let _: () = msg_send![target, performSelector: sel!(flushPopup:) withObject: nil afterDelay: 0.0_f64];
        }
    }
}

extern "C" fn flush_popup(_this: &Object, _sel: Sel, _sender: id) {
    let operations = std::mem::take(&mut *POPUP_OPS.lock());
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        for operation in operations {
            match operation {
                PopupOp::Hide(native) => {
                    let _: () = msg_send![native as id, orderOut: nil];
                }
                PopupOp::Show(native, bounds) => {
                    let native = native as id;
                    let screens: id = msg_send![class!(NSScreen), screens];
                    let primary: id = msg_send![screens, objectAtIndex: 0usize];
                    let screen: NSRect = msg_send![primary, frame];
                    let content_size = NSSize::new(
                        f32::from(bounds.size.width) as f64,
                        f32::from(bounds.size.height) as f64,
                    );
                    let _: () = msg_send![native, setContentSize: content_size];
                    let origin = NSPoint::new(
                        f32::from(bounds.origin.x) as f64,
                        screen.size.height - f32::from(bounds.bottom()) as f64,
                    );
                    let _: () = msg_send![native, setFrameOrigin: origin];
                    let app = NSApplication::sharedApplication(nil);
                    let _: () = msg_send![app, activateIgnoringOtherApps: YES];
                    let _: () = msg_send![native, makeKeyAndOrderFront: nil];
                }
            }
        }
        let _: () = msg_send![pool, drain];
    }
}

static MAIN_WINDOW: AtomicUsize = AtomicUsize::new(0);
static STATUS_ITEM: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicBool = AtomicBool::new(false);

extern "C" fn toggle_popup(_this: &Object, _sel: Sel, _sender: id) {
    super::queue_command(super::TrayCommand::TogglePopup);
}

fn format_fixed_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        return "   0 B/s".to_string();
    }
    if bytes_per_sec < 1000 {
        return format!("{:>4} B/s", bytes_per_sec);
    }
    let kb = bytes_per_sec as f64 / 1024.0;
    if kb < 9.95 {
        return format!("{:>4.1} K/s", kb);
    }
    if kb < 999.5 {
        return format!("{:>4.0} K/s", kb);
    }
    let mb = kb / 1024.0;
    if mb < 9.95 {
        return format!("{:>4.1} M/s", mb);
    }
    if mb < 999.5 {
        return format!("{:>4.0} M/s", mb);
    }
    let gb = mb / 1024.0;
    if gb < 9.95 {
        return format!("{:>4.1} G/s", gb);
    }
    format!("{:>4.0} G/s", gb.min(999.0))
}

unsafe fn build_template_logo_image() -> id {
    let png_bytes = crate::brand_assets::TRAY_TEMPLATE;
    unsafe {
        let data: id = msg_send![
            class!(NSData),
            dataWithBytes: png_bytes.as_ptr() as *const c_void
            length: png_bytes.len()
        ];
        if data == nil {
            return nil;
        }
        let image: id = msg_send![class!(NSImage), alloc];
        let image: id = msg_send![image, initWithData: data];
        if image != nil {
            let _: () = msg_send![image, setSize: NSSize::new(18.0, 18.0)];
            let _: () = msg_send![image, setTemplate: YES];
        }
        image
    }
}

unsafe fn build_speed_image(logo: id, up_speed: u64, down_speed: u64) -> id {
    unsafe {
        let size = NSSize::new(80.0, 22.0);
        let image: id = msg_send![class!(NSImage), alloc];
        let image: id = msg_send![image, initWithSize: size];
        if image == nil {
            return nil;
        }
        let _: () = msg_send![image, lockFocus];

        if logo != nil {
            let logo_rect = NSRect::new(NSPoint::new(2.0, 2.0), NSSize::new(18.0, 18.0));
            let _: () = msg_send![logo, drawInRect: logo_rect];
        }

        let font: id = msg_send![
            class!(NSFont),
            monospacedSystemFontOfSize: 9.0f64
            weight: 0.0f64
        ];
        let color: id = msg_send![class!(NSColor), blackColor];
        if font != nil && color != nil {
            let font_key = string("NSFont");
            let color_key = string("NSColor");
            let keys = [font_key, color_key];
            let objects = [font, color];
            let attrs: id = msg_send![
                class!(NSDictionary),
                dictionaryWithObjects: objects.as_ptr()
                forKeys: keys.as_ptr()
                count: 2usize
            ];
            if attrs != nil {
                let up_text = format!("↑{}", format_fixed_speed(up_speed));
                let down_text = format!("↓{}", format_fixed_speed(down_speed));
                let up_ns = string(&up_text);
                let down_ns = string(&down_text);
                let up_pt = NSPoint::new(24.0, 11.0);
                let down_pt = NSPoint::new(24.0, 1.0);
                let _: () = msg_send![up_ns, drawAtPoint: up_pt withAttributes: attrs];
                let _: () = msg_send![down_ns, drawAtPoint: down_pt withAttributes: attrs];
            }
        }

        let _: () = msg_send![image, unlockFocus];
        let _: () = msg_send![image, setTemplate: YES];
        image
    }
}

pub struct Backend {
    item: id,
    target: id,
    last_running: Option<bool>,
    last_show_speed: Option<bool>,
    last_speeds: Option<(u64, u64)>,
    current_speed_image: id,
    logo_image: id,
}

impl Backend {
    pub fn new(_service: &AppService) -> Option<Self> {
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let mut decl = ClassDecl::new("XroseFlyoutTarget", class!(NSObject))
                    .expect("status target class");
                decl.add_method(
                    sel!(togglePopup:),
                    toggle_popup as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(flushPopup:),
                    flush_popup as extern "C" fn(&Object, Sel, id),
                );
                decl.register();
            });
            let pool = NSAutoreleasePool::new(nil);
            let bar = NSStatusBar::systemStatusBar(nil);
            let item = bar.statusItemWithLength_(NSVariableStatusItemLength);
            if item == nil {
                let _: () = msg_send![pool, drain];
                return None;
            }
            let _: () = msg_send![item, retain];
            let target: id = msg_send![class!(XroseFlyoutTarget), new];
            POPUP_TARGET.store(target as usize, Ordering::Relaxed);
            let button: id = msg_send![item, button];
            let _: () = msg_send![button, setTarget: target];
            let _: () = msg_send![button, setAction: sel!(togglePopup:)];
            let _: isize = msg_send![button, sendActionOn: (1u64 << 2) | (1u64 << 4)];

            let logo_image = build_template_logo_image();
            if logo_image != nil {
                let _: () = msg_send![button, setImage: logo_image];
                let _: () = msg_send![button, setImagePosition: 1usize]; // NSImageOnly = 1
            }
            let _: () = msg_send![button, setTitle: string("")];
            STATUS_ITEM.store(item as usize, Ordering::Relaxed);
            READY.store(true, Ordering::Relaxed);
            let _: () = msg_send![pool, drain];
            Some(Self {
                item,
                target,
                last_running: None,
                last_show_speed: None,
                last_speeds: None,
                current_speed_image: nil,
                logo_image,
            })
        }
    }

    pub fn tick(
        &mut self,
        service: &AppService,
        _cx: &gpui_kit::App,
    ) -> Option<super::TrayCommand> {
        let dashboard = super::dashboard::build(service);
        let show_speed = dashboard.running && dashboard.show_speed;
        let speeds = (dashboard.up_speed, dashboard.down_speed);

        let state_changed = self.last_running != Some(dashboard.running)
            || self.last_show_speed != Some(show_speed)
            || (show_speed && self.last_speeds != Some(speeds));

        if state_changed {
            unsafe {
                let pool = NSAutoreleasePool::new(nil);
                let button: id = msg_send![self.item, button];

                if show_speed {
                    let new_image = build_speed_image(
                        self.logo_image,
                        dashboard.up_speed,
                        dashboard.down_speed,
                    );
                    if new_image != nil {
                        let _: () = msg_send![button, setImage: new_image];
                        let _: () = msg_send![button, setImagePosition: 1usize]; // NSImageOnly
                        let _: () = msg_send![button, setTitle: string("")];
                        let _: () = msg_send![self.item, setLength: 82.0f64];

                        if self.current_speed_image != nil {
                            let _: () = msg_send![self.current_speed_image, release];
                        }
                        self.current_speed_image = new_image;
                    }
                } else {
                    if self.current_speed_image != nil {
                        let _: () = msg_send![self.current_speed_image, release];
                        self.current_speed_image = nil;
                    }
                    if self.logo_image != nil {
                        let _: () = msg_send![button, setImage: self.logo_image];
                        let _: () = msg_send![button, setImagePosition: 1usize];
                        // NSImageOnly
                    }
                    let _: () = msg_send![button, setTitle: string("")];
                    let _: () = msg_send![self.item, setLength: NSVariableStatusItemLength];
                }

                let tooltip = if let Some(traffic) = &dashboard.traffic_line {
                    format!("{}\n{}", dashboard.status_line, traffic)
                } else {
                    dashboard.status_line.clone()
                };
                let _: () = msg_send![button, setToolTip: string(&tooltip)];
                let _: () = msg_send![pool, drain];
            }

            self.last_running = Some(dashboard.running);
            self.last_show_speed = Some(show_speed);
            self.last_speeds = Some(speeds);
        }

        None
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        READY.store(false, Ordering::Relaxed);
        STATUS_ITEM.store(0, Ordering::Relaxed);
        POPUP_TARGET.store(0, Ordering::Relaxed);
        unsafe {
            let bar = NSStatusBar::systemStatusBar(nil);
            let _: () = msg_send![bar, removeStatusItem: self.item];
            let _: () = msg_send![self.item, release];
            let _: () = msg_send![self.target, release];
            if self.logo_image != nil {
                let _: () = msg_send![self.logo_image, release];
            }
            if self.current_speed_image != nil {
                let _: () = msg_send![self.current_speed_image, release];
            }
        }
    }
}

pub fn available() -> bool {
    READY.load(Ordering::Relaxed)
}

pub fn register_main_window(window: &Window) {
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::AppKit(raw) = handle.as_raw() {
            unsafe {
                let view = raw.ns_view.as_ptr() as id;
                let window: id = msg_send![view, window];
                MAIN_WINDOW.store(window as usize, Ordering::Relaxed);
                crate::app_icon::setup_window_icon(window);
            }
        }
    }
}

pub fn hide_main_window() {
    let window = MAIN_WINDOW.load(Ordering::Relaxed) as id;
    if window != nil {
        unsafe {
            let _: () = msg_send![window, orderOut: nil];
        }
    }
}

pub fn show_main_window() {
    let window = MAIN_WINDOW.load(Ordering::Relaxed) as id;
    if window != nil {
        unsafe {
            let app = NSApplication::sharedApplication(nil);
            let _: () = msg_send![app, activateIgnoringOtherApps: YES];
            let _: () = msg_send![window, deminiaturize: nil];
            let _: () = msg_send![window, makeKeyAndOrderFront: nil];
        }
    }
}

pub fn main_window_minimized() -> bool {
    let window = MAIN_WINDOW.load(Ordering::Relaxed) as id;
    window != nil
        && unsafe {
            let mini: BOOL = msg_send![window, isMiniaturized];
            mini == YES
        }
}

unsafe fn icon_frame() -> Option<NSRect> {
    let item = STATUS_ITEM.load(Ordering::Relaxed) as id;
    if item == nil {
        return None;
    }
    let button: id = msg_send![item, button];
    let window: id = msg_send![button, window];
    if window == nil {
        return None;
    }
    Some(msg_send![window, frame])
}

pub fn popup_anchor() -> Option<super::PopupAnchor> {
    unsafe {
        let frame = icon_frame()?;
        let screens: id = msg_send![class!(NSScreen), screens];
        let primary: id = msg_send![screens, objectAtIndex: 0usize];
        let screen: NSRect = msg_send![primary, frame];
        Some(super::PopupAnchor {
            position: point(
                px((frame.origin.x + frame.size.width / 2.) as f32),
                px((screen.size.height - frame.origin.y - frame.size.height / 2.) as f32),
            ),
            work_area: None,
        })
    }
}

pub fn pointer_on_icon() -> bool {
    unsafe {
        let Some(rect) = icon_frame() else {
            return false;
        };
        let p: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        p.x >= rect.origin.x
            && p.x < rect.origin.x + rect.size.width
            && p.y >= rect.origin.y
            && p.y < rect.origin.y + rect.size.height
    }
}

pub fn setup_window() {
    setup_macos_window();
}

unsafe extern "C" {
    fn class_replaceMethod(
        cls: *const objc::runtime::Class,
        name: Sel,
        imp: objc::runtime::Imp,
        types: *const c_char,
    ) -> objc::runtime::Imp;
}

extern "C" fn custom_blurred_view_update_layer(this: &Object, _sel: Sel) {
    unsafe {
        let superclass = class!(NSVisualEffectView);
        let _: () = msg_send![super(this, superclass), updateLayer];
        let layer: id = msg_send![this, layer];
        if layer != nil {
            let sublayers: id = msg_send![layer, sublayers];
            if sublayers != nil {
                let count: usize = msg_send![sublayers, count];
                for i in 0..count {
                    let sl: id = msg_send![sublayers, objectAtIndex: i];
                    let _: () = msg_send![sl, setHidden: cocoa::base::NO];
                }
            }
        }
    }
}

pub fn setup_macos_window() {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        static SWIZZLE_ONCE: Once = Once::new();
        SWIZZLE_ONCE.call_once(|| {
            if let Some(cls) = objc::runtime::Class::get("BlurredView") {
                let types = b"v@:\0";
                class_replaceMethod(
                    cls as *const _,
                    sel!(updateLayer),
                    std::mem::transmute::<extern "C" fn(&Object, Sel), objc::runtime::Imp>(
                        custom_blurred_view_update_layer,
                    ),
                    types.as_ptr() as *const _,
                );
            }
        });

        let app = NSApplication::sharedApplication(nil);
        let windows: id = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let win: id = msg_send![windows, objectAtIndex: i];
            let win_class: id = msg_send![win, className];
            let is_gpui: BOOL = msg_send![win_class, isEqualToString: string("GPUIWindow")];
            if is_gpui == YES && win as usize == MAIN_WINDOW.load(Ordering::Relaxed) {
                let mask: usize = msg_send![win, styleMask];
                let full_size_mask: usize = 1 << 15; // NSWindowStyleMaskFullSizeContentView
                let _: () = msg_send![win, setStyleMask: mask | full_size_mask];
                let _: () = msg_send![win, setTitlebarAppearsTransparent: YES];
                let _: () = msg_send![win, setOpaque: cocoa::base::NO];
                let _: () = msg_send![win, setHasShadow: YES];
                let clear_color: id = msg_send![class!(NSColor), clearColor];
                let _: () = msg_send![win, setBackgroundColor: clear_color];
                let _: () = msg_send![win, setMovableByWindowBackground: cocoa::base::NO];

                let content_view: id = msg_send![win, contentView];
                configure_views_tree(content_view);
            }
        }
        let _: () = msg_send![pool, drain];
    }
}

unsafe fn configure_views_tree(view: id) {
    if view == nil {
        return;
    }
    let is_effect: BOOL = msg_send![view, isKindOfClass: class!(NSVisualEffectView)];
    if is_effect == YES {
        // Material 7: NSVisualEffectMaterialSidebar.
        let _: () = msg_send![view, setMaterial: 7isize];
        let _: () = msg_send![view, setBlendingMode: 0isize]; // BehindWindow
        let _: () = msg_send![view, setState: 1isize]; // Active
        let _: () = msg_send![view, setWantsLayer: YES];
    }

    let layer: id = msg_send![view, layer];
    if layer != nil {
        let _: () = msg_send![layer, setOpaque: cocoa::base::NO];
        let sublayers: id = msg_send![layer, sublayers];
        if sublayers != nil {
            let sl_count: usize = msg_send![sublayers, count];
            for s in 0..sl_count {
                let sl: id = msg_send![sublayers, objectAtIndex: s];
                let _: () = msg_send![sl, setHidden: cocoa::base::NO];
            }
        }
    }

    let subviews: id = msg_send![view, subviews];
    if subviews != nil {
        let count: usize = msg_send![subviews, count];
        for i in 0..count {
            let sub: id = msg_send![subviews, objectAtIndex: i];
            unsafe { configure_views_tree(sub) };
        }
    }
}

fn native_window(window: &Window) -> Option<id> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    if let RawWindowHandle::AppKit(raw) = handle.as_raw() {
        unsafe {
            let view = raw.ns_view.as_ptr() as id;
            Some(msg_send![view, window])
        }
    } else {
        None
    }
}

pub fn hide_popup(window: &mut Window) {
    if let Some(native) = native_window(window) {
        post_popup(PopupOp::Hide(native as usize));
    }
}

pub fn show_popup(window: &mut Window, bounds: gpui_kit::Bounds<gpui_kit::Pixels>) {
    if let Some(native) = native_window(window) {
        post_popup(PopupOp::Show(native as usize, bounds));
    }
}

#[cfg(test)]
mod tests {
    use super::format_fixed_speed;

    #[test]
    fn fixed_speed_always_returns_eight_ascii_chars() {
        for sample in [
            0,
            1,
            9,
            188,
            999,
            1000,
            1024,
            3993,
            10_180,
            10_240,
            512_000,
            1_023_488,
            1_048_576,
            10_485_760,
            999_000_000,
            1_073_741_824,
            100_000_000_000,
            u64::MAX,
        ] {
            let formatted = format_fixed_speed(sample);
            assert_eq!(
                formatted.len(),
                8,
                "expected 8 chars for {sample}, got {formatted:?}"
            );
            assert!(formatted.is_ascii());
            assert!(formatted.starts_with(' '));
        }
    }
}
