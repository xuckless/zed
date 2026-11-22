// Ported from chromium/chromium ui/ozone/platform/wayland/host/wayland_subsurface.cc and
// ui/ozone/platform/wayland/host/wayland_frame_manager.cc @ 4ea2b624ed0c500ec2f23ff17203dff91ad8faa2.
// Copyright 2020 The Chromium Authors. BSD-3-Clause, see LICENSE-CHROMIUM.
//
// A native view on Wayland is a `wl_surface` the app made on GPUI's connection and fills with its own
// buffers. GPUI makes it a `wl_subsurface` below the window's surface and places it each frame, while
// the renderer cuts a hole in the window where it shows. Chromium does the same for its overlays; this
// keeps its names and comments. Its buffer bookkeeping and presentation feedback are left out: the app
// owns the buffers, and commits each through `NativeViewBuffers`, which crops it to the view's clip.

use std::{
    any::Any,
    cell::RefCell,
    ffi::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use gpui::{
    Bounds, NativeView, NativeViewId, PaintNativeView, ScaledPixels, Scene, Size,
    native_view_placement::{hole_inset, round_outwards, visible},
    point, size,
};
use wayland_client::{
    Proxy, QueueHandle,
    protocol::{wl_buffer, wl_region, wl_subcompositor, wl_subsurface, wl_surface},
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
    // GPUI's crop is in buffer pixels, which only match the surface's at scale 1 and with no
    // transform (`NativeViewBuffers`). Both are pending until the surface's first commit.
    if surface.version() >= 3 {
        surface.set_buffer_scale(1);
    }
    if surface.version() >= 2 {
        surface.set_buffer_transform(wayland_client::protocol::wl_output::Transform::Normal);
    }
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

/// Where a native view's subsurface is, in the window surface's logical pixels, and which part of
/// the app's buffer it shows.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Shown {
    bounds: Bounds<i32>,
    crop: Crop,
}

/// The part of the app's buffer a shown view's subsurface shows (`wp_viewport.set_source`).
#[derive(Copy, Clone, Debug, PartialEq)]
enum Crop {
    /// All of it: the clip leaves the whole view.
    Whole,
    /// The part the clip leaves, as fractions of the view's width and height, so it fits any buffer
    /// size the app commits.
    Part(Bounds<f32>),
}

/// Where a painted view shows, the way macOS, iOS and Windows show it: only the part its clip leaves,
/// with the view at its full size and position. `wl_subsurface.set_position` and
/// `wp_viewport.set_destination` take whole logical pixels, so the subsurface's bounds are rounded
/// outwards from the device pixels GPUI paints in (`round_outwards`; Chromium's
/// `AdjustSubsurfaceBounds` scales the same way). A subsurface can't be clipped, so it covers just the
/// visible part and shows the matching part of the buffer. `None` when the clip leaves none of it.
fn shown(painted: &PaintNativeView, scale: f32) -> Option<Shown> {
    let visible = visible(painted)?;
    let bounds = painted.bounds;
    if visible == bounds {
        return Some(Shown {
            bounds: round_outwards(bounds, scale),
            crop: Crop::Whole,
        });
    }
    let rounded = round_outwards(visible, scale);
    // The rounded part's edges as fractions of the view, from its top left.
    let fraction = |edge: i32, origin: ScaledPixels, length: ScaledPixels| {
        ((edge as f32 * scale - origin.0) / length.0).clamp(0., 1.)
    };
    let (x, y) = (bounds.origin.x, bounds.origin.y);
    let (width, height) = (bounds.size.width, bounds.size.height);
    let left = fraction(rounded.origin.x, x, width);
    let top = fraction(rounded.origin.y, y, height);
    let right = fraction(rounded.origin.x + rounded.size.width, x, width);
    let bottom = fraction(rounded.origin.y + rounded.size.height, y, height);
    Some(Shown {
        bounds: rounded,
        crop: Crop::Part(Bounds {
            origin: point(left, top),
            size: size(right - left, bottom - top),
        }),
    })
}

/// The `wp_viewport` source for `part` of a `buffer`-sized buffer: whole buffer pixels, rounded
/// outwards, at least one pixel and never past the buffer's edge, which would be a protocol error.
fn source(part: Bounds<f32>, buffer: Size<i32>) -> Bounds<i32> {
    let span = |start: f32, length: f32, total: i32| {
        let total = total.max(1);
        let first = ((start * total as f32).floor() as i32).clamp(0, total - 1);
        let end = (((start + length) * total as f32).ceil() as i32).clamp(first + 1, total);
        (first, end - first)
    };
    let (x, width) = span(part.origin.x, part.size.width, buffer.width);
    let (y, height) = span(part.origin.y, part.size.height, buffer.height);
    Bounds {
        origin: point(x, y),
        size: size(width, height),
    }
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
    buffers: NativeViewBuffers,
    placement: Placement<wl_subsurface::WlSubsurface>,
}

impl WaylandSubsurface {
    fn new(
        id: NativeViewId,
        wayland_surface: wl_surface::WlSurface,
        viewport: wp_viewport::WpViewport,
    ) -> Self {
        let buffers = NativeViewBuffers(Arc::new(Mutex::new(Buffers {
            surface: wayland_surface.clone(),
            viewport,
            crop: None,
            attached: None,
            source: None,
        })));
        Self {
            id,
            wayland_surface,
            buffers,
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
                self.buffers.lock().hide();
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
        let mut buffers = self.buffers.lock();
        if previous.map(|previous| previous.bounds.size) != Some(bounds.size) {
            buffers
                .viewport
                .set_destination(bounds.size.width.max(1), bounds.size.height.max(1));
        }
        // Synchronized, so the commit is cached until the window's.
        buffers.place(shown.crop);
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
        self.buffers.lock().detach();
    }
}

/// How the app commits buffers to a native view on Wayland, from any thread. Every commit goes
/// through it: GPUI crops a clipped view's buffer (`wp_viewport.set_source`), and a crop must fit
/// the buffer of the commit that carries it, or the compositor ends the app's connection. GPUI's
/// own commits of the view (each frame that places it) and the app's take turns under one lock,
/// and each sets the source for the buffer it commits with.
///
/// The surface must keep buffer scale 1 and the normal transform (GPUI sets both when the view is
/// attached): the crop is in buffer pixels.
#[derive(Clone)]
pub struct NativeViewBuffers(Arc<Mutex<Buffers>>);

/// What [`NativeViewBuffers::commit_buffer`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Committed {
    /// Attached and committed; the compositor releases the buffer as usual.
    Attached,
    /// The view isn't showing (not painted yet, hidden, or its last `NativeView` handle dropped):
    /// nothing was attached and the buffer is free again. Commit one when the view shows again
    /// (`NativeView::bounds` is `Some`).
    NotShown,
    /// `surface` is not the view's own surface: nothing was sent.
    OtherSurface,
}

/// The view's crop and buffer, shared by GPUI's main thread and the app's threads.
struct Buffers {
    /// The view's surface, as GPUI holds it.
    surface: wl_surface::WlSurface,
    viewport: wp_viewport::WpViewport,
    /// What GPUI last placed: the part of the buffer to show, or `None` while the view isn't shown.
    crop: Option<Crop>,
    /// The size of the buffer on the surface, `None` while there is none.
    attached: Option<Size<i32>>,
    /// The source last sent, `None` for unset (the whole buffer).
    source: Option<Bounds<i32>>,
}

impl NativeViewBuffers {
    /// The buffers of a native view shown in a Wayland window; `None` for a view of any other
    /// platform.
    pub fn of(view: &NativeView) -> Option<Self> {
        view.platform_attachment()
            .downcast_ref::<Rc<RefCell<WaylandSubsurface>>>()
            .map(|subsurface| subsurface.borrow().buffers.clone())
    }

    /// Attaches `buffer` (`size` pixels) to the view's `surface`, damages it whole and commits it,
    /// with the crop GPUI placed the view with, set for this buffer's size.
    pub fn commit_buffer(
        &self,
        surface: &wl_surface::WlSurface,
        buffer: &wl_buffer::WlBuffer,
        size: Size<i32>,
    ) -> Committed {
        let mut buffers = self.lock();
        if surface.id().as_ptr() != buffers.surface.id().as_ptr() {
            return Committed::OtherSurface;
        }
        if buffers.crop.is_none() {
            return Committed::NotShown;
        }
        buffers.attached = Some(size);
        buffers.update_source();
        surface.attach(Some(buffer), 0, 0);
        surface.damage(0, 0, i32::MAX, i32::MAX);
        surface.commit();
        Committed::Attached
    }

    fn lock(&self) -> MutexGuard<'_, Buffers> {
        // A panic while holding the lock leaves plain values behind; the next commit sets them again.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Buffers {
    /// GPUI places the view: the crop for the attached buffer, and a commit (cached until the
    /// window's, as the subsurface is synchronized).
    fn place(&mut self, crop: Crop) {
        self.crop = Some(crop);
        self.update_source();
        self.surface.commit();
    }

    /// GPUI hides the view: no buffer, so nothing shows, and no source, which only a buffer can fit.
    fn hide(&mut self) {
        self.crop = None;
        self.attached = None;
        self.update_source();
        self.surface.attach(None, 0, 0);
        self.surface.commit();
    }

    /// The last handle is gone: the viewport goes, and the app's commits attach nothing.
    fn detach(&mut self) {
        self.crop = None;
        self.viewport.destroy();
    }

    /// Sends the source the crop and the attached buffer ask for, if it isn't the one sent last.
    fn update_source(&mut self) {
        // (crop, attached buffer) → source. `None` unsets it: the whole buffer.
        let wanted = match (self.crop, self.attached) {
            (Some(Crop::Part(part)), Some(buffer)) => Some(source(part, buffer)),
            (Some(Crop::Whole) | None, _) | (Some(Crop::Part(_)), None) => None,
        };
        if wanted == self.source {
            return;
        }
        match wanted {
            Some(source) => self.viewport.set_source(
                source.origin.x.into(),
                source.origin.y.into(),
                source.size.width.into(),
                source.size.height.into(),
            ),
            // All -1 unsets the source.
            None => self.viewport.set_source(-1., -1., -1., -1.),
        }
        self.source = wanted;
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

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Bounds<i32> {
        Bounds {
            origin: point(x, y),
            size: size(width, height),
        }
    }

    fn whole(bounds: Bounds<i32>) -> Shown {
        Shown {
            bounds,
            crop: Crop::Whole,
        }
    }

    fn fractions(x: f32, y: f32, width: f32, height: f32) -> Bounds<f32> {
        Bounds {
            origin: point(x, y),
            size: size(width, height),
        }
    }

    fn physical(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    fn painted(bounds: Bounds<ScaledPixels>, clip: Bounds<ScaledPixels>) -> PaintNativeView {
        PaintNativeView {
            order: 0,
            id: NativeViewId::for_test(1),
            hitbox_id: gpui::HitboxId::placeholder(),
            bounds,
            content_mask: gpui::ContentMask { bounds: clip },
            background: gpui::Hsla::transparent_black(),
        }
    }

    #[test]
    fn steps() {
        let a = whole(rect(0, 0, 10, 10));
        let b = whole(rect(5, 0, 10, 10));
        let cropped = Shown {
            bounds: rect(0, 5, 10, 5),
            crop: Crop::Part(fractions(0., 0.5, 1., 0.5)),
        };
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
            (
                "a new crop moves",
                shown(a, desync),
                Some(cropped),
                Step::Move {
                    subsurface: (),
                    mode: desync,
                    from: a,
                    to: cropped,
                },
            ),
        ];
        for (name, placement, shown_at, expected) in rows {
            assert_eq!(step(placement, shown_at), expected, "{name}");
        }
    }

    #[test]
    fn clipped_views_show_their_visible_part() {
        // The example's `--clipped` view: 640x400 logical at (100, -120), half scrolled out of its
        // container, whose clip is 640x200 at (100, 80).
        let view = |scale: f32| physical(100. * scale, -120. * scale, 640. * scale, 400. * scale);
        let clip = |scale: f32| physical(100. * scale, 80. * scale, 640. * scale, 200. * scale);
        let lower_half = Shown {
            bounds: rect(100, 80, 640, 200),
            crop: Crop::Part(fractions(0., 0.5, 1., 0.5)),
        };
        // (row name, view bounds, clip, scale, what shows)
        let rows = [
            (
                "unclipped shows whole",
                view(1.),
                physical(0., -200., 1000., 1000.),
                1.0,
                Some(whole(rect(100, -120, 640, 400))),
            ),
            (
                "half scrolled out shows the lower half",
                view(1.),
                clip(1.),
                1.0,
                Some(lower_half),
            ),
            (
                "half scrolled out at 1.25",
                view(1.25),
                clip(1.25),
                1.25,
                Some(lower_half),
            ),
            (
                "a clip partway through a logical pixel rounds outwards",
                view(1.25),
                physical(125., 101., 800., 249.),
                1.25,
                Some(lower_half),
            ),
            (
                "clipped on the left at 2",
                physical(0., 0., 400., 200.),
                physical(100., 0., 400., 200.),
                2.0,
                Some(Shown {
                    bounds: rect(50, 0, 150, 100),
                    crop: Crop::Part(fractions(0.25, 0., 0.75, 1.)),
                }),
            ),
            (
                "scrolled out of the clip shows nothing",
                view(1.),
                physical(100., 400., 640., 200.),
                1.0,
                None,
            ),
        ];
        for (name, bounds, clip, scale, expected) in rows {
            assert_eq!(shown(&painted(bounds, clip), scale), expected, "{name}");
        }
    }

    #[test]
    fn sources_stay_inside_the_buffer() {
        // (row name, part of the view, buffer size, source in buffer pixels)
        let rows = [
            (
                "the lower half",
                fractions(0., 0.5, 1., 0.5),
                size(640, 400),
                rect(0, 200, 640, 200),
            ),
            (
                "the same part of a bigger buffer",
                fractions(0., 0.5, 1., 0.5),
                size(1280, 800),
                rect(0, 400, 1280, 400),
            ),
            (
                "the same part of a smaller buffer, as after a resize",
                fractions(0., 0.5, 1., 0.5),
                size(320, 200),
                rect(0, 100, 320, 100),
            ),
            (
                "partial pixels round outwards",
                fractions(0.125, 0.25, 0.5, 0.5),
                size(20, 10),
                rect(2, 2, 11, 6),
            ),
            (
                "a sliver is one pixel",
                fractions(0.5, 0.999, 0.0001, 0.001),
                size(100, 100),
                rect(50, 99, 1, 1),
            ),
            (
                "an empty buffer is one pixel",
                fractions(0., 0.5, 1., 0.5),
                size(0, 0),
                rect(0, 0, 1, 1),
            ),
        ];
        for (name, part, buffer, expected) in rows {
            assert_eq!(source(part, buffer), expected, "{name}");
        }
    }
}
