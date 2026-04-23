#![no_main]
//! Fuzz target: handoff state deserializer.
//!
//! `deserialize_state` takes attacker-controlled bytes (gzip-compressed JSON
//! of a `HandoffState`). The handoff flow sees these bytes from cross-pod
//! peers, so any panic here is a remote-pre-auth DoS.
//!
//! Target must be **total**: every Err is acceptable, every panic is a bug.

use libfuzzer_sys::fuzz_target;
use azureclaw_inference_router::handoff;

fuzz_target!(|data: &[u8]| {
    let _ = handoff::deserialize_state(data);
});
