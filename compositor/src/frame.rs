// Adapter between the frame pacer of `skorupa-compositor-core` and Smithay.
//
// Frame callbacks and presentation feedback are ported from niri,
// https://github.com/niri-wm/niri, GPL-3.0-or-later, revision
// 02ee0585b9da359cb30aa2a13a6023a6e471cc23:
//   src/niri.rs:207, 5526-5643 (send_frame_callbacks), 5645-5716
//     (send_frame_callbacks_on_fallback_timer), 5718-5799
//     (take_presentation_feedbacks)
//   src/backend/tty.rs:1787-1809 (presentation feedback of a flip)
// Changes are listed in docs/niri-ports.md.

use std::cell::Cell;
use std::error::Error;
use std::time::Duration;

use skorupa_compositor_core::frame_clock::{Effect, Event, RedrawState, RenderOutcome, Timer};
use skorupa_compositor_core::lock::{Clocks, Content, Event as LockEvent};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::{
    Element, Kind, RenderElementStates, default_primary_scanout_output_compare,
};
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::backend::renderer::{Color32F, ImportAll, Renderer};
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree, surface_primary_scanout_output,
    take_presentation_feedback_surface_tree, update_surface_primary_scanout_output,
    with_surfaces_surface_tree,
};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Monotonic, Point, Rectangle};
use smithay::wayland::compositor::{
    SurfaceAttributes, SurfaceData, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::presentation::Refresh;
use tracing::warn;

use crate::drm::DrmFlip;
use crate::layout::WindowId;
use crate::state::{Backend, State, Window};

/// Shown where no window is.
pub(crate) const BACKGROUND: Color32F = Color32F::new(0.1, 0.1, 0.1, 1.0);
// REQ-LOCK-014: compositor-owned fallback even when no lock UI is available.
pub(crate) const PROTECTED_BACKGROUND: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);
pub(crate) type Placement = (WindowId, Rectangle<i32, Logical>);

pub(crate) fn background(content: Content) -> Color32F {
    match content {
        Content::Session => BACKGROUND,
        Content::Protected => PROTECTED_BACKGROUND,
    }
}

pub(crate) fn elements<R: Renderer + ImportAll>(
    renderer: &mut R,
    output: &Output,
    windows: &[Window],
    placements: &[Placement],
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R::TextureId: Clone + 'static,
{
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let viewport = Rectangle::from_size(mode.size);
    let mut elements: Vec<WaylandSurfaceRenderElement<R>> = Vec::new();
    for (id, rect) in placements.iter().rev() {
        let Some(window) = windows.iter().find(|window| window.id == *id) else {
            continue;
        };
        let surface = window.toplevel.wl_surface();
        let geometry = crate::state::geometry::effective(surface);
        let origin = if window.dialog {
            let center_offset = Point::from((
                (rect.size.w - geometry.size.w) / 2,
                (rect.size.h - geometry.size.h) / 2,
            ));
            rect.loc + center_offset - geometry.loc
        } else {
            rect.loc - geometry.loc
        };
        elements.extend(render_elements_from_surface_tree(
            renderer,
            surface,
            origin.to_physical(1),
            1.0,
            1.0,
            Kind::Unspecified,
        ));
    }
    // DRM can mark every element below an opaque fullscreen buffer as skipped,
    // even one outside the output. Keep the render states tied to this viewport.
    elements.retain(|element| viewport.overlaps(element.geometry(1.0.into())));
    elements
}

#[derive(Debug)]
pub(crate) struct RenderFailure {
    pub(crate) cause: Box<dyn Error>,
    pub(crate) redraw_needed: bool,
}

impl From<Box<dyn Error>> for RenderFailure {
    fn from(cause: Box<dyn Error>) -> Self {
        Self {
            cause,
            redraw_needed: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct RenderedFrame {
    pub(crate) damaged: bool,
    pub(crate) states: RenderElementStates,
    pub(crate) feedback: Option<OutputPresentationFeedback>,
    pub(crate) redraw_needed: bool,
}

impl RenderedFrame {
    pub(crate) fn new(
        output: &Output,
        windows: &[Window],
        content: Content,
        damaged: bool,
        states: RenderElementStates,
    ) -> Self {
        // REQ-PERF-015: take callbacks from the commits used by render_frame,
        // before a fence wait gives clients a chance to replace those commits.
        let feedback = (damaged && content == Content::Session).then(|| {
            let mut feedback = OutputPresentationFeedback::new(output);
            for window in windows {
                take_presentation_feedback_surface_tree(
                    window.toplevel.wl_surface(),
                    &mut feedback,
                    |surface, _| {
                        states
                            .element_was_presented(surface)
                            .then(|| output.clone())
                    },
                    |_, _| wp_presentation_feedback::Kind::empty(),
                );
            }
            feedback
        });
        Self {
            damaged,
            states,
            feedback,
            redraw_needed: false,
        }
    }
}

/// A surface that is not shown gets a frame callback when its last one is
/// older than this.
const FRAME_CALLBACK_THROTTLE: Duration = Duration::from_millis(995);
/// How long after a round of frame callbacks the surfaces that are still
/// waiting for one are looked at again.
const FRAME_CALLBACK_FALLBACK: Duration = Duration::from_secs(1);
const COVER_TIMEOUT: Duration = Duration::from_millis(500);
const COVER_RETRY: Duration = Duration::from_millis(100);
const COVER_BLANK_ATTEMPTS: u8 = 3;

#[derive(Debug, Default)]
struct CoverTimer {
    deadline: Option<Duration>,
    attempts: u8,
}

impl CoverTimer {
    fn arm_at(&mut self, deadline: Duration) {
        if self.attempts == 0 && self.deadline.is_none() {
            self.deadline = Some(deadline);
        }
    }

    fn blank_due(&mut self, now: Duration, waiting_for_cover: bool) -> bool {
        if !waiting_for_cover {
            *self = Self::default();
            return false;
        }
        self.arm_at(now.saturating_add(COVER_TIMEOUT));
        if self.deadline.is_none_or(|at| now < at) {
            return false;
        }
        self.attempts += 1;
        self.deadline =
            (self.attempts < COVER_BLANK_ATTEMPTS).then_some(now.saturating_add(COVER_RETRY));
        true
    }

    fn recovery_signal(&mut self, now: Duration) {
        if self.attempts == COVER_BLANK_ATTEMPTS {
            self.attempts = 0;
            self.deadline = Some(now.saturating_add(COVER_RETRY));
        }
    }
}

/// Output deadlines for frame pacing, client callbacks and lock fallback.
#[derive(Debug, Default)]
pub(crate) struct Timers {
    estimated_vblank: Option<Duration>,
    throttle: Option<Duration>,
    flip_watchdog: Option<Duration>,
    fallback: Option<Duration>,
    cover: CoverTimer,
}

const PACER_TIMERS: [Timer; 3] = [Timer::EstimatedVBlank, Timer::Throttle, Timer::FlipWatchdog];

impl Timers {
    fn slot(&mut self, timer: Timer) -> &mut Option<Duration> {
        match timer {
            Timer::EstimatedVBlank => &mut self.estimated_vblank,
            Timer::Throttle => &mut self.throttle,
            Timer::FlipWatchdog => &mut self.flip_watchdog,
        }
    }

    pub(crate) fn next(&self) -> Option<Duration> {
        [
            self.estimated_vblank,
            self.throttle,
            self.flip_watchdog,
            self.fallback,
            self.cover.deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Takes the earliest pacer timer that is due at `now`.
    fn take_due(&mut self, now: Duration) -> Option<Timer> {
        let (at, timer) = PACER_TIMERS
            .into_iter()
            .filter_map(|timer| Some(((*self.slot(timer))?, timer)))
            .min()?;
        if at > now {
            return None;
        }
        *self.slot(timer) = None;
        Some(timer)
    }
}

/// The refresh cycle in which the surface last got a frame callback.
#[derive(Default)]
struct LastFrameCallback(Cell<Option<u32>>);

impl State {
    pub(crate) fn arm_cover_deadline(&mut self, now: Duration) {
        if let Backend::Drm(backend) = &self.backend
            && backend.can_blank()
        {
            let deadline = if backend.was_off() {
                now
            } else {
                now.saturating_add(COVER_TIMEOUT)
            };
            self.timers.cover.arm_at(deadline);
        }
    }

    pub(crate) fn queue_redraw(&mut self) {
        if let Backend::Drm(backend) = &mut self.backend {
            backend.queue_redraw();
        }
        // Queueing has no effects, so there is nothing to carry out.
        self.pacer = self.pacer.clone().step(Event::QueueRedraw).0;
    }

    pub(crate) fn redraw_queued(&self) -> bool {
        matches!(
            self.pacer.redraw_state(),
            RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued
        )
    }

    /// The timers that are due, then the event-loop tail. At most one frame
    /// is rendered per call.
    pub(crate) fn tick(&mut self, now: Duration, sample: &mut impl FnMut() -> Clocks) {
        self.blank_uncovered_output(now);
        // Each kind at most once: a timer armed while these run is due
        // later than `now`, and if not, the next call takes it.
        for _ in PACER_TIMERS {
            let Some(timer) = self.timers.take_due(now) else {
                break;
            };
            self.pace(now, Event::TimerFired { now, timer }, sample);
        }
        if self.timers.fallback.is_some_and(|at| at <= now) {
            self.timers.fallback = None;
            self.send_overdue_frame_callbacks(now);
        }
        self.pace(now, Event::Redraw { now }, sample);
    }

    fn blank_uncovered_output(&mut self, now: Duration) {
        let Backend::Drm(backend) = &mut self.backend else {
            return;
        };
        let waiting_for_cover = backend.can_blank()
            && self
                .session_lock
                .as_ref()
                .is_some_and(crate::session_lock::SessionLock::needs_cover_frame);
        // REQ-LOCK-011: the frame watchdog may be waiting for an older frame.
        // Bound the cover wait independently of page-flip and render retries.
        if self.timers.cover.blank_due(now, waiting_for_cover) {
            match backend.blank() {
                Ok(()) => {
                    self.reset_frame_clock();
                    self.lock_event(LockEvent::OutputOff);
                }
                Err(err) => warn!(%err, "could not blank the uncovered DRM output"),
            }
        }
    }

    /// Feeds one event to the pacer and carries out what it asks for.
    fn pace(&mut self, now: Duration, event: Event, sample: &mut impl FnMut() -> Clocks) {
        let (pacer, effects) = self.pacer.clone().step(event);
        self.pacer = pacer;
        for effect in effects {
            match effect {
                Effect::Render { .. } => {
                    // Nothing animates yet, so the target time has no user.
                    let content = if self.session_visible() {
                        Content::Session
                    } else {
                        Content::Protected
                    };
                    let (outcome, retry) = self.render(now, content, sample);
                    let retry = retry
                        || (outcome == RenderOutcome::Failed && content == Content::Protected);
                    if outcome == RenderOutcome::Failed {
                        // The pacer is already rendering. Its estimated vblank
                        // below will pace protected retries.
                        self.lock_event(LockEvent::RenderFailed);
                    }
                    let (now, immediate) = if outcome == RenderOutcome::Submitted {
                        self.lock_event(LockEvent::FrameSubmitted(content));
                        // Rendering and a blocking KMS commit can advance the clock.
                        // The watchdog starts at submission. An immediate presentation
                        // uses that same sample and has no retrace counter.
                        let finished = sample();
                        self.lock_clock(finished);
                        let immediate = match &self.backend {
                            Backend::Headless(_) => true,
                            Backend::Drm(backend) => backend.committed_synchronously(),
                        };
                        (finished.monotonic, immediate)
                    } else {
                        (now, false)
                    };
                    self.pace(
                        now,
                        Event::Rendered {
                            now,
                            outcome,
                            animations_remain: retry,
                        },
                        sample,
                    );
                    if immediate {
                        self.lock_event(LockEvent::FramePresented(content));
                        self.pace(
                            now,
                            Event::FlipCompleted {
                                now,
                                time: None,
                                sequence: 0,
                            },
                            sample,
                        );
                    }
                }
                Effect::ArmTimer { timer, at } => *self.timers.slot(timer) = Some(at),
                Effect::CancelTimer(timer) => *self.timers.slot(timer) = None,
                Effect::SendFrameCallbacks { sequence } => self.send_frame_callbacks(now, sequence),
                Effect::Presented {
                    time,
                    refresh,
                    sequence,
                    hw_clock,
                } => {
                    if let Some(mut feedback) = self.pending_feedback.take() {
                        if !self.session_visible() {
                            feedback.discarded();
                            continue;
                        }
                        // The presentation protocol carries the interval in u32 nanoseconds.
                        let refresh = refresh
                            .filter(|interval| u32::try_from(interval.as_nanos()).is_ok())
                            .map_or(Refresh::Unknown, Refresh::Fixed);
                        let flags = if hw_clock {
                            wp_presentation_feedback::Kind::Vsync
                                | wp_presentation_feedback::Kind::HwClock
                                | wp_presentation_feedback::Kind::HwCompletion
                        } else {
                            wp_presentation_feedback::Kind::empty()
                        };
                        feedback.presented::<_, Monotonic>(
                            time,
                            refresh,
                            u64::from(sequence),
                            flags,
                        );
                    }
                }
                Effect::FlipTimedOut => {
                    warn!("flip did not complete; the frame counts as lost");
                    self.discard_pending_feedback();
                    if let Backend::Drm(backend) = &mut self.backend {
                        backend.expire_flip();
                    }
                    self.lock_event(LockEvent::FlipFailed);
                }
                Effect::Anomaly(anomaly) => warn!(?anomaly, "frame pacer anomaly"),
            }
        }
    }

    fn needs_cover_frame(&self) -> bool {
        self.session_lock
            .as_ref()
            .is_some_and(crate::session_lock::SessionLock::needs_cover_frame)
    }

    fn render(
        &mut self,
        now: Duration,
        content: Content,
        sample: &mut impl FnMut() -> Clocks,
    ) -> (RenderOutcome, bool) {
        let protected = content == Content::Protected;
        let force_full_redraw = protected && self.needs_cover_frame();
        if let Backend::Drm(backend) = &mut self.backend {
            if !backend.ready_to_render(content) {
                return (RenderOutcome::Skipped, false);
            }
            let placements = if protected { &[][..] } else { &self.placements };
            match backend.prepare(
                &self.output,
                &self.windows,
                placements,
                content,
                force_full_redraw,
            ) {
                Ok(true) => {}
                Ok(false) => return (RenderOutcome::NoDamage, true),
                Err(err) => {
                    warn!(%err, "preparing the DRM frame failed");
                    return (RenderOutcome::Failed, false);
                }
            }
        }

        // REQ-LOCK-025: rendering and fence checks can outlive the clock
        // sample used to select the scene. Check again before submission.
        let now = self.refresh_lock_clock(now, sample);
        if !protected && !self.session_visible() {
            if let Backend::Drm(backend) = &mut self.backend {
                backend.discard_prepared();
            }
            return (RenderOutcome::Failed, true);
        }
        if protected && self.timers.cover.deadline.is_some_and(|at| now >= at) {
            return (RenderOutcome::Failed, true);
        }

        // A resume in the sample above can require another presentation
        // even when the old and new covers have identical pixels.
        let force_full_redraw = protected && self.needs_cover_frame();
        let rendered = match &mut self.backend {
            Backend::Headless(backend) => {
                let placements = if protected { &[][..] } else { &self.placements };
                let elements =
                    elements(backend.renderer(), &self.output, &self.windows, placements);
                backend
                    .render_scene(&elements, background(content), force_full_redraw)
                    .map(|(damaged, states)| {
                        RenderedFrame::new(&self.output, &self.windows, content, damaged, states)
                    })
                    .map_err(RenderFailure::from)
            }
            Backend::Drm(backend) => backend.submit(),
        };
        match rendered {
            Ok(frame) => {
                self.update_primary_scanout_output(&frame.states);
                if frame.damaged {
                    self.pending_feedback = frame.feedback;
                    (RenderOutcome::Submitted, frame.redraw_needed)
                } else if force_full_redraw {
                    (RenderOutcome::Failed, false)
                } else {
                    (RenderOutcome::NoDamage, false)
                }
            }
            Err(failure) => {
                warn!(err = %failure.cause, "rendering the frame failed");
                (RenderOutcome::Failed, failure.redraw_needed)
            }
        }
    }

    pub(crate) fn complete_drm(&mut self, now: Duration, time: Option<Duration>, sequence: u32) {
        let Backend::Drm(backend) = &mut self.backend else {
            return;
        };
        match backend.complete() {
            DrmFlip::Presented(content) => {
                self.timers.cover.recovery_signal(now);
                // REQ-LOCK-002 uses KMS completion even when the pacer delays client feedback.
                self.lock_event(LockEvent::FramePresented(content));
                self.pace(
                    now,
                    Event::FlipCompleted {
                        now,
                        time,
                        sequence,
                    },
                    &mut crate::read_lock_clocks,
                );
            }
            DrmFlip::Expired => {
                self.timers.cover.recovery_signal(now);
                let woke = backend.wake_if_ready();
                let ready = backend.output_on();
                if woke {
                    self.lock_event(LockEvent::OutputOn);
                }
                if ready {
                    self.queue_redraw();
                }
            }
            DrmFlip::Ignored => {}
        }
    }

    pub(crate) fn pause_drm(&mut self) {
        if let Backend::Drm(backend) = &mut self.backend {
            backend.pause();
            // Losing the seat invalidates a pending flip as lock evidence.
            // The kernel has not confirmed the output is off.
            self.lock_event(LockEvent::OutputUnavailable);
            self.reset_frame_clock();
        }
    }

    fn reset_frame_clock(&mut self) {
        self.pacer.reset();
        self.timers = Timers::default();
        self.discard_pending_feedback();
    }

    pub(crate) fn wake_drm(&mut self, now: Duration) {
        // Input can ask a recovered device to retry after the bounded burst.
        self.timers.cover.recovery_signal(now);
        if let Backend::Drm(backend) = &mut self.backend
            && backend.request_wake()
        {
            self.lock_event(LockEvent::OutputOn);
        }
    }

    pub(crate) fn activate_drm(&mut self) {
        if let Backend::Drm(backend) = &mut self.backend {
            match backend.activate() {
                Ok(()) => {
                    self.lock_event(LockEvent::OutputOn);
                    self.queue_redraw();
                }
                Err(err) => warn!(%err, "could not activate the DRM output"),
            }
        }
    }

    pub(crate) fn fence_frame_feedback(&mut self) {
        if let Backend::Drm(backend) = &mut self.backend {
            backend.discard_prepared();
        }
        self.timers.fallback = None;
        self.discard_pending_feedback();
    }

    fn discard_pending_feedback(&mut self) {
        if let Some(mut feedback) = self.pending_feedback.take() {
            feedback.discarded();
        }
    }

    /// Records which surfaces this frame shows, and tells them that they are
    /// on the output.
    pub(super) fn update_primary_scanout_output(&self, states: &RenderElementStates) {
        for window in &self.windows {
            with_surfaces_surface_tree(window.toplevel.wl_surface(), |surface, data| {
                // Fully occluded elements still belong to the output. Use the
                // submitted frame, even if clients changed state during a fence wait.
                if states.element_render_state(surface).is_some() {
                    self.output.enter(surface);
                } else {
                    self.output.leave(surface);
                }
                update_surface_primary_scanout_output(
                    surface,
                    &self.output,
                    data,
                    None,
                    states,
                    default_primary_scanout_output_compare,
                );
            });
        }
    }

    fn send_frame_callbacks(&mut self, now: Duration, sequence: u32) {
        if !self.session_visible() {
            return;
        }
        let output = &self.output;
        let should_send = |surface: &WlSurface, states: &SurfaceData| -> Option<Output> {
            // Only a surface that the last frame showed.
            if surface_primary_scanout_output(surface, states).as_ref() != Some(output) {
                return None;
            }
            // If we already sent a frame callback to this surface this output
            // refresh cycle, don't send one again to prevent empty-damage
            // commit busy loops.
            let last = states.data_map.get_or_insert(LastFrameCallback::default);
            if last.0.replace(Some(sequence)) == Some(sequence) {
                return None;
            }
            Some(output.clone())
        };
        for window in &self.windows {
            send_frames_surface_tree(
                window.toplevel.wl_surface(),
                output,
                now,
                Some(FRAME_CALLBACK_THROTTLE),
                should_send,
            );
        }
        self.arm_fallback(now);
    }

    /// The fallback for surfaces that no frame shows: they get a callback
    /// about once a second, so that their clients neither stall nor spin.
    fn send_overdue_frame_callbacks(&mut self, now: Duration) {
        if !self.session_visible() {
            return;
        }
        for window in &self.windows {
            send_frames_surface_tree(
                window.toplevel.wl_surface(),
                &self.output,
                now,
                Some(FRAME_CALLBACK_THROTTLE),
                |_, _| None,
            );
        }
        self.arm_fallback(now);
    }

    /// Keeps the fallback timer running only while some surface still waits
    /// for a frame callback, so that an idle compositor does not wake up.
    fn arm_fallback(&mut self, now: Duration) {
        if self.timers.fallback.is_some() {
            return;
        }
        let waiting = self.windows.iter().any(|window| {
            let mut waiting = false;
            with_surface_tree_downward(
                window.toplevel.wl_surface(),
                (),
                |_, states, &()| {
                    // Frame delivery skips a bufferless surface and all its descendants.
                    let mapped = states
                        .data_map
                        .get::<RendererSurfaceStateUserData>()
                        .is_some_and(|state| {
                            state.lock().is_ok_and(|state| state.view().is_some())
                        });
                    if !mapped {
                        return TraversalAction::SkipChildren;
                    }
                    let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                    if attributes.current().frame_callbacks.is_empty() {
                        TraversalAction::DoChildren(())
                    } else {
                        waiting = true;
                        TraversalAction::Break
                    }
                },
                |_, _, &()| {},
                |_, _, &()| true,
            );
            waiting
        });
        if waiting {
            self.timers.fallback = Some(now.saturating_add(FRAME_CALLBACK_FALLBACK));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_perf_014_frame_clock_reset_does_not_reuse_the_last_callback_cycle() {
        let mut compositor = crate::Compositor::new(crate::Config::default()).unwrap();
        let now = Duration::from_secs(1);
        compositor.dispatch(now).unwrap();
        let previous = compositor.state.pacer.frame_callback_sequence();
        assert_eq!(previous, 1);
        compositor.state.reset_frame_clock();

        // The surfaces still carry the previous cycle when the seat returns.
        let mut pacer = compositor.state.pacer.clone();
        for event in [
            Event::QueueRedraw,
            Event::Redraw { now },
            Event::Rendered {
                now,
                outcome: RenderOutcome::Submitted,
                animations_remain: false,
            },
        ] {
            pacer = pacer.step(event).0;
        }
        assert_ne!(pacer.frame_callback_sequence(), previous);
    }

    #[test]
    fn req_perf_003_reset_keeps_the_exact_mode_interval() {
        let mut compositor = crate::Compositor::new(crate::Config {
            refresh_mhz: 59_940,
            ..crate::Config::default()
        })
        .unwrap();
        // 148.352 MHz and 2200 by 1125 total pixels.
        let interval = Some(Duration::from_nanos(16_683_294));
        compositor.state.pacer = skorupa_compositor_core::frame_clock::FramePacer::new(
            interval,
            compositor.state.config.flip_timeout,
        );
        compositor.state.reset_frame_clock();
        assert_eq!(compositor.state.pacer.refresh_interval(), interval);
    }

    #[test]
    fn req_lock_011_blanking_retries_are_bounded_and_rearm_on_recovery() {
        let mut timer = CoverTimer::default();
        timer.arm_at(COVER_TIMEOUT);
        let mut now = COVER_TIMEOUT;
        for _ in 0..COVER_BLANK_ATTEMPTS {
            assert!(timer.blank_due(now, true));
            now += COVER_RETRY;
        }
        assert!(!timer.blank_due(now, true));
        assert_eq!(timer.deadline, None);

        timer.recovery_signal(now);
        assert!(!timer.blank_due(now, true));
        assert!(timer.blank_due(now + COVER_RETRY, true));
    }
}
