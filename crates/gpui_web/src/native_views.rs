// `render_content`, `ensure_content_correctly_sized`, `create_slot`, `reset_anchor` and `detach_native_view` are
// ported from flutter/flutter engine/src/flutter/lib/web_ui/lib/src/engine/platform_views/content_manager.dart,
// slots.dart and embedder.dart (`_resetAnchor`, `disposeView`) @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

//! Native views on the web: the app's DOM element shown under GPUI's canvas through the renderer's hole
//! (gpui_wgpu's `native_view_hole`), placed in the frame that cuts it, with the browser giving pointer input over an
//! uncovered view to the element itself ([`gpui::NativeViewInput::View`], as on macOS).
//!
//! DOM, inserted before the canvas, which is raised above it (`position: relative; z-index: 1`) while views exist:
//!
//! ```html
//! <gpui-native-views>                       the canvas's box; takes the pointer the canvas lets through
//!   <div>                                   slots, in paint order
//!     <gpui-native-view-slot>               the view's bounds clipped to its content mask
//!       <gpui-native-view>                  the view's bounds
//!         <the app's element />
//!   <div>                                   shields: where GPUI's content takes input over a view
//! ```
//!
//! The canvas takes the pointer except while it is over an uncovered view (`native_view_layout::step`, decided by a
//! capture listener on the document before any element sees the event): then it is `pointer-events: none` and the
//! browser hit-tests what is under it. A press, move or scroll that lands on the layer or a shield is GPUI's, and a
//! copy goes to the canvas. Events on the rest of the page are left alone.

use crate::events::{EventListenerHandle, Phase};
use crate::native_view_layout::{self, Hit, Over, Placement, PointerKind, PointerTarget, Send};
use crate::window::WebWindowInner;
use gpui::{
    Bounds, NativeViewId, Pixels, Point, Rgba, Scene, native_view_placement, point, px, size,
};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

/// gpui_ios's `CallbackSlot` (ios.rs): GPUI's callback, out of its slot while it runs, so GPUI re-entering the
/// window finds no borrow, and not put back if GPUI replaced it meanwhile.
struct CallbackSlot<T> {
    value: RefCell<Option<T>>,
    generation: Cell<u64>,
}

impl<T> Default for CallbackSlot<T> {
    fn default() -> Self {
        Self {
            value: RefCell::new(None),
            generation: Cell::new(0),
        }
    }
}

impl<T> CallbackSlot<T> {
    fn set(&self, value: T) {
        self.generation.set(self.generation.get().wrapping_add(1));
        drop(self.value.replace(Some(value)));
    }

    fn with<R>(&self, callback: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut value = self.value.borrow_mut().take()?;
        let generation = self.generation.get();
        let result = callback(&mut value);
        // Do not resurrect a handler that application code replaced or removed.
        if self.generation.get() == generation {
            drop(self.value.replace(Some(value)));
        }
        Some(result)
    }
}

type HitTest = Box<dyn FnMut(Point<Pixels>) -> Option<NativeViewId>>;
type Occluders = Box<dyn FnMut() -> Vec<Bounds<Pixels>>>;
/// GPUI's callback that turns DOM focus inside a view into GPUI focus.
type Focused = CallbackSlot<Box<dyn FnMut(NativeViewId)>>;

#[derive(Default)]
pub(crate) struct NativeViews {
    /// Exists while views are attached: the canvas is raised and the document listened to only then.
    layer: RefCell<Option<Layer>>,
    hit_test: CallbackSlot<HitTest>,
    occluders: CallbackSlot<Occluders>,
    focused: Focused,
}

struct Layer {
    root: web_sys::HtmlElement,
    slots_element: web_sys::HtmlElement,
    shields_element: web_sys::HtmlElement,
    slots: Vec<Slot>,
    /// The occluders the shields stand for, and the shield elements, one per occluder or more (spares hidden).
    occluders_shown: Vec<Bounds<Pixels>>,
    shields: Vec<web_sys::HtmlElement>,
    /// A frame placed views since the shields were last updated: GPUI's hitboxes over them may have moved.
    shields_stale: bool,
    target: PointerTarget,
    /// The translation and size last given to `root`, which follow the canvas's box.
    root_box: Bounds<Pixels>,
    /// The canvas's own values of the styles the layer changes, put back when the last view goes.
    canvas_styles: Vec<(&'static str, String)>,
    _listeners: Vec<EventListenerHandle>,
}

struct Slot {
    id: NativeViewId,
    slot: web_sys::HtmlElement,
    wrapper: web_sys::HtmlElement,
    content: web_sys::HtmlElement,
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

/// The pointer events the document's capture listener decides on, each with how it reads one. Add a row to decide on
/// another event.
const POINTER_EVENTS: [(&str, fn(&web_sys::MouseEvent) -> PointerKind); 5] = [
    ("pointermove", |event| match event.buttons() {
        0 => PointerKind::Hover,
        _ => PointerKind::Drag,
    }),
    ("pointerdown", |_| PointerKind::Press),
    ("pointerup", release_kind),
    ("pointercancel", release_kind),
    ("wheel", |_| PointerKind::Wheel),
];

/// The canvas styles the layer sets while views exist, and the canvas's `pointer-events`, which the layer toggles.
const CANVAS_STYLES: [(&str, &str); 3] = [
    ("position", "relative"),
    ("z-index", "1"),
    ("pointer-events", "auto"),
];

fn release_kind(event: &web_sys::MouseEvent) -> PointerKind {
    match event
        .dyn_ref::<web_sys::PointerEvent>()
        .map(|event| event.pointer_type())
        .as_deref()
    {
        Some("touch") => PointerKind::TouchRelease,
        _ => PointerKind::Release,
    }
}

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

/// Dispatches a copy of `event` on `target` in a microtask, right after the listener returns: dispatching it from the
/// listener would run that listener (a wasm-bindgen `FnMut`) inside itself, which throws. The copy is the event's
/// own constructor with the event itself as its init dictionary, which keeps every field (pointer id, buttons,
/// deltas).
fn send_copy(event: &web_sys::MouseEvent, target: web_sys::EventTarget) {
    let copy = js_sys::Reflect::get(event, &JsValue::from_str("constructor"))
        .ok()
        .and_then(|constructor| constructor.dyn_into::<js_sys::Function>().ok())
        .and_then(|constructor| {
            let arguments = js_sys::Array::of2(&JsValue::from(event.type_()), event);
            js_sys::Reflect::construct(&constructor, &arguments).ok()
        });
    let queue_microtask =
        js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("queueMicrotask"))
            .ok()
            .and_then(|queue| queue.dyn_into::<js_sys::Function>().ok());
    let (Some(copy), Some(queue_microtask)) = (copy, queue_microtask) else {
        log::warn!("native views: cannot copy a {} event", event.type_());
        return;
    };
    let dispatch = Closure::once_into_js(move || {
        target.dispatch_event(copy.unchecked_ref()).ok();
    });
    queue_microtask.call1(&JsValue::UNDEFINED, &dispatch).ok();
}

impl NativeViews {
    pub(crate) fn target(&self) -> PointerTarget {
        self.layer
            .borrow()
            .as_ref()
            .map_or(PointerTarget::Canvas, |layer| layer.target)
    }

    pub(crate) fn set_hit_test(&self, callback: HitTest) {
        self.hit_test.set(callback);
    }

    pub(crate) fn set_occluders(&self, callback: Occluders) {
        self.occluders.set(callback);
    }

    pub(crate) fn set_focused(&self, callback: Box<dyn FnMut(NativeViewId)>) {
        self.focused.set(callback);
    }

    fn shows_views(&self) -> bool {
        self.layer
            .borrow()
            .as_ref()
            .is_some_and(|layer| layer.slots.iter().any(|slot| slot.placed.is_some()))
    }

    fn over(&self, position: Point<Pixels>) -> Over {
        match self.hit_test.with(|hit_test| hit_test(position)).flatten() {
            Some(_) => Over::View,
            None => Over::Window,
        }
    }
}

impl Layer {
    fn set_target(&mut self, target: PointerTarget, canvas: &web_sys::HtmlCanvasElement) {
        if std::mem::replace(&mut self.target, target) == target {
            return;
        }
        let pointer_events = match target {
            PointerTarget::Canvas => "auto",
            PointerTarget::Views => "none",
        };
        set_styles(canvas, &[("pointer-events", pointer_events)]);
    }

    /// What the browser gave an event to.
    fn hit(
        &self,
        target: Option<web_sys::EventTarget>,
        canvas: &web_sys::HtmlCanvasElement,
    ) -> Hit {
        let Some(node) = target.and_then(|target| target.dyn_into::<web_sys::Node>().ok()) else {
            return Hit::Outside;
        };
        if js_sys::Object::is(&node, canvas) {
            Hit::Canvas
        } else if self
            .slots
            .iter()
            .any(|slot| slot.slot.contains(Some(&node)))
        {
            Hit::Element
        } else if self.root.contains(Some(&node)) {
            Hit::Cover
        } else {
            Hit::Outside
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
        let shields_element = create(document, "div")?;
        reset_anchor(&root);
        set_styles(
            &root,
            &[
                ("z-index", "0"),
                ("overflow", "hidden"),
                ("touch-action", "none"),
                ("user-select", "none"),
                ("-webkit-user-select", "none"),
            ],
        );
        for (container, z_index) in [(&slots_element, "0"), (&shields_element, "1")] {
            set_styles(
                container,
                &[
                    ("position", "absolute"),
                    ("inset", "0"),
                    ("z-index", z_index),
                    ("pointer-events", "none"),
                ],
            );
            root.append_child(container).ok()?;
        }
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

        let pointer = POINTER_EVENTS.iter().map(|&(event_name, kind)| {
            let window = Rc::downgrade(self);
            EventListenerHandle::add_non_passive_in(
                document.as_ref(),
                event_name,
                Phase::Capture,
                move |event| {
                    if let Some(window) = window.upgrade() {
                        let event: web_sys::MouseEvent = event.unchecked_into();
                        window.decide_native_view_pointer(&event, kind(&event));
                    }
                },
            )
        });
        // DOM focus moving into a view: `focusin` within this document; the window's `blur` when it moves into an
        // iframe, whose document gets the events (the parent sees no `focusin`).
        let focus_targets: [(&web_sys::EventTarget, &'static str); 2] = [
            (document.as_ref(), "focusin"),
            (self.browser_window.as_ref(), "blur"),
        ];
        let focus = focus_targets.map(|(target, event_name)| {
            let window = Rc::downgrade(self);
            let browser_window = self.browser_window.clone();
            EventListenerHandle::add(target, event_name, move |_| {
                // After this task: Firefox fires the window's `blur` before `activeElement` is the iframe.
                let window = window.clone();
                let report = Closure::once_into_js(move || {
                    if let Some(window) = window.upgrade() {
                        window.report_native_view_focus();
                    }
                });
                browser_window
                    .set_timeout_with_callback(report.unchecked_ref())
                    .ok();
            })
        });
        let listeners = pointer.chain(focus).collect();
        Some(Layer {
            root,
            slots_element,
            shields_element,
            slots: Vec::new(),
            occluders_shown: Vec::new(),
            shields: Vec::new(),
            shields_stale: false,
            target: PointerTarget::Canvas,
            root_box: Bounds::default(),
            canvas_styles,
            _listeners: listeners,
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
        set_styles(
            &slot,
            &[
                ("pointer-events", "auto"),
                ("overflow", "hidden"),
                ("display", "none"),
            ],
        );
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
            content,
            placed: None,
        })
    }

    /// DOM focus moved: if it is inside a view's slot now, GPUI focuses that view's handle.
    fn report_native_view_focus(&self) {
        let focused = self.native_views.layer.borrow().as_ref().and_then(|layer| {
            layer
                .slots
                .iter()
                .find(|slot| self.holds_focus(&slot.slot))
                .map(|slot| slot.id)
        });
        if let Some(id) = focused {
            self.native_views.focused.with(|callback| callback(id));
        }
    }

    /// Whether DOM focus is inside `element`.
    fn holds_focus(&self, element: &web_sys::Node) -> bool {
        self.browser_window
            .document()
            .and_then(|document| document.active_element())
            .is_some_and(|active| element.contains(Some(&active)))
    }

    /// Flutter's `clearPlatformView` / `disposeView`: the slot leaves the document with the content in it. Focus
    /// inside it goes back to GPUI's input element (the browser would leave it on the body, where no key reaches
    /// GPUI). The last view takes the layer with it: the canvas gets its own styles back and the document's
    /// listeners go.
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
        // Focusing fires focus events GPUI may answer, so no borrow is held.
        if self.holds_focus(&slot.slot) {
            self.ime_mirror.focus();
        }
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
        } else if !current.slots.iter().any(|slot| slot.placed.is_some()) {
            current.set_target(PointerTarget::Canvas, &self.canvas);
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
        let showed = layer.slots.iter().any(|slot| slot.placed.is_some());
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
        let shows = layer.slots.iter().any(|slot| slot.placed.is_some());
        layer.shields_stale = true;
        // Views that start to show take the pointer until it is seen over GPUI's content: a touch has no hover
        // to switch on, so its first tap must find the element. The layer sends anything else back.
        match (showed, shows) {
            (false, true) => layer.set_target(PointerTarget::Views, &self.canvas),
            (true | false, false) => layer.set_target(PointerTarget::Canvas, &self.canvas),
            (true, true) => {}
        }
    }

    /// After a frame that placed views, outside GPUI's update: a shield over each region of a view where GPUI's
    /// content takes input, so the browser hit-tests the pointer there to GPUI's layer, not the view's element. Then
    /// the pointer, if it stayed still, is decided again where the frame may have moved content under it.
    pub(crate) fn update_native_view_shields(&self) {
        let stale = self
            .native_views
            .layer
            .borrow_mut()
            .as_mut()
            .is_some_and(|layer| std::mem::replace(&mut layer.shields_stale, false));
        if !stale {
            return;
        }
        self.show_shields();
        let kind = match self.pressed_button.get() {
            Some(_) => PointerKind::Drag,
            None => PointerKind::Placed,
        };
        let position = self.state.borrow().mouse_position;
        let over = self.native_views.over(position);
        if let Some(layer) = self.native_views.layer.borrow_mut().as_mut() {
            // A frame has no event target; the canvas stands in (the `Placed` rows don't read it).
            let step = native_view_layout::step(layer.target, kind, over, Hit::Canvas);
            layer.set_target(step.target, &self.canvas);
        }
    }

    /// Puts a shield over every occluder GPUI's last frame has.
    fn show_shields(&self) {
        let Some(occluders) = self.native_views.occluders.with(|occluders| occluders()) else {
            return;
        };
        let mut layer = self.native_views.layer.borrow_mut();
        let (Some(layer), Some(document)) = (layer.as_mut(), self.browser_window.document()) else {
            return;
        };
        if layer.occluders_shown == occluders {
            return;
        }
        while layer.shields.len() < occluders.len() {
            let Some(shield) = create(&document, "div") else {
                return;
            };
            reset_anchor(&shield);
            set_styles(&shield, &[("pointer-events", "auto")]);
            if layer.shields_element.append_child(&shield).is_err() {
                return;
            }
            layer.shields.push(shield);
        }
        for (index, shield) in layer.shields.iter().enumerate() {
            match occluders.get(index) {
                Some(occluder) => {
                    place(shield, *occluder);
                    set_styles(shield, &[("display", "")]);
                }
                None => set_styles(shield, &[("display", "none")]),
            }
        }
        layer.occluders_shown = occluders;
    }

    /// The document's capture listener, ahead of every element: which of the canvas and what is under it takes the
    /// pointer from now on, and whether this event is GPUI's after all.
    fn decide_native_view_pointer(&self, event: &web_sys::MouseEvent, kind: PointerKind) {
        // A copy sent to the canvas comes through here too, already decided.
        if !event.is_trusted() || !self.native_views.shows_views() {
            return;
        }
        let Some(hit) = self
            .native_views
            .layer
            .borrow()
            .as_ref()
            .map(|layer| layer.hit(event.target(), &self.canvas))
        else {
            return;
        };
        let canvas = self.canvas.get_bounding_client_rect();
        let position = point(
            px((f64::from(event.client_x()) - canvas.left()) as f32),
            px((f64::from(event.client_y()) - canvas.top()) as f32),
        );
        let over = match (kind, hit) {
            // Off the window GPUI's pointer is not there; a drag stays with its press's target, wherever it is.
            (_, Hit::Outside) | (PointerKind::Drag, _) => Over::Window,
            (
                PointerKind::Hover
                | PointerKind::Press
                | PointerKind::Wheel
                | PointerKind::Release
                | PointerKind::TouchRelease
                | PointerKind::Placed,
                Hit::Canvas | Hit::Cover | Hit::Element,
            ) => {
                self.state.borrow_mut().mouse_position = position;
                self.native_views.over(position)
            }
        };
        let step = {
            let mut layer = self.native_views.layer.borrow_mut();
            let Some(layer) = layer.as_mut() else {
                return;
            };
            let step = native_view_layout::step(layer.target, kind, over, hit);
            layer.set_target(step.target, &self.canvas);
            step
        };
        let target: Option<web_sys::EventTarget> = match step.send {
            Send::Nothing => return,
            Send::Canvas => Some(self.canvas.clone().into()),
            Send::ElementUnder => {
                // Every shield is put back where GPUI's last frame has its occluders, changed or not.
                if let Some(layer) = self.native_views.layer.borrow_mut().as_mut() {
                    layer.occluders_shown.clear();
                }
                self.show_shields();
                // Only an element in a slot: the layer or a shield still there would take the copy back here.
                self.browser_window
                    .document()
                    .and_then(|document| {
                        document
                            .element_from_point(event.client_x() as f32, event.client_y() as f32)
                    })
                    .filter(|element| {
                        self.native_views
                            .layer
                            .borrow()
                            .as_ref()
                            .is_some_and(|layer| {
                                layer.hit(Some(element.clone().into()), &self.canvas)
                                    == Hit::Element
                            })
                    })
                    .map(Into::into)
            }
        };
        event.prevent_default();
        event.stop_propagation();
        if let Some(target) = target {
            send_copy(event, target);
        }
    }

    /// Gives the view's element DOM focus, or takes it back to GPUI's own input element.
    /// Focusing fires focus events GPUI may answer, so the layer is not borrowed meanwhile.
    pub(crate) fn focus_native_view(&self, id: Option<NativeViewId>) {
        let Some((content, root)) = self.native_views.layer.borrow().as_ref().map(|layer| {
            let content = id.and_then(|id| {
                layer
                    .slots
                    .iter()
                    .find(|slot| slot.id == id)
                    .map(|slot| slot.content.clone())
            });
            (content, layer.root.clone())
        }) else {
            return;
        };
        match (id, content) {
            // Focus already inside the element (an `<input>` in it) stays where the user put it.
            (Some(_), Some(content)) if self.holds_focus(&content) => {}
            // The `focusin` this fires reports the view GPUI has just focused, which changes nothing.
            (Some(_), Some(content)) => {
                content.focus().ok();
            }
            (Some(_), None) => {}
            (None, _) => {
                if self.holds_focus(&root) {
                    self.ime_mirror.focus();
                }
            }
        }
    }
}
