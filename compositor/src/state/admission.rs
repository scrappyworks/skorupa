//! Bounds the registered toplevel roles before they reach the layout.

use skorupa_compositor_core::layout::{MAX_WINDOWS, WindowId};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::__interfaces::WL_DISPLAY_INTERFACE;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

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
pub(super) fn window_limit_reached(surface: &WlSurface) {
    const DISPLAY_ID: u32 = 1;
    const NO_MEMORY: u32 = 2;
    if let Some(handle) = surface.handle().upgrade()
        && let Ok(client) = handle.get_client(surface.id())
        && let Ok(display) =
            handle.object_for_protocol_id(client, &WL_DISPLAY_INTERFACE, DISPLAY_ID)
    {
        handle.post_error(
            display,
            NO_MEMORY,
            c"no toplevel window slots available".into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_WINDOWS, WindowId, take_window_id};

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
