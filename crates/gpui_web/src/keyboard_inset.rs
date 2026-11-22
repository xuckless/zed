// Ported from flutter/flutter engine/src/flutter/lib/web_ui/lib/src/engine/view_embedder/dimensions_provider/
// full_page_dimensions_provider.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7): `computeKeyboardInsets`
// and its use of `_iOSHeight`; the test rows from engine/src/flutter/lib/web_ui/test/engine/view_embedder/
// dimensions_provider/full_page_dimensions_provider_test.dart @ 3.50.1. Copyright 2014 The Flutter Authors.
// BSD-3-Clause, see third_party/flutter/LICENSE.

//! How much of the canvas the on-screen keyboard covers, from the visual viewport. Pure, so it is tested on any
//! host; `viewport.rs` measures the inputs and reports the result as `WindowInsets::ime`.
//!
//! Browsers shrink the visual viewport, not the layout viewport, for the keyboard (`interactive-widget=
//! resizes-visual`, the default), so Flutter reports the difference as the keyboard's inset. Flutter takes the
//! page's height; a GPUI canvas need not fill the page, so here it is the canvas's bottom edge.

/// The visual viewport, in layout CSS pixels (`window.visualViewport`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VisualViewport {
    pub(crate) offset_top: f64,
    pub(crate) height: f64,
    pub(crate) scale: f64,
}

/// The keyboard's inset from the canvas's bottom edge, in CSS pixels, never negative.
///
/// `canvas_bottom` is the canvas's bottom edge in the layout viewport. Flutter's rules, then GPUI's:
/// - no visual viewport (old browsers): Flutter measures `innerHeight`, which the keyboard doesn't shrink: none;
/// - iOS while not editing: Flutter measures the view's own height (`_iOSHeight`), so no keyboard shows; a visual
///   viewport iOS has not resized back (a rotation, the toolbar) must not read as one;
/// - pinch-zoomed (scale ≠ 1): none. Zoom shrinks the visual viewport too; Flutter's pages can't zoom, and
///   gpui_web infers nothing while zoomed (`keyboard_likely_dismissed`).
pub(crate) fn keyboard_inset(
    canvas_bottom: f64,
    viewport: Option<VisualViewport>,
    ios: bool,
    is_editing_on_mobile: bool,
) -> f64 {
    let Some(viewport) = viewport else {
        return 0.;
    };
    if (ios && !is_editing_on_mobile) || (viewport.scale - 1.).abs() >= 0.001 {
        return 0.;
    }
    (canvas_bottom - (viewport.offset_top + viewport.height)).max(0.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(height: f64) -> Option<VisualViewport> {
        Some(VisualViewport {
            offset_top: 0.,
            height,
            scale: 1.,
        })
    }

    /// (Flutter test or GPUI rule, canvas bottom, viewport, iOS, editing, inset). Flutter's iPhone rows use its
    /// fake: `documentElement.clientHeight` 775 (the canvas fills it), visual viewport heights as in each test.
    #[test]
    fn insets() {
        let rows: [(&str, f64, Option<VisualViewport>, bool, bool, f64); 8] = [
            (
                "from viewport physical size (simulated keyboard)",
                700.,
                viewport(600.),
                false,
                false,
                100.,
            ),
            (
                "reports no keyboard inset when the browser toolbar collapses",
                775.,
                viewport(815.),
                true,
                false,
                0.,
            ),
            (
                "reports the keyboard inset while editing",
                775.,
                viewport(426.),
                true,
                true,
                775. - 426.,
            ),
            (
                "iOS: a shorter visual viewport while not editing is no keyboard",
                775.,
                viewport(426.),
                true,
                false,
                0.,
            ),
            ("GPUI: no visual viewport", 700., None, false, false, 0.),
            (
                "GPUI: the visual viewport scrolled down by the keyboard",
                700.,
                Some(VisualViewport {
                    offset_top: 50.,
                    height: 400.,
                    scale: 1.,
                }),
                false,
                true,
                250.,
            ),
            (
                "GPUI: pinch-zoomed is no keyboard",
                700.,
                Some(VisualViewport {
                    offset_top: 0.,
                    height: 350.,
                    scale: 2.,
                }),
                false,
                true,
                0.,
            ),
            (
                "GPUI: a canvas above the keyboard",
                300.,
                viewport(400.),
                false,
                true,
                0.,
            ),
        ];
        for (name, canvas_bottom, viewport, ios, editing, inset) in rows {
            assert_eq!(
                keyboard_inset(canvas_bottom, viewport, ios, editing),
                inset,
                "{name}"
            );
        }
    }
}
