//! REQ-AUTH-001: the verdict gate checks each binding and consumes a decision once.

use super::*;

fn verdict(granted: bool) -> Outcome {
    if granted {
        Outcome::Granted
    } else {
        Outcome::Denied
    }
}

fn initial_state() -> State {
    let config = Config {
        instance: InstanceId(kani::any()),
        session: SessionId(kani::any()),
        jump_threshold: Duration::from_secs(1),
    };
    State::start(config, Clocks::exact(Duration::ZERO, Duration::ZERO)).0
}

#[kani::proof]
#[kani::unwind(33)]
fn req_auth_001_only_the_open_binding_is_eligible() {
    let mut state = initial_state();
    state.phase = match kani::any::<u8>() {
        0 => Phase::Locking,
        1 => Phase::Unlocked,
        _ => Phase::Locked,
    };
    state.hold = kani::any();
    state.epoch = kani::any();
    state.attempt = kani::any();
    state.output = match kani::any::<u8>() {
        0 => Output::Off,
        1 => Output::Unavailable,
        _ => Output::On,
    };
    state.ui = match kani::any::<u8>() {
        0 => Ui::Stopped,
        1 => Ui::Starting,
        2 => Ui::Connected,
        _ => Ui::Checking {
            deadline: ATTEMPT_TIMEOUT,
        },
    };

    let binding = Binding {
        instance: InstanceId(kani::any()),
        session: SessionId(kani::any()),
        epoch: kani::any(),
        attempt: kani::any(),
    };
    let eligible = state.phase == Phase::Locked
        && !state.hold
        && state.output == Output::On
        && matches!(state.ui, Ui::Checking { .. })
        && state.epoch < u64::MAX
        && binding.instance == state.config.instance
        && binding.session == state.config.session
        && binding.epoch == state.epoch
        && binding.attempt == state.attempt;
    assert_eq!(state.accepts_verdict(binding), eligible);
    kani::cover!(eligible, "a matching binding is eligible");
    if !eligible {
        kani::cover!(
            state.config.session.0[0] != state.config.session.0[1],
            "rejection is reachable with distinct stored session bytes"
        );
        let phase = state.phase();
        let mut effects = Vec::with_capacity(8);
        state.judge(binding, verdict(kani::any()), &mut effects);
        assert_eq!(state.phase(), phase);
        assert!(effects.is_empty());
    }
}

#[kani::proof]
#[kani::unwind(33)]
fn req_auth_001_each_decision_consumes_the_binding() {
    let mut state = initial_state();
    state.phase = Phase::Locked;
    state.hold = false;
    state.attempt = 1;
    state.ui_serial = 1;
    state.ui = Ui::Checking {
        deadline: ATTEMPT_TIMEOUT,
    };
    let binding = state.binding();
    assert!(state.accepts_verdict(binding));

    let granted: bool = kani::any();
    let mut effects = Vec::with_capacity(8);
    state.judge(binding, verdict(granted), &mut effects);
    assert_eq!(state.phase() == Phase::Unlocked, granted);
    assert_eq!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Unlock { .. })),
        granted
    );
    assert!(!state.accepts_verdict(binding));

    // Presenting the same binding again must preserve the resulting phase.
    let phase = state.phase();
    effects.clear();
    state.judge(binding, verdict(kani::any()), &mut effects);
    assert_eq!(state.phase(), phase);
    assert!(effects.is_empty());
    kani::cover!(granted, "a granted verdict is reachable");
    kani::cover!(!granted, "a denied verdict is reachable");
}
