// Ported from flutter/flutter packages/flutter/test/widgets/scroll_physics_test.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.

//! Flutter's `BouncingScrollPhysics` and `ClampingScrollPhysics` maths tests, one table row per Flutter test,
//! named after it. Dropped (widget or `ScrollPhysics` API, not ported): `allowSelectionEdgeScrolling`,
//! `applyTo()`, `subclasses applyTo()`, `PageScrollPhysics work with NestedScrollView`, the `shouldUpdate` and
//! shorthand tests, and the three `testWidgets` updating a position; the `PageScrollPhysics` row of "Creating the
//! simulation doesn't alter the velocity for time 0" (not ported).

use super::*;

/// Flutter's `FixedScrollMetrics(minScrollExtent: 0.0, ..., axisDirection: AxisDirection.down)`.
fn metrics(
    max_scroll_extent: f64,
    pixels: f64,
    viewport_dimension: f64,
    device_pixel_ratio: f64,
) -> ScrollMetrics {
    ScrollMetrics {
        min_scroll_extent: 0.0,
        max_scroll_extent,
        pixels,
        viewport_dimension,
        device_pixel_ratio,
    }
}

const MOBILE: ScrollPhysics = ScrollPhysics::Bouncing {
    deceleration_rate: ScrollDecelerationRate::Normal,
};
const DESKTOP: ScrollPhysics = ScrollPhysics::Bouncing {
    deceleration_rate: ScrollDecelerationRate::Fast,
};

/// Flutter's `moreOrLessEquals`.
fn more_or_less_equals(got: f64, wanted: f64) -> bool {
    (got - wanted).abs() <= super::super::physics::precisionErrorTolerance
}

#[test]
fn creating_the_simulation_does_not_alter_the_velocity_for_time_0() {
    // Calls to createBallisticSimulation may happen on every frame (i.e. when the maxScrollExtent changes)
    // Changing velocity for time 0 may cause a sudden, unwanted damping/speedup effect
    let position = metrics(100.0, 20.0, 500.0, 3.0);
    for physics in [MOBILE, ScrollPhysics::Clamping] {
        let simulation = physics
            .create_ballistic_simulation(&position, 1000.0)
            .expect("a moving release makes a simulation");
        assert!(
            more_or_less_equals(simulation.dx(0.0), 1000.0),
            "{physics:?}"
        );
    }
}

#[test]
fn bouncing_scroll_physics_test() {
    // (Flutter test, check)
    let cases: &[(&str, fn() -> bool)] = &[
        ("overscroll is progressively harder", || {
            let less =
                MOBILE.apply_physics_to_user_offset(&metrics(1000.0, -20.0, 100.0, 3.0), 10.0);
            let more =
                MOBILE.apply_physics_to_user_offset(&metrics(1000.0, -40.0, 100.0, 3.0), 10.0);
            // Scrolling from a more overscrolled position meets more resistance.
            less > 1.0 && less < 20.0 && more > 1.0 && more < 20.0 && less.abs() > more.abs()
        }),
        ("easing an overscroll still has resistance", || {
            let easing =
                MOBILE.apply_physics_to_user_offset(&metrics(1000.0, -20.0, 100.0, 3.0), -10.0);
            easing < -1.0 && easing > -10.0
        }),
        ("no resistance when not overscrolled", || {
            let position = metrics(1000.0, 300.0, 100.0, 3.0);
            MOBILE.apply_physics_to_user_offset(&position, 10.0) == 10.0
                && MOBILE.apply_physics_to_user_offset(&position, -10.0) == -10.0
        }),
        (
            "easing an overscroll meets less resistance than tensioning",
            || {
                let position = metrics(1000.0, -20.0, 100.0, 3.0);
                let easing = MOBILE.apply_physics_to_user_offset(&position, -10.0);
                let tensioning = MOBILE.apply_physics_to_user_offset(&position, 10.0);
                easing.abs() > tensioning.abs()
            },
        ),
        (
            "no easing resistance for ScrollDecelerationRate.fast",
            || {
                let position = metrics(1000.0, -20.0, 100.0, 3.0);
                let easing = DESKTOP.apply_physics_to_user_offset(&position, -10.0);
                let tensioning = DESKTOP.apply_physics_to_user_offset(&position, 10.0);
                tensioning.abs() < easing.abs() && easing == -10.0
            },
        ),
        (
            "overscroll a small list and a big list works the same way",
            || {
                let small =
                    MOBILE.apply_physics_to_user_offset(&metrics(10.0, -20.0, 100.0, 3.0), 10.0);
                let big =
                    MOBILE.apply_physics_to_user_offset(&metrics(1000.0, -20.0, 100.0, 3.0), 10.0);
                small == big && small > 1.0 && small < 20.0
            },
        ),
        ("frictionFactor", || {
            ScrollDecelerationRate::Fast.friction_factor(0.0) == 0.26
                && ScrollDecelerationRate::Normal.friction_factor(0.0) == 0.52
                && more_or_less_equals(ScrollDecelerationRate::Fast.friction_factor(0.4), 0.0936)
                && more_or_less_equals(ScrollDecelerationRate::Normal.friction_factor(0.4), 0.1872)
                && more_or_less_equals(ScrollDecelerationRate::Fast.friction_factor(0.8), 0.0104)
                && more_or_less_equals(ScrollDecelerationRate::Normal.friction_factor(0.8), 0.0208)
        }),
    ];
    for (name, check) in cases {
        assert!(check(), "BouncingScrollPhysics test: {name}");
    }
}

#[test]
fn bouncing_scroll_physics_selects_correct_spring_for_create_ballistic_simulation() {
    // (Flutter test, pixels, stationary x(0.1), x(0.2), x(0.3), x(0.4))
    let cases = [
        (
            "on leading edge overscroll",
            -500.0,
            [
                -185.7511436536831,
                -69.00628466755506,
                -25.635736232654022,
                -9.523639409892818,
            ],
        ),
        (
            "on trailing edge overscroll",
            1500.0,
            [
                1185.7511436536831,
                1069.006284667555,
                1025.635736232654,
                1009.5236394098928,
            ],
        ),
    ];
    for (name, pixels, stationary_x) in cases {
        let position = metrics(1000.0, pixels, 500.0, 1.0);
        let sim_stationary = MOBILE
            .create_ballistic_simulation(&position, 0.0)
            .expect("an out-of-range release makes a simulation");
        let sim_moving = MOBILE
            .create_ballistic_simulation(&position, -100.0)
            .expect("an out-of-range release makes a simulation");
        // Stationary simulation should follow the expected spring trajectory.
        for (index, wanted) in stationary_x.into_iter().enumerate() {
            let time = 0.1 * (index + 1) as f64;
            let got = sim_stationary.x(time);
            assert!(
                (got - wanted).abs() <= 0.01,
                "{name}: x({time}) = {got}, wanted {wanted}"
            );
        }
        // Stationary and moving simulations should produce different positions.
        assert!(
            !more_or_less_equals(sim_stationary.x(0.2), sim_moving.x(0.2)),
            "{name}: the moving release uses the other spring"
        );
    }
}

#[test]
fn clamping_scroll_physics_apply_boundary_conditions() {
    // (case, pixels, value, boundary overscroll); the range is 0..1000.
    let cases = [
        // Flutter's "ClampingScrollPhysics assertion test": a redundant call (value == pixels). Flutter asserts in
        // debug builds; the port returns what Flutter's release builds do.
        ("ClampingScrollPhysics assertion test", 500.0, 500.0, 0.0),
        ("underscroll", 0.0, -10.0, -10.0),
        ("overscroll", 1000.0, 1010.0, 10.0),
        ("hit top edge", 5.0, -10.0, -10.0),
        ("hit bottom edge", 995.0, 1010.0, 10.0),
        ("in range", 500.0, 510.0, 0.0),
    ];
    for (name, pixels, value, wanted) in cases {
        let position = metrics(1000.0, pixels, 0.0, 3.0);
        assert_eq!(
            ScrollPhysics::Clamping.apply_boundary_conditions(&position, value),
            wanted,
            "{name}"
        );
    }
}
