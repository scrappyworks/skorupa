//! Fuzz target for the frame decoder (REQ-PROTO-008).
//!
//! The decoder must return for every input. When it accepts a frame, the
//! frame must be canonical: encoding the decoded message gives the same bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use skorupa_protocol::{MAX_FRAME_LEN, decode};

fuzz_target!(|data: &[u8]| {
    let Ok((message, used)) = decode(data) else {
        return;
    };
    assert!(used <= data.len(), "the frame lies within the input");
    assert!(used <= MAX_FRAME_LEN, "the frame is within the size limit");

    let mut buffer = [0; MAX_FRAME_LEN];
    let written = message
        .encode(&mut buffer)
        .expect("a decoded message fits the largest frame");
    assert_eq!(&buffer[..written], &data[..used], "the frame is canonical");
});
