//! X11 can route scanner buttons independently of wheel and keyboard input.
use super::platform::Pointer;
use gpui_kit::{DisplayId, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        shape::{ConnectionExt as _, SK, SO},
        xfixes::ConnectionExt as _,
        xinput::{self, ConnectionExt as _},
        xproto::*,
    },
    rust_connection::RustConnection,
};

pub(super) struct DesktopInput {
    conn: RustConnection,
    root: u32,
    window: u32,
    scale: f32,
    revision: u32,
    cursor_hidden: bool,
    desktop_atoms: [Atom; 2],
}
impl DesktopInput {
    pub(super) fn new(window: &Window, _display: DisplayId) -> Option<Self> {
        let id = match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Xcb(w) => w.window.get(),
            RawWindowHandle::Xlib(w) => w.window as u32,
            _ => return None,
        };
        let (conn, screen) = RustConnection::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let min = conn.setup().min_keycode;
        let keys = conn
            .get_keyboard_mapping(min, conn.setup().max_keycode - min + 1)
            .ok()?
            .reply()
            .ok()?;
        let esc = keys
            .keysyms
            .chunks(keys.keysyms_per_keycode as usize)
            .position(|s| s.first() == Some(&0xff1b))? as u8
            + min;
        conn.grab_key(
            false,
            root,
            ModMask::default(),
            esc,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .ok()?
        .check()
        .ok()?;
        // Raw button notifications also work when a window manager owns a
        // conflicting passive grab. Leave its shortcuts and all wheel events alone.
        for button in [ButtonIndex::M1, ButtonIndex::M3] {
            if let Ok(grab) = conn.grab_button(
                false,
                root,
                EventMask::BUTTON_PRESS,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                button,
                ModMask::default(),
            ) {
                let _ = grab.check();
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
        conn.shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, id, 0, 0, &[])
            .ok()?
            .check()
            .ok()?;
        conn.flush().ok()?;
        Some(Self {
            conn,
            root,
            window: id,
            scale: window.scale_factor(),
            revision: 0,
            cursor_hidden: true,
            desktop_atoms,
        })
    }
    pub(super) fn sample(&mut self) -> Pointer {
        let Ok(query) = self.conn.query_pointer(self.window) else {
            return Pointer {
                cancel: true,
                ..Default::default()
            };
        };
        let Ok(p) = query.reply() else {
            return Pointer {
                cancel: true,
                ..Default::default()
            };
        };
        let pos = [p.win_x as f32 / self.scale, p.win_y as f32 / self.scale];
        let mut sample = Pointer {
            position: pos,
            ..Default::default()
        };
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::KeyPress(_) => sample.cancel = true,
                Event::ButtonPress(b) if b.detail == 3 => sample.cancel = true,
                Event::ButtonPress(b) if b.detail == 1 => {
                    sample.clicked = Some([
                        (i32::from(b.root_x) - i32::from(p.root_x) + i32::from(p.win_x)) as f32
                            / self.scale,
                        (i32::from(b.root_y) - i32::from(p.root_y) + i32::from(p.win_y)) as f32
                            / self.scale,
                    ])
                }
                Event::XinputRawButtonPress(b) if b.detail == 3 => sample.cancel = true,
                Event::XinputRawButtonPress(b) if b.detail == 1 => sample.clicked = Some(pos),
                Event::XinputRawButtonPress(b) if (4..=7).contains(&b.detail) => {
                    self.revision = self.revision.wrapping_add(1)
                }
                Event::PropertyNotify(event) if self.desktop_atoms.contains(&event.atom) => {
                    self.revision = self.revision.wrapping_add(1)
                }
                _ => {}
            }
        }
        sample.revision = self.revision;
        sample
    }
    pub(super) fn set_cursor_visible(&mut self, visible: bool) {
        if self.cursor_hidden != visible {
            return;
        }
        let request = if visible {
            self.conn.xfixes_show_cursor(self.root)
        } else {
            self.conn.xfixes_hide_cursor(self.root)
        };
        if request.is_ok() && self.conn.flush().is_ok() {
            self.cursor_hidden = !visible;
        }
    }
}
impl Drop for DesktopInput {
    fn drop(&mut self) {
        self.set_cursor_visible(true);
    }
}
