//! Wayland compositor for one screen.
//!
//! Stage 1 is headless: real Wayland clients, the Pixman software renderer
//! drawing into memory, and frame timing from the pure reducer in
//! `skorupa-compositor-core`. Nothing here reads a clock: [`Compositor`] is
//! stepped with the time as an argument, by `main` from the monotonic clock
//! and by the tests from a simulated one.

mod drm;
mod frame;
pub mod headless;
pub mod layout;
mod state;

use std::error::Error;
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

pub use skorupa_compositor_core as core;
pub use skorupa_protocol as protocol;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::Display;

pub use crate::state::Config;
use crate::state::{Backend, State};

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

    /// One iteration of the event loop at time `now` on the presentation
    /// clock: client requests, the timers that are due, at most one frame,
    /// and the events for the clients.
    ///
    /// # Errors
    ///
    /// When reading from or writing to the clients fails as a whole; a
    /// single broken client is disconnected instead.
    pub fn dispatch(&mut self, now: Duration) -> Result<(), Box<dyn Error>> {
        self.display.dispatch_clients(&mut self.state)?;
        self.state.tick(now);
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
        self.dispatch(now())?;
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
        self.state.timers.next()
    }

    /// Readable when a client has sent something.
    pub fn poll_fd(&mut self) -> BorrowedFd<'_> {
        self.display.backend().poll_fd()
    }

    /// Presses or releases a key of the seat's keyboard. The headless
    /// backend has no input device, so this is its only source of keys.
    pub fn key(&mut self, keycode: u32, pressed: bool, now: Duration) {
        self.state.key(keycode, pressed, now);
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
