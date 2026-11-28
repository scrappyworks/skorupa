//! Bounds registered toplevel roles and their pending configure queues.

use std::ffi::CStr;

use skorupa_compositor_core::layout::{MAX_WINDOWS, WindowId};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::__interfaces::WL_DISPLAY_INTERFACE;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData};

/// Per toplevel, enough to absorb a burst of state changes between round trips.
const MAX_PENDING_CONFIGURES: usize = 32;

pub(super) fn send_configure(surface: &ToplevelSurface) {
    send_configure_with_limit(surface, MAX_PENDING_CONFIGURES);
}

fn send_configure_with_limit(surface: &ToplevelSurface, limit: usize) {
    if configure_slot_available(surface, limit) {
        surface.send_configure();
    } else {
        resource_limit_reached(
            surface.wl_surface(),
            c"too many unacknowledged window configures",
        );
    }
}

/// Layout changes share Smithay’s single pending state until an ACK frees a slot.
pub(super) fn send_layout_configure(surface: &ToplevelSurface) {
    if surface.is_initial_configure_sent()
        && configure_slot_available(surface, MAX_PENDING_CONFIGURES)
    {
        surface.send_pending_configure();
    }
}

fn configure_slot_available(surface: &ToplevelSurface, limit: usize) -> bool {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .is_some_and(|data| {
                data.lock()
                    .is_ok_and(|role| role.pending_configures().len() < limit)
            })
    })
}

/// Reserve a fresh id for an xdg role before its first commit.
pub(super) fn take_window_id(next: &mut u64, registered: usize) -> Option<WindowId> {
    if registered >= MAX_WINDOWS {
        return None;
    }
    let id = WindowId(*next);
    *next = next.checked_add(1)?;
    Some(id)
}

/// `wayland-server` handles `wl_display` internally. Report its global `no_memory`
/// error on the display object itself, so the client decodes the right enum.
pub(super) fn resource_limit_reached(surface: &WlSurface, reason: &'static CStr) {
    const DISPLAY_ID: u32 = 1;
    const NO_MEMORY: u32 = 2;
    if let Some(handle) = surface.handle().upgrade()
        && let Ok(client) = handle.get_client(surface.id())
        && let Ok(display) =
            handle.object_for_protocol_id(client, &WL_DISPLAY_INTERFACE, DISPLAY_ID)
    {
        handle.post_error(display, NO_MEMORY, reason.into());
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_WINDOWS, WindowId, take_window_id};

    #[test]
    fn req_proto_004_acknowledging_a_configure_releases_its_queue_slot() {
        use crate::test_support::{connect, exchange};
        use crate::{Compositor, Config};

        let mut compositor = Compositor::new(Config::default()).unwrap();
        let mut client = connect(&mut compositor);
        let _window = client.create_window();
        exchange(&mut compositor, &mut client);
        let toplevel = compositor.state.windows[0].toplevel.clone();

        // The client's ACK for its initial configure makes the single slot available.
        exchange(&mut compositor, &mut client);
        super::send_configure_with_limit(&toplevel, 1);
        compositor.display.flush_clients().unwrap();
        client.read();
        assert_eq!(client.data.configures.len(), 2);

        // The second configure is still outstanding until the next exchange.
        super::send_configure_with_limit(&toplevel, 1);
        compositor.display.flush_clients().unwrap();
        client.read();
        let error = client.protocol_error().unwrap();
        assert_eq!(error.object_interface, "wl_display");
        assert_eq!(
            error.code,
            wayland_client::protocol::wl_display::Error::NoMemory as u32
        );
    }

    #[test]
    fn req_api_012_window_id_admission_checks_the_registered_role_count() {
        let mut next = 40;
        assert_eq!(
            take_window_id(&mut next, MAX_WINDOWS - 1),
            Some(WindowId(40))
        );
        assert_eq!(take_window_id(&mut next, MAX_WINDOWS), None);
        assert_eq!(
            take_window_id(&mut next, MAX_WINDOWS - 1),
            Some(WindowId(41))
        );
    }

    #[test]
    fn req_api_012_exhausted_window_ids_are_never_reused() {
        let mut next = u64::MAX - 1;
        assert_eq!(take_window_id(&mut next, 0), Some(WindowId(u64::MAX - 1)));
        assert_eq!(take_window_id(&mut next, 0), None);
        assert_eq!(next, u64::MAX);
    }
}
