//! Native views on the web, the decisions: where a view's DOM element goes under the canvas. Pure, so it is tested
//! on any host; `native_views.rs` is the DOM shell.

use gpui::{Bounds, PaintNativeView, Pixels, ScaledPixels, native_view_placement, point, px, size};

/// Where a shown view's elements go, in CSS pixels: its slot (the part of the view its content mask leaves, from the
/// canvas's top left) and the view's own box inside the slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub(crate) slot: Bounds<Pixels>,
    pub(crate) view: Bounds<Pixels>,
}

/// `None` when the content mask leaves none of the view (it hides). The bounds are whole device pixels already
/// (`native_view_placement`), so both boxes land on the device pixels of the renderer's hole at any scale.
pub(crate) fn placement(native_view: &PaintNativeView, scale_factor: f32) -> Option<Placement> {
    let slot = native_view_placement::visible(native_view)?;
    let css = |origin: gpui::Point<ScaledPixels>, extent: gpui::Size<ScaledPixels>| {
        Bounds::new(
            point(px(origin.x.0 / scale_factor), px(origin.y.0 / scale_factor)),
            size(
                px(extent.width.0 / scale_factor),
                px(extent.height.0 / scale_factor),
            ),
        )
    };
    Some(Placement {
        slot: css(slot.origin, slot.size),
        view: css(
            native_view.bounds.origin - slot.origin,
            native_view.bounds.size,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ContentMask, HitboxId, Hsla, NativeViewId};

    fn view(bounds: [f32; 4], mask: [f32; 4]) -> PaintNativeView {
        let scaled = |[x, y, width, height]: [f32; 4]| Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        };
        PaintNativeView {
            order: 0,
            id: NativeViewId::for_test(1),
            hitbox_id: HitboxId::placeholder(),
            bounds: scaled(bounds),
            content_mask: ContentMask {
                bounds: scaled(mask),
            },
            background: Hsla::default(),
        }
    }

    fn css([x, y, width, height]: [f32; 4]) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn placement_of_a_view() {
        let everything = [0., 0., 10_000., 10_000.];
        // (row name, view bounds and content mask in device pixels, scale, slot and view in CSS pixels or hidden).
        // The core's `visible` decides what shows; these rows are the web's CSS boxes. Add a row per rule.
        #[rustfmt::skip]
        let rows: [(&str, [f32; 4], [f32; 4], f32, Option<([f32; 4], [f32; 4])>); 4] = [
            ("whole pixels at 1", [10., 20., 300., 200.], everything, 1.,
                Some(([10., 20., 300., 200.], [0., 0., 300., 200.]))),
            ("device pixels at 1.25", [125., 250., 375., 250.], everything, 1.25,
                Some(([100., 200., 300., 200.], [0., 0., 300., 200.]))),
            ("the content mask clips the slot, the view keeps its size", [0., 0., 200., 100.], [50., 0., 100., 100.], 1.,
                Some(([50., 0., 100., 100.], [-50., 0., 200., 100.]))),
            ("clipped away entirely hides", [0., 0., 200., 100.], [300., 0., 100., 100.], 1., None),
        ];
        for (name, bounds, mask, scale, expected) in rows {
            let expected = expected.map(|(slot, view)| Placement {
                slot: css(slot),
                view: css(view),
            });
            assert_eq!(placement(&view(bounds, mask), scale), expected, "{name}");
        }
    }
}
