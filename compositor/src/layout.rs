//! The seam where the layout reducer plugs in.
//!
//! The compositor asks a [`Layout`] which windows are shown, where, and which
//! one has the keyboard. Stage 1 has a single rule, [`FillOutput`]; the
//! reducer in `skorupa-compositor-core` replaces it behind the same trait.

use smithay::utils::{Logical, Rectangle, Size};

/// Names a toplevel for as long as it exists. Never reused within one
/// compositor instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

pub trait Layout {
    fn window_added(&mut self, window: WindowId);
    fn window_removed(&mut self, window: WindowId);
    /// The windows to show, bottom to top, each with its rectangle on the
    /// output. A window that is left out is hidden.
    fn placements(&self, output: Size<i32, Logical>) -> Vec<(WindowId, Rectangle<i32, Logical>)>;
    /// The window that gets keyboard input.
    fn focus(&self) -> Option<WindowId>;
}

/// Every toplevel fills the output; the newest one is on top and focused.
#[derive(Debug, Default)]
pub struct FillOutput {
    /// Oldest first.
    windows: Vec<WindowId>,
}

impl Layout for FillOutput {
    fn window_added(&mut self, window: WindowId) {
        self.windows.push(window);
    }

    fn window_removed(&mut self, window: WindowId) {
        self.windows.retain(|other| *other != window);
    }

    fn placements(&self, output: Size<i32, Logical>) -> Vec<(WindowId, Rectangle<i32, Logical>)> {
        let whole = Rectangle::from_size(output);
        self.windows.iter().map(|window| (*window, whole)).collect()
    }

    fn focus(&self) -> Option<WindowId> {
        self.windows.last().copied()
    }
}
