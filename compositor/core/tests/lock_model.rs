//! Counterexamples of the Quint model, replayed on the Rust reducers.
//!
//! `spec/lock.qnt` has one trace `cx_<defect>` for every variant in
//! `spec/variants.qnt`. The trace breaks a property once the rule named by the
//! defect is removed. Each test here replays one trace as events on the lock
//! and grant reducers, composed with a small model of the panel ([`World`]),
//! and asserts what the matching guard test asserts on the chosen design.
//!
//! | Test | Trace | Variant | Property broken in the variant |
//! | --- | --- | --- | --- |
//! | `req_lock_008_cx_client_unlock` | `cx_client_unlock` | `rejected_client_unlock` | `propA_verdict` |
//! | `req_lock_017_cx_honor_logind_unlock` | `cx_honor_logind_unlock` | `ablate_logind_unlock` | `propA_verdict` |
//! | `req_lock_011_cx_unlock_on_render_error` | `cx_unlock_on_render_error` | `ablate_render_error` | `propA_verdict` |
//! | `req_auth_015_cx_skip_instance_check` | `cx_skip_instance_check` | `ablate_instance` | `propA_verdict` |
//! | `req_auth_001_cx_skip_epoch_check` | `cx_skip_epoch_check` | `ablate_epoch` | `propA_verdict` |
//! | `req_auth_004_cx_skip_attempt_check` | `cx_skip_attempt_check` | `ablate_attempt` | `propA_verdict` |
//! | `req_auth_018_cx_shared_ui_channel` | `cx_shared_ui_channel` | `ablate_ui_channel` | `propA_typed` (compositor's half) |
//! | `req_lock_002_cx_ack_on_submit` | `cx_ack_on_submit` | `ablate_ack_on_submit` | `propB_covered` |
//! | `req_lock_002_cx_ack_any_flip` | `cx_ack_any_flip` | `ablate_ack_any_flip` | `propB_covered` |
//! | `req_lock_025_cx_no_boottime` | `cx_no_boottime` | `ablate_boottime` | `propB_resume` |
//! | `req_auth_016_cx_keep_attempt_on_resume` | `cx_keep_attempt_on_resume` | `ablate_resume_attempt` | `propA_wake` |
//! | `req_lock_024_cx_stale_ack` | `cx_stale_ack` | `ablate_stale_ack` | `propS_sleep` (compositor's half) |
//! | `req_lock_021_cx_no_revoke_on_lock` | `cx_no_revoke_on_lock` | `ablate_lock_revokes` | `propC_lockRevokes` |
//! | `req_grant_013_cx_check_at_enqueue_only` | `cx_check_at_enqueue_only` | `ablate_dispatch_check` | `propC_noLateDelivery` |
//! | `req_agent_005_cx_check_at_enqueue_only_agent` | `cx_check_at_enqueue_only_agent` | `ablate_dispatch_check` | `propD_agent` |
//! | `req_grant_014_cx_grant_by_name` | `cx_grant_by_name` | `ablate_grant_by_name` | `propC_noLateDelivery` |
//! | `req_grant_013_cx_timer_only_expiry` | `cx_timer_only_expiry` | `ablate_timer_expiry` | `propC_noLateDelivery` |
//! | `req_grant_007_cx_skip_window_gen` | `cx_skip_window_gen` | `ablate_window_gen` | `propC_noLateDelivery` |
//! | `req_auth_017_cx_keep_attempt_on_authd_disconnect` | `cx_keep_attempt_on_authd_disconnect` | `ablate_broker_disconnect` | `propA_broker` |
//! | `req_lock_015_cx_ack_stale_flip` | `cx_ack_stale_flip` | `ablate_stale_flip` | `propB_covered` |
//! | `req_grant_008_cx_stop_by_holder` | `cx_stop_by_holder` | `ablate_stop_generation` | `propC_stopExact` |
//! | `req_lock_018_sleep_hold_closes_gap` | `sleepHoldClosesGap` | `option_sleep_hold` | `strongS_ackedSleepCovered` |
//! | `req_lock_018_sleep_hold_survives_restart` | `sleepHoldSurvivesRestart` | `option_sleep_hold` | `strongS_ackedSleepCovered` |
//!
//! Where a replay differs from its trace:
//!
//! - The model starts unlocked. A compositor instance here starts covered and
//!   in the sleep hold (REQ-LOCK-026), so a replay first walks the
//!   plain path to an unlock, and its bindings start at epoch 2.
//! - After `compRestart` the new instance is in the hold. The replay adds the
//!   bridge's matched report (`SleepOver`) before `compOpenAttempt`.
//! - Steps of `authd`, `pwcheck`, `lock/ui` and the bridge have no event in
//!   these reducers. The replay carries the message they would send. Two
//!   traces break a rule of another process (`cx_shared_ui_channel` in
//!   `authd`, `cx_stale_ack` in the bridge); their replays check the
//!   compositor's half of the rule.
//! - The model has one grant per holder. The reducer keeps a table, so in
//!   `cx_stop_by_holder` the first stream is still live when the second
//!   starts.
//!
//! The last test walks random sequences of the same actions and checks the
//! model's safety invariants after every step.

#![expect(
    clippy::unwrap_used,
    reason = "clippy.toml exempts test functions only; the helpers here are test code too"
)]

use core::time::Duration;
use std::collections::HashSet;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use skorupa_compositor_core::grant::{self, ConnectionId, Grant, Kind, Request, Target, WindowRef};
use skorupa_compositor_core::lock::{
    self, ATTEMPT_TIMEOUT, Binding, Clocks, Config, Content, InstanceId, Outcome, Phase, SessionId,
    SleepWatch,
};

const THRESHOLD: Duration = Duration::from_secs(1);
const BOOT: Clocks = Clocks {
    monotonic: Duration::from_secs(100),
    boottime: Duration::from_secs(100),
};
const TICK: Duration = Duration::from_millis(1);
const TTL: Duration = Duration::from_secs(60);

/// The reducers of one compositor instance and the world around them: the
/// panel as the kernel sees it, the clocks, and the facts the model keeps in
/// its history variables.
struct World {
    lock: lock::State,
    grants: grant::State,
    instance: u8,
    now: Clocks,
    /// What the panel shows; `None` is blank.
    screen: Option<Content>,
    /// The pending page flip, and whether the kernel settled it at a suspend.
    flip: Option<(Content, bool)>,
    output_on: bool,
    /// Serial of the `lock/ui` process started last.
    ui: u32,
    /// The challenge sent last, and the one `authd` still remembers.
    challenge: Option<Binding>,
    authd: Option<Binding>,
    /// Every verdict ever issued: each stays deliverable forever.
    verdicts: Vec<(Binding, Outcome)>,
    releases: Vec<u64>,
    window: WindowRef,
    portal: ConnectionId,
    agent: ConnectionId,
    prompt: Option<grant::PromptId>,
    issued: Vec<Grant>,
    seen: History,
}

/// The model's history variables: no reducer reads them, the checks do.
#[derive(Default)]
struct History {
    /// Acknowledged and not yet unlocked by a verdict.
    must_cover: bool,
    /// Resumed and not yet unlocked by a verdict since.
    must_relock: bool,
    opened_this_wake: bool,
    /// Bindings of verdicts issued by an `authd` that has since gone.
    dead_broker: HashSet<Binding>,
    used: HashSet<Binding>,
    /// Grants made since the session was last locked, asleep or restarted.
    fresh: HashSet<u64>,
    stopped: HashSet<u64>,
    delivered: Vec<u64>,
    deliveries: usize,
}

impl World {
    /// `compRestart`, and the first start: a new instance, covered.
    fn start() -> Self {
        let world = Self {
            lock: lock::State::start(config(1), BOOT).0,
            grants: grant::State::new(config(1).instance, SleepWatch::new(BOOT, THRESHOLD), false),
            instance: 1,
            now: BOOT,
            screen: None,
            flip: None,
            output_on: true,
            ui: 0,
            challenge: None,
            authd: None,
            verdicts: Vec::new(),
            releases: Vec::new(),
            window: WindowRef {
                id: 1,
                generation: 1,
            },
            portal: ConnectionId(1),
            agent: ConnectionId(100),
            prompt: None,
            issued: Vec::new(),
            seen: History::default(),
        };
        world.check();
        world
    }

    fn comp_restart(&mut self) {
        self.instance += 1;
        let config = config(self.instance);
        self.lock = lock::State::start(config, self.now).0;
        let watch = SleepWatch::new(self.now, THRESHOLD);
        self.grants = grant::State::new(config.instance, watch, false);
        (self.screen, self.flip) = (None, None);
        // Generations restart with the instance, and nothing queued survives it.
        self.issued.clear();
        self.seen.fresh.clear();
        self.seen.stopped.clear();
        self.seen.delivered.clear();
        self.check();
    }

    /// The model's `init`: an unlocked session, reached here by the plain path.
    fn unlocked() -> Self {
        let mut world = Self::start();
        world.present();
        world.bridge_report();
        let binding = world.reach_verdict();
        world.verdict(binding, Outcome::Granted);
        assert_eq!(world.lock.phase(), Phase::Unlocked);
        world
    }

    fn bridge_report(&mut self) {
        if let Some(token) = self.lock.hold_token() {
            self.lock_event(lock::Event::SleepOver { token });
        }
    }

    fn phase(&self) -> Phase {
        self.lock.phase()
    }

    /// The model's `safety`, as far as it concerns the compositor.
    fn check(&self) {
        let covered = self.screen != Some(Content::Session);
        assert!(
            covered || !self.seen.must_cover,
            "B: session on an acknowledged lock"
        );
        let locked = self.phase() != Phase::Unlocked;
        assert!(
            self.seen.must_cover || self.phase() != Phase::Locked,
            "B: Locked unacknowledged"
        );
        assert!(
            !locked || self.grants.grants().is_empty(),
            "C: grant outlived the lock"
        );
    }

    fn lock_event(&mut self, event: lock::Event) -> Vec<lock::Effect> {
        self.now.monotonic += TICK;
        self.now.boottime += TICK;
        let was_unlocked = self.phase() == Phase::Unlocked;
        let input = lock::Input {
            now: self.now,
            event,
        };
        let (state, effects) = lock::step(self.lock, input);
        self.lock = state;
        for effect in &effects {
            match *effect {
                lock::Effect::StartUi { ui, .. } => self.ui = ui,
                lock::Effect::SendChallenge { binding, .. } => {
                    self.challenge = Some(binding);
                    self.authd = Some(binding);
                    self.seen.opened_this_wake = true;
                }
                lock::Effect::LockAcknowledged { .. } => self.seen.must_cover = true,
                lock::Effect::ReleaseSleepInhibitor { nonce, .. } => self.releases.push(nonce),
                lock::Effect::Unlock { .. } => {
                    self.seen.must_cover = false;
                    self.seen.must_relock = false;
                }
                _ => {}
            }
        }
        if effects.contains(&lock::Effect::RevokeAllGrants) {
            self.grant_event(grant::Event::Lock);
        }
        // Property A, with the model's history variables.
        if !was_unlocked && self.phase() == Phase::Unlocked {
            let outcome = Outcome::Granted;
            let binding = self.challenge.unwrap();
            assert_eq!(event, lock::Event::Verdict { binding, outcome });
            assert!(
                self.verdicts.contains(&(binding, outcome)),
                "A: verdict never issued"
            );
            assert!(
                self.seen.opened_this_wake,
                "A: attempt from before the resume"
            );
            assert!(
                !self.seen.dead_broker.contains(&binding),
                "A: verdict of a dead authd"
            );
            assert!(self.seen.used.insert(binding), "A: verdict used twice");
        }
        if self.phase() != Phase::Unlocked {
            self.seen.fresh.clear();
        }
        self.check();
        effects
    }

    fn comp_lock(&mut self) {
        self.lock_event(lock::Event::LockRequested);
    }

    /// `compRender`: the adapter reads the clocks, then draws what the phase
    /// allows. Returns what it submitted.
    fn comp_render(&mut self, ok: bool) -> Option<Content> {
        self.lock_event(lock::Event::Tick);
        if !self.output_on || self.flip.is_some() {
            return None;
        }
        let content = match self.phase() {
            Phase::Unlocked => Content::Session,
            Phase::Locking | Phase::Locked => Content::Protected,
        };
        if !ok {
            self.lock_event(lock::Event::RenderFailed);
            return None;
        }
        let exposed = self.seen.must_relock && content == Content::Session;
        assert!(!exposed, "B: session frame submitted after a resume");
        self.flip = Some((content, false));
        self.lock_event(lock::Event::FrameSubmitted(content));
        Some(content)
    }

    fn kms_flip_done(&mut self) {
        if let Some((content, _)) = self.flip.take_if(|(_, settled)| !*settled) {
            self.screen = Some(content);
            self.lock_event(lock::Event::FramePresented(content));
        }
    }

    fn kms_flip_fail(&mut self) {
        if self.flip.take_if(|(_, settled)| !*settled).is_some() {
            self.lock_event(lock::Event::FlipFailed);
        }
    }

    /// `compReadStaleFlip`: the event of a flip the kernel settled at the suspend.
    fn comp_read_stale_flip(&mut self) {
        if let Some((content, _)) = self.flip.take_if(|(_, settled)| *settled) {
            self.lock_event(lock::Event::FramePresented(content));
        }
    }

    fn output_off(&mut self) {
        (self.screen, self.flip, self.output_on) = (None, None, false);
        self.lock_event(lock::Event::OutputOff);
    }

    fn output_back(&mut self) {
        self.output_on = true;
        self.lock_event(lock::Event::OutputOn);
    }

    /// `compRender(true)` then `kmsFlipDone`.
    fn present(&mut self) {
        self.comp_render(true);
        self.kms_flip_done();
    }

    /// The model's `toLocked`.
    fn reach_locked(&mut self) {
        self.comp_lock();
        self.present();
        assert_eq!(self.phase(), Phase::Locked);
    }

    /// The suspend (`logindSuspend` or `inhibitTimeout`) and the `resume`.
    /// No process runs in between. `drain` says whether the kernel showed a
    /// pending frame before sleeping or dropped it.
    fn sleep(&mut self, drain: bool) {
        if let Some((content, settled)) = &mut self.flip {
            if drain && !*settled {
                self.screen = Some(*content);
            }
            *settled = true;
        }
        self.now.boottime += Duration::from_secs(3600);
        self.seen.must_relock = true;
        self.seen.opened_this_wake = false;
        self.seen.fresh.clear();
        self.check();
    }

    /// `compSeeJump`.
    fn comp_see_jump(&mut self) {
        self.lock_event(lock::Event::Tick);
    }

    /// `compOpenAttempt`: the UI of this attempt connects and asks for a challenge.
    fn comp_open_attempt(&mut self) -> Option<Binding> {
        self.lock_event(lock::Event::UiConnected { ui: self.ui });
        let effects = self.lock_event(lock::Event::AttemptOpened { ui: self.ui });
        effects.iter().find_map(|effect| match effect {
            lock::Effect::SendChallenge { binding, .. } => Some(*binding),
            _ => None,
        })
    }

    /// `authdRecvPam`: `authd` issues a verdict for the challenge it holds.
    fn authd_issue(&mut self, outcome: Outcome) {
        self.verdicts
            .extend(self.authd.map(|binding| (binding, outcome)));
    }

    /// The model's `toVerdict11`: an attempt is open and its positive verdict
    /// is in flight.
    fn reach_verdict(&mut self) -> Binding {
        let binding = self.comp_open_attempt().unwrap();
        self.authd_issue(Outcome::Granted);
        binding
    }

    /// `compCloseAttempt`: the attempt times out.
    fn comp_close_attempt(&mut self) {
        self.now.monotonic += ATTEMPT_TIMEOUT;
        self.now.boottime += ATTEMPT_TIMEOUT;
        self.lock_event(lock::Event::Tick);
    }

    /// `compRecvVerdict`.
    fn verdict(&mut self, binding: Binding, outcome: Outcome) {
        self.lock_event(lock::Event::Verdict { binding, outcome });
    }

    /// `authdRestart`: the compositor reads the end of the connection in the same step.
    fn authd_restart(&mut self) {
        self.authd = None;
        let issued = self.verdicts.iter().map(|(binding, _)| *binding);
        self.seen.dead_broker = issued.collect();
        self.lock_event(lock::Event::AuthdDisconnected);
    }

    /// `bridgeSeeSleep` and the delivery of its lock request.
    fn sleep_request(&mut self, nonce: u64) {
        self.lock_event(lock::Event::PrepareForSleep { nonce });
    }

    fn connection(&self, kind: Kind) -> ConnectionId {
        if kind.is_agent() {
            self.agent
        } else {
            self.portal
        }
    }

    fn grant_event(&mut self, event: grant::Event) -> Vec<grant::Effect> {
        let input = grant::Input {
            now: self.now,
            lock: self.lock.view(),
            event,
        };
        let (state, effects) = grant::step(self.grants.clone(), input);
        self.grants = state;
        for effect in &effects {
            match *effect {
                grant::Effect::ShowPrompt { prompt, .. } => self.prompt = Some(prompt),
                grant::Effect::Granted(grant) => {
                    self.issued.push(grant);
                    self.seen.fresh.insert(grant.generation);
                }
                // Properties C and D, from facts of the world alone.
                grant::Effect::Deliver { generation } => {
                    let grant = self
                        .issued
                        .iter()
                        .find(|grant| grant.generation == generation);
                    let grant = *grant.unwrap();
                    assert_eq!(self.phase(), Phase::Unlocked, "C: delivery while locked");
                    assert!(
                        self.seen.fresh.contains(&generation),
                        "C: delivery after lock or sleep"
                    );
                    assert!(
                        !self.seen.stopped.contains(&generation),
                        "C: delivery after Stop"
                    );
                    assert_eq!(
                        grant.connection,
                        self.connection(grant.kind),
                        "C: reconnected"
                    );
                    assert_eq!(
                        grant.target,
                        Target::Window(self.window),
                        "C: window replaced"
                    );
                    assert!(
                        self.now.boottime < grant.expires,
                        "C: delivery after the deadline"
                    );
                    let again = self.seen.delivered.contains(&generation);
                    assert!(
                        !(again && grant.kind.is_agent()),
                        "D: agent grant used twice"
                    );
                    assert_ne!(grant.kind, Kind::AgentCapture, "D: agent capture in v1");
                    self.seen.delivered.push(generation);
                    self.seen.deliveries += 1;
                }
                _ => {}
            }
        }
        self.check();
        effects
    }

    /// `compAsk`.
    fn comp_ask(&mut self, kind: Kind) {
        let request = Request {
            kind,
            connection: self.connection(kind),
            target: Target::Window(self.window),
            ttl: TTL,
        };
        self.grant_event(grant::Event::Requested(request));
    }

    /// `consentAnswer` and `compRecvAnswer` for the prompt shown last.
    fn answer(&mut self, allow: bool) -> Option<Grant> {
        let prompt = self.prompt?;
        let effects = self.grant_event(grant::Event::Answered { prompt, allow });
        effects.iter().find_map(|effect| match effect {
            grant::Effect::Granted(grant) => Some(*grant),
            _ => None,
        })
    }

    /// The model's `toGrant`.
    fn reach_grant(&mut self, kind: Kind) -> Grant {
        self.comp_ask(kind);
        self.answer(true).unwrap()
    }

    /// `compDispatch`: the adapter is about to write a delivery that was
    /// queued under `grant`, to the holder's current connection, showing the
    /// current window. Returns whether it left.
    fn comp_dispatch(&mut self, grant: &Grant) -> bool {
        let generation = grant.generation;
        let effects = self.grant_event(grant::Event::Use {
            generation,
            kind: grant.kind,
            connection: self.connection(grant.kind),
            target: Target::Window(self.window),
        });
        effects.contains(&grant::Effect::Deliver { generation })
    }

    /// `windowReplaced`. `told` is whether the grant reducer gets the event;
    /// a delivery must be dropped either way.
    fn window_replaced(&mut self, told: bool) {
        let old = self.window;
        self.window.generation += 1;
        if told {
            self.grant_event(grant::Event::WindowGone(old));
        }
    }

    /// `compRecvStop`.
    fn stop(&mut self, generation: u64) {
        self.seen.stopped.insert(generation);
        let instance = config(self.instance).instance;
        self.grant_event(grant::Event::Stop {
            instance,
            generation,
        });
    }
}

fn config(instance: u8) -> Config {
    Config {
        instance: InstanceId([instance; 16]),
        session: SessionId([b'1'; 32]),
        jump_threshold: THRESHOLD,
    }
}

#[test]
fn req_lock_008_cx_client_unlock() {
    let mut world = World::unlocked();
    world.reach_locked();
    // `uiUnlockAndDestroy` has no counterpart: everything `lock/ui` can cause is tried.
    let ui = world.ui;
    world.lock_event(lock::Event::UiConnected { ui });
    world.lock_event(lock::Event::AttemptOpened { ui });
    world.lock_event(lock::Event::UiExited { ui });
    assert_eq!(world.phase(), Phase::Locked);
}

#[test]
fn req_lock_017_cx_honor_logind_unlock() {
    let mut world = World::unlocked();
    world.reach_locked();
    world.lock_event(lock::Event::LogindUnlock);
    assert_eq!(world.phase(), Phase::Locked);
}

#[test]
fn req_lock_011_cx_unlock_on_render_error() {
    let mut world = World::unlocked();
    world.comp_lock();
    world.comp_render(false);
    assert_eq!(world.phase(), Phase::Locking);
}

/// REQ-AUTH-015: counters restart with the compositor, so the two bindings
/// differ in the instance only.
#[test]
fn req_auth_015_cx_skip_instance_check() {
    let mut world = World::start();
    world.present();
    world.bridge_report();
    let old = world.reach_verdict();
    world.verdict(old, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Unlocked);
    world.comp_restart();
    world.present();
    world.bridge_report();
    let new = world.comp_open_attempt().unwrap();
    assert_eq!((new.epoch, new.attempt), (old.epoch, old.attempt));
    assert_ne!(new.instance, old.instance);
    world.verdict(old, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Locked);
}

#[test]
fn req_auth_001_cx_skip_epoch_check() {
    let mut world = World::unlocked();
    world.reach_locked();
    let old = world.reach_verdict();
    world.verdict(old, Outcome::Granted);
    world.reach_locked();
    let new = world.comp_open_attempt().unwrap();
    assert_eq!((new.epoch, new.attempt), (old.epoch + 1, old.attempt));
    world.verdict(old, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Locked);
}

#[test]
fn req_auth_004_cx_skip_attempt_check() {
    let mut world = World::unlocked();
    world.reach_locked();
    let binding = world.reach_verdict();
    world.comp_close_attempt();
    world.verdict(binding, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Locked);
}

/// REQ-AUTH-018: the compositor's half. The second attempt gets a new UI
/// process, and with it a new channel, and the first one is killed, so the
/// secret typed for attempt 1 has no channel into attempt 2.
#[test]
fn req_auth_018_cx_shared_ui_channel() {
    let mut world = World::unlocked();
    world.reach_locked();
    let first = world.comp_open_attempt().unwrap();
    let old = world.ui;
    world.now.monotonic += ATTEMPT_TIMEOUT;
    world.now.boottime += ATTEMPT_TIMEOUT;
    let closed = world.lock_event(lock::Event::Tick);
    assert!(closed.contains(&lock::Effect::StopUi { ui: old }));
    world.lock_event(lock::Event::UiConnected { ui: world.ui });
    let opened = world.lock_event(lock::Event::AttemptOpened { ui: world.ui });
    let Some(&lock::Effect::SendChallenge { binding, ui, .. }) = opened.first() else {
        panic!("no challenge in {opened:?}");
    };
    assert_ne!(ui, old);
    assert_eq!(binding.attempt, first.attempt + 1);
    // The old process cannot open or reopen anything.
    assert_eq!(world.lock_event(lock::Event::AttemptOpened { ui: old }), []);
}

#[test]
fn req_lock_002_cx_ack_on_submit() {
    let mut world = World::unlocked();
    world.present();
    world.comp_lock();
    assert_eq!(world.comp_render(true), Some(Content::Protected));
    assert_eq!(world.phase(), Phase::Locking);
    assert_eq!(world.screen, Some(Content::Session));
    assert!(!world.seen.must_cover);
}

#[test]
fn req_lock_002_cx_ack_any_flip() {
    let mut world = World::unlocked();
    assert_eq!(world.comp_render(true), Some(Content::Session));
    world.comp_lock();
    world.kms_flip_done();
    assert_eq!(world.phase(), Phase::Locking);
    assert_eq!(world.screen, Some(Content::Session));
    assert!(!world.seen.must_cover);
    world.comp_render(true);
    world.kms_flip_fail();
    assert_eq!(world.phase(), Phase::Locking);
    world.present();
    assert_eq!(world.phase(), Phase::Locked);
    assert_eq!(world.screen, Some(Content::Protected));
}

/// REQ-LOCK-025: the first frame after a resume cannot be a session frame,
/// because the jump is handled before the adapter learns what to draw.
#[test]
fn req_lock_025_cx_no_boottime() {
    let mut world = World::unlocked();
    world.present();
    world.sleep(true);
    assert_eq!(world.comp_render(true), Some(Content::Protected));
    world.kms_flip_done();
    assert_eq!(world.phase(), Phase::Locked);
    assert_eq!(world.screen, Some(Content::Protected));
}

#[test]
fn req_auth_016_cx_keep_attempt_on_resume() {
    let mut world = World::unlocked();
    world.reach_locked();
    let binding = world.reach_verdict();
    world.sleep(true);
    world.comp_see_jump();
    world.verdict(binding, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Locking);
    assert_eq!(world.comp_open_attempt(), None);
    world.present();
    assert_eq!(world.phase(), Phase::Locked);
}

/// REQ-LOCK-024: the compositor's half. It answers the second request with
/// the second nonce, only once `Locked`, and never repeats the first.
#[test]
fn req_lock_024_cx_stale_ack() {
    let mut world = World::unlocked();
    world.sleep_request(1);
    world.present();
    assert_eq!(world.releases, [1]);
    world.sleep(true);
    world.comp_see_jump();
    world.present();
    let binding = world.reach_verdict();
    world.verdict(binding, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Unlocked);
    world.sleep_request(2);
    assert_eq!(world.phase(), Phase::Locking);
    assert_eq!(world.releases, [1]);
    world.present();
    assert_eq!(world.releases, [1, 2]);
}

#[test]
fn req_lock_021_cx_no_revoke_on_lock() {
    let mut world = World::unlocked();
    world.reach_grant(Kind::PortalStream);
    world.comp_ask(Kind::AgentTransfer);
    world.comp_lock();
    assert_eq!(world.grants.grants(), []);
    assert_eq!(world.answer(true), None);
}

#[test]
fn req_grant_013_cx_check_at_enqueue_only() {
    let mut world = World::unlocked();
    let grant = world.reach_grant(Kind::PortalStream);
    assert!(world.comp_dispatch(&grant));
    // The next delivery is queued, then the session locks.
    world.comp_lock();
    assert!(!world.comp_dispatch(&grant));
    assert_eq!(world.seen.delivered, [grant.generation]);
}

#[test]
fn req_agent_005_cx_check_at_enqueue_only_agent() {
    let mut world = World::unlocked();
    let grant = world.reach_grant(Kind::AgentTransfer);
    // Two deliveries were queued on the one grant.
    assert!(world.comp_dispatch(&grant));
    assert!(!world.comp_dispatch(&grant));
}

#[test]
fn req_grant_014_cx_grant_by_name() {
    let mut world = World::unlocked();
    let grant = world.reach_grant(Kind::PortalStream);
    // `holderRestart`: the portal reconnects. Nothing tells the reducer.
    world.portal = ConnectionId(2);
    assert!(!world.comp_dispatch(&grant));
}

#[test]
fn req_grant_013_cx_timer_only_expiry() {
    let mut world = World::unlocked();
    let grant = world.reach_grant(Kind::PortalStream);
    // `deadlinePasses`: the timer has not fired yet.
    world.now.monotonic += TTL;
    world.now.boottime += TTL;
    assert!(!world.comp_dispatch(&grant));
}

/// REQ-GRANT-007: also `consentForReplacedWindowGuardTest`.
#[test]
fn req_grant_007_cx_skip_window_gen() {
    for told in [true, false] {
        let mut world = World::unlocked();
        let grant = world.reach_grant(Kind::PortalStream);
        world.window_replaced(told);
        assert!(!world.comp_dispatch(&grant), "told: {told}");
    }
    let mut world = World::unlocked();
    world.comp_ask(Kind::AgentTransfer);
    world.window_replaced(true);
    assert_eq!(world.answer(true), None);
}

#[test]
fn req_auth_017_cx_keep_attempt_on_authd_disconnect() {
    let mut world = World::unlocked();
    world.reach_locked();
    let binding = world.reach_verdict();
    world.authd_restart();
    world.verdict(binding, Outcome::Granted);
    assert_eq!(world.phase(), Phase::Locked);
    assert_eq!(world.lock.view().epoch, binding.epoch);
}

#[test]
fn req_lock_015_cx_ack_stale_flip() {
    let mut world = World::unlocked();
    world.present();
    world.comp_lock();
    assert_eq!(world.comp_render(true), Some(Content::Protected));
    // The kernel drops the pending lock frame at the suspend.
    world.sleep(false);
    world.comp_see_jump();
    world.comp_read_stale_flip();
    assert_eq!(world.phase(), Phase::Locking);
    assert_eq!(world.screen, Some(Content::Session));
    assert!(!world.seen.must_cover);
    world.present();
    assert_eq!(world.phase(), Phase::Locked);
    assert_eq!(world.screen, Some(Content::Protected));
}

#[test]
fn req_grant_008_cx_stop_by_holder() {
    let mut world = World::unlocked();
    let first = world.reach_grant(Kind::PortalStream);
    // `userPressStop` for the first stream is in flight when the second starts.
    let second = world.reach_grant(Kind::PortalStream);
    world.stop(first.generation);
    assert_eq!(world.grants.grants(), [second]);
    assert!(world.comp_dispatch(&second));
    assert!(!world.comp_dispatch(&first));
}

/// REQ-LOCK-018 and REQ-LOCK-026: `knownGapUnlockBetweenAckAndSuspendTest`
/// with the hold. The verdict that was in flight at the release is dropped,
/// so the machine sleeps covered.
#[test]
fn req_lock_018_sleep_hold_closes_gap() {
    let mut world = World::unlocked();
    world.present();
    world.reach_locked();
    let binding = world.reach_verdict();
    world.sleep_request(1);
    assert_eq!(world.releases, [1]);
    world.verdict(binding, Outcome::Granted);
    world.present();
    assert_eq!(world.phase(), Phase::Locked);
    world.sleep(true);
    assert_eq!(world.screen, Some(Content::Protected));
}

/// REQ-LOCK-018 and REQ-LOCK-026: the compositor restarts between the
/// release and the suspend. The new instance opens no attempt.
#[test]
fn req_lock_018_sleep_hold_survives_restart() {
    let mut world = World::unlocked();
    world.sleep_request(1);
    world.present();
    assert_eq!(world.releases, [1]);
    world.comp_restart();
    world.present();
    assert_eq!(world.phase(), Phase::Locked);
    assert_eq!(world.comp_open_attempt(), None);
    world.sleep(true);
    assert_eq!(world.screen, Some(Content::Protected));
}

/// One action of the model, or one fault of its environment.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// The next stretch of the plain path, as in the model's progress tests:
    /// towards `Locked`, then an unlock, then a grant and a delivery.
    Progress,
    Lock(lock::Event),
    Render(bool),
    FlipDone,
    FlipFail,
    StaleFlip,
    OutputOff,
    OutputBack,
    Sleep {
        drain: bool,
    },
    OpenAttempt,
    CloseAttempt,
    UiExit,
    Issue(Outcome),
    /// Delivers the `n`th newest verdict ever issued: late, twice, or stale.
    Deliver(usize),
    AuthdRestart,
    Restart,
    Ask(Kind),
    Answer(bool),
    /// A delivery on the `n`th newest grant.
    Dispatch(usize),
    Stop(usize),
    WindowReplaced {
        told: bool,
    },
    HolderRestart,
    DeadlinePasses,
}

fn action() -> impl Strategy<Value = Action> {
    let outcome = || prop_oneof![3 => Just(Outcome::Granted), 1 => Just(Outcome::Denied)];
    let kind = prop_oneof![
        Just(Kind::PortalStream),
        Just(Kind::AgentTransfer),
        Just(Kind::AgentCapture)
    ];
    let event = prop_oneof![
        3 => Just(lock::Event::LockRequested),
        2 => Just(lock::Event::BridgeConnected),
        1 => (1..4u64).prop_map(|nonce| lock::Event::PrepareForSleep { nonce }),
        1 => Just(lock::Event::InhibitorTimedOut),
        1 => Just(lock::Event::LogindUnlock),
        1 => Just(lock::Event::Tick),
    ];
    prop_oneof![
        12 => Just(Action::Progress),
        9 => event.prop_map(Action::Lock),
        12 => prop::bool::weighted(0.9).prop_map(Action::Render),
        12 => Just(Action::FlipDone),
        1 => Just(Action::FlipFail),
        2 => Just(Action::StaleFlip),
        1 => Just(Action::OutputOff),
        2 => Just(Action::OutputBack),
        2 => any::<bool>().prop_map(|drain| Action::Sleep { drain }),
        10 => Just(Action::OpenAttempt),
        1 => Just(Action::CloseAttempt),
        1 => Just(Action::UiExit),
        8 => outcome().prop_map(Action::Issue),
        10 => (0..3usize).prop_map(Action::Deliver),
        1 => Just(Action::AuthdRestart),
        1 => Just(Action::Restart),
        6 => kind.prop_map(Action::Ask),
        6 => prop::bool::weighted(0.8).prop_map(Action::Answer),
        10 => (0..3usize).prop_map(Action::Dispatch),
        1 => (0..3usize).prop_map(Action::Stop),
        1 => any::<bool>().prop_map(|told| Action::WindowReplaced { told }),
        1 => Just(Action::HolderRestart),
        1 => Just(Action::DeadlinePasses),
    ]
}

impl World {
    fn progress(&mut self) {
        match self.phase() {
            Phase::Locking | Phase::Locked if !self.output_on => self.output_back(),
            Phase::Locking if self.flip.is_some() => {
                self.comp_read_stale_flip();
                self.kms_flip_done();
            }
            Phase::Locking => {
                self.comp_render(true);
            }
            Phase::Locked => match self.comp_open_attempt() {
                Some(binding) => {
                    self.authd_issue(Outcome::Granted);
                    self.verdict(binding, Outcome::Granted);
                }
                // In the hold, or an attempt is open already: start over.
                None => {
                    self.bridge_report();
                }
            },
            Phase::Unlocked => {
                let kinds = [Kind::PortalStream, Kind::AgentTransfer];
                self.comp_ask(kinds[self.issued.len() % 2]);
                if let Some(grant) = self.answer(true) {
                    self.comp_dispatch(&grant);
                }
            }
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Progress => self.progress(),
            Action::Lock(event) => {
                self.lock_event(event);
            }
            Action::UiExit => {
                self.lock_event(lock::Event::UiExited { ui: self.ui });
            }
            Action::Render(ok) => {
                self.comp_render(ok);
            }
            Action::FlipDone => self.kms_flip_done(),
            Action::FlipFail => self.kms_flip_fail(),
            Action::StaleFlip => self.comp_read_stale_flip(),
            Action::OutputOff => self.output_off(),
            Action::OutputBack => self.output_back(),
            Action::Sleep { drain } => self.sleep(drain),
            Action::OpenAttempt => {
                self.comp_open_attempt();
            }
            Action::CloseAttempt => self.comp_close_attempt(),
            Action::Issue(outcome) => self.authd_issue(outcome),
            Action::Deliver(nth) => {
                if let Some(&(binding, outcome)) = self.verdicts.iter().rev().nth(nth) {
                    self.verdict(binding, outcome);
                }
            }
            Action::AuthdRestart => self.authd_restart(),
            Action::Restart => self.comp_restart(),
            Action::Ask(kind) => self.comp_ask(kind),
            Action::Answer(allow) => {
                self.answer(allow);
            }
            Action::Dispatch(nth) => {
                if let Some(grant) = self.issued.iter().rev().nth(nth).copied() {
                    self.comp_dispatch(&grant);
                }
            }
            Action::Stop(nth) => {
                if let Some(grant) = self.issued.iter().rev().nth(nth) {
                    self.stop(grant.generation);
                }
            }
            Action::WindowReplaced { told } => self.window_replaced(told),
            Action::HolderRestart => self.portal.0 += 1,
            Action::DeadlinePasses => {
                self.now.monotonic += TTL;
                self.now.boottime += TTL;
            }
        }
    }
}

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: 1024,
        rng_seed: RngSeed::Fixed(0x534b_4f52_4d4f_444c),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    /// REQ-LOCK-001, REQ-AUTH-001, REQ-GRANT-001, REQ-AGENT-001: the model's
    /// safety invariants hold after every step of a random walk through its
    /// actions and faults. The assertions are in [`World`].
    #[test]
    fn req_lock_001_safety_holds_on_random_walks(actions in vec(action(), 0..200)) {
        let mut world = World::start();
        for action in actions {
            world.act(action);
        }
    }
}

/// The walks reach what the model's witnesses name: an unlock, a second
/// unlock and a delivery. Without this the invariants could hold vacuously.
#[test]
fn req_lock_001_random_walks_reach_the_witnesses() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::new(proptest_config());
    let (mut unlocks, mut wakes, mut deliveries) = (0, 0, 0);
    for _ in 0..256 {
        let actions = vec(action(), 0..200)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let mut world = World::start();
        for action in actions {
            world.act(action);
        }
        unlocks += usize::from(!world.seen.used.is_empty());
        wakes += usize::from(world.seen.used.len() > 1);
        deliveries += usize::from(world.seen.deliveries > 0);
    }
    assert!(unlocks > 128, "{unlocks} of 256 walks unlocked");
    assert!(wakes > 100, "{wakes} of 256 walks unlocked twice");
    assert!(deliveries > 100, "{deliveries} of 256 walks delivered");
}
