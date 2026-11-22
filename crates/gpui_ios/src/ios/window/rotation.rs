//! The window's view nesting and how it turns, kept out of upstream's window.rs behind one-line hooks.
//!
//! GPUI's view fills a host view, where native views go beneath it (native_views.rs), and the host sits in the view
//! controller's plain root view. GPUI's `CAMetalLayer` is its view's own backing layer, so a native view can only
//! go beneath it as a sibling in a superview, and upstream makes GPUI's view the view controller's root view. The
//! nesting exists from the window's start, not only while native views show: creating it for the first view would
//! swap the view controller's view and move GPUI's view, with its text input view (the first responder while a
//! GPUI field has the keyboard), into new superviews in a live window, each time the first view comes or the last
//! goes. The two plain views draw nothing and cost no frame.
//!
//! UIKit lays the root view out inside its rotation animation, which would stretch GPUI's last frame to each
//! in-between size while native views jump to their new frames. Instead the host takes the new size at once and
//! only its centre follows the animation, and the frame at the new size is drawn in that same transaction, so
//! GPUI's content, the holes and the views turn as one rigid picture (every app gets this turn, views or not).
//! Flutter's FlutterViewController.mm (`viewWillTransitionToSize:withTransitionCoordinator:`) @ 3.50.1 only delays
//! its viewport update to halfway through the turn, which halves the stretch.

use super::super::native_view_state::Rotation;
use super::{IosDisplay, IosWindowState};
use gpui::native_view_placement::hole_inset;
use objc2::{MainThreadMarker, MainThreadOnly, msg_send, rc::Retained, runtime::AnyObject};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_quartz_core::CACurrentMediaTime;
use objc2_ui_kit::UIView;
use std::rc::Weak;

/// Nests GPUI's view in a host view in a plain root view, and returns the root, the view controller's view.
pub(super) fn nest(
    view: &UIView,
    frame: CGRect,
    main_thread: MainThreadMarker,
) -> Retained<UIView> {
    let host = UIView::initWithFrame(UIView::alloc(main_thread), frame);
    host.addSubview(view);
    let root = UIView::initWithFrame(UIView::alloc(main_thread), frame);
    root.addSubview(&host);
    root
}

/// After UIKit's own `viewWillTransitionToSize:withTransitionCoordinator:`: marks the turn until the coordinator's
/// completion, since its resampled edges need the holes cut inside the views. A turn with no duration is not
/// marked, as in Flutter.
pub(super) fn will_transition(window: Weak<IosWindowState>, coordinator: &AnyObject) {
    // SAFETY: A live UIViewControllerTransitionCoordinator, on the main thread.
    let duration: f64 = unsafe { msg_send![coordinator, transitionDuration] };
    if duration == 0. {
        return;
    }
    let Some(turning) = window.upgrade() else {
        return;
    };
    turning.set_rotation(Rotation::Turning);
    let completion = block2::RcBlock::new(move |_: *mut AnyObject| {
        if let Some(window) = window.upgrade() {
            window.set_rotation(Rotation::Settled);
        }
    });
    // SAFETY: `animateAlongsideTransition:completion:` takes an optional animation block and a completion block
    // given the transition context; it copies the block.
    let _: objc2::runtime::Bool = unsafe {
        msg_send![
            coordinator,
            animateAlongsideTransition: std::ptr::null::<block2::Block<dyn Fn(*mut AnyObject)>>(),
            completion: &*completion
        ]
    };
}

impl IosWindowState {
    /// The start of `handle_layout_change`: the host takes the root's new size at once, and only its centre follows
    /// UIKit's animation.
    pub(super) fn follow_root(&self) {
        let (Some(host), Some(root)) = (
            self.view.superview(),
            self.view.superview().and_then(|host| host.superview()),
        ) else {
            return;
        };
        let root_size = root.bounds().size;
        let resize = block2::RcBlock::new(|| host.setBounds(CGRect::new(CGPoint::ZERO, root_size)));
        UIView::performWithoutAnimation(&resize, MainThreadMarker::from(&*root));
        host.setCenter(CGPoint::new(root_size.width / 2., root_size.height / 2.));
    }

    /// Gives the renderer the hole inset for the window's rotation and display (native_view_state.rs).
    pub(super) fn update_hole_inset(&self) {
        let display = IosDisplay::from_screen(self.window.screen());
        let resamples = self
            .rotation
            .get()
            .resamples(display.scale() as f64, display.native_scale() as f64);
        self.renderer
            .lock()
            .set_native_view_hole_inset(hole_inset(resamples));
    }

    /// UIKit starts or ends turning the window. The frame drawn when it ends cuts the holes exactly again.
    fn set_rotation(&self, rotation: Rotation) {
        self.rotation.set(rotation);
        super::native_views::rotation_changed(self);
        self.update_hole_inset();
        self.force_next_frame.set(true);
    }

    /// The end of a `handle_layout_change` that resized the window: the frame at the new size goes out with UIKit's
    /// layout, in its transaction, not at the next display link.
    pub(super) fn draw_resized_frame(&self) {
        self.force_next_frame.set(true);
        self.request_frame(CACurrentMediaTime());
    }
}
