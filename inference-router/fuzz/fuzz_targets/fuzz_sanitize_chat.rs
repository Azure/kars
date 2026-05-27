#![no_main]
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fuzz target: chat-snapshot sanitizer.
//!
//! `sanitize_chat_snapshot` returns `Vec<u8>` (no Result) so *any* panic is a
//! hard bug. Input is opaque bytes from the handoff path — an adversarial
//! peer or a compromised sub-agent can put anything in here: non-UTF8, deeply
//! nested JSON, malformed arrays, SSE-looking text, huge strings, etc.

use libfuzzer_sys::fuzz_target;
use kars_inference_router::handoff;

fuzz_target!(|data: &[u8]| {
    let _ = handoff::sanitize_chat_snapshot(data);
});
