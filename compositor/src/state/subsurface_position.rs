//! Attach `wl_subsurface.set_position` to the parent's surface transaction.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use smithay::reexports::wayland_server::protocol::{
    wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::{DisplayHandle, Resource, Weak};
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::{
    Cacheable, SubsurfaceCachedState, SubsurfaceUserData, get_parent, with_states,
};

type Location = Point<i32, Logical>;

#[derive(Default)]
struct Applied(Cell<Location>);

type Locations = HashMap<Weak<WlSubsurface>, Location>;

#[derive(Default)]
struct Moves(Locations);

impl Cacheable for Moves {
    fn commit(&mut self, _: &DisplayHandle) -> Self {
        std::mem::take(self)
    }

    fn merge_into(self, into: &mut Self, _: &DisplayHandle) {
        if !self.0.is_empty() {
            into.0.retain(|role, _| role.is_alive());
            into.0.extend(self.0);
        }
    }
}

#[derive(Default)]
struct Requested(RefCell<Locations>);

/// Smithay converted the coordinates for this client, and placed the result
/// in the child's pending cache. Only a real parent commit may latch it.
pub(super) fn requested(role: &WlSubsurface) {
    let Some(data) = role.data::<SubsurfaceUserData>() else {
        return;
    };
    let child = data.surface();
    let Some(parent) = get_parent(child) else {
        return;
    };
    let location = with_states(child, |states| {
        states.data_map.get_or_insert(Applied::default);
        states
            .cached_state
            .get::<SubsurfaceCachedState>()
            .pending()
            .location
    });
    with_states(&parent, |states| {
        states
            .data_map
            .get_or_insert(Requested::default)
            .0
            .borrow_mut()
            .insert(role.downgrade(), location);
    });
}

pub(super) fn destroyed(role: &WlSubsurface, child: &WlSurface) {
    with_states(child, |states| {
        if let Some(applied) = states.data_map.get::<Applied>() {
            applied.0.set(Location::default());
        }
    });
    if let Some(parent) = get_parent(child) {
        with_states(&parent, |states| {
            if let Some(requested) = states.data_map.get::<Requested>() {
                requested.0.borrow_mut().remove(&role.downgrade());
            }
        });
    }
}

pub(super) fn capture(parent: &WlSurface) {
    with_states(parent, |states| {
        if let Some(moves) = states.data_map.get::<Requested>()
            && !moves.0.borrow().is_empty()
        {
            *states.cached_state.get::<Moves>().pending() = Moves(moves.0.take());
        }
    });
}

pub(super) fn apply(surface: &WlSurface) {
    let moves = with_states(surface, |states| {
        // A child transaction can also apply Smithay's copy of the position.
        // Overwrite it with the parent's last applied value before rendering.
        if let Some(applied) = states.data_map.get::<Applied>() {
            states
                .cached_state
                .get::<SubsurfaceCachedState>()
                .current()
                .location = applied.0.get();
        }
        states
            .cached_state
            .has::<Moves>()
            .then(|| std::mem::take(states.cached_state.get::<Moves>().current()))
    });
    if let Some(moves) = moves {
        for (weak_role, location) in moves.0 {
            if let Ok(role) = weak_role.upgrade()
                && let Some(child) = role.data::<SubsurfaceUserData>()
            {
                with_states(child.surface(), |states| {
                    states
                        .data_map
                        .get_or_insert(Applied::default)
                        .0
                        .set(location);
                    states
                        .cached_state
                        .get::<SubsurfaceCachedState>()
                        .current()
                        .location = location;
                });
            }
        }
    }
}
