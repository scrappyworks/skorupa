//! The redraw state machine as a reducer: every time is an input, every
//! timer, render and client notification is an effect.
//!
//! The harness below plays the adapter: it keeps one deadline per timer kind,
//! as the contract of `Effect::ArmTimer` and `Effect::CancelTimer` asks.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "clippy.toml exempts test functions only; the helpers here are test code too"
)]

use std::time::Duration;

use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use skorupa_compositor_core::frame_clock::{
    Anomaly, Effect, Event, FramePacer, RedrawState, RenderOutcome, Timer,
};

const HZ_60: Duration = Duration::from_nanos(16_666_667);
const HZ_180: Duration = Duration::from_nanos(5_555_556);
const FLIP_TIMEOUT: Duration = Duration::from_secs(1);
const TIMERS: [Timer; 3] = [Timer::EstimatedVBlank, Timer::Throttle, Timer::FlipWatchdog];

const fn ns(nanos: u64) -> Duration {
    Duration::from_nanos(nanos)
}

const fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

struct Harness {
    pacer: FramePacer,
    timers: [Option<Duration>; 3],
}

impl Harness {
    fn new(refresh: Option<Duration>) -> Self {
        Self {
            pacer: FramePacer::new(refresh, FLIP_TIMEOUT),
            timers: [None; 3],
        }
    }

    fn slot(timer: Timer) -> usize {
        TIMERS.iter().position(|kind| *kind == timer).unwrap()
    }

    fn deadline(&self, timer: Timer) -> Option<Duration> {
        self.timers[Self::slot(timer)]
    }

    fn state(&self) -> RedrawState {
        self.pacer.redraw_state()
    }

    fn send(&mut self, event: Event) -> Vec<Effect> {
        let (pacer, effects) = self.pacer.clone().step(event);
        self.pacer = pacer;
        for effect in &effects {
            match *effect {
                Effect::ArmTimer { timer, at } => self.timers[Self::slot(timer)] = Some(at),
                Effect::CancelTimer(timer) => {
                    assert!(
                        self.deadline(timer).is_some(),
                        "cancelled an idle {timer:?}"
                    );
                    self.timers[Self::slot(timer)] = None;
                }
                _ => {}
            }
        }
        effects
    }

    /// Fires an armed timer at its deadline.
    fn fire(&mut self, timer: Timer) -> Vec<Effect> {
        let now = self.timers[Self::slot(timer)].take().unwrap();
        self.send(Event::TimerFired { now, timer })
    }

    /// A commit followed by the event-loop tail; returns the render target.
    fn redraw(&mut self, now: Duration) -> Duration {
        self.send(Event::QueueRedraw);
        self.render(now)
    }

    /// The event-loop tail with a redraw already queued.
    fn render(&mut self, now: Duration) -> Duration {
        match self.send(Event::Redraw { now })[..] {
            [Effect::Render { target }] => target,
            ref other => panic!("expected one render, got {other:?}"),
        }
    }

    fn rendered(&mut self, now: Duration, outcome: RenderOutcome) -> Vec<Effect> {
        self.send(Event::Rendered {
            now,
            outcome,
            animations_remain: false,
        })
    }

    fn flip(&mut self, now: Duration, time: Option<Duration>, sequence: u32) -> Vec<Effect> {
        self.send(Event::FlipCompleted {
            now,
            time,
            sequence,
        })
    }
}

fn callbacks(sequence: u32) -> Effect {
    Effect::SendFrameCallbacks { sequence }
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 512,
        rng_seed: RngSeed::Fixed(0x534b_4f52_5550_4133),
        ..ProptestConfig::default()
    }
}

#[test]
fn req_perf_003_queue_redraw_follows_the_niri_transition_table() {
    use RedrawState::{
        Idle, Queued, WaitingForEstimatedVBlank, WaitingForEstimatedVBlankAndQueued,
        WaitingForVBlank,
    };
    assert_eq!(Idle.queue_redraw(), Queued);
    assert_eq!(Queued.queue_redraw(), Queued);
    assert_eq!(
        WaitingForEstimatedVBlank.queue_redraw(),
        WaitingForEstimatedVBlankAndQueued
    );
    assert_eq!(
        WaitingForEstimatedVBlankAndQueued.queue_redraw(),
        WaitingForEstimatedVBlankAndQueued
    );
    for redraw_needed in [false, true] {
        assert_eq!(
            WaitingForVBlank { redraw_needed }.queue_redraw(),
            WaitingForVBlank {
                redraw_needed: true
            }
        );
    }
}

#[test]
fn req_perf_003_event_loop_tail_renders_only_when_a_redraw_is_queued() {
    let mut output = Harness::new(Some(HZ_180));
    assert_eq!(output.send(Event::Redraw { now: ms(1) }), []);
    assert_eq!(output.send(Event::QueueRedraw), []);
    assert_eq!(output.state(), RedrawState::Queued);
    // Nothing was presented yet, so the frame is due now.
    assert_eq!(
        output.send(Event::Redraw { now: ms(2) }),
        [Effect::Render { target: ms(2) }]
    );
}

#[test]
fn req_perf_003_submitted_frame_waits_for_its_flip_and_reports_it() {
    let mut output = Harness::new(Some(HZ_180));
    output.redraw(ms(10));
    assert_eq!(
        output.rendered(ms(11), RenderOutcome::Submitted),
        [
            Effect::ArmTimer {
                timer: Timer::FlipWatchdog,
                at: ms(11) + FLIP_TIMEOUT
            },
            callbacks(1),
        ]
    );
    assert_eq!(
        output.state(),
        RedrawState::WaitingForVBlank {
            redraw_needed: false
        }
    );

    let flip_time = ms(15);
    assert_eq!(
        output.flip(
            flip_time.saturating_sub(Duration::from_micros(300)),
            Some(flip_time),
            77
        ),
        [
            Effect::CancelTimer(Timer::FlipWatchdog),
            Effect::Presented {
                time: flip_time,
                refresh: Some(HZ_180),
                sequence: 77,
                hw_clock: true
            },
            callbacks(1),
        ]
    );
    assert_eq!(output.state(), RedrawState::Idle);
    assert_eq!(output.timers, [None; 3]);

    // The next frame is scheduled from the flip that actually happened.
    assert_eq!(output.redraw(ms(16)), flip_time + HZ_180);
}

#[test]
fn req_perf_003_commit_during_a_flip_redraws_right_after_it() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    assert_eq!(output.send(Event::QueueRedraw), []);
    assert_eq!(output.send(Event::Redraw { now: ms(12) }), []);

    let effects = output.flip(ms(20), Some(ms(20)), 1);
    assert!(
        !effects.contains(&callbacks(1)),
        "the redraw sends them: {effects:?}"
    );
    assert_eq!(output.state(), RedrawState::Queued);
    assert_eq!(output.render(ms(20)), ms(20) + HZ_60);
}

#[test]
fn req_perf_003_unfinished_animations_redraw_after_every_flip_and_timer() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    output.send(Event::Rendered {
        now: ms(10),
        outcome: RenderOutcome::Submitted,
        animations_remain: true,
    });
    output.flip(ms(20), Some(ms(20)), 1);
    assert_eq!(output.state(), RedrawState::Queued);

    output.render(ms(20));
    output.send(Event::Rendered {
        now: ms(21),
        outcome: RenderOutcome::NoDamage,
        animations_remain: true,
    });
    assert_eq!(output.fire(Timer::EstimatedVBlank), []);
    assert_eq!(output.state(), RedrawState::Queued);
}

#[test]
fn req_perf_003_frame_without_damage_waits_for_the_estimated_vblank() {
    let mut output = Harness::new(Some(HZ_180));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    output.flip(ms(15), Some(ms(15)), 1);

    let target = output.redraw(ms(16));
    assert_eq!(target, ms(15) + HZ_180);
    assert_eq!(
        output.rendered(ms(17), RenderOutcome::NoDamage),
        [
            Effect::ArmTimer {
                timer: Timer::EstimatedVBlank,
                at: target
            },
            callbacks(1),
        ]
    );
    assert_eq!(output.state(), RedrawState::WaitingForEstimatedVBlank);

    assert_eq!(output.fire(Timer::EstimatedVBlank), [callbacks(2)]);
    assert_eq!(output.state(), RedrawState::Idle);
}

#[test]
fn req_perf_003_estimated_vblank_is_one_interval_away_when_the_target_is_now() {
    let mut known = Harness::new(Some(HZ_180));
    assert_eq!(known.redraw(ms(10)), ms(10));
    known.rendered(ms(10), RenderOutcome::NoDamage);
    assert_eq!(
        known.deadline(Timer::EstimatedVBlank),
        Some(ms(10) + HZ_180)
    );

    let mut unknown = Harness::new(None);
    unknown.redraw(ms(10));
    unknown.rendered(ms(10), RenderOutcome::NoDamage);
    assert_eq!(
        unknown.deadline(Timer::EstimatedVBlank),
        Some(ms(10) + Duration::from_micros(16_667))
    );
}

#[test]
fn req_perf_003_redraw_queued_before_the_estimated_vblank_keeps_its_timer() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::NoDamage);
    let deadline = output.deadline(Timer::EstimatedVBlank);

    // Still nothing to show: the timer that is already running stays.
    output.redraw(ms(12));
    assert_eq!(
        output.state(),
        RedrawState::WaitingForEstimatedVBlankAndQueued
    );
    assert_eq!(
        output.rendered(ms(12), RenderOutcome::NoDamage),
        [callbacks(0)]
    );
    assert_eq!(output.state(), RedrawState::WaitingForEstimatedVBlank);
    assert_eq!(output.deadline(Timer::EstimatedVBlank), deadline);

    // Now there is damage: the real flip replaces the estimate.
    output.redraw(ms(13));
    assert_eq!(
        output.rendered(ms(13), RenderOutcome::Submitted),
        [
            Effect::CancelTimer(Timer::EstimatedVBlank),
            Effect::ArmTimer {
                timer: Timer::FlipWatchdog,
                at: ms(13) + FLIP_TIMEOUT
            },
            callbacks(1),
        ]
    );
}

#[test]
fn req_perf_003_estimated_vblank_just_before_a_queued_redraw_leaves_it_queued() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::NoDamage);
    output.send(Event::QueueRedraw);
    assert_eq!(output.fire(Timer::EstimatedVBlank), []);
    assert_eq!(output.state(), RedrawState::Queued);
    assert_eq!(output.pacer.frame_callback_sequence(), 1);
}

#[test]
fn req_perf_003_failed_render_is_paced_like_a_frame_without_damage() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    assert_eq!(
        output.rendered(ms(10), RenderOutcome::Failed),
        [
            Effect::ArmTimer {
                timer: Timer::EstimatedVBlank,
                at: ms(10) + HZ_60
            },
            callbacks(0),
        ]
    );
    assert_eq!(output.fire(Timer::EstimatedVBlank), [callbacks(1)]);
    assert_eq!(output.state(), RedrawState::Idle);
}

#[test]
fn req_perf_003_skipped_render_returns_to_idle_or_to_the_running_timer() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    assert_eq!(
        output.rendered(ms(10), RenderOutcome::Skipped),
        [callbacks(0)]
    );
    assert_eq!(output.state(), RedrawState::Idle);
    assert_eq!(output.timers, [None; 3]);

    output.redraw(ms(11));
    output.rendered(ms(11), RenderOutcome::NoDamage);
    output.redraw(ms(12));
    output.rendered(ms(12), RenderOutcome::Skipped);
    assert_eq!(output.state(), RedrawState::WaitingForEstimatedVBlank);
    assert!(output.deadline(Timer::EstimatedVBlank).is_some());
}

/// Seen in niri after resume and when monitors power on (niri issues 556 and
/// 615): the flip is taken as real and a redraw is forced.
#[test]
fn req_perf_003_flip_in_an_unexpected_state_forces_a_redraw() {
    let mut idle = Harness::new(Some(HZ_60));
    assert_eq!(
        idle.flip(ms(10), Some(ms(10)), 3),
        [
            Effect::Anomaly(Anomaly::UnexpectedFlip),
            Effect::Presented {
                time: ms(10),
                refresh: Some(HZ_60),
                sequence: 3,
                hw_clock: true
            },
        ]
    );
    assert_eq!(idle.state(), RedrawState::Queued);

    let mut waiting = Harness::new(Some(HZ_60));
    waiting.redraw(ms(10));
    waiting.rendered(ms(10), RenderOutcome::NoDamage);
    let effects = waiting.flip(ms(11), Some(ms(11)), 3);
    assert!(effects.contains(&Effect::CancelTimer(Timer::EstimatedVBlank)));
    assert_eq!(waiting.state(), RedrawState::Queued);
    assert_eq!(waiting.timers, [None; 3]);
}

#[test]
fn req_perf_003_timer_or_render_result_nobody_asked_for_changes_nothing() {
    for timer in TIMERS {
        let mut output = Harness::new(Some(HZ_60));
        output.send(Event::QueueRedraw);
        let before = output.pacer.clone();
        assert_eq!(
            output.send(Event::TimerFired { now: ms(5), timer }),
            [Effect::Anomaly(Anomaly::UnexpectedTimer(timer))]
        );
        assert_eq!(output.pacer, before);
    }

    let mut output = Harness::new(Some(HZ_60));
    let before = output.pacer.clone();
    assert_eq!(
        output.rendered(ms(5), RenderOutcome::Submitted),
        [Effect::Anomaly(Anomaly::UnexpectedRendered)]
    );
    assert_eq!(output.pacer, before);
}

#[test]
fn req_perf_003_impossible_future_flip_does_not_move_the_refresh_grid() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    let effects = output.flip(ms(11), Some(ms(60_011)), 1);
    assert!(effects.contains(&Effect::Anomaly(Anomaly::FlipFarInFuture)));
    assert!(effects.contains(&Effect::Presented {
        time: ms(11),
        refresh: Some(HZ_60),
        sequence: 1,
        hw_clock: false,
    }));
    output.redraw(ms(12));
    output.rendered(ms(12), RenderOutcome::Submitted);
    output.flip(ms(12), Some(ms(12)), 2);
    assert_eq!(output.deadline(Timer::Throttle), Some(ms(11) + HZ_60));
    output.fire(Timer::Throttle);
    assert_eq!(output.timers, [None; 3]);
}

/// A client that commits on every frame callback: one flip per refresh, each
/// frame aimed at the refresh after the flip that started it.
fn steady_state(refresh: Duration) {
    // The flip event of the target panel arrives this long before its timestamp.
    let early = Duration::from_micros(300);
    let mut output = Harness::new(Some(refresh));
    let start = ms(1000);

    assert_eq!(output.redraw(start), start);
    output.rendered(start, RenderOutcome::Submitted);
    for frame in 1..=200_u32 {
        let flip_time = start + refresh * frame;
        let now = flip_time.saturating_sub(early);
        // The client answered the frame callback sent at submission.
        output.send(Event::QueueRedraw);
        let effects = output.flip(now, Some(flip_time), frame);
        assert!(effects.contains(&Effect::Presented {
            time: flip_time,
            refresh: Some(refresh),
            sequence: frame,
            hw_clock: true
        }));
        assert_eq!(output.render(now), flip_time + refresh);
        assert_eq!(
            output
                .rendered(now + early, RenderOutcome::Submitted)
                .last(),
            Some(&callbacks(frame + 1))
        );
    }
}

#[test]
fn req_perf_003_steady_state_at_180_hz_targets_every_refresh() {
    steady_state(HZ_180);
}

#[test]
fn req_perf_003_steady_state_at_60_hz_targets_every_refresh() {
    steady_state(HZ_60);
}

/// virtio-gpu and the headless backend complete a flip at once. The times of
/// the frames that reach clients must still be one refresh interval apart.
fn immediate_flips(refresh: Duration, time_known: bool) {
    let mut output = Harness::new(Some(refresh));
    let start = ms(1000);
    let mut presented = Vec::new();
    let mut now = start;

    for _ in 0..50 {
        // The client commits as soon as it may; the backend completes at once.
        output.redraw(now);
        output.rendered(now, RenderOutcome::Submitted);
        let mut effects = output.flip(now, time_known.then_some(now), 0);
        if let Some(at) = output.deadline(Timer::Throttle) {
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::Presented { .. })),
                "a held flip must not be reported yet: {effects:?}"
            );
            now = at;
            effects = output.fire(Timer::Throttle);
        }
        presented.extend(effects.iter().filter_map(|effect| match *effect {
            Effect::Presented { time, .. } => Some(time),
            _ => None,
        }));
        assert_eq!(output.state(), RedrawState::Idle);
    }

    let expected: Vec<Duration> = (0..50).map(|frame| start + refresh * frame).collect();
    assert_eq!(presented, expected);
}

#[test]
fn req_perf_012_immediate_flips_are_presented_one_interval_apart_at_60_hz() {
    immediate_flips(HZ_60, false);
    immediate_flips(HZ_60, true);
}

#[test]
fn req_perf_012_immediate_flips_are_presented_one_interval_apart_at_180_hz() {
    immediate_flips(HZ_180, false);
    immediate_flips(HZ_180, true);
}

#[test]
fn req_perf_012_held_flip_is_reported_without_a_hardware_timestamp() {
    let mut output = Harness::new(Some(HZ_60));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    output.flip(ms(10), Some(ms(10)), 0);

    output.redraw(ms(11));
    output.rendered(ms(11), RenderOutcome::Submitted);
    // The flip did complete, so the watchdog has nothing left to guard.
    assert_eq!(
        output.flip(ms(11), Some(ms(11)), 0),
        [
            Effect::CancelTimer(Timer::FlipWatchdog),
            Effect::ArmTimer {
                timer: Timer::Throttle,
                at: ms(10) + HZ_60
            },
        ]
    );
    assert_eq!(
        output.state(),
        RedrawState::WaitingForVBlank {
            redraw_needed: false
        }
    );
    assert_eq!(
        output.fire(Timer::Throttle),
        [
            Effect::Presented {
                time: ms(10) + HZ_60,
                refresh: Some(HZ_60),
                sequence: 0,
                hw_clock: false
            },
            callbacks(2),
        ]
    );
    assert_eq!(output.timers, [None; 3]);
}

#[test]
fn req_perf_013_flip_that_never_completes_is_given_up_and_redrawn() {
    let mut output = Harness::new(Some(HZ_180));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    assert_eq!(
        output.deadline(Timer::FlipWatchdog),
        Some(ms(10) + FLIP_TIMEOUT)
    );

    assert_eq!(output.fire(Timer::FlipWatchdog), [Effect::FlipTimedOut]);
    assert_eq!(output.state(), RedrawState::Queued);
    assert_eq!(output.timers, [None; 3]);
    // The lost frame released no new frame callbacks and reported nothing.
    assert_eq!(output.pacer.frame_callback_sequence(), 1);
    assert_eq!(output.render(ms(1010)), ms(1010));
}

#[test]
fn req_perf_013_flip_that_completes_after_the_watchdog_is_still_taken() {
    let mut output = Harness::new(Some(HZ_180));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    output.fire(Timer::FlipWatchdog);
    output.render(ms(1010));
    output.rendered(ms(1010), RenderOutcome::Failed);

    let effects = output.flip(ms(1012), Some(ms(1012)), 9);
    assert_eq!(effects[0], Effect::Anomaly(Anomaly::UnexpectedFlip));
    assert_eq!(output.state(), RedrawState::Queued);
    assert_eq!(output.timers, [None; 3]);
    assert_eq!(output.render(ms(1013)), ms(1012) + HZ_180);
}

/// An unrequested flip received before submission cannot present the new frame.
#[test]
fn req_perf_015_held_flip_cannot_complete_a_later_submission() {
    let mut output = Harness::new(Some(HZ_60));
    output.flip(ms(10), None, 1);
    output.render(ms(10));
    output.rendered(ms(10), RenderOutcome::Skipped);
    output.flip(ms(11), None, 2);
    assert!(output.deadline(Timer::Throttle).is_some());
    output.redraw(ms(12));
    output.rendered(ms(12), RenderOutcome::Submitted);

    assert_eq!(output.deadline(Timer::Throttle), None);
    assert_eq!(
        output.deadline(Timer::FlipWatchdog),
        Some(ms(12) + FLIP_TIMEOUT)
    );
    let effects = output.flip(ms(30), Some(ms(30)), 3);
    assert!(effects.contains(&Effect::Presented {
        time: ms(30),
        refresh: Some(HZ_60),
        sequence: 3,
        hw_clock: true,
    }));
    assert_eq!(output.timers, [None; 3]);
}

#[test]
fn req_perf_012_delayed_dispatch_consumes_the_elapsed_throttle_interval() {
    let mut output = Harness::new(Some(ms(10)));
    output.redraw(ms(90));
    output.rendered(ms(90), RenderOutcome::Submitted);
    output.flip(ms(100), Some(ms(100)), 1);
    output.redraw(ms(100));
    output.rendered(ms(100), RenderOutcome::Submitted);
    let effects = output.flip(ms(120), Some(ms(101)), 2);

    assert!(effects.contains(&Effect::Presented {
        time: ms(120),
        refresh: Some(ms(10)),
        sequence: 2,
        hw_clock: false,
    }));
    assert_eq!(output.timers, [None; 3]);
}

/// A client that commits without damage on every frame callback must not spin:
/// a new callback is released only when the next refresh cycle starts.
#[test]
fn req_perf_014_frame_callbacks_advance_once_per_refresh_cycle() {
    let mut output = Harness::new(Some(HZ_180));
    output.redraw(ms(10));
    output.rendered(ms(10), RenderOutcome::Submitted);
    output.flip(ms(15), Some(ms(15)), 1);

    let mut now = ms(15);
    for cycle in 1..=20_u32 {
        // Several empty commits inside one cycle: all get the same sequence.
        for _ in 0..3 {
            now += Duration::from_micros(100);
            output.redraw(now);
            assert_eq!(
                output.rendered(now, RenderOutcome::NoDamage).last(),
                Some(&callbacks(cycle))
            );
        }
        let deadline = output.deadline(Timer::EstimatedVBlank).unwrap();
        assert_eq!(deadline, ms(15) + HZ_180 * cycle);
        now = deadline;
        assert_eq!(output.fire(Timer::EstimatedVBlank), [callbacks(cycle + 1)]);
    }
}

#[derive(Debug, Clone)]
enum Op {
    Queue,
    Redraw,
    Rendered(RenderOutcome, bool),
    Flip(Option<i32>, u32),
    Fire(Timer),
    Stale(Timer),
}

fn op() -> impl Strategy<Value = Op> {
    let outcome = prop_oneof![
        Just(RenderOutcome::Submitted),
        Just(RenderOutcome::NoDamage),
        Just(RenderOutcome::Failed),
        Just(RenderOutcome::Skipped),
    ];
    let timer = prop_oneof![
        Just(Timer::EstimatedVBlank),
        Just(Timer::Throttle),
        Just(Timer::FlipWatchdog),
    ];
    prop_oneof![
        3 => Just(Op::Queue),
        3 => Just(Op::Redraw),
        3 => (outcome, any::<bool>()).prop_map(|(outcome, more)| Op::Rendered(outcome, more)),
        3 => (proptest::option::of(-40_000_000..40_000_000_i32), any::<u32>())
            .prop_map(|(offset, sequence)| Op::Flip(offset, sequence)),
        3 => timer.clone().prop_map(Op::Fire),
        1 => timer.prop_map(Op::Stale),
    ]
}

/// What must hold between the redraw state and the timers after every step.
fn check_timers(output: &Harness) -> Result<(), TestCaseError> {
    let estimated = output.deadline(Timer::EstimatedVBlank).is_some();
    let watchdog = output.deadline(Timer::FlipWatchdog).is_some();
    let throttle = output.deadline(Timer::Throttle).is_some();
    match output.state() {
        RedrawState::Idle | RedrawState::Queued => {
            prop_assert!(!estimated && !watchdog);
        }
        RedrawState::WaitingForVBlank { .. } => {
            prop_assert!(!estimated);
            prop_assert!(watchdog || throttle, "nothing would end this wait");
        }
        RedrawState::WaitingForEstimatedVBlank
        | RedrawState::WaitingForEstimatedVBlankAndQueued => {
            prop_assert!(estimated && !watchdog);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    /// Any order of events, sensible or not: the reducer never panics, never
    /// leaves a wait without a timer that ends it, emits a bounded number of
    /// effects, and settles to idle once clients stop committing.
    #[test]
    fn req_perf_013_no_event_order_leaves_the_output_stuck(
        refresh in prop_oneof![Just(None), Just(Some(HZ_60)), Just(Some(HZ_180))],
        ops in proptest::collection::vec((op(), 0_u64..30_000_000), 0..200),
    ) {
        let mut output = Harness::new(refresh);
        let mut now = ms(1000);
        for (op, delta) in ops {
            now += ns(delta);
            let effects = match op {
                Op::Queue => output.send(Event::QueueRedraw),
                Op::Redraw => output.send(Event::Redraw { now }),
                Op::Rendered(outcome, animations_remain) => {
                    output.send(Event::Rendered { now, outcome, animations_remain })
                }
                Op::Flip(offset, sequence) => {
                    let time = offset.map(|offset| {
                        let shift = ns(u64::from(offset.unsigned_abs()));
                        if offset < 0 { now.saturating_sub(shift) } else { now + shift }
                    });
                    output.send(Event::FlipCompleted { now, time, sequence })
                }
                Op::Fire(timer) => match output.deadline(timer) {
                    Some(at) => {
                        now = now.max(at);
                        output.timers[Harness::slot(timer)] = None;
                        output.send(Event::TimerFired { now, timer })
                    }
                    None => Vec::new(),
                },
                Op::Stale(timer) => {
                    if output.deadline(timer).is_some() {
                        Vec::new()
                    } else {
                        output.send(Event::TimerFired { now, timer })
                    }
                }
            };
            prop_assert!(effects.len() <= 5, "{effects:?}");
            check_timers(&output)?;
        }

        // Clients go quiet and nothing animates: every wait must end.
        for _ in 0..32 {
            if output.send(Event::Redraw { now }).is_empty() {
                let next = TIMERS
                    .into_iter()
                    .filter_map(|timer| output.deadline(timer).map(|at| (at, timer)))
                    .min();
                let Some((at, timer)) = next else { break };
                now = now.max(at);
                output.timers[Harness::slot(timer)] = None;
                output.send(Event::TimerFired { now, timer });
            } else {
                output.send(Event::Rendered {
                    now,
                    outcome: RenderOutcome::NoDamage,
                    animations_remain: false,
                });
            }
            check_timers(&output)?;
        }
        prop_assert_eq!(output.state(), RedrawState::Idle);
        prop_assert_eq!(output.timers, [None; 3]);
    }
}

#[test]
fn req_perf_012_software_flip_between_half_and_full_refresh_waits() {
    let mut output = Harness::new(Some(ms(10)));
    output.redraw(ms(100));
    output.rendered(ms(100), RenderOutcome::Submitted);
    output.flip(ms(100), None, 1);
    output.redraw(ms(106));
    output.rendered(ms(106), RenderOutcome::Submitted);
    let early = output.flip(ms(106), None, 2);
    assert!(early.iter().all(|e| !matches!(e, Effect::Presented { .. })));
    assert_eq!(output.deadline(Timer::Throttle), Some(ms(110)));
    assert!(output.fire(Timer::Throttle).contains(&Effect::Presented {
        time: ms(110),
        refresh: Some(ms(10)),
        sequence: 2,
        hw_clock: false,
    }));
}

#[test]
fn req_perf_012_software_flip_without_a_known_rate_uses_the_fallback() {
    let mut output = Harness::new(None);
    output.redraw(ms(100));
    output.rendered(ms(100), RenderOutcome::Submitted);
    output.flip(ms(100), None, 1);
    output.redraw(ms(106));
    output.rendered(ms(106), RenderOutcome::Submitted);
    output.flip(ms(106), None, 2);
    let at = ms(100) + Duration::from_micros(16_667);
    assert_eq!(output.deadline(Timer::Throttle), Some(at));
    assert!(output.fire(Timer::Throttle).contains(&Effect::Presented {
        time: at,
        refresh: None,
        sequence: 2,
        hw_clock: false,
    }));
}

#[test]
fn req_perf_013_unknown_refresh_cannot_hold_a_flip_until_a_corrupt_timestamp() {
    let mut output = Harness::new(None);
    output.redraw(ms(100));
    output.rendered(ms(100), RenderOutcome::Submitted);
    let first = output.flip(ms(100), Some(Duration::MAX), 1);
    assert!(first.contains(&Effect::Anomaly(Anomaly::FlipFarInFuture)));
    output.redraw(ms(106));
    output.rendered(ms(106), RenderOutcome::Submitted);
    output.flip(ms(106), None, 2);
    assert_eq!(
        output.deadline(Timer::Throttle),
        Some(ms(100) + Duration::from_micros(16_667))
    );
}
