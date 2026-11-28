//! `skorupa-compositor`: defaults to headless. `--drm` opens the active seat.
//!
//! Listens on the next free `wayland-N` socket in `XDG_RUNTIME_DIR`, prints
//! that name on standard output, and runs until it is killed.

use std::convert::Infallible;
use std::error::Error;
use std::ffi::OsString;
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

fn drm_requested(mut args: impl Iterator<Item = OsString>) -> Result<bool, &'static str> {
    let mode = args.next();
    if args.next().is_some() {
        return Err("expected at most one backend argument");
    }
    match mode.as_deref() {
        Some(mode) if mode == "--drm" => Ok(true),
        None => Ok(false),
        Some(mode) if mode == "--headless" => Ok(false),
        Some(_) => Err("expected --headless or --drm"),
    }
}

fn run() -> Result<Infallible, Box<dyn Error>> {
    let drm = drm_requested(std::env::args_os().skip(1))?;
    let mut event_loop: EventLoop<Compositor> = EventLoop::try_new()?;
    let mut compositor = if drm {
        Compositor::new_drm(&event_loop.handle())?
    } else {
        Compositor::new(Config::default())?
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
        Generic::new(poll_fd, Interest::READ, Mode::Edge),
        |_, _, _| Ok(PostAction::Continue),
    )?;

    let clock = Clock::<Monotonic>::new();
    loop {
        let timeout = compositor.dispatch_timeout(|| Duration::from(clock.now()))?;
        event_loop.dispatch(timeout, &mut compositor)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn req_ops_012_drm_requires_one_unambiguous_backend_argument() {
        let parse = |args: &[&str]| drm_requested(args.iter().map(OsString::from));
        assert_eq!(parse(&[]), Ok(false));
        assert_eq!(parse(&["--headless"]), Ok(false));
        assert_eq!(parse(&["--drm"]), Ok(true));
        for args in [
            &["--drm", "--headless"][..],
            &["--headless", "extra"],
            &["typo"],
        ] {
            assert!(parse(args).is_err());
        }

        assert!(drm_requested([OsString::from_vec(vec![0xff])].into_iter()).is_err());
    }
}
