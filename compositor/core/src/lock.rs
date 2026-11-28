//! Session lock: epochs, attempts and the unlock verdict.
//!
//! [`step`] is the compositor's part of the contract modelled in
//! `spec/lock.qnt`. The session is [`Phase::Unlocked`], [`Phase::Locking`]
//! (requested, no lock frame on the panel yet) or [`Phase::Locked`]
//! (acknowledged). Two rules carry the design:
//!
//! - **Property A.** The only way out of `Locked` is a granted verdict from
//!   `authd` that names this compositor instance, this session, the current
//!   epoch and the attempt that is open, and it is used once.
//! - **Property B.** The lock is acknowledged for two causes only: the flip of
//!   a frame that was tagged as a lock frame when it was submitted, or an
//!   output that is confirmed off.
//!
//! Every event carries both clocks ([`Clocks`]). A boot-time clock that ran
//! ahead of the monotonic one means the machine slept, and that is handled
//! before the event itself. A new instance starts in `Locking`: there is no
//! way to construct an unlocked state.
//!
//! What the renderer may draw follows [`State::phase`] alone: outside
//! `Unlocked` every frame is a lock frame. The adapter steps the reducer with
//! fresh clocks before it draws.
//!
//! # Sleep hold
//!
//! REQ-LOCK-026: an instance starts held and enters a fresh hold on receipt
//! of a sleep request. A current bridge report or a clock jump ends the hold.
//! Only a fresh lock presentation can make authentication available after
//! waking or returning to an enabled output.

use core::time::Duration;

/// How long `authd` has to answer a challenge before the attempt is void.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Delay before `lock/ui` is restarted after its first failure. It doubles
/// with each further failure. Open decision O-6 may change both limits.
pub const UI_BACKOFF_BASE: Duration = Duration::from_millis(250);

/// Longest delay between two starts of `lock/ui`. The restarts never stop,
/// so that unlock works again once a UI survives.
pub const UI_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// Identity of one compositor process: a random value chosen at start.
/// Epoch and attempt numbers restart with the process, so this is the only
/// field that tells an old verdict from a new attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(pub [u8; 16]);

/// The instance and generation of one sleep hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HoldToken {
    pub instance: InstanceId,
    pub generation: u64,
}

/// The login session, as the bytes of its identifier padded with zeros.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(pub [u8; 32]);

/// The two clock readings every event is stamped with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clocks {
    /// `CLOCK_MONOTONIC`: stands still while the machine sleeps.
    pub monotonic: Duration,
    /// `CLOCK_BOOTTIME`: keeps running while the machine sleeps.
    pub boottime: Duration,
    boottime_before: Option<Duration>,
}

impl Clocks {
    /// Simultaneous readings, for simulated time.
    #[must_use]
    pub const fn exact(monotonic: Duration, boottime: Duration) -> Self {
        Self {
            monotonic,
            boottime,
            boottime_before: None,
        }
    }

    /// A monotonic reading between two boot-time readings.
    #[must_use]
    pub const fn bracketed(before: Duration, monotonic: Duration, after: Duration) -> Self {
        Self {
            monotonic,
            boottime: after,
            boottime_before: Some(before),
        }
    }

    const fn sleep_bounds(self) -> (Duration, Duration) {
        let most = self.boottime.saturating_sub(self.monotonic);
        let least = match self.boottime_before {
            Some(before) => before.saturating_sub(self.monotonic),
            None => most,
        };
        (least, most)
    }
}

/// Detects a sleep from the two clocks. Each reducer keeps its own, so that
/// none depends on another one having seen the event first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepWatch {
    /// Possible offsets when the last sleep was seen.
    asleep: (Duration, Duration),
    threshold: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClockChange {
    Stable,
    Ambiguous,
    Resumed,
}

impl SleepWatch {
    /// Starts watching at `now`. A sleep is a change of more than `threshold`.
    #[must_use]
    pub const fn new(now: Clocks, threshold: Duration) -> Self {
        Self {
            asleep: now.sleep_bounds(),
            threshold,
        }
    }

    /// Whether the machine slept since the last sleep this watch reported.
    ///
    /// Sleeps below the threshold add up until they pass it. A difference
    /// that shrinks cannot come from real clocks and counts as a sleep too.
    pub fn slept(&mut self, now: Clocks) -> bool {
        self.observe(now) == ClockChange::Resumed
    }

    fn observe(&mut self, now: Clocks) -> ClockChange {
        let (least, most) = now.sleep_bounds();
        let (old_least, old_most) = self.asleep;
        if least.saturating_sub(old_most) > self.threshold
            || old_least.saturating_sub(most) > self.threshold
        {
            self.asleep = (least, most);
            return ClockChange::Resumed;
        }
        // Refine overlapping samples. Keep disjoint offsets below the
        // threshold relative to the same baseline so short sleeps add up.
        let intersection = (least.max(old_least), most.min(old_most));
        if intersection.0 <= intersection.1 {
            self.asleep = intersection;
        }
        if most.saturating_sub(old_least) > self.threshold
            || old_most.saturating_sub(least) > self.threshold
        {
            ClockChange::Ambiguous
        } else {
            ClockChange::Stable
        }
    }
}

/// What a new compositor instance is told once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Random identity of this process.
    pub instance: InstanceId,
    /// The session this compositor runs.
    pub session: SessionId,
    /// How far the boot-time clock must run ahead to count as a sleep.
    pub jump_threshold: Duration,
}

/// Lock state of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Clients are drawn and get input.
    Unlocked,
    /// A lock was requested. Input and scene are switched; no lock frame is
    /// known to be on the panel yet.
    Locking,
    /// The lock is acknowledged.
    Locked,
}

/// What a verdict must name to be accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    /// Compositor process.
    pub instance: InstanceId,
    /// Login session.
    pub session: SessionId,
    /// Number of the lock.
    pub epoch: u64,
    /// Number of the attempt within the epoch.
    pub attempt: u32,
}

/// What `authd` decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The user authenticated.
    Granted,
    /// The attempt failed.
    Denied,
}

/// What a frame shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    /// Anything of the unlocked session.
    Session,
    /// The compositor's cover, alone or below the surface of `lock/ui`.
    Protected,
}

/// Who gets keyboard, pointer and touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// The clients of the session.
    Session,
    /// No client. The compositor keeps its own keys.
    Nobody,
    /// The connected `lock/ui` process only. Its surface is drawn above the
    /// cover exactly while it has the input.
    LockUi,
}

/// The part of the lock state that grants depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockView {
    /// Whether the session is unlocked.
    pub unlocked: bool,
    /// Number of the current or last lock.
    pub epoch: u64,
}

/// One event with the clocks read when it was taken from the event loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    /// Both clocks, read before the event is handled.
    pub now: Clocks,
    /// What happened.
    pub event: Event,
}

/// Everything the lock reducer reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing but the passing of time: a timer fired, or a frame is due.
    Tick,
    /// logind `Lock`, the lock shortcut or the idle timer.
    LockRequested,
    /// The bridge relays `PrepareForSleep(true)` with a nonce it minted.
    PrepareForSleep {
        /// Echoed in [`Effect::ReleaseSleepInhibitor`].
        nonce: u64,
    },
    /// A bridge connected on the authenticated bridge channel.
    BridgeConnected,
    /// The bridge holds a delay inhibitor and logind is no longer preparing
    /// for the sleep associated with this token.
    SleepOver { token: HoldToken },
    /// logind's delay ran out before the sleep was released.
    InhibitorTimedOut,
    /// logind `Unlock`. Ignored.
    LogindUnlock,
    /// A frame went to the kernel, with what it shows.
    FrameSubmitted(Content),
    /// The page flip completed, with the tag the frame was submitted under.
    FramePresented(Content),
    /// A frame could not be rendered; nothing was submitted.
    RenderFailed,
    /// The page flip of the submitted frame failed.
    FlipFailed,
    /// The kernel confirmed that the only output is off or gone.
    OutputOff,
    /// An output is about to be driven again. Sent before its first frame is
    /// drawn, so that the frame follows the phase this event leaves.
    OutputOn,
    /// The `lock/ui` process with this serial has a surface.
    UiConnected {
        /// From [`Effect::StartUi`].
        ui: u32,
    },
    /// The `lock/ui` process with this serial exited on its own.
    UiExited {
        /// From [`Effect::StartUi`].
        ui: u32,
    },
    /// `lock/ui` has a secret to submit and asks for a challenge.
    AttemptOpened {
        /// From [`Effect::StartUi`].
        ui: u32,
    },
    /// A verdict arrived on the `authd` connection.
    Verdict {
        /// What the verdict names.
        binding: Binding,
        /// The decision.
        outcome: Outcome,
    },
    /// The `authd` connection closed.
    AuthdDisconnected,
}

/// What the adapter must do, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Route input to this target from now on.
    RouteInput(InputMode),
    /// Compose the cover alone: no client surface, no `lock/ui` surface.
    ShowFallback,
    /// A new frame is needed although nothing else may ask for one.
    ScheduleFrame,
    /// End every grant and consent prompt.
    RevokeAllGrants,
    /// The lock of this epoch is in force: tell logind and the shell.
    LockAcknowledged {
        /// The lock that is now acknowledged.
        epoch: u64,
    },
    /// Send the current hold token to the bridge.
    OfferSleepHold(HoldToken),
    /// Answer the bridge's sleep request so that it releases the inhibitor.
    ReleaseSleepInhibitor {
        /// Nonce of the request this answers.
        nonce: u64,
        token: HoldToken,
    },
    /// Start a new `lock/ui` process with a new credential channel.
    StartUi {
        /// Serial that this process's events must carry.
        ui: u32,
        /// How long to wait first.
        delay: Duration,
    },
    /// Kill this `lock/ui` process, or cancel its start.
    StopUi {
        /// The process to end.
        ui: u32,
    },
    /// Tell `authd` which attempt it may judge and on which channel.
    SendChallenge {
        /// What the verdict must name.
        binding: Binding,
        /// The `lock/ui` process whose channel carries the secret.
        ui: u32,
        /// Monotonic time at which the attempt is void: arm a timer.
        deadline: Duration,
    },
    /// The session is unlocked: draw the clients again.
    Unlock {
        /// The lock that ended.
        epoch: u64,
    },
}

/// The frame in flight, as far as an acknowledgement may rely on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flight {
    Idle,
    /// One lock frame, submitted since the last sleep.
    Protected,
    /// A session frame, a frame from before a sleep, or more than one frame.
    Unverified,
}

/// The `lock/ui` process. It runs exactly while an attempt could be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ui {
    Stopped,
    Starting,
    Connected,
    /// An attempt is open until this monotonic time.
    Checking {
        deadline: Duration,
    },
}

/// Lock state of one compositor instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    config: Config,
    watch: SleepWatch,
    phase: Phase,
    epoch: u64,
    attempt: u32,
    hold: bool,
    hold_generation: u64,
    /// Nonce of a sleep request that waits for `Locked`.
    sleep_request: Option<u64>,
    /// Nonce answered last, so that a repeated request is answered once.
    answered: Option<u64>,
    output_on: bool,
    flight: Flight,
    ui: Ui,
    ui_serial: u32,
    /// Attempts and UI processes that ended without a verdict, in a row.
    ui_failures: u32,
}

impl State {
    /// The state of a new compositor instance, and what to do first.
    ///
    /// REQ-LOCK-013: the instance knows nothing of an earlier lock, so it
    /// starts covered. The output counts as on until [`Event::OutputOff`].
    #[must_use]
    pub fn start(config: Config, now: Clocks) -> (Self, Vec<Effect>) {
        let state = Self {
            config,
            watch: SleepWatch::new(now, config.jump_threshold),
            phase: Phase::Locking,
            epoch: 1,
            attempt: 0,
            hold: true,
            hold_generation: 0,
            sleep_request: None,
            answered: None,
            output_on: true,
            flight: Flight::Idle,
            ui: Ui::Stopped,
            ui_serial: 0,
            ui_failures: 0,
        };
        let mut effects = vec![
            Effect::RouteInput(InputMode::Nobody),
            Effect::ShowFallback,
            Effect::ScheduleFrame,
        ];
        state.offer_sleep_hold(&mut effects);
        (state, effects)
    }

    /// The current phase. Outside `Unlocked` every frame is a lock frame.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// What the grant reducer needs to know of the lock.
    #[must_use]
    pub const fn view(&self) -> LockView {
        LockView {
            unlocked: matches!(self.phase, Phase::Unlocked),
            epoch: self.epoch,
        }
    }

    const fn binding(&self) -> Binding {
        Binding {
            instance: self.config.instance,
            session: self.config.session,
            epoch: self.epoch,
            attempt: self.attempt,
        }
    }

    /// The active token, if one can still be issued without reuse.
    #[must_use]
    pub const fn hold_token(&self) -> Option<HoldToken> {
        if self.hold && self.hold_generation < u64::MAX {
            Some(HoldToken {
                instance: self.config.instance,
                generation: self.hold_generation,
            })
        } else {
            None
        }
    }

    fn offer_sleep_hold(&self, effects: &mut Vec<Effect>) {
        if let Some(token) = self.hold_token() {
            effects.push(Effect::OfferSleepHold(token));
        }
    }

    fn begin_sleep_hold(&mut self, effects: &mut Vec<Effect>) {
        self.hold = true;
        self.hold_generation = self.hold_generation.saturating_add(1);
        self.sync_ui(effects);
        self.offer_sleep_hold(effects);
    }

    /// REQ-LOCK-007, REQ-LOCK-003, REQ-LOCK-021: a lock is a new epoch, takes
    /// the input from the clients and ends every grant, in one step.
    fn lock(&mut self, effects: &mut Vec<Effect>, acknowledge_dark: bool) {
        if self.phase != Phase::Unlocked {
            return;
        }
        self.phase = Phase::Locking;
        self.epoch = self.epoch.saturating_add(1);
        self.attempt = 0;
        effects.extend([
            Effect::RouteInput(InputMode::Nobody),
            Effect::RevokeAllGrants,
            Effect::ShowFallback,
            Effect::ScheduleFrame,
        ]);
        // REQ-LOCK-010: nothing is visible, so no flip has to be awaited.
        if acknowledge_dark && !self.output_on {
            self.acknowledge(effects);
        }
    }

    /// The single way into `Locked`. Callers are the two causes of REQ-LOCK-002.
    fn acknowledge(&mut self, effects: &mut Vec<Effect>) {
        self.phase = Phase::Locked;
        effects.push(Effect::LockAcknowledged { epoch: self.epoch });
        self.answer_sleep_request(effects);
        self.sync_ui(effects);
    }

    /// REQ-LOCK-024: a sleep request is answered only while `Locked`, with
    /// its own nonce.
    fn answer_sleep_request(&mut self, effects: &mut Vec<Effect>) {
        if self.phase == Phase::Locked
            && let Some(nonce) = self.sleep_request.take()
            && let Some(token) = self.hold_token()
        {
            self.answered = Some(nonce);
            effects.push(Effect::ReleaseSleepInhibitor { nonce, token });
        }
    }

    /// The clock sample shows a resume or cannot rule one out.
    ///
    /// REQ-LOCK-015, REQ-LOCK-016, REQ-AUTH-016, REQ-GRANT-006: the session
    /// locks, the open attempt and every grant are void, and a flip from
    /// before the sleep proves nothing.
    fn require_fresh_cover(&mut self, effects: &mut Vec<Effect>) {
        if self.flight != Flight::Idle {
            self.flight = Flight::Unverified;
        }
        if self.phase == Phase::Unlocked {
            self.lock(effects, false);
        } else {
            self.phase = Phase::Locking;
            self.sync_ui(effects);
            effects.extend([Effect::RevokeAllGrants, Effect::ScheduleFrame]);
        }
    }

    /// Ends the current `lock/ui` process and the attempt it may have open.
    /// `kill` is false when the process is already gone.
    fn drop_ui(&mut self, kill: bool, effects: &mut Vec<Effect>) {
        match self.ui {
            Ui::Stopped => return,
            Ui::Starting => {}
            // REQ-TRUST-010: the input stays in lock mode with the cover up.
            Ui::Connected | Ui::Checking { .. } => {
                effects.extend([Effect::ShowFallback, Effect::RouteInput(InputMode::Nobody)]);
            }
        }
        if kill {
            effects.push(Effect::StopUi { ui: self.ui_serial });
        }
        self.ui = Ui::Stopped;
    }

    /// Runs `lock/ui` exactly while an attempt could be opened: `Locked` and
    /// not in the sleep hold. REQ-TRUST-002: every start is a new process.
    fn sync_ui(&mut self, effects: &mut Vec<Effect>) {
        if self.phase != Phase::Locked || self.hold || !self.output_on {
            self.drop_ui(true, effects);
        } else if self.ui == Ui::Stopped {
            // Serials only tell a process from the ones around it.
            self.ui_serial = self.ui_serial.wrapping_add(1);
            self.ui = Ui::Starting;
            let doublings = self.ui_failures.saturating_sub(1);
            let delay = if self.ui_failures == 0 {
                Duration::ZERO
            } else {
                let factor = 1_u32.checked_shl(doublings).unwrap_or(u32::MAX);
                UI_BACKOFF_BASE.saturating_mul(factor).min(UI_BACKOFF_MAX)
            };
            effects.push(Effect::StartUi {
                ui: self.ui_serial,
                delay,
            });
        }
    }

    /// REQ-LOCK-014, REQ-AUTH-004: the UI or its attempt ended without a
    /// verdict. The attempt is consumed and the next start waits longer.
    fn fail_ui(&mut self, kill: bool, effects: &mut Vec<Effect>) {
        self.ui_failures = self.ui_failures.saturating_add(1);
        self.drop_ui(kill, effects);
        self.sync_ui(effects);
    }

    fn redraw(&self, effects: &mut Vec<Effect>) {
        if self.phase != Phase::Unlocked {
            effects.push(Effect::ScheduleFrame);
        }
    }

    fn handle(&mut self, input: Input, effects: &mut Vec<Effect>) {
        match input.event {
            // REQ-LOCK-017: `loginctl unlock-session` must not end a lock.
            Event::Tick | Event::LogindUnlock => {}
            // After the timeout the machine sleeps whatever happens here; a
            // pending request is kept so that the hold still begins.
            Event::LockRequested | Event::InhibitorTimedOut => self.lock(effects, true),
            Event::PrepareForSleep { nonce } => {
                if self.answered != Some(nonce) && self.sleep_request != Some(nonce) {
                    self.sleep_request = Some(nonce);
                    self.begin_sleep_hold(effects);
                    self.lock(effects, true);
                    self.answer_sleep_request(effects);
                }
            }
            Event::BridgeConnected => self.offer_sleep_hold(effects),
            Event::SleepOver { token } => {
                if self.hold_token() == Some(token) {
                    self.hold = false;
                    self.sleep_request = None;
                    self.sync_ui(effects);
                }
            }
            Event::FrameSubmitted(content) => {
                let alone = self.flight == Flight::Idle && content == Content::Protected;
                self.flight = if alone {
                    Flight::Protected
                } else {
                    Flight::Unverified
                };
            }
            Event::FramePresented(content) => {
                // REQ-LOCK-002: the tag recorded at submission decides.
                let covered = self.flight == Flight::Protected && content == Content::Protected;
                self.flight = Flight::Idle;
                if !covered {
                    self.redraw(effects);
                } else if self.phase == Phase::Locking {
                    self.acknowledge(effects);
                }
            }
            // REQ-LOCK-011: a failure never cancels the lock. Pacing of the
            // retries belongs to the frame clock.
            Event::RenderFailed => self.redraw(effects),
            Event::FlipFailed => {
                self.flight = Flight::Idle;
                self.redraw(effects);
            }
            Event::OutputOff => {
                self.output_on = false;
                self.flight = Flight::Idle;
                if self.phase == Phase::Locking {
                    self.acknowledge(effects);
                } else {
                    self.sync_ui(effects);
                }
            }
            Event::OutputOn => {
                if !self.output_on && self.phase == Phase::Locked {
                    self.phase = Phase::Locking;
                    self.flight = Flight::Idle;
                }
                self.output_on = true;
                self.redraw(effects);
            }
            Event::UiConnected { ui } => {
                if ui == self.ui_serial && self.ui == Ui::Starting {
                    self.ui = Ui::Connected;
                    effects.push(Effect::RouteInput(InputMode::LockUi));
                }
            }
            Event::UiExited { ui } => {
                if ui == self.ui_serial && self.ui != Ui::Stopped {
                    self.fail_ui(false, effects);
                }
            }
            Event::AttemptOpened { ui } => {
                // REQ-AUTH-018: one attempt per process, so per channel.
                if ui == self.ui_serial
                    && self.ui == Ui::Connected
                    && let Some(attempt) = self.attempt.checked_add(1)
                {
                    self.attempt = attempt;
                    let deadline = input.now.monotonic.saturating_add(ATTEMPT_TIMEOUT);
                    self.ui = Ui::Checking { deadline };
                    let binding = self.binding();
                    effects.push(Effect::SendChallenge {
                        binding,
                        ui,
                        deadline,
                    });
                }
            }
            Event::Verdict { binding, outcome } => self.judge(binding, outcome, effects),
            // REQ-AUTH-017: nothing the old connection issued counts.
            Event::AuthdDisconnected => {
                if matches!(self.ui, Ui::Checking { .. }) {
                    self.fail_ui(true, effects);
                }
            }
        }
    }

    /// REQ-AUTH-001: a verdict counts only for the open attempt of this
    /// instance, session and epoch, and consumes it either way. Anything else
    /// is dropped and consumes nothing.
    fn judge(&mut self, binding: Binding, outcome: Outcome, effects: &mut Vec<Effect>) {
        // The last epoch can never be followed by a new one, so it must not end.
        let open = self.phase == Phase::Locked
            && self.output_on
            && !self.hold
            && matches!(self.ui, Ui::Checking { .. })
            && self.epoch < u64::MAX;
        if !open || binding != self.binding() {
            return;
        }
        self.ui_failures = 0;
        match outcome {
            Outcome::Granted => {
                self.ui = Ui::Stopped;
                self.phase = Phase::Unlocked;
                effects.extend([
                    Effect::StopUi { ui: self.ui_serial },
                    Effect::Unlock { epoch: self.epoch },
                    Effect::RouteInput(InputMode::Session),
                ]);
            }
            Outcome::Denied => {
                self.drop_ui(true, effects);
                self.sync_ui(effects);
            }
        }
    }
}

/// Handles one event.
///
/// REQ-LOCK-025: the clocks are compared before the event is looked at, so a
/// verdict or a flip that arrives first after a resume meets a state that
/// already knows of the sleep.
#[must_use]
pub fn step(mut state: State, input: Input) -> (State, Vec<Effect>) {
    let before = state;
    let mut effects = Vec::new();
    let change = state.watch.observe(input.now);
    if change == ClockChange::Resumed {
        state.hold = false;
        state.sleep_request = None;
    }
    if change != ClockChange::Stable {
        state.require_fresh_cover(&mut effects);
    }
    if let Ui::Checking { deadline } = state.ui
        && input.now.monotonic >= deadline
    {
        state.fail_ui(true, &mut effects);
    }
    // An ambiguous sample may hide a resume. Keep the hold and require a
    // trustworthy reading before processing authentication or bridge release.
    if change != ClockChange::Ambiguous
        || !matches!(
            input.event,
            Event::SleepOver { .. }
                | Event::UiConnected { .. }
                | Event::AttemptOpened { .. }
                | Event::Verdict { .. }
        )
    {
        state.handle(input, &mut effects);
    }
    debug_assert_eq!(audit(&before, input, &state, &effects), Ok(()));
    (state, effects)
}

/// Properties A and B and the state invariants as one check of a transition.
/// It restates the rules instead of reusing them; debug builds run it on
/// every step.
fn audit(
    before: &State,
    input: Input,
    after: &State,
    effects: &[Effect],
) -> Result<(), &'static str> {
    let mut watch = before.watch;
    let stable = watch.observe(input.now) == ClockChange::Stable;
    let has = |wanted: fn(&Effect) -> bool| effects.iter().any(wanted);

    let unlocked = before.phase != Phase::Unlocked && after.phase == Phase::Unlocked;
    let by_verdict = matches!(
        (before.ui, input.event),
        (Ui::Checking { deadline }, Event::Verdict { binding, outcome: Outcome::Granted })
            if binding == before.binding() && input.now.monotonic < deadline
    );
    let may_unlock = by_verdict && before.phase == Phase::Locked && !before.hold && stable;
    if unlocked != has(|effect| matches!(effect, Effect::Unlock { .. }))
        || (unlocked && !may_unlock)
    {
        return Err("A: unlocked without a verdict for the open attempt");
    }

    let reacknowledged = !stable && input.event == Event::OutputOff;
    let locked = (before.phase != Phase::Locked || reacknowledged) && after.phase == Phase::Locked;
    let shown = before.phase == Phase::Locking
        && before.flight == Flight::Protected
        && input.event == Event::FramePresented(Content::Protected)
        && stable;
    let dark = !before.output_on || input.event == Event::OutputOff;
    if locked != has(|effect| matches!(effect, Effect::LockAcknowledged { .. }))
        || (locked && !shown && !dark)
    {
        return Err("B: acknowledged without a presented lock frame or an output that is off");
    }

    let relocked = before.phase == Phase::Unlocked && after.phase != Phase::Unlocked;
    let fresh = after.epoch > before.epoch;
    if after.epoch < before.epoch || (relocked != fresh && before.epoch != u64::MAX) {
        return Err("epoch reused or skipped");
    }
    if after.ui != Ui::Stopped && (after.phase != Phase::Locked || after.hold || !after.output_on) {
        return Err("lock/ui runs while no attempt may open");
    }
    if after.sleep_request.is_some() && after.phase != Phase::Locking {
        return Err("sleep request left unanswered");
    }
    if has(|effect| matches!(effect, Effect::ReleaseSleepInhibitor { .. }))
        && after.phase != Phase::Locked
    {
        return Err("sleep released while not locked");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REQ-LOCK-007: an epoch that cannot be followed by a new one is never
    /// left, so no epoch is used for two locks.
    #[test]
    fn req_lock_007_last_epoch_never_unlocks() {
        let now = Clocks::exact(Duration::ZERO, Duration::ZERO);
        let config = Config {
            instance: InstanceId([1; 16]),
            session: SessionId([2; 32]),
            jump_threshold: Duration::from_secs(1),
        };
        let mut state = State::start(config, now).0;
        state.epoch = u64::MAX;
        state.phase = Phase::Locked;
        state.hold = false;
        state.ui = Ui::Connected;
        let send = |state, event| step(state, Input { now, event }).0;
        let state = send(state, Event::AttemptOpened { ui: 0 });
        let binding = state.binding();
        let outcome = Outcome::Granted;
        let state = send(state, Event::Verdict { binding, outcome });
        assert_eq!(state.phase, Phase::Locked);
    }
}
