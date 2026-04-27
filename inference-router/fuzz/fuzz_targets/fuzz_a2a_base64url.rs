#![no_main]
//! Fuzz target: A2A 1.0.0 base64url decoder.
//!
//! `base64url_decode` parses the base64url segments of a JWS at
//! verification time. Attacker-controlled bytes; total function
//! required.
use libfuzzer_sys::fuzz_target;
use azureclaw_inference_router::a2a;

fuzz_target!(|data: &[u8]| {
    let s = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = a2a::base64url_decode(s);
});
