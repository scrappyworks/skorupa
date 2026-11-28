//! The effective xdg window geometry is fixed by a particular surface commit.

use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_surface;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::compositor::{Cacheable, with_states};
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelSurface, XDG_TOPLEVEL_ROLE, XdgShellSurfaceUserData,
};

#[derive(Default)]
struct WindowGeometry {
    requested: Option<Rectangle<i32, Logical>>,
    effective: Option<Rectangle<i32, Logical>>,
}

impl Cacheable for WindowGeometry {
    fn commit(&mut self, _: &DisplayHandle) -> Self {
        std::mem::take(self)
    }

    fn merge_into(self, current: &mut Self, _: &DisplayHandle) {
        if self.requested.is_some() {
            *current = self;
        }
    }
}

/// Smithay normally copies the pending rectangle into every later commit. Take
/// each new request into the same transaction so repeating a rectangle remains
/// distinguishable from committing unrelated surface state.
pub(super) fn capture(surface: &WlSurface) {
    with_states(surface, |states| {
        if states.role == Some(XDG_TOPLEVEL_ROLE) {
            let requested = states
                .cached_state
                .get::<SurfaceCachedState>()
                .pending()
                .geometry
                .take();
            states
                .cached_state
                .get::<WindowGeometry>()
                .pending()
                .requested = requested;
        }
    });
}

/// Called after the renderer has applied this commit's buffer and subsurfaces.
pub(super) fn apply(toplevel: &ToplevelSurface, was_mapped: bool, mapped: bool) {
    let surface = toplevel.wl_surface();
    let requested = with_states(surface, |states| {
        let mut cached = states.cached_state.get::<WindowGeometry>();
        let geometry = cached.current();
        if was_mapped && !mapped {
            *geometry = WindowGeometry::default();
        }
        // Keep the raw committed rectangle available to other xdg consumers.
        states
            .cached_state
            .get::<SurfaceCachedState>()
            .current()
            .geometry = geometry.requested;
        geometry
            .requested
            .filter(|_| mapped && geometry.effective.is_none())
    });
    if let Some(requested) = requested {
        let bounds = bbox_from_surface_tree(surface, (0, 0));
        let Some(effective) = requested.intersection(bounds) else {
            if let Some(role) = toplevel.xdg_toplevel().data::<XdgShellSurfaceUserData>() {
                role.xdg_surface().post_error(
                    xdg_surface::Error::InvalidSize,
                    "window geometry has no area inside the surface tree",
                );
            }
            return;
        };
        with_states(surface, |states| {
            states
                .cached_state
                .get::<WindowGeometry>()
                .current()
                .effective = Some(effective);
        });
    }
}

pub(super) fn reset(surface: &WlSurface) {
    with_states(surface, |states| {
        let mut geometry = states.cached_state.get::<WindowGeometry>();
        *geometry.pending() = WindowGeometry::default();
        *geometry.current() = WindowGeometry::default();
    });
}

pub(crate) fn effective(surface: &WlSurface) -> Rectangle<i32, Logical> {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<WindowGeometry>()
            .current()
            .effective
    })
    .unwrap_or_else(|| bbox_from_surface_tree(surface, (0, 0)))
}
