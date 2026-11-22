// Ported from flutter/flutter packages/flutter/test/gestures/tap_test.dart, double_tap_test.dart,
// long_press_test.dart and drag_test.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

//! Flutter's recognizer tests as the oracle for the touch recognizer, driven
//! through the test platform: a recorded touch trace goes into a window, the
//! gestures the window dispatches come out. One row per Flutter test; its name
//! is the row's. Flutter's arena (`closeArena`, `sweep`) has no counterpart:
//! the recognizer resolves a touch by claims.

use std::{cell::RefCell, rc::Rc, time::Duration};

use scheduler::Instant;

use crate::{
    App, Bounds, Context, DispatchPhase, GestureTuning, HitboxBehavior, InputEvent as _,
    IntoElement, LongPressEvent, MouseDownEvent, Pixels, PointerDevice, Render, ScrollDelta,
    ScrollEvent, TestAppContext, TouchDragEvent, TouchEvent, TouchId, TouchPhase, Window, canvas,
    point, px, size,
};

use TouchPhase::{Cancelled, Ended, Moved, Started};

/// One touch event of a recorded trace: what the platform reported, and when,
/// in milliseconds from the trace's start. Wave 12's recorder emits these.
#[derive(Clone, Copy)]
pub(crate) struct TraceEvent {
    pub(crate) at_ms: u64,
    id: u64,
    phase: TouchPhase,
    x: f32,
    y: f32,
}

pub(crate) const fn touch(at_ms: u64, id: u64, phase: TouchPhase, x: f32, y: f32) -> TraceEvent {
    TraceEvent {
        at_ms,
        id,
        phase,
        x,
        y,
    }
}

/// A gesture the window dispatched, as the element under the touch saw it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seen {
    /// A tap's synthesized press, with its click count: 2 is a double tap.
    Tap(usize),
    /// A pan step: its phase and scroll delta.
    Scroll(TouchPhase, f32, f32),
    /// A claimed long press: its phase and the touch's position.
    LongPress(TouchPhase, f32, f32),
    /// A claimed touch drag: its phase and the touch's position.
    TouchDrag(TouchPhase, f32, f32),
}

use Seen::{LongPress, Scroll, Tap, TouchDrag};

/// What the element claims, standing in for the recognizer the Flutter test
/// builds. Taps and pans are always recognized; a long press or touch drag
/// only reaches an element that claims it.
#[derive(Clone, Copy)]
enum Claim {
    Nothing,
    LongPress,
    TouchDrag,
}

struct Row<'a> {
    name: &'static str,
    tuning: GestureTuning,
    claim: Claim,
    /// Hitboxes the element inserts: what a tap lands on (none: every tap
    /// lands on nothing, the same target).
    targets: &'a [Bounds<Pixels>],
    trace: &'a [TraceEvent],
    /// The clock runs on to here after the trace, for timers still pending.
    until_ms: u64,
    /// Each gesture with when it was dispatched, in milliseconds.
    expected: &'a [(u64, Seen)],
}

struct Recorder {
    claim: Claim,
    targets: Vec<Bounds<Pixels>>,
    started_at: Instant,
    seen: Rc<RefCell<Vec<(u64, Seen)>>>,
}

impl Render for Recorder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (seen, started_at, claim) = (self.seen.clone(), self.started_at, self.claim);
        let targets = self.targets.clone();
        let record = move |cx: &App, gesture| {
            let at = cx.background_executor().now().duration_since(started_at);
            seen.borrow_mut().push((at.as_millis() as u64, gesture));
        };
        canvas(
            move |_, window, _| {
                for bounds in targets {
                    window.insert_hitbox(bounds, HitboxBehavior::Normal);
                }
            },
            move |_, _, window, _| {
                let on_tap = record.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_tap(cx, Tap(event.click_count));
                    }
                });
                let on_scroll = record.clone();
                window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx| {
                    let ScrollDelta::Pixels(delta) = event.delta else {
                        return;
                    };
                    if phase == DispatchPhase::Bubble {
                        on_scroll(
                            cx,
                            Scroll(event.touch_phase, delta.x.into(), delta.y.into()),
                        );
                    }
                });
                match claim {
                    Claim::Nothing => {}
                    Claim::LongPress => {
                        window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let at = event.position;
                            record(cx, LongPress(event.phase, at.x.into(), at.y.into()));
                            window.prevent_default();
                        })
                    }
                    Claim::TouchDrag => {
                        window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let at = event.position;
                            record(cx, TouchDrag(event.phase, at.x.into(), at.y.into()));
                            window.prevent_default();
                        })
                    }
                }
            },
        )
    }
}

/// Replays `row.trace` into a fresh window and returns what it dispatched.
fn replay(row: &Row, cx: &mut TestAppContext) -> Vec<(u64, Seen)> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let started_at = cx.executor().now();
    let window = cx.add_window({
        let seen = seen.clone();
        let claim = row.claim;
        let targets = row.targets.to_vec();
        move |_, _| Recorder {
            claim,
            targets,
            started_at,
            seen,
        }
    });
    let tuning = row.tuning;
    window
        .update(cx, |_, window, _| window.set_gesture_tuning(tuning))
        .unwrap();
    let trace = row.trace.iter().map(|event| event.to_timed_input());
    crate::play_trace(cx, window.into(), trace, None, row.until_ms, |_, _, _| {});
    seen.take()
}

impl TraceEvent {
    /// The platform event this trace event replays.
    pub(crate) fn to_touch_event(self) -> TouchEvent {
        TouchEvent {
            id: TouchId(self.id),
            phase: self.phase,
            position: point(px(self.x), px(self.y)),
            predicted_position: None,
            force: None,
            device: PointerDevice::Touch,
        }
    }

    /// The event as [`crate::play_trace`] input: its time and the platform input.
    pub(crate) fn to_timed_input(self) -> (u64, crate::PlatformInput) {
        (self.at_ms, self.to_touch_event().to_platform_input())
    }
}

/// Two side-by-side tap targets for the per-target rows.
const BUTTON_A: Bounds<Pixels> = Bounds {
    origin: point(px(0.), px(0.)),
    size: size(px(50.), px(50.)),
};
const BUTTON_B: Bounds<Pixels> = Bounds {
    origin: point(px(60.), px(0.)),
    size: size(px(50.), px(50.)),
};

#[crate::test]
fn flutter_recognizer_tests_pass(cx: &mut TestAppContext) {
    // Flutter's constants.dart values, which every row starts from.
    let flutter = GestureTuning::default();
    let rows = [
        // tap_test.dart. down1/up1 at (10, 10)/(11, 9); down2/up2 at (30, 30)/(31, 29).
        Row {
            name: "Should recognize tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[touch(0, 1, Started, 10., 10.), touch(0, 1, Ended, 11., 9.)],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        Row {
            // Two taps, the second within kDoubleTapMinTime: not a double tap.
            name: "No duplicate tap events",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
            ],
            until_ms: 0,
            expected: &[(0, Tap(1)), (0, Tap(1))],
        },
        Row {
            name: "Should not recognize two overlapping taps (FIFO)",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 2, Started, 30., 30.),
                touch(0, 1, Ended, 11., 9.),
                touch(0, 2, Ended, 31., 29.),
            ],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        Row {
            name: "Should not recognize two overlapping taps (FILO)",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 2, Started, 30., 30.),
                touch(0, 2, Ended, 31., 29.),
                touch(0, 1, Ended, 11., 9.),
            ],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        Row {
            // Moves ~21 px, more than kTouchSlop: a pan, never a tap.
            name: "Distance cancels tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 3, Started, 10., 10.),
                touch(0, 3, Moved, 25., 25.),
                touch(0, 3, Ended, 25., 25.),
            ],
            until_ms: 0,
            expected: &[(0, Scroll(Started, 0., 15.)), (0, Scroll(Ended, 0., 0.))],
        },
        Row {
            // Moves ~17 px, less than kTouchSlop.
            name: "Short distance does not cancel tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 4, Started, 10., 10.),
                touch(0, 4, Moved, 22., 22.),
                touch(0, 4, Ended, 22., 22.),
            ],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        Row {
            name: "Timeout does not cancel tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(500, 1, Ended, 11., 9.),
            ],
            until_ms: 500,
            expected: &[(500, Tap(1))],
        },
        Row {
            name: "PointerCancelEvent cancels tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(5000, 5, Cancelled, 10., 10.),
            ],
            until_ms: 5000,
            expected: &[],
        },
        Row {
            name: "PointerCancelEvent after exceeding deadline cancels tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(1000, 5, Cancelled, 10., 10.),
            ],
            until_ms: 1000,
            expected: &[],
        },
        Row {
            name: "losing tap gesture recognizer does not send onTapCancel",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[touch(0, 3, Started, 10., 10.), touch(0, 3, Moved, 25., 25.)],
            until_ms: 0,
            expected: &[(0, Scroll(Started, 0., 15.))],
        },
        Row {
            name: "non-primary pointers does not trigger timeout",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 2, Started, 30., 30.),
                touch(0, 1, Ended, 11., 9.),
                touch(200, 2, Ended, 31., 29.),
            ],
            until_ms: 200,
            expected: &[(0, Tap(1))],
        },
        Row {
            name: "A second tap after rejection is ignored",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 0., 0.),
                touch(0, 1, Moved, 50., 0.),
                touch(0, 2, Started, 10., 20.),
            ],
            until_ms: 0,
            expected: &[(0, Scroll(Started, 50., 0.))],
        },
        // double_tap_test.dart. down1/up1 (10, 10)/(11, 9); down2/up2 (12, 12)/(13, 11);
        // down3/up3 (130, 130)/(131, 129); down4/move4/up4 (10, 10)/(25, 25)/(25, 25);
        // down5/up5 as down1/up1.
        Row {
            name: "Should recognize double tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(100, 2, Started, 12., 12.),
                touch(100, 2, Ended, 13., 11.),
            ],
            until_ms: 100,
            expected: &[(0, Tap(1)), (100, Tap(2))],
        },
        Row {
            name: "Inter-tap distance cancels double tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(0, 3, Started, 130., 130.),
                touch(0, 3, Ended, 131., 129.),
            ],
            until_ms: 0,
            expected: &[(0, Tap(1)), (0, Tap(1))],
        },
        Row {
            name: "Intra-tap distance cancels double tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 4, Started, 10., 10.),
                touch(0, 4, Moved, 25., 25.),
                touch(0, 4, Ended, 25., 25.),
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
            ],
            until_ms: 0,
            expected: &[
                (0, Scroll(Started, 0., 15.)),
                (0, Scroll(Ended, 0., 0.)),
                (0, Tap(1)),
            ],
        },
        Row {
            name: "Inter-tap delay cancels double tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(5000, 2, Started, 12., 12.),
                touch(5000, 2, Ended, 13., 11.),
            ],
            until_ms: 5000,
            expected: &[(0, Tap(1)), (5000, Tap(1))],
        },
        Row {
            name: "Inter-tap delay resets double tap, allowing third tap to be a double-tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(5000, 2, Started, 12., 12.),
                touch(5000, 2, Ended, 13., 11.),
                touch(5100, 5, Started, 10., 10.),
                touch(5100, 5, Ended, 11., 9.),
            ],
            until_ms: 5100,
            expected: &[(0, Tap(1)), (5000, Tap(1)), (5100, Tap(2))],
        },
        Row {
            name: "Intra-tap delay does not cancel double tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(1000, 1, Ended, 11., 9.),
                touch(1000, 2, Started, 12., 12.),
                touch(1000, 2, Ended, 13., 11.),
            ],
            until_ms: 1000,
            expected: &[(1000, Tap(1)), (1000, Tap(2))],
        },
        Row {
            name: "Should not recognize two overlapping taps",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 2, Started, 12., 12.),
                touch(0, 1, Ended, 11., 9.),
                touch(0, 2, Ended, 13., 11.),
            ],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        Row {
            name: "Should recognize one tap of group followed by second tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 2, Started, 12., 12.),
                touch(0, 1, Ended, 11., 9.),
                touch(0, 2, Ended, 13., 11.),
                touch(100, 1, Started, 10., 10.),
                touch(100, 1, Ended, 11., 9.),
            ],
            until_ms: 100,
            expected: &[(0, Tap(1)), (100, Tap(2))],
        },
        Row {
            name: "Should not recognize two over-rapid taps",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(10, 2, Started, 12., 12.),
                touch(10, 2, Ended, 13., 11.),
            ],
            until_ms: 10,
            expected: &[(0, Tap(1)), (10, Tap(1))],
        },
        Row {
            name: "Over-rapid taps resets double tap, allowing third tap to be a double-tap",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 11., 9.),
                touch(10, 2, Started, 12., 12.),
                touch(10, 2, Ended, 13., 11.),
                touch(110, 5, Started, 10., 10.),
                touch(110, 5, Ended, 11., 9.),
            ],
            until_ms: 110,
            expected: &[(0, Tap(1)), (10, Tap(1)), (110, Tap(2))],
        },
        // long_press_test.dart. down/up (10, 10)/(11, 9), move (100, 200).
        Row {
            name: "Long press: Should recognize long press",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.)],
            until_ms: 1000,
            expected: &[(500, LongPress(Started, 10., 10.))],
        },
        Row {
            name: "Long press: Should recognize long press with altered duration",
            tuning: GestureTuning {
                long_press_duration: Duration::from_millis(100),
                ..flutter
            },
            claim: Claim::LongPress,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.)],
            until_ms: 100,
            expected: &[(100, LongPress(Started, 10., 10.))],
        },
        Row {
            // The touch that didn't last long enough is a tap.
            name: "Long press: Up cancels long press",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(300, 5, Ended, 11., 9.),
            ],
            until_ms: 1300,
            expected: &[(300, Tap(1))],
        },
        Row {
            name: "Long press: Moving before accept cancels",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(300, 5, Moved, 100., 200.),
                touch(1300, 5, Ended, 11., 9.),
            ],
            until_ms: 1600,
            expected: &[
                (300, Scroll(Started, 0., 190.)),
                (1300, Scroll(Ended, 0., -191.)),
            ],
        },
        Row {
            name: "Long press: Moving after accept is ok",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(1000, 5, Moved, 100., 200.),
                touch(1000, 5, Ended, 11., 9.),
            ],
            until_ms: 1300,
            expected: &[
                (500, LongPress(Started, 10., 10.)),
                (1000, LongPress(Moved, 100., 200.)),
                (1000, LongPress(Ended, 11., 9.)),
            ],
        },
        Row {
            name: "Long press: Should recognize long press up",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(1000, 5, Ended, 11., 9.),
            ],
            until_ms: 1000,
            expected: &[
                (500, LongPress(Started, 10., 10.)),
                (1000, LongPress(Ended, 11., 9.)),
            ],
        },
        Row {
            name: "long press drag: Should recognize long press down",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.)],
            until_ms: 1000,
            expected: &[(500, LongPress(Started, 10., 10.))],
        },
        Row {
            name: "long press drag: Short up cancels long press",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(300, 5, Ended, 11., 9.),
            ],
            until_ms: 1300,
            expected: &[(300, Tap(1))],
        },
        Row {
            name: "long press drag: Moving before accept cancels",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(300, 5, Moved, 100., 200.),
                touch(1300, 5, Ended, 11., 9.),
            ],
            until_ms: 1600,
            expected: &[
                (300, Scroll(Started, 0., 190.)),
                (1300, Scroll(Ended, 0., -191.)),
            ],
        },
        Row {
            name: "long press drag: Moving after accept does not cancel",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(1000, 5, Moved, 100., 200.),
                touch(1000, 5, Ended, 11., 9.),
            ],
            until_ms: 1300,
            expected: &[
                (500, LongPress(Started, 10., 10.)),
                (1000, LongPress(Moved, 100., 200.)),
                (1000, LongPress(Ended, 11., 9.)),
            ],
        },
        Row {
            name: "A primary long press recognizer forms competition with a primary tap recognizer",
            tuning: flutter,
            claim: Claim::LongPress,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.), touch(0, 5, Ended, 11., 9.)],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        // drag_test.dart. A drag recognizer alone in its arena wins at the down
        // event: the element claims the touch drag. With a competitor it waits
        // for the slop: a pan.
        Row {
            // A tap competes: the pan starts once the touch passes kTouchSlop
            // (14 px no, 22 px yes) and carries the whole movement so far.
            name: "Should recognize pan",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(0, 5, Moved, 20., 20.),
                touch(0, 5, Moved, 20., 30.),
                touch(0, 5, Moved, 20., 25.),
                touch(0, 5, Ended, 20., 25.),
            ],
            until_ms: 0,
            expected: &[
                (0, Scroll(Started, 0., 20.)),
                (0, Scroll(Moved, 0., -5.)),
                (0, Scroll(Ended, 0., 0.)),
            ],
        },
        Row {
            name: "Should report most recent point to onStart by default",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.), touch(0, 5, Moved, 30., 0.)],
            until_ms: 0,
            expected: &[(0, Scroll(Started, 20., 0.))],
        },
        Row {
            name: "Should report most recent point to onStart with a start configuration",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.), touch(0, 5, Moved, 30., 0.)],
            until_ms: 0,
            expected: &[(0, Scroll(Started, 20., 0.))],
        },
        Row {
            // The horizontal drag ignores the vertical delta: (20, 0).
            name: "Should report initial down point to onStart with a down configuration",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[touch(0, 5, Started, 10., 10.), touch(0, 5, Moved, 30., 0.)],
            until_ms: 0,
            expected: &[(0, Scroll(Started, 20., 0.))],
        },
        Row {
            name: "Should recognize drag (line 137)",
            tuning: flutter,
            claim: Claim::TouchDrag,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(0, 5, Moved, 20., 25.),
                touch(0, 5, Moved, 20., 25.),
                touch(0, 5, Ended, 20., 25.),
            ],
            until_ms: 0,
            expected: &[
                (0, TouchDrag(Started, 10., 10.)),
                (0, TouchDrag(Moved, 20., 25.)),
                (0, TouchDrag(Moved, 20., 25.)),
                (0, TouchDrag(Ended, 20., 25.)),
            ],
        },
        Row {
            name: "DragGestureRecognizer.onStart behavior test (no competing)",
            tuning: flutter,
            claim: Claim::TouchDrag,
            targets: &[],
            trace: &[
                touch(100, 5, Started, 10., 10.),
                touch(200, 5, Moved, 20., 25.),
                touch(300, 5, Moved, 20., 25.),
                touch(300, 5, Ended, 20., 25.),
            ],
            until_ms: 300,
            expected: &[
                (100, TouchDrag(Started, 10., 10.)),
                (200, TouchDrag(Moved, 20., 25.)),
                (300, TouchDrag(Moved, 20., 25.)),
                (300, TouchDrag(Ended, 20., 25.)),
            ],
        },
        Row {
            name: "DragGestureRecognizer.onStart behavior test (with competing)",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[
                touch(600, 5, Started, 10., 10.),
                touch(700, 5, Moved, 30., 10.),
                touch(700, 5, Ended, 30., 10.),
            ],
            until_ms: 700,
            expected: &[
                (700, Scroll(Started, 20., 0.)),
                (700, Scroll(Ended, 0., 0.)),
            ],
        },
        Row {
            name: "Should report original timestamps",
            tuning: flutter,
            claim: Claim::TouchDrag,
            targets: &[],
            trace: &[
                touch(100, 5, Started, 10., 10.),
                touch(200, 5, Moved, 20., 25.),
                touch(300, 5, Moved, 20., 25.),
            ],
            until_ms: 300,
            expected: &[
                (100, TouchDrag(Started, 10., 10.)),
                (200, TouchDrag(Moved, 20., 25.)),
                (300, TouchDrag(Moved, 20., 25.)),
            ],
        },
        Row {
            name: "Should recognize drag (line 1603)",
            tuning: flutter,
            claim: Claim::TouchDrag,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(0, 5, Moved, 20., 25.),
                touch(0, 5, Moved, 20., 25.),
                touch(0, 5, Ended, 20., 25.),
            ],
            until_ms: 0,
            expected: &[
                (0, TouchDrag(Started, 10., 10.)),
                (0, TouchDrag(Moved, 20., 25.)),
                (0, TouchDrag(Moved, 20., 25.)),
                (0, TouchDrag(Ended, 20., 25.)),
            ],
        },
        Row {
            name: "Should recognize drag (line 1665)",
            tuning: flutter,
            claim: Claim::TouchDrag,
            targets: &[],
            trace: &[
                touch(0, 5, Started, 10., 10.),
                touch(0, 5, Moved, 20., 25.),
                touch(0, 5, Moved, 0., 45.),
                touch(0, 5, Ended, 0., 45.),
            ],
            until_ms: 0,
            expected: &[
                (0, TouchDrag(Started, 10., 10.)),
                (0, TouchDrag(Moved, 20., 25.)),
                (0, TouchDrag(Moved, 0., 45.)),
                (0, TouchDrag(Ended, 0., 45.)),
            ],
        },
        Row {
            name: "A primary pan recognizer forms competition with a primary tap recognizer",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[],
            trace: &[touch(0, 1, Started, 10., 10.), touch(0, 1, Ended, 10., 10.)],
            until_ms: 0,
            expected: &[(0, Tap(1))],
        },
        // Not a Flutter test: Flutter counts double taps per widget, GPUI's
        // one recognizer per window counts them per target (the innermost
        // hitbox). Two 50 px buttons side by side, taps 60 px apart.
        Row {
            name: "Double tap on one target counts 2",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[BUTTON_A, BUTTON_B],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 10., 10.),
                touch(100, 2, Started, 40., 10.),
                touch(100, 2, Ended, 40., 10.),
            ],
            until_ms: 100,
            expected: &[(0, Tap(1)), (100, Tap(2))],
        },
        Row {
            name: "A tap on another target starts a new count",
            tuning: flutter,
            claim: Claim::Nothing,
            targets: &[BUTTON_A, BUTTON_B],
            trace: &[
                touch(0, 1, Started, 10., 10.),
                touch(0, 1, Ended, 10., 10.),
                touch(100, 2, Started, 70., 10.),
                touch(100, 2, Ended, 70., 10.),
            ],
            until_ms: 100,
            expected: &[(0, Tap(1)), (100, Tap(1))],
        },
    ];
    let failures: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let seen = replay(row, cx);
            (seen != row.expected).then(|| {
                format!(
                    "{}\n  expected {:?}\n  seen     {:?}",
                    row.name, row.expected, seen
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} rows failed:\n{}",
        failures.len(),
        rows.len(),
        failures.join("\n")
    );
}
