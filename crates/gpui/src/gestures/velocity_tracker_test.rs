// Ported from flutter/flutter packages/flutter/test/gestures/velocity_tracker_test.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//
// Each row names the Flutter test it ports. Flutter's timestamps are `Duration`s; here they are offsets from one
// `Instant`. Flutter's tracker reads "time since the last sample" from a stopwatch the test never advances (except by
// `pumpAndSettle`, 100 ms); GPUI's tracker takes that clock as `now`, so a query passes the newest sample's time plus
// the elapsed time Flutter's stopwatch would show.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use scheduler::Instant;

use super::VelocityTrackerKind::{self, IOSScrollViewFling, LeastSquares, MacOSScrollViewFling};
use super::velocity_tracker::VelocityTracker;
use super::velocity_tracker_data::{
    INTERRUPTED_VELOCITY_EVENT_DATA, PointerEvent, VELOCITY_EVENT_DATA,
};
use crate::{Point, point, px};

fn within_tolerance(actual: f64, expected: f64) -> bool {
    const K_TOLERANCE: f64 = 0.001; // Within .1% of expected value
    let diff = (actual - expected) / expected;
    diff.abs() < K_TOLERANCE
}

fn check_velocity(actual: Point<f32>, expected: (f64, f64)) -> bool {
    within_tolerance(f64::from(actual.x), expected.0)
        && within_tolerance(f64::from(actual.y), expected.1)
}

const EXPECTED: &[(f64, f64)] = &[
    (219.59280094228163, 1304.701682306001),
    (355.71046950050845, 967.2112857054104),
    (12.657970884022308, -36.90447839251946),
    (714.1399654786744, -2561.534447931869),
    (-19.668121066218564, -2910.105747052462),
    (646.8690114934209, 2976.977762577527),
    (396.6988447819592, 2106.225572911095),
    (298.31594440044495, -3660.8315955215294),
    (-1.7334232785165882, -3288.13174127454),
    (384.6361280392334, -2645.6612524779835),
    (176.37900397918557, 2711.2542876273264),
    (396.9328560260098, 4280.651578291764),
    (-71.51939428321249, 3716.7385187526947),
];

/// Recorded traces through a touch `VelocityTracker`: the velocity at each `PointerUpEvent`, in order.
/// To add one: a trace in velocity_tracker_data.rs and its velocities at each up.
const TRACES: &[(&str, &[(PointerEvent, u64, f64, f64)], &[(f64, f64)])] = &[
    (
        "Velocity tracker gives expected results",
        VELOCITY_EVENT_DATA,
        EXPECTED,
    ),
    // Regression test for https://github.com/flutter/flutter/pull/7510
    (
        "Interrupted velocity estimation",
        INTERRUPTED_VELOCITY_EVENT_DATA,
        &[(649.5, 3890.3)],
    ),
];

#[test]
fn velocity_tracker_traces() {
    for (name, events, expected) in TRACES {
        let start = Instant::now();
        let mut tracker = VelocityTracker::with_kind(LeastSquares);
        let mut last_sample = start;
        let mut ups = 0;
        for (event, milliseconds, x, y) in *events {
            let time = start + Duration::from_millis(*milliseconds);
            match event {
                PointerEvent::Down | PointerEvent::Move => {
                    tracker.add_position(time, point(px(*x as f32), px(*y as f32)));
                    last_sample = time;
                }
                PointerEvent::Up => {
                    let velocity = tracker.get_velocity(last_sample);
                    assert!(
                        check_velocity(velocity, expected[ups]),
                        "{name}: up {ups}: got {velocity:?}, expected {:?}",
                        expected[ups]
                    );
                    ups += 1;
                }
            }
        }
        assert_eq!(ups, expected.len(), "{name}: up events");
    }
}

/// When a steady drag is queried: after each sample from the `from`-th on, or once after the last sample.
enum Query {
    AfterEach { from: usize },
    AtEnd { elapsed: Duration },
}

/// A pointer moving by (0, -1) every second, `samples` times, then queried. `None` expects only that no
/// assertion fires.
/// To add one: a row with the Flutter test's name, tracker, sample count, query and expected velocity.
const STEADY_DRAGS: &[(&str, VelocityTrackerKind, usize, Query, Option<(f32, f32)>)] = &[
    (
        "No data velocity estimation",
        LeastSquares,
        0,
        Query::AtEnd {
            elapsed: Duration::ZERO,
        },
        Some((0., 0.)),
    ),
    (
        "Estimate does not throw when there are more than 1 point",
        IOSScrollViewFling,
        5,
        Query::AfterEach { from: 0 },
        None,
    ),
    (
        "Makes consistent velocity estimates with consistent velocity",
        IOSScrollViewFling,
        10,
        Query::AfterEach { from: 3 },
        Some((0., -1.)),
    ),
    (
        "Assume zero velocity when there are no recent samples - base VelocityTracker",
        LeastSquares,
        10,
        // `tester.pumpAndSettle()` with nothing scheduled pumps one 100 ms frame.
        Query::AtEnd {
            elapsed: Duration::from_millis(100),
        },
        Some((0., 0.)),
    ),
    (
        "Assume zero velocity when there are no recent samples - IOS",
        IOSScrollViewFling,
        10,
        Query::AtEnd {
            elapsed: Duration::from_millis(100),
        },
        Some((0., 0.)),
    ),
    (
        "Assume zero velocity when there are no recent samples - MacOS",
        MacOSScrollViewFling,
        10,
        Query::AtEnd {
            elapsed: Duration::from_millis(100),
        },
        Some((0., 0.)),
    ),
];

#[test]
fn velocity_tracker_steady_drags() {
    let position_delta = point(px(0.), px(-1.));
    let duration_delta = Duration::from_secs(1);
    for (name, kind, samples, query, expected) in STEADY_DRAGS {
        let mut tracker = VelocityTracker::with_kind(*kind);
        let mut position = Point::default();
        let mut time = Instant::now();
        let check = |velocity: Point<f32>| {
            if let Some((x, y)) = expected {
                assert_eq!(velocity, point(*x, *y), "{name}");
            }
        };
        for i in 0..*samples {
            position = position + position_delta;
            time += duration_delta;
            tracker.add_position(time, position);
            if let Query::AfterEach { from } = query
                && i >= *from
            {
                check(tracker.get_velocity(time));
            }
        }
        if let Query::AtEnd { elapsed } = query {
            check(tracker.get_velocity(time + *elapsed));
        }
    }
}

/// One step of an assertion row: add a sample at this offset from the tracker's start, or query the velocity.
enum Step {
    AddPosition(Duration),
    GetVelocity,
}

/// Flutter's debug-mode assertions, which Rust's `debug_assert!` mirrors: the steps, and the text the assertion
/// must contain. To add one: a row with the Flutter test's name, the steps and the assertion text.
const ASSERTIONS: &[(&str, &[Step], &str)] = &[
    (
        "FreeScrollStartVelocityTracker.getVelocity throws when no points",
        &[Step::GetVelocity],
        "at least 1 point",
    ),
    (
        "FreeScrollStartVelocityTracker.getVelocity throws when the new point precedes the previous point",
        &[
            Step::AddPosition(Duration::from_secs(60 * 60)),
            Step::GetVelocity,
            Step::AddPosition(Duration::from_secs(1)),
        ],
        "has a smaller timestamp",
    ),
];

#[test]
fn velocity_tracker_assertions() {
    if !cfg!(debug_assertions) {
        return;
    }
    for (name, steps, message) in ASSERTIONS {
        let start = Instant::now();
        let mut tracker = VelocityTracker::with_kind(IOSScrollViewFling);
        let mut now = start;
        let panic = catch_unwind(AssertUnwindSafe(|| {
            for step in *steps {
                match step {
                    Step::AddPosition(time) => {
                        now = start + *time;
                        tracker.add_position(now, Point::default());
                    }
                    Step::GetVelocity => {
                        tracker.get_velocity(now);
                    }
                }
            }
        }))
        .expect_err(name);
        let text = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(text.contains(message), "{name}: assertion said {text:?}");
    }
}
