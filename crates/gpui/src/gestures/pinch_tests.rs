//! Touch pinches through the test platform: recorded touch traces into a window of real `on_pinch` divs, the
//! pinch, scroll, tap and long-press events each element saw out. One row per case of the arena (module docs of
//! `pinch.rs`); a row whose outcome Flutter's arena decides names the Flutter code that decides it.

use std::{cell::RefCell, rc::Rc};

use super::super::recognizer_tests::{TraceEvent, touch};
use crate::{
    Context, DispatchPhase, InteractiveElement as _, IntoElement, LongPressEvent, MouseButton,
    ParentElement as _, Pixels, Point, Render, Styled as _, TestAppContext, TouchPhase, TouchPinch,
    Window, canvas, div, px,
};

use TouchPhase::{Cancelled, Ended, Moved, Started};

/// What an element saw, values rounded to hundredths.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seen {
    /// A tap's synthesized press.
    Tap,
    /// A pan step's phase.
    Scroll(TouchPhase),
    /// A long press's phase; the root claims every long press.
    LongPress(TouchPhase),
    /// `on_pinch` of the named element: `Start` with its pointer count and focal point.
    Start(&'static str, usize, (f32, f32)),
    /// `Update`: scale, rotation, pointer count, local focal point.
    Update(&'static str, f32, f32, usize, (f32, f32)),
    /// `End`: pointer count.
    End(&'static str, usize),
}

use Seen::{End, LongPress, Scroll, Start, Tap, Update};

struct Row<'a> {
    name: &'static str,
    trace: &'a [TraceEvent],
    expected: &'a [Seen],
}

/// `outer` (0, 0, 400 × 400) pinches and holds `inner` (200, 0, 200 × 200), which pinches too; `plain`
/// (400, 0, 200 × 400) does not. The root records taps and pans, and claims long presses.
struct Pinchable {
    seen: Rc<RefCell<Vec<Seen>>>,
}

fn round(value: f32) -> f32 {
    (value * 100.).round() / 100.
}

fn rounded(point: Point<Pixels>) -> (f32, f32) {
    (round(point.x.into()), round(point.y.into()))
}

impl Render for Pinchable {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pinchable = |name: &'static str, left: f32| {
            let seen = self.seen.clone();
            div()
                .id(name)
                .absolute()
                .left(px(left))
                .top(px(0.))
                .w(px(200.))
                .h(px(200.))
                .on_pinch(move |event, _, _| {
                    let seen_now = match &event.touch {
                        Some(TouchPinch::Start(start)) => {
                            Start(name, start.pointer_count, rounded(start.focal_point))
                        }
                        Some(TouchPinch::Update(update)) => Update(
                            name,
                            round(update.scale),
                            round(update.rotation),
                            update.pointer_count,
                            rounded(update.local_focal_point),
                        ),
                        Some(TouchPinch::End(end)) => End(name, end.pointer_count),
                        None => return,
                    };
                    seen.borrow_mut().push(seen_now);
                })
        };
        let (on_tap, on_scroll) = (self.seen.clone(), self.seen.clone());
        let on_long_press = self.seen.clone();
        div()
            .id("root")
            .size_full()
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                on_tap.borrow_mut().push(Tap)
            })
            .on_scroll(move |event, _, _| on_scroll.borrow_mut().push(Scroll(event.touch_phase)))
            .child(canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    window.on_mouse_event(move |event: &LongPressEvent, phase, window, _| {
                        if phase == DispatchPhase::Bubble {
                            on_long_press.borrow_mut().push(LongPress(event.phase));
                            window.prevent_default();
                        }
                    })
                },
            ))
            .child(
                pinchable("outer", 0.)
                    .w(px(400.))
                    .h(px(400.))
                    .child(pinchable("inner", 200.)),
            )
            .child(
                div()
                    .id("plain")
                    .absolute()
                    .left(px(400.))
                    .top(px(0.))
                    .w(px(200.))
                    .h(px(400.)),
            )
    }
}

#[crate::test]
fn touch_pinches_follow_flutters_arena(cx: &mut TestAppContext) {
    // Two fingers 40 apart on `outer` (focal point (60, 100), span 20) that pinch out to 80 apart.
    const PINCH_OUT: [TraceEvent; 3] = [
        touch(0, 1, Started, 40., 100.),
        touch(0, 2, Started, 80., 100.),
        touch(10, 2, Moved, 120., 100.),
    ];
    let rows = [
        Row {
            // The span grows by 20, past the scale slop (18): the scale wins and starts at once.
            name: "two fingers pinch out",
            trace: &[
                PINCH_OUT[0],
                PINCH_OUT[1],
                PINCH_OUT[2],
                touch(20, 2, Moved, 160., 100.),
                touch(30, 1, Ended, 40., 100.),
                touch(30, 2, Ended, 160., 100.),
            ],
            expected: &[
                Start("outer", 2, (80., 100.)),
                Update("outer", 2., 0., 2, (80., 100.)),
                Update("outer", 3., 0., 2, (100., 100.)),
                End("outer", 1),
            ],
        },
        Row {
            // On `inner`, which `outer` holds: the deepest wins, and the focal point is local to it.
            name: "two fingers pinch in on the nested element",
            trace: &[
                touch(0, 1, Started, 220., 100.),
                touch(0, 2, Started, 300., 100.),
                touch(10, 2, Moved, 260., 100.),
                touch(20, 1, Ended, 220., 100.),
                touch(20, 2, Ended, 260., 100.),
            ],
            expected: &[
                Start("inner", 2, (240., 100.)),
                Update("inner", 0.5, 0., 2, (40., 100.)),
                End("inner", 1),
            ],
        },
        Row {
            // The second finger swings from left of the first to above it: a quarter turn, and its focal point
            // moves 36.06, past the pan slop (36).
            name: "two fingers rotate",
            trace: &[
                touch(0, 1, Started, 140., 100.),
                touch(0, 2, Started, 100., 100.),
                touch(10, 2, Moved, 140., 40.),
                touch(20, 1, Ended, 140., 100.),
                touch(20, 2, Ended, 140., 40.),
            ],
            expected: &[
                Start("outer", 2, (140., 70.)),
                Update("outer", 1.5, 1.57, 2, (140., 70.)),
                End("outer", 1),
            ],
        },
        Row {
            // A third finger ends the gesture and its first move starts it again, as in Flutter.
            name: "a third finger joins the pinch",
            trace: &[
                PINCH_OUT[0],
                PINCH_OUT[1],
                PINCH_OUT[2],
                touch(20, 3, Started, 80., 100.),
                touch(30, 3, Moved, 80., 130.),
                touch(40, 1, Ended, 40., 100.),
                touch(40, 2, Ended, 120., 100.),
                touch(40, 3, Ended, 80., 130.),
            ],
            expected: &[
                Start("outer", 2, (80., 100.)),
                Update("outer", 2., 0., 2, (80., 100.)),
                End("outer", 3),
                Start("outer", 3, (80., 110.)),
                Update("outer", 1.28, 0., 3, (80., 110.)),
                End("outer", 2),
            ],
        },
        Row {
            // The last finger keeps the gesture: Flutter's one-pointer scale, panning at scale 1.
            name: "a finger lifted mid-pinch",
            trace: &[
                PINCH_OUT[0],
                PINCH_OUT[1],
                PINCH_OUT[2],
                touch(20, 2, Ended, 120., 100.),
                touch(30, 1, Moved, 30., 100.),
                touch(40, 1, Ended, 30., 100.),
            ],
            expected: &[
                Start("outer", 2, (80., 100.)),
                Update("outer", 2., 0., 2, (80., 100.)),
                End("outer", 1),
                Start("outer", 1, (30., 100.)),
                Update("outer", 1., 0., 1, (30., 100.)),
                End("outer", 0),
            ],
        },
        Row {
            // The pan won its arena before the second finger landed; Flutter's drag accepts a pointer that joins
            // it (monodrag.dart `_addPointer`, case accepted), so the scale never gets one: no pinch.
            name: "a running pan never becomes a pinch",
            trace: &[
                touch(0, 1, Started, 40., 100.),
                touch(10, 1, Moved, 40., 140.),
                touch(20, 2, Started, 80., 140.),
                touch(30, 2, Moved, 160., 140.),
                touch(1000, 1, Ended, 40., 140.),
                touch(1000, 2, Ended, 160., 140.),
            ],
            expected: &[Scroll(Started), Scroll(Ended)],
        },
        Row {
            // Both fingers move 20 down together: the focal point travels 20, past the pan's hit slop (18) but
            // under the scale's pan slop (36), and the span does not change. The pan wins both arenas.
            name: "two fingers dragged together pan",
            trace: &[
                touch(0, 1, Started, 40., 100.),
                touch(0, 2, Started, 80., 100.),
                touch(10, 1, Moved, 40., 120.),
                touch(10, 2, Moved, 80., 120.),
                touch(20, 1, Moved, 40., 160.),
                touch(20, 2, Moved, 80., 160.),
                touch(1000, 1, Ended, 40., 160.),
                touch(1000, 2, Ended, 80., 160.),
            ],
            expected: &[Scroll(Started), Scroll(Moved), Scroll(Ended)],
        },
        Row {
            // Each finger moves 20 outward in turn: the first passes the pan's hit slop (18) and pans, but the
            // focal point travels only 10, so the drag has not won (Flutter's drag adds up every pointer's
            // movement, and these cancel); the span grows past the scale slop and the pinch takes both.
            name: "fingers spread apart pinch though the first passed the hit slop",
            trace: &[
                touch(0, 1, Started, 60., 100.),
                touch(0, 2, Started, 100., 100.),
                touch(10, 1, Moved, 40., 100.),
                touch(10, 2, Moved, 120., 100.),
                touch(20, 1, Moved, 20., 100.),
                touch(20, 2, Moved, 140., 100.),
                touch(30, 1, Ended, 20., 100.),
                touch(30, 2, Ended, 140., 100.),
            ],
            expected: &[
                Scroll(Started),
                Scroll(Cancelled),
                Start("outer", 2, (80., 100.)),
                Update("outer", 2., 0., 2, (80., 100.)),
                Update("outer", 2.5, 0., 2, (70., 100.)),
                Update("outer", 3., 0., 2, (80., 100.)),
                End("outer", 1),
            ],
        },
        Row {
            // LongPressGestureRecognizer.didExceedDeadline accepts in both pointers' arenas (long_press.dart,
            // recognizer.dart `resolve`), so the scale is rejected: spreading the fingers afterwards never pinches.
            name: "two fingers held past the long-press deadline long-press",
            trace: &[
                touch(0, 1, Started, 40., 100.),
                touch(0, 2, Started, 80., 100.),
                touch(600, 2, Moved, 160., 100.),
                touch(700, 1, Ended, 40., 100.),
                touch(700, 2, Ended, 160., 100.),
            ],
            expected: &[LongPress(Started), LongPress(Ended)],
        },
        Row {
            // The long press won the first finger's arena before the second landed: the second is the scale's
            // alone (a one-pointer scale, which wins past the pan slop of 36), and the long press goes on.
            name: "a finger landing during a long press pans a one-finger pinch",
            trace: &[
                touch(0, 1, Started, 40., 100.),
                touch(600, 2, Started, 80., 100.),
                touch(610, 2, Moved, 80., 140.),
                touch(620, 2, Moved, 80., 160.),
                touch(700, 2, Ended, 80., 160.),
                touch(800, 1, Ended, 40., 100.),
            ],
            expected: &[
                LongPress(Started),
                Start("outer", 1, (80., 140.)),
                Update("outer", 1., 0., 1, (80., 140.)),
                Update("outer", 1., 0., 1, (80., 160.)),
                End("outer", 0),
                LongPress(Ended),
            ],
        },
        Row {
            // The first finger is on `outer` only, the second on `inner` inside it: both are in `outer`'s
            // scale's arena (every scale recognizer on a hit path holds the pointer), so `outer` pinches.
            name: "a finger on each of two nested pinch elements pinches the outer one",
            trace: &[
                touch(0, 1, Started, 100., 300.),
                touch(0, 2, Started, 300., 100.),
                touch(10, 2, Moved, 340., 60.),
                touch(20, 1, Ended, 100., 300.),
                touch(20, 2, Ended, 340., 60.),
            ],
            expected: &[
                Start("outer", 2, (220., 180.)),
                Update("outer", 1.2, 0., 2, (220., 180.)),
                End("outer", 1),
            ],
        },
        Row {
            // Neither finger moves: the first finger's tap wins, as Flutter's tap does at its up.
            name: "a two-finger tap taps once",
            trace: &[
                touch(0, 1, Started, 40., 100.),
                touch(0, 2, Started, 80., 100.),
                touch(50, 1, Ended, 40., 100.),
                touch(50, 2, Ended, 80., 100.),
            ],
            expected: &[Tap],
        },
        Row {
            // Lifting back to one finger before the slop: the scale gives up and that finger pans as today.
            name: "one finger left before the slop pans",
            trace: &[
                touch(0, 1, Started, 40., 100.),
                touch(0, 2, Started, 80., 100.),
                touch(10, 2, Ended, 80., 100.),
                touch(20, 1, Moved, 40., 160.),
                touch(1000, 1, Ended, 40., 160.),
            ],
            expected: &[Scroll(Started), Scroll(Ended)],
        },
        Row {
            // `plain` has no `on_pinch`: today's behaviour, the first finger pans and the second is ignored.
            name: "two fingers pan without on_pinch",
            trace: &[
                touch(0, 1, Started, 450., 100.),
                touch(0, 2, Started, 500., 100.),
                touch(10, 2, Moved, 540., 100.),
                touch(20, 1, Moved, 450., 160.),
                touch(1000, 1, Ended, 450., 160.),
                touch(1000, 2, Ended, 540., 100.),
            ],
            expected: &[Scroll(Started), Scroll(Ended)],
        },
    ];
    let failures: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let window = cx.add_window({
                let seen = seen.clone();
                move |_, _| Pinchable { seen }
            });
            let trace = row.trace.iter().map(|event| event.to_timed_input());
            crate::play_trace(cx, window.into(), trace, None, 0, |_, _, _| {});
            let seen = seen.take();
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
