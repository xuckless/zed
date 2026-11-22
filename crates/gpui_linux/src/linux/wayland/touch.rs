//! Touch handling.
//!
//! Ported from rust-windowing/winit src/platform_impl/linux/wayland/seat/touch/mod.rs @ v0.30.13
//! (e9809ef54b18499bb4f2cac945719ecc2a61061b). Copyright the winit contributors. Apache-2.0, see NOTICE.
//! Second reference: Flutter's Linux embedder, engine/src/flutter/shell/platform/linux/fl_touch_manager.cc
//! @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
//!
//! Left out of winit's file: smithay-client-toolkit's `TouchHandler` plumbing and the per-seat lookup (GPUI binds
//! one seat), and the conversion to physical pixels (Wayland's surface coordinates are logical, as GPUI's are).
//! Added for GPUI: a fresh [`TouchId`] per touch, because Wayland reuses `wl_touch` ids and GPUI never reuses one,
//! and the event's time stamp on GPUI's clock (`linux::clock`).

use std::time::Instant;

use collections::HashMap;
use gpui::{Pixels, Point, PointerDevice, TouchEvent, TouchId, TouchPhase};

/// One `wl_touch` event that moves a touch point. `frame` groups events but winit (and GTK, under Flutter) delivers
/// each one as it comes, so it, `shape` and `orientation` (both blank in winit) never become a `WlTouchEvent`.
#[derive(Clone, Debug)]
pub(crate) enum WlTouchEvent<S> {
    Down {
        id: i32,
        surface: S,
        location: Point<Pixels>,
    },
    Up {
        id: i32,
    },
    Motion {
        id: i32,
        location: Point<Pixels>,
    },
    Cancel,
}

/// The state of the touch point.
#[derive(Clone, Debug)]
struct TouchPoint<S> {
    /// The surface on which the point is present.
    surface: S,

    /// The location of the point on the surface.
    location: Point<Pixels>,

    /// GPUI's id for this touch.
    touch_id: TouchId,
}

/// The touched points of the seat, by `wl_touch` id, and the next GPUI id to hand out.
#[derive(Debug)]
pub(crate) struct Touches<S> {
    /// The mapping from touched points to the surfaces they're present.
    touch_map: HashMap<i32, TouchPoint<S>>,
    next_touch_id: u64,
}

impl<S> Default for Touches<S> {
    fn default() -> Self {
        Self {
            touch_map: HashMap::default(),
            next_touch_id: 0,
        }
    }
}

impl<S: Clone> Touches<S> {
    /// The touches one `wl_touch` event makes, each with the surface it lands on. A down must only be passed for a
    /// surface that is a window (winit drops the rest), so the motion and up that follow it are dropped too.
    pub(crate) fn event(
        &mut self,
        event: WlTouchEvent<S>,
        time_stamp: Option<Instant>,
    ) -> Vec<(S, TouchEvent)> {
        let touch = |point: &TouchPoint<S>, phase| {
            (
                point.surface.clone(),
                TouchEvent {
                    id: point.touch_id,
                    phase,
                    position: point.location,
                    predicted_position: None,
                    force: None,
                    device: PointerDevice::Touch,
                    time_stamp,
                },
            )
        };
        match event {
            WlTouchEvent::Down {
                id,
                surface,
                location,
            } => {
                let point = TouchPoint {
                    surface,
                    location,
                    touch_id: TouchId(self.next_touch_id),
                };
                // 2^64 touches never happen; wrapping keeps the counter panic-free.
                self.next_touch_id = self.next_touch_id.wrapping_add(1);
                // Update the state of the point. A compositor that reuses an id still down would leave GPUI's
                // touch open, so that touch is cancelled first.
                let replaced = self.touch_map.insert(id, point.clone());
                replaced
                    .iter()
                    .map(|old| touch(old, TouchPhase::Cancelled))
                    .chain([touch(&point, TouchPhase::Started)])
                    .collect()
            }
            WlTouchEvent::Up { id } => {
                // Remove the touch point.
                self.touch_map
                    .remove(&id)
                    .map(|point| touch(&point, TouchPhase::Ended))
                    .into_iter()
                    .collect()
            }
            WlTouchEvent::Motion { id, location } => self
                .touch_map
                .get_mut(&id)
                .map(|point| {
                    point.location = location;
                    touch(point, TouchPhase::Moved)
                })
                .into_iter()
                .collect(),
            WlTouchEvent::Cancel => {
                let mut points: Vec<_> = self.touch_map.drain().map(|(_, point)| point).collect();
                // The map's order is arbitrary; cancel in the order the touches went down.
                points.sort_by_key(|point| point.touch_id);
                points
                    .iter()
                    .map(|point| touch(point, TouchPhase::Cancelled))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn touches_follow_wl_touch_events() {
        use TouchPhase::*;
        let at = |x: f32, y: f32| point(px(x), px(y));
        let down = |id, surface, x, y| WlTouchEvent::Down {
            id,
            surface,
            location: at(x, y),
        };
        let motion = |id, x, y| WlTouchEvent::Motion {
            id,
            location: at(x, y),
        };
        let up = |id| WlTouchEvent::Up { id };
        let cancel = WlTouchEvent::Cancel;
        // (row name, wl_touch events (a surface is a name), touches out: (surface, GPUI id, phase, x, y)).
        let cases: &[(
            &str,
            Vec<WlTouchEvent<&str>>,
            &[(&str, u64, TouchPhase, f32, f32)],
        )] = &[
            (
                "tap",
                vec![down(0, "a", 1., 2.), up(0)],
                &[("a", 0, Started, 1., 2.), ("a", 0, Ended, 1., 2.)],
            ),
            (
                "up is where the last motion was",
                vec![down(3, "a", 1., 2.), motion(3, 5., 6.), up(3)],
                &[
                    ("a", 0, Started, 1., 2.),
                    ("a", 0, Moved, 5., 6.),
                    ("a", 0, Ended, 5., 6.),
                ],
            ),
            (
                "a reused wl_touch id is a new touch",
                vec![down(0, "a", 1., 1.), up(0), down(0, "a", 2., 2.), up(0)],
                &[
                    ("a", 0, Started, 1., 1.),
                    ("a", 0, Ended, 1., 1.),
                    ("a", 1, Started, 2., 2.),
                    ("a", 1, Ended, 2., 2.),
                ],
            ),
            (
                "two fingers on two surfaces keep their own",
                vec![
                    down(0, "a", 1., 1.),
                    down(1, "b", 9., 9.),
                    motion(1, 8., 8.),
                    up(0),
                    up(1),
                ],
                &[
                    ("a", 0, Started, 1., 1.),
                    ("b", 1, Started, 9., 9.),
                    ("b", 1, Moved, 8., 8.),
                    ("a", 0, Ended, 1., 1.),
                    ("b", 1, Ended, 8., 8.),
                ],
            ),
            (
                "cancel ends every touch where it was, in the order they went down",
                vec![
                    down(4, "a", 1., 1.),
                    down(2, "b", 2., 2.),
                    motion(4, 3., 3.),
                    cancel.clone(),
                    up(4),
                ],
                &[
                    ("a", 0, Started, 1., 1.),
                    ("b", 1, Started, 2., 2.),
                    ("a", 0, Moved, 3., 3.),
                    ("a", 0, Cancelled, 3., 3.),
                    ("b", 1, Cancelled, 2., 2.),
                ],
            ),
            (
                "rejected: motion, up and cancel without a live down",
                vec![motion(0, 1., 1.), up(0), cancel.clone()],
                &[],
            ),
            (
                "a down on an id still down cancels the old touch",
                vec![down(0, "a", 1., 1.), down(0, "b", 2., 2.)],
                &[
                    ("a", 0, Started, 1., 1.),
                    ("a", 0, Cancelled, 1., 1.),
                    ("b", 1, Started, 2., 2.),
                ],
            ),
        ];
        for (name, events, expected) in cases {
            let mut touches = Touches::default();
            let out: Vec<_> = events
                .iter()
                .flat_map(|event| touches.event(event.clone(), None))
                .map(|(surface, touch)| {
                    (
                        surface,
                        touch.id.0,
                        touch.phase,
                        touch.position,
                        touch.device,
                    )
                })
                .collect();
            let expected: Vec<_> = expected
                .iter()
                .map(|&(surface, id, phase, x, y)| {
                    (surface, id, phase, at(x, y), PointerDevice::Touch)
                })
                .collect();
            assert_eq!(out, expected, "{name}");
        }
    }
}
