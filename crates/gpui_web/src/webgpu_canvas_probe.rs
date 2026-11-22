//! A browser can hand out a working WebGPU device whose canvases never show what it draws (Chromium 158 with its
//! Vulkan feature on NVIDIA's Linux driver: every WebGPU canvas composites black). Only reading a canvas back tells,
//! so a test clear decides before a window commits to WebGPU (`WebBackendPreference::Auto` only).

use crate::events::EventListenerHandle;
use crate::window::WebWindow;
use gpui_wgpu::{PreparedWebGraphics, WebBackendPreference, WgpuContext, wgpu};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue};

/// Clears a detached 4x4 WebGPU canvas to red and reads it back through a 2D canvas in the same task, while the
/// clear is still the canvas's current texture. False when the read gives anything but opaque red.
pub(crate) fn shows_drawing(context: &WgpuContext) -> bool {
    const SIZE: u32 = 4;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    let Some(canvas) = document
        .create_element("canvas")
        .ok()
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
    else {
        return false;
    };
    canvas.set_width(SIZE);
    canvas.set_height(SIZE);
    let Ok(surface) = context
        .instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
    else {
        return false;
    };
    let Some(format) = surface
        .get_capabilities(&context.adapter)
        .formats
        .first()
        .copied()
    else {
        return false;
    };
    surface.configure(
        &context.device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: SIZE,
            height: SIZE,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: Vec::new(),
        },
    );
    let wgpu::CurrentSurfaceTexture::Success(frame) = surface.get_current_texture() else {
        return false;
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("webgpu_canvas_probe"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::RED),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    }));
    context.queue.submit([encoder.finish()]);
    context.queue.present(frame);
    let Some(reader) = web_sys::OffscreenCanvas::new(SIZE, SIZE)
        .ok()
        .and_then(|reader| reader.get_context("2d").ok().flatten())
        .and_then(|reader| {
            reader
                .dyn_into::<web_sys::OffscreenCanvasRenderingContext2d>()
                .ok()
        })
    else {
        return false;
    };
    if reader
        .draw_image_with_html_canvas_element(&canvas, 0.0, 0.0)
        .is_err()
    {
        return false;
    }
    reader
        .get_image_data(1.0, 1.0, 1.0, 1.0)
        .map(|pixel| matches!(pixel.data().0.as_slice(), [200..=255, 0..=40, 0..=40, 255]))
        .unwrap_or(false)
}

/// Tries WebGL2 on a new canvas, once more after the next animation frame if the first try fails, while keeping the
/// WebGPU device that drew nothing visible: a window that may show nothing still beats no window, so it is the answer
/// when WebGL2 fails twice (logged as an error).
pub(crate) async fn fall_back_to_webgl2(
    browser_window: &web_sys::Window,
    webgpu_canvas: web_sys::HtmlCanvasElement,
    webgpu: PreparedWebGraphics,
) -> anyhow::Result<(
    web_sys::HtmlCanvasElement,
    WgpuContext,
    wgpu::Surface<'static>,
)> {
    log::warn!(
        "WebGPU initialization failed; falling back to WebGL2: WebGPU draws nothing to a canvas here \
         (a test clear read back blank)"
    );
    let webgl = match try_webgl2(browser_window).await {
        Ok(webgl) => Ok(webgl),
        Err(error) => {
            // A context the browser refused while the page was still starting up can come at the next frame.
            log::warn!("WebGL2 initialization failed ({error:#}); trying again after the next animation frame");
            next_animation_frame(browser_window).await;
            try_webgl2(browser_window).await
        }
    };
    match webgl {
        Ok((canvas, PreparedWebGraphics { context, surface })) => {
            let element: &web_sys::Element = webgpu_canvas.as_ref();
            element.remove();
            Ok((canvas, context, surface))
        }
        Err(error) => {
            log::error!(
                "WebGL2 initialization failed too ({error:#}); staying on WebGPU, whose canvas test read back \
                 blank: this window may show nothing"
            );
            Ok((webgpu_canvas, webgpu.context, webgpu.surface))
        }
    }
}

/// One WebGL2 context on a new canvas; the canvas is removed again if the browser gives none.
async fn try_webgl2(
    browser_window: &web_sys::Window,
) -> anyhow::Result<(web_sys::HtmlCanvasElement, PreparedWebGraphics)> {
    let canvas = WebWindow::prepare_canvas(browser_window)?;
    // The browser says why it gave no context only in this event.
    let reasons = Rc::new(RefCell::new(Vec::new()));
    let _reasons_listener = EventListenerHandle::add(canvas.as_ref(), "webglcontextcreationerror", {
        let reasons = Rc::clone(&reasons);
        move |event| {
            let message = js_sys::Reflect::get(&event, &JsValue::from_str("statusMessage"))
                .ok()
                .and_then(|message| message.as_string())
                .unwrap_or_default();
            reasons.borrow_mut().push(message);
        }
    });
    match WgpuContext::new_web(&canvas, WebBackendPreference::WebGl).await {
        Ok(prepared) => Ok((canvas, prepared)),
        Err(error) => {
            let element: &web_sys::Element = canvas.as_ref();
            element.remove();
            Err(error.context(format!("webglcontextcreationerror: {:?}", reasons.borrow())))
        }
    }
}

/// Resolves at the browser's next animation frame (at once if it won't schedule one).
async fn next_animation_frame(browser_window: &web_sys::Window) {
    let frame = js_sys::Promise::new(&mut |resolve, _reject| {
        if browser_window.request_animation_frame(&resolve).is_err() {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(frame).await;
}
