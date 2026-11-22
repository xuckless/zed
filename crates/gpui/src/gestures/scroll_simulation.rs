// Ported from flutter/flutter packages/flutter/lib/src/widgets/scroll_simulation.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7), with its tests from packages/flutter/test/widgets/
// scroll_simulation_test.dart and the BouncingScrollSimulation cases of packages/flutter/test/physics/newton_test.dart.
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//
// Dropped: the `leadingExtent <= trailingExtent` assertion, `BouncingScrollSimulation`'s mutable `tolerance` (fixed at
// construction; Flutter copies it into the inner simulations on every call), and `ClampingScrollSimulation`'s
// `tolerance` (its `isDone` never reads it).

//! The ballistic scroll simulations [`ScrollPhysics`](super::ScrollPhysics)
//! creates: [`BouncingScrollSimulation`] (iOS, macOS) and
//! [`ClampingScrollSimulation`] (Android and the other platforms).

use super::physics::{FrictionSimulation, ScrollSpringSimulation, Simulation, Tolerance};
use crate::SpringConfig;

/// An implementation of scroll physics that matches iOS.
///
/// The simulation starts in a friction phase (or in a spring phase if it starts out of range), and switches to the
/// spring once the friction would carry the position past an edge.
#[derive(Clone, Copy, Debug)]
pub struct BouncingScrollSimulation {
    /// When [`Simulation::x`] falls below this value the simulation switches from an internal friction model to a
    /// spring model which causes [`Simulation::x`] to "spring" back to [`Self::leading_extent`].
    pub leading_extent: f64,
    /// When [`Simulation::x`] exceeds this value the simulation switches from an internal friction model to a
    /// spring model which causes [`Simulation::x`] to "spring" back to [`Self::trailing_extent`].
    pub trailing_extent: f64,
    /// The spring used to return [`Simulation::x`] to either [`Self::leading_extent`] or [`Self::trailing_extent`].
    pub spring: SpringConfig,
    /// How close the simulation must come to rest before it is done.
    pub tolerance: Tolerance,
    phase: BouncingPhase,
}

/// Flutter's `_frictionSimulation`, `_springSimulation` and `_springTime`, as the one phase plan they encode.
#[derive(Clone, Copy, Debug)]
enum BouncingPhase {
    /// Started out of range (`_springTime` is negative infinity): only the spring runs.
    Spring(ScrollSpringSimulation),
    /// Friction, then the spring from `spring_time` when the friction would leave the range (`_springTime` is
    /// infinite when `then` is `None`).
    Friction {
        friction: FrictionSimulation,
        then: Option<(f64, ScrollSpringSimulation)>,
    },
}

impl BouncingScrollSimulation {
    /// The maximum velocity that can be transferred from the inertia of a ballistic scroll into overscroll.
    pub const MAX_SPRING_TRANSFER_VELOCITY: f64 = 5000.0;

    /// Creates a simulation group for scrolling on iOS, with the given parameters.
    ///
    /// The position and velocity arguments must use the same units as will be expected from the [`Simulation::x`]
    /// and [`Simulation::dx`] methods respectively (typically logical pixels and logical pixels per second
    /// respectively).
    ///
    /// The leading and trailing extents must use the unit of length, the same unit as used for the position
    /// argument and as expected from the [`Simulation::x`] method (typically logical pixels).
    ///
    /// The units used with the provided spring must similarly be consistent with the other
    /// arguments. A default set of constants is used for the `spring` description if it is omitted; these
    /// defaults assume that the unit of length is the logical pixel.
    pub fn new(
        position: f64,
        velocity: f64,
        leading_extent: f64,
        trailing_extent: f64,
        spring: SpringConfig,
        constant_deceleration: f64,
        tolerance: Tolerance,
    ) -> Self {
        let spring_to =
            |x: f64, end: f64, dx: f64| ScrollSpringSimulation::new(spring, x, end, dx, tolerance);
        let phase = if position < leading_extent {
            // _underscrollSimulation
            BouncingPhase::Spring(spring_to(position, leading_extent, velocity))
        } else if position > trailing_extent {
            // _overscrollSimulation
            BouncingPhase::Spring(spring_to(position, trailing_extent, velocity))
        } else {
            // Taken from UIScrollView.decelerationRate (.normal = 0.998)
            // 0.998^1000 = ~0.135
            let friction = FrictionSimulation::new(
                0.135,
                position,
                velocity,
                tolerance,
                constant_deceleration,
            );
            let final_x = friction.final_x();
            let edge = if velocity > 0.0 && final_x > trailing_extent {
                Some(trailing_extent)
            } else if velocity < 0.0 && final_x < leading_extent {
                Some(leading_extent)
            } else {
                None
            };
            let then = edge.map(|edge| {
                let spring_time = friction.time_at_x(edge);
                let dx = friction
                    .dx(spring_time)
                    .min(Self::MAX_SPRING_TRANSFER_VELOCITY);
                (spring_time, spring_to(edge, edge, dx))
            });
            BouncingPhase::Friction { friction, then }
        };
        Self {
            leading_extent,
            trailing_extent,
            spring,
            tolerance,
            phase,
        }
    }

    /// Flutter's `_simulation(time)`: the simulation that runs at `time`, and the time it started (`_timeOffset`).
    fn simulation(&self, time: f64) -> (&dyn Simulation, f64) {
        match &self.phase {
            BouncingPhase::Spring(spring) => (spring, 0.0),
            BouncingPhase::Friction {
                then: Some((spring_time, spring)),
                ..
            } if time > *spring_time => (spring, *spring_time),
            BouncingPhase::Friction { friction, .. } => (friction, 0.0),
        }
    }
}

impl Simulation for BouncingScrollSimulation {
    fn x(&self, time: f64) -> f64 {
        let (simulation, time_offset) = self.simulation(time);
        simulation.x(time - time_offset)
    }

    fn dx(&self, time: f64) -> f64 {
        let (simulation, time_offset) = self.simulation(time);
        simulation.dx(time - time_offset)
    }

    fn is_done(&self, time: f64) -> bool {
        let (simulation, time_offset) = self.simulation(time);
        simulation.is_done(time - time_offset)
    }
}

/// An implementation of scroll physics that aligns with Android.
///
/// For any value of `velocity`, this travels the same total distance as the
/// Android scroll physics.
///
/// This scroll physics has been adjusted relative to Android's in order to make
/// it ballistic, meaning that the deceleration at any moment is a function only
/// of the current velocity [`Simulation::dx`] and does not depend on how long ago the
/// simulation was started. Compared to this scroll physics, Android's moves
/// faster at the very beginning, then slower, and it ends at the same place but
/// a little later.
//
// This class is based on OverScroller.java from Android:
//   https://android.googlesource.com/platform/frameworks/base/+/android-13.0.0_r24/core/java/android/widget/OverScroller.java#738
// and in particular class SplineOverScroller (at the end of the file), starting
// at method "fling".  (A very similar algorithm is in Scroller.java in the same
// directory, but OverScroller is what's used by RecyclerView.)
//
// In the Android implementation, times are in milliseconds, positions are in
// physical pixels, but velocity is in physical pixels per whole second.
//
// The "See..." comments below refer to SplineOverScroller methods and values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClampingScrollSimulation {
    /// The position of the particle at the beginning of the simulation, in
    /// pixels.
    position: f64,
    /// The velocity at which the particle is traveling at the beginning of the
    /// simulation, in pixels per second.
    velocity: f64,
    /// The total time the simulation will run, in seconds.
    duration: f64,
    /// The total, signed, distance the simulation will travel, in pixels.
    distance: f64,
}

impl ClampingScrollSimulation {
    // See INFLEXION.
    const K_INFLEXION: f64 = 0.35;

    // See mPhysicalCoeff.  This has a value of 0.84 times Earth gravity,
    // expressed in units of logical pixels per second^2.
    const PHYSICAL_COEFF: f64 = 9.80665 // g, in meters per second^2
        * 39.37 // 1 meter / 1 inch
        * 160.0 // 1 inch / 1 logical pixel
        * 0.84; // "look and feel tuning"

    /// Flutter's default `friction`.
    pub const DEFAULT_FRICTION: f64 = 0.015;

    /// Creates a scroll physics simulation that aligns with Android scrolling.
    ///
    /// `friction` is the amount of friction the particle experiences as it
    /// travels: the more, the sooner it stops and the less far it travels.
    /// `0.015` travels the same total distance as the Android scroll physics
    /// (see mFlingFriction).
    pub fn new(position: f64, velocity: f64, friction: f64) -> Self {
        // See DECELERATION_RATE.
        let k_deceleration_rate = Self::k_deceleration_rate();

        // See getSplineFlingDuration().
        // See getSplineDeceleration().  That function's value is
        // math.log(velocity.abs() / referenceVelocity).
        let reference_velocity = friction * Self::PHYSICAL_COEFF / Self::K_INFLEXION;
        // This is the value getSplineFlingDuration() would return, but in seconds.
        let android_duration =
            (velocity.abs() / reference_velocity).powf(1. / (k_deceleration_rate - 1.0));
        // We finish a bit sooner than Android, in order to travel the
        // same total distance.
        let duration = k_deceleration_rate * Self::K_INFLEXION * android_duration;

        // See getSplineFlingDistance().  This returns the same value but with the
        // sign of [velocity], and in pixels.
        let distance = velocity * duration / k_deceleration_rate;

        Self {
            position,
            velocity,
            duration,
            distance,
        }
    }

    // See DECELERATION_RATE.
    fn k_deceleration_rate() -> f64 {
        0.78f64.ln() / 0.9f64.ln()
    }
}

impl Simulation for ClampingScrollSimulation {
    fn x(&self, time: f64) -> f64 {
        let t = (time / self.duration).clamp(0.0, 1.0);
        self.position + self.distance * (1.0 - (1.0 - t).powf(Self::k_deceleration_rate()))
    }

    fn dx(&self, time: f64) -> f64 {
        let t = (time / self.duration).clamp(0.0, 1.0);
        self.velocity * (1.0 - t).powf(Self::k_deceleration_rate() - 1.0)
    }

    fn is_done(&self, time: f64) -> bool {
        time >= self.duration
    }
}

/// Flutter's tests as the spec (AGENTS.md, Flutter rule 6): each row is named
/// after the Flutter test it ports. Add a row per new Flutter case.
#[cfg(test)]
mod tests {
    use super::super::physics::precisionErrorTolerance as PRECISION;
    use super::*;

    /// The (position, velocity) pairs Flutter's clamping tests start from.
    const CLAMPING_STARTS: &[(f64, f64)] = &[
        (51.0, 2866.91537),
        (584.0, 2617.294734),
        (345.0, 1982.785934),
        (0.0, 1831.366634),
        (-156.2, 1541.57665),
        (4.0, 1139.250439),
        (4534.0, 1073.553798),
        (75.0, 614.2093),
        (5469.0, 182.114534),
    ];

    fn clamping(position: f64, velocity: f64) -> ClampingScrollSimulation {
        ClampingScrollSimulation::new(
            position,
            velocity,
            ClampingScrollSimulation::DEFAULT_FRICTION,
        )
    }

    type ClampingCheck = fn() -> Result<(), String>;

    /// Flutter's `scroll_simulation_test.dart`, one row per test.
    const CLAMPING_CASES: &[(&str, ClampingCheck)] = &[
        (
            "ClampingScrollSimulation has a stable initial conditions",
            || {
                for &(position, velocity) in CLAMPING_STARTS {
                    let simulation = clamping(position, velocity);
                    let (x, dx) = (simulation.x(0.0), simulation.dx(0.0));
                    if (x - position).abs() > PRECISION || (dx - velocity).abs() > PRECISION {
                        return Err(format!("({position}, {velocity}) starts at ({x}, {dx})"));
                    }
                }
                Ok(())
            },
        ),
        (
            "ClampingScrollSimulation only decelerates, never speeds up",
            || {
                // Regression test for flutter/flutter#113424
                let simulation = clamping(0., 8000.0);
                let mut time = 0.0;
                let mut velocity = simulation.dx(time);
                while !simulation.is_done(time) {
                    if time >= 3.0 {
                        return Err(format!("still running at {time}"));
                    }
                    time += 1. / 60.;
                    let next_velocity = simulation.dx(time);
                    if next_velocity > velocity {
                        return Err(format!(
                            "speeds up at {time}: {velocity} -> {next_velocity}"
                        ));
                    }
                    velocity = next_velocity;
                }
                Ok(())
            },
        ),
        (
            "ClampingScrollSimulation reaches a smooth stop: velocity is continuous and goes to zero",
            || {
                // Regression test for flutter/flutter#113424
                let initial_velocity = 8000.0;
                let max_deceleration = 5130.0; // -acceleration(initialVelocity), from formula below
                let simulation = clamping(0., initial_velocity);
                let mut time = 0.0;
                let mut velocity = simulation.dx(time);
                let delta = 1. / 60.;
                loop {
                    if time >= 3.0 {
                        return Err(format!("still running at {time}"));
                    }
                    time += delta;
                    let next_velocity = simulation.dx(time);
                    if (next_velocity - velocity).abs() >= delta * max_deceleration {
                        return Err(format!("jumps at {time}: {velocity} -> {next_velocity}"));
                    }
                    velocity = next_velocity;
                    if simulation.is_done(time) {
                        break;
                    }
                }
                if velocity.abs() > PRECISION {
                    return Err(format!("ends at velocity {velocity}"));
                }
                Ok(())
            },
        ),
        ("ClampingScrollSimulation is ballistic", || {
            // Regression test for flutter/flutter#120338
            let delta = 1. / 90.;
            let undisturbed = clamping(0., 8000.0);
            let mut time = 0.0;
            let mut restarted = undisturbed;
            let mut trajectories = Vec::new();
            loop {
                if time >= 4.0 {
                    return Err(format!("still running at {time}"));
                }
                time += delta;
                restarted = clamping(restarted.x(delta), restarted.dx(delta));
                trajectories.push((
                    (restarted.x(0.), undisturbed.x(time)),
                    (restarted.dx(0.), undisturbed.dx(time)),
                ));
                if restarted.is_done(0.) && undisturbed.is_done(time) {
                    break;
                }
            }
            // Compare the headline number first: the total distances traveled.
            // This way, if the test fails, it shows the big final difference
            // instead of the tiny difference that's in the very first frame.
            let ((restarted_x, undisturbed_x), _) = trajectories[trajectories.len() - 1];
            if (restarted_x - undisturbed_x).abs() > PRECISION {
                return Err(format!("ends at {restarted_x}, not {undisturbed_x}"));
            }
            // The whole trajectories along the way should match too.
            for (frame, ((xr, xu), (dxr, dxu))) in trajectories.into_iter().enumerate() {
                if (xr - xu).abs() > PRECISION || (dxr - dxu).abs() > PRECISION {
                    return Err(format!("frame {frame}: ({xr}, {dxr}) vs ({xu}, {dxu})"));
                }
            }
            Ok(())
        }),
        (
            "ClampingScrollSimulation satisfies a physical acceleration formula",
            || {
                // Different regression test for flutter/flutter#120338
                //
                // This one provides a formula for the particle's acceleration as a function
                // of its velocity, and checks that it behaves according to that formula.
                // The point isn't that it's this specific formula, but just that there's
                // some formula which depends only on velocity, not time, so that the
                // physical metaphor makes sense.

                // Copied from the implementation.
                let k_deceleration_rate = 0.78f64.ln() / 0.9f64.ln();
                // Same as the referenceVelocity in _flingDuration.
                let reference_velocity = 0.015 * 9.80665 * 39.37 * 160.0 * 0.84 / 0.35;
                // The value of _duration when velocity == referenceVelocity.
                let reference_duration = k_deceleration_rate * 0.35;
                // The rate of deceleration when dx(time) == referenceVelocity.
                let reference_deceleration =
                    (k_deceleration_rate - 1.) * reference_velocity / reference_duration;
                let acceleration = |velocity: f64| {
                    -velocity.signum()
                        * reference_deceleration
                        * (velocity.abs() / reference_velocity)
                            .powf((k_deceleration_rate - 2.) / (k_deceleration_rate - 1.))
                };
                let jerk = |velocity: f64| {
                    reference_velocity / reference_duration / reference_duration
                        * (k_deceleration_rate - 1.)
                        * (k_deceleration_rate - 2.)
                        * (velocity.abs() / reference_velocity)
                            .powf((k_deceleration_rate - 3.) / (k_deceleration_rate - 1.))
                };
                for &(position, velocity) in CLAMPING_STARTS {
                    let simulation = clamping(position, velocity);
                    let delta = 1. / 60.;
                    let mut time = 0.0;
                    while time < 2.0 {
                        let difference = simulation.dx(time + delta) - simulation.dx(time);
                        let predicted_difference =
                            delta * acceleration(simulation.dx(time + delta / 2.));
                        let max_third_derivative = jerk(simulation.dx(time + delta));
                        if (difference - predicted_difference).abs()
                            >= max_third_derivative * delta.powi(2) / 2.
                        {
                            return Err(format!("({position}, {velocity}) at {time}"));
                        }
                        time += delta;
                    }
                }
                Ok(())
            },
        ),
    ];

    #[test]
    fn clamping_scroll_simulation_matches_flutter() {
        let failures: Vec<String> = CLAMPING_CASES
            .iter()
            .filter_map(|(flutter_test, check)| {
                check()
                    .err()
                    .map(|error| format!("{flutter_test}: {error}"))
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    fn bouncing(
        position: f64,
        velocity: f64,
        trailing_extent: f64,
        spring: SpringConfig,
        tolerance: Tolerance,
    ) -> BouncingScrollSimulation {
        BouncingScrollSimulation::new(
            position,
            velocity,
            0.0,
            trailing_extent,
            spring,
            0.0,
            tolerance,
        )
    }

    /// One `expect` on a [`BouncingScrollSimulation`].
    enum Expect {
        IsDone(f64, bool),
        /// `x(time)` within epsilon of a value.
        X(f64, f64, f64),
        /// `dx(time)` within epsilon of a value.
        Dx(f64, f64, f64),
        /// `dx(time)` strictly below a value.
        DxBelow(f64, f64),
    }
    use Expect::*;

    /// newton_test.dart's `BouncingScrollSimulation` tests: (Flutter test,
    /// position, velocity, trailing extent, spring (mass, stiffness, ratio),
    /// tolerance (velocity, distance), expects). The leading extent is 0.
    const BOUNCING_CASES: &[(&str, f64, f64, f64, (f32, f32, f32), (f64, f64), &[Expect])] = &[
        (
            "newton_test.dart: test_kinetic_scroll",
            100.0,
            800.0,
            300.0,
            (1.0, 50.0, 0.5),
            (0.5, 0.1),
            // switch from friction to spring at 0.5
            &[IsDone(0.0, false), IsDone(0.5, false), IsDone(3.5, true)],
        ),
        (
            "newton_test.dart: test_kinetic_scroll (negative velocity)",
            100.0,
            -800.0,
            300.0,
            (1.0, 50.0, 0.5),
            (0.5, 0.1),
            &[IsDone(0.0, false), IsDone(0.5, false), IsDone(3.5, true)],
        ),
        (
            "newton_test.dart: scroll_with_inf_edge_ends",
            100.0,
            400.0,
            f64::INFINITY,
            (1.0, 50.0, 0.5),
            (1.0, Tolerance::DEFAULT_TOLERANCE.distance),
            &[
                IsDone(0.0, false),
                X(0.0, 100.0, 0.0),
                Dx(0.0, 400.0, 0.0),
                X(1.0, 272.0, 1.0),
                Dx(1.0, 54.0, 1.0),
                Dx(2.0, 7.0, 1.0),
                DxBelow(3.0, 1.0),
                IsDone(5.0, true),
                X(5.0, 300.0, 1.0),
            ],
        ),
        (
            "newton_test.dart: over/under scroll spring",
            500.0,
            -7500.0,
            1000.0,
            (1.0, 170.0, 1.1),
            (45.0, 1.5),
            &[
                IsDone(0.0, false),
                X(0.0, 500.0, PRECISION),
                Dx(0.0, -7500.0, PRECISION),
                // Expect to reach 0.0 at about t=.07 at which point the simulation will
                // switch from friction to the spring
                IsDone(0.065, false),
                X(0.065, 42.0, 1.0),
                Dx(0.065, -6584.0, 1.0),
                // We've overscrolled (0.1 > 0.07). Trigger the underscroll
                // simulation, and reverse direction
                IsDone(0.1, false),
                X(0.1, -123.0, 1.0),
                Dx(0.1, -2613.0, 1.0),
                // Headed back towards 0.0 and slowing down.
                IsDone(0.5, false),
                X(0.5, -15.0, 1.0),
                Dx(0.5, 124.0, 1.0),
                // Now jump back to the beginning, because we can.
                IsDone(0.0, false),
                X(0.0, 500.0, PRECISION),
                Dx(0.0, -7500.0, PRECISION),
                IsDone(2.0, true),
                X(2.0, 0.0, 0.0),
                Dx(2.0, 0.0, 1.0),
            ],
        ),
    ];

    #[test]
    fn bouncing_scroll_simulation_matches_flutter() {
        let mut failures = Vec::new();
        for &(
            flutter_test,
            position,
            velocity,
            trailing,
            (mass, stiffness, ratio),
            (tv, td),
            expects,
        ) in BOUNCING_CASES
        {
            let tolerance = Tolerance {
                velocity: tv,
                distance: td,
                ..Tolerance::DEFAULT_TOLERANCE
            };
            let spring = SpringConfig::with_damping_ratio(mass, stiffness, ratio);
            let scroll = bouncing(position, velocity, trailing, spring, tolerance);
            for (index, expect) in expects.iter().enumerate() {
                let (got, ok) = match *expect {
                    IsDone(time, want) => {
                        let got = scroll.is_done(time);
                        (f64::from(u8::from(got)), got == want)
                    }
                    X(time, want, epsilon) => {
                        let got = scroll.x(time);
                        (got, (got - want).abs() <= epsilon)
                    }
                    Dx(time, want, epsilon) => {
                        let got = scroll.dx(time);
                        (got, (got - want).abs() <= epsilon)
                    }
                    DxBelow(time, high) => {
                        let got = scroll.dx(time);
                        (got, got < high)
                    }
                };
                if !ok {
                    failures.push(format!("{flutter_test}, expect #{index}: got {got}"));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
