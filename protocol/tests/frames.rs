//! Frame contract of the protocol crate, checked through its public API.
//!
//! Property tests run from a fixed seed so that the merge gate gives the same
//! answer on every run. Exploration beyond that seed is the fuzzer's job
//! (`fuzz/fuzz_targets/decode.rs`). `PROPTEST_RNG_SEED` and `PROPTEST_CASES`
//! override the seed and the case count for a local hunt.

#![expect(
    clippy::unwrap_used,
    reason = "clippy.toml exempts test functions only; the helpers here are test code too"
)]

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use skorupa_protocol::{
    Attempt, CompositorInstance, DecodeError, EncodeError, HEADER_LEN, Kind, LockEpoch,
    MAX_FRAME_LEN, Message, Outcome, PREFIX_LEN, SessionId, SessionIdError, VERSION, Verdict,
    decode, frame_len,
};

/// The frame that pins the wire layout. The fuzzer starts from the same bytes.
const GOLDEN: &[u8] = include_bytes!("../../fuzz/seeds/decode/verdict-granted");

/// Payload bytes of a verdict before the session identifier.
const VERDICT_FIXED_LEN: usize = 30;

fn golden_message() -> Message {
    Message::Verdict(Verdict {
        compositor_instance: CompositorInstance([
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        ]),
        session: SessionId::new(b"c2").unwrap(),
        epoch: LockEpoch(0x0102_0304_0506_0708),
        attempt: Attempt(0x0a0b_0c0d),
        outcome: Outcome::Granted,
    })
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 2048,
        rng_seed: RngSeed::Fixed(0x534b_4f52_5550_4131),
        ..ProptestConfig::default()
    }
}

fn session() -> impl Strategy<Value = SessionId> {
    let byte = prop_oneof![b'0'..=b'9', b'a'..=b'z', b'A'..=b'Z'];
    vec(byte, 1..=SessionId::MAX_LEN).prop_map(|id| SessionId::new(&id).unwrap())
}

fn message() -> impl Strategy<Value = Message> {
    let outcome = prop_oneof![Just(Outcome::Granted), Just(Outcome::Denied)];
    (
        any::<[u8; 16]>(),
        session(),
        any::<u64>(),
        any::<u32>(),
        outcome,
    )
        .prop_map(|(instance, session, epoch, attempt, outcome)| {
            Message::Verdict(Verdict {
                compositor_instance: CompositorInstance(instance),
                session,
                epoch: LockEpoch(epoch),
                attempt: Attempt(attempt),
                outcome,
            })
        })
}

fn encoded(message: &Message) -> Vec<u8> {
    let mut buffer = [0; MAX_FRAME_LEN];
    let written = message.encode(&mut buffer).unwrap();
    buffer[..written].to_vec()
}

/// Builds a frame around `payload` with a correct length prefix, so that a
/// test can break exactly one other thing.
fn frame(version: u8, kind: u8, payload: &[u8]) -> Vec<u8> {
    let body = u32::try_from(payload.len() + 2).unwrap();
    let mut bytes = body.to_le_bytes().to_vec();
    bytes.extend([version, kind]);
    bytes.extend(payload);
    bytes
}

/// The golden frame's payload, for tests that change one field of it.
fn golden_payload() -> Vec<u8> {
    GOLDEN[HEADER_LEN..].to_vec()
}

/// A decoder result is canonical when the accepted bytes are exactly what the
/// encoder writes for the decoded message.
fn assert_canonical(bytes: &[u8]) -> Result<(), TestCaseError> {
    if let Ok((message, used)) = decode(bytes) {
        prop_assert!(used <= bytes.len());
        prop_assert!(used <= MAX_FRAME_LEN);
        prop_assert_eq!(encoded(&message), &bytes[..used]);
    }
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    /// REQ-PROTO-007: decoding an encoded message returns that message and
    /// the length of its frame, and leaves following bytes alone.
    #[test]
    fn req_proto_007_round_trip(message in message(), tail in vec(any::<u8>(), 0..16)) {
        let mut bytes = encoded(&message);
        let len = bytes.len();
        prop_assert_eq!(message.encoded_len(), len);
        prop_assert_eq!(frame_len(bytes[..PREFIX_LEN].try_into().unwrap()), Ok(len));
        bytes.extend(tail);
        prop_assert_eq!(decode(&bytes), Ok((message, len)));
    }

    /// REQ-PROTO-008: the decoder returns a value for any input. A panic
    /// fails this test.
    #[test]
    fn req_proto_008_arbitrary_bytes_never_panic(bytes in vec(any::<u8>(), 0..128)) {
        assert_canonical(&bytes)?;
    }

    /// REQ-PROTO-008: the same for inputs one byte away from a valid frame,
    /// which reach every field check.
    #[test]
    fn req_proto_008_damaged_frames_never_panic(
        message in message(),
        index in any::<prop::sample::Index>(),
        value in any::<u8>(),
    ) {
        let mut bytes = encoded(&message);
        let index = index.index(bytes.len());
        bytes[index] = value;
        assert_canonical(&bytes)?;
    }

    /// REQ-PROTO-009: a frame is judged by its length prefix alone. A length
    /// above the limit is refused before any body byte is present.
    #[test]
    fn req_proto_009_oversized_length_refused_from_prefix(
        declared in (MAX_FRAME_LEN - PREFIX_LEN + 1)..=(u32::MAX as usize),
        body in vec(any::<u8>(), 0..8),
    ) {
        let declared = u32::try_from(declared).unwrap();
        let error = Err(DecodeError::FrameTooLong { declared });
        prop_assert_eq!(frame_len(declared.to_le_bytes()), error);
        let mut bytes = declared.to_le_bytes().to_vec();
        bytes.extend(body);
        prop_assert_eq!(decode(&bytes), error.map(|len| (golden_message(), len)));
    }

    /// REQ-PROTO-010: a frame of any version but the current one is refused,
    /// whatever else it holds.
    #[test]
    fn req_proto_010_other_versions_refused(message in message(), version in any::<u8>()) {
        prop_assume!(version != VERSION);
        let mut bytes = encoded(&message);
        bytes[PREFIX_LEN] = version;
        prop_assert_eq!(decode(&bytes), Err(DecodeError::UnsupportedVersion { found: version }));
    }

    /// REQ-PROTO-011: part of a frame is never a message. The error says how
    /// many bytes the reader needs: the prefix first, then the whole frame.
    #[test]
    fn req_proto_011_partial_frame_is_truncated(
        message in message(),
        cut in any::<prop::sample::Index>(),
    ) {
        let bytes = encoded(&message);
        let available = cut.index(bytes.len());
        let needed = if available < PREFIX_LEN { PREFIX_LEN } else { bytes.len() };
        prop_assert_eq!(
            decode(&bytes[..available]),
            Err(DecodeError::Truncated { needed, available })
        );
    }

    /// REQ-PROTO-014: encoding into a buffer that is too small fails and
    /// reports both sizes.
    #[test]
    fn req_proto_014_small_buffer_refused(
        message in message(),
        size in any::<prop::sample::Index>(),
    ) {
        let needed = message.encoded_len();
        let available = size.index(needed);
        let mut buffer = vec![0; available];
        prop_assert_eq!(
            message.encode(&mut buffer),
            Err(EncodeError::BufferTooSmall { needed, available })
        );
    }

    /// REQ-PROTO-013: a session identifier with any byte outside ASCII
    /// letters and digits is refused, when built and when decoded.
    #[test]
    fn req_proto_013_session_bytes_checked(
        session in session(),
        index in any::<prop::sample::Index>(),
        found in any::<u8>(),
    ) {
        prop_assume!(!found.is_ascii_alphanumeric());
        let mut id = session.as_bytes().to_vec();
        let index = index.index(id.len());
        id[index] = found;
        let error = SessionIdError::InvalidByte { found };
        prop_assert_eq!(SessionId::new(&id), Err(error));

        let mut payload = golden_payload();
        payload.truncate(VERDICT_FIXED_LEN);
        payload[VERDICT_FIXED_LEN - 1] = u8::try_from(id.len()).unwrap();
        payload.extend(id);
        prop_assert_eq!(
            decode(&frame(VERSION, 1, &payload)),
            Err(DecodeError::InvalidSession(error))
        );
    }
}

/// REQ-PROTO-012: the wire layout of a verdict is fixed byte for byte.
#[test]
fn req_proto_012_verdict_layout_is_pinned() {
    let expected: &[u8] = &[
        0x22, 0x00, 0x00, 0x00, // body length: 34
        0x01, // version
        0x01, // kind: verdict
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, // compositor instance
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, //
        0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // epoch
        0x0d, 0x0c, 0x0b, 0x0a, // attempt
        0x01, // outcome: granted
        0x02, // session identifier length
        b'c', b'2', // session identifier
    ];
    assert_eq!(GOLDEN, expected);
    assert_eq!(encoded(&golden_message()), expected);
    assert_eq!(decode(expected), Ok((golden_message(), expected.len())));

    let mut denied = expected.to_vec();
    denied[HEADER_LEN + 28] = 2;
    let Ok((Message::Verdict(verdict), _)) = decode(&denied) else {
        panic!("a verdict with outcome 2 decodes");
    };
    assert_eq!(verdict.outcome, Outcome::Denied);
    assert_eq!(verdict.session.as_str(), "c2");
    assert_eq!(format!("{:?}", verdict.session), "SessionId(\"c2\")");
}

/// REQ-PROTO-009: the length bounds sit exactly at the documented limits.
#[test]
fn req_proto_009_length_bounds_are_exact() {
    let largest = u32::try_from(MAX_FRAME_LEN - PREFIX_LEN).unwrap();
    assert_eq!(frame_len(largest.to_le_bytes()), Ok(MAX_FRAME_LEN));
    assert_eq!(
        frame_len((largest + 1).to_le_bytes()),
        Err(DecodeError::FrameTooLong {
            declared: largest + 1
        })
    );
    assert_eq!(frame_len(2_u32.to_le_bytes()), Ok(HEADER_LEN));
    for declared in [0, 1] {
        let error = Err(DecodeError::FrameTooShort { declared });
        assert_eq!(frame_len(declared.to_le_bytes()), error);
        assert_eq!(
            decode(&declared.to_le_bytes()),
            error.map(|len| (golden_message(), len))
        );
    }
}

/// REQ-PROTO-010: each of the 255 other version bytes is refused, zero
/// included.
#[test]
fn req_proto_010_every_other_version_refused() {
    for version in (0..=u8::MAX).filter(|&version| version != VERSION) {
        assert_eq!(
            decode(&frame(version, 1, &golden_payload())),
            Err(DecodeError::UnsupportedVersion { found: version })
        );
    }
}

/// REQ-PROTO-013: every field value outside the protocol is refused with its
/// own error, and the largest valid frame still decodes.
#[test]
fn req_proto_013_invalid_fields_refused() {
    let payload = golden_payload();

    for kind in (0..=u8::MAX).filter(|&kind| kind != 1) {
        assert_eq!(
            decode(&frame(VERSION, kind, &payload)),
            Err(DecodeError::UnknownKind { found: kind })
        );
    }

    for outcome in (0..=u8::MAX).filter(|outcome| !(1..=2).contains(outcome)) {
        let mut payload = payload.clone();
        payload[28] = outcome;
        assert_eq!(
            decode(&frame(VERSION, 1, &payload)),
            Err(DecodeError::UnknownOutcome { found: outcome })
        );
    }

    let kind = Kind::Verdict;
    for len in 0..payload.len() {
        assert_eq!(
            decode(&frame(VERSION, 1, &payload[..len])),
            Err(DecodeError::PayloadTooShort { kind }),
            "payload cut to {len} bytes"
        );
    }
    let mut long = payload.clone();
    long.extend([0, 0, 0]);
    assert_eq!(
        decode(&frame(VERSION, 1, &long)),
        Err(DecodeError::PayloadTooLong { kind, extra: 3 })
    );

    let with_session = |len: u8, id: &[u8]| {
        let mut payload = payload[..VERDICT_FIXED_LEN].to_vec();
        payload[VERDICT_FIXED_LEN - 1] = len;
        payload.extend(id);
        decode(&frame(VERSION, 1, &payload))
    };
    assert_eq!(
        with_session(0, b""),
        Err(DecodeError::InvalidSession(SessionIdError::Empty))
    );
    // The declared length is refused before the identifier is read, so the
    // answer is the same whether or not the bytes are there.
    for id in [&[b'a'; 33][..], b""] {
        assert_eq!(
            with_session(33, id),
            Err(DecodeError::InvalidSession(SessionIdError::TooLong {
                len: 33
            }))
        );
    }
    let longest = [b'z'; SessionId::MAX_LEN];
    let Ok((Message::Verdict(verdict), used)) = with_session(32, &longest) else {
        panic!("a 32-byte session identifier decodes");
    };
    assert_eq!(verdict.session.as_bytes(), longest);
    assert_eq!(used, HEADER_LEN + VERDICT_FIXED_LEN + SessionId::MAX_LEN);

    assert_eq!(SessionId::new(b""), Err(SessionIdError::Empty));
    assert_eq!(
        SessionId::new(&[b'a'; 33]),
        Err(SessionIdError::TooLong { len: 33 })
    );
}

/// REQ-PROTO-013: every error names what was wrong, so a log line is enough
/// to tell the cases apart. Decisions are still made on the variant.
#[test]
fn req_proto_013_errors_describe_themselves() {
    let kind = Kind::Verdict;
    let session = SessionIdError::InvalidByte { found: b'/' };
    let decode_errors = [
        DecodeError::Truncated {
            needed: 4,
            available: 1,
        },
        DecodeError::FrameTooShort { declared: 1 },
        DecodeError::FrameTooLong { declared: 5000 },
        DecodeError::UnsupportedVersion { found: 9 },
        DecodeError::UnknownKind { found: 7 },
        DecodeError::PayloadTooShort { kind },
        DecodeError::PayloadTooLong { kind, extra: 3 },
        DecodeError::UnknownOutcome { found: 0 },
        DecodeError::InvalidSession(session),
    ];
    let mut texts: Vec<String> = decode_errors.iter().map(ToString::to_string).collect();
    texts.push(SessionIdError::Empty.to_string());
    texts.push(SessionIdError::TooLong { len: 33 }.to_string());
    texts.push(
        EncodeError::BufferTooSmall {
            needed: 38,
            available: 0,
        }
        .to_string(),
    );
    assert!(texts.iter().all(|text| !text.is_empty()));
    let mut unique = texts.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), texts.len());

    let invalid = DecodeError::InvalidSession(session);
    let source = std::error::Error::source(&invalid);
    assert_eq!(source.map(ToString::to_string), Some(session.to_string()));
    assert!(std::error::Error::source(&DecodeError::UnknownKind { found: 7 }).is_none());
}
