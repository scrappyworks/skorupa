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
use std::time::Duration;

use skorupa_compositor_core::frame_clock::{Effect, Event, RedrawState, RenderOutcome, Timer};
use skorupa_compositor_core::lock::{Content, Event as LockEvent};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::{
    Kind, RenderElementStates, default_primary_scanout_output_compare,
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
use smithay::wayland::compositor::{SurfaceAttributes, SurfaceData, with_states};
use smithay::wayland::presentation::Refresh;
use smithay::wayland::shell::xdg::SurfaceCachedState;
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
    windows: &[Window],
    placements: &[Placement],
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R::TextureId: Clone + 'static,
{
    let mut elements = Vec::new();
    for (id, rect) in placements.iter().rev() {
        let Some(window) = windows.iter().find(|window| window.id == *id) else {
            continue;
        };
        let surface = window.toplevel.wl_surface();
        let geometry = with_states(surface, |states| {
            states
                .cached_state
                .get::<SurfaceCachedState>()
                .current()
                .geometry
        });
        let offset = geometry.map_or_else(Point::default, |geometry| geometry.loc);
        elements.extend(render_elements_from_surface_tree(
            renderer,
            surface,
            (rect.loc - offset).to_physical(1),
            1.0,
            1.0,
            Kind::Unspecified,
        ));
    }
    elements
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
    fn blank_due(&mut self, now: Duration, waiting_for_cover: bool) -> bool {
        if !waiting_for_cover {
            *self = Self::default();
            return false;
        }
        if self.attempts == 0 && self.deadline.is_none() {
            self.deadline = Some(now.saturating_add(COVER_TIMEOUT));
        }
        if self.deadline.is_none_or(|at| now < at) {
            return false;
        }
        self.attempts += 1;
        self.deadline =
            (self.attempts < COVER_BLANK_ATTEMPTS).then_some(now.saturating_add(COVER_RETRY));
        true
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
    pub(crate) fn queue_redraw(&mut self) {
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
    pub(crate) fn tick(&mut self, now: Duration) {
        self.blank_uncovered_output(now);
        // Each kind at most once: a timer armed while these run is due
        // later than `now`, and if not, the next call takes it.
        for _ in PACER_TIMERS {
            let Some(timer) = self.timers.take_due(now) else {
                break;
            };
            self.pace(now, Event::TimerFired { now, timer });
        }
        if self.timers.fallback.is_some_and(|at| at <= now) {
            self.timers.fallback = None;
            self.send_overdue_frame_callbacks(now);
        }
        self.pace(now, Event::Redraw { now });
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
    fn pace(&mut self, now: Duration, event: Event) {
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
                    let outcome = self.render(content);
                    let retry = outcome == RenderOutcome::Failed && content == Content::Protected;
                    if outcome == RenderOutcome::Failed {
                        // The pacer is already rendering. Its estimated vblank
                        // below will pace protected retries.
                        self.lock_event(LockEvent::RenderFailed);
                    }
                    self.pace(
                        now,
                        Event::Rendered {
                            now,
                            outcome,
                            animations_remain: retry,
                        },
                    );
                    if outcome == RenderOutcome::Submitted {
                        self.lock_event(LockEvent::FrameSubmitted(content));
                        if matches!(self.backend, Backend::Headless(_)) {
                            // Nothing to wait for: the image in memory is the screen.
                            self.lock_event(LockEvent::FramePresented(content));
                            self.flips = self.flips.wrapping_add(1);
                            self.pace(
                                now,
                                Event::FlipCompleted {
                                    now,
                                    time: None,
                                    sequence: self.flips,
                                },
                            );
                        }
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
                        let refresh = refresh.map_or(Refresh::Unknown, Refresh::Fixed);
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

    fn render(&mut self, content: Content) -> RenderOutcome {
        let protected = content == Content::Protected;
        let force_full_redraw = protected
            && self
                .session_lock
                .as_ref()
                .is_some_and(crate::session_lock::SessionLock::needs_cover_frame);
        let placements = if protected { &[][..] } else { &self.placements };
        let rendered = match &mut self.backend {
            Backend::Headless(backend) => {
                let elements = elements(backend.renderer(), &self.windows, placements);
                backend.render_scene(&elements, background(content), force_full_redraw)
            }
            Backend::Drm(backend) => {
                if !backend.ready_to_render() {
                    return RenderOutcome::Skipped;
                }
                backend.render(&self.windows, placements, content, force_full_redraw)
            }
        };
        match rendered {
            Ok((damaged, states)) => {
                self.update_primary_scanout_output(&states);
                if damaged {
                    if !protected {
                        self.pending_feedback = Some(self.take_presentation_feedback());
                    }
                    RenderOutcome::Submitted
                } else if force_full_redraw {
                    RenderOutcome::Failed
                } else {
                    RenderOutcome::NoDamage
                }
            }
            Err(err) => {
                warn!(%err, "rendering the frame failed");
                RenderOutcome::Failed
            }
        }
    }

    pub(crate) fn complete_drm(&mut self, now: Duration, time: Option<Duration>, sequence: u32) {
        let Backend::Drm(backend) = &mut self.backend else {
            return;
        };
        match backend.complete() {
            DrmFlip::Presented(content) => {
                // REQ-LOCK-002 uses KMS completion even when the pacer delays client feedback.
                self.lock_event(LockEvent::FramePresented(content));
                self.pace(
                    now,
                    Event::FlipCompleted {
                        now,
                        time,
                        sequence,
                    },
                );
            }
            DrmFlip::Expired => {
                let woke = backend.wake_if_ready();
                let ready = backend.ready_to_render();
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
            self.lock_event(LockEvent::FlipFailed);
            self.reset_frame_clock();
        }
    }

    fn reset_frame_clock(&mut self) {
        self.pacer = skorupa_compositor_core::frame_clock::FramePacer::new(
            self.config.refresh_interval(),
            self.config.flip_timeout,
        );
        self.timers = Timers::default();
        self.discard_pending_feedback();
    }

    pub(crate) fn wake_drm(&mut self) {
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
    fn update_primary_scanout_output(&self, states: &RenderElementStates) {
        for window in &self.windows {
            let placed =
                self.session_visible() && self.placements.iter().any(|(id, _)| *id == window.id);
            with_surfaces_surface_tree(window.toplevel.wl_surface(), |surface, data| {
                update_surface_primary_scanout_output(
                    surface,
                    &self.output,
                    data,
                    None,
                    states,
                    default_primary_scanout_output_compare,
                );
                if placed {
                    self.output.enter(surface);
                } else {
                    self.output.leave(surface);
                }
            });
        }
    }

    /// The feedback of the surfaces that the frame shows. That of a surface
    /// that is not shown stays with it until a frame shows it or the client
    /// replaces it; so does all of it when a frame changed nothing.
    fn take_presentation_feedback(&self) -> OutputPresentationFeedback {
        let mut feedback = OutputPresentationFeedback::new(&self.output);
        for window in &self.windows {
            take_presentation_feedback_surface_tree(
                window.toplevel.wl_surface(),
                &mut feedback,
                surface_primary_scanout_output,
                |_, _| wp_presentation_feedback::Kind::empty(),
            );
        }
        feedback
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
        let mut waiting = false;
        for window in &self.windows {
            with_surfaces_surface_tree(window.toplevel.wl_surface(), |_, states| {
                // A surface without a buffer never gets a callback, so
                // waiting for it would keep the timer running for nothing.
                let mapped = states
                    .data_map
                    .get::<RendererSurfaceStateUserData>()
                    .is_some_and(|state| state.lock().is_ok_and(|state| state.view().is_some()));
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                waiting |= mapped && !attributes.current().frame_callbacks.is_empty();
            });
        }
        if waiting {
            self.timers.fallback = Some(now.saturating_add(FRAME_CALLBACK_FALLBACK));
        }
    }
}
