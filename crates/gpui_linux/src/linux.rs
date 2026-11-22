#[cfg(any(feature = "wayland", feature = "x11"))]
mod clock;
mod dispatcher;
mod display_connection;
mod headless;
mod keyboard;
mod platform;
#[cfg(any(feature = "wayland", feature = "x11"))]
mod power;
mod system_notifications;
#[cfg(any(feature = "wayland", feature = "x11"))]
mod text_system;
#[cfg(feature = "wayland")]
mod wayland;
#[cfg(feature = "x11")]
mod x11;

#[cfg(any(feature = "wayland", feature = "x11"))]
mod xdg_desktop_portal;

pub use dispatcher::*;
pub(crate) use display_connection::{Backend, DisplayConnection, select_backend};
pub(crate) use headless::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
#[cfg(any(feature = "wayland", feature = "x11"))]
pub(crate) use text_system::*;
#[cfg(feature = "wayland")]
pub(crate) use wayland::*;
#[cfg(feature = "x11")]
pub(crate) use x11::*;

use std::rc::Rc;

use gpui::WindowingModes;

/// Whether `input` reaches a window a modal child blocks. A touch's end or cancel does, or GPUI's touch would stay
/// open once the child closes; other input is dropped, as upstream drops all of it.
#[cfg(any(feature = "wayland", feature = "x11"))]
pub(crate) fn reaches_blocked_window(input: &gpui::PlatformInput) -> bool {
    use gpui::{PlatformInput, TouchEvent, TouchPhase};
    matches!(
        input,
        PlatformInput::Touch(TouchEvent {
            phase: TouchPhase::Ended | TouchPhase::Cancelled,
            ..
        })
    )
}

#[cfg(all(test, any(feature = "wayland", feature = "x11")))]
mod tests {
    use gpui::{PlatformInput, TouchEvent, TouchPhase};

    #[test]
    fn blocked_window_input() {
        let touch = |phase| {
            PlatformInput::Touch(TouchEvent {
                phase,
                ..TouchEvent::default()
            })
        };
        let rows = [
            ("a touch ends", touch(TouchPhase::Ended), true),
            ("a touch is cancelled", touch(TouchPhase::Cancelled), true),
            (
                "rejected: a touch starts",
                touch(TouchPhase::Started),
                false,
            ),
            ("rejected: a touch moves", touch(TouchPhase::Moved), false),
            (
                "rejected: a key",
                PlatformInput::ModifiersChanged(Default::default()),
                false,
            ),
        ];
        for (name, input, reaches) in rows {
            assert_eq!(super::reaches_blocked_window(&input), reaches, "{name}");
        }
    }
}

/// Returns the default platform implementation for the current OS.
///
/// A windowed platform connects to the display server the process environment names, or starts
/// headless when it names none, and can switch between those modes later. A headless platform
/// stays headless.
pub fn current_platform(headless: bool) -> Rc<dyn gpui::Platform> {
    if headless || std::env::var_os("ZED_HEADLESS").is_some() {
        linux_platform(WindowingModes::HEADLESS)
    } else {
        linux_platform(WindowingModes::all())
    }
}

/// Returns a platform that may switch among `allowed_modes`.
///
/// It starts windowed in the process's own environment, or headless if that names no allowed
/// display server. Set another initial mode with [`gpui::Application::with_windowing`].
pub fn linux_platform(allowed_modes: WindowingModes) -> Rc<dyn gpui::Platform> {
    Rc::new(LinuxPlatform::new(allowed_modes))
}
