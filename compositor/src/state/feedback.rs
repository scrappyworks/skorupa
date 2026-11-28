//! Keep presentation feedback with the client commit that requested it.

use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{Cacheable, with_states};
use smithay::wayland::presentation::PresentationFeedbackCachedState;

/// None is an internal transaction merge without a new client commit. A
/// client commit can carry an empty list and still supersede earlier feedback.
#[derive(Default)]
struct CommitFeedback(Option<PresentationFeedbackCachedState>);

impl Cacheable for CommitFeedback {
    fn commit(&mut self, _: &DisplayHandle) -> Self {
        std::mem::take(self)
    }

    fn merge_into(self, current: &mut Self, _: &DisplayHandle) {
        if self.0.is_some() {
            *current = self;
        }
    }
}

pub(super) fn capture(surface: &WlSurface) {
    with_states(surface, |states| {
        if states.cached_state.has::<PresentationFeedbackCachedState>() {
            let mut feedback = states.cached_state.get::<PresentationFeedbackCachedState>();
            let pending = std::mem::take(feedback.pending());
            states.cached_state.get::<CommitFeedback>().pending().0 = Some(pending);
        }
    });
}

pub(super) fn apply(surface: &WlSurface) {
    with_states(surface, |states| {
        if states.cached_state.has::<CommitFeedback>()
            && let Some(next) = states
                .cached_state
                .get::<CommitFeedback>()
                .current()
                .0
                .take()
        {
            // Smithay discards the unpresented callbacks when their state is dropped.
            *states
                .cached_state
                .get::<PresentationFeedbackCachedState>()
                .current() = next;
        }
    });
}
