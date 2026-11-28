//! `skorupa-compositor`: defaults to headless. `--drm` opens the active seat.
//!
//! Listens on the next free `wayland-N` socket in `XDG_RUNTIME_DIR`, prints
//! that name on standard output, and runs until it is killed.

use std::convert::Infallible;
use std::error::Error;
use std::process::ExitCode;
use std::time::Duration;

use skorupa_compositor::{Compositor, Config};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::utils::{Clock, Monotonic};
use smithay::wayland::socket::ListeningSocketSource;

fn main() -> ExitCode {
    let Err(err) = run();
    eprintln!("skorupa-compositor: {err}");
    ExitCode::FAILURE
}

fn run() -> Result<Infallible, Box<dyn Error>> {
    let mut event_loop: EventLoop<Compositor> = EventLoop::try_new()?;
    let mut compositor = match std::env::args().nth(1).as_deref() {
        Some("--drm") => Compositor::new_drm(&event_loop.handle())?,
        None | Some("--headless") => Compositor::new(Config::default())?,
        Some(_) => return Err("expected --headless or --drm".into()),
    };

    let listener = ListeningSocketSource::new_auto()?;
    println!("{}", listener.socket_name().to_string_lossy());
    event_loop
        .handle()
        .insert_source(listener, |stream, (), compositor| {
            if let Err(err) = compositor.add_client(stream) {
                tracing::warn!(%err, "could not take a new client");
            }
        })?;

    // Only wakes the loop up: the requests are read by `dispatch` below.
    let poll_fd = compositor.poll_fd().try_clone_to_owned()?;
    event_loop.handle().insert_source(
        Generic::new(poll_fd, Interest::READ, Mode::Level),
        |_, _, _| Ok(PostAction::Continue),
    )?;

    let clock = Clock::<Monotonic>::new();
    loop {
        let now = Duration::from(clock.now());
        compositor.dispatch(now)?;
        let timeout = compositor
            .next_deadline()
            .map(|deadline| deadline.saturating_sub(now));
        event_loop.dispatch(timeout, &mut compositor)?;
    }
}
