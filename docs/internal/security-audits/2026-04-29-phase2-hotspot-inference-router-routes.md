# Phase 2 / S15.c — `inference-router/src/routes/inference.rs` hotspot decomposition

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-inference-router-routes`
**Sub-slice of:** S15 `phase2-hotspot-pass3` (§4.2 file-budget enforcement)

## Summary

Per `docs/implementation-plan.md` §4.2, `inference-router/src/routes/inference.rs` carried a Phase 2 cap of **800 LOC**. Pre-slice: **1359 LOC**. Post-slice: **776 LOC** (under cap).

The decomposition extracts the largest single handler — the 582-line `chat_completions` body for `POST /v1/chat/completions` — into its own sibling module `routes/chat_completions.rs`. `inference.rs` retains the route-builder fns plus the smaller handlers (`completions`, `responses`, `embeddings`, `images_generations*`, `list_models`, `list_deployments`, `foundry_proxy`).

## Existing implementation surveyed

(§0.2 #8 anti-duplication discipline.)

- `inference-router/src/routes/inference.rs` (1359 LOC) — the only file containing inference-route handlers; no parallel implementation elsewhere.
- `inference-router/src/routes/mod.rs` — module registry; pattern of `pub(crate) mod xyz` + `pub use xyz::yyy` already established for siblings (`audit_events`, `inference_policy`, `inference_translate`, `signing_ops`, `spawn_policy`, `chat_completions` is added under the same convention).
- `super::inference_translate::{chat_to_responses_body, responses_to_chat_body}` — the only consumer was `chat_completions`; after extraction these imports go with it.
- `crate::safety` and `futures::stream::StreamExt` — only used by `chat_completions` (streaming SSE path); imports go with it.
- `crate::errors`, `crate::proxy`, `super::AppState` — used by both halves; remain in both files.

No new abstraction layer was introduced — the new module is a peer of the existing `inference_translate` / `inference_policy` siblings, registered the same way.

## Decomposition

| File | Pre | Post | Note |
|---|---|---|---|
| `inference-router/src/routes/inference.rs` | 1359 | **776** | retains route-builders + smaller handlers |
| `inference-router/src/routes/chat_completions.rs` | (new) | 604 | `pub(super) async fn chat_completions(...)` body |
| `inference-router/src/routes/mod.rs` | — | +2 lines | `mod chat_completions;` (private to `routes`) |

`inference_routes()` continues to register the `/v1/chat/completions` route via `post(chat_completions)` — `chat_completions` is now imported from the new sibling module via `use super::chat_completions::chat_completions;`.

## Verification

| Gate | Result |
|---|---|
| `inference-router/src/routes/inference.rs` LOC | 1359 → **776** (under §4.2 cap of 800) |
| `cargo build --package azureclaw-inference-router` | clean |
| `cargo clippy --package azureclaw-inference-router --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo test --package azureclaw-inference-router --lib` | **608 passed; 0 failed** |

## Behavior delta

**None.** The 582-line `chat_completions` body was moved verbatim — only its visibility changed from module-private (`async fn`) to crate-private (`pub(super) async fn`) so the parent module's `inference_routes()` builder can register it. No refactoring of the request/response logic, streaming SSE handling, content-safety integration, or token-budget accounting was performed.

External clients calling `POST /v1/chat/completions` see no change in request format, response format, headers, status codes, streaming behavior, or error semantics.

## Threat-model considerations

No new attack surface. The slice does not change:

- The `chat_completions` request validator chain (sandbox-name extraction, content-safety pre-check, deployment-name normalization).
- Streaming SSE proxy semantics.
- Token-budget enforcement, audit-trail emission, or AGT chain integration.
- Foundry upstream proxy authentication (IMDS / WI tokens).

Custom-crypto lint and no-stubs lint continue to pass — moved code is the same code.

## Sign-offs

- Core: ✅ — pure extraction; behavior preserved; LOC budget enforced; no new public surface.
- Security: ✅ — no change to request validation, content-safety integration, token-budget enforcement, or upstream auth path; threat model unchanged.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
