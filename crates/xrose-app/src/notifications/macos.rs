//! UserNotifications authorization, delivery and foreground presentation.

use std::ffi::CStr;
use std::time::Duration;

use block::ConcreteBlock;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use objc::{class, msg_send, sel, sel_impl};

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

const DENIED: &str = "系统通知未获授权，请在系统设置 → 通知 → Xrose 中允许通知";

fn with_center<T>(f: impl FnOnce(id) -> Result<T, String>) -> Result<T, String> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let bundle: id = msg_send![class!(NSBundle), mainBundle];
        let identifier: id = msg_send![bundle, bundleIdentifier];
        // Calling currentNotificationCenter without an app identity raises an ObjC exception.
        let result = if identifier == nil {
            Err("无法识别应用身份，请重新启动 Xrose 或从 Xrose.app 启动".into())
        } else {
            let center: id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
            install_delegate(center);
            f(center)
        };
        let _: () = msg_send![pool, drain];
        result
    }
}

fn authorization_status(center: id) -> Result<usize, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let completion = ConcreteBlock::new(move |settings: id| unsafe {
        let status: usize = msg_send![settings, authorizationStatus];
        let _ = sender.send(status);
    })
    .copy();
    unsafe {
        let _: () = msg_send![center, getNotificationSettingsWithCompletionHandler: &*completion];
    }
    receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "读取系统通知权限超时，请重新开启系统通知".into())
}

pub(super) fn request_permission() -> Result<(), String> {
    with_center(|center| {
        match authorization_status(center)? {
            1 => return Err(DENIED.into()),
            2 | 4 => return Ok(()),
            _ => {}
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let completion = ConcreteBlock::new(move |granted: objc::runtime::BOOL, error: id| {
            let result = if error != nil {
                Err(format!("请求系统通知权限失败：{}", describe_error(error)))
            } else if granted == objc::runtime::YES {
                Ok(())
            } else {
                Err(DENIED.into())
            };
            let _ = sender.send(result);
        })
        .copy();
        unsafe {
            // Alert and sound. This runs on the worker, leaving the GUI responsive to the prompt.
            let _: () = msg_send![center, requestAuthorizationWithOptions: 6usize completionHandler: &*completion];
        }
        receiver
            .recv_timeout(Duration::from_secs(120))
            .map_err(|_| "等待通知授权超时，请完成系统提示后重新开启系统通知".to_string())?
    })
}

pub(super) fn show(id: u64, message: &str) -> Result<(), String> {
    with_center(|center| unsafe {
        // Detect permission changes made in System Settings after this worker started.
        if !matches!(authorization_status(center)?, 2..=4) {
            return Err(DENIED.into());
        }
        let content: cocoa::base::id = msg_send![class!(UNMutableNotificationContent), new];
        let title = NSString::alloc(nil).init_str("Xrose");
        let body = NSString::alloc(nil).init_str(message);
        let request_id = NSString::alloc(nil).init_str(&format!("xrose-{id}"));
        let _: () = msg_send![content, setTitle: title];
        let _: () = msg_send![content, setBody: body];
        let request: cocoa::base::id = msg_send![class!(UNNotificationRequest), requestWithIdentifier: request_id content: content trigger: nil];
        let (sender, receiver) = std::sync::mpsc::channel();
        let completion = ConcreteBlock::new(move |error: cocoa::base::id| {
            let result = if error == nil {
                Ok(())
            } else {
                Err(format!("提交系统通知失败：{}", describe_error(error)))
            };
            let _ = sender.send(result);
        })
        .copy();
        let _: () =
            msg_send![center, addNotificationRequest: request withCompletionHandler: &*completion];
        let _: () = msg_send![content, release];
        let _: () = msg_send![title, release];
        let _: () = msg_send![body, release];
        let _: () = msg_send![request_id, release];
        receiver
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "提交系统通知超时，请重新开启系统通知".to_string())?
    })
}

fn describe_error(error: id) -> String {
    unsafe {
        let description: id = msg_send![error, localizedDescription];
        let domain: id = msg_send![error, domain];
        let code: isize = msg_send![error, code];
        format!("{}（{} / {code}）", string(description), string(domain))
    }
}

unsafe fn string(value: id) -> String {
    unsafe {
        if value == nil {
            return String::new();
        }
        let pointer: *const std::ffi::c_char = msg_send![value, UTF8String];
        if pointer.is_null() {
            String::new()
        } else {
            CStr::from_ptr(pointer).to_string_lossy().into_owned()
        }
    }
}

unsafe fn install_delegate(center: id) {
    use objc::runtime::{Object, Sel};
    extern "C" fn present(
        _this: &Object,
        _sel: Sel,
        _center: id,
        _notification: id,
        completion: id,
    ) {
        // Respect system permission and Focus while allowing foreground alerts.
        unsafe { (&*(completion as *const block::Block<(usize,), ()>)).call((4,)) }
    }
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| unsafe {
        let mut class =
            objc::declare::ClassDecl::new("XroseNotificationDelegate", class!(NSObject))
                .expect("notification delegate class");
        if let Some(protocol) = objc::runtime::Protocol::get("UNUserNotificationCenterDelegate") {
            class.add_protocol(protocol);
        }
        class.add_method(
            sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
            present as extern "C" fn(&Object, Sel, id, id, id),
        );
        class.register();
        // The center's delegate is weak; retain this instance for the process lifetime.
        let delegate: id = msg_send![class!(XroseNotificationDelegate), new];
        let _: () = msg_send![center, setDelegate: delegate];
    });
}
