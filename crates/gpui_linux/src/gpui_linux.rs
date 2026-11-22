#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::{current_platform, linux_platform};
/// A Wayland native view's buffers: how the app commits to the `wl_surface` it gave `NativeView::new`.
#[cfg(feature = "wayland")]
pub use linux::{Committed, NativeViewBuffers};
