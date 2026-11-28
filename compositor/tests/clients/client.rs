//! A Wayland client for the tests, built on wayland-client. It never blocks:
//! the fixture moves bytes by calling `flush` and `read` in turn with the
//! compositor.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::backend::protocol::ProtocolError;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configure {
    pub size: (i32, i32),
    pub states: Vec<xdg_toplevel::State>,
}

impl Configure {
    pub fn activated(&self) -> bool {
        self.states.contains(&xdg_toplevel::State::Activated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presented {
    pub time: Duration,
    pub refresh: Duration,
    pub sequence: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Keymap,
    Enter,
    Leave,
    Press { key: u32, pressed: bool, time: u32 },
}

pub enum CallbackKind {
    Sync,
    Frame,
}

/// Everything the compositor told this client.
#[derive(Default)]
pub struct Data {
    pub globals: Vec<String>,
    compositor: Option<WlCompositor>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    presentation: Option<WpPresentation>,
    /// Current mode of the output: width, height, refresh in mHz.
    pub mode: Option<(i32, i32, i32)>,
    pub clock_id: Option<u32>,
    pending_configure: Option<Configure>,
    pub configures: Vec<Configure>,
    /// Times of the frame callbacks, in milliseconds.
    pub frames: Vec<u32>,
    pub presented: Vec<Presented>,
    pub discarded: u32,
    pub keys: Vec<Key>,
    /// Surfaces that received an enter without a subsequent leave.
    surfaces_on_output: HashSet<WlSurface>,
    syncs: u32,
}

impl Data {
    #[allow(dead_code)] // Read by the headless integration tests.
    pub fn on_output(&self, surface: &WlSurface) -> bool {
        self.surfaces_on_output.contains(surface)
    }
}

pub struct Window {
    pub surface: WlSurface,
    #[allow(dead_code)] // Read by the headless geometry test.
    pub xdg_surface: XdgSurface,
    toplevel: XdgToplevel,
}

pub struct Client {
    connection: Connection,
    queue: EventQueue<Data>,
    qh: QueueHandle<Data>,
    pub data: Data,
}

impl Client {
    pub fn new(stream: UnixStream) -> Self {
        Self::from_connection(Connection::from_socket(stream).unwrap())
    }

    pub fn from_connection(connection: Connection) -> Self {
        let queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        Self {
            connection,
            queue,
            qh,
            data: Data::default(),
        }
    }

    #[allow(dead_code)] // The VM client talks to a separate process.
    pub fn roundtrip(&mut self) -> Result<usize, wayland_client::DispatchError> {
        self.queue.roundtrip(&mut self.data)
    }

    #[allow(dead_code)] // The VM client talks to a separate process.
    pub fn wait(&mut self) -> Result<usize, wayland_client::DispatchError> {
        self.queue.blocking_dispatch(&mut self.data)
    }

    pub fn flush(&self) {
        // A dead connection shows up in `protocol_error`.
        let _ = self.connection.flush();
    }

    /// Takes everything the compositor has sent so far.
    pub fn read(&mut self) {
        // Bounded, although the socket runs dry long before: nobody writes
        // to it while this runs.
        for _ in 0..1024 {
            let _ = self.queue.dispatch_pending(&mut self.data);
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            // An empty socket and a dead connection both end the reading; a
            // dead one shows up in `protocol_error`.
            if guard.read().is_err() {
                break;
            }
        }
        let _ = self.queue.dispatch_pending(&mut self.data);
    }

    /// Asks for a `wl_display.sync`; returns the number of syncs that will
    /// have completed when its answer has arrived.
    pub fn sync(&mut self) -> u32 {
        self.connection.display().sync(&self.qh, CallbackKind::Sync);
        self.data.syncs + 1
    }

    pub fn synced(&self, target: u32) -> bool {
        self.data.syncs >= target
    }

    pub fn protocol_error(&self) -> Option<ProtocolError> {
        self.connection.protocol_error()
    }

    /// A toplevel after its first, bufferless commit.
    pub fn create_window(&mut self) -> Window {
        self.create_toplevel(None)
    }

    pub fn create_toplevel(&mut self, parent: Option<&Window>) -> Window {
        let surface = self
            .data
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.qh, ());
        let xdg_surface =
            self.data
                .wm_base
                .as_ref()
                .unwrap()
                .get_xdg_surface(&surface, &self.qh, ());
        let toplevel = xdg_surface.get_toplevel(&self.qh, ());
        if let Some(parent) = parent {
            toplevel.set_parent(Some(&parent.toplevel));
        }
        surface.commit();
        Window {
            surface,
            xdg_surface,
            toplevel,
        }
    }

    #[allow(dead_code)] // Used by headless subsurface tests.
    pub fn create_subsurface(&self, parent: &WlSurface) -> (WlSurface, WlSubsurface) {
        let child = self
            .data
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.qh, ());
        let role =
            self.data
                .subcompositor
                .as_ref()
                .unwrap()
                .get_subsurface(&child, parent, &self.qh, ());
        (child, role)
    }

    /// A shared-memory buffer filled with one color. `format` says whether
    /// the top byte of `pixel` is alpha or padding.
    pub fn buffer(&mut self, width: i32, height: i32, format: Format, pixel: u32) -> WlBuffer {
        self.buffer_with(width, height, format, |_, _| pixel)
    }

    pub fn buffer_with(
        &mut self,
        width: i32,
        height: i32,
        format: Format,
        pixel: impl Fn(i32, i32) -> u32,
    ) -> WlBuffer {
        let count = usize::try_from(width * height).unwrap();
        let mut pixels = Vec::with_capacity(count * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend(pixel(x, y).to_ne_bytes());
            }
        }
        let mut file = File::from(memfd_create("skorupa-test", MemfdFlags::CLOEXEC).unwrap());
        file.write_all(&pixels).unwrap();
        let shm = self.data.shm.as_ref().unwrap();
        let pool = shm.create_pool(file.as_fd(), width * height * 4, &self.qh, ());
        let buffer = pool.create_buffer(0, width, height, width * 4, format, &self.qh, ());
        pool.destroy();
        buffer
    }

    /// Commits the buffer as fully damaged, asking for a frame callback and
    /// for presentation feedback.
    pub fn commit(&mut self, window: &Window, buffer: Option<&WlBuffer>) {
        if let Some(buffer) = buffer {
            window.surface.attach(Some(buffer), 0, 0);
            window.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        }
        window.surface.frame(&self.qh, CallbackKind::Frame);
        let presentation = self.data.presentation.as_ref().unwrap();
        presentation.feedback(&window.surface, &self.qh, ());
        window.surface.commit();
    }
}

impl Dispatch<WlRegistry, ()> for Data {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, version, qh, ())),
            "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, version, qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, version, qh, ())),
            "xdg_wm_base" => state.wm_base = Some(registry.bind(name, version, qh, ())),
            "wp_presentation" => state.presentation = Some(registry.bind(name, version, qh, ())),
            "wl_seat" => {
                registry.bind::<WlSeat, _, _>(name, version, qh, ());
            }
            "wl_output" => {
                registry.bind::<WlOutput, _, _>(name, version, qh, ());
            }
            _ => {}
        }
        state.globals.push(interface);
    }
}

impl Dispatch<WlCallback, CallbackKind> for Data {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        kind: &CallbackKind,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_callback::Event::Done { callback_data } = event else {
            return;
        };
        match kind {
            CallbackKind::Sync => state.syncs += 1,
            CallbackKind::Frame => state.frames.push(callback_data),
        }
    }
}

impl Dispatch<WlOutput, ()> for Data {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode {
            width,
            height,
            refresh,
            ..
        } = event
        {
            state.mode = Some((width, height, refresh));
        }
    }
}

impl Dispatch<WlSurface, ()> for Data {
    fn event(
        state: &mut Self,
        surface: &WlSurface,
        event: wayland_client::protocol::wl_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_surface::Event;
        match event {
            Event::Enter { .. } => {
                state.surfaces_on_output.insert(surface.clone());
            }
            Event::Leave { .. } => {
                state.surfaces_on_output.remove(surface);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Data {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Data {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = event
        {
            let states = states
                .as_chunks::<4>()
                .0
                .iter()
                .filter_map(|raw| xdg_toplevel::State::try_from(u32::from_ne_bytes(*raw)).ok())
                .collect();
            state.pending_configure = Some(Configure {
                size: (width, height),
                states,
            });
        }
    }
}

impl Dispatch<XdgSurface, ()> for Data {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configures.extend(state.pending_configure.take());
        }
    }
}

impl Dispatch<WpPresentation, ()> for Data {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock_id = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for Data {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => state.presented.push(Presented {
                time: Duration::new(u64::from(tv_sec_hi) << 32 | u64::from(tv_sec_lo), tv_nsec),
                refresh: Duration::from_nanos(refresh.into()),
                sequence: u64::from(seq_hi) << 32 | u64::from(seq_lo),
                flags: match flags {
                    WEnum::Value(kind) => kind.bits(),
                    WEnum::Unknown(bits) => bits,
                },
            }),
            wp_presentation_feedback::Event::Discarded => state.discarded += 1,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for Data {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
        {
            seat.get_keyboard(qh, ());
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Data {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { .. } => state.keys.push(Key::Keymap),
            wl_keyboard::Event::Enter { .. } => state.keys.push(Key::Enter),
            wl_keyboard::Event::Leave { .. } => state.keys.push(Key::Leave),
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                time,
                ..
            } => state.keys.push(Key::Press {
                key,
                pressed: key_state == WEnum::Value(wl_keyboard::KeyState::Pressed),
                time,
            }),
            _ => {}
        }
    }
}

delegate_noop!(Data: WlCompositor);
delegate_noop!(Data: WlSubcompositor);
delegate_noop!(Data: WlSubsurface);
delegate_noop!(Data: WlShmPool);
delegate_noop!(Data: ignore WlShm);
delegate_noop!(Data: ignore WlBuffer);
