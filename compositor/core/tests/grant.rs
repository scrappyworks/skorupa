//! The grant reducer through its public API: one test per requirement, then
//! properties C and D over arbitrary event sequences.
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
use skorupa_compositor_core::grant::{
    Cause, ConnectionId, Effect, Event, Grant, Input, Kind, MAX_GRANTS, MAX_QUEUED, PromptId,
    Request, State, Target, WindowRef, step,
};
use skorupa_compositor_core::lock::{Clocks, InstanceId, LockView, SleepWatch};

const INSTANCE: InstanceId = InstanceId([7; 16]);
const OTHER_INSTANCE: InstanceId = InstanceId([8; 16]);
const THRESHOLD: Duration = Duration::from_secs(1);
const BOOT: Clocks = Clocks {
    monotonic: Duration::from_secs(100),
    boottime: Duration::from_secs(100),
};
const TICK: Duration = Duration::from_millis(1);
const TTL: Duration = Duration::from_secs(60);
const PORTAL: ConnectionId = ConnectionId(10);
const AGENT: ConnectionId = ConnectionId(20);
const WINDOW_REF: WindowRef = WindowRef {
    id: 4,
    generation: 1,
};
const WINDOW: Target = Target::Window(WINDOW_REF);

const fn request(kind: Kind, connection: ConnectionId, target: Target) -> Request {
    Request {
        kind,
        connection,
        target,
        ttl: TTL,
    }
}

const STREAM: Request = request(Kind::PortalStream, PORTAL, WINDOW);
const TRANSFER: Request = request(Kind::AgentTransfer, AGENT, WINDOW);
const CAPTURE: Request = request(Kind::AgentCapture, AGENT, WINDOW);

/// The grant table of one compositor instance, its clocks and the lock state
/// each event is judged against.
struct Rig {
    state: State,
    now: Clocks,
    lock: LockView,
    /// The prompt shown last.
    prompt: Option<PromptId>,
}

impl Rig {
    fn new(agent_capture: bool) -> Self {
        Self {
            state: State::new(INSTANCE, SleepWatch::new(BOOT, THRESHOLD), agent_capture),
            now: BOOT,
            lock: LockView {
                unlocked: true,
                epoch: 1,
            },
            prompt: None,
        }
    }

    fn v1() -> Self {
        Self::new(false)
    }

    fn send(&mut self, event: Event) -> Vec<Effect> {
        self.pass(TICK);
        self.step(event)
    }

    /// One step at the current clock readings.
    fn step(&mut self, event: Event) -> Vec<Effect> {
        let input = Input {
            now: self.now,
            lock: self.lock,
            event,
        };
        let (state, effects) = step(self.state.clone(), input);
        self.state = state;
        for effect in &effects {
            if let Effect::ShowPrompt { prompt, .. } = effect {
                self.prompt = Some(*prompt);
            }
        }
        effects
    }

    fn answer(&mut self, allow: bool) -> Vec<Effect> {
        let prompt = self.prompt.unwrap();
        self.send(Event::Answered { prompt, allow })
    }

    /// The request is shown to the user, who allows it.
    fn grant(&mut self, request: Request) -> Grant {
        self.send(Event::Requested(request));
        let granted = self
            .answer(true)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Granted(grant) => Some(grant),
                _ => None,
            });
        granted.unwrap()
    }

    /// One delivery to the grant's own connection and target, as the adapter
    /// asks immediately before the data leaves. Returns whether it may leave.
    fn deliver(&mut self, grant: &Grant) -> bool {
        let generation = grant.generation;
        let effects = self.send(Event::Use {
            generation,
            connection: grant.connection,
            target: grant.target,
        });
        effects.contains(&Effect::Deliver { generation })
    }

    fn pass(&mut self, time: Duration) {
        self.now.monotonic += time;
        self.now.boottime += time;
    }

    fn live(&self) -> Vec<u64> {
        let grants = self.state.grants().iter();
        grants.map(|grant| grant.generation).collect()
    }
}

#[test]
fn req_grant_015_answer_grants_only_for_the_open_prompt() {
    let mut rig = Rig::v1();
    let shown = rig.send(Event::Requested(STREAM));
    let prompt = PromptId {
        instance: INSTANCE,
        number: 1,
    };
    assert_eq!(
        shown,
        [Effect::ShowPrompt {
            prompt,
            request: STREAM
        }]
    );
    let stale = [
        PromptId {
            number: 2,
            ..prompt
        },
        PromptId {
            instance: OTHER_INSTANCE,
            ..prompt
        },
    ];
    for prompt in stale {
        let allow = true;
        assert_eq!(rig.send(Event::Answered { prompt, allow }), []);
    }
    let granted = rig.answer(true);
    let expected = Grant {
        generation: 1,
        kind: Kind::PortalStream,
        connection: PORTAL,
        target: WINDOW,
        expires: rig.now.boottime + TTL,
        epoch: 1,
    };
    assert_eq!(granted, [Effect::Granted(expected)]);
    // The answer consumed the prompt: a replay creates nothing.
    assert_eq!(rig.answer(true), []);
    assert_eq!(rig.live(), [1]);
}

#[test]
fn req_grant_015_answer_while_locked_grants_nothing() {
    let mut rig = Rig::v1();
    rig.send(Event::Requested(STREAM));
    rig.lock.unlocked = false;
    let prompt = rig.prompt.unwrap();
    let request = STREAM;
    let cause = Cause::Lock;
    assert_eq!(
        rig.answer(true),
        [
            Effect::ClosePrompt { prompt },
            Effect::Denied { request, cause }
        ]
    );
    assert_eq!(
        rig.send(Event::Requested(STREAM)),
        [Effect::Denied { request, cause }]
    );
    rig.lock.unlocked = true;
    assert_eq!(rig.answer(true), []);
    assert_eq!(rig.live(), []);
}

/// REQ-GRANT-013: the four things read at the last point the compositor
/// controls. Each change happens after the grant and reaches the reducer only
/// with the delivery itself: no revocation event, no timer.
#[test]
fn req_grant_013_delivery_is_checked_when_it_leaves() {
    let mut rig = Rig::v1();
    let grant = rig.grant(STREAM);
    assert!(rig.deliver(&grant));
    assert!(rig.deliver(&grant), "a stream delivers more than once");

    let reconnected = Grant {
        connection: ConnectionId(11),
        ..grant
    };
    assert!(!rig.deliver(&reconnected));
    let replaced = Target::Window(WindowRef {
        id: 4,
        generation: 2,
    });
    assert!(!rig.deliver(&Grant {
        target: replaced,
        ..grant
    }));
    assert!(!rig.deliver(&Grant {
        target: Target::Screen,
        ..grant
    }));
    assert!(rig.deliver(&grant), "a refused delivery ends nothing");

    let mut locked = Rig::v1();
    let grant = locked.grant(STREAM);
    locked.lock.unlocked = false;
    assert!(!locked.deliver(&grant));

    let mut late = Rig::v1();
    let grant = late.grant(STREAM);
    late.pass(Duration::from_secs(59));
    assert!(late.deliver(&grant), "the deadline has not passed yet");
    late.pass(Duration::from_secs(1));
    assert!(!late.deliver(&grant));
}

#[test]
fn req_grant_014_grant_belongs_to_one_connection() {
    let mut rig = Rig::v1();
    let grant = rig.grant(STREAM);
    let cause = Cause::ConnectionClosed;
    assert_eq!(
        rig.send(Event::ConnectionClosed(PORTAL)),
        [Effect::Revoked { grant, cause }]
    );
    assert!(!rig.deliver(&grant));
    // A prompt asked for by the closed connection ends with it too.
    rig.send(Event::Requested(STREAM));
    rig.send(Event::Requested(TRANSFER));
    let closed = rig.send(Event::ConnectionClosed(PORTAL));
    assert!(closed.contains(&Effect::Denied {
        request: STREAM,
        cause
    }));
    let granted = rig.answer(true);
    let connection = AGENT;
    assert!(matches!(
        granted[..],
        [Effect::Granted(Grant { connection: holder, .. })] if holder == connection
    ));
}

#[test]
fn req_grant_007_grant_and_consent_end_with_the_window_generation() {
    let mut rig = Rig::v1();
    let grant = rig.grant(STREAM);
    let window = WINDOW_REF;
    let other = WindowRef {
        generation: 2,
        ..window
    };
    assert_eq!(rig.send(Event::WindowGone(other)), []);
    let cause = Cause::WindowGone;
    assert_eq!(
        rig.send(Event::WindowGone(window)),
        [Effect::Revoked { grant, cause }]
    );
    assert!(!rig.deliver(&grant));
    // Consent given for a window that was replaced while the prompt was open.
    rig.send(Event::Requested(TRANSFER));
    let prompt = rig.prompt.unwrap();
    let request = TRANSFER;
    assert_eq!(
        rig.send(Event::WindowGone(window)),
        [
            Effect::ClosePrompt { prompt },
            Effect::Denied { request, cause }
        ]
    );
    assert_eq!(rig.answer(true), []);
    assert_eq!(rig.live(), []);
}

/// REQ-GRANT-008, REQ-CAP-006: Stop names one stream generation of this
/// instance, and after it nothing is delivered.
#[test]
fn req_grant_008_stop_ends_exactly_the_named_generation() {
    let mut rig = Rig::v1();
    let first = rig.grant(STREAM);
    let stop = |generation| Event::Stop {
        instance: INSTANCE,
        generation,
    };
    let cause = Cause::Stop;
    let grant = first;
    assert_eq!(
        rig.send(stop(first.generation)),
        [Effect::Revoked { grant, cause }]
    );
    let second = rig.grant(STREAM);
    assert_ne!(first.generation, second.generation);
    // The Stop for the first stream arrives again, and one from an earlier
    // compositor instance names the number of the second.
    assert_eq!(rig.send(stop(first.generation)), []);
    let instance = OTHER_INSTANCE;
    let generation = second.generation;
    assert_eq!(
        rig.send(Event::Stop {
            instance,
            generation
        }),
        []
    );
    assert!(rig.deliver(&second));
    rig.send(stop(second.generation));
    assert!(!rig.deliver(&second));
}

/// Changes the world of a rig and returns the event that tells the reducer.
type Ending = fn(&mut Rig) -> Event;

/// REQ-GRANT-006: lock, suspend, restart, window gone, expiry and a failed
/// broker each end a live stream. REQ-LOCK-021 is the first of them.
#[test]
fn req_grant_006_each_ending_closes_a_live_stream() {
    let endings: [(Cause, Ending); 5] = [
        (Cause::Lock, |_| Event::Lock),
        (Cause::Suspend, |rig| {
            rig.now.boottime += Duration::from_secs(3600);
            Event::Tick
        }),
        (Cause::WindowGone, |_| Event::WindowGone(WINDOW_REF)),
        (Cause::Expired, |rig| {
            rig.pass(TTL);
            Event::Tick
        }),
        (Cause::ConnectionClosed, |_| Event::ConnectionClosed(PORTAL)),
    ];
    for (cause, ending) in endings {
        let mut rig = Rig::v1();
        let grant = rig.grant(STREAM);
        assert_eq!(rig.state.next_expiry(), Some(grant.expires));
        let event = ending(&mut rig);
        assert_eq!(rig.send(event), [Effect::Revoked { grant, cause }]);
        assert!(!rig.deliver(&grant), "{cause:?}");
        assert_eq!(rig.state.next_expiry(), None);
    }
    // Restart: the new instance has an empty table.
    let mut old = Rig::v1();
    let grant = old.grant(STREAM);
    assert!(!Rig::v1().deliver(&grant));
}

/// REQ-GRANT-005: one event ends the stream, the pending transfer and the
/// prompts, before any other event is handled.
#[test]
fn req_grant_005_one_step_revokes_everything() {
    let mut rig = Rig::v1();
    let stream = rig.grant(STREAM);
    let transfer = rig.grant(TRANSFER);
    rig.send(Event::Requested(STREAM));
    rig.send(Event::Requested(TRANSFER));
    let prompt = rig.prompt.unwrap();
    let cause = Cause::Lock;
    let effects = rig.send(Event::Lock);
    assert_eq!(
        effects,
        [
            Effect::ClosePrompt { prompt },
            Effect::Denied {
                request: STREAM,
                cause
            },
            Effect::Denied {
                request: TRANSFER,
                cause
            },
            Effect::Revoked {
                grant: stream,
                cause
            },
            Effect::Revoked {
                grant: transfer,
                cause
            },
        ]
    );
    assert_eq!(rig.live(), []);
    assert_eq!(rig.answer(true), []);
}

/// REQ-LOCK-021: a grant carries the epoch it was made in, so it is dead
/// after a lock even if this reducer saw no event while the session was locked.
#[test]
fn req_lock_021_grant_does_not_outlive_a_lock_it_never_saw() {
    let mut rig = Rig::v1();
    let grant = rig.grant(STREAM);
    rig.lock.epoch += 1;
    let generation = grant.generation;
    let cause = Cause::Lock;
    assert_eq!(
        rig.send(Event::Use {
            generation,
            connection: grant.connection,
            target: grant.target
        }),
        [
            Effect::Revoked { grant, cause },
            Effect::Withhold { generation }
        ]
    );
}

/// REQ-GRANT-001: the table is bounded, and a full table denies.
#[test]
fn req_grant_001_grant_table_is_bounded() {
    let mut rig = Rig::v1();
    for _ in 0..MAX_GRANTS {
        rig.grant(STREAM);
    }
    rig.send(Event::Requested(STREAM));
    let request = STREAM;
    let cause = Cause::Full;
    assert_eq!(rig.answer(true), [Effect::Denied { request, cause }]);
    assert_eq!(rig.state.grants().len(), MAX_GRANTS);
}

/// REQ-TRUST-012: one prompt at a time; the rest wait in a bounded queue and
/// are shown in order.
#[test]
fn req_trust_012_one_prompt_at_a_time_with_a_bounded_queue() {
    let mut rig = Rig::v1();
    let first = rig.send(Event::Requested(STREAM));
    assert!(matches!(first[..], [Effect::ShowPrompt { .. }]));
    for _ in 0..MAX_QUEUED {
        assert_eq!(rig.send(Event::Requested(TRANSFER)), []);
    }
    let request = STREAM;
    let cause = Cause::Full;
    assert_eq!(
        rig.send(Event::Requested(STREAM)),
        [Effect::Denied { request, cause }]
    );
    let answered = rig.answer(false);
    let cause = Cause::Declined;
    let prompt = PromptId {
        instance: INSTANCE,
        number: 2,
    };
    let request = TRANSFER;
    assert_eq!(
        answered,
        [
            Effect::Denied {
                request: STREAM,
                cause
            },
            Effect::ShowPrompt { prompt, request }
        ]
    );
}

#[test]
fn req_trust_010_prompt_that_exits_without_an_answer_denies() {
    let mut rig = Rig::v1();
    rig.send(Event::Requested(STREAM));
    let prompt = rig.prompt.unwrap();
    let request = STREAM;
    let cause = Cause::PromptExited;
    assert_eq!(
        rig.send(Event::PromptExited { prompt }),
        [Effect::Denied { request, cause }]
    );
    assert_eq!(rig.answer(true), []);
}

/// REQ-AGENT-001: in v1 no grant for agent capture of a host window exists,
/// so nothing can be delivered on one.
#[test]
fn req_agent_001_agent_capture_is_refused_in_v1() {
    let mut rig = Rig::v1();
    let request = CAPTURE;
    let cause = Cause::NotOffered;
    assert_eq!(
        rig.send(Event::Requested(CAPTURE)),
        [Effect::Denied { request, cause }]
    );
    assert_eq!(rig.prompt, None);
    let guess = Grant {
        generation: 1,
        kind: Kind::AgentCapture,
        connection: AGENT,
        target: WINDOW,
        expires: rig.now.boottime + TTL,
        epoch: 1,
    };
    assert!(!rig.deliver(&guess));
}

/// REQ-CAP-011 (after v1): with agent capture offered, the grant is for one
/// capture of one window generation.
#[test]
fn req_cap_011_agent_capture_is_one_shot_when_offered() {
    let mut rig = Rig::new(true);
    let grant = rig.grant(CAPTURE);
    assert!(rig.deliver(&grant));
    assert!(!rig.deliver(&grant));
    let grant = rig.grant(CAPTURE);
    rig.send(Event::WindowGone(WINDOW_REF));
    assert!(!rig.deliver(&grant));
}

#[test]
fn req_agent_005_transfer_is_delivered_once() {
    let mut rig = Rig::v1();
    let grant = rig.grant(TRANSFER);
    let generation = grant.generation;
    let cause = Cause::Used;
    let used = rig.send(Event::Use {
        generation,
        connection: AGENT,
        target: WINDOW,
    });
    assert_eq!(
        used,
        [
            Effect::Deliver { generation },
            Effect::Revoked { grant, cause }
        ]
    );
    assert!(!rig.deliver(&grant));
}

#[test]
fn req_agent_006_kill_switch_revokes_agent_grants_before_it_freezes() {
    let mut rig = Rig::v1();
    let stream = rig.grant(STREAM);
    let grant = rig.grant(TRANSFER);
    rig.send(Event::Requested(TRANSFER));
    let prompt = rig.prompt.unwrap();
    let request = TRANSFER;
    let cause = Cause::KillSwitch;
    assert_eq!(
        rig.send(Event::KillSwitch),
        [
            Effect::ClosePrompt { prompt },
            Effect::Denied { request, cause },
            Effect::Revoked { grant, cause },
            Effect::FreezeAgents
        ]
    );
    assert!(rig.deliver(&stream), "the user's own share goes on");
}

/// One step of a generated sequence. Identifiers come from small sets, so
/// that stale and current ones collide.
#[derive(Clone, Copy, Debug)]
enum Op {
    Wait(Duration),
    Sleep(Duration),
    /// The session locks; `told` says whether the reducer gets the event.
    Lock {
        told: bool,
    },
    Unlock,
    Request(Request),
    /// `stale` answers an earlier prompt or one of another instance.
    Answer {
        allow: bool,
        stale: Option<bool>,
    },
    PromptExited,
    /// A delivery on the `nth` newest grant; `own` uses its connection and
    /// target, otherwise the given ones.
    Use {
        nth: usize,
        own: bool,
        connection: ConnectionId,
        target: Target,
    },
    Stop {
        nth: usize,
        own_instance: bool,
    },
    Revoke {
        nth: usize,
    },
    Plain(Event),
}

fn connection() -> impl Strategy<Value = ConnectionId> {
    (1..=3u64).prop_map(ConnectionId)
}

fn window() -> impl Strategy<Value = WindowRef> {
    (1..=2u64, 1..=2u32).prop_map(|(id, generation)| WindowRef { id, generation })
}

fn target() -> impl Strategy<Value = Target> {
    prop_oneof![1 => Just(Target::Screen), 4 => window().prop_map(Target::Window)]
}

fn op() -> impl Strategy<Value = Op> {
    let kind = prop_oneof![
        Just(Kind::PortalStream),
        Just(Kind::AgentTransfer),
        Just(Kind::AgentCapture)
    ];
    let ttl = prop_oneof![1 => Just(0), 2 => Just(5), 4 => Just(3600)];
    let ttl = ttl.prop_map(Duration::from_secs);
    // Mostly the newest grant, which is the one most likely to be live.
    let nth = || prop_oneof![3 => Just(0), 1 => 1..6usize];
    let request =
        (kind, connection(), target(), ttl).prop_map(|(kind, connection, target, ttl)| Request {
            kind,
            connection,
            target,
            ttl,
        });
    let stale = prop_oneof![8 => Just(None), 1 => Just(Some(false)), 1 => Just(Some(true))];
    let plain = prop_oneof![
        2 => Just(Event::Tick),
        1 => Just(Event::KillSwitch),
        2 => window().prop_map(Event::WindowGone),
        2 => connection().prop_map(Event::ConnectionClosed),
    ];
    prop_oneof![
        2 => (0..4_000u64).prop_map(|ms| Op::Wait(Duration::from_millis(ms))),
        1 => (0..1_500u64).prop_map(|ms| Op::Sleep(Duration::from_millis(ms))),
        1 => any::<bool>().prop_map(|told| Op::Lock { told }),
        3 => Just(Op::Unlock),
        8 => request.prop_map(Op::Request),
        8 => (prop::bool::weighted(0.8), stale).prop_map(|(allow, stale)| Op::Answer { allow, stale }),
        1 => Just(Op::PromptExited),
        12 => (nth(), prop::bool::weighted(0.8), connection(), target()).prop_map(
            |(nth, own, connection, target)| Op::Use { nth, own, connection, target }
        ),
        2 => (nth(), prop::bool::weighted(0.8))
            .prop_map(|(nth, own_instance)| Op::Stop { nth, own_instance }),
        1 => nth().prop_map(|nth| Op::Revoke { nth }),
        6 => plain.prop_map(Op::Plain),
    ]
}

/// An independent record of which grants exist and which have ended, kept
/// from the inputs and from the `Granted` and `ShowPrompt` effects alone.
struct Walk {
    rig: Rig,
    watch: SleepWatch,
    issued: Vec<Grant>,
    dead: HashSet<u64>,
    prompt: Option<(PromptId, Request)>,
    prompts: u64,
    deliveries: usize,
}

impl Walk {
    fn new(agent_capture: bool) -> Self {
        Self {
            rig: Rig::new(agent_capture),
            watch: SleepWatch::new(BOOT, THRESHOLD),
            issued: Vec::new(),
            dead: HashSet::new(),
            prompt: None,
            prompts: 0,
            deliveries: 0,
        }
    }

    /// The `nth` newest grant ever issued.
    fn pick(&self, nth: usize) -> Option<&Grant> {
        self.issued.iter().rev().nth(nth)
    }

    fn nth(&self, nth: usize) -> u64 {
        // Past the newest stands for a grant that does not exist.
        let beyond = (self.issued.len() + nth) as u64 + 1;
        self.pick(nth).map_or(beyond, |grant| grant.generation)
    }

    fn event(&mut self, op: Op) -> Option<Event> {
        let rig = &mut self.rig;
        Some(match op {
            Op::Wait(time) => {
                rig.pass(time);
                return None;
            }
            Op::Sleep(time) => {
                rig.now.boottime += time;
                return None;
            }
            Op::Lock { told } => {
                if rig.lock.unlocked {
                    rig.lock.unlocked = false;
                    rig.lock.epoch += 1;
                }
                if !told {
                    return None;
                }
                Event::Lock
            }
            Op::Unlock => {
                rig.lock.unlocked = true;
                return None;
            }
            Op::Request(request) => Event::Requested(request),
            Op::Answer { allow, stale } => {
                let current = rig.prompt.unwrap_or(PromptId {
                    instance: INSTANCE,
                    number: 1,
                });
                let prompt = match stale {
                    None => current,
                    Some(false) => PromptId {
                        number: current.number.saturating_sub(1),
                        ..current
                    },
                    Some(true) => PromptId {
                        instance: OTHER_INSTANCE,
                        ..current
                    },
                };
                Event::Answered { prompt, allow }
            }
            Op::PromptExited => Event::PromptExited {
                prompt: rig.prompt?,
            },
            Op::Use {
                nth,
                own,
                connection,
                target,
            } => {
                let generation = self.nth(nth);
                match self.pick(nth).filter(|_| own) {
                    Some(grant) => Event::Use {
                        generation,
                        connection: grant.connection,
                        target: grant.target,
                    },
                    None => Event::Use {
                        generation,
                        connection,
                        target,
                    },
                }
            }
            Op::Stop { nth, own_instance } => Event::Stop {
                instance: if own_instance {
                    INSTANCE
                } else {
                    OTHER_INSTANCE
                },
                generation: self.nth(nth),
            },
            Op::Revoke { nth } => Event::Revoke {
                generation: self.nth(nth),
            },
            Op::Plain(event) => event,
        })
    }

    /// Which grants the inputs of this step end, whatever the reducer does.
    fn ended_by(&self, event: Event, slept: bool) -> HashSet<u64> {
        let (now, lock) = (self.rig.now, self.rig.lock);
        let ends = |grant: &Grant| {
            let target = grant.target;
            slept
                || !lock.unlocked
                || grant.epoch != lock.epoch
                || now.boottime >= grant.expires
                || match event {
                    Event::Lock => true,
                    Event::KillSwitch => grant.kind != Kind::PortalStream,
                    Event::Stop {
                        instance,
                        generation,
                    } => instance == INSTANCE && generation == grant.generation,
                    Event::Revoke { generation } => generation == grant.generation,
                    Event::WindowGone(window) => target == Target::Window(window),
                    Event::ConnectionClosed(connection) => connection == grant.connection,
                    _ => false,
                }
        };
        let live = self
            .issued
            .iter()
            .filter(|grant| !self.dead.contains(&grant.generation));
        live.filter(|grant| ends(grant))
            .map(|grant| grant.generation)
            .collect()
    }

    /// Properties C and D for one answer to a delivery: it leaves under a
    /// grant that was issued and has not ended, on that grant's connection
    /// and target, and it is withheld only when one of those fails. Returns
    /// the grant that the delivery used up.
    fn check_delivery(
        &mut self,
        event: Event,
        generation: u64,
        delivered: bool,
    ) -> Result<Option<u64>, TestCaseError> {
        let claimed = |grant: &&Grant| {
            let used = Event::Use {
                generation,
                connection: grant.connection,
                target: grant.target,
            };
            grant.generation == generation && event == used
        };
        let live = self.issued.iter().find(claimed).copied();
        let live = live.filter(|_| !self.dead.contains(&generation));
        prop_assert_eq!(live.is_some(), delivered, "{:?}", event);
        prop_assert!(!delivered || self.rig.lock.unlocked);
        self.deliveries += usize::from(delivered);
        // REQ-AGENT-005: an agent's grant is used up by one delivery.
        let used_up = live.filter(|grant| grant.kind != Kind::PortalStream);
        self.dead.extend(used_up.map(|grant| grant.generation));
        Ok(used_up.map(|grant| grant.generation))
    }

    /// REQ-GRANT-015: a grant needs the user's allow for the open prompt of
    /// this instance, while unlocked, and is what that prompt asked for.
    fn check_grant(
        &mut self,
        event: Event,
        answered: Option<(PromptId, Request)>,
        grant: Grant,
    ) -> Result<(), TestCaseError> {
        let (prompt, request) = answered.ok_or_else(|| TestCaseError::fail("no prompt"))?;
        let allow = true;
        prop_assert_eq!(event, Event::Answered { prompt, allow });
        let newest = self.issued.last().map_or(0, |grant| grant.generation);
        let expected = Grant {
            generation: newest + 1,
            kind: request.kind,
            connection: request.connection,
            target: request.target,
            expires: self.rig.now.boottime + request.ttl,
            epoch: self.rig.lock.epoch,
        };
        prop_assert_eq!(grant, expected);
        prop_assert!(self.rig.lock.unlocked);
        self.issued.push(grant);
        Ok(())
    }

    /// Runs one step and checks properties C and D on it.
    fn run(&mut self, op: Op, agent_capture: bool) -> Result<(), TestCaseError> {
        self.rig.pass(TICK);
        let Some(event) = self.event(op) else {
            return Ok(());
        };
        let (now, lock) = (self.rig.now, self.rig.lock);
        let slept = self.watch.slept(now);
        let mut ended = self.ended_by(event, slept);
        // The prompt this event answers, unless the sleep or the lock closes it first.
        let answered = match event {
            Event::Answered { prompt, .. } | Event::PromptExited { prompt }
                if !slept && lock.unlocked =>
            {
                self.prompt.take_if(|(open, _)| *open == prompt)
            }
            _ => None,
        };
        let effects = self.rig.step(event);
        prop_assert!(effects.len() <= MAX_GRANTS + MAX_QUEUED + 4, "{effects:?}");
        self.dead.extend(&ended);

        let asked = matches!(event, Event::Use { .. });
        let mut answers = 0;
        for (index, effect) in effects.iter().enumerate() {
            match *effect {
                Effect::Deliver { generation } | Effect::Withhold { generation } => {
                    let delivered = matches!(effect, Effect::Deliver { .. });
                    ended.extend(self.check_delivery(event, generation, delivered)?);
                    answers += 1;
                }
                Effect::Granted(grant) => self.check_grant(event, answered, grant)?,
                // REQ-TRUST-012: one prompt at a time, numbered once.
                Effect::ShowPrompt { prompt, request } => {
                    prop_assert_eq!(self.prompt, None);
                    self.prompts += 1;
                    let instance = INSTANCE;
                    let number = self.prompts;
                    prop_assert_eq!(prompt, PromptId { instance, number });
                    // REQ-AGENT-001: in v1 nobody is even asked.
                    prop_assert!(agent_capture || request.kind != Kind::AgentCapture);
                    prop_assert!(lock.unlocked);
                    self.prompt = Some((prompt, request));
                }
                Effect::ClosePrompt { prompt } => {
                    let open = self.prompt.take().map(|(open, _)| open);
                    prop_assert_eq!(open, Some(prompt));
                }
                Effect::Denied { .. } => {}
                // Every end is reported once, in the step that caused it.
                Effect::Revoked { grant, .. } => {
                    prop_assert!(ended.remove(&grant.generation), "{effect:?}");
                }
                // REQ-AGENT-006: the freeze comes after the revocations.
                Effect::FreezeAgents => {
                    prop_assert_eq!(event, Event::KillSwitch);
                    let later = effects.iter().skip(index);
                    let revoked = later.filter(|e| matches!(e, Effect::Revoked { .. }));
                    prop_assert_eq!(revoked.count(), 0);
                }
            }
        }
        prop_assert!(ended.is_empty(), "ended without a report: {ended:?}");
        prop_assert_eq!(answers, usize::from(asked));
        // Property C as an equation: the table is what was issued minus what ended.
        let expected = self.issued.iter().map(|grant| grant.generation);
        let expected: Vec<u64> = expected.filter(|g| !self.dead.contains(g)).collect();
        prop_assert_eq!(self.rig.live(), expected);
        prop_assert!(self.rig.state.grants().len() <= MAX_GRANTS);
        let soonest = self
            .rig
            .state
            .grants()
            .iter()
            .map(|grant| grant.expires)
            .min();
        prop_assert_eq!(self.rig.state.next_expiry(), soonest);
        Ok(())
    }
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 1024,
        rng_seed: RngSeed::Fixed(0x534b_4f52_4752_4e54),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(config())]

    /// REQ-GRANT-001, REQ-AGENT-001: properties C and D on every step of
    /// arbitrary event sequences, in v1 and with agent capture offered.
    #[test]
    fn req_grant_001_no_delivery_without_a_live_grant(
        ops in vec(op(), 0..150),
        agent_capture: bool,
    ) {
        let mut walk = Walk::new(agent_capture);
        for op in ops {
            walk.run(op, agent_capture)?;
        }
    }
}

/// The generated sequences do issue grants, deliver on them and end them;
/// without this the properties above could hold vacuously.
#[test]
fn req_grant_001_generated_sequences_reach_the_interesting_states() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::new(config());
    let (mut grants, mut deliveries, mut ends) = (0, 0, 0);
    for _ in 0..256 {
        let ops = vec(op(), 0..150).new_tree(&mut runner).unwrap().current();
        let mut walk = Walk::new(false);
        for op in ops {
            walk.run(op, false).unwrap();
        }
        grants += usize::from(walk.issued.len() > 1);
        deliveries += usize::from(walk.deliveries > 1);
        ends += usize::from(walk.dead.len() > 1);
    }
    assert!(grants > 128, "{grants} of 256 sequences issued two grants");
    assert!(deliveries > 64, "{deliveries} of 256 delivered twice");
    assert!(ends > 100, "{ends} of 256 ended two grants");
}
