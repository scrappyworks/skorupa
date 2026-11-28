//! Framing: length prefix, version, kind, and the errors of both directions.

use core::fmt;

use crate::verdict::{Challenge, SessionIdError, Verdict};
use crate::wire::{Reader, Writer};

/// Size of the length prefix in bytes.
pub const PREFIX_LEN: usize = 4;

/// Size of everything before the payload: length prefix, version and kind.
pub const HEADER_LEN: usize = PREFIX_LEN + 2;

/// Largest frame in bytes, length prefix included. A buffer of this size
/// holds any frame.
pub const MAX_FRAME_LEN: usize = 4096;

/// Protocol version this crate writes and the only one it reads.
pub const VERSION: u8 = 1;

/// Smallest body: version and kind with an empty payload.
const MIN_BODY_LEN: usize = HEADER_LEN - PREFIX_LEN;

/// Largest body: a full frame minus the length prefix.
const MAX_BODY_LEN: usize = MAX_FRAME_LEN - PREFIX_LEN;

// Every message must fit a frame, so encoding has no "message too long" error.
const _: () = assert!(HEADER_LEN + Verdict::MAX_PAYLOAD_LEN <= MAX_FRAME_LEN);
const _: () = assert!(HEADER_LEN + Challenge::MAX_PAYLOAD_LEN <= MAX_FRAME_LEN);

/// Which message a frame carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// [`Verdict`]. Wire value 1.
    Verdict,
    /// [`Challenge`]. Wire value 2.
    Challenge,
}

impl Kind {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Verdict => 1,
            Self::Challenge => 2,
        }
    }

    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Verdict),
            2 => Some(Self::Challenge),
            _ => None,
        }
    }
}

/// A message of the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Message {
    /// The authentication broker's answer to one unlock attempt.
    Verdict(Verdict),
    /// The compositor asks the authentication broker to judge a new attempt.
    Challenge(Challenge),
}

impl Message {
    /// Which message this is.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Verdict(_) => Kind::Verdict,
            Self::Challenge(_) => Kind::Challenge,
        }
    }

    /// Size of the encoded frame in bytes, at most [`MAX_FRAME_LEN`].
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        let payload = match self {
            Self::Verdict(verdict) => verdict.payload_len(),
            Self::Challenge(challenge) => challenge.payload_len(),
        };
        HEADER_LEN.saturating_add(payload)
    }

    /// Writes the frame to the start of `out` and returns its length.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::BufferTooSmall`] if `out` is shorter than
    /// [`Self::encoded_len`]. Nothing is written in that case: `out` keeps
    /// its contents, so a short buffer never holds part of a frame.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let needed = self.encoded_len();
        let available = out.len();
        let too_small = EncodeError::BufferTooSmall { needed, available };
        // Judged before the first byte is written.
        if available < needed {
            return Err(too_small);
        }
        self.write(out, needed).ok_or(too_small)
    }

    fn write(&self, out: &mut [u8], needed: usize) -> Option<usize> {
        let body_len = u32::try_from(needed.checked_sub(PREFIX_LEN)?).ok()?;
        let mut writer = Writer::new(out.get_mut(..needed)?);
        writer.put(&body_len.to_le_bytes())?;
        writer.put(&[VERSION, self.kind().to_wire()])?;
        match self {
            Self::Verdict(verdict) => verdict.write(&mut writer)?,
            Self::Challenge(challenge) => challenge.write(&mut writer)?,
        }
        (writer.written() == needed).then_some(needed)
    }
}

/// Why a message was not encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncodeError {
    /// The output buffer is shorter than the frame.
    BufferTooSmall {
        /// Size of the frame.
        needed: usize,
        /// Size of the buffer.
        available: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall { needed, available } => {
                write!(f, "frame needs {needed} bytes, the buffer has {available}")
            }
        }
    }
}

impl core::error::Error for EncodeError {}

/// Why bytes were not decoded.
///
/// [`DecodeError::Truncated`] means "read more and try again". Every other
/// variant means the peer broke the protocol and the connection must be
/// dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeError {
    /// The input ends before the frame does.
    Truncated {
        /// Bytes needed from the start of the frame before decoding can go
        /// further: the length prefix first, then the whole frame.
        needed: usize,
        /// Bytes that were offered.
        available: usize,
    },
    /// The length prefix declares a body with no room for version and kind.
    FrameTooShort {
        /// Body length from the prefix.
        declared: u32,
    },
    /// The length prefix declares a frame longer than [`MAX_FRAME_LEN`].
    FrameTooLong {
        /// Body length from the prefix.
        declared: u32,
    },
    /// The frame has a version other than [`VERSION`].
    UnsupportedVersion {
        /// Version byte of the frame.
        found: u8,
    },
    /// The frame has a kind this version does not define.
    UnknownKind {
        /// Kind byte of the frame.
        found: u8,
    },
    /// The payload ends before the message does.
    PayloadTooShort {
        /// Message being decoded.
        kind: Kind,
    },
    /// The payload continues after the message ends.
    PayloadTooLong {
        /// Message being decoded.
        kind: Kind,
        /// Bytes left over.
        extra: usize,
    },
    /// A verdict has an outcome this version does not define.
    UnknownOutcome {
        /// Outcome byte of the payload.
        found: u8,
    },
    /// A message has a malformed session identifier.
    InvalidSession(SessionIdError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => {
                write!(f, "frame needs {needed} bytes, got {available}")
            }
            Self::FrameTooShort { declared } => write!(
                f,
                "declared body of {declared} bytes is below the minimum of {MIN_BODY_LEN}"
            ),
            Self::FrameTooLong { declared } => write!(
                f,
                "declared body of {declared} bytes is above the maximum of {MAX_BODY_LEN}"
            ),
            Self::UnsupportedVersion { found } => write!(
                f,
                "protocol version {found} is not supported, expected {VERSION}"
            ),
            Self::UnknownKind { found } => write!(f, "message kind {found} is not defined"),
            Self::PayloadTooShort { kind } => write!(f, "payload of {kind:?} is too short"),
            Self::PayloadTooLong { kind, extra } => {
                write!(f, "payload of {kind:?} has {extra} bytes too many")
            }
            Self::UnknownOutcome { found } => write!(f, "verdict outcome {found} is not defined"),
            Self::InvalidSession(error) => write!(f, "{error}"),
        }
    }
}

impl core::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::InvalidSession(error) => Some(error),
            _ => None,
        }
    }
}

/// Reads the body length from a prefix and checks both bounds.
fn body_len(prefix: [u8; PREFIX_LEN]) -> Result<usize, DecodeError> {
    let declared = u32::from_le_bytes(prefix);
    match usize::try_from(declared) {
        Ok(len) if len < MIN_BODY_LEN => Err(DecodeError::FrameTooShort { declared }),
        Ok(len) if len <= MAX_BODY_LEN => Ok(len),
        _ => Err(DecodeError::FrameTooLong { declared }),
    }
}

/// Returns the size of the whole frame, length prefix included, from the
/// prefix alone.
///
/// A stream reader calls this after reading [`PREFIX_LEN`] bytes to learn how
/// many bytes the frame has, and so never buffers more than
/// [`MAX_FRAME_LEN`] for a peer.
///
/// # Errors
///
/// Returns [`DecodeError::FrameTooShort`] or [`DecodeError::FrameTooLong`] if
/// the declared length is out of bounds.
pub fn frame_len(prefix: [u8; PREFIX_LEN]) -> Result<usize, DecodeError> {
    body_len(prefix).map(|body| body.saturating_add(PREFIX_LEN))
}

/// Decodes the frame at the start of `input` and returns the message with the
/// number of bytes the frame takes. Bytes after the frame are not looked at.
///
/// # Errors
///
/// Returns [`DecodeError::Truncated`] if `input` holds only part of a frame,
/// and one of the other variants if the frame breaks the protocol. See
/// [`DecodeError`] for what the caller must do with each.
pub fn decode(input: &[u8]) -> Result<(Message, usize), DecodeError> {
    let truncated = |needed| DecodeError::Truncated {
        needed,
        available: input.len(),
    };
    let mut frame = Reader::new(input);
    let prefix = frame.array().ok_or(truncated(PREFIX_LEN))?;
    let declared = u32::from_le_bytes(prefix);
    let body_len = body_len(prefix)?;
    let frame_len = body_len.saturating_add(PREFIX_LEN);
    let mut body = Reader::new(frame.bytes(body_len).ok_or(truncated(frame_len))?);

    let [version, kind] = body
        .array()
        .ok_or(DecodeError::FrameTooShort { declared })?;
    if version != VERSION {
        return Err(DecodeError::UnsupportedVersion { found: version });
    }
    let kind = Kind::from_wire(kind).ok_or(DecodeError::UnknownKind { found: kind })?;
    let message = match kind {
        Kind::Verdict => Message::Verdict(Verdict::read(&mut body)?),
        Kind::Challenge => Message::Challenge(Challenge::read(&mut body)?),
    };
    match body.remaining() {
        0 => Ok((message, frame_len)),
        extra => Err(DecodeError::PayloadTooLong { kind, extra }),
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// REQ-PROTO-009: classify all four-byte prefixes before reading a body.
    #[kani::proof]
    #[kani::unwind(2)]
    fn req_proto_009_prefix_bounds() {
        let prefix: [u8; PREFIX_LEN] = kani::any();
        let declared = u32::from_le_bytes(prefix);
        // The v1 frame limit is 4096 bytes including the four-byte prefix.
        let expected = match declared {
            0 | 1 => Err(DecodeError::FrameTooShort { declared }),
            2..=4092 => Ok(declared as usize + 4),
            _ => Err(DecodeError::FrameTooLong { declared }),
        };
        assert_eq!(frame_len(prefix), expected);
        let error = match expected {
            Ok(needed) => DecodeError::Truncated {
                needed,
                available: PREFIX_LEN,
            },
            Err(error) => error,
        };
        assert_eq!(decode(&prefix), Err(error));
    }

    /// REQ-PROTO-008: all inputs up to the 38-byte golden verdict. Both
    /// message kinds and both accepted and refused sessions are reachable.
    #[kani::proof]
    #[kani::unwind(6)]
    fn req_proto_008_short_frames_are_canonical() {
        const CAP: usize = 38;
        let input: [u8; CAP] = kani::any();
        let end = usize::from(kani::any::<u8>());
        kani::assume(end <= CAP);
        let decoded = decode(&input[..end]);
        // The v1 binding has 16 instance bytes, 8 epoch bytes and 4 attempt
        // bytes. A verdict also carries an outcome before the session length.
        let len_at = match input[5] {
            1 if matches!(input[34], 1 | 2) => Some(35),
            2 => Some(34),
            _ => None,
        };
        if let Some(at) = len_at
            && input[4] == 1
            && end == at + 1 + usize::from(input[at])
            && input[..4] == ((end - 4) as u32).to_le_bytes()
        {
            let id = &input[at + 1..end];
            let error = if id.is_empty() {
                Some(SessionIdError::Empty)
            } else {
                id.iter()
                    .find(|byte| !byte.is_ascii_alphanumeric())
                    .map(|&found| SessionIdError::InvalidByte { found })
            };
            if let Some(error) = error {
                assert_eq!(decoded, Err(DecodeError::InvalidSession(error)));
                kani::cover!(
                    input[5] == 1 && !id.is_empty(),
                    "invalid verdict identifier"
                );
                kani::cover!(
                    input[5] == 2 && !id.is_empty(),
                    "invalid challenge identifier"
                );
            }
        }
        if let Ok((message, used)) = decoded {
            assert!(used <= end);
            let mut output = [0; CAP];
            assert_eq!(message.encode(&mut output), Ok(used));
            let index = usize::from(kani::any::<u8>());
            if index < used {
                assert_eq!(output[index], input[index]);
            }
            kani::cover!(matches!(message, Message::Verdict(_)), "accepted verdict");
            kani::cover!(
                matches!(message, Message::Challenge(_)),
                "accepted challenge"
            );
        }
    }
}
