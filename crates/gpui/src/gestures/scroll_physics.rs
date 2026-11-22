// Ported from flutter/flutter packages/flutter/lib/src/widgets/scroll_physics.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7): `ScrollDecelerationRate`, `ScrollPhysics`' fling limits, spring,
// drag threshold and `toleranceFor`, and its `BouncingScrollPhysics` and `ClampingScrollPhysics` subclasses.
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//
// Flutter's class and its two subclasses are one enum, a variant per subclass, so a const table can name them.
// Dropped: the `parent` chain (`applyTo`, `buildParent`, `shouldUpdate`); `shouldAcceptUserOffset`,
// `adjustPositionForNewDimensions`, `recommendDeferredLoading`, `allowImplicitScrolling`, `carriedMomentum` and the
// other physics classes; `ClampingScrollPhysics.applyBoundaryConditions`' debug assertion that the call is redundant
// (it returns 0.0 then, as Flutter does in release builds); `ScrollMetrics`' axis direction.

use super::constants::{kMaxFlingVelocity, kMinFlingVelocity, kTouchSlop};
use super::physics::{ScrollSpringSimulation, Simulation, Tolerance};
use super::scroll_simulation::{BouncingScrollSimulation, ClampingScrollSimulation};
use crate::{Pixels, SpringConfig, px};

/// The rate at which scroll momentum will be decelerated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollDecelerationRate {
    /// Standard deceleration, aligned with mobile software expectations.
    #[default]
    Normal,
    /// Increased deceleration, aligned with desktop software expectations.
    ///
    /// Appropriate for use with input devices more precise than touch screens,
    /// such as trackpads or mouse wheels.
    Fast,
}

/// Flutter's `FixedScrollMetrics`: a description of a scrollable's contents, useful for modeling the state of its
/// viewport. All values are in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    /// The minimum in-range value for `pixels`.
    pub min_scroll_extent: f64,
    /// The maximum in-range value for `pixels`.
    pub max_scroll_extent: f64,
    /// The current scroll position, in logical pixels along the axis direction.
    pub pixels: f64,
    /// The extent of the viewport along the axis direction.
    pub viewport_dimension: f64,
    /// The number of device pixels per logical pixel.
    pub device_pixel_ratio: f64,
}

impl ScrollMetrics {
    /// Whether the `pixels` value is outside the `min_scroll_extent` and `max_scroll_extent`.
    pub fn out_of_range(&self) -> bool {
        self.pixels < self.min_scroll_extent || self.pixels > self.max_scroll_extent
    }
}

/// `ScrollPhysics.toleranceFor`: the tolerance to use for ballistic simulations on a display of
/// `device_pixel_ratio` device pixels per logical pixel.
pub fn tolerance_for(device_pixel_ratio: f64) -> Tolerance {
    Tolerance {
        velocity: 1.0 / (0.050 * device_pixel_ratio), // logical pixels per second
        distance: 1.0 / device_pixel_ratio,           // logical pixels
        time: Tolerance::DEFAULT_TOLERANCE.time,
    }
}

/// How a scrollable decelerates after a fling and behaves past its edges: Flutter's `ScrollPhysics` subclasses.
///
/// A fling with no extents (GPUI's touch recognizer) runs the friction part only; bouncing, glow and clamping at the
/// edges are the scroll container's, which knows its extents and passes them in [`ScrollMetrics`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollPhysics {
    /// `BouncingScrollPhysics`: scroll physics for environments that allow the
    /// scroll offset to go beyond the bounds of the content, but then bounce the
    /// content back to the edge of those bounds. This is the behavior typically
    /// seen on iOS.
    Bouncing {
        /// Used to determine parameters for friction simulations.
        deceleration_rate: ScrollDecelerationRate,
    },
    /// `ClampingScrollPhysics`: scroll physics for environments that prevent the
    /// scroll offset from reaching beyond the bounds of the content. This is the
    /// behavior typically seen on Android.
    Clamping,
}

impl ScrollPhysics {
    /// iOS scroll feel: `BouncingScrollPhysics` at `UIScrollView`'s normal
    /// deceleration rate.
    pub const fn ios() -> Self {
        Self::Bouncing {
            deceleration_rate: ScrollDecelerationRate::Normal,
        }
    }

    /// Android scroll feel: `ClampingScrollPhysics`, Android's `OverScroller`.
    pub const fn android() -> Self {
        Self::Clamping
    }

    /// The minimum distance an input pointer drag must have moved to be
    /// considered a scroll fling gesture.
    ///
    /// This value is typically compared with the distance traveled along the
    /// scrolling axis.
    pub fn min_fling_distance(&self) -> Pixels {
        kTouchSlop
    }

    /// The minimum velocity for an input pointer drag to be considered a
    /// scroll fling.
    pub fn min_fling_velocity(&self) -> f32 {
        match self {
            Self::Bouncing { .. } => kMinFlingVelocity * 2.0,
            Self::Clamping => kMinFlingVelocity,
        }
    }

    /// Scroll fling velocity magnitudes will be clamped to this value.
    pub fn max_fling_velocity(&self) -> f32 {
        match self {
            Self::Bouncing {
                deceleration_rate: ScrollDecelerationRate::Fast,
            } => kMaxFlingVelocity * 8.0,
            Self::Bouncing {
                deceleration_rate: ScrollDecelerationRate::Normal,
            }
            | Self::Clamping => kMaxFlingVelocity,
        }
    }

    /// The minimum amount of pixel distance drags must move by to start motion
    /// the first time or after each time the drag motion stopped.
    ///
    /// If `None`, no minimum threshold is enforced.
    pub fn drag_start_distance_motion_threshold(&self) -> Option<Pixels> {
        match self {
            Self::Bouncing { .. } => Some(px(3.5)),
            Self::Clamping => None,
        }
    }

    /// The spring to use for ballistic simulations.
    pub fn spring(&self) -> SpringConfig {
        match self {
            Self::Bouncing {
                deceleration_rate: ScrollDecelerationRate::Fast,
            } => SpringConfig::with_damping_ratio(0.3, 75.0, 1.3),
            Self::Bouncing {
                deceleration_rate: ScrollDecelerationRate::Normal,
            }
            // ScrollPhysics._kDefaultSpring
            | Self::Clamping => SpringConfig::with_damping_ratio(0.5, 100.0, 1.1),
        }
    }

    /// Used by `DragScrollActivity` and other user-driven activities to convert
    /// an offset in logical pixels as provided by the `DragUpdateDetails` into a
    /// delta to apply (subtract from the current position).
    ///
    /// `offset` must not be zero, and the metrics' extents must be ordered.
    pub fn apply_physics_to_user_offset(&self, position: &ScrollMetrics, offset: f64) -> f64 {
        let Self::Bouncing { deceleration_rate } = *self else {
            return offset;
        };
        if !position.out_of_range() {
            return offset;
        }

        let overscroll_past_start = (position.min_scroll_extent - position.pixels).max(0.0);
        let overscroll_past_end = (position.pixels - position.max_scroll_extent).max(0.0);
        let overscroll_past = overscroll_past_start.max(overscroll_past_end);
        let easing = (overscroll_past_start > 0.0 && offset < 0.0)
            || (overscroll_past_end > 0.0 && offset > 0.0);

        let friction = if easing {
            // Apply less resistance when easing the overscroll vs tensioning.
            deceleration_rate
                .friction_factor((overscroll_past - offset.abs()) / position.viewport_dimension)
        } else {
            deceleration_rate.friction_factor(overscroll_past / position.viewport_dimension)
        };
        let direction = offset.signum();

        if easing && deceleration_rate == ScrollDecelerationRate::Fast {
            return direction * offset.abs();
        }
        direction * apply_friction(overscroll_past, offset.abs(), friction)
    }

    /// Determines the overscroll by applying the boundary conditions: the part of the move from `position.pixels`
    /// to `value` that would leave the range, which the scrollable refuses (and reports as overscroll). Bouncing
    /// physics lets the position leave the range: none.
    pub fn apply_boundary_conditions(&self, position: &ScrollMetrics, value: f64) -> f64 {
        if let Self::Bouncing { .. } = self {
            return 0.0;
        }
        if value < position.pixels && position.pixels <= position.min_scroll_extent {
            // Underscroll.
            return value - position.pixels;
        }
        if position.max_scroll_extent <= position.pixels && position.pixels < value {
            // Overscroll.
            return value - position.pixels;
        }
        if value < position.min_scroll_extent && position.min_scroll_extent < position.pixels {
            // Hit top edge.
            return value - position.min_scroll_extent;
        }
        if position.pixels < position.max_scroll_extent && position.max_scroll_extent < value {
            // Hit bottom edge.
            return value - position.max_scroll_extent;
        }
        0.0
    }

    /// Returns a simulation for ballistic scrolling starting from the given
    /// position with the given velocity (logical pixels per second), or `None`
    /// when there is nothing to animate.
    pub fn create_ballistic_simulation(
        &self,
        position: &ScrollMetrics,
        velocity: f64,
    ) -> Option<Box<dyn Simulation>> {
        let tolerance = tolerance_for(position.device_pixel_ratio);
        match *self {
            Self::Bouncing { deceleration_rate } => {
                let is_stationary = velocity.abs() < tolerance.velocity;
                let is_rubber_band = is_stationary && position.out_of_range();

                if !is_stationary || position.out_of_range() {
                    return Some(Box::new(BouncingScrollSimulation::new(
                        position.pixels,
                        velocity,
                        position.min_scroll_extent,
                        position.max_scroll_extent,
                        if is_rubber_band {
                            rubber_band_spring()
                        } else {
                            self.spring()
                        },
                        match deceleration_rate {
                            ScrollDecelerationRate::Fast => 1400.0,
                            ScrollDecelerationRate::Normal => 0.0,
                        },
                        tolerance,
                    )));
                }
                None
            }
            Self::Clamping => {
                if position.out_of_range() {
                    let end = if position.pixels > position.max_scroll_extent {
                        position.max_scroll_extent
                    } else {
                        position.min_scroll_extent
                    };
                    return Some(Box::new(ScrollSpringSimulation::new(
                        self.spring(),
                        position.pixels,
                        end,
                        velocity.min(0.0),
                        tolerance,
                    )));
                }
                if velocity.abs() < tolerance.velocity {
                    return None;
                }
                if velocity > 0.0 && position.pixels >= position.max_scroll_extent {
                    return None;
                }
                if velocity < 0.0 && position.pixels <= position.min_scroll_extent {
                    return None;
                }
                Some(Box::new(ClampingScrollSimulation::new(
                    position.pixels,
                    velocity,
                    ClampingScrollSimulation::DEFAULT_FRICTION,
                )))
            }
        }
    }
}

impl ScrollDecelerationRate {
    /// `BouncingScrollPhysics.frictionFactor`: the multiple applied to
    /// overscroll to make it appear that scrolling past the edge of the
    /// scrollable contents is harder than scrolling within bounds. This is done
    /// by reducing the ratio of the scroll effect output vs the scroll gesture
    /// input.
    ///
    /// This factor starts at 0.52 for [`ScrollDecelerationRate::Normal`] and 0.26 for
    /// [`ScrollDecelerationRate::Fast`].
    ///
    /// The `overscroll_fraction` represents how far past the edge the user has
    /// dragged, where 0.0 means no overscroll. As this value increases, the
    /// friction factor decreases quadratically, making further overscroll harder.
    pub fn friction_factor(self, overscroll_fraction: f64) -> f64 {
        (1.0 - overscroll_fraction).powi(2)
            * match self {
                Self::Fast => 0.26,
                Self::Normal => 0.52,
            }
    }
}

// BouncingScrollPhysics._applyFriction
fn apply_friction(extent_outside: f64, mut abs_delta: f64, gamma: f64) -> f64 {
    let mut total = 0.0;
    if extent_outside > 0.0 {
        let delta_to_limit = extent_outside / gamma;
        if abs_delta < delta_to_limit {
            return abs_delta * gamma;
        }
        total += extent_outside;
        abs_delta -= delta_to_limit;
    }
    total + abs_delta
}

// Approximation of iOS native rubber band decay rate.
const RUBBER_BAND_HALF_LIFE_SECONDS: f64 = 0.07;

/// `BouncingScrollPhysics._rubberBandSpring`: spring used to animate
/// overscroll bounce from a stationary release in iOS native style.
///
/// Research indicates that iOS employs a distinct decay function when a
/// scrollable area is released in an overscroll and stationary state (zero
/// initial velocity), conforming to an exponential decay model.
//
// ## Mathematical derivation
//
// A standard spring-damper system follows the second-order differential equation:
// m*x'' + c*x' + k*x = 0
//
// To force this second-order system to behave like a first-order exponential
// decay x(t) = C * e^(-lambda * t), we configure it as an overdamped spring
// with two explicitly defined roots (r1 and r2) for its characteristic
// equation:
//
// * r1 = -lambda (the primary root driving the visible exponential decay)
// * r2 = -100000 * lambda (an extremely large negative root)
//
// Because r2 is massive and negative, its corresponding term in the exact
// mathematical solution (C2 * e^(r2 * t)) decays to zero almost
// instantaneously. The system movement becomes dominated by r1.
//
// Using Vieta's formulas for the characteristic equation r^2 + (c/m)r + (k/m) = 0:
// * r1 + r2 = -c/m => damping (c) = -(r1 + r2) * m
// * r1 * r2 = k/m  => stiffness (k) = (r1 * r2) * m
fn rubber_band_spring() -> SpringConfig {
    // Decay constant (lambda) for rubber band spring simulation.
    let rubber_band_lambda = (2f64.ln() / RUBBER_BAND_HALF_LIFE_SECONDS) as f32;
    SpringConfig {
        mass: 1.0,
        stiffness: 1e5 * rubber_band_lambda * rubber_band_lambda,
        damping: (1e5 + 1.0) * rubber_band_lambda,
    }
}

#[cfg(test)]
#[path = "scroll_physics_tests.rs"]
mod tests;
