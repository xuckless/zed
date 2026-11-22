//! iOS Window implementation using UIWindow and UIViewController.
//!
//! iOS windows are fundamentally different from desktop windows:
//! - Always fullscreen (or split-screen on iPad)
//! - No title bar or window chrome
//! - Touch-based input
//! - Safe area insets for notch/home indicator
//!
//! The window is backed by a UIWindow containing a UIViewController
//! whose view hosts a CAMetalLayer.

use super::events::*;

mod native_views;
mod rotation;
use super::keyboard::{KeyResponder, Press, PressPhase, Primary};
use super::keyboard_inset::{
    InsetSample, KeyboardInsetAction, KeyboardInsetManager, KeyboardNotification, KeyboardSpring,
    NotificationName, Rect, ViewGeometry,
};
use super::native_view_state::{KeyboardEvent, Rotation};
use super::text_input::TextInputView;
use super::{CallbackSlot, IosDisplay, platform::IosPlatformState};
use gpui::{
    AnyWindowHandle, BackGestureEvent, BackGesturePhase, BackGestureSource, Bounds, Capslock,
    Corners, DevicePixels, DispatchEventResult, DisplayId, Edges, GpuSpecs, Modifiers, Pixels,
    PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Point,
    PromptButton, PromptLevel, RequestFrameOptions, Scene, Size, TextInputConfiguration,
    TextInputStateChange, TouchEvent, TouchId, TouchPhase, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowInsets, WindowParams,
    WindowVisibility, px, size,
};
use gpui_apple::{
    main_thread_marker,
    metal_renderer::{Context as MetalContext, MetalRenderer},
    process_info,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol, NSSet, NSString,
    NSValue,
};
use objc2_quartz_core::{CACurrentMediaTime, CAMetalLayer, CASpringAnimation};
use objc2_ui_kit::{
    NSValueUIGeometryExtensions, UIKeyboardAnimationDurationUserInfoKey,
    UIKeyboardFrameBeginUserInfoKey, UIKeyboardFrameEndUserInfoKey, UIKeyboardIsLocalUserInfoKey,
    UIKeyboardWillChangeFrameNotification, UIKeyboardWillHideNotification,
    UIKeyboardWillShowNotification, UIStatusBarStyle, UIUserInterfaceIdiom,
    UIUserInterfaceSizeClass,
};
use objc2_ui_kit::{
    UICornerConfiguration, UICornerRadius, UIEvent, UIGestureRecognizer,
    UIGestureRecognizerDelegate, UIPanGestureRecognizer, UIPress, UIPressPhase, UIPressesEvent,
    UIRectCorner, UIRectEdge, UIScreen, UIScreenEdgePanGestureRecognizer, UIScrollTypeMask,
    UITouch, UITraitCollection, UITraitEnvironment, UIUserInterfaceLayoutDirection,
    UIUserInterfaceStyle, UIView, UIViewAutoresizing, UIViewController, UIWindow, UIWindowScene,
};
use parking_lot::Mutex;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, UiKitDisplayHandle, UiKitWindowHandle};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::Arc,
};

const KEYBOARD_DISMISS_DISTANCE: Pixels = px(24.);

// AudioToolbox's system sounds, for `Haptic::Vibrate` (no objc2 crate is
// vendored for AudioToolbox; the one function is declared here).
#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioServicesPlaySystemSound(sound: u32);
}
/// `kSystemSoundID_Vibrate` (AudioServices.h).
const K_SYSTEM_SOUND_ID_VIBRATE: u32 = 0x0000_0FFF;

#[derive(Clone, Copy)]
struct KeyboardDismissTouch {
    id: TouchId,
    start_position: Point<Pixels>,
}

/// Global storage for the current status bar style.
/// 0 = default (dark content), 1 = light content.
/// Accessed from the main thread only.
static STATUS_BAR_STYLE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[derive(Default)]
pub(super) struct WindowReference(pub(super) RefCell<Weak<IosWindowState>>);

impl WindowReference {
    pub(super) fn with_window<R>(&self, callback: impl FnOnce(&IosWindowState) -> R) -> Option<R> {
        let window = self.0.borrow().upgrade()?;
        // Keep callback storage and native objects alive even if the callback
        // synchronously closes the platform window.
        Some(callback(&window))
    }
}

define_class!(
    #[unsafe(super = UIViewController)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIViewController"]
    #[ivars = WindowReference]
    struct ViewController;

    impl ViewController {
        #[unsafe(method(preferredStatusBarStyle))]
        fn preferred_status_bar_style(&self) -> UIStatusBarStyle {
            let style = STATUS_BAR_STYLE.load(std::sync::atomic::Ordering::Relaxed);
            if style == 1 {
                UIStatusBarStyle::LightContent
            } else {
                UIStatusBarStyle::DarkContent
            }
        }

        // Marks a rotation's turn (rotation.rs).
        #[unsafe(method(viewWillTransitionToSize:withTransitionCoordinator:))]
        fn view_will_transition_to_size(&self, size: CGSize, coordinator: &AnyObject) {
            // SAFETY: UIViewController's own method, with UIKit's arguments; the coordinator is live for the call.
            let _: () = unsafe {
                msg_send![super(self), viewWillTransitionToSize: size, withTransitionCoordinator: coordinator]
            };
            rotation::will_transition(self.ivars().0.borrow().clone(), coordinator);
        }

        #[unsafe(method(viewDidLayoutSubviews))]
        fn view_did_layout_subviews(&self) {
            unsafe {
                let _: () = msg_send![super(self), viewDidLayoutSubviews];
            }
            self.ivars().with_window(IosWindowState::handle_layout_change);
        }

        // Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/framework/Source/
        // FlutterViewController.mm (`pressesBegan:withEvent:` … `pressesCancelled:withEvent:`) @ 3.50.1
        // (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
        // third_party/flutter/LICENSE.
        // Each press goes to GPUI first; one GPUI leaves unhandled goes on to `super`, alone, as Flutter's
        // `superPresses…`, and up the responder chain to UIKit's text system, which types it into the focused field.
        // The text input view is first responder while a field is focused; it has no presses methods of its own,
        // so its presses come here through the chain.
        #[unsafe(method(pressesBegan:withEvent:))]
        fn presses_began(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            // SAFETY: UIResponder's own `pressesBegan:withEvent:`, with a set of UIKit's presses and its event.
            self.handle_presses(presses, |press| unsafe {
                msg_send![super(self), pressesBegan: press, withEvent: event]
            });
        }

        #[unsafe(method(pressesChanged:withEvent:))]
        fn presses_changed(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            // SAFETY: UIResponder's own `pressesChanged:withEvent:`, with a set of UIKit's presses and its event.
            self.handle_presses(presses, |press| unsafe {
                msg_send![super(self), pressesChanged: press, withEvent: event]
            });
        }

        #[unsafe(method(pressesEnded:withEvent:))]
        fn presses_ended(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            // SAFETY: UIResponder's own `pressesEnded:withEvent:`, with a set of UIKit's presses and its event.
            self.handle_presses(presses, |press| unsafe {
                msg_send![super(self), pressesEnded: press, withEvent: event]
            });
        }

        #[unsafe(method(pressesCancelled:withEvent:))]
        fn presses_cancelled(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            // SAFETY: UIResponder's own `pressesCancelled:withEvent:`, with a set of UIKit's presses and its event.
            self.handle_presses(presses, |press| unsafe {
                msg_send![super(self), pressesCancelled: press, withEvent: event]
            });
        }
    }
);

impl ViewController {
    fn new(main_thread: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(main_thread).set_ivars(WindowReference::default());
        unsafe { msg_send![super(this), init] }
    }

    /// Offers each press to GPUI; `next` (UIKit's handling, as `super`) gets those GPUI leaves unhandled, and the
    /// ups and cancels of the downs it got, one at a time. Flutter: "I have not yet seen multiple presses in the set
    /// in the wild".
    fn handle_presses(&self, presses: &NSSet<UIPress>, next: impl Fn(&NSSet<UIPress>)) {
        for press in presses {
            let uikit = self
                .ivars()
                .with_window(|window| window.handle_press(&press))
                .unwrap_or(true);
            if uikit {
                next(&NSSet::from_slice(&[&*press]));
            }
        }
    }
}

/// Set the iOS status bar content style (light or dark text/icons).
///
/// This updates the stored style and asks the root view controller
/// to re-query `preferredStatusBarStyle`.
pub fn set_status_bar_style(style: crate::StatusBarContentStyle) {
    use crate::StatusBarContentStyle;

    let value = match style {
        StatusBarContentStyle::Light => 1,
        StatusBarContentStyle::Dark => 0,
    };
    STATUS_BAR_STYLE.store(value, std::sync::atomic::Ordering::Relaxed);

    // This public free function predates explicit platform ownership.
    // Keep its single-application behavior without making runtime events global.
    if let Some(platform) = super::application::current_platform() {
        platform.application.with_windows(|window| {
            window.view_controller.setNeedsStatusBarAppearanceUpdate();
        });
    }
}

define_class!(
    #[unsafe(super = UIView)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIMetalView"]
    #[ivars = WindowReference]
    struct MetalView;

    unsafe impl NSObjectProtocol for MetalView {}

    impl MetalView {
        #[unsafe(method(layerClass))]
        fn layer_class() -> &'static AnyClass {
            CAMetalLayer::class()
        }

        // This callback also covers iOS 15/16, before UIKit's trait-registration API.
        #[unsafe(method(traitCollectionDidChange:))]
        fn trait_collection_did_change(&self, previous: Option<&UITraitCollection>) {
            unsafe {
                let _: () = msg_send![super(self), traitCollectionDidChange: previous];
                if self
                    .traitCollection()
                    .hasDifferentColorAppearanceComparedToTraitCollection(previous)
                {
                    self.ivars()
                        .with_window(IosWindowState::notify_appearance_changed);
                }
            }
        }

        #[unsafe(method(touchesBegan:withEvent:))]
        fn touches_began(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(touchesMoved:withEvent:))]
        fn touches_moved(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(touchesEnded:withEvent:))]
        fn touches_ended(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(touchesCancelled:withEvent:))]
        fn touches_cancelled(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(handleScroll:))]
        fn handle_scroll(&self, recognizer: &UIPanGestureRecognizer) {
            self.ivars()
                .with_window(|window| window.handle_scroll(recognizer));
        }

        #[unsafe(method(handleBackGesture:))]
        fn handle_back_gesture(&self, recognizer: &UIScreenEdgePanGestureRecognizer) {
            self.ivars()
                .with_window(|window| window.handle_back_gesture(recognizer));
        }
    }

    unsafe impl UIGestureRecognizerDelegate for MetalView {
        // Only the back-gesture recognizer uses this view as its delegate.
        #[unsafe(method(gestureRecognizerShouldBegin:))]
        fn gesture_recognizer_should_begin(&self, recognizer: &UIGestureRecognizer) -> bool {
            self.ivars()
                .with_window(|window| window.begin_back_gesture(recognizer))
                .unwrap_or(false)
        }
    }
);

impl MetalView {
    fn new(frame: CGRect, main_thread: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(main_thread).set_ivars(WindowReference::default());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn handle_touches(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
        for touch in touches {
            self.ivars()
                .with_window(|window| window.handle_touch(&touch, event));
        }
    }
}

pub(crate) struct IosWindow {
    state: Rc<IosWindowState>,
}

impl std::ops::Deref for IosWindow {
    type Target = IosWindowState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

pub(crate) struct IosWindowState {
    platform: Weak<IosPlatformState>,
    keyboard_observers:
        RefCell<Vec<Retained<objc2::runtime::ProtocolObject<dyn NSObjectProtocol>>>>,
    /// The UIWindow object
    window: Retained<UIWindow>,
    /// The UIViewController
    view_controller: Retained<ViewController>,
    /// The Metal-backed UIView
    view: Retained<MetalView>,
    /// The hidden text input view for keyboard input
    text_input_view: Retained<TextInputView>,
    /// Current bounds in pixels
    bounds: Cell<Bounds<Pixels>>,
    /// Scale factor
    scale_factor: Cell<f32>,
    /// Input handler for text input
    input_handler: CallbackSlot<PlatformInputHandler>,
    request_frame_callback: CallbackSlot<Box<dyn FnMut(RequestFrameOptions)>>,
    force_next_frame: Cell<bool>,
    /// Callback for input events
    input_callback: CallbackSlot<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    /// Callback for active status changes
    active_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    visibility: Cell<WindowVisibility>,
    visibility_callback: CallbackSlot<Box<dyn FnMut(WindowVisibility)>>,
    /// Callback for hover status changes (not really applicable on iOS)
    hover_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    /// Callback for resize events
    resize_callback: CallbackSlot<Box<dyn FnMut(Size<Pixels>, f32)>>,
    display_id: Cell<DisplayId>,
    display_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    /// Callback for move events (not applicable on iOS)
    moved_callback: CallbackSlot<Box<dyn FnMut()>>,
    /// Callback for should close
    should_close_callback: CallbackSlot<Box<dyn FnMut() -> bool>>,
    /// Callback for hit test
    hit_test_callback: CallbackSlot<Box<dyn FnMut() -> Option<WindowControlArea>>>,
    /// Callback for close
    close_callback: CallbackSlot<Box<dyn FnOnce()>>,
    /// Callback for appearance changes
    appearance_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    insets_changed_callback: CallbackSlot<Box<dyn FnMut(WindowInsets)>>,
    keyboard_dismiss_touch: Cell<Option<KeyboardDismissTouch>>,
    /// GPUI's ids for the touches UIKit has down (events.rs).
    touch_ids: RefCell<TouchIds>,
    /// Where the back gesture an element claimed started, while it runs.
    back_gesture_start: Cell<Option<Point<Pixels>>>,
    /// The keyboard's bottom inset GPUI was last told, in points: the keyboard animation's current value.
    keyboard_height: Cell<f32>,
    /// The inset's target and animation (keyboard_inset.rs).
    keyboard_inset: RefCell<KeyboardInsetManager>,
    /// Flutter's `keyboardAnimationView`: a hidden view whose `y` UIKit animates with the keyboard's duration and
    /// curve; its spring (or, without one, its presentation layer) gives each frame's inset.
    keyboard_animation_view: Retained<UIView>,
    /// Current mouse position (from touch)
    mouse_position: Cell<Point<Pixels>>,
    /// Current modifiers
    modifiers: Cell<Modifiers>,
    /// The hardware keyboard's keys down and modifier state (keyboard.rs).
    key_responder: RefCell<KeyResponder>,
    /// Fires the held key's repeats while `key_responder` has a key that repeats.
    key_repeat_timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    renderer: Mutex<MetalRenderer>,
    /// The app's views shown beneath GPUI's (native_views.rs).
    native_views: RefCell<native_views::NativeViews>,
    /// Whether UIKit is turning the window (`viewWillTransitionToSize:…` to its completion).
    rotation: Cell<Rotation>,
}

impl IosWindow {
    #[allow(deprecated)] // Window construction can precede scene connection.
    pub fn new(
        _handle: AnyWindowHandle,
        _params: WindowParams,
        platform: &Rc<IosPlatformState>,
    ) -> anyhow::Result<Self> {
        // Create the window on the main screen
        let screen = IosDisplay::main();
        let screen_bounds = screen.bounds();
        let scale_factor = screen.scale();

        let main_thread = main_thread_marker().expect("UIKit requires the main thread");
        // Create UIWindow
        let window_scene = platform.application.window_scene();
        let window_scene = window_scene.as_deref();
        let (window, screen_bounds_cg, scale) = if let Some(scene) = window_scene {
            let screen = scene.screen();
            let bounds = screen.bounds();
            let window = UIWindow::initWithWindowScene(UIWindow::alloc(main_thread), scene);
            window.setFrame(bounds);
            (window, bounds, screen.scale())
        } else {
            let screen = UIScreen::mainScreen(main_thread);
            let bounds = screen.bounds();
            let window = UIWindow::initWithFrame(UIWindow::alloc(main_thread), bounds);
            (window, bounds, screen.scale())
        };

        let metal_frame = CGRect::new(CGPoint::ZERO, screen_bounds_cg.size);
        let view = MetalView::new(metal_frame, main_thread);

        let layer = view.layer();
        layer.setContentsScale(scale);
        let mut renderer = MetalRenderer::from_layer(
            MetalContext::default(),
            layer
                .downcast_ref::<CAMetalLayer>()
                .ok_or_else(|| anyhow::anyhow!("UIKit view must have a CAMetalLayer"))?,
            false,
        )?;
        let pixel_w = (screen_bounds_cg.size.width * scale) as i32;
        let pixel_h = (screen_bounds_cg.size.height * scale) as i32;
        renderer.update_drawable_size(size(DevicePixels(pixel_w), DevicePixels(pixel_h)));

        // Auto-resize the Metal view when the parent view changes size
        // (e.g. rotation). UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight
        view.setAutoresizingMask(
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
        );

        // Enable user interaction on the Metal view for touch handling
        view.setUserInteractionEnabled(true);
        view.setMultipleTouchEnabled(true);

        // UIKit has no public API for the display's corner radius, but since
        // iOS 26 a view filling the window with a container-concentric corner
        // configuration resolves its radii to the display's. This only sets the
        // layer's corner radius; without `masksToBounds` the Metal drawable is
        // not clipped.
        if reports_corner_radii(&view) {
            view.setCornerConfiguration(&UICornerConfiguration::configurationWithRadius(
                &UICornerRadius::containerConcentricRadius(),
            ));
        }

        // Trackpad and mouse-wheel scrolling reaches UIKit apps only through a
        // pan recognizer that accepts scroll types. With no allowed touch types
        // it never claims finger touches, so those keep reaching `touchesBegan`
        // and GPUI's own recognizer.
        let scroll_target: &AnyObject = &view;
        // SAFETY: `MetalView` implements `handleScroll:` and owns the recognizer.
        let scroll_recognizer = unsafe {
            UIPanGestureRecognizer::initWithTarget_action(
                UIPanGestureRecognizer::alloc(main_thread),
                Some(scroll_target),
                Some(sel!(handleScroll:)),
            )
        };
        scroll_recognizer.setAllowedScrollTypesMask(UIScrollTypeMask::All);
        scroll_recognizer.setAllowedTouchTypes(&NSArray::new());
        scroll_recognizer.setCancelsTouchesInView(false);
        view.addGestureRecognizer(&scroll_recognizer);

        // The leading-edge swipe is iOS's back gesture. The delegate lets it
        // begin only when an element claims it; it then cancels the touches it
        // took over, so GPUI's own recognizer abandons any pan it had started.
        let back_target: &AnyObject = &view;
        // SAFETY: `MetalView` implements `handleBackGesture:` and owns the recognizer.
        let back_recognizer = unsafe {
            UIScreenEdgePanGestureRecognizer::initWithTarget_action(
                UIScreenEdgePanGestureRecognizer::alloc(main_thread),
                Some(back_target),
                Some(sel!(handleBackGesture:)),
            )
        };
        back_recognizer.setEdges(
            if view.effectiveUserInterfaceLayoutDirection()
                == UIUserInterfaceLayoutDirection::RightToLeft
            {
                UIRectEdge::Right
            } else {
                UIRectEdge::Left
            },
        );
        back_recognizer.setDelegate(Some(ProtocolObject::from_ref(&*view)));
        view.addGestureRecognizer(&back_recognizer);

        let view_controller = ViewController::new(main_thread);
        // In a host view, where native views go beneath it (rotation.rs).
        view_controller.setView(Some(&rotation::nest(&view, metal_frame, main_thread)));
        window.setRootViewController(Some(&view_controller));

        // Make the window visible
        window.makeKeyAndVisible();

        // Create a transparent text input view for keyboard handling.
        // Uses our custom GPUITextInputView which implements UIKeyInput
        // so iOS actually routes keyboard text to us. It covers the view, where
        // UIKit draws its selection UI; it takes only touches on edited text.
        let text_input_view = TextInputView::new(view.bounds(), main_thread);
        text_input_view.setAutoresizingMask(
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
        );
        text_input_view.setUserInteractionEnabled(true);
        view.addSubview(&text_input_view);

        let keyboard_animation_view = UIView::new(main_thread);
        keyboard_animation_view.setHidden(true);
        view.addSubview(&keyboard_animation_view);

        let state = IosWindowState {
            platform: Rc::downgrade(platform),
            keyboard_observers: RefCell::default(),
            display_id: Cell::new(IosDisplay::from_screen(window.screen()).id()),
            display_changed_callback: CallbackSlot::default(),
            window,
            view_controller,
            view,
            text_input_view,
            bounds: Cell::new(screen_bounds),
            scale_factor: Cell::new(scale_factor),
            input_handler: CallbackSlot::default(),
            request_frame_callback: CallbackSlot::default(),
            force_next_frame: Cell::new(true),
            input_callback: CallbackSlot::default(),
            active_status_callback: CallbackSlot::default(),
            visibility: Cell::new(WindowVisibility::Visible),
            visibility_callback: CallbackSlot::default(),
            hover_status_callback: CallbackSlot::default(),
            resize_callback: CallbackSlot::default(),
            moved_callback: CallbackSlot::default(),
            should_close_callback: CallbackSlot::default(),
            hit_test_callback: CallbackSlot::default(),
            close_callback: CallbackSlot::default(),
            appearance_changed_callback: CallbackSlot::default(),
            insets_changed_callback: CallbackSlot::default(),
            keyboard_dismiss_touch: Cell::new(None),
            touch_ids: RefCell::default(),
            back_gesture_start: Cell::new(None),
            keyboard_height: Cell::new(0.),
            keyboard_inset: RefCell::default(),
            keyboard_animation_view,
            mouse_position: Cell::new(Point::default()),
            modifiers: Cell::new(Modifiers::default()),
            key_responder: RefCell::default(),
            key_repeat_timer: RefCell::default(),
            renderer: Mutex::new(renderer),
            native_views: RefCell::default(),
            rotation: Cell::new(Rotation::Settled),
        };

        Ok(Self {
            state: Rc::new(state),
        })
    }

    pub(crate) fn register(&self) {
        if let Some(platform) = self.platform.upgrade() {
            platform.application.register_window(&self.state);
        }

        let window = Rc::downgrade(&self.state);
        *self.view_controller.ivars().0.borrow_mut() = window.clone();
        *self.view.ivars().0.borrow_mut() = window.clone();
        self.text_input_view.set_window(window);

        self.state.register_keyboard_observers();
    }
}

impl IosWindowState {
    pub(super) fn attach_to_scene(&self, scene: &UIWindowScene) {
        self.window.setWindowScene(Some(scene));
        self.window.makeKeyAndVisible();
        self.handle_layout_change();
    }

    fn register_keyboard_observers(self: &Rc<Self>) {
        let notification_center = NSNotificationCenter::defaultCenter();
        // SAFETY: UIKit's notification names, immutable statics.
        let notifications = unsafe {
            [
                (UIKeyboardWillShowNotification, NotificationName::WillShow),
                (
                    UIKeyboardWillChangeFrameNotification,
                    NotificationName::WillChangeFrame,
                ),
                (UIKeyboardWillHideNotification, NotificationName::WillHide),
            ]
        };
        let observers = notifications.map(|(notification, name)| {
            let window = Rc::downgrade(self);
            let block = block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
                if let Some(window) = window.upgrade() {
                    // SAFETY: NSNotificationCenter passes a live notification.
                    window.handle_keyboard_notification(name, unsafe { notification.as_ref() });
                }
            });
            // SAFETY: the block holds only a weak window; the observer is removed on drop.
            unsafe {
                notification_center.addObserverForName_object_queue_usingBlock(
                    Some(notification),
                    None,
                    None,
                    &block,
                )
            }
        });
        self.keyboard_observers.borrow_mut().extend(observers);
    }

    // Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/framework/Source/
    // KeyboardInsetManager.swift (`handleKeyboardNotification(_:)`, `startKeyBoardAnimation(_:)`,
    // `handleKeyboardAnimationCallback(withTargetTime:)`) and FlutterViewController.mm (`keyboardWillShowNotification:`,
    // `keyboardWillChangeFrame:`, `keyboardWillBeHidden:`, `isPadInSlideOverOrStageManagerMode`,
    // `convertViewRectToScreen:`) @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter
    // Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
    /// Turns a keyboard notification into the inset's target and starts the animation toward it.
    fn handle_keyboard_notification(&self, name: NotificationName, notification: &NSNotification) {
        let user_info = notification.userInfo();
        let value = |key: &NSString| user_info.as_ref().and_then(|info| info.objectForKey(key));
        let rect = |key: &NSString| {
            value(key)
                .and_then(|value| value.downcast::<NSValue>().ok())
                // SAFETY: UIKit stores the keyboard's frames as CGRect values.
                .map_or(Rect::default(), |value| {
                    rect(unsafe { value.CGRectValue() })
                })
        };
        let number =
            |key: &NSString| value(key).and_then(|value| value.downcast::<NSNumber>().ok());
        // SAFETY: UIKit's user-info keys, immutable statics.
        let notification = unsafe {
            KeyboardNotification {
                name,
                begin: rect(UIKeyboardFrameBeginUserInfoKey),
                end: rect(UIKeyboardFrameEndUserInfoKey),
                duration: number(UIKeyboardAnimationDurationUserInfoKey)
                    .map_or(0., |duration| duration.doubleValue()),
                is_local: number(UIKeyboardIsLocalUserInfoKey).map(|local| local.boolValue()),
            }
        };
        let action = self
            .keyboard_inset
            .borrow_mut()
            .handle_keyboard_notification(&notification, &self.keyboard_geometry());
        match action {
            KeyboardInsetAction::Ignore
            | KeyboardInsetAction::Unchanged
            | KeyboardInsetAction::Compound => {}
            KeyboardInsetAction::HideImmediately => {
                self.keyboard_animation_view.layer().removeAllAnimations();
                self.set_keyboard_height(0.);
            }
            KeyboardInsetAction::Start {
                duration,
                generation,
            } => self.start_keyboard_animation(duration, generation),
        }
    }

    /// Where the view sits on its screen, for the keyboard's frame (which is in the screen's coordinate space).
    fn keyboard_geometry(&self) -> ViewGeometry {
        let screen = self.window.screen();
        let coordinate_space = screen.coordinateSpace();
        // SAFETY: UICoordinateSpace's `convertRect:toCoordinateSpace:`, which UIView implements.
        let view_in_screen: CGRect = unsafe {
            msg_send![&*self.view, convertRect: self.view.bounds(), toCoordinateSpace: &*coordinate_space]
        };
        let traits = self.view.traitCollection();
        // SAFETY: plain trait-collection getters on the main thread.
        let pad_in_slide_over_or_stage_manager = unsafe {
            traits.userInterfaceIdiom() == UIUserInterfaceIdiom::Pad
                && traits.horizontalSizeClass() == UIUserInterfaceSizeClass::Compact
                && traits.verticalSizeClass() == UIUserInterfaceSizeClass::Regular
        };
        ViewGeometry {
            screen: rect(screen.bounds()),
            view_in_screen: rect(view_in_screen),
            pad_in_slide_over_or_stage_manager,
        }
    }

    /// Flutter's `startKeyBoardAnimation(_:)`: UIKit animates the hidden view's `y` from the current inset to the
    /// target with the keyboard's duration (and, inside the notification, its curve); its `CASpringAnimation`, if
    /// it is one, becomes the spring `step_keyboard_animation` samples each frame. The completion of the animation
    /// still running sets the target.
    fn start_keyboard_animation(&self, duration: f64, generation: u64) {
        let view = &self.keyboard_animation_view;
        view.layer().removeAllAnimations();
        let frame_at = |inset: f64| CGRect::new(CGPoint::new(0., inset), CGSize::ZERO);
        let original_view_inset_bottom = self.keyboard_height.get() as f64;
        view.setFrame(frame_at(original_view_inset_bottom));
        // Core Animation's clock, the one the display link's target times are on.
        let start_time = CACurrentMediaTime();
        let target = self.keyboard_inset.borrow().target_view_inset_bottom;
        let animations = block2::RcBlock::new({
            let view = view.clone();
            move || {
                view.setFrame(frame_at(target));
                view.layoutIfNeeded();
            }
        });
        let window = self.view.ivars().0.borrow().clone();
        let completion = block2::RcBlock::new(move |_finished: objc2::runtime::Bool| {
            let Some(window) = window.upgrade() else {
                return;
            };
            if window.keyboard_inset.borrow_mut().finish(generation) {
                let target = window.keyboard_inset.borrow().target_view_inset_bottom;
                window.set_keyboard_height(target as f32);
            }
        });
        UIView::animateWithDuration_animations_completion(
            duration,
            &animations,
            Some(&completion),
            view.mtm(),
        );
        // The animation UIKit added for the new frame inside the notification: the keyboard's own curve.
        // SAFETY: reading the layer's animation for a key on the main thread.
        let animation = unsafe { view.layer().animationForKey(&NSString::from_str("position")) };
        let keyboard_spring = animation
            .and_then(|animation| animation.downcast::<CASpringAnimation>().ok())
            .map(|spring| KeyboardSpring {
                stiffness: spring.stiffness(),
                damping: spring.damping(),
                mass: spring.mass(),
                initial_velocity: spring.initialVelocity(),
            });
        self.keyboard_inset
            .borrow_mut()
            .set_up_keyboard_spring_animation_if_needed(
                keyboard_spring,
                original_view_inset_bottom,
                start_time,
            );
    }

    /// Flutter's `handleKeyboardAnimationCallback(withTargetTime:)`: while the animation runs, the inset is the
    /// keyboard's spring at the frame's vsync target time, or without a spring the hidden view's presented `y`.
    fn step_keyboard_animation(&self, target_time: f64) {
        let sample = self.keyboard_inset.borrow().sample(target_time);
        match sample {
            InsetSample::Idle => {}
            InsetSample::Spring(inset) => self.set_keyboard_height(inset as f32),
            InsetSample::PresentationLayer => {
                // SAFETY: reading the presentation copy of a layer on the main thread.
                if let Some(presented) =
                    unsafe { self.keyboard_animation_view.layer().presentationLayer() }
                {
                    self.set_keyboard_height(presented.frame().origin.y as f32);
                }
            }
        }
    }

    /// Delivers a UIKit touch through GPUI's platform-neutral touch API.
    pub fn handle_touch(&self, touch: &UITouch, _event: Option<&UIEvent>) {
        // Each touch carries UIKit's own time stamp. UIKit's historical coalesced
        // samples are not replayed: one sample per touch callback, as Flutter's embedder.
        let touches = self
            .touch_ids
            .borrow_mut()
            .track(touch as *const UITouch as usize, touch_phase(touch));
        let position = touch_location_in_view(touch, &self.view);
        self.mouse_position.set(position);
        for (id, phase) in touches {
            let event = TouchEvent {
                id,
                phase,
                position,
                predicted_position: None,
                force: touch_force(touch),
                device: touch_device(touch),
                time_stamp: process_info::uptime_instant(touch.timestamp()),
            };
            self.handle_keyboard_dismiss_touch(&event);
            self.input_callback
                .with(|callback| callback(PlatformInput::Touch(event)));
        }
    }

    /// Delivers a hardware keyboard press as GPUI key events, returning whether UIKit gets it too (`super`, and so
    /// UIKit's text system). While the focused field composes (marked text), downs are UIKit's alone (keyboard.rs).
    /// The responder is never borrowed while GPUI runs, which may ask for `capslock()`.
    fn handle_press(&self, press: &UIPress) -> bool {
        let Some(key) = press.key(self.view.mtm()) else {
            // Not a keyboard's press (a remote's button): UIKit's.
            return true;
        };
        let composing = self
            .with_input_handler(|handler| handler.marked_text_range())
            .flatten()
            .is_some();
        let characters = key.characters().to_string();
        let characters_ignoring_modifiers = key.charactersIgnoringModifiers().to_string();
        let key_code = key.keyCode().0 as u32;
        let repeating = self.key_responder.borrow().repeating_key();
        let events = self.key_responder.borrow_mut().handle_press(
            &Press {
                phase: press_phase(press.phase()),
                key_code,
                modifier_flags: key.modifierFlags().0 as u32,
                characters: &characters,
                characters_ignoring_modifiers: &characters_ignoring_modifiers,
            },
            composing,
        );
        self.modifiers.set(self.key_responder.borrow().modifiers());
        if let Some(event) = events.modifiers_changed {
            self.input_callback
                .with(|callback| callback(PlatformInput::ModifiersChanged(event)));
        }
        let uikit = match events.primary {
            Primary::Offer(event) => {
                let handled = self
                    .input_callback
                    .with(|callback| !callback(event.clone()).propagate)
                    .unwrap_or(false);
                self.key_responder
                    .borrow_mut()
                    .offered(key_code, &event, handled);
                !handled
            }
            Primary::Send { event, uikit } => {
                if let Some(event) = event {
                    self.input_callback.with(|callback| callback(event));
                }
                uikit
            }
        };
        if self.key_responder.borrow().repeating_key() != repeating {
            self.restart_key_repeat();
        }
        uikit
    }

    /// Stops the held key's repeat timer and, if a key repeats now, starts one for it: `KEY_REPEAT_DELAY`, then a
    /// repeat every `KEY_REPEAT_INTERVAL` until the key's up, another key's down or the window resigning.
    fn restart_key_repeat(&self) {
        if let Some(timer) = self.key_repeat_timer.take() {
            timer.invalidate();
        }
        if self.key_responder.borrow().repeating_key().is_none() {
            return;
        }
        let window = self.view.ivars().0.borrow().clone();
        let block = block2::RcBlock::new(move |_: NonNull<objc2_foundation::NSTimer>| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let repeat = window.key_responder.borrow().repeat();
            if let Some(event) = repeat {
                window.input_callback.with(|callback| callback(event));
            }
        });
        // SAFETY: scheduled on the main thread's run loop, so the block (which holds only a weak window) runs on the
        // main thread it was made on; the timer is invalidated before it is replaced and when the window resigns.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                super::keyboard::KEY_REPEAT_INTERVAL.as_secs_f64(),
                true,
                &block,
            )
        };
        timer.setFireDate(&objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(
            super::keyboard::KEY_REPEAT_DELAY.as_secs_f64(),
        ));
        self.key_repeat_timer.replace(Some(timer));
    }

    /// Delivers one step of a trackpad or mouse-wheel scroll gesture.
    fn handle_scroll(&self, recognizer: &UIPanGestureRecognizer) {
        let Some(event) = scroll_event(recognizer, &self.view) else {
            return;
        };
        self.mouse_position.set(event.position);
        self.modifiers.set(event.modifiers);
        self.input_callback
            .with(|callback| callback(PlatformInput::Scroll(event)));
    }

    /// Offers a starting back gesture to GPUI, returning whether an element
    /// claimed it.
    fn begin_back_gesture(&self, recognizer: &UIGestureRecognizer) -> bool {
        let location = recognizer.locationInView(Some(&self.view));
        let start_position = Point::new(px(location.x as f32), px(location.y as f32));
        let claimed = self
            .input_callback
            .with(|callback| {
                callback(PlatformInput::BackGesture(BackGestureEvent {
                    phase: BackGesturePhase::Started,
                    start_position,
                    progress: 0.,
                    source: BackGestureSource::EdgeSwipe,
                }))
            })
            .is_some_and(|result| result.default_prevented);
        self.back_gesture_start
            .set(claimed.then_some(start_position));
        claimed
    }

    /// Delivers a step of a claimed back gesture.
    fn handle_back_gesture(&self, recognizer: &UIScreenEdgePanGestureRecognizer) {
        let Some(start_position) = self.back_gesture_start.get() else {
            return;
        };
        let Some(event) = back_event(recognizer, &self.view, start_position) else {
            return;
        };
        if matches!(
            event.phase,
            BackGesturePhase::Committed | BackGesturePhase::Cancelled
        ) {
            self.back_gesture_start.set(None);
        }
        self.input_callback
            .with(|callback| callback(PlatformInput::BackGesture(event)));
    }

    fn handle_keyboard_dismiss_touch(&self, event: &TouchEvent) {
        if self.keyboard_height.get() <= 0. {
            self.keyboard_dismiss_touch.set(None);
            return;
        }

        match event.phase {
            TouchPhase::Started if self.keyboard_dismiss_touch.get().is_none() => {
                self.keyboard_dismiss_touch.set(Some(KeyboardDismissTouch {
                    id: event.id,
                    start_position: event.position,
                }));
            }
            TouchPhase::Moved => {
                let Some(touch) = self
                    .keyboard_dismiss_touch
                    .get()
                    .filter(|touch| touch.id == event.id)
                else {
                    return;
                };
                let delta = event.position - touch.start_position;
                if delta.y >= KEYBOARD_DISMISS_DISTANCE && delta.y.abs() > delta.x.abs() * 1.25 {
                    self.keyboard_dismiss_touch.set(None);
                    self.hide_keyboard();
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self
                    .keyboard_dismiss_touch
                    .get()
                    .is_some_and(|touch| touch.id == event.id)
                {
                    self.keyboard_dismiss_touch.set(None);
                }
            }
            TouchPhase::Started => {}
        }
    }

    /// One display-link frame; `target_time` is its vsync target (`CADisplayLink.targetTimestamp`).
    pub(super) fn request_frame(&self, target_time: f64) {
        self.step_keyboard_animation(target_time);
        self.request_frame_callback.with(|callback| {
            let force_render = self.force_next_frame.replace(false);
            callback(RequestFrameOptions {
                force_render,
                ..Default::default()
            });
        });
    }

    /// Query the safe area insets from the UIView.
    ///
    /// Returns `(top, bottom, left, right)` in logical points.
    /// These represent the areas occupied by system UI (status bar,
    /// home indicator, camera notch) that content should avoid.
    fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        let insets = self.view.safeAreaInsets();
        (
            insets.top as f32,
            insets.bottom as f32,
            insets.left as f32,
            insets.right as f32,
        )
    }

    fn current_insets(&self) -> WindowInsets {
        let (top, bottom, left, right) = self.safe_area_insets();
        WindowInsets {
            safe_area: Edges {
                top: px(top),
                right: px(right),
                bottom: px(bottom),
                left: px(left),
            },
            ime: Edges {
                bottom: px(self.keyboard_height.get()),
                ..Default::default()
            },
            corner_radii: self.display_corner_radii(),
        }
    }

    fn display_corner_radii(&self) -> Corners<Pixels> {
        if !reports_corner_radii(&self.view) {
            return Corners::default();
        }
        let radius = |corner| px(self.view.effectiveRadiusForCorner(corner) as f32);
        Corners {
            top_left: radius(UIRectCorner::TopLeft),
            top_right: radius(UIRectCorner::TopRight),
            bottom_right: radius(UIRectCorner::BottomRight),
            bottom_left: radius(UIRectCorner::BottomLeft),
        }
    }

    fn notify_insets_changed(&self) {
        self.insets_changed_callback
            .with(|callback| callback(self.current_insets()));
    }

    fn notify_appearance_changed(&self) {
        self.appearance_changed_callback.with(|callback| callback());
    }

    fn set_keyboard_height(&self, height: f32) {
        let height = height.max(0.);
        if (self.keyboard_height.get() - height).abs() <= 0.5 {
            return;
        }
        self.keyboard_height.set(height);
        if height <= 0. {
            self.keyboard_dismiss_touch.set(None);
        }
        self.notify_insets_changed();
    }

    pub fn show_keyboard(&self) {
        self.text_input_view.set_keyboard_visible(true);
    }

    pub fn hide_keyboard(&self) {
        self.text_input_view.set_keyboard_visible(false);
    }

    pub(super) fn with_input_handler<R>(
        &self,
        callback: impl FnOnce(&mut PlatformInputHandler) -> R,
    ) -> Option<R> {
        self.input_handler.with(callback)
    }

    /// Notify the window when its UIKit scene becomes active or inactive.
    pub fn notify_active_status_change(&self, is_active: bool) {
        log::info!("GPUI iOS: Window active status changed to: {}", is_active);

        if !is_active && self.key_responder.borrow().repeating_key().is_some() {
            // A held key's up may never come once the window resigns (app switcher, backgrounding).
            self.key_responder.borrow_mut().stop_repeat();
            self.restart_key_repeat();
        }
        self.active_status_callback
            .with(|callback| callback(is_active));
    }

    pub(crate) fn notify_visibility_change(&self, visibility: WindowVisibility) {
        if self.visibility.replace(visibility) != visibility {
            self.visibility_callback
                .with(|callback| callback(visibility));
        }
    }

    pub fn handle_layout_change(&self) {
        self.follow_root();
        let view_bounds = self.view.bounds();
        let display = IosDisplay::from_screen(self.window.screen());
        if self.display_id.replace(display.id()) != display.id() {
            self.display_changed_callback.with(|callback| callback());
        }
        let scale = display.scale() as f64;
        self.update_hole_inset();

        let width = view_bounds.size.width as f32;
        let height = view_bounds.size.height as f32;
        let new_scale = scale as f32;

        let old_bounds = self.bounds.get();
        let old_scale = self.scale_factor.get();

        let new_size = size(px(width), px(height));
        self.notify_insets_changed();

        if old_bounds.size == new_size && (old_scale - new_scale).abs() < 0.01 {
            return;
        }

        log::info!(
            "GPUI iOS: Layout changed — {:?} @{:.1}x → {:?} @{:.1}x",
            old_bounds.size,
            old_scale,
            new_size,
            new_scale,
        );

        // Update stored bounds (in logical pixels, matching GPUI convention).
        let new_bounds = Bounds {
            origin: Default::default(),
            size: new_size,
        };
        self.bounds.set(new_bounds);
        self.scale_factor.set(new_scale);
        // Update the Metal layer's contentsScale so the drawable has the
        // correct pixel dimensions.
        self.view.layer().setContentsScale(scale);

        let pixel_width = (width * new_scale) as i32;
        let pixel_height = (height * new_scale) as i32;
        self.renderer
            .lock()
            .update_drawable_size(size(DevicePixels(pixel_width), DevicePixels(pixel_height)));

        self.resize_callback
            .with(|callback| callback(new_size, new_scale));
        self.draw_resized_frame();
    }
}

/// A `CGRect` as keyboard_inset.rs's.
fn rect(rect: CGRect) -> Rect {
    Rect::new(
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    )
}

/// A `UIPress`'s phase as keyboard.rs's.
fn press_phase(phase: UIPressPhase) -> PressPhase {
    match phase {
        UIPressPhase::Began => PressPhase::Began,
        UIPressPhase::Changed => PressPhase::Changed,
        UIPressPhase::Ended => PressPhase::Ended,
        UIPressPhase::Cancelled => PressPhase::Cancelled,
        // objc2 makes `UIPressPhase` an open newtype, so a match needs this arm; `Stationary` and any phase UIKit
        // adds later are UIKit's.
        _ => PressPhase::Stationary,
    }
}

/// Whether UIKit can resolve corner configurations, which arrived in iOS 26.
fn reports_corner_radii(view: &UIView) -> bool {
    view.respondsToSelector(sel!(effectiveRadiusForCorner:))
}

impl Drop for IosWindow {
    fn drop(&mut self) {
        if let Some(platform) = self.platform.upgrade() {
            platform.application.unregister_window(&self.state);
        }
        let observers = std::mem::take(&mut *self.keyboard_observers.borrow_mut());
        for observer in observers {
            unsafe { NSNotificationCenter::defaultCenter().removeObserver((&*observer).as_ref()) };
        }

        *self.view_controller.ivars().0.borrow_mut() = Weak::new();
        *self.view.ivars().0.borrow_mut() = Weak::new();
        self.text_input_view.set_window(Weak::new());
        self.text_input_view.removeFromSuperview();
    }
}

impl HasWindowHandle for IosWindow {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        let view = NonNull::new(Retained::as_ptr(&self.view).cast_mut().cast::<c_void>())
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        let handle = UiKitWindowHandle::new(view);
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl HasDisplayHandle for IosWindow {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
    {
        let handle = UiKitDisplayHandle::new();
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(handle.into()) })
    }
}

impl PlatformWindow for IosWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds.get()
    }

    fn is_maximized(&self) -> bool {
        true // iOS windows are always "maximized"
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds.get())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.bounds.get().size
    }

    fn resize(&mut self, _size: Size<Pixels>) {
        // iOS windows cannot be resized programmatically
    }

    fn scale_factor(&self) -> f32 {
        self.scale_factor.get()
    }

    fn appearance(&self) -> WindowAppearance {
        match unsafe { self.view.traitCollection().userInterfaceStyle() } {
            UIUserInterfaceStyle::Dark => WindowAppearance::Dark,
            _ => WindowAppearance::Light,
        }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(IosDisplay::from_screen(self.window.screen())))
    }

    fn on_display_changed(&self, callback: Box<dyn FnMut()>) {
        self.display_changed_callback.set(callback);
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.mouse_position.get()
    }

    fn modifiers(&self) -> Modifiers {
        self.modifiers.get()
    }

    fn capslock(&self) -> Capslock {
        self.key_responder.borrow().capslock()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.text_input_view
            .set_input_field(input_handler.focus_id());
        self.input_handler.set(input_handler);
        self.text_input_view.refresh_keyboard();
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        let handler = self.input_handler.take();
        self.text_input_view.refresh_keyboard();
        handler
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {
        self.window.makeKeyAndVisible();
    }

    fn is_active(&self) -> bool {
        self.window.isKeyWindow()
    }

    fn is_hovered(&self) -> bool {
        // Hover isn't really applicable on iOS
        false
    }

    fn set_title(&mut self, _title: &str) {
        // iOS apps don't have window titles
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {
        // Could adjust view background color
    }

    fn minimize(&self) {
        // iOS apps cannot be minimized
    }

    fn zoom(&self) {
        // iOS apps cannot be zoomed
    }

    fn toggle_fullscreen(&self) {
        // iOS apps are always fullscreen
    }

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.request_frame_callback.set(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.input_callback.set(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.active_status_callback.set(callback);
    }

    fn visibility(&self) -> WindowVisibility {
        self.visibility.get()
    }

    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.visibility_callback.set(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.hover_status_callback.set(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.resize_callback.set(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.moved_callback.set(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.should_close_callback.set(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.hit_test_callback.set(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.close_callback.set(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.appearance_changed_callback.set(callback);
    }

    fn draw(&self, scene: &Scene) {
        native_views::draw(self, scene);
    }

    unsafe fn attach_native_view(
        &self,
        id: gpui::NativeViewId,
        view: NonNull<c_void>,
    ) -> Option<Box<dyn std::any::Any>> {
        // SAFETY: The caller upholds `attach`'s requirements for `view`, and
        // GPUI's windows run on the main thread.
        unsafe { native_views::attach(&self.state, id, view) }
    }

    fn native_view_input(&self) -> gpui::NativeViewInput {
        gpui::NativeViewInput::Window
    }

    fn set_focused_native_view(&self, id: Option<gpui::NativeViewId>) {
        self.text_input_view
            .move_keyboard_input(KeyboardEvent::Focused(id));
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.renderer.lock().sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        None
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // iOS handles IME positioning automatically
    }

    fn insets(&self) -> WindowInsets {
        self.current_insets()
    }

    fn on_insets_changed(&self, callback: Box<dyn FnMut(WindowInsets)>) {
        self.insets_changed_callback.set(callback);
    }

    fn show_soft_keyboard(&self) {
        self.show_keyboard();
    }

    fn hide_soft_keyboard(&self) {
        self.hide_keyboard();
    }

    // Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/
    // framework/Source/FlutterPlatformPlugin.mm (`vibrateHapticFeedback:`) @ 3.50.1
    // (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter
    // Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
    // A fresh generator per call, as Flutter's; the match is exhaustive, one arm
    // per Flutter branch.
    #[allow(
        deprecated,
        reason = "Flutter's initWithStyle:, deprecated by iOS 17.5's feedbackGeneratorWithStyle:forView:"
    )]
    fn perform_haptic(&self, haptic: gpui::Haptic) {
        use gpui::Haptic;
        use objc2_ui_kit::{
            UIImpactFeedbackGenerator, UIImpactFeedbackStyle, UINotificationFeedbackGenerator,
            UINotificationFeedbackType, UISelectionFeedbackGenerator,
        };
        let main_thread = self.view.mtm();
        let impact = |style| {
            UIImpactFeedbackGenerator::initWithStyle(
                UIImpactFeedbackGenerator::alloc(main_thread),
                style,
            )
            .impactOccurred()
        };
        let notification =
            |kind| UINotificationFeedbackGenerator::new(main_thread).notificationOccurred(kind);
        match haptic {
            // SAFETY: a C call taking a system sound id; it has no other inputs.
            Haptic::Vibrate => unsafe { AudioServicesPlaySystemSound(K_SYSTEM_SOUND_ID_VIBRATE) },
            Haptic::LightImpact => impact(UIImpactFeedbackStyle::Light),
            Haptic::MediumImpact => impact(UIImpactFeedbackStyle::Medium),
            Haptic::HeavyImpact => impact(UIImpactFeedbackStyle::Heavy),
            Haptic::SelectionClick => {
                UISelectionFeedbackGenerator::new(main_thread).selectionChanged()
            }
            Haptic::SuccessNotification => notification(UINotificationFeedbackType::Success),
            Haptic::WarningNotification => notification(UINotificationFeedbackType::Warning),
            Haptic::ErrorNotification => notification(UINotificationFeedbackType::Error),
        }
    }

    fn text_input_state_changed(&self, change: TextInputStateChange) {
        self.text_input_view.state_changed(change);
    }

    fn set_text_input_configuration(&mut self, configuration: TextInputConfiguration) {
        self.text_input_view.set_configuration(&configuration);
    }
}

#[cfg(test)]
mod tests {
    use super::CallbackSlot;

    #[test]
    fn callback_is_unavailable_during_dispatch_and_restored_afterward() {
        let slot = CallbackSlot::default();
        slot.set(1);
        assert_eq!(
            slot.with(|value| {
                *value += 1;
                assert!(
                    slot.with(|_| panic!("reentered the same handler"))
                        .is_none()
                );
                *value
            }),
            Some(2)
        );
        assert_eq!(slot.with(|value| *value), Some(2));
    }

    #[test]
    fn callback_replacement_during_dispatch_is_preserved() {
        let slot = CallbackSlot::default();
        slot.set(1);
        slot.with(|_| {
            slot.set(2);
            slot.with(|value| *value += 1);
        });
        assert_eq!(slot.take(), Some(3));
    }

    #[test]
    fn withdrawing_an_in_flight_handler_does_not_restore_it() {
        let slot = CallbackSlot::default();
        slot.set(1);
        slot.with(|_| assert!(slot.take().is_none()));
        assert!(
            slot.with(|_| panic!("restored a withdrawn handler"))
                .is_none()
        );
    }

    #[test]
    fn removing_a_replacement_does_not_resurrect_the_previous_handler() {
        let slot = CallbackSlot::default();
        slot.set(1);
        slot.with(|_| {
            slot.set(2);
            assert_eq!(slot.take(), Some(2));
        });
        assert!(slot.take().is_none());
    }
}
