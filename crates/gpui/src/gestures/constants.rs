// Ported from flutter/flutter packages/flutter/lib/src/gestures/constants.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.
//
// Flutter's names are kept so its changes diff in. Distances are logical pixels (`Pixels`), velocities logical
// pixels per second. Dropped: nothing.

//! Flutter's gesture constants, the one copy GPUI's recognizers and Parallax's platform behaviour table read.

#![allow(
    non_upper_case_globals,
    reason = "Flutter's constant names, kept so Flutter changes diff in"
)]

use std::time::Duration;

use crate::{Pixels, px};

// Modeled after Android's ViewConfiguration:
// https://github.com/android/platform_frameworks_base/blob/main/core/java/android/view/ViewConfiguration.java

/// The time that must elapse before a tap gesture sends onTapDown, if there's
/// any doubt that the gesture is a tap.
pub const kPressTimeout: Duration = Duration::from_millis(100);

/// Maximum length of time between a tap down and a tap up for the gesture to be
/// considered a tap. (Currently not honored by the TapGestureRecognizer.)
// TODO(ianh): Remove this, or implement a hover-tap gesture recognizer which
// uses this.
pub const kHoverTapTimeout: Duration = Duration::from_millis(150);

/// Maximum distance between the down and up pointers for a tap. (Currently not
/// honored by the TapGestureRecognizer; PrimaryPointerGestureRecognizer,
/// which TapGestureRecognizer inherits from, uses [`kTouchSlop`].)
// TODO(ianh): Remove this or implement it correctly.
pub const kHoverTapSlop: Pixels = px(20.0); // Logical pixels

/// The time before a long press gesture attempts to win.
pub const kLongPressTimeout: Duration = Duration::from_millis(500);

/// The maximum time from the start of the first tap to the start of the second
/// tap in a double-tap gesture.
// In Android, this is actually the time from the first's up event
// to the second's down event, according to the ViewConfiguration docs.
pub const kDoubleTapTimeout: Duration = Duration::from_millis(300);

/// The minimum time from the end of the first tap to the start of the second
/// tap in a double-tap gesture.
pub const kDoubleTapMinTime: Duration = Duration::from_millis(40);

/// The maximum distance that the first touch in a double-tap gesture can travel
/// before deciding that it is not part of a double-tap gesture.
/// DoubleTapGestureRecognizer also restricts the second touch to this distance.
pub const kDoubleTapTouchSlop: Pixels = kTouchSlop; // Logical pixels

/// Distance between the initial position of the first touch and the start
/// position of a potential second touch for the second touch to be considered
/// the second touch of a double-tap gesture.
pub const kDoubleTapSlop: Pixels = px(100.0); // Logical pixels

/// The time for which zoom controls (e.g. in a map interface) are to be
/// displayed on the screen, from the moment they were last requested.
pub const kZoomControlsTimeout: Duration = Duration::from_millis(3000);

/// The distance a touch has to travel for the framework to be confident that
/// the gesture is a scroll gesture, or, inversely, the maximum distance that a
/// touch can travel before the framework becomes confident that it is not a
/// tap.
///
/// A total delta less than or equal to [`kTouchSlop`] is not considered to be a
/// drag, whereas if the delta is greater than [`kTouchSlop`] it is considered to
/// be a drag.
// This value was empirically derived. We started at 8.0 and increased it to
// 18.0 after getting complaints that it was too difficult to hit targets.
pub const kTouchSlop: Pixels = px(18.0); // Logical pixels

/// The distance a touch has to travel for the framework to be confident that
/// the gesture is a paging gesture. (Currently not used, because paging uses a
/// regular drag gesture, which uses kTouchSlop.)
// TODO(ianh): Create variants of HorizontalDragGestureRecognizer et al for
// paging, which use this constant.
pub const kPagingTouchSlop: Pixels = px(kTouchSlop.0 * 2.0); // Logical pixels

/// The distance a touch has to travel for the framework to be confident that
/// the gesture is a panning gesture.
pub const kPanSlop: Pixels = px(kTouchSlop.0 * 2.0); // Logical pixels

/// The distance a touch has to travel for the framework to be confident that
/// the gesture is a scale gesture.
pub const kScaleSlop: Pixels = kTouchSlop; // Logical pixels

/// The margin around a dialog, popup menu, or other window-like widget inside
/// which we do not consider a tap to dismiss the widget. (Not currently used.)
// TODO(ianh): Make ModalBarrier support this.
pub const kWindowTouchSlop: Pixels = px(16.0); // Logical pixels

/// The minimum velocity for a touch to consider that touch to trigger a fling
/// gesture.
// TODO(ianh): Make sure nobody has their own version of this.
pub const kMinFlingVelocity: f32 = 50.0; // Logical pixels / second
// const Velocity kMinFlingVelocity = const Velocity(pixelsPerSecond: 50.0);

/// Drag gesture fling velocities are clipped to this value.
// TODO(ianh): Make sure nobody has their own version of this.
pub const kMaxFlingVelocity: f32 = 8000.0; // Logical pixels / second

/// The maximum time from the start of the first tap to the start of the second
/// tap in a jump-tap gesture.
// TODO(ianh): Implement jump-tap gestures.
pub const kJumpTapTimeout: Duration = Duration::from_millis(500);

/// Like [`kTouchSlop`], but for more precise pointers like mice and trackpads.
pub const kPrecisePointerHitSlop: Pixels = px(1.0); // Logical pixels;

/// Like [`kPanSlop`], but for more precise pointers like mice and trackpads.
pub const kPrecisePointerPanSlop: Pixels = px(kPrecisePointerHitSlop.0 * 2.0); // Logical pixels

/// Like [`kScaleSlop`], but for more precise pointers like mice and trackpads.
pub const kPrecisePointerScaleSlop: Pixels = kPrecisePointerHitSlop; // Logical pixels
