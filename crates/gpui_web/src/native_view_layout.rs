//! Native views on the web, the decisions: where a view's DOM element goes under the canvas, and whether the canvas
//! or the element under it takes the pointer. Pure, so it is tested on any host; `native_views.rs` is the DOM shell.

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

/// Who the browser hit-tests the pointer to while a view shows: GPUI's canvas, or what is under it (the canvas's
/// `pointer-events: none`), where an uncovered view's element takes trusted events of its own (an iframe's clicks
/// and scrolls, a video's controls) and the native view layer and its shields take the rest back for GPUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PointerTarget {
    /// The canvas's own `pointer-events: auto`, before any view shows.
    #[default]
    Canvas,
    Views,
}

/// A pointer event seen by the document's capture listener, or a frame that placed views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerKind {
    /// `pointermove` with no button pressed.
    Hover,
    /// `pointermove` with a button pressed: the press's target keeps it.
    Drag,
    Press,
    Wheel,
    /// A mouse or pen `pointerup` / `pointercancel`.
    Release,
    /// A touch `pointerup` / `pointercancel`: a touch has no hover to switch on, so the next one must find the
    /// view's element, and the layer and shields send it back if it lands on GPUI's content.
    TouchRelease,
    /// A frame placed views while no button is pressed: content may have moved a view under a still pointer, whose
    /// next wheel tick must already find the element.
    Placed,
}

/// What GPUI's hit test says is at the pointer (`hit_test_native_view`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Over {
    /// A view with no GPUI hitbox above it there.
    View,
    Window,
}

/// What the browser gave the event to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    Canvas,
    /// The native view layer itself or a shield: GPUI's, standing in for the canvas.
    Cover,
    /// Inside a view's slot: the app's element.
    Element,
    /// Page content outside the window, none of GPUI's business.
    Outside,
}

/// Where the capture listener sends a copy of the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Send {
    Nothing,
    /// GPUI's content is there, but the browser gave the event to the layer or a shield.
    Canvas,
    /// A view is there, but a shield from an earlier frame took the event: the shields are refreshed and the copy
    /// goes to the element now under the pointer.
    ElementUnder,
}

/// What the capture listener does with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) target: PointerTarget,
    pub(crate) send: Send,
}

/// `current × event → step`. Every arm is explicit: extend by adding a `PointerKind`, `Over` or `Hit` and its arms.
pub(crate) fn step(current: PointerTarget, kind: PointerKind, over: Over, hit: Hit) -> Step {
    use PointerKind::*;
    let to = |target, send| Step { target, send };
    match (kind, over, hit) {
        (
            Drag,
            Over::View | Over::Window,
            Hit::Canvas | Hit::Cover | Hit::Element | Hit::Outside,
        ) => to(current, Send::Nothing),
        (
            Hover | Press | Wheel | Release | TouchRelease,
            Over::View | Over::Window,
            Hit::Outside,
        ) => to(current, Send::Nothing),
        (TouchRelease, Over::View | Over::Window, Hit::Canvas | Hit::Cover | Hit::Element) => {
            to(PointerTarget::Views, Send::Nothing)
        }
        (Hover | Press | Wheel | Release, Over::View, Hit::Canvas | Hit::Element) => {
            to(PointerTarget::Views, Send::Nothing)
        }
        (Hover | Press | Wheel | Release, Over::View, Hit::Cover) => {
            to(PointerTarget::Views, Send::ElementUnder)
        }
        (Hover | Press | Wheel, Over::Window, Hit::Cover) => {
            to(PointerTarget::Canvas, Send::Canvas)
        }
        // The element keeps this one event; the canvas takes the next (the shield goes up after the frame).
        (Hover | Press | Wheel, Over::Window, Hit::Canvas | Hit::Element) => {
            to(PointerTarget::Canvas, Send::Nothing)
        }
        // A release off the canvas ends a press GPUI never had (the press's target captured it otherwise).
        (Release, Over::Window, Hit::Canvas | Hit::Cover | Hit::Element) => {
            to(PointerTarget::Canvas, Send::Nothing)
        }
        (Placed, Over::View, Hit::Canvas | Hit::Cover | Hit::Element | Hit::Outside) => {
            to(PointerTarget::Views, Send::Nothing)
        }
        // A touch's views keep the pointer; a mouse over GPUI's content lands on the layer, which sends it back.
        (Placed, Over::Window, Hit::Canvas | Hit::Cover | Hit::Element | Hit::Outside) => {
            to(current, Send::Nothing)
        }
    }
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

    #[test]
    fn pointer_target_steps() {
        use Hit::{Canvas as OnCanvas, Cover, Element, Outside};
        use Over::{View, Window};
        use PointerKind::*;
        use PointerTarget::{Canvas, Views};
        // (row name, current, event, over, what the browser hit, next target, copy sent). One row per
        // (state, event), rejected ones included.
        #[rustfmt::skip]
        let rows = [
            ("hover onto an uncovered view", Canvas, Hover, View, OnCanvas, Views, Send::Nothing),
            ("hover within a view's element", Views, Hover, View, Element, Views, Send::Nothing),
            ("hover onto GPUI's content from a view", Views, Hover, Window, Cover, Canvas, Send::Canvas),
            ("hover over GPUI's content", Canvas, Hover, Window, OnCanvas, Canvas, Send::Nothing),
            ("hover where GPUI's content covers an element before its shield", Views, Hover, Window, Element, Canvas, Send::Nothing),
            ("hover on a stale shield over a view", Views, Hover, View, Cover, Views, Send::ElementUnder),
            ("drag from the canvas over a view", Canvas, Drag, Window, OnCanvas, Canvas, Send::Nothing),
            ("drag from a view over GPUI's content", Views, Drag, Window, Cover, Views, Send::Nothing),
            ("drag outside the window", Views, Drag, Window, Outside, Views, Send::Nothing),
            ("press on a view the canvas still had", Canvas, Press, View, OnCanvas, Views, Send::Nothing),
            ("press on a view's element", Views, Press, View, Element, Views, Send::Nothing),
            ("press on a shield over a view", Views, Press, Window, Cover, Canvas, Send::Canvas),
            ("press on a stale shield over a view", Views, Press, View, Cover, Views, Send::ElementUnder),
            ("press on GPUI's content", Canvas, Press, Window, OnCanvas, Canvas, Send::Nothing),
            ("wheel over a view's element", Views, Wheel, View, Element, Views, Send::Nothing),
            ("wheel over a shield", Views, Wheel, Window, Cover, Canvas, Send::Canvas),
            ("wheel over a stale shield", Views, Wheel, View, Cover, Views, Send::ElementUnder),
            ("wheel over GPUI's content", Canvas, Wheel, Window, OnCanvas, Canvas, Send::Nothing),
            ("mouse release over a view", Canvas, Release, View, OnCanvas, Views, Send::Nothing),
            ("mouse release on a stale shield", Views, Release, View, Cover, Views, Send::ElementUnder),
            ("mouse release on the layer", Views, Release, Window, Cover, Canvas, Send::Nothing),
            ("mouse release on the canvas", Canvas, Release, Window, OnCanvas, Canvas, Send::Nothing),
            ("touch release on GPUI's content", Canvas, TouchRelease, Window, OnCanvas, Views, Send::Nothing),
            ("touch release on a view", Views, TouchRelease, View, Element, Views, Send::Nothing),
            ("hover outside the window", Views, Hover, Window, Outside, Views, Send::Nothing),
            ("press outside the window", Canvas, Press, Window, Outside, Canvas, Send::Nothing),
            ("wheel outside the window", Views, Wheel, Window, Outside, Views, Send::Nothing),
            ("mouse release outside the window", Views, Release, Window, Outside, Views, Send::Nothing),
            ("touch release outside the window", Canvas, TouchRelease, Window, Outside, Canvas, Send::Nothing),
            ("content moved a view under a still pointer", Canvas, Placed, View, OnCanvas, Views, Send::Nothing),
            ("content moved over a touch's views", Views, Placed, Window, OnCanvas, Views, Send::Nothing),
            ("a frame with the pointer on GPUI's content", Canvas, Placed, Window, OnCanvas, Canvas, Send::Nothing),
        ];
        for (name, current, kind, over, hit, target, send) in rows {
            assert_eq!(
                step(current, kind, over, hit),
                Step { target, send },
                "{name}"
            );
        }
    }
}
