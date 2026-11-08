//! Compositor capabilities and output layout in compositor logical coordinates.
//! GPUI's display list uses the integer wl_output scale, which can differ from
//! the logical geometry reported by xdg-output on fractional-scale desktops.
pub mod main_window;

use gpui_kit::{App, Bounds, Pixels, PlatformDisplay, point, px, size};
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::LazyLock;
use uuid::Uuid;
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output, wl_registry},
};
use wayland_protocols::{
    ext::background_effect::v1::client::ext_background_effect_manager_v1 as effects,
    xdg::xdg_output::zv1::client::{zxdg_output_manager_v1 as outputs, zxdg_output_v1 as output},
};

#[derive(Default)]
pub struct Desktop {
    pub blur: bool,
    pub layer_shell: bool,
    outputs: Vec<(Option<Uuid>, Bounds<Pixels>)>,
}

static DESKTOP: LazyLock<RwLock<Desktop>> = LazyLock::new(Default::default);

pub fn desktop() -> RwLockReadGuard<'static, Desktop> {
    DESKTOP.read()
}

pub fn init(cx: &mut App) {
    if cx.compositor_name() == "Wayland" {
        *DESKTOP.write() = Desktop::probe().unwrap_or_default();
        cx.observe_displays(|_, _| {
            if let Some(desktop) = Desktop::probe() {
                *DESKTOP.write() = desktop;
            }
        })
        .detach();
    }
}

impl Desktop {
    pub fn bounds(&self, display: &dyn PlatformDisplay) -> Bounds<Pixels> {
        // Older wl_output versions don't provide GPUI with the output name.
        if let [(_, bounds)] = self.outputs.as_slice() {
            return *bounds;
        }
        display
            .uuid()
            .ok()
            .and_then(|id| {
                self.outputs
                    .iter()
                    .find(|(uuid, _)| *uuid == Some(id))
                    .map(|(_, b)| *b)
            })
            .unwrap_or_else(|| display.bounds())
    }

    fn probe() -> Option<Self> {
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<Self>(&conn).ok()?;
        let qh = queue.handle();
        let list = globals.contents().clone_list();
        let has = |name| list.iter().any(|g| g.interface == name);
        let mut desktop = Self {
            layer_shell: has("zwlr_layer_shell_v1"),
            blur: has("org_kde_kwin_blur_manager"),
            outputs: Vec::new(),
        };
        let _effects = globals
            .bind::<effects::ExtBackgroundEffectManagerV1, _, _>(&qh, 1..=1, ())
            .ok();
        if let Ok(manager) = globals.bind::<outputs::ZxdgOutputManagerV1, _, _>(&qh, 2..=3, ()) {
            for g in list.iter().filter(|g| g.interface == "wl_output") {
                let monitor = globals.registry().bind::<wl_output::WlOutput, _, _>(
                    g.name,
                    g.version.min(4),
                    &qh,
                    (),
                );
                manager.get_xdg_output(&monitor, &qh, desktop.outputs.len());
                desktop.outputs.push((None, Bounds::default()));
            }
        }
        queue.roundtrip(&mut desktop).ok()?;
        desktop
            .outputs
            .retain(|(_, b)| b.size.width > px(0.) && b.size.height > px(0.));
        Some(desktop)
    }
}

delegate_noop!(Desktop: ignore wl_output::WlOutput);
delegate_noop!(Desktop: ignore outputs::ZxdgOutputManagerV1);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Desktop {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<effects::ExtBackgroundEffectManagerV1, ()> for Desktop {
    fn event(
        state: &mut Self,
        _: &effects::ExtBackgroundEffectManagerV1,
        event: effects::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let effects::Event::Capabilities { flags } = event {
            state.blur =
                matches!(flags, WEnum::Value(caps) if caps.contains(effects::Capability::Blur));
        }
    }
}

impl Dispatch<output::ZxdgOutputV1, usize> for Desktop {
    fn event(
        state: &mut Self,
        _: &output::ZxdgOutputV1,
        event: output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let (id, bounds) = &mut state.outputs[*index];
        match event {
            output::Event::Name { name } => {
                *id = Some(Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes()))
            }
            output::Event::LogicalPosition { x, y } => {
                bounds.origin = point(px(x as f32), px(y as f32))
            }
            output::Event::LogicalSize { width, height } => {
                bounds.size = size(px(width as f32), px(height as f32))
            }
            _ => {}
        }
    }
}
