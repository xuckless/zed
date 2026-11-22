//! The state of Android's stretch and glow overscroll indicators, ported from Flutter: pure values and functions of
//! time, driven by scroll notifications; nothing here is wired into GPUI's scroll containers yet. Their physics
//! (`ScrollPhysics`, the springs) is `gestures/scroll_physics.rs` and `gpui::physics`.

mod overscroll_indicator;

pub use overscroll_indicator::*;
