//! Compositor state and the Wayland protocol handlers.
//!
//! Stage 1 offers `wl_compositor`, `wl_shm`, `xdg_wm_base`, `wl_seat` with a
//! keyboard, `wl_output` and `wp_presentation`, each for every client.

use std::error::Error;
use std::time::Duration;

use skorupa_compositor_core::frame_clock::FramePacer;
use smithay::backend::input::{InputTime, KeyState};
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::backend::renderer::{ImportMemWl, buffer_dimensions};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::input::keyboard::{FilterResult, XkbConfig};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::{self, WlSurface};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{Logical, Rectangle, SERIAL_COUNTER, Serial, Size};
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
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use tracing::warn;

use crate::frame::Timers;
use crate::headless::Headless;
use crate::layout::{FillOutput, Layout, WindowId};

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
    pub(crate) backend: Headless,
    /// Oldest first.
    pub(crate) windows: Vec<Window>,
    next_window_id: u64,
    layout: Box<dyn Layout>,
    /// What the layout last said: shown windows, bottom to top.
    pub(crate) placements: Vec<(WindowId, Rectangle<i32, Logical>)>,
    focus: Option<WindowId>,

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
        let output_global = output.create_global::<Self>(dh);

        let mut backend = Headless::new(&output, mode.size)?;
        let shm = ShmState::new::<Self>(dh, backend.renderer().shm_formats());

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
            layout: Box::new(FillOutput::default()),
            placements: Vec::new(),
            focus: None,
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

    /// Applies what the layout says: sizes and states to the clients, the
    /// keyboard to the focused window.
    fn arrange(&mut self) {
        self.placements = self.layout.placements(self.output_size());
        let focus = self.layout.focus();

        for window in &self.windows {
            let shown = self.placements.iter().find(|(id, _)| *id == window.id);
            window.toplevel.with_pending_state(|state| {
                state.size = Some(shown.map_or(self.output_size(), |(_, rect)| rect.size));
                // Edge to edge: no shadows or rounded corners to draw.
                for edge in [
                    xdg_toplevel::State::TiledLeft,
                    xdg_toplevel::State::TiledRight,
                    xdg_toplevel::State::TiledTop,
                    xdg_toplevel::State::TiledBottom,
                ] {
                    state.states.set(edge);
                }
                if focus == Some(window.id) {
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
        self.queue_redraw();
    }

    /// A key of the seat's keyboard, sent to the focused window. `time` is on
    /// the presentation clock.
    pub(crate) fn key(&mut self, keycode: u32, pressed: bool, time: Duration) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let time = InputTime::from_micros(u64::try_from(time.as_micros()).unwrap_or(u64::MAX));
        keyboard.input::<(), _>(
            self,
            keycode.into(),
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
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
        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.toplevel.wl_surface() == surface)
        {
            let mapped = with_renderer_surface_state(surface, |state| state.buffer().is_some())
                .unwrap_or(false);
            if mapped != window.mapped {
                window.mapped = mapped;
                if mapped {
                    self.layout.window_added(window.id);
                } else {
                    self.layout.window_removed(window.id);
                }
                self.arrange();
                return;
            }
            if !window.toplevel.is_initial_configure_sent() {
                window.toplevel.send_configure();
            }
        }
        self.queue_redraw();
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
            self.layout.window_removed(window.id);
            self.arrange();
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
