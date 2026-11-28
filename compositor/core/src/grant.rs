//! Grants: who may capture or control what, and until when.
//!
//! A grant exists only after the user allowed one consent prompt. It is bound
//! to the connection that asked, to one target, to a deadline on the
//! boot-time clock and to the lock epoch it was made in, and it is named by a
//! stream generation that is never used twice by one compositor instance.
//!
//! - **Property C.** Nothing is delivered on a grant that has ended. Ending a
//!   grant in the table cannot stop an effect that already left the reducer,
//!   so the adapter sends [`Event::Use`] immediately before data leaves and
//!   delivers only on [`Effect::Deliver`]. That step reads the connection, the
//!   target, the clock and the lock state at that moment.
//! - **Property D.** An agent's grant is used up by one delivery, and in v1
//!   no grant for agent capture of a host window is ever made.
//!
//! Every event carries both clocks and the current lock state, so a sleep, a
//! lock and a passed deadline end grants before the event itself is judged,
//! whether or not a timer or the lock reducer reported them first.

use core::time::Duration;
use std::collections::VecDeque;

use crate::lock::{ClockChange, Clocks, InstanceId, LockView, SleepWatch};

/// Most grants that can be live at once. A full table denies.
pub const MAX_GRANTS: usize = 16;

/// Most consent requests that can wait behind the open prompt.
pub const MAX_QUEUED: usize = 4;

/// The most time an unanswered consent prompt can remain open.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// One client connection. The adapter never reuses a value within one
/// compositor instance, so a holder that reconnects is a stranger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

/// The compositor's identity of a window. The generation changes when the
/// window behind the identifier is replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowRef {
    /// Identifier, which may be reused.
    pub id: u64,
    /// Which window carries the identifier.
    pub generation: u32,
}

/// What a grant covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The one screen.
    Screen,
    /// One window generation.
    Window(WindowRef),
}

/// What is delivered under a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Frames of a portal screen cast, until the grant ends.
    PortalStream,
    /// One clipboard item between the user and an agent.
    AgentTransfer,
    /// One capture of a host window by an agent. Not offered in v1.
    AgentCapture,
}

impl Kind {
    /// Whether the holder is an agent. Its grants are used up by one delivery.
    #[must_use]
    pub const fn is_agent(self) -> bool {
        !matches!(self, Self::PortalStream)
    }
}

/// What a client asks the user to allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// What would be delivered.
    pub kind: Kind,
    /// Who asks, and the only connection that would receive it.
    pub connection: ConnectionId,
    /// What it covers.
    pub target: Target,
    /// How long the grant lasts from the moment the user allows it.
    pub ttl: Duration,
}

/// One consent prompt. Numbers restart with the compositor, so the instance
/// is part of the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PromptId {
    /// The compositor process that opened the prompt.
    pub instance: InstanceId,
    /// Number of the prompt within that process.
    pub number: u64,
}

/// One live grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    /// Names the grant, and the stream of a screen cast.
    pub generation: u64,
    /// What is delivered.
    pub kind: Kind,
    /// The only connection that receives it.
    pub connection: ConnectionId,
    /// What it covers.
    pub target: Target,
    /// Boot time at which it ends.
    pub expires: Duration,
    /// The lock epoch it was made in.
    pub epoch: u64,
}

/// One event with the clocks and the lock state read when it is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    /// Both clocks.
    pub now: Clocks,
    /// The lock state at this moment.
    pub lock: LockView,
    /// What happened.
    pub event: Event,
}

/// Everything the grant reducer reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing but the passing of time: the expiry timer fired.
    Tick,
    /// A client asks for a grant.
    Requested(Request),
    /// The consent UI reports the user's choice.
    Answered {
        /// The prompt the choice was made in.
        prompt: PromptId,
        /// Whether the user allowed the request.
        allow: bool,
    },
    /// The consent UI of this prompt exited without an answer.
    PromptExited {
        /// The prompt that is gone.
        prompt: PromptId,
    },
    /// Data is about to leave under a grant. Sent at the last point the
    /// compositor controls, with what is true at that moment.
    Use {
        /// The grant the delivery claims.
        generation: u64,
        /// The connection the data would be written to.
        connection: ConnectionId,
        /// What the data shows.
        target: Target,
    },
    /// The user pressed Stop on the indicator of one stream.
    Stop {
        /// The compositor process the indicator belonged to.
        instance: InstanceId,
        /// The stream the indicator was drawn for.
        generation: u64,
    },
    /// The compositor ends one grant for a reason of its own.
    Revoke {
        /// The grant to end.
        generation: u64,
    },
    /// This window generation is gone: closed, or replaced under its id.
    WindowGone(WindowRef),
    /// A client connection closed.
    ConnectionClosed(ConnectionId),
    /// The lock reducer asked to revoke everything.
    Lock,
    /// The physical kill switch for agents.
    KillSwitch,
}

/// Why a grant ended or a request was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The session locked.
    Lock,
    /// The machine slept.
    Suspend,
    /// The deadline passed.
    Expired,
    /// The user pressed Stop.
    Stop,
    /// The compositor revoked it.
    Revoked,
    /// The target window is gone.
    WindowGone,
    /// The holder's connection closed.
    ConnectionClosed,
    /// An agent's grant was used up by its one delivery.
    Used,
    /// The kill switch for agents.
    KillSwitch,
    /// The user said no.
    Declined,
    /// The consent UI exited without an answer.
    PromptExited,
    /// The queue or the table is full.
    Full,
    /// This kind of grant does not exist in this build.
    NotOffered,
}

/// What the adapter must do, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Start a consent UI for this request.
    ShowPrompt {
        /// Name the answer must carry.
        prompt: PromptId,
        /// What the user is asked.
        request: Request,
    },
    /// Kill the consent UI of this prompt.
    ClosePrompt {
        /// The prompt that is void.
        prompt: PromptId,
    },
    /// Tell the client that it gets no grant.
    Denied {
        /// What was asked.
        request: Request,
        /// Why not.
        cause: Cause,
    },
    /// A grant exists from now on.
    Granted(Grant),
    /// A grant ended: close its stream, drop what is queued for it and
    /// cancel its pending transfer.
    Revoked {
        /// The grant as it was.
        grant: Grant,
        /// Why it ended.
        cause: Cause,
    },
    /// The delivery that asked may leave.
    Deliver {
        /// The grant it leaves under.
        generation: u64,
    },
    /// The delivery that asked must be dropped.
    Withhold {
        /// The grant it claimed.
        generation: u64,
    },
    /// Freeze the agent cgroups.
    FreezeAgents,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenPrompt {
    number: u64,
    request: Request,
    deadline: Duration,
}

/// Grant table and consent prompts of one compositor instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    instance: InstanceId,
    agent_capture: bool,
    watch: SleepWatch,
    grants: Vec<Grant>,
    /// The open prompt and its number.
    prompt: Option<OpenPrompt>,
    queue: VecDeque<Request>,
    prompts: u64,
    generations: u64,
}

impl State {
    /// The empty table of a new compositor instance.
    ///
    /// `agent_capture` offers [`Kind::AgentCapture`], which is planned for
    /// after v1 (REQ-CAP-011); v1 passes `false`.
    #[must_use]
    pub const fn new(instance: InstanceId, watch: SleepWatch, agent_capture: bool) -> Self {
        Self {
            instance,
            agent_capture,
            watch,
            grants: Vec::new(),
            prompt: None,
            queue: VecDeque::new(),
            prompts: 0,
            generations: 0,
        }
    }

    /// The live grants, oldest first.
    #[must_use]
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// Boot time of the next grant or consent deadline: when to send [`Event::Tick`].
    #[must_use]
    pub fn next_expiry(&self) -> Option<Duration> {
        self.grants
            .iter()
            .map(|grant| grant.expires)
            .chain(self.prompt.map(|prompt| prompt.deadline))
            .min()
    }

    /// REQ-GRANT-005: a grant leaves the table and its end is reported in
    /// the same step.
    fn end_grants(&mut self, effects: &mut Vec<Effect>, cause: impl Fn(&Grant) -> Option<Cause>) {
        self.grants.retain(|grant| {
            let cause = cause(grant);
            effects.extend(cause.map(|cause| Effect::Revoked {
                grant: *grant,
                cause,
            }));
            cause.is_none()
        });
    }

    fn close_prompt(&self, effects: &mut Vec<Effect>, open: OpenPrompt, cause: Cause) {
        let prompt = PromptId {
            instance: self.instance,
            number: open.number,
        };
        effects.extend([
            Effect::ClosePrompt { prompt },
            Effect::Denied {
                request: open.request,
                cause,
            },
        ]);
    }

    /// Denies the open prompt and the waiting requests that `hit` selects.
    fn deny(&mut self, effects: &mut Vec<Effect>, cause: Cause, hit: impl Fn(&Request) -> bool) {
        if let Some(open) = self.prompt.take_if(|open| hit(&open.request)) {
            self.close_prompt(effects, open, cause);
        }
        self.queue.retain(|request| {
            let denied = hit(request);
            if denied {
                let request = *request;
                effects.push(Effect::Denied { request, cause });
            }
            !denied
        });
    }

    fn end_all(&mut self, effects: &mut Vec<Effect>, cause: Cause) {
        self.deny(effects, cause, |_| true);
        self.end_grants(effects, |_| Some(cause));
    }

    /// REQ-GRANT-015: only the open prompt of this instance can be answered,
    /// and an answer consumes it.
    fn take_prompt(&mut self, prompt: PromptId) -> Option<Request> {
        let open = self
            .prompt
            .take_if(|open| prompt.instance == self.instance && open.number == prompt.number);
        open.map(|open| open.request)
    }

    fn issue(&mut self, request: Request, now: Clocks, epoch: u64) -> Option<Grant> {
        let generation = self.generations.checked_add(1)?;
        let expires = now.boottime.checked_add(request.ttl)?;
        if self.grants.len() >= MAX_GRANTS {
            return None;
        }
        self.generations = generation;
        let grant = Grant {
            generation,
            kind: request.kind,
            connection: request.connection,
            target: request.target,
            expires,
            epoch,
        };
        self.grants.push(grant);
        Some(grant)
    }

    /// REQ-TRUST-012: at most one prompt is open; the next request in line
    /// is shown when it closes.
    fn show_next(&mut self, now: Duration, effects: &mut Vec<Effect>) {
        while self.prompt.is_none()
            && let Some(request) = self.queue.pop_front()
        {
            let cause = Cause::Full;
            let Some(number) = self.prompts.checked_add(1) else {
                effects.push(Effect::Denied { request, cause });
                continue;
            };
            self.prompts = number;
            self.prompt = Some(OpenPrompt {
                number,
                request,
                deadline: now.saturating_add(PROMPT_TIMEOUT),
            });
            let prompt = PromptId {
                instance: self.instance,
                number,
            };
            effects.push(Effect::ShowPrompt { prompt, request });
        }
    }

    fn handle(&mut self, input: Input, prompt_was_open: bool, effects: &mut Vec<Effect>) {
        let Input { now, lock, event } = input;
        match event {
            Event::Tick => {}
            Event::Lock => self.end_all(effects, Cause::Lock),
            // REQ-AGENT-006: every agent grant is gone before the freeze.
            Event::KillSwitch => {
                let cause = Cause::KillSwitch;
                self.deny(effects, cause, |request| request.kind.is_agent());
                self.end_grants(effects, |grant| grant.kind.is_agent().then_some(cause));
                effects.push(Effect::FreezeAgents);
            }
            Event::Requested(request) => {
                let cause = if !lock.unlocked {
                    Some(Cause::Lock)
                } else if request.kind == Kind::AgentCapture && !self.agent_capture {
                    // REQ-AGENT-001: in v1 the user is not even asked.
                    Some(Cause::NotOffered)
                } else if self.queue.len() >= MAX_QUEUED {
                    Some(Cause::Full)
                } else {
                    None
                };
                match cause {
                    Some(cause) => effects.push(Effect::Denied { request, cause }),
                    None => self.queue.push_back(request),
                }
            }
            Event::Answered { prompt, allow } => {
                if let Some(request) = self.take_prompt(prompt) {
                    let denied = |cause| Effect::Denied { request, cause };
                    effects.push(if allow {
                        let grant = self.issue(request, now, lock.epoch);
                        grant.map_or(denied(Cause::Full), Effect::Granted)
                    } else {
                        denied(Cause::Declined)
                    });
                }
            }
            // REQ-TRUST-010: no answer is a denial.
            Event::PromptExited { prompt } => {
                let cause = Cause::PromptExited;
                let request = self.take_prompt(prompt);
                effects.extend(request.map(|request| Effect::Denied { request, cause }));
            }
            Event::Use {
                generation,
                connection,
                target,
            } => {
                // REQ-GRANT-013: lock, sleep and deadline were applied to the
                // table before this point, with the readings of this event.
                let claimed = |grant: &Grant| {
                    grant.generation == generation
                        && grant.connection == connection
                        && grant.target == target
                };
                // REQ-TRUST-006: keep a live grant paused through a prompt,
                // including a delivery racing the step that times it out.
                if !prompt_was_open && self.grants.iter().any(claimed) {
                    effects.push(Effect::Deliver { generation });
                    // REQ-AGENT-005: one delivery per agent grant.
                    self.end_grants(effects, |grant| {
                        (claimed(grant) && grant.kind.is_agent()).then_some(Cause::Used)
                    });
                } else {
                    effects.push(Effect::Withhold { generation });
                }
            }
            // REQ-GRANT-008: generations restart with the compositor, so a
            // Stop from an earlier instance must not end a stream of this one.
            Event::Stop {
                instance,
                generation,
            } => {
                let named = instance == self.instance;
                self.end_grants(effects, |grant| {
                    (named && grant.generation == generation).then_some(Cause::Stop)
                });
            }
            Event::Revoke { generation } => self.end_grants(effects, |grant| {
                (grant.generation == generation).then_some(Cause::Revoked)
            }),
            // REQ-GRANT-007.
            Event::WindowGone(window) => {
                let cause = Cause::WindowGone;
                let gone = Target::Window(window);
                self.deny(effects, cause, |request| request.target == gone);
                self.end_grants(effects, |grant| (grant.target == gone).then_some(cause));
            }
            // REQ-GRANT-014.
            Event::ConnectionClosed(connection) => {
                let cause = Cause::ConnectionClosed;
                self.deny(effects, cause, |request| request.connection == connection);
                self.end_grants(effects, |grant| {
                    (grant.connection == connection).then_some(cause)
                });
            }
        }
    }
}

/// Handles one event.
///
/// REQ-GRANT-006, REQ-LOCK-021, REQ-LOCK-025: what the clocks and the lock
/// state end is ended first, so no event is judged against a stale table.
#[must_use]
pub fn step(mut state: State, mut input: Input) -> (State, Vec<Effect>) {
    let now = input.now;
    let mut effects = Vec::new();
    if state.watch.observe(now) != ClockChange::Stable {
        state.end_all(&mut effects, Cause::Suspend);
        input.lock.unlocked = false;
    }
    let lock = input.lock;
    if !lock.unlocked {
        state.end_all(&mut effects, Cause::Lock);
    }
    state.end_grants(&mut effects, |grant| {
        if grant.epoch != lock.epoch {
            Some(Cause::Lock)
        } else if now.boottime >= grant.expires {
            Some(Cause::Expired)
        } else {
            None
        }
    });
    let prompt_was_open = state.prompt.is_some();
    if let Some(expired) = state.prompt.take_if(|open| now.boottime >= open.deadline) {
        state.close_prompt(&mut effects, expired, Cause::Expired);
    }
    state.handle(input, prompt_was_open, &mut effects);
    state.show_next(now.boottime, &mut effects);
    debug_assert!(state.grants.len() <= MAX_GRANTS && state.queue.len() <= MAX_QUEUED);
    debug_assert!(lock.unlocked || (state.grants.is_empty() && state.prompt.is_none()));
    (state, effects)
}
