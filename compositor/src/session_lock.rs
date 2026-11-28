//! Adapts the session lock reducer to the compositor and to the private authd channel.

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

/// Reads the two clocks used to check for a resume before a lock event.
#[must_use]
pub fn read_lock_clocks() -> Clocks {
    Clocks {
        monotonic: Clock::<Monotonic>::new().now().into(),
        boottime: Clock::<BootTime>::new().now().into(),
    }
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
        // Convert once at startup. The wire and reducer must compare exactly
        // the same session, including the reducer's zero padding.
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
        if let Some(session) = &mut self.session_lock {
            session.now = now;
            self.lock_event(Event::Tick);
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
                Effect::ScheduleFrame | Effect::Unlock { .. } => self.queue_redraw(),
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

    pub(crate) fn receive_verdict(&mut self) {
        let Some(authd) = self
            .session_lock
            .as_mut()
            .and_then(|session| session.authd.as_mut())
        else {
            return;
        };
        match authd.receive() {
            Ok(Some(verdict)) => self.lock_event(verdict_event(verdict)),
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
        // One challenge can be outstanding. A short or blocked write retires
        // the channel instead of leaving a partial message or a write queue.
        if stream.write(&buffer[..len])? != len {
            return Err(Error::new(
                ErrorKind::WriteZero,
                "incomplete authd challenge",
            ));
        }
        Ok(())
    }

    fn receive(&mut self) -> io::Result<Option<Verdict>> {
        // At most two reads: one for a missing prefix and one for a body.
        for _ in 0..2 {
            match decode(&self.buffer[..self.received]) {
                Ok((message, _)) => return self.finish(message).map(Some),
                Err(DecodeError::Truncated { needed, .. }) => {
                    match self.stream.read(&mut self.buffer[self.received..needed]) {
                        Ok(0) => return Err(authd_disconnected()),
                        Ok(read) => self.received += read,
                        Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(None),
                        Err(err) => return Err(err),
                    }
                }
                Err(err) => return Err(Error::new(ErrorKind::InvalidData, err)),
            }
        }

        match decode(&self.buffer[..self.received]) {
            Ok((message, _)) => self.finish(message).map(Some),
            Err(DecodeError::Truncated { .. }) => Ok(None),
            Err(err) => Err(Error::new(ErrorKind::InvalidData, err)),
        }
    }

    fn finish(&mut self, message: Message) -> io::Result<Verdict> {
        let Message::Verdict(verdict) = message else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "authd sent the wrong message",
            ));
        };
        // A timed-out attempt's late reply may precede a current reply. Keep
        // those frames separate and check for a half-close even behind them.
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
