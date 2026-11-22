//! Where a native view's clipping container and view go in the window: shared by gpui_macos (AppKit, y up) and
//! gpui_ios (UIKit, y down). Pure, so scripts/patch-test.sh runs its tests on the host (`apple-pure`).
//!
//! What shows of a view and how far inside it the hole is cut are GPUI's one copy (`gpui::native_view_placement`):
//! the element's bounds are whole device pixels already, so this only turns them into points.

use gpui::{
    Bounds, NativeViewId, PaintNativeView, ScaledPixels, Scene, native_view_placement::visible,
};

/// Which way the platform's view coordinates grow vertically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum YAxis {
    /// UIKit: from the top left.
    Down,
    /// AppKit: from the bottom left.
    Up,
}

/// Where a native view is shown, as `[x, y, width, height]` frames in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The visible part of the view, in the superview of GPUI's view.
    pub container: [f64; 4],
    /// The whole view, in its container.
    pub view: [f64; 4],
}

impl Placement {
    /// Places a painted view, whose bounds and clip are in device pixels from the top left of GPUI's view, whose
    /// frame in its superview is `gpui_frame`. Returns `None` if the clip leaves none of the view visible.
    pub fn new(
        native_view: &PaintNativeView,
        scale_factor: f64,
        gpui_frame: [f64; 4],
        y_axis: YAxis,
    ) -> Option<Self> {
        let visible = visible(native_view)?;
        let to_frame = |bounds: Bounds<ScaledPixels>| {
            let [gpui_x, gpui_y, _, gpui_height] = gpui_frame;
            let to_points = |value: ScaledPixels| f64::from(value) / scale_factor;
            let top = to_points(bounds.origin.y);
            let height = to_points(bounds.size.height);
            let y = match y_axis {
                YAxis::Down => gpui_y + top,
                YAxis::Up => gpui_y + gpui_height - top - height,
            };
            [
                gpui_x + to_points(bounds.origin.x),
                y,
                to_points(bounds.size.width),
                height,
            ]
        };
        let container = to_frame(visible);
        let [x, y, width, height] = to_frame(native_view.bounds);
        Some(Self {
            container,
            view: [x - container[0], y - container[1], width, height],
        })
    }
}

/// What a scene changes about a window's attached views.
#[derive(Default)]
pub struct Layout {
    /// Views to place somewhere new, or to hide for `None`.
    pub placements: Vec<(NativeViewId, Option<Placement>)>,
    /// The views the scene shows, from back to front, if that order changed.
    pub stacking: Option<Vec<NativeViewId>>,
}

/// Works out which attached views the scene moves, shows, hides, or restacks. `attached` is each attached view with
/// where it was last placed, `stacking` the order the last scene showed them in, back to front.
pub fn plan_layout(
    attached: impl IntoIterator<Item = (NativeViewId, Option<Placement>)>,
    stacking: &[NativeViewId],
    scene: &Scene,
    scale_factor: f64,
    gpui_frame: [f64; 4],
    y_axis: YAxis,
) -> Layout {
    let mut attached = attached.into_iter().peekable();
    if attached.peek().is_none() {
        return Layout::default();
    }
    let placements = attached
        .filter_map(|(id, last)| {
            let placement = scene
                .native_views
                .iter()
                .find(|native_view| native_view.id == id)
                .and_then(|native_view| {
                    Placement::new(native_view, scale_factor, gpui_frame, y_axis)
                });
            (last != placement).then_some((id, placement))
        })
        .collect();
    // The scene's native views are sorted by draw order, so the ones painted
    // later, which belong in front, come last.
    let mut shown = Vec::new();
    for native_view in &scene.native_views {
        if !shown.contains(&native_view.id) {
            shown.push(native_view.id);
        }
    }
    Layout {
        placements,
        stacking: (shown != stacking).then_some(shown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ContentMask, HitboxId, Hsla, NativeViewId, point, size};

    /// Bounds in device pixels.
    fn scaled(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    #[test]
    fn placement_clips_a_view_to_its_content_mask() {
        // macOS's rows: a 400 by 300 point GPUI view at twice the scale, 10 points above the bottom of the content
        // view.
        let mac_frame = [0., 10., 400., 300.];
        // (name, bounds, content mask, scale, GPUI frame, y axis, placement)
        let cases = [
            (
                "macOS: unclipped",
                scaled(100., 40., 200., 400.),
                scaled(0., 0., 800., 600.),
                2.,
                mac_frame,
                YAxis::Up,
                Some(Placement {
                    container: [50., 90., 100., 200.],
                    view: [0., 0., 100., 200.],
                }),
            ),
            (
                // The container shrinks, and the view keeps its size and stays where it was in the window.
                "macOS: clipped on the top and left",
                scaled(100., 40., 200., 400.),
                scaled(120., 100., 800., 600.),
                2.,
                mac_frame,
                YAxis::Up,
                Some(Placement {
                    container: [60., 90., 90., 170.],
                    view: [-10., 0., 100., 200.],
                }),
            ),
            (
                "macOS: clipped away",
                scaled(100., 40., 200., 400.),
                scaled(0., 500., 800., 100.),
                2.,
                mac_frame,
                YAxis::Up,
                None,
            ),
            (
                "iOS: unclipped at 2x",
                scaled(20., 40., 200., 100.),
                scaled(0., 0., 1000., 1000.),
                2.,
                [0., 0., 500., 500.],
                YAxis::Down,
                Some(Placement {
                    container: [10., 20., 100., 50.],
                    view: [0., 0., 100., 50.],
                }),
            ),
            (
                "iOS: clipped on the top left at 3x",
                scaled(0., 0., 300., 300.),
                scaled(30., 60., 600., 600.),
                3.,
                [0., 0., 500., 500.],
                YAxis::Down,
                Some(Placement {
                    container: [10., 20., 90., 80.],
                    view: [-10., -20., 100., 100.],
                }),
            ),
            (
                "iOS: GPUI's view offset in the window",
                scaled(0., 0., 100., 100.),
                scaled(0., 0., 100., 100.),
                1.,
                [5., 7., 100., 100.],
                YAxis::Down,
                Some(Placement {
                    container: [5., 7., 100., 100.],
                    view: [0., 0., 100., 100.],
                }),
            ),
            (
                "iOS: clipped away",
                scaled(0., 0., 100., 100.),
                scaled(200., 200., 10., 10.),
                1.,
                [0., 0., 500., 500.],
                YAxis::Down,
                None,
            ),
        ];
        for (name, bounds, mask, scale, frame, y_axis, placement) in cases {
            let native_view = PaintNativeView {
                order: 0,
                id: NativeViewId::for_test(1),
                hitbox_id: HitboxId::placeholder(),
                bounds,
                content_mask: ContentMask { bounds: mask },
                background: Hsla::transparent_black(),
            };
            assert_eq!(
                Placement::new(&native_view, scale, frame, y_axis),
                placement,
                "{name}"
            );
        }
    }

    #[test]
    fn plan_layout_changes_only_what_the_scene_moves() {
        let painted = |id: u64, x: f32| PaintNativeView {
            order: 0,
            id: NativeViewId::for_test(id),
            hitbox_id: HitboxId::placeholder(),
            bounds: scaled(x, 0., 10., 10.),
            content_mask: ContentMask {
                bounds: scaled(0., 0., 100., 100.),
            },
            background: Hsla::transparent_black(),
        };
        let at = |x: f64| {
            Some(Placement {
                container: [x, 0., 10., 10.],
                view: [0., 0., 10., 10.],
            })
        };
        let one = NativeViewId::for_test(1);
        let two = NativeViewId::for_test(2);
        // (name, attached views with their last placement, last stacking, the scene's views, placements, stacking)
        let cases: [(&str, Vec<_>, Vec<_>, Vec<_>, Vec<_>, Option<Vec<_>>); 6] = [
            ("nothing attached", vec![], vec![], vec![painted(1, 0.)], vec![], None),
            ("unmoved", vec![(one, at(0.))], vec![one], vec![painted(1, 0.)], vec![], None),
            ("moved", vec![(one, at(0.))], vec![one], vec![painted(1, 5.)], vec![(one, at(5.))], None),
            ("shown", vec![(one, None)], vec![], vec![painted(1, 0.)], vec![(one, at(0.))], Some(vec![one])),
            ("hidden", vec![(one, at(0.))], vec![one], vec![], vec![(one, None)], Some(vec![])),
            (
                "restacked",
                vec![(one, at(0.)), (two, at(5.))],
                vec![one, two],
                vec![painted(2, 5.), painted(1, 0.)],
                vec![],
                Some(vec![two, one]),
            ),
        ];
        for (name, attached, stacking, shown, placements, restacked) in cases {
            let mut scene = Scene::default();
            scene.native_views = shown;
            let layout = plan_layout(attached, &stacking, &scene, 1., [0., 0., 100., 100.], YAxis::Down);
            assert_eq!(layout.placements, placements, "{name}");
            assert_eq!(layout.stacking, restacked, "{name}");
        }
    }
}
