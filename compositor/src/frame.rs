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
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::{
    Kind, RenderElementStates, default_primary_scanout_output_compare,
};
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree, surface_primary_scanout_output,
    take_presentation_feedback_surface_tree, update_surface_primary_scanout_output,
    with_surfaces_surface_tree,
};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Monotonic, Point};
use smithay::wayland::compositor::{SurfaceAttributes, SurfaceData, with_states};
use smithay::wayland::presentation::Refresh;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use tracing::warn;

use crate::state::State;

/// A surface that is not shown gets a frame callback when its last one is
/// older than this.
const FRAME_CALLBACK_THROTTLE: Duration = Duration::from_millis(995);
/// How long after a round of frame callbacks the surfaces that are still
/// waiting for one are looked at again.
const FRAME_CALLBACK_FALLBACK: Duration = Duration::from_secs(1);

/// One deadline per timer of the output: the three of the frame pacer and
/// the frame-callback fallback.
#[derive(Debug, Default)]
pub(crate) struct Timers {
    estimated_vblank: Option<Duration>,
    throttle: Option<Duration>,
    flip_watchdog: Option<Duration>,
    fallback: Option<Duration>,
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

    /// Feeds one event to the pacer and carries out what it asks for.
    fn pace(&mut self, now: Duration, event: Event) {
        let (pacer, effects) = self.pacer.clone().step(event);
        self.pacer = pacer;
        for effect in effects {
            match effect {
                Effect::Render { .. } => {
                    // Nothing animates yet, so the target time has no user.
                    let outcome = self.render();
                    self.pace(
                        now,
                        Event::Rendered {
                            now,
                            outcome,
                            animations_remain: false,
                        },
                    );
                    if outcome == RenderOutcome::Submitted {
                        // Nothing to wait for: the image in memory is the screen.
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
                Effect::ArmTimer { timer, at } => *self.timers.slot(timer) = Some(at),
                Effect::CancelTimer(timer) => *self.timers.slot(timer) = None,
                Effect::SendFrameCallbacks { sequence } => self.send_frame_callbacks(now, sequence),
                Effect::Presented {
                    time,
                    refresh,
                    sequence,
                    hw_clock: _,
                } => {
                    if let Some(mut feedback) = self.pending_feedback.take() {
                        let refresh = refresh.map_or(Refresh::Unknown, Refresh::Fixed);
                        // No vsync, hardware clock or hardware completion to claim.
                        feedback.presented::<_, Monotonic>(
                            time,
                            refresh,
                            u64::from(sequence),
                            wp_presentation_feedback::Kind::empty(),
                        );
                    }
                }
                Effect::FlipTimedOut => {
                    warn!("flip did not complete; the frame counts as lost");
                    if let Some(mut feedback) = self.pending_feedback.take() {
                        feedback.discarded();
                    }
                }
                Effect::Anomaly(anomaly) => warn!(?anomaly, "frame pacer anomaly"),
            }
        }
    }

    fn render(&mut self) -> RenderOutcome {
        let mut elements = Vec::new();
        // Front to back, as the damage tracker wants them.
        for (id, rect) in self.placements.iter().rev() {
            let Some(window) = self.windows.iter().find(|window| window.id == *id) else {
                continue;
            };
            let surface = window.toplevel.wl_surface();
            // The window geometry, not the buffer, is what fills the rectangle.
            let geometry = with_states(surface, |states| {
                states
                    .cached_state
                    .get::<SurfaceCachedState>()
                    .current()
                    .geometry
            });
            let offset = geometry.map_or_else(Point::default, |geometry| geometry.loc);
            elements.extend(render_elements_from_surface_tree(
                self.backend.renderer(),
                surface,
                (rect.loc - offset).to_physical(1),
                1.0,
                1.0,
                Kind::Unspecified,
            ));
        }

        match self.backend.render(&elements) {
            Ok((damaged, states)) => {
                self.update_primary_scanout_output(&states);
                if damaged {
                    self.pending_feedback = Some(self.take_presentation_feedback());
                    RenderOutcome::Submitted
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

    /// Records which surfaces this frame shows, and tells them that they are
    /// on the output.
    fn update_primary_scanout_output(&self, states: &RenderElementStates) {
        for window in &self.windows {
            let placed = self.placements.iter().any(|(id, _)| *id == window.id);
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
