use x11rb::protocol::{
    xinput,
    xproto::{self, ModMask},
};

use crate::linux::clock::Clock;
use gpui::{
    Modifiers, MouseButton, NavigationDirection, PointerDevice, TouchEvent, TouchId, TouchPhase,
    point, px,
};

pub(crate) enum ButtonOrScroll {
    Button(MouseButton),
    Scroll(ScrollDirection),
}

pub(crate) enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

pub(crate) fn button_or_scroll_from_event_detail(detail: u32) -> Option<ButtonOrScroll> {
    Some(match detail {
        1 => ButtonOrScroll::Button(MouseButton::Left),
        2 => ButtonOrScroll::Button(MouseButton::Middle),
        3 => ButtonOrScroll::Button(MouseButton::Right),
        4 => ButtonOrScroll::Scroll(ScrollDirection::Up),
        5 => ButtonOrScroll::Scroll(ScrollDirection::Down),
        6 => ButtonOrScroll::Scroll(ScrollDirection::Left),
        7 => ButtonOrScroll::Scroll(ScrollDirection::Right),
        8 => ButtonOrScroll::Button(MouseButton::Navigate(NavigationDirection::Back)),
        9 => ButtonOrScroll::Button(MouseButton::Navigate(NavigationDirection::Forward)),
        _ => return None,
    })
}

pub(crate) fn modifiers_from_state(state: xproto::KeyButMask) -> Modifiers {
    Modifiers {
        control: state.contains(xproto::KeyButMask::CONTROL),
        alt: state.contains(xproto::KeyButMask::MOD1),
        shift: state.contains(xproto::KeyButMask::SHIFT),
        platform: state.contains(xproto::KeyButMask::MOD4),
        function: false,
    }
}

pub(crate) fn modifiers_from_xinput_info(modifier_info: xinput::ModifierInfo) -> Modifiers {
    Modifiers {
        control: modifier_info.effective as u16 & ModMask::CONTROL.bits()
            == ModMask::CONTROL.bits(),
        alt: modifier_info.effective as u16 & ModMask::M1.bits() == ModMask::M1.bits(),
        shift: modifier_info.effective as u16 & ModMask::SHIFT.bits() == ModMask::SHIFT.bits(),
        platform: modifier_info.effective as u16 & ModMask::M4.bits() == ModMask::M4.bits(),
        function: false,
    }
}

pub(crate) fn pressed_button_from_mask(button_mask: u32) -> Option<MouseButton> {
    Some(if button_mask & 2 == 2 {
        MouseButton::Left
    } else if button_mask & 4 == 4 {
        MouseButton::Middle
    } else if button_mask & 8 == 8 {
        MouseButton::Right
    } else {
        return None;
    })
}

// XInput 2 touch, ported from rust-windowing/winit src/platform_impl/linux/x11/event_processor.rs @ v0.30.13
// (e9809ef54b18499bb4f2cac945719ecc2a61061b): the `XI_Touch*` arm of `process_xevent`, `xinput2_touch`, and the
// `XIPointerEmulated` check of `xinput2_button_input`; window.rs's touch masks. Copyright the winit contributors.
// Apache-2.0.

/// XInput 2 touch event type → GPUI phase, winit's `XI_Touch*` arm. There is no cancel row, as in winit and GDK:
/// a client that selects touch events without `XI_TouchOwnership` gets a touch only once it owns it, and an owned
/// touch ends only with `XI_TouchEnd` (XI2proto, "Ownership of touch sequences"). Extend: one row per XI2 touch
/// event type the window selects.
const XI2_TOUCH_PHASES: [(u16, TouchPhase); 3] = [
    (xinput::TOUCH_BEGIN_EVENT, TouchPhase::Started),
    (xinput::TOUCH_UPDATE_EVENT, TouchPhase::Moved),
    (xinput::TOUCH_END_EVENT, TouchPhase::Ended),
];

/// The XInput version the server agreed to (`XIQueryVersion`), as (major, minor); orders like versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct XinputVersion(pub(crate) u16, pub(crate) u16);

impl XinputVersion {
    /// The XInput 2 events a window selects on this server. Selecting a mask the server's version lacks fails the
    /// whole selection, so each row names the version that defines its events. Extend: one row per XI2 version.
    pub(crate) fn window_events(self) -> xinput::XIEventMask {
        let rows = [
            (
                XinputVersion(2, 0),
                xinput::XIEventMask::MOTION
                    | xinput::XIEventMask::BUTTON_PRESS
                    | xinput::XIEventMask::BUTTON_RELEASE
                    | xinput::XIEventMask::ENTER
                    | xinput::XIEventMask::LEAVE,
            ),
            // The server takes the three touch masks only together (XI2proto, XISelectEvents).
            (
                XinputVersion(2, 2),
                xinput::XIEventMask::TOUCH_BEGIN
                    | xinput::XIEventMask::TOUCH_UPDATE
                    | xinput::XIEventMask::TOUCH_END,
            ),
            // x11rb 0.13 has no XIEventMask constants for gesture events: XInput event type N is mask bit N.
            (
                XinputVersion(2, 4),
                xinput::XIEventMask::from(
                    1u32 << xinput::GESTURE_PINCH_BEGIN_EVENT
                        | 1u32 << xinput::GESTURE_PINCH_UPDATE_EVENT
                        | 1u32 << xinput::GESTURE_PINCH_END_EVENT,
                ),
            ),
        ];
        rows.into_iter()
            .filter(|(since, _)| *since <= self)
            .fold(xinput::XIEventMask::from(0u32), |mask, (_, events)| {
                mask | events
            })
    }
}

/// Whether the server made this XI2 pointer event up from other input: from a touch (dropped: the touch arrives
/// as a [`TouchEvent`] and GPUI makes its own mouse events from it, so a tap clicks once) or from a smooth-scroll
/// valuator (dropped: its motion event scrolls). winit's `xinput2_button_input` check.
pub(crate) fn is_pointer_emulated(flags: xinput::PointerEventFlags) -> bool {
    flags.contains(xinput::PointerEventFlags::POINTER_EMULATED)
}

/// The GPUI touch for an XI2 touch event (winit's `xinput2_touch`), in logical pixels at `scale_factor`. XI2 touch
/// ids are globally unique and increase for each new touch (XI2proto), so the detail is the [`TouchId`]. `None` for
/// an event type without an [`XI2_TOUCH_PHASES`] row.
pub(crate) fn touch_from_xinput(
    event: &xinput::TouchBeginEvent,
    scale_factor: f32,
    clock: Clock,
) -> Option<TouchEvent> {
    let (_, phase) = XI2_TOUCH_PHASES
        .iter()
        .find(|(event_type, _)| *event_type == event.event_type)?;
    Some(TouchEvent {
        id: TouchId(u64::from(event.detail)),
        phase: *phase,
        // Converted as client.rs converts pointer positions, so touch and mouse agree.
        position: point(
            px(event.event_x as f32 / u16::MAX as f32 / scale_factor),
            px(event.event_y as f32 / u16::MAX as f32 / scale_factor),
        ),
        predicted_position: None,
        // winit leaves force a TODO: XI2 reports pressure only as a per-device valuator.
        force: None,
        device: PointerDevice::Touch,
        time_stamp: clock.at(event.time),
    })
}

pub(crate) fn get_valuator_axis_index(
    valuator_mask: &Vec<u32>,
    valuator_number: u16,
) -> Option<usize> {
    // XInput valuator masks have a 1 at the bit indexes corresponding to each
    // valuator present in this event's axisvalues. Axisvalues is ordered from
    // lowest valuator number to highest, so counting bits before the 1 bit for
    // this valuator yields the index in axisvalues.
    if bit_is_set_in_vec(valuator_mask, valuator_number) {
        Some(popcount_upto_bit_index(valuator_mask, valuator_number) as usize)
    } else {
        None
    }
}

/// Returns the number of 1 bits in `bit_vec` for all bits where `i < bit_index`.
fn popcount_upto_bit_index(bit_vec: &Vec<u32>, bit_index: u16) -> u32 {
    let array_index = bit_index as usize / 32;
    let popcount: u32 = bit_vec
        .get(array_index)
        .map_or(0, |bits| keep_bits_upto(*bits, bit_index % 32).count_ones());
    if array_index == 0 {
        popcount
    } else {
        // Valuator numbers over 32 probably never occur for scroll position, but may as well
        // support it.
        let leading_popcount: u32 = bit_vec
            .iter()
            .take(array_index)
            .map(|bits| bits.count_ones())
            .sum();
        popcount + leading_popcount
    }
}

fn bit_is_set_in_vec(bit_vec: &Vec<u32>, bit_index: u16) -> bool {
    let array_index = bit_index as usize / 32;
    bit_vec
        .get(array_index)
        .is_some_and(|bits| bit_is_set(*bits, bit_index % 32))
}

fn bit_is_set(bits: u32, bit_index: u16) -> bool {
    bits & (1 << bit_index) != 0
}

/// Sets every bit with `i >= bit_index` to 0.
fn keep_bits_upto(bits: u32, bit_index: u16) -> u32 {
    if bit_index == 0 {
        0
    } else if bit_index >= 32 {
        u32::MAX
    } else {
        bits & ((1 << bit_index) - 1)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// An XI2 touch event as the server sends it: the fields `touch_from_xinput` reads, the rest as parsed from
    /// a wire event with zeros. Positions are FP16.16.
    struct Xi2Touch {
        event_type: u16,
        detail: u32,
        time: xproto::Timestamp,
        event_x: i32,
        event_y: i32,
    }

    impl Xi2Touch {
        fn event(&self) -> xinput::TouchBeginEvent {
            xinput::TouchBeginEvent {
                event_type: self.event_type,
                detail: self.detail,
                time: self.time,
                event_x: self.event_x,
                event_y: self.event_y,
                ..Default::default()
            }
        }
    }

    // Extend: one row per XI2 touch event shape. The gesture rows follow Xwayland's events in the live x11 traces
    // (parallax-platform's tests/traces/x11, docs/traces-desktop.md): ids, positions and ages as recorded.
    #[test]
    fn xi2_touch_events() {
        let clock = Clock {
            now: Instant::now(),
            monotonic_millis: 50_000,
        };
        // (name, event, scale factor, expected (id, phase, x, y, age in ms))
        let rows: [(
            &str,
            Xi2Touch,
            f32,
            Option<(u64, TouchPhase, f32, f32, u64)>,
        ); 14] = [
            (
                "begin at scale 1",
                Xi2Touch {
                    event_type: xinput::TOUCH_BEGIN_EVENT,
                    detail: 7,
                    time: 50_000,
                    event_x: 100 * 65535,
                    event_y: 40 * 65535,
                },
                1.0,
                Some((7, TouchPhase::Started, 100.0, 40.0, 0)),
            ),
            (
                "update at scale 2 is in logical pixels",
                Xi2Touch {
                    event_type: xinput::TOUCH_UPDATE_EVENT,
                    detail: 7,
                    time: 49_984,
                    event_x: 300 * 65535,
                    event_y: 80 * 65535,
                },
                2.0,
                Some((7, TouchPhase::Moved, 150.0, 40.0, 16)),
            ),
            (
                "end",
                Xi2Touch {
                    event_type: xinput::TOUCH_END_EVENT,
                    detail: 7,
                    time: 49_990,
                    event_x: 0,
                    event_y: 0,
                },
                1.0,
                Some((7, TouchPhase::Ended, 0.0, 0.0, 10)),
            ),
            (
                "the detail is the touch id",
                Xi2Touch {
                    event_type: xinput::TOUCH_BEGIN_EVENT,
                    detail: u32::MAX,
                    time: 50_000,
                    event_x: 0,
                    event_y: 0,
                },
                1.0,
                Some((u64::from(u32::MAX), TouchPhase::Started, 0.0, 0.0, 0)),
            ),
            (
                "tap: begin (Xwayland, as recorded)",
                Xi2Touch {
                    event_type: xinput::TOUCH_BEGIN_EVENT,
                    detail: 2,
                    time: 49_950,
                    event_x: 200 * 65535,
                    event_y: 400 * 65535,
                },
                1.0,
                Some((2, TouchPhase::Started, 200.0, 400.0, 50)),
            ),
            (
                "tap: end where it began",
                Xi2Touch {
                    event_type: xinput::TOUCH_END_EVENT,
                    detail: 2,
                    time: 50_000,
                    event_x: 200 * 65535,
                    event_y: 400 * 65535,
                },
                1.0,
                Some((2, TouchPhase::Ended, 200.0, 400.0, 0)),
            ),
            (
                "double tap: the second touch has the next detail",
                Xi2Touch {
                    event_type: xinput::TOUCH_BEGIN_EVENT,
                    detail: 3,
                    time: 50_000,
                    event_x: 200 * 65535,
                    event_y: 400 * 65535,
                },
                1.0,
                Some((3, TouchPhase::Started, 200.0, 400.0, 0)),
            ),
            (
                "long press: an end 800 ms after its begin keeps its age",
                Xi2Touch {
                    event_type: xinput::TOUCH_END_EVENT,
                    detail: 4,
                    time: 49_200,
                    event_x: 200 * 65535,
                    event_y: 400 * 65535,
                },
                1.0,
                Some((4, TouchPhase::Ended, 200.0, 400.0, 800)),
            ),
            (
                "slow drag: an update moves",
                Xi2Touch {
                    event_type: xinput::TOUCH_UPDATE_EVENT,
                    detail: 10,
                    time: 49_984,
                    event_x: 200 * 65535,
                    event_y: 599 * 65535,
                },
                1.0,
                Some((10, TouchPhase::Moved, 200.0, 599.0, 16)),
            ),
            (
                "fling: the last update",
                Xi2Touch {
                    event_type: xinput::TOUCH_UPDATE_EVENT,
                    detail: 12,
                    time: 50_000,
                    event_x: 200 * 65535,
                    event_y: 250 * 65535,
                },
                1.0,
                Some((12, TouchPhase::Moved, 200.0, 250.0, 0)),
            ),
            (
                "pinch: the first finger",
                Xi2Touch {
                    event_type: xinput::TOUCH_BEGIN_EVENT,
                    detail: 14,
                    time: 50_000,
                    event_x: 150 * 65535,
                    event_y: 400 * 65535,
                },
                1.0,
                Some((14, TouchPhase::Started, 150.0, 400.0, 0)),
            ),
            (
                "pinch: the second finger has its own detail",
                Xi2Touch {
                    event_type: xinput::TOUCH_BEGIN_EVENT,
                    detail: 15,
                    time: 50_000,
                    event_x: 250 * 65535,
                    event_y: 400 * 65535,
                },
                1.0,
                Some((15, TouchPhase::Started, 250.0, 400.0, 0)),
            ),
            (
                "pinch at scale 2: a spread finger in logical pixels",
                Xi2Touch {
                    event_type: xinput::TOUCH_UPDATE_EVENT,
                    detail: 15,
                    time: 50_000,
                    event_x: 600 * 65535,
                    event_y: 800 * 65535,
                },
                2.0,
                Some((15, TouchPhase::Moved, 300.0, 400.0, 0)),
            ),
            (
                "ownership is not a phase",
                Xi2Touch {
                    event_type: xinput::TOUCH_OWNERSHIP_EVENT,
                    detail: 7,
                    time: 50_000,
                    event_x: 0,
                    event_y: 0,
                },
                1.0,
                None,
            ),
        ];
        for (name, row, scale_factor, expected) in rows {
            let touch = touch_from_xinput(&row.event(), scale_factor, clock);
            let expected = expected.map(|(id, phase, x, y, age)| TouchEvent {
                id: TouchId(id),
                phase,
                position: point(px(x), px(y)),
                predicted_position: None,
                force: None,
                device: PointerDevice::Touch,
                time_stamp: Some(clock.now - Duration::from_millis(age)),
            });
            // TouchEvent has no PartialEq; its Debug form shows every field.
            assert_eq!(format!("{touch:?}"), format!("{expected:?}"), "{name}");
        }
    }

    #[test]
    fn xi2_window_events_by_version() {
        let touch = xinput::XIEventMask::TOUCH_BEGIN
            | xinput::XIEventMask::TOUCH_UPDATE
            | xinput::XIEventMask::TOUCH_END;
        let pinch = xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_BEGIN_EVENT);
        // (version, selects touch, selects pinch gestures)
        let rows = [
            (XinputVersion(2, 0), false, false),
            (XinputVersion(2, 1), false, false),
            (XinputVersion(2, 2), true, false),
            (XinputVersion(2, 3), true, false),
            (XinputVersion(2, 4), true, true),
            (XinputVersion(3, 0), true, true),
        ];
        for (version, selects_touch, selects_pinch) in rows {
            let mask = version.window_events();
            assert!(
                mask.contains(xinput::XIEventMask::BUTTON_PRESS),
                "{version:?}"
            );
            assert_eq!(mask.contains(touch), selects_touch, "{version:?} touch");
            assert_eq!(mask.contains(pinch), selects_pinch, "{version:?} pinch");
        }
    }

    #[test]
    fn test_get_valuator_axis_index() {
        assert!(get_valuator_axis_index(&vec![0b11], 0) == Some(0));
        assert!(get_valuator_axis_index(&vec![0b11], 1) == Some(1));
        assert!(get_valuator_axis_index(&vec![0b11], 2) == None);

        assert!(get_valuator_axis_index(&vec![0b100], 0) == None);
        assert!(get_valuator_axis_index(&vec![0b100], 1) == None);
        assert!(get_valuator_axis_index(&vec![0b100], 2) == Some(0));
        assert!(get_valuator_axis_index(&vec![0b100], 3) == None);

        assert!(get_valuator_axis_index(&vec![0b1010, 0], 0) == None);
        assert!(get_valuator_axis_index(&vec![0b1010, 0], 1) == Some(0));
        assert!(get_valuator_axis_index(&vec![0b1010, 0], 2) == None);
        assert!(get_valuator_axis_index(&vec![0b1010, 0], 3) == Some(1));

        assert!(get_valuator_axis_index(&vec![0b1010, 0b1], 0) == None);
        assert!(get_valuator_axis_index(&vec![0b1010, 0b1], 1) == Some(0));
        assert!(get_valuator_axis_index(&vec![0b1010, 0b1], 2) == None);
        assert!(get_valuator_axis_index(&vec![0b1010, 0b1], 3) == Some(1));
        assert!(get_valuator_axis_index(&vec![0b1010, 0b1], 32) == Some(2));
        assert!(get_valuator_axis_index(&vec![0b1010, 0b1], 33) == None);

        assert!(get_valuator_axis_index(&vec![0b1010, 0b101], 34) == Some(3));
    }
}
