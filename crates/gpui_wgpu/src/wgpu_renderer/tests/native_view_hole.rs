use super::*;
use gpui::{PlatformHeadlessRenderer, Scene};

/// A native view's hole replaces what was painted beneath it with transparent black,
/// within its clip, and what is painted after it covers it, as Metal's does.
#[cfg(target_os = "linux")]
#[test]
fn native_view_hole_is_cut_in_paint_order() -> anyhow::Result<()> {
    const HOLE: [u8; 4] = [0, 0, 0, 0];
    let view_bounds = Bounds {
        origin: Point {
            x: 16.0.into(),
            y: 0.0.into(),
        },
        size: Size {
            width: 32.0.into(),
            height: 32.0.into(),
        },
    };
    let mut clip = view_bounds;
    clip.size.width = 24.0.into();
    // (row name, hole inset, pixel, expected colour). Add a row per rule.
    let rows: [(&str, f32, (u32, u32), [u8; 4]); 7] = [
        ("outside the view keeps the window", 0., (8, 16), RED),
        (
            "opaque quad painted after covers the hole",
            0.,
            (20, 16),
            BLUE,
        ),
        ("opaque quad painted before is erased", 0., (30, 2), HOLE),
        (
            "translucent quad painted before is erased",
            0.,
            (30, 16),
            HOLE,
        ),
        ("the view's clip bounds the hole", 0., (44, 16), RED),
        (
            "an inset hole leaves its edge to the window",
            1.,
            (30, 0),
            RED,
        ),
        ("an inset hole is cut inside its edge", 1., (30, 1), HOLE),
    ];
    let mut renderer = WgpuHeadlessRenderer::new()?;
    for (name, inset, (x, y), expected) in rows {
        let mut scene = Scene::default();
        scene.insert_primitive(solid_quad(0.0, 0.0, 64.0, 32.0, gpui::red()));
        let mut translucent = gpui::green();
        translucent.a = 0.5;
        scene.insert_primitive(solid_quad(24.0, 8.0, 16.0, 16.0, translucent));
        scene.insert_primitive(gpui::PaintNativeView {
            order: 0,
            id: gpui::NativeViewId::for_test(1),
            hitbox_id: gpui::HitboxId::placeholder(),
            bounds: view_bounds,
            content_mask: ContentMask { bounds: clip },
            background: Hsla::default(),
        });
        scene.insert_primitive(solid_quad(16.0, 0.0, 8.0, 32.0, gpui::blue()));
        scene.native_views_attached = true;
        scene.finish();

        renderer.core.native_view_hole_inset = ScaledPixels(inset);
        let image = renderer.render_scene_to_image(&scene, device_size(64, 32))?;
        let actual = image.get_pixel(x, y).0;
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= 3),
            "{name}: pixel ({x}, {y}) was {actual:?}, expected {expected:?}"
        );
    }
    Ok(())
}

/// The hole pipeline exists only while the window has native views attached, so a window
/// without views never pays for it, and a view that hides and shows again rebuilds nothing.
#[cfg(target_os = "linux")]
#[test]
fn hole_pipeline_exists_only_while_native_views_are_attached() -> anyhow::Result<()> {
    // (row name, views attached in the frame's scene, hole pipeline after the frame)
    let rows = [
        ("a window without views never builds it", false, false),
        ("the first attached view builds it", true, true),
        ("a frame where the view is hidden keeps it", true, true),
        ("the last view dropped drops it", false, false),
    ];
    let mut renderer = WgpuHeadlessRenderer::new()?;
    for (name, attached, expected) in rows {
        let mut scene = Scene::default();
        scene.insert_primitive(solid_quad(0.0, 0.0, 8.0, 8.0, gpui::red()));
        scene.native_views_attached = attached;
        scene.finish();
        renderer.render_scene(&scene, device_size(8, 8))?;
        assert_eq!(
            renderer
                .core
                .resources
                .pipelines
                .native_view_holes
                .is_some(),
            expected,
            "{name}"
        );
    }
    Ok(())
}

/// Native views show only where something blends the window; the alpha log names what.
#[test]
fn native_view_support_names_what_blends_the_window() {
    use crate::wgpu_renderer::native_view_hole::{
        NativeViewSupport, NativeViewsUnsupported, support,
    };
    use wgpu::CompositeAlphaMode::{Inherit, Opaque, PostMultiplied, PreMultiplied};
    // (row name, WebGL2 canvas, surface alpha mode, expected). Add a row per rule.
    let rows = [
        (
            "premultiplied surface blends",
            false,
            PreMultiplied,
            Ok(NativeViewSupport::Surface(PreMultiplied)),
        ),
        (
            "inherit surface blends",
            false,
            Inherit,
            Ok(NativeViewSupport::Surface(Inherit)),
        ),
        (
            "opaque surface can't show views",
            false,
            Opaque,
            Err(NativeViewsUnsupported { alpha_mode: Opaque }),
        ),
        (
            "postmultiplied surface can't show views",
            false,
            PostMultiplied,
            Err(NativeViewsUnsupported {
                alpha_mode: PostMultiplied,
            }),
        ),
        (
            "WebGL2 canvas reporting opaque is premultiplied",
            true,
            Opaque,
            Ok(NativeViewSupport::WebGl2Premultiplied),
        ),
    ];
    for (name, webgl2_canvas, alpha_mode, expected) in rows {
        assert_eq!(support(alpha_mode, webgl2_canvas), expected, "{name}");
    }
}
