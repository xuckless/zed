//! What DirectComposition shows of a window's native views, decided on plain values so it is tested on any host
//! (Parallax's `scripts/patch-test.sh` builds this file on its own); `directx_renderer.rs` applies it to the visual
//! tree. Modelled on Chromium's `ui/gl/dc_layer_tree.cc` (`VisualTree::BuildTree`, `VisualSubtree::Update`): every
//! view is a clip visual holding a content visual, stacked back to front under GPUI's own swap chain visual, and the
//! tree is changed and committed only when a frame moves, shows, hides or restacks a view.

use gpui::{
    NativeViewId, PaintNativeView, Rgba, WindowBackgroundAppearance,
    native_view_placement::{background, visible},
};

/// Where one native view's visuals sit, in whole device pixels from the window's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// The content visual's offset: the view's top left, so its content shows 1:1 from there.
    pub(crate) origin: [f32; 2],
    /// The clip visual's clip, `[left, top, right, bottom]`: the part of the view its content mask leaves.
    pub(crate) clip: [f32; 4],
}

impl Placement {
    /// `None` when the content mask leaves none of the view. The element's bounds and clip are whole device pixels,
    /// and the hole is drawn from the same bounds, so the visuals sit exactly where the rasterizer cleared the hole.
    pub(crate) fn new(view: &PaintNativeView) -> Option<Self> {
        let visible = visible(view)?;
        let corner = visible.bottom_right();
        Some(Self {
            origin: [view.bounds.origin.x.0, view.bounds.origin.y.0],
            clip: [visible.origin.x.0, visible.origin.y.0, corner.x.0, corner.y.0],
        })
    }
}

/// What GPUI's own visual shows, as a plain value (`directx_renderer`'s `Presenting` holds the surface).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Showing {
    /// GPUI's swap chain, as upstream.
    SwapChain,
    /// The composition surface GPUI's frames are copied into while a view shows.
    Surface,
    /// Still the surface, the frame after the last view went; the swap chain already holds that frame.
    LeavingSurface,
}

/// How a frame reaches DirectComposition. A swap chain's `Present` and a `Commit` are not atomic (DComp has no API to
/// tie them), so a frame that moves a view could show its new hole a composition frame before or after the view's new
/// placement. A composition surface changes only on `Commit`, so GPUI's frame, its holes and the views' placement
/// change together, at one GPU copy of GPUI's frame per GPUI frame (never per frame of a view's own content).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresentStep {
    /// `Present` on GPUI's swap chain, as upstream: no copy, no `Commit`.
    Present,
    /// Copy into the surface (made first, or again at a new size) and commit it with the views' placement.
    CopyToSurface,
    /// The last view went: this hole-less frame is copied into the surface and committed with the views' removal,
    /// and presented to the swap chain too, so the swap chain holds it when it shows again.
    CopyToSurfaceAndPresent,
    /// Present, then show the swap chain again (`SetContent` and a `Commit`): it already holds the previous
    /// hole-less frame, so whichever of the two frames DWM composes first is current, never a stale one.
    PresentAndShowSwapChain,
}

/// `showing × whether the frame shows a view → (step, showing next)`. A window pays for the surface only while it
/// shows a native view, and leaves it over two frames, so no stale swap chain frame ever flashes.
pub(crate) fn present_step(showing: Showing, shows_views: bool) -> (PresentStep, Showing) {
    match (showing, shows_views) {
        (Showing::SwapChain, false) => (PresentStep::Present, Showing::SwapChain),
        (Showing::SwapChain | Showing::Surface | Showing::LeavingSurface, true) => {
            (PresentStep::CopyToSurface, Showing::Surface)
        }
        (Showing::Surface, false) => (PresentStep::CopyToSurfaceAndPresent, Showing::LeavingSurface),
        (Showing::LeavingSurface, false) => (PresentStep::PresentAndShowSwapChain, Showing::SwapChain),
    }
}

/// What one frame shows of the window's native views.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Frame {
    /// The views shown, back to front, each with its placement.
    pub(crate) views: Vec<(NativeViewId, Placement)>,
    /// The opaque color under every view, shown where a view's content doesn't reach (its content hasn't caught up
    /// with a resize yet). `None` for a window whose backdrop is meant to show through, or with no view shown.
    pub(crate) background: Option<[f32; 3]>,
}

impl Frame {
    /// The scene's native views are sorted by draw order and painted once each, so the ones painted later, which
    /// belong in front, come last. An opaque window shows black under its views unless one asks for a color
    /// (`native_view_placement::background`).
    pub(crate) fn new(native_views: &[PaintNativeView], appearance: WindowBackgroundAppearance) -> Self {
        let views: Vec<_> = native_views
            .iter()
            .filter_map(|view| Placement::new(view).map(|placement| (view.id, placement)))
            .collect();
        let background = (appearance == WindowBackgroundAppearance::Opaque && !views.is_empty()).then(|| {
            let Rgba { r, g, b, .. } = background(native_views).unwrap_or(Rgba { r: 0., g: 0., b: 0., a: 1. });
            [r, g, b]
        });
        Self { views, background }
    }

    pub(crate) fn stacking(&self) -> impl Iterator<Item = NativeViewId> + '_ {
        self.views.iter().map(|(id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, ContentMask, DrawOrder, HitboxId, Hsla, ScaledPixels, hsla, point, size};

    fn device_bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    #[test]
    fn placement_of_a_view() {
        // (name, bounds, content mask, placement). Extend with a row per layout case DirectComposition must match.
        let rows: [(&str, [f32; 4], [f32; 4], Option<Placement>); 3] = [
            (
                "unclipped view shows whole at its origin",
                [10., 20., 100., 50.],
                [0., 0., 800., 600.],
                Some(Placement { origin: [10., 20.], clip: [10., 20., 110., 70.] }),
            ),
            (
                "content mask clips the view, origin stays",
                [10., 20., 100., 50.],
                [0., 40., 60., 600.],
                Some(Placement { origin: [10., 20.], clip: [10., 40., 60., 70.] }),
            ),
            ("clipped away entirely is hidden", [10., 20., 100., 50.], [200., 0., 50., 50.], None),
        ];
        for (name, [x, y, w, h], [mx, my, mw, mh], expected) in rows {
            let view = PaintNativeView {
                order: 0 as DrawOrder,
                id: NativeViewId::for_test(1),
                hitbox_id: HitboxId::placeholder(),
                bounds: device_bounds(x, y, w, h),
                content_mask: ContentMask { bounds: device_bounds(mx, my, mw, mh) },
                background: Hsla::transparent_black(),
            };
            assert_eq!(Placement::new(&view), expected, "{name}");
        }
    }

    #[test]
    fn present_steps() {
        use PresentStep::*;
        use Showing::*;
        // (row name, showing, the frame shows a view, step, showing next). One row per (showing, views) pair.
        let rows = [
            ("no view: the swap chain, as upstream", SwapChain, false, Present, SwapChain),
            ("the first view: the surface", SwapChain, true, CopyToSurface, Surface),
            ("views go on showing", Surface, true, CopyToSurface, Surface),
            ("the last view goes: the surface once more, the swap chain primed", Surface, false, CopyToSurfaceAndPresent, LeavingSurface),
            ("the frame after: the swap chain again", LeavingSurface, false, PresentAndShowSwapChain, SwapChain),
            ("a view shows again while leaving: the surface stays", LeavingSurface, true, CopyToSurface, Surface),
        ];
        for (name, showing, shows_views, step, next) in rows {
            assert_eq!(present_step(showing, shows_views), (step, next), "{name}");
        }
    }

    #[test]
    fn frame_of_a_scene() {
        let view = |id: u64, x: f32, background: Hsla| PaintNativeView {
            order: 0 as DrawOrder,
            id: NativeViewId::for_test(id),
            hitbox_id: HitboxId::placeholder(),
            bounds: device_bounds(x, 0., 10., 10.),
            content_mask: ContentMask { bounds: device_bounds(0., 0., 100., 100.) },
            background,
        };
        let clear = Hsla::transparent_black();
        let red = hsla(0., 1., 0.5, 1.);
        let at = |x: f32| Placement { origin: [x, 0.], clip: [x, 0., x + 10., 10.] };
        // (name, scene views, appearance, frame). Extend with a row per stacking or background rule.
        let rows = [
            ("no views, no background", vec![], WindowBackgroundAppearance::Opaque, Frame::default()),
            (
                "back to front in paint order, black when no view asks for a color",
                vec![view(2, 0., clear), view(1, 20., clear)],
                WindowBackgroundAppearance::Opaque,
                Frame {
                    views: vec![(NativeViewId::for_test(2), at(0.)), (NativeViewId::for_test(1), at(20.))],
                    background: Some([0., 0., 0.]),
                },
            ),
            (
                "the last view asking for a color wins",
                vec![view(1, 0., red), view(2, 20., clear)],
                WindowBackgroundAppearance::Opaque,
                Frame {
                    views: vec![(NativeViewId::for_test(1), at(0.)), (NativeViewId::for_test(2), at(20.))],
                    background: Some([1., 0., 0.]),
                },
            ),
            (
                "a transparent window keeps its backdrop",
                vec![view(1, 0., red)],
                WindowBackgroundAppearance::Transparent,
                Frame { views: vec![(NativeViewId::for_test(1), at(0.))], background: None },
            ),
            (
                "a clipped-away view is hidden and asks for no background",
                vec![view(1, 200., red)],
                WindowBackgroundAppearance::Opaque,
                Frame::default(),
            ),
        ];
        for (name, views, appearance, expected) in rows {
            assert_eq!(Frame::new(&views, appearance), expected, "{name}");
        }
    }
}
