use crate::{Action, KeyBinding, KeyDownEvent, Keystroke, PlatformInput};
use crate::{
    AnyWindowHandle, App, Bounds, Element, ElementId, FocusHandle, FocusId, Focusable,
    GlobalElementId, Hitbox, HitboxId, Hsla, InspectorElementId, InteractiveElement, IntoElement,
    LayoutId, PaintNativeView, ParentElement, Pixels, Point, Style, Styled, Window, div,
};
use anyhow::{Context as _, Result};
use collections::HashSet;
use std::{
    any::{Any, TypeId},
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
};

/// Who hears a key press first while a native view is focused
/// (`Window::native_view_key_order`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyOrder {
    /// Key bindings, then the view (or, where the window gets the view's keys,
    /// the view's element) with what they leave.
    BindingsFirst,
    /// The view, or the view's element where the window gets its keys; key
    /// bindings get what it leaves.
    ViewFirst,
}

/// Identifies a [`NativeView`] within its window.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct NativeViewId(u64);

#[cfg(any(test, feature = "test-support"))]
impl NativeViewId {
    /// Makes an ID for a test that needs one without a window to attach a view to.
    pub fn for_test(id: u64) -> Self {
        Self(id)
    }
}

/// A view owned by the platform's UI toolkit, shown inside a GPUI window.
///
/// macOS shows an `NSView`, iOS a `UIView`, Windows DirectComposition
/// content (usually a composition swap chain), and Wayland a `wl_surface` the
/// app created on GPUI's own connection. [`NativeView::new`] fails on every
/// other platform.
///
/// On a platform whose native views take no input of their own
/// ([`crate::NativeViewInput::Window`]: iOS, Windows, Wayland), pointer input
/// over the view is hit-tested like any solid element's: listeners added to
/// the element this handle becomes (`view.into_element().on_mouse_move(…)`)
/// and its ancestors hear it, elements painted before the view hear nothing
/// over it, elements painted after the view take theirs, and
/// [`bounds`](Self::bounds) makes positions view-relative. Keys pressed while
/// the view's [`FocusHandle`] is focused reach that element's key listeners
/// before key bindings, as they reach the view itself on macOS, so the app
/// forwards the ones it wants to the view and stops their propagation; key
/// bindings get the rest. Bindings for reserved actions and the rest of a
/// pending binding apply first, as described below.
/// The rest of this comment describes macOS ([`crate::NativeViewInput::View`]).
///
/// The view is composited beneath the window's GPUI content and shows through
/// wherever this handle is used as an element, so elements painted after it
/// cover it like they would cover any other element. The view is treated as
/// opaque, and is not affected by element opacity or transformations.
///
/// Mouse input over the view goes to the view, and to nothing behind it,
/// wherever no element painted after the view takes mouse input. An element
/// that is only drawn over the view covers it without taking its mouse input. The window's own content is still told of a mouse press
/// there, as a press outside every element. An element behind the view hears
/// no mouse moves while the pointer is over it, so follow a drag that can cross
/// the view with `on_drag_move`, or with an element in front of the view.
///
/// The view takes keyboard input directly from the platform while its
/// [`FocusHandle`] is focused, which a click on the view does, as can an
/// assistive technology such as VoiceOver moving its cursor into the view.
/// A key the view doesn't use is then dispatched from the view's element like
/// any other key press, so key bindings apply to what the view leaves.
///
/// Two kinds of key reach key bindings before the view. Bindings for actions
/// passed to [`reserve_action`](Self::reserve_action) apply first, and their
/// keys never reach the view. A key held with Command or Control is offered to
/// the view as a key equivalent, which is how a web view takes it; a view that
/// only takes such keys as ordinary key presses, as AppKit's text views do,
/// gets one after key bindings have had it.
///
/// The view may take a moment to leave a key, so the keys of any other
/// multi-key binding can reach the view when they are typed quickly.
///
/// The view stays attached to the window it was created in, hidden when it
/// isn't painted, until the last clone of this handle is dropped. Painted more
/// than once in a frame, it shows only where it was painted first. It shows
/// nothing as an element of any other window, so showing the same platform
/// view in another window takes dropping this handle and creating one there.
#[derive(Clone)]
pub struct NativeView(Rc<NativeViewState>);

pub(crate) struct NativeViewState {
    id: NativeViewId,
    window: AnyWindowHandle,
    focus_handle: FocusHandle,
    background: Cell<Hsla>,
    /// The hitbox of the occurrence that shows the view, which is the one
    /// painted first in a frame that paints the view more than once.
    shown_hitbox: Cell<Option<HitboxId>>,
    reserved_actions: RefCell<HashSet<TypeId>>,
    _attachment: Box<dyn Any>,
}

impl NativeView {
    /// Attaches a platform view to the window.
    ///
    /// # Errors
    ///
    /// Fails on platforms that cannot composite native views, which is every
    /// platform but macOS for now.
    ///
    /// # Safety
    ///
    /// `view` must point to a live view of the kind the window's platform
    /// embeds, which is an `NSView` on macOS, and that view must not be in
    /// another view hierarchy. On iOS it is a `UIView`. On Windows it is a
    /// COM object (`IUnknown`) that `IDCompositionVisual::SetContent` takes,
    /// usually a composition swap chain the app presents to on its own.
    /// On Wayland it is a `wl_proxy` of a `wl_surface`
    /// made on the window's own `wl_display`, with no role, no `wp_viewport`
    /// and no subsurfaces of its own; GPUI makes it a subsurface of the window
    /// and places it, and the app commits every buffer through
    /// `gpui_linux::NativeViewBuffers`, from a thread it stops and joins before
    /// GPUI's connection closes (in `cx.on_app_quit`, or when the window
    /// closes).
    ///
    /// GPUI retains the view and puts it inside a view of its own, until the
    /// last clone of the returned handle is dropped. Until then GPUI decides the
    /// view's frame, whether it is hidden, and its order among the window's other
    /// views, so the caller must leave those alone and must not move the view to
    /// another superview.
    pub unsafe fn new(view: NonNull<c_void>, window: &mut Window, cx: &mut App) -> Result<Self> {
        let id = NativeViewId(window.next_native_view_id);
        // SAFETY: The caller upholds the platform's requirements for `view`.
        let attachment = unsafe { window.platform_window.attach_native_view(id, view) }
            .context("this platform cannot show native views")?;
        window.next_native_view_id += 1;
        let state = Rc::new(NativeViewState {
            id,
            window: window.handle,
            focus_handle: cx.focus_handle(),
            background: Cell::new(Hsla::transparent_black()),
            shown_hitbox: Cell::new(None),
            reserved_actions: RefCell::default(),
            _attachment: attachment,
        });
        window.native_views.push(Rc::downgrade(&state));
        Ok(Self(state))
    }
}

impl NativeView {
    /// Sets the color shown where the view has not caught up with its element.
    ///
    /// The platform moves the view through its own compositor, so for a frame
    /// or two after its element moves or resizes, part of the element can be
    /// uncovered. A color close to the view's own background hides that. A
    /// window showing several native views uses the color of the one painted
    /// last.
    pub fn set_background(&self, color: impl Into<Hsla>, cx: &mut App) {
        let color = color.into();
        if self.0.background.replace(color) == color {
            return;
        }
        // The color is part of what the element paints.
        let window = self.0.window;
        cx.defer(move |cx| {
            window.update(cx, |_, window, _| window.refresh()).ok();
        });
    }

    /// Where the view showed in the frame last drawn, in window coordinates,
    /// which turns a pointer position into one relative to the view.
    pub fn bounds(&self, window: &Window) -> Option<Bounds<Pixels>> {
        let frame = &window.rendered_frame;
        let shown = frame
            .scene
            .native_views
            .iter()
            .find(|native_view| native_view.id == self.0.id)?;
        frame
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.id == shown.hitbox_id)
            .map(|hitbox| hitbox.bounds)
    }

    /// What the window's platform keeps of the view, for that platform's own
    /// API to reach: on Wayland, `gpui_linux::NativeViewBuffers::of(&view)`,
    /// through which the app commits its buffers.
    pub fn platform_attachment(&self) -> &dyn Any {
        &*self.0._attachment
    }

    /// Makes key bindings for `A` apply before the view sees the key.
    ///
    /// The view otherwise sees every key first and can keep any of them, so
    /// this is for the few bindings that must work whatever the view shows.
    /// The view never sees a key that such a binding starts with, even when the
    /// rest of the binding's keys don't follow.
    pub fn reserve_action<A: Action>(&self) {
        self.0
            .reserved_actions
            .borrow_mut()
            .insert(TypeId::of::<A>());
    }
}

impl Focusable for NativeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.0.focus_handle.clone()
    }
}

impl IntoElement for NativeView {
    type Element = crate::Div;

    fn into_element(self) -> Self::Element {
        div()
            .track_focus(&self.0.focus_handle)
            .size_full()
            .child(NativeViewElement(self))
    }
}

struct NativeViewElement(NativeView);

impl Element for NativeViewElement {
    type RequestLayoutState = ();
    /// `None` in a window other than the one the view is attached to.
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = crate::relative(1.).into();
        style.size.height = crate::relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<Hitbox> {
        let state = &self.0.0;
        if state.window != window.handle {
            return None;
        }
        // The view is only where it is painted first, so a later occurrence
        // must not keep mouse input from what is behind it. It still has a
        // hitbox, which becomes the view's if a frame reuses this occurrence
        // after the first one has gone. An occurrence that is clipped away
        // paints nothing, and doesn't count as the first.
        let is_visible = !bounds.intersect(&window.content_mask().bounds).is_empty();
        let is_shown_elsewhere = state.shown_hitbox.get().is_some_and(|shown| {
            window
                .next_frame
                .hitboxes
                .iter()
                .any(|hitbox| hitbox.id == shown)
        });
        let shows_view = is_visible && !is_shown_elsewhere;
        let behavior = window
            .platform_window
            .native_view_input()
            .hitbox_behavior(shows_view);
        let hitbox = window.insert_hitbox(bounds, behavior);
        if shows_view {
            state.shown_hitbox.set(Some(hitbox.id));
        }
        Some(hitbox)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let Some(hitbox) = hitbox else {
            return;
        };
        let id = self.0.0.id;
        let bounds = window.snap_bounds(hitbox.bounds);
        let content_mask = window.snapped_content_mask();
        window.next_frame.scene.insert_primitive(PaintNativeView {
            order: 0,
            id,
            hitbox_id: hitbox.id,
            bounds,
            content_mask,
            background: self.0.0.background.get(),
        });
    }
}

impl Window {
    /// Tells the platform which native view, if any, the focused handle belongs
    /// to, whenever that changes: on a focus change, and on a frame drawn after
    /// the focused view's last handle was dropped, even with no view left.
    pub(super) fn sync_native_view_focus(&mut self) {
        self.native_views.retain(|view| view.strong_count() > 0);
        let focused = self
            .focus
            .and_then(|focus| self.native_view_for_focus(focus));
        if focused != self.focused_native_view_sent {
            self.focused_native_view_sent = focused;
            self.platform_window.set_focused_native_view(focused);
        }
    }

    /// Ends an unfinished composition in the window's own content when keyboard
    /// input is about to move to a native view.
    ///
    /// The platform also asks for this as its own view loses keyboard input, but
    /// that request needs the app, which is already borrowed when focus is moved
    /// from inside an update. The entity being updated may also be the one that
    /// holds the composition, so this waits for the update to end.
    pub(super) fn end_composition_before_focusing_native_view(
        &mut self,
        focus: FocusId,
        cx: &mut App,
    ) {
        if self.native_view_for_focus(focus).is_none() {
            return;
        }
        self.defer(cx, |window, cx| {
            if let Some(mut input_handler) = window.platform_window.take_input_handler() {
                input_handler.unmark_text_during_update(window, cx);
                window.platform_window.set_input_handler(input_handler);
            }
        });
    }

    fn native_view_for_focus(&self, focus: FocusId) -> Option<NativeViewId> {
        self.native_views
            .iter()
            .filter_map(Weak::upgrade)
            .find(|view| view.focus_handle.id == focus)
            .map(|view| view.id)
    }

    /// The native view that a mouse event at this position reaches, if no
    /// GPUI hitbox is above it there.
    pub(super) fn hit_test_native_view(&self, position: Point<Pixels>) -> Option<NativeViewId> {
        let hit_test = self.rendered_frame.hit_test(position);
        let topmost = hit_test.ids.first()?;
        self.rendered_frame
            .scene
            .native_views
            .iter()
            .find(|native_view| native_view.hitbox_id == *topmost)
            .map(|native_view| native_view.id)
    }

    /// Dispatches a key press that the focused native view has not seen yet, if
    /// it continues a pending binding or is bound to an action the view reserved.
    pub(super) fn dispatch_key_down_before_native_view(
        &mut self,
        event: KeyDownEvent,
        cx: &mut App,
    ) -> bool {
        match self.native_view_key_order(&event.keystroke, cx) {
            None | Some(KeyOrder::ViewFirst) => false,
            Some(KeyOrder::BindingsFirst) => {
                // The key is GPUI's whether or not anything acts on it. Leaving it to
                // the view as well would have it dispatched again when the view left it.
                self.dispatch_event(PlatformInput::KeyDown(event), cx);
                true
            }
        }
    }

    /// Who hears a key press first while a native view's handle is focused, the
    /// rule macOS's views follow: key bindings, for a key that continues a
    /// pending binding or starts one bound to an action the view reserved; the
    /// view for every other key. `None` while no native view is focused. A
    /// pending binding the key doesn't continue ends here, as it would in the
    /// window's own content; left pending, it would be completed by a later key
    /// even after the view had used this one.
    pub(super) fn native_view_key_order(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut App,
    ) -> Option<KeyOrder> {
        let view = self
            .native_views
            .iter()
            .filter_map(Weak::upgrade)
            .find(|view| Some(view.focus_handle.id) == self.focus)?;
        let node_id = self.focus_node_id_in_rendered_frame(self.focus);
        let dispatch_path = self.rendered_frame.dispatch_tree.dispatch_path(node_id);
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        let pending = self.pending_input_keystrokes();
        let is_pending = pending.is_some();
        let continues_pending = pending.is_some_and(|pending| {
            let mut input = pending.to_vec();
            input.push(keystroke.clone());
            let (bindings, is_pending, _) =
                dispatch_tree.bindings_for_input(&input, &dispatch_path);
            is_pending || !bindings.is_empty()
        });
        let is_reserved = !continues_pending && {
            let input = std::slice::from_ref(keystroke);
            let (bindings, _, context_stack) =
                dispatch_tree.bindings_for_input(input, &dispatch_path);
            let reserved_actions = view.reserved_actions.borrow();
            let is_reserved = |binding: &KeyBinding| {
                reserved_actions.contains(&binding.action.as_any().type_id())
            };
            // The first key of a longer binding is reserved too, or the view
            // could keep that key and the binding would never begin.
            bindings.first().is_some_and(is_reserved)
                || dispatch_tree
                    .possible_next_bindings_for_input(input, &context_stack)
                    .iter()
                    .any(is_reserved)
        };
        if is_pending && !continues_pending {
            self.flush_pending_input(cx);
        }
        // (continues a pending binding, reserved) → order.
        Some(match (continues_pending, is_reserved) {
            (true, _) | (false, true) => KeyOrder::BindingsFirst,
            (false, false) => KeyOrder::ViewFirst,
        })
    }

    /// Focuses the handle of a native view that the platform gave keyboard input to.
    pub(super) fn focus_native_view(&mut self, id: NativeViewId, cx: &mut App) {
        let view = self
            .native_views
            .iter()
            .filter_map(Weak::upgrade)
            .find(|view| view.id == id);
        if let Some(view) = view {
            self.focus(&view.focus_handle, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Context, Entity, FocusHandle, InputHandler, KeyBinding, Keystroke, Modifiers, MouseButton,
        MouseDownEvent, MouseMoveEvent, Render, ScaledPixels, ScrollEvent, TestAppContext,
        UTF16Selection, VisualTestContext, WindowHandle, canvas, point, prelude::*, px, rgb,
    };
    use std::ops::Range;

    mod window_input;

    #[gpui::test]
    fn focusing_a_native_view_gives_it_keyboard_input(cx: &mut TestAppContext) {
        let window = open_window(cx);
        let platform_window = cx.test_window(window.into());

        let id = window
            .update(cx, |view, window, cx| {
                window.focus(&view.native_view.focus_handle(cx), cx);
                view.native_view.0.id
            })
            .unwrap();
        assert_eq!(platform_window.focused_native_view(), Some(id));

        window
            .update(cx, |view, window, cx| window.focus(&view.other_focus, cx))
            .unwrap();
        assert_eq!(platform_window.focused_native_view(), None);

        window
            .update(cx, |view, window, cx| {
                window.focus(&view.native_view.focus_handle(cx), cx);
                window.blur(cx);
            })
            .unwrap();
        assert_eq!(platform_window.focused_native_view(), None);
    }

    #[gpui::test]
    fn the_platform_hears_every_change_of_the_focused_native_view(cx: &mut TestAppContext) {
        let window = open_window(cx);
        let other_window = open_window(cx);
        let platform_window = cx.test_window(window.into());
        let id = window
            .update(cx, |view, _, _| view.native_view.0.id)
            .unwrap();
        type Step = Box<dyn Fn(&mut TestAppContext)>;
        // (row name, step, the view the platform was last told is focused)
        let rows: [(&str, Step, Option<NativeViewId>); 3] = [
            (
                "focusing the view gives it keyboard input",
                Box::new(move |cx| {
                    window
                        .update(cx, |view, window, cx| {
                            window.focus(&view.native_view.focus_handle(cx), cx)
                        })
                        .unwrap()
                }),
                Some(id),
            ),
            (
                // The window's only view goes: its handle is swapped for one of
                // another window's views. No view is left to compare against.
                "dropping the focused view's last handle gives the keyboard back",
                Box::new(move |cx| {
                    let elsewhere = other_window
                        .update(cx, |view, _, _| view.native_view.clone())
                        .unwrap();
                    window
                        .update(cx, |view, _, cx| {
                            view.native_view = elsewhere;
                            cx.notify();
                        })
                        .unwrap();
                    cx.run_until_parked();
                }),
                None,
            ),
            (
                "moving focus with no view left changes nothing",
                Box::new(move |cx| {
                    window
                        .update(cx, |view, window, cx| window.focus(&view.other_focus, cx))
                        .unwrap()
                }),
                None,
            ),
        ];
        for (name, step, focused) in rows {
            step(cx);
            assert_eq!(platform_window.focused_native_view(), focused, "{name}");
        }
    }

    #[gpui::test]
    fn platform_focus_of_a_native_view_focuses_its_handle(cx: &mut TestAppContext) {
        let window = open_window(cx);
        let platform_window = cx.test_window(window.into());
        let id = window
            .update(cx, |view, window, cx| {
                window.focus(&view.other_focus, cx);
                view.native_view.0.id
            })
            .unwrap();

        platform_window.simulate_native_view_focused(id);

        window
            .update(cx, |view, window, cx| {
                assert!(view.native_view.focus_handle(cx).is_focused(window));
            })
            .unwrap();
        assert_eq!(platform_window.focused_native_view(), Some(id));
    }

    #[gpui::test]
    fn only_reserved_and_pending_bindings_apply_before_a_native_view(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("cmd-r", Reserved, None),
                KeyBinding::new("cmd-o", Ordinary, None),
                KeyBinding::new("cmd-k o", Ordinary, None),
                KeyBinding::new("cmd-j r", Reserved, None),
            ]);
        });
        let window = open_window(cx);
        let platform_window = cx.test_window(window.into());
        window
            .update(cx, |view, window, cx| {
                view.native_view.reserve_action::<Reserved>();
                window.focus(&view.native_view.focus_handle(cx), cx);
            })
            .unwrap();
        cx.run_until_parked();
        let press = |keystroke: &'static str| KeyDownEvent {
            keystroke: Keystroke::parse(keystroke).unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        let actions = |cx: &mut TestAppContext| {
            window
                .update(cx, |view, _, _| std::mem::take(&mut view.actions))
                .unwrap()
        };

        assert!(platform_window.simulate_key_down_before_native_view(press("cmd-r")));
        assert_eq!(actions(cx), ["reserved"]);

        // The view gets to see an ordinary binding's key first.
        assert!(!platform_window.simulate_key_down_before_native_view(press("cmd-o")));
        assert_eq!(actions(cx), [] as [&str; 0]);

        // Once the view has left the first key of a binding to GPUI, the rest
        // of that binding's keys are GPUI's.
        assert!(!platform_window.simulate_key_down_before_native_view(press("cmd-k")));
        cx.simulate_keystrokes(window.into(), "cmd-k");
        // A key that doesn't continue the binding ends it. The key is the
        // view's to see first, and isn't dispatched here, which would dispatch
        // it twice if the view then left it.
        assert!(!platform_window.simulate_key_down_before_native_view(press("x")));
        assert!(
            !window
                .update(cx, |_, window, _| window.has_pending_keystrokes())
                .unwrap()
        );
        let keys = |cx: &mut TestAppContext| {
            window
                .update(cx, |view, _, _| std::mem::take(&mut view.keys))
                .unwrap()
        };
        // The binding's first key is dispatched on its own, as it is when
        // the binding times out.
        assert_eq!(keys(cx), ["k"]);
        cx.simulate_keystrokes(window.into(), "x");
        assert_eq!(keys(cx), ["x"]);
        // Whether or not the view used that key, the rest of the binding's
        // keys no longer complete it, and are the view's again.
        assert!(!platform_window.simulate_key_down_before_native_view(press("o")));
        assert_eq!(actions(cx), [] as [&str; 0]);

        // The view never sees the first key of a reserved binding, which it
        // could otherwise keep.
        assert!(platform_window.simulate_key_down_before_native_view(press("cmd-j")));
        assert!(platform_window.simulate_key_down_before_native_view(press("r")));
        assert_eq!(actions(cx), ["reserved"]);

        // Nothing applies ahead of the window's own content.
        window
            .update(cx, |view, window, cx| window.focus(&view.other_focus, cx))
            .unwrap();
        assert!(!platform_window.simulate_key_down_before_native_view(press("cmd-r")));
    }

    #[gpui::test]
    fn mouse_reaches_a_native_view_only_where_nothing_covers_it(cx: &mut TestAppContext) {
        let window = open_window(cx);
        let platform_window = cx.test_window(window.into());
        let id = window
            .update(cx, |view, _, _| view.native_view.0.id)
            .unwrap();
        cx.run_until_parked();

        let uncovered = point(px(75.), px(75.));
        let covered = point(px(25.), px(25.));
        let outside = point(px(150.), px(150.));
        assert_eq!(platform_window.hit_test_native_view(uncovered), Some(id));
        assert_eq!(platform_window.hit_test_native_view(covered), None);
        assert_eq!(platform_window.hit_test_native_view(outside), None);
    }

    #[gpui::test]
    fn mouse_press_on_a_native_view_reaches_nothing_behind_it(cx: &mut TestAppContext) {
        let window = open_window(cx);
        cx.run_until_parked();
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        // The platform passes on a press that it gives to the view.
        cx.simulate_mouse_down(
            point(px(75.), px(75.)),
            MouseButton::Left,
            Modifiers::default(),
        );

        let presses = window
            .update(&mut cx, |view, _, _| std::mem::take(&mut view.presses))
            .unwrap();
        assert_eq!(presses, ["outside cover"]);
    }

    #[gpui::test]
    fn changing_the_background_repaints_the_native_view(cx: &mut TestAppContext) {
        let window = open_window(cx);
        cx.run_until_parked();

        window
            .update(cx, |view, _, cx| {
                view.native_view.set_background(rgb(0x00ff00), cx);
            })
            .unwrap();
        cx.run_until_parked();

        window
            .update(cx, |_, window, _| {
                let backgrounds = window
                    .rendered_frame
                    .scene
                    .native_views
                    .iter()
                    .map(|native_view| native_view.background)
                    .collect::<Vec<_>>();
                assert_eq!(backgrounds, [Hsla::from(rgb(0x00ff00))]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn native_view_painted_twice_shows_where_it_was_painted_first(cx: &mut TestAppContext) {
        let window = open_window(cx);
        window
            .update(cx, |view, _, cx| {
                view.paints_native_view_twice = true;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();

        window
            .update(cx, |_, window, _| {
                let bounds = window
                    .rendered_frame
                    .scene
                    .native_views
                    .iter()
                    .map(|native_view| native_view.bounds.origin.x)
                    .collect::<Vec<_>>();
                assert_eq!(bounds, [ScaledPixels::from(0.)]);
            })
            .unwrap();

        // Where the view was painted again, it isn't shown and takes no mouse
        // input, so what is behind it there gets the press.
        let id = window
            .update(cx, |view, _, _| view.native_view.0.id)
            .unwrap();
        let platform_window = cx.test_window(window.into());
        let painted_first = point(px(75.), px(75.));
        let painted_again = point(px(250.), px(50.));
        assert_eq!(
            platform_window.hit_test_native_view(painted_first),
            Some(id)
        );
        assert_eq!(platform_window.hit_test_native_view(painted_again), None);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.simulate_mouse_down(painted_again, MouseButton::Left, Modifiers::default());
        let presses = window
            .update(&mut cx, |view, _, _| std::mem::take(&mut view.presses))
            .unwrap();
        assert_eq!(presses, ["outside cover", "behind second occurrence"]);
    }

    #[gpui::test]
    fn native_view_is_drawn_in_paint_order(cx: &mut TestAppContext) {
        let window = open_window(cx);
        cx.run_until_parked();

        // The view shows through its container's background, and the element
        // painted after it covers it.
        window
            .update(cx, |_, window, _| {
                let labels = window
                    .rendered_frame
                    .scene
                    .batches()
                    .map(|batch| batch.label())
                    .collect::<Vec<_>>();
                assert_eq!(labels, ["quads (1)", "native views (1)", "quads (1)"]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn native_view_stays_attached_until_its_last_handle_is_dropped(cx: &mut TestAppContext) {
        let window = open_window(cx);
        let platform_window = cx.test_window(window.into());
        let other_handle = window
            .update(cx, |view, _, _| view.native_view.clone())
            .unwrap();
        assert_eq!(platform_window.attached_native_view_count(), 1);

        window
            .update(cx, |view, window, cx| {
                view.native_view = new_native_view(window, cx);
            })
            .unwrap();
        assert_eq!(platform_window.attached_native_view_count(), 2);

        drop(other_handle);
        assert_eq!(platform_window.attached_native_view_count(), 1);
    }

    #[gpui::test]
    fn native_view_shows_nothing_in_another_window(cx: &mut TestAppContext) {
        let window = open_window(cx);
        let native_view = window
            .update(cx, |view, _, _| view.native_view.clone())
            .unwrap();
        let other_window = cx.add_window(|_, cx| TestView {
            native_view,
            other_focus: cx.focus_handle(),
            actions: Vec::new(),
            keys: Vec::new(),
            presses: Vec::new(),
            view_events: Vec::new(),
            paints_native_view_twice: false,
            paints_under_view: false,
            unmark_count: 0,
        });
        cx.run_until_parked();

        other_window
            .update(cx, |_, window, _| {
                assert!(window.rendered_frame.scene.native_views.is_empty());
            })
            .unwrap();
        window
            .update(cx, |_, window, _| {
                assert_eq!(window.rendered_frame.scene.native_views.len(), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn focusing_a_native_view_ends_a_composition(cx: &mut TestAppContext) {
        let window = open_window(cx);
        window
            .update(cx, |view, window, cx| window.focus(&view.other_focus, cx))
            .unwrap();
        cx.run_until_parked();

        // The view that holds the composition moves focus from inside its own
        // update, so the composition can only be ended once that is over.
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.native_view.focus_handle(cx), cx);
            })
            .unwrap();
        let unmark_count = window.update(cx, |view, _, _| view.unmark_count).unwrap();
        assert_eq!(unmark_count, 1);
    }

    actions!(native_view_tests, [Reserved, Ordinary]);

    struct TestView {
        native_view: NativeView,
        other_focus: FocusHandle,
        actions: Vec<&'static str>,
        /// The key presses dispatched to the window's content.
        keys: Vec<String>,
        presses: Vec<&'static str>,
        /// The input the native view's own element hears, with positions
        /// relative to the view.
        view_events: Vec<String>,
        paints_native_view_twice: bool,
        /// Paints an element before the view's container, under the view,
        /// which a view that occludes keeps from the input over it.
        paints_under_view: bool,
        /// How many times the input handler of the "cover" element was told to
        /// end its composition.
        unmark_count: usize,
    }

    impl Render for TestView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .on_action(cx.listener(|this, _: &Reserved, _, _| this.actions.push("reserved")))
                .on_action(cx.listener(|this, _: &Ordinary, _, _| this.actions.push("ordinary")))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                    this.keys.push(event.keystroke.key.clone())
                }))
                .when(self.paints_under_view, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(10.))
                            .left_0()
                            .size(px(100.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| this.presses.push("under view")),
                            )
                            .on_scroll(
                                cx.listener(|this, _, _, _| this.presses.push("scroll under view")),
                            ),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .left_0()
                        .size(px(100.))
                        .bg(rgb(0xff0000))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.presses.push("behind view")),
                        )
                        .child(
                            self.native_view
                                .clone()
                                .into_element()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, event: &MouseDownEvent, window, _| {
                                        this.view_event("down", event.position, window)
                                    }),
                                )
                                .on_mouse_move(cx.listener(
                                    |this, event: &MouseMoveEvent, window, _| {
                                        this.view_event("move", event.position, window)
                                    },
                                ))
                                .on_scroll(cx.listener(|this, event: &ScrollEvent, window, _| {
                                    this.view_event("scroll", event.position, window)
                                }))
                                .on_key_down(cx.listener(
                                    |this, event: &KeyDownEvent, _, cx| {
                                        this.view_events
                                            .push(format!("key {}", event.keystroke.key));
                                        // The view uses "q" and leaves every other key.
                                        if event.keystroke.key == "q" {
                                            cx.stop_propagation();
                                        }
                                    },
                                )),
                        ),
                )
                .when(self.paints_native_view_twice, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left(px(200.))
                            .size(px(100.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| {
                                    this.presses.push("behind second occurrence")
                                }),
                            )
                            .child(self.native_view.clone()),
                    )
                })
                .child(
                    div()
                        .id("cover")
                        .track_focus(&self.other_focus)
                        .absolute()
                        .top_0()
                        .left_0()
                        .size(px(50.))
                        .occlude()
                        .bg(rgb(0x0000ff))
                        .on_mouse_down_out(
                            cx.listener(|this, _, _, _| this.presses.push("outside cover")),
                        )
                        .child({
                            let focus = self.other_focus.clone();
                            let view = cx.entity();
                            canvas(
                                |_, _, _| {},
                                move |_, _, window, cx| {
                                    window.handle_input(&focus, TestInput { view }, cx);
                                },
                            )
                            .size_full()
                        }),
                )
        }
    }

    /// Reaches its view as an element's input handler does, by updating it.
    struct TestInput {
        view: Entity<TestView>,
    }

    impl InputHandler for TestInput {
        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut App,
        ) -> Option<UTF16Selection> {
            None
        }

        fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
            None
        }

        fn text_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<String> {
            None
        }

        fn replace_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: &mut Window,
            _: &mut App,
        ) {
        }

        fn replace_and_mark_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) {
        }

        fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
            self.view.update(cx, |view, _| view.unmark_count += 1);
        }

        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<Bounds<Pixels>> {
            None
        }

        fn character_index_for_point(
            &mut self,
            _: Point<Pixels>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<usize> {
            None
        }
    }

    fn open_window(cx: &mut TestAppContext) -> WindowHandle<TestView> {
        cx.add_window(|window, cx| TestView {
            native_view: new_native_view(window, cx),
            other_focus: cx.focus_handle(),
            actions: Vec::new(),
            keys: Vec::new(),
            presses: Vec::new(),
            view_events: Vec::new(),
            paints_native_view_twice: false,
            paints_under_view: false,
            unmark_count: 0,
        })
    }

    fn new_native_view(window: &mut Window, cx: &mut App) -> NativeView {
        // SAFETY: The test platform never reads the view it is given.
        unsafe { NativeView::new(NonNull::dangling(), window, cx) }
            .expect("the test platform attaches native views")
    }
}
