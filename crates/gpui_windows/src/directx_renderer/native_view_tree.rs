//! The hole each native view shows through in GPUI's frame (docs/native-view-windows.md in Parallax), cleared in
//! paint order as the Metal renderer's `native_views` pipeline does.

use std::ops::Range;

use anyhow::{Context as _, Result};
use gpui::{Bounds, ScaledPixels, Scene};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use super::{DirectXRenderer, PipelineState, create_opaque_blend_state, shader_resources::ShaderModule};

/// The part of the swap chain a native view shows through: cleared to transparent in paint order, as the Metal
/// renderer's `native_views` pipeline does, so elements painted after the view still cover it.
#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct NativeViewHole {
    bounds: Bounds<ScaledPixels>,
    content_mask: Bounds<ScaledPixels>,
}

/// Writes with blending off at the batch's fixed depth, the depth test on, as every non-quad batch draws.
pub(super) fn hole_pipeline(device: &ID3D11Device) -> Result<PipelineState<NativeViewHole>> {
    PipelineState::new(
        device,
        "native_view_hole_pipeline",
        ShaderModule::NativeView,
        4,
        create_opaque_blend_state(device)?,
    )
}

impl DirectXRenderer {
    pub(super) fn upload_native_view_holes(&mut self, scene: &Scene) -> Result<()> {
        if scene.native_views.is_empty() {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        let holes: Vec<_> = scene
            .native_views
            .iter()
            .map(|native_view| NativeViewHole {
                bounds: native_view.bounds,
                content_mask: native_view.content_mask.bounds,
            })
            .collect();
        self.pipelines
            .native_view_holes
            .update_buffer(&devices.device, &devices.device_context, &holes)
    }

    /// Opaque quads painted after the view, which the opaque pass drew first, are nearer than the batch's depth and
    /// keep their pixels; everything painted before it is erased.
    pub(super) fn draw_native_view_holes(&mut self, range: Range<usize>) -> Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        self.pipelines.native_view_holes.draw_range(
            &devices.device_context,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            range.start as u32,
            range.len() as u32,
        )
    }
}
