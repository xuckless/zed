// Ported from chromium/chromium ui/ozone/platform/wayland/host/wayland_subsurface.cc and
// ui/ozone/platform/wayland/host/wayland_frame_manager.cc @ 4ea2b624ed0c500ec2f23ff17203dff91ad8faa2.
// Copyright 2020 The Chromium Authors. BSD-3-Clause, see LICENSE-CHROMIUM.
//
// A native view on Wayland is a `wl_surface` the app made on GPUI's connection and fills with its own
// buffers. GPUI makes it a `wl_subsurface` below the window's surface and places it each frame, while
// the renderer cuts a hole in the window where it shows. Chromium does the same for its overlays; this
// keeps its names and comments. Its buffer bookkeeping and presentation feedback are left out: the app
// owns the buffers and commits them to its surface itself.

use std::{
    any::Any,
    cell::RefCell,
    ffi::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use gpui::{
    Bounds, NativeViewId, PaintNativeView, Scene,
    native_view_placement::{hole_inset, round_outwards, visible},
};
use wayland_client::{
    Proxy, QueueHandle,
    protocol::{wl_region, wl_subcompositor, wl_subsurface, wl_surface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport;

use super::WaylandWindowState;
use crate::linux::{Globals, WaylandClientStatePtr};

/// A window's native views, owned by the app's `NativeView` handles (a view is gone once its last
/// handle is dropped), and what GPUI placed of them.
#[derive(Default)]
pub(crate) struct NativeViews {
    views: Vec<Weak<RefCell<WaylandSubsurface>>>,
    /// What the last drawn frame placed.
    placed: Frame,
    /// What the window's last commit shows. A frame whose draw fails places this again, so the
    /// retry that commits the window's old buffer shows the views where that buffer's holes are.
    committed: Frame,
}

/// What one frame shows of the window's native views.
#[derive(Clone, Debug, Default, PartialEq)]
struct Frame {
    /// The views shown, back to front (paint order), each where it shows.
    views: Vec<(NativeViewId, Shown)>,
}

impl Frame {
    fn new(scene: &Scene, scale: f32) -> Self {
        let views: Vec<_> = scene
            .native_views
            .iter()
            .filter_map(|painted| shown(painted, scale).map(|shown| (painted.id, shown)))
            .collect();
        Self { views }
    }
}

/// `PlatformWindow::attach_native_view` on Wayland: the app's `wl_surface` becomes a view of
/// this window. `None` when the compositor lacks `wl_subcompositor` or `wp_viewporter`.
///
/// # Safety
///
/// `view` must be a live `wl_proxy` of a role-less `wl_surface` made on the window's display.
pub(crate) unsafe fn attach(
    state: &mut WaylandWindowState,
    id: NativeViewId,
    view: NonNull<c_void>,
) -> Option<Box<dyn Any>> {
    let viewporter = state.globals.viewporter.as_ref()?;
    state.globals.subcompositor.as_ref()?;
    // The window's surface must blend for a view to show through its hole; without, the app
    // gets NativeView::new's error and can fall back.
    if let Some(Err(unsupported)) = state
        .renderer
        .as_ref()
        .map(|renderer| renderer.native_view_support())
    {
        log::error!("native views: {unsupported}");
        return None;
    }
    let connection = wayland_client::Connection::from_backend(state.surface.backend().upgrade()?);
    // SAFETY: the caller passes a live wl_surface proxy made on this display.
    let surface = unsafe { app_surface(&connection, view) }?;
    let viewport = viewporter.get_viewport(&surface, &state.globals.qh, ());
    let subsurface = Rc::new(RefCell::new(WaylandSubsurface::new(id, surface, viewport)));
    state.native_views.views.push(Rc::downgrade(&subsurface));
    Some(Box::new(subsurface))
}

/// Places every view where `scene` shows it, before the window commits the frame, so a frame that
/// moves, resizes, shows, hides or restacks one carries its subsurface's change in that same commit.
/// Returns whether the frame places views (macOS's `places_views()`), so the opaque region needs
/// updating.
pub(crate) fn place_views(state: &mut WaylandWindowState, scene: &Scene) -> bool {
    if let Some(renderer) = &mut state.renderer {
        renderer.set_native_view_hole_inset(hole_inset(state.scale.fract() != 0.));
    }
    let frame = Frame::new(scene, state.scale);
    place(state, frame)
}

/// The frame's draw committed the window: shown views go back to desynchronized, so the app's
/// commits show without waiting for a window commit.
pub(crate) fn frame_committed(native_views: &mut NativeViews) {
    for view in native_views.views.iter().filter_map(Weak::upgrade) {
        view.borrow_mut().frame_committed();
    }
    native_views.committed = native_views.placed.clone();
}

/// The frame's draw failed, so the window's next commit (a retry) carries its old buffer: the views
/// go back to where that buffer's holes are, until a draw succeeds.
pub(crate) fn frame_failed(state: &mut WaylandWindowState) {
    let committed = state.native_views.committed.clone();
    place(state, committed);
}

/// Takes the shown views out of the window's opaque region: the compositor must blend their holes,
/// and can still treat the rest of the window as opaque.
pub(crate) fn subtract_holes(state: &WaylandWindowState, region: &wl_region::WlRegion) {
    for (_, shown) in &state.native_views.placed.views {
        let hole = shown.bounds;
        region.subtract(
            hole.origin.x,
            hole.origin.y,
            hole.size.width,
            hole.size.height,
        );
    }
}

fn place(state: &mut WaylandWindowState, frame: Frame) -> bool {
    let WaylandWindowState {
        native_views,
        surface,
        globals,
        ..
    } = state;
    native_views.views.retain(|view| view.strong_count() > 0);
    let Some(subcompositor) = &globals.subcompositor else {
        return false;
    };
    if frame == native_views.placed {
        return false;
    }
    let parent = Parent {
        surface,
        globals,
        subcompositor,
        qh: &globals.qh,
    };
    let views: Vec<_> = native_views
        .views
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    for view in &views {
        let mut view = view.borrow_mut();
        let shown_at = frame
            .views
            .iter()
            .find(|(id, _)| *id == view.id)
            .map(|(_, shown)| *shown);
        view.configure(shown_at, &parent);
    }
    let stacking = |frame: &Frame| frame.views.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    // A new subsurface starts on top of the window, and the views' order must follow their paint
    // order, so the whole stack is set again whenever it changes. Each `place_below(window)` puts
    // the subsurface right under the window, so the last one placed is the frontmost.
    if stacking(&frame) != stacking(&native_views.placed) {
        for (id, _) in &frame.views {
            for view in &views {
                let view = view.borrow();
                if view.id == *id
                    && let Placement::Shown { subsurface, .. } = &view.placement
                {
                    subsurface.place_below(parent.surface);
                }
            }
        }
    }
    native_views.placed = frame;
    true
}

/// Where a native view's subsurface is, in the window surface's logical pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Shown {
    bounds: Bounds<i32>,
}

/// Where a painted view shows: its subsurface covers the element's bounds. `wl_subsurface.set_position`
/// and `wp_viewport.set_destination` take whole logical pixels, so the subsurface's bounds are rounded
/// outwards from the device pixels GPUI paints in (`round_outwards`; Chromium's
/// `AdjustSubsurfaceBounds` scales the same way). `None` when the clip leaves none of it.
fn shown(painted: &PaintNativeView, scale: f32) -> Option<Shown> {
    visible(painted)?;
    Some(Shown {
        bounds: round_outwards(painted.bounds, scale),
    })
}

/// The subsurface's commit mode. Placement changes land in the window's next commit only while it
/// is synchronized, and the app's own commits show at once only while it is not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    Synchronized,
    Desynchronized,
}

/// A view's subsurface, `S` (a `wl_subsurface`, or a stand-in in the tests).
#[derive(Debug, PartialEq)]
enum Placement<S> {
    /// Never shown, so the surface has no role yet.
    Unmapped,
    Shown {
        subsurface: S,
        shown: Shown,
        mode: Mode,
    },
    /// Shown before; its subsurface stays, synchronized and with no buffer, until it shows again
    /// (Chromium's mutter workaround: mutter may never release a buffer whose surface lost its role).
    Hidden { subsurface: S },
}

/// What a frame does to a view's subsurface, holding the subsurface it acts on.
#[derive(Debug, PartialEq)]
enum Step<S> {
    Keep(Placement<S>),
    /// The first show: a new subsurface.
    Create(Shown),
    /// Shown again after a hide, on the subsurface kept since.
    Show {
        subsurface: S,
        shown: Shown,
    },
    Move {
        subsurface: S,
        mode: Mode,
        from: Shown,
        to: Shown,
    },
    Hide {
        subsurface: S,
        mode: Mode,
    },
}

/// `placement × where the frame shows the view → step`. The one place placement changes are decided.
fn step<S>(placement: Placement<S>, shown_at: Option<Shown>) -> Step<S> {
    match (placement, shown_at) {
        (placement @ (Placement::Unmapped | Placement::Hidden { .. }), None) => {
            Step::Keep(placement)
        }
        (Placement::Unmapped, Some(shown)) => Step::Create(shown),
        (Placement::Hidden { subsurface }, Some(shown)) => Step::Show { subsurface, shown },
        (placement @ Placement::Shown { shown: from, .. }, Some(to)) if from == to => {
            Step::Keep(placement)
        }
        (
            Placement::Shown {
                subsurface,
                shown: from,
                mode,
            },
            Some(to),
        ) => Step::Move {
            subsurface,
            mode,
            from,
            to,
        },
        (
            Placement::Shown {
                subsurface, mode, ..
            },
            None,
        ) => Step::Hide { subsurface, mode },
    }
}

/// A native view's `wl_surface` as a subsurface of a window. Dropping it destroys the subsurface
/// and viewport, which unmaps the surface at once.
pub(crate) struct WaylandSubsurface {
    id: NativeViewId,
    /// The app's surface. GPUI only ever adds the subsurface role, the viewport and the empty input
    /// region to it, commits it while placing it, and attaches no buffer but a null one.
    wayland_surface: wl_surface::WlSurface,
    viewport: wp_viewport::WpViewport,
    placement: Placement<wl_subsurface::WlSubsurface>,
}

impl WaylandSubsurface {
    fn new(
        id: NativeViewId,
        wayland_surface: wl_surface::WlSurface,
        viewport: wp_viewport::WpViewport,
    ) -> Self {
        Self {
            id,
            wayland_surface,
            viewport,
            placement: Placement::Unmapped,
        }
    }

    /// Places the view where this frame shows it, before the window commits the frame, so the
    /// placement lands in that same commit.
    fn configure(&mut self, shown_at: Option<Shown>, parent: &Parent<'_>) {
        let placement = std::mem::replace(&mut self.placement, Placement::Unmapped);
        self.placement = match step(placement, shown_at) {
            Step::Keep(placement) => placement,
            // WaylandSubsurface::Show(), on first show.
            Step::Create(shown) => {
                let subsurface = self.create_subsurface(parent);
                self.configure_and_show_surface(&subsurface, None, shown);
                Placement::Shown {
                    subsurface,
                    shown,
                    mode: Mode::Synchronized,
                }
            }
            // WaylandSubsurface::Show() makes a fresh subsurface every time; the one kept since the
            // hide still has its role, so it is shown again instead.
            Step::Show { subsurface, shown } => {
                self.configure_and_show_surface(&subsurface, None, shown);
                Placement::Shown {
                    subsurface,
                    shown,
                    mode: Mode::Synchronized,
                }
            }
            Step::Move {
                subsurface,
                mode,
                from,
                to,
            } => {
                set_sync(&subsurface, mode);
                self.configure_and_show_surface(&subsurface, Some(from), to);
                Placement::Shown {
                    subsurface,
                    shown: to,
                    mode: Mode::Synchronized,
                }
            }
            // WaylandSubsurface::Hide(), with the hiding half of WaylandFrameManager::PlayBackFrame().
            Step::Hide { subsurface, mode } => {
                set_sync(&subsurface, mode);
                // Mutter sometimes does not call buffer.release if wl_surface role is destroyed,
                // causing graphics freeze. (Chromium, wayland_frame_manager.cc.) So the role stays,
                // and the buffer is taken off the surface instead, in step with the window's commit.
                self.wayland_surface.attach(None, 0, 0);
                self.wayland_surface.commit();
                Placement::Hidden { subsurface }
            }
        };
    }

    /// Called once the window has committed a frame.
    fn frame_committed(&mut self) {
        if let Placement::Shown {
            subsurface, mode, ..
        } = &mut self.placement
            && *mode == Mode::Synchronized
        {
            subsurface.set_desync();
            *mode = Mode::Desynchronized;
        }
    }

    // WaylandSubsurface::CreateSubsurface()
    fn create_subsurface(&self, parent: &Parent<'_>) -> wl_subsurface::WlSubsurface {
        let subsurface = parent.subcompositor.get_subsurface(
            &self.wayland_surface,
            parent.surface,
            parent.qh,
            (),
        );
        // A new sub-surface is initially added as the top-most in the stack (`place` restacks it).
        subsurface.set_sync();
        set_empty_input_region(&self.wayland_surface, parent);
        subsurface
    }

    // WaylandSubsurface::ConfigureAndShowSurface()
    fn configure_and_show_surface(
        &self,
        subsurface: &wl_subsurface::WlSubsurface,
        previous: Option<Shown>,
        shown: Shown,
    ) {
        let bounds = shown.bounds;
        if previous.map(|previous| previous.bounds.origin) != Some(bounds.origin) {
            subsurface.set_position(bounds.origin.x, bounds.origin.y);
        }
        if previous.map(|previous| previous.bounds.size) != Some(bounds.size) {
            self.viewport
                .set_destination(bounds.size.width.max(1), bounds.size.height.max(1));
        }
        // Synchronized, so the commit is cached until the window's.
        self.wayland_surface.commit();
    }
}

fn set_sync(subsurface: &wl_subsurface::WlSubsurface, mode: Mode) {
    // mode → request. Only a desynchronized subsurface needs telling.
    match mode {
        Mode::Synchronized => {}
        Mode::Desynchronized => subsurface.set_sync(),
    }
}

// Subsurfaces don't need to trap input events. Its display rect is fully contained in |parent_|'s.
// Setting input_region to empty allows |parent_| to dispatch all of the input to platform window.
fn set_empty_input_region(surface: &wl_surface::WlSurface, parent: &Parent<'_>) {
    let region = parent.globals.compositor.create_region(parent.qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();
}

impl Drop for WaylandSubsurface {
    fn drop(&mut self) {
        // The viewport must go before the surface, which the app destroys after this.
        if let Placement::Shown { subsurface, .. } | Placement::Hidden { subsurface } =
            &self.placement
        {
            subsurface.destroy();
        }
        self.viewport.destroy();
    }
}

/// The window surface a view's subsurface belongs to, and the globals that make one.
struct Parent<'a> {
    surface: &'a wl_surface::WlSurface,
    globals: &'a Globals,
    subcompositor: &'a wl_subcompositor::WlSubcompositor,
    qh: &'a QueueHandle<WaylandClientStatePtr>,
}

/// Wraps the app's `wl_surface` proxy so GPUI can send requests on it.
///
/// # Safety
///
/// `proxy` must be a live `wl_proxy` of a `wl_surface` made on `connection`'s display.
unsafe fn app_surface(
    connection: &wayland_client::Connection,
    proxy: std::ptr::NonNull<std::ffi::c_void>,
) -> Option<wl_surface::WlSurface> {
    // SAFETY: the caller guarantees a live wl_surface proxy; from_ptr checks its interface.
    let id = unsafe {
        wayland_backend::client::ObjectId::from_ptr(
            wl_surface::WlSurface::interface(),
            proxy.as_ptr().cast(),
        )
    }
    .ok()?;
    wl_surface::WlSurface::from_id(connection, id).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Bounds<i32> {
        Bounds {
            origin: point(x, y),
            size: size(width, height),
        }
    }

    fn whole(bounds: Bounds<i32>) -> Shown {
        Shown { bounds }
    }

    #[test]
    fn steps() {
        let a = whole(rect(0, 0, 10, 10));
        let b = whole(rect(5, 0, 10, 10));
        let (sync, desync) = (Mode::Synchronized, Mode::Desynchronized);
        // The subsurface is a stand-in: `()`.
        let shown = |shown, mode| Placement::Shown {
            subsurface: (),
            shown,
            mode,
        };
        let hidden = || Placement::Hidden { subsurface: () };
        // (row name, placement, where the frame shows the view, step)
        let rows = [
            (
                "unmapped and not painted",
                Placement::Unmapped,
                None,
                Step::Keep(Placement::Unmapped),
            ),
            (
                "first paint creates the subsurface",
                Placement::Unmapped,
                Some(a),
                Step::Create(a),
            ),
            (
                "same bounds keep",
                shown(a, desync),
                Some(a),
                Step::Keep(shown(a, desync)),
            ),
            (
                "new bounds move a desynchronized view",
                shown(a, desync),
                Some(b),
                Step::Move {
                    subsurface: (),
                    mode: desync,
                    from: a,
                    to: b,
                },
            ),
            (
                "new bounds move a view still synchronized",
                shown(a, sync),
                Some(b),
                Step::Move {
                    subsurface: (),
                    mode: sync,
                    from: a,
                    to: b,
                },
            ),
            (
                "not painted hides",
                shown(a, desync),
                None,
                Step::Hide {
                    subsurface: (),
                    mode: desync,
                },
            ),
            (
                "hidden and not painted",
                hidden(),
                None,
                Step::Keep(hidden()),
            ),
            (
                "painted again shows on the kept subsurface",
                hidden(),
                Some(b),
                Step::Show {
                    subsurface: (),
                    shown: b,
                },
            ),
        ];
        for (name, placement, shown_at, expected) in rows {
            assert_eq!(step(placement, shown_at), expected, "{name}");
        }
    }
}
