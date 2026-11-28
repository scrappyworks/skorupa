//! Wayland compositor for one screen, backed by DRM/KMS or an in-memory Pixman output.

mod drm;
mod frame;
pub mod headless;
pub mod layout;
mod session_lock;
mod state;

use std::error::Error;
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

pub use skorupa_compositor_core as core;
pub use skorupa_protocol as protocol;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::Display;

use crate::session_lock::SessionLock;
pub use crate::session_lock::read_lock_clocks;
pub use crate::state::Config;
use crate::state::{Backend, State};
use skorupa_compositor_core::lock::{self, Clocks};

pub struct Compositor {
    display: Display<State>,
    state: State,
}

impl Compositor {
    /// # Errors
    ///
    /// When the Wayland display or the renderer cannot be created.
    pub fn new(config: Config) -> Result<Self, Box<dyn Error>> {
        let display = Display::new()?;
        let state = State::new(&display.handle(), config)?;
        Ok(Self { display, state })
    }

    /// Opens one atomic KMS output and registers the logind seat devices.
    ///
    /// # Errors
    /// When the seat, output, renderer or device events cannot be set up.
    pub fn new_drm(handle: &LoopHandle<'_, Self>) -> Result<Self, Box<dyn Error>> {
        let (backend, devices, output, config) = drm::Drm::open()?;
        let display = Display::new()?;
        let backend = Backend::Drm(Box::new(backend));
        let state = State::with_backend(&display.handle(), config, output, backend)?;
        devices.register(handle)?;
        Ok(Self { display, state })
    }

    /// Fences ordinary input, schedules the cover, and attaches the authd endpoint.
    ///
    /// The launcher must give the peer exclusively to authd. The lock reaches
    /// [`lock::Phase::Locked`] after the backend presents the cover.
    ///
    /// # Errors
    /// Returns `AlreadyExists` for a second installation. An invalid session
    /// identifier or socket setup error leaves input fenced and the cover queued.
    pub fn enable_session_lock(
        &mut self,
        config: lock::Config,
        now: Clocks,
        authd: UnixStream,
    ) -> io::Result<()> {
        if self.state.session_lock.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "session lock already enabled",
            ));
        }
        self.state.set_session_visible(false);
        let (session, effects) = SessionLock::start(config, now, authd)?;
        self.state.session_lock = Some(session);
        self.state.apply_lock_effects(effects);
        Ok(())
    }

    /// The phase of the session lock, when enabled.
    #[must_use]
    pub fn session_phase(&self) -> Option<lock::Phase> {
        self.state.session_lock.as_ref().map(SessionLock::phase)
    }

    /// Requests a lock from a trusted input or service adapter.
    pub fn request_lock(&mut self, now: Clocks) {
        self.state.lock_clock(now);
        self.state.lock_event(lock::Event::LockRequested);
    }

    /// The token a trusted bridge must echo to end the current sleep hold.
    #[must_use]
    pub fn sleep_hold_token(&self) -> Option<lock::HoldToken> {
        self.state
            .session_lock
            .as_ref()
            .and_then(SessionLock::hold_token)
    }

    /// Relays a trusted bridge's report that it has rearmed its inhibitor.
    pub fn end_sleep_hold(&mut self, now: Clocks, token: lock::HoldToken) {
        self.state.lock_clock(now);
        self.state.lock_event(lock::Event::SleepOver { token });
    }

    /// The prompt serial and earliest monotonic time for its trusted adapter.
    /// The adapter schedules its own wakeup for that time.
    #[must_use]
    pub fn pending_lock_ui(&self) -> Option<(u32, Duration)> {
        self.state
            .session_lock
            .as_ref()
            .and_then(SessionLock::pending_ui)
    }

    /// Opens the attempt for the serial from [`Self::pending_lock_ui`].
    /// The serial must still be current and its scheduled time must have arrived.
    pub fn open_lock_attempt(&mut self, now: Clocks, ui: u32) {
        self.state.lock_clock(now);
        self.state.open_lock_attempt(ui);
    }

    /// The event loop can poll this descriptor for a verdict or a disconnect.
    /// [`Self::dispatch_at`] owns reads from it.
    #[must_use]
    pub fn authd_fd(&self) -> Option<BorrowedFd<'_>> {
        self.state
            .session_lock
            .as_ref()
            .and_then(SessionLock::authd_fd)
    }

    /// Takes a connected client. It gets the baseline globals.
    ///
    /// # Errors
    ///
    /// When the stream cannot be registered with the display.
    pub fn add_client(&mut self, stream: UnixStream) -> Result<(), Box<dyn Error>> {
        self.display
            .handle()
            .insert_client(stream, Arc::new(State::new_client_state()))?;
        Ok(())
    }

    /// One iteration of the event loop: client requests, due timers, at most
    /// one frame and outgoing events. With the session lock enabled this
    /// reads both system clocks. Otherwise `now` is the presentation time.
    ///
    /// # Errors
    ///
    /// When reading from or writing to the clients fails as a whole; a
    /// single broken client is disconnected instead.
    pub fn dispatch(&mut self, now: Duration) -> Result<(), Box<dyn Error>> {
        if self.state.session_lock.is_some() {
            return self.dispatch_at(read_lock_clocks());
        }
        self.dispatch_wayland(now)
    }

    /// Dispatches clients, authentication and presentation with supplied clocks.
    ///
    /// # Errors
    /// When the display fails to dispatch or flush clients.
    pub fn dispatch_at(&mut self, now: Clocks) -> Result<(), Box<dyn Error>> {
        self.state.lock_clock(now);
        self.state.receive_verdict();
        self.dispatch_wayland(now.monotonic)
    }

    fn dispatch_wayland(&mut self, now: Duration) -> Result<(), Box<dyn Error>> {
        self.display.dispatch_clients(&mut self.state)?;
        self.state.tick(now);
        self.display.flush_clients()?;
        Ok(())
    }

    /// When [`dispatch`](Self::dispatch) has to run again even if no client
    /// sends anything. A time that is not after `now` means at once.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        if self.state.redraw_queued() {
            return Some(Duration::ZERO);
        }
        self.state
            .timers
            .next()
            .into_iter()
            .chain(
                self.state
                    .session_lock
                    .as_ref()
                    .and_then(SessionLock::deadline),
            )
            .min()
    }

    /// Readable when a client has sent something.
    pub fn poll_fd(&mut self) -> BorrowedFd<'_> {
        self.display.backend().poll_fd()
    }

    /// Presses or releases a key of the seat's keyboard.
    /// With the session lock enabled this reads both system clocks. Use
    /// [`Self::key_at`] to supply controlled times for the lock and seat.
    pub fn key(&mut self, keycode: u32, pressed: bool, now: Duration) {
        if self.state.session_lock.is_some() {
            self.key_at(keycode, pressed, read_lock_clocks());
        } else {
            self.state.key(keycode, pressed, now);
        }
    }

    /// Checks the sleep clocks before routing a key from the seat.
    pub fn key_at(&mut self, keycode: u32, pressed: bool, now: Clocks) {
        self.state.lock_clock(now);
        self.state.key(keycode, pressed, now.monotonic);
    }

    /// The last frame as ARGB8888 rows, top to bottom.
    ///
    /// # Errors
    ///
    /// When the image cannot be read back.
    pub fn pixels(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        match &mut self.state.backend {
            Backend::Headless(backend) => backend.pixels(),
            Backend::Drm(_) => Err("pixels are available only with the headless backend".into()),
        }
    }
}
