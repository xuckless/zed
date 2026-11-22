//! Direct touches (a touchscreen Mac, or an iPad by Sidecar) as GPUI touches: the decisions, on plain values.
//! `touch_recognizer.rs` reads AppKit and acts on what these return.
//!
//! AppKit hands direct touches to gesture recognizers only, from macOS 27. Per `touches…WithEvent:` callback the
//! recognizer turns each changed `NSTouch` in that callback's phase into a touch phase (`PHASES`), gives it GPUI's
//! id (`TouchIds::track_callback`), and moves its own
//! `NSGestureRecognizer` state (`next_state`). While it holds touches, the mouse, scroll and magnify events AppKit
//! makes from them are dropped (`is_touch_copy`).

use gpui::{Pixels, Point, TouchEvent, TouchId, TouchPhase};
use objc2_app_kit::{NSEventSubtype, NSEventType};
use objc2_foundation::{NSInteger, NSUInteger};
use std::collections::VecDeque;

// `NSTouchPhase` bits (NSTouch.h).
pub(super) const BEGAN: NSUInteger = 1 << 0;
pub(super) const MOVED: NSUInteger = 1 << 1;
pub(super) const STATIONARY: NSUInteger = 1 << 2;
pub(super) const ENDED: NSUInteger = 1 << 3;
pub(super) const CANCELLED: NSUInteger = 1 << 4;
/// The touches still on the screen.
pub(super) const TOUCHING: NSUInteger = BEGAN | MOVED | STATIONARY;
/// The touches an event changed: stationary ones have nothing new for GPUI.
pub(super) const CHANGED: NSUInteger = BEGAN | MOVED | ENDED | CANCELLED;

/// AppKit touch phase → GPUI's. Extend: one row per `NSTouchPhase` bit in `CHANGED`.
pub(super) const PHASES: [(NSUInteger, TouchPhase); 4] = [
    (BEGAN, TouchPhase::Started),
    (MOVED, TouchPhase::Moved),
    (ENDED, TouchPhase::Ended),
    (CANCELLED, TouchPhase::Cancelled),
];

/// `NSGestureRecognizerState` (NSGestureRecognizer.h), as the recognizer reports it to AppKit.
#[repr(isize)] // NSInteger
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecognizerState {
    Possible = 0,
    Began = 1,
    Changed = 2,
    Ended = 3,
    Cancelled = 4,
    Failed = 5,
}

impl RecognizerState {
    const ALL: [Self; 6] = [
        Self::Possible,
        Self::Began,
        Self::Changed,
        Self::Ended,
        Self::Cancelled,
        Self::Failed,
    ];

    /// AppKit's `state`; `None` for a value NSGestureRecognizer.h does not define.
    pub(super) fn from_raw(state: NSInteger) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|known| *known as NSInteger == state)
    }
}

/// What an event left on the screen: fingers still touching, or none, the last lifted or cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Left {
    Touching,
    Lifted,
    Cancelled,
}

/// The recognizer's next state once an event is handled, `None` when it stays (NSGestureRecognizer.h's legal
/// transitions only). It claims the touch sequence from its first touch (GPUI recognizes the gestures itself) and
/// finishes when no finger is down; a sequence that ends before it began (a tap within one event) fails. AppKit
/// resets a finished recognizer to `Possible` itself, so nothing moves on from `Ended`, `Cancelled` or `Failed`.
pub(super) fn next_state(current: RecognizerState, left: Left) -> Option<RecognizerState> {
    use RecognizerState::*;
    match (current, left) {
        (Possible, Left::Touching) => Some(Began),
        (Possible, Left::Lifted | Left::Cancelled) => Some(Failed),
        (Began | Changed, Left::Touching) => Some(Changed),
        (Began | Changed, Left::Lifted) => Some(Ended),
        (Began | Changed, Left::Cancelled) => Some(Cancelled),
        (Ended | Cancelled | Failed, _) => None,
    }
}

/// GPUI's ids for the touches down now, with where each last was. `NSTouch` objects do not persist and their
/// `identity` may be reused by a later touch, so a new identity takes the next id (counting from 0, wrapping) and
/// gives it up when its touch ends. Generic so the id rules are tested without AppKit.
pub(super) struct TouchIds<K> {
    next: u64,
    live: Vec<(K, TouchId, Point<Pixels>)>,
}

impl<K> Default for TouchIds<K> {
    fn default() -> Self {
        Self {
            next: 0,
            live: Vec::new(),
        }
    }
}

impl<K: PartialEq> TouchIds<K> {
    /// The touches (id, phase, position) one `touches…WithEvent:` callback makes, for the callback's own
    /// `NSTouchPhase` bit and the event's changed touches as (identity, phase bit, position): only the touches in
    /// the callback's phase. AppKit calls every callback that one of the event's touches is in, each with the same
    /// event (a finger landing while another moves calls `touchesBegan` and `touchesMoved`), so taking every changed
    /// touch in each would handle it twice: the landing finger started, cancelled and started again, the moving one
    /// moved twice.
    pub(super) fn track_callback(
        &mut self,
        callback: NSUInteger,
        touches: impl IntoIterator<Item = (K, NSUInteger, Point<Pixels>)>,
    ) -> Vec<(TouchId, TouchPhase, Point<Pixels>)> {
        let Some(phase) = PHASES
            .iter()
            .find(|(bit, _)| *bit == callback)
            .map(|(_, phase)| *phase)
        else {
            return Vec::new();
        };
        touches
            .into_iter()
            .filter(|(_, bit, _)| *bit == callback)
            .flat_map(|(key, _, position)| self.track(key, phase, position))
            .collect()
    }

    /// The touches (id, phase, position) one changed `NSTouch` makes. A move or end of an identity with no live
    /// touch is dropped; a begin of one still live cancels that stale touch where it was, then starts its own. A
    /// cancel is where the touch last was: AppKit puts a touch the system took at no real place ((0, -32) in the
    /// window on a Sidecar display).
    fn track(
        &mut self,
        key: K,
        phase: TouchPhase,
        position: Point<Pixels>,
    ) -> Vec<(TouchId, TouchPhase, Point<Pixels>)> {
        let index = self.live.iter().position(|(live, _, _)| *live == key);
        match (index, phase) {
            (None, TouchPhase::Started) => vec![(self.start(key, position), phase, position)],
            (None, TouchPhase::Moved | TouchPhase::Ended | TouchPhase::Cancelled) => Vec::new(),
            (Some(index), TouchPhase::Started) => {
                let (_, stale, at) = self.live.remove(index);
                vec![
                    (stale, TouchPhase::Cancelled, at),
                    (self.start(key, position), phase, position),
                ]
            }
            (Some(index), TouchPhase::Moved) => {
                self.live[index].2 = position;
                vec![(self.live[index].1, phase, position)]
            }
            (Some(index), TouchPhase::Ended) => {
                let (_, id, _) = self.live.remove(index);
                vec![(id, phase, position)]
            }
            (Some(index), TouchPhase::Cancelled) => {
                let (_, id, at) = self.live.remove(index);
                vec![(id, phase, at)]
            }
        }
    }

    fn start(&mut self, key: K, position: Point<Pixels>) -> TouchId {
        let id = TouchId(self.next);
        self.next = self.next.wrapping_add(1);
        self.live.push((key, id, position));
        id
    }

    /// Cancels every live touch where it last was (AppKit reset the recognizer under them).
    pub(super) fn cancel_all(&mut self) -> Vec<(TouchId, Point<Pixels>)> {
        self.live.drain(..).map(|(_, id, at)| (id, at)).collect()
    }

    pub(super) fn is_touching(&self) -> bool {
        !self.live.is_empty()
    }
}

/// Touches on their way to GPUI's input callback. While the callback runs it cannot take another event (AppKit
/// re-enters the run loop, in a menu or modal loop say): an end or cancel then waits for it, or GPUI's touch would
/// stay open, and a start or move is dropped, as other input is then. gpui_windows has the same queue.
#[derive(Debug, Default)]
pub(super) struct TouchQueue(VecDeque<TouchEvent>);

impl TouchQueue {
    /// Queues `event`, for a callback that is free (`deliverable`) or busy.
    pub(super) fn push(&mut self, event: TouchEvent, deliverable: bool) {
        match (deliverable, event.phase) {
            (true, _) | (false, TouchPhase::Ended | TouchPhase::Cancelled) => {
                self.0.push_back(event)
            }
            (false, TouchPhase::Started | TouchPhase::Moved) => {}
        }
    }

    pub(super) fn pop(&mut self) -> Option<TouchEvent> {
        self.0.pop_front()
    }
}

/// How AppKit marks an event it made from a direct touch (an app without `NSIsTouchNative`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TouchCopy {
    /// A mouse event says so in `subtype` (`NSEventSubtypeTouch`).
    Subtype,
    /// A scroll or magnify has no subtype (NSEvent.h: valid for mouse and kit-defined events only), so one that
    /// arrives while the recognizer holds touches is taken as theirs.
    WhileTouching,
}

/// The events GPUIView handles that AppKit also makes from touches. GPUI makes its own mouse events from the
/// touches, so these copies are dropped while the recognizer holds touches, as gpui_windows drops `MI_WP_SIGNATURE`
/// copies and gpui_linux `XIPointerEmulated` ones. Extend: one row per event type GPUIView routes to GPUI.
const TOUCH_COPIES: [(NSEventType, TouchCopy); 12] = [
    (NSEventType::LeftMouseDown, TouchCopy::Subtype),
    (NSEventType::LeftMouseUp, TouchCopy::Subtype),
    (NSEventType::LeftMouseDragged, TouchCopy::Subtype),
    (NSEventType::RightMouseDown, TouchCopy::Subtype),
    (NSEventType::RightMouseUp, TouchCopy::Subtype),
    (NSEventType::RightMouseDragged, TouchCopy::Subtype),
    (NSEventType::OtherMouseDown, TouchCopy::Subtype),
    (NSEventType::OtherMouseUp, TouchCopy::Subtype),
    (NSEventType::OtherMouseDragged, TouchCopy::Subtype),
    (NSEventType::MouseMoved, TouchCopy::Subtype),
    (NSEventType::ScrollWheel, TouchCopy::WhileTouching),
    (NSEventType::Magnify, TouchCopy::WhileTouching),
];

/// Whether GPUIView drops an event as AppKit's copy of a touch. `subtype` is read only for the rows that carry one.
pub(super) fn is_touch_copy(
    event_type: NSEventType,
    touching: bool,
    subtype: impl FnOnce() -> NSEventSubtype,
) -> bool {
    let copy = TOUCH_COPIES
        .iter()
        .find(|(copied, _)| *copied == event_type)
        .map(|(_, copy)| *copy);
    match (touching, copy) {
        (false, _) | (true, None) => false,
        (true, Some(TouchCopy::Subtype)) => subtype() == NSEventSubtype::Touch,
        (true, Some(TouchCopy::WhileTouching)) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn touch_phases() {
        let stationary = PHASES.iter().find(|(bit, _)| *bit == STATIONARY);
        assert_eq!(stationary, None, "stationary is not a change");
    }

    /// One row per (state, what the event left).
    #[test]
    fn recognizer_states() {
        use RecognizerState::*;
        let rows = [
            (
                "first touch claims the sequence",
                Possible,
                Left::Touching,
                Some(Began),
            ),
            (
                "tap within one event fails",
                Possible,
                Left::Lifted,
                Some(Failed),
            ),
            (
                "cancelled before it began fails",
                Possible,
                Left::Cancelled,
                Some(Failed),
            ),
            ("touches go on", Began, Left::Touching, Some(Changed)),
            (
                "last finger lifts after the first event",
                Began,
                Left::Lifted,
                Some(Ended),
            ),
            (
                "cancelled after the first event",
                Began,
                Left::Cancelled,
                Some(Cancelled),
            ),
            (
                "a finger lifts, another stays",
                Changed,
                Left::Touching,
                Some(Changed),
            ),
            ("last finger lifts", Changed, Left::Lifted, Some(Ended)),
            (
                "system takes the touches",
                Changed,
                Left::Cancelled,
                Some(Cancelled),
            ),
            (
                "rejected: ended waits for AppKit's reset",
                Ended,
                Left::Touching,
                None,
            ),
            (
                "rejected: cancelled waits for AppKit's reset",
                Cancelled,
                Left::Lifted,
                None,
            ),
            (
                "rejected: failed waits for AppKit's reset",
                Failed,
                Left::Cancelled,
                None,
            ),
        ];
        for (name, current, left, expected) in rows {
            assert_eq!(next_state(current, left), expected, "{name}");
        }
        assert_eq!(RecognizerState::from_raw(5), Some(Failed), "raw failed");
        assert_eq!(RecognizerState::from_raw(6), None, "rejected: raw unknown");
    }

    #[test]
    fn touch_ids() {
        use TouchPhase::*;
        let at = |x: f32| point(px(x), px(0.));
        // (name, identity, phase, x, touches out as (id, phase, x)): identities are reused, live ids never are.
        let rows: [(&str, u32, TouchPhase, f32, &[(u64, TouchPhase, f32)]); 9] = [
            ("first finger down", 7, Started, 1., &[(0, Started, 1.)]),
            ("second finger down", 8, Started, 2., &[(1, Started, 2.)]),
            ("first finger moves", 7, Moved, 3., &[(0, Moved, 3.)]),
            ("first finger lifts", 7, Ended, 4., &[(0, Ended, 4.)]),
            (
                "identity reused by a new touch",
                7,
                Started,
                5.,
                &[(2, Started, 5.)],
            ),
            (
                "a begin of a live identity cancels its stale touch",
                8,
                Started,
                6.,
                &[(1, Cancelled, 2.), (3, Started, 6.)],
            ),
            (
                "a cancel is where the touch last was, not where AppKit says",
                7,
                Cancelled,
                0.,
                &[(2, Cancelled, 5.)],
            ),
            ("rejected: a move with no live begin", 9, Moved, 7., &[]),
            ("rejected: an end with no live begin", 9, Ended, 7., &[]),
        ];
        let mut ids = TouchIds::default();
        for (name, identity, phase, x, expected) in rows {
            let got: Vec<_> = ids
                .track(identity, phase, at(x))
                .into_iter()
                .map(|(id, phase, position)| (id.0, phase, position.x.as_f32()))
                .collect();
            assert_eq!(got, expected, "{name}");
        }
        let cancelled: Vec<_> = ids
            .cancel_all()
            .into_iter()
            .map(|(id, position)| (id.0, position.x.as_f32()))
            .collect();
        assert_eq!(cancelled, [(3, 6.)], "reset cancels the live touches");
        assert!(!ids.is_touching(), "and forgets them");
    }

    /// The owner's Sidecar logs (macOS 27.3, Wave 13), replayed: one row per `touches…WithEvent:` call, with the
    /// event's changed touches as (identity, phase bit, x, y) and the touches out as (id, phase, x, y). Every
    /// touch is handled once though AppKit calls a callback per phase with the same event, and every id started
    /// ends or is cancelled.
    #[test]
    fn appkit_callbacks() {
        use TouchPhase::*;
        type Touches<'a> = &'a [(u32, NSUInteger, f32, f32)];
        type Out<'a> = &'a [(u64, TouchPhase, f32, f32)];
        let rows: [(&str, NSUInteger, Touches, Out); 12] = [
            (
                "first finger down",
                BEGAN,
                &[(1, BEGAN, 256., 441.)],
                &[(0, Started, 256., 441.)],
            ),
            (
                "second finger lands while the first moves: begin callback (13:54:14.606)",
                BEGAN,
                &[(1, MOVED, 256., 441.), (2, BEGAN, 127.5, 402.)],
                &[(1, Started, 127.5, 402.)],
            ),
            (
                "same event, move callback: no second start, one move",
                MOVED,
                &[(1, MOVED, 256., 441.), (2, BEGAN, 127.5, 402.)],
                &[(0, Moved, 256., 441.)],
            ),
            (
                "a finger lifts while the other moves: end callback (13:57:14.251)",
                ENDED,
                &[(1, ENDED, 272., 217.5), (2, MOVED, 145.5, 166.5)],
                &[(0, Ended, 272., 217.5)],
            ),
            (
                "same event, move callback: one move",
                MOVED,
                &[(1, ENDED, 272., 217.5), (2, MOVED, 145.5, 166.5)],
                &[(1, Moved, 145.5, 166.5)],
            ),
            (
                "last finger lifts",
                ENDED,
                &[(2, ENDED, 147.5, 164.5)],
                &[(1, Ended, 147.5, 164.5)],
            ),
            (
                "three fingers land (Spotlight swipe, 13:51:11)",
                BEGAN,
                &[
                    (3, BEGAN, 226.5, 256.5),
                    (4, BEGAN, 324.5, 326.5),
                    (5, BEGAN, 83., 248.5),
                ],
                &[
                    (2, Started, 226.5, 256.5),
                    (3, Started, 324.5, 326.5),
                    (4, Started, 83., 248.5),
                ],
            ),
            (
                "they move",
                MOVED,
                &[
                    (3, MOVED, 234., 247.5),
                    (4, MOVED, 336., 313.5),
                    (5, MOVED, 92.5, 244.5),
                ],
                &[
                    (2, Moved, 234., 247.5),
                    (3, Moved, 336., 313.5),
                    (4, Moved, 92.5, 244.5),
                ],
            ),
            (
                "the system cancels them at (0, -32): each where it last was",
                CANCELLED,
                &[
                    (3, CANCELLED, 0., -32.),
                    (4, CANCELLED, 0., -32.),
                    (5, CANCELLED, 0., -32.),
                ],
                &[
                    (2, Cancelled, 234., 247.5),
                    (3, Cancelled, 336., 313.5),
                    (4, Cancelled, 92.5, 244.5),
                ],
            ),
            (
                "rejected: a move callback with only began touches",
                MOVED,
                &[(6, BEGAN, 1., 1.)],
                &[],
            ),
            (
                "rejected: an end callback with only began touches",
                ENDED,
                &[(6, BEGAN, 1., 1.)],
                &[],
            ),
            (
                "rejected: no callback has the stationary phase",
                STATIONARY,
                &[(6, STATIONARY, 1., 1.)],
                &[],
            ),
        ];
        let mut ids = TouchIds::default();
        for (name, callback, touches, expected) in rows {
            let touches = touches
                .iter()
                .map(|&(identity, bit, x, y)| (identity, bit, point(px(x), px(y))));
            let got: Vec<_> = ids
                .track_callback(callback, touches)
                .into_iter()
                .map(|(id, phase, at)| (id.0, phase, at.x.as_f32(), at.y.as_f32()))
                .collect();
            assert_eq!(got, expected, "{name}");
        }
        assert!(
            !ids.is_touching(),
            "every started id ended or was cancelled"
        );
    }

    /// One row per (callback free or busy × phase): what GPUI gets, in order, once the callback is free.
    #[test]
    fn touch_queue() {
        use TouchPhase::{Cancelled, Ended, Moved, Started};
        let rows: [(&str, &[(TouchPhase, bool)], &[TouchPhase]); 3] = [
            (
                "free: every touch, in order",
                &[(Started, true), (Moved, true), (Ended, true)],
                &[Started, Moved, Ended],
            ),
            (
                "busy: ends and cancels wait",
                &[(Ended, false), (Cancelled, false)],
                &[Ended, Cancelled],
            ),
            (
                "rejected: busy starts and moves are dropped",
                &[(Started, false), (Moved, false), (Ended, true)],
                &[Ended],
            ),
        ];
        for (name, pushes, expected) in rows {
            let mut queue = TouchQueue::default();
            for &(phase, deliverable) in pushes {
                let event = TouchEvent {
                    phase,
                    ..TouchEvent::default()
                };
                queue.push(event, deliverable);
            }
            let got: Vec<_> = std::iter::from_fn(|| queue.pop())
                .map(|event| event.phase)
                .collect();
            assert_eq!(got, expected, "{name}");
        }
    }

    #[test]
    fn touch_copies() {
        let touch = NSEventSubtype::Touch;
        let mouse = NSEventSubtype::MouseEvent;
        let rows = [
            (
                "a tap's mouse down",
                NSEventType::LeftMouseDown,
                true,
                touch,
                true,
            ),
            (
                "a long press's right click",
                NSEventType::RightMouseUp,
                true,
                touch,
                true,
            ),
            (
                "a pan's scroll",
                NSEventType::ScrollWheel,
                true,
                mouse,
                true,
            ),
            ("a pinch's magnify", NSEventType::Magnify, true, mouse, true),
            (
                "rejected: a real mouse mid-touch",
                NSEventType::LeftMouseDown,
                true,
                mouse,
                false,
            ),
            (
                "rejected: no touches held",
                NSEventType::LeftMouseDown,
                false,
                touch,
                false,
            ),
            (
                "rejected: a scroll with no touches held",
                NSEventType::ScrollWheel,
                false,
                mouse,
                false,
            ),
            ("rejected: a key", NSEventType::KeyDown, true, touch, false),
        ];
        for (name, event_type, touching, subtype, dropped) in rows {
            assert_eq!(
                is_touch_copy(event_type, touching, || subtype),
                dropped,
                "{name}"
            );
        }
    }
}
