//! Touch and pen from Windows' pointer messages (`WM_POINTERDOWN`, `WM_POINTERUPDATE`, `WM_POINTERUP`) as GPUI
//! touches. The decisions live here, on plain values, so they are tested on any host (Parallax's
//! `scripts/patch-test.sh` builds this file on its own); `events.rs` reads the OS and dispatches what they return.
//!
//! Ported from rust-windowing/winit src/platform_impl/windows/event_loop.rs (`WM_POINTERDOWN | WM_POINTERUPDATE |
//! WM_POINTERUP`, `normalize_pointer_pressure`) @ v0.30.13 (e9809ef54b18499bb4f2cac945719ecc2a61061b). Copyright
//! the winit contributors. Apache-2.0.
//!
//! Ported from flutter/flutter engine/src/flutter/shell/platform/windows/flutter_window.cc
//! (`GetFlutterPointerDeviceKind`, `WM_POINTERUPDATE` without contact as a hover) @ 3.50.1
//! (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
//! third_party/flutter/LICENSE.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use gpui::{Pixels, Point, PointerDevice, TouchEvent, TouchId, TouchPhase};

// Values from WinUser.h, written out so this file builds without the `windows` crate; `events.rs` asserts each
// against it.
pub(crate) const PT_TOUCH: i32 = 2;
pub(crate) const PT_PEN: i32 = 3;
pub(crate) const POINTER_FLAG_INCONTACT: u32 = 0x4;
pub(crate) const POINTER_FLAG_CANCELED: u32 = 0x8000;
pub(crate) const POINTER_FLAG_DOWN: u32 = 0x1_0000;
pub(crate) const POINTER_FLAG_UPDATE: u32 = 0x2_0000;
pub(crate) const POINTER_FLAG_UP: u32 = 0x4_0000;
pub(crate) const WM_MOUSEMOVE: u32 = 0x0200;
pub(crate) const WM_LBUTTONDOWN: u32 = 0x0201;
pub(crate) const WM_LBUTTONUP: u32 = 0x0202;
pub(crate) const WM_RBUTTONDOWN: u32 = 0x0204;
pub(crate) const WM_RBUTTONUP: u32 = 0x0205;
pub(crate) const WM_MBUTTONDOWN: u32 = 0x0207;
pub(crate) const WM_MBUTTONUP: u32 = 0x0208;
pub(crate) const WM_XBUTTONDOWN: u32 = 0x020B;
pub(crate) const WM_XBUTTONUP: u32 = 0x020C;

/// Pointer type → the GPUI device it is. Types not listed (mouse, touchpad) stay with the mouse messages and
/// Direct Manipulation, as before. To extend, add a row.
const DEVICES: [(i32, PointerDevice); 2] = [
    (PT_TOUCH, PointerDevice::Touch),
    (PT_PEN, PointerDevice::Pen),
];

/// The GPUI device for a pointer type, `None` when GPUI leaves it to the mouse messages.
pub(crate) fn device(pointer_type: i32) -> Option<PointerDevice> {
    DEVICES
        .iter()
        .find(|(ty, _)| *ty == pointer_type)
        .map(|(_, device)| *device)
}

/// What one pointer sample is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    /// In contact: a touch phase.
    Contact(TouchPhase),
    /// A pen in range above the screen, not touching it (Flutter: a pointer move with no buttons).
    Hover,
}

/// Pointer flags → what the sample is: `(mask, value)`, the first row whose masked flags equal `value` wins. winit's
/// order (down, up, update); Windows marks a cancelled pointer's up `POINTER_FLAG_CANCELED`, so that row comes first.
/// Flags matching no row (a bare `POINTER_FLAG_NEW`, …) are dropped. To extend, add a row in priority order.
const SAMPLES: [(u32, u32, Sample); 5] = [
    (
        POINTER_FLAG_CANCELED,
        POINTER_FLAG_CANCELED,
        Sample::Contact(TouchPhase::Cancelled),
    ),
    (
        POINTER_FLAG_DOWN,
        POINTER_FLAG_DOWN,
        Sample::Contact(TouchPhase::Started),
    ),
    (
        POINTER_FLAG_UP,
        POINTER_FLAG_UP,
        Sample::Contact(TouchPhase::Ended),
    ),
    (
        POINTER_FLAG_UPDATE | POINTER_FLAG_INCONTACT,
        POINTER_FLAG_UPDATE | POINTER_FLAG_INCONTACT,
        Sample::Contact(TouchPhase::Moved),
    ),
    (
        POINTER_FLAG_UPDATE | POINTER_FLAG_INCONTACT,
        POINTER_FLAG_UPDATE,
        Sample::Hover,
    ),
];

/// `POINTER_TOUCH_INFO` / `POINTER_PEN_INFO` pressure (0 to 1024; 0 when the digitizer reports none) as GPUI's
/// normalized force (winit's `normalize_pointer_pressure`).
fn force(pressure: u32) -> Option<f32> {
    match pressure {
        1..=1024 => Some(pressure as f32 / 1024.0),
        _ => None,
    }
}

/// One pointer sample: a `POINTER_INFO` (with its touch or pen pressure), as plain values.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PointerSample {
    pub pointer_id: u32,
    pub pointer_type: i32,
    pub pointer_flags: u32,
    /// In the window, in logical pixels.
    pub position: Point<Pixels>,
    pub pressure: u32,
    pub time_stamp: Option<Instant>,
}

/// What GPUI gets for a pointer sample.
#[derive(Clone, Debug)]
pub(crate) enum PointerInput {
    Touch(TouchEvent),
    /// A pen hovering at the sample's position: a mouse move, as Windows' own promoted move would have been.
    Hover,
}

/// A pointer in contact with this window, from its down to its up or cancel.
#[derive(Clone, Copy, Debug)]
struct Contact {
    pointer_id: u32,
    touch: TouchId,
    device: PointerDevice,
    position: Point<Pixels>,
}

/// The pointers in contact with this window. A pointer is down exactly while it is listed. GPUI touch ids count from
/// 0 and wrap, never reused while live; Windows' pointer ids are reused.
#[derive(Debug, Default)]
pub(crate) struct Contacts {
    next_touch: u64,
    down: Vec<Contact>,
}

impl Contacts {
    /// GPUI's input for one sample, in order; empty when GPUI gets nothing: a pointer type left to the mouse
    /// messages, flags no row matches, or a move, up or cancel for a pointer that never went down in this window
    /// (it went down on the title bar or in another window).
    pub(crate) fn input(&mut self, sample: PointerSample) -> Vec<PointerInput> {
        let Some(device) = device(sample.pointer_type) else {
            return Vec::new();
        };
        let Some(kind) = SAMPLES
            .iter()
            .find(|(mask, value, _)| sample.pointer_flags & mask == *value)
            .map(|(_, _, kind)| *kind)
        else {
            return Vec::new();
        };
        let index = self
            .down
            .iter()
            .position(|contact| contact.pointer_id == sample.pointer_id);
        let force = force(sample.pressure);
        let touch = |contact: Contact, phase| {
            PointerInput::Touch(contact.event(phase, force, sample.time_stamp))
        };
        // pointer state × sample → next state and input; the only place a contact starts or ends.
        match (index, kind) {
            (None, Sample::Hover) => vec![PointerInput::Hover],
            (None, Sample::Contact(TouchPhase::Started)) => {
                vec![touch(self.start(&sample, device), TouchPhase::Started)]
            }
            (
                None,
                Sample::Contact(TouchPhase::Moved | TouchPhase::Ended | TouchPhase::Cancelled),
            ) => Vec::new(),
            // A down or a hover of a pointer still down: its up never reached this window, so GPUI's open touch is
            // cancelled where it last was before the new down starts its own touch, or the hover moves the mouse.
            (Some(index), Sample::Contact(TouchPhase::Started)) => {
                let stale = self.down.remove(index);
                vec![
                    PointerInput::Touch(stale.event(TouchPhase::Cancelled, None, None)),
                    touch(self.start(&sample, device), TouchPhase::Started),
                ]
            }
            (Some(index), Sample::Hover) => {
                let stale = self.down.remove(index);
                vec![
                    PointerInput::Touch(stale.event(TouchPhase::Cancelled, None, None)),
                    PointerInput::Hover,
                ]
            }
            (Some(index), Sample::Contact(TouchPhase::Moved)) => {
                self.down[index].position = sample.position;
                vec![touch(self.down[index], TouchPhase::Moved)]
            }
            (Some(index), Sample::Contact(TouchPhase::Ended)) => {
                let mut contact = self.down.remove(index);
                contact.position = sample.position;
                vec![touch(contact, TouchPhase::Ended)]
            }
            // Where it last was, as for a lost capture: Windows moves a cancelled pointer's location (a display
            // change cancels it off the window).
            (Some(index), Sample::Contact(TouchPhase::Cancelled)) => {
                vec![touch(self.down.remove(index), TouchPhase::Cancelled)]
            }
        }
    }

    /// A new contact for `sample`'s pointer, with the next touch id.
    fn start(&mut self, sample: &PointerSample, device: PointerDevice) -> Contact {
        let contact = Contact {
            pointer_id: sample.pointer_id,
            touch: TouchId(self.next_touch),
            device,
            position: sample.position,
        };
        self.next_touch = self.next_touch.wrapping_add(1);
        self.down.push(contact);
        contact
    }

    /// The cancel for a pointer this window lost (`WM_POINTERCAPTURECHANGED`) while it was down, at its last
    /// position; `None` when it was not down.
    pub(crate) fn capture_lost(&mut self, pointer_id: u32) -> Option<TouchEvent> {
        let index = self
            .down
            .iter()
            .position(|contact| contact.pointer_id == pointer_id)?;
        Some(
            self.down
                .remove(index)
                .event(TouchPhase::Cancelled, None, None),
        )
    }
}

impl Contact {
    fn event(
        self,
        phase: TouchPhase,
        force: Option<f32>,
        time_stamp: Option<Instant>,
    ) -> TouchEvent {
        TouchEvent {
            id: self.touch,
            phase,
            position: self.position,
            predicted_position: None,
            force,
            device: self.device,
            time_stamp,
        }
    }
}

/// Mouse messages Windows also makes from touch and pen. Those arrive as `WM_POINTER` touches and pen hovers, so
/// GPUI drops these copies and a tap fires once. Non-client messages stay: a touch on a title bar still moves the
/// window. To extend, add a client-area message GPUI handles.
const PROMOTED_MOUSE_MESSAGES: [u32; 9] = [
    WM_MOUSEMOVE,
    WM_LBUTTONDOWN,
    WM_LBUTTONUP,
    WM_RBUTTONDOWN,
    WM_RBUTTONUP,
    WM_MBUTTONDOWN,
    WM_MBUTTONUP,
    WM_XBUTTONDOWN,
    WM_XBUTTONUP,
];

/// `GetMessageExtraInfo` of a mouse message Windows made from touch or pen (`MI_WP_SIGNATURE`; bit 7 set for touch).
const TOUCH_OR_PEN_SIGNATURE: isize = 0xFF51_5700;
const SIGNATURE_MASK: isize = 0xFFFF_FF00;

/// Whether `msg`, with its `GetMessageExtraInfo`, is a mouse message Windows made from touch or pen
/// (Flutter's `GetFlutterPointerDeviceKind` is not `kFlutterPointerDeviceKindMouse`).
pub(crate) fn is_promoted_mouse_message(msg: u32, extra_info: isize) -> bool {
    PROMOTED_MOUSE_MESSAGES.contains(&msg) && extra_info & SIGNATURE_MASK == TOUCH_OR_PEN_SIGNATURE
}

/// Whether a `WM_POINTERLEAVE` ends GPUI's hover: a pen out of contact (its hovers were mouse moves) that left while
/// the real cursor is off the client area, as Flutter's `OnPointerLeave` ends a hovering pointer. A finger ends with
/// its up, and a mouse has its own `WM_MOUSELEAVE`.
pub(crate) fn pen_leave_is_mouse_leave(
    pointer_type: i32,
    pointer_flags: u32,
    cursor_in_client: bool,
) -> bool {
    matches!(
        (
            device(pointer_type),
            pointer_flags & POINTER_FLAG_INCONTACT != 0,
            cursor_in_client
        ),
        (Some(PointerDevice::Pen), false, false)
    )
}

/// Touches on their way to GPUI's input callback. While the callback runs it cannot take another event (it re-enters
/// the message loop, in a modal loop say): an end or cancel then waits for it, or GPUI's touch would stay open, and
/// a start or move is dropped, as other input is then.
#[derive(Debug, Default)]
pub(crate) struct TouchQueue(VecDeque<TouchEvent>);

impl TouchQueue {
    /// Queues `event`, for a callback that is free (`deliverable`) or busy.
    pub(crate) fn push(&mut self, event: TouchEvent, deliverable: bool) {
        match (deliverable, event.phase) {
            (true, _) | (false, TouchPhase::Ended | TouchPhase::Cancelled) => {
                self.0.push_back(event)
            }
            (false, TouchPhase::Started | TouchPhase::Moved) => {}
        }
    }

    pub(crate) fn pop(&mut self) -> Option<TouchEvent> {
        self.0.pop_front()
    }
}

/// GPUI's clock read together with Windows' two pointer clocks, so a `POINTER_INFO`'s time becomes a GPUI `Instant`
/// by its age.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    pub now: Instant,
    /// `QueryPerformanceCounter` at `now`, and its ticks per second (`QueryPerformanceFrequency`).
    pub performance_count: u64,
    pub performance_frequency: u64,
    /// `GetTickCount` at `now`, in milliseconds: `dwTime`'s clock.
    pub tick_count: u32,
}

impl Clock {
    /// When a sample happened, on GPUI's clock: by its `PerformanceCount` when set (high resolution), else by its
    /// `dwTime`; `None` when Windows set neither. A time after the clock read counts as now.
    pub(crate) fn at(self, performance_count: u64, time: u32) -> Option<Instant> {
        let age = if performance_count != 0 && self.performance_frequency != 0 {
            let ticks = u128::from(self.performance_count.saturating_sub(performance_count));
            Duration::from_nanos(
                (ticks * 1_000_000_000 / u128::from(self.performance_frequency)) as u64,
            )
        } else if time != 0 {
            // dwTime wraps every 49.7 days; a difference read as signed survives the wrap.
            let age_ms = self.tick_count.wrapping_sub(time) as i32;
            Duration::from_millis(age_ms.max(0) as u64)
        } else {
            return None;
        };
        self.now.checked_sub(age)
    }
}

/// Where a pointer is on screen, in physical pixels with winit's sub-pixel precision: `ptHimetricLocation` mapped
/// from the digitizer's rect onto its display's (`GetPointerDeviceRects`, both `[left, top, right, bottom]`).
/// HIMETRIC's origin is 0,0 even on multi-monitor setups, so the display's origin is added. `None` for an empty
/// digitizer rect: the caller falls back to the whole-pixel `ptPixelLocation`.
pub(crate) fn himetric_to_screen(
    himetric: [i32; 2],
    device: [i32; 4],
    display: [i32; 4],
) -> Option<[f64; 2]> {
    let [device_left, device_top, device_right, device_bottom] = device.map(f64::from);
    let [display_left, display_top, display_right, display_bottom] = display.map(f64::from);
    let (device_width, device_height) = (device_right - device_left, device_bottom - device_top);
    if device_width <= 0. || device_height <= 0. {
        return None;
    }
    let ratio_x = (display_right - display_left) / device_width;
    let ratio_y = (display_bottom - display_top) / device_height;
    Some([
        display_left + f64::from(himetric[0]) * ratio_x,
        display_top + f64::from(himetric[1]) * ratio_y,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    const TOUCH_DOWN: u32 = POINTER_FLAG_DOWN | POINTER_FLAG_INCONTACT;
    const TOUCH_MOVE: u32 = POINTER_FLAG_UPDATE | POINTER_FLAG_INCONTACT;
    const HOVER: u32 = POINTER_FLAG_UPDATE;
    const CANCEL: u32 = POINTER_FLAG_UP | POINTER_FLAG_CANCELED;
    const PT_MOUSE: i32 = 4;
    const PT_TOUCHPAD: i32 = 5;

    fn at(pointer_id: u32, pointer_type: i32, pointer_flags: u32, x: f32) -> PointerSample {
        PointerSample {
            pointer_id,
            pointer_type,
            pointer_flags,
            position: point(px(x), px(0.)),
            pressure: 0,
            time_stamp: None,
        }
    }

    /// What a test row expects from one sample: touches (id, phase, device, x) and hovers, or nothing.
    #[derive(Debug, PartialEq)]
    enum Out {
        Touch(u64, TouchPhase, PointerDevice, f32),
        Hover,
        Nothing,
    }

    fn out(inputs: Vec<PointerInput>) -> Vec<Out> {
        if inputs.is_empty() {
            return vec![Out::Nothing];
        }
        inputs
            .into_iter()
            .map(|input| match input {
                PointerInput::Touch(event) => Out::Touch(
                    event.id.0,
                    event.phase,
                    event.device,
                    event.position.x.as_f32(),
                ),
                PointerInput::Hover => Out::Hover,
            })
            .collect()
    }

    /// One row per (pointer state × sample) → input, each a sequence of samples from a fresh window.
    #[test]
    fn pointer_samples_to_touches() {
        use PointerDevice::{Pen, Touch};
        use TouchPhase::{Cancelled, Ended, Moved, Started};
        let rows: [(&str, Vec<PointerSample>, Vec<Out>); 13] = [
            (
                "finger tap: down, up",
                vec![
                    at(7, PT_TOUCH, TOUCH_DOWN, 1.),
                    at(7, PT_TOUCH, POINTER_FLAG_UP, 2.),
                ],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(0, Ended, Touch, 2.),
                ],
            ),
            (
                "finger drag: moves follow the contact",
                vec![
                    at(7, PT_TOUCH, TOUCH_DOWN, 1.),
                    at(7, PT_TOUCH, TOUCH_MOVE, 5.),
                    at(7, PT_TOUCH, POINTER_FLAG_UP, 9.),
                ],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(0, Moved, Touch, 5.),
                    Out::Touch(0, Ended, Touch, 9.),
                ],
            ),
            (
                "cancelled up cancels where the pointer last was",
                vec![at(7, PT_TOUCH, TOUCH_DOWN, 1.), at(7, PT_TOUCH, CANCEL, 3.)],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(0, Cancelled, Touch, 1.),
                ],
            ),
            (
                "pen stroke is a pen touch",
                vec![
                    at(3, PT_PEN, TOUCH_DOWN, 1.),
                    at(3, PT_PEN, POINTER_FLAG_UP, 2.),
                ],
                vec![
                    Out::Touch(0, Started, Pen, 1.),
                    Out::Touch(0, Ended, Pen, 2.),
                ],
            ),
            (
                "pen hover, stroke, hover: hovers are mouse moves",
                vec![
                    at(3, PT_PEN, HOVER, 1.),
                    at(3, PT_PEN, TOUCH_DOWN, 2.),
                    at(3, PT_PEN, POINTER_FLAG_UP, 3.),
                    at(3, PT_PEN, HOVER, 4.),
                ],
                vec![
                    Out::Hover,
                    Out::Touch(0, Started, Pen, 2.),
                    Out::Touch(0, Ended, Pen, 3.),
                    Out::Hover,
                ],
            ),
            (
                "two fingers get their own touches",
                vec![
                    at(7, PT_TOUCH, TOUCH_DOWN, 1.),
                    at(8, PT_TOUCH, TOUCH_DOWN, 2.),
                    at(8, PT_TOUCH, POINTER_FLAG_UP, 3.),
                    at(7, PT_TOUCH, TOUCH_MOVE, 4.),
                ],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(1, Started, Touch, 2.),
                    Out::Touch(1, Ended, Touch, 3.),
                    Out::Touch(0, Moved, Touch, 4.),
                ],
            ),
            (
                "a reused pointer id is a new touch",
                vec![
                    at(7, PT_TOUCH, TOUCH_DOWN, 1.),
                    at(7, PT_TOUCH, POINTER_FLAG_UP, 1.),
                    at(7, PT_TOUCH, TOUCH_DOWN, 2.),
                ],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(0, Ended, Touch, 1.),
                    Out::Touch(1, Started, Touch, 2.),
                ],
            ),
            (
                "rejected: move, up and cancel of a pointer that never went down here",
                vec![
                    at(7, PT_TOUCH, TOUCH_MOVE, 1.),
                    at(7, PT_TOUCH, POINTER_FLAG_UP, 1.),
                    at(7, PT_TOUCH, CANCEL, 1.),
                ],
                vec![Out::Nothing, Out::Nothing, Out::Nothing],
            ),
            (
                "a down of a pointer still down cancels its stale touch, then starts a new one",
                vec![
                    at(7, PT_TOUCH, TOUCH_DOWN, 1.),
                    at(7, PT_TOUCH, TOUCH_DOWN, 2.),
                    at(7, PT_TOUCH, TOUCH_MOVE, 4.),
                ],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(0, Cancelled, Touch, 1.),
                    Out::Touch(1, Started, Touch, 2.),
                    Out::Touch(1, Moved, Touch, 4.),
                ],
            ),
            (
                "a hover of a pen still down cancels its stale touch, then hovers",
                vec![
                    at(3, PT_PEN, TOUCH_DOWN, 1.),
                    at(3, PT_PEN, HOVER, 3.),
                    at(3, PT_PEN, TOUCH_MOVE, 4.),
                ],
                vec![
                    Out::Touch(0, Started, Pen, 1.),
                    Out::Touch(0, Cancelled, Pen, 1.),
                    Out::Hover,
                    Out::Nothing,
                ],
            ),
            (
                "rejected: mouse and touchpad pointers stay with the mouse messages",
                vec![
                    at(1, PT_MOUSE, TOUCH_DOWN, 1.),
                    at(2, PT_TOUCHPAD, TOUCH_DOWN, 1.),
                ],
                vec![Out::Nothing, Out::Nothing],
            ),
            (
                "rejected: flags no row matches",
                vec![
                    at(7, PT_TOUCH, POINTER_FLAG_INCONTACT, 1.),
                    at(7, PT_TOUCH, 0, 1.),
                ],
                vec![Out::Nothing, Out::Nothing],
            ),
            (
                "rejected: a cancelled pointer is no longer down",
                vec![
                    at(7, PT_TOUCH, TOUCH_DOWN, 1.),
                    at(7, PT_TOUCH, CANCEL, 1.),
                    at(7, PT_TOUCH, TOUCH_MOVE, 2.),
                ],
                vec![
                    Out::Touch(0, Started, Touch, 1.),
                    Out::Touch(0, Cancelled, Touch, 1.),
                    Out::Nothing,
                ],
            ),
        ];
        for (name, samples, expected) in rows {
            let mut contacts = Contacts::default();
            let got: Vec<Out> = samples
                .into_iter()
                .flat_map(|s| out(contacts.input(s)))
                .collect();
            assert_eq!(got, expected, "{name}");
        }
    }

    #[test]
    fn capture_loss_cancels_at_last_position() {
        let mut contacts = Contacts::default();
        contacts.input(at(7, PT_TOUCH, TOUCH_DOWN, 1.));
        contacts.input(at(7, PT_TOUCH, TOUCH_MOVE, 6.));
        let cancel = contacts
            .capture_lost(7)
            .map(|event| (event.id.0, event.phase, event.position.x.as_f32()));
        assert_eq!(cancel, Some((0, TouchPhase::Cancelled, 6.)), "down pointer");
        assert!(contacts.capture_lost(7).is_none(), "already cancelled");
        assert!(contacts.capture_lost(9).is_none(), "never down");
    }

    #[test]
    fn pressure_to_force() {
        let rows = [
            ("no pressure", 0, None),
            ("lightest", 1, Some(1. / 1024.)),
            ("half", 512, Some(0.5)),
            ("full", 1024, Some(1.)),
            ("out of range", 1025, None),
        ];
        for (name, pressure, expected) in rows {
            assert_eq!(force(pressure), expected, "{name}");
        }
        let mut contacts = Contacts::default();
        let pressed = PointerSample {
            pressure: 512,
            ..at(3, PT_PEN, TOUCH_DOWN, 0.)
        };
        let [PointerInput::Touch(event)] = &contacts.input(pressed)[..] else {
            panic!("a pen down is a touch");
        };
        assert_eq!(event.force, Some(0.5), "the touch carries it");
    }

    #[test]
    fn pen_leaves() {
        const HOVERING: u32 = POINTER_FLAG_UPDATE;
        let rows = [
            (
                "hovering pen, cursor off the window",
                PT_PEN,
                HOVERING,
                false,
                true,
            ),
            (
                "rejected: hovering pen, cursor still on it",
                PT_PEN,
                HOVERING,
                true,
                false,
            ),
            ("rejected: pen in contact", PT_PEN, TOUCH_MOVE, false, false),
            ("rejected: finger", PT_TOUCH, HOVERING, false, false),
            ("rejected: mouse", PT_MOUSE, HOVERING, false, false),
        ];
        for (name, pointer_type, flags, cursor_in_client, leaves) in rows {
            assert_eq!(
                pen_leave_is_mouse_leave(pointer_type, flags, cursor_in_client),
                leaves,
                "{name}"
            );
        }
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
    fn promoted_mouse_messages() {
        const WM_MOUSEWHEEL: u32 = 0x020A;
        const WM_NCLBUTTONDOWN: u32 = 0x00A1;
        let touch = 0xFF51_5780_isize;
        let pen = 0xFF51_5700_isize;
        let rows = [
            ("touch tap's button down", WM_LBUTTONDOWN, touch, true),
            ("touch tap's button up", WM_LBUTTONUP, touch, true),
            ("touch's move", WM_MOUSEMOVE, touch, true),
            ("press and hold's right click", WM_RBUTTONUP, touch, true),
            ("pen's move", WM_MOUSEMOVE, pen, true),
            ("pen barrel button", WM_RBUTTONDOWN, pen, true),
            (
                "sign-extended to 64 bits",
                WM_LBUTTONDOWN,
                0xFF51_5780_u32 as i32 as isize,
                true,
            ),
            ("kept: a real mouse's button", WM_LBUTTONDOWN, 0, false),
            (
                "kept: another app's extra info",
                WM_LBUTTONDOWN,
                0x1234_5678,
                false,
            ),
            (
                "kept: title bar (non-client) from touch",
                WM_NCLBUTTONDOWN,
                touch,
                false,
            ),
            ("kept: wheel", WM_MOUSEWHEEL, touch, false),
        ];
        for (name, msg, extra_info, dropped) in rows {
            assert_eq!(
                is_promoted_mouse_message(msg, extra_info),
                dropped,
                "{name}"
            );
        }
    }

    #[test]
    fn pointer_time_stamps() {
        let now = Instant::now();
        let clock = Clock {
            now,
            performance_count: 10_000_000,
            performance_frequency: 10_000_000,
            tick_count: 5_000,
        };
        let ms = Duration::from_millis;
        let rows = [
            (
                "performance count, 16 ms old",
                9_840_000,
                4_000,
                Some(now - ms(16)),
            ),
            (
                "performance count wins over dwTime",
                10_000_000,
                1_000,
                Some(now),
            ),
            (
                "no performance count: dwTime, 20 ms old",
                0,
                4_980,
                Some(now - ms(20)),
            ),
            ("dwTime after the clock read is now", 0, 5_010, Some(now)),
            (
                "performance count after the clock read is now",
                10_000_001,
                0,
                Some(now),
            ),
            ("neither: timed at dispatch", 0, 0, None),
        ];
        for (name, performance_count, time, expected) in rows {
            assert_eq!(clock.at(performance_count, time), expected, "{name}");
        }
        let wrapped = Clock {
            tick_count: 10,
            ..clock
        };
        assert_eq!(
            wrapped.at(0, u32::MAX - 9),
            Some(now - ms(20)),
            "dwTime across the wrap"
        );
    }

    #[test]
    fn himetric_locations() {
        let rows = [
            (
                "one display at the origin",
                [5_000, 2_500],
                [0, 0, 10_000, 5_000],
                [0, 0, 1920, 1080],
                Some([960., 540.]),
            ),
            (
                "sub-pixel",
                [1, 0],
                [0, 0, 10_000, 5_000],
                [0, 0, 1920, 1080],
                Some([0.192, 0.]),
            ),
            (
                "second display: its origin added",
                [0, 0],
                [0, 0, 10_000, 5_000],
                [1920, 0, 3840, 1080],
                Some([1920., 0.]),
            ),
            (
                "empty digitizer rect",
                [10, 10],
                [0, 0, 0, 0],
                [0, 0, 1920, 1080],
                None,
            ),
        ];
        for (name, himetric, device, display, expected) in rows {
            assert_eq!(
                himetric_to_screen(himetric, device, display),
                expected,
                "{name}"
            );
        }
    }
}
