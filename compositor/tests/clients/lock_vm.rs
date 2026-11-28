//! Test-only driver for the lock scenario on the NixOS VM's actual Wayland seat.
//! The scripted broker sees only a connected descriptor. No verdict path is
//! added to the shipping binary or to an ordinary Wayland client.

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use skorupa_compositor::core::lock::{Config, InstanceId, Phase, SessionId};
use skorupa_compositor::protocol::{
    Attempt, Challenge, MAX_FRAME_LEN, Message, Outcome, PREFIX_LEN, Verdict, decode, frame_len,
};
use skorupa_compositor::{Compositor, read_lock_clocks};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::wayland::socket::ListeningSocketSource;

const CONTROL_POLL: Duration = Duration::from_millis(10);

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let role = arguments.next().ok_or("expected a VM fixture role")?;
    let control = PathBuf::from(
        arguments
            .next()
            .ok_or("expected the VM control directory")?,
    );
    match role.to_str() {
        Some("seat") => run_seat(&control),
        Some("broker") => run_broker(&control),
        _ => Err("unknown VM fixture role".into()),
    }
}

struct Broker(Child);

impl Broker {
    fn start(control: &Path) -> io::Result<(Self, UnixStream)> {
        let (compositor, broker) = UnixStream::pair()?;
        let output: OwnedFd = broker.try_clone()?.into();
        let input: OwnedFd = broker.into();
        let child = Command::new(std::env::current_exe()?)
            .arg("broker")
            .arg(control)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .spawn()?;
        Ok((Self(child), compositor))
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_seat(control: &Path) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<Compositor> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let mut compositor = Compositor::new_drm(&handle)?;
    let (_broker, private_channel) = Broker::start(control)?;
    let mut instance = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut instance)?;
    let mut session = [0; 32];
    session[..6].copy_from_slice(b"vmlock");
    let config = Config {
        instance: InstanceId(instance),
        session: SessionId(session),
        jump_threshold: Duration::from_secs(1),
    };
    compositor.enable_session_lock(config, read_lock_clocks(), private_channel)?;

    let listener = ListeningSocketSource::new_auto()?;
    handle.insert_source(listener, |stream, (), compositor| {
        if let Err(err) = compositor.add_client(stream) {
            tracing::warn!(%err, "could not attach the VM Wayland client");
        }
    })?;
    let wayland = compositor.poll_fd().try_clone_to_owned()?;
    handle.insert_source(
        Generic::new(wayland, Interest::READ, Mode::Level),
        |_, _, _| Ok(PostAction::Continue),
    )?;

    let mut startup_marked = false;
    let mut startup_released = false;
    let mut relocked = false;
    let mut disconnect_marked = false;
    let mut invalid_replies = [
        ("invalid-reply-1", "invalid-dispatched-1"),
        ("invalid-reply-3", "invalid-dispatched-3"),
    ]
    .into_iter()
    .peekable();
    loop {
        let now = read_lock_clocks();
        if !startup_released && control.join("bridge-awake").try_exists()? {
            let token = compositor.sleep_hold_token().ok_or("no startup hold")?;
            compositor.end_sleep_hold(now, token);
            startup_released = true;
        }
        if !relocked && control.join("lock").try_exists()? {
            if compositor.session_phase() != Some(Phase::Unlocked) {
                return Err("relock requested before the session unlocked".into());
            }
            compositor.request_lock(now);
            relocked = true;
        }
        if let Some((ui, ready)) = compositor.pending_lock_ui()
            && now.monotonic >= ready
        {
            compositor.open_lock_attempt(now, ui);
        }
        let mut invalid_dispatched = None;
        if let Some((sent, dispatched)) = invalid_replies.peek().copied()
            && control.join(sent).try_exists()?
        {
            invalid_dispatched = Some(dispatched);
            let _ = invalid_replies.next();
        }
        compositor.dispatch_at(now)?;

        if let Some(marker) = invalid_dispatched {
            if compositor.session_phase() != Some(Phase::Locked) || compositor.authd_fd().is_none()
            {
                return Err("invalid verdict changed the lock or disconnected its broker".into());
            }
            fs::write(control.join(marker), [])?;
        }
        if !startup_marked && compositor.session_phase() == Some(Phase::Locked) {
            fs::write(control.join("startup-covered"), [])?;
            startup_marked = true;
        }
        if !disconnect_marked
            && compositor.authd_fd().is_none()
            && compositor.session_phase() == Some(Phase::Locked)
        {
            fs::write(control.join("disconnect-covered"), [])?;
            disconnect_marked = true;
        }

        let timeout = compositor.next_deadline().map_or(CONTROL_POLL, |deadline| {
            deadline.saturating_sub(now.monotonic).min(CONTROL_POLL)
        });
        event_loop.dispatch(Some(timeout), &mut compositor)?;
    }
}

fn run_broker(control: &Path) -> Result<(), Box<dyn Error>> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut previous_grant = None;

    for number in 1..=4 {
        let challenge = read_challenge(&mut input)?;
        fs::write(control.join(format!("challenge-{number}")), [])?;
        wait_for_marker(&control.join(format!("answer-{number}")))?;

        let current = Verdict {
            compositor_instance: challenge.compositor_instance,
            session: challenge.session,
            epoch: challenge.epoch,
            attempt: challenge.attempt,
            outcome: Outcome::Granted,
        };
        match number {
            1 => {
                let next_attempt = current
                    .attempt
                    .0
                    .checked_add(1)
                    .ok_or("attempt exhausted")?;
                send_verdict(
                    &mut output,
                    Verdict {
                        attempt: Attempt(next_attempt),
                        ..current
                    },
                )?;
            }
            3 => {
                let replay = previous_grant.ok_or("no earlier grant to replay")?;
                send_verdict(&mut output, replay)?;
            }
            _ => {
                send_verdict(&mut output, current)?;
                previous_grant = Some(current);
            }
        }
        if matches!(number, 1 | 3) {
            // Wait until the compositor dispatched the invalid reply before
            // sending the current denial, so it reads one reply at a time.
            fs::write(control.join(format!("invalid-reply-{number}")), [])?;
            wait_for_marker(&control.join(format!("deny-{number}")))?;
            send_verdict(
                &mut output,
                Verdict {
                    outcome: Outcome::Denied,
                    ..current
                },
            )?;
        }
    }

    // The test first observes the unlocked scene on the live connection.
    wait_for_marker(&control.join("close-broker"))?;
    Ok(())
}

fn read_challenge(input: &mut impl Read) -> Result<Challenge, Box<dyn Error>> {
    let mut bytes = [0; MAX_FRAME_LEN];
    let mut prefix = [0; PREFIX_LEN];
    input.read_exact(&mut prefix)?;
    let len = frame_len(prefix)?;
    bytes[..PREFIX_LEN].copy_from_slice(&prefix);
    input.read_exact(&mut bytes[PREFIX_LEN..len])?;
    let (Message::Challenge(challenge), _) = decode(&bytes[..len])? else {
        return Err("unexpected message on the VM broker descriptor".into());
    };
    Ok(challenge)
}

fn send_verdict(output: &mut impl Write, verdict: Verdict) -> Result<(), Box<dyn Error>> {
    let mut bytes = [0; MAX_FRAME_LEN];
    let len = Message::Verdict(verdict).encode(&mut bytes)?;
    output.write_all(&bytes[..len])?;
    output.flush()?;
    Ok(())
}

fn wait_for_marker(path: &Path) -> io::Result<()> {
    for _ in 0..2_000 {
        if path.try_exists()? {
            return Ok(());
        }
        thread::sleep(CONTROL_POLL);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "VM broker response was not requested",
    ))
}
