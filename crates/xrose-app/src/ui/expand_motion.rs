//! Fast, natural disclosure transitions (height reveal, height morph, subtle fade/slide,
//! and chevron rotation) respecting the OS reduced-motion preference.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_base::Easing;
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Style, Styled, Window, div,
    point, px, relative, size,
};

/// Duration for expanding a collapsed disclosure panel (DESIGN.md §3.4: 120–180ms ease-out).
pub(crate) const EXPAND_DURATION: Duration = Duration::from_millis(165);

/// Duration for collapsing an expanded disclosure panel.
pub(crate) const COLLAPSE_DURATION: Duration = Duration::from_millis(135);

/// Duration for morphing height when switching content inside an open container
/// (e.g., Mesh device list <-> port forwarding list).
pub(crate) const MORPH_DURATION: Duration = Duration::from_millis(150);

/// Subtle vertical travel (in logical pixels) applied to content during reveal/collapse.
const REVEAL_SLIDE_PX: f32 = 4.0;

/// Subtle vertical travel applied to incoming content during an in-place variant morph.
const MORPH_SLIDE_PX: f32 = 3.0;

/// Minimum animated travel height cap used when viewport height is unavailable or tiny.
const MIN_VIEWPORT_CAP_PX: f32 = 320.0;

/// Emphasized ease-out curve matching `gpui-component`'s `easing_move`: brisk launch,
/// smooth deceleration, zero overshoot.
const REVEAL_EASING: Easing = Easing::CubicBezier {
    x1: 0.2,
    y1: 0.0,
    x2: 0.0,
    y2: 1.0,
};

/// Sampled state of a single expandable section for the current frame.
#[derive(Clone, Debug)]
pub(crate) struct ExpandSample {
    /// Visual open progress in `0.0..=1.0` (eased).
    pub progress: f32,
    /// True while open/close or variant-morph motion is actively running.
    pub animating: bool,
    /// True when content must remain mounted (either open or still collapsing).
    pub mounted: bool,
    mode: RevealMode,
    measured_height: Rc<Cell<Option<Pixels>>>,
    visible_height: Rc<Cell<Option<Pixels>>>,
}

impl ExpandSample {
    fn closed_untracked() -> Self {
        Self {
            progress: 0.0,
            animating: false,
            mounted: false,
            mode: RevealMode::Settled,
            measured_height: Rc::new(Cell::new(None)),
            visible_height: Rc::new(Cell::new(None)),
        }
    }

    /// Wrap expandable content in a measured, clipped reveal container driven by this sample.
    ///
    /// When settled open, the child lays out directly in native flex flow with zero clipping
    /// mask or extra measurement pass, while recording its height for future collapses/morphs.
    pub(crate) fn wrap(&self, id: impl Into<ElementId>, content: impl IntoElement) -> AnyElement {
        let child = match self.mode {
            RevealMode::Settled => content.into_any_element(),
            RevealMode::Reveal {
                progress, entering, ..
            } => {
                let alpha = reveal_opacity(progress, entering);
                div()
                    .w_full()
                    .opacity(alpha)
                    .child(content)
                    .into_any_element()
            }
            RevealMode::Morph { progress, .. } => {
                let alpha = morph_opacity(progress);
                div()
                    .w_full()
                    .opacity(alpha)
                    .child(content)
                    .into_any_element()
            }
        };
        RevealElement {
            id: id.into(),
            mode: self.mode,
            measured_height: self.measured_height.clone(),
            visible_height: self.visible_height.clone(),
            child,
        }
        .into_any_element()
    }

    /// Wrap content with only the gentle entrance fade (used inside fixed-size OS popup windows
    /// like the tray flyout where the window itself resizes externally).
    pub(crate) fn wrap_fade(&self, content: impl IntoElement) -> AnyElement {
        if !self.animating {
            return content.into_any_element();
        }
        div()
            .w_full()
            .opacity(reveal_opacity(self.progress, true))
            .child(content)
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug)]
enum RevealMode {
    /// Fully open and settled: native Taffy flex layout, records bounds height on prepaint.
    Settled,
    /// Opening or closing between `0` and full content height (or `from_height` when collapsing
    /// out of an active morph).
    Reveal {
        progress: f32,
        entering: bool,
        collapse_from: Option<Pixels>,
    },
    /// Already open, morphing smoothly from a previous rendered height to the new content height.
    Morph { from_height: Pixels, progress: f32 },
}

#[derive(Clone)]
struct RevealEntry {
    open: bool,
    from: f32,
    target: f32,
    started_at: Instant,
    duration: Duration,
    measured_height: Rc<Cell<Option<Pixels>>>,
    visible_height: Rc<Cell<Option<Pixels>>>,
    collapse_from_height: Option<Pixels>,
    variant: Option<u8>,
    morph_from_height: Option<Pixels>,
    morph_started_at: Option<Instant>,
}

impl RevealEntry {
    fn settled(open: bool, variant: Option<u8>, now: Instant) -> Self {
        let value = if open { 1.0 } else { 0.0 };
        Self {
            open,
            from: value,
            target: value,
            started_at: now,
            duration: Duration::ZERO,
            measured_height: Rc::new(Cell::new(None)),
            visible_height: Rc::new(Cell::new(None)),
            collapse_from_height: None,
            variant,
            morph_from_height: None,
            morph_started_at: None,
        }
    }

    fn current_open_progress(&self, now: Instant) -> (f32, bool) {
        if self.duration.is_zero() {
            return (self.target, false);
        }
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed >= self.duration {
            return (self.target, false);
        }
        let raw = (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        let eased = REVEAL_EASING.sample(raw).clamp(0.0, 1.0);
        let value = (self.from + (self.target - self.from) * eased).clamp(0.0, 1.0);
        (value, true)
    }

    fn retarget(&mut self, open: bool, now: Instant, reduce_motion: bool) {
        let next_target = if open { 1.0 } else { 0.0 };
        let was_morphing = self.morph_started_at.is_some();
        self.open = open;
        self.morph_from_height = None;
        self.morph_started_at = None;
        self.collapse_from_height = if !open && was_morphing {
            self.visible_height.get().filter(|h| *h > px(0.5))
        } else {
            None
        };
        if reduce_motion {
            self.from = next_target;
            self.target = next_target;
            self.started_at = now;
            self.duration = Duration::ZERO;
            self.collapse_from_height = None;
            return;
        }
        let (current, _) = self.current_open_progress(now);
        let distance = (next_target - current).abs();
        if distance <= 0.001 {
            self.from = next_target;
            self.target = next_target;
            self.started_at = now;
            self.duration = Duration::ZERO;
            self.collapse_from_height = None;
            return;
        }
        let full = if open {
            EXPAND_DURATION
        } else {
            COLLAPSE_DURATION
        };
        let duration = full.mul_f32(distance).max(Duration::from_millis(16));
        self.from = current;
        self.target = next_target;
        self.started_at = now;
        self.duration = duration;
    }

    fn current_morph_progress(&self, now: Instant) -> Option<(Pixels, f32)> {
        let from_height = self.morph_from_height?;
        let started_at = self.morph_started_at?;
        let elapsed = now.saturating_duration_since(started_at);
        if elapsed >= MORPH_DURATION {
            return None;
        }
        let raw = (elapsed.as_secs_f32() / MORPH_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let eased = REVEAL_EASING.sample(raw).clamp(0.0, 1.0);
        Some((from_height, eased))
    }

    fn start_morph(&mut self, next_variant: u8, now: Instant, reduce_motion: bool) {
        self.variant = Some(next_variant);
        if reduce_motion {
            self.morph_from_height = None;
            self.morph_started_at = None;
            return;
        }
        if let Some(prev_height) = self
            .visible_height
            .get()
            .or_else(|| self.measured_height.get())
            .filter(|h| *h > px(0.5))
        {
            self.morph_from_height = Some(prev_height);
            self.morph_started_at = Some(now);
        }
    }

    fn finish_if_settled(&mut self, now: Instant) {
        if !self.duration.is_zero()
            && now.saturating_duration_since(self.started_at) >= self.duration
        {
            self.from = self.target;
            self.duration = Duration::ZERO;
            self.collapse_from_height = None;
        }
        if let Some(started_at) = self.morph_started_at
            && now.saturating_duration_since(started_at) >= MORPH_DURATION
        {
            self.morph_from_height = None;
            self.morph_started_at = None;
        }
    }
}

/// View-scoped registry of active and settled disclosure transitions.
///
/// Uses interior mutability (`RefCell`) so `&self` render methods can sample transitions
/// without restructuring view signatures, while keeping state independent of GPUI's
/// element-tree nesting (critical for virtualized node rows that move in and out of
/// `node_rows` chunks when expanded).
#[derive(Default)]
pub(crate) struct ExpandTransitions {
    sections: RefCell<HashMap<String, RevealEntry>>,
    nodes: RefCell<HashMap<String, RevealEntry>>,
}

impl ExpandTransitions {
    /// Sample a named collapsible section (mesh card subsection, group fold, inline form, etc.).
    ///
    /// First render adopts `open` immediately so initially-open sections do not spuriously
    /// animate on page navigation. Any subsequent change to `open` smoothly transitions.
    pub(crate) fn sample(
        &self,
        key: &str,
        open: bool,
        window: &mut Window,
        cx: &App,
    ) -> ExpandSample {
        let now = cx.background_executor().now();
        let sample = self.sample_internal(key, open, None, now, cx.reduce_motion());
        if sample.animating {
            window.request_animation_frame();
        }
        sample
    }

    /// Sample a collapsible section that also switches between content variants while open
    /// (e.g., Mesh device list `variant = 0` <-> port forwarding list `variant = 1`).
    pub(crate) fn sample_variant(
        &self,
        key: &str,
        open: bool,
        variant: u8,
        window: &mut Window,
        cx: &App,
    ) -> ExpandSample {
        let now = cx.background_executor().now();
        let sample = self.sample_internal(key, open, Some(variant), now, cx.reduce_motion());
        if sample.animating {
            window.request_animation_frame();
        }
        sample
    }

    fn sample_internal(
        &self,
        key: &str,
        open: bool,
        variant: Option<u8>,
        now: Instant,
        reduce_motion: bool,
    ) -> ExpandSample {
        let mut map = self.sections.borrow_mut();
        let entry = map
            .entry(key.to_owned())
            .or_insert_with(|| RevealEntry::settled(open, variant, now));

        if reduce_motion {
            entry.open = open;
            entry.from = if open { 1.0 } else { 0.0 };
            entry.target = entry.from;
            entry.duration = Duration::ZERO;
            entry.variant = variant;
            entry.morph_from_height = None;
            entry.morph_started_at = None;
        } else if entry.open != open {
            entry.variant = variant;
            entry.retarget(open, now, false);
        } else if let Some(next_variant) = variant
            && entry.variant != Some(next_variant)
        {
            if open {
                entry.start_morph(next_variant, now, false);
            } else {
                entry.variant = Some(next_variant);
            }
        }

        entry.finish_if_settled(now);
        build_sample(entry, now)
    }

    /// Explicitly start a node row expand or collapse transition.
    ///
    /// Called whenever `AppView::expanded_node` changes so virtualized node rows that
    /// were not mounted while collapsed still start cleanly from `0.0 -> 1.0`, and
    /// closing rows smoothly animate `current -> 0.0` before returning to virtualization.
    pub(crate) fn begin_node(&self, key: &str, open: bool, cx: &App) {
        let now = cx.background_executor().now();
        self.begin_node_at(key, open, now, cx.reduce_motion());
    }

    fn begin_node_at(&self, key: &str, open: bool, now: Instant, reduce_motion: bool) {
        let mut map = self.nodes.borrow_mut();
        if reduce_motion {
            if open {
                map.insert(key.to_owned(), RevealEntry::settled(true, None, now));
            } else {
                map.remove(key);
            }
            return;
        }
        let entry = map
            .entry(key.to_owned())
            .or_insert_with(|| RevealEntry::settled(!open, None, now));
        entry.retarget(open, now, false);
    }

    /// Return all node row keys that must currently be rendered outside fixed-height
    /// virtualized chunks (the expanded node, plus any recently-closed nodes still collapsing).
    /// Also prunes settled-closed node entries so the table stays bounded.
    pub(crate) fn active_node_keys(&self, expanded_node: Option<&str>, cx: &App) -> Vec<String> {
        let now = cx.background_executor().now();
        self.active_node_keys_at(expanded_node, now, cx.reduce_motion())
    }

    fn active_node_keys_at(
        &self,
        expanded_node: Option<&str>,
        now: Instant,
        reduce_motion: bool,
    ) -> Vec<String> {
        let mut map = self.nodes.borrow_mut();
        if reduce_motion {
            map.retain(|k, _| expanded_node == Some(k.as_str()));
            return expanded_node
                .map(|k| vec![k.to_owned()])
                .unwrap_or_default();
        }
        let mut active = Vec::new();
        map.retain(|key, entry| {
            let is_expanded = expanded_node == Some(key.as_str());
            if entry.open != is_expanded {
                entry.retarget(is_expanded, now, false);
            }
            entry.finish_if_settled(now);
            let (progress, running) = entry.current_open_progress(now);
            let keep = is_expanded || running || progress > 0.001;
            if keep {
                active.push(key.clone());
            }
            keep
        });
        if let Some(key) = expanded_node
            && !active.iter().any(|existing| existing == key)
        {
            map.insert(key.to_owned(), RevealEntry::settled(true, None, now));
            active.push(key.to_owned());
        }
        active
    }

    /// Sample a node row's disclosure progress during render.
    ///
    /// Fast-paths untouched collapsed rows without inserting into the map.
    pub(crate) fn sample_node(
        &self,
        key: &str,
        expanded: bool,
        window: &mut Window,
        cx: &App,
    ) -> ExpandSample {
        let now = cx.background_executor().now();
        let sample = self.sample_node_at(key, expanded, now, cx.reduce_motion());
        if sample.animating {
            window.request_animation_frame();
        }
        sample
    }

    fn sample_node_at(
        &self,
        key: &str,
        expanded: bool,
        now: Instant,
        reduce_motion: bool,
    ) -> ExpandSample {
        let mut map = self.nodes.borrow_mut();
        if !expanded && !map.contains_key(key) {
            return ExpandSample::closed_untracked();
        }
        let entry = map
            .entry(key.to_owned())
            .or_insert_with(|| RevealEntry::settled(expanded, None, now));
        if reduce_motion {
            if expanded {
                entry.open = true;
                entry.from = 1.0;
                entry.target = 1.0;
                entry.duration = Duration::ZERO;
                return build_sample(entry, now);
            } else {
                map.remove(key);
                return ExpandSample::closed_untracked();
            }
        }
        if entry.open != expanded {
            entry.retarget(expanded, now, false);
        }
        entry.finish_if_settled(now);
        let sample = build_sample(entry, now);
        if !sample.mounted {
            map.remove(key);
        }
        sample
    }
}

fn build_sample(entry: &RevealEntry, now: Instant) -> ExpandSample {
    let (progress, open_running) = entry.current_open_progress(now);
    if open_running {
        return ExpandSample {
            progress,
            animating: true,
            mounted: true,
            mode: RevealMode::Reveal {
                progress,
                entering: entry.open,
                collapse_from: entry.collapse_from_height,
            },
            measured_height: entry.measured_height.clone(),
            visible_height: entry.visible_height.clone(),
        };
    }
    if entry.open {
        if let Some((from_height, morph_progress)) = entry.current_morph_progress(now) {
            return ExpandSample {
                progress: 1.0,
                animating: true,
                mounted: true,
                mode: RevealMode::Morph {
                    from_height,
                    progress: morph_progress,
                },
                measured_height: entry.measured_height.clone(),
                visible_height: entry.visible_height.clone(),
            };
        }
        ExpandSample {
            progress: 1.0,
            animating: false,
            mounted: true,
            mode: RevealMode::Settled,
            measured_height: entry.measured_height.clone(),
            visible_height: entry.visible_height.clone(),
        }
    } else {
        ExpandSample {
            progress: 0.0,
            animating: false,
            mounted: false,
            mode: RevealMode::Settled,
            measured_height: entry.measured_height.clone(),
            visible_height: entry.visible_height.clone(),
        }
    }
}

/// Opacity curve paired with height reveal: opening reaches crisp legibility slightly
/// before height finishes; closing fades softly as height collapses.
fn reveal_opacity(progress: f32, entering: bool) -> f32 {
    let p = progress.clamp(0.0, 1.0);
    if entering {
        (p * 1.25).clamp(0.0, 1.0)
    } else {
        p.powf(1.15).clamp(0.0, 1.0)
    }
}

fn morph_opacity(progress: f32) -> f32 {
    (0.35 + 0.65 * progress.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// Compute the animated target height while capping huge lists to viewport height so
/// off-screen pixels never swallow the visible transition.
fn capped_animated_height(measured: Pixels, viewport_height: Pixels) -> Pixels {
    let cap = viewport_height.max(px(MIN_VIEWPORT_CAP_PX));
    measured.min(cap)
}

struct RevealElement {
    id: ElementId,
    mode: RevealMode,
    measured_height: Rc<Cell<Option<Pixels>>>,
    visible_height: Rc<Cell<Option<Pixels>>>,
    child: AnyElement,
}

impl IntoElement for RevealElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

enum RevealLayoutState {
    Settled,
    Clipped,
}

struct RevealPrepaintState {
    painted: bool,
    clipped: bool,
}

impl Element for RevealElement {
    type RequestLayoutState = RevealLayoutState;
    type PrepaintState = RevealPrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let viewport_h = window.viewport_size().height;

        match self.mode {
            RevealMode::Settled => {
                let child_layout = self.child.request_layout(window, cx);
                (
                    window.request_layout(style, Some(child_layout), cx),
                    RevealLayoutState::Settled,
                )
            }
            RevealMode::Reveal {
                progress,
                collapse_from,
                ..
            } => {
                let base = collapse_from.or_else(|| self.measured_height.get());
                let target_h = match base {
                    Some(measured) => capped_animated_height(measured, viewport_h) * progress,
                    None => px(0.0),
                };
                style.size.height = target_h.into();
                (
                    window.request_layout(style, None, cx),
                    RevealLayoutState::Clipped,
                )
            }
            RevealMode::Morph {
                from_height,
                progress,
            } => {
                let from_h = capped_animated_height(from_height, viewport_h);
                let to_h = self
                    .measured_height
                    .get()
                    .map(|m| capped_animated_height(m, viewport_h))
                    .unwrap_or(from_h);
                let current_h = from_h + (to_h - from_h) * progress;
                style.size.height = current_h.max(px(0.0)).into();
                (
                    window.request_layout(style, None, cx),
                    RevealLayoutState::Clipped,
                )
            }
        }
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        match layout_state {
            RevealLayoutState::Settled => {
                self.measured_height.set(Some(bounds.size.height));
                self.visible_height.set(Some(bounds.size.height));
                self.child.prepaint(window, cx);
                RevealPrepaintState {
                    painted: true,
                    clipped: false,
                }
            }
            RevealLayoutState::Clipped => {
                let measured = self.child.layout_as_root(
                    size(
                        AvailableSpace::Definite(bounds.size.width),
                        AvailableSpace::MinContent,
                    ),
                    window,
                    cx,
                );
                self.measured_height.set(Some(measured.height));
                self.visible_height.set(Some(bounds.size.height));

                if bounds.size.height <= px(0.5) {
                    return RevealPrepaintState {
                        painted: false,
                        clipped: true,
                    };
                }

                let slide_y = match self.mode {
                    RevealMode::Reveal { progress, .. } => {
                        px(-REVEAL_SLIDE_PX * (1.0 - progress.clamp(0.0, 1.0)))
                    }
                    RevealMode::Morph { progress, .. } => {
                        px(-MORPH_SLIDE_PX * (1.0 - progress.clamp(0.0, 1.0)))
                    }
                    RevealMode::Settled => px(0.0),
                };
                let origin = point(bounds.origin.x, bounds.origin.y + slide_y);
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    self.child.prepaint_at(origin, window, cx);
                });
                RevealPrepaintState {
                    painted: true,
                    clipped: true,
                }
            }
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if !prepaint.painted {
            return;
        }
        if prepaint.clipped {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                self.child.paint(window, cx);
            });
        } else {
            self.child.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_render_adopts_open_state_without_spurious_animation() {
        let motion = ExpandTransitions::default();
        let t0 = Instant::now();

        let open_sample = motion.sample_internal("mesh-card-1", true, Some(0), t0, false);
        assert_eq!(open_sample.progress, 1.0);
        assert!(!open_sample.animating);
        assert!(open_sample.mounted);

        let closed_sample = motion.sample_internal("dns-form", false, None, t0, false);
        assert_eq!(closed_sample.progress, 0.0);
        assert!(!closed_sample.animating);
        assert!(!closed_sample.mounted);
    }

    #[test]
    fn open_close_and_midflight_reversal_stay_continuous() {
        let motion = ExpandTransitions::default();
        let t0 = Instant::now();

        let closed = motion.sample_internal("section", false, None, t0, false);
        assert!(!closed.mounted);

        // Start opening
        let open_start = motion.sample_internal("section", true, None, t0, false);
        assert!(open_start.animating);
        assert!(open_start.mounted);
        assert_eq!(open_start.progress, 0.0);

        // Mid-flight during opening
        let t_mid = t0 + Duration::from_millis(55);
        let mid = motion.sample_internal("section", true, None, t_mid, false);
        assert!(mid.animating);
        assert!(mid.progress > 0.3 && mid.progress < 1.0);

        // Reverse to closing mid-flight: position stays continuous without jumping to 1.0
        let rev_start = motion.sample_internal("section", false, None, t_mid, false);
        assert!(rev_start.animating);
        assert!(rev_start.mounted);
        assert!((rev_start.progress - mid.progress).abs() < 0.01);

        // Settles cleanly closed and unmounts
        let t_done = t_mid + COLLAPSE_DURATION + Duration::from_millis(10);
        let settled = motion.sample_internal("section", false, None, t_done, false);
        assert!(!settled.animating);
        assert!(!settled.mounted);
        assert_eq!(settled.progress, 0.0);
    }

    #[test]
    fn variant_switch_while_open_triggers_height_morph_and_settles() {
        let motion = ExpandTransitions::default();
        let t0 = Instant::now();

        let initial = motion.sample_internal("mesh-1", true, Some(0), t0, false);
        initial.visible_height.set(Some(px(260.0)));
        initial.measured_height.set(Some(px(260.0)));

        // Switch from Devices (0) to Forwards (1) while open
        let morph_start = motion.sample_internal("mesh-1", true, Some(1), t0, false);
        assert!(morph_start.animating);
        assert!(morph_start.mounted);
        assert_eq!(morph_start.progress, 1.0);
        assert!(matches!(
            morph_start.mode,
            RevealMode::Morph { from_height, .. } if from_height == px(260.0)
        ));

        let settled = motion.sample_internal(
            "mesh-1",
            true,
            Some(1),
            t0 + MORPH_DURATION + Duration::from_millis(5),
            false,
        );
        assert!(!settled.animating);
        assert!(matches!(settled.mode, RevealMode::Settled));
    }

    #[test]
    fn collapsing_during_active_morph_preserves_visible_height() {
        let motion = ExpandTransitions::default();
        let t0 = Instant::now();

        let initial = motion.sample_internal("mesh-2", true, Some(0), t0, false);
        initial.visible_height.set(Some(px(260.0)));
        initial.measured_height.set(Some(px(260.0)));

        // Begin morph to variant 1, then simulate mid-morph visible height of 180px.
        let morph = motion.sample_internal("mesh-2", true, Some(1), t0, false);
        morph.visible_height.set(Some(px(180.0)));
        morph.measured_height.set(Some(px(72.0)));

        // Collapse mid-morph at t = 40ms: must collapse from 180px rather than snapping to 72px.
        let t_mid = t0 + Duration::from_millis(40);
        let collapsing = motion.sample_internal("mesh-2", false, Some(1), t_mid, false);
        assert!(collapsing.animating && collapsing.mounted);
        assert!(matches!(
            collapsing.mode,
            RevealMode::Reveal {
                entering: false,
                collapse_from: Some(h),
                ..
            } if h == px(180.0)
        ));
    }

    #[test]
    fn accordion_node_switch_keeps_both_closing_and_opening_rows_active_until_settled() {
        let motion = ExpandTransitions::default();
        let t0 = Instant::now();

        // Expand node-a
        motion.begin_node_at("node-a", true, t0, false);
        let active = motion.active_node_keys_at(Some("node-a"), t0, false);
        assert_eq!(active, vec!["node-a"]);

        // Settle node-a open
        let t1 = t0 + EXPAND_DURATION + Duration::from_millis(20);
        let sample_a = motion.sample_node_at("node-a", true, t1, false);
        assert_eq!(sample_a.progress, 1.0);
        assert!(!sample_a.animating);

        // Switch accordion from node-a -> node-b
        motion.begin_node_at("node-a", false, t1, false);
        motion.begin_node_at("node-b", true, t1, false);
        let t_mid = t1 + Duration::from_millis(50);
        let mut active_mid = motion.active_node_keys_at(Some("node-b"), t_mid, false);
        active_mid.sort();
        assert_eq!(active_mid, vec!["node-a", "node-b"]);

        let closing_a = motion.sample_node_at("node-a", false, t_mid, false);
        let opening_b = motion.sample_node_at("node-b", true, t_mid, false);
        assert!(closing_a.animating && closing_a.mounted);
        assert!(opening_b.animating && opening_b.mounted);
        assert!(closing_a.progress > 0.0 && closing_a.progress < 1.0);
        assert!(opening_b.progress > 0.0 && opening_b.progress < 1.0);

        // After collapse finishes, node-a is pruned and only node-b remains active
        let t_after = t1 + EXPAND_DURATION + Duration::from_millis(20);
        let active_done = motion.active_node_keys_at(Some("node-b"), t_after, false);
        assert_eq!(active_done, vec!["node-b"]);
        assert!(
            !motion
                .sample_node_at("node-a", false, t_after, false)
                .mounted
        );
    }

    #[test]
    fn reduce_motion_snaps_immediately_without_animation() {
        let motion = ExpandTransitions::default();
        let t0 = Instant::now();

        let _ = motion.sample_internal("rule-form", false, None, t0, true);
        let open = motion.sample_internal("rule-form", true, None, t0, true);
        assert_eq!(open.progress, 1.0);
        assert!(!open.animating);
        assert!(open.mounted);

        motion.begin_node_at("node-x", true, t0, true);
        let node_open = motion.sample_node_at("node-x", true, t0, true);
        assert_eq!(node_open.progress, 1.0);
        assert!(!node_open.animating);

        motion.begin_node_at("node-x", false, t0, true);
        assert!(motion.active_node_keys_at(None, t0, true).is_empty());
    }

    #[test]
    fn tall_list_height_is_capped_to_viewport_during_animation() {
        assert_eq!(capped_animated_height(px(240.0), px(800.0)), px(240.0));
        assert_eq!(capped_animated_height(px(36_000.0), px(800.0)), px(800.0));
        assert_eq!(capped_animated_height(px(500.0), px(100.0)), px(320.0));
    }
}
