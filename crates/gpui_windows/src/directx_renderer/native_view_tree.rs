//! Native views in the renderer's DirectComposition tree (docs/native-view-windows.md in Parallax). Each view is the
//! app's content (a composition swap chain or anything `IDCompositionVisual::SetContent` takes) in a subtree under
//! GPUI's own swap chain visual, shown through a hole the renderer clears in paint order. What to show is decided on
//! plain values in `crate::native_views`; this file applies it.
//!
//! Modelled on Chromium's ui/gl/dc_layer_tree.cc @ 4fc26f32cd03f0e8443c4aa11d6859a901d90659 (`VisualTree::BuildTree`,
//! `VisualSubtree::Update`, `CommitAndClearPendingOverlays`). Copyright 2019 The Chromium Authors. BSD-3-Clause, see
//! LICENSE-CHROMIUM.

use std::{
    any::Any,
    cell::RefCell,
    ops::Range,
    rc::{Rc, Weak},
};

use anyhow::{Context as _, Result};
use gpui::{Bounds, NativeViewId, ScaledPixels, Scene, WindowBackgroundAppearance};
use gpui_util::ResultExt as _;
use windows::{
    Win32::Graphics::{
        Direct3D11::ID3D11Device,
        DirectComposition::{
            DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, DCOMPOSITION_BORDER_MODE_HARD,
            IDCompositionDevice, IDCompositionRectangleClip, IDCompositionVisual,
        },
        Dxgi::{DXGI_PRESENT, IDXGISwapChain1},
    },
    core::IUnknown,
};

use super::{
    DirectComposition, DirectXRenderer, DirectXRendererDevices, PipelineState,
    create_opaque_blend_state, create_render_target_and_its_view,
    create_swap_chain_for_composition, shader_resources::ShaderModule,
};
use crate::WindowsWindowInner;
use super::native_view_surface::Presenting;
use crate::native_views::{Frame, Placement, PresentStep};

/// What the renderer keeps of its native views across devices.
#[derive(Default)]
pub(super) struct NativeViews {
    /// The content of every attached view, as the app gave it; a lost device's new composition device shows it again.
    contents: Vec<(NativeViewId, IUnknown)>,
    /// Views whose last handle was dropped while the renderer was borrowed (it draws), taken out of the tree when the
    /// next frame starts. Shared with each `NativeViewAttachment`.
    detached: Rc<RefCell<Vec<NativeViewId>>>,
}

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

/// How far the 1×1 background swap chain is stretched: past any window's size, so it never needs resizing.
const BACKGROUND_SCALE: f32 = 32768.;

/// The views' part of the window's visual tree, under GPUI's swap chain visual: a background visual at the bottom,
/// then a container of one subtree per view, back to front.
pub(super) struct NativeViewTree {
    views_visual: IDCompositionVisual,
    /// Shows [`Frame::background`] under the views: a 1×1 swap chain cleared to the colour and stretched over the
    /// window, as Chromium's `VisualSubtree` fills a background colour from a small solid colour surface.
    background_visual: IDCompositionVisual,
    background: Option<IDXGISwapChain1>,
    views: Vec<(NativeViewId, ViewVisuals)>,
    /// What the tree shows now; a frame that shows the same changes nothing and commits nothing.
    shown: Frame,
    /// What GPUI's own visual shows: its swap chain, or while views show, the surface its frames are copied into.
    pub(super) presenting: Presenting,
}

/// One view's subtree, after Chromium's `VisualSubtree`: the clip sits on an untransformed parent, and the content
/// visual is offset to the view's origin, so the app's content shows 1:1 in device pixels.
struct ViewVisuals {
    clip_visual: IDCompositionVisual,
    clip: IDCompositionRectangleClip,
    content_visual: IDCompositionVisual,
    /// Where the visuals were last placed; kept while the view is hidden, so showing it again in place sets nothing.
    placement: Option<Placement>,
}

impl NativeViewTree {
    /// Adds the views' visuals to the bottom of `root`, below whatever is added after.
    pub(super) fn new(comp_device: &IDCompositionDevice, root: &IDCompositionVisual) -> Result<Self> {
        // SAFETY: both visuals are created on `comp_device`, as `root` was, and each is added to it once.
        unsafe {
            let background_visual = comp_device.CreateVisual()?;
            let views_visual = comp_device.CreateVisual()?;
            // Each above the one before it, the first at the bottom, as Chromium's BuildTree stacks its subtrees.
            root.AddVisual(&background_visual, true, None)?;
            root.AddVisual(&views_visual, true, &background_visual)?;
            background_visual.SetTransform2(&windows_numerics::Matrix3x2::scale(
                BACKGROUND_SCALE,
                BACKGROUND_SCALE,
            ))?;
            background_visual
                .SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR)?;
            background_visual.SetBorderMode(DCOMPOSITION_BORDER_MODE_HARD)?;
            Ok(Self {
                views_visual,
                background_visual,
                background: None,
                views: Vec::new(),
                shown: Frame::default(),
                // `DirectComposition::set_swap_chain` shows the swap chain on a new device.
                presenting: Presenting::SwapChain,
            })
        }
    }

    fn attach(&mut self, comp_device: &IDCompositionDevice, id: NativeViewId, content: &IUnknown) -> Result<()> {
        // SAFETY: the visuals and clip are created on this device; `content` is a live COM object the caller holds.
        let visuals = unsafe {
            let clip_visual = comp_device.CreateVisual()?;
            let clip = comp_device.CreateRectangleClip()?;
            let content_visual = comp_device.CreateVisual()?;
            clip_visual.SetClip(&clip)?;
            clip_visual.AddVisual(&content_visual, false, None)?;
            content_visual
                .SetContent(content)
                .context("the native view's content is not something DirectComposition shows")?;
            ViewVisuals {
                clip_visual,
                clip,
                content_visual,
                placement: None,
            }
        };
        self.views.push((id, visuals));
        Ok(())
    }

    /// Takes the view out of the tree at once, as a dropped `NSView` leaves its window, without waiting for a frame.
    fn detach(&mut self, comp_device: &IDCompositionDevice, id: NativeViewId) -> Result<()> {
        let Some(index) = self.views.iter().position(|(view, _)| *view == id) else {
            return Ok(());
        };
        let (_, visuals) = self.views.remove(index);
        // SAFETY: the visuals belong to this device; a shown view's clip visual is a child of `views_visual`.
        unsafe {
            if self.shown.stacking().any(|shown| shown == id) {
                self.shown.views.retain(|(view, _)| *view != id);
                self.views_visual.RemoveVisual(&visuals.clip_visual)?;
            }
            visuals.content_visual.SetContent(None)?;
            comp_device.Commit()?;
        }
        Ok(())
    }

    /// Changes the tree to show `frame`, after Chromium's `VisualTree::BuildTree`: restacks only when the views or
    /// their order change and places only the views that moved. Returns whether the frame places views (moves,
    /// resizes, shows, hides or restacks one, as macOS's `Layout::places_views`) and so needs a commit.
    fn place_views(&mut self, frame: Frame, devices: &DirectXRendererDevices) -> Result<bool> {
        if frame == self.shown {
            return Ok(false);
        }
        if frame.background != self.shown.background {
            self.set_background(frame.background, devices)?;
        }
        if !frame.stacking().eq(self.shown.stacking()) {
            // SAFETY: the clip visuals belong to this device and are added to `views_visual` once each.
            unsafe { self.views_visual.RemoveAllVisuals() }?;
            let mut below: Option<&IDCompositionVisual> = None;
            for id in frame.stacking() {
                let Some((_, visuals)) = self.views.iter().find(|(view, _)| *view == id) else {
                    continue;
                };
                unsafe { self.views_visual.AddVisual(&visuals.clip_visual, true, below) }?;
                below = Some(&visuals.clip_visual);
            }
        }
        for (id, placement) in &frame.views {
            let Some((_, visuals)) = self.views.iter_mut().find(|(view, _)| view == id) else {
                continue;
            };
            if visuals.placement == Some(*placement) {
                continue;
            }
            let [left, top, right, bottom] = placement.clip;
            let [x, y] = placement.origin;
            // SAFETY: the clip and visual belong to this device.
            unsafe {
                visuals.clip.SetLeft2(left)?;
                visuals.clip.SetTop2(top)?;
                visuals.clip.SetRight2(right)?;
                visuals.clip.SetBottom2(bottom)?;
                visuals.content_visual.SetOffsetX2(x)?;
                visuals.content_visual.SetOffsetY2(y)?;
            }
            visuals.placement = Some(*placement);
        }
        log::debug!("native views: showing {:?}", frame.views);
        self.shown = frame;
        Ok(true)
    }

    fn set_background(&mut self, color: Option<[f32; 3]>, devices: &DirectXRendererDevices) -> Result<()> {
        let Some([r, g, b]) = color else {
            // SAFETY: the visual belongs to this device.
            unsafe { self.background_visual.SetContent(None) }?;
            self.background = None;
            return Ok(());
        };
        let swap_chain = match self.background.take() {
            Some(swap_chain) => swap_chain,
            None => create_swap_chain_for_composition(&devices.dxgi_factory, &devices.device, 1, 1)?,
        };
        let (_, render_target_view) = create_render_target_and_its_view(&swap_chain, &devices.device)?;
        // SAFETY: the view is of this swap chain's current back buffer, on GPUI's device and context.
        unsafe {
            devices.device_context.ClearRenderTargetView(
                render_target_view
                    .as_ref()
                    .context("missing background render target view")?,
                &[r, g, b, 1.],
            );
            swap_chain.Present(0, DXGI_PRESENT(0)).ok()?;
            self.background_visual.SetContent(&swap_chain)?;
        }
        self.background = Some(swap_chain);
        Ok(())
    }
}

impl DirectComposition {
    /// Shows every attached view's content again, on a new device after the old one was lost.
    pub(super) fn reattach_native_views(&mut self, native_views: &NativeViews) -> Result<()> {
        for (id, content) in &native_views.contents {
            self.view_tree.attach(&self.comp_device, *id, content)?;
        }
        Ok(())
    }
}

/// Keeps a native view in its window's visual tree; dropping it takes the view out and releases its content.
struct NativeViewAttachment {
    window: Weak<WindowsWindowInner>,
    id: NativeViewId,
    /// The renderer's [`NativeViews::detached`].
    detached: Rc<RefCell<Vec<NativeViewId>>>,
}

impl Drop for NativeViewAttachment {
    fn drop(&mut self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        // At once, as a dropped `NSView` leaves its window; while the window draws (app code it runs, such as a
        // device-changed callback, may drop a view), at the start of the next frame.
        match window.state.renderer.try_borrow_mut() {
            Ok(mut renderer) => {
                renderer.detach_native_view(self.id).log_err();
            }
            Err(_) => self.detached.borrow_mut().push(self.id),
        }
    }
}

impl DirectXRenderer {
    /// Shows `content` under the window's GPUI content once a scene paints the native view `id`. The attachment
    /// returned keeps it there.
    pub(crate) fn attach_native_view(
        &mut self,
        id: NativeViewId,
        content: IUnknown,
        window: &Rc<WindowsWindowInner>,
    ) -> Result<Box<dyn Any>> {
        let composition = self
            .direct_composition
            .as_mut()
            .context("native views need DirectComposition, which GPUI_DISABLE_DIRECT_COMPOSITION turned off")?;
        composition.view_tree.attach(&composition.comp_device, id, &content)?;
        self.native_views.contents.push((id, content));
        Ok(Box::new(NativeViewAttachment {
            window: Rc::downgrade(window),
            id,
            detached: self.native_views.detached.clone(),
        }))
    }

    fn detach_native_view(&mut self, id: NativeViewId) -> Result<()> {
        self.native_views.contents.retain(|(view, _)| *view != id);
        let Some(composition) = &mut self.direct_composition else {
            return Ok(());
        };
        composition.view_tree.detach(&composition.comp_device, id)
    }

    /// Before a frame renders: takes out the views dropped while the renderer was borrowed, and works out what the
    /// frame shows of the views.
    pub(super) fn prepare_native_views(
        &mut self,
        scene: &Scene,
        appearance: WindowBackgroundAppearance,
    ) -> Result<Frame> {
        let detached = std::mem::take(&mut *self.native_views.detached.borrow_mut());
        for id in detached {
            self.detach_native_view(id)?;
        }
        Ok(Frame::new(&scene.native_views, appearance))
    }

    /// After a frame renders: shows it (`Presenting::show`), then changes the visual tree and commits once, after
    /// Chromium's DCLayerTree, which presents its swap chains, then builds and commits the tree. Through the surface,
    /// the frame, its holes and the views' new placement change in that one commit. A swap chain frame that moves no
    /// view commits nothing, and the views' own presents never need GPUI: DWM picks up a swap chain's present by
    /// itself.
    pub(super) fn present_native_views(&mut self, frame: Frame) -> Result<()> {
        let Some(composition) = &mut self.direct_composition else {
            return self.present();
        };
        let devices = self.devices.as_ref().context("devices missing")?;
        let resources = self.resources.as_ref().context("resources missing")?;
        let step = composition.view_tree.presenting.show(
            !frame.views.is_empty(),
            &composition.comp_device,
            &composition.comp_visual,
            resources,
            &devices.device_context,
            [self.width, self.height],
        )?;
        let placed = composition.view_tree.place_views(frame, devices)?;
        // step → whether it changes what DirectComposition shows. Only a plain present doesn't.
        let commits = match step {
            PresentStep::Present => placed,
            PresentStep::CopyToSurface
            | PresentStep::CopyToSurfaceAndPresent
            | PresentStep::PresentAndShowSwapChain => true,
        };
        if commits {
            // SAFETY: plain call on a live device.
            unsafe { composition.comp_device.Commit() }?;
        }
        Ok(())
    }

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
