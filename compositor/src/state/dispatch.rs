//! Forward Wayland dispatch to Smithay and observe subsurface role lifetimes.

use smithay::desktop::utils::with_surfaces_surface_tree;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::SubsurfaceUserData;
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use super::{ClientState, State};
use std::sync::atomic::Ordering;

impl<I: Resource, U: Dispatch2<I, Self>> Dispatch<I, U> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &I,
        request: I::Request,
        data: &U,
        handle: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        data.request(state, client, resource, request, handle, init);
    }

    fn destroyed(state: &mut Self, client: ClientId, resource: &I, data: &U) {
        // Smithay unlinks the child when the role dies, but its wl_surface
        // and descendants can remain alive. Retire them while still reachable.
        if let Some(role) = resource.data::<SubsurfaceUserData>() {
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
