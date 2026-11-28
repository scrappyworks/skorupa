//! When the next frame is presented and when to redraw.
//!
//! Three pieces, all pure, with every time passed in as a `Duration` on the
//! presentation clock:
//!
//! - [`FrameClock`] predicts the next presentation from the last real one;
//! - [`VBlankThrottle`] holds back flips that complete sooner than the
//!   output can refresh;
//! - [`FramePacer`] is the redraw state machine of one output. It is a
//!   reducer: an [`Event`] goes in, [`Effect`]s come out, and the adapter
//!   renders, keeps the timers and talks to clients.
//!
//! Ported from niri; the provenance of each file is in its header and in
//! `docs/niri-ports.md`. Variable refresh rate is left out for now.

mod clock;
mod redraw;
mod throttle;

pub use clock::{FrameClock, ModeTimings, refresh_interval};
pub use redraw::{Anomaly, Effect, Event, FramePacer, RedrawState, RenderOutcome, Timer};
pub use throttle::VBlankThrottle;
