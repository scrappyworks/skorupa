//! Compositor state and the Wayland protocol handlers.
//!
//! Stage 1 offers `wl_compositor`, `wl_shm`, `xdg_wm_base`, `wl_seat` with a
//! keyboard, `wl_output` and `wp_presentation`, each for every client.

use std::error::Error;
use std::time::Duration;

use skorupa_compositor_core::frame_clock::FramePacer;
use skorupa_compositor_core::layout::{Effect, Event, MAX_APP_ID_LEN};
use smithay::backend::input::{InputTime, KeyState};
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::backend::renderer::{ImportMemWl, buffer_dimensions};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::input::keyboard::{FilterResult, KeyboardSource, XkbConfig, keysyms};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::{self, WlSurface};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{Logical, SERIAL_COUNTER, Serial, Size};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    add_pre_commit_hook, with_states,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::PointerConstraintsHandler;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use tracing::warn;

use crate::drm::Drm;
use crate::frame::{Placement, Timers};
use crate::headless::Headless;
use crate::layout::{Layout, WindowId, WorkspaceLayout};

/// `CLOCK_MONOTONIC`: the clock that `now` and all presentation times are on.
const PRESENTATION_CLOCK: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Output size in pixels; the scale is 1.
    pub output_size: (i32, i32),
    /// Refresh rate in mHz.
    pub refresh_mhz: i32,
    /// Largest client buffer that is accepted, in pixels per side. Enough
    /// for an 8K video frame; anything larger is refused before it is read.
    pub max_buffer_size: (i32, i32),
    /// How long a submitted frame may wait for its flip.
    pub flip_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_size: (1280, 720),
            refresh_mhz: 60_000,
            max_buffer_size: (8192, 8192),
            flip_timeout: Duration::from_secs(1),
        }
    }
}

impl Config {
    /// `None` for a rate that is not positive.
    #[must_use]
    pub fn refresh_interval(&self) -> Option<Duration> {
        let mhz = u64::try_from(self.refresh_mhz).ok()?;
        // Rounded to the nearest nanosecond, like the interval of a KMS mode.
        let nanos = (1_000_000_000_000 + mhz / 2).checked_div(mhz)?;
        Some(Duration::from_nanos(nanos))
    }
}

pub(crate) struct Window {
    pub(crate) id: WindowId,
    pub(crate) toplevel: ToplevelSurface,
    mapped: bool,
}

/// Per-client data that Smithay asks for.
#[derive(Default)]
pub(crate) struct ClientState {
    compositor: CompositorClientState,
}

impl ClientData for ClientState {}

pub(crate) enum Backend {
    Headless(Box<Headless>),
    Drm(Box<Drm>),
}

pub struct State {
    pub(crate) config: Config,
    compositor: CompositorState,
    xdg_shell: XdgShellState,
    shm: ShmState,
    seats: SeatState<Self>,
    seat: Seat<Self>,
    // Globals that live as long as the compositor.
    _presentation: PresentationState,
    _output_global: GlobalId,

    pub(crate) output: Output,
    pub(crate) backend: Backend,
    /// Oldest first.
    pub(crate) windows: Vec<Window>,
    next_window_id: u64,
    layout: Box<dyn Layout>,
    /// What the layout last said: shown windows, bottom to top.
    pub(crate) placements: Vec<Placement>,
    focus: Option<WindowId>,
    workspace_keys: [Option<u32>; 9],
    session_visible: bool,
    pub(crate) session_lock: Option<crate::session_lock::SessionLock>,

    pub(crate) pacer: FramePacer,
    pub(crate) timers: Timers,
    /// Feedback of the frame that was submitted and whose flip is awaited.
    pub(crate) pending_feedback: Option<OutputPresentationFeedback>,
    /// Flips completed so far; headless has no hardware counter.
    pub(crate) flips: u32,
}

impl State {
    pub(crate) fn new(dh: &DisplayHandle, config: Config) -> Result<Self, Box<dyn Error>> {
        let mode = Mode {
            size: config.output_size.into(),
            refresh: config.refresh_mhz,
        };
        let output = Output::new(
            "HEADLESS-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Skorupa".into(),
                model: "headless".into(),
                serial_number: "1".into(),
            },
        );
        output.set_preferred(mode);
        output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
        let backend = Headless::new(&output, mode.size)?;
        Self::with_backend(dh, config, output, Backend::Headless(Box::new(backend)))
    }

    pub(crate) fn with_backend(
        dh: &DisplayHandle,
        config: Config,
        output: Output,
        mut backend: Backend,
    ) -> Result<Self, Box<dyn Error>> {
        let formats: Vec<_> = match &mut backend {
            Backend::Headless(backend) => backend.renderer().shm_formats().collect(),
            Backend::Drm(backend) => backend.shm_formats()?,
        };
        let output_global = output.create_global::<Self>(dh);
        let shm = ShmState::new::<Self>(dh, formats);

        let mut seats = SeatState::new();
        let mut seat = seats.new_wl_seat(dh, "seat0");
        // Without a keymap (no XKB data on this system) clients still get a
        // seat, only without a keyboard.
        if let Err(err) = seat.add_keyboard(XkbConfig::default(), 200, 25) {
            warn!(%err, "no keyboard: the default keymap did not compile");
        }

        let mut state = Self {
            config,
            compositor: CompositorState::new::<Self>(dh),
            xdg_shell: XdgShellState::new::<Self>(dh),
            shm,
            seats,
            seat,
            _presentation: PresentationState::new::<Self>(dh, PRESENTATION_CLOCK),
            _output_global: output_global,
            output,
            backend,
            windows: Vec::new(),
            next_window_id: 0,
            layout: Box::new(WorkspaceLayout::new(
                config.output_size.0,
                config.output_size.1,
            )),
            placements: Vec::new(),
            focus: None,
            workspace_keys: [None; 9],
            session_visible: true,
            session_lock: None,
            pacer: FramePacer::new(config.refresh_interval(), config.flip_timeout),
            timers: Timers::default(),
            pending_feedback: None,
            flips: 0,
        };
        // The first frame shows the background before any client commits.
        state.queue_redraw();
        Ok(state)
    }

    pub(crate) fn new_client_state() -> ClientState {
        ClientState::default()
    }

    fn output_size(&self) -> Size<i32, Logical> {
        self.config.output_size.into()
    }

    pub(crate) fn session_visible(&self) -> bool {
        self.session_visible
    }

    /// REQ-LOCK-003, REQ-LOCK-019: fence client focus and frame delivery before
    /// processing the next input event or rendering the protected cover.
    pub(crate) fn set_session_visible(&mut self, visible: bool) {
        if self.session_visible == visible {
            return;
        }
        self.session_visible = visible;
        self.arrange();
        if !visible {
            self.fence_frame_feedback();
            // The app already got wl_keyboard.leave. Clear held keys before
            // any client regains focus.
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.release_source(self, KeyboardSource::MAIN);
            }
            self.workspace_keys = [None; 9];
        }
        self.queue_redraw();
    }

    /// Applies what the layout says: sizes and states to the clients, the
    /// keyboard to the focused window.
    fn arrange(&mut self) {
        self.placements = self.layout.placements();
        let focus = if self.session_visible {
            self.layout.focus()
        } else {
            None
        };

        for window in &self.windows {
            let configured = self.layout.configure(window.id);
            let initial_size = window
                .toplevel
                .parent()
                .is_none()
                .then(|| self.output_size());
            window.toplevel.with_pending_state(|state| {
                state.size = configured.map_or_else(
                    || initial_size,
                    |configured| configured.size.map(|size| (size.w, size.h).into()),
                );
                if configured.is_some_and(|configured| configured.fullscreen) {
                    state.states.set(xdg_toplevel::State::Fullscreen);
                } else {
                    state.states.unset(xdg_toplevel::State::Fullscreen);
                }
                // The tiling states describe independent windows.
                for edge in [
                    xdg_toplevel::State::TiledLeft,
                    xdg_toplevel::State::TiledRight,
                    xdg_toplevel::State::TiledTop,
                    xdg_toplevel::State::TiledBottom,
                ] {
                    if state.size.is_some() {
                        state.states.set(edge);
                    } else {
                        state.states.unset(edge);
                    }
                }
                if self.session_visible && configured.is_some_and(|configured| configured.activated)
                {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
            });
            // The first configure answers the client's first commit, not this.
            if window.toplevel.is_initial_configure_sent() {
                window.toplevel.send_pending_configure();
            }
        }

        if focus != self.focus {
            self.focus = focus;
            let surface = self
                .windows
                .iter()
                .find(|window| Some(window.id) == focus)
                .map(|window| window.toplevel.wl_surface().clone());
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
            }
        }
        if self.session_visible {
            self.queue_redraw();
        }
    }

    /// A key of the seat's keyboard, sent to the focused window. `time` is on
    /// the presentation clock.
    pub(crate) fn key(&mut self, keycode: u32, pressed: bool, time: Duration) {
        if !self.session_visible {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let time = InputTime::from_micros(u64::try_from(time.as_micros()).unwrap_or(u64::MAX));
        let selected = keyboard.input(
            self,
            keycode.into(),
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |state, modifiers, symbols| {
                if let Some(held) = state
                    .workspace_keys
                    .iter_mut()
                    .find(|held| **held == Some(keycode))
                {
                    if !pressed {
                        *held = None;
                    }
                    return FilterResult::Intercept(None);
                }

                let symbol = symbols.modified_sym().raw();
                if pressed
                    && modifiers.logo
                    && !modifiers.ctrl
                    && !modifiers.alt
                    && !modifiers.shift
                    && (keysyms::KEY_1..=keysyms::KEY_9).contains(&symbol)
                {
                    let index = (symbol - keysyms::KEY_1) as usize;
                    state.workspace_keys[index] = Some(keycode);
                    FilterResult::Intercept(Some(index))
                } else {
                    FilterResult::Forward
                }
            },
        );
        if let Some(Some(index)) = selected {
            self.layout_event(Event::WorkspaceIndex(index));
        }
    }

    fn layout_event(&mut self, event: Event) {
        for effect in self.layout.handle(event) {
            if let Effect::Close(id) = effect
                && let Some(window) = self.windows.iter().find(|window| window.id == id)
            {
                window.toplevel.send_close();
            }
        }
        self.arrange();
    }

    /// Refuses a pending buffer above the configured size before anything
    /// reads it: the buffer is dropped from the commit and the client gets a
    /// protocol error, which disconnects it.
    fn refuse_oversized_buffer(&self, surface: &WlSurface) {
        let (max_w, max_h) = self.config.max_buffer_size;
        let oversized = with_states(surface, |states| {
            let mut attributes = states.cached_state.get::<SurfaceAttributes>();
            let pending = attributes.pending();
            let Some(BufferAssignment::NewBuffer(buffer)) = &pending.buffer else {
                return None;
            };
            let size = buffer_dimensions(buffer)?;
            if size.w <= max_w && size.h <= max_h {
                return None;
            }
            pending.buffer = None;
            Some(size)
        });
        if let Some(size) = oversized {
            surface.post_error(
                wl_surface::Error::InvalidSize,
                format!(
                    "buffer of {}x{} is larger than the limit of {max_w}x{max_h}",
                    size.w, size.h
                ),
            );
        }
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client is inserted with a `ClientState`, so the fallback is
        // never taken; it only spares a panic path.
        static DETACHED: std::sync::OnceLock<CompositorClientState> = std::sync::OnceLock::new();
        match client.get_data::<ClientState>() {
            Some(state) => &state.compositor,
            None => DETACHED.get_or_init(CompositorClientState::default),
        }
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, |state, _, surface| {
            state.refuse_oversized_buffer(surface);
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.toplevel.wl_surface() == surface)
        {
            let window = &mut self.windows[index];
            let mapped = with_renderer_surface_state(surface, |state| state.buffer().is_some())
                .unwrap_or(false);
            if mapped != window.mapped {
                window.mapped = mapped;
                let id = window.id;
                let event = if mapped {
                    let parent = window.toplevel.parent().and_then(|parent| {
                        self.windows
                            .iter()
                            .find(|window| window.mapped && *window.toplevel.wl_surface() == parent)
                            .map(|window| window.id)
                    });
                    let app_id = with_states(surface, |states| {
                        let role = states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()?
                            .lock()
                            .ok()?;
                        role.app_id
                            .as_ref()
                            .filter(|id| id.len() <= MAX_APP_ID_LEN)
                            .cloned()
                    });
                    // xdg_toplevel.set_parent can only name a toplevel on its own connection.
                    Event::Map {
                        window: id,
                        app_id,
                        parent,
                        parent_same_client: parent.is_some(),
                        valid_token: false,
                    }
                } else {
                    Event::Unmap(id)
                };
                self.layout_event(event);
                return;
            }
            if !window.toplevel.is_initial_configure_sent() {
                window.toplevel.send_configure();
            }
        }
        if self.session_visible {
            self.queue_redraw();
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.windows.push(Window {
            id,
            toplevel: surface,
            mapped: false,
        });
        self.arrange();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.toplevel == surface)
        else {
            return;
        };
        let window = self.windows.remove(index);
        if window.mapped {
            self.layout_event(Event::Unmap(window.id));
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // Popups are not placed or drawn yet. Dismissing one at once is the
        // protocol's way to say so, and the client does not wait for a
        // configure that never comes.
        surface.send_popup_done();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seats
    }
}

impl OutputHandler for State {}

// Smithay wants this of every state whose pointer focus is a `wl_surface`;
// there is no pointer yet.
impl PointerConstraintsHandler for State {}

smithay::delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use smithay::reexports::wayland_server::Display;

    use super::*;

    #[test]
    fn req_lock_003_clears_held_keys_and_skips_keys_while_fenced() -> Result<(), Box<dyn Error>> {
        let display = Display::<State>::new()?;
        let mut state = State::new(&display.handle(), Config::default())?;
        let keyboard = state.seat.get_keyboard().ok_or("missing keyboard")?;
        let now = Duration::from_secs(1);
        // XKB's keycode for left shift.
        state.key(50, true, now);
        assert!(keyboard.modifier_state().shift);

        state.set_session_visible(false);
        assert!(keyboard.pressed_keys().is_empty());
        state.key(50, false, now);
        state.key(64, true, now);

        state.set_session_visible(true);
        assert!(keyboard.pressed_keys().is_empty());
        assert!(!keyboard.modifier_state().shift);
        assert!(!keyboard.modifier_state().alt);
        Ok(())
    }
}
