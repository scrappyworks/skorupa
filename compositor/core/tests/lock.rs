//! The lock reducer through its public API: one test per requirement, then
//! properties over arbitrary event sequences.
//!
//! Property tests run from a fixed seed so that the merge gate gives the same
//! answer on every run. `PROPTEST_RNG_SEED` and `PROPTEST_CASES` override the
//! seed and the case count for a local hunt.

#![expect(
    clippy::unwrap_used,
    reason = "clippy.toml exempts test functions only; the helpers here are test code too"
)]

use core::time::Duration;
use std::collections::HashSet;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use skorupa_compositor_core::lock::{
    ATTEMPT_TIMEOUT, Binding, Clocks, Config, Content, Effect, Event, Input, InputMode, InstanceId,
    Outcome, Phase, SessionId, SleepWatch, State, UI_BACKOFF_BASE, UI_BACKOFF_MAX, step,
};

const CONFIG: Config = Config {
    instance: InstanceId([7; 16]),
    session: SessionId([b'c'; 32]),
    jump_threshold: Duration::from_secs(1),
};
const BOOT: Clocks = Clocks::exact(Duration::from_secs(100), Duration::from_secs(100));
/// What the first attempt of the first lock is bound to.
const FIRST: Binding = Binding {
    instance: CONFIG.instance,
    session: CONFIG.session,
    epoch: 1,
    attempt: 1,
};
const TICK: Duration = Duration::from_millis(1);
const NAP: Duration = Duration::from_secs(3600);

/// One compositor instance and the two clocks its events are stamped with.
struct Rig {
    state: State,
    now: Clocks,
    /// Serial of the `lock/ui` process the reducer started last.
    ui: u32,
}

impl Rig {
    /// A new instance: `Locking`, and held until the bridge reports.
    fn start() -> Self {
        let state = State::start(CONFIG, BOOT).0;
        Self {
            state,
            now: BOOT,
            ui: 0,
        }
    }

    fn send(&mut self, event: Event) -> Vec<Effect> {
        self.now.monotonic += TICK;
        self.now.boottime += TICK;
        let (state, effects) = step(
            self.state,
            Input {
                now: self.now,
                event,
            },
        );
        self.state = state;
        for effect in &effects {
            if let Effect::StartUi { ui, .. } = effect {
                self.ui = *ui;
            }
        }
        effects
    }

    fn bridge_report(&mut self) -> Vec<Effect> {
        self.send(self.bridge_report_event())
    }

    fn bridge_report_event(&self) -> Event {
        self.state
            .hold_token()
            .map_or(Event::Tick, |token| Event::SleepOver { token })
    }

    /// The machine sleeps: only the boot-time clock moves.
    fn sleep(&mut self) {
        self.now.boottime += NAP;
    }

    fn present(&mut self, content: Content) -> Vec<Effect> {
        self.send(Event::FrameSubmitted(content));
        self.send(Event::FramePresented(content))
    }

    /// `Locked`, out of the hold, with a `lock/ui` process starting.
    fn locked() -> Self {
        let mut rig = Self::start();
        rig.present(Content::Protected);
        rig.bridge_report();
        assert_eq!(rig.state.phase(), Phase::Locked);
        rig
    }

    /// The current UI connects and opens an attempt; returns its binding.
    fn open_attempt(&mut self) -> Binding {
        self.send(Event::UiConnected { ui: self.ui });
        let effects = self.send(Event::AttemptOpened { ui: self.ui });
        challenge(&effects).unwrap()
    }

    fn verdict(&mut self, binding: Binding, outcome: Outcome) -> Vec<Effect> {
        self.send(Event::Verdict { binding, outcome })
    }

    fn unlocked() -> Self {
        let mut rig = Self::locked();
        let binding = rig.open_attempt();
        rig.verdict(binding, Outcome::Granted);
        assert_eq!(rig.state.phase(), Phase::Unlocked);
        rig
    }

    /// From `Unlocked` to an acknowledged lock with a UI starting.
    fn relock(&mut self) {
        self.send(Event::LockRequested);
        self.present(Content::Protected);
        assert_eq!(self.state.phase(), Phase::Locked);
    }
}

fn challenge(effects: &[Effect]) -> Option<Binding> {
    effects.iter().find_map(|effect| match effect {
        Effect::SendChallenge { binding, .. } => Some(*binding),
        _ => None,
    })
}

fn acknowledged(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, Effect::LockAcknowledged { .. }))
}

fn restart_delay(effects: &[Effect]) -> Option<Duration> {
    effects.iter().find_map(|effect| match effect {
        Effect::StartUi { delay, .. } => Some(*delay),
        _ => None,
    })
}

/// REQ-LOCK-013, REQ-AUTH-015: a new instance has no trustworthy lock state,
/// so it starts covered and with the input away from every client.
#[test]
fn req_lock_013_new_instance_starts_covered() {
    let (state, effects) = State::start(CONFIG, BOOT);
    assert_eq!(state.phase(), Phase::Locking);
    assert_eq!(
        effects,
        [
            Effect::RouteInput(InputMode::Nobody),
            Effect::ShowFallback,
            Effect::ScheduleFrame,
            Effect::OfferSleepHold(state.hold_token().unwrap())
        ]
    );
    let view = state.view();
    assert!(!view.unlocked);
    assert_eq!(view.epoch, 1);
}

#[test]
fn req_lock_002_acknowledges_on_the_presented_lock_frame_only() {
    let mut rig = Rig::start();
    let submitted = rig.send(Event::FrameSubmitted(Content::Protected));
    assert_eq!(submitted, []);
    assert_eq!(rig.state.phase(), Phase::Locking);
    let presented = rig.send(Event::FramePresented(Content::Protected));
    assert!(presented.contains(&Effect::LockAcknowledged { epoch: 1 }));
    assert_eq!(rig.state.phase(), Phase::Locked);
}

#[test]
fn req_lock_002_session_frame_queued_before_the_lock_does_not_acknowledge() {
    let mut rig = Rig::unlocked();
    rig.send(Event::FrameSubmitted(Content::Session));
    rig.send(Event::LockRequested);
    let late = rig.send(Event::FramePresented(Content::Session));
    assert_eq!(late, [Effect::ScheduleFrame]);
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(acknowledged(&rig.present(Content::Protected)));
}

/// REQ-LOCK-002: the tag is the one recorded at submission. A flip that
/// claims a lock frame the reducer never saw submitted proves nothing.
#[test]
fn req_lock_002_flip_tag_must_match_the_submitted_frame() {
    let mut rig = Rig::start();
    assert!(!acknowledged(
        &rig.send(Event::FramePresented(Content::Protected))
    ));
    rig.send(Event::FrameSubmitted(Content::Session));
    assert!(!acknowledged(
        &rig.send(Event::FramePresented(Content::Protected))
    ));
    rig.send(Event::FrameSubmitted(Content::Protected));
    rig.send(Event::FrameSubmitted(Content::Protected));
    assert!(!acknowledged(
        &rig.send(Event::FramePresented(Content::Protected))
    ));
    assert_eq!(rig.state.phase(), Phase::Locking);
}

/// REQ-LOCK-003, REQ-LOCK-021: input leaves the clients and every grant ends
/// in the step that takes the request.
#[test]
fn req_lock_003_lock_request_reroutes_input_and_revokes_in_one_step() {
    let mut rig = Rig::unlocked();
    let effects = rig.send(Event::LockRequested);
    assert_eq!(
        effects,
        [
            Effect::RouteInput(InputMode::Nobody),
            Effect::RevokeAllGrants,
            Effect::ShowFallback,
            Effect::ScheduleFrame
        ]
    );
}

#[test]
fn req_lock_004_every_source_locks_and_nothing_cancels() {
    let sources = [
        Event::LockRequested,
        Event::PrepareForSleep { nonce: 1 },
        Event::InhibitorTimedOut,
    ];
    for source in sources {
        let mut rig = Rig::unlocked();
        rig.send(source);
        assert_eq!(rig.state.phase(), Phase::Locking, "{source:?}");
        let cancels = [
            Event::LogindUnlock,
            Event::LockRequested,
            Event::InhibitorTimedOut,
            Event::BridgeConnected,
            Event::OutputOn,
            Event::AuthdDisconnected,
            Event::UiExited { ui: rig.ui },
        ];
        for event in cancels {
            rig.send(event);
            assert_eq!(
                rig.state.phase(),
                Phase::Locking,
                "{source:?} then {event:?}"
            );
        }
    }
}

#[test]
fn req_lock_007_every_lock_gets_the_next_epoch() {
    let mut rig = Rig::unlocked();
    let mut epochs = vec![rig.state.view().epoch];
    for _ in 0..3 {
        rig.relock();
        let binding = rig.open_attempt();
        epochs.push(binding.epoch);
        assert_eq!(binding.attempt, 1);
        rig.verdict(binding, Outcome::Granted);
    }
    assert_eq!(epochs, [1, 2, 3, 4]);
}

#[test]
fn req_lock_010_seat_loss_voids_the_open_attempt_until_a_fresh_cover() {
    let mut rig = Rig::locked();
    let old = rig.open_attempt();
    let effects = rig.send(Event::OutputUnavailable);
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(!acknowledged(&effects));
    assert!(effects.contains(&Effect::StopUi { ui: rig.ui }));
    assert!(!acknowledged(&rig.present(Content::Protected)));
    rig.send(Event::OutputOn);
    rig.verdict(old, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(acknowledged(&rig.present(Content::Protected)));
    let current = rig.open_attempt();
    rig.verdict(old, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Locked);
    rig.verdict(current, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

#[test]
fn req_lock_010_output_off_acknowledges_without_a_flip() {
    let mut rig = Rig::unlocked();
    rig.send(Event::LockRequested);
    assert!(acknowledged(&rig.send(Event::OutputOff)));
    assert_eq!(rig.state.phase(), Phase::Locked);
    assert_eq!(rig.send(Event::OutputOn), [Effect::ScheduleFrame]);
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert_eq!(rig.send(Event::AttemptOpened { ui: rig.ui }), []);
    assert!(acknowledged(&rig.present(Content::Protected)));
}

/// REQ-LOCK-010: with the output already off, the request is acknowledged in
/// its own step, after input and scene are switched.
#[test]
fn req_lock_010_lock_with_the_output_off_switches_input_first() {
    let mut rig = Rig::unlocked();
    rig.send(Event::OutputOff);
    let effects = rig.send(Event::LockRequested);
    let position = |wanted: &Effect| effects.iter().position(|effect| effect == wanted).unwrap();
    let epoch = rig.state.view().epoch;
    assert!(
        position(&Effect::RouteInput(InputMode::Nobody))
            < position(&Effect::LockAcknowledged { epoch })
    );
    assert!(position(&Effect::ShowFallback) < position(&Effect::LockAcknowledged { epoch }));
}

#[test]
fn req_lock_011_failed_render_and_flip_keep_the_lock_and_retry() {
    let mut rig = Rig::unlocked();
    rig.send(Event::LockRequested);
    assert_eq!(rig.send(Event::RenderFailed), [Effect::ScheduleFrame]);
    rig.send(Event::FrameSubmitted(Content::Protected));
    assert_eq!(rig.send(Event::FlipFailed), [Effect::ScheduleFrame]);
    assert_eq!(rig.state.phase(), Phase::Locking);
    // The failed flip left nothing in flight for a later event to confirm.
    assert!(!acknowledged(
        &rig.send(Event::FramePresented(Content::Protected))
    ));
    assert!(acknowledged(&rig.present(Content::Protected)));
}

/// REQ-LOCK-014, REQ-TRUST-010: a UI that keeps dying leaves the cover up and
/// the input away from clients, and is restarted ever more slowly up to a cap.
#[test]
fn req_lock_014_ui_crash_loop_stays_covered_with_bounded_restarts() {
    let mut rig = Rig::locked();
    let mut delays = Vec::new();
    for _ in 0..8 {
        let ui = rig.ui;
        assert_eq!(
            rig.send(Event::UiConnected { ui }),
            [Effect::RouteInput(InputMode::LockUi)]
        );
        let effects = rig.send(Event::UiExited { ui });
        assert!(
            effects.starts_with(&[Effect::ShowFallback, Effect::RouteInput(InputMode::Nobody)])
        );
        assert_ne!(rig.ui, ui, "each restart is a new process");
        assert_eq!(rig.state.phase(), Phase::Locked);
        delays.push(restart_delay(&effects).unwrap());
    }
    assert_eq!(delays[0], UI_BACKOFF_BASE);
    assert!(delays.is_sorted());
    assert_eq!(delays.last(), Some(&UI_BACKOFF_MAX));
    // Unlock works again once a UI survives, and that ends the backoff.
    let binding = rig.open_attempt();
    let denied = rig.verdict(binding, Outcome::Denied);
    assert_eq!(restart_delay(&denied), Some(Duration::ZERO));
    let binding = rig.open_attempt();
    rig.verdict(binding, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

/// REQ-LOCK-014: the exit of a process that was already replaced says
/// nothing about its successor.
#[test]
fn req_lock_014_exit_of_a_replaced_ui_is_ignored() {
    let mut rig = Rig::locked();
    let old = rig.ui;
    rig.send(Event::UiExited { ui: old });
    assert_eq!(rig.send(Event::UiExited { ui: old }), []);
    assert_eq!(rig.send(Event::UiConnected { ui: old }), []);
    assert_eq!(rig.send(Event::AttemptOpened { ui: old }), []);
}

#[test]
fn req_lock_015_flip_from_before_the_sleep_does_not_acknowledge() {
    let mut rig = Rig::unlocked();
    rig.send(Event::LockRequested);
    rig.send(Event::FrameSubmitted(Content::Protected));
    rig.sleep();
    let stale = rig.send(Event::FramePresented(Content::Protected));
    assert!(!acknowledged(&stale));
    assert!(stale.contains(&Effect::ScheduleFrame));
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(acknowledged(&rig.present(Content::Protected)));
}

#[test]
fn req_lock_016_clock_jump_locks_before_any_frame() {
    let mut rig = Rig::unlocked();
    rig.sleep();
    let effects = rig.send(Event::FrameSubmitted(Content::Session));
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(effects.contains(&Effect::RevokeAllGrants));
    assert!(effects.contains(&Effect::RouteInput(InputMode::Nobody)));
    assert!(!acknowledged(
        &rig.send(Event::FramePresented(Content::Session))
    ));
}

/// REQ-LOCK-016: naps below the threshold add up instead of being forgotten.
#[test]
fn req_lock_016_short_sleeps_accumulate_to_a_jump() {
    let mut rig = Rig::unlocked();
    let nap = CONFIG.jump_threshold / 2;
    rig.now.boottime += nap;
    rig.send(Event::Tick);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
    rig.now.boottime += nap;
    rig.send(Event::Tick);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
    rig.now.boottime += nap;
    rig.send(Event::Tick);
    assert_eq!(rig.state.phase(), Phase::Locking);
}

#[test]
fn req_lock_017_logind_unlock_is_ignored_in_every_phase() {
    let mut rig = Rig::start();
    assert_eq!(rig.send(Event::LogindUnlock), []);
    assert_eq!(rig.state.phase(), Phase::Locking);
    let mut rig = Rig::locked();
    rig.open_attempt();
    assert_eq!(rig.send(Event::LogindUnlock), []);
    assert_eq!(rig.state.phase(), Phase::Locked);
    let mut rig = Rig::unlocked();
    assert_eq!(rig.send(Event::LogindUnlock), []);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

/// REQ-LOCK-018, REQ-LOCK-024: the sleep is released only once `Locked`, with
/// the nonce of the request, and a repeated request is answered once.
#[test]
fn req_lock_024_sleep_release_echoes_the_nonce_and_waits_for_locked() {
    let mut rig = Rig::unlocked();
    let requested = rig.send(Event::PrepareForSleep { nonce: 41 });
    let token = rig.state.hold_token().unwrap();
    assert!(
        !requested
            .iter()
            .any(|effect| matches!(effect, Effect::ReleaseSleepInhibitor { .. }))
    );
    let presented = rig.present(Content::Protected);
    let epoch = rig.state.view().epoch;
    assert_eq!(
        presented,
        [
            Effect::LockAcknowledged { epoch },
            Effect::ReleaseSleepInhibitor { nonce: 41, token }
        ]
    );
    assert_eq!(rig.send(Event::PrepareForSleep { nonce: 41 }), []);
    let effects = rig.send(Event::PrepareForSleep { nonce: 42 });
    let token = rig.state.hold_token().unwrap();
    assert_eq!(
        effects,
        [
            Effect::OfferSleepHold(token),
            Effect::ReleaseSleepInhibitor { nonce: 42, token }
        ]
    );
}

/// REQ-LOCK-018 and REQ-LOCK-026: after the release, the attempt is closed
/// and a valid verdict is dropped until the sleep is over.
#[test]
fn req_lock_018_sleep_hold_closes_the_attempt_and_drops_the_verdict() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    let ui = rig.ui;
    let effects = rig.send(Event::PrepareForSleep { nonce: 5 });
    let token = rig.state.hold_token().unwrap();
    assert_eq!(
        effects,
        [
            Effect::ShowFallback,
            Effect::RouteInput(InputMode::Nobody),
            Effect::StopUi { ui },
            Effect::OfferSleepHold(token),
            Effect::ReleaseSleepInhibitor { nonce: 5, token }
        ]
    );
    assert_eq!(rig.verdict(binding, Outcome::Granted), []);
    assert_eq!(rig.state.phase(), Phase::Locked);
    assert_eq!(rig.send(Event::UiConnected { ui }), []);
    // The bridge reports that the sleep is over: a new UI may start.
    assert!(restart_delay(&rig.bridge_report()).is_some());
    let binding = rig.open_attempt();
    rig.verdict(binding, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

/// REQ-LOCK-018 and REQ-LOCK-026: a new instance cannot know whether its
/// predecessor released a sleep, so it starts in the hold. The clock jump of
/// the resume ends the hold as well as the bridge's report does.
#[test]
fn req_lock_018_sleep_hold_survives_a_restart_and_ends_on_resume() {
    let mut rig = Rig::start();
    let presented = rig.present(Content::Protected);
    assert_eq!(presented, [Effect::LockAcknowledged { epoch: 1 }]);
    assert_eq!(rig.send(Event::UiConnected { ui: 1 }), []);
    assert_eq!(rig.send(Event::AttemptOpened { ui: 1 }), []);
    rig.sleep();
    assert!(restart_delay(&rig.send(Event::Tick)).is_none());
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(restart_delay(&rig.present(Content::Protected)).is_some());
    let binding = rig.open_attempt();
    rig.verdict(binding, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

#[test]
fn req_lock_025_verdict_as_first_event_after_a_jump_is_rejected() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    rig.sleep();
    let effects = rig.verdict(binding, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert!(!effects.iter().any(|e| matches!(e, Effect::Unlock { .. })));
}

#[test]
fn req_auth_001_matching_verdict_unlocks_exactly_once() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    let ui = rig.ui;
    assert_eq!(binding, FIRST);
    assert_eq!(
        rig.verdict(binding, Outcome::Granted),
        [
            Effect::StopUi { ui },
            Effect::Unlock { epoch: 1 },
            Effect::RouteInput(InputMode::Session)
        ]
    );
    assert!(rig.state.view().unlocked);
    // A replay finds nothing to unlock, now and in the next lock.
    assert_eq!(rig.verdict(binding, Outcome::Granted), []);
    rig.relock();
    assert_eq!(rig.verdict(binding, Outcome::Granted), []);
    rig.open_attempt();
    assert_eq!(rig.verdict(binding, Outcome::Granted), []);
    assert_eq!(rig.state.phase(), Phase::Locked);
}

/// REQ-AUTH-001: instance and session are separate fields here although the
/// model merges them, so each is tested on its own. A rejected verdict does
/// not consume the attempt.
#[test]
fn req_auth_001_each_binding_field_is_checked_on_its_own() {
    let mut rig = Rig::unlocked();
    rig.relock();
    let right = rig.open_attempt();
    let mut wrong = [right; 6];
    wrong[0].instance = InstanceId([8; 16]);
    wrong[1].session = SessionId([b'd'; 32]);
    wrong[2].epoch -= 1;
    wrong[3].epoch += 1;
    wrong[4].attempt -= 1;
    wrong[5].attempt += 1;
    for binding in wrong {
        assert_eq!(rig.verdict(binding, Outcome::Granted), [], "{binding:?}");
        assert_eq!(rig.state.phase(), Phase::Locked);
    }
    rig.verdict(right, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

#[test]
fn req_auth_001_verdict_without_an_open_attempt_is_rejected() {
    let guess = FIRST;
    let mut rig = Rig::start();
    assert_eq!(rig.verdict(guess, Outcome::Granted), []);
    rig.present(Content::Protected);
    assert_eq!(rig.verdict(guess, Outcome::Granted), []);
    rig.bridge_report();
    rig.send(Event::UiConnected { ui: rig.ui });
    assert_eq!(rig.verdict(guess, Outcome::Granted), []);
    // The attempt counter before any attempt is not an open attempt either.
    let unopened = Binding {
        attempt: 0,
        ..guess
    };
    assert_eq!(rig.verdict(unopened, Outcome::Granted), []);
    assert_eq!(rig.state.phase(), Phase::Locked);
}

/// REQ-AUTH-003, REQ-TRUST-002: a denial consumes the attempt and the next
/// one gets a new UI process.
#[test]
fn req_auth_003_denial_consumes_the_attempt() {
    let mut rig = Rig::locked();
    let first = rig.open_attempt();
    let old = rig.ui;
    let effects = rig.verdict(first, Outcome::Denied);
    assert!(effects.contains(&Effect::StopUi { ui: old }));
    assert_ne!(rig.ui, old);
    assert_eq!(rig.verdict(first, Outcome::Granted), []);
    let second = rig.open_attempt();
    assert_eq!(second.attempt, first.attempt + 1);
    assert_eq!(rig.verdict(first, Outcome::Granted), []);
    assert_eq!(rig.state.phase(), Phase::Locked);
}

#[test]
fn req_auth_004_verdict_after_the_attempt_timed_out_is_rejected() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    let ui = rig.ui;
    rig.now.monotonic += ATTEMPT_TIMEOUT;
    rig.now.boottime += ATTEMPT_TIMEOUT;
    let effects = rig.verdict(binding, Outcome::Granted);
    assert!(effects.contains(&Effect::StopUi { ui }));
    assert_eq!(restart_delay(&effects), Some(UI_BACKOFF_BASE));
    assert_eq!(rig.state.phase(), Phase::Locked);
}

#[test]
fn req_auth_016_resume_voids_the_open_attempt() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    rig.sleep();
    rig.send(Event::Tick);
    assert_eq!(rig.verdict(binding, Outcome::Granted), []);
    assert_eq!(rig.state.phase(), Phase::Locking);
    assert_eq!(rig.send(Event::AttemptOpened { ui: rig.ui }), []);
    assert!(acknowledged(&rig.present(Content::Protected)));
    let binding = rig.open_attempt();
    rig.verdict(binding, Outcome::Granted);
    assert_eq!(rig.state.phase(), Phase::Unlocked);
}

#[test]
fn req_auth_017_authd_disconnect_voids_the_open_attempt() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    let ui = rig.ui;
    let effects = rig.send(Event::AuthdDisconnected);
    assert!(effects.contains(&Effect::StopUi { ui }));
    assert_eq!(rig.verdict(binding, Outcome::Granted), []);
    assert_eq!(rig.state.phase(), Phase::Locked);
    assert_eq!(rig.state.view().epoch, binding.epoch);
}

#[test]
fn req_auth_017_disconnect_retires_the_ui_even_before_its_first_attempt() {
    for connected in [false, true] {
        let mut rig = Rig::locked();
        let ui = rig.ui;
        if connected {
            rig.send(Event::UiConnected { ui });
        }
        let effects = rig.send(Event::AuthdDisconnected);
        assert!(effects.contains(&Effect::StopUi { ui }));
        assert_ne!(rig.ui, ui);
        assert_eq!(rig.send(Event::UiConnected { ui }), []);
        assert_eq!(rig.send(Event::AttemptOpened { ui }), []);
        let binding = rig.open_attempt();
        assert_eq!(binding, FIRST);
    }
}

/// REQ-AUTH-018, REQ-TRUST-002: one attempt per UI process, so the channel
/// the challenge names is never shared between attempts.
#[test]
fn req_auth_018_each_attempt_names_a_new_ui_process() {
    let mut rig = Rig::locked();
    let mut channels = HashSet::new();
    for _ in 0..3 {
        rig.send(Event::UiConnected { ui: rig.ui });
        let opened = rig.send(Event::AttemptOpened { ui: rig.ui });
        let Some(&Effect::SendChallenge { binding, ui, .. }) = opened.first() else {
            panic!("no challenge in {opened:?}");
        };
        assert!(channels.insert(ui));
        assert_eq!(rig.send(Event::AttemptOpened { ui }), []);
        rig.verdict(binding, Outcome::Denied);
    }
}

/// REQ-LOCK-008: nothing `lock/ui` can cause ends the lock.
#[test]
fn req_lock_008_no_ui_event_unlocks() {
    let mut rig = Rig::locked();
    let ui = rig.ui;
    for event in [
        Event::UiConnected { ui },
        Event::AttemptOpened { ui },
        Event::AttemptOpened { ui },
        Event::UiExited { ui },
    ] {
        rig.send(event);
        assert_eq!(rig.state.phase(), Phase::Locked);
    }
}

/// One step of a generated sequence. Identifiers that must hit the current
/// state to matter are resolved when the step runs.
#[derive(Clone, Copy, Debug)]
enum Op {
    Plain(Event),
    /// The next step of the plain path from wherever the session is: the lock
    /// frame, the end of the hold, the UI, the attempt, a granted verdict.
    Progress,
    /// Both clocks advance.
    Wait(Duration),
    /// Only the boot-time clock advances.
    Sleep(Duration),
    /// `stale` picks an older process instead of the current one.
    UiConnected {
        stale: bool,
    },
    UiExited {
        stale: bool,
    },
    AttemptOpened {
        stale: bool,
    },
    Verdict(Aim, Outcome),
}

#[derive(Clone, Copy, Debug)]
enum Aim {
    /// The binding of the challenge sent last.
    Current,
    /// An earlier challenge, by index from the oldest.
    Earlier(usize),
    WrongInstance,
    WrongSession,
    NextEpoch,
    NextAttempt,
}

fn op() -> impl Strategy<Value = Op> {
    let outcome = prop_oneof![Just(Outcome::Granted), Just(Outcome::Denied)];
    let aim = prop_oneof![
        6 => Just(Aim::Current),
        2 => (0..8usize).prop_map(Aim::Earlier),
        1 => Just(Aim::WrongInstance),
        1 => Just(Aim::WrongSession),
        1 => Just(Aim::NextEpoch),
        1 => Just(Aim::NextAttempt),
    ];
    let plain = prop_oneof![
        1 => Just(Event::Tick),
        3 => Just(Event::LockRequested),
        1 => (0..3u64).prop_map(|nonce| Event::PrepareForSleep { nonce }),
        1 => Just(Event::BridgeConnected),
        1 => Just(Event::InhibitorTimedOut),
        1 => Just(Event::LogindUnlock),
        1 => Just(Event::RenderFailed),
        1 => Just(Event::FlipFailed),
        1 => Just(Event::OutputOff),
        1 => Just(Event::OutputOn),
        1 => Just(Event::AuthdDisconnected),
        2 => Just(Event::FrameSubmitted(Content::Protected)),
        2 => Just(Event::FramePresented(Content::Protected)),
        2 => Just(Event::FrameSubmitted(Content::Session)),
        2 => Just(Event::FramePresented(Content::Session)),
    ];
    prop_oneof![
        24 => plain.prop_map(Op::Plain),
        16 => Just(Op::Progress),
        1 => (0..40_000u64).prop_map(|ms| Op::Wait(Duration::from_millis(ms))),
        1 => (0..1_500u64).prop_map(|ms| Op::Sleep(Duration::from_millis(ms))),
        2 => stale().prop_map(|stale| Op::UiConnected { stale }),
        1 => stale().prop_map(|stale| Op::UiExited { stale }),
        2 => stale().prop_map(|stale| Op::AttemptOpened { stale }),
        6 => (aim, outcome).prop_map(|(aim, outcome)| Op::Verdict(aim, outcome)),
    ]
}

/// Whether a UI event names a replaced process: the rare case.
fn stale() -> impl Strategy<Value = bool> {
    prop_oneof![9 => Just(false), 1 => Just(true)]
}

/// Where the `lock/ui` process stands, read from the effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ui {
    Stopped,
    Starting,
    Connected,
    Checking,
}

/// What the properties need to remember, kept from inputs and effects alone.
struct Walk {
    rig: Rig,
    watch: SleepWatch,
    mode: InputMode,
    ui: Ui,
    challenges: Vec<Binding>,
    /// The last challenge can still be answered: nothing has voided it.
    answerable: Option<Duration>,
    used: HashSet<Binding>,
    /// What the frame in flight shows, if exactly one fresh frame is in flight.
    flight: Option<Content>,
    output_on: bool,
    /// Open decision O-29: no unlock from a sleep release to the end of the sleep.
    held: bool,
    nonces: HashSet<u64>,
    releases: usize,
}

impl Walk {
    fn start() -> Self {
        Self {
            rig: Rig::start(),
            watch: SleepWatch::new(BOOT, CONFIG.jump_threshold),
            mode: InputMode::Nobody,
            ui: Ui::Stopped,
            challenges: Vec::new(),
            answerable: None,
            used: HashSet::new(),
            flight: None,
            output_on: true,
            held: true,
            nonces: HashSet::new(),
            releases: 0,
        }
    }

    fn event(&mut self, op: Op) -> Option<Event> {
        let ui = |stale: bool| self.rig.ui.wrapping_sub(u32::from(stale));
        let last = self.challenges.last().copied().unwrap_or(FIRST);
        Some(match op {
            Op::Plain(event) => event,
            Op::Progress => match (self.rig.state.phase(), self.ui) {
                (Phase::Unlocked, _) => Event::Tick,
                (Phase::Locking, _) if self.flight.is_some() => {
                    Event::FramePresented(Content::Protected)
                }
                (Phase::Locking, _) => Event::FrameSubmitted(Content::Protected),
                (Phase::Locked, Ui::Stopped) if !self.output_on => Event::OutputOn,
                (Phase::Locked, Ui::Stopped) => self.rig.bridge_report_event(),
                (Phase::Locked, Ui::Starting) => Event::UiConnected { ui: ui(false) },
                (Phase::Locked, Ui::Connected) => Event::AttemptOpened { ui: ui(false) },
                (Phase::Locked, Ui::Checking) => Event::Verdict {
                    binding: last,
                    outcome: Outcome::Granted,
                },
            },
            Op::Wait(by) => {
                self.rig.now.monotonic += by;
                self.rig.now.boottime += by;
                return None;
            }
            Op::Sleep(by) => {
                self.rig.now.boottime += by;
                return None;
            }
            Op::UiConnected { stale } => Event::UiConnected { ui: ui(stale) },
            Op::UiExited { stale } => Event::UiExited { ui: ui(stale) },
            Op::AttemptOpened { stale } => Event::AttemptOpened { ui: ui(stale) },
            Op::Verdict(aim, outcome) => {
                let mut binding = last;
                match aim {
                    Aim::Current => {}
                    Aim::Earlier(index) => binding = *self.challenges.get(index).unwrap_or(&last),
                    Aim::WrongInstance => binding.instance = InstanceId([8; 16]),
                    Aim::WrongSession => binding.session = SessionId([b'd'; 32]),
                    Aim::NextEpoch => binding.epoch += 1,
                    Aim::NextAttempt => binding.attempt += 1,
                }
                Event::Verdict { binding, outcome }
            }
        })
    }

    /// Runs one step and checks every property on it.
    fn run(&mut self, op: Op) -> Result<(), TestCaseError> {
        let Some(event) = self.event(op) else {
            return Ok(());
        };
        let (before, ui) = (self.rig.state, self.rig.ui);
        let effects = self.rig.send(event);
        let (after, now) = (self.rig.state, self.rig.now);
        prop_assert!(effects.len() <= 8, "unbounded effects: {effects:?}");

        // REQ-LOCK-025: the clock check is a prelude. Handling the jump alone
        // and then the event gives the same result as handling them together.
        let tick = Event::Tick;
        let (seen, mut split) = step(before, Input { now, event: tick });
        let (rest, tail) = step(seen, Input { now, event });
        split.extend(tail);
        prop_assert_eq!((rest, &split), (after, &effects));

        // What the inputs void, before the event itself is judged.
        let slept = self.watch.slept(now);
        let hold_ended =
            matches!(event, Event::SleepOver { token } if before.hold_token() == Some(token));
        if slept || hold_ended {
            self.held = false;
            self.nonces.clear();
        }
        if slept {
            self.flight = self.flight.map(|_| Content::Session);
        }
        let timed_out = self.answerable.is_some_and(|end| now.monotonic >= end);
        let gone = matches!(event, Event::OutputOff | Event::AuthdDisconnected)
            || event == Event::UiExited { ui };
        if slept || timed_out || gone {
            self.answerable = None;
        }
        let taken = Taken {
            before,
            after,
            event,
            effects,
            slept,
        };
        self.check_unlock(&taken)?;
        self.check_acknowledgement(&taken)?;
        self.check_effects(&taken)
    }

    /// Property A: only a verdict for the current instance, session, epoch
    /// and attempt unlocks, once. Every other step stays locked.
    fn check_unlock(&mut self, taken: &Taken) -> Result<(), TestCaseError> {
        let (before, after) = (taken.before.view(), taken.after.view());
        let unlocks = !before.unlocked && after.unlocked;
        let told = taken.effects.contains(&Effect::Unlock {
            epoch: before.epoch,
        });
        prop_assert_eq!(unlocks, told);
        let answer = match taken.event {
            Event::Verdict { binding, outcome } => Some((binding, outcome)),
            _ => None,
        };
        if unlocks {
            let last = self.challenges.last().copied();
            prop_assert_eq!(answer.unzip(), (last, Some(Outcome::Granted)));
            prop_assert_eq!(last.map(|binding| binding.epoch), Some(before.epoch));
            prop_assert!(self.answerable.is_some(), "the attempt was void");
            prop_assert!(!self.held, "unlocked in the sleep hold");
            let fresh = last.is_some_and(|binding| self.used.insert(binding));
            prop_assert!(fresh, "verdict used twice");
            prop_assert_eq!(taken.before.phase(), Phase::Locked);
        }
        if answer.is_some_and(|(binding, _)| Some(&binding) == self.challenges.last()) {
            self.answerable = None;
        }
        Ok(())
    }

    /// Property B: `Locked` is entered by an acknowledgement, and that has
    /// two causes only.
    fn check_acknowledgement(&mut self, taken: &Taken) -> Result<(), TestCaseError> {
        let reacknowledged = taken.slept && taken.event == Event::OutputOff;
        let entered = (taken.before.phase() != Phase::Locked || reacknowledged)
            && taken.after.phase() == Phase::Locked;
        prop_assert_eq!(entered, acknowledged(&taken.effects));
        let shown = taken.event == Event::FramePresented(Content::Protected)
            && self.flight == Some(Content::Protected);
        let dark = !self.output_on || taken.event == Event::OutputOff;
        match taken.event {
            Event::OutputOff => (self.output_on, self.flight) = (false, None),
            Event::OutputOn => {
                if !self.output_on && taken.before.phase() == Phase::Locked && !taken.slept {
                    self.flight = None;
                }
                self.output_on = true;
            }
            Event::FrameSubmitted(content) => {
                let alone = self.flight.is_none();
                self.flight = Some(if alone { content } else { Content::Session });
            }
            Event::FramePresented(_) | Event::FlipFailed => self.flight = None,
            _ => {}
        }
        prop_assert!(!entered || shown || dark, "{:?}", taken.event);
        Ok(())
    }

    fn check_effects(&mut self, taken: &Taken) -> Result<(), TestCaseError> {
        let (before, after) = (taken.before.view(), taken.after.view());
        // REQ-LOCK-007, REQ-LOCK-021: a lock is a new epoch and a revocation.
        let locks = before.unlocked && !after.unlocked;
        prop_assert_eq!(after.epoch, before.epoch + u64::from(locks));
        let revokes = taken.effects.contains(&Effect::RevokeAllGrants);
        prop_assert!(revokes || !(locks || taken.slept));

        let locked = taken.after.phase() == Phase::Locked;
        for effect in &taken.effects {
            match *effect {
                Effect::RouteInput(mode) => {
                    self.mode = mode;
                    self.ui = match mode {
                        InputMode::LockUi => Ui::Connected,
                        InputMode::Nobody | InputMode::Session => Ui::Stopped,
                    };
                }
                Effect::StopUi { .. } => self.ui = Ui::Stopped,
                Effect::SendChallenge {
                    binding, deadline, ..
                } => {
                    self.ui = Ui::Checking;
                    prop_assert!(!self.challenges.contains(&binding), "binding reused");
                    prop_assert!(locked && !self.held, "attempt opened too early");
                    self.challenges.push(binding);
                    self.answerable = Some(deadline);
                }
                Effect::StartUi { delay, .. } => {
                    self.ui = Ui::Starting;
                    prop_assert!(locked && delay <= UI_BACKOFF_MAX);
                }
                // REQ-LOCK-024: released only while `Locked`, for a request
                // of this wake period.
                Effect::OfferSleepHold(offered)
                    if matches!(taken.event, Event::PrepareForSleep { .. }) =>
                {
                    prop_assert_eq!(taken.after.hold_token(), Some(offered));
                    self.held = true;
                    self.answerable = None;
                }
                Effect::ReleaseSleepInhibitor { nonce, .. } => {
                    let requested = taken.event == Event::PrepareForSleep { nonce };
                    prop_assert!(locked && (requested || self.nonces.contains(&nonce)));
                    self.held = true;
                    self.answerable = None;
                    self.releases += 1;
                }
                _ => {}
            }
        }
        if let Event::PrepareForSleep { nonce } = taken.event {
            self.nonces.insert(nonce);
        }
        // REQ-LOCK-003, REQ-LOCK-019: clients have the input exactly while unlocked.
        prop_assert_eq!(self.mode == InputMode::Session, after.unlocked);
        Ok(())
    }
}

/// One executed step, as the properties see it.
struct Taken {
    before: State,
    after: State,
    event: Event,
    effects: Vec<Effect>,
    slept: bool,
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 1024,
        rng_seed: RngSeed::Fixed(0x534b_4f52_4c4f_434b),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(config())]

    /// REQ-AUTH-001, REQ-AUTH-003, REQ-LOCK-002, REQ-LOCK-025: properties A
    /// and B, "every failure stays locked" and the clock prelude, on every
    /// step of arbitrary event sequences.
    #[test]
    fn req_auth_001_properties_hold_on_arbitrary_sequences(ops in vec(op(), 0..120)) {
        let mut walk = Walk::start();
        for op in ops {
            walk.run(op)?;
        }
    }
}

/// The generated sequences do reach an unlock, a second lock and a sleep
/// release; without this the properties above could hold vacuously.
#[test]
fn req_auth_001_generated_sequences_reach_the_interesting_states() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::new(config());
    let (mut unlocks, mut second_epochs, mut releases) = (0, 0, 0);
    for _ in 0..256 {
        let ops = vec(op(), 0..120).new_tree(&mut runner).unwrap().current();
        let mut walk = Walk::start();
        for op in ops {
            walk.run(op).unwrap();
        }
        unlocks += usize::from(!walk.used.is_empty());
        second_epochs += usize::from(walk.rig.state.view().epoch > 1);
        releases += usize::from(walk.releases > 0);
    }
    assert!(unlocks > 128, "{unlocks} of 256 sequences unlocked");
    assert!(
        second_epochs > 128,
        "{second_epochs} of 256 reached a second lock"
    );
    assert!(releases > 64, "{releases} of 256 released a sleep");
}

#[test]
fn req_lock_026_descheduled_clock_read_retains_hold() {
    let mut rig = Rig::start();
    assert!(acknowledged(&rig.present(Content::Protected)));
    let token = rig.state.hold_token().unwrap();
    let before = rig.now.monotonic;
    let after = before + 2 * CONFIG.jump_threshold;
    let (state, effects) = step(
        rig.state,
        Input {
            now: Clocks::bracketed(before, before, after),
            event: Event::SleepOver { token },
        },
    );
    assert_eq!(state.hold_token(), Some(token));
    assert_eq!(state.phase(), Phase::Locking);
    assert_eq!(restart_delay(&effects), None);

    let (state, _) = step(
        state,
        Input {
            now: Clocks::bracketed(after + NAP, after, after + NAP + TICK),
            event: Event::Tick,
        },
    );
    assert_eq!(state.hold_token(), None);
}

#[test]
fn req_lock_025_ambiguous_clock_read_voids_the_open_verdict() {
    let mut rig = Rig::locked();
    let binding = rig.open_attempt();
    let monotonic = rig.now.monotonic;
    let (state, effects) = step(
        rig.state,
        Input {
            // A resume after the monotonic read makes the sample ambiguous.
            now: Clocks::bracketed(monotonic, monotonic, monotonic + NAP),
            event: Event::Verdict {
                binding,
                outcome: Outcome::Granted,
            },
        },
    );
    assert_eq!(state.phase(), Phase::Locking);
    assert!(effects.contains(&Effect::StopUi { ui: rig.ui }));
}

#[test]
fn req_lock_026_stale_bridge_report_does_not_end_the_current_hold() {
    let mut rig = Rig::start();
    let old = rig.state.hold_token().unwrap();
    rig.present(Content::Protected);
    rig.bridge_report();
    rig.send(Event::PrepareForSleep { nonce: 1 });
    let current = rig.state.hold_token().unwrap();
    assert_ne!(old, current);
    assert_eq!(
        rig.send(Event::BridgeConnected),
        [Effect::OfferSleepHold(current)]
    );
    assert_eq!(rig.send(Event::SleepOver { token: old }), []);
    assert_eq!(rig.state.hold_token(), Some(current));
    assert!(restart_delay(&rig.bridge_report()).is_some());
    assert_eq!(rig.state.hold_token(), None);
}
