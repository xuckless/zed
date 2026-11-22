//! Native views: the hole the renderer cuts where one shows. The platform composites the view
//! beneath the window (a Wayland subsurface, an Android SurfaceView, a DOM element under the
//! canvas); every wgpu target shares this hole.

use gpui::{PaintNativeView, Quad, ScaledPixels};

/// The holes of a scene's native views, as quads the hole shader reads with either instance
/// transport, each cut `inset` inside its view's bounds.
pub(super) fn holes(native_views: &[PaintNativeView], inset: ScaledPixels) -> Vec<Quad> {
    native_views
        .iter()
        .map(|native_view| Quad {
            order: native_view.order,
            bounds: native_view.bounds.inset(inset),
            content_mask: native_view.content_mask,
            ..Quad::default()
        })
        .collect()
}

/// No blending, so a hole replaces what was drawn beneath it (Metal's native view pipeline does
/// the same).
pub(super) fn color_target(format: wgpu::TextureFormat) -> wgpu::ColorTargetState {
    wgpu::ColorTargetState {
        format,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    }
}

/// A surface native views can't show through: the compositor doesn't blend it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeViewsUnsupported {
    /// The surface's alpha mode for a see-through window, the best it offers.
    pub alpha_mode: wgpu::CompositeAlphaMode,
}

impl std::fmt::Display for NativeViewsUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the window surface offers no premultiplied alpha mode, only {:?}, so native views \
             would show black through their holes",
            self.alpha_mode
        )
    }
}

impl std::error::Error for NativeViewsUnsupported {}

/// Why a surface can show native views: what blends the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeViewSupport {
    /// The compositor blends the surface by this premultiplied alpha mode.
    Surface(wgpu::CompositeAlphaMode),
    /// A WebGL2 canvas blends by its context's `premultipliedAlpha`, which browsers default to
    /// true and wgpu leaves alone, whatever surface mode wgpu reports (only `Opaque`).
    WebGl2Premultiplied,
}

/// Native views need a compositor that blends the window. NVIDIA's Vulkan WSI has been
/// known to offer only `Opaque` on Wayland, where every hole would show black.
/// `webgl2_canvas`: the surface is a browser's WebGL2 canvas (wasm on wgpu's GL backend).
pub(super) fn support(
    alpha_mode: wgpu::CompositeAlphaMode,
    webgl2_canvas: bool,
) -> Result<NativeViewSupport, NativeViewsUnsupported> {
    // (WebGL2 canvas, alpha mode) → what blends the window. Add a row per new mode.
    match (webgl2_canvas, alpha_mode) {
        (true, _) => Ok(NativeViewSupport::WebGl2Premultiplied),
        (false, wgpu::CompositeAlphaMode::PreMultiplied | wgpu::CompositeAlphaMode::Inherit) => {
            Ok(NativeViewSupport::Surface(alpha_mode))
        }
        (
            false,
            wgpu::CompositeAlphaMode::Auto
            | wgpu::CompositeAlphaMode::Opaque
            | wgpu::CompositeAlphaMode::PostMultiplied,
        ) => Err(NativeViewsUnsupported { alpha_mode }),
    }
}

/// Logged once when a window's first native view is attached.
pub(super) fn log_alpha_mode(support: Result<NativeViewSupport, NativeViewsUnsupported>) {
    match support {
        Ok(NativeViewSupport::Surface(alpha_mode)) => {
            log::info!("native views: window surface alpha mode {alpha_mode:?}")
        }
        Ok(NativeViewSupport::WebGl2Premultiplied) => {
            log::info!("native views: WebGL2 canvas, premultiplied alpha (the context's default)")
        }
        Err(unsupported) => log::warn!("native views: {unsupported}"),
    }
}
