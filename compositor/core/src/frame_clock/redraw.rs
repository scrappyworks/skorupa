// Ported from niri, https://github.com/niri-wm/niri, GPL-3.0-or-later,
// revision 02ee0585b9da359cb30aa2a13a6023a6e471cc23:
//   src/niri.rs:528-557 (per-output state), 570-583 (RedrawState),
//     716-735 (queue_redraw), 5071-5129 (redraw)
//   src/backend/tty.rs:1652-1846 (on_vblank), 1848-1887
//     (on_estimated_vblank_timer), 2023-2048 and 2066-2070 (render result),
//     3141-3185 (queue_estimated_vblank_timer)
// Changes are listed in docs/niri-ports.md.

use std::time::Duration;

use super::clock::FrameClock;
use super::throttle::VBlankThrottle;

/// Estimated-vblank delay for an output with an unknown refresh rate.
const FALLBACK_REFRESH_INTERVAL: Duration = Duration::from_micros(16_667);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RedrawState {
    /// The compositor is idle.
    #[default]
    Idle,
    /// A redraw is queued.
    Queued,
    /// A frame was submitted and its flip has not completed yet.
    WaitingForVBlank { redraw_needed: bool },
    /// Nothing was submitted; a timer fires at the estimated vblank.
    WaitingForEstimatedVBlank,
    /// A redraw is queued on top of the above.
    WaitingForEstimatedVBlankAndQueued,
}

impl RedrawState {
    /// Schedules a redraw if one is not already scheduled.
    #[must_use]
    pub fn queue_redraw(self) -> Self {
        match self {
            Self::Idle => Self::Queued,
            Self::WaitingForEstimatedVBlank => Self::WaitingForEstimatedVBlankAndQueued,
            // A redraw is already queued.
            Self::Queued | Self::WaitingForEstimatedVBlankAndQueued => self,
            // Waiting for the flip: redraw right after it.
            Self::WaitingForVBlank { .. } => Self::WaitingForVBlank {
                redraw_needed: true,
            },
        }
    }
}

/// The timers of one output. The adapter keeps one deadline per kind:
/// arming a kind again replaces its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Timer {
    /// Stands in for the vblank of a frame that was not submitted.
    EstimatedVBlank,
    /// Delivers a flip that completed too early.
    Throttle,
    /// Gives up on a flip that does not complete.
    FlipWatchdog,
}

/// What the adapter did with a [`Effect::Render`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOutcome {
    /// A frame is on its way to the screen; [`Event::FlipCompleted`] follows.
    Submitted,
    /// Nothing changed on screen, so nothing was submitted.
    NoDamage,
    /// Rendering or submitting failed. Paced like `NoDamage`; whoever needs
    /// the frame queues it again.
    Failed,
    /// The output cannot show anything now (the session is on another VT).
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Something on screen changed: a commit, input, a layout change.
    QueueRedraw,
    /// The event-loop tail, after all other events of this iteration.
    Redraw {
        now: Duration,
    },
    /// The answer to [`Effect::Render`], before any other event.
    Rendered {
        now: Duration,
        outcome: RenderOutcome,
        /// Animations are still running, so another frame is needed.
        animations_remain: bool,
    },
    /// The flip of a submitted frame completed. `time` is its timestamp on
    /// the presentation clock, `None` (or zero) when the backend has none.
    FlipCompleted {
        now: Duration,
        time: Option<Duration>,
        sequence: u32,
    },
    TimerFired {
        now: Duration,
        timer: Timer,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Render the output for presentation at `target`, sample animations at
    /// that time, and answer with [`Event::Rendered`].
    Render {
        target: Duration,
    },
    ArmTimer {
        timer: Timer,
        at: Duration,
    },
    /// Emitted only for a timer that is armed.
    CancelTimer(Timer),
    /// Send frame callbacks to the surfaces that have not had one in the
    /// refresh cycle `sequence`.
    SendFrameCallbacks {
        sequence: u32,
    },
    /// The submitted frame is on screen: send its presentation feedback, if
    /// any is held. Without `hw_clock`, `time` is when the event was handled.
    Presented {
        time: Duration,
        refresh: Option<Duration>,
        sequence: u32,
        hw_clock: bool,
    },
    /// The flip did not complete in time. The frame counts as lost: discard
    /// its feedback and reset whatever still waits for it. A redraw is queued.
    FlipTimedOut,
    /// Should not happen; reported so that the adapter can log it.
    Anomaly(Anomaly),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly {
    /// A flip completed that no submitted frame waited for. niri sees this
    /// after resume and when monitors power on (niri issues 556 and 615).
    UnexpectedFlip,
    /// A timer fired that was not armed.
    UnexpectedTimer(Timer),
    /// A render result arrived that no `Render` effect asked for.
    UnexpectedRendered,
    /// The last flip is more than one refresh interval in the future.
    FlipFarInFuture,
}

/// Frame timing of one output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePacer {
    clock: FrameClock,
    throttle: VBlankThrottle,
    redraw: RedrawState,
    flip_timeout: Duration,
    /// Target of the render that was asked for and not answered yet.
    target: Option<Duration>,
    /// Sequence of a completed flip that the throttle holds back.
    held_flip: Option<u32>,
    watchdog_armed: bool,
    /// After the last redraw, some ongoing animations still remain.
    unfinished_animations_remain: bool,
    /// Sequence for frame callback throttling.
    ///
    /// We want to send frame callbacks for each surface at most once per
    /// monitor refresh cycle.
    ///
    /// Even if a surface commit resulted in empty damage to the monitor, we
    /// want to delay the next frame callback until roughly when a vblank
    /// would occur, had the monitor been damaged. This is necessary to
    /// prevent clients busy-looping with frame callbacks that result in empty
    /// damage.
    ///
    /// This counter wrapping-increments by 1 every time we move into the next
    /// refresh cycle, as far as frame callback throttling is concerned.
    /// Specifically, it happens:
    ///
    /// 1. Upon a successful frame submission. Notably, we don't wait for the
    ///    vblank here, because the client buffers are already "latched" at the
    ///    point of submission. Even if a client submits a new buffer right
    ///    away, we will wait for a vblank to draw it, which means that busy
    ///    looping is avoided.
    /// 2. If a frame resulted in empty damage, a timer is queued to fire
    ///    roughly when a vblank would occur, based on the last presentation
    ///    time and output refresh interval. Sequence is incremented in that
    ///    timer, before attempting a redraw or sending frame callbacks.
    frame_callback_sequence: u32,
}

impl FramePacer {
    /// `flip_timeout` is how long a submitted frame may wait for its flip
    /// before it counts as lost.
    #[must_use]
    pub fn new(refresh_interval: Option<Duration>, flip_timeout: Duration) -> Self {
        Self {
            clock: FrameClock::new(refresh_interval),
            throttle: VBlankThrottle::default(),
            redraw: RedrawState::Idle,
            flip_timeout,
            target: None,
            held_flip: None,
            watchdog_armed: false,
            unfinished_animations_remain: false,
            frame_callback_sequence: 0,
        }
    }

    #[must_use]
    pub fn redraw_state(&self) -> RedrawState {
        self.redraw
    }

    #[must_use]
    pub fn frame_callback_sequence(&self) -> u32 {
        self.frame_callback_sequence
    }

    #[must_use]
    pub fn refresh_interval(&self) -> Option<Duration> {
        self.clock.refresh_interval()
    }

    /// At most five effects come out of one event.
    #[must_use]
    pub fn step(mut self, event: Event) -> (Self, Vec<Effect>) {
        let mut effects = Vec::new();
        match event {
            Event::QueueRedraw => self.redraw = self.redraw.queue_redraw(),
            Event::Redraw { now } => self.start_redraw(now, &mut effects),
            Event::Rendered {
                now,
                outcome,
                animations_remain,
            } => self.rendered(now, outcome, animations_remain, &mut effects),
            Event::FlipCompleted {
                now,
                time,
                sequence,
            } => {
                // niri's rule: a zero timestamp is one the kernel did not fill in.
                let time = time.filter(|time| !time.is_zero());
                self.flip_completed(now, time, sequence, &mut effects);
            }
            Event::TimerFired { now, timer } => self.timer_fired(now, timer, &mut effects),
        }
        (self, effects)
    }

    fn start_redraw(&mut self, now: Duration, effects: &mut Vec<Effect>) {
        if !matches!(
            self.redraw,
            RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued
        ) {
            return;
        }
        let (target, far_early) = self.clock.predict(now);
        if far_early {
            effects.push(Effect::Anomaly(Anomaly::FlipFarInFuture));
        }
        self.target = Some(target);
        effects.push(Effect::Render { target });
    }

    fn rendered(
        &mut self,
        now: Duration,
        outcome: RenderOutcome,
        animations_remain: bool,
        effects: &mut Vec<Effect>,
    ) {
        let timer_running = match self.redraw {
            RedrawState::Queued => false,
            RedrawState::WaitingForEstimatedVBlankAndQueued => true,
            RedrawState::Idle
            | RedrawState::WaitingForVBlank { .. }
            | RedrawState::WaitingForEstimatedVBlank => {
                effects.push(Effect::Anomaly(Anomaly::UnexpectedRendered));
                return;
            }
        };
        let Some(target) = self.target.take() else {
            effects.push(Effect::Anomaly(Anomaly::UnexpectedRendered));
            return;
        };
        self.unfinished_animations_remain = animations_remain;

        match outcome {
            RenderOutcome::Submitted => {
                // A flip already received belongs to an older submission.
                if self.held_flip.take().is_some() {
                    effects.push(Effect::CancelTimer(Timer::Throttle));
                }
                if timer_running {
                    effects.push(Effect::CancelTimer(Timer::EstimatedVBlank));
                }
                self.redraw = RedrawState::WaitingForVBlank {
                    redraw_needed: false,
                };
                // The frame was queued, so the current client buffers were
                // latched. Frame callbacks can go out now: a new commit will
                // no longer overwrite this frame and will wait for a vblank.
                self.frame_callback_sequence = self.frame_callback_sequence.wrapping_add(1);
                self.watchdog_armed = true;
                effects.push(Effect::ArmTimer {
                    timer: Timer::FlipWatchdog,
                    at: now.saturating_add(self.flip_timeout),
                });
            }
            RenderOutcome::NoDamage | RenderOutcome::Failed => {
                if !timer_running {
                    let mut delay = target.saturating_sub(now);
                    // No use in a zero timer: frame callbacks go out right
                    // below anyway. Happens when no presentation time is known.
                    if delay.is_zero() {
                        delay = self
                            .clock
                            .refresh_interval()
                            .unwrap_or(FALLBACK_REFRESH_INTERVAL);
                    }
                    effects.push(Effect::ArmTimer {
                        timer: Timer::EstimatedVBlank,
                        at: now.saturating_add(delay),
                    });
                }
                self.redraw = RedrawState::WaitingForEstimatedVBlank;
            }
            RenderOutcome::Skipped => {
                self.redraw = if timer_running {
                    RedrawState::WaitingForEstimatedVBlank
                } else {
                    RedrawState::Idle
                };
            }
        }

        effects.push(Effect::SendFrameCallbacks {
            sequence: self.frame_callback_sequence,
        });
    }

    fn flip_completed(
        &mut self,
        now: Duration,
        time: Option<Duration>,
        sequence: u32,
        effects: &mut Vec<Effect>,
    ) {
        // A hardware stamp slightly ahead of dispatch is common. A stamp
        // more than a refresh ahead cannot anchor either pacing or feedback.
        let refresh = self.clock.refresh_interval();
        let mut time = time.filter(|stamp| {
            let valid = refresh.is_none_or(|interval| *stamp <= now.saturating_add(interval));
            if !valid {
                effects.push(Effect::Anomaly(Anomaly::FlipFarInFuture));
            }
            valid
        });
        let mut timestamp = time.unwrap_or(now);
        let replaces_held_flip = self.held_flip.take().is_some();

        // A software completion has no physical vblank to pace it when the
        // output rate is unknown. Use the same estimate as an undamaged frame.
        let pacing_interval =
            refresh.or_else(|| time.is_none().then_some(FALLBACK_REFRESH_INTERVAL));
        if let Some(at) = self.throttle.deadline(pacing_interval, time, now) {
            if at > now {
                self.cancel_watchdog(effects);
                self.held_flip = Some(sequence);
                effects.push(Effect::ArmTimer {
                    timer: Timer::Throttle,
                    at,
                });
                return;
            }
            // Dispatch already consumed the whole pacing delay. Deliver as
            // if the throttle timer had just fired at the dispatch time.
            timestamp = now;
            time = None;
            self.throttle.throttle(None, now);
        }
        if replaces_held_flip {
            effects.push(Effect::CancelTimer(Timer::Throttle));
        }

        let redraw_needed = match self.redraw {
            RedrawState::WaitingForVBlank { redraw_needed } => {
                self.cancel_watchdog(effects);
                redraw_needed
            }
            state => {
                effects.push(Effect::Anomaly(Anomaly::UnexpectedFlip));
                // Remove the leftover timer.
                if matches!(
                    state,
                    RedrawState::WaitingForEstimatedVBlank
                        | RedrawState::WaitingForEstimatedVBlankAndQueued
                ) {
                    effects.push(Effect::CancelTimer(Timer::EstimatedVBlank));
                }
                true
            }
        };
        self.redraw = RedrawState::Idle;

        effects.push(Effect::Presented {
            time: timestamp,
            refresh: self.clock.refresh_interval(),
            sequence,
            hw_clock: time.is_some(),
        });
        self.clock.presented(time);

        if redraw_needed || self.unfinished_animations_remain {
            self.redraw = RedrawState::Queued;
        } else {
            effects.push(Effect::SendFrameCallbacks {
                sequence: self.frame_callback_sequence,
            });
        }
    }

    fn timer_fired(&mut self, now: Duration, timer: Timer, effects: &mut Vec<Effect>) {
        // A timer that fires in a state that did not arm it is reported and
        // changes nothing. niri resets the output to idle here, which drops
        // a queued redraw.
        let unexpected = Effect::Anomaly(Anomaly::UnexpectedTimer(timer));
        match timer {
            Timer::EstimatedVBlank => match self.redraw {
                RedrawState::WaitingForEstimatedVBlank => {
                    // We waited for the timer, now frame callbacks can go out again.
                    self.frame_callback_sequence = self.frame_callback_sequence.wrapping_add(1);
                    if self.unfinished_animations_remain {
                        self.redraw = RedrawState::Queued;
                    } else {
                        self.redraw = RedrawState::Idle;
                        effects.push(Effect::SendFrameCallbacks {
                            sequence: self.frame_callback_sequence,
                        });
                    }
                }
                // The timer fired just in front of a redraw.
                RedrawState::WaitingForEstimatedVBlankAndQueued => {
                    self.frame_callback_sequence = self.frame_callback_sequence.wrapping_add(1);
                    self.redraw = RedrawState::Queued;
                }
                RedrawState::Idle | RedrawState::Queued | RedrawState::WaitingForVBlank { .. } => {
                    effects.push(unexpected);
                }
            },
            Timer::Throttle => match self.held_flip {
                Some(sequence) => {
                    // Delivered again as a flip with an unknown time; its own
                    // timer is spent, so it must not be cancelled.
                    self.held_flip = None;
                    self.flip_completed(now, None, sequence, effects);
                }
                None => effects.push(unexpected),
            },
            Timer::FlipWatchdog => {
                if self.watchdog_armed {
                    self.watchdog_armed = false;
                    self.redraw = RedrawState::Queued;
                    effects.push(Effect::FlipTimedOut);
                } else {
                    effects.push(unexpected);
                }
            }
        }
    }

    fn cancel_watchdog(&mut self, effects: &mut Vec<Effect>) {
        if self.watchdog_armed {
            self.watchdog_armed = false;
            effects.push(Effect::CancelTimer(Timer::FlipWatchdog));
        }
    }
}
