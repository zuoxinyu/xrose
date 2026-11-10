//! The X11 overlays share one root connection: a key/button grab is exclusive
//! to its client, even when the pointer moves to a different monitor.
use super::platform::{Pointer, SelectionSource};
use gpui_kit::{Bounds, DisplayId, Pixels, Window, point, px, size};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        randr::ConnectionExt as _,
        shape::{ConnectionExt as _, SK, SO},
        xfixes::ConnectionExt as _,
        xinput::{self, ConnectionExt as _},
        xproto::*,
    },
    rust_connection::RustConnection,
};

pub(super) fn outputs(
    display: DisplayId,
    bounds: Bounds<Pixels>,
) -> Option<Vec<(Bounds<Pixels>, u32)>> {
    let (conn, _) = RustConnection::connect(None).ok()?;
    let screen = conn.setup().roots.get(u64::from(display) as usize)?;
    let scale = screen.width_in_pixels as f32 / f32::from(bounds.size.width);
    let monitors = conn
        .randr_get_monitors(screen.root, true)
        .ok()?
        .reply()
        .ok()?
        .monitors;
    let monitors: Vec<_> = monitors
        .into_iter()
        .filter_map(|m| {
            Some((
                Bounds::new(
                    point(px(m.x as f32 / scale), px(m.y as f32 / scale)),
                    size(px(m.width as f32 / scale), px(m.height as f32 / scale)),
                ),
                *m.outputs.first()?,
            ))
        })
        .collect();
    (!monitors.is_empty()).then_some(monitors)
}

thread_local! { static SHARED: RefCell<HashMap<u64, Weak<Root>>> = RefCell::default(); }

#[derive(Clone, Copy, Default)]
struct Events {
    cancel: bool,
    down: Option<[i16; 2]>,
    confirmations: u32,
    selection: Option<([i16; 2], SelectionSource)>,
    revision: u32,
    zoom: i32,
}
struct Root {
    conn: RustConnection,
    root: u32,
    escape: u8,
    desktop_atoms: [Atom; 2],
    events: Cell<Events>,
}

impl Root {
    fn open(screen: usize) -> Option<Rc<Self>> {
        let (conn, _) = RustConnection::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let min = conn.setup().min_keycode;
        let keys = conn
            .get_keyboard_mapping(min, conn.setup().max_keycode - min + 1)
            .ok()?
            .reply()
            .ok()?;
        let keycode = |sym| {
            keys.keysyms
                .chunks(keys.keysyms_per_keycode as usize)
                .position(|s| s.first() == Some(&sym))
                .map(|i| i as u8 + min)
        };
        let escape = keycode(0xff1b)?;
        for locks in [
            ModMask::default(),
            ModMask::LOCK,
            ModMask::M2,
            ModMask::LOCK | ModMask::M2,
        ] {
            for key in [Some(escape), keycode(0xff0d), keycode(0x20)]
                .into_iter()
                .flatten()
            {
                let grab = conn
                    .grab_key(false, root, locks, key, GrabMode::ASYNC, GrabMode::ASYNC)
                    .ok()?;
                if grab.check().is_err() && key == escape && locks == ModMask::default() {
                    return None;
                }
            }
            for (button, modifiers) in [
                (ButtonIndex::M1, locks),
                (ButtonIndex::M3, locks),
                (ButtonIndex::M4, ModMask::CONTROL | locks),
                (ButtonIndex::M5, ModMask::CONTROL | locks),
            ] {
                let grab = conn
                    .grab_button(
                        false,
                        root,
                        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                        x11rb::NONE,
                        x11rb::NONE,
                        button,
                        modifiers,
                    )
                    .ok()?;
                if grab.check().is_err()
                    && locks == ModMask::default()
                    && matches!(button, ButtonIndex::M1 | ButtonIndex::M3)
                {
                    return None;
                }
            }
        }
        let atom = |name: &[u8]| {
            conn.intern_atom(false, name)
                .ok()?
                .reply()
                .ok()
                .map(|r| r.atom)
        };
        let desktop_atoms = [atom(b"_NET_ACTIVE_WINDOW")?, atom(b"_NET_CURRENT_DESKTOP")?];
        conn.xinput_xi_query_version(2, 0).ok()?.reply().ok()?;
        conn.xinput_xi_select_events(
            root,
            &[xinput::EventMask {
                deviceid: xinput::Device::ALL_MASTER.into(),
                mask: vec![xinput::XIEventMask::RAW_BUTTON_PRESS],
            }],
        )
        .ok()?
        .check()
        .ok()?;
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .ok()?;
        conn.xfixes_query_version(4, 0).ok()?.reply().ok()?;
        conn.xfixes_hide_cursor(root).ok()?.check().ok()?;
        Some(Rc::new(Self {
            conn,
            root,
            escape,
            desktop_atoms,
            events: Cell::default(),
        }))
    }

    fn poll(&self) -> Events {
        let mut state = self.events.get();
        let mut selection = None;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::KeyPress(e) if e.detail == self.escape => state.cancel = true,
                Event::KeyPress(e) => {
                    selection = Some(([e.root_x, e.root_y], SelectionSource::Keyboard));
                }
                Event::ButtonPress(e) => match e.detail {
                    1 => state.down = Some([e.root_x, e.root_y]),
                    4 => state.zoom = state.zoom.wrapping_add(1),
                    5 => state.zoom = state.zoom.wrapping_sub(1),
                    _ => {}
                },
                Event::ButtonRelease(e) if e.detail == 1 => {
                    if let Some(pos) = state.down.take() {
                        selection = Some((pos, SelectionSource::Click));
                    }
                }
                Event::ButtonRelease(e) if e.detail == 3 => state.cancel = true,
                Event::XinputRawButtonPress(e) if (4..=7).contains(&e.detail) => {
                    state.revision = state.revision.wrapping_add(1)
                }
                Event::PropertyNotify(e) if self.desktop_atoms.contains(&e.atom) => {
                    state.revision = state.revision.wrapping_add(1)
                }
                _ => {}
            }
        }
        if selection.is_some() {
            state.confirmations = state.confirmations.wrapping_add(1);
            state.selection = selection;
        }
        self.events.set(state);
        state
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = self.conn.xfixes_show_cursor(self.root);
        let _ = self.conn.flush();
    }
}

pub(super) struct DesktopInput {
    shared: Rc<Root>,
    window: u32,
    scale: f32,
    last: Events,
}
impl DesktopInput {
    pub(super) fn new(window: &Window, display: DisplayId) -> Option<Self> {
        let id = match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Xcb(w) => w.window.get(),
            RawWindowHandle::Xlib(w) => w.window as u32,
            _ => return None,
        };
        let shared = SHARED.with_borrow_mut(|slot| {
            let display = u64::from(display);
            if let Some(root) = slot.get(&display).and_then(Weak::upgrade) {
                return Some(root);
            }
            let root = Root::open(display as usize)?;
            slot.insert(display, Rc::downgrade(&root));
            Some(root)
        })?;
        shared
            .conn
            .shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, id, 0, 0, &[])
            .ok()?
            .check()
            .ok()?;
        Some(Self {
            last: shared.events.get(),
            shared,
            window: id,
            scale: window.scale_factor(),
        })
    }

    pub(super) fn sample(&mut self) -> Pointer {
        let Some(p) = self
            .shared
            .conn
            .query_pointer(self.window)
            .ok()
            .and_then(|q| q.reply().ok())
        else {
            return Pointer {
                cancel: true,
                ..Default::default()
            };
        };
        let state = self.shared.poll();
        let last = std::mem::replace(&mut self.last, state);
        let local = |root: i16, pointer: i16, window: i16| {
            (i32::from(root) - i32::from(pointer) + i32::from(window)) as f32 / self.scale
        };
        Pointer {
            position: [p.win_x as f32 / self.scale, p.win_y as f32 / self.scale],
            selection: state
                .selection
                .filter(|_| last.confirmations != state.confirmations)
                .map(|([x, y], source)| {
                    (
                        [local(x, p.root_x, p.win_x), local(y, p.root_y, p.win_y)],
                        source,
                    )
                }),
            cancel: state.cancel,
            revision: state.revision,
            pressed: p.mask.contains(KeyButMask::BUTTON1),
            zoom_delta: state.zoom.wrapping_sub(last.zoom) as f32,
        }
    }
}
