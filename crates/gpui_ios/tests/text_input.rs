#[cfg(target_os = "ios")]
#[path = "../src/ios/text_input.rs"]
mod text_input;

// text_input.rs moves keyboard input with native_view_state.rs's pure transitions.
#[cfg(target_os = "ios")]
#[path = "../src/ios/native_view_state.rs"]
#[allow(dead_code, unused_imports, reason = "text_input.rs uses only its keyboard transitions here; its tests run in scripts/patch-test.sh's ios-pure crate")]
mod native_view_state;

// Native protocol construction does not need a GPUI window, renderer, or a
// second text model. This fixture deliberately has no registered input handler.
#[cfg(target_os = "ios")]
mod window {
    use gpui::PlatformInputHandler;
    use std::{
        cell::{Cell, RefCell},
        rc::Weak,
    };

    thread_local! {
        static HANDLER_QUERIES: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn handler_queries() -> usize {
        HANDLER_QUERIES.with(Cell::get)
    }

    /// Upstream's protocol tests never upgrade to one. Flutter's oracle rows each make one around their fake app.
    pub(crate) struct IosWindowState(pub(crate) RefCell<PlatformInputHandler>);

    impl IosWindowState {
        pub(crate) fn with_input_handler<R>(
            &self,
            callback: impl FnOnce(&mut PlatformInputHandler) -> R,
        ) -> Option<R> {
            Some(callback(&mut self.0.borrow_mut()))
        }
    }

    #[derive(Default)]
    pub(crate) struct WindowReference(pub(crate) RefCell<Weak<IosWindowState>>);

    impl WindowReference {
        pub(crate) fn with_window<R>(
            &self,
            callback: impl FnOnce(&IosWindowState) -> R,
        ) -> Option<R> {
            HANDLER_QUERIES.with(|queries| queries.set(queries.get() + 1));
            self.0.borrow().upgrade().map(|window| callback(&window))
        }
    }
}

fn main() {
    #[cfg(target_os = "ios")]
    {
        let main_thread = objc2::MainThreadMarker::new().expect("native test runs on main thread");
        text_input::tests::native_protocol_smoke_test(main_thread);
        text_input::tests::stale_objects_do_not_route_to_handler(
            main_thread,
            window::handler_queries,
        );
        println!("UIKit text input native protocol smoke test passed");

        // Flutter's FlutterTextInputPluginTest.mm cases; an argument runs only the rows whose name contains it.
        let filter = std::env::args().nth(1).unwrap_or_default();
        let attach = |view: &text_input::TextInputView, handler| -> Box<dyn std::any::Any> {
            let state = std::rc::Rc::new(window::IosWindowState(std::cell::RefCell::new(handler)));
            view.set_window(std::rc::Rc::downgrade(&state));
            Box::new(state)
        };
        if !text_input::flutter_oracle::run(main_thread, &filter, &attach) {
            eprintln!("Flutter text input oracle: rows failed");
            std::process::exit(1);
        }
        println!("Flutter text input oracle passed");
    }
}
