// Ported from flutter/flutter packages/flutter/lib/src/widgets/scroll_position.dart (`setPixels`,
// `applyNewDimensions`), scroll_position_with_single_context.dart (`applyUserOffset`, `goBallistic`, `goIdle`,
// `jumpTo`, `hold`), scroll_activity.dart (`ScrollDragController.end` / `cancel`, `HoldScrollActivity`,
// `BallisticScrollActivity._tick` / `_end` / `applyMoveTo` / `applyNewDimensions` / `shouldIgnorePointer`,
// `IdleScrollActivity.applyNewDimensions`), scroll_physics.dart (`shouldAcceptUserOffset`), scroll_view.dart (a
// vertical primary scroll view's `AlwaysScrollableScrollPhysics`), gestures/monodrag.dart (a trackpad drag's
// velocity tracking and `_checkEnd` / `considerFling`) and stretch_effect.dart (the `Transform` fallback's scale and
// alignment) @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors.
// BSD-3-Clause, see third_party/flutter/LICENSE.
//
// Dropped: the activity delegate plumbing, `carriedMomentum`, user scroll direction, and dispatching
// `OverscrollIndicatorNotification` to ancestors (nobody receives it). Glue: GPUI's scroll offset along an axis is
// Flutter's `pixels` negated, and a touch or trackpad scroll's delta is Flutter's drag delta. A hold is a drag from
// the first pixel (the touch recognizer's `Caught` touch), so `HoldScrollActivity` is the drag's start.

use scheduler::Instant;

use super::{
    AxisDirection, GlowingOverscrollIndicator, OverscrollDrag, OverscrollIndicatorNotification,
    ScrollNotification, StretchingOverscrollIndicator,
};
use crate::gestures::VelocityTracker;
use crate::physics::{Simulation, precisionErrorTolerance};
use crate::{
    Bounds, GestureTuning, Pixels, Point, ScrollMetrics, ScrollPhysics, ScrollSource, TouchPhase,
    point, px,
};

/// What a scroll container does past its edge when a touch or trackpad drags or flings it there. The physics
/// (`GestureTuning::scroll_physics`) decides whether it may go past: bouncing physics overscrolls and springs back,
/// clamping physics refuses, and the refused overscroll drives the indicator. Mouse wheels never overscroll, as in
/// Flutter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overscroll {
    /// The physics alone: iOS's and macOS's bounce, with `ScrollPhysics::Bouncing`.
    Bounce,
    /// Material 3's stretch (`StretchingOverscrollIndicator`).
    Stretch,
    /// Material 2's glow (`GlowingOverscrollIndicator`).
    Glow,
    /// A hard stop at the edge: GPUI's scrolling as it was, touch momentum included.
    None,
}

/// `ScrollActivity`: what moves one axis. To add one, add a variant: every match on it is exhaustive.
#[derive(Default)]
enum Activity {
    #[default]
    Idle,
    /// A touch or a trackpad drags the axis (`DragScrollActivity`; a touch that holds a moving axis starts here).
    Drag(DragInput),
    Ballistic {
        simulation: Box<dyn Simulation>,
        started: Instant,
    },
}

/// What drags an axis.
enum DragInput {
    /// A finger: the touch recognizer measures its release velocity.
    Touch,
    /// A trackpad's pan (Flutter's `PointerPanZoom` events, which its drag recognizer tracks): this axis measures
    /// the release velocity from the pan so far, `travelled` along the axis.
    Trackpad {
        tracker: Box<VelocityTracker>,
        travelled: f64,
    },
}

/// The indicator an axis shows, kept across drags so a recede finishes.
#[derive(Default)]
enum Indicator {
    #[default]
    Off,
    Stretch(StretchingOverscrollIndicator),
    Glow(GlowingOverscrollIndicator),
}

/// One axis of a scroll container as Flutter's `ScrollPosition`: `pixels` grows as the content moves up or left.
#[derive(Default)]
pub(crate) struct AxisPosition {
    activity: Activity,
    indicator: Indicator,
    /// The last layout's metrics: `pixels` is overwritten per call.
    metrics: Option<ScrollMetrics>,
    /// The container's size across this axis.
    cross_extent: f64,
    /// The scroll offset this axis wrote last. While it moves the axis, another value means someone else set the
    /// offset (a `ScrollHandle`, `scroll_to_bottom`): Flutter's `jumpTo`.
    written: Pixels,
}

/// One frame's layout of an axis.
pub(crate) struct AxisExtents {
    pub max_scroll_extent: f64,
    pub viewport: f64,
    /// The container's size across the axis.
    pub cross_extent: f64,
    pub scale: f64,
}

/// What drives a drag step.
#[derive(Clone, Copy)]
pub(crate) enum DragSource {
    /// A touch scroll step, with the finger's release velocity along the axis (`DragEndDetails.primaryVelocity`):
    /// zero unless it flings.
    Touch { velocity: f64 },
    /// A trackpad scroll step.
    Trackpad,
}

/// What one touch or trackpad scroll step tells an axis.
pub(crate) struct DragStep {
    pub phase: TouchPhase,
    /// The finger's movement along the axis.
    pub delta: f64,
    pub source: DragSource,
    /// Where the pan is across the axis, from the container's leading edge.
    pub cross_axis_offset: f64,
}

/// How a container takes a scroll event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScrollInput {
    /// A touch pan: it drags the axes.
    Touch,
    /// A trackpad pan (pixel deltas with phases): it drags the axes, as Flutter's `PointerPanZoom` events do.
    Trackpad,
    /// The platform's momentum after a trackpad pan: ignored, the axes fling themselves (Flutter's macOS embedder
    /// skips momentum events, "the framework will generate scroll momentum").
    Momentum,
    /// A wheel: it stops a drag or fling and moves within the extents (Flutter's `PointerScrollEvent`).
    Wheel,
}

impl AxisPosition {
    /// This frame: records the layout (`applyNewDimensions`: a new extent restarts the activity's ballistic with
    /// the new range), follows an offset set from outside, and advances the ballistic simulation and the indicator.
    pub(crate) fn layout(
        &mut self,
        extents: AxisExtents,
        offset: &mut Pixels,
        physics: ScrollPhysics,
        now: Instant,
    ) {
        let mut pixels = -offset.to_f64();
        self.follow_jump(*offset, pixels, physics, now);
        let metrics = ScrollMetrics {
            min_scroll_extent: 0.0,
            max_scroll_extent: extents.max_scroll_extent,
            pixels: 0.0,
            viewport_dimension: extents.viewport,
            device_pixel_ratio: extents.scale,
        };
        let resized = self.metrics.is_some_and(|old| {
            (old.max_scroll_extent, old.viewport_dimension)
                != (metrics.max_scroll_extent, metrics.viewport_dimension)
        });
        self.metrics = Some(metrics);
        self.cross_extent = extents.cross_extent;
        if resized {
            // ScrollActivity.applyNewDimensions per activity.
            match &self.activity {
                Activity::Idle => self.go_ballistic(0.0, pixels, physics, now),
                Activity::Drag(_) => {}
                Activity::Ballistic {
                    simulation,
                    started,
                } => {
                    let velocity =
                        simulation.dx(now.saturating_duration_since(*started).as_secs_f64());
                    self.go_ballistic(velocity, pixels, physics, now);
                }
            }
        }
        self.tick(&mut pixels, physics, now);
        self.write(offset, pixels);
    }

    /// Whether the physics, not the layout, owns `pixels`: while dragged or flung it may be out of range.
    pub(crate) fn is_moving(&self) -> bool {
        match self.activity {
            Activity::Idle => false,
            Activity::Drag(_) | Activity::Ballistic { .. } => true,
        }
    }

    /// Whether the next frame changes what this axis draws.
    pub(crate) fn is_animating(&self) -> bool {
        let indicator_visible = match &self.indicator {
            Indicator::Off => false,
            Indicator::Stretch(stretch) => stretch.stretch_strength() != 0.0,
            Indicator::Glow(glow) => {
                glow.leading.paint(1.0, 1.0).is_some() || glow.trailing.paint(1.0, 1.0).is_some()
            }
        };
        indicator_visible || matches!(self.activity, Activity::Ballistic { .. })
    }

    /// `StretchEffect.stretchStrength`: positive stretches from the leading edge, negative from the trailing one.
    pub(crate) fn stretch_strength(&self) -> f64 {
        match &self.indicator {
            Indicator::Stretch(stretch) => stretch.stretch_strength(),
            Indicator::Off | Indicator::Glow(_) => 0.0,
        }
    }

    pub(crate) fn glow(&self) -> Option<&GlowingOverscrollIndicator> {
        match &self.indicator {
            Indicator::Glow(glow) => Some(glow),
            Indicator::Off | Indicator::Stretch(_) => None,
        }
    }

    /// A wheel takes over: the drag or fling stops where it is.
    pub(crate) fn stop(&mut self, offset: Pixels, now: Instant) {
        self.go_idle(-offset.to_f64(), now);
    }

    /// One touch or trackpad scroll step: `state × phase → next`. Steps of GPUI's own touch fling (moves and ends
    /// after the drag ended) are ignored: this axis runs Flutter's ballistic simulation itself, with its extents.
    pub(crate) fn handle_drag(
        &mut self,
        step: DragStep,
        offset: &mut Pixels,
        tuning: &GestureTuning,
        overscroll: Overscroll,
        axis_direction: AxisDirection,
        now: Instant,
    ) {
        let physics = tuning.scroll_physics;
        let mut pixels = -offset.to_f64();
        self.follow_jump(*offset, pixels, physics, now);
        match (step.phase, &mut self.activity) {
            (
                TouchPhase::Started,
                Activity::Idle | Activity::Drag(_) | Activity::Ballistic { .. },
            ) => {
                // Scrollable.setCanDrag(shouldAcceptUserOffset): a vertical container is a primary scroll view,
                // always scrollable; any other drags only when it has a range or is off its start.
                let vertical = matches!(axis_direction, AxisDirection::Up | AxisDirection::Down);
                let accepts = self.metrics_at(pixels).is_some_and(|metrics| {
                    vertical
                        || metrics.pixels != 0.0
                        || metrics.min_scroll_extent != metrics.max_scroll_extent
                });
                if accepts {
                    self.indicator = match (overscroll, std::mem::take(&mut self.indicator)) {
                        (Overscroll::Stretch, Indicator::Stretch(stretch)) => {
                            Indicator::Stretch(stretch)
                        }
                        (Overscroll::Stretch, _) => {
                            Indicator::Stretch(StretchingOverscrollIndicator::new(axis_direction))
                        }
                        (Overscroll::Glow, Indicator::Glow(glow)) => Indicator::Glow(glow),
                        (Overscroll::Glow, _) => {
                            Indicator::Glow(GlowingOverscrollIndicator::default())
                        }
                        (Overscroll::Bounce | Overscroll::None, _) => Indicator::Off,
                    };
                    self.go_idle(pixels, now);
                    self.activity = Activity::Drag(match step.source {
                        DragSource::Touch { .. } => DragInput::Touch,
                        DragSource::Trackpad => {
                            let mut tracker =
                                Box::new(VelocityTracker::with_kind(tuning.velocity_tracker));
                            tracker.add_position(now, Point::default());
                            DragInput::Trackpad {
                                tracker,
                                travelled: 0.0,
                            }
                        }
                    });
                    self.notify(ScrollNotification::Start, pixels, now);
                    self.drag_by(&step, &mut pixels, physics, now);
                }
            }
            (TouchPhase::Moved, Activity::Drag(_)) => {
                self.drag_by(&step, &mut pixels, physics, now)
            }
            (TouchPhase::Ended, Activity::Drag(input)) => {
                let velocity = match (input, step.source) {
                    (DragInput::Trackpad { tracker, .. }, _) => {
                        let estimate = tracker.get_velocity_estimate(now);
                        let (speed, distance) = (estimate.pixels_per_second.x, estimate.offset.x);
                        // considerFling: a fling only past both of isFlingGesture's minimums, then clamped.
                        if tuning.is_fling_gesture(speed.abs(), distance.abs()) {
                            f64::from(
                                speed.clamp(-tuning.max_fling_velocity, tuning.max_fling_velocity),
                            )
                        } else {
                            0.0
                        }
                    }
                    (DragInput::Touch, DragSource::Touch { velocity }) => velocity,
                    (DragInput::Touch, DragSource::Trackpad) => 0.0,
                };
                self.drag_by(&step, &mut pixels, physics, now);
                self.notify(
                    ScrollNotification::End {
                        drag_velocity: Some(velocity),
                    },
                    pixels,
                    now,
                );
                // ScrollDragController.end: a downward drag scrolls the content up.
                self.go_ballistic(-velocity, pixels, physics, now);
            }
            (TouchPhase::Cancelled, Activity::Drag(_)) => {
                self.end(pixels, now);
                self.go_ballistic(0.0, pixels, physics, now);
            }
            (
                TouchPhase::Moved | TouchPhase::Ended | TouchPhase::Cancelled,
                Activity::Idle | Activity::Ballistic { .. },
            ) => {}
        }
        self.write(offset, pixels);
    }

    /// `jumpTo` when the offset is not the one this axis wrote while moving it: `goIdle`, then `goBallistic(0)` from
    /// where it was set.
    fn follow_jump(&mut self, offset: Pixels, pixels: f64, physics: ScrollPhysics, now: Instant) {
        if self.is_moving() && offset != self.written {
            self.go_idle(pixels, now);
            self.go_ballistic(0.0, pixels, physics, now);
        }
    }

    fn write(&mut self, offset: &mut Pixels, pixels: f64) {
        *offset = px(-pixels as f32);
        self.written = *offset;
    }

    /// The ballistic simulation's frame at `now` (`BallisticScrollActivity._tick`, then `_end` once it is done),
    /// and the indicator's.
    fn tick(&mut self, pixels: &mut f64, physics: ScrollPhysics, now: Instant) {
        if let Activity::Ballistic {
            simulation,
            started,
        } = &self.activity
        {
            let time = now.saturating_duration_since(*started).as_secs_f64();
            let (value, velocity, done) = (
                simulation.x(time),
                simulation.dx(time),
                simulation.is_done(time),
            );
            // applyMoveTo: a move the boundary refuses ends the fling.
            if self.set_pixels(value, velocity, None, pixels, physics, now) != 0.0 {
                self.activity = Activity::Idle;
                self.end(*pixels, now);
            } else if done {
                self.go_ballistic(0.0, *pixels, physics, now);
            }
        }
        match &mut self.indicator {
            Indicator::Off => {}
            Indicator::Stretch(stretch) => stretch.controller.tick(now),
            Indicator::Glow(glow) => glow.tick(now),
        }
    }

    /// `applyUserOffset`, after a trackpad drag's velocity tracker records the step.
    fn drag_by(&mut self, step: &DragStep, pixels: &mut f64, physics: ScrollPhysics, now: Instant) {
        if let Activity::Drag(DragInput::Trackpad { tracker, travelled }) = &mut self.activity {
            *travelled += step.delta;
            tracker.add_position(now, point(px(*travelled as f32), px(0.)));
        }
        if step.delta == 0.0 {
            return;
        }
        let Some(metrics) = self.metrics_at(*pixels) else {
            return;
        };
        let new_pixels = *pixels - physics.apply_physics_to_user_offset(&metrics, step.delta);
        let drag = OverscrollDrag {
            cross_axis_offset: step.cross_axis_offset,
            main_axis_extent: metrics.viewport_dimension,
            cross_axis_extent: self.cross_extent,
        };
        self.set_pixels(new_pixels, 0.0, Some(drag), pixels, physics, now);
    }

    /// `goIdle`: a drag or fling stops where it is.
    fn go_idle(&mut self, pixels: f64, now: Instant) {
        if self.is_moving() {
            self.activity = Activity::Idle;
            self.end(pixels, now);
        }
    }

    /// `goBallistic`: a simulation when the physics has one, else idle.
    fn go_ballistic(&mut self, velocity: f64, pixels: f64, physics: ScrollPhysics, now: Instant) {
        let simulation = self
            .metrics_at(pixels)
            .and_then(|metrics| physics.create_ballistic_simulation(&metrics, velocity));
        self.activity = match simulation {
            Some(simulation) => {
                self.notify(ScrollNotification::Start, pixels, now);
                Activity::Ballistic {
                    simulation,
                    started: now,
                }
            }
            None => {
                if matches!(self.activity, Activity::Ballistic { .. }) {
                    self.end(pixels, now);
                }
                Activity::Idle
            }
        };
    }

    /// The scroll ended without a drag's velocity.
    fn end(&mut self, pixels: f64, now: Instant) {
        self.notify(
            ScrollNotification::End {
                drag_velocity: None,
            },
            pixels,
            now,
        );
    }

    /// `setPixels`: moves to what the boundary allows of `new_pixels` and returns the overscroll it refused, sent to
    /// the indicator with the activity's `velocity` and the `drag` that caused it.
    fn set_pixels(
        &mut self,
        new_pixels: f64,
        velocity: f64,
        drag: Option<OverscrollDrag>,
        pixels: &mut f64,
        physics: ScrollPhysics,
        now: Instant,
    ) -> f64 {
        let Some(metrics) = self.metrics_at(*pixels) else {
            return 0.0;
        };
        if new_pixels == *pixels {
            return 0.0;
        }
        let overscroll = physics.apply_boundary_conditions(&metrics, new_pixels);
        let old_pixels = *pixels;
        *pixels = new_pixels - overscroll;
        if *pixels != old_pixels {
            let dragging = drag.is_some();
            self.notify(ScrollNotification::Update { dragging }, *pixels, now);
        }
        if overscroll.abs() > precisionErrorTolerance {
            self.notify(
                ScrollNotification::Overscroll {
                    overscroll,
                    velocity,
                    drag,
                },
                *pixels,
                now,
            );
            return overscroll;
        }
        0.0
    }

    fn metrics_at(&self, pixels: f64) -> Option<ScrollMetrics> {
        self.metrics
            .map(|metrics| ScrollMetrics { pixels, ..metrics })
    }

    fn notify(&mut self, notification: ScrollNotification, pixels: f64, now: Instant) {
        let Some(metrics) = self.metrics_at(pixels) else {
            return;
        };
        // OverscrollIndicatorNotification has no receiver in GPUI: every indicator is allowed.
        let mut dispatch = |_: &mut OverscrollIndicatorNotification| {};
        match &mut self.indicator {
            Indicator::Off => {}
            Indicator::Stretch(stretch) => {
                stretch.handle_scroll_notification(notification, &metrics, now, &mut dispatch)
            }
            Indicator::Glow(glow) => {
                glow.handle_scroll_notification(notification, &metrics, now, &mut dispatch)
            }
        }
    }
}

/// Both axes of one scroll container.
#[derive(Default)]
pub(crate) struct OverscrollPosition {
    pub x: AxisPosition,
    pub y: AxisPosition,
}

/// Material 2's default light theme's `colorScheme.secondary` (`Colors.blue`), the colour
/// `MaterialScrollBehavior` gives the glow; GPUI has no theme.
pub(crate) const GLOW_COLOR: u32 = 0x2196F3;

/// Segments of the glow's circle outline.
const GLOW_SEGMENTS: usize = 64;

/// One glow to draw: its circle, as an outline, clipped to `clip`, in [`GLOW_COLOR`] at `opacity`.
pub(crate) struct GlowShape {
    pub clip: Bounds<Pixels>,
    pub outline: Vec<Point<Pixels>>,
    pub opacity: f32,
}

impl OverscrollPosition {
    /// Whether a touch landing on this container holds it: an axis is moving (`hold` on a drag, fling or spring
    /// back). The touch is then a drag from its first pixel, and the container's children get no tap from it
    /// (`BallisticScrollActivity.shouldIgnorePointer`).
    pub(crate) fn holds_touch(&self) -> bool {
        self.x.is_moving() || self.y.is_moving()
    }

    /// How this container takes a scroll event: `precise` is a pixel delta, `phase` its platform phase (a
    /// trackpad's start, move and end; a wheel's are all moves).
    pub(crate) fn scroll_input(
        &self,
        source: ScrollSource,
        phase: TouchPhase,
        precise: bool,
    ) -> ScrollInput {
        let axes = [&self.x, &self.y];
        let trackpad_drag = axes
            .iter()
            .any(|axis| matches!(axis.activity, Activity::Drag(DragInput::Trackpad { .. })));
        let ballistic = axes
            .iter()
            .any(|axis| matches!(axis.activity, Activity::Ballistic { .. }));
        match (source, precise, phase) {
            (ScrollSource::Touch, _, _) => ScrollInput::Touch,
            (ScrollSource::Pointer, true, TouchPhase::Started) => ScrollInput::Trackpad,
            (ScrollSource::Pointer, true, _) if trackpad_drag => ScrollInput::Trackpad,
            (ScrollSource::Pointer, true, TouchPhase::Moved) if ballistic => ScrollInput::Momentum,
            (ScrollSource::Pointer, _, _) => ScrollInput::Wheel,
        }
    }

    /// The glows to draw over a container at `bounds`, as `_GlowingOverscrollIndicatorPainter` does: each
    /// controller paints its edge as if it were the top, mirrored for a trailing edge and transposed for a
    /// horizontal axis. To add an edge, add a row.
    pub(crate) fn glow_shapes(&self, bounds: Bounds<Pixels>) -> Vec<GlowShape> {
        // (glow, transposed: horizontal axis, mirrored: trailing edge)
        let edges = [
            (self.y.glow().map(|glow| &glow.leading), false, false),
            (self.y.glow().map(|glow| &glow.trailing), false, true),
            (self.x.glow().map(|glow| &glow.leading), true, false),
            (self.x.glow().map(|glow| &glow.trailing), true, true),
        ];
        let (left, top) = (bounds.left().to_f64(), bounds.top().to_f64());
        let (width, height) = (bounds.size.width.to_f64(), bounds.size.height.to_f64());
        edges
            .into_iter()
            .filter_map(|(glow, transposed, mirrored)| {
                let (cross, main) = if transposed {
                    (height, width)
                } else {
                    (width, height)
                };
                let paint = glow?.paint(cross, main)?;
                // canvas.translate(0, translate_y); canvas.scale(1, scale_y), then the edge's flip.
                let map = |u: f64, v: f64| {
                    let v = paint.translate_y + v * paint.scale_y;
                    let v = if mirrored { main - v } else { v };
                    let (x, y) = if transposed {
                        (left + v, top + u)
                    } else {
                        (left + u, top + v)
                    };
                    point(px(x as f32), px(y as f32))
                };
                let (corner, opposite) = (map(0.0, 0.0), map(cross, paint.clip_height));
                let clip = Bounds::from_corners(corner.min(&opposite), corner.max(&opposite));
                let outline = (0..GLOW_SEGMENTS)
                    .map(|segment| {
                        let angle = std::f64::consts::TAU * segment as f64 / GLOW_SEGMENTS as f64;
                        map(
                            paint.center.0 + paint.radius * angle.cos(),
                            paint.center.1 + paint.radius * angle.sin(),
                        )
                    })
                    .collect();
                Some(GlowShape {
                    clip,
                    outline,
                    opacity: paint.opacity as f32,
                })
            })
            .collect()
    }
}

impl std::fmt::Debug for OverscrollPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverscrollPosition")
            .field("x_moving", &self.x.is_moving())
            .field("y_moving", &self.y.is_moving())
            .finish()
    }
}

/// How far the stretch moves a child centred at `center` along an axis of a viewport from `start` to `end`:
/// `StretchEffect`'s fallback scales the content by `1 + |strength|` about the leading edge (positive strength) or
/// the trailing one. GPUI has no scale transform for every primitive, so children move as the scale would move their
/// centres; their sizes stay.
pub(crate) fn stretch_displacement(strength: f64, center: f64, start: f64, end: f64) -> f64 {
    let anchor = if strength > 0.0 { start } else { end };
    (center - anchor) * strength.abs()
}

#[cfg(test)]
#[path = "scroll_position_tests.rs"]
mod tests;
