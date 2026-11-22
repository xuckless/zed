use crate::{
    App, Bounds, ClipboardItem, Context, Entity, InputHandler, Pixels, TextInputConfiguration,
    UTF16Selection, Window,
};
use std::ops::Range;

/// Implement this trait to allow views to handle textual input when implementing an editor, field, etc.
///
/// Once your view implements this trait, you can use it to construct an [`ElementInputHandler<V>`].
/// This input handler can then be assigned during paint by calling [`Window::handle_input`].
///
/// Dismissing the software keyboard can leave the component focused. Call
/// [`Window::request_virtual_keyboard`] from an editing gesture, such as tapping
/// the field, to reopen it; repainting alone should not undo a dismissal.
///
/// After application-driven edits or selection changes, also call
/// [`Window::notify_text_input_changed`] or [`Window::notify_text_selection_changed`]
/// so native text services can invalidate their state.
///
/// UIKit text input also needs `text_length_utf16` (or a bounded
/// `text_input_editable_range`) and native selection updates through
/// `set_selected_text_range`. Supply `selection_bounds_for_range` for native
/// selection rectangles; it should return line/run fragments in one batch.
///
/// See [`InputHandler`] for details on how to implement each method.
pub trait EntityInputHandler: 'static + Sized {
    /// See [`InputHandler::text_for_range`] for details
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String>;

    /// See [`InputHandler::selected_text_range`] for details
    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection>;

    /// See [`InputHandler::marked_text_range`] for details
    fn marked_text_range(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>>;

    /// See [`InputHandler::unmark_text`] for details
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>);

    /// See [`InputHandler::paste`] for details
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    /// See [`InputHandler::replace_text_in_range`] for details
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    );

    /// See [`InputHandler::replace_and_mark_text_in_range`] for details
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );

    /// See [`InputHandler::bounds_for_range`] for details
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>>;

    /// See [`InputHandler::selection_bounds_for_range`] for details.
    fn selection_bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        Vec::new()
    }

    /// See [`InputHandler::character_index_for_point`] for details
    fn character_index_for_point(
        &mut self,
        point: crate::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize>;

    /// See [`InputHandler::set_selected_text_range`] for details
    fn set_selected_text_range(
        &mut self,
        _range_utf16: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// See [`InputHandler::text_length_utf16`] for details
    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    /// See [`InputHandler::accepts_text_input`] for details
    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        true
    }

    /// See [`InputHandler::text_input_configuration`] for details
    fn text_input_configuration(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> TextInputConfiguration {
        TextInputConfiguration::default()
    }

    /// See [`InputHandler::text_input_editable_range`] for details
    fn text_input_editable_range(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }
}

/// The canonical implementation of [`crate::PlatformInputHandler`]. Call [`Window::handle_input`]
/// with an instance during your element's paint.
pub struct ElementInputHandler<V> {
    view: Entity<V>,
    element_bounds: Bounds<Pixels>,
}

impl<V: 'static> ElementInputHandler<V> {
    /// Used in [`Element::paint`][element_paint] with the element's bounds, a `Window`, and a `App` context.
    ///
    /// [element_paint]: crate::Element::paint
    pub fn new(element_bounds: Bounds<Pixels>, view: Entity<V>) -> Self {
        ElementInputHandler {
            view,
            element_bounds,
        }
    }
}

impl<V: EntityInputHandler> InputHandler for ElementInputHandler<V> {
    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.view.update(cx, |view, cx| {
            view.selected_text_range(ignore_disabled_input, window, cx)
        })
    }

    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .update(cx, |view, cx| view.marked_text_range(window, cx))
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.view.update(cx, |view, cx| {
            view.text_for_range(range_utf16, adjusted_range, window, cx)
        })
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.replace_text_in_range(replacement_range, text, window, cx)
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(
                range_utf16,
                new_text,
                new_selected_range,
                window,
                cx,
            )
        });
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        self.view
            .update(cx, |view, cx| view.unmark_text(window, cx));
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut App) {
        self.view
            .update(cx, |view, cx| view.paste(item, window, cx));
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.view.update(cx, |view, cx| {
            view.bounds_for_range(range_utf16, self.element_bounds, window, cx)
        })
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<Bounds<Pixels>> {
        self.view.update(cx, |view, cx| {
            view.selection_bounds_for_range(range_utf16, self.element_bounds, window, cx)
        })
    }

    fn character_index_for_point(
        &mut self,
        point: crate::Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        self.view.update(cx, |view, cx| {
            view.character_index_for_point(point, window, cx)
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.set_selected_text_range(range_utf16, window, cx)
        })
    }

    fn element_bounds(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Bounds<Pixels>> {
        Some(self.element_bounds)
    }

    fn text_length_utf16(&mut self, window: &mut Window, cx: &mut App) -> Option<usize> {
        self.view
            .update(cx, |view, cx| view.text_length_utf16(window, cx))
    }

    fn accepts_text_input(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.view
            .update(cx, |view, cx| view.accepts_text_input(window, cx))
    }

    fn prefers_ime_for_printable_keys(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.view
            .update(cx, |view, cx| view.accepts_text_input(window, cx))
    }

    fn text_input_configuration(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) -> TextInputConfiguration {
        self.view
            .update(cx, |view, cx| view.text_input_configuration(window, cx))
    }

    fn text_input_editable_range(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Range<usize>> {
        self.view
            .update(cx, |view, cx| view.text_input_editable_range(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnyWindowHandle, AppContext as _, AutofillHint, FocusHandle, InputEvent as _,
        InteractiveElement as _, IntoElement, KeyboardType, MouseButton, ParentElement as _,
        PointerDevice, Render, Styled as _, TestAppContext, TextInputAction, TextInputStateChange,
        TouchEvent, TouchId, TouchPhase, canvas, div, point, px,
    };

    #[gpui::test]
    fn text_input_configuration_and_focus_state_are_forwarded_on_change(cx: &mut TestAppContext) {
        let custom = TextInputConfiguration {
            autocorrect: true,
            input_action: TextInputAction::Send,
            keyboard_type: KeyboardType::Email,
            secure: true,
            autofill: Some(AutofillHint::Username),
            ..Default::default()
        };
        let window = cx.add_window({
            let custom = custom.clone();
            move |_, cx| ConfigurationTestView {
                focus_handle: cx.focus_handle(),
                configuration: custom,
                accepts_text_input: true,
            }
        });
        let view = window.root(cx).unwrap();
        let test_window = cx.test_window(window.into());
        let window = AnyWindowHandle::from(window);
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
        };

        // Nothing is focused, so the platform learns the default configuration.
        draw(cx);
        assert_eq!(
            test_window.text_input_configurations(),
            vec![TextInputConfiguration::default()]
        );
        assert!(test_window.text_input_state_changes().is_empty());

        // Focusing the view routes its configuration to the platform.
        cx.update_window(window, |_, window, cx| {
            let focus_handle = view.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
        })
        .unwrap();
        draw(cx);
        assert_eq!(
            test_window.text_input_configurations(),
            vec![TextInputConfiguration::default(), custom.clone()]
        );
        assert_eq!(
            test_window.text_input_state_changes(),
            vec![TextInputStateChange::FocusGained]
        );

        // Redrawing without a change forwards nothing.
        draw(cx);
        assert_eq!(test_window.text_input_configurations().len(), 2);
        assert_eq!(test_window.text_input_state_changes().len(), 1);

        // Changing the configuration forwards the new value.
        let updated = TextInputConfiguration {
            suggestions: true,
            ..custom
        };
        view.update(cx, {
            let updated = updated.clone();
            |view, cx| {
                view.configuration = updated;
                cx.notify();
            }
        });
        draw(cx);
        assert_eq!(
            test_window.text_input_configurations().last(),
            Some(&updated)
        );
        assert_eq!(test_window.text_input_configurations().len(), 3);
        assert_eq!(test_window.text_input_state_changes().len(), 1);

        // Losing focus reverts the platform to the default configuration.
        cx.update_window(window, |_, window, cx| window.blur(cx))
            .unwrap();
        draw(cx);
        assert_eq!(
            test_window.text_input_configurations().last(),
            Some(&TextInputConfiguration::default())
        );
        assert_eq!(test_window.text_input_configurations().len(), 4);
        assert_eq!(
            test_window.text_input_state_changes(),
            vec![
                TextInputStateChange::FocusGained,
                TextInputStateChange::FocusLost
            ]
        );
    }

    #[gpui::test]
    fn text_notifications_require_a_focused_input_and_track_field_changes(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, cx| ConfigurationTestView {
            focus_handle: cx.focus_handle(),
            configuration: TextInputConfiguration::default(),
            accepts_text_input: true,
        });
        let view = window.root(cx).unwrap();
        let mut test_window = cx.test_window(window.into());
        let window = AnyWindowHandle::from(window);
        let first_focus = view.read_with(cx, |view, _| view.focus_handle.clone());
        cx.update_window(window, |_, window, cx| {
            window.focus(&first_focus, cx);
            window.draw(cx).clear(cx);
            window.notify_text_input_changed(&first_focus);
            window.notify_text_selection_changed(&first_focus);
        })
        .unwrap();

        use crate::PlatformWindow as _;
        let mut installed_handler = test_window.take_input_handler().unwrap();
        assert!(installed_handler.query_accepts_focused_text_input());
        assert_eq!(installed_handler.focus_id(), Some(first_focus.id));
        let second_focus = cx
            .update_window(window, |_, window, cx| {
                let second_focus = view.update(cx, |view, cx| {
                    view.focus_handle = cx.focus_handle();
                    cx.notify();
                    view.focus_handle.clone()
                });
                window.focus(&second_focus, cx);
                second_focus
            })
            .unwrap();
        assert!(!installed_handler.query_accepts_focused_text_input());
        test_window.set_input_handler(installed_handler);
        cx.update_window(window, |_, window, cx| {
            window.draw(cx).clear(cx);
            window.notify_text_input_changed(&first_focus);
        })
        .unwrap();
        assert_eq!(
            test_window.text_input_state_changes(),
            [
                TextInputStateChange::FocusGained,
                TextInputStateChange::ContentChanged,
                TextInputStateChange::SelectionChanged,
            ]
        );

        let installed_handler = test_window.take_input_handler().unwrap();
        assert_eq!(installed_handler.focus_id(), Some(second_focus.id));
        test_window.set_input_handler(installed_handler);
        cx.update_window(window, |_, window, cx| {
            window.draw(cx).clear(cx);
            window.notify_text_selection_changed(&second_focus);
        })
        .unwrap();

        view.update(cx, |view, cx| {
            view.accepts_text_input = false;
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.draw(cx).clear(cx);
            window.notify_text_input_changed(&second_focus);
            window.notify_text_selection_changed(&second_focus);
            let ordinary_focus = cx.focus_handle();
            window.focus(&ordinary_focus, cx);
            window.draw(cx).clear(cx);
            window.notify_text_input_changed(&ordinary_focus);
        })
        .unwrap();
        assert_eq!(
            test_window.text_input_state_changes(),
            [
                TextInputStateChange::FocusGained,
                TextInputStateChange::ContentChanged,
                TextInputStateChange::SelectionChanged,
                TextInputStateChange::SelectionChanged,
                TextInputStateChange::FocusLost,
            ]
        );
    }

    /// How focus leaves (or stays on) the field in [`focus_moves_end_the_left_fields_composition`].
    #[derive(Clone, Copy, Debug)]
    enum FocusMove {
        /// `Window::focus` on another element, then a frame.
        Other,
        /// `Window::blur`, then a frame.
        Blur,
        /// A tap on another element that focuses it: drawn inside the touch dispatch, no frame after.
        Tap,
        /// `Window::focus` on the field itself, then a frame.
        Same,
    }

    #[gpui::test]
    fn focus_moves_end_the_left_fields_composition(cx: &mut TestAppContext) {
        // (name, marked text before, how focus moves) → marked text after, `unmark_text` calls,
        // `PlatformWindow::end_composition` calls (once exactly when the field had marked text).
        type Row = (
            &'static str,
            Option<Range<usize>>,
            FocusMove,
            Option<Range<usize>>,
            usize,
            usize,
        );
        let rows: [Row; 5] = [
            (
                "another element, marked: unmarked",
                Some(0..1),
                FocusMove::Other,
                None,
                1,
                1,
            ),
            (
                "blur, marked: unmarked",
                Some(0..1),
                FocusMove::Blur,
                None,
                1,
                1,
            ),
            (
                "tap on another element, marked: unmarked",
                Some(0..1),
                FocusMove::Tap,
                None,
                1,
                1,
            ),
            (
                "another element, nothing marked: no call",
                None,
                FocusMove::Other,
                None,
                0,
                0,
            ),
            (
                "same element, marked: kept",
                Some(0..1),
                FocusMove::Same,
                Some(0..1),
                0,
                0,
            ),
        ];
        for (name, marked, focus_move, want_marked, want_unmarks, want_platform_ends) in rows {
            let window = cx.add_window(|_, cx| CompositionTestView {
                field: cx.focus_handle(),
                other: cx.focus_handle(),
                marked: None,
                unmarks: 0,
            });
            let view = window.root(cx).unwrap();
            let test_window = cx.test_window(window.into());
            let window = AnyWindowHandle::from(window);
            cx.update_window(window, |_, window, cx| {
                let field = view.read(cx).field.clone();
                window.focus(&field, cx);
                window.draw(cx).clear(cx);
            })
            .unwrap();
            view.update(cx, |view, _| view.marked = marked);
            cx.update_window(window, |_, window, cx| {
                let (field, other) =
                    view.read_with(cx, |view, _| (view.field.clone(), view.other.clone()));
                match focus_move {
                    FocusMove::Other => window.focus(&other, cx),
                    FocusMove::Blur => window.blur(cx),
                    FocusMove::Same => window.focus(&field, cx),
                    FocusMove::Tap => {
                        for phase in [TouchPhase::Started, TouchPhase::Ended] {
                            let touch = TouchEvent {
                                id: TouchId(1),
                                phase,
                                position: point(px(10.), px(75.)),
                                predicted_position: None,
                                force: None,
                                device: PointerDevice::Touch,
                                time_stamp: None,
                            };
                            window.dispatch_event(touch.to_platform_input(), cx);
                        }
                        return;
                    }
                }
                window.draw(cx).clear(cx);
            })
            .unwrap();
            let (marked, unmarks) =
                view.read_with(cx, |view, _| (view.marked.clone(), view.unmarks));
            assert_eq!(marked, want_marked, "{name}: marked text");
            assert_eq!(unmarks, want_unmarks, "{name}: unmark_text calls");
            assert_eq!(
                test_window.composition_ends(),
                want_platform_ends,
                "{name}: end_composition calls"
            );
        }
    }

    /// A field (top 50 px) with marked text to end, and another focusable element below it that a tap focuses.
    struct CompositionTestView {
        field: FocusHandle,
        other: FocusHandle,
        marked: Option<Range<usize>>,
        unmarks: usize,
    }

    impl Render for CompositionTestView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let field = self.field.clone();
            div()
                .size_full()
                .child(
                    div().h(px(50.)).w_full().track_focus(&self.field).child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                window.handle_input(
                                    &field,
                                    ElementInputHandler::new(bounds, view),
                                    cx,
                                );
                            },
                        )
                        .size_full(),
                    ),
                )
                .child(
                    div()
                        .h(px(50.))
                        .w_full()
                        .track_focus(&self.other)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, window, cx| window.focus(&view.other, cx)),
                        ),
                )
        }
    }

    impl EntityInputHandler for CompositionTestView {
        fn text_for_range(
            &mut self,
            _range: Range<usize>,
            _adjusted_range: &mut Option<Range<usize>>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<String> {
            Some("a".into())
        }

        fn selected_text_range(
            &mut self,
            _ignore_disabled_input: bool,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<UTF16Selection> {
            None
        }

        fn marked_text_range(
            &self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<Range<usize>> {
            self.marked.clone()
        }

        fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
            self.marked = None;
            self.unmarks += 1;
        }

        fn replace_text_in_range(
            &mut self,
            _range: Option<Range<usize>>,
            _text: &str,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) {
        }

        fn replace_and_mark_text_in_range(
            &mut self,
            _range: Option<Range<usize>>,
            _new_text: &str,
            _new_selected_range: Option<Range<usize>>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) {
        }

        fn bounds_for_range(
            &mut self,
            _range_utf16: Range<usize>,
            _element_bounds: Bounds<Pixels>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<Bounds<Pixels>> {
            None
        }

        fn character_index_for_point(
            &mut self,
            _point: crate::Point<Pixels>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<usize> {
            None
        }
    }

    struct ConfigurationTestView {
        focus_handle: FocusHandle,
        configuration: TextInputConfiguration,
        accepts_text_input: bool,
    }

    impl Render for ConfigurationTestView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let focus_handle = self.focus_handle.clone();
            div().size_full().track_focus(&self.focus_handle).child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, view),
                            cx,
                        );
                    },
                )
                .size_full(),
            )
        }
    }

    impl EntityInputHandler for ConfigurationTestView {
        fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
            self.accepts_text_input
        }

        fn text_for_range(
            &mut self,
            _range: std::ops::Range<usize>,
            _adjusted_range: &mut Option<std::ops::Range<usize>>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<String> {
            None
        }

        fn selected_text_range(
            &mut self,
            _ignore_disabled_input: bool,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<UTF16Selection> {
            None
        }

        fn marked_text_range(
            &self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<std::ops::Range<usize>> {
            None
        }

        fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

        fn replace_text_in_range(
            &mut self,
            _range: Option<std::ops::Range<usize>>,
            _text: &str,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) {
        }

        fn replace_and_mark_text_in_range(
            &mut self,
            _range: Option<std::ops::Range<usize>>,
            _new_text: &str,
            _new_selected_range: Option<std::ops::Range<usize>>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) {
        }

        fn bounds_for_range(
            &mut self,
            _range_utf16: std::ops::Range<usize>,
            _element_bounds: Bounds<Pixels>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<Bounds<Pixels>> {
            None
        }

        fn character_index_for_point(
            &mut self,
            _point: crate::Point<Pixels>,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> Option<usize> {
            None
        }

        fn text_input_configuration(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> TextInputConfiguration {
            self.configuration.clone()
        }
    }
}
