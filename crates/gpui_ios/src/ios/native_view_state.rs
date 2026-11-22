//! The two lifecycles native views add to an iOS window, as plain values: who gets keyboard input, and whether
//! UIKit is turning the window. Pure, so scripts/patch-test.sh runs its tests on the host (`ios-pure`).

use gpui::NativeViewId;

/// Who GPUI's focus gives keyboard input to. GPUI's text input view becomes first responder only for `Gpui`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum KeyboardInput {
    /// GPUI's own content: its text input view becomes first responder while a GPUI field is focused.
    Gpui,
    /// This native view's focus handle is focused: GPUI's text input view never becomes first responder, so the
    /// app can make its own `UIView` first responder.
    NativeView(NativeViewId),
}

/// What moves keyboard input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum KeyboardEvent {
    /// GPUI's focus is on this native view's handle, or (`None`) on none (`PlatformWindow::set_focused_native_view`).
    Focused(Option<NativeViewId>),
    /// The last handle of this native view was dropped. A dropped view can't keep the keyboard, whether or not
    /// GPUI tells the window its focus moved, or a GPUI field focused next could never take it back.
    Dropped(NativeViewId),
}

impl KeyboardInput {
    /// The one place keyboard input changes hands.
    pub(super) fn next(self, event: KeyboardEvent) -> Self {
        use KeyboardEvent::{Dropped, Focused};
        use KeyboardInput::{Gpui, NativeView};
        match (self, event) {
            (Gpui | NativeView(_), Focused(None)) => Gpui,
            (Gpui | NativeView(_), Focused(Some(id))) => NativeView(id),
            (NativeView(focused), Dropped(id)) if focused == id => Gpui,
            (NativeView(focused), Dropped(_)) => NativeView(focused),
            (Gpui, Dropped(_)) => Gpui,
        }
    }
}

/// Whether UIKit is animating a rotation (`viewWillTransitionToSize:…` to its coordinator's completion), during
/// which Core Animation resamples every pixel of the turning content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Rotation {
    Settled,
    Turning,
}

impl Rotation {
    /// Whether the screen resamples GPUI's pixels, given the scale GPUI draws at and the screen's native scale:
    /// while turning, and under Display Zoom (`nativeScale` != `scale`). Where it does, holes are cut a pixel inside
    /// their views (gpui's `native_view_placement::hole_inset`).
    pub(super) fn resamples(self, scale: f64, native_scale: f64) -> bool {
        match self {
            Rotation::Turning => true,
            Rotation::Settled => scale != native_scale,
        }
    }

    /// Whether the host view shows the views' gap colour (`NativeView::set_background`). Not while turning: Core
    /// Animation antialiases the turning window's edge layer by layer, so a colour under a view at that edge would
    /// fringe it for a frame; the rigid turn leaves no gap for the colour to cover.
    pub(super) fn shows_background(self) -> bool {
        match self {
            Rotation::Turning => false,
            Rotation::Settled => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyboardEvent::{Dropped, Focused};
    use KeyboardInput::{Gpui, NativeView};

    #[test]
    fn keyboard_input_follows_focus_and_leaves_a_dropped_view() {
        let (a, b) = (NativeViewId::for_test(1), NativeViewId::for_test(2));
        // (name, state, event, next)
        let cases = [
            (
                "GPUI to a focused view",
                Gpui,
                Focused(Some(a)),
                NativeView(a),
            ),
            (
                "GPUI stays without a focused view",
                Gpui,
                Focused(None),
                Gpui,
            ),
            ("a view back to GPUI", NativeView(a), Focused(None), Gpui),
            (
                "one view to another",
                NativeView(a),
                Focused(Some(b)),
                NativeView(b),
            ),
            (
                "the same view again",
                NativeView(a),
                Focused(Some(a)),
                NativeView(a),
            ),
            // M1: GPUI never says None once the last view is gone; a GPUI field must still get the keyboard.
            (
                "focused view dropped: back to GPUI",
                NativeView(a),
                Dropped(a),
                Gpui,
            ),
            (
                "another view dropped: the focused one keeps it",
                NativeView(a),
                Dropped(b),
                NativeView(a),
            ),
            ("a view dropped while GPUI has it", Gpui, Dropped(a), Gpui),
        ];
        for (name, state, event, next) in cases {
            assert_eq!(state.next(event), next, "{name}");
        }
    }

    #[test]
    fn the_screen_resamples_while_turning_and_under_display_zoom() {
        // (name, rotation, GPUI's scale, the screen's native scale, resamples)
        let cases = [
            ("iPhone at 3x, settled", Rotation::Settled, 3., 3., false),
            ("iPad at 2x, settled", Rotation::Settled, 2., 2., false),
            ("iPhone at 3x, turning", Rotation::Turning, 3., 3., true),
            (
                "Display Zoom (3x drawn, 2.88x shown)",
                Rotation::Settled,
                3.,
                2.88,
                true,
            ),
            (
                "iPad mini (2x drawn, 2.18x shown)",
                Rotation::Settled,
                2.,
                2.1875,
                true,
            ),
        ];
        for (name, rotation, scale, native_scale, resamples) in cases {
            assert_eq!(rotation.resamples(scale, native_scale), resamples, "{name}");
        }
    }

    #[test]
    fn the_gap_colour_shows_only_while_settled() {
        // (name, rotation, shows the background)
        let cases = [
            ("settled", Rotation::Settled, true),
            ("turning: it would fringe the turning edge", Rotation::Turning, false),
        ];
        for (name, rotation, shows) in cases {
            assert_eq!(rotation.shows_background(), shows, "{name}");
        }
    }
}
