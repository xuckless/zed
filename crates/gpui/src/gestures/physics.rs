// Ported from flutter/flutter packages/flutter/lib/src/physics/{tolerance,simulation,friction_simulation,
// spring_simulation,utils}.dart and packages/flutter/lib/src/foundation/constants.dart (`precisionErrorTolerance`)
// @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.
//
// Flutter's physics library, one module. Springs run on GPUI's own `SpringConfig` (spring.rs), which passes Flutter's
// newton_test.dart spring cases (the table below), so `SpringDescription` is `SpringConfig` and `SpringSimulation` is
// glue over `SpringConfig::step` with Flutter's `isDone`; Flutter's `_SpringSolution` classes are not ported.
// Dropped: `FrictionSimulation.through`, `BoundedFrictionSimulation`, `GravitySimulation`, `ClampedSimulation`,
// `SpringDescription.withDurationAndBounce` (and `duration`, `bounce`), `SpringType` (GPUI's
// `SpringConfig::canonical` gives the damping ratio), `SpringSimulation`'s `snapToEnd` (unused); `nearEqual`'s null
// arguments.

//! Flutter's physics simulations: [`FrictionSimulation`], [`SpringSimulation`]
//! and [`ScrollSpringSimulation`], with [`Tolerance`] and the [`Simulation`]
//! trait the scroll simulations (`scroll_simulation.rs`) build on. Times are in
//! seconds, positions in pixels, velocities in pixels per second.

#![allow(
    non_upper_case_globals,
    reason = "Flutter's constant names, kept so Flutter changes diff in"
)]

use crate::{SpringConfig, SpringState};

/// A tolerance for floating point comparisons (foundation/constants.dart),
/// `moreOrLessEquals`' default epsilon in Flutter's tests.
pub const precisionErrorTolerance: f64 = 1e-10;

/// Structure that specifies maximum allowable magnitudes for distances,
/// durations, and velocity differences to be considered equal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    /// The magnitude of the maximum distance between two points for them to be
    /// considered within tolerance.
    pub distance: f64,
    /// The magnitude of the maximum duration between two times for them to be
    /// considered within tolerance.
    pub time: f64,
    /// The magnitude of the maximum difference between two velocities for them to
    /// be considered within tolerance.
    pub velocity: f64,
}

impl Tolerance {
    const EPSILON_DEFAULT: f64 = 1e-3;

    /// A default tolerance of 0.001 for all three values.
    pub const DEFAULT_TOLERANCE: Tolerance = Tolerance {
        distance: Self::EPSILON_DEFAULT,
        time: Self::EPSILON_DEFAULT,
        velocity: Self::EPSILON_DEFAULT,
    };
}

/// The base class for all simulations.
///
/// A simulation models an object, in a one-dimensional space, on which particular
/// forces are being applied, and exposes:
///
///  * The object's position, [`Simulation::x`]
///  * The object's velocity, [`Simulation::dx`]
///  * Whether the simulation is "done", [`Simulation::is_done`]
///
/// A simulation is generally "done" if the object has, to a given tolerance,
/// come to a complete rest.
pub trait Simulation {
    /// The position of the object in the simulation at the given time.
    fn x(&self, time: f64) -> f64;
    /// The velocity of the object in the simulation at the given time.
    fn dx(&self, time: f64) -> f64;
    /// Whether the simulation is "done" at the given time.
    fn is_done(&self, time: f64) -> bool;
}

/// Numerically determine the input value which produces output value `target`
/// for a function `f`, given its first-derivative `df`.
fn newtons_method(
    initial_guess: f64,
    target: f64,
    f: impl Fn(f64) -> f64,
    df: impl Fn(f64) -> f64,
    iterations: usize,
) -> f64 {
    let mut guess = initial_guess;
    for _ in 0..iterations {
        guess -= (f(guess) - target) / df(guess);
    }
    guess
}

/// A simulation that applies a drag to slow a particle down.
///
/// Models a particle affected by fluid drag, e.g. air resistance.
///
/// The simulation ends when the velocity of the particle drops to zero (within
/// the current velocity tolerance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrictionSimulation {
    drag: f64,
    drag_log: f64,
    x: f64,
    v: f64,
    constant_deceleration: f64,
    // The time at which the simulation should be stopped.
    // This is needed when constantDeceleration is not zero (on Desktop), when
    // using the pure friction simulation, acceleration naturally reduces to zero
    // and creates a stopping point.
    final_time: f64,
    /// How close to the actual end of the simulation a value at a particular time
    /// must be before [`Simulation::is_done`] considers the simulation to be "done".
    pub tolerance: Tolerance,
}

impl FrictionSimulation {
    /// Creates a [`FrictionSimulation`] with the given arguments, namely: the fluid
    /// drag coefficient _cₓ_, a unitless value; the initial position _x₀_, in the same
    /// length units as used for [`Simulation::x`]; and the initial velocity _dx₀_, in the same
    /// velocity units as used for [`Simulation::dx`].
    pub fn new(
        drag: f64,
        position: f64,
        velocity: f64,
        tolerance: Tolerance,
        constant_deceleration: f64,
    ) -> Self {
        let mut simulation = Self {
            drag,
            drag_log: drag.ln(),
            x: position,
            v: velocity,
            // Dart's `double.sign` is 0.0 at zero, Rust's `signum` is not.
            constant_deceleration: constant_deceleration
                * if velocity == 0. {
                    0.
                } else {
                    velocity.signum()
                },
            // needs to be infinity for newtonsMethod call in constructor.
            final_time: f64::INFINITY,
            tolerance,
        };
        simulation.final_time = newtons_method(
            0.,
            0.,
            |time| simulation.dx(time),
            |time| {
                (simulation.v * simulation.drag.powf(time) * simulation.drag_log)
                    - simulation.constant_deceleration
            },
            10,
        );
        simulation
    }

    /// The value of [`Simulation::x`] at `f64::INFINITY`.
    pub fn final_x(&self) -> f64 {
        if self.constant_deceleration == 0. {
            return self.x - self.v / self.drag_log;
        }
        self.x(self.final_time)
    }

    /// The time at which the value of `x(time)` will equal `x`.
    ///
    /// Returns `f64::INFINITY` if the simulation will never reach `x`.
    pub fn time_at_x(&self, x: f64) -> f64 {
        if x == self.x {
            return 0.0;
        }
        let final_x = self.final_x();
        let out_of_reach = if self.v > 0. {
            x < self.x || x > final_x
        } else {
            x > self.x || x < final_x
        };
        if self.v == 0.0 || out_of_reach {
            return f64::INFINITY;
        }
        newtons_method(0., x, |time| self.x(time), |time| self.dx(time), 10)
    }
}

impl Simulation for FrictionSimulation {
    fn x(&self, time: f64) -> f64 {
        if time > self.final_time {
            return self.final_x();
        }
        self.x + self.v * self.drag.powf(time) / self.drag_log
            - self.v / self.drag_log
            - ((self.constant_deceleration / 2.) * time * time)
    }

    fn dx(&self, time: f64) -> f64 {
        if time > self.final_time {
            return 0.;
        }
        self.v * self.drag.powf(time) - self.constant_deceleration * time
    }

    fn is_done(&self, time: f64) -> bool {
        self.dx(time).abs() < self.tolerance.velocity
    }
}

impl SpringConfig {
    /// Flutter's `SpringDescription.withDampingRatio`: creates a spring given
    /// the mass (m), stiffness (k), and damping ratio (ζ). The damping ratio
    /// describes a gradual reduction in a spring oscillation.
    ///
    /// A ratio of 1.0 creates a critically damped spring, > 1.0 creates an
    /// overdamped spring and < 1.0 an underdamped one.
    pub fn with_damping_ratio(mass: f32, stiffness: f32, ratio: f32) -> Self {
        Self::new(stiffness, ratio * 2.0 * (mass * stiffness).sqrt(), mass)
    }
}

/// A spring simulation.
///
/// Models a particle attached to a spring that follows Hooke's law. Flutter's
/// closed forms are GPUI's [`SpringConfig::step`], which evaluates the spring
/// at any time from its start.
#[derive(Clone, Copy, Debug)]
pub struct SpringSimulation {
    spring: SpringConfig,
    start: SpringState,
    end_position: f64,
    /// How close to the end of the simulation a value must be before
    /// [`Simulation::is_done`] holds.
    pub tolerance: Tolerance,
}

impl SpringSimulation {
    /// Creates a spring simulation from the provided spring description, start
    /// distance, end distance, and initial velocity.
    ///
    /// The units for distance and velocity are arbitrary, but must be consistent
    /// with the units used for other lengths in the system.
    pub fn new(
        spring: SpringConfig,
        start: f64,
        end: f64,
        velocity: f64,
        tolerance: Tolerance,
    ) -> Self {
        Self {
            spring,
            start: SpringState {
                position: start as f32,
                velocity: velocity as f32,
            },
            end_position: end,
            tolerance,
        }
    }

    fn state(&self, time: f64) -> SpringState {
        self.spring
            .step(self.start, self.end_position as f32, time as f32)
    }
}

impl Simulation for SpringSimulation {
    fn x(&self, time: f64) -> f64 {
        f64::from(self.state(time).position)
    }

    fn dx(&self, time: f64) -> f64 {
        f64::from(self.state(time).velocity)
    }

    fn is_done(&self, time: f64) -> bool {
        near_zero(self.x(time) - self.end_position, self.tolerance.distance)
            && near_zero(self.dx(time), self.tolerance.velocity)
    }
}

/// A [`SpringSimulation`] where the value of [`Simulation::x`] is guaranteed to
/// have exactly the end value when the simulation [`Simulation::is_done`].
#[derive(Clone, Copy, Debug)]
pub struct ScrollSpringSimulation(pub SpringSimulation);

impl ScrollSpringSimulation {
    /// Creates a spring simulation from the provided spring description, start
    /// distance, end distance, and initial velocity.
    pub fn new(
        spring: SpringConfig,
        start: f64,
        end: f64,
        velocity: f64,
        tolerance: Tolerance,
    ) -> Self {
        Self(SpringSimulation::new(
            spring, start, end, velocity, tolerance,
        ))
    }
}

impl Simulation for ScrollSpringSimulation {
    fn x(&self, time: f64) -> f64 {
        if self.is_done(time) {
            self.0.end_position
        } else {
            self.0.x(time)
        }
    }

    fn dx(&self, time: f64) -> f64 {
        self.0.dx(time)
    }

    fn is_done(&self, time: f64) -> bool {
        self.0.is_done(time)
    }
}

/// Whether two doubles are within a given distance of each other.
///
/// The `epsilon` argument must be positive.
pub fn near_equal(a: f64, b: f64, epsilon: f64) -> bool {
    debug_assert!(epsilon >= 0.0);
    (a > (b - epsilon)) && (a < (b + epsilon)) || a == b
}

/// Whether a double is within a given distance of zero.
///
/// The epsilon argument must be positive.
pub fn near_zero(a: f64, epsilon: f64) -> bool {
    near_equal(a, 0.0, epsilon)
}

/// Flutter's tests as the spec (AGENTS.md, Flutter rule 6): each row is named
/// after the Flutter test it ports. Add a row per new Flutter case.
#[cfg(test)]
mod tests {
    use super::*;

    const PRECISION: f64 = precisionErrorTolerance;
    const INF: f64 = f64::INFINITY;

    /// One `expect` on a [`FrictionSimulation`].
    enum Expect {
        /// `x(time)` within epsilon of a value.
        X(f64, f64, f64),
        /// `dx(time)` within epsilon of a value.
        Dx(f64, f64, f64),
        /// `x(time)` strictly between two values.
        XBetween(f64, f64, f64),
        /// `dx(time)` strictly below a value.
        DxBelow(f64, f64),
        /// `finalX` within epsilon of a value.
        FinalX(f64, f64),
        /// `timeAtX(x)` equals a value exactly.
        TimeAtX(f64, f64),
        /// `timeAtX(x(time))` within [`PRECISION`] of `time`.
        TimeAtOwnX(f64),
        IsDone(f64, bool),
    }
    use Expect::*;

    struct FrictionCase {
        flutter_test: &'static str,
        drag: f64,
        position: f64,
        velocity: f64,
        tolerance_velocity: f64,
        constant_deceleration: f64,
        expects: &'static [Expect],
    }

    const DEFAULT_VELOCITY: f64 = Tolerance::DEFAULT_TOLERANCE.velocity;

    const FRICTION_CASES: &[FrictionCase] = &[
        FrictionCase {
            flutter_test: "friction_simulation_test.dart: Friction simulation positive velocity",
            drag: 0.135,
            position: 100.0,
            velocity: 100.0,
            tolerance_velocity: DEFAULT_VELOCITY,
            constant_deceleration: 0.,
            expects: &[
                X(0.0, 100.0, PRECISION),
                Dx(0.0, 100.0, PRECISION),
                X(0.1, 110.0, 1.0),
                X(0.5, 131.0, 1.0),
                X(2.0, 149.0, 1.0),
                FinalX(149.0, 1.0),
                TimeAtX(100.0, 0.0),
                TimeAtOwnX(0.1),
                TimeAtOwnX(0.5),
                TimeAtOwnX(2.0),
                TimeAtX(-1.0, INF),
                TimeAtX(200.0, INF),
            ],
        },
        FrictionCase {
            flutter_test: "friction_simulation_test.dart: Friction simulation negative velocity",
            drag: 0.135,
            position: 100.0,
            velocity: -100.0,
            tolerance_velocity: DEFAULT_VELOCITY,
            constant_deceleration: 0.,
            expects: &[
                X(0.0, 100.0, PRECISION),
                Dx(0.0, -100.0, PRECISION),
                X(0.1, 91.0, 1.0),
                X(0.5, 68.0, 1.0),
                X(2.0, 51.0, 1.0),
                FinalX(50.0, 1.0),
                TimeAtX(100.0, 0.0),
                TimeAtOwnX(0.1),
                TimeAtOwnX(0.5),
                TimeAtOwnX(2.0),
                TimeAtX(101.0, INF),
                TimeAtX(40.0, INF),
            ],
        },
        FrictionCase {
            flutter_test: "friction_simulation_test.dart: Friction simulation constant deceleration",
            drag: 0.135,
            position: 100.0,
            velocity: -100.0,
            tolerance_velocity: DEFAULT_VELOCITY,
            constant_deceleration: 100.,
            expects: &[
                X(0.0, 100.0, PRECISION),
                Dx(0.0, -100.0, PRECISION),
                X(0.1, 91.0, 1.0),
                X(0.5, 80.0, 1.0),
                X(2.0, 80.0, 1.0),
                FinalX(80.0, 1.0),
                TimeAtX(100.0, 0.0),
                TimeAtOwnX(0.1),
                TimeAtOwnX(0.2),
                TimeAtOwnX(0.3),
                TimeAtX(101.0, INF),
                TimeAtX(40.0, INF),
            ],
        },
        FrictionCase {
            flutter_test: "newton_test.dart: test_friction",
            drag: 0.3,
            position: 100.0,
            velocity: 400.0,
            tolerance_velocity: 1.0,
            constant_deceleration: 0.,
            expects: &[
                IsDone(0.0, false),
                X(0.0, 100.0, 0.0),
                Dx(0.0, 400.0, 0.0),
                XBetween(1.0, 330.0, 335.0),
                Dx(1.0, 120.0, 0.0),
                Dx(2.0, 36.0, 0.0),
                Dx(3.0, 10.8, PRECISION),
                DxBelow(4.0, 3.5),
                IsDone(5.0, true),
                XBetween(5.0, 431.0, 432.0),
            ],
        },
    ];

    #[test]
    fn friction_simulation_matches_flutter() {
        let mut failures = Vec::new();
        for case in FRICTION_CASES {
            let tolerance = Tolerance {
                velocity: case.tolerance_velocity,
                ..Tolerance::DEFAULT_TOLERANCE
            };
            let friction = FrictionSimulation::new(
                case.drag,
                case.position,
                case.velocity,
                tolerance,
                case.constant_deceleration,
            );
            for (index, expect) in case.expects.iter().enumerate() {
                let (got, ok) = match *expect {
                    X(time, want, epsilon) => {
                        let got = friction.x(time);
                        (got, (got - want).abs() <= epsilon)
                    }
                    Dx(time, want, epsilon) => {
                        let got = friction.dx(time);
                        (got, (got - want).abs() <= epsilon)
                    }
                    XBetween(time, low, high) => {
                        let got = friction.x(time);
                        (got, low < got && got < high)
                    }
                    DxBelow(time, high) => {
                        let got = friction.dx(time);
                        (got, got < high)
                    }
                    FinalX(want, epsilon) => {
                        let got = friction.final_x();
                        (got, (got - want).abs() <= epsilon)
                    }
                    TimeAtX(x, want) => {
                        let got = friction.time_at_x(x);
                        (got, got == want)
                    }
                    TimeAtOwnX(time) => {
                        let got = friction.time_at_x(friction.x(time));
                        (got, (got - time).abs() <= PRECISION)
                    }
                    IsDone(time, want) => {
                        let got = friction.is_done(time);
                        (f64::from(u8::from(got)), got == want)
                    }
                };
                if !ok {
                    failures.push(format!("{}, expect #{index}: got {got}", case.flutter_test));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    type SpringCheck = fn() -> Result<(), String>;

    fn expect(ok: bool, what: impl FnOnce() -> String) -> Result<(), String> {
        if ok { Ok(()) } else { Err(what()) }
    }

    /// Flutter's `expect(x(t).floor(), …)` and `expect(dx(t).floor(), …)` rows.
    fn floors(
        simulation: &SpringSimulation,
        x: &[(f64, f64)],
        dx: &[(f64, f64)],
    ) -> Result<(), String> {
        for &(time, want) in x {
            let got = simulation.x(time);
            expect(got.floor() == want, || {
                format!("x({time}) = {got}, floor wanted {want}")
            })?;
        }
        for &(time, want) in dx {
            let got = simulation.dx(time);
            expect(got.floor() == want, || {
                format!("dx({time}) = {got}, floor wanted {want}")
            })?;
        }
        Ok(())
    }

    const SPRING_TOLERANCE: Tolerance = Tolerance {
        distance: 0.01,
        velocity: 0.01,
        ..Tolerance::DEFAULT_TOLERANCE
    };

    /// `newton_test.dart`'s spring tests, one row per test, run on GPUI's
    /// `SpringConfig`. Each check is one of Flutter's `expect`s.
    const SPRING_CASES: &[(&str, SpringCheck)] = &[
        ("newton_test.dart: spring_types", || {
            // (spring, Flutter's SpringType): GPUI's damping ratio decides it.
            let springs = [
                (
                    SpringConfig::with_damping_ratio(1.0, 100.0, 1.0),
                    "criticallyDamped",
                ),
                (
                    SpringConfig::with_damping_ratio(1.0, 100.0, 0.75),
                    "underDamped",
                ),
                (
                    SpringConfig::with_damping_ratio(1.0, 100.0, 1.25),
                    "overDamped",
                ),
                // Just so we don't forget how to create a desc without the ratio.
                (SpringConfig::new(100.0, 20.0, 1.0), "criticallyDamped"),
            ];
            for (spring, want) in springs {
                let (_, ratio) = spring.canonical();
                let got = if ratio < 1.0 {
                    "underDamped"
                } else if ratio > 1.0 {
                    "overDamped"
                } else {
                    "criticallyDamped"
                };
                expect(got == want, || format!("{spring:?}: {got}, wanted {want}"))?;
            }
            Ok(())
        }),
        ("newton_test.dart: crit_spring", || {
            let spring = SpringConfig::with_damping_ratio(1.0, 100.0, 1.0);
            let crit = SpringSimulation::new(spring, 0.0, 500.0, 0.0, SPRING_TOLERANCE);
            expect(!crit.is_done(0.0), || "done at 0".into())?;
            expect(crit.x(0.0) == 0.0 && crit.dx(0.0) == 0.0, || {
                "moves at 0".into()
            })?;
            floors(
                &crit,
                &[(0.25, 356.0), (0.5, 479.0), (0.75, 497.0)],
                &[(0.25, 1026.0), (0.5, 168.0), (0.75, 20.0)],
            )?;
            expect(crit.x(1.5) > 499.0 && crit.x(1.5) < 501.0, || {
                format!("x(1.5) = {}", crit.x(1.5))
            })?;
            // basically within tolerance
            expect(crit.dx(1.5) < 0.1, || format!("dx(1.5) = {}", crit.dx(1.5)))?;
            expect(crit.is_done(1.60), || "not done at 1.6".into())
        }),
        ("newton_test.dart: overdamped_spring", || {
            let spring = SpringConfig::with_damping_ratio(1.0, 100.0, 1.25);
            let over = SpringSimulation::new(spring, 0.0, 500.0, 0.0, SPRING_TOLERANCE);
            expect(!over.is_done(0.0), || "done at 0".into())?;
            expect(
                over.x(0.0) == 0.0 && over.dx(0.0).abs() <= PRECISION,
                || "moves at 0".into(),
            )?;
            floors(
                &over,
                &[(0.5, 445.0), (1.0, 495.0), (1.5, 499.0)],
                &[(0.5, 273.0), (1.0, 22.0), (1.5, 1.0)],
            )?;
            expect(over.is_done(3.0), || "not done at 3".into())
        }),
        ("newton_test.dart: underdamped_spring", || {
            let spring = SpringConfig::with_damping_ratio(1.0, 100.0, 0.25);
            let under =
                SpringSimulation::new(spring, 0.0, 300.0, 0.0, Tolerance::DEFAULT_TOLERANCE);
            expect(!under.is_done(0.0), || "done at 0".into())?;
            expect(
                under.x(0.0).abs() <= PRECISION && under.dx(0.0).abs() <= PRECISION,
                || "moves at 0".into(),
            )?;
            // Overshot with negative velocity
            floors(
                &under,
                &[(1.0, 325.0), (6.0, 299.0)],
                &[(1.0, -65.0), (6.0, 0.0)],
            )?;
            expect(under.is_done(6.0), || "not done at 6".into())
        }),
    ];

    #[test]
    fn spring_simulation_matches_flutter() {
        let failures: Vec<String> = SPRING_CASES
            .iter()
            .filter_map(|(flutter_test, check)| {
                check()
                    .err()
                    .map(|error| format!("{flutter_test}: {error}"))
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
