# Security Audit, Multi-provider LLM upstreams + pluggable guardrail pipeline (PR #488)

Date: 2026-08-25
Scope: `inference-router/src/routes/` (`chat_completions.rs`, `anthropic_messages.rs`,
`inference.rs`, `mod.rs`), `inference-router/src/guardrails/`,
`inference-router/src/provider.rs`, `inference-router/src/proxy.rs`,
`controller/src/inference_policy*.rs`, `controller/src/crd_validations.rs`,
`controller/src/reconciler/`.
Gated paths: `inference-router/src/routes/*`, `controller/src/reconciler/*`.

## Summary

Adds policy-driven routing to non-Azure LLM providers (Anthropic native, Ollama
OpenAI-compat; Bedrock stubbed → 501) via a new `InferencePolicy.spec.provider`,
and an ordered, fail-closed guardrail pipeline (`spec.guardrails[]`; first backend
OpenAI Moderation) that scans request input and model output for buffered and SSE
streaming responses. Provider credentials are held router-side only and never
reach the agent container. This audit also covers the review-driven hardening:
the raw-path/dot-segment guard on the Foundry proxy, and six functional
fail-closed/accounting fixes.

## T1: New capability / attack surface? (YES)

- **New egress targets.** The router can now originate requests to Anthropic and
  Ollama endpoints. Targets are derived from router-side config
  (`ANTHROPIC_ENDPOINT`, `OLLAMA_ENDPOINT`), not from request or CR content, so
  the destinations are operator-controlled. Note the provider forward paths do
  NOT currently invoke the blocklist (`is_blocked`); the blocklist/egress guard
  applies to the Foundry proxy path, not these provider calls. `provider::resolve`
  fails closed (501 unimplemented / 503 unconfigured) rather than defaulting to an
  attacker-influenced target.
- **New outbound credential use.** Anthropic uses a router-held `x-api-key`
  (`ANTHROPIC_API_KEY`); Ollama is unauthenticated. Inbound agent-supplied
  `x-api-key` / `authorization` are stripped and replaced before forwarding
  (verified by `multi_provider_guardrails` tests). Keys are forwarded only to the
  router sidecar, never the agent container (`reconciler` skip-empty env block),
  and only endpoints (never keys) enter the config hash.
- **New moderation backend call.** The guardrail pipeline calls the configured
  OpenAI Moderation endpoint with a router-held key. A declared stage with no key
  fails pipeline construction (503 `guardrail_misconfigured`).
- **New public request surface reachable pre-auth.** The guardrail/provider
  enforcement runs on the unauthenticated public router. Enforcement scope is
  bounded and documented (see T2).

## T2: Security-control change? (YES, net strengthening, fail-closed)

- **Guardrail enforcement is fail-closed throughout.** Unbuildable pipeline →
  503; backend outage → 502; violation → 403; streaming uses hold-and-release so
  no model text reaches the client before a scan covers it, and a flagged scan
  cuts the stream.
- **Route-coverage guard (default-deny).** Routes that cannot run the pipeline
  refuse a policy that needs it rather than silently bypassing: `/v1/completions`,
  `/v1/responses`, `/v1/embeddings`, image generation, and the Foundry proxy
  inference families (`/agents*`, `/openai/responses*`, `/openai/conversations*`).
  The Foundry proxy guard is **default-deny**: everything is guarded except an
  explicit exempt list of canonical management/storage APIs.
- **Path-canonicalization guard (bypass fix).** `foundry_proxy` percent-decodes
  each path segment and rejects (400 `invalid_path`) any dot / empty /
  encoded-slash segment before classification or forwarding, closing a traversal
  bypass where `/openai/files/../responses` normalized upstream into a guarded
  inference route. Proven live (mock upstream received `/openai/responses`
  pre-fix; 400 post-fix) and pinned by `foundry_route_guard` tests including a
  `reqwest::Url` normalization premise test.
- **Malformed guardrail config fails closed.** A present-but-malformed
  `guardrails` block no longer degrades to "no guardrails"; it poisons the policy
  so every request refuses (previously a silent disable of both the scan and the
  route-gap guard).
- **Buffered non-JSON output now scanned.** Output enforcement was hoisted out of
  the JSON-parse block so a non-JSON/truncated upstream body is still scanned via
  the raw-text fallback rather than returned verbatim.
- **UTF-8 integrity of scanned text.** SSE moderation now reconstructs multi-byte
  code points split across chunk boundaries, so the moderator scans the same text
  the client receives (no `U+FFFD` divergence).
- **All choices scanned.** Streaming extraction reads every `choices[]` entry, not
  just `choices[0]`.
- **Credential handling unchanged elsewhere.** No change to Azure WI/IMDS/Copilot
  auth; hop-by-hop headers stripped on Anthropic pass-through relays.

## T3: Availability / fail-open risk? (NEUTRAL-to-INCREASED-strictness)

- Fail-closed choices can turn backend outages into request denials (moderation
  502, provider 503). This is intended and documented; a plain Azure policy with
  no guardrails is unaffected and uses every route exactly as before
  (back-compat tests: `absent_provider_and_guardrails_keep_backcompat_defaults`).
- Streaming Anthropic now records token usage, closing a budget-accounting gap
  (no new denial path; best-effort, recorded once at end-of-stream).
- SSE hold-and-release bounds retained scan context (`MAX_SCAN_CHARS`), so memory
  stays bounded on long streams (regression-pinned).
- No new unbounded allocation, no new panics on request paths (path decoder and
  UTF-8 carry are total; malformed input → 400 / lossy, never panic).

## Verification

- `cargo build --workspace`: clean.
- `cargo clippy --workspace --all-targets`: 0 warnings (includes the CI
  `-D warnings` lints, `result_large_err` boxed, `collapsible_else_if` collapsed).
- `cargo fmt --all --check`: clean.
- `cargo test`: full router + controller suites pass, including new tests:
  `foundry_route_guard` (traversal matrix), `multi_provider_guardrails`, guardrail
  UTF-8 split / multi-choice / malformed-config / fail-closed, Anthropic streaming
  usage.
- LOC gate: `guardrails.rs` decomposed into `guardrails/{mod,backend,stream,tests}.rs`
  (all < 800); `reconciler/mod.rs` reduced to < 3700 via `reconciler/pod_spec.rs`.
- Live reproduction of the traversal bypass and its fix recorded against a mock
  upstream.

## Verdict

Accept, net fail-closed strengthening of the inference plane; the new egress and
credential surface is router-side-only, bounded, and default-deny; the identified
bypass and functional gaps are fixed and regression-tested.

Signed-off-by: John Seong <sandole97@gmail.com>
