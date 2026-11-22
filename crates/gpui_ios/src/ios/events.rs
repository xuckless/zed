//! Conversion from UIKit events to GPUI input events.

use gpui::{
    BackGestureEvent, BackGesturePhase, BackGestureSource, Modifiers, Pixels, Point, PointerDevice,
    ScrollDelta, ScrollEvent, ScrollSource, TouchId, TouchPhase, px,
};
use objc2_core_foundation::CGPoint;
use objc2_ui_kit::{
    UIGestureRecognizerState, UIKeyModifierFlags, UIPanGestureRecognizer, UIRectEdge,
    UIScreenEdgePanGestureRecognizer, UITouch, UITouchPhase, UITouchType, UIView,
};

/// A release faster than this, in points per second toward going back, commits
/// a back gesture regardless of how far it travelled.
const BACK_COMMIT_VELOCITY: f64 = 300.;

/// Converts a step of the screen-edge back gesture, whose `Started` phase was
/// already delivered when the recognizer asked to begin.
///
/// Like `UINavigationController`, a release commits when the gesture is past
/// halfway without flicking back, or when it flicks forward quickly. Returns
/// `None` for states that carry no step.
pub fn back_event(
    recognizer: &UIScreenEdgePanGestureRecognizer,
    view: &UIView,
    start_position: Point<Pixels>,
) -> Option<BackGestureEvent> {
    // Gestures from the right edge move content leftward toward going back.
    let direction = if recognizer.edges() == UIRectEdge::Right {
        -1.
    } else {
        1.
    };
    let width = view.bounds().size.width.max(1.);
    let travel = recognizer.translationInView(Some(view)).x * direction;
    let progress = (travel / width).clamp(0., 1.);
    let phase = match recognizer.state() {
        UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed => {
            BackGesturePhase::Progressed
        }
        UIGestureRecognizerState::Ended => {
            let velocity = recognizer.velocityInView(Some(view)).x * direction;
            if velocity > BACK_COMMIT_VELOCITY
                || (progress > 0.5 && velocity > -BACK_COMMIT_VELOCITY)
            {
                BackGesturePhase::Committed
            } else {
                BackGesturePhase::Cancelled
            }
        }
        UIGestureRecognizerState::Cancelled | UIGestureRecognizerState::Failed => {
            BackGesturePhase::Cancelled
        }
        _ => return None,
    };
    Some(BackGestureEvent {
        phase,
        start_position,
        progress: progress as f32,
        source: BackGestureSource::EdgeSwipe,
    })
}

pub fn touch_location_in_view(touch: &UITouch, view: &UIView) -> Point<Pixels> {
    let location = touch.locationInView(Some(view));
    Point::new(px(location.x as f32), px(location.y as f32))
}

pub fn touch_phase(touch: &UITouch) -> TouchPhase {
    match touch.phase() {
        UITouchPhase::Began => TouchPhase::Started,
        UITouchPhase::Moved | UITouchPhase::Stationary => TouchPhase::Moved,
        UITouchPhase::Ended => TouchPhase::Ended,
        _ => TouchPhase::Cancelled,
    }
}

// Ported from flutter/flutter engine/src/flutter/lib/ui/window/pointer_data_packet_converter.cc
// (`UpdatePointerIdentifier`: each down takes `++pointer_`, kept until the device's up or cancel erases its state)
// and engine/src/flutter/shell/platform/darwin/ios/framework/Source/FlutterViewController.mm (`dispatchTouches:`,
// `pointer_data.device = reinterpret_cast<int64_t>(touch)`) @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
/// GPUI's ids for the touches UIKit has down, keyed by `UITouch` address as Flutter keys its device. UIKit reuses a
/// finished touch's object, and so its address, for a later touch, so an address names a touch only while it is
/// live: each began touch takes the next id (from 0, wrapping) and gives it up when it ends or is cancelled.
#[derive(Debug, Default)]
pub struct TouchIds {
    next: u64,
    live: Vec<(usize, TouchId)>,
}

impl TouchIds {
    /// The touches (id, phase) one UIKit sample makes. A move or end of an address with no live touch is dropped; a
    /// begin of one still live cancels that stale touch, then starts its own.
    pub fn track(&mut self, touch: usize, phase: TouchPhase) -> Vec<(TouchId, TouchPhase)> {
        let index = self.live.iter().position(|(live, _)| *live == touch);
        match (index, phase) {
            (None, TouchPhase::Started) => vec![(self.start(touch), phase)],
            (None, TouchPhase::Moved | TouchPhase::Ended | TouchPhase::Cancelled) => Vec::new(),
            (Some(index), TouchPhase::Started) => {
                let (_, stale) = self.live.remove(index);
                vec![(stale, TouchPhase::Cancelled), (self.start(touch), phase)]
            }
            (Some(index), TouchPhase::Moved) => vec![(self.live[index].1, phase)],
            (Some(index), TouchPhase::Ended | TouchPhase::Cancelled) => {
                vec![(self.live.remove(index).1, phase)]
            }
        }
    }

    fn start(&mut self, touch: usize) -> TouchId {
        let id = TouchId(self.next);
        self.next = self.next.wrapping_add(1);
        self.live.push((touch, id));
        id
    }
}

/// Converts one step of a trackpad or mouse-wheel scroll gesture.
///
/// The recognizer's translation is reset after each step, so every event carries
/// only the movement since the previous one. UIKit reports the translation in
/// the direction the content should move, which matches GPUI's pixel deltas.
/// Returns `None` for states that carry no scroll.
pub fn scroll_event(recognizer: &UIPanGestureRecognizer, view: &UIView) -> Option<ScrollEvent> {
    let touch_phase = match recognizer.state() {
        UIGestureRecognizerState::Began => TouchPhase::Started,
        UIGestureRecognizerState::Changed => TouchPhase::Moved,
        UIGestureRecognizerState::Ended => TouchPhase::Ended,
        UIGestureRecognizerState::Cancelled | UIGestureRecognizerState::Failed => {
            TouchPhase::Cancelled
        }
        _ => return None,
    };
    let translation = recognizer.translationInView(Some(view));
    recognizer.setTranslation_inView(CGPoint::ZERO, Some(view));
    let location = recognizer.locationInView(Some(view));
    Some(ScrollEvent {
        position: Point::new(px(location.x as f32), px(location.y as f32)),
        delta: ScrollDelta::Pixels(Point::new(
            px(translation.x as f32),
            px(translation.y as f32),
        )),
        modifiers: modifiers(recognizer.modifierFlags()),
        touch_phase,
        source: ScrollSource::Pointer,
    })
}

fn modifiers(flags: UIKeyModifierFlags) -> Modifiers {
    Modifiers {
        control: flags.contains(UIKeyModifierFlags::Control),
        alt: flags.contains(UIKeyModifierFlags::Alternate),
        shift: flags.contains(UIKeyModifierFlags::Shift),
        platform: flags.contains(UIKeyModifierFlags::Command),
        function: false,
    }
}

pub fn touch_force(touch: &UITouch) -> Option<f32> {
    let maximum_force = touch.maximumPossibleForce();
    if maximum_force <= 0.0 {
        return None;
    }
    Some((touch.force() / maximum_force).clamp(0.0, 1.0) as f32)
}

/// The device behind a touch: Apple Pencil is a pen, and an iPad pointer's
/// click arrives as an indirect-pointer touch.
pub fn touch_device(touch: &UITouch) -> PointerDevice {
    match touch.r#type() {
        UITouchType::Pencil => PointerDevice::Pen,
        UITouchType::IndirectPointer => PointerDevice::Mouse,
        // `Indirect` is a remote's touch surface: a finger too.
        UITouchType::Direct | UITouchType::Indirect => PointerDevice::Touch,
        // objc2 makes `UITouchType` an open newtype, so a match needs this arm; a
        // type UIKit adds later reads as a finger.
        _ => PointerDevice::Touch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_ids() {
        use TouchPhase::*;
        // (name, UITouch address, phase, touches out as (id, phase)): addresses are reused, live ids never are.
        let rows: [(&str, usize, TouchPhase, &[(u64, TouchPhase)]); 9] = [
            ("first finger down", 0x10, Started, &[(0, Started)]),
            ("second finger down", 0x20, Started, &[(1, Started)]),
            ("first finger moves", 0x10, Moved, &[(0, Moved)]),
            ("first finger lifts", 0x10, Ended, &[(0, Ended)]),
            (
                "its address reused by the next tap",
                0x10,
                Started,
                &[(2, Started)],
            ),
            (
                "second finger cancelled",
                0x20,
                Cancelled,
                &[(1, Cancelled)],
            ),
            (
                "a begin of a live address cancels its stale touch",
                0x10,
                Started,
                &[(2, Cancelled), (3, Started)],
            ),
            ("rejected: a move with no live begin", 0x30, Moved, &[]),
            ("rejected: an end with no live begin", 0x30, Ended, &[]),
        ];
        let mut ids = TouchIds::default();
        for (name, touch, phase, expected) in rows {
            let got: Vec<_> = ids
                .track(touch, phase)
                .into_iter()
                .map(|(id, phase)| (id.0, phase))
                .collect();
            assert_eq!(got, expected, "{name}");
        }
    }
}
