// Ported from flutter/flutter packages/flutter/lib/src/widgets/overscroll_indicator.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7): `_GlowController` and `_StretchController`, the scroll notification
// handling of `_GlowingOverscrollIndicatorState` and `_StretchingOverscrollIndicatorState`, and
// `OverscrollIndicatorNotification`. Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.
//
// Dropped: the widgets, painters and `StretchEffect` (drawing is the caller's; `GlowController::paint` and
// `StretchingOverscrollIndicator::stretch_strength` give what they drew from), `notificationPredicate` and the
// axis check (the caller routes only its own scrollable's notifications), colours, `showLeading`/`showTrailing`,
// clip behaviour, listeners and `dispose`. Flutter's `AnimationController`, `Ticker` and `Timer` become explicit
// state advanced by `tick(now)`: call it once per frame, with the frame time. An animation starts at the `now` it
// was started with (Flutter's ticker starts on the next frame).

use std::time::Duration;

use scheduler::Instant;

use crate::physics::{Simulation, SpringSimulation, Tolerance, near_equal};
use crate::{ScrollMetrics, SpringConfig};

/// Flutter's `AxisDirection` (painting/basic_types.dart): a direction along
/// either the horizontal or vertical axis, in which a scrollable's offset grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisDirection {
    /// Zero is at the bottom and positive values are above it.
    Up,
    /// Zero is on the left and positive values are to the right of it.
    Right,
    /// Zero is at the top and positive values are below it.
    Down,
    /// Zero is to the right and positive values are to the left of it.
    Left,
}

impl AxisDirection {
    /// `axisDirectionIsReversed`: whether this direction is opposite of the
    /// reading direction: up and left.
    pub fn is_reversed(self) -> bool {
        match self {
            Self::Up | Self::Left => true,
            Self::Down | Self::Right => false,
        }
    }
}

/// Flutter's `clampDouble`: like `f64::clamp`, but never panics (NaN gives `max`, and `min` wins over `max`).
fn clamp_double(x: f64, min: f64, max: f64) -> f64 {
    if x < min {
        return min;
    }
    if x > max {
        return max;
    }
    if x.is_nan() {
        return max;
    }
    x
}

/// A notification that either a [`GlowingOverscrollIndicator`] or a [`StretchingOverscrollIndicator`] will start
/// showing an overscroll indication. The caller dispatches it to the scrollable's ancestors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverscrollIndicatorNotification {
    /// Whether the indication will be shown on the leading edge of the scroll view.
    pub leading: bool,
    /// Controls at which offset a [`GlowingOverscrollIndicator`] draws.
    ///
    /// A positive offset will move the glow away from its edge, i.e. for a vertical, leading indicator, a
    /// `paint_offset` of 100.0 will be drawn starting at 100.0 from the top of the underlying scrollable.
    ///
    /// Has no effect on a [`StretchingOverscrollIndicator`].
    pub paint_offset: f64,
    /// Whether the current overscroll event will allow for the indicator to be shown. Calling
    /// [`Self::disallow_indicator`] sets this to false, preventing the over scroll indicator from showing.
    pub accepted: bool,
}

impl OverscrollIndicatorNotification {
    fn new(leading: bool) -> Self {
        Self {
            leading,
            paint_offset: 0.0,
            accepted: true,
        }
    }

    /// Call this method if the overscroll indicator should be prevented.
    pub fn disallow_indicator(&mut self) {
        self.accepted = false;
    }
}

/// The scroll notifications the indicators read (Flutter's `ScrollNotification` subclasses), with the metrics of the
/// scrollable that sent them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollNotification {
    /// `ScrollStartNotification`: a scrollable has started scrolling.
    Start,
    /// `ScrollUpdateNotification`: the scroll position changed; `dragging` when a drag moved it.
    Update {
        /// Whether `dragDetails` is set.
        dragging: bool,
    },
    /// `OverscrollNotification`: a scrollable refused to move by `overscroll` (negative past the leading edge).
    Overscroll {
        /// The number of logical pixels that the scrollable avoided scrolling.
        overscroll: f64,
        /// The velocity at which the scroll position was changing when this overscroll occurred, or zero for a
        /// drag.
        velocity: f64,
        /// The drag that caused it, if one did.
        drag: Option<OverscrollDrag>,
    },
    /// `ScrollEndNotification`: a scrollable has stopped scrolling.
    End {
        /// The velocity along the axis, in screen coordinates (`dragDetails.velocity`), when a drag ended it.
        drag_velocity: Option<f64>,
    },
}

/// Where an overscrolling drag is, from its `dragDetails.globalPosition` and the scrollable's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverscrollDrag {
    /// The drag's position across the scroll axis, from the scrollable's left (or top, for a horizontal one).
    pub cross_axis_offset: f64,
    /// The scrollable's size along the scroll axis.
    pub main_axis_extent: f64,
    /// The scrollable's size across the scroll axis.
    pub cross_axis_extent: f64,
}

// The Glow logic is a port of the logic in the following file:
// https://android.googlesource.com/platform/frameworks/base/+/main/core/java/android/widget/EdgeEffect.java
// as of December 2016.

/// `_GlowState`, with the pull's recede timer (`_pullRecedeTimer`), which only runs while pulling.
#[derive(Clone, Copy, Debug, PartialEq)]
enum GlowState {
    Idle,
    Absorb,
    Pull { recede_at: Instant },
    Recede,
}

/// `_glowController` (an `AnimationController` run forward from 0.0) as of the last tick.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GlowAnimation {
    value: f64,
    /// When the running forward animation started, and its duration; `None` once completed (or never started).
    running: Option<(Instant, Duration)>,
}

/// `_displacementTicker` and `_displacementTickerLastElapsed`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DisplacementTicker {
    Stopped,
    Ticking { last: Option<Instant> },
}

/// One `Tween<double>`: `begin` to `end`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Tween {
    begin: f64,
    end: f64,
}

impl Tween {
    fn transform(&self, t: f64) -> f64 {
        self.begin + (self.end - self.begin) * t
    }
}

/// What [`GlowController::paint`] draws: a circle of `radius` at `center`, clipped to `(0, 0, width, clip_height)`,
/// after translating by `translate_y` and scaling by `(1, scale_y)`, in the glow's colour at `opacity`. The leading
/// edge is at the top; the caller rotates it to the scrollable's edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowPaint {
    /// The colour's opacity.
    pub opacity: f64,
    /// The canvas translation along y.
    pub translate_y: f64,
    /// The canvas scale along y.
    pub scale_y: f64,
    /// The clip rectangle's height.
    pub clip_height: f64,
    /// The circle's centre.
    pub center: (f64, f64),
    /// The circle's radius.
    pub radius: f64,
}

/// One edge's glow: Flutter's `_GlowController`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowController {
    // animation of the main axis direction
    state: GlowState,
    glow_controller: GlowAnimation,
    paint_offset: f64,
    paint_offset_scroll_pixels: f64,

    // animation values
    glow_opacity_tween: Tween,
    glow_size_tween: Tween,

    // animation of the cross axis position
    displacement_ticker: DisplacementTicker,
    displacement_target: f64,
    displacement: f64,

    // tracking the pull distance
    pull_distance: f64,
}

impl Default for GlowController {
    fn default() -> Self {
        Self {
            state: GlowState::Idle,
            glow_controller: GlowAnimation {
                value: 0.0,
                running: None,
            },
            paint_offset: 0.0,
            paint_offset_scroll_pixels: 0.0,
            glow_opacity_tween: Tween::default(),
            glow_size_tween: Tween::default(),
            displacement_ticker: DisplacementTicker::Stopped,
            displacement_target: 0.5,
            displacement: 0.5,
            pull_distance: 0.0,
        }
    }
}

impl GlowController {
    const RECEDE_TIME: Duration = Duration::from_millis(600);
    const PULL_TIME: Duration = Duration::from_millis(167);
    const PULL_HOLD_TIME: Duration = Duration::from_millis(167);
    const PULL_DECAY_TIME: Duration = Duration::from_millis(2000);
    // (Duration.microsecondsPerSecond / 60.0).round()
    const CROSS_AXIS_HALF_TIME: Duration = Duration::from_micros(16667);

    const MAX_OPACITY: f64 = 0.5;
    const PULL_OPACITY_GLOW_FACTOR: f64 = 0.8;
    const VELOCITY_GLOW_FACTOR: f64 = 0.00006;
    const SQRT3: f64 = 1.73205080757; // const math.sqrt(3)
    const WIDTH_TO_HEIGHT_FACTOR: f64 = (3.0 / 4.0) * (2.0 - Self::SQRT3);

    // absorbed velocities are clamped to the range MIN_VELOCITY..MAX_VELOCITY
    const MIN_VELOCITY: f64 = 100.0; // logical pixels per second
    const MAX_VELOCITY: f64 = 10000.0; // logical pixels per second

    /// `_decelerator` (`Curves.decelerate`) over the controller's value.
    fn decelerator(&self) -> f64 {
        let t = 1.0 - self.glow_controller.value;
        1.0 - t * t
    }

    fn glow_opacity(&self) -> f64 {
        self.glow_opacity_tween.transform(self.decelerator())
    }

    fn glow_size(&self) -> f64 {
        self.glow_size_tween.transform(self.decelerator())
    }

    fn forward_from_zero(&mut self, duration: Duration, now: Instant) {
        self.glow_controller = GlowAnimation {
            value: 0.0,
            running: Some((now, duration)),
        };
    }

    /// Handle a scroll slamming into the edge at a particular velocity.
    ///
    /// The velocity must be positive.
    pub fn absorb_impact(&mut self, velocity: f64, now: Instant) {
        let velocity = clamp_double(velocity, Self::MIN_VELOCITY, Self::MAX_VELOCITY);
        self.glow_opacity_tween.begin = if self.state == GlowState::Idle {
            0.3
        } else {
            self.glow_opacity()
        };
        self.glow_opacity_tween.end = clamp_double(
            velocity * Self::VELOCITY_GLOW_FACTOR,
            self.glow_opacity_tween.begin,
            Self::MAX_OPACITY,
        );
        self.glow_size_tween.begin = self.glow_size();
        self.glow_size_tween.end = (0.025 + 7.5e-7 * velocity * velocity).min(1.0);
        let duration = Duration::from_millis((0.15 + velocity * 0.02).round() as u64);
        self.forward_from_zero(duration, now);
        self.displacement = 0.5;
        self.state = GlowState::Absorb;
    }

    /// Handle a user-driven overscroll.
    ///
    /// The `overscroll` argument should be the scroll distance in logical pixels,
    /// the `extent` argument should be the total dimension of the viewport in the
    /// main axis in logical pixels, the `cross_axis_offset` argument should be the
    /// distance from the leading (left or top) edge of the cross axis of the
    /// viewport, and the `cross_extent` should be the size of the cross axis. For
    /// example, a pull of 50 pixels up the middle of a 200 pixel high and 100
    /// pixel wide vertical viewport should result in a call of `pull(50.0, 200.0,
    /// 50.0, 100.0)`. The `overscroll` value should be positive regardless of the
    /// direction.
    pub fn pull(
        &mut self,
        overscroll: f64,
        extent: f64,
        cross_axis_offset: f64,
        cross_extent: f64,
        now: Instant,
    ) {
        self.pull_distance += overscroll / 200.0; // This factor is magic. Not clear why we need it to match Android.
        self.glow_opacity_tween.begin = self.glow_opacity();
        self.glow_opacity_tween.end = (self.glow_opacity()
            + overscroll / extent * Self::PULL_OPACITY_GLOW_FACTOR)
            .min(Self::MAX_OPACITY);
        let height = extent.min(cross_extent * Self::WIDTH_TO_HEIGHT_FACTOR);
        self.glow_size_tween.begin = self.glow_size();
        self.glow_size_tween.end =
            (1.0 - 1.0 / (0.7 * (self.pull_distance * height).sqrt())).max(self.glow_size());
        self.displacement_target = cross_axis_offset / cross_extent;
        if self.displacement_target != self.displacement {
            if self.displacement_ticker == DisplacementTicker::Stopped {
                self.displacement_ticker = DisplacementTicker::Ticking { last: None };
            }
        } else {
            self.displacement_ticker = DisplacementTicker::Stopped;
        }
        if !matches!(self.state, GlowState::Pull { .. }) {
            self.forward_from_zero(Self::PULL_TIME, now);
        }
        self.state = GlowState::Pull {
            recede_at: now + Self::PULL_HOLD_TIME,
        };
    }

    /// The scroll that pulled the glow ended.
    pub fn scroll_end(&mut self, now: Instant) {
        if matches!(self.state, GlowState::Pull { .. }) {
            self.recede(Self::RECEDE_TIME, now);
        }
    }

    /// `_changePhase`, when the glow animation completes.
    fn change_phase(&mut self, now: Instant) {
        match self.state {
            GlowState::Absorb => self.recede(Self::RECEDE_TIME, now),
            GlowState::Recede => {
                self.state = GlowState::Idle;
                self.pull_distance = 0.0;
            }
            GlowState::Pull { .. } | GlowState::Idle => {}
        }
    }

    fn recede(&mut self, duration: Duration, now: Instant) {
        if matches!(self.state, GlowState::Recede | GlowState::Idle) {
            return;
        }
        self.glow_opacity_tween.begin = self.glow_opacity();
        self.glow_opacity_tween.end = 0.0;
        self.glow_size_tween.begin = self.glow_size();
        self.glow_size_tween.end = 0.0;
        self.forward_from_zero(duration, now);
        self.state = GlowState::Recede;
    }

    /// Advances to the frame at `now`: the glow animation and its phase change, the pull's recede timer, and the
    /// cross-axis displacement (`_tickDisplacement`).
    pub fn tick(&mut self, now: Instant) {
        if let GlowState::Pull { recede_at } = self.state
            && now >= recede_at
        {
            self.recede(Self::PULL_DECAY_TIME, recede_at);
        }
        if let Some((started, duration)) = self.glow_controller.running {
            let elapsed = now.saturating_duration_since(started);
            self.glow_controller.value = (elapsed.as_secs_f64() / duration.as_secs_f64()).min(1.0);
            // _InterpolationSimulation.isDone: past the duration.
            if elapsed > duration {
                self.glow_controller.running = None;
                self.change_phase(now);
            }
        }
        if let DisplacementTicker::Ticking { last } = self.displacement_ticker {
            if let Some(last) = last {
                let t = now.saturating_duration_since(last).as_micros() as f64;
                self.displacement = self.displacement_target
                    - (self.displacement_target - self.displacement)
                        * 2f64.powf(-t / Self::CROSS_AXIS_HALF_TIME.as_micros() as f64);
            }
            self.displacement_ticker = if near_equal(
                self.displacement_target,
                self.displacement,
                Tolerance::DEFAULT_TOLERANCE.distance,
            ) {
                DisplacementTicker::Stopped
            } else {
                DisplacementTicker::Ticking { last: Some(now) }
            };
        }
    }

    /// The glow to draw in a box of `width` × `height` (the leading edge at the top), or `None` when it is invisible.
    pub fn paint(&self, width: f64, height: f64) -> Option<GlowPaint> {
        let opacity = self.glow_opacity();
        if opacity == 0.0 {
            return None;
        }
        let base_glow_scale = if width > height { height / width } else { 1.0 };
        let radius = width * 3.0 / 2.0;
        let clip_height = height.min(width * Self::WIDTH_TO_HEIGHT_FACTOR);
        let scale_y = self.glow_size() * base_glow_scale;
        let center = (
            (width / 2.0) * (0.5 + self.displacement),
            clip_height - radius,
        );
        Some(GlowPaint {
            opacity,
            translate_y: self.paint_offset + self.paint_offset_scroll_pixels,
            scale_y,
            clip_height,
            center,
            radius,
        })
    }
}

/// The state of a `GlowingOverscrollIndicator`: one [`GlowController`] per edge, fed by scroll notifications.
///
/// Flutter's `_accepted` map is gone: every overscroll is confirmed (see below), so it only ever held the
/// confirmation just made.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlowingOverscrollIndicator {
    /// The glow at the leading edge.
    pub leading: GlowController,
    /// The glow at the trailing edge.
    pub trailing: GlowController,
}

impl GlowingOverscrollIndicator {
    /// `_handleScrollNotification`. `dispatch` sends the [`OverscrollIndicatorNotification`] to the scrollable's
    /// ancestors, which may disallow the indicator or set its paint offset.
    pub fn handle_scroll_notification(
        &mut self,
        notification: ScrollNotification,
        metrics: &ScrollMetrics,
        now: Instant,
        dispatch: &mut dyn FnMut(&mut OverscrollIndicatorNotification),
    ) {
        // Update the paint offset with the current scroll position. This makes
        // sure that the glow effect correctly scrolls in line with the current
        // scroll, e.g. when scrolling in the opposite direction again to hide
        // the glow. Otherwise, the glow would always stay in a fixed position,
        // even if the top of the content already scrolled away.
        self.leading.paint_offset_scroll_pixels =
            -(metrics.pixels - metrics.min_scroll_extent).min(self.leading.paint_offset);
        self.trailing.paint_offset_scroll_pixels =
            -(metrics.max_scroll_extent - metrics.pixels).min(self.trailing.paint_offset);

        match notification {
            ScrollNotification::Overscroll {
                overscroll,
                velocity,
                drag,
            } => {
                // Flutter asserts the overscroll is not zero.
                if overscroll == 0.0 {
                    return;
                }
                let is_leading = overscroll < 0.0;
                // Flutter only confirms after a notification of another type, but compares the last notification's
                // `runtimeType` (a `Type`) with `is!`, which always holds: every overscroll is confirmed.
                let mut confirmation = OverscrollIndicatorNotification::new(is_leading);
                dispatch(&mut confirmation);
                let controller = if is_leading {
                    &mut self.leading
                } else {
                    &mut self.trailing
                };
                if confirmation.accepted {
                    controller.paint_offset = confirmation.paint_offset;
                    if velocity != 0.0 {
                        controller.absorb_impact(velocity.abs(), now);
                    } else if let Some(drag) = drag {
                        controller.pull(
                            overscroll.abs(),
                            drag.main_axis_extent,
                            clamp_double(drag.cross_axis_offset, 0.0, drag.cross_axis_extent),
                            drag.cross_axis_extent,
                            now,
                        );
                    }
                }
            }
            ScrollNotification::End {
                drag_velocity: Some(_),
            }
            | ScrollNotification::Update { dragging: true } => {
                self.leading.scroll_end(now);
                self.trailing.scroll_end(now);
            }
            ScrollNotification::Start
            | ScrollNotification::Update { dragging: false }
            | ScrollNotification::End {
                drag_velocity: None,
            } => {}
        }
    }

    /// Advances both glows to the frame at `now`.
    pub fn tick(&mut self, now: Instant) {
        self.leading.tick(now);
        self.trailing.tick(now);
    }
}

/// `_StretchController`'s animation: an unbounded `AnimationController` running a spring.
#[derive(Clone, Copy, Debug)]
enum StretchAnimation {
    Idle,
    Running {
        simulation: SpringSimulation,
        started: Instant,
        /// The controller's value as of the last tick.
        value: f64,
    },
}

/// The stretch of a `StretchingOverscrollIndicator`: Flutter's `_StretchController`.
#[derive(Clone, Copy, Debug)]
pub struct StretchController {
    controller: StretchAnimation,
    overscroll: f64,
    /// Stores the `overscroll` value from an ongoing animation at the precise
    /// moment it is interrupted by a new `pull()` gesture.
    ///
    /// This captured value is then added to the overscroll amount calculated
    /// by the `pull()` method, to create a smoother visual transition from an
    /// animated overscroll state to a direct, user-driven pull.
    ///
    /// It is reset to `0.0` when an animation completes naturally via `animate()`.
    interrupted_overscroll: f64,
}

impl Default for StretchController {
    fn default() -> Self {
        Self {
            controller: StretchAnimation::Idle,
            overscroll: 0.0,
            interrupted_overscroll: 0.0,
        }
    }
}

impl StretchController {
    // Constants from Android.
    const EXPONENTIAL_SCALAR: f64 = std::f64::consts::E / 0.33;
    const STRETCH_INTENSITY: f64 = 0.016;

    /// The smallest overscroll value.
    pub const MIN_OVERSCROLL: f64 = -1.0;
    /// The largest overscroll value.
    pub const MAX_OVERSCROLL: f64 = 1.0;

    /// A fraction used to adjust the input velocity, measured in pixels,
    /// to a value between -1 and 1 when gesture fling.
    const FLING_VELOCITY_FRICTION: f64 = 1.0 / 6000.0;

    /// A fraction used to scale the absorbed impact velocity,
    /// converting raw velocity into a normalized value for simulation.
    const ABSORB_IMPACT_VELOCITY_FRICTION: f64 = 1.0 / 3000.0;

    /// The maximum velocity allowed for a fling after scaling
    /// to prevent applying an excessive stretch effect.
    const MAX_FLING_VELOCITY: f64 = 0.5;

    /// The maximum velocity allowed when absorbing an impact,
    /// ensuring the stretch effect does not exceed a reasonable limit.
    const MAX_ABSORB_IMPACT_VELOCITY: f64 = 1.25;

    // Physical constants ported directly from Android's EdgeEffect.java.
    const NATURAL_FREQUENCY: f64 = 24.657;
    const DAMPING_RATIO: f64 = 0.98;

    /// A correction factor applied to the simulation time.
    ///
    /// The physical constants `NATURAL_FREQUENCY` and `DAMPING_RATIO` were ported directly from Android's
    /// `EdgeEffect.java` source. However, using these constants as-is resulted in an animation that was noticeably
    /// faster than the native Android behavior. This factor, determined by visual comparison ("eyeballing") to
    /// match the platform's timing, slows down the simulation.
    const TIME_CORRECTION_FACTOR: f64 = 0.8;

    /// Stiffness coefficient for the spring, derived from the natural frequency.
    const STIFFNESS: f64 = Self::NATURAL_FREQUENCY * Self::NATURAL_FREQUENCY;

    /// The overscroll, from [`Self::MIN_OVERSCROLL`] to [`Self::MAX_OVERSCROLL`].
    pub fn overscroll(&self) -> f64 {
        self.overscroll
    }

    fn set_overscroll(&mut self, new_value: f64) {
        self.overscroll = clamp_double(new_value, Self::MIN_OVERSCROLL, Self::MAX_OVERSCROLL);
    }

    /// Creates a stretching-only [`Simulation`] ported from Android 12.
    fn create_stretch_simulation(&self, velocity: f64) -> SpringSimulation {
        // _kStretchSpringDescription: a mass of 1, a stiffness adjusted by the time correction factor squared to
        // match platform timing, and a damping ratio corresponding to Android's native `EdgeEffect`.
        let spring = SpringConfig::with_damping_ratio(
            1.0,
            (Self::STIFFNESS * Self::TIME_CORRECTION_FACTOR * Self::TIME_CORRECTION_FACTOR) as f32,
            Self::DAMPING_RATIO as f32,
        );
        SpringSimulation::new(
            spring,
            self.overscroll,
            0.0,
            velocity * Self::TIME_CORRECTION_FACTOR,
            Tolerance::DEFAULT_TOLERANCE,
        )
    }

    /// Handle a fling to the edge of the viewport at a particular velocity.
    pub fn absorb_impact(&mut self, velocity: f64, now: Instant) {
        if velocity == 0.0 {
            return;
        }
        let scaled_velocity = clamp_double(
            velocity * Self::ABSORB_IMPACT_VELOCITY_FRICTION,
            -Self::MAX_ABSORB_IMPACT_VELOCITY,
            Self::MAX_ABSORB_IMPACT_VELOCITY,
        );
        self.animate(self.create_stretch_simulation(scaled_velocity), now);
    }

    /// Called when the overscroll ends to trigger a fling animation if needed.
    pub fn scroll_end(&mut self, velocity: f64, now: Instant) {
        if velocity == 0.0 && self.overscroll == 0.0 {
            return;
        }
        let scaled_velocity = clamp_double(
            -(velocity * Self::FLING_VELOCITY_FRICTION),
            -Self::MAX_FLING_VELOCITY,
            Self::MAX_FLING_VELOCITY,
        );
        if let StretchAnimation::Idle = self.controller {
            self.animate(self.create_stretch_simulation(scaled_velocity), now);
        }
    }

    /// Starts a new animation using the given `simulation`, replacing any running one.
    fn animate(&mut self, simulation: SpringSimulation, now: Instant) {
        self.controller = StretchAnimation::Running {
            simulation,
            started: now,
            value: simulation.x(0.0),
        };
    }

    /// Handle a user-driven overscroll.
    ///
    /// The `normalized_overscroll` argument should be the scroll distance in
    /// logical pixels, divided by the extent of the viewport in the main axis.
    pub fn pull(&mut self, normalized_overscroll: f64) {
        if let StretchAnimation::Running { value, .. } = self.controller {
            self.interrupted_overscroll = value;
            self.controller = StretchAnimation::Idle;
        }

        let pull_distance = normalized_overscroll;
        let abs_distance = pull_distance.abs();
        let linear_intensity = Self::STRETCH_INTENSITY * abs_distance;
        let exponential_intensity =
            Self::STRETCH_INTENSITY * (1.0 - (-abs_distance * Self::EXPONENTIAL_SCALAR).exp());

        // Maintain sign of overscroll for direction.
        let direction_sign = pull_distance.signum();
        let new_overscroll = direction_sign * (linear_intensity + exponential_intensity);
        self.set_overscroll(new_overscroll + self.interrupted_overscroll);
    }

    /// Advances the animation to the frame at `now`; a finished one resets the overscroll to zero.
    pub fn tick(&mut self, now: Instant) {
        let StretchAnimation::Running {
            simulation,
            started,
            ..
        } = self.controller
        else {
            return;
        };
        let time = now.saturating_duration_since(started).as_secs_f64();
        let value = simulation.x(time);
        self.set_overscroll(value);
        self.controller = if simulation.is_done(time) {
            self.overscroll = 0.0;
            self.interrupted_overscroll = 0.0;
            StretchAnimation::Idle
        } else {
            StretchAnimation::Running {
                simulation,
                started,
                value,
            }
        };
    }
}

/// The state of a `StretchingOverscrollIndicator`, fed by scroll notifications.
#[derive(Clone, Copy, Debug)]
pub struct StretchingOverscrollIndicator {
    /// The stretch.
    pub controller: StretchController,
    /// The scrollable's axis direction; up and left reverse its stretch and drag velocities.
    pub axis_direction: AxisDirection,
    total_overscroll: f64,
    accepted: bool,
}

impl StretchingOverscrollIndicator {
    /// An indicator for a scrollable whose offset grows in `axis_direction`.
    pub fn new(axis_direction: AxisDirection) -> Self {
        Self {
            controller: StretchController::default(),
            axis_direction,
            total_overscroll: 0.0,
            accepted: true,
        }
    }

    /// `_handleScrollNotification`. `dispatch` sends the [`OverscrollIndicatorNotification`] to the scrollable's
    /// ancestors, which may disallow the indicator.
    pub fn handle_scroll_notification(
        &mut self,
        notification: ScrollNotification,
        metrics: &ScrollMetrics,
        now: Instant,
        dispatch: &mut dyn FnMut(&mut OverscrollIndicatorNotification),
    ) {
        match notification {
            ScrollNotification::Start => {
                self.accepted = true;
                self.total_overscroll = 0.0;
            }
            ScrollNotification::Overscroll {
                overscroll,
                velocity,
                drag,
            } => {
                // As in the glow's handler, Flutter's check of the last notification's `runtimeType` always holds.
                let mut confirmation = OverscrollIndicatorNotification::new(overscroll < 0.0);
                dispatch(&mut confirmation);
                self.accepted = confirmation.accepted;

                if self.accepted {
                    self.total_overscroll += overscroll;

                    if velocity != 0.0 {
                        self.controller.absorb_impact(velocity, now);
                    } else if drag.is_some() {
                        // We clamp the overscroll amount relative to the length of the viewport,
                        // which is the furthest distance a single pointer could pull on the
                        // screen. This is because more than one pointer will multiply the
                        // amount of overscroll - https://github.com/flutter/flutter/issues/11884
                        let distance_for_pull = self.total_overscroll / metrics.viewport_dimension;
                        let clamped_overscroll = clamp_double(distance_for_pull, -1.0, 1.0);
                        self.controller.pull(clamped_overscroll);
                    }
                }
            }
            ScrollNotification::End { drag_velocity } => {
                // Reverse axis directions report dragDetails velocity in
                // the opposite screen coordinate, so the value must be inverted.
                let velocity = drag_velocity.unwrap_or(0.0);
                let velocity = if self.axis_direction.is_reversed() {
                    -velocity
                } else {
                    velocity
                };

                // Since the overscrolling ended, we reset the total overscroll amount.
                self.total_overscroll = 0.0;
                if self.accepted {
                    self.controller.scroll_end(velocity, now);
                }
            }
            ScrollNotification::Update { .. } => {
                self.total_overscroll = 0.0;
                self.controller.scroll_end(0.0, now);
            }
        }
    }

    /// The `stretchStrength` Flutter's `StretchEffect` draws with: the overscroll, negated, and negated again for
    /// an up or left axis direction.
    pub fn stretch_strength(&self) -> f64 {
        let overscroll = -self.controller.overscroll();
        // Adjust overscroll for reverse scroll directions.
        if self.axis_direction.is_reversed() {
            -overscroll
        } else {
            overscroll
        }
    }
}

#[cfg(test)]
#[path = "overscroll_indicator_tests.rs"]
mod tests;
