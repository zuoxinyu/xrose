//! Platform-independent geometry and timing for taskbar flyout motion.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    pub fn detect(monitor: [i32; 4], work: [i32; 4], anchor: [i32; 2]) -> Self {
        let candidates = [
            (
                Self::Left,
                work[0] > monitor[0],
                (anchor[0] - monitor[0]).abs(),
            ),
            (
                Self::Top,
                work[1] > monitor[1],
                (anchor[1] - monitor[1]).abs(),
            ),
            (
                Self::Right,
                work[2] < monitor[2],
                (monitor[2] - anchor[0]).abs(),
            ),
            (
                Self::Bottom,
                work[3] < monitor[3],
                (monitor[3] - anchor[1]).abs(),
            ),
        ];
        let has_inset = candidates.iter().any(|(_, inset, _)| *inset);
        candidates
            .into_iter()
            .filter(|(_, inset, _)| !has_inset || *inset)
            .min_by_key(|(_, _, distance)| *distance)
            .map(|(edge, _, _)| edge)
            .unwrap_or(Self::Bottom)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub origin: [i32; 2],
    pub size: [i32; 2],
    pub work: [i32; 4],
    pub edge: Edge,
}

impl Placement {
    pub fn hidden_origin(self) -> [i32; 2] {
        let [x, y] = self.origin;
        match self.edge {
            Edge::Left => [self.work[0] - self.size[0], y],
            Edge::Top => [x, self.work[1] - self.size[1]],
            Edge::Right => [self.work[2], y],
            Edge::Bottom => [x, self.work[3]],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub showing: bool,
    from: [i32; 2],
    to: [i32; 2],
    started: Instant,
    pub duration: Duration,
}

impl Motion {
    pub fn new(showing: bool, from: [i32; 2], placement: Placement, now: Instant) -> Self {
        let hidden = placement.hidden_origin();
        let to = if showing { placement.origin } else { hidden };
        let full = distance(placement.origin, hidden).max(1) as f32;
        let fraction = (distance(from, to) as f32 / full).clamp(0., 1.);
        let millis = if showing { 250. } else { 167. };
        Self {
            showing,
            from,
            to,
            started: now,
            duration: Duration::from_secs_f32((millis * fraction).max(1.) / 1000.),
        }
    }

    pub fn sample(self, now: Instant) -> ([i32; 2], bool) {
        let progress = (now.saturating_duration_since(self.started).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0., 1.);
        let ease = easing(self.showing, progress);
        let position = std::array::from_fn(|i| {
            (self.from[i] as f32 + (self.to[i] - self.from[i]) as f32 * ease).round() as i32
        });
        (position, progress >= 1.)
    }
}

fn distance(a: [i32; 2], b: [i32; 2]) -> i32 {
    (a[0] - b[0]).abs().max((a[1] - b[1]).abs())
}

/// Windows Direct Entrance (0,0,0,1) and Gentle Exit (1,0,1,1).
/// Inverting their x curves analytically avoids timer-dependent approximations.
fn easing(showing: bool, progress: f32) -> f32 {
    if progress <= 0. {
        return 0.;
    }
    if progress >= 1. {
        return 1.;
    }
    let t = if showing {
        progress.cbrt()
    } else {
        1. - (1. - progress).cbrt()
    };
    t * t * (3. - 2. * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskbar_edge_detection_handles_docks_and_autohide() {
        let monitor = [-1920, 0, 0, 1080];
        assert_eq!(
            Edge::detect(monitor, [-1920, 0, 0, 1032], [-50, 1056]),
            Edge::Bottom
        );
        assert_eq!(
            Edge::detect(monitor, [-1920, 48, 0, 1080], [-50, 24]),
            Edge::Top
        );
        assert_eq!(
            Edge::detect(monitor, [-1872, 0, 0, 1080], [-1900, 600]),
            Edge::Left
        );
        assert_eq!(
            Edge::detect(monitor, [-1920, 0, -48, 1080], [-24, 600]),
            Edge::Right
        );
        assert_eq!(Edge::detect(monitor, monitor, [-50, 1079]), Edge::Bottom);
    }

    #[test]
    fn flyouts_start_behind_the_correct_taskbar_edge() {
        let placement = Placement {
            origin: [100, 100],
            size: [400, 600],
            work: [0, 0, 1920, 1032],
            edge: Edge::Bottom,
        };
        assert_eq!(placement.hidden_origin(), [100, 1032]);
        assert_eq!(
            Placement {
                edge: Edge::Top,
                ..placement
            }
            .hidden_origin(),
            [100, -600]
        );
        assert_eq!(
            Placement {
                edge: Edge::Left,
                ..placement
            }
            .hidden_origin(),
            [-400, 100]
        );
        assert_eq!(
            Placement {
                edge: Edge::Right,
                ..placement
            }
            .hidden_origin(),
            [1920, 100]
        );
    }

    #[test]
    fn curves_are_monotonic_and_have_exact_endpoints() {
        for showing in [true, false] {
            assert_eq!(easing(showing, 0.), 0.);
            assert_eq!(easing(showing, 1.), 1.);
            let mut last = 0.;
            for i in 0..=100 {
                let value = easing(showing, i as f32 / 100.);
                assert!(value >= last && value <= 1.);
                last = value;
            }
        }
        assert!(easing(true, 0.5) > 0.5);
        assert!(easing(false, 0.5) < 0.5);
    }

    #[test]
    fn reversing_an_animation_preserves_position_and_scales_duration() {
        let placement = Placement {
            origin: [100, 400],
            size: [400, 600],
            work: [0, 0, 1920, 1032],
            edge: Edge::Bottom,
        };
        let now = Instant::now();
        let opening = Motion::new(true, placement.hidden_origin(), placement, now);
        assert_eq!(opening.duration, Duration::from_millis(250));
        let halfway = now + Duration::from_millis(80);
        let (position, _) = opening.sample(halfway);
        let closing = Motion::new(false, position, placement, halfway);
        assert_eq!(closing.sample(halfway).0, position);
        assert!(closing.duration < Duration::from_millis(167));
        assert_eq!(
            closing.sample(halfway + closing.duration),
            (placement.hidden_origin(), true)
        );
    }
}
