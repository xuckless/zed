//! Overscroll, ported from Flutter: Android's stretch and glow indicators (pure values and functions of time, driven
//! by scroll notifications) and the scroll position glue that drives them and the bouncing physics from a scroll
//! container's touch scrolls. The physics (`ScrollPhysics`, the springs) is `gestures/scroll_physics.rs` and
//! `gpui::physics`.

mod overscroll_indicator;
mod scroll_position;

pub use overscroll_indicator::*;
pub use scroll_position::Overscroll;
pub(crate) use scroll_position::{
    AxisExtents, DragSource, DragStep, GLOW_COLOR, OverscrollPosition, ScrollInput,
    stretch_displacement,
};
