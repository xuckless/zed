// Ported from flutter/flutter packages/flutter/lib/src/gestures/scale.dart @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7): `ScaleGestureRecognizer`, with the parts of
// `OneSequenceGestureRecognizer` (recognizer.dart) it uses. Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.

//! Pinch, rotate and pan recognition from touches: Flutter's `ScaleGestureRecognizer`.
//!
//! A pure state machine: [`TouchEvent`]s and arena decisions in, [`ScaleOutput`]s out. Flutter's gesture arena is
//! the caller's: [`ScaleGestureRecognizer::add_pointer`] enters a touch, [`ScaleOutput::Resolve`] asks to win or
//! leave it, [`ScaleGestureRecognizer::accept_gesture`] / [`ScaleGestureRecognizer::reject_gesture`] report its
//! outcome. Flutter's callbacks (`onStart`, `onUpdate`, `onEnd`) become the outputs, in the order Flutter calls them.
//!
//! Dropped from Flutter: pointer pan/zoom events (trackpad gestures; GPUI reports them as `PinchEvent` and
//! `ScrollEvent`) and with them `trackpadScrollCausesScale`, `trackpadScrollToScaleFactor` and the pan/zoom
//! factors; event transforms (GPUI positions are window coordinates, so `local_focal_point` starts as the focal
//! point and the element that receives the pinch makes it local); `allowedButtonsFilter`
//! (touches carry no buttons); synthesized moves (GPUI never synthesizes touches).

use std::mem;

use collections::{HashMap, HashSet, IndexMap};
use scheduler::Instant;

use super::constants::{kMaxFlingVelocity, kMinFlingVelocity};
use super::{VelocityTracker, VelocityTrackerKind, slops};
use crate::{Pixels, Point, PointerDevice, TouchEvent, TouchId, TouchPhase, point, px};

/// Flutter's `GestureDisposition` (arena.dart): whether a gesture was accepted or rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureDisposition {
    /// This gesture was accepted as the interpretation of the user's input.
    Accepted,
    /// This gesture was rejected as the interpretation of the user's input.
    Rejected,
}

/// Flutter's `DragStartBehavior` (recognizer.dart): how the start of a gesture is reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DragStartBehavior {
    /// Set the initial offset at the position where the first down event was detected.
    #[default]
    Down,
    /// Set the initial position at the position where this gesture recognizer won the arena.
    Start,
}

/// The possible states of a [`ScaleGestureRecognizer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScaleState {
    /// The recognizer is ready to start recognizing a gesture.
    Ready,
    /// The sequence of pointer events seen thus far is consistent with a scale
    /// gesture but the gesture has not been accepted definitively.
    Possible,
    /// The sequence of pointer events seen thus far has been accepted
    /// definitively as a scale gesture.
    Accepted,
    /// The sequence of pointer events seen thus far has been accepted
    /// definitively as a scale gesture and the pointers established a focal point
    /// and initial scale.
    Started,
}

/// Details for `GestureScaleStartCallback`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleStartDetails {
    /// The initial focal point of the pointers in contact with the screen.
    pub focal_point: Point<Pixels>,
    /// The initial focal point of the pointers in contact with the screen, in the receiving element's coordinates.
    pub local_focal_point: Point<Pixels>,
    /// The number of pointers being tracked by the gesture recognizer.
    pub pointer_count: usize,
    /// Recorded timestamp of the source pointer event that triggered the scale event.
    pub source_time_stamp: Option<Instant>,
    /// The kind of the device that initiated the event. If multiple pointers are touching the screen, the kind of
    /// the pointer device that first initiated the event is used.
    pub kind: Option<PointerDevice>,
}

/// Details for `GestureScaleUpdateCallback`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleUpdateDetails {
    /// The amount the gesture's focal point has moved since the previous update.
    pub focal_point_delta: Point<Pixels>,
    /// The focal point of the pointers in contact with the screen.
    pub focal_point: Point<Pixels>,
    /// The focal point of the pointers in contact with the screen, in the receiving element's coordinates.
    pub local_focal_point: Point<Pixels>,
    /// The scale implied by the average distance between the pointers in contact with the screen.
    pub scale: f32,
    /// The scale implied by the average distance along the horizontal axis between the pointers.
    pub horizontal_scale: f32,
    /// The scale implied by the average distance along the vertical axis between the pointers.
    pub vertical_scale: f32,
    /// The angle implied by the first two pointers to enter in contact with the screen. Expressed in radians.
    pub rotation: f32,
    /// The number of pointers being tracked by the gesture recognizer.
    pub pointer_count: usize,
    /// Recorded timestamp of the source pointer event that triggered the scale event.
    pub source_time_stamp: Instant,
}

/// Details for `GestureScaleEndCallback`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleEndDetails {
    /// The velocity of the last pointer to be lifted off of the screen, in logical pixels per second.
    pub velocity: Point<f32>,
    /// The final velocity of the scale factor reported by the gesture.
    pub scale_velocity: f32,
    /// The number of pointers being tracked by the gesture recognizer.
    pub pointer_count: usize,
}

/// What the recognizer asks of its caller, in the order Flutter does it.
#[derive(Clone, Debug, PartialEq)]
pub enum ScaleOutput {
    /// Flutter's `resolve`: win (or leave) the arenas of these pointers. On `Accepted` the recognizer has already
    /// run `acceptGesture` for them, as Flutter's closed arena does synchronously; the caller claims them.
    Resolve {
        /// Accepted or rejected.
        disposition: GestureDisposition,
        /// The pointers whose arena entries this resolves.
        pointers: Vec<TouchId>,
    },
    /// `onStart`: the pointers in contact with the screen have established a focal point and initial scale of 1.0.
    Start(ScaleStartDetails),
    /// `onUpdate`: the pointers in contact with the screen have indicated a new focal point and/or scale.
    Update(ScaleUpdateDetails),
    /// `onEnd`: the pointers are no longer in contact with the screen.
    End(ScaleEndDetails),
}

/// Defines a line between two pointers on screen.
///
/// [`LineBetweenPointers`] is an abstraction of a line between two pointers in
/// contact with the screen. Used to track the rotation of a scale gesture.
#[derive(Clone, Copy, Debug)]
struct LineBetweenPointers {
    // The location and the id of the pointer that marks the start of the line.
    pointer_start_location: Point<f32>,
    pointer_start_id: TouchId,
    // The location and the id of the pointer that marks the end of the line.
    pointer_end_location: Point<f32>,
    pointer_end_id: TouchId,
}

/// Recognizes a scale gesture.
///
/// The recognizer tracks the pointers in contact with the screen and
/// calculates their focal point, indicated scale, and rotation. When a focal
/// point is established, the recognizer outputs [`ScaleOutput::Start`]. As the
/// focal point, scale, and rotation change, it outputs [`ScaleOutput::Update`].
/// When the pointers are no longer in contact with the screen, it outputs
/// [`ScaleOutput::End`].
pub struct ScaleGestureRecognizer {
    /// Determines what point is used as the starting point in all calculations
    /// involving this gesture.
    pub drag_start_behavior: DragStartBehavior,
    /// The device kinds this recognizer accepts pointers from; `None` accepts every kind.
    pub supported_devices: Option<Vec<PointerDevice>>,
    /// Flutter's `gestureSettings.touchSlop`: the platform's touch slop, or `None` for
    /// [`kTouchSlop`](super::constants::kTouchSlop).
    pub touch_slop: Option<Pixels>,

    state: ScaleState,
    initial_focal_point: Point<f32>,
    current_focal_point: Option<Point<f32>>,
    initial_span: f32,
    current_span: f32,
    initial_horizontal_span: f32,
    current_horizontal_span: f32,
    initial_vertical_span: f32,
    current_vertical_span: f32,
    initial_line: Option<LineBetweenPointers>,
    current_line: Option<LineBetweenPointers>,
    pointer_locations: IndexMap<TouchId, Point<f32>>,
    // A queue to sort pointers in order of entrance
    pointer_queue: Vec<TouchId>,
    velocity_trackers: HashMap<TouchId, VelocityTracker>,
    scale_velocity_tracker: Option<VelocityTracker>,
    delta: Point<f32>,
    initial_event_timestamp: Option<Instant>,
    // GestureRecognizer's `_pointerToKind`.
    pointer_to_kind: HashMap<TouchId, PointerDevice>,
    // OneSequenceGestureRecognizer's `_trackedPointers` and `_entries` (the pointers whose arena is unresolved).
    tracked_pointers: HashSet<TouchId>,
    entries: Vec<TouchId>,
}

impl Default for ScaleGestureRecognizer {
    fn default() -> Self {
        Self {
            drag_start_behavior: DragStartBehavior::Down,
            supported_devices: None,
            touch_slop: None,
            state: ScaleState::Ready,
            initial_focal_point: Point::default(),
            current_focal_point: None,
            initial_span: 0.0,
            current_span: 0.0,
            initial_horizontal_span: 0.0,
            current_horizontal_span: 0.0,
            initial_vertical_span: 0.0,
            current_vertical_span: 0.0,
            initial_line: None,
            current_line: None,
            pointer_locations: IndexMap::default(),
            pointer_queue: Vec::new(),
            velocity_trackers: HashMap::default(),
            scale_velocity_tracker: None,
            delta: Point::default(),
            initial_event_timestamp: None,
            pointer_to_kind: HashMap::default(),
            tracked_pointers: HashSet::default(),
            entries: Vec::new(),
        }
    }
}

impl ScaleGestureRecognizer {
    /// The number of pointers being tracked by the gesture recognizer.
    pub fn pointer_count(&self) -> usize {
        self.pointer_queue.len()
    }

    fn pointer_scale_factor(&self) -> f32 {
        if self.initial_span > 0.0 {
            self.current_span / self.initial_span
        } else {
            1.0
        }
    }

    fn pointer_horizontal_scale_factor(&self) -> f32 {
        if self.initial_horizontal_span > 0.0 {
            self.current_horizontal_span / self.initial_horizontal_span
        } else {
            1.0
        }
    }

    fn pointer_vertical_scale_factor(&self) -> f32 {
        if self.initial_vertical_span > 0.0 {
            self.current_vertical_span / self.initial_vertical_span
        } else {
            1.0
        }
    }

    fn compute_rotation_factor(&self) -> f32 {
        let mut factor = 0.0;
        if let (Some(initial), Some(current)) = (self.initial_line, self.current_line) {
            let fx = initial.pointer_start_location.x;
            let fy = initial.pointer_start_location.y;
            let sx = initial.pointer_end_location.x;
            let sy = initial.pointer_end_location.y;

            let nfx = current.pointer_start_location.x;
            let nfy = current.pointer_start_location.y;
            let nsx = current.pointer_end_location.x;
            let nsy = current.pointer_end_location.y;

            let angle1 = (fy - sy).atan2(fx - sx);
            let angle2 = (nfy - nsy).atan2(nfx - nsx);

            factor = angle2 - angle1;
        }
        factor
    }

    /// Flutter's `addPointer`: registers a new touch ([`TouchPhase::Started`]) that might be relevant to this
    /// recognizer. An allowed touch is tracked and enters the arena; route the same event to
    /// [`Self::handle_event`] next.
    pub fn add_pointer(&mut self, event: &TouchEvent, time_stamp: Instant) {
        self.pointer_to_kind.insert(event.id, event.device);
        let allowed = self
            .supported_devices
            .as_ref()
            .is_none_or(|devices| devices.contains(&event.device));
        if !allowed {
            return;
        }
        // OneSequenceGestureRecognizer.addAllowedPointer → startTrackingPointer.
        self.tracked_pointers.insert(event.id);
        self.entries.push(event.id);
        self.velocity_trackers.insert(
            event.id,
            VelocityTracker::with_kind(VelocityTrackerKind::LeastSquares),
        );
        self.initial_event_timestamp = Some(time_stamp);
        if self.state == ScaleState::Ready {
            self.state = ScaleState::Possible;
            self.initial_span = 0.0;
            self.current_span = 0.0;
            self.initial_horizontal_span = 0.0;
            self.current_horizontal_span = 0.0;
            self.initial_vertical_span = 0.0;
            self.current_vertical_span = 0.0;
        }
    }

    /// Flutter's `handleEvent`, for an event of a tracked touch (the pointer router routes no other).
    pub fn handle_event(&mut self, event: &TouchEvent, time_stamp: Instant) -> Vec<ScaleOutput> {
        let mut out = Vec::new();
        if !self.tracked_pointers.contains(&event.id) {
            return out;
        }
        let position = event.position.map(f32::from);
        let mut did_change_configuration = false;
        let mut should_start_if_accepted = false;
        match event.phase {
            TouchPhase::Moved => {
                self.velocity_trackers
                    .get_mut(&event.id)
                    .expect("add_pointer gives every tracked pointer a velocity tracker")
                    .add_position(time_stamp, event.position);
                self.pointer_locations.insert(event.id, position);
                should_start_if_accepted = true;
            }
            TouchPhase::Started => {
                self.pointer_locations.insert(event.id, position);
                self.pointer_queue.push(event.id);
                did_change_configuration = true;
                should_start_if_accepted = true;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.pointer_locations.shift_remove(&event.id);
                self.pointer_queue.retain(|pointer| *pointer != event.id);
                did_change_configuration = true;
            }
        }

        self.update_lines();
        self.update();

        if !did_change_configuration || self.reconfigure(event.id, time_stamp, &mut out) {
            self.advance_state_machine(
                should_start_if_accepted,
                event.device,
                time_stamp,
                &mut out,
            );
        }
        // stopTrackingIfPointerNoLongerDown.
        if matches!(event.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.stop_tracking_pointer(event.id, &mut out);
        }
        out
    }

    fn update(&mut self) {
        let previous_focal_point = self.current_focal_point;

        // Compute the focal point
        let count = self.pointer_locations.len();
        let focal_point = self
            .pointer_locations
            .values()
            .fold(Point::default(), |sum, location| sum + *location)
            / count.max(1) as f32;
        self.current_focal_point = Some(focal_point);

        self.delta = match previous_focal_point {
            None => Point::default(),
            Some(previous) => focal_point - previous,
        };

        // Span is the average deviation from focal point. Horizontal and vertical
        // spans are the average deviations from the focal point's horizontal and
        // vertical coordinates, respectively.
        let mut total_deviation = 0.0;
        let mut total_horizontal_deviation = 0.0;
        let mut total_vertical_deviation = 0.0;
        for location in self.pointer_locations.values() {
            let deviation = focal_point - *location;
            total_deviation += (deviation.x * deviation.x + deviation.y * deviation.y).sqrt();
            total_horizontal_deviation += deviation.x.abs();
            total_vertical_deviation += deviation.y.abs();
        }
        let (span, horizontal_span, vertical_span) = if count > 0 {
            (
                total_deviation / count as f32,
                total_horizontal_deviation / count as f32,
                total_vertical_deviation / count as f32,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        self.current_span = span;
        self.current_horizontal_span = horizontal_span;
        self.current_vertical_span = vertical_span;
    }

    /// Updates [`Self::initial_line`] and [`Self::current_line`] accordingly to the situation of the registered
    /// pointers.
    fn update_lines(&mut self) {
        let count = self.pointer_locations.len();

        // In case of just one pointer registered, reconfigure [initial_line]
        if count < 2 {
            self.initial_line = self.current_line;
            return;
        }
        // Flutter asserts the queue holds every located pointer (a touch's `Started` comes first).
        let (Some(&start_id), Some(&end_id)) =
            (self.pointer_queue.first(), self.pointer_queue.get(1))
        else {
            return;
        };
        let (Some(&start_location), Some(&end_location)) = (
            self.pointer_locations.get(&start_id),
            self.pointer_locations.get(&end_id),
        ) else {
            return;
        };
        let line = LineBetweenPointers {
            pointer_start_id: start_id,
            pointer_start_location: start_location,
            pointer_end_id: end_id,
            pointer_end_location: end_location,
        };
        match self.initial_line {
            Some(initial)
                if initial.pointer_start_id == start_id && initial.pointer_end_id == end_id =>
            {
                // Rotation updated, set the [current_line]
                self.current_line = Some(line);
            }
            _ => {
                // A new rotation process is on the way, set the [initial_line]
                self.initial_line = Some(line);
                self.current_line = self.initial_line;
            }
        }
    }

    fn reconfigure(
        &mut self,
        pointer: TouchId,
        time_stamp: Instant,
        out: &mut Vec<ScaleOutput>,
    ) -> bool {
        self.initial_focal_point = self
            .current_focal_point
            .expect("update sets the focal point before reconfigure");
        self.initial_span = self.current_span;
        self.initial_line = self.current_line;
        self.initial_horizontal_span = self.current_horizontal_span;
        self.initial_vertical_span = self.current_vertical_span;
        if self.state == ScaleState::Started {
            let tracker = self
                .velocity_trackers
                .get(&pointer)
                .expect("add_pointer gives every tracked pointer a velocity tracker");
            let pixels_per_second = tracker.get_velocity(time_stamp);
            let speed_squared = pixels_per_second.x * pixels_per_second.x
                + pixels_per_second.y * pixels_per_second.y;
            // _isFlingGesture
            let velocity = if speed_squared > kMinFlingVelocity * kMinFlingVelocity {
                if speed_squared > kMaxFlingVelocity * kMaxFlingVelocity {
                    pixels_per_second / speed_squared.sqrt() * kMaxFlingVelocity
                } else {
                    pixels_per_second
                }
            } else {
                Point::default()
            };
            out.push(ScaleOutput::End(ScaleEndDetails {
                velocity,
                scale_velocity: self
                    .scale_velocity_tracker
                    .as_ref()
                    .map_or(-1.0, |tracker| tracker.get_velocity(time_stamp).x),
                pointer_count: self.pointer_count(),
            }));
            self.state = ScaleState::Accepted;
            // arbitrary PointerDeviceKind
            self.scale_velocity_tracker = Some(VelocityTracker::with_kind(
                VelocityTrackerKind::LeastSquares,
            ));
            return false;
        }
        // arbitrary PointerDeviceKind
        self.scale_velocity_tracker = Some(VelocityTracker::with_kind(
            VelocityTrackerKind::LeastSquares,
        ));
        true
    }

    fn advance_state_machine(
        &mut self,
        should_start_if_accepted: bool,
        kind: PointerDevice,
        time_stamp: Instant,
        out: &mut Vec<ScaleOutput>,
    ) {
        if self.state == ScaleState::Ready {
            self.state = ScaleState::Possible;
        }

        match self.state {
            ScaleState::Possible => {
                let current_focal_point = self
                    .current_focal_point
                    .expect("update sets the focal point before the state machine runs");
                let span_delta = (self.current_span - self.initial_span).abs();
                let focal_point_delta = current_focal_point - self.initial_focal_point;
                let focal_point_delta = (focal_point_delta.x * focal_point_delta.x
                    + focal_point_delta.y * focal_point_delta.y)
                    .sqrt();
                let slops = slops(kind, self.touch_slop);
                // Flutter's third test, the pan/zoom scale ratio above 1.05, is always 1 without pan/zoom pointers.
                if span_delta > f32::from(slops.scale) || focal_point_delta > f32::from(slops.pan) {
                    self.resolve(GestureDisposition::Accepted, out);
                }
            }
            ScaleState::Accepted | ScaleState::Started => {
                self.resolve(GestureDisposition::Accepted, out);
            }
            ScaleState::Ready => {}
        }

        if self.state == ScaleState::Accepted && should_start_if_accepted {
            self.initial_event_timestamp = Some(time_stamp);
            self.state = ScaleState::Started;
            self.dispatch_on_start_callback_if_needed(out);
        }

        if self.state == ScaleState::Started {
            let scale_factor = self.pointer_scale_factor();
            if let Some(tracker) = self.scale_velocity_tracker.as_mut() {
                tracker.add_position(time_stamp, point(px(scale_factor), px(0.0)));
            }
            let focal_point = self
                .current_focal_point
                .expect("update sets the focal point before the state machine runs")
                .map(px);
            out.push(ScaleOutput::Update(ScaleUpdateDetails {
                scale: scale_factor,
                horizontal_scale: self.pointer_horizontal_scale_factor(),
                vertical_scale: self.pointer_vertical_scale_factor(),
                focal_point,
                local_focal_point: focal_point,
                rotation: self.compute_rotation_factor(),
                pointer_count: self.pointer_count(),
                focal_point_delta: self.delta.map(px),
                source_time_stamp: time_stamp,
            }));
        }
    }

    fn dispatch_on_start_callback_if_needed(&mut self, out: &mut Vec<ScaleOutput>) {
        let focal_point = self
            .current_focal_point
            .expect("a pointer event set the focal point before the gesture starts")
            .map(px);
        out.push(ScaleOutput::Start(ScaleStartDetails {
            focal_point,
            local_focal_point: focal_point,
            pointer_count: self.pointer_count(),
            source_time_stamp: self.initial_event_timestamp,
            kind: self
                .pointer_queue
                .first()
                .and_then(|pointer| self.pointer_to_kind.get(pointer).copied()),
        }));
        self.initial_event_timestamp = None;
    }

    /// Flutter's `resolve`: resolves every unresolved arena entry. Accepting runs `acceptGesture` for each at once,
    /// as Flutter's arena does once closed (GPUI hit-tests a touch before dispatching it); rejecting runs
    /// `rejectGesture`, as the arena does for a member that leaves.
    fn resolve(&mut self, disposition: GestureDisposition, out: &mut Vec<ScaleOutput>) {
        let pointers = mem::take(&mut self.entries);
        if pointers.is_empty() {
            return;
        }
        out.push(ScaleOutput::Resolve {
            disposition,
            pointers: pointers.clone(),
        });
        for pointer in pointers {
            match disposition {
                GestureDisposition::Accepted => self.accept(out),
                GestureDisposition::Rejected => self.reject(pointer, out),
            }
        }
    }

    /// Flutter's `acceptGesture`: the arena gave this pointer to the recognizer (it was the only member, or it
    /// won). Ignored for a pointer whose arena already resolved.
    pub fn accept_gesture(&mut self, pointer: TouchId) -> Vec<ScaleOutput> {
        let mut out = Vec::new();
        if self.take_entry(pointer) {
            self.accept(&mut out);
        }
        out
    }

    fn accept(&mut self, out: &mut Vec<ScaleOutput>) {
        if self.state != ScaleState::Possible {
            return;
        }
        self.state = ScaleState::Started;
        self.dispatch_on_start_callback_if_needed(out);
        if self.drag_start_behavior == DragStartBehavior::Start {
            self.initial_focal_point = self
                .current_focal_point
                .expect("a pointer event set the focal point before the gesture starts");
            self.initial_span = self.current_span;
            self.initial_line = self.current_line;
            self.initial_horizontal_span = self.current_horizontal_span;
            self.initial_vertical_span = self.current_vertical_span;
        }
    }

    /// Flutter's `rejectGesture`: another recognizer won this pointer. Ignored for a pointer whose arena already
    /// resolved.
    pub fn reject_gesture(&mut self, pointer: TouchId) -> Vec<ScaleOutput> {
        let mut out = Vec::new();
        if self.take_entry(pointer) {
            self.reject(pointer, &mut out);
        }
        out
    }

    /// Removes the pointer's arena entry; false when it has none (its arena already resolved, or it was never
    /// allowed), where Flutter's arena would not call the recognizer.
    fn take_entry(&mut self, pointer: TouchId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| *entry != pointer);
        self.entries.len() != before
    }

    fn reject(&mut self, pointer: TouchId, out: &mut Vec<ScaleOutput>) {
        self.pointer_locations.shift_remove(&pointer);
        self.pointer_queue.retain(|queued| *queued != pointer);
        self.stop_tracking_pointer(pointer, out);
    }

    /// OneSequenceGestureRecognizer's `stopTrackingPointer`.
    fn stop_tracking_pointer(&mut self, pointer: TouchId, out: &mut Vec<ScaleOutput>) {
        if !self.tracked_pointers.remove(&pointer) || !self.tracked_pointers.is_empty() {
            return;
        }
        // didStopTrackingLastPointer. Flutter asserts it is never reached in `ready` (no pointer was seen) or
        // `started` (the last pointer's up moved the gesture to `accepted` first).
        match self.state {
            ScaleState::Possible => self.resolve(GestureDisposition::Rejected, out),
            ScaleState::Ready | ScaleState::Accepted | ScaleState::Started => {}
        }
        self.state = ScaleState::Ready;
    }
}

#[cfg(test)]
#[path = "scale_tests.rs"]
mod tests;
