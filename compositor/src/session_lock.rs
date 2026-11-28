use std::io::{self, Error, ErrorKind, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use skorupa_compositor_core::lock::{self, Clocks, Effect, Event, InputMode};
use skorupa_protocol::{
    Attempt, Challenge, CompositorInstance, DecodeError, LockEpoch, MAX_FRAME_LEN, Message,
    SessionId, Verdict, decode,
};
use smithay::reexports::rustix::event::{PollFd, PollFlags, Timespec, poll};
use smithay::reexports::rustix::time::ClockId;
use smithay::utils::{Clock, ClockSource, Monotonic, NonNegativeClockSource};

use crate::state::State;

struct BootTime;

impl ClockSource for BootTime {
    const ID: ClockId = ClockId::Boottime;
}

impl NonNegativeClockSource for BootTime {}

/// Samples the clocks whose difference lets the reducer detect time spent asleep.
#[must_use]
pub fn read_lock_clocks() -> Clocks {
    let clock = Clock::<BootTime>::new();
    let before = clock.now();
    let monotonic = Clock::<Monotonic>::new().now();
    // Include a resume that occurs after reading the monotonic clock.
    Clocks::bracketed(before.into(), monotonic.into(), clock.now().into())
}

pub(crate) struct SessionLock {
    reducer: lock::State,
    now: Clocks,
    authd: Option<Authd>,
    session: SessionId,
    pending_ui: Option<(u32, Duration)>,
    deadline: Option<Duration>,
}

impl SessionLock {
    pub(crate) fn start(
        config: lock::Config,
        now: Clocks,
        stream: UnixStream,
    ) -> io::Result<(Self, Vec<Effect>)> {
        // Reject nonzero padding so the wire ID round-trips to the reducer's ID.
        let id = config.session.0;
        let end = id.iter().position(|&byte| byte == 0).unwrap_or(id.len());
        let (name, padding) = id.split_at(end);
        if padding.iter().any(|&byte| byte != 0) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid session padding",
            ));
        }
        let session =
            SessionId::new(name).map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
        let authd = Some(Authd::new(stream)?);
        let (reducer, effects) = lock::State::start(config, now);
        Ok((
            Self {
                reducer,
                now,
                authd,
                session,
                pending_ui: None,
                deadline: None,
            },
            effects,
        ))
    }

    pub(crate) const fn phase(&self) -> lock::Phase {
        self.reducer.phase()
    }

    pub(crate) const fn needs_cover_frame(&self) -> bool {
        matches!(self.phase(), lock::Phase::Locking)
    }

    pub(crate) const fn hold_token(&self) -> Option<lock::HoldToken> {
        self.reducer.hold_token()
    }

    pub(crate) fn authd_fd(&self) -> Option<BorrowedFd<'_>> {
        self.authd.as_ref().map(|authd| authd.stream.as_fd())
    }

    pub(crate) const fn pending_ui(&self) -> Option<(u32, Duration)> {
        self.pending_ui
    }

    pub(crate) const fn deadline(&self) -> Option<Duration> {
        self.deadline
    }

    fn step(&mut self, event: Event) -> Vec<Effect> {
        let (reducer, effects) = lock::step(
            self.reducer,
            lock::Input {
                now: self.now,
                event,
            },
        );
        self.reducer = reducer;
        effects
    }

    fn send_challenge(&mut self, binding: lock::Binding) -> io::Result<()> {
        let authd = self.authd.as_ref().ok_or_else(authd_disconnected)?;
        authd.send(Challenge {
            compositor_instance: CompositorInstance(binding.instance.0),
            session: self.session,
            epoch: LockEpoch(binding.epoch),
            attempt: Attempt(binding.attempt),
        })
    }
}

impl State {
    pub(crate) fn lock_clock(&mut self, now: Clocks) {
        self.lock_event_at(now, Event::Tick);
    }

    pub(crate) fn lock_event_at(&mut self, now: Clocks, event: Event) {
        if let Some(session) = &mut self.session_lock {
            session.now = now;
            self.lock_event(event);
        }
    }

    pub(crate) fn refresh_lock_clock(
        &mut self,
        now: Duration,
        sample: &mut impl FnMut() -> Clocks,
    ) -> Duration {
        if self.session_lock.is_some() {
            let clocks = sample();
            self.lock_clock(clocks);
            clocks.monotonic
        } else {
            now
        }
    }

    pub(crate) fn lock_event(&mut self, event: Event) {
        if let Some(session) = &mut self.session_lock {
            let effects = session.step(event);
            self.apply_lock_effects(effects);
        }
    }

    pub(crate) fn apply_lock_effects(&mut self, effects: Vec<Effect>) {
        let mut channel_failed = false;
        for effect in effects {
            let Some(session) = self.session_lock.as_mut() else {
                return;
            };
            match effect {
                Effect::RouteInput(mode) => self.set_session_visible(mode == InputMode::Session),
                Effect::ShowFallback => self.set_session_visible(false),
                Effect::ScheduleFrame => {
                    if session.needs_cover_frame() {
                        let now = session.now.monotonic;
                        self.arm_cover_deadline(now);
                    }
                    self.queue_redraw();
                }
                Effect::Unlock { .. } => self.queue_redraw(),
                Effect::StartUi { ui, delay } => {
                    session.pending_ui = session
                        .authd
                        .as_ref()
                        .map(|_| (ui, session.now.monotonic.saturating_add(delay)));
                }
                Effect::StopUi { .. } => {
                    session.pending_ui = None;
                    session.deadline = None;
                }
                Effect::SendChallenge {
                    binding, deadline, ..
                } => {
                    session.deadline = Some(deadline);
                    channel_failed |= session.send_challenge(binding).is_err();
                }
                // Capture, grant consumers and the logind bridge are not
                // connected to this compositor adapter yet.
                Effect::RevokeAllGrants
                | Effect::LockAcknowledged { .. }
                | Effect::OfferSleepHold(_)
                | Effect::ReleaseSleepInhibitor { .. } => {}
            }
        }
        if channel_failed {
            self.disconnect_authd();
        }
    }

    pub(crate) fn open_lock_attempt(&mut self, ui: u32) {
        let Some(session) = &mut self.session_lock else {
            return;
        };
        if session.authd.is_some()
            && session
                .pending_ui
                .is_some_and(|(pending, ready)| pending == ui && session.now.monotonic >= ready)
        {
            session.pending_ui = None;
            self.lock_event(Event::UiConnected { ui });
            self.lock_event(Event::AttemptOpened { ui });
        }
    }

    pub(crate) fn receive_verdict(&mut self, sample: &mut impl FnMut() -> Clocks) {
        let Some(authd) = self
            .session_lock
            .as_mut()
            .and_then(|session| session.authd.as_mut())
        else {
            return;
        };
        match authd.receive() {
            Ok(Some(verdict)) => self.lock_event_at(sample(), verdict_event(verdict)),
            Ok(None) => {}
            Err(_) => self.disconnect_authd(),
        }
    }

    fn disconnect_authd(&mut self) {
        let Some(session) = &mut self.session_lock else {
            return;
        };
        session.authd = None;
        session.pending_ui = None;
        self.lock_event(Event::AuthdDisconnected);
        self.lock_event(Event::LockRequested);
    }
}

fn verdict_event(verdict: Verdict) -> Event {
    let mut session = [0; SessionId::MAX_LEN];
    let id = verdict.session.as_bytes();
    session[..id.len()].copy_from_slice(id);
    let binding = lock::Binding {
        instance: lock::InstanceId(verdict.compositor_instance.0),
        session: lock::SessionId(session),
        epoch: verdict.epoch.0,
        attempt: verdict.attempt.0,
    };
    let outcome = match verdict.outcome {
        skorupa_protocol::Outcome::Granted => lock::Outcome::Granted,
        skorupa_protocol::Outcome::Denied => lock::Outcome::Denied,
    };
    Event::Verdict { binding, outcome }
}

struct Authd {
    stream: UnixStream,
    buffer: [u8; MAX_FRAME_LEN],
    received: usize,
}

impl Authd {
    fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            buffer: [0; MAX_FRAME_LEN],
            received: 0,
        })
    }

    fn send(&self, challenge: Challenge) -> io::Result<()> {
        let mut stream = &self.stream;
        let mut buffer = [0; MAX_FRAME_LEN];
        let len = Message::Challenge(challenge)
            .encode(&mut buffer)
            .map_err(Error::other)?;
        // Retire the channel on a short or blocked write to bound the queue.
        if stream.write(&buffer[..len])? != len {
            return Err(Error::new(
                ErrorKind::WriteZero,
                "incomplete authd challenge",
            ));
        }
        Ok(())
    }

    fn receive(&mut self) -> io::Result<Option<Verdict>> {
        // A peer that fragments a frame gets at most two reads per dispatch.
        let mut reads_left = 2;
        loop {
            match decode(&self.buffer[..self.received]) {
                Ok((message, _)) => return self.finish(message).map(Some),
                Err(DecodeError::Truncated { needed, .. }) if reads_left > 0 => {
                    reads_left -= 1;
                    match self.stream.read(&mut self.buffer[self.received..needed]) {
                        Ok(0) => return Err(authd_disconnected()),
                        Ok(read) => self.received += read,
                        Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(None),
                        Err(err) => return Err(err),
                    }
                }
                Err(DecodeError::Truncated { .. }) => return Ok(None),
                Err(err) => return Err(Error::new(ErrorKind::InvalidData, err)),
            }
        }
    }

    fn finish(&mut self, message: Message) -> io::Result<Verdict> {
        let Message::Verdict(verdict) = message else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "authd sent the wrong message",
            ));
        };
        // Queued verdicts can hide a half-close from a read. Check hangup
        // before the reducer can apply any verdict from a closed broker.
        let mut status = PollFd::new(&self.stream, PollFlags::RDHUP);
        poll(
            std::slice::from_mut(&mut status),
            Some(&Timespec::default()),
        )?;
        let closed = PollFlags::RDHUP | PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL;
        if status.revents().intersects(closed) {
            return Err(authd_disconnected());
        }
        self.received = 0;
        Ok(verdict)
    }
}

fn authd_disconnected() -> Error {
    Error::new(ErrorKind::UnexpectedEof, "authd disconnected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;

    use crate::{Compositor, Config};

    const BOOT: Duration = Duration::from_secs(100);

    fn covered() -> (Compositor, UnixStream) {
        let (socket, peer) = UnixStream::pair().unwrap();
        let mut compositor = Compositor::new(Config::default()).unwrap();
        let mut session = [0; SessionId::MAX_LEN];
        session[..2].copy_from_slice(b"c2");
        let now = Clocks::exact(BOOT, BOOT);
        compositor
            .enable_session_lock(
                lock::Config {
                    instance: lock::InstanceId([3; 16]),
                    session: lock::SessionId(session),
                    jump_threshold: Duration::from_secs(1),
                },
                now,
                socket,
            )
            .unwrap();
        compositor.dispatch_at(now).unwrap();
        (compositor, peer)
    }

    fn queue_grant(compositor: &mut Compositor, peer: &mut UnixStream) {
        let now = Clocks::exact(BOOT, BOOT);
        assert!(compositor.end_sleep_hold(now, compositor.sleep_hold_token().unwrap()));
        let (ui, _) = compositor.pending_lock_ui().unwrap();
        compositor.open_lock_attempt(now, ui);
        write_message(peer, Message::Verdict(verdict()));
    }

    #[test]
    fn req_lock_026_bridge_retries_a_report_deferred_by_ambiguous_clocks() {
        let (mut compositor, _peer) = covered();
        let token = compositor.sleep_hold_token().unwrap();
        let later = BOOT + Duration::from_secs(2);
        let ambiguous = Clocks::bracketed(BOOT, BOOT, later);
        assert!(!compositor.end_sleep_hold(ambiguous, token));
        assert_eq!(compositor.sleep_hold_token(), Some(token));
        assert!(compositor.end_sleep_hold(Clocks::exact(later, later), token));
    }

    #[test]
    fn req_lock_025_resamples_after_receiving_the_verdict() {
        let (mut compositor, mut peer) = covered();
        queue_grant(&mut compositor, &mut peer);
        let now = BOOT + Duration::from_secs(1);
        let resumed = Clocks::exact(now, now + Duration::from_secs(60));
        let mut samples = [Clocks::exact(now, now)].into_iter();
        compositor
            .dispatch_with_clock(now, &mut || samples.next().unwrap_or(resumed))
            .unwrap();
        assert_ne!(compositor.session_phase(), Some(lock::Phase::Unlocked));
    }

    #[test]
    fn req_lock_025_resamples_before_submitting_the_session_frame() {
        let (mut compositor, mut peer) = covered();
        queue_grant(&mut compositor, &mut peer);
        let now = BOOT + Duration::from_secs(1);
        compositor.dispatch_at(Clocks::exact(now, now)).unwrap();
        assert_eq!(compositor.session_phase(), Some(lock::Phase::Unlocked));

        let now = now + Duration::from_secs(1);
        let resumed = Clocks::exact(now, now + Duration::from_secs(60));
        let mut samples = [Clocks::exact(now, now); 2].into_iter();
        compositor.state.queue_redraw();
        let flips = compositor.state.flips;
        compositor
            .dispatch_with_clock(now, &mut || samples.next().unwrap_or(resumed))
            .unwrap();
        assert_eq!(compositor.state.flips, flips);
        assert_eq!(compositor.session_phase(), Some(lock::Phase::Locking));
    }

    fn verdict() -> Verdict {
        Verdict {
            compositor_instance: CompositorInstance([3; 16]),
            session: SessionId::new(b"c2").unwrap(),
            epoch: LockEpoch(1),
            attempt: Attempt(1),
            outcome: skorupa_protocol::Outcome::Granted,
        }
    }

    fn write_message(peer: &mut UnixStream, message: Message) {
        let mut frame = [0; MAX_FRAME_LEN];
        let len = message.encode(&mut frame).unwrap();
        peer.write_all(&frame[..len]).unwrap();
    }

    #[test]
    fn req_auth_001_split_verdict_is_decoded_only_when_complete() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let mut authd = Authd::new(socket).unwrap();
        let mut frame = [0; MAX_FRAME_LEN];
        let verdict = verdict();
        let len = Message::Verdict(verdict).encode(&mut frame).unwrap();

        for byte in &frame[..len - 1] {
            peer.write_all(std::slice::from_ref(byte)).unwrap();
            assert_eq!(authd.receive().unwrap(), None);
        }
        peer.write_all(&frame[len - 1..len]).unwrap();
        assert_eq!(authd.receive().unwrap(), Some(verdict));
    }

    #[test]
    fn req_auth_017_queued_verdict_is_discarded_if_broker_has_closed() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let mut authd = Authd::new(socket).unwrap();
        write_message(&mut peer, Message::Verdict(verdict()));
        drop(peer);

        assert_eq!(
            authd.receive().unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn req_auth_017_half_close_retires_verdicts_behind_queued_data() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let mut authd = Authd::new(socket).unwrap();
        write_message(&mut peer, Message::Verdict(verdict()));
        write_message(&mut peer, Message::Verdict(verdict()));
        peer.shutdown(Shutdown::Write).unwrap();

        assert_eq!(
            authd.receive().unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn req_auth_002_authd_cannot_send_a_challenge() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let mut authd = Authd::new(socket).unwrap();
        let Verdict {
            compositor_instance,
            session,
            epoch,
            attempt,
            ..
        } = verdict();
        write_message(
            &mut peer,
            Message::Challenge(Challenge {
                compositor_instance,
                session,
                epoch,
                attempt,
            }),
        );
        assert_eq!(authd.receive().unwrap_err().kind(), ErrorKind::InvalidData);
    }
}
