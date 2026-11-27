//! The unlock verdict: the one message that may end a lock.

use core::fmt;

use crate::frame::{DecodeError, Kind};
use crate::wire::{Reader, Writer};

/// Identity of one compositor process, chosen at random when it starts.
///
/// A verdict made for a compositor that has since restarted carries the old
/// value and is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompositorInstance(pub [u8; 16]);

/// Number of the lock the verdict answers. Each new lock gets the next epoch,
/// so a verdict for an earlier lock cannot end a later one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LockEpoch(pub u64);

/// Number of the authentication attempt within one epoch. A verdict is used
/// once: it answers this attempt and no other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Attempt(pub u32);

/// Identifier of the login session: 1 to [`SessionId::MAX_LEN`] ASCII letters
/// and digits, which is the form of the identifiers logind hands out, such as
/// `3` or `c2`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId {
    len: u8,
    /// Bytes past `len` are zero, so derived equality compares identifiers only.
    bytes: [u8; Self::MAX_LEN],
}

impl SessionId {
    /// Longest accepted identifier in bytes.
    pub const MAX_LEN: usize = 32;

    /// Checks `id` and stores it.
    ///
    /// # Errors
    ///
    /// Returns which rule `id` breaks: it is empty, longer than
    /// [`Self::MAX_LEN`], or contains a byte other than an ASCII letter or
    /// digit.
    pub fn new(id: &[u8]) -> Result<Self, SessionIdError> {
        let too_long = SessionIdError::TooLong { len: id.len() };
        if id.is_empty() {
            return Err(SessionIdError::Empty);
        }
        let mut bytes = [0; Self::MAX_LEN];
        bytes
            .get_mut(..id.len())
            .ok_or(too_long)?
            .copy_from_slice(id);
        if let Some(&found) = id.iter().find(|byte| !byte.is_ascii_alphanumeric()) {
            return Err(SessionIdError::InvalidByte { found });
        }
        let len = u8::try_from(id.len()).map_err(|_| too_long)?;
        Ok(Self { len, bytes })
    }

    /// The identifier as bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // `new` keeps `len` within the array, so the fallback is never taken.
        self.bytes.get(..usize::from(self.len)).unwrap_or_default()
    }

    /// The identifier as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // `new` admits ASCII only, so the fallback is never taken.
        core::str::from_utf8(self.as_bytes()).unwrap_or_default()
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionId").field(&self.as_str()).finish()
    }
}

/// Why bytes are not a session identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionIdError {
    /// The identifier has no bytes.
    Empty,
    /// The identifier is longer than [`SessionId::MAX_LEN`].
    TooLong {
        /// Length that was offered.
        len: usize,
    },
    /// The identifier contains a byte other than an ASCII letter or digit.
    InvalidByte {
        /// First offending byte.
        found: u8,
    },
}

impl fmt::Display for SessionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "session identifier is empty"),
            Self::TooLong { len } => write!(
                f,
                "session identifier has {len} bytes, the limit is {}",
                SessionId::MAX_LEN
            ),
            Self::InvalidByte { found } => write!(
                f,
                "session identifier contains byte {found:#04x}, only ASCII letters and digits are allowed"
            ),
        }
    }
}

impl core::error::Error for SessionIdError {}

/// What the authentication broker decided.
///
/// Only [`Outcome::Granted`] ends a lock. A denial, a frame that does not
/// decode and a closed connection all leave the session locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// The user authenticated. Wire value 1.
    Granted,
    /// The attempt failed. Wire value 2.
    Denied,
}

impl Outcome {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Granted => 1,
            Self::Denied => 2,
        }
    }

    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Granted),
            2 => Some(Self::Denied),
            _ => None,
        }
    }
}

/// The authentication broker's answer to one unlock attempt.
///
/// The compositor acts on a verdict only when all four bindings match its own
/// state: the same compositor process, the same session, the current lock
/// epoch and the attempt it is waiting for.
///
/// Payload layout:
///
/// ```text
/// offset  size  field
/// 0       16    compositor instance
/// 16      8     epoch, little-endian
/// 24      4     attempt, little-endian
/// 28      1     outcome
/// 29      1     session identifier length n, 1 to 32
/// 30      n     session identifier
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Verdict {
    /// Compositor process the verdict is for.
    pub compositor_instance: CompositorInstance,
    /// Login session the verdict is for.
    pub session: SessionId,
    /// Lock the verdict answers.
    pub epoch: LockEpoch,
    /// Attempt the verdict answers.
    pub attempt: Attempt,
    /// The decision.
    pub outcome: Outcome,
}

impl Verdict {
    /// Payload bytes before the session identifier.
    const FIXED_LEN: usize = 16 + 8 + 4 + 1 + 1;

    /// Largest payload, used to prove at compile time that it fits a frame.
    pub(crate) const MAX_PAYLOAD_LEN: usize = Self::FIXED_LEN + SessionId::MAX_LEN;

    pub(crate) fn payload_len(&self) -> usize {
        Self::FIXED_LEN.saturating_add(self.session.as_bytes().len())
    }

    pub(crate) fn write(&self, writer: &mut Writer<'_>) -> Option<()> {
        writer.put(&self.compositor_instance.0)?;
        writer.put(&self.epoch.0.to_le_bytes())?;
        writer.put(&self.attempt.0.to_le_bytes())?;
        writer.put(&[self.outcome.to_wire(), self.session.len])?;
        writer.put(self.session.as_bytes())
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let too_short = DecodeError::PayloadTooShort {
            kind: Kind::Verdict,
        };
        let compositor_instance = CompositorInstance(reader.array().ok_or(too_short)?);
        let epoch = LockEpoch(reader.u64().ok_or(too_short)?);
        let attempt = Attempt(reader.u32().ok_or(too_short)?);
        let outcome = reader.u8().ok_or(too_short)?;
        let outcome =
            Outcome::from_wire(outcome).ok_or(DecodeError::UnknownOutcome { found: outcome })?;
        let session_len = usize::from(reader.u8().ok_or(too_short)?);
        // The length is judged before any byte of the identifier is read.
        if session_len > SessionId::MAX_LEN {
            return Err(DecodeError::InvalidSession(SessionIdError::TooLong {
                len: session_len,
            }));
        }
        let session = reader.bytes(session_len).ok_or(too_short)?;
        let session = SessionId::new(session).map_err(DecodeError::InvalidSession)?;
        Ok(Self {
            compositor_instance,
            session,
            epoch,
            attempt,
            outcome,
        })
    }
}
