//! Touch gesture recognition vocabulary.
//!
//! GPUI recognizes gestures from raw [`TouchEvent`](crate::TouchEvent)s using
//! a portable recognizer. Taps synthesize mouse presses and releases so
//! existing click listeners and text selection work on mobile. Pans produce
//! [`ScrollEvent`](crate::ScrollEvent)s, including post-release
//! momentum. Long presses and touch drags can be claimed by element listeners.

pub mod constants;
mod lsq_solver;
#[cfg(test)]
mod lsq_solver_test;
mod velocity_tracker;
#[cfg(test)]
mod velocity_tracker_data;
#[cfg(test)]
mod velocity_tracker_test;

use constants::{
    kDoubleTapMinTime, kDoubleTapSlop, kDoubleTapTimeout, kLongPressTimeout, kPanSlop,
    kPrecisePointerHitSlop, kPrecisePointerPanSlop, kPrecisePointerScaleSlop, kScaleSlop,
    kTouchSlop,
};
use velocity_tracker::VelocityEstimate;
pub(crate) use velocity_tracker::VelocityTracker;
pub use velocity_tracker::VelocityTrackerKind;

use std::mem;
use std::time::Duration;

use scheduler::Instant;
use smallvec::SmallVec;

use crate::{
    Axis, Bounds, GestureEvent, InputEvent, IsZero, Modifiers, MouseButton, MouseDownEvent,
    MouseEvent, MouseUpEvent, Pixels, PlatformInput, Point, PointerDevice, ScrollDelta,
    ScrollEvent, ScrollSource, TouchEvent, TouchId, TouchPhase, overscroll::Overscroll, point, px,
    seal::Sealed,
};

mod pinch;
pub mod scale;
pub(crate) use pinch::{PinchArena, PinchChange};
pub use pinch::TouchPinch;

const SCROLL_EVENT_SEPARATION: Duration = Duration::from_millis(28);

fn dominant_axis(delta: Point<Pixels>) -> Axis {
    if delta.x.abs() <= delta.y.abs() {
        Axis::Vertical
    } else {
        Axis::Horizontal
    }
}

fn lock_delta_to_axis(delta: &mut Point<Pixels>, axis: Axis) {
    match axis {
        Axis::Vertical => delta.x = Pixels::ZERO,
        Axis::Horizontal => delta.y = Pixels::ZERO,
    }
}

fn movements_oppose(left: Point<Pixels>, right: Point<Pixels>) -> bool {
    f32::from(left.x) * f32::from(right.x) + f32::from(left.y) * f32::from(right.y) < 0.
}

/// Tracks the dominant axis across the events in a scroll gesture.
#[derive(Clone, Copy, Debug, Default)]
pub struct OngoingScroll {
    last_event: Option<Instant>,
    axis: Option<Axis>,
}

impl OngoingScroll {
    /// Filters the given delta to the dominant axis of the current scroll gesture.
    ///
    /// Gestures are delimited by their touch phase when available, with a timeout
    /// fallback for platforms that only emit [`TouchPhase::Moved`].
    pub fn filter(&mut self, delta: &mut Point<Pixels>, touch_phase: TouchPhase) {
        self.filter_at(delta, touch_phase, Instant::now())
    }

    fn filter_at(&mut self, delta: &mut Point<Pixels>, touch_phase: TouchPhase, now: Instant) {
        const UNLOCK_PERCENT: f32 = 1.9;
        const UNLOCK_LOWER_BOUND: Pixels = px(6.);

        if matches!(touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.last_event = None;
            self.axis = None;
            return;
        }

        let x = delta.x.abs();
        let y = delta.y.abs();
        if x.is_zero() && y.is_zero() {
            if touch_phase == TouchPhase::Started {
                self.last_event = None;
                self.axis = None;
            }
            return;
        }

        let starts_new_gesture = touch_phase == TouchPhase::Started
            || self
                .last_event
                .is_none_or(|last_event| now.duration_since(last_event) >= SCROLL_EVENT_SEPARATION);
        let mut axis = self.axis;
        if starts_new_gesture {
            axis = Some(dominant_axis(*delta));
        } else if x.max(y) >= UNLOCK_LOWER_BOUND {
            match axis {
                Some(Axis::Vertical) if x > y && x >= y * UNLOCK_PERCENT => {
                    axis = None;
                }
                Some(Axis::Horizontal) if y > x && y >= x * UNLOCK_PERCENT => {
                    axis = None;
                }
                _ => {}
            }
        }

        self.last_event = Some(now);
        self.axis = axis;
        if let Some(axis) = axis {
            lock_delta_to_axis(delta, axis);
        }
    }
}

/// Feel constants consumed by gesture recognizers. Provided on a best-effort
/// basis, depending on each platform's support, defaulting to Flutter's
/// gesture constants (the same on every platform), whose recognizer tests
/// these values pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureTuning {
    /// Distance a touch may travel before it stops being a potential tap and
    /// becomes a pan/drag.
    pub touch_slop: Pixels,
    /// Maximum interval from the end of one tap to the start of the next for
    /// them to accumulate a tap count.
    pub multi_tap_interval: Duration,
    /// Maximum distance from where one tap started to where the next starts
    /// for them to accumulate a tap count.
    pub multi_tap_slop: Pixels,
    /// Minimum interval from the start of one tap to the start of the next
    /// for them to accumulate a tap count: touch screens often report one
    /// touch as two in quick succession.
    pub multi_tap_min_interval: Duration,
    /// How long a touch must remain within [`Self::touch_slop`] to be
    /// recognized as a long press.
    pub long_press_duration: Duration,
    /// How scroll momentum decelerates after a fling.
    pub scroll_physics: ScrollPhysics,
    /// Minimum release velocity, in pixels per second, required to start
    /// scroll momentum.
    pub min_fling_velocity: f32,
    /// Release velocities are clamped to this magnitude, in pixels per second.
    pub max_fling_velocity: f32,
    /// How a touch's release velocity is estimated: one of Flutter's
    /// per-platform velocity trackers.
    pub velocity_tracker: VelocityTrackerKind,
    /// What scroll containers do when a touch drags or flings them past an
    /// edge, unless a container sets its own
    /// ([`StatefulInteractiveElement::overscroll`](crate::StatefulInteractiveElement::overscroll)).
    pub overscroll: Overscroll,
    /// The haptic the window plays when an element claims a long press
    /// (Flutter's `Feedback.forLongPress`, which its widgets call when they
    /// handle one), or `None` for none.
    pub long_press_haptic: Option<Haptic>,
}

/// A haptic the platform plays through [`Window::perform_haptic`](crate::Window::perform_haptic):
/// one of Flutter's `HapticFeedback` calls (services/haptic_feedback.dart).
/// Each backend maps every kind to its own feedback; desktops play none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Haptic {
    /// `HapticFeedback.vibrate`: a short vibration.
    Vibrate,
    /// `HapticFeedback.lightImpact`.
    LightImpact,
    /// `HapticFeedback.mediumImpact`.
    MediumImpact,
    /// `HapticFeedback.heavyImpact`.
    HeavyImpact,
    /// `HapticFeedback.selectionClick`: a selection moved by one value.
    SelectionClick,
    /// `HapticFeedback.successNotification`.
    SuccessNotification,
    /// `HapticFeedback.warningNotification`.
    WarningNotification,
    /// `HapticFeedback.errorNotification`.
    ErrorNotification,
}

// Flutter's gesture constants (constants.rs), whose recognizer tests these
// values pass.
impl Default for GestureTuning {
    fn default() -> Self {
        Self {
            touch_slop: kTouchSlop,
            // DoubleTapGestureRecognizer times kDoubleTapTimeout from the
            // first tap's up event to the second's down event, as Android does.
            multi_tap_interval: kDoubleTapTimeout,
            multi_tap_slop: kDoubleTapSlop,
            // DoubleTapGestureRecognizer counts kDoubleTapMinTime from the
            // first tap's down event.
            multi_tap_min_interval: kDoubleTapMinTime,
            long_press_duration: kLongPressTimeout,
            scroll_physics: ScrollPhysics::ios(),
            // BouncingScrollPhysics' fling limits, with the iOS physics above.
            min_fling_velocity: ScrollPhysics::ios().min_fling_velocity(),
            max_fling_velocity: ScrollPhysics::ios().max_fling_velocity(),
            velocity_tracker: VelocityTrackerKind::IOSScrollViewFling,
            // GPUI's hard stop at the edge; platforms that overscroll set it.
            overscroll: Overscroll::None,
            // Platform feel, not a constant: a platform or app that wants one sets it.
            long_press_haptic: None,
        }
    }
}

impl GestureTuning {
    /// Flutter's `isFlingGesture` (monodrag.dart): a release is a fling when
    /// its speed (px/s) and the distance its estimate covers (px) both pass
    /// the minimums.
    pub(crate) fn is_fling_gesture(&self, speed: f32, distance: f32) -> bool {
        speed > self.min_fling_velocity
            && distance > f32::from(self.scroll_physics.min_fling_distance())
    }
}

/// How far a pointer travels before it stops being a tap (`hit`), starts a
/// pan (`pan`) or a scale (`scale`), in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slops {
    /// The distance a tap may travel (`computeHitSlop`).
    pub hit: Pixels,
    /// The distance a pan must travel to start (`computePanSlop`).
    pub pan: Pixels,
    /// The span change a scale must make to start (`computeScaleSlop`).
    pub scale: Pixels,
}

// Ported from flutter/flutter packages/flutter/lib/src/gestures/events.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7) (computeHitSlop, computePanSlop, computeScaleSlop, and
// gesture_settings.dart's DeviceGestureSettings.panSlop). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.
/// The slops for a pointer of `kind`. `touch_slop` is the device's setting
/// (`DeviceGestureSettings.touchSlop`: Android's `ViewConfiguration`), `None`
/// for Flutter's [`kTouchSlop`]; its pan slop is twice it. A new device kind
/// gets one arm.
pub fn slops(kind: PointerDevice, touch_slop: Option<Pixels>) -> Slops {
    match kind {
        PointerDevice::Mouse => Slops {
            hit: kPrecisePointerHitSlop,
            pan: kPrecisePointerPanSlop,
            scale: kPrecisePointerScaleSlop,
        },
        PointerDevice::Touch | PointerDevice::Pen => Slops {
            hit: touch_slop.unwrap_or(kTouchSlop),
            pan: touch_slop.map_or(kPanSlop, |slop| slop * 2.0),
            scale: kScaleSlop,
        },
    }
}

pub mod physics;
mod scroll_physics;
mod scroll_simulation;
use physics::Simulation;
pub use scroll_physics::{ScrollDecelerationRate, ScrollMetrics, ScrollPhysics, tolerance_for};
pub use scroll_simulation::{BouncingScrollSimulation, ClampingScrollSimulation};

/// The window's device pixel ratio isn't passed to the recognizer yet, so its
/// flings use Flutter's `toleranceFor` at a ratio of 2.
const FLING_DEVICE_PIXEL_RATIO: f64 = 2.0;

/// The fling released at `speed` pixels per second, starting at 0: Flutter's
/// `createBallisticSimulation` with no extents, since the recognizer knows
/// none (the scroll container clamps). `None` when there is nothing to fling.
fn fling_simulation(physics: ScrollPhysics, speed: f32) -> Option<Box<dyn Simulation>> {
    physics.create_ballistic_simulation(
        &ScrollMetrics {
            min_scroll_extent: f64::NEG_INFINITY,
            max_scroll_extent: f64::INFINITY,
            pixels: 0.,
            viewport_dimension: f64::INFINITY,
            device_pixel_ratio: FLING_DEVICE_PIXEL_RATIO,
        },
        f64::from(speed),
    )
}

/// The set of gesture kinds that participate in recognition.
///
/// Used by [`PlatformGestures::native_recognizers`] to declare which gestures
/// the platform recognizes natively rather than leaving to gpui core's
/// portable recognizers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GestureKinds {
    /// Tap (and multi-tap), synthesized as mouse presses by the portable recognizer.
    pub tap: bool,
    /// Long press, surfaced as [`LongPressEvent`].
    pub long_press: bool,
    /// Pan/scroll (including fling momentum), surfaced as
    /// [`ScrollEvent`](crate::ScrollEvent)s.
    pub pan: bool,
    /// Pinch to zoom, surfaced as [`PinchEvent`](crate::PinchEvent)s.
    pub pinch: bool,
}

impl GestureKinds {
    /// No gestures; gpui core's portable recognizers handle everything.
    pub const NONE: Self = Self {
        tap: false,
        long_press: false,
        pan: false,
        pinch: false,
    };

    /// All gesture kinds.
    pub const ALL: Self = Self {
        tap: true,
        long_press: true,
        pan: true,
        pinch: true,
    };
}

/// A direct touch drag an element took from the touch recognizer: claimed at
/// touch down, before the touch can become a tap, long press or pan, or won
/// once it passed the drag's slop ([`Window::compete_for_touch_drag`](crate::Window::compete_for_touch_drag)).
#[derive(Clone, Debug)]
pub struct TouchDragEvent {
    /// The phase of the touch drag.
    pub phase: TouchPhase,
    /// How the drag started: every event of one drag carries the same value.
    pub start: TouchDragStart,
    /// The position where the touch started.
    pub start_position: Point<Pixels>,
    /// The touch's current position.
    pub position: Point<Pixels>,
}

/// How a touch drag started, in Flutter's arena terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchDragStart {
    /// Offered at touch down (the offer is the `Started` event): claiming it
    /// with `prevent_default` wins the touch at once, as a drag recognizer
    /// alone in its arena does.
    Down,
    /// Won once the touch moved past this kind's slop, ahead of the tap, the
    /// long press and the pan it competed with (the `Started` event comes
    /// then, at the touch's position).
    PastSlop(TouchDragKind),
}

/// The drag an element competes for, and so what the touch must travel for it
/// to win: Flutter's `TapAndHorizontalDragGestureRecognizer` (a text field on
/// mobile) or `TapAndPanGestureRecognizer` (on desktop). A new kind gets one
/// arm in each `match` of [`TouchDragKind`]'s methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchDragKind {
    /// Wins when the horizontal travel passes the hit slop.
    Horizontal,
    /// Wins when the travel on any axis passes the pan slop.
    Pan,
}

// Ported from flutter/flutter packages/flutter/lib/src/gestures/tap_and_drag.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7) (BaseTapAndDragGestureRecognizer._checkDrag;
// TapAndHorizontalDragGestureRecognizer's and TapAndPanGestureRecognizer's _getDeltaForDetails,
// _getPrimaryValueFromOffset and _hasSufficientGlobalDistanceToAccept). Copyright 2014 The Flutter Authors.
// BSD-3-Clause, see third_party/flutter/LICENSE.
impl TouchDragKind {
    /// What one move of `delta` adds to `_globalDistanceMoved`:
    /// `_getDeltaForDetails(delta).distance * (_getPrimaryValueFromOffset(delta) ?? 1).sign`.
    fn global_distance_moved(self, delta: Point<Pixels>) -> f32 {
        match self {
            // Offset(delta.dx, 0.0), signed by dx: the horizontal travel.
            TouchDragKind::Horizontal => f32::from(delta.x),
            // The whole delta, unsigned.
            TouchDragKind::Pan => delta.magnitude() as f32,
        }
    }

    /// `_hasSufficientGlobalDistanceToAccept`.
    fn has_sufficient_global_distance_to_accept(
        self,
        global_distance_moved: f32,
        slops: Slops,
    ) -> bool {
        let slop = match self {
            TouchDragKind::Horizontal => slops.hit,
            TouchDragKind::Pan => slops.pan,
        };
        global_distance_moved.abs() > f32::from(slop)
    }
}

/// The scroll axes of the containers under a touch: which travel lets GPUI's
/// pan, standing in for their drag recognizers, accept the touch. Every
/// scroll container under the touch joins in while its `Started` is
/// dispatched (`Window::scroll_container_under_touch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanAxes {
    /// No scroll container: the pan accepts in any direction.
    NoContainer,
    /// Only containers scrolling on this axis: Flutter's
    /// `VerticalDragGestureRecognizer` or `HorizontalDragGestureRecognizer`.
    One(Axis),
    /// Containers on both axes, or one that scrolls on both: the pan
    /// accepts in any direction, as Flutter's `PanGestureRecognizer`.
    Both,
}

impl PanAxes {
    /// A container that scrolls on x (`horizontal`) and on y (`vertical`).
    pub(crate) fn of(horizontal: bool, vertical: bool) -> Self {
        match (horizontal, vertical) {
            (false, false) => PanAxes::NoContainer,
            (true, false) => PanAxes::One(Axis::Horizontal),
            (false, true) => PanAxes::One(Axis::Vertical),
            (true, true) => PanAxes::Both,
        }
    }

    /// The axes once another container under the touch joins.
    pub(crate) fn join(self, other: PanAxes) -> Self {
        match (self, other) {
            (PanAxes::NoContainer, joined) | (joined, PanAxes::NoContainer) => joined,
            (PanAxes::One(axis), PanAxes::One(other)) if axis == other => PanAxes::One(axis),
            (PanAxes::One(_) | PanAxes::Both, PanAxes::One(_) | PanAxes::Both) => PanAxes::Both,
        }
    }

    /// The axis the pan locks to once `accumulated` (the travel since the
    /// touch started) passes `touch_slop`; `None` while it has not. One axis:
    /// monodrag.dart's `_hasSufficientGlobalDistanceToAccept`, the travel
    /// along that axis past the hit slop. Otherwise any direction, locked to
    /// the dominant axis.
    fn accepts(self, accumulated: Point<Pixels>, touch_slop: Pixels) -> Option<Axis> {
        match self {
            PanAxes::One(axis @ Axis::Vertical) => {
                (accumulated.y.abs() > touch_slop).then_some(axis)
            }
            PanAxes::One(axis @ Axis::Horizontal) => {
                (accumulated.x.abs() > touch_slop).then_some(axis)
            }
            PanAxes::NoContainer | PanAxes::Both => (accumulated.magnitude()
                > f64::from(touch_slop))
            .then(|| dominant_axis(accumulated)),
        }
    }
}

impl Sealed for TouchDragEvent {}
impl InputEvent for TouchDragEvent {
    fn to_platform_input(self) -> PlatformInput {
        PlatformInput::TouchDrag(self)
    }
}
impl GestureEvent for TouchDragEvent {}
impl MouseEvent for TouchDragEvent {}

/// A back-navigation gesture, such as the iOS swipe from the leading screen
/// edge.
///
/// Elements receive it through
/// [`InteractiveElement::on_back_gesture`](crate::InteractiveElement::on_back_gesture).
/// The innermost element under [`Self::start_position`] claims the whole
/// gesture when it starts; the platform cancels the gesture if no element does.
/// Every phase is delivered at the start position, so the claiming element
/// should stay in place while it animates its contents.
#[derive(Clone, Debug)]
pub struct BackGestureEvent {
    /// The phase of the gesture.
    pub phase: BackGesturePhase,
    /// Where the gesture started.
    pub start_position: Point<Pixels>,
    /// How far the gesture has travelled toward going back, from 0 to 1.
    pub progress: f32,
    /// What the user did to go back.
    pub source: BackGestureSource,
}

/// What produced a [`BackGestureEvent`]. No default: every gesture names
/// its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackGestureSource {
    /// A swipe from the screen edge, recognized from touches (iOS).
    EdgeSwipe,
    /// A key or button with no pointer behind it, such as Android's Back key
    /// or navigation-bar button.
    Key,
}

/// The phase of a [`BackGestureEvent`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackGesturePhase {
    /// The gesture began. Claiming it here routes the remaining phases to the
    /// same element.
    #[default]
    Started,
    /// The gesture moved; [`BackGestureEvent::progress`] changed.
    Progressed,
    /// The gesture ended and the platform decided to go back.
    Committed,
    /// The gesture ended without going back.
    Cancelled,
}

impl Sealed for BackGestureEvent {}
impl InputEvent for BackGestureEvent {
    fn to_platform_input(self) -> PlatformInput {
        PlatformInput::BackGesture(self)
    }
}
impl GestureEvent for BackGestureEvent {}
impl MouseEvent for BackGestureEvent {}

/// A phased long-press gesture recognized from a touch.
#[derive(Clone, Debug)]
pub struct LongPressEvent {
    /// The phase of the long press.
    pub phase: TouchPhase,
    /// The position where the touch started.
    pub start_position: Point<Pixels>,
    /// The touch's current position.
    pub position: Point<Pixels>,
}

impl Default for LongPressEvent {
    fn default() -> Self {
        Self {
            phase: TouchPhase::Started,
            start_position: Point::default(),
            position: Point::default(),
        }
    }
}

impl Sealed for LongPressEvent {}
impl InputEvent for LongPressEvent {
    fn to_platform_input(self) -> PlatformInput {
        PlatformInput::LongPress(self)
    }
}
impl GestureEvent for LongPressEvent {}
impl MouseEvent for LongPressEvent {}

/// Platform gesture recognition services.
///
/// If your mobile platform supports native gesture recognition, use this
/// to share it with GPUI.
pub trait PlatformGestures {
    /// Feel constants for the portable recognizers on this platform.
    fn tuning(&self) -> GestureTuning {
        GestureTuning::default()
    }

    /// The gesture kinds this platform recognizes natively.
    fn native_recognizers(&self) -> GestureKinds {
        GestureKinds::NONE
    }
}

/// A no-op [`PlatformGestures`] implementation: no native recognizers and
/// default tuning. Suitable for desktop platforms and tests.
pub struct NullPlatformGestures;

impl PlatformGestures for NullPlatformGestures {}

/// The portable recognizer behind raw touch input: it watches the
/// [`TouchEvent`] stream for one touch at a time and resolves it into either
/// a tap or a pan, following the competition model described in the module
/// docs. Pans continue into post-release momentum when the touch lifts at
/// speed; the window drives that phase through [`Self::tick_momentum`].
///
/// Taps are currently surfaced as synthesized mouse presses rather than
/// [`ClickEvent::Touch`](crate::ClickEvent), which keeps every existing
/// mouse-driven behavior (click listeners, caret placement, double-tap
/// selection) working before elements grow a direct tap-delivery path.
/// Additional touches are ignored while one is being recognized; two or more
/// on an element with `on_pinch` may become a pinch ([`TouchPinch`]).
pub(crate) struct TouchGestureRecognizer {
    tuning: GestureTuning,
    state: TouchGestureState,
    momentum: Option<Momentum>,
    last_tap: Option<CompletedTap>,
}

/// A semantic event recognized from raw touches, ready to dispatch through
/// the window's existing input paths.
#[derive(Debug)]
pub(crate) enum RecognizedTouchGesture {
    /// One step of a pan (or of its post-release momentum), delivered to
    /// scroll listeners at the pan's starting position.
    Scroll(ScrollEvent),
    /// A recognized tap, delivered as a synthesized mouse press and release.
    Tap {
        down: MouseDownEvent,
        up: MouseUpEvent,
    },
    TouchDrag(TouchDragEvent),
    LongPress(LongPressEvent),
}

/// The touch recognizer's hold on its touch, in Flutter's arena terms: what
/// the pinch arena competes with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TouchClaim {
    /// It has no touch.
    None,
    /// It has this touch, still a tap or a pan to be.
    Possible(TouchId),
    /// Its pan won this touch, and takes any touch that joins it (Flutter's
    /// drag accepts a joining pointer).
    Pan(TouchId),
    /// An element claimed this touch's long press or touch drag.
    Press(TouchId),
}

enum TouchGestureState {
    Idle,
    /// No pan or drag has accepted the touch yet: it can still resolve into a
    /// pan, a drag, or (while `press_slop` is `Within`) a tap or long press.
    Pending {
        touch: ActiveTouch,
        deadline: Instant,
        long_press_offered: bool,
        touch_drag: TouchDragOffer,
        pan_axes: PanAxes,
        press_slop: PressSlop,
    },
    /// The touch exceeded `touch_slop`: it is a pan until it ends, and its
    /// movement flows out as scroll events.
    Panning {
        touch: ActiveTouch,
        axis: Axis,
    },
    /// The touch caught a fling: it is a pan from its first pixel, and can
    /// never be a tap, but its axis is its own — decided by its first
    /// movement, not inherited from the fling it stopped.
    Caught(ActiveTouch),
    LongPressing(ActiveTouch),
    TouchDragging {
        touch: ActiveTouch,
        start: TouchDragStart,
    },
}

/// Whether a pending touch is still a tap or long press to be: Flutter's
/// `PrimaryPointerGestureRecognizer` rejects both once the touch strays past
/// `preAcceptSlopTolerance` (`kTouchSlop`) from where it went down, whatever
/// the drag recognizers do. A one-axis container's pan or a competing drag can
/// still win an `Exceeded` touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PressSlop {
    Within,
    Exceeded,
}

/// A pending touch's drag offer: the touch-down dispatch offers it once.
#[derive(Clone, Copy)]
enum TouchDragOffer {
    NotOffered,
    /// Offered; nothing competes for it (a claim leaves `Pending`).
    Offered,
    /// The deepest element that competes for it, with Flutter's
    /// `_globalDistanceMoved` so far: the touch's travel as its kind counts it.
    Competing {
        kind: TouchDragKind,
        global_distance_moved: f32,
    },
}

/// What a tap lands on: the bounds of the innermost hitbox under the touch
/// where it starts, `None` when it hits none. Flutter counts double taps per
/// recognizer, so per widget; GPUI's one recognizer per window counts them per
/// target instead.
pub(crate) type TapTarget = Option<Bounds<Pixels>>;

struct ActiveTouch {
    id: TouchId,
    /// What the touch started on; read when it ends as a tap.
    target: TapTarget,
    /// When the touch started: a tap's count is decided from it.
    start_time: Instant,
    /// What the gestures recognized from this touch report as their device.
    device: PointerDevice,
    start_position: Point<Pixels>,
    /// The latest raw position reported for this touch.
    last_position: Point<Pixels>,
    /// The position pan output has scrolled to so far. While panning this
    /// may run ahead of the raw touch by the event's predicted position;
    /// the release event targets the raw position again, so the total
    /// scrolled distance always converges to the finger's actual travel.
    emitted_position: Point<Pixels>,
    /// Retained across stationary samples so prediction corrections cannot
    /// reverse a pan when integer browser coordinates repeat.
    last_movement: Point<Pixels>,
    velocity_tracker: VelocityTracker,
}

struct CompletedTap {
    target: TapTarget,
    start_position: Point<Pixels>,
    start_time: Instant,
    end_time: Instant,
    count: usize,
}

/// One fling in progress. The trajectory is a closed-form curve of elapsed
/// time — each tick evaluates it and emits the increment — so a stalled
/// frame simply resumes further along the same curve. The fling ends on the
/// first tick its simulation is done at, as Flutter's ballistic scroll does.
struct Momentum {
    /// The device of the touch that flung.
    device: PointerDevice,
    /// Where the pan started; synthesized scroll events keep hit-testing
    /// there so momentum stays with the container the gesture began on.
    position: Point<Pixels>,
    /// Unit vector of the release velocity.
    direction: Point<f32>,
    /// Distance along `direction` over time, from the release speed.
    simulation: Box<dyn Simulation>,
    started_at: Instant,
    /// Distance already emitted along `direction`, in pixels.
    emitted_distance: f32,
}

impl TouchGestureRecognizer {
    pub(crate) fn tuning(&self) -> GestureTuning {
        self.tuning
    }

    /// The touch this recognizer has, and whether it won it.
    pub(crate) fn claim(&self) -> TouchClaim {
        match &self.state {
            TouchGestureState::Idle => TouchClaim::None,
            TouchGestureState::Pending { touch, .. } | TouchGestureState::Caught(touch) => {
                TouchClaim::Possible(touch.id)
            }
            TouchGestureState::Panning { touch, .. } => TouchClaim::Pan(touch.id),
            TouchGestureState::LongPressing(touch)
            | TouchGestureState::TouchDragging { touch, .. } => TouchClaim::Press(touch.id),
        }
    }

    pub(crate) fn new(tuning: GestureTuning) -> Self {
        Self {
            tuning,
            state: TouchGestureState::Idle,
            momentum: None,
            last_tap: None,
        }
    }

    /// Runs with `tuning` from the next event on; touches in progress keep
    /// their state and are judged by the new values.
    pub(crate) fn set_tuning(&mut self, tuning: GestureTuning) {
        self.tuning = tuning;
    }

    /// Each gesture is paired with the device it reports: the touch's own,
    /// except the closing step of a fling this touch catches, which keeps the
    /// device of the touch that flung, like the momentum steps before it.
    /// `now` is the executor's clock, so tests can drive time. `target` is
    /// what the event's position hits; a touch keeps the one it started on.
    pub(crate) fn handle_event(
        &mut self,
        event: &TouchEvent,
        now: Instant,
        target: TapTarget,
    ) -> SmallVec<[(PointerDevice, RecognizedTouchGesture); 2]> {
        let flung_by = match event.phase {
            TouchPhase::Started => self.momentum.as_ref().map(|momentum| momentum.device),
            TouchPhase::Moved | TouchPhase::Ended | TouchPhase::Cancelled => None,
        };
        let mut gestures = self.handle_event_at(event, now, target).into_iter();
        // A touch that catches a fling closes the fling's scroll first.
        let fling_end = flung_by.and_then(|device| Some((device, gestures.next()?)));
        fling_end
            .into_iter()
            .chain(gestures.map(|gesture| (event.device, gesture)))
            .collect()
    }

    fn handle_event_at(
        &mut self,
        event: &TouchEvent,
        now: Instant,
        target: TapTarget,
    ) -> SmallVec<[RecognizedTouchGesture; 2]> {
        // Velocity samples are timed by the platform's event time, as Flutter's
        // trackers take `PointerEvent.timeStamp`; timers and deadlines stay on
        // `now`, the dispatch clock, as Flutter's run on its own timers.
        let sampled_at = event.time_stamp.unwrap_or(now);
        let mut recognized = SmallVec::new();
        match event.phase {
            TouchPhase::Started => {
                // Pushed first: `handle_event` tags it with the momentum's device.
                let caught_fling = if let Some(momentum) = self.momentum.take() {
                    recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                        momentum.position,
                        Point::default(),
                        TouchPhase::Ended,
                    )));
                    true
                } else {
                    false
                };
                if matches!(self.state, TouchGestureState::Idle) {
                    let mut velocity_tracker =
                        VelocityTracker::with_kind(self.tuning.velocity_tracker);
                    velocity_tracker.add_position(sampled_at, event.position);
                    let touch = ActiveTouch {
                        id: event.id,
                        target,
                        start_time: now,
                        device: event.device,
                        start_position: event.position,
                        last_position: event.position,
                        emitted_position: event.position,
                        last_movement: Point::default(),
                        velocity_tracker,
                    };
                    if caught_fling {
                        // A touch that catches a fling is a drag from the
                        // first pixel: waiting out the slop would freeze the
                        // content mid-scroll and then jump. It can also never
                        // be a tap; releasing it just leaves the content
                        // stopped, as on Android and iOS. Which way it pans
                        // is its own to decide: a finger that stops a
                        // sideways fling and moves up scrolls up.
                        recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                            touch.start_position,
                            Point::default(),
                            TouchPhase::Started,
                        )));
                        self.state = TouchGestureState::Caught(touch);
                    } else {
                        self.state = TouchGestureState::Pending {
                            touch,
                            deadline: now + self.tuning.long_press_duration,
                            long_press_offered: false,
                            touch_drag: TouchDragOffer::NotOffered,
                            pan_axes: PanAxes::NoContainer,
                            press_slop: PressSlop::Within,
                        };
                    }
                }
            }
            TouchPhase::Moved => match mem::replace(&mut self.state, TouchGestureState::Idle) {
                TouchGestureState::Pending {
                    mut touch,
                    deadline,
                    long_press_offered,
                    mut touch_drag,
                    pan_axes,
                    press_slop,
                } if touch.id == event.id => {
                    let moved = event.position - touch.last_position;
                    touch
                        .velocity_tracker
                        .add_position(sampled_at, event.position);
                    touch.last_position = event.position;
                    let accumulated = event.position - touch.start_position;
                    let press_slop = match press_slop {
                        PressSlop::Within
                            if accumulated.magnitude() > f64::from(self.tuning.touch_slop) =>
                        {
                            PressSlop::Exceeded
                        }
                        kept => kept,
                    };
                    // A competing drag sees the move before the pan does, as
                    // the deeper recognizer's route runs first in Flutter: on
                    // a move both accept, the drag wins.
                    let pan_axis = pan_axes.accepts(accumulated, self.tuning.touch_slop);
                    let drag_won = match &mut touch_drag {
                        TouchDragOffer::Competing {
                            kind,
                            global_distance_moved,
                        } => {
                            *global_distance_moved += kind.global_distance_moved(moved);
                            kind.has_sufficient_global_distance_to_accept(
                                *global_distance_moved,
                                slops(touch.device, Some(self.tuning.touch_slop)),
                            )
                            .then_some(*kind)
                        }
                        TouchDragOffer::NotOffered | TouchDragOffer::Offered => None,
                    };
                    if let Some(kind) = drag_won {
                        let start = TouchDragStart::PastSlop(kind);
                        recognized.push(RecognizedTouchGesture::TouchDrag(TouchDragEvent {
                            phase: TouchPhase::Started,
                            start,
                            start_position: touch.start_position,
                            position: event.position,
                        }));
                        self.state = TouchGestureState::TouchDragging { touch, start };
                    } else if let Some(axis) = pan_axis {
                        // Carry the full movement so far into the first scroll
                        // step: the content catches up to the finger instead
                        // of losing the slop distance.
                        let mut target = event.predicted_position.unwrap_or(event.position);
                        let mut delta = target - touch.start_position;
                        lock_delta_to_axis(&mut delta, axis);
                        touch.last_movement = accumulated;
                        lock_delta_to_axis(&mut touch.last_movement, axis);
                        if movements_oppose(delta, touch.last_movement) {
                            target = event.position;
                            delta = accumulated;
                            lock_delta_to_axis(&mut delta, axis);
                        }
                        touch.emitted_position = target;
                        recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                            touch.start_position,
                            delta,
                            TouchPhase::Started,
                        )));
                        self.state = TouchGestureState::Panning { touch, axis };
                    } else {
                        self.state = TouchGestureState::Pending {
                            touch,
                            deadline,
                            long_press_offered,
                            touch_drag,
                            pan_axes,
                            press_slop,
                        };
                    }
                }
                TouchGestureState::Caught(touch) if touch.id == event.id => {
                    let accumulated = event.position - touch.start_position;
                    if accumulated == Point::default() {
                        self.state = TouchGestureState::Caught(touch);
                    } else {
                        // The first movement picks the axis; from here on this
                        // is an ordinary pan, so let the pan arm emit it.
                        let axis = dominant_axis(accumulated);
                        self.state = TouchGestureState::Panning { touch, axis };
                        return self.handle_event_at(event, now, target);
                    }
                }
                TouchGestureState::Panning { mut touch, axis } if touch.id == event.id => {
                    let mut raw_delta = event.position - touch.last_position;
                    lock_delta_to_axis(&mut raw_delta, axis);
                    if raw_delta != Point::default() {
                        touch.last_movement = raw_delta;
                    }
                    touch
                        .velocity_tracker
                        .add_position(sampled_at, event.position);
                    touch.last_position = event.position;
                    let mut target = event.predicted_position.unwrap_or(event.position);
                    let mut delta = target - touch.emitted_position;
                    lock_delta_to_axis(&mut delta, axis);
                    // Prediction error must not reverse content while the raw
                    // touch still advances. Fall back to the raw position so a
                    // real finger reversal remains responsive.
                    if movements_oppose(delta, touch.last_movement) {
                        target = event.position;
                        delta = target - touch.emitted_position;
                        lock_delta_to_axis(&mut delta, axis);
                        if movements_oppose(delta, touch.last_movement) {
                            target = touch.emitted_position;
                            delta = Point::default();
                        }
                    }
                    touch.emitted_position = target;
                    recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                        touch.start_position,
                        delta,
                        TouchPhase::Moved,
                    )));
                    self.state = TouchGestureState::Panning { touch, axis };
                }
                TouchGestureState::LongPressing(mut touch) if touch.id == event.id => {
                    touch.last_position = event.position;
                    recognized.push(RecognizedTouchGesture::LongPress(LongPressEvent {
                        phase: TouchPhase::Moved,
                        start_position: touch.start_position,
                        position: event.position,
                    }));
                    self.state = TouchGestureState::LongPressing(touch);
                }
                TouchGestureState::TouchDragging { mut touch, start } if touch.id == event.id => {
                    touch.last_position = event.position;
                    recognized.push(RecognizedTouchGesture::TouchDrag(TouchDragEvent {
                        phase: TouchPhase::Moved,
                        start,
                        start_position: touch.start_position,
                        position: event.position,
                    }));
                    self.state = TouchGestureState::TouchDragging { touch, start };
                }
                other => self.state = other,
            },
            TouchPhase::Ended => match mem::replace(&mut self.state, TouchGestureState::Idle) {
                TouchGestureState::Pending {
                    touch,
                    press_slop: PressSlop::Exceeded,
                    ..
                } if touch.id == event.id => {
                    // Strayed past the tap's slop: no tap, and a double tap's
                    // first tap is rejected with it (multitap.dart `_reject`).
                    self.last_tap = None;
                }
                TouchGestureState::Pending {
                    touch,
                    press_slop: PressSlop::Within,
                    ..
                } if touch.id == event.id => {
                    // Ported from flutter/flutter packages/flutter/lib/src/gestures/multitap.dart
                    // @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7)
                    // (DoubleTapGestureRecognizer.addAllowedPointer, _TapTracker).
                    // Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
                    // The double-tap timer runs from the previous tap's up event and
                    // resets the sequence when it fires. Out-of-bounds second taps, and
                    // ones too close to the first (touch screens often detect touches
                    // intermittently), start a new sequence. So does a tap on
                    // another target: Flutter's recognizer only sees its own
                    // widget's pointers.
                    let tap_count = match &self.last_tap {
                        Some(tap)
                            if touch.target == tap.target
                                && touch.start_time.duration_since(tap.end_time)
                                    < self.tuning.multi_tap_interval
                                && (touch.start_position - tap.start_position).magnitude()
                                    <= f64::from(self.tuning.multi_tap_slop)
                                && touch.start_time.duration_since(tap.start_time)
                                    >= self.tuning.multi_tap_min_interval =>
                        {
                            tap.count + 1
                        }
                        _ => 1,
                    };
                    self.last_tap = Some(CompletedTap {
                        target: touch.target,
                        start_position: touch.start_position,
                        start_time: touch.start_time,
                        end_time: now,
                        count: tap_count,
                    });
                    recognized.push(RecognizedTouchGesture::Tap {
                        down: MouseDownEvent {
                            button: MouseButton::Left,
                            position: event.position,
                            modifiers: Modifiers::default(),
                            click_count: tap_count,
                            first_mouse: false,
                        },
                        up: MouseUpEvent {
                            button: MouseButton::Left,
                            position: event.position,
                            modifiers: Modifiers::default(),
                            click_count: tap_count,
                        },
                    });
                }
                TouchGestureState::Caught(touch) if touch.id == event.id => {
                    // Never moved: the content it stopped stays where it is.
                    recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                        touch.start_position,
                        Point::default(),
                        TouchPhase::Ended,
                    )));
                }
                TouchGestureState::Panning { touch, axis } if touch.id == event.id => {
                    // The release deliberately contributes no velocity
                    // sample: it usually repeats the last movement's position
                    // with a later timestamp, which would dilute the
                    // estimate. A release long after the last movement
                    // means the finger had already stopped: the tracker
                    // reports zero, so nothing flings.
                    let VelocityEstimate {
                        pixels_per_second: mut velocity,
                        mut offset,
                    } = touch.velocity_tracker.get_velocity_estimate(sampled_at);
                    match axis {
                        Axis::Vertical => (velocity.x, offset.x) = (0., 0.),
                        Axis::Horizontal => (velocity.y, offset.y) = (0., 0.),
                    }
                    let speed = (velocity.x.powi(2) + velocity.y.powi(2)).sqrt();
                    let distance = (offset.x.powi(2) + offset.y.powi(2)).sqrt();
                    let mut release_delta = event.position - touch.emitted_position;
                    lock_delta_to_axis(&mut release_delta, axis);
                    if self.tuning.is_fling_gesture(speed, distance) {
                        let direction = point(velocity.x / speed, velocity.y / speed);
                        let speed = speed.min(self.tuning.max_fling_velocity);
                        if let Some(simulation) =
                            fling_simulation(self.tuning.scroll_physics, speed)
                        {
                            let total_distance = simulation.x(f64::INFINITY) as f32;
                            // Prediction may have left the content ahead of
                            // the raw release position. Emitting that
                            // correction here would visibly snap the content
                            // backwards just as the fling launches, so fold
                            // it into the fling instead: start the curve
                            // already advanced by the overshoot, keeping the
                            // total travel exact while staying monotonic.
                            let overshoot = -(f32::from(release_delta.x) * direction.x
                                + f32::from(release_delta.y) * direction.y);
                            let emitted_distance = if overshoot > 0. && overshoot < total_distance {
                                release_delta +=
                                    point(px(direction.x * overshoot), px(direction.y * overshoot));
                                overshoot
                            } else {
                                0.
                            };
                            self.momentum = Some(Momentum {
                                device: touch.device,
                                position: touch.start_position,
                                direction,
                                simulation,
                                started_at: now,
                                emitted_distance,
                            });
                        }
                    }
                    recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                        touch.start_position,
                        release_delta,
                        TouchPhase::Ended,
                    )));
                }
                TouchGestureState::LongPressing(touch) if touch.id == event.id => {
                    recognized.push(RecognizedTouchGesture::LongPress(LongPressEvent {
                        phase: TouchPhase::Ended,
                        start_position: touch.start_position,
                        position: event.position,
                    }));
                }
                TouchGestureState::TouchDragging { touch, start } if touch.id == event.id => {
                    recognized.push(RecognizedTouchGesture::TouchDrag(TouchDragEvent {
                        phase: TouchPhase::Ended,
                        start,
                        start_position: touch.start_position,
                        position: event.position,
                    }));
                }
                other => self.state = other,
            },
            TouchPhase::Cancelled => match mem::replace(&mut self.state, TouchGestureState::Idle) {
                TouchGestureState::Pending { touch, .. } if touch.id == event.id => {}
                TouchGestureState::Panning { touch, .. } | TouchGestureState::Caught(touch)
                    if touch.id == event.id =>
                {
                    recognized.push(RecognizedTouchGesture::Scroll(scroll_event(
                        touch.start_position,
                        Point::default(),
                        TouchPhase::Cancelled,
                    )));
                }
                TouchGestureState::LongPressing(touch) if touch.id == event.id => {
                    recognized.push(RecognizedTouchGesture::LongPress(LongPressEvent {
                        phase: TouchPhase::Cancelled,
                        start_position: touch.start_position,
                        position: event.position,
                    }));
                }
                TouchGestureState::TouchDragging { touch, start } if touch.id == event.id => {
                    recognized.push(RecognizedTouchGesture::TouchDrag(TouchDragEvent {
                        phase: TouchPhase::Cancelled,
                        start,
                        start_position: touch.start_position,
                        position: event.position,
                    }));
                }
                other => self.state = other,
            },
        }
        recognized
    }

    pub(crate) fn pending_long_press(
        &self,
        now: Instant,
    ) -> Option<(TouchId, PointerDevice, Duration)> {
        let TouchGestureState::Pending {
            touch,
            deadline,
            long_press_offered: false,
            press_slop: PressSlop::Within,
            ..
        } = &self.state
        else {
            return None;
        };
        Some((
            touch.id,
            touch.device,
            deadline.saturating_duration_since(now),
        ))
    }

    pub(crate) fn offer_long_press(&mut self, id: TouchId) -> Option<RecognizedTouchGesture> {
        let TouchGestureState::Pending {
            touch,
            long_press_offered,
            press_slop: PressSlop::Within,
            ..
        } = &mut self.state
        else {
            return None;
        };
        if touch.id != id || *long_press_offered {
            return None;
        }
        *long_press_offered = true;
        Some(RecognizedTouchGesture::LongPress(LongPressEvent {
            phase: TouchPhase::Started,
            start_position: touch.start_position,
            position: touch.last_position,
        }))
    }

    pub(crate) fn resolve_long_press(&mut self, claimed: bool) {
        if !claimed {
            return;
        }
        let state = mem::replace(&mut self.state, TouchGestureState::Idle);
        self.state = match state {
            TouchGestureState::Pending {
                touch,
                long_press_offered: true,
                ..
            } => TouchGestureState::LongPressing(touch),
            other => other,
        };
    }

    /// A touch that holds a moving scroll container (Flutter's
    /// `HoldScrollActivity`) is a pan from its first pixel, as one that
    /// catches this recognizer's fling is: it opens the scroll stream at once
    /// and never taps or long-presses.
    pub(crate) fn hold(&mut self, id: TouchId) -> Option<RecognizedTouchGesture> {
        match mem::replace(&mut self.state, TouchGestureState::Idle) {
            TouchGestureState::Pending { touch, .. } if touch.id == id => {
                let started =
                    scroll_event(touch.start_position, Point::default(), TouchPhase::Started);
                self.state = TouchGestureState::Caught(touch);
                Some(RecognizedTouchGesture::Scroll(started))
            }
            other => {
                self.state = other;
                None
            }
        }
    }

    /// After the touch's `Started` dispatch: the scroll axes under it, which
    /// decide when its pan accepts.
    pub(crate) fn set_pan_axes(&mut self, id: TouchId, axes: PanAxes) {
        if let TouchGestureState::Pending {
            touch, pan_axes, ..
        } = &mut self.state
            && touch.id == id
        {
            *pan_axes = axes;
        }
    }

    pub(crate) fn offer_touch_drag(&mut self, id: TouchId) -> Option<RecognizedTouchGesture> {
        let TouchGestureState::Pending {
            touch,
            touch_drag: touch_drag @ TouchDragOffer::NotOffered,
            ..
        } = &mut self.state
        else {
            return None;
        };
        if touch.id != id {
            return None;
        }
        *touch_drag = TouchDragOffer::Offered;
        Some(RecognizedTouchGesture::TouchDrag(TouchDragEvent {
            phase: TouchPhase::Started,
            start: TouchDragStart::Down,
            start_position: touch.start_position,
            position: touch.last_position,
        }))
    }

    /// While the offer is dispatched: the first (deepest) element to compete
    /// for it gets it, unless one claims it.
    pub(crate) fn compete_for_touch_drag(&mut self, kind: TouchDragKind) {
        if let TouchGestureState::Pending { touch_drag, .. } = &mut self.state
            && let TouchDragOffer::Offered = touch_drag
        {
            *touch_drag = TouchDragOffer::Competing {
                kind,
                global_distance_moved: 0.,
            };
        }
    }

    /// After the offer's dispatch: a claim wins the touch now; otherwise it
    /// stays pending, with any drag that competes for it.
    pub(crate) fn resolve_touch_drag(&mut self, claimed: bool) {
        if !claimed {
            return;
        }
        let state = mem::replace(&mut self.state, TouchGestureState::Idle);
        self.state = match state {
            TouchGestureState::Pending {
                touch,
                touch_drag: TouchDragOffer::Offered | TouchDragOffer::Competing { .. },
                ..
            } => TouchGestureState::TouchDragging {
                touch,
                start: TouchDragStart::Down,
            },
            other => other,
        };
    }

    pub(crate) fn has_momentum(&self) -> bool {
        self.momentum.is_some()
    }

    /// The finger velocity of the fling in progress at `now`, in pixels per
    /// second (zero when none): during the release's scroll step, the
    /// release velocity Flutter's `DragEndDetails` carries.
    pub(crate) fn fling_velocity(&self, now: Instant) -> Point<f32> {
        self.momentum
            .as_ref()
            .map_or_else(Point::default, |momentum| {
                let elapsed = now.duration_since(momentum.started_at).as_secs_f64();
                let speed = momentum.simulation.dx(elapsed) as f32;
                point(momentum.direction.x * speed, momentum.direction.y * speed)
            })
    }

    /// Advances post-fling momentum by one frame, returning the scroll step
    /// to dispatch, or `None` when no momentum is in progress. The final step
    /// carries [`TouchPhase::Ended`] to close the synthetic scroll stream.
    /// Paired with the device of the touch that flung.
    pub(crate) fn tick_momentum(
        &mut self,
        now: Instant,
    ) -> Option<(PointerDevice, RecognizedTouchGesture)> {
        let device = self.momentum.as_ref()?.device;
        Some((device, self.tick_momentum_at(now)?))
    }

    fn tick_momentum_at(&mut self, now: Instant) -> Option<RecognizedTouchGesture> {
        let momentum = self.momentum.as_mut()?;
        let elapsed = now.duration_since(momentum.started_at).as_secs_f64();
        let distance = momentum.simulation.x(elapsed) as f32;
        // Prediction overshoot can start momentum ahead of its curve. Hold
        // that position until the curve catches up instead of stepping back.
        let step = (distance - momentum.emitted_distance).max(0.);
        momentum.emitted_distance = momentum.emitted_distance.max(distance);
        let delta = point(
            px(momentum.direction.x * step),
            px(momentum.direction.y * step),
        );
        let position = momentum.position;
        if momentum.simulation.is_done(elapsed) {
            self.momentum = None;
            Some(RecognizedTouchGesture::Scroll(scroll_event(
                position,
                delta,
                TouchPhase::Ended,
            )))
        } else {
            Some(RecognizedTouchGesture::Scroll(scroll_event(
                position,
                delta,
                TouchPhase::Moved,
            )))
        }
    }
}

fn scroll_event(
    position: Point<Pixels>,
    delta: Point<Pixels>,
    touch_phase: TouchPhase,
) -> ScrollEvent {
    ScrollEvent {
        position,
        delta: ScrollDelta::Pixels(delta),
        modifiers: Modifiers::default(),
        touch_phase,
        source: ScrollSource::Touch,
    }
}

#[cfg(test)]
pub(crate) mod recognizer_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point;

    #[test]
    fn pan_axes_join_the_containers_under_a_touch() {
        use Axis::{Horizontal, Vertical};
        use PanAxes::{Both, NoContainer, One};
        // (name, axes so far, the joining container, joined)
        let rows = [
            ("a first container sets the axes", NoContainer, One(Vertical), One(Vertical)),
            ("no container joins nothing", One(Vertical), NoContainer, One(Vertical)),
            ("the same axis stays one axis", One(Vertical), One(Vertical), One(Vertical)),
            ("a vertical list in a horizontal pager pans both ways", One(Horizontal), One(Vertical), Both),
            ("a two-axis container pans both ways", One(Vertical), Both, Both),
            ("both stay both", Both, One(Horizontal), Both),
        ];
        for (name, axes, joining, joined) in rows {
            assert_eq!(axes.join(joining), joined, "{name}");
        }
    }

    #[test]
    fn ongoing_scroll_locks_to_dominant_axis() {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let mut horizontal_delta = point(px(10.), px(2.));
        ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);
        assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
        assert_eq!(horizontal_delta, point(px(10.), px(0.)));

        let mut continued_delta = point(px(3.), px(2.));
        ongoing_scroll.filter_at(
            &mut continued_delta,
            TouchPhase::Moved,
            now + Duration::from_millis(1),
        );
        assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
        assert_eq!(continued_delta, point(px(3.), px(0.)));
    }

    #[test]
    fn ongoing_scroll_unlocks_when_direction_changes() {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let mut horizontal_delta = point(px(10.), px(2.));
        ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);

        let mut vertical_delta = point(px(2.), px(10.));
        ongoing_scroll.filter_at(
            &mut vertical_delta,
            TouchPhase::Moved,
            now + Duration::from_millis(1),
        );
        assert_eq!(ongoing_scroll.axis, None);
        assert_eq!(vertical_delta, point(px(2.), px(10.)));
    }

    #[test]
    fn ongoing_scroll_starts_new_gesture_at_timeout_boundary() {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let mut horizontal_delta = point(px(10.), px(2.));
        ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Moved, now);

        let mut vertical_delta = point(px(2.), px(10.));
        ongoing_scroll.filter_at(
            &mut vertical_delta,
            TouchPhase::Moved,
            now + SCROLL_EVENT_SEPARATION,
        );
        assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
        assert_eq!(vertical_delta, point(px(0.), px(10.)));
    }

    #[test]
    fn ongoing_scroll_ignores_zero_delta_and_resets_when_ended() {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let mut horizontal_delta = point(px(10.), px(2.));
        ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);

        let mut zero_delta = Point::default();
        ongoing_scroll.filter_at(
            &mut zero_delta,
            TouchPhase::Ended,
            now + Duration::from_millis(1),
        );
        assert_eq!(ongoing_scroll.axis, None);

        let mut vertical_delta = point(px(2.), px(3.));
        ongoing_scroll.filter_at(
            &mut vertical_delta,
            TouchPhase::Moved,
            now + Duration::from_millis(2),
        );
        assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
        assert_eq!(vertical_delta, point(px(0.), px(3.)));
    }

    #[test]
    fn ongoing_scroll_ignores_zero_delta_movement() {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let mut horizontal_delta = point(px(10.), px(2.));
        ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);

        let mut zero_delta = Point::default();
        ongoing_scroll.filter_at(
            &mut zero_delta,
            TouchPhase::Moved,
            now + SCROLL_EVENT_SEPARATION,
        );

        let mut vertical_delta = point(px(2.), px(10.));
        ongoing_scroll.filter_at(
            &mut vertical_delta,
            TouchPhase::Moved,
            now + SCROLL_EVENT_SEPARATION,
        );
        assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
        assert_eq!(vertical_delta, point(px(0.), px(10.)));
    }

    #[test]
    fn ongoing_scroll_supports_moved_only_platforms() {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let mut horizontal_delta = point(px(10.), px(2.));
        ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Moved, now);
        assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
        assert_eq!(horizontal_delta, point(px(10.), px(0.)));
    }

    #[test]
    fn touch_within_slop_resolves_to_tap() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 10., 10.),
            now,
            None,
        );
        assert!(recognized.is_empty());
        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 12., 11.),
            now + Duration::from_millis(20),
            None,
        );
        assert!(recognized.is_empty());

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 12., 11.),
            now + Duration::from_millis(60),
            None,
        );
        let [RecognizedTouchGesture::Tap { down, up }] = recognized.as_slice() else {
            panic!("expected tap, got {recognized:?}");
        };
        assert_eq!(down.click_count, 1);
        assert_eq!(down.position, point(px(12.), px(11.)));
        assert_eq!(up.click_count, 1);
    }

    #[test]
    fn consecutive_taps_accumulate_tap_count() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();

        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Started, 10., 10.),
            now,
            None,
        );
        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Ended, 10., 10.),
            now + Duration::from_millis(40),
            None,
        );

        let second_down = now + Duration::from_millis(200);
        recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Started, 14., 10.),
            second_down,
            None,
        );
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Ended, 14., 10.),
            second_down + Duration::from_millis(40),
            None,
        );
        let [RecognizedTouchGesture::Tap { down, .. }] = recognized.as_slice() else {
            panic!("expected tap, got {recognized:?}");
        };
        assert_eq!(down.click_count, 2);

        let late_down = second_down + Duration::from_secs(2);
        recognizer.handle_event_at(
            &touch_event(TouchId(3), TouchPhase::Started, 14., 10.),
            late_down,
            None,
        );
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(3), TouchPhase::Ended, 14., 10.),
            late_down + Duration::from_millis(40),
            None,
        );
        let [RecognizedTouchGesture::Tap { down, .. }] = recognized.as_slice() else {
            panic!("expected tap, got {recognized:?}");
        };
        assert_eq!(down.click_count, 1);
    }

    #[test]
    fn touch_beyond_slop_resolves_to_pan() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 100., 120.),
            now + Duration::from_millis(16),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Started);
        assert_eq!(scroll.position, point(px(100.), px(100.)));
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(20.)));

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 100., 135.),
            now + Duration::from_millis(32),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Moved);
        assert_eq!(scroll.position, point(px(100.), px(100.)));
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(15.)));

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 100., 135.),
            now + Duration::from_millis(48),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Ended);
    }

    #[test]
    fn touch_pan_stays_locked_to_its_initial_dominant_axis() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 104., 120.),
            now + Duration::from_millis(16),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(20.)));

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 134., 125.),
            now + Duration::from_millis(32),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(5.)));
    }

    #[test]
    fn touch_pan_locks_to_horizontal_axis() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );

        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 120., 104.),
            now + Duration::from_millis(16),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(20.), px(0.)));
    }

    #[test]
    fn predicted_positions_lead_the_pan_but_totals_converge_on_release() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );

        // The first pan step scrolls to the predicted position, not the raw one.
        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 120.);
        moved.predicted_position = Some(point(px(106.), px(128.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(16), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(28.)));

        // The next step is measured from where the previous prediction left
        // the content, so an overshoot is paid back here.
        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 130.);
        moved.predicted_position = Some(point(px(104.), px(134.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(32), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(6.)));

        // A release without a fling (the finger stopped long before lifting)
        // targets the raw position: the total scrolled distance equals the
        // finger's actual travel despite the predictions.
        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 100., 130.),
            now + Duration::from_millis(120),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Ended);
        assert!(!recognizer.has_momentum());
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(-4.)));
    }

    #[test]
    fn predicted_positions_do_not_emit_false_reversals() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );

        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 120.);
        moved.predicted_position = Some(point(px(100.), px(130.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(16), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(30.)));

        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 125.);
        moved.predicted_position = Some(point(px(100.), px(127.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(32), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(
            scroll.delta.pixel_delta(px(16.)),
            Point::<Pixels>::default()
        );

        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 125.);
        moved.predicted_position = Some(point(px(100.), px(126.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(40), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(
            scroll.delta.pixel_delta(px(16.)),
            Point::<Pixels>::default()
        );

        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 132.);
        moved.predicted_position = Some(point(px(100.), px(136.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(48), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(6.)));

        let mut moved = touch_event(touch, TouchPhase::Moved, 100., 124.);
        moved.predicted_position = Some(point(px(100.), px(140.)));
        let recognized = recognizer.handle_event_at(&moved, now + Duration::from_millis(64), None);
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(-12.)));
    }

    #[test]
    fn predicted_overshoot_folds_into_the_fling_without_scrolling_backwards() {
        let now = Instant::now();
        let mut total_with_prediction = 0f32;
        let mut total_without_prediction = 0f32;
        for use_prediction in [true, false] {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
            let mut total = 0f32;
            let mut drain = |recognized: &[RecognizedTouchGesture], upward_only: bool| {
                for gesture in recognized {
                    let RecognizedTouchGesture::Scroll(scroll) = gesture else {
                        panic!("expected scroll, got {gesture:?}");
                    };
                    let delta = scroll.delta.pixel_delta(px(16.)).y;
                    if upward_only {
                        assert!(
                            delta <= px(0.),
                            "content moved backwards by {delta:?} during an upward gesture"
                        );
                    }
                    total += f32::from(delta);
                }
            };

            recognizer.handle_event_at(
                &touch_event(TouchId(1), TouchPhase::Started, 100., 500.),
                now,
                None,
            );
            for step in 1..=5u64 {
                let raw_y = 500. - step as f32 * 40.;
                let mut moved = touch_event(TouchId(1), TouchPhase::Moved, 100., raw_y);
                if use_prediction {
                    moved.predicted_position = Some(point(px(100.), px(raw_y - 25.)));
                }
                let recognized = recognizer.handle_event_at(
                    &moved,
                    now + Duration::from_millis(step * 16),
                    None,
                );
                drain(&recognized, use_prediction);
            }
            // The release leaves the emitted position 25px ahead of the raw
            // one; with prediction the correction must not scroll backwards.
            let recognized = recognizer.handle_event_at(
                &touch_event(TouchId(1), TouchPhase::Ended, 100., 300.),
                now + Duration::from_millis(90),
                None,
            );
            drain(&recognized, use_prediction);
            assert!(recognizer.has_momentum());
            let mut tick = now + Duration::from_millis(91);
            while recognizer.has_momentum() {
                if let Some(gesture) = recognizer.tick_momentum_at(tick) {
                    drain(&[gesture], use_prediction);
                }
                tick += Duration::from_millis(16);
            }

            if use_prediction {
                total_with_prediction = total;
            } else {
                total_without_prediction = total;
            }
        }
        // Folding the overshoot into the fling redistributes the travel but
        // must not change where the content comes to rest.
        assert!(
            (total_with_prediction - total_without_prediction).abs() < 0.01,
            "totals diverged: {total_with_prediction} vs {total_without_prediction}"
        );
    }

    #[test]
    fn fast_release_starts_momentum_that_decays_to_a_stop() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 300.),
            now,
            None,
        );
        for step in 1..=5 {
            recognizer.handle_event_at(
                &touch_event(touch, TouchPhase::Moved, 100., 300. - step as f32 * 20.),
                now + Duration::from_millis(step * 16),
                None,
            );
        }
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 100., 200.),
            now + Duration::from_millis(6 * 16),
            None,
        );
        assert!(recognizer.has_momentum());

        let tick = now + Duration::from_millis(6 * 16 + 16);
        let recognized = recognizer.tick_momentum_at(tick);
        let Some(RecognizedTouchGesture::Scroll(scroll)) = recognized else {
            panic!("expected momentum scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Moved);
        assert_eq!(scroll.position, point(px(100.), px(300.)));
        let delta = scroll.delta.pixel_delta(px(16.));
        assert!(
            delta.y < px(0.),
            "momentum should continue upward, got {delta:?}"
        );
        // The least-squares fit may leave float residue on the motionless axis.
        assert!(
            delta.x.abs() < px(0.001),
            "expected no x motion, got {delta:?}"
        );

        let mut last_phase = TouchPhase::Moved;
        let mut ticks = 0;
        let mut time = tick;
        while recognizer.has_momentum() {
            time += Duration::from_millis(16);
            ticks += 1;
            assert!(ticks < 1000, "momentum never stopped");
            if let Some(RecognizedTouchGesture::Scroll(scroll)) = recognizer.tick_momentum_at(time)
            {
                last_phase = scroll.touch_phase;
            }
        }
        assert_eq!(last_phase, TouchPhase::Ended);
        assert!(recognizer.tick_momentum_at(time).is_none());
    }

    #[test]
    fn diagonal_release_flings_only_on_locked_axis() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 300.),
            now,
            None,
        );
        for step in 1..=5 {
            let recognized = recognizer.handle_event_at(
                &touch_event(
                    touch,
                    TouchPhase::Moved,
                    100. + step as f32 * 3.,
                    300. - step as f32 * 20.,
                ),
                now + Duration::from_millis(step * 16),
                None,
            );
            let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
                panic!("expected scroll, got {recognized:?}");
            };
            assert_eq!(scroll.delta.pixel_delta(px(16.)).x, px(0.));
        }
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 115., 200.),
            now + Duration::from_millis(6 * 16),
            None,
        );
        assert!(recognizer.has_momentum());

        let mut time = now + Duration::from_millis(6 * 16);
        while recognizer.has_momentum() {
            time += Duration::from_millis(16);
            if let Some(RecognizedTouchGesture::Scroll(scroll)) = recognizer.tick_momentum_at(time)
            {
                let delta = scroll.delta.pixel_delta(px(16.));
                assert_eq!(delta.x, px(0.));
                assert!(delta.y <= px(0.));
            }
        }
    }

    /// Flutter's `isFlingGesture` (monodrag.dart): a release flings only when
    /// its estimate is over `minFlingVelocity` and spans more than the
    /// physics' `minFlingDistance` (kTouchSlop). (row, y step per 25 ms over
    /// the last 100 ms, flings)
    #[test]
    fn fling_needs_flutters_velocity_and_distance() {
        let rows = [
            (
                "slow 60 px/s drag moving 6 px: too short to fling",
                1.5,
                false,
            ),
            ("300 px/s drag moving 30 px flings", 7.5, true),
        ];
        for (name, step, flings) in rows {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning {
                scroll_physics: ScrollPhysics::Clamping,
                min_fling_velocity: ScrollPhysics::Clamping.min_fling_velocity(),
                velocity_tracker: VelocityTrackerKind::LeastSquares,
                ..GestureTuning::default()
            });
            let now = Instant::now();
            let at = |ms| now + Duration::from_millis(ms);
            let touch = TouchId(1);
            recognizer.handle_event_at(&touch_event(touch, TouchPhase::Started, 0., 0.), now, None);
            // Past the slop; the pause before the slow part keeps it out of the estimate.
            recognizer.handle_event_at(
                &touch_event(touch, TouchPhase::Moved, 0., 30.),
                at(50),
                None,
            );
            let mut y = 30.;
            for ms in (200..=300).step_by(25) {
                recognizer.handle_event_at(
                    &touch_event(touch, TouchPhase::Moved, 0., y),
                    at(ms),
                    None,
                );
                y += step;
            }
            recognizer.handle_event_at(
                &touch_event(touch, TouchPhase::Ended, 0., y - step),
                at(300),
                None,
            );
            assert_eq!(recognizer.has_momentum(), flings, "{name}");
        }
    }

    /// Flutter's trackers take each event's `timeStamp`: a fling sampled every
    /// 16 ms flings as fast however late the platform delivers the samples.
    /// (row, dispatch delay of each event after its time stamp, in ms)
    #[test]
    fn velocity_follows_event_time_stamps_not_dispatch() {
        let rows: [(&str, fn(u64) -> u64); 3] = [
            ("dispatched as they happen", |_| 0),
            ("dispatched at the next 33 ms frame", |ms| {
                (33 - ms % 33) % 33
            }),
            ("dispatched 50 ms late", |_| 50),
        ];
        let mut velocities = Vec::new();
        for (name, delay) in rows {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning {
                velocity_tracker: VelocityTrackerKind::LeastSquares,
                ..GestureTuning::default()
            });
            let origin = Instant::now();
            let at = |ms| origin + Duration::from_millis(ms);
            let touch = TouchId(1);
            let mut release_dispatched_at = origin;
            for (ms, phase, y) in [
                (0, TouchPhase::Started, 0.),
                (16, TouchPhase::Moved, 40.),
                (32, TouchPhase::Moved, 80.),
                (48, TouchPhase::Moved, 120.),
                (64, TouchPhase::Moved, 160.),
                (80, TouchPhase::Moved, 200.),
                (80, TouchPhase::Ended, 200.),
            ] {
                let event = TouchEvent {
                    time_stamp: Some(at(ms)),
                    ..touch_event(touch, phase, 0., y)
                };
                release_dispatched_at = at(ms + delay(ms));
                recognizer.handle_event_at(&event, release_dispatched_at, None);
            }
            assert!(recognizer.has_momentum(), "{name}");
            velocities.push((name, recognizer.fling_velocity(release_dispatched_at).y));
        }
        let (_, expected) = velocities[0];
        assert!((expected - 2500.).abs() < 1., "{expected} px/s");
        for (name, velocity) in velocities {
            assert!((velocity - expected).abs() < 1., "{name}: {velocity} px/s");
        }
    }

    /// A 20000 px/s flick flings at the tuning's `max_fling_velocity`:
    /// (row, max fling velocity).
    #[test]
    fn fling_is_clamped_to_the_tunings_max_velocity() {
        let rows = [
            ("kMaxFlingVelocity", 8000.),
            ("BouncingScrollPhysics(fast): kMaxFlingVelocity * 8", 64000.),
        ];
        for (name, max_fling_velocity) in rows {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning {
                max_fling_velocity,
                ..GestureTuning::default()
            });
            let now = Instant::now();
            let touch = TouchId(1);
            for (ms, y, phase) in [
                (0, 0., TouchPhase::Started),
                (10, 200., TouchPhase::Moved),
                (20, 400., TouchPhase::Moved),
                (30, 600., TouchPhase::Moved),
                (30, 600., TouchPhase::Ended),
            ] {
                let at = now + Duration::from_millis(ms);
                recognizer.handle_event_at(&touch_event(touch, phase, 0., y), at, None);
            }
            let speed = recognizer
                .momentum
                .as_ref()
                .map(|momentum| momentum.simulation.dx(0.) as f32);
            assert_eq!(speed, Some(max_fling_velocity.min(20000.)), "{name}");
        }
    }

    #[test]
    fn slow_release_does_not_start_momentum() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 300.),
            now,
            None,
        );
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 100., 280.),
            now + Duration::from_millis(16),
            None,
        );
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 100., 279.),
            now + Duration::from_millis(500),
            None,
        );
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 100., 279.),
            now + Duration::from_millis(600),
            None,
        );
        assert!(!recognizer.has_momentum());
    }

    #[test]
    fn new_touch_interrupts_momentum() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();

        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Started, 100., 300.),
            now,
            None,
        );
        for step in 1..=3 {
            recognizer.handle_event_at(
                &touch_event(
                    TouchId(1),
                    TouchPhase::Moved,
                    100.,
                    300. - step as f32 * 33.,
                ),
                now + Duration::from_millis(step * 16),
                None,
            );
        }
        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Ended, 100., 200.),
            now + Duration::from_millis(64),
            None,
        );
        assert!(recognizer.has_momentum());

        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Started, 100., 200.),
            now + Duration::from_millis(200),
            None,
        );
        assert!(!recognizer.has_momentum());
        let [
            RecognizedTouchGesture::Scroll(closing),
            RecognizedTouchGesture::Scroll(opening),
        ] = recognized.as_slice()
        else {
            panic!("expected closing and opening scrolls, got {recognized:?}");
        };
        assert_eq!(closing.touch_phase, TouchPhase::Ended);
        assert!(closing.delta.pixel_delta(px(16.)).is_zero());
        assert_eq!(opening.touch_phase, TouchPhase::Started);
        assert!(opening.delta.pixel_delta(px(16.)).is_zero());
    }

    #[test]
    fn catching_a_fling_pans_immediately_and_never_taps() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();

        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Started, 100., 300.),
            now,
            None,
        );
        for step in 1..=3 {
            recognizer.handle_event_at(
                &touch_event(
                    TouchId(1),
                    TouchPhase::Moved,
                    100.,
                    300. - step as f32 * 33.,
                ),
                now + Duration::from_millis(step * 16),
                None,
            );
        }
        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Ended, 100., 200.),
            now + Duration::from_millis(64),
            None,
        );
        assert!(recognizer.has_momentum());

        recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Started, 100., 200.),
            now + Duration::from_millis(200),
            None,
        );

        // A movement well within the slop scrolls immediately.
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Moved, 100., 197.),
            now + Duration::from_millis(216),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Moved);
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(-3.)));

        // Releasing the catch is not a tap.
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Ended, 100., 197.),
            now + Duration::from_millis(232),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Ended);
    }

    #[test]
    fn catching_a_fling_takes_the_axis_from_the_new_touch() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();

        // A fast horizontal pan flings sideways.
        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Started, 300., 300.),
            now,
            None,
        );
        for step in 1..=3 {
            recognizer.handle_event_at(
                &touch_event(
                    TouchId(1),
                    TouchPhase::Moved,
                    300. - step as f32 * 33.,
                    300.,
                ),
                now + Duration::from_millis(step * 16),
                None,
            );
        }
        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Ended, 200., 300.),
            now + Duration::from_millis(64),
            None,
        );
        assert!(recognizer.has_momentum());

        // A finger that catches it and moves up scrolls up: the fling's axis
        // is not this touch's axis.
        recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Started, 200., 300.),
            now + Duration::from_millis(200),
            None,
        );
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Moved, 201., 280.),
            now + Duration::from_millis(216),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Moved);
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(-20.)));

        // And stays on that axis for the rest of the pan.
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Moved, 205., 260.),
            now + Duration::from_millis(232),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.delta.pixel_delta(px(16.)), point(px(0.), px(-20.)));
    }

    #[test]
    fn cancelled_pan_emits_cancelled_scroll_and_no_tap() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();
        let touch = TouchId(1);

        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 100., 150.),
            now + Duration::from_millis(16),
            None,
        );
        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Cancelled, 100., 150.),
            now + Duration::from_millis(32),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected cancelled scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Cancelled);
        assert!(!recognizer.has_momentum());

        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 100., 100.),
            now,
            None,
        );
        let recognized = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Cancelled, 100., 102.),
            now + Duration::from_millis(16),
            None,
        );
        assert!(recognized.is_empty(), "cancelled tap must not click");
    }

    #[test]
    fn concurrent_touches_are_ignored_while_one_is_active() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let now = Instant::now();

        recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Started, 100., 100.),
            now,
            None,
        );
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Started, 200., 200.),
            now + Duration::from_millis(8),
            None,
        );
        assert!(recognized.is_empty());
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Moved, 200., 300.),
            now + Duration::from_millis(16),
            None,
        );
        assert!(recognized.is_empty());
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(2), TouchPhase::Ended, 200., 300.),
            now + Duration::from_millis(24),
            None,
        );
        assert!(recognized.is_empty());

        // The first touch still resolves normally.
        let recognized = recognizer.handle_event_at(
            &touch_event(TouchId(1), TouchPhase::Moved, 100., 150.),
            now + Duration::from_millis(32),
            None,
        );
        let [RecognizedTouchGesture::Scroll(scroll)] = recognized.as_slice() else {
            panic!("expected scroll, got {recognized:?}");
        };
        assert_eq!(scroll.touch_phase, TouchPhase::Started);
    }

    #[test]
    fn fling_curves_are_sane_for_both_physics() {
        for physics in [ScrollPhysics::ios(), ScrollPhysics::android()] {
            let (Some(slow), Some(fast)) = (
                fling_simulation(physics, 500.),
                fling_simulation(physics, 4000.),
            ) else {
                panic!("{physics:?}: both releases fling");
            };
            let stop_time = |simulation: &dyn Simulation| {
                (1..)
                    .map(|frame| frame as f64 / 60.)
                    .find(|&t| simulation.is_done(t))
            };
            let (slow_stop, fast_stop) = (stop_time(&*slow), stop_time(&*fast));
            assert!(slow_stop > Some(0.), "{physics:?}");
            assert!(
                fast_stop > slow_stop,
                "faster flings must coast longer: {physics:?}"
            );
            assert!(
                fast.x(f64::INFINITY) > slow.x(f64::INFINITY),
                "faster flings must travel further: {physics:?}"
            );
        }
    }

    #[test]
    fn momentum_is_frame_rate_independent() {
        // The same fling ticked at 60Hz and as one huge stalled frame must
        // cover identical ground.
        let total_distance_with_tick_length = |tick: Duration| -> f32 {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning {
                scroll_physics: ScrollPhysics::android(),
                ..GestureTuning::default()
            });
            let now = Instant::now();
            recognizer.handle_event_at(
                &touch_event(TouchId(1), TouchPhase::Started, 100., 500.),
                now,
                None,
            );
            for step in 1..=3 {
                recognizer.handle_event_at(
                    &touch_event(
                        TouchId(1),
                        TouchPhase::Moved,
                        100.,
                        500. - step as f32 * 40.,
                    ),
                    now + Duration::from_millis(step * 16),
                    None,
                );
            }
            recognizer.handle_event_at(
                &touch_event(TouchId(1), TouchPhase::Ended, 100., 380.),
                now + Duration::from_millis(64),
                None,
            );
            assert!(recognizer.has_momentum());

            let mut total = 0f32;
            let mut time = now + Duration::from_millis(64);
            let mut guard = 0;
            while recognizer.has_momentum() {
                time += tick;
                guard += 1;
                assert!(guard < 10_000, "momentum never stopped");
                if let Some(RecognizedTouchGesture::Scroll(scroll)) =
                    recognizer.tick_momentum_at(time)
                {
                    total += f32::from(scroll.delta.pixel_delta(px(16.)).y);
                }
            }
            total
        };

        let smooth = total_distance_with_tick_length(Duration::from_millis(16));
        let stalled = total_distance_with_tick_length(Duration::from_secs(10));
        assert!(
            (smooth - stalled).abs() < 0.01,
            "expected identical fling distance, got {smooth} vs {stalled}"
        );
    }

    #[test]
    fn flick_velocity_reflects_release_speed_not_window_average() {
        // A uniformly accelerating flick: position grows quadratically, so
        // the speed at the newest sample (2·k·t) is twice the window
        // average (k·t). The estimator must report the former.
        let mut velocity_tracker = VelocityTracker::with_kind(VelocityTrackerKind::LeastSquares);
        let start = Instant::now();
        for step in 0..=6 {
            let t = step as f32 * 0.016;
            velocity_tracker.add_position(
                start + Duration::from_millis(step * 16),
                point(px(0.), px(1000. * t * t)),
            );
        }
        let velocity = velocity_tracker.get_velocity(start + Duration::from_millis(96));
        let release_speed = 2. * 1000. * 0.096;
        assert!(
            (velocity.y - release_speed).abs() < 1.,
            "expected ≈{release_speed} px/s at release, got {} px/s",
            velocity.y
        );
        assert_eq!(velocity.x, 0.);
    }

    #[test]
    fn samples_before_a_pause_do_not_contribute_velocity() {
        // Fast motion, then a hold longer than the stopped-finger gap, then
        // a slow nudge: only the motion after the pause describes the
        // release.
        let mut velocity_tracker = VelocityTracker::with_kind(VelocityTrackerKind::LeastSquares);
        let start = Instant::now();
        velocity_tracker.add_position(start, point(px(0.), px(0.)));
        velocity_tracker.add_position(start + Duration::from_millis(16), point(px(0.), px(50.)));
        velocity_tracker.add_position(start + Duration::from_millis(80), point(px(0.), px(52.)));
        velocity_tracker.add_position(start + Duration::from_millis(96), point(px(0.), px(54.)));
        let velocity = velocity_tracker.get_velocity(start + Duration::from_millis(96));
        assert!(
            velocity.y < 200.,
            "pre-pause motion leaked into the estimate: {} px/s",
            velocity.y
        );
    }

    #[test]
    fn claimed_touch_drag_emits_phased_stream_without_pan_or_tap() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        let now = Instant::now();
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 10., 20.),
            now,
            None,
        );
        let Some(RecognizedTouchGesture::TouchDrag(started)) = recognizer.offer_touch_drag(touch)
        else {
            panic!("expected touch drag");
        };
        assert_eq!(started.phase, TouchPhase::Started);
        assert_eq!(started.start_position, point(px(10.), px(20.)));
        recognizer.resolve_touch_drag(true);

        let moved = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 40., 50.),
            now + Duration::from_millis(10),
            None,
        );
        let [RecognizedTouchGesture::TouchDrag(moved)] = moved.as_slice() else {
            panic!("expected moved touch drag, got {moved:?}");
        };
        assert_eq!(moved.phase, TouchPhase::Moved);
        assert_eq!(moved.position, point(px(40.), px(50.)));

        let ended = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 45., 55.),
            now + Duration::from_millis(20),
            None,
        );
        let [RecognizedTouchGesture::TouchDrag(ended)] = ended.as_slice() else {
            panic!("expected ended touch drag, got {ended:?}");
        };
        assert_eq!(ended.phase, TouchPhase::Ended);
        assert_eq!(ended.position, point(px(45.), px(55.)));
    }

    #[test]
    fn unclaimed_touch_drag_remains_a_pan_candidate() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        let now = Instant::now();
        recognizer.handle_event_at(&touch_event(touch, TouchPhase::Started, 0., 0.), now, None);
        assert!(recognizer.offer_touch_drag(touch).is_some());
        recognizer.resolve_touch_drag(false);

        let moved = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 20., 0.),
            now + Duration::from_millis(10),
            None,
        );
        assert!(matches!(
            moved.as_slice(),
            [RecognizedTouchGesture::Scroll(ScrollEvent {
                touch_phase: TouchPhase::Started,
                ..
            })]
        ));
    }

    #[test]
    fn claimed_long_press_emits_phased_stream_without_tap() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        let now = Instant::now();
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 10., 20.),
            now,
            None,
        );
        let Some(RecognizedTouchGesture::LongPress(started)) = recognizer.offer_long_press(touch)
        else {
            panic!("expected long press");
        };
        assert_eq!(started.phase, TouchPhase::Started);
        assert_eq!(started.start_position, point(px(10.), px(20.)));
        recognizer.resolve_long_press(true);

        let moved = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 12., 21.),
            now + Duration::from_millis(510),
            None,
        );
        let [RecognizedTouchGesture::LongPress(moved)] = moved.as_slice() else {
            panic!("expected moved long press, got {moved:?}");
        };
        assert_eq!(moved.phase, TouchPhase::Moved);

        let ended = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 12., 21.),
            now + Duration::from_millis(520),
            None,
        );
        let [RecognizedTouchGesture::LongPress(ended)] = ended.as_slice() else {
            panic!("expected ended long press, got {ended:?}");
        };
        assert_eq!(ended.phase, TouchPhase::Ended);
    }

    #[test]
    fn unclaimed_long_press_remains_a_tap_candidate() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        let now = Instant::now();
        recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Started, 10., 20.),
            now,
            None,
        );
        assert!(recognizer.offer_long_press(touch).is_some());
        recognizer.resolve_long_press(false);

        let ended = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Ended, 10., 20.),
            now + Duration::from_millis(510),
            None,
        );
        assert!(matches!(
            ended.as_slice(),
            [RecognizedTouchGesture::Tap { .. }]
        ));
    }

    #[test]
    fn unclaimed_long_press_can_still_become_a_pan() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        let now = Instant::now();
        recognizer.handle_event_at(&touch_event(touch, TouchPhase::Started, 0., 0.), now, None);
        assert!(recognizer.offer_long_press(touch).is_some());
        recognizer.resolve_long_press(false);

        let moved = recognizer.handle_event_at(
            &touch_event(touch, TouchPhase::Moved, 20., 0.),
            now + Duration::from_millis(510),
            None,
        );
        assert!(matches!(
            moved.as_slice(),
            [RecognizedTouchGesture::Scroll(ScrollEvent {
                touch_phase: TouchPhase::Started,
                ..
            })]
        ));
    }

    #[test]
    fn long_press_offer_is_one_shot_and_specific_to_pending_touch() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        recognizer.handle_event(
            &touch_event(touch, TouchPhase::Started, 0., 0.),
            Instant::now(),
            None,
        );

        assert!(
            recognizer
                .handle_event(
                    &touch_event(TouchId(2), TouchPhase::Moved, 20., 0.),
                    Instant::now(),
                    None
                )
                .is_empty()
        );
        assert!(recognizer.offer_long_press(TouchId(2)).is_none());
        assert!(recognizer.offer_long_press(touch).is_some());
        assert!(recognizer.offer_long_press(touch).is_none());
    }

    #[test]
    fn long_press_cannot_be_offered_after_pending_touch_resolves() {
        for phase in [TouchPhase::Ended, TouchPhase::Cancelled, TouchPhase::Moved] {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
            let touch = TouchId(1);
            let now = Instant::now();
            recognizer.handle_event_at(&touch_event(touch, TouchPhase::Started, 0., 0.), now, None);
            let position = if phase == TouchPhase::Moved { 20. } else { 0. };
            recognizer.handle_event_at(
                &touch_event(touch, phase, position, 0.),
                now + Duration::from_millis(10),
                None,
            );
            assert!(recognizer.offer_long_press(touch).is_none());
        }
    }

    #[test]
    fn claimed_long_press_emits_cancelled_for_its_touch_only() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let touch = TouchId(1);
        recognizer.handle_event(
            &touch_event(touch, TouchPhase::Started, 4., 5.),
            Instant::now(),
            None,
        );
        assert!(recognizer.offer_long_press(touch).is_some());
        recognizer.resolve_long_press(true);

        assert!(
            recognizer
                .handle_event(
                    &touch_event(TouchId(2), TouchPhase::Cancelled, 9., 9.),
                    Instant::now(),
                    None
                )
                .is_empty()
        );
        let cancelled = recognizer.handle_event(
            &touch_event(touch, TouchPhase::Cancelled, 6., 7.),
            Instant::now(),
            None,
        );
        let [(_, RecognizedTouchGesture::LongPress(cancelled))] = cancelled.as_slice() else {
            panic!("expected cancelled long press, got {cancelled:?}");
        };
        assert_eq!(cancelled.phase, TouchPhase::Cancelled);
        assert_eq!(cancelled.start_position, point(px(4.), px(5.)));
        assert_eq!(cancelled.position, point(px(6.), px(7.)));
    }

    #[test]
    fn unrelated_touch_cannot_end_or_cancel_pending_touch() {
        for phase in [TouchPhase::Ended, TouchPhase::Cancelled] {
            let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
            let touch = TouchId(1);
            recognizer.handle_event(
                &touch_event(touch, TouchPhase::Started, 4., 5.),
                Instant::now(),
                None,
            );

            assert!(
                recognizer
                    .handle_event(
                        &touch_event(TouchId(2), phase, 9., 9.),
                        Instant::now(),
                        None
                    )
                    .is_empty()
            );
            assert!(recognizer.offer_long_press(touch).is_some());
        }
    }

    #[test]
    fn completed_touch_id_cannot_claim_replacement_touch() {
        let mut recognizer = TouchGestureRecognizer::new(GestureTuning::default());
        let completed_touch = TouchId(1);
        let replacement_touch = TouchId(2);
        recognizer.handle_event(
            &touch_event(completed_touch, TouchPhase::Started, 0., 0.),
            Instant::now(),
            None,
        );
        recognizer.handle_event(
            &touch_event(completed_touch, TouchPhase::Cancelled, 0., 0.),
            Instant::now(),
            None,
        );
        recognizer.handle_event(
            &touch_event(replacement_touch, TouchPhase::Started, 5., 5.),
            Instant::now(),
            None,
        );

        assert!(recognizer.offer_long_press(completed_touch).is_none());
        assert!(recognizer.offer_long_press(replacement_touch).is_some());
    }

    fn touch_event(id: TouchId, phase: TouchPhase, x: f32, y: f32) -> TouchEvent {
        TouchEvent {
            id,
            phase,
            position: point(px(x), px(y)),
            predicted_position: None,
            force: None,
            device: PointerDevice::Touch,
            time_stamp: None,
        }
    }
}
