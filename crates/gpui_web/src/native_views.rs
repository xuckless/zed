// `render_content`, `ensure_content_correctly_sized`, `create_slot`, `reset_anchor` and `detach_native_view` are
// ported from flutter/flutter engine/src/flutter/lib/web_ui/lib/src/engine/platform_views/content_manager.dart,
// slots.dart and embedder.dart (`_resetAnchor`, `disposeView`) @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

//! Native views on the web: the app's DOM element shown under GPUI's canvas through the renderer's hole
//! (gpui_wgpu's `native_view_hole`), placed in the frame that cuts it.
//!
//! DOM, inserted before the canvas, which is raised above it (`position: relative; z-index: 1`) while views exist:
//!
//! ```html
//! <gpui-native-views>                       the canvas's box
//!   <div>                                   slots, in paint order
//!     <gpui-native-view-slot>               the view's bounds clipped to its content mask
//!       <gpui-native-view>                  the view's bounds
//!         <the app's element />
//! ```

use crate::native_view_layout::{self, Placement};
use crate::window::WebWindowInner;
use gpui::{Bounds, NativeViewId, Pixels, Rgba, Scene, native_view_placement, point, px, size};
use std::{
    any::Any,
    cell::RefCell,
    rc::{Rc, Weak},
};
use wasm_bindgen::JsCast;

#[derive(Default)]
pub(crate) struct NativeViews {
    /// Exists while views are attached: the canvas is raised only then.
    layer: RefCell<Option<Layer>>,
}

struct Layer {
    root: web_sys::HtmlElement,
    slots_element: web_sys::HtmlElement,
    slots: Vec<Slot>,
    /// The translation and size last given to `root`, which follow the canvas's box.
    root_box: Bounds<Pixels>,
    /// The canvas's own values of the styles the layer changes, put back when the last view goes.
    canvas_styles: Vec<(&'static str, String)>,
}

struct Slot {
    id: NativeViewId,
    slot: web_sys::HtmlElement,
    wrapper: web_sys::HtmlElement,
    /// Where it was last placed, its rank in paint order and the CSS colour under it; `None` while hidden.
    placed: Option<(Placement, usize, String)>,
}

/// Removes its view's slot from the document when the last `NativeView` handle goes.
struct Attachment {
    id: NativeViewId,
    window: Weak<WebWindowInner>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        if let Some(window) = self.window.upgrade() {
            window.detach_native_view(self.id);
        }
    }
}

/// The canvas styles the layer sets while views exist.
const CANVAS_STYLES: [(&str, &str); 2] = [("position", "relative"), ("z-index", "1")];

fn set_styles(element: &web_sys::HtmlElement, styles: &[(&str, &str)]) {
    let style = element.style();
    for (property, value) in styles {
        if let Err(error) = style.set_property(property, value) {
            log::warn!("native views: cannot set {property}: {error:?}");
        }
    }
}

fn create(document: &web_sys::Document, tag: &str) -> Option<web_sys::HtmlElement> {
    document
        .create_element(tag)
        .ok()
        .and_then(|element| element.dyn_into().ok())
}

/// Flutter's `_resetAnchor`: transforms place the element from its top left.
fn reset_anchor(element: &web_sys::HtmlElement) {
    set_styles(
        element,
        &[
            ("transform-origin", "0 0 0"),
            ("position", "absolute"),
            ("left", "0"),
            ("top", "0"),
        ],
    );
}

fn place(element: &web_sys::HtmlElement, bounds: Bounds<Pixels>) {
    let css = |value: Pixels| format!("{}px", f32::from(value));
    set_styles(
        element,
        &[
            (
                "transform",
                &format!(
                    "translate({}, {})",
                    css(bounds.origin.x),
                    css(bounds.origin.y)
                ),
            ),
            ("width", &css(bounds.size.width)),
            ("height", &css(bounds.size.height)),
        ],
    );
}

/// Flutter's `_ensureContentCorrectlySized`: fills the slot unless the app sized the element.
fn ensure_content_correctly_sized(content: &web_sys::HtmlElement) {
    let style = content.style();
    for property in ["height", "width"] {
        if style
            .get_property_value(property)
            .unwrap_or_default()
            .is_empty()
        {
            log::warn!(
                "native views: {property} of the view's element is not set; defaulting to 100%"
            );
            set_styles(content, &[(property, "100%")]);
        }
    }
}

impl WebWindowInner {
    pub(crate) fn attach_native_view(
        self: &Rc<Self>,
        id: NativeViewId,
        content: web_sys::HtmlElement,
    ) -> Option<Box<dyn Any>> {
        let document = self.browser_window.document()?;
        if self.native_views.layer.borrow().is_none() {
            let layer = self.create_layer(&document)?;
            *self.native_views.layer.borrow_mut() = Some(layer);
        }
        let slot = self.render_content(&document, id, content)?;
        self.native_views
            .layer
            .borrow_mut()
            .as_mut()?
            .slots
            .push(slot);
        Some(Box::new(Attachment {
            id,
            window: Rc::downgrade(self),
        }))
    }

    fn create_layer(self: &Rc<Self>, document: &web_sys::Document) -> Option<Layer> {
        let root = create(document, "gpui-native-views")?;
        let slots_element = create(document, "div")?;
        reset_anchor(&root);
        set_styles(&root, &[("z-index", "0"), ("overflow", "hidden")]);
        set_styles(
            &slots_element,
            &[("position", "absolute"), ("inset", "0"), ("z-index", "0")],
        );
        root.append_child(&slots_element).ok()?;
        self.canvas
            .parent_node()?
            .insert_before(&root, Some(&self.canvas))
            .ok()?;
        let style = self.canvas.style();
        let canvas_styles = CANVAS_STYLES
            .iter()
            .map(|&(property, _)| {
                (
                    property,
                    style.get_property_value(property).unwrap_or_default(),
                )
            })
            .collect();
        set_styles(&self.canvas, &CANVAS_STYLES);

        Some(Layer {
            root,
            slots_element,
            slots: Vec::new(),
            root_box: Bounds::default(),
            canvas_styles,
        })
    }

    /// Flutter's `renderContent` and `createPlatformViewSlot`. Flutter's slot reveals the content through a
    /// `<slot>` in its shadow root; the canvas here has none, so the slot holds the content's wrapper itself.
    fn render_content(
        &self,
        document: &web_sys::Document,
        id: NativeViewId,
        content: web_sys::HtmlElement,
    ) -> Option<Slot> {
        let slot = create(document, "gpui-native-view-slot")?;
        set_styles(&slot, &[("overflow", "hidden"), ("display", "none")]);
        reset_anchor(&slot);
        let wrapper = create(document, "gpui-native-view")?;
        reset_anchor(&wrapper);
        ensure_content_correctly_sized(&content);
        wrapper.append_child(&content).ok()?;
        slot.append_child(&wrapper).ok()?;
        self.native_views
            .layer
            .borrow()
            .as_ref()?
            .slots_element
            .append_child(&slot)
            .ok()?;
        Some(Slot {
            id,
            slot,
            wrapper,
            placed: None,
        })
    }

    /// Flutter's `clearPlatformView` / `disposeView`: the slot leaves the document with the content in it.
    /// The last view takes the layer with it: the canvas gets its own styles back.
    fn detach_native_view(&self, id: NativeViewId) {
        let slot = {
            let mut layer = self.native_views.layer.borrow_mut();
            let Some(layer) = layer.as_mut() else {
                return;
            };
            let Some(index) = layer.slots.iter().position(|slot| slot.id == id) else {
                return;
            };
            layer.slots.remove(index)
        };
        slot.slot.remove();
        let mut layer = self.native_views.layer.borrow_mut();
        let Some(current) = layer.as_mut() else {
            return;
        };
        if current.slots.is_empty() {
            current.root.remove();
            let restore: Vec<(&str, &str)> = current
                .canvas_styles
                .iter()
                .map(|(property, value)| (*property, value.as_str()))
                .collect();
            set_styles(&self.canvas, &restore);
            *layer = None;
        }
    }

    /// Places every view as `scene` shows it, before the scene is drawn: the DOM changes and the canvas reach the
    /// screen in the same browser frame. A view whose placement is unchanged is not touched.
    pub(crate) fn place_native_views(&self, scene: &Scene) {
        let mut layer = self.native_views.layer.borrow_mut();
        let Some(layer) = layer.as_mut() else {
            return;
        };
        // The root follows the canvas's box wherever the page puts it: its translation moves by how far the two
        // boxes are apart (fractional, as the browser lays them out), whatever the root's containing block is.
        let canvas = self.canvas.get_bounding_client_rect();
        let root = layer.root.get_bounding_client_rect();
        let root_box = Bounds::new(
            layer.root_box.origin
                + point(
                    px((canvas.left() - root.left()) as f32),
                    px((canvas.top() - root.top()) as f32),
                ),
            size(px(canvas.width() as f32), px(canvas.height() as f32)),
        );
        if root_box != layer.root_box {
            place(&layer.root, root_box);
            layer.root_box = root_box;
        }
        let scale_factor = self.state.borrow().scale_factor;
        // `NativeView::set_background`: shows where an element has not caught up with its slot, as a cross-origin
        // iframe's own process does for a frame after a resize. `None`: the page behind the canvas shows there.
        let background = native_view_placement::background(&scene.native_views)
            .map(|Rgba { r, g, b, a }| {
                format!("rgba({}, {}, {}, {a})", r * 255., g * 255., b * 255.)
            })
            .unwrap_or_default();
        for slot in layer.slots.iter_mut() {
            let shown = scene
                .native_views
                .iter()
                .enumerate()
                .find(|(_, native_view)| native_view.id == slot.id)
                .and_then(|(rank, native_view)| {
                    native_view_layout::placement(native_view, scale_factor)
                        .map(|placement| (placement, rank, background.clone()))
                });
            if shown == slot.placed {
                continue;
            }
            match &shown {
                Some((placement, rank, background)) => {
                    place(&slot.slot, placement.slot);
                    place(&slot.wrapper, placement.view);
                    set_styles(
                        &slot.slot,
                        &[
                            ("display", ""),
                            ("z-index", &rank.to_string()),
                            ("background-color", background),
                        ],
                    );
                }
                None => set_styles(&slot.slot, &[("display", "none")]),
            }
            slot.placed = shown;
        }
    }
}
