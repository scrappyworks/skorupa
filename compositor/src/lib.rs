//! Wayland compositor for one screen, backed by DRM/KMS or an in-memory Pixman output.

mod drm;
mod frame;
pub mod headless;
pub mod layout;
mod session_lock;
mod state;
#[cfg(test)]
mod test_support;

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
    /// [`lock::Phase::Locked`] after the backend presents the cover or confirms
    /// that the output is off.
    ///
    /// # Errors
    /// Returns `AlreadyExists` for a second installation. An invalid session
    /// identifier or socket setup error keeps the cover and its output-off
    /// fallback active. The launcher can retry with a valid configuration.
    pub fn enable_session_lock(
        &mut self,
        config: lock::Config,
        now: Clocks,
        authd: UnixStream,
    ) -> io::Result<()> {
        if self
            .state
            .session_lock
            .as_ref()
            .is_some_and(SessionLock::configured)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "session lock already enabled",
            ));
        }
        let (mut session, effects) = SessionLock::start(config, now);
        let setup = session.configure(config.session, authd);
        // An unsuccessful retry must not reset the pending cover or its deadline.
        if setup.is_ok() || self.state.session_lock.is_none() {
            self.state.session_lock = Some(session);
            self.state.apply_lock_effects(effects);
        }
        setup
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
    /// Returns true once the installed lock has left the sleep hold. On false,
    /// retry with a fresh clock sample only when [`Self::sleep_hold_token`]
    /// still returns `token`. A different token belongs to a later hold and
    /// requires a fresh inhibitor and sleep-state check before reporting.
    #[must_use]
    pub fn end_sleep_hold(&mut self, now: Clocks, token: lock::HoldToken) -> bool {
        if !self
            .state
            .session_lock
            .as_ref()
            .is_some_and(SessionLock::configured)
        {
            self.state.lock_clock(now);
            return false;
        }
        self.state
            .lock_event_at(now, lock::Event::SleepOver { token });
        self.state
            .session_lock
            .as_ref()
            .is_some_and(SessionLock::sleep_hold_over)
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

    /// Reports that the trusted lock UI identified by `ui` exited or failed
    /// to start. Its attempt ends before any later verdict is handled. An
    /// exit reported for a retired UI has no effect on its replacement.
    pub fn lock_ui_exited(&mut self, now: Clocks, ui: u32) {
        self.state.lock_event_at(now, lock::Event::UiExited { ui });
    }

    /// Opens the attempt for the serial from [`Self::pending_lock_ui`].
    /// The serial must still be current and its scheduled time must have arrived.
    pub fn open_lock_attempt(&mut self, now: Clocks, ui: u32) {
        self.state.lock_clock(now);
        self.state.open_lock_attempt(ui);
    }

    /// Attaches the private endpoint for a restarted authentication broker.
    ///
    /// The trusted launcher must give its peer exclusively to the new broker
    /// by inheritance. The existing lock and its attempt counter stay in force.
    ///
    /// # Errors
    /// Returns `NotConnected` before session lock setup, `AlreadyExists` while
    /// the old broker is attached, or the error from configuring the socket.
    pub fn reattach_authd(&mut self, stream: UnixStream) -> io::Result<()> {
        self.state
            .session_lock
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "session lock is absent"))?
            .attach_authd(stream)
    }

    /// Poll this descriptor with level-triggered readiness for verdicts and
    /// disconnects. Each dispatch handles at most one verdict. Register the
    /// new descriptor after [`Self::reattach_authd`].
    /// [`Self::dispatch`] owns reads from it.
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
        self.dispatch_from_clock(now, || now)
    }

    fn dispatch_from_clock(
        &mut self,
        start: Duration,
        mut monotonic: impl FnMut() -> Duration,
    ) -> Result<(), Box<dyn Error>> {
        if self.state.session_lock.is_some() {
            self.dispatch_with_clock(start, &mut read_lock_clocks)
        } else {
            self.dispatch_with_clock(start, &mut || {
                let now = monotonic();
                Clocks::exact(now, now)
            })
        }
    }

    /// Dispatches clients, authentication and presentation with supplied clocks.
    ///
    /// # Errors
    /// When the display fails to dispatch or flush clients.
    pub fn dispatch_at(&mut self, now: Clocks) -> Result<(), Box<dyn Error>> {
        self.dispatch_with_clock(now.monotonic, &mut || now)
    }

    fn dispatch_with_clock(
        &mut self,
        now: Duration,
        sample: &mut impl FnMut() -> Clocks,
    ) -> Result<(), Box<dyn Error>> {
        self.state.refresh_lock_clock(now, sample);
        self.state.receive_verdict(sample);
        // Wayland dispatch drains ready requests. While a cover is pending,
        // only its presentation or confirmed output-off can release that work.
        let cover_pending = self.session_phase() == Some(lock::Phase::Locking);
        if !cover_pending {
            self.display.dispatch_clients(&mut self.state)?;
        }
        let now = self.state.refresh_lock_clock(now, sample);
        self.state.tick(now, sample);
        if cover_pending && self.session_phase() == Some(lock::Phase::Locked) {
            self.display.dispatch_clients(&mut self.state)?;
        }
        self.display.flush_clients()?;
        Ok(())
    }

    /// Dispatches one iteration and returns the time to wait for the next
    /// deadline. Read the presentation clock again after dispatch so time
    /// spent rendering counts toward the wait. An overdue deadline gives
    /// a zero wait and an idle compositor gives `None`.
    ///
    /// # Errors
    /// Returns errors from [`Self::dispatch`].
    pub fn dispatch_timeout(
        &mut self,
        mut now: impl FnMut() -> Duration,
    ) -> Result<Option<Duration>, Box<dyn Error>> {
        self.dispatch_from_clock(now(), &mut now)?;
        Ok(self
            .next_deadline()
            .map(|deadline| deadline.saturating_sub(now())))
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

    /// Readable when clients have pending requests. Use edge-triggered
    /// readiness and also dispatch after timer and backend events: requests
    /// stay queued while the lock waits for protected presentation.
    pub fn poll_fd(&mut self) -> BorrowedFd<'_> {
        self.display.backend().poll_fd()
    }

    /// Presses or releases an XKB keycode on the seat. Values below 8 are ignored.
    /// With the session lock enabled this reads both system clocks. Use
    /// [`Self::key_at`] to supply controlled times for the lock and seat.
    pub fn key(&mut self, keycode: u32, pressed: bool, now: Duration) {
        if self.state.session_lock.is_some() {
            self.key_at(keycode, pressed, read_lock_clocks());
        } else {
            self.state.key(keycode, pressed, now);
        }
    }

    /// Checks the sleep clocks before routing an XKB keycode from the seat, as in [`Self::key`].
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
