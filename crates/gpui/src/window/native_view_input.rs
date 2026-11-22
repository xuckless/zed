//! Who gets pointer input over a native view, per platform (`PlatformWindow::native_view_input`),
//! and how a view whose input reaches the window occludes what is behind it.

use crate::{DispatchNodeId, DispatchTree, Hitbox, HitboxBehavior};
use smallvec::SmallVec;

/// Who a platform gives pointer input over a shown native view to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NativeViewInput {
    /// The view itself, as on macOS, where AppKit hit-tests the `NSView`.
    /// The view's element blocks the mouse from everything behind it.
    View,
    /// The window's own content, as on iOS, Windows, Wayland and Android, where the view
    /// takes no input of its own. The view's element occludes like a solid
    /// element: it and its ancestors hear the input, elements painted behind it
    /// don't, elements painted after it keep theirs. The view hears nothing
    /// unless the app forwards it.
    Window,
}

impl NativeViewInput {
    /// The hitbox behavior of a native view's element. Only the occurrence that
    /// shows the view occludes; a later one paints nothing.
    pub(super) fn hitbox_behavior(self, shows_view: bool) -> HitboxBehavior {
        match (self, shows_view) {
            (NativeViewInput::View, true) => HitboxBehavior::BlockMouse,
            (NativeViewInput::Window, true) => HitboxBehavior::BlockMouseExceptAncestors,
            (NativeViewInput::View | NativeViewInput::Window, false) => HitboxBehavior::Normal,
        }
    }
}

/// How far a hit test has got, front to back: past a
/// [`HitboxBehavior::BlockMouseExceptAncestors`] hitbox, only the dispatch
/// nodes on its path (its element's ancestors) are still hit.
pub(super) enum Occlusion {
    None,
    ExceptAncestors(SmallVec<[DispatchNodeId; 32]>),
}

impl Occlusion {
    /// Whether `hitbox`, behind every hitbox seen so far, is hidden.
    pub(super) fn hides(&self, hitbox: &Hitbox) -> bool {
        match self {
            Occlusion::None => false,
            Occlusion::ExceptAncestors(path) => !hitbox
                .dispatch_node
                .is_some_and(|node| path.contains(&node)),
        }
    }

    /// The occlusion for the hitboxes behind `hitbox`, a hit one.
    pub(super) fn behind(self, hitbox: &Hitbox, tree: &DispatchTree) -> Occlusion {
        match (self, hitbox.behavior) {
            (Occlusion::None, HitboxBehavior::BlockMouseExceptAncestors) => {
                Occlusion::ExceptAncestors(
                    hitbox
                        .dispatch_node
                        .map(|node| tree.dispatch_path(node))
                        .unwrap_or_default(),
                )
            }
            // Already behind a view (its ancestors stay hit), or a hitbox that
            // occludes no other way than before.
            (occlusion @ Occlusion::ExceptAncestors(_), HitboxBehavior::BlockMouseExceptAncestors)
            | (
                occlusion,
                HitboxBehavior::Normal
                | HitboxBehavior::BlockMouse
                | HitboxBehavior::BlockMouseExceptScroll,
            ) => occlusion,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_view_occludes_only_where_it_shows() {
        // (name, input, shows view, behavior)
        let cases = [
            (
                "macOS, shown",
                NativeViewInput::View,
                true,
                HitboxBehavior::BlockMouse,
            ),
            (
                "macOS, shown elsewhere",
                NativeViewInput::View,
                false,
                HitboxBehavior::Normal,
            ),
            (
                "iOS, Windows, Wayland, shown",
                NativeViewInput::Window,
                true,
                HitboxBehavior::BlockMouseExceptAncestors,
            ),
            (
                "iOS, Windows, Wayland, shown elsewhere",
                NativeViewInput::Window,
                false,
                HitboxBehavior::Normal,
            ),
        ];
        for (name, input, shows_view, behavior) in cases {
            assert_eq!(input.hitbox_behavior(shows_view), behavior, "{name}");
        }
    }
}
