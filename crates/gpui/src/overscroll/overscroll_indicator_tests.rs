// Ported from flutter/flutter packages/flutter/test/widgets/overscroll_indicator_test.dart and
// overscroll_stretch_indicator_test.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The
// Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

//! Flutter's indicator tests are widget tests: gestures on a real scrollable, checked through what it paints. Here
//! each keeps its name and its check, driven by the notifications its gesture sends: `slowDrag` (ten 5 px moves,
//! 20 ms apart, then up) becomes ten overscroll notifications a frame apart and a drag end; a fling into an edge
//! becomes one overscroll notification with a velocity. Flutter's painted circle becomes [`GlowController::paint`]
//! and `StretchEffect.stretchStrength` [`StretchingOverscrollIndicator::stretch_strength`].
//!
//! Dropped (widget plumbing or layout): "Nested scrollable", "Overscroll indicator changes side when you shift
//! sides", "Flipping direction of scrollable doesn't change overscroll behavior", "Overscroll ignored from alternate
//! axis", "Overscroll horizontally", "Nested overscrolls do not throw exceptions", "Changing settings",
//! "CustomScrollView overscroll indicator works if there is sliver before center", "CustomScrollView overscroll
//! indicator works well with [CustomScrollView.center] and [OverscrollIndicatorNotification.paintOffset]", "The
//! OverscrollIndicator should not overflow the scrollable view edge", the four "does not crash" tests, "Stretch
//! overscroll will do nothing when axes do not match", the box position tests "Stretch overscroll vertically" /
//! "horizontally" (and RTL, reverse and "change direction mid scroll" variants), "Stretch does not overflow bounds
//! of container", the clip and image-filter tests.

use std::cell::Cell;

use super::*;

const FRAME: Duration = Duration::from_millis(20);
/// Flutter's test viewport: 800 × 600.
const WIDTH: f64 = 800.0;
const HEIGHT: f64 = 600.0;

/// A 2000 px list in the 600 px viewport, scrolled to `pixels`.
fn metrics(pixels: f64) -> ScrollMetrics {
    ScrollMetrics {
        min_scroll_extent: 0.0,
        max_scroll_extent: 1400.0,
        pixels,
        viewport_dimension: HEIGHT,
        device_pixel_ratio: 3.0,
    }
}

fn drag_at(x: f64) -> Option<OverscrollDrag> {
    Some(OverscrollDrag {
        cross_axis_offset: x,
        main_axis_extent: HEIGHT,
        cross_axis_extent: WIDTH,
    })
}

fn accept_all(_: &mut OverscrollIndicatorNotification) {}

/// Flutter's `slowDrag` at `x` into the leading (`overscroll` < 0) or trailing edge, from `*now`; ends with the up.
fn slow_drag(
    glow: &mut GlowingOverscrollIndicator,
    now: &mut Instant,
    x: f64,
    overscroll: f64,
    pixels: f64,
) {
    for _ in 0..10 {
        let notification = ScrollNotification::Overscroll {
            overscroll,
            velocity: 0.0,
            drag: drag_at(x),
        };
        glow.handle_scroll_notification(notification, &metrics(pixels), *now, &mut accept_all);
        *now += FRAME;
        glow.tick(*now);
    }
    let end = ScrollNotification::End {
        drag_velocity: Some(0.0),
    };
    glow.handle_scroll_notification(end, &metrics(pixels), *now, &mut accept_all);
}

/// Flutter's `pumpAndSettle(const Duration(seconds: 1))`, in frames.
fn settle(glow: &mut GlowingOverscrollIndicator, now: &mut Instant) {
    for _ in 0..50 {
        *now += FRAME;
        glow.tick(*now);
    }
}

#[test]
fn overscroll_indicator_color() {
    let mut glow = GlowingOverscrollIndicator::default();
    let mut now = Instant::now();
    assert_eq!(glow.leading.paint(WIDTH, HEIGHT), None, "doesNotOverscroll");

    // the scroll gesture from tester.scroll happens in zero time, so nothing should appear:
    let pull = ScrollNotification::Overscroll {
        overscroll: -100.0,
        velocity: 0.0,
        drag: drag_at(400.0),
    };
    glow.handle_scroll_notification(pull, &metrics(0.0), now, &mut accept_all);
    let end = ScrollNotification::End {
        drag_velocity: Some(0.0),
    };
    glow.handle_scroll_notification(end, &metrics(0.0), now, &mut accept_all);
    assert_eq!(glow.leading.paint(WIDTH, HEIGHT), None, "zero-time drag");
    now += Duration::from_millis(100); // animate
    glow.tick(now);
    assert_eq!(
        glow.leading.paint(WIDTH, HEIGHT),
        None,
        "zero-time drag, animated"
    );

    slow_drag(&mut glow, &mut now, 200.0, -5.0, 0.0);
    assert!(
        glow.leading.paint(WIDTH, HEIGHT).is_some(),
        "slow drag paints the glow"
    );

    settle(&mut glow, &mut now);
    assert_eq!(glow.leading.paint(WIDTH, HEIGHT), None, "settled");
}

#[test]
fn overscroll_indicator_changes_side_when_you_drag_on_the_other_side() {
    let mut glow = GlowingOverscrollIndicator::default();
    let mut now = Instant::now();
    let center_x = |glow: &GlowingOverscrollIndicator| {
        glow.leading
            .paint(WIDTH, HEIGHT)
            .map(|paint| paint.center.0)
            .unwrap_or(f64::NAN)
    };

    slow_drag(&mut glow, &mut now, 400.0, -10.0, 0.0);
    assert_eq!(center_x(&glow), 400.0);
    slow_drag(&mut glow, &mut now, 100.0, -10.0, 0.0);
    assert!(
        center_x(&glow) < 400.0,
        "Dragging on left hand side did not overscroll on left hand side."
    );
    slow_drag(&mut glow, &mut now, 700.0, -10.0, 0.0);
    assert!(
        center_x(&glow) > 400.0,
        "Dragging on right hand side did not overscroll on right hand side."
    );

    settle(&mut glow, &mut now);
    assert_eq!(glow.leading.paint(WIDTH, HEIGHT), None, "settled");
}

#[test]
fn overscroll_in_both_directions() {
    let mut glow = GlowingOverscrollIndicator::default();
    let mut now = Instant::now();
    slow_drag(&mut glow, &mut now, 200.0, -5.0, 0.0);
    assert!(glow.leading.paint(WIDTH, HEIGHT).is_some());
    assert_eq!(glow.trailing.paint(WIDTH, HEIGHT), None, "one circle");
    slow_drag(&mut glow, &mut now, 200.0, 5.0, 1400.0);
    assert!(
        glow.leading.paint(WIDTH, HEIGHT).is_some() && glow.trailing.paint(WIDTH, HEIGHT).is_some()
    );

    settle(&mut glow, &mut now);
    assert_eq!(
        glow.leading
            .paint(WIDTH, HEIGHT)
            .or(glow.trailing.paint(WIDTH, HEIGHT)),
        None,
        "settled"
    );
}

#[test]
fn overscroll_indicator_notification_paint_offset_test() {
    // (Flutter test, overscroll at the edge, edge pixels, reversed-scroll pixels)
    let cases = [
        ("Leading", -5.0, 0.0, 30.0),
        ("Trailing", 5.0, 1400.0, 1370.0),
    ];
    for (name, overscroll, edge, reversed) in cases {
        let mut glow = GlowingOverscrollIndicator::default();
        let mut now = Instant::now();
        let leading = overscroll < 0.0;
        let mut dispatch = |notification: &mut OverscrollIndicatorNotification| {
            if notification.leading == leading {
                notification.paint_offset = 50.0;
            }
        };
        for _ in 0..10 {
            let pull = ScrollNotification::Overscroll {
                overscroll,
                velocity: 0.0,
                drag: drag_at(200.0),
            };
            glow.handle_scroll_notification(pull, &metrics(edge), now, &mut dispatch);
            now += FRAME;
            glow.tick(now);
        }
        let controller =
            |glow: &GlowingOverscrollIndicator| if leading { glow.leading } else { glow.trailing };
        let translate = |glow: &GlowingOverscrollIndicator| {
            controller(glow)
                .paint(WIDTH, HEIGHT)
                .map(|paint| paint.translate_y)
        };
        // The OverscrollIndicator should respect the [OverscrollIndicatorNotification.paintOffset] setting.
        assert_eq!(translate(&glow), Some(50.0), "{name}");
        // Reverse scroll direction. The OverscrollIndicator should move with the CustomScrollView.
        let update = ScrollNotification::Update { dragging: true };
        glow.handle_scroll_notification(update, &metrics(reversed), now, &mut dispatch);
        now += FRAME;
        glow.tick(now);
        assert_eq!(translate(&glow), Some(50.0 - 30.0), "{name}, reversed");
    }
}

/// Sends `notifications` to `stretch` one frame apart from `*now`, ticking each frame.
fn send(
    stretch: &mut StretchingOverscrollIndicator,
    now: &mut Instant,
    notifications: &[ScrollNotification],
    dispatch: &mut dyn FnMut(&mut OverscrollIndicatorNotification),
) {
    for notification in notifications {
        stretch.handle_scroll_notification(*notification, &metrics(0.0), *now, dispatch);
        *now += FRAME;
        stretch.controller.tick(*now);
    }
}

/// Ticks `stretch` for `duration`, a frame at a time (`pump`, or `pumpAndSettle` with 5 s).
fn pump(stretch: &mut StretchingOverscrollIndicator, now: &mut Instant, duration: Duration) {
    let end = *now + duration;
    while *now < end {
        *now += FRAME;
        stretch.controller.tick(*now);
    }
}

const SETTLE: Duration = Duration::from_secs(5);

fn drag_overscroll(overscroll: f64) -> ScrollNotification {
    ScrollNotification::Overscroll {
        overscroll,
        velocity: 0.0,
        drag: drag_at(400.0),
    }
}

fn fling_overscroll(overscroll: f64, velocity: f64) -> ScrollNotification {
    ScrollNotification::Overscroll {
        overscroll,
        velocity,
        drag: None,
    }
}

#[test]
fn stretch_limit() {
    // Regression test for https://github.com/flutter/flutter/issues/99264
    let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
    let mut now = Instant::now();
    // Overscroll beyond the limit (the viewport is 600.0).
    send(
        &mut stretch,
        &mut now,
        &[ScrollNotification::Start, drag_overscroll(-610.0)],
        &mut accept_all,
    );
    let max_stretch = stretch.stretch_strength();
    assert!(max_stretch > 0.0);
    // Overscroll way way beyond the limit
    send(
        &mut stretch,
        &mut now,
        &[ScrollNotification::Start, drag_overscroll(-1000.0)],
        &mut accept_all,
    );
    assert_eq!(stretch.stretch_strength(), max_stretch);
}

#[test]
fn multiple_pointers_will_not_exceed_stretch_limit() {
    // Regression test for https://github.com/flutter/flutter/issues/99264
    let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
    let mut now = Instant::now();
    send(
        &mut stretch,
        &mut now,
        &[ScrollNotification::Start],
        &mut accept_all,
    );
    let mut last_stretched = 0.0;
    // Each pointer adds 210 of overscroll; the third exceeds the max stretch (600).
    for (pointer, grows) in [(1, true), (2, true), (3, true), (4, false)] {
        send(
            &mut stretch,
            &mut now,
            &[drag_overscroll(-210.0)],
            &mut accept_all,
        );
        let stretched = stretch.stretch_strength();
        // Since we have maxed out the overscroll, it should not have stretched
        // further, regardless of the number of pointers.
        assert_eq!(stretched > last_stretched, grows, "pointer {pointer}");
        assert!(stretched >= last_stretched, "pointer {pointer}");
        last_stretched = stretched;
    }
}

#[test]
fn disallow_stretching_overscroll() {
    // (Flutter test, overscroll notification)
    let cases = [
        ("Disallow stretching overscroll", drag_overscroll(-200.0)),
        (
            "Disallow stretching overscroll during a fling",
            fling_overscroll(-50.0, -10000.0),
        ),
    ];
    for (name, overscroll) in cases {
        let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
        let mut now = Instant::now();
        let mut disallow =
            |notification: &mut OverscrollIndicatorNotification| notification.disallow_indicator();
        send(
            &mut stretch,
            &mut now,
            &[ScrollNotification::Start, overscroll],
            &mut disallow,
        );
        pump(&mut stretch, &mut now, Duration::from_millis(100));
        assert_eq!(stretch.stretch_strength(), 0.0, "{name}");
    }
}

#[test]
fn disallowed_stretch_does_not_persist_into_a_new_scroll_sequence() {
    let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
    let mut now = Instant::now();
    let disallow_stretch = Cell::new(true);
    let indicator_notification = Cell::new(0);
    let mut dispatch = |notification: &mut OverscrollIndicatorNotification| {
        if disallow_stretch.get() {
            notification.disallow_indicator();
        }
        indicator_notification.set(indicator_notification.get() + 1);
    };
    let disallowed_gesture = [
        ScrollNotification::Start,
        drag_overscroll(-200.0),
        ScrollNotification::End {
            drag_velocity: Some(0.0),
        },
    ];
    send(&mut stretch, &mut now, &disallowed_gesture, &mut dispatch);
    pump(&mut stretch, &mut now, SETTLE);
    assert_eq!(indicator_notification.get(), 1);
    assert_eq!(stretch.stretch_strength(), 0.0);

    disallow_stretch.set(false);
    send(
        &mut stretch,
        &mut now,
        &[ScrollNotification::Start, fling_overscroll(50.0, 10000.0)],
        &mut dispatch,
    );
    pump(&mut stretch, &mut now, Duration::from_millis(100));
    assert_eq!(indicator_notification.get(), 2);
    assert_ne!(stretch.stretch_strength(), 0.0);
}

#[test]
fn fling_toward_an_edge_causes_stretch_toward_the_other_edge() {
    // (Flutter test, overscroll velocity, sign of the stretch strength mid-fling)
    let cases = [
        (
            "Fling toward the trailing edge causes stretch toward the leading edge",
            10000.0_f64,
            -1.0,
        ),
        (
            "Fling toward the leading edge causes stretch toward the trailing edge",
            -10000.0,
            1.0,
        ),
    ];
    for (name, velocity, sign) in cases {
        let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
        let mut now = Instant::now();
        send(
            &mut stretch,
            &mut now,
            &[fling_overscroll(velocity.signum(), velocity)],
            &mut accept_all,
        );
        pump(&mut stretch, &mut now, Duration::from_millis(100));
        assert_eq!(stretch.stretch_strength().signum(), sign, "{name}");
        pump(&mut stretch, &mut now, SETTLE);
        // The boxes should now be at their final position.
        assert_eq!(stretch.stretch_strength(), 0.0, "{name}, settled");
    }
}

#[test]
fn changing_scroll_direction_during_recede_animation_will_not_change_the_stretch_direction() {
    let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
    let mut now = Instant::now();
    // Overscroll to the trailing edge
    send(
        &mut stretch,
        &mut now,
        &[ScrollNotification::Start, drag_overscroll(200.0)],
        &mut accept_all,
    );
    let stretched = stretch.stretch_strength();
    assert!(stretched < 0.0, "stretched toward the leading edge");
    // This will trigger the recede animation
    send(
        &mut stretch,
        &mut now,
        &[ScrollNotification::Update { dragging: true }],
        &mut accept_all,
    );
    pump(&mut stretch, &mut now, Duration::from_millis(100));
    // Explicitly check that the stretch is not 0.0, since this would probably mean that
    // the stretch direction is wrong.
    assert_ne!(stretch.stretch_strength(), 0.0);
    assert_eq!(stretch.stretch_strength().signum(), stretched.signum());

    let up = ScrollNotification::End {
        drag_velocity: Some(-1000.0),
    };
    send(&mut stretch, &mut now, &[up], &mut accept_all);
    pump(&mut stretch, &mut now, Duration::from_millis(100));
    // At this point, the stretchStrength should be close to 0.
    assert!(stretch.stretch_strength().abs() < 0.01);
    pump(&mut stretch, &mut now, SETTLE);
    // The recede animation is done now.
    assert_eq!(stretch.stretch_strength(), 0.0);
}

#[test]
fn stretching_animation_completes_after_fling_under_scroll_physics_with_high_friction() {
    // Regression test for https://github.com/flutter/flutter/issues/146277
    let mut stretch = StretchingOverscrollIndicator::new(AxisDirection::Down);
    let mut now = Instant::now();
    // The fling reaches the trailing edge slowly: overscrollNotification.velocity < 25.
    send(
        &mut stretch,
        &mut now,
        &[fling_overscroll(0.1, 20.0)],
        &mut accept_all,
    );
    pump(&mut stretch, &mut now, SETTLE);
    assert_eq!(stretch.stretch_strength(), 0.0);
}

#[test]
fn fling_overscroll_triggers_stretching_animation_correctly() {
    // Regression test for https://github.com/flutter/flutter/issues/169659
    // (Flutter test, axis direction, whether the stretch strength grows in the next 50 ms)
    let cases = [
        (
            "Fling overscroll triggers stretching animation correctly",
            AxisDirection::Down,
            true,
        ),
        (
            "Fling overscroll triggers stretching animation correctly in reverse",
            AxisDirection::Up,
            false,
        ),
    ];
    for (name, axis_direction, grows) in cases {
        let mut stretch = StretchingOverscrollIndicator::new(axis_direction);
        let mut now = Instant::now();
        // Simulates a fast and short user fling into the leading edge.
        stretch.handle_scroll_notification(
            fling_overscroll(-1.0, -1000.0),
            &metrics(0.0),
            now,
            &mut accept_all,
        );
        let progress_strength = stretch.stretch_strength();
        pump(&mut stretch, &mut now, Duration::from_millis(50));
        // Verifies that the stretchStrength has changed compared to the
        // previously recorded value, ensuring the fling triggers stretching.
        assert_eq!(
            stretch.stretch_strength() > progress_strength,
            grows,
            "{name}"
        );
        assert_ne!(stretch.stretch_strength(), progress_strength, "{name}");
        pump(&mut stretch, &mut now, SETTLE);
        assert_eq!(stretch.stretch_strength().abs(), 0.0, "{name}, settled");
    }
}
