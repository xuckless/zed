//! Composites the app's `UIView`s beneath GPUI's view, which is non-opaque
//! while one shows (gpui_apple's `MetalRenderer` clears a hole over each).
//!
//! Each view sits in a clipping container, a sibling below GPUI's view in its
//! host view (window.rs), so GPUI's view stays on top and takes every
//! touch ([`gpui::NativeViewInput::Window`]); the app forwards touches into its
//! view if it wants them. A scene that moves, resizes, shows or hides a view is
//! presented with the Core Animation transaction that places it, so hole and
//! view reach the screen together.
//!
//! Lifecycle, z-order and clipping follow Flutter's
//! `FlutterPlatformViewsController.mm` (oracle only: Flutter draws over its
//! views, this draws under them): a view stays attached while its handle
//! lives, is hidden in a frame that doesn't paint it, and is reordered only
//! when the frame's order changes. The placement maths, shared with macOS, is
//! gpui_apple's native_view_placement.rs.

use gpui::{NativeViewId, Rgba, Scene, native_view_placement::background};
use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_ui_kit::UIView;
use std::{any::Any, ffi::c_void, ptr::NonNull, rc::Weak};

use super::{IosWindow, IosWindowState};
use gpui_apple::native_view_placement::{Layout, Placement, YAxis, plan_layout};

#[derive(Default)]
pub(super) struct NativeViews {
    views: Vec<AttachedView>,
    /// The views the last scene showed, from back to front.
    stacking: Vec<NativeViewId>,
    /// The color the window was last given behind its native views.
    background: Option<Rgba>,
}

struct AttachedView {
    id: NativeViewId,
    view: Retained<UIView>,
    /// The view's superview, which clips it to its element's content mask.
    container: Retained<UIView>,
    /// Where the view was last placed, or `None` while it is hidden.
    placement: Option<Placement>,
}

/// Adds `view` beneath GPUI's view, hidden until a scene shows it.
///
/// # Safety
///
/// `view` must point to a live `UIView` that is not in another view hierarchy,
/// and this must be called on the main thread.
pub(super) unsafe fn attach(
    window: &std::rc::Rc<IosWindowState>,
    id: NativeViewId,
    view: NonNull<c_void>,
) -> Option<Box<dyn Any>> {
    // SAFETY: The caller guarantees `view` is a live `UIView`.
    let view = unsafe { Retained::retain(view.as_ptr().cast::<UIView>()) }?;
    let gpui_view: &UIView = &window.view;
    let superview = gpui_view.superview()?;
    // SAFETY: Plain UIView construction and configuration on the main thread;
    // GPUI's view is in `superview`, so the container goes directly below it.
    let container = unsafe {
        let container: Retained<UIView> = msg_send![objc2::class!(UIView), new];
        let _: () = msg_send![&*container, setClipsToBounds: true];
        container.setHidden(true);
        container.addSubview(&view);
        let _: () = msg_send![&*superview, insertSubview: &*container, belowSubview: gpui_view];
        container
    };
    window.native_views.borrow_mut().views.push(AttachedView {
        id,
        view,
        container,
        placement: None,
    });
    Some(Box::new(Attachment {
        window: std::rc::Rc::downgrade(window),
        id,
    }))
}

/// Detaches its view when the last `gpui::NativeView` handle is dropped.
struct Attachment {
    window: Weak<IosWindowState>,
    id: NativeViewId,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let attached = {
            let mut native_views = window.native_views.borrow_mut();
            let Some(index) = native_views
                .views
                .iter()
                .position(|attached| attached.id == self.id)
            else {
                return;
            };
            native_views.stacking.retain(|id| *id != self.id);
            native_views.views.remove(index)
        };
        // Handles are dropped on the main thread: `gpui::NativeView` is not `Send`.
        attached.container.removeFromSuperview();
        attached.view.removeFromSuperview();
    }
}

/// Draws a scene: plans its placements, presents it in step with them when it
/// places a view, then applies them.
pub(super) fn draw(window: &IosWindow, scene: &Scene) {
    let CGRect { origin, size } = window.view.frame();
    let (layout, background) = {
        let native_views = window.native_views.borrow();
        let layout = plan_layout(
            native_views
                .views
                .iter()
                .map(|attached| (attached.id, attached.placement)),
            &native_views.stacking,
            scene,
            f64::from(window.scale_factor.get()),
            [origin.x, origin.y, size.width, size.height],
            YAxis::Down,
        );
        // Core's rule, kept until a scene without native views: it covers any
        // frame the compositor shows early.
        let background = background(&scene.native_views);
        let background = (!native_views.views.is_empty() && background != native_views.background)
            .then_some(background);
        (layout, background)
    };
    let presented = window
        .renderer
        .lock()
        .draw_placing_native_views(scene, layout.places_views());
    // A scene that never reached the screen has no opening for the views to
    // move to; the next one plans them again.
    if presented {
        apply_layout(window, &window.view, layout, background);
    }
}

/// Applies a planned layout and the window's new background, if it changed.
/// The views are cloned out first, because UIKit can call back into the window
/// while frames change.
fn apply_layout(
    window: &IosWindowState,
    gpui_view: &UIView,
    layout: Layout,
    background: Option<Option<Rgba>>,
) {
    let (placements, stacking) = {
        let mut native_views = window.native_views.borrow_mut();
        let mut placements = Vec::new();
        for (id, placement) in layout.placements {
            if let Some(attached) = native_views.views.iter_mut().find(|view| view.id == id) {
                attached.placement = placement;
                placements.push((attached.view.clone(), attached.container.clone(), placement));
            }
        }
        if let Some(background) = background {
            native_views.background = background;
        }
        let stacking = layout.stacking.map(|stacking| {
            let containers: Vec<_> = stacking
                .iter()
                .filter_map(|id| native_views.views.iter().find(|view| view.id == *id))
                .map(|attached| attached.container.clone())
                .collect();
            native_views.stacking = stacking;
            containers
        });
        (placements, stacking)
    };
    let to_rect = |[x, y, width, height]: [f64; 4]| {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    };
    // The colour goes on GPUI's host view, right behind GPUI's view and the
    // views, not on the window: through a rotation the window shows around the
    // turning content, as in any app, and only a hole that left its view shows it.
    let host = gpui_view.superview();
    // A scene can be drawn inside one of UIKit's animations (a rotation's
    // layout, rotation.rs), which must not animate the views away from their
    // holes, nor the colour behind them.
    let place = block2::RcBlock::new(|| {
        for (view, container, placement) in &placements {
            match placement {
                Some(placement) => {
                    container.setFrame(to_rect(placement.container));
                    view.setFrame(to_rect(placement.view));
                    container.setHidden(false);
                }
                None => container.setHidden(true),
            }
        }
        if let (Some(background), Some(host)) = (background, &host) {
            colour_host(host, background.filter(|_| window.rotation.get().shows_background()));
        }
    });
    UIView::performWithoutAnimation(&place, objc2::MainThreadMarker::from(gpui_view));
    if let (Some(containers), Some(superview)) = (stacking, &host) {
        // Each one goes directly below GPUI's view, so the last inserted, which
        // was painted last, ends up in front.
        for container in containers {
            // SAFETY: Both views are live and in `superview`, on the main thread.
            let _: () = unsafe {
                msg_send![&**superview, insertSubview: &*container, belowSubview: gpui_view]
            };
        }
    }
}

/// Gives GPUI's host view the views' gap colour, or none.
fn colour_host(host: &UIView, colour: Option<Rgba>) {
    // SAFETY: `+[UIColor colorWithRed:green:blue:alpha:]` returns a
    // UIColor; the host is live, on the main thread. Nil clears it.
    unsafe {
        let color: *mut AnyObject = match colour {
            Some(Rgba { r, g, b, .. }) => msg_send![
                objc2::class!(UIColor),
                colorWithRed: r as f64,
                green: g as f64,
                blue: b as f64,
                alpha: 1.0f64
            ],
            None => std::ptr::null_mut(),
        };
        let _: () = msg_send![host, setBackgroundColor: color];
    }
}

/// The window starts or ends turning: the host shows the gap colour only while settled (`Rotation::shows_background`).
pub(super) fn rotation_changed(window: &IosWindowState) {
    let Some(host) = window.view.superview() else {
        return;
    };
    let colour = window.native_views.borrow().background;
    let recolour = block2::RcBlock::new(|| {
        colour_host(&host, colour.filter(|_| window.rotation.get().shows_background()))
    });
    UIView::performWithoutAnimation(&recolour, objc2::MainThreadMarker::from(&*host));
}
