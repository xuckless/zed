//! GPUI's DirectComposition device, for apps that host content which wants a visual in the window's own tree rather
//! than content for one: WebView2's `CreateCoreWebView2CompositionController` renders into a `RootVisualTarget`
//! visual, which must be made on the device of the tree it joins (docs/native-view-windows.md in Parallax). The app
//! makes a visual on this device and hands it to `NativeView::new`; `NativeViewTree::attach` then adds it under the
//! view's clip instead of setting it as content.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{Foundation::HWND, Graphics::DirectComposition::IDCompositionDevice};

use crate::{WindowsWindowInner, window_from_hwnd};

/// The DirectComposition device of GPUI's window behind `window` (a `gpui::Window`, through its window handle).
/// `None` off the main thread's own windows, when GPUI_DISABLE_DIRECT_COMPOSITION turned DirectComposition off, or
/// while the window draws. A lost GPU device makes a new one: a visual made on the old device cannot join the new tree
/// ([`on_composition_device_changed`]).
pub fn composition_device(window: &impl HasWindowHandle) -> Option<IDCompositionDevice> {
    let window = windows_window(window)?;
    let renderer = window.state.renderer.try_borrow().ok()?;
    Some(renderer.direct_composition.as_ref()?.comp_device.clone())
}

/// Calls `callback` each time the window behind `window` gets a new DirectComposition device (GPUI recovered from a
/// lost GPU device), once the new device's tree is in place. A visual made on the old device
/// ([`composition_device`]) cannot join the new tree, so its native view shows nothing until the app makes its visual
/// again on the new device (WebView2: a new composition controller) and attaches a new `NativeView`. The callback runs
/// inside GPUI's handling of the device loss: schedule that work (an `AsyncApp` spawn), don't do it there. Returns
/// false where [`composition_device`] gives `None`.
pub fn on_composition_device_changed(
    window: &impl HasWindowHandle,
    callback: impl FnMut() + 'static,
) -> bool {
    let Some(window) = windows_window(window) else {
        return false;
    };
    let Ok(mut renderer) = window.state.renderer.try_borrow_mut() else {
        return false;
    };
    if renderer.direct_composition.is_none() {
        return false;
    }
    renderer
        .native_views
        .composition_device_changed
        .push(Box::new(callback));
    true
}

/// After a device loss was recovered, outside the renderer's borrow: tells the app
/// ([`on_composition_device_changed`]). Callbacks registered meanwhile are kept too.
pub(crate) fn composition_device_changed(window: &WindowsWindowInner) {
    let mut callbacks = std::mem::take(
        &mut window
            .state
            .renderer
            .borrow_mut()
            .native_views
            .composition_device_changed,
    );
    for callback in &mut callbacks {
        callback();
    }
    let mut renderer = window.state.renderer.borrow_mut();
    callbacks.append(&mut renderer.native_views.composition_device_changed);
    renderer.native_views.composition_device_changed = callbacks;
}

fn windows_window(window: &impl HasWindowHandle) -> Option<std::rc::Rc<WindowsWindowInner>> {
    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    window_from_hwnd(HWND(handle.hwnd.get() as _))
}
