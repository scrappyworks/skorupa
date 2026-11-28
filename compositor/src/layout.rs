//! Adapts the window layout reducer to Smithay's logical coordinates.

use skorupa_compositor_core::layout::{Configure, Effect, Event, Layout as Reducer, Size, step};
use smithay::utils::{Logical, Rectangle};

pub use skorupa_compositor_core::layout::WindowId;

pub trait Layout {
    fn handle(&mut self, event: Event) -> Vec<Effect>;
    fn configure(&self, window: WindowId) -> Option<Configure>;
    /// Windows to show, back to front, with their slots on the output.
    fn placements(&self) -> Vec<(WindowId, Rectangle<i32, Logical>)>;
    fn focus(&self) -> Option<WindowId>;
}

pub(crate) struct WorkspaceLayout(Reducer);

impl WorkspaceLayout {
    pub(crate) fn new(width: i32, height: i32) -> Self {
        Self(Reducer::new(
            Size {
                w: width,
                h: height,
            },
            0,
        ))
    }
}

impl Layout for WorkspaceLayout {
    fn handle(&mut self, event: Event) -> Vec<Effect> {
        let (next, effects) = step(self.0.clone(), event);
        self.0 = next;
        effects
    }

    fn configure(&self, window: WindowId) -> Option<Configure> {
        self.0.configure(window)
    }

    fn placements(&self) -> Vec<(WindowId, Rectangle<i32, Logical>)> {
        self.0
            .project()
            .windows
            .into_iter()
            .map(|placement| {
                let rect = placement.rect;
                (
                    placement.window,
                    Rectangle::new((rect.x, rect.y).into(), (rect.w, rect.h).into()),
                )
            })
            .collect()
    }

    fn focus(&self) -> Option<WindowId> {
        self.0.focus()
    }
}
