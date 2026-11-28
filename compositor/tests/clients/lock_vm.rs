//! Test driver for the NixOS lock scenario. A separate broker inherits the
//! peer descriptor so the guest exercises the real authd socket boundary.

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
    let startup_token = compositor.sleep_hold_token().ok_or("no startup hold")?;

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

    // Submit the startup cover before the loop can deliver its KMS event.
    compositor.dispatch(read_lock_clocks().monotonic)?;
    if compositor.session_phase() != Some(Phase::Locking) {
        return Err("lock acknowledged before dispatching the KMS completion".into());
    }

    let mut startup_marked = false;
    let mut startup_released = false;
    let mut relocked = false;
    let mut loss_tested = false;
    let mut vt_locked = false;
    let mut vt_paused = false;
    let mut disconnect_marked = false;
    loop {
        let now = read_lock_clocks();
        if !startup_released && control.join("bridge-awake").try_exists()? {
            startup_released = compositor.end_sleep_hold(now, startup_token);
        }
        if !relocked && control.join("lock").try_exists()? {
            if compositor.session_phase() != Some(Phase::Unlocked) {
                return Err("relock requested before the session unlocked".into());
            }
            compositor.request_lock(now);
            relocked = true;
        }
        if !loss_tested && control.join("delay-kms").try_exists()? {
            lock_without_kms(&mut compositor, &mut event_loop, control)?;
            loss_tested = true;
            continue;
        }
        if !vt_locked && control.join("vt-lock").try_exists()? {
            compositor.request_lock(now);
            vt_locked = true;
        }
        if !vt_paused
            && control.join("vt-switch").try_exists()?
            && compositor.session_phase() == Some(Phase::Locking)
        {
            fs::write(control.join("vt-fenced"), [])?;
            vt_paused = true;
        }
        if let Some((ui, ready)) = compositor.pending_lock_ui()
            && now.monotonic >= ready
        {
            compositor.open_lock_attempt(now, ui);
        }
        compositor.dispatch(now.monotonic)?;

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

fn lock_without_kms(
    compositor: &mut Compositor,
    event_loop: &mut EventLoop<'_, Compositor>,
    control: &Path,
) -> Result<(), Box<dyn Error>> {
    let now = read_lock_clocks();
    compositor.request_lock(now);
    // Stop reading KMS events while the compositor still services its timers.
    let end = now.monotonic + Duration::from_secs(1);
    loop {
        compositor.dispatch(read_lock_clocks().monotonic)?;
        if read_lock_clocks().monotonic > end {
            return Err("DRM output remained enabled past the lock budget".into());
        }
        if compositor.session_phase() == Some(Phase::Locked) {
            break;
        }
        thread::sleep(CONTROL_POLL);
    }
    expect_lock(compositor, Phase::Locked)?;
    fs::write(control.join("fallback-dark"), [])?;
    wait_for_marker(&control.join("resume-kms"))?;

    // Queue the wake while the old completion is still held by the notifier.
    compositor.key_at(38, true, read_lock_clocks());
    expect_lock(compositor, Phase::Locked)?;
    for _ in 0..100 {
        event_loop.dispatch(Some(CONTROL_POLL), compositor)?;
        if compositor.session_phase() == Some(Phase::Locking) {
            expect_lock(compositor, Phase::Locking)?;
            // Submit the new cover without dispatching its completion yet.
            compositor.dispatch(read_lock_clocks().monotonic)?;
            return expect_lock(compositor, Phase::Locking);
        }
        expect_lock(compositor, Phase::Locked)?;
    }
    Err("the delayed KMS completion did not release its slot".into())
}

fn expect_lock(compositor: &Compositor, phase: Phase) -> Result<(), Box<dyn Error>> {
    if compositor.session_phase() != Some(phase) || compositor.pending_lock_ui().is_some() {
        return Err(format!("expected {phase:?} with authentication fenced").into());
    }
    Ok(())
}

fn run_broker(control: &Path) -> Result<(), Box<dyn Error>> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut previous_grant = None;

    for number in 1..=7 {
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
        let stale = match number {
            1 => Some(Verdict {
                attempt: Attempt(current.attempt.0.wrapping_add(1)),
                ..current
            }),
            3 => Some(previous_grant.ok_or("no earlier grant to replay")?),
            _ => None,
        };
        if let Some(stale) = stale {
            send_verdict(&mut output, stale)?;
            send_verdict(
                &mut output,
                Verdict {
                    outcome: Outcome::Denied,
                    ..current
                },
            )?;
        } else {
            send_verdict(&mut output, current)?;
            previous_grant = Some(current);
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
