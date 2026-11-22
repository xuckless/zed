// Ported from flutter/flutter packages/flutter/test/gestures/scale_test.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.

//! Flutter's scale recognizer tests, one table per `testGesture`, named after it. Each row is one
//! `tester.route` (a down row also runs `addPointer` and `closeArena`) and the callbacks it must produce, in order.
//! Values Flutter checks are checked; the rest are left open.
//!
//! The arena: `Solo` is Flutter's arena with the scale its only member, so `closeArena` accepts it once the next
//! event has been routed (`resolveByDefault` in a microtask). `Shared` keeps a rival in it that never wins in these
//! traces (the tap and horizontal drag of Flutter's tests: the scale is routed first, so it claims every touch
//! before them), so only the scale's own `resolve` accepts it.
//!
//! Dropped (pan/zoom pointers are not ported): "Should recognize scale gestures from pointer pan/zoom events",
//! "Pointer pan/zooms should work alongside touches", "Scale gesture competes with drag for trackpad gesture",
//! "Scale gesture from pan/zoom events properly handles DragStartBehavior.start", "scale trackpadScrollCausesScale",
//! "scale ending velocity"; and the invertedStylus, unknown and trackpad pointers of "ScaleStartDetails should
//! contain the correct PointerDeviceKind" (GPUI has no such devices).

use std::f32::consts::PI;
use std::time::Duration;

use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Arena {
    Solo,
    Shared,
}

#[derive(Clone, Copy)]
enum Ev {
    Down(u64, PointerDevice, f32, f32, u64),
    Move(u64, f32, f32, u64),
    Up(u64),
}

use Ev::*;
const TOUCH: PointerDevice = PointerDevice::Touch;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Callback {
    Start,
    Update,
    End,
}

/// One expected callback: its kind and the fields Flutter checks.
#[derive(Clone, Copy)]
struct Want {
    callback: Callback,
    focal_point: Option<(f32, f32)>,
    delta: Option<(f32, f32)>,
    scale: Option<f32>,
    horizontal_scale: Option<f32>,
    vertical_scale: Option<f32>,
    rotation: Option<f32>,
    pointer_count: Option<usize>,
    time_ms: Option<u64>,
    kind: Option<PointerDevice>,
    /// Flutter's `closeTo` for the delta; everything else is exact (up to f32 rounding).
    delta_tolerance: f32,
}

const fn want(callback: Callback) -> Want {
    Want {
        callback,
        focal_point: None,
        delta: None,
        scale: None,
        horizontal_scale: None,
        vertical_scale: None,
        rotation: None,
        pointer_count: None,
        time_ms: None,
        kind: None,
        delta_tolerance: EXACT,
    }
}
const EXACT: f32 = 1e-4;
const START: Want = want(Callback::Start);
const UPDATE: Want = want(Callback::Update);
const END: Want = want(Callback::End);

impl Want {
    const fn focal(mut self, x: f32, y: f32) -> Self {
        self.focal_point = Some((x, y));
        self
    }
    const fn delta(mut self, x: f32, y: f32) -> Self {
        self.delta = Some((x, y));
        self
    }
    const fn scale(mut self, scale: f32) -> Self {
        self.scale = Some(scale);
        self
    }
    const fn hv(mut self, horizontal: f32, vertical: f32) -> Self {
        self.horizontal_scale = Some(horizontal);
        self.vertical_scale = Some(vertical);
        self
    }
    const fn rotation(mut self, rotation: f32) -> Self {
        self.rotation = Some(rotation);
        self
    }
    const fn count(mut self, count: usize) -> Self {
        self.pointer_count = Some(count);
        self
    }
    const fn at(mut self, ms: u64) -> Self {
        self.time_ms = Some(ms);
        self
    }
    const fn kind(mut self, kind: PointerDevice) -> Self {
        self.kind = Some(kind);
        self
    }
    const fn delta_within(mut self, tolerance: f32) -> Self {
        self.delta_tolerance = tolerance;
        self
    }
}

type Row<'a> = (Ev, &'a [Want]);

/// Replays `rows` through a fresh recognizer (`configure` sets its options) and checks each row's callbacks.
fn replay(name: &str, arena: Arena, configure: fn(&mut ScaleGestureRecognizer), rows: &[Row<'_>]) {
    let origin = Instant::now();
    let at = |ms: u64| origin + Duration::from_millis(ms);
    let mut scale = ScaleGestureRecognizer::default();
    configure(&mut scale);
    let mut devices = HashMap::default();
    for (index, (event, wants)) in rows.iter().enumerate() {
        let touch = |id: u64, phase, x: f32, y: f32, device| TouchEvent {
            id: TouchId(id),
            phase,
            position: point(px(x), px(y)),
            device,
            ..Default::default()
        };
        let mut outputs = Vec::new();
        match *event {
            Down(id, device, x, y, ms) => {
                devices.insert(id, device);
                let down = touch(id, TouchPhase::Started, x, y, device);
                scale.add_pointer(&down, at(ms));
                outputs.extend(scale.handle_event(&down, at(ms)));
                if arena == Arena::Solo {
                    outputs.extend(scale.accept_gesture(TouchId(id)));
                }
            }
            Move(id, x, y, ms) => {
                let moved = touch(id, TouchPhase::Moved, x, y, devices[&id]);
                outputs.extend(scale.handle_event(&moved, at(ms)));
            }
            Up(id) => {
                // TestPointer.up keeps the last position; the recognizer drops it.
                let up = touch(id, TouchPhase::Ended, 0.0, 0.0, devices[&id]);
                outputs.extend(scale.handle_event(&up, at(0)));
            }
        }
        let got: Vec<&ScaleOutput> = outputs
            .iter()
            .filter(|output| !matches!(output, ScaleOutput::Resolve { .. }))
            .collect();
        let kinds: Vec<Callback> = got
            .iter()
            .map(|output| match output {
                ScaleOutput::Start(_) => Callback::Start,
                ScaleOutput::Update(_) => Callback::Update,
                ScaleOutput::End(_) => Callback::End,
                ScaleOutput::Resolve { .. } => unreachable!(),
            })
            .collect();
        let want_kinds: Vec<Callback> = wants.iter().map(|want| want.callback).collect();
        assert_eq!(kinds, want_kinds, "{name}, row {index}: callbacks");
        for (output, want) in got.iter().zip(wants.iter()) {
            let near_within = |field: &str, got: f32, wanted: Option<f32>, tolerance: f32| {
                if let Some(wanted) = wanted {
                    assert!(
                        (got - wanted).abs() <= tolerance,
                        "{name}, row {index}: {field} {got} != {wanted}"
                    );
                }
            };
            let near =
                |field: &str, got: f32, wanted: Option<f32>| near_within(field, got, wanted, EXACT);
            let near_point =
                |field: &str, got: Point<Pixels>, wanted: Option<(f32, f32)>, tolerance: f32| {
                    near_within(
                        field,
                        f32::from(got.x),
                        wanted.map(|wanted| wanted.0),
                        tolerance,
                    );
                    near_within(
                        field,
                        f32::from(got.y),
                        wanted.map(|wanted| wanted.1),
                        tolerance,
                    );
                };
            let count = |got: usize| {
                if let Some(wanted) = want.pointer_count {
                    assert_eq!(got, wanted, "{name}, row {index}: pointer count");
                }
            };
            match output {
                ScaleOutput::Start(details) => {
                    near_point(
                        "start focal point",
                        details.focal_point,
                        want.focal_point,
                        EXACT,
                    );
                    count(details.pointer_count);
                    if let Some(ms) = want.time_ms {
                        assert_eq!(
                            details.source_time_stamp,
                            Some(at(ms)),
                            "{name}, row {index}: start time"
                        );
                    }
                    if let Some(kind) = want.kind {
                        assert_eq!(details.kind, Some(kind), "{name}, row {index}: kind");
                    }
                }
                ScaleOutput::Update(details) => {
                    near_point("focal point", details.focal_point, want.focal_point, EXACT);
                    near_point(
                        "delta",
                        details.focal_point_delta,
                        want.delta,
                        want.delta_tolerance,
                    );
                    near("scale", details.scale, want.scale);
                    near(
                        "horizontal scale",
                        details.horizontal_scale,
                        want.horizontal_scale,
                    );
                    near(
                        "vertical scale",
                        details.vertical_scale,
                        want.vertical_scale,
                    );
                    near("rotation", details.rotation, want.rotation);
                    count(details.pointer_count);
                    if let Some(ms) = want.time_ms {
                        assert_eq!(
                            details.source_time_stamp,
                            at(ms),
                            "{name}, row {index}: update time"
                        );
                    }
                }
                ScaleOutput::End(details) => count(details.pointer_count),
                ScaleOutput::Resolve { .. } => unreachable!(),
            }
        }
    }
}

fn defaults(_: &mut ScaleGestureRecognizer) {}

fn touch_only(scale: &mut ScaleGestureRecognizer) {
    scale.supported_devices = Some(vec![PointerDevice::Touch]);
}

#[test]
fn should_recognize_scale_gestures() {
    replay(
        "Should recognize scale gestures",
        Arena::Shared,
        defaults,
        &[
            (Down(1, TOUCH, 0.0, 0.0, 0), &[]),
            // One-finger panning
            (
                Move(1, 20.0, 30.0, 0),
                &[
                    START.focal(20.0, 30.0).count(1),
                    UPDATE
                        .focal(20.0, 30.0)
                        .scale(1.0)
                        .delta(20.0, 30.0)
                        .count(1),
                ],
            ),
            // Two-finger scaling
            (Down(2, TOUCH, 10.0, 20.0, 0), &[END]),
            // Zoom in
            (
                Move(2, 0.0, 10.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(10.0, 20.0)
                        .scale(2.0)
                        .hv(2.0, 2.0)
                        .delta(-5.0, -5.0)
                        .count(2),
                ],
            ),
            // Zoom out
            (
                Move(2, 15.0, 25.0, 0),
                &[UPDATE
                    .focal(17.5, 27.5)
                    .scale(0.5)
                    .hv(0.5, 0.5)
                    .delta(7.5, 7.5)
                    .count(2)],
            ),
            // Horizontal scaling
            (Move(2, 0.0, 20.0, 0), &[UPDATE.hv(2.0, 1.0).count(2)]),
            // Vertical scaling
            (
                Move(2, 10.0, 10.0, 0),
                &[UPDATE.hv(1.0, 2.0).delta(5.0, -5.0).count(2)],
            ),
            (Move(2, 15.0, 25.0, 0), &[UPDATE]),
            // Three-finger scaling
            (Down(3, TOUCH, 25.0, 35.0, 0), &[END]),
            // Zoom in
            (
                Move(3, 55.0, 65.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(30.0, 40.0)
                        .scale(5.0)
                        .delta(10.0, 10.0)
                        .count(3),
                ],
            ),
            // Return to original positions but with different fingers
            (Move(1, 25.0, 35.0, 0), &[UPDATE]),
            (Move(2, 20.0, 30.0, 0), &[UPDATE]),
            (
                Move(3, 15.0, 25.0, 0),
                &[UPDATE
                    .focal(20.0, 30.0)
                    .scale(1.0)
                    .delta(-13.3, -13.3)
                    .delta_within(0.1)
                    .count(3)],
            ),
            (Up(1), &[END]),
            // Continue scaling with two fingers
            (
                Move(3, 10.0, 20.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(15.0, 25.0)
                        .scale(2.0)
                        .delta(-2.5, -2.5)
                        .count(2),
                ],
            ),
            // Continue rotating with two fingers
            (
                Move(3, 30.0, 40.0, 0),
                &[UPDATE.focal(25.0, 35.0).scale(2.0).delta(10.0, 10.0)],
            ),
            (
                Move(3, 10.0, 20.0, 0),
                &[UPDATE
                    .focal(15.0, 25.0)
                    .scale(2.0)
                    .delta(-10.0, -10.0)
                    .count(2)],
            ),
            (Up(2), &[END]),
            // Continue panning with one finger
            (
                Move(3, 0.0, 0.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(0.0, 0.0)
                        .scale(1.0)
                        .delta(-10.0, -20.0)
                        .count(1),
                ],
            ),
            // We are done
            (Up(3), &[END]),
        ],
    );
}

#[test]
fn rejects_scale_gestures_from_unallowed_device_kinds() {
    replay(
        "Rejects scale gestures from unallowed device kinds",
        Arena::Solo,
        touch_only,
        &[
            // One-finger panning
            (Down(1, PointerDevice::Mouse, 0.0, 0.0, 0), &[]),
            // Using a mouse, the scale gesture shouldn't even start.
            (Move(1, 20.0, 30.0, 0), &[]),
        ],
    );
}

#[test]
fn scale_gestures_starting_from_allowed_device_kinds_cannot_be_ended_from_unallowed_devices() {
    replay(
        "Scale gestures starting from allowed device kinds cannot be ended from unallowed devices",
        Arena::Solo,
        touch_only,
        &[
            // One-finger panning
            (Down(1, TOUCH, 0.0, 0.0, 0), &[START.focal(0.0, 0.0)]),
            // The gesture can start using one touch finger.
            (
                Move(1, 20.0, 30.0, 0),
                &[UPDATE.focal(20.0, 30.0).scale(1.0)],
            ),
            // Two-finger scaling. Mouse-generated events are ignored.
            (Down(2, PointerDevice::Mouse, 10.0, 20.0, 0), &[]),
            // Zoom in using a mouse doesn't work either.
            (Move(2, 0.0, 10.0, 0), &[]),
        ],
    );
}

#[test]
fn scale_gesture_competes_with_drag() {
    replay(
        "Scale gesture competes with drag",
        Arena::Shared,
        defaults,
        &[
            // Vertical moves are scales.
            (Down(1, TOUCH, 10.0, 10.0, 0), &[]),
            // Scale will win if focal point delta exceeds 18.0*2.
            (Move(1, 10.0, 50.0, 0), &[START, UPDATE]), // Delta of 40.0 exceeds 18.0*2.
            // Second pointer joins scale even though it moves horizontally.
            (Down(2, TOUCH, 10.0, 20.0, 0), &[END]),
            (Move(2, 30.0, 20.0, 0), &[START, UPDATE]),
            (Up(1), &[END]),
            (Up(2), &[]),
            // Horizontal moves are either drags or scales, depending on which wins first.
            // In this case, we move fast, so that the scale wins. If we moved slowly,
            // the horizontal drag would win, since it was added first.
            (Down(3, TOUCH, 30.0, 30.0, 0), &[]),
            (Move(3, 100.0, 30.0, 0), &[START, UPDATE]),
            (Up(3), &[END]),
        ],
    );
}

#[test]
fn should_recognize_rotation_gestures() {
    replay(
        "Should recognize rotation gestures",
        Arena::Shared,
        defaults,
        &[
            (Down(1, TOUCH, 0.0, 0.0, 0), &[]),
            (
                Move(1, 20.0, 30.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(20.0, 30.0)
                        .delta(20.0, 30.0)
                        .rotation(0.0)
                        .count(1),
                ],
            ),
            // Two-finger scaling
            (Down(2, TOUCH, 30.0, 40.0, 0), &[END]),
            // Zoom in
            (
                Move(2, 40.0, 50.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(30.0, 40.0)
                        .delta(5.0, 5.0)
                        .rotation(0.0)
                        .count(2),
                ],
            ),
            // Rotation
            (
                Move(2, 0.0, 10.0, 0),
                &[UPDATE
                    .focal(10.0, 20.0)
                    .delta(-20.0, -20.0)
                    .rotation(PI)
                    .count(2)],
            ),
            // Three-finger scaling
            (Down(3, TOUCH, 25.0, 35.0, 0), &[END]),
            // Zoom in
            (
                Move(3, 55.0, 65.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(25.0, 35.0)
                        .delta(10.0, 10.0)
                        .rotation(0.0)
                        .count(3),
                ],
            ),
            // Return to original positions but with different fingers
            (Move(1, 25.0, 35.0, 0), &[UPDATE]),
            (Move(2, 20.0, 30.0, 0), &[UPDATE]),
            (
                Move(3, 15.0, 25.0, 0),
                &[UPDATE
                    .focal(20.0, 30.0)
                    .delta(-13.3, -13.3)
                    .delta_within(0.1)
                    .rotation(0.0)
                    .count(3)],
            ),
            (Up(1), &[END]),
            // Continue scaling with two fingers
            (
                Move(3, 10.0, 20.0, 0),
                &[
                    START,
                    UPDATE
                        .focal(15.0, 25.0)
                        .delta(-2.5, -2.5)
                        .rotation(0.0)
                        .count(2),
                ],
            ),
            // Continue rotating with two fingers
            (
                Move(3, 30.0, 40.0, 0),
                &[UPDATE
                    .focal(25.0, 35.0)
                    .delta(10.0, 10.0)
                    .rotation(-PI)
                    .count(2)],
            ),
            (
                Move(3, 10.0, 20.0, 0),
                &[UPDATE
                    .focal(15.0, 25.0)
                    .delta(-10.0, -10.0)
                    .rotation(0.0)
                    .count(2)],
            ),
            (Up(2), &[END]),
            // We are done
            (Up(3), &[]),
        ],
    );
}

// Regressing test for https://github.com/flutter/flutter/issues/78941
#[test]
fn first_rotation_test() {
    replay(
        "First rotation test",
        Arena::Solo,
        defaults,
        &[
            (Down(1, TOUCH, 0.0, 0.0, 0), &[START]),
            (Down(2, TOUCH, 10.0, 10.0, 0), &[END]),
            // Rotation 45°.
            (Move(2, 0.0, 10.0, 0), &[START, UPDATE.rotation(PI / 4.0)]),
        ],
    );
}

#[test]
fn scale_gestures_pointer_count_test() {
    replay(
        "Scale gestures pointer count test",
        Arena::Solo,
        defaults,
        &[
            // One-finger panning. One pointer in contact with the screen now.
            (Down(1, TOUCH, 0.0, 0.0, 0), &[START.count(1)]),
            (Move(1, 20.0, 30.0, 0), &[UPDATE.count(1)]),
            // Two-finger scaling. Additional pointer down will trigger an end event.
            (Down(2, TOUCH, 10.0, 20.0, 0), &[END.count(2)]),
            // The new pointer move will trigger a start event.
            (Move(2, 0.0, 10.0, 0), &[START.count(2), UPDATE.count(2)]),
            // One pointer in contact with the screen now.
            (Up(1), &[END.count(1)]),
            (Move(2, 0.0, 10.0, 0), &[START.count(1), UPDATE.count(1)]),
            // No pointer in contact with the screen now.
            (Up(2), &[END.count(0)]),
        ],
    );
}

#[test]
fn scale_start_details_and_scale_update_details_callbacks_should_contain_their_event_timestamp() {
    replay(
        "ScaleStartDetails and ScaleUpdateDetails callbacks should contain their event.timestamp",
        Arena::Shared,
        defaults,
        &[
            // One-finger panning.
            (Down(1, TOUCH, 0.0, 0.0, 10), &[]),
            (
                Move(1, 20.0, 30.0, 20),
                &[
                    START.at(10),
                    UPDATE
                        .focal(20.0, 30.0)
                        .scale(1.0)
                        .delta(20.0, 30.0)
                        .at(20)
                        .count(1),
                ],
            ),
            // Two-finger scaling.
            (Down(2, TOUCH, 10.0, 20.0, 30), &[END.count(2)]),
            // Zoom in.
            (
                Move(2, 0.0, 10.0, 40),
                &[
                    START.at(40),
                    UPDATE
                        .focal(10.0, 20.0)
                        .scale(2.0)
                        .hv(2.0, 2.0)
                        .delta(-5.0, -5.0)
                        .at(40),
                ],
            ),
            // Zoom out.
            (
                Move(2, 15.0, 25.0, 50),
                &[UPDATE
                    .focal(17.5, 27.5)
                    .scale(0.5)
                    .hv(0.5, 0.5)
                    .delta(7.5, 7.5)
                    .at(50)],
            ),
            // Horizontal scaling.
            (Move(2, 0.0, 20.0, 60), &[UPDATE.hv(2.0, 1.0).at(60)]),
            // Vertical scaling.
            (
                Move(2, 10.0, 10.0, 70),
                &[UPDATE.hv(1.0, 2.0).delta(5.0, -5.0).at(70)],
            ),
            (Move(2, 15.0, 25.0, 0), &[UPDATE]),
            // Three-finger scaling.
            (Down(3, TOUCH, 25.0, 35.0, 80), &[END]),
            // Zoom in.
            (
                Move(3, 55.0, 65.0, 90),
                &[
                    START.at(90),
                    UPDATE.focal(30.0, 40.0).scale(5.0).delta(10.0, 10.0).at(90),
                ],
            ),
            // Return to original positions but with different fingers.
            (Move(1, 25.0, 35.0, 100), &[UPDATE]),
            (Move(2, 20.0, 30.0, 110), &[UPDATE]),
            (
                Move(3, 15.0, 25.0, 120),
                &[UPDATE
                    .focal(20.0, 30.0)
                    .scale(1.0)
                    .delta(-13.3, -13.3)
                    .delta_within(0.1)
                    .at(120)],
            ),
            (Up(1), &[END]),
            // Continue scaling with two fingers.
            (
                Move(3, 10.0, 20.0, 130),
                &[
                    START.at(130),
                    UPDATE
                        .focal(15.0, 25.0)
                        .scale(2.0)
                        .delta(-2.5, -2.5)
                        .at(130),
                ],
            ),
            // Continue rotating with two fingers.
            (
                Move(3, 30.0, 40.0, 140),
                &[UPDATE
                    .focal(25.0, 35.0)
                    .scale(2.0)
                    .delta(10.0, 10.0)
                    .at(140)],
            ),
            (
                Move(3, 10.0, 20.0, 140),
                &[UPDATE
                    .focal(15.0, 25.0)
                    .scale(2.0)
                    .delta(-10.0, -10.0)
                    .at(140)],
            ),
            (Up(2), &[END]),
            // Continue panning with one finger.
            (
                Move(3, 0.0, 0.0, 150),
                &[
                    START.at(150),
                    UPDATE
                        .focal(0.0, 0.0)
                        .scale(1.0)
                        .delta(-10.0, -20.0)
                        .at(150),
                ],
            ),
            // We are done.
            (Up(3), &[END]),
        ],
    );
}

#[test]
fn scale_start_details_should_contain_the_correct_pointer_device_kind() {
    replay(
        "ScaleStartDetails should contain the correct PointerDeviceKind",
        Arena::Solo,
        defaults,
        &[
            // The default kind is touch.
            (Down(1, TOUCH, 0.0, 0.0, 0), &[START.kind(TOUCH)]),
            (Move(1, 20.0, 30.0, 0), &[UPDATE]),
            (Up(1), &[END]),
            (
                Down(2, PointerDevice::Mouse, 10.0, 20.0, 0),
                &[START.kind(PointerDevice::Mouse)],
            ),
            (Move(2, 20.0, 30.0, 0), &[UPDATE]),
            (Up(2), &[END]),
            // Flutter's stylus.
            (
                Down(3, PointerDevice::Pen, 10.0, 20.0, 0),
                &[START.kind(PointerDevice::Pen)],
            ),
            (Move(3, 20.0, 30.0, 0), &[UPDATE]),
            (Up(3), &[END]),
        ],
    );
}
