# Security Audit — Inference routing and local failover

Date: 2026-09-07
Review status: Implementation evidence prepared; independent review and sign-off pending.

## Scope

PR4, initially based on `8fe755b4`, extracts inference routing from canonical
reference `ce9044077` and integrates foundation hash centralization `ea6f5789`.
Gated paths include `controller/src/reconciler/`,
`controller/src/kars_task.rs`, `inference-router/src/routes/`, and the
corresponding Helm CRD and configuration templates.

No runtime orchestration, task delivery, access-request service, GitHub write
service, witness deployment, private cluster values, or publishing workflow is
introduced. No cluster changes, image publication, or pull-request merges were
performed.

## T1: New capability / attack surface? YES

- Operator-configured named provider endpoints and per-provider credentials.
- Ordered primary/fallback model routes, including pre-acceptance SSE failover.
- Optional local-inference NetworkPolicy destinations.
- Optional router-only mirroring of the controller namespace's
  `kars-inference-providers` Secret.

The agent container does not receive the new Secret. Policy routing determines
which configured provider is used. Fallback reconstruction starts from the true
default rather than inheriting another candidate's endpoint or credential.

## T2: Security-control change? YES, bounded and additive

- Existing Content Safety and prompt-shield defaults remain enabled.
- Existing input/output guardrail and policy-floor enforcement paths remain.
- Host matching uses parsed hostnames. Named routes carry explicit authentication
  provenance and use their own credential or no auth; they never borrow the
  default API key, WI/IMDS, or sidecar token. Legacy default behavior is retained.
- Named Copilot routes exchange their own GitHub seat token in an isolated
  provider cache; only the legacy default uses the global Copilot account.
- Local egress is opt-in. Precise targets require namespace, pod labels, and
  valid TCP ports; they replace broad namespace allowances. Invalid targets
  fail reconciliation rather than falling back to broader access.
- New fallback bounds apply to the new field only. Existing team roster sizes,
  primary model fields, installation profiles, and explicit client model choice
  without a model preference remain compatible.
- Secret revisions change a pod-template annotation so the next reconciliation
  refreshes environment credentials. Source removal removes only a matching
  controller-mirrored copy.

## T3: Availability / fail-open risk? MIXED, explicitly bounded

- Connection failures known to precede acceptance and HTTP 429/5xx can use
  another configured provider. Typed authentication/configuration failures and
  unknown acceptance states fail closed.
- Unavailable-model recovery is bounded to the configured default and cached
  per provider identity/endpoint/model; Responses-only recovery remains with the actual selected
  provider and deployment.
- No buffered or streaming generation is replayed after accepted 2xx headers,
  including a body failure during chat-to-Responses recovery.
- Explicit policy providers retain primary-route precedence; true-default
  fallback remains separate. Missing local-inference Helm values stay disabled
  on `--reuse-values` upgrades.
- Named-provider Secrets remain optional for installations using only the
  existing default route. Mirror errors are propagated, not concealed.
- The existing public typed provider gates remain; no cross-family
  OpenAI-to-Anthropic translation is claimed.

## Verification

Closure follow-up after `c47418fc` distinguishes legacy primary-model metadata
from explicit native/named routing, independently of native credential presence.
It also permits retry after a known 429/5xx rejection whose body truncates,
without retrying accepted, ambiguous, authentication/configuration, or ordinary
4xx failures. New HTTP regressions cover metadata/default compatibility,
registered versus native intent, health isolation, buffered/streaming truncated
rejections, and chat-to-Responses recovery. These closure regressions await the
parent-controlled Cargo lease; the earlier qualification below covers
`c47418fc`, not this follow-up.

Six independent-review blockers were repaired after `7a2a5d11`. Added
regressions cover ambient API-key/sidecar isolation, the real Copilot-host
exchange branch, provider precedence in buffered and streaming chat, separate
accounts at the same endpoint/model, typed failures, and actual
chat-to-Responses recovery with truncated accepted bodies.

Repair qualification after merging immutable, parent-qualified `415b53ca`:

- 76 targeted router unit/HTTP tests passed, including both first-time and
  cached Responses recovery, provider credential isolation, and existing
  moderation, Foundry-route, egress, and explicit-client-model regressions.
- 70 targeted controller tests passed. New integration cases verify that
  `modelFallbacks` survives the shared effective-blueprint normalizer, changes
  task authorization when mutated or reordered, reaches the materialized
  InferencePolicy, and is inherited by principal/member/run specifications.
  Explicit role overrides remain complete overrides.
- `UnsupportedLaunchBudget` remains fail closed; finite total/subtree and
  monetary budgets are not restored as misleading daily-token mappings.
- Strict Clippy passed for both crates with all targets and warnings denied.
- Six offline Helm regressions passed using the existing CLI Vitest runner
  and cached Vitest 4.1.10. The fixture chart has no new defaults, reproducing
  absent legacy maps rather than letting normal Helm coalescing hide the defect.
- Formatting, LOC/crypto gates, Helm lint, test lint, and diff checks passed.

Rust validation used the exclusively leased existing root target, offline and
locked, with incremental compilation disabled. No duplicate target directory,
dependency installation, live deployment, or external push was performed.

Local validation at implementation commit `df0540c3`, before foundation
integration and independent review:

- Controller/router Rust suites: 2,119 tests passed; three existing doctests
  ignored.
- New HTTP regressions cover cross-provider credentials, identical endpoints
  with distinct keys, default recovery, streaming acceptance, ordinary auth
  failures, and preservation of explicit client model selection.
- Controller tests cover bounded fallback schema without roster restriction,
  ordered deduplication, stable label values, and precise egress behavior.
- Existing publication/CLI compatibility checks: 94 passed, two skipped.
  Used the authorized existing cache with Vitest 4.1.10, not lockfile-exact
  4.1.8. Locked dependency validation remains the responsibility of GitHub CI.
- Helm lint and default, local-dev, generic, and existing-AKS renders passed.
- Strict Clippy passed for all controller/router targets with warnings denied.

The foundation integration preserves the existing `sha256_hex` re-export used
by PR4's label helper. The repair qualification above supersedes the earlier
pre-integration build evidence for this delta. Review of PR4's
added documentation and Helm example/configuration lines found no private
hardware, registry, subscription, or resource identifiers.

## Verdict

Pending independent review. This document is not an approval. Required
author/reviewer sign-offs must be supplied by the publication review process
before the capability-audit gate can pass.
