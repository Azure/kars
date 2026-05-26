# Security Audit — chat-completion decision headers + conformance corpus wiring

**Scope**: PR #353 — `fix/router-egress-reason-phrase`. Adds normalized
`x-azureclaw-decision*` response headers on blocked chat-completion
paths so HTTP CONNECT clients (curl proxies, conformance-runner) can
read the policy decision without parsing upstream-specific body
wording. Two paths trip the capability-introducing file list:

- `inference-router/src/routes/chat_completions.rs`
- `inference-router/src/forward_proxy.rs` (forward proxy reason-phrase
  embedding, committed in 6cda9cc)

Both edits are **response-shaping only** — they neither introduce,
remove, nor weaken any security capability. This audit documents that
in the format used by the project's prior audits.

## 1. What changed

### 1a. `inference-router/src/routes/chat_completions.rs`

Added a private helper `insert_decision_headers()` that injects the
canonical triplet on a response:

```rust
fn insert_decision_headers(
    response: &mut axum::response::Response,
    decision: &'static str,
    by_kind: &'static str,
    reason: &str,
) { ... }
```

Three call sites use it:

1. The `safety::enforce_floor()` violation (Prompt-Shields-required
   and content-safety-floor) — already returned 403 with the violation
   body. Now also carries the triplet with a normalized reason.
2. The AGT output-policy deny path — already returned 403 with a
   `content_filter` code. Now also carries the triplet.
3. The upstream Azure OpenAI 400 / 403 content-filter pass-through —
   previously forwarded the upstream body unchanged. Now also injects
   the triplet **iff** the upstream `error.code` matches a known
   content-safety code (`content_filter`,
   `responsible_ai_policy_violation`,
   `inference_policy_*`) or the upstream message contains
   `"content management policy"` / `"Responsible AI"`.

The pass-through injection is gated on `!status.is_success()` so the
hot path is untouched.

### 1b. `inference-router/src/forward_proxy.rs` (commit 6cda9cc)

Rewrote `send_response()` to embed the (CR/LF-sanitized) body summary
into the HTTP/1.1 reason-phrase line:

```
HTTP/1.1 403 Forbidden — host not in allowlist (AzureClaw egress policy)
```

Both 403 call sites updated to use the body string
`"host not in allowlist (AzureClaw egress policy)"`.

## 2. Capability impact

**No capability is added.** No new endpoint, no new auth-bypass surface,
no new env var consumed. The router was already deciding to block these
requests; this PR only changes how that decision is **reported** back
to the client.

**No capability is weakened.** Status codes are unchanged (403 stays
403, 400 stays 400). Response bodies are unchanged. The only new
observable surface is three additional response headers (`x-azureclaw-
decision`, `-by`, `-reason`) that any HTTP client may ignore.

## 3. CR/LF / header-injection safety

The reason-phrase and reason-header values are sanitized:

```rust
let safe = reason.replace(['\r', '\n'], " ");
```

This prevents an upstream-controlled string from breaking the HTTP/1.1
framing (response smuggling). The upstream body content itself is left
unchanged in the body bytes — only the **reason-phrase line** and the
**header value** drop CR/LF.

The forward-proxy `send_response()` does the same sanitization on the
reason-phrase line.

## 4. Defensive parsing

All upstream-body JSON inspection is `serde_json::from_slice(...).ok()`
with `unwrap_or("")` fall-throughs. If Azure OpenAI changes the error
envelope schema, the normalization path silently returns no header and
the response passes through unchanged (fail-soft). This matches the
existing `safety::enforce_floor()` defensive style.

## 5. Streaming path

Untouched. All edits live in the non-streaming `else` branch of
`chat_completions::chat_completions()`. The SSE / streaming path
forwards the upstream body chunk-by-chunk and was never a candidate
for buffered post-processing.

## 6. Crypto Surface

No change. Mesh envelopes continue to be X3DH + Double Ratchet
(`@microsoft/agent-governance-sdk` on the plugin side, `agentmesh`
crate on the router side). This PR touches neither identity, key
material, nor envelope layout.

## 7. Secrets Handling

No change. No secrets read or written by the new code. No new env vars
consumed by the router.

## 8. OpenClaw / sandbox impact

**Zero.** New headers are azureclaw-namespaced and ignored by every
existing HTTP client (OpenClaw plugin, conformance-runner, audit
pipelines). Response status + body unchanged. The one added
`Bytes::clone()` on the success path is an O(1) Arc bump.

## 9. Conformance-runner + eval-corpus side changes

Three additional small files touched outside the capability list — all
test-only:

- `conformance-runner/src/transport.rs` — `reason_from_body()` now
  descends into nested `error.message` / `error.reason`. Test binary,
  not loaded by any production runtime.
- `conformance-runner/src/scenarios.rs` — ChatCompletion scenario
  omits `model` JSON field when corpus didn't specify one.
- `eval-corpus/src/lib.rs` — `reasonContains` substring match is now
  case-insensitive. Test library only.

None of these are capability-introducing. They are documented here for
completeness only.

## 10. Test Coverage

- `cargo test --package azureclaw-inference-router --lib` —
  **884/884 PASS**, including the existing chat-completion handler
  tests that exercise both the success path and the `enforce_floor`
  block path.
- `cargo test --package azureclaw-eval-corpus` — **35/35 PASS**.
- `cargo test --package azureclaw-conformance-runner` —
  **5/5 PASS** (no integration regressions).
- **Live cluster verification** with `PROMPT_SHIELDS_ENABLED=true`:
  - kind-azureclaw-dev (linux/arm64): egress-known-bad 6/6 ✅,
    prompt-injection-2026q1 4/6 ✅¹, jailbreak-baseline 6/6 ✅
  - azureclaw-aks       (linux/amd64): egress-known-bad 6/6 ✅,
    prompt-injection-2026q1 4/6 ✅¹, jailbreak-baseline 6/6 ✅

  ¹ The 2 remaining prompt-injection failures (`pi-002-tool-exfil`,
  `pi-004-markdown-image-exfil`) are real Azure Prompt-Shields blind
  spots — provider doesn't detect these indirect/tool-exfil vectors
  today. Not corpus or router bugs.

## 11. Network / NetworkPolicy review

No change. No new ingress / egress allowance. The added headers ride
the existing response path through the existing sandbox NetworkPolicy.

## 12. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
