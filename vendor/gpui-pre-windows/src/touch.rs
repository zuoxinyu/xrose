//! Deliver client-area touchscreen contacts to GPUI's tap and pan recognizer.

use std::collections::HashMap;

use gpui::{Pixels, PlatformInput, Point, TouchEvent, TouchId, TouchPhase};
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    UI::{
        Input::Pointer::{GetPointerInfo, POINTER_INFO},
        WindowsAndMessaging::*,
    },
};

use crate::{HiLoWord, WindowsWindowInner, logical_point};

#[derive(Default)]
pub(crate) struct TouchContacts {
    // Win32 may recycle a pointer ID after release. GPUI IDs identify a single contact.
    next_id: u64,
    active: HashMap<u16, (TouchId, Point<Pixels>)>,
}

impl TouchContacts {
    fn event(
        &mut self,
        pointer: u16,
        phase: TouchPhase,
        position: Option<Point<Pixels>>,
    ) -> Option<TouchEvent> {
        let (id, position) = match phase {
            TouchPhase::Started => {
                let position = position?;
                let std::collections::hash_map::Entry::Vacant(slot) = self.active.entry(pointer)
                else {
                    return None;
                };
                let contact = (TouchId(self.next_id), position);
                self.next_id += 1;
                slot.insert(contact);
                contact
            }
            TouchPhase::Moved => {
                let contact = self.active.get_mut(&pointer)?;
                contact.1 = position?;
                *contact
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let (id, last) = self.active.remove(&pointer)?;
                (id, position.unwrap_or(last))
            }
        };
        Some(TouchEvent {
            id,
            phase,
            position,
            ..Default::default()
        })
    }
}

impl WindowsWindowInner {
    pub(crate) fn handle_touch_msg(
        &self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<isize> {
        let pointer = wparam.loword();
        let mut info = POINTER_INFO::default();
        let has_info = unsafe { GetPointerInfo(pointer.into(), &mut info) }.is_ok();
        if !(has_info && info.pointerType == PT_TOUCH)
            && !self
                .state
                .touch_contacts
                .borrow()
                .active
                .contains_key(&pointer)
        {
            return None;
        }

        let phase = if u32::from(wparam.hiword()) & POINTER_MESSAGE_FLAG_CANCELED != 0 {
            TouchPhase::Cancelled
        } else {
            match msg {
                WM_POINTERDOWN => TouchPhase::Started,
                WM_POINTERUPDATE => TouchPhase::Moved,
                WM_POINTERUP => TouchPhase::Ended,
                WM_POINTERLEAVE | WM_POINTERCAPTURECHANGED => TouchPhase::Cancelled,
                _ => return Some(0),
            }
        };
        // CAPTURECHANGED carries an HWND in lParam, not a point. Cancel at the last sample.
        let position = if phase == TouchPhase::Cancelled {
            None
        } else {
            let mut pt = if has_info {
                info.ptPixelLocationRaw
            } else {
                POINT {
                    x: lparam.signed_loword().into(),
                    y: lparam.signed_hiword().into(),
                }
            };
            unsafe { ScreenToClient(hwnd, &mut pt) }
                .as_bool()
                .then(|| logical_point(pt.x as f32, pt.y as f32, self.state.scale_factor.get()))
        };
        let event = self
            .state
            .touch_contacts
            .borrow_mut()
            .event(pointer, phase, position);
        if let Some(event) = event
            && let Some(mut input) = self.state.callbacks.input.take()
        {
            input(PlatformInput::Touch(event));
            self.state.callbacks.input.set(Some(input));
        }
        // DefWindowProc would promote this contact to a second stream of mouse input.
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn capture_loss_cancels_at_last_position_and_reused_ids_start_fresh() {
        let mut contacts = TouchContacts::default();
        let start = point(px(30.), px(80.));
        let moved = point(px(30.), px(40.));
        let first = contacts.event(7, TouchPhase::Started, Some(start)).unwrap();
        let other = contacts.event(8, TouchPhase::Started, Some(start)).unwrap();
        let pan = contacts.event(7, TouchPhase::Moved, Some(moved)).unwrap();
        let cancel = contacts.event(7, TouchPhase::Cancelled, None).unwrap();
        assert_eq!(
            (pan.id, cancel.id, cancel.position),
            (first.id, first.id, moved)
        );
        assert!(contacts.event(7, TouchPhase::Ended, Some(moved)).is_none());
        assert_eq!(
            contacts
                .event(8, TouchPhase::Ended, Some(start))
                .unwrap()
                .id,
            other.id
        );
        let next = contacts.event(7, TouchPhase::Started, Some(start)).unwrap();
        assert_ne!(first.id, next.id);
    }
}
