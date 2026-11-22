// Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/framework/Source/KeyboardInsetManager.swift
// and the keyboard test cases of FlutterViewControllerTest.mm @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//! The keyboard's bottom inset, decided on plain values (no UIKit), so the host can test it.
//!
//! Flutter's `FlutterKeyboardInsetManager`: a keyboard notification's end frame becomes the part of the docked
//! keyboard that covers the view (floating, undocked and split keyboards cover nothing), in the screen's coordinate
//! space, with Slide Over's offset; then an animation toward it, which the window samples each frame: on the
//! keyboard's own spring curve when UIKit animates it with a `CASpringAnimation` (spring_animation.rs), else from the
//! hidden view's presentation layer. Lengths are points (Flutter's are physical pixels: it multiplies by the screen's
//! scale, GPUI's insets are logical).

use super::spring_animation::SpringAnimation;

/// A `CGRect` as plain values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn max_y(&self) -> f64 {
        self.y + self.height
    }

    /// `CGRect.isEmpty`: no width or no height.
    fn is_empty(&self) -> bool {
        self.width <= 0. || self.height <= 0.
    }

    /// `CGRect.intersection`, with no size where they don't meet (UIKit's null rect has none).
    fn intersection(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = (self.x + self.width).min(other.x + other.width) - x;
        let height = self.max_y().min(other.max_y()) - y;
        if width < 0. || height < 0. {
            return Rect::default();
        }
        Rect::new(x, y, width, height)
    }
}

/// Which keyboard notification UIKit sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationName {
    WillShow,
    WillChangeFrame,
    WillHide,
}

/// A keyboard notification's `userInfo`, in the screen's coordinate space as UIKit gives it.
#[derive(Clone, Copy, Debug)]
pub struct KeyboardNotification {
    pub name: NotificationName,
    /// `UIKeyboardFrameBeginUserInfoKey` (zero when absent).
    pub begin: Rect,
    /// `UIKeyboardFrameEndUserInfoKey` (zero when absent).
    pub end: Rect,
    /// `UIKeyboardAnimationDurationUserInfoKey`, seconds (0 when absent).
    pub duration: f64,
    /// `UIKeyboardIsLocalUserInfoKey`: false for another app's keyboard (iPad Split View); absent counts as local.
    pub is_local: Option<bool>,
}

/// Where the view is: the answers of Flutter's `FlutterKeyboardInsetManagerDelegate`.
#[derive(Clone, Copy, Debug)]
pub struct ViewGeometry {
    /// The screen's bounds.
    pub screen: Rect,
    /// The view's bounds in the screen's coordinate space (`convertViewRectToScreen:`).
    pub view_in_screen: Rect,
    /// `isPadInSlideOverOrStageManagerMode`: an iPad view of compact width and regular height.
    pub pad_in_slide_over_or_stage_manager: bool,
}

/// Flutter's `FlutterKeyboardMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardMode {
    Hidden,
    Docked,
    Floating,
}

/// The animation toward the target inset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyboardAnimation {
    Idle,
    /// Flutter's `keyboardAnimationIsShowing` while its vsync client lives; `generation` names the animation, so a
    /// completion of one that was replaced is ignored (Flutter compares vsync clients). `spring` is Flutter's
    /// `keyboardSpringAnimation` with its `keyboardAnimationStartTime` (`CACurrentMediaTime`, seconds), set once
    /// UIKit animates with a spring; `None`: the presentation layer is sampled.
    Running {
        showing: bool,
        generation: u64,
        spring: Option<(SpringAnimation, f64)>,
    },
}

/// A `CASpringAnimation`'s parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyboardSpring {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
    pub initial_velocity: f64,
}

/// Where the inset is on a frame (Flutter's `handleKeyboardAnimationCallback(withTargetTime:)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InsetSample {
    /// No animation runs: the inset stays.
    Idle,
    /// The spring's value at the frame's target time.
    Spring(f64),
    /// No spring: the hidden view's presented `y`.
    PresentationLayer,
}

/// What the window does for a notification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyboardInsetAction {
    /// Another app's keyboard, or a dragged one: nothing.
    Ignore,
    /// The inset is already the target: nothing.
    Unchanged,
    /// Cancel any animation and set the inset to 0 now (Flutter's `hideKeyboardImmediately`).
    HideImmediately,
    /// Animate from the current inset to `target_view_inset_bottom` over `duration` seconds, as animation `generation`.
    Start { duration: f64, generation: u64 },
    /// The running animation goes the same way: it continues, and its completion lands on the new target.
    Compound,
}

/// Flutter's `FlutterKeyboardInsetManager` state.
#[derive(Debug)]
pub struct KeyboardInsetManager {
    pub target_view_inset_bottom: f64,
    pub animation: KeyboardAnimation,
    next_generation: u64,
}

impl Default for KeyboardInsetManager {
    fn default() -> Self {
        Self {
            target_view_inset_bottom: 0.,
            animation: KeyboardAnimation::Idle,
            next_generation: 0,
        }
    }
}

impl KeyboardInsetManager {
    /// Flutter's `handleKeyboardNotification(_:)`. See https://flutter.dev/go/ios-keyboard-calculating-inset.
    pub fn handle_keyboard_notification(
        &mut self,
        notification: &KeyboardNotification,
        geometry: &ViewGeometry,
    ) -> KeyboardInsetAction {
        if should_ignore_keyboard_notification(notification) {
            return KeyboardInsetAction::Ignore;
        }
        let keyboard_mode = calculate_keyboard_attach_mode(notification, geometry);
        let calculated_inset = calculate_keyboard_inset(&notification.end, keyboard_mode, geometry);

        // If the software keyboard is displayed before displaying the PasswordManager prompt,
        // UIKeyboardWillHideNotification will occur immediately after UIKeyboardWillShowNotification.
        // The duration of the animation will be 0.0, and the calculated inset will be 0.0. In this
        // case, it is necessary to cancel the animation and hide the keyboard immediately.
        // https://github.com/flutter/flutter/pull/164884
        if keyboard_mode == KeyboardMode::Hidden
            && calculated_inset == 0.
            && notification.duration == 0.
        {
            self.hide_keyboard_immediately();
            return KeyboardInsetAction::HideImmediately;
        }

        // Avoid double triggering startKeyBoardAnimation.
        if self.target_view_inset_bottom == calculated_inset {
            return KeyboardInsetAction::Unchanged;
        }
        self.target_view_inset_bottom = calculated_inset;

        // Animation calls made while the keyboard animates the same way let the current animation continue with
        // the new target instead of starting a new one.
        let keyboard_will_show = notification.begin.y > notification.end.y;
        if let KeyboardAnimation::Running {
            showing, spring, ..
        } = &mut self.animation
            && *showing == keyboard_will_show
        {
            if let Some((spring, _)) = spring {
                spring.to_value = self.target_view_inset_bottom;
            }
            return KeyboardInsetAction::Compound;
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.animation = KeyboardAnimation::Running {
            showing: keyboard_will_show,
            generation,
            spring: None,
        };
        KeyboardInsetAction::Start {
            duration: notification.duration,
            generation,
        }
    }

    /// Flutter's `setUpKeyboardSpringAnimationIfNeeded(_:)`, once UIKit animates the running animation: a spring
    /// (`keyboard_spring`, the hidden view's `CASpringAnimation`) from `original_view_inset_bottom` to the target,
    /// started at `start_time`; `None` (not a spring) samples the presentation layer.
    pub fn set_up_keyboard_spring_animation_if_needed(
        &mut self,
        keyboard_spring: Option<KeyboardSpring>,
        original_view_inset_bottom: f64,
        start_time: f64,
    ) {
        let target = self.target_view_inset_bottom;
        if let KeyboardAnimation::Running { spring, .. } = &mut self.animation {
            *spring = keyboard_spring.map(|keyboard| {
                let animation = SpringAnimation::new(
                    keyboard.stiffness,
                    keyboard.damping,
                    keyboard.mass,
                    keyboard.initial_velocity,
                    original_view_inset_bottom,
                    target,
                );
                (animation, start_time)
            });
        }
    }

    /// The inset on a frame whose vsync target is `target_time` (`CACurrentMediaTime`'s clock).
    pub fn sample(&self, target_time: f64) -> InsetSample {
        match self.animation {
            KeyboardAnimation::Idle => InsetSample::Idle,
            KeyboardAnimation::Running {
                spring: Some((spring, start_time)),
                ..
            } => InsetSample::Spring(spring.curve_function(target_time - start_time)),
            KeyboardAnimation::Running { spring: None, .. } => InsetSample::PresentationLayer,
        }
    }

    /// Flutter's animation completion: when `generation` is still the running animation, it is over and the inset
    /// is the target (`ensureViewportMetricsIsCorrect`). Returns whether it was.
    pub fn finish(&mut self, generation: u64) -> bool {
        let current = matches!(
            self.animation,
            KeyboardAnimation::Running { generation: running, .. } if running == generation
        );
        if current {
            self.animation = KeyboardAnimation::Idle;
        }
        current
    }

    /// Flutter's `hideKeyboardImmediately`.
    pub fn hide_keyboard_immediately(&mut self) {
        self.animation = KeyboardAnimation::Idle;
        self.target_view_inset_bottom = 0.;
    }
}

/// Flutter's `shouldIgnoreKeyboardNotification(_:)`. Its other-view-controller check is not needed: each GPUI window
/// observes for its own view.
pub fn should_ignore_keyboard_notification(notification: &KeyboardNotification) -> bool {
    // Don't ignore UIKeyboardWillHideNotification notifications.
    // Even if the notification is triggered in the background or by a different app/view
    // controller, we want to always handle this notification to avoid inaccurate inset when in a
    // multitasking mode or when switching between apps.
    if notification.name == NotificationName::WillHide {
        return false;
    }
    // Ignore notification when keyboard's dimensions and position are all zeroes for
    // UIKeyboardWillChangeFrameNotification. This happens when keyboard is dragged. Do not ignore
    // if the notification is UIKeyboardWillShowNotification, as CGRectZero for that notification
    // only occurs when Minimized/Expanded Shortcuts Bar is dropped after dragging, which we later
    // use to categorize it as floating.
    if notification.name == NotificationName::WillChangeFrame && notification.end == Rect::default()
    {
        return true;
    }
    // When keyboard's height or width is set to 0, don't ignore. This does not happen often but can
    // happen sometimes when switching between multitasking modes.
    if notification.end.is_empty() {
        return false;
    }
    // Keyboard notifications related to other apps (e.g. in split view mode on iPad).
    notification.is_local == Some(false)
}

/// Flutter's `calculateKeyboardAttachMode(_:)`.
///
/// Docked mode includes docked, split docked, expanded shortcuts bar (when opening via click), and minimized
/// shortcuts bar (when opened via click). Floating includes undocked, split, floating, expanded shortcuts bar (when
/// dragged and dropped), and minimized shortcuts bar (when dragged and dropped).
pub fn calculate_keyboard_attach_mode(
    notification: &KeyboardNotification,
    geometry: &ViewGeometry,
) -> KeyboardMode {
    if notification.name == NotificationName::WillHide {
        return KeyboardMode::Hidden;
    }
    let keyboard_frame = notification.end;
    // If keyboard's dimensions and position are all zeroes, that means it's a Minimized/Expanded
    // Shortcuts Bar that has been dropped after dragging, which we categorize as floating.
    if keyboard_frame == Rect::default() {
        return KeyboardMode::Floating;
    }
    // If keyboard's width or height are 0, it's hidden.
    if keyboard_frame.is_empty() {
        return KeyboardMode::Hidden;
    }
    let screen_rect = geometry.screen;
    let mut adjusted_keyboard_frame = keyboard_frame;
    adjusted_keyboard_frame.y +=
        calculate_multitasking_adjustment(&screen_rect, &keyboard_frame, geometry);

    // If the keyboard is partially or fully showing within the screen, it's either docked or
    // floating. Sometimes with custom keyboard extensions, the keyboard's position may be off by a
    // small decimal amount (which is why CGRectIntersectRect can't be used). Round to compare.
    let intersection = adjusted_keyboard_frame.intersection(&screen_rect);
    if intersection.height.round() > 0. && intersection.width > 0. {
        if adjusted_keyboard_frame.max_y().round() < screen_rect.height {
            return KeyboardMode::Floating;
        }
        return KeyboardMode::Docked;
    }
    KeyboardMode::Hidden
}

/// Flutter's `calculateMultitaskingAdjustment(_:keyboardFrame:)`.
pub fn calculate_multitasking_adjustment(
    screen_rect: &Rect,
    keyboard_frame: &Rect,
    geometry: &ViewGeometry,
) -> f64 {
    // In Slide Over mode, the keyboard's frame does not include the space below the app, even
    // though the keyboard may be at the bottom of the screen. To handle, shift the Y origin by the
    // amount of space below the app.
    if !geometry.pad_in_slide_over_or_stage_manager {
        return 0.;
    }
    // Stage Manager mode will also meet the above parameters, but it does not handle the keyboard
    // positioning the same way, so skip if keyboard is at bottom of page.
    if screen_rect.height == keyboard_frame.max_y() {
        return 0.;
    }
    let offset = screen_rect.height - geometry.view_in_screen.max_y();
    offset.max(0.)
}

/// Flutter's `calculateKeyboardInset(_:keyboardMode:)`, in points: the height of the docked keyboard's part that
/// covers the view.
pub fn calculate_keyboard_inset(
    keyboard_frame: &Rect,
    keyboard_mode: KeyboardMode,
    geometry: &ViewGeometry,
) -> f64 {
    // Only docked keyboards will have an inset.
    if keyboard_mode != KeyboardMode::Docked {
        return 0.;
    }
    keyboard_frame.intersection(&geometry.view_in_screen).height
}

#[cfg(test)]
mod tests {
    use super::*;
    use NotificationName::*;

    // FlutterViewControllerTest's mock screen (iPhone 14 pixels, scale 1).
    const W: f64 = 1170.;
    const H: f64 = 2532.;
    const SCREEN: Rect = Rect::new(0., 0., W, H);
    const FULL: ViewGeometry = ViewGeometry {
        screen: SCREEN,
        view_in_screen: SCREEN,
        pad_in_slide_over_or_stage_manager: false,
    };

    fn notification(name: NotificationName, end: Rect, is_local: bool) -> KeyboardNotification {
        KeyboardNotification {
            name,
            begin: Rect::default(),
            end,
            duration: 0.25,
            is_local: Some(is_local),
        }
    }

    #[test]
    fn calculate_keyboard_attach_mode_rows() {
        // testCalculateKeyboardAttachMode: (case, notification, end frame, mode).
        let long_decimal_height = 320.666666666666666;
        let rows = [
            (
                "hide notification",
                WillHide,
                Rect::default(),
                KeyboardMode::Hidden,
            ),
            (
                "all zeros",
                WillChangeFrame,
                Rect::default(),
                KeyboardMode::Floating,
            ),
            (
                "0 height",
                WillChangeFrame,
                Rect::new(0., 0., W, 0.),
                KeyboardMode::Hidden,
            ),
            (
                "floating",
                WillChangeFrame,
                Rect::new(0., 0., 320., 320.),
                KeyboardMode::Floating,
            ),
            (
                "undocked",
                WillChangeFrame,
                Rect::new(0., 0., W, 320.),
                KeyboardMode::Floating,
            ),
            (
                "docked",
                WillChangeFrame,
                Rect::new(0., H - 320., W, 320.),
                KeyboardMode::Docked,
            ),
            (
                "docked - rounded values",
                WillChangeFrame,
                Rect::new(0., H - long_decimal_height, W, long_decimal_height),
                KeyboardMode::Docked,
            ),
            (
                "hidden - rounded values",
                WillChangeFrame,
                Rect::new(0., H - 0.0000001, W, long_decimal_height),
                KeyboardMode::Hidden,
            ),
            (
                "hidden",
                WillChangeFrame,
                Rect::new(0., H, W, 320.),
                KeyboardMode::Hidden,
            ),
        ];
        for (name, notification_name, end, mode) in rows {
            let got =
                calculate_keyboard_attach_mode(&notification(notification_name, end, true), &FULL);
            assert_eq!(got, mode, "{name}");
        }
    }

    #[test]
    fn should_ignore_keyboard_notification_rows() {
        // testShouldIgnoreKeyboardNotification: (case, notification, end frame, is local, ignored).
        let valid = Rect::new(0., H - 320., W, 320.);
        let rows = [
            (
                "Hide notification, valid keyboard",
                WillHide,
                valid,
                false,
                false,
            ),
            (
                "All zero keyboard",
                WillChangeFrame,
                Rect::default(),
                true,
                true,
            ),
            (
                "Zero height keyboard",
                WillChangeFrame,
                Rect::new(0., 0., W, 0.),
                false,
                false,
            ),
            (
                "Valid keyboard, triggered from another app",
                WillChangeFrame,
                valid,
                false,
                true,
            ),
            ("Valid keyboard", WillChangeFrame, valid, true, false),
        ];
        for (name, notification_name, end, is_local, ignored) in rows {
            let got = should_ignore_keyboard_notification(&notification(
                notification_name,
                end,
                is_local,
            ));
            assert_eq!(got, ignored, "{name}");
        }
    }

    #[test]
    fn multitasking_adjustment_and_inset() {
        // testCalculateMultitaskingAdjustment and testCalculateKeyboardInset: a view 20 points short of the
        // screen's bottom, a keyboard 20 points above it.
        let view = Rect::new(0., 0., 320., H - 20.);
        let keyboard = Rect::new(20., H - 320., W, 300.);
        let slide_over = ViewGeometry {
            screen: SCREEN,
            view_in_screen: view,
            pad_in_slide_over_or_stage_manager: true,
        };
        assert_eq!(
            calculate_multitasking_adjustment(&SCREEN, &keyboard, &slide_over),
            20.
        );
        let geometry = ViewGeometry {
            pad_in_slide_over_or_stage_manager: false,
            ..slide_over
        };
        assert_eq!(
            calculate_keyboard_inset(&keyboard, KeyboardMode::Docked, &geometry),
            300.
        );
        assert_eq!(
            calculate_keyboard_inset(&keyboard, KeyboardMode::Floating, &geometry),
            0.
        );
    }

    #[test]
    fn handle_keyboard_notification_rows() {
        // One row per Flutter test: a starting target inset, then notifications (name, begin, end, duration) with
        // the action each gives and the target after it.
        type Step = (
            (NotificationName, Rect, Rect, f64),
            KeyboardInsetAction,
            f64,
        );
        let docked = Rect::new(0., H - 320., W, 320.);
        let start = |duration, generation| KeyboardInsetAction::Start {
            duration,
            generation,
        };
        let rows: &[(&str, f64, &[Step])] = &[
            (
                "testHandleKeyboardNotification",
                0.,
                &[(
                    (WillShow, Rect::default(), docked, 0.25),
                    start(0.25, 0),
                    320.,
                )],
            ),
            (
                "testEnsureBottomInsetIsZeroWhenKeyboardDismissed",
                10.,
                &[(
                    (WillHide, Rect::default(), Rect::default(), 0.25),
                    start(0.25, 0),
                    0.,
                )],
            ),
            (
                "testStopKeyBoardAnimationWhenReceivedWillHideNotificationAfterWillShowNotification",
                0.,
                &[
                    (
                        (WillShow, Rect::default(), docked, 0.25),
                        start(0.25, 0),
                        320.,
                    ),
                    (
                        (WillHide, Rect::default(), docked, 0.),
                        KeyboardInsetAction::HideImmediately,
                        0.,
                    ),
                ],
            ),
            (
                "testKeyboardAnimationIsShowingAndCompounding",
                0.,
                &[
                    (
                        (
                            WillChangeFrame,
                            Rect::new(0., H, W, 250.),
                            Rect::new(0., H - 250., W, 500.),
                            0.25,
                        ),
                        start(0.25, 0),
                        250.,
                    ),
                    (
                        (
                            WillChangeFrame,
                            Rect::new(0., H - 250., W, 250.),
                            Rect::new(0., H - 500., W, 500.),
                            0.25,
                        ),
                        KeyboardInsetAction::Compound,
                        500.,
                    ),
                    (
                        (
                            WillChangeFrame,
                            Rect::new(0., H - 500., W, 250.),
                            Rect::new(0., H - 250., W, 500.),
                            0.25,
                        ),
                        start(0.25, 1),
                        250.,
                    ),
                    (
                        (
                            WillChangeFrame,
                            Rect::new(0., H - 250., W, 250.),
                            Rect::new(0., H, W, 500.),
                            0.25,
                        ),
                        KeyboardInsetAction::Compound,
                        0.,
                    ),
                ],
            ),
            (
                "GPUI: a repeated frame is unchanged; another app's keyboard is ignored",
                0.,
                &[
                    (
                        (WillShow, Rect::default(), docked, 0.25),
                        start(0.25, 0),
                        320.,
                    ),
                    (
                        (WillChangeFrame, docked, docked, 0.25),
                        KeyboardInsetAction::Unchanged,
                        320.,
                    ),
                ],
            ),
        ];
        for (name, initial, steps) in rows {
            let mut manager = KeyboardInsetManager {
                target_view_inset_bottom: *initial,
                ..Default::default()
            };
            for (index, ((notification_name, begin, end, duration), action, target)) in
                steps.iter().enumerate()
            {
                let notification = KeyboardNotification {
                    name: *notification_name,
                    begin: *begin,
                    end: *end,
                    duration: *duration,
                    is_local: Some(true),
                };
                assert_eq!(
                    manager.handle_keyboard_notification(&notification, &FULL),
                    *action,
                    "{name}, step {index}"
                );
                assert_eq!(
                    manager.target_view_inset_bottom, *target,
                    "{name}, step {index}"
                );
            }
        }
        let mut other_app = KeyboardInsetManager::default();
        let notification = KeyboardNotification {
            is_local: Some(false),
            ..notification(WillChangeFrame, docked, false)
        };
        assert_eq!(
            other_app.handle_keyboard_notification(&notification, &FULL),
            KeyboardInsetAction::Ignore
        );
    }

    #[test]
    fn keyboard_spring_rows() {
        // The keyboard's spring (iOS's CASpringAnimation: mass 3, stiffness 1000, damping 500), as Flutter's
        // keyboardSpringAnimation drives the inset. (row, frame time after the start, sample)
        let keyboard = KeyboardSpring {
            stiffness: 1000.,
            damping: 500.,
            mass: 3.,
            initial_velocity: 0.,
        };
        let show = KeyboardNotification {
            name: WillShow,
            begin: Rect::new(0., H, W, 320.),
            end: Rect::new(0., H - 320., W, 320.),
            duration: 0.5,
            is_local: Some(true),
        };
        let mut manager = KeyboardInsetManager::default();
        assert_eq!(manager.sample(10.), InsetSample::Idle, "idle");
        manager.handle_keyboard_notification(&show, &FULL);
        assert_eq!(
            manager.sample(10.),
            InsetSample::PresentationLayer,
            "no spring yet: the presentation layer"
        );
        manager.set_up_keyboard_spring_animation_if_needed(Some(keyboard), 0., 10.);
        let spring = |manager: &KeyboardInsetManager, time| match manager.sample(time) {
            InsetSample::Spring(value) => value,
            other => panic!("not a spring sample: {other:?}"),
        };
        let rows = [
            ("starts at the current inset", 10., 0.),
            ("reaches the target by 0.6 s", 10.6, 320.),
        ];
        for (name, time, expected) in rows {
            let value = spring(&manager, time);
            assert!((value - expected).abs() < 1., "{name}: {value}");
        }
        let halfway = spring(&manager, 10.1);
        assert!(0. < halfway && halfway < 320., "between: {halfway}");
        // testKeyboardAnimationIsShowingAndCompounding: a same-direction change moves the spring's target.
        let higher = KeyboardNotification {
            name: WillChangeFrame,
            begin: show.end,
            end: Rect::new(0., H - 400., W, 400.),
            ..show
        };
        assert_eq!(
            manager.handle_keyboard_notification(&higher, &FULL),
            KeyboardInsetAction::Compound
        );
        assert!((spring(&manager, 10.6) - 400.).abs() < 1., "compounded target");
        manager.set_up_keyboard_spring_animation_if_needed(None, 0., 10.);
        assert_eq!(
            manager.sample(10.6),
            InsetSample::PresentationLayer,
            "not a spring"
        );
    }

    #[test]
    fn finish_only_the_running_animation() {
        // Flutter's completion block acts only while its vsync client is still the current one.
        let mut manager = KeyboardInsetManager::default();
        let show = KeyboardNotification {
            name: WillShow,
            begin: Rect::new(0., H, W, 320.),
            end: Rect::new(0., H - 320., W, 320.),
            duration: 0.25,
            is_local: Some(true),
        };
        assert_eq!(
            manager.handle_keyboard_notification(&show, &FULL),
            KeyboardInsetAction::Start {
                duration: 0.25,
                generation: 0
            }
        );
        let hide = KeyboardNotification {
            begin: show.end,
            end: show.begin,
            ..show
        };
        assert_eq!(
            manager.handle_keyboard_notification(&hide, &FULL),
            KeyboardInsetAction::Start {
                duration: 0.25,
                generation: 1
            }
        );
        assert!(!manager.finish(0), "the replaced show's completion");
        assert!(manager.finish(1), "the hide's completion");
        assert_eq!(manager.animation, KeyboardAnimation::Idle);
        assert!(!manager.finish(1), "a second completion");
    }
}
