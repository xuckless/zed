//! Native views whose input reaches the window ([`NativeViewInput::Window`]: iOS, Windows, Wayland, Android).

use super::*;
use crate::{NativeViewInput, ScrollDelta};

impl TestView {
    pub(super) fn view_event(&mut self, kind: &str, position: Point<Pixels>, window: &Window) {
        let origin = self
            .native_view
            .bounds(window)
            .expect("the view hears input only where it shows")
            .origin;
        let position = position - origin;
        self.view_events.push(format!(
            "{kind} {} {}",
            f32::from(position.x),
            f32::from(position.y)
        ));
    }
}

/// Where the window gets a native view's input, as on Wayland, the view's
/// own element hears it with view-relative positions, and so do its ancestors (the
/// container, "behind view"), as any element's input. The view occludes like a
/// solid element: the element painted before it under it ("under view") hears
/// nothing over it, the element painted after it over it (the cover) keeps its
/// own input. The container is at (0, 10).
#[gpui::test]
fn window_input_over_a_native_view_reaches_its_element(cx: &mut TestAppContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-r", Reserved, None)]));
    let window = open_window(cx);
    cx.test_window(window.into())
        .set_native_view_input(NativeViewInput::Window);
    window
        .update(cx, |view, window, cx| {
            view.native_view = new_native_view(window, cx);
            view.native_view.reserve_action::<Reserved>();
            view.paints_under_view = true;
            window.focus(&view.native_view.focus_handle(cx), cx);
        })
        .unwrap();
    cx.run_until_parked();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let uncovered = point(px(75.), px(75.));
    let covered = point(px(25.), px(25.));
    let scroll = ScrollEvent {
        position: uncovered,
        delta: ScrollDelta::Pixels(point(px(0.), px(10.))),
        ..Default::default()
    };

    type Input = Box<dyn Fn(&mut VisualTestContext)>;
    // (row name, input, what the view's element hears, presses heard elsewhere). The
    // press on the cover moves focus to it, so it comes last.
    let rows: [(&str, Input, &[&str], &[&str]); 6] = [
        (
            "a key while the view is focused",
            Box::new(|cx| cx.simulate_keystrokes("a")),
            &["key a"],
            &[],
        ),
        (
            "a reserved key while the view is focused",
            Box::new(|cx| cx.simulate_keystrokes("cmd-r")),
            &[],
            &[],
        ),
        (
            "a press on the uncovered view",
            Box::new(move |cx| {
                cx.simulate_mouse_down(uncovered, MouseButton::Left, Modifiers::default())
            }),
            &["down 75 65"],
            &["outside cover", "behind view"],
        ),
        (
            "a move over the uncovered view",
            Box::new(move |cx| cx.simulate_mouse_move(uncovered, None, Modifiers::default())),
            &["move 75 65"],
            &[],
        ),
        (
            "a scroll over the uncovered view",
            Box::new(move |cx| cx.simulate_event(scroll.clone())),
            &["scroll 75 65"],
            &[],
        ),
        (
            "a press on the element painted over the view",
            Box::new(move |cx| {
                cx.simulate_mouse_down(covered, MouseButton::Left, Modifiers::default())
            }),
            &[],
            &[],
        ),
    ];
    for (name, input, view_events, presses) in rows {
        input(&mut cx);
        let (heard_by_view, heard_elsewhere) = window
            .update(&mut cx, |view, _, _| {
                (
                    std::mem::take(&mut view.view_events),
                    std::mem::take(&mut view.presses),
                )
            })
            .unwrap();
        assert_eq!(heard_by_view, view_events, "{name}: the view's element");
        assert_eq!(heard_elsewhere, presses, "{name}: the rest of the window");
    }
    let actions = window
        .update(&mut cx, |view, _, _| std::mem::take(&mut view.actions))
        .unwrap();
    assert_eq!(actions, ["reserved"]);
}

/// Where the window gets a focused native view's keys, the view's element
/// hears each first, as the view itself does on macOS; key bindings get the keys
/// it leaves. Reserved bindings and the rest of a pending binding go first and
/// never reach the element. The element uses "q" and leaves every other key.
#[gpui::test]
fn window_keys_reach_the_view_element_before_key_bindings(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("cmd-r", Reserved, None),
            KeyBinding::new("cmd-o", Ordinary, None),
            KeyBinding::new("q", Ordinary, None),
            KeyBinding::new("cmd-k o", Ordinary, None),
        ])
    });
    let window = open_window(cx);
    cx.test_window(window.into())
        .set_native_view_input(NativeViewInput::Window);
    window
        .update(cx, |view, window, cx| {
            view.native_view = new_native_view(window, cx);
            view.native_view.reserve_action::<Reserved>();
            window.focus(&view.native_view.focus_handle(cx), cx);
        })
        .unwrap();
    cx.run_until_parked();

    // (row name, keystrokes, what the view's element hears, actions run)
    let rows: [(&str, &str, &[&str], &[&str]); 5] = [
        (
            "a bound key the element leaves reaches it, then its binding",
            "cmd-o",
            &["key o"],
            &["ordinary"],
        ),
        (
            "a bound key the element uses never reaches its binding",
            "q",
            &["key q"],
            &[],
        ),
        (
            "a reserved key goes to its binding and never to the element",
            "cmd-r",
            &[],
            &["reserved"],
        ),
        (
            "the first key of a longer binding reaches the element, then starts the binding",
            "cmd-k",
            &["key k"],
            &[],
        ),
        (
            "a key that continues a pending binding goes to it, not to the element",
            "o",
            &[],
            &["ordinary"],
        ),
    ];
    for (name, keystrokes, view_events, actions) in rows {
        cx.simulate_keystrokes(window.into(), keystrokes);
        let (heard, ran) = window
            .update(cx, |view, _, _| {
                (
                    std::mem::take(&mut view.view_events),
                    std::mem::take(&mut view.actions),
                )
            })
            .unwrap();
        assert_eq!(heard, view_events, "{name}: the view's element");
        assert_eq!(ran, actions, "{name}: key bindings");
    }
}
