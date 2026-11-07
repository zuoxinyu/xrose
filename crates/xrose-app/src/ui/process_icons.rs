//! Session-local process icons shared by the live, recorded and process lists.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::*;

use super::{AppView, theme::ICON_SIZE};

#[cfg(target_os = "linux")]
mod linux;

const CAPACITY: usize = 256;
const BATCH: usize = 8;
const PIXELS: u32 = 32;
const DEFAULT_ICON: &str = "";

struct Entry {
    image: Option<Arc<RenderImage>>,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct ProcessIcons {
    visible: Rc<RefCell<HashSet<SharedString>>>,
    entries: HashMap<String, Entry>,
    revision: u64,
    task: Option<Task<()>>,
}

impl ProcessIcons {
    fn next_batch(&mut self) -> Vec<String> {
        self.revision += 1;
        let mut keys = self.visible.borrow_mut();
        keys.remove(DEFAULT_ICON);
        let mut missing = Vec::new();
        for key in std::iter::once(SharedString::from(DEFAULT_ICON)).chain(keys.drain()) {
            if let Some(hit) = self.entries.get_mut(&*key) {
                hit.last_used = self.revision;
            } else if missing.len() < BATCH {
                missing.push(key.to_string());
            }
        }
        missing
    }

    pub(super) fn load_visible(&mut self, cx: &mut Context<AppView>) {
        if self.task.is_some() || self.visible.borrow().is_empty() {
            return;
        }
        let missing = self.next_batch();
        if missing.is_empty() {
            return;
        }
        let work = cx.background_executor().spawn(async move {
            missing
                .into_iter()
                .map(|key| {
                    let image = load(&key).map(|pixels| {
                        Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                            pixels
                        )]))
                    });
                    (key, image)
                })
                .collect::<Vec<_>>()
        });
        self.task = Some(cx.spawn(async move |view, cx| {
            let results = work.await;
            let _ = view.update(cx, |view, cx| {
                let Some(cache) = &mut view.process_icons else {
                    return;
                };
                for (key, image) in results {
                    if cache.entries.len() == CAPACITY {
                        let oldest = cache
                            .entries
                            .iter()
                            .filter(|(k, _)| !k.is_empty())
                            .min_by_key(|(_, entry)| entry.last_used)
                            .map(|(k, _)| k.clone());
                        if let Some(image) = oldest
                            .and_then(|k| cache.entries.remove(&k))
                            .and_then(|e| e.image)
                        {
                            cx.drop_image(image, None);
                        }
                    }
                    cache.entries.insert(
                        key,
                        Entry {
                            image,
                            last_used: cache.revision,
                        },
                    );
                }
                cache.task = None;
                cx.notify();
            });
        }));
    }

    pub(super) fn view(&self, key: &str) -> AnyElement {
        let image = |k: &str| self.entries.get(k).and_then(|entry| entry.image.as_ref());
        let content = if let Some(image) = image(key).or_else(|| image(DEFAULT_ICON)) {
            img(image.clone())
                .size(px(ICON_SIZE))
                .flex_shrink_0()
                .into_any_element()
        } else {
            Icon::new(IconName::AppWindow)
                .size(px(ICON_SIZE))
                .into_any_element()
        };
        let key = SharedString::from(key.to_owned());
        let visible = self.visible.clone();
        div()
            .size(px(ICON_SIZE))
            .flex_shrink_0()
            .child(content)
            .on_children_prepainted(move |bounds, window, _| {
                if bounds
                    .first()
                    .is_some_and(|b| b.intersects(&window.content_mask().bounds))
                {
                    visible.borrow_mut().insert(key.clone());
                }
            })
            .into_any_element()
    }

    pub(super) fn clear(self, window: &mut Window, cx: &mut App) {
        for image in self.entries.into_values().filter_map(|entry| entry.image) {
            cx.drop_image(image, Some(window));
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn load(key: &str) -> Option<image::RgbaImage> {
    #[cfg(windows)]
    let key = {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = std::ffi::OsStr::new(key).encode_wide().collect();
        if wide.contains(&0) {
            return None;
        }
        wide.push(0);
        wide
    };
    #[cfg(target_os = "macos")]
    let key = std::ffi::CString::new(key).ok()?;
    unsafe extern "C" {
        fn xrose_process_icon(key: *const std::ffi::c_void, pixels: *mut u8, size: i32) -> i32;
    }
    let mut pixels = image::RgbaImage::new(PIXELS, PIXELS);
    // The platform routine writes exactly size x size BGRA pixels into our owned buffer.
    let ok = unsafe { xrose_process_icon(key.as_ptr().cast(), pixels.as_mut_ptr(), PIXELS as i32) };
    (ok != 0).then_some(pixels)
}

#[cfg(target_os = "linux")]
use linux::load;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn load(_: &str) -> Option<image::RgbaImage> {
    None
}

#[cfg(test)]
mod tests {
    use super::{BATCH, DEFAULT_ICON, Entry, ProcessIcons};

    #[test]
    fn repeated_connections_and_failed_lookups_share_a_single_request() {
        let mut cache = ProcessIcons::default();
        for _ in 0..100 {
            cache.visible.borrow_mut().insert("example-app".into());
        }
        let batch = cache.next_batch();
        assert_eq!(batch, [DEFAULT_ICON, "example-app"]);
        for key in batch {
            cache.entries.insert(
                key,
                Entry {
                    image: None,
                    last_used: 0,
                },
            );
        }
        cache.visible.borrow_mut().insert("example-app".into());
        assert!(cache.next_batch().is_empty());
        for i in 0..100 {
            cache
                .visible
                .borrow_mut()
                .insert(format!("example-app-{i}").into());
        }
        assert_eq!(cache.next_batch().len(), BATCH);
    }
}
