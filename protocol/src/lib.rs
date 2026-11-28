//! The only contract between Skorupa processes.
//!
//! Every message travels in one frame:
//!
//! ```text
//! offset  size  field
//! 0       4     body length in bytes, little-endian (version + kind + payload)
//! 4       1     protocol version, currently 1
//! 5       1     message kind
//! 6       n     payload, laid out per kind
//! ```
//!
//! Rules that hold for every message, with the requirement each test names:
//!
//! - REQ-PROTO-007: decoding an encoded message returns that message.
//! - REQ-PROTO-008: decoding never panics and never allocates. The crate is
//!   `no_std` without `alloc`, so it cannot allocate, read a clock or do I/O.
//!   An accepted frame is canonical: it re-encodes to the same bytes, so
//!   there are no padding bytes, trailing bytes or alternative spellings to
//!   smuggle data in.
//! - REQ-PROTO-009: a frame is at most [`MAX_FRAME_LEN`] bytes. The limit is
//!   checked on the length prefix alone, before the body is read.
//! - REQ-PROTO-010: a frame of another version is refused whole.
//! - REQ-PROTO-011: part of a frame is never a message; the error says how
//!   many bytes are needed.
//! - REQ-PROTO-012: the layout of a message is fixed byte for byte.
//! - REQ-PROTO-013: every field value outside the protocol is refused with
//!   its own error. Zero is never a valid version, kind or enum value, so
//!   zeroed memory does not decode.
//! - REQ-PROTO-014: encoding into a buffer that is too small is an error.
//!
//! A reader that gets any [`DecodeError`] other than
//! [`DecodeError::Truncated`] must drop the connection. There is no way to
//! resynchronise, by design.
//!
//! ```
//! use skorupa_protocol::{
//!     Attempt, CompositorInstance, LockEpoch, MAX_FRAME_LEN, Message, Outcome, SessionId,
//!     Verdict, decode,
//! };
//!
//! let verdict = Message::Verdict(Verdict {
//!     compositor_instance: CompositorInstance([7; 16]),
//!     session: SessionId::new(b"c2")?,
//!     epoch: LockEpoch(41),
//!     attempt: Attempt(3),
//!     outcome: Outcome::Denied,
//! });
//!
//! let mut buffer = [0; MAX_FRAME_LEN];
//! let written = verdict.encode(&mut buffer)?;
//! let frame = buffer.get(..written).ok_or("encode reported more than the buffer holds")?;
//! assert_eq!(decode(frame)?, (verdict, written));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![no_std]
#![deny(missing_docs)]
// The decoder reads bytes that another process controls. These restriction
// lints keep every way to panic or lose bits out of non-test code.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::string_slice
)]

mod frame;
mod verdict;
mod wire;

pub use frame::{
    DecodeError, EncodeError, HEADER_LEN, Kind, MAX_FRAME_LEN, Message, PREFIX_LEN, VERSION,
    decode, frame_len,
};
pub use verdict::{
    Attempt, Challenge, CompositorInstance, LockEpoch, Outcome, SessionId, SessionIdError, Verdict,
};
