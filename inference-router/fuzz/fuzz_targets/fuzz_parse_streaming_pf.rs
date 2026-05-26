#![no_main]
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fuzz target: streaming Azure content-safety parser.
//!
//! `parse_streaming_prompt_filter` runs on every SSE chunk returned from
//! Azure OpenAI. Input is a `&str` — stringly-typed parsers are prime fuzz
//! targets (unicode edge cases, empty frames, spurious `data:` prefixes,
//! malformed JSON after the SSE framing).

use libfuzzer_sys::fuzz_target;
use kars_inference_router::safety;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = safety::parse_streaming_prompt_filter(s);
    }
});
