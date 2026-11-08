//! KWin clips background effects in the visible frame's coordinate space.
//! Keep the client shadow for resizing, but exclude its inset from the blur origin.
use super::{Desktop, effects};
use gpui_kit::{AnyWindowHandle, App, Context, Global, Window, WindowBackgroundAppearance};
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle, RawWindowHandle};
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    backend::{Backend, ObjectId},
    delegate_noop,
    globals::registry_queue_init,
    protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_surface::WlSurface},
};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

struct MainWindow {
    handle: AnyWindowHandle,
    blur: Weak<SidebarBlur>,
}

impl Global for MainWindow {}

pub fn install(window: &mut Window, cx: &mut Context<crate::ui::AppView>) {
    if cx.compositor_name() != "Wayland" {
        return;
    }
    let blur = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|s| s.eq_ignore_ascii_case("KDE"))
        .then(|| SidebarBlur::new(window))
        .flatten()
        .map(Rc::new);
    cx.set_global(MainWindow {
        handle: window.window_handle(),
        blur: blur.as_ref().map(Rc::downgrade).unwrap_or_default(),
    });
    if let Some(blur) = blur {
        window.set_background_appearance(background(
            window,
            cx,
            crate::window_effects::window_background(),
        ));
        cx.observe_window_bounds(window, move |_, window, _| {
            blur.update(window, crate::window_effects::blur_backdrop())
        })
        .detach();
    }
}

pub fn background(
    window: &Window,
    cx: &App,
    appearance: WindowBackgroundAppearance,
) -> WindowBackgroundAppearance {
    if let Some(blur) = cx
        .try_global::<MainWindow>()
        .filter(|m| m.handle == window.window_handle())
        .and_then(|m| m.blur.upgrade())
    {
        let enabled = appearance == WindowBackgroundAppearance::Blurred;
        blur.update(window, enabled);
        if enabled {
            return WindowBackgroundAppearance::Transparent;
        }
    }
    appearance
}

struct SidebarBlur {
    effect: ExtBackgroundEffectSurfaceV1,
    compositor: WlCompositor,
    queue: QueueHandle<Desktop>,
    height: Cell<Option<i32>>,
    // The native connection is borrowed from GPUI for the lifetime of the window observer.
    _connection: Connection,
}

impl SidebarBlur {
    fn new(window: &Window) -> Option<Self> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
            return None;
        };
        let RawWindowHandle::Wayland(surface) =
            raw_window_handle::HasWindowHandle::window_handle(window)
                .ok()?
                .as_raw()
        else {
            return None;
        };
        // GPUI owns both handles. The bounds observer owns this wrapper; the app global only borrows it weakly.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) }
                .ok()?;
        let surface = WlSurface::from_id(&connection, id).ok()?;
        let (globals, queue) = registry_queue_init::<Desktop>(&connection).ok()?;
        let queue = queue.handle();
        let manager = globals
            .bind::<effects::ExtBackgroundEffectManagerV1, _, _>(&queue, 1..=1, ())
            .ok()?;
        let compositor = globals.bind::<WlCompositor, _, _>(&queue, 1..=1, ()).ok()?;
        // Release GPUI's full-window effect before attaching the sidebar effect to the same surface.
        window.set_background_appearance(WindowBackgroundAppearance::Transparent);
        let effect = manager.get_background_effect(&surface, &queue, ());
        manager.destroy();
        Some(Self {
            effect,
            compositor,
            queue,
            height: Cell::new(None),
            _connection: connection,
        })
    }

    fn update(&self, window: &Window, enabled: bool) {
        let height = enabled.then(|| f32::from(window.viewport_size().height).ceil() as i32);
        if self.height.replace(height) == height {
            return;
        }
        if let Some(height) = height {
            let region = self.compositor.create_region(&self.queue, ());
            // Include the 1px client border. KWin clips the height to the visible window.
            region.add(0, 0, crate::ui::theme::SIDEBAR_WIDTH as i32 + 1, height);
            self.effect.set_blur_region(Some(&region));
            region.destroy();
        } else {
            self.effect.set_blur_region(None);
        }
    }
}

impl Drop for SidebarBlur {
    fn drop(&mut self) {
        self.effect.destroy();
    }
}

delegate_noop!(Desktop: ignore WlCompositor);
delegate_noop!(Desktop: ignore WlRegion);
delegate_noop!(Desktop: ignore ExtBackgroundEffectSurfaceV1);
