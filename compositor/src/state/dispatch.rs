//! Forward Wayland dispatch to Smithay and observe subsurface role lifetimes.

use std::any::Any;
use std::cell::RefCell;

use smithay::desktop::utils::with_surfaces_surface_tree;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_subsurface::{self, WlSubsurface};
use smithay::reexports::wayland_server::protocol::wl_surface::{self, WlSurface};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::{
    Cacheable, SUBSURFACE_ROLE, SubsurfaceUserData, SurfaceAttributes, with_states,
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use super::{ClientState, State};
use std::sync::atomic::Ordering;

impl<I: Resource + 'static, U: Dispatch2<I, Self>> Dispatch<I, U> for State
where
    I::Request: 'static,
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &I,
        request: I::Request,
        data: &U,
        handle: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let moving = matches!(
            (&request as &dyn Any).downcast_ref::<wl_subsurface::Request>(),
            Some(wl_subsurface::Request::SetPosition { .. })
        );
        let staging = (resource as &dyn Any)
            .downcast_ref::<WlSurface>()
            .zip((&request as &dyn Any).downcast_ref::<wl_surface::Request>())
            .and_then(|(surface, request)| {
                begin_request(surface, request, handle).then_some(surface)
            });
        data.request(state, client, resource, request, handle, init);
        if moving && let Some(role) = (resource as &dyn Any).downcast_ref::<WlSubsurface>() {
            super::subsurface_position::requested(role);
        }
        if let Some(surface) = staging {
            swap_pending(surface);
        }
    }

    fn destroyed(state: &mut Self, client: ClientId, resource: &I, data: &U) {
        // Smithay unlinks the child when the role dies, but its wl_surface
        // and descendants can remain alive. Retire them while still reachable.
        if let Some(role) = resource.data::<SubsurfaceUserData>() {
            if let Some(object) = (resource as &dyn Any).downcast_ref::<WlSubsurface>() {
                super::subsurface_position::destroyed(object, role.surface());
            }
            // The child wl_surface may already be gone. Its role still belongs
            // to the connection identified by Wayland for this destructor.
            if let Some(handle) = resource.handle().upgrade()
                && let Ok(owner) = handle.get_client_data(client.clone())
                && let Some(data) = owner.downcast_ref::<ClientState>()
            {
                data.subsurfaces.fetch_sub(1, Ordering::Relaxed);
            }
            with_surfaces_surface_tree(role.surface(), |surface, _| state.output.leave(surface));
            if state.session_visible() {
                state.queue_redraw();
            }
        }
        data.destroyed(state, client, resource);
    }
}

impl<I: Resource, U: GlobalDispatch2<I, Self>> GlobalDispatch<I, U> for State {
    fn bind(
        state: &mut Self,
        handle: &DisplayHandle,
        client: &Client,
        resource: New<I>,
        data: &U,
        init: &mut DataInit<'_, Self>,
    ) {
        data.bind(state, handle, client, resource, init);
    }

    fn can_view(client: Client, data: &U) -> bool {
        data.can_view(&client)
    }
}

/// An implicit Smithay child merge must see only state from explicit child
/// commits. Isolate requests on the child that owns them. Regions stay in
/// Smithay until replaced, so staging never duplicates their rectangle lists.
#[derive(Default)]
struct PendingUpdate {
    attributes: SurfaceAttributes,
    input_region_set: bool,
    opaque_region_set: bool,
}

#[derive(Default)]
struct ChildUpdates(RefCell<Option<PendingUpdate>>);

fn begin_request(
    surface: &WlSurface,
    request: &wl_surface::Request,
    handle: &DisplayHandle,
) -> bool {
    with_states(surface, |states| {
        let previous = states.data_map.get::<ChildUpdates>();
        if matches!(
            request,
            wl_surface::Request::Commit | wl_surface::Request::Destroy
        ) {
            if let Some(mut update) = previous.and_then(|slot| slot.0.take()) {
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                let pending = attributes.pending();
                if !update.input_region_set {
                    update.attributes.input_region = pending.input_region.take();
                }
                if !update.opaque_region_set {
                    update.attributes.opaque_region = pending.opaque_region.take();
                }
                update.attributes.merge_into(pending, handle);
            }
            return false;
        }
        if states.role.is_some_and(|role| role != SUBSURFACE_ROLE)
            && previous.is_none_or(|slot| slot.0.borrow().is_none())
        {
            return false;
        }
        let slot = states.data_map.get_or_insert(ChildUpdates::default);
        let mut stored = slot.0.borrow_mut();
        let mut attributes = states.cached_state.get::<SurfaceAttributes>();
        let pending = attributes.pending();
        let update = stored.get_or_insert_with(|| {
            let mut update = PendingUpdate::default();
            update.attributes.buffer_scale = pending.buffer_scale;
            update.attributes.buffer_transform = pending.buffer_transform;
            update
        });
        update.input_region_set |= matches!(request, wl_surface::Request::SetInputRegion { .. });
        update.opaque_region_set |= matches!(request, wl_surface::Request::SetOpaqueRegion { .. });
        std::mem::swap(&mut update.attributes, pending);
        true
    })
}

fn swap_pending(surface: &WlSurface) {
    with_states(surface, |states| {
        if let Some(slot) = states.data_map.get::<ChildUpdates>()
            && let Some(update) = slot.0.borrow_mut().as_mut()
        {
            std::mem::swap(
                &mut update.attributes,
                states.cached_state.get::<SurfaceAttributes>().pending(),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{connect, exchange};
    use crate::{Compositor, Config};

    #[test]
    fn req_perf_004_a_toplevel_commit_needs_no_subsurface_snapshot() {
        let mut compositor = Compositor::new(Config::default()).unwrap();
        let mut client = connect(&mut compositor);
        let _window = client.create_window();
        exchange(&mut compositor, &mut client);
        let surface = compositor.state.windows[0].toplevel.wl_surface();
        with_states(surface, |states| {
            assert!(states.data_map.get::<ChildUpdates>().is_none());
        });
    }
}
