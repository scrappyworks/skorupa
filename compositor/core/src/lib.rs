//! Pure rules of the compositor.
//!
//! Every module here is a `step(state, event) -> (state, effects)` reducer or
//! a pure function. Nothing in this crate reads a clock, does I/O, spawns a
//! thread or depends on Smithay: time, results and identifiers arrive as
//! data, so every rule can be tested deterministically.

pub mod frame_clock;
pub mod grant;
pub mod layout;
pub mod lock;
