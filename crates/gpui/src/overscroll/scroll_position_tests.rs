//! Touch and trackpad overscroll through the test platform: a recorded input trace (the recognizer tests' touch
//! format, scroll events for wheels and trackpads) goes into a window holding scroll containers through
//! [`crate::play_trace`], and each frame's scroll offsets, stretch, glow and taps come out. One row per behaviour;
//! the expected values are what Flutter's physics, indicators and scroll activities do with the same input.

use std::{cell::Cell, ops::RangeInclusive, rc::Rc};

use crate::gestures::recognizer_tests::{TraceEvent, touch};
use crate::{
    AnyWindowHandle, AppContext as _, Context, GestureTuning, InputEvent as _,
    InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _,
    PlatformInput, Render, ScrollDelta, ScrollEvent, ScrollHandle, ScrollPhysics, ScrollSource,
    StatefulInteractiveElement as _, Styled as _, TestAppContext, TouchPhase, TraceStep, Window,
    div, point, px,
};

use super::Overscroll;
use TouchPhase::{Ended, Moved, Started};

/// A 100 × 200 vertical list (max offset 400 with its 19 rows) whose first child is a horizontal strip, 100 × 30, of
/// three 80 px cells (max offset 140); the rows below it are 30 px each. Beside it, `fit`: a 100 × 30 horizontal
/// strip at x 200 whose one 80 px cell fits. The root counts taps.
struct Scroller {
    outer: ScrollHandle,
    inner: ScrollHandle,
    fit: ScrollHandle,
    opt_out: Option<Overscroll>,
    rows: Rc<Cell<usize>>,
    taps: Rc<Cell<usize>>,
}

/// The row whose rendered position shows the stretch: its layout centre is 195 px below the list's top.
const STRETCH_PROBE: &str = "row-5";

impl Render for Scroller {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .id("outer")
            .h(px(200.))
            .w(px(100.))
            .overflow_y_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.outer);
        if let Some(overscroll) = self.opt_out {
            list = list.overscroll(overscroll);
        }
        let taps = self.taps.clone();
        div()
            .id("root")
            .size_full()
            .on_mouse_down(MouseButton::Left, move |_, _, _| taps.set(taps.get() + 1))
            .child(
                list.child(
                    div()
                        .id("inner")
                        .flex()
                        .h(px(30.))
                        .w_full()
                        .flex_none()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&self.inner)
                        .children((0..3).map(|_| div().flex_none().w(px(80.)).h_full())),
                )
                .children((0..self.rows.get()).map(|index| {
                    div()
                        .debug_selector(move || format!("row-{index}"))
                        .w_full()
                        .h(px(30.))
                        .flex_none()
                })),
            )
            .child(
                div()
                    .id("fit")
                    .absolute()
                    .left(px(200.))
                    .top(px(0.))
                    .flex()
                    .h(px(30.))
                    .w(px(100.))
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.fit)
                    .child(div().flex_none().w(px(80.)).h_full()),
            )
    }
}

#[derive(Clone, Copy)]
enum Input {
    Touch(TraceEvent),
    /// A mouse wheel notch's worth of pixels at the list's middle, at `at_ms`.
    Wheel {
        at_ms: u64,
        dy: f32,
    },
    /// A trackpad pan step at the list's middle: pixel deltas with the platform's phase.
    Trackpad {
        at_ms: u64,
        phase: TouchPhase,
        dy: f32,
    },
}

impl Input {
    fn timed(self) -> (u64, PlatformInput) {
        let scroll = |at_ms, phase, dy, delta: fn(f32) -> ScrollDelta| {
            let event = ScrollEvent {
                position: point(px(50.), px(100.)),
                delta: delta(dy),
                modifiers: Modifiers::default(),
                touch_phase: phase,
                source: ScrollSource::Pointer,
            };
            (at_ms, event.to_platform_input())
        };
        let pixels = |dy| ScrollDelta::Pixels(point(px(0.), px(dy)));
        match self {
            Self::Touch(event) => event.to_timed_input(),
            Self::Wheel { at_ms, dy } => scroll(at_ms, Moved, dy, pixels),
            Self::Trackpad { at_ms, phase, dy } => scroll(at_ms, phase, dy, pixels),
        }
    }
}

const fn t(at_ms: u64, phase: TouchPhase, x: f32, y: f32) -> Input {
    Input::Touch(touch(at_ms, 1, phase, x, y))
}

const fn pad(at_ms: u64, phase: TouchPhase, dy: f32) -> Input {
    Input::Trackpad { at_ms, phase, dy }
}

/// What the app does at a frame, before it is drawn.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// `ScrollHandle::set_offset` on the list.
    SetY(f32),
    /// The list now has this many rows.
    Rows(usize),
}

/// What one frame shows.
#[derive(Clone, Debug)]
enum Check {
    /// The list's offset (positive: pulled down past its top).
    Y(RangeInclusive<f32>),
    /// The strip's offset (positive: pulled right past its left edge).
    InnerX(RangeInclusive<f32>),
    /// The fitting strip's offset.
    FitX(RangeInclusive<f32>),
    /// How far the stretch moved [`STRETCH_PROBE`] down from where its layout puts it.
    Shift(RangeInclusive<f32>),
    /// How many glows the frame painted.
    Glows(RangeInclusive<usize>),
    /// How many taps the root saw so far.
    Taps(RangeInclusive<usize>),
}

struct Row<'a> {
    name: &'static str,
    tuning: GestureTuning,
    opt_out: Option<Overscroll>,
    trace: &'a [Input],
    actions: &'a [(u64, Action)],
    until_ms: u64,
    /// `(time in ms, what the last frame by then shows)`.
    checks: &'a [(u64, Check)],
    /// The furthest any container went past an edge over all frames (the list's 19-row edges).
    overshoot: RangeInclusive<f32>,
}

/// One frame's sample.
#[derive(Debug)]
struct Frame {
    at_ms: u64,
    y: f32,
    inner_x: f32,
    fit_x: f32,
    shift: f32,
    glows: usize,
    taps: usize,
}

const FRAME_MS: u64 = 16;

fn replay(row: &Row, cx: &mut TestAppContext) -> Vec<Frame> {
    let (outer, inner, fit) = (
        ScrollHandle::new(),
        ScrollHandle::new(),
        ScrollHandle::new(),
    );
    let (rows, taps) = (Rc::new(Cell::new(19)), Rc::new(Cell::new(0)));
    let window: AnyWindowHandle = cx
        .add_window({
            let (outer, inner, fit, opt_out) =
                (outer.clone(), inner.clone(), fit.clone(), row.opt_out);
            let (rows, taps) = (rows.clone(), taps.clone());
            move |_, _| Scroller {
                outer,
                inner,
                fit,
                opt_out,
                rows,
                taps,
            }
        })
        .into();
    let tuning = row.tuning;
    cx.update_window(window, |_, window, cx| {
        window.set_gesture_tuning(tuning);
        window.draw(cx).clear(cx);
    })
    .unwrap();
    let mut actions = row.actions.iter().peekable();
    let mut frames = Vec::new();
    let trace = row.trace.iter().map(|input| input.timed());
    crate::play_trace(
        cx,
        window,
        trace,
        Some(FRAME_MS),
        row.until_ms,
        |step, window, cx| {
            let TraceStep::Frame(at_ms) = step else {
                return;
            };
            while let Some((_, action)) = actions.next_if(|(at, _)| *at <= at_ms) {
                match action {
                    Action::SetY(y) => outer.set_offset(point(px(0.), px(*y))),
                    Action::Rows(count) => rows.set(*count),
                }
                window.refresh();
            }
            window.draw(cx).clear(cx);
            let y = f32::from(outer.offset().y);
            let probe = window.rendered_frame.debug_bounds[STRETCH_PROBE];
            frames.push(Frame {
                at_ms,
                y,
                inner_x: f32::from(inner.offset().x),
                fit_x: f32::from(fit.offset().x),
                shift: f32::from(probe.origin.y) - (180. + y),
                glows: window.rendered_frame.scene.paths.len(),
                taps: taps.get(),
            });
        },
    );
    frames
}

#[crate::test]
fn touch_overscroll_follows_flutter(cx: &mut TestAppContext) {
    let bounce = GestureTuning {
        overscroll: Overscroll::Bounce,
        ..GestureTuning::default()
    };
    let clamping = |overscroll| GestureTuning {
        scroll_physics: ScrollPhysics::android(),
        overscroll,
        ..GestureTuning::default()
    };
    // A slow drag 100 px down from the list's middle, held still before release: no fling.
    const PULL_DOWN: [Input; 7] = [
        t(0, Started, 50., 100.),
        t(16, Moved, 50., 120.),
        t(32, Moved, 50., 140.),
        t(48, Moved, 50., 160.),
        t(64, Moved, 50., 180.),
        t(80, Moved, 50., 200.),
        t(300, Ended, 50., 200.),
    ];
    // A fast swipe 120 px up from low in the list: it flings on toward the bottom.
    const FLING_DOWN: [Input; 6] = [
        t(0, Started, 50., 180.),
        t(16, Moved, 50., 150.),
        t(32, Moved, 50., 120.),
        t(48, Moved, 50., 90.),
        t(64, Moved, 50., 60.),
        t(64, Ended, 50., 60.),
    ];
    let rows = [
        Row {
            // BouncingScrollPhysics.applyPhysicsToUserOffset: 0.52 friction at the edge, falling as it goes.
            name: "Bounce: a drag past the top eases with Flutter's friction, then springs back",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &PULL_DOWN,
            until_ms: 1500,
            checks: &[(288, Check::Y(20.0..=60.0)), (1500, Check::Y(0.0..=0.0))],
            overshoot: 20.0..=60.0,
        },
        Row {
            // BouncingScrollSimulation: friction until the edge, then a spring past it and back.
            name: "Bounce: a fling into the bottom edge goes past it and springs back",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &FLING_DOWN,
            until_ms: 3000,
            checks: &[(3000, Check::Y(-400.0..=-400.0))],
            overshoot: 1.0..=200.0,
        },
        Row {
            name: "None: a drag stops hard at the top, as GPUI did",
            tuning: GestureTuning {
                overscroll: Overscroll::None,
                ..bounce
            },
            opt_out: None,
            actions: &[],
            trace: &PULL_DOWN,
            until_ms: 400,
            checks: &[(288, Check::Y(0.0..=0.0)), (288, Check::Shift(0.0..=0.0))],
            overshoot: 0.0..=0.0,
        },
        Row {
            name: "A container's .overscroll(None) opts it out of the tuning's bounce",
            tuning: bounce,
            opt_out: Some(Overscroll::None),
            actions: &[],
            trace: &PULL_DOWN,
            until_ms: 400,
            checks: &[(288, Check::Y(0.0..=0.0))],
            overshoot: 0.0..=0.0,
        },
        Row {
            // ClampingScrollPhysics refuses the overscroll; StretchingOverscrollIndicator pulls with it, anchored at
            // the top, and its spring recedes after release.
            name: "Stretch: clamping physics holds the edge, the content stretches from the top and recedes",
            tuning: clamping(Overscroll::Stretch),
            opt_out: None,
            actions: &[],
            trace: &PULL_DOWN,
            until_ms: 2000,
            checks: &[
                (288, Check::Y(0.0..=0.0)),
                (288, Check::Shift(1.0..=20.0)),
                (2000, Check::Shift(0.0..=0.0)),
            ],
            overshoot: 0.0..=0.0,
        },
        Row {
            name: "Glow: clamping physics holds the edge, the top glows and fades",
            tuning: clamping(Overscroll::Glow),
            opt_out: None,
            actions: &[],
            trace: &PULL_DOWN,
            until_ms: 3000,
            checks: &[
                (96, Check::Glows(1..=1)),
                (96, Check::Shift(0.0..=0.0)),
                (3000, Check::Glows(0..=0)),
            ],
            overshoot: 0.0..=0.0,
        },
        Row {
            // Flutter's PointerScrollEvent moves a scrollable within its extents only.
            name: "A wheel never overscrolls, even with bounce",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[Input::Wheel { at_ms: 0, dy: 50. }],
            until_ms: 100,
            checks: &[(100, Check::Y(0.0..=0.0))],
            overshoot: 0.0..=0.0,
        },
        Row {
            name: "Nested: a vertical drag on the strip overscrolls the list only",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                t(0, Started, 50., 15.),
                t(16, Moved, 50., 35.),
                t(32, Moved, 50., 55.),
                t(48, Moved, 50., 75.),
                t(64, Moved, 50., 95.),
                t(80, Moved, 50., 115.),
                t(300, Ended, 50., 115.),
            ],
            until_ms: 1500,
            checks: &[
                (288, Check::Y(20.0..=60.0)),
                (288, Check::InnerX(0.0..=0.0)),
                (1500, Check::Y(0.0..=0.0)),
            ],
            overshoot: 20.0..=60.0,
        },
        Row {
            name: "Nested: a horizontal drag on the strip overscrolls the strip only",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                t(0, Started, 10., 15.),
                t(16, Moved, 30., 15.),
                t(32, Moved, 50., 15.),
                t(48, Moved, 70., 15.),
                t(64, Moved, 90., 15.),
                t(80, Moved, 110., 15.),
                t(300, Ended, 110., 15.),
            ],
            until_ms: 1500,
            checks: &[
                (288, Check::InnerX(20.0..=60.0)),
                (288, Check::Y(0.0..=0.0)),
                (1500, Check::InnerX(0.0..=0.0)),
            ],
            overshoot: 20.0..=60.0,
        },
        Row {
            // ScrollHandle::set_offset is Flutter's jumpTo: goIdle, then goBallistic(0) from the new offset.
            name: "An offset set during a fling wins: the fling stops there",
            tuning: bounce,
            opt_out: None,
            actions: &[(160, Action::SetY(-100.))],
            trace: &FLING_DOWN,
            until_ms: 2000,
            checks: &[
                (176, Check::Y(-100.0..=-100.0)),
                (2000, Check::Y(-100.0..=-100.0)),
            ],
            overshoot: 0.0..=0.0,
        },
        Row {
            // HoldScrollActivity: a touch on a springing-back list stops it where it is; released unmoved, the
            // hold's cancel goes ballistic again and the spring finishes.
            name: "A touch holds a list springing back, and lets it go on release",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                PULL_DOWN[0],
                PULL_DOWN[1],
                PULL_DOWN[2],
                PULL_DOWN[3],
                PULL_DOWN[4],
                PULL_DOWN[5],
                PULL_DOWN[6],
                t(400, Started, 50., 100.),
                t(1000, Ended, 50., 100.),
            ],
            until_ms: 2500,
            checks: &[
                (400, Check::Y(17.0..=20.0)),
                (992, Check::Y(17.0..=20.0)),
                (992, Check::Taps(0..=0)),
                (2500, Check::Y(0.0..=0.0)),
                (2500, Check::Taps(0..=0)),
            ],
            overshoot: 20.0..=60.0,
        },
        Row {
            // The held touch drags from its first pixel: 6 px moves the content, under the 18 px slop.
            name: "A touch on a moving list drags it from the first pixel and never taps",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                PULL_DOWN[0],
                PULL_DOWN[1],
                PULL_DOWN[2],
                PULL_DOWN[3],
                PULL_DOWN[4],
                PULL_DOWN[5],
                PULL_DOWN[6],
                t(400, Started, 50., 100.),
                t(416, Moved, 50., 106.),
                t(432, Ended, 50., 106.),
            ],
            until_ms: 2500,
            checks: &[
                (400, Check::Y(17.0..=20.0)),
                (432, Check::Y(20.0..=22.0)),
                (448, Check::Taps(0..=0)),
                (2500, Check::Y(0.0..=0.0)),
            ],
            overshoot: 20.0..=60.0,
        },
        Row {
            // Flutter routes trackpad pans (PointerPanZoom) through the drag recognizer: bouncing friction past
            // the top, then the spring.
            name: "Bounce: a trackpad pan past the top eases with Flutter's friction, then springs back",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                pad(0, Started, 0.),
                pad(16, Moved, 20.),
                pad(32, Moved, 20.),
                pad(48, Moved, 20.),
                pad(64, Moved, 20.),
                pad(80, Moved, 20.),
                pad(300, Ended, 0.),
            ],
            until_ms: 1500,
            checks: &[(288, Check::Y(20.0..=60.0)), (1500, Check::Y(0.0..=0.0))],
            overshoot: 20.0..=60.0,
        },
        Row {
            // The pan's own velocity flings it (monodrag.dart `_checkEnd`); the platform's momentum steps after
            // the release are skipped, as Flutter's macOS embedder skips them.
            name: "Bounce: a trackpad fling bounces at the bottom; the platform's momentum is ignored",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                pad(0, Started, 0.),
                pad(16, Moved, -30.),
                pad(32, Moved, -30.),
                pad(48, Moved, -30.),
                pad(64, Moved, -30.),
                pad(64, Ended, 0.),
                pad(80, Moved, -10.),
                pad(96, Moved, -10.),
                pad(112, Moved, -5.),
            ],
            until_ms: 3000,
            checks: &[(3000, Check::Y(-400.0..=-400.0))],
            overshoot: 1.0..=200.0,
        },
        Row {
            // applyNewDimensions: the fling restarts with its velocity in the new range, so it bounces off the
            // new end instead of flying on to the old one.
            name: "Rows removed during a fling: it bounces at the new end",
            tuning: bounce,
            opt_out: None,
            actions: &[(96, Action::Rows(9))],
            trace: &FLING_DOWN,
            until_ms: 3000,
            checks: &[
                (600, Check::Y(-160.0..=-100.0)),
                (3000, Check::Y(-100.0..=-100.0)),
            ],
            overshoot: 0.0..=0.0,
        },
        Row {
            // shouldAcceptUserOffset: a horizontal scroll view with nothing to scroll takes no drag (only a
            // vertical primary one is always scrollable).
            name: "A horizontal strip whose content fits does not overscroll",
            tuning: bounce,
            opt_out: None,
            actions: &[],
            trace: &[
                t(0, Started, 210., 15.),
                t(16, Moved, 230., 15.),
                t(32, Moved, 250., 15.),
                t(48, Moved, 270., 15.),
                t(300, Ended, 270., 15.),
            ],
            until_ms: 500,
            checks: &[(288, Check::FitX(0.0..=0.0))],
            overshoot: 0.0..=0.0,
        },
    ];

    let mut failures = Vec::new();
    for row in &rows {
        let frames = replay(row, cx);
        let overshoot = frames
            .iter()
            .map(|frame| {
                frame
                    .y
                    .max(-frame.y - 400.)
                    .max(frame.inner_x)
                    .max(-frame.inner_x - 140.)
                    .max(frame.fit_x.abs())
                    .max(0.)
            })
            .fold(0f32, f32::max);
        if !row.overshoot.contains(&overshoot) {
            failures.push(format!(
                "{}: overshoot {overshoot}, expected {:?}",
                row.name, row.overshoot
            ));
        }
        for (at_ms, check) in row.checks {
            let Some(frame) = frames.iter().rev().find(|frame| frame.at_ms <= *at_ms) else {
                failures.push(format!("{}: no frame at {at_ms} ms", row.name));
                continue;
            };
            // Half a pixel: what Flutter's tolerance at a device pixel ratio of 2 calls settled.
            let near = |range: &RangeInclusive<f32>, value: f32| {
                *range.start() - 0.5 <= value && value <= *range.end() + 0.5
            };
            let holds = match check {
                Check::Y(range) => near(range, frame.y),
                Check::InnerX(range) => near(range, frame.inner_x),
                Check::FitX(range) => near(range, frame.fit_x),
                Check::Shift(range) => near(range, frame.shift),
                Check::Glows(range) => range.contains(&frame.glows),
                Check::Taps(range) => range.contains(&frame.taps),
            };
            if !holds {
                failures.push(format!(
                    "{}: at {at_ms} ms {check:?} failed: {frame:?}",
                    row.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
