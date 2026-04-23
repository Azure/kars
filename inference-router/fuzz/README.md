# Inference-router fuzz targets (s4)

Fuzz targets for attacker-controlled parsers in the router. Targets are
deliberately excluded from the workspace (nightly-only toolchain) and must
be run explicitly.

## Prereqs

```bash
cargo install cargo-fuzz            # one-time
rustup toolchain install nightly    # if you don't have it
```

## Targets

| Target                        | Function under test                                  | Why it matters |
|-------------------------------|------------------------------------------------------|----------------|
| `fuzz_deserialize_state`      | `handoff::deserialize_state(&[u8])`                  | Cross-pod attacker-supplied gzip+JSON; any panic = pre-auth DoS. |
| `fuzz_sanitize_chat`          | `handoff::sanitize_chat_snapshot(&[u8])`             | Returns `Vec<u8>` (no Result); any panic is a hard bug. Runs on every handoff. |
| `fuzz_parse_streaming_pf`     | `safety::parse_streaming_prompt_filter(&str)`        | Runs on every Azure-OpenAI SSE chunk. Stringly-typed → classic fuzz target. |

## Run

```bash
cd inference-router
cargo +nightly fuzz run fuzz_deserialize_state   -- -max_total_time=60
cargo +nightly fuzz run fuzz_sanitize_chat       -- -max_total_time=60
cargo +nightly fuzz run fuzz_parse_streaming_pf  -- -max_total_time=60
```

`-max_total_time` is in seconds. For CI, 60s per target is a reasonable
smoke run; for release gating, run overnight (`-max_total_time=28800`).

## Corpus

Seed corpora live under `corpus/<target>/`. Record interesting crashes to
`artifacts/<target>/` and add them as static regression tests in
`inference-router/src/*.rs` proptest blocks once minimized.

## Contract

Every target must be **total**: no panic, no abort, no unwinding on any
input. That's the only invariant the fuzz harness checks. Richer invariants
(roundtrip, idempotence, size bounds) live in proptest blocks (s5).
