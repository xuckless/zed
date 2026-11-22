//! The lifecycle native views add to an iOS window, as a plain value: whether UIKit is turning the window. Pure,
//! so scripts/patch-test.sh runs its tests on the host (`ios-pure`).

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
