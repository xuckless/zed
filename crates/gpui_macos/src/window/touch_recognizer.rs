//! `GPUITouchRecognizer`: the AppKit glue that hands direct touches to GPUI, from macOS 27.
//!
//! AppKit hands direct touches to gesture recognizers only, never to a view's responder methods, so every
//! GPUIView carries one `GPUITouchRecognizer`: an `NSGestureRecognizer` subclass overriding
//! `touches…WithEvent:` and `reset`, Apple's custom-recognizer pattern (`NSGestureRecognizer (NSSubclassUse)` in
//! the SDK headers). Each callback reads the event's touches in its own phase and hands them to GPUI, whose own
//! recognizers (taps, pans, pinch) take it from there; the decisions are in `touch.rs`. Before macOS 27 no recognizer is added.

use super::{
    MacWindowState, is_macos_version_at_least,
    touch::{BEGAN, CANCELLED, CHANGED, ENDED, Left, MOVED, RecognizerState, TOUCHING, next_state},
    window_state_if_set,
};
use gpui::{PlatformInput, PointerDevice, TouchEvent, TouchPhase, point, px};
use gpui_apple::process_info::uptime_instant;
use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel},
    sel,
};
use objc2_foundation::{NSInteger, NSOperatingSystemVersion, NSPoint, NSUInteger};
use parking_lot::Mutex;
use std::{ptr, time::Instant};

/// `NSTouchTypeMaskDirect` (NSTouch.h): a finger on a screen.
const TOUCH_TYPE_MASK_DIRECT: NSUInteger = 1 << 0;

/// The first macOS with direct touch (`NSScreen.touchCapabilities`, `NSIsTouchNative` are `macos(27.0)`).
const DIRECT_TOUCH_MACOS: NSOperatingSystemVersion = NSOperatingSystemVersion {
    majorVersion: 27,
    minorVersion: 0,
    patchVersion: 0,
};

/// An `NSTouch.identity`, compared with `isEqual:` as NSTouch.h asks.
pub(super) struct TouchIdentity(Retained<AnyObject>);

impl PartialEq for TouchIdentity {
    fn eq(&self, other: &Self) -> bool {
        // SAFETY: both are live identity objects, which implement `NSObject`'s `isEqual:`.
        let equal: Bool = unsafe { msg_send![&*self.0, isEqual: &*other.0] };
        equal.as_bool()
    }
}

/// Registers `GPUITouchRecognizer`; `build_classes` calls it once at load.
pub(super) unsafe fn register_touch_recognizer_class() {
    let mut decl = ClassBuilder::new(c"GPUITouchRecognizer", class!(NSGestureRecognizer))
        .expect("GPUITouchRecognizer is registered once, by build_classes");
    // Callback → the `NSTouchPhase` it handles: AppKit calls each one that a touch of the event is in, all with the
    // same event. Extend: one row per `touches…WithEvent:` callback.
    let callbacks: [(Sel, extern "C" fn(&'static AnyObject, Sel, *mut AnyObject)); 4] = [
        (sel!(touchesBeganWithEvent:), handle_touches::<BEGAN>),
        (sel!(touchesMovedWithEvent:), handle_touches::<MOVED>),
        (sel!(touchesEndedWithEvent:), handle_touches::<ENDED>),
        (
            sel!(touchesCancelledWithEvent:),
            handle_touches::<CANCELLED>,
        ),
    ];
    for (selector, handler) in callbacks {
        // SAFETY: the signature matches `- (void)touches…WithEvent:(NSEvent *)event`.
        unsafe { decl.add_method(selector, handler) };
    }
    // SAFETY: the signature matches `- (void)reset`.
    unsafe { decl.add_method(sel!(reset), handle_reset as extern "C" fn(_, _)) };
    decl.register();
}

/// Gives `view` (a GPUIView) its direct-touch recognizer, on macOS 27 and later.
pub(super) unsafe fn add_touch_recognizer(view: *mut AnyObject) {
    if !is_macos_version_at_least(DIRECT_TOUCH_MACOS) {
        return;
    }
    let class: &AnyClass = class!(GPUITouchRecognizer);
    // SAFETY: on the main thread during window creation; `initWithTarget:action:` takes a nil target and action
    // (the handler is the subclass's own `touches…WithEvent:`). The view retains the recognizer, so ours is
    // autoreleased.
    unsafe {
        let recognizer: *mut AnyObject = msg_send![class, alloc];
        let recognizer: *mut AnyObject = msg_send![
            recognizer,
            initWithTarget: ptr::null_mut::<AnyObject>(),
            action: None::<Sel>
        ];
        let _: () = msg_send![recognizer, setAllowedTouchTypes: TOUCH_TYPE_MASK_DIRECT];
        let _: () = msg_send![view, addGestureRecognizer: recognizer];
        let _: *mut AnyObject = msg_send![recognizer, autorelease];
    }
}

/// Hands `event` to GPUI through `touch::TouchQueue`, so an end that arrives while GPUI's callback is busy still
/// reaches it.
fn dispatch_touch(window_state: &Mutex<MacWindowState>, event: TouchEvent) {
    let mut lock = window_state.lock();
    let callback = lock.event_callback.take();
    lock.touch_queue.push(event, callback.is_some());
    drop(lock);
    let Some(mut callback) = callback else {
        return;
    };
    // Every touch waiting, those queued while `callback` ran among them.
    loop {
        let next = window_state.lock().touch_queue.pop();
        let Some(event) = next else {
            break;
        };
        callback(PlatformInput::Touch(event));
    }
    window_state.lock().event_callback = Some(callback);
}

fn touch_event(
    (id, phase, position): (gpui::TouchId, TouchPhase, gpui::Point<gpui::Pixels>),
    time_stamp: Option<Instant>,
) -> TouchEvent {
    TouchEvent {
        id,
        phase,
        position,
        predicted_position: None,
        force: None,
        device: PointerDevice::Touch,
        time_stamp,
    }
}

/// One `touches…WithEvent:` callback, `CALLBACK` the `NSTouchPhase` bit it handles (`TouchIds::track_callback`
/// takes the event's touches in that phase only).
extern "C" fn handle_touches<const CALLBACK: NSUInteger>(
    this: &AnyObject,
    _: Sel,
    native_event: *mut AnyObject,
) {
    // SAFETY: AppKit calls this on the main thread with a live `NSEvent`, for a recognizer that
    // `add_touch_recognizer` put on a GPUIView, which owns the window-state ivar.
    unsafe {
        let view: *mut AnyObject = msg_send![this, view];
        let Some(view) = view.as_ref() else { return };
        let Some(window_state) = window_state_if_set(view) else {
            return;
        };
        // `NSEvent.timestamp` counts seconds since boot, as Flutter's macOS embedder reads it (`uptime_instant`).
        let time_stamp = uptime_instant(msg_send![native_event, timestamp]);
        let changed: *mut AnyObject =
            msg_send![native_event, touchesMatchingPhase: CHANGED, inView: view];
        let changed: *mut AnyObject = msg_send![changed, allObjects];
        let count: NSUInteger = msg_send![changed, count];
        let height = window_state.lock().content_size().height;
        let mut touches = Vec::new();
        for index in 0..count {
            let touch: *mut AnyObject = msg_send![changed, objectAtIndex: index];
            let bit: NSUInteger = msg_send![touch, phase];
            let identity: *mut AnyObject = msg_send![touch, identity];
            let Some(identity) = Retained::retain(identity) else {
                continue;
            };
            let location: NSPoint = msg_send![touch, locationInView: view];
            // GPUIView is not flipped: AppKit's y grows upward, as for mouse events.
            let position = point(px(location.x as f32), height - px(location.y as f32));
            touches.push((TouchIdentity(identity), bit, position));
        }
        let touches = window_state
            .lock()
            .touch_ids
            .track_callback(CALLBACK, touches);
        for touch in touches {
            dispatch_touch(&window_state, touch_event(touch, time_stamp));
        }
        let touching: *mut AnyObject =
            msg_send![native_event, touchesMatchingPhase: TOUCHING, inView: view];
        let touching: NSUInteger = msg_send![touching, count];
        let left = match (touching > 0, CALLBACK == CANCELLED) {
            (true, _) => Left::Touching,
            (false, true) => Left::Cancelled,
            (false, false) => Left::Lifted,
        };
        let state: NSInteger = msg_send![this, state];
        if let Some(next) =
            RecognizerState::from_raw(state).and_then(|state| next_state(state, left))
        {
            let _: () = msg_send![this, setState: next as NSInteger];
        }
    }
}

/// AppKit resets the recognizer after it finishes, or when it drops it mid-sequence (its view leaves the window, a
/// system gesture takes over): GPUI's touches still live there are cancelled where they last were.
extern "C" fn handle_reset(this: &AnyObject, _: Sel) {
    // SAFETY: AppKit calls this on the main thread, for a recognizer that `add_touch_recognizer` put on a GPUIView,
    // which owns the window-state ivar; `super` is NSGestureRecognizer, whose `reset` the subclass must call.
    unsafe {
        let _: () = msg_send![super(this, class!(NSGestureRecognizer)), reset];
        let view: *mut AnyObject = msg_send![this, view];
        let Some(view) = view.as_ref() else { return };
        let Some(window_state) = window_state_if_set(view) else {
            return;
        };
        let cancelled = window_state.lock().touch_ids.cancel_all();
        for (id, position) in cancelled {
            dispatch_touch(
                &window_state,
                touch_event((id, TouchPhase::Cancelled, position), None),
            );
        }
    }
}
