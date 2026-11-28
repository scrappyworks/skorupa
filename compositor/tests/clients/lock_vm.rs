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

use skorupa_compositor::core::lock::{Clocks, Config, InstanceId, Phase, SessionId};
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
    let listener = ListeningSocketSource::new_auto()?;
    handle.insert_source(listener, |stream, (), compositor| {
        if let Err(err) = compositor.add_client(stream) {
            tracing::warn!(%err, "could not attach the VM Wayland client");
        }
    })?;
    let wayland = compositor.poll_fd().try_clone_to_owned()?;
    handle.insert_source(
        Generic::new(wayland, Interest::READ, Mode::Edge),
        |_, _, _| Ok(PostAction::Continue),
    )?;

    fail_setup_without_kms(&mut compositor, config, control)?;
    let now = read_lock_clocks();
    compositor.enable_session_lock(config, now, private_channel)?;
    // This lock has no KMS confirmation of its own yet.
    expect_lock(&compositor, Phase::Locking)?;
    wait_for_dark(&mut compositor, now.monotonic)?;
    let startup_token = compositor.sleep_hold_token().ok_or("no startup hold")?;
    wake_with_queued_flip(&mut compositor, &mut event_loop)?;

    let mut startup_marked = false;
    let mut startup_released = false;
    let mut relocked = false;
    let mut loss_tested = false;
    let mut session_flip_tested = false;
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
        if !session_flip_tested && control.join("hold-session-flip").try_exists()? {
            for (digit, marker) in [(2, "return-to-first"), (1, "release-session-flip")] {
                switch_without_kms(&mut compositor, digit, &control.join(marker))?;
            }
            session_flip_tested = true;
            continue;
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
            verify_verdict_while_paused(&mut compositor, control)?;
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
            deadline
                .saturating_sub(read_lock_clocks().monotonic)
                .min(CONTROL_POLL)
        });
        event_loop.dispatch(Some(timeout), &mut compositor)?;
    }
}

// REQ-AUTH-004: consume the stale verdict before the guest returns to the seat.
fn verify_verdict_while_paused(
    compositor: &mut Compositor,
    control: &Path,
) -> Result<(), Box<dyn Error>> {
    let unread = |compositor: &Compositor| -> Result<u64, Box<dyn Error>> {
        let fd = compositor.authd_fd().ok_or("broker connection closed")?;
        Ok(rustix::io::ioctl_fionread(fd)?)
    };
    fs::write(control.join("vt-fenced"), [])?;
    wait_for_marker(&control.join("vt-verdict-sent"))?;
    if unread(compositor)? == 0 {
        return Err("expected a queued verdict before dispatch".into());
    }
    compositor.dispatch(read_lock_clocks().monotonic)?;
    expect_lock(compositor, Phase::Locking)?;
    if unread(compositor)? != 0 {
        return Err("verdict was not consumed while the seat was inactive".into());
    }
    fs::write(control.join("vt-verdict-handled"), [])?;
    Ok(())
}

// REQ-PERF-013: keep handling redraws and watchdog timers while the notifier
// holds the first flip. The driver verifies the pixels before releasing it.
fn switch_without_kms(
    compositor: &mut Compositor,
    digit: u32,
    marker: &Path,
) -> Result<(), Box<dyn Error>> {
    let number = 9 + digit;
    for (key, pressed) in [(133, true), (number, true), (number, false), (133, false)] {
        compositor.key_at(key, pressed, read_lock_clocks());
    }
    let end = read_lock_clocks().monotonic + Duration::from_secs(30);
    while !marker.try_exists()? {
        let now = read_lock_clocks().monotonic;
        if now >= end {
            return Err("timed out waiting for the driver to verify the workspace".into());
        }
        compositor.dispatch(now)?;
        thread::sleep(CONTROL_POLL);
    }
    Ok(())
}

// REQ-LOCK-011: an invalid authentication configuration still has to hide
// the previous session frame when the notifier withholds its completion.
fn fail_setup_without_kms(
    compositor: &mut Compositor,
    config: Config,
    control: &Path,
) -> Result<(), Box<dyn Error>> {
    // Submit a session frame and withhold its completion while lock setup fails.
    compositor.dispatch(read_lock_clocks().monotonic)?;
    let invalid = Config {
        session: SessionId([0; 32]),
        ..config
    };
    let (socket, _peer) = UnixStream::pair()?;
    if !matches!(
        compositor.enable_session_lock(invalid, read_lock_clocks(), socket),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput
    ) {
        return Err("invalid session was not rejected".into());
    }
    wait_for_dark(compositor, read_lock_clocks().monotonic)?;
    fs::write(control.join("setup-failed-dark"), [])?;
    wait_for_marker(&control.join("retry-setup"))?;

    // REQ-LOCK-002/015: the first clear predates a possible resume. Retry
    // before the provisional lock has a chance to obtain fresh KMS evidence.
    let now = read_lock_clocks();
    let uncertain = Clocks::bracketed(
        now.boottime,
        now.monotonic,
        now.boottime + Duration::from_secs(10),
    );
    compositor.request_lock(uncertain);
    expect_lock(compositor, Phase::Locking)
}

fn lock_without_kms(
    compositor: &mut Compositor,
    event_loop: &mut EventLoop<'_, Compositor>,
    control: &Path,
) -> Result<(), Box<dyn Error>> {
    let now = read_lock_clocks();
    compositor.request_lock(now);
    wait_for_dark(compositor, now.monotonic)?;
    fs::write(control.join("fallback-dark"), [])?;
    wait_for_marker(&control.join("resume-kms"))?;
    wake_with_queued_flip(compositor, event_loop)
}

// Stop reading KMS events while the compositor still services its timers.
fn wait_for_dark(compositor: &mut Compositor, start: Duration) -> Result<(), Box<dyn Error>> {
    let end = start + Duration::from_secs(1);
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
    expect_lock(compositor, Phase::Locked)
}

fn wake_with_queued_flip(
    compositor: &mut Compositor,
    event_loop: &mut EventLoop<'_, Compositor>,
) -> Result<(), Box<dyn Error>> {
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

        let current = challenge.verdict(Outcome::Granted);
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
            if number == 6 {
                fs::write(control.join("vt-verdict-sent"), [])?;
            }
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
