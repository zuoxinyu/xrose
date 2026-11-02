//! macOS tray backend: AppKit status item and NSMenu dashboard.
//!
//! The status-item menu is unchanged from the original implementation; the
//! wrapper at the bottom maps it onto the cross-platform `tray` interface.

mod imp {
    use std::ffi::{c_char, CStr};
    use std::sync::Once;

    use cocoa::appkit::{NSApplication, NSStatusBar, NSVariableStatusItemLength};
    use cocoa::base::{id, nil, BOOL, YES};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::declare::ClassDecl;
    use objc::runtime::{Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use parking_lot::RwLock;
    use xross_core::service::LatencyScope;
    use xross_core::util::{format_bytes, format_speed};
    use xross_core::AppService;

    static TRAY_SERVICE: RwLock<Option<AppService>> = RwLock::new(None);
    static REGISTER_CLASS_ONCE: Once = Once::new();
    static mut TARGET_INSTANCE: id = nil;

    extern "C" fn show_window(_this: &Object, _sel: Sel, _sender: id) {
        show_main_window();
    }

    extern "C" fn toggle_core(_this: &Object, _sel: Sel, _sender: id) {
        if let Some(service) = TRAY_SERVICE.read().as_ref() {
            service.toggle_core();
        }
    }

    extern "C" fn test_all(_this: &Object, _sel: Sel, _sender: id) {
        if let Some(service) = TRAY_SERVICE.read().as_ref() {
            service.test_latency(LatencyScope::All);
        }
    }

    extern "C" fn select_node(_this: &Object, _sel: Sel, sender: id) {
        unsafe {
            let rep: id = msg_send![sender, representedObject];
            if rep != nil {
                let utf8: *const c_char = msg_send![rep, UTF8String];
                if !utf8.is_null() {
                    let node_id = CStr::from_ptr(utf8).to_string_lossy().to_string();
                    if let Some(service) = TRAY_SERVICE.read().as_ref() {
                        let snap = service.snapshot();
                        let group_id = snap
                            .model
                            .default_group_id()
                            .or_else(|| snap.model.groups.first().map(|g| g.id.clone()))
                            .unwrap_or_else(|| "proxy".to_string());
                        service.select_node(&group_id, &node_id);
                    }
                }
            }
        }
    }

    extern "C" fn quit_app(_this: &Object, _sel: Sel, _sender: id) {
        unsafe {
            crate::tray::request_quit();
            let app = NSApplication::sharedApplication(nil);
            let _: () = msg_send![app, terminate: nil];
        }
    }

    fn init_target_class() {
        REGISTER_CLASS_ONCE.call_once(|| {
            let superclass = class!(NSObject);
            let mut decl = ClassDecl::new("XrossTrayTarget", superclass)
                .expect("Failed to declare XrossTrayTarget class");

            unsafe {
                decl.add_method(
                    sel!(showWindow:),
                    show_window as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(toggleCore:),
                    toggle_core as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(sel!(testAll:), test_all as extern "C" fn(&Object, Sel, id));
                decl.add_method(
                    sel!(selectNode:),
                    select_node as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(sel!(quitApp:), quit_app as extern "C" fn(&Object, Sel, id));
            }
            let cls = decl.register();
            unsafe {
                let inst: id = msg_send![cls, new];
                TARGET_INSTANCE = inst;
            }
        });
    }

    pub struct TrayManager {
        status_item: id,
        last_down: u64,
        last_up: u64,
        last_phase: String,
    }

    // Safety: AppKit status item calls are executed on main thread in GPUI tick
    unsafe impl Send for TrayManager {}
    unsafe impl Sync for TrayManager {}

    impl TrayManager {
        pub fn new(service: AppService) -> Self {
            init_target_class();
            *TRAY_SERVICE.write() = Some(service.clone());

            let status_item = unsafe {
                let pool = NSAutoreleasePool::new(nil);
                let bar = NSStatusBar::systemStatusBar(nil);
                let item = bar.statusItemWithLength_(NSVariableStatusItemLength);
                let _: () = msg_send![item, retain];
                let button: id = msg_send![item, button];
                if button != nil {
                    let title = NSString::alloc(nil).init_str("◈ Xross");
                    let _: () = msg_send![button, setTitle: title];
                    let tooltip = NSString::alloc(nil).init_str("Xross 代理客户端");
                    let _: () = msg_send![button, setToolTip: tooltip];
                }
                let _: () = msg_send![pool, drain];
                item
            };

            let mgr = Self {
                status_item,
                last_down: 0,
                last_up: 0,
                last_phase: String::new(),
            };
            mgr.refresh_menu(&service);
            mgr
        }

        pub fn tick(&mut self, service: &AppService) {
            let snap = service.snapshot();
            let phase_str = snap.process.phase.label().to_string();
            let down = snap.runtime.down_speed;
            let up = snap.runtime.up_speed;

            // Update title text if speeds or phase changed
            if down != self.last_down || up != self.last_up || phase_str != self.last_phase {
                self.last_down = down;
                self.last_up = up;
                self.last_phase = phase_str;
                self.update_title(&snap);
                self.refresh_menu(service);
            }
        }

        fn update_title(&self, snap: &xross_core::service::UiSnapshot) {
            unsafe {
                let pool = NSAutoreleasePool::new(nil);
                let button: id = msg_send![self.status_item, button];
                if button != nil {
                    let title_text = if snap.process.phase.is_alive() {
                        if snap.runtime.down_speed > 0 || snap.runtime.up_speed > 0 {
                            format!(
                                "◈ ↓{} ↑{}",
                                format_speed(snap.runtime.down_speed),
                                format_speed(snap.runtime.up_speed)
                            )
                        } else {
                            "◈ Xross".to_string()
                        }
                    } else {
                        "◇ Xross".to_string()
                    };
                    let title = NSString::alloc(nil).init_str(&title_text);
                    let _: () = msg_send![button, setTitle: title];
                }
                let _: () = msg_send![pool, drain];
            }
        }

        pub fn refresh_menu(&self, service: &AppService) {
            let snap = service.snapshot();
            let running = snap.process.phase.is_alive();
            let model = snap.model.clone();
            let default_group = model
                .default_group_id()
                .or_else(|| model.groups.first().map(|g| g.id.clone()))
                .and_then(|id| model.group(&id).cloned());
            let active_node_id = default_group
                .as_ref()
                .and_then(|g| g.active_node().cloned());

            unsafe {
                let pool = NSAutoreleasePool::new(nil);
                let menu: id = msg_send![class!(NSMenu), new];
                let _target = TARGET_INSTANCE;

                // 1. App status header
                let status_text = format!(
                    "Xross v{} · {} ({})",
                    snap.app_version,
                    if running { "运行中" } else { "已停止" },
                    snap.settings.core.display_name()
                );
                add_menu_item(menu, &status_text, None, false, false, nil);

                // Traffic speed info
                if running {
                    let traffic_text = format!(
                        "↓ {}  ↑ {}  累计: {}",
                        format_speed(snap.runtime.down_speed),
                        format_speed(snap.runtime.up_speed),
                        format_bytes(snap.runtime.down_total + snap.runtime.up_total)
                    );
                    add_menu_item(menu, &traffic_text, None, false, false, nil);
                }

                add_separator(menu);

                // 2. Outbound nodes list (up to 12 enabled nodes)
                if !model.nodes.is_empty() {
                    let section_title = format!(
                        "出站节点 [{}]:",
                        default_group
                            .as_ref()
                            .map(|g| g.name.as_str())
                            .unwrap_or("代理")
                    );
                    add_menu_item(menu, &section_title, None, false, false, nil);

                    for node in model.nodes.iter().filter(|n| n.enabled).take(12) {
                        let is_active = active_node_id.as_deref() == Some(node.id.as_str());
                        let latency_text = snap
                            .latency
                            .get(&node.id)
                            .and_then(|l| l.delay_ms)
                            .map(|ms| format!(" ({ms}ms)"))
                            .unwrap_or_default();
                        let item_title = format!(
                            "{} {}{}",
                            if is_active { "●" } else { "○" },
                            node.name,
                            latency_text
                        );

                        let node_id_ns = NSString::alloc(nil).init_str(&node.id);
                        add_menu_item(
                            menu,
                            &item_title,
                            Some(sel!(selectNode:)),
                            true,
                            is_active,
                            node_id_ns,
                        );
                    }
                    add_separator(menu);
                }

                // 3. Actions: Toggle Core, Test latency
                let toggle_label = if running {
                    "⏹ 停止 Core"
                } else {
                    "▶ 启动 Core"
                };
                add_menu_item(
                    menu,
                    toggle_label,
                    Some(sel!(toggleCore:)),
                    true,
                    false,
                    nil,
                );

                add_menu_item(
                    menu,
                    "⚡ 全部节点测速",
                    Some(sel!(testAll:)),
                    true,
                    false,
                    nil,
                );

                add_separator(menu);

                // 4. Show window and quit
                add_menu_item(
                    menu,
                    "🪟 显示主窗口",
                    Some(sel!(showWindow:)),
                    true,
                    false,
                    nil,
                );
                add_menu_item(menu, "✕ 退出 Xross", Some(sel!(quitApp:)), true, false, nil);

                let _: () = msg_send![self.status_item, setMenu: menu];
                let _: () = msg_send![pool, drain];
            }
        }
    }

    unsafe fn add_menu_item(
        menu: id,
        title: &str,
        action: Option<Sel>,
        enabled: bool,
        checked: bool,
        represented_object: id,
    ) {
        let title_ns = NSString::alloc(nil).init_str(title);
        let empty_key = NSString::alloc(nil).init_str("");
        let item: id = msg_send![class!(NSMenuItem), alloc];
        let sel_action = action.unwrap_or_else(|| sel!(showWindow:));
        let item: id =
            msg_send![item, initWithTitle: title_ns action: sel_action keyEquivalent: empty_key];
        let _: () = msg_send![item, setTarget: TARGET_INSTANCE];
        let _: () = msg_send![item, setEnabled: if enabled { YES } else { cocoa::base::NO }];
        if checked {
            let _: () = msg_send![item, setState: 1isize]; // NSControlStateValueOn
        }
        if represented_object != nil {
            let _: () = msg_send![item, setRepresentedObject: represented_object];
        }
        let _: () = msg_send![menu, addItem: item];
    }

    unsafe fn add_separator(menu: id) {
        let sep: id = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep];
    }

    extern "C" {
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
                let is_gpui: BOOL = msg_send![win_class, isEqualToString: NSString::alloc(nil).init_str("GPUIWindow")];
                if is_gpui == YES {
                    let mask: usize = msg_send![win, styleMask];
                    let full_size_mask: usize = 1 << 15; // NSWindowStyleMaskFullSizeContentView
                    let _: () = msg_send![win, setStyleMask: mask | full_size_mask];
                    let _: () = msg_send![win, setTitlebarAppearsTransparent: YES];
                    let _: () = msg_send![win, setOpaque: cocoa::base::NO];
                    let _: () = msg_send![win, setHasShadow: YES];
                    let clear_color: id = msg_send![class!(NSColor), clearColor];
                    let _: () = msg_send![win, setBackgroundColor: clear_color];
                    let _: () = msg_send![win, setMovableByWindowBackground: YES];

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
            // Material 7: NSVisualEffectMaterialSidebar (macOS 原生侧边栏半透明磨砂)
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
                configure_views_tree(sub);
            }
        }
    }

    fn each_window(mut visit: impl FnMut(id)) {
        unsafe {
            let app = NSApplication::sharedApplication(nil);
            let windows: id = msg_send![app, windows];
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                visit(msg_send![windows, objectAtIndex: index]);
            }
        }
    }

    /// Orders every app window out, which is the AppKit equivalent of hiding
    /// into the tray (the window stays alive and can be shown again).
    pub fn hide_main_window() {
        each_window(|window| unsafe {
            let _: () = msg_send![window, orderOut: nil];
        });
    }

    pub fn show_main_window() {
        unsafe {
            let app = NSApplication::sharedApplication(nil);
            let _: () = msg_send![app, activateIgnoringOtherApps: YES];
        }
        each_window(|window| unsafe {
            let miniaturized: BOOL = msg_send![window, isMiniaturized];
            if miniaturized == YES {
                let _: () = msg_send![window, deminiaturize: nil];
            }
            let _: () = msg_send![window, makeKeyAndOrderFront: nil];
            let _: () = msg_send![window, setIsVisible: YES];
        });
    }

    pub fn main_window_minimized() -> bool {
        let mut minimized = false;
        each_window(|window| unsafe {
            let miniaturized: BOOL = msg_send![window, isMiniaturized];
            if miniaturized == YES {
                minimized = true;
            }
        });
        minimized
    }
}

pub struct Backend {
    tray: imp::TrayManager,
}

impl Backend {
    pub fn new(service: &xross_core::AppService) -> Option<Self> {
        Some(Self {
            tray: imp::TrayManager::new(service.clone()),
        })
    }

    pub fn tick(&mut self, service: &xross_core::AppService) -> Option<super::TrayCommand> {
        self.tray.tick(service);
        None
    }
}

pub fn available() -> bool {
    true
}

pub fn hide_main_window() {
    imp::hide_main_window();
}

pub fn show_main_window() {
    imp::show_main_window();
}

pub fn main_window_minimized() -> bool {
    imp::main_window_minimized()
}

pub fn setup_window() {
    imp::setup_macos_window();
}
