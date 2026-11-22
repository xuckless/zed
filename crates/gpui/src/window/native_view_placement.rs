//! Where a painted native view shows, the one copy every platform places its views from (Wayland, Windows, the Apple
//! backends, the web, Android). Pure: plain values in, plain values out.
//!
//! A [`PaintNativeView`]'s bounds and content mask are already whole device pixels (the element snaps both), so a
//! platform whose placement grid is the device pixel uses [`visible`] as is. Only a coarser grid rounds again
//! ([`round_outwards`]: Wayland's logical pixels at a fractional scale), and only a compositor that resamples the
//! window cuts the hole inside the view ([`hole_inset`]).

use crate::{Bounds, PaintNativeView, Rgba, ScaledPixels, point, size};

/// The part of the view its content mask leaves, in device pixels, or `None` when the mask leaves none of it.
pub fn visible(view: &PaintNativeView) -> Option<Bounds<ScaledPixels>> {
    let visible = view.bounds.intersect(&view.content_mask.bounds);
    (!visible.is_empty()).then_some(visible)
}

/// The smallest whole-unit bounds that hold `bounds` (device pixels), on a grid of `px_per_unit` device pixels per
/// unit. Chromium's rule for overlays under a window (ui/ozone wayland_subsurface.cc): an overlay never ends partway
/// through a unit, so it covers every pixel of its hole, and [`hole_inset`] keeps the window over its rounded edges.
pub fn round_outwards(bounds: Bounds<ScaledPixels>, px_per_unit: f32) -> Bounds<i32> {
    let unit = |value: ScaledPixels| value.0 / px_per_unit;
    let left = unit(bounds.origin.x).floor() as i32;
    let top = unit(bounds.origin.y).floor() as i32;
    let right = unit(bounds.origin.x + bounds.size.width).ceil() as i32;
    let bottom = unit(bounds.origin.y + bounds.size.height).ceil() as i32;
    Bounds {
        origin: point(left, top),
        size: size(right - left, bottom - top),
    }
}

/// How far inside a view's bounds its hole is cut. Where the compositor resamples (a fractional Wayland scale, iOS
/// Display Zoom or a rotation's turn), the view's and the hole's shared edge both blend and would leave a seam of what
/// is beneath, so the hole is cut one device pixel inside and the window's own pixels cover the view's edge.
pub fn hole_inset(resamples: bool) -> ScaledPixels {
    // resamples → inset, in device pixels.
    match resamples {
        false => ScaledPixels(0.),
        true => ScaledPixels(1.),
    }
}

/// The colour shown under the window's native views where they haven't caught up with their elements: the background
/// of the last painted view that shows and asks for one ([`crate::NativeView::set_background`]). `None` when none
/// does; what each platform shows then is in Parallax's docs/native-view.md.
pub fn background(views: &[PaintNativeView]) -> Option<Rgba> {
    views
        .iter()
        .rev()
        .filter(|view| visible(view).is_some())
        .map(|view| Rgba::from(view.background))
        .find(|color| color.a > 0.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentMask, HitboxId, Hsla, NativeViewId, hsla};

    fn device(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    fn view(
        bounds: Bounds<ScaledPixels>,
        mask: Bounds<ScaledPixels>,
        background: Hsla,
    ) -> PaintNativeView {
        PaintNativeView {
            order: 0,
            id: NativeViewId::for_test(1),
            hitbox_id: HitboxId::placeholder(),
            bounds,
            content_mask: ContentMask { bounds: mask },
            background,
        }
    }

    #[test]
    fn visible_part_of_a_view() {
        let clear = Hsla::transparent_black();
        // (row name, bounds, content mask, visible part). Extend with a row per clipping case a platform must match.
        let rows = [
            (
                "unclipped shows whole",
                device(10., 20., 100., 50.),
                device(0., 0., 800., 600.),
                Some(device(10., 20., 100., 50.)),
            ),
            (
                "clipped on the top and right",
                device(10., 20., 100., 50.),
                device(0., 40., 60., 600.),
                Some(device(10., 40., 50., 30.)),
            ),
            (
                "clipped away entirely shows nothing",
                device(10., 20., 100., 50.),
                device(200., 0., 50., 50.),
                None,
            ),
            (
                "zero sized shows nothing",
                device(10., 20., 0., 50.),
                device(0., 0., 800., 600.),
                None,
            ),
            (
                "touching the clip's edge shows nothing",
                device(10., 20., 100., 50.),
                device(110., 0., 50., 600.),
                None,
            ),
        ];
        for (name, bounds, mask, expected) in rows {
            assert_eq!(visible(&view(bounds, mask, clear)), expected, "{name}");
        }
    }

    #[test]
    fn round_outwards_to_whole_units() {
        let rect = |x, y, width, height| Bounds {
            origin: point(x, y),
            size: size(width, height),
        };
        // (row name, device bounds, device pixels per unit, whole units). Extend with a row per scale a platform uses.
        let rows = [
            (
                "whole pixels at 1",
                device(10., 20., 300., 200.),
                1.,
                rect(10, 20, 300, 200),
            ),
            (
                "whole pixels at 2",
                device(20., 40., 600., 400.),
                2.,
                rect(10, 20, 300, 200),
            ),
            (
                "half units at 2 grow",
                device(21., 41., 600., 400.),
                2.,
                rect(10, 20, 301, 201),
            ),
            (
                "exact at 1.25",
                device(125., 250., 500., 250.),
                1.25,
                rect(100, 200, 400, 200),
            ),
            (
                "partial at 1.25 grow",
                device(126., 251., 500., 250.),
                1.25,
                rect(100, 200, 401, 201),
            ),
            (
                "negative origins floor away from zero",
                device(-151., -120., 300., 120.),
                1.5,
                rect(-101, -80, 201, 80),
            ),
        ];
        for (name, bounds, px_per_unit, expected) in rows {
            assert_eq!(round_outwards(bounds, px_per_unit), expected, "{name}");
        }
    }

    #[test]
    fn hole_is_cut_inside_only_where_the_compositor_resamples() {
        // (row name, resamples, inset in device pixels)
        let rows = [
            ("exact pixels: the hole is the view", false, 0.),
            ("resampled: one pixel inside", true, 1.),
        ];
        for (name, resamples, inset) in rows {
            assert_eq!(hole_inset(resamples), ScaledPixels(inset), "{name}");
        }
    }

    #[test]
    fn background_under_the_views() {
        let clear = Hsla::transparent_black();
        let red = hsla(0., 1., 0.5, 1.);
        let blue = hsla(2. / 3., 1., 0.5, 1.);
        let mask = device(0., 0., 100., 100.);
        let shown = |color| view(device(0., 0., 10., 10.), mask, color);
        let clipped_away = |color| view(device(200., 0., 10., 10.), mask, color);
        // (row name, scene views in paint order, background). Extend with a row per rule.
        let rows = [
            ("no views, none", vec![], None),
            ("no view asks, none", vec![shown(clear), shown(clear)], None),
            (
                "the only view that asks",
                vec![shown(red), shown(clear)],
                Some(red),
            ),
            (
                "the last painted view that asks wins",
                vec![shown(red), shown(blue)],
                Some(blue),
            ),
            (
                "a view clipped away asks for nothing",
                vec![shown(red), clipped_away(blue)],
                Some(red),
            ),
        ];
        for (name, views, expected) in rows {
            assert_eq!(background(&views), expected.map(Rgba::from), "{name}");
        }
    }
}
