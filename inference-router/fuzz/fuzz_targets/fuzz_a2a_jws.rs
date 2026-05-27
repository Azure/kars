#![no_main]
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fuzz target: A2A 1.0.0 JWS detached-content signing input builder.
//!
//! `build_signing_input` parses an attacker-controlled protected
//! header (JSON) and concatenates with attacker-controlled payload
//! bytes. The function lives on the inbound AgentCard verification
//! path: it sees bytes from any caller that posts an AgentCard, so
//! a panic here is a remote pre-auth DoS at the gateway.
//!
//! The split is `[header_len: u16 LE][header bytes][payload bytes]`
//! so the fuzzer can vary both halves independently.
//!
//! Target must be **total**: every Err is acceptable, every panic
//! is a bug.
use libfuzzer_sys::fuzz_target;
use kars_inference_router::a2a;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let header_len = u16::from_le_bytes([data[0], data[1]]) as usize;
    if data.len() < 2 + header_len {
        return;
    }
    let header = &data[2..2 + header_len];
    let payload = &data[2 + header_len..];
    let _ = a2a::build_signing_input(header, payload);
});
