//! Bounded consent admission and invalidation of a live stream grant.

use super::*;

/// REQ-AGENT-001: consent is available only for explicitly offered targets.
#[kani::proof]
fn req_proto_004_admitted_requests_fit_the_consent_queue() {
    let queued: usize = kani::any();
    let unlocked = kani::any();
    let agent_capture = kani::any();
    let kind = match kani::any::<u8>() {
        0 => Kind::PortalStream,
        1 => Kind::AgentTransfer,
        _ => Kind::AgentCapture,
    };
    let target = if kani::any() {
        Target::Screen
    } else {
        Target::Window(WindowRef {
            id: 1,
            generation: 1,
        })
    };
    let ttl = Duration::from_nanos(kani::any());
    let request = Request {
        kind,
        target,
        connection: ConnectionId(1),
        ttl,
    };
    let admitted = request
        .denial_reason(unlocked, agent_capture, queued)
        .is_none();
    let offered = match kind {
        Kind::PortalStream => true,
        Kind::AgentTransfer => matches!(target, Target::Window(_)),
        Kind::AgentCapture => agent_capture && matches!(target, Target::Window(_)),
    };
    assert_eq!(
        admitted,
        unlocked && offered && !ttl.is_zero() && queued < MAX_QUEUED
    );
    if admitted {
        assert!(queued + 1 <= MAX_QUEUED);
    }
    kani::cover!(
        queued == MAX_QUEUED - 1 && admitted,
        "the last free slot can be filled"
    );
}

#[kani::proof]
#[kani::unwind(5)]
fn req_grant_001_revocation_removes_only_the_named_generation() {
    let now = Clocks::exact(Duration::ZERO, Duration::ZERO);
    let lock = LockView {
        unlocked: true,
        epoch: 1,
    };
    let mut state = State::new(
        InstanceId([9; 16]),
        SleepWatch::new(now, Duration::from_secs(1)),
        false,
    );
    state.epoch = lock.epoch;
    let generation = 1;
    let request = Request {
        kind: Kind::PortalStream,
        connection: ConnectionId(4),
        target: Target::Screen,
        ttl: Duration::from_secs(10),
    };
    state.grants = Vec::with_capacity(1);
    let issued = state.issue(request, now, lock.epoch).unwrap();
    assert_eq!(issued.generation, generation);

    let revoked = kani::any();
    let mut effects = Vec::with_capacity(2);
    state.handle(
        Input {
            now,
            lock,
            event: Event::Revoke {
                generation: revoked,
            },
        },
        false,
        &mut effects,
    );
    assert_eq!(state.grants().is_empty(), revoked == generation);
    if revoked == generation {
        assert!(matches!(
            effects.as_slice(),
            [Effect::Revoked {
                cause: Cause::Revoked,
                grant
            }] if *grant == issued
        ));
    } else {
        assert_eq!(state.grants(), &[issued]);
        assert!(effects.is_empty());
    }
    kani::cover!(revoked == generation, "a live grant is revoked");
    kani::cover!(revoked != generation, "an unrelated generation survives");
}

/// REQ-TRUST-012: one open prompt and the last waiting slot at the public entry point.
#[kani::proof]
#[kani::unwind(7)]
fn req_proto_004_last_waiting_request_fills_the_queue() {
    let now = Clocks::exact(Duration::ZERO, Duration::ZERO);
    let lock = LockView {
        unlocked: true,
        epoch: 1,
    };
    let mut state = State::new(
        InstanceId([9; 16]),
        SleepWatch::new(now, Duration::from_secs(1)),
        false,
    );
    let stream = Request {
        kind: Kind::PortalStream,
        connection: ConnectionId(1),
        target: Target::Screen,
        ttl: Duration::from_secs(10),
    };
    state.epoch = lock.epoch;
    state.prompt = Some(OpenPrompt {
        number: 1,
        request: stream,
        deadline: PROMPT_TIMEOUT,
    });
    state.prompts = 1;
    state.queue = VecDeque::with_capacity(MAX_QUEUED + 1);
    state.queue.resize(MAX_QUEUED - 1, stream);
    let last = Request {
        connection: ConnectionId(kani::any()),
        ..stream
    };
    let input = |request| Input {
        now,
        lock,
        event: Event::Requested(request),
    };
    let (state, admitted) = step(state, input(last));
    assert!(admitted.is_empty());
    assert_eq!(state.queue.len(), MAX_QUEUED);
    assert_eq!(state.queue.back(), Some(&last));
    let (state, denied) = step(state, input(stream));
    assert_eq!(
        denied.as_slice(),
        &[Effect::Denied {
            request: stream,
            cause: Cause::Full
        }]
    );
    assert_eq!(state.queue.len(), MAX_QUEUED);
    assert_eq!(state.queue.back(), Some(&last));
}
