//! GPUI's frame while native views show (docs/native-view-windows.md in Parallax): copied into a DirectComposition
//! surface, which changes only on `Commit`, so the frame, its holes and the views' placement reach the screen in the
//! same composition frame. With no view shown, the swap chain presents as upstream. `crate::native_views::present_step`
//! decides each frame's step; this file applies it.

use anyhow::{Context as _, Result};
use windows::Win32::{
    Foundation::POINT,
    Graphics::{
        Direct3D11::{ID3D11DeviceContext, ID3D11Texture2D},
        DirectComposition::{IDCompositionDevice, IDCompositionSurface, IDCompositionVisual},
        Dxgi::{Common::DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_PRESENT},
    },
};

use super::{DirectXResources, RENDER_TARGET_FORMAT};
use crate::native_views::{PresentStep, Showing, present_step};

/// What GPUI's own visual shows (`Showing`, with the surface).
pub(super) enum Presenting {
    /// GPUI's swap chain.
    SwapChain,
    /// `surface`, `size` device px (the swap chain's size when it was made), which each GPUI frame is copied into.
    Surface {
        surface: IDCompositionSurface,
        size: [u32; 2],
    },
    /// Still `surface`, the frame after the last view went (`Showing::LeavingSurface`).
    LeavingSurface {
        surface: IDCompositionSurface,
        size: [u32; 2],
    },
}

impl Presenting {
    fn showing(&self) -> Showing {
        match self {
            Self::SwapChain => Showing::SwapChain,
            Self::Surface { .. } => Showing::Surface,
            Self::LeavingSurface { .. } => Showing::LeavingSurface,
        }
    }

    /// Shows this frame, rendered into the swap chain's back buffer, by `present_step`. Returns the step, which says
    /// whether it needs a `Commit`.
    pub(super) fn show(
        &mut self,
        shows_views: bool,
        comp_device: &IDCompositionDevice,
        comp_visual: &IDCompositionVisual,
        resources: &DirectXResources,
        context: &ID3D11DeviceContext,
        size: [u32; 2],
    ) -> Result<PresentStep> {
        let (step, next) = present_step(self.showing(), shows_views);
        let present = || {
            // SAFETY: the swap chain is live and its back buffer holds this frame.
            unsafe { resources.swap_chain.Present(0, DXGI_PRESENT(0)) }
                .ok()
                .context("Presenting swap chain failed")
        };
        let old = std::mem::replace(self, Self::SwapChain);
        *self = match (step, next) {
            (PresentStep::Present, _) => {
                present()?;
                Self::SwapChain
            }
            (PresentStep::PresentAndShowSwapChain, _) => {
                present()?;
                log::info!("native views: GPUI's frame presents through its swap chain again");
                // SAFETY: the visual belongs to this device and the swap chain is live.
                unsafe { comp_visual.SetContent(&resources.swap_chain) }?;
                Self::SwapChain
            }
            (PresentStep::CopyToSurface | PresentStep::CopyToSurfaceAndPresent, next) => {
                let surface = match old {
                    Self::Surface { surface, size: made } | Self::LeavingSurface { surface, size: made }
                        if made == size =>
                    {
                        surface
                    }
                    Self::SwapChain | Self::Surface { .. } | Self::LeavingSurface { .. } => {
                        if let Self::SwapChain = old {
                            log::info!(
                                "native views: GPUI's frame shows through a composition surface, alpha mode Premultiplied"
                            );
                        }
                        let [width, height] = size;
                        // SAFETY: plain creation on a live device; the visual belongs to it, and what it shows
                        // changes only on the commit after this frame is drawn into the surface.
                        unsafe {
                            let surface = comp_device.CreateSurface(
                                width,
                                height,
                                RENDER_TARGET_FORMAT,
                                DXGI_ALPHA_MODE_PREMULTIPLIED,
                            )?;
                            comp_visual.SetContent(&surface)?;
                            surface
                        }
                    }
                };
                copy(&surface, resources, context)?;
                if step == PresentStep::CopyToSurfaceAndPresent {
                    present()?;
                }
                match next {
                    Showing::LeavingSurface => Self::LeavingSurface { surface, size },
                    Showing::Surface | Showing::SwapChain => Self::Surface { surface, size },
                }
            }
        };
        Ok(step)
    }
}

/// Copies the frame in the swap chain's back buffer into `surface`.
fn copy(
    surface: &IDCompositionSurface,
    resources: &DirectXResources,
    context: &ID3D11DeviceContext,
) -> Result<()> {
    let render_target = resources
        .render_target
        .as_ref()
        .context("missing render target")?;
    let mut offset = POINT::default();
    // SAFETY: a whole-surface update (no rect) is filled entirely by the copy of a back buffer of the surface's
    // size; both textures are on GPUI's device and share RENDER_TARGET_FORMAT.
    unsafe {
        let texture: ID3D11Texture2D = surface.BeginDraw(None, &mut offset)?;
        context.CopySubresourceRegion(
            &texture,
            0,
            offset.x as u32,
            offset.y as u32,
            0,
            render_target,
            0,
            None,
        );
        surface.EndDraw()?;
    }
    log::debug!("native views: GPUI frame copied into the surface");
    Ok(())
}
