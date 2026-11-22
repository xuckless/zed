//! Touch pinches: which touches the scale recognizer ([`super::scale`], Flutter's `ScaleGestureRecognizer`) gets,
//! and the [`PinchEvent`]s it reports. This is Flutter's gesture arena between an element's scale recognizer and
//! the window's touch recognizer (taps, pans, long presses), as claims:
//!
//! - a touch joins the pinch of every element with `on_pinch` under it ([`Window::join_pinch`]), unless a deeper
//!   `on_touch` claimed it; the deepest pinches, or the one an earlier touch is already pinching (a pinch element
//!   inside another: the outer one pinches a finger on each). One element pinches at a time per window;
//! - one touch on that element stays the touch recognizer's: its pan (hit slop) always beats the scale's (pan slop),
//!   and once it pans, a touch that joins is the pan's (Flutter's drag accepts a joining pointer): a running pan
//!   never becomes a pinch;
//! - a second touch on it, before that, hands both to the scale recognizer, the first where it is now (Flutter's
//!   recognizer restarts its focal point and span at every new pointer anyway). Both still reach the touch
//!   recognizer, so a two-finger tap taps, until one side wins. The scale wins past its slops (span or focal point
//!   travel): the touch recognizer then cancels them and they reach only the pinch until they lift. The drag wins
//!   when the touches' focal point travels past the hit slop first (Flutter's drag recognizer adds up every
//!   tracked pointer's movement, so fingers dragged together pan, fingers spread apart do not), and a long press
//!   or touch drag an element claims wins too (`LongPressGestureRecognizer` accepts in every pointer's arena): the
//!   scale loses the touches, which never pinch until they lift;
//! - a touch that lands while the touch recognizer is busy with a touch the scale does not have (a claimed long
//!   press, a touch drag, a tap to be elsewhere) is the scale's alone: Flutter's one-pointer scale, which wins past
//!   the pan slop;
//! - back to one touch before the scale wins, it gives up and waits for a second again, if the touch recognizer has
//!   that touch; after it wins, the last finger keeps the gesture (`pointer_count` 1, scale 1).
//!
//! [`Window::join_pinch`]: crate::Window::join_pinch

use std::mem;

use scheduler::Instant;
use smallvec::{SmallVec, smallvec};

use super::scale::{
    GestureDisposition, ScaleEndDetails, ScaleGestureRecognizer, ScaleOutput, ScaleStartDetails,
    ScaleUpdateDetails,
};
use super::{TouchClaim, slops};
use crate::{GlobalElementId, Pixels, Point, TouchEvent, TouchId, TouchPhase};

/// What a touch pinch reports in a [`PinchEvent`](crate::PinchEvent): Flutter's `onStart`, `onUpdate` and `onEnd`
/// details. A finger joining or leaving ends the gesture and the next move starts it again, as in Flutter.
#[derive(Clone, Debug, PartialEq)]
pub enum TouchPinch {
    /// The touches established a focal point and an initial scale of 1 (`phase` is `Started`).
    Start(ScaleStartDetails),
    /// The focal point, scale or rotation changed (`phase` is `Moved`). Scale and rotation are since the start.
    Update(ScaleUpdateDetails),
    /// The gesture ended, with the velocity of the last finger lifted (`phase` is `Ended`).
    End(ScaleEndDetails),
}

impl TouchPinch {
    /// The [`PinchEvent::phase`](crate::PinchEvent::phase) of this callback.
    pub fn phase(&self) -> TouchPhase {
        match self {
            Self::Start(_) => TouchPhase::Started,
            Self::Update(_) => TouchPhase::Moved,
            Self::End(_) => TouchPhase::Ended,
        }
    }

    /// Flutter's `localFocalPoint`: the focal point relative to `origin`, the receiving element's.
    pub(crate) fn localize(&mut self, origin: Point<Pixels>) {
        match self {
            Self::Start(details) => details.local_focal_point = details.focal_point - origin,
            Self::Update(details) => details.local_focal_point = details.focal_point - origin,
            Self::End(_) => {}
        }
    }
}

/// The window's touch pinch: one element's, through the stages of Flutter's arena (module docs).
#[derive(Default)]
pub(crate) enum PinchArena {
    /// No touch is on a pinch element, or the touch recognizer won the ones that were.
    #[default]
    Idle,
    /// One touch on `owner`, which the touch recognizer has. Its latest event is replayed when a second lands.
    Single {
        owner: GlobalElementId,
        touch: TouchEvent,
    },
    /// Touches on `owner` the scale recognizer has: two or more, which the touch recognizer has a claim on too
    /// until either wins, or one it does not have (it was busy with another).
    Contested(Contest),
    /// The scale won `touches`: they reach only it.
    Won {
        owner: GlobalElementId,
        touches: SmallVec<[TouchId; 2]>,
        scale: Box<ScaleGestureRecognizer>,
        last: Reported,
    },
}

/// The contested stage's touches, each with its latest event.
pub(crate) struct Contest {
    owner: GlobalElementId,
    touches: SmallVec<[TouchEvent; 2]>,
    scale: Box<ScaleGestureRecognizer>,
    /// The touches' focal point when the contest last gained a touch: the drag wins when it travels past the hit
    /// slop.
    focal_start: Point<Pixels>,
}

/// What the gesture reported last: `delta` is relative to it.
#[derive(Clone, Copy)]
pub(crate) struct Reported {
    focal_point: Point<Pixels>,
    scale: f32,
}

/// One touch pinch event to dispatch: the callback, and the trackpad fields it fills.
pub(crate) struct PinchChange {
    pub touch: TouchPinch,
    /// The focal point (window pixels).
    pub position: Point<Pixels>,
    /// The scale's change since the last event (a trackpad pinch's meaning: zoom by `1 + delta`).
    pub delta: f32,
}

/// What the window does with one touch event, as the arena decided.
pub(crate) struct PinchStep {
    /// Whether the touch recognizer gets the event too.
    pub to_recognizer: bool,
    /// Touches the pinch just won, as `Cancelled` events: the touch recognizer drops them first.
    pub won: SmallVec<[TouchEvent; 2]>,
    /// The pinch events to dispatch, to the element that owns them.
    pub pinch: Option<(GlobalElementId, Vec<PinchChange>)>,
}

fn lifted(phase: TouchPhase) -> bool {
    matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled)
}

/// The touches' focal point: their mean position.
fn focal_point(touches: &[TouchEvent]) -> Point<Pixels> {
    let sum = touches
        .iter()
        .fold(Point::<Pixels>::default(), |sum, touch| {
            sum + touch.position
        });
    sum.map(|value| value / touches.len().max(1) as f32)
}

impl PinchArena {
    /// Routes one touch event. `joins` are the pinch elements its `Started` joined, deepest first
    /// ([`Window::join_pinch`]); later phases ignore them. `recognizer` is the touch recognizer's claim before this
    /// event. `touch_slop` is the tuning's (`GestureTuning::touch_slop`): the scale's slops and the drag's follow it.
    ///
    /// [`Window::join_pinch`]: crate::Window::join_pinch
    pub(crate) fn handle_event(
        &mut self,
        event: &TouchEvent,
        joins: &[GlobalElementId],
        recognizer: TouchClaim,
        now: Instant,
        touch_slop: Pixels,
    ) -> PinchStep {
        // Flutter's scale recognizer is timed by `PointerEvent.timeStamp`; the
        // dispatch time `now` stands in when the platform reports none.
        let now = event.time_stamp.unwrap_or(now);
        let mut step = PinchStep {
            to_recognizer: true,
            won: SmallVec::new(),
            pinch: None,
        };
        let joins = if event.phase == TouchPhase::Started {
            joins
        } else {
            &[]
        };
        let hit_slop = slops(event.device, Some(touch_slop)).hit;
        let new_scale = || {
            let mut scale = Box::<ScaleGestureRecognizer>::default();
            scale.touch_slop = Some(touch_slop);
            scale
        };
        *self = match mem::take(self) {
            Self::Idle => match (joins.first(), recognizer) {
                // A running pan takes a joining touch.
                (None, _) | (Some(_), TouchClaim::Pan(_)) => Self::Idle,
                // The touch recognizer takes this touch.
                (Some(owner), TouchClaim::None) => Self::Single {
                    owner: owner.clone(),
                    touch: event.clone(),
                },
                // It is busy with another: the scale has this one alone.
                (Some(owner), TouchClaim::Possible(_) | TouchClaim::Press(_)) => {
                    let mut scale = new_scale();
                    scale.add_pointer(event, now);
                    let outputs = scale.handle_event(event, now);
                    let contest = Contest {
                        owner: owner.clone(),
                        touches: smallvec![event.clone()],
                        scale,
                        focal_start: event.position,
                    };
                    contest.decide(outputs, recognizer, hit_slop, &mut step)
                }
            },
            Self::Single { owner, touch } if joins.contains(&owner) => {
                let mut scale = new_scale();
                let first = TouchEvent {
                    phase: TouchPhase::Started,
                    ..touch
                };
                let mut outputs = Vec::new();
                for started in [&first, event] {
                    scale.add_pointer(started, now);
                    outputs.extend(scale.handle_event(started, now));
                }
                let touches: SmallVec<[TouchEvent; 2]> = smallvec![touch, event.clone()];
                let focal_start = focal_point(&touches);
                let contest = Contest {
                    owner,
                    touches,
                    scale,
                    focal_start,
                };
                contest.decide(outputs, recognizer, hit_slop, &mut step)
            }
            Self::Single { owner, touch } if touch.id == event.id => {
                if lifted(event.phase) {
                    Self::Idle
                } else {
                    Self::Single {
                        owner,
                        touch: event.clone(),
                    }
                }
            }
            Self::Contested(mut contest)
                if joins.contains(&contest.owner)
                    || contest.touches.iter().any(|touch| touch.id == event.id) =>
            {
                if joins.contains(&contest.owner) {
                    contest.scale.add_pointer(event, now);
                    contest.touches.push(event.clone());
                    contest.focal_start = focal_point(&contest.touches);
                } else {
                    for touch in contest
                        .touches
                        .iter_mut()
                        .filter(|touch| touch.id == event.id)
                    {
                        *touch = event.clone();
                    }
                }
                let outputs = contest.scale.handle_event(event, now);
                contest.decide(outputs, recognizer, hit_slop, &mut step)
            }
            Self::Won {
                owner,
                mut touches,
                mut scale,
                mut last,
            } if joins.contains(&owner) || touches.contains(&event.id) => {
                step.to_recognizer = false;
                if joins.contains(&owner) {
                    scale.add_pointer(event, now);
                    touches.push(event.id);
                }
                let changes = pinch_changes(scale.handle_event(event, now), &mut last);
                if lifted(event.phase) {
                    touches.retain(|touch| *touch != event.id);
                }
                step.pinch = (!changes.is_empty()).then(|| (owner.clone(), changes));
                if touches.is_empty() {
                    Self::Idle
                } else {
                    Self::Won {
                        owner,
                        touches,
                        scale,
                        last,
                    }
                }
            }
            // A touch that is not this pinch's.
            other @ (Self::Single { .. } | Self::Contested(_) | Self::Won { .. }) => other,
        };
        step
    }

    /// The touch recognizer's claim after it handled a touch or a timer. A pan of the one touch the scale holds
    /// wins it (the scale never had a second); a long press or touch drag an element claimed wins every touch the
    /// scale holds before winning. A pan in a contest is judged by the touches' focal point instead
    /// ([`Contest::decide`]).
    pub(crate) fn touch_recognizer_claimed(&mut self, claim: TouchClaim) {
        let loses = match (&*self, claim) {
            (Self::Single { touch, .. }, TouchClaim::Pan(won) | TouchClaim::Press(won)) => {
                touch.id == won
            }
            (Self::Contested(contest), TouchClaim::Press(won)) => {
                contest.touches.iter().any(|touch| touch.id == won)
            }
            (Self::Contested(_), TouchClaim::Pan(_))
            | (_, TouchClaim::None | TouchClaim::Possible(_))
            | (Self::Idle | Self::Won { .. }, _) => false,
        };
        if loses {
            *self = Self::Idle;
        }
    }
}

impl Contest {
    /// The contest after the scale saw an event: won if the scale accepted; lost if the touches' focal point
    /// travelled past the hit slop and the touch recognizer has one of them (its drag got there first); else back
    /// to one touch the touch recognizer has, or none, if fingers lifted (the scale gives up); else still contested.
    fn decide(
        mut self,
        outputs: Vec<ScaleOutput>,
        recognizer: TouchClaim,
        hit_slop: Pixels,
        step: &mut PinchStep,
    ) -> PinchArena {
        let accepted = outputs.iter().any(|output| {
            matches!(
                output,
                ScaleOutput::Resolve {
                    disposition: GestureDisposition::Accepted,
                    ..
                }
            )
        });
        if accepted {
            step.to_recognizer = false;
            step.won = self
                .touches
                .iter()
                .map(|touch| TouchEvent {
                    phase: TouchPhase::Cancelled,
                    ..touch.clone()
                })
                .collect();
            let mut last = Reported {
                focal_point: Point::default(),
                scale: 1.0,
            };
            let changes = pinch_changes(outputs, &mut last);
            step.pinch = (!changes.is_empty()).then(|| (self.owner.clone(), changes));
            let touches: SmallVec<[TouchId; 2]> = self
                .touches
                .iter()
                .filter(|touch| !lifted(touch.phase))
                .map(|touch| touch.id)
                .collect();
            return if touches.is_empty() {
                PinchArena::Idle
            } else {
                PinchArena::Won {
                    owner: self.owner,
                    touches,
                    scale: self.scale,
                    last,
                }
            };
        }
        let recognizers = match recognizer {
            TouchClaim::None => None,
            TouchClaim::Possible(id) | TouchClaim::Pan(id) | TouchClaim::Press(id) => Some(id),
        };
        let dragged = recognizers.is_some_and(|id| self.touches.iter().any(|touch| touch.id == id))
            && (focal_point(&self.touches) - self.focal_start).magnitude() > f64::from(hit_slop);
        if dragged {
            return PinchArena::Idle;
        }
        self.touches.retain(|touch| !lifted(touch.phase));
        match self.touches.len() {
            0 => PinchArena::Idle,
            1 if recognizers == Some(self.touches[0].id) => PinchArena::Single {
                owner: self.owner,
                touch: self.touches.remove(0),
            },
            _ => PinchArena::Contested(self),
        }
    }
}

/// The scale's callbacks as pinch changes, from `last`, what the gesture reported last.
fn pinch_changes(outputs: Vec<ScaleOutput>, last: &mut Reported) -> Vec<PinchChange> {
    outputs
        .into_iter()
        .filter_map(|output| {
            let (delta, touch) = match output {
                ScaleOutput::Resolve { .. } => return None,
                ScaleOutput::Start(details) => {
                    *last = Reported {
                        focal_point: details.focal_point,
                        scale: 1.0,
                    };
                    (0.0, TouchPinch::Start(details))
                }
                ScaleOutput::Update(details) => {
                    // Fingers that met make a zero scale: no relative change from it.
                    let delta = if last.scale > 0.0 {
                        details.scale / last.scale - 1.0
                    } else {
                        0.0
                    };
                    *last = Reported {
                        focal_point: details.focal_point,
                        scale: details.scale,
                    };
                    (delta, TouchPinch::Update(details))
                }
                ScaleOutput::End(details) => (0.0, TouchPinch::End(details)),
            };
            Some(PinchChange {
                touch,
                position: last.focal_point,
                delta,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "pinch_tests.rs"]
mod tests;
