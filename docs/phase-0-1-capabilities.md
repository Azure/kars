# Phase 0 + Phase 1 Capability Index

**Purpose:** authoritative, evidence-based index of every capability shipped on
the `dev` branch since the last `main` baseline. This is the documentation
manifest for **PR #44 (`dev → main` uplift)**.

Every row maps to:

- code path (verifiable by `view` / `grep`),
- security-audit doc under `docs/security-audits/`,
- conformance / compat / unit test coverage,
- the implementation-plan section that authorises it.

If a row in this index is missing one of those four columns, it does not ship
in PR #44. Period. CI gate `ci/security-audit-required.sh` enforces the audit
column for every capability-introducing PR.

> **Companion docs:**
> [`architecture.md`](architecture.md) · [`security.md`](security.md) ·
> [`threat-model.md`](threat-model.md) ·
> [`agt-vendored-patch-audit.md`](agt-vendored-patch-audit.md) ·
> [`security-mcp-top10.md`](security-mcp-top10.md) ·
> [`sigs-agent-sandbox-compat.md`](sigs-agent-sandbox-compat.md)

---

## Phase 0 — Foundation

Goal (per implementation-plan §6): the repo is ready to change without breaking.
Compat suite is the safety net; provider seams unlock AGT-backed alternates;
six CI gates enforce non-negotiables.

### CI gates (`ci/`)

| Gate | Enforces | Notes |
|---|---|---|
| `ci/check-loc.sh` | LOC budget (`ci/loc-budget.yaml`) — 800-line hard cap on new files; budgeted hotspots on monotonic-decrease | Inline override `// ci:loc-ok` allowed |
| `ci/no-stubs.sh` | No `TODO`/`FIXME`/`unimplemented!`/`todo!`/`panic!("not.impl")`/`.stub`/`.mock`/`placeholder` on production paths | Inline override `// ci:stub-ok: <reason>` |
| `ci/no-custom-crypto.sh` | Forbids hand-rolled Signal / X3DH / HMAC / AES / Ed25519 / KDF / nonce construction outside `providers/signing*.rs`, vendored AgentMesh, and the AGT SDK | No override |
| `ci/no-null-provider-prod.sh` | `Null*` provider only with `metadata.labels.azureclaw.azure.com/dev-only: "true"` | No override; mirrored by VAP at admission time |
| `ci/security-audit-required.sh` | Capability-introducing PRs must ship `docs/security-audits/<date>-<slug>.md` with **two** sign-offs | No override |
| `ci/vendored-patch-audit.sh` | On AGT SDK version bump, requires updated row in `docs/agt-vendored-patch-audit.md` confirming each patch is still needed (or upstream-absorbed) | No override |
| `ci/a2a-module-isolation.sh` | A2A scaffold cannot leak into non-opt-in router routes; enforces `forbid(unsafe_code)` in `inference-router/src/a2a/` | Phase 1 addition |

### Provider-seam scaffolds (`inference-router/src/providers/`, `controller/src/providers/`)

Four trait seams isolate governance, audit, signing, and mesh from the request
hot path so each can be swapped for an AGT-backed alternate without touching
call sites.

| Seam | File | Status |
|---|---|---|
| `MeshProvider` | `inference-router/src/providers/mesh.rs` | **Trait file is documentation only.** Mesh is plugin-side; router has no in-tree impl. |
| `PolicyDecisionProvider` | `providers/policy.rs` (trait) + `providers/policy_impl.rs` (in-tree impl on `Governance`) | Phase 1: in-tree impl wired into `inference.rs`, `mcp/`, `a2a/`, `spawn_policy.rs` |
| `AuditSink` | `providers/audit.rs` (trait) + `providers/audit_impl.rs` | Phase 1: in-tree impl + 13 handoff sites migrated |
| `SigningProvider` | `providers/signing.rs` (trait) + `providers/signing_impl.rs` | Phase 1: A2A AgentCard sign + AP2 mandate sign use this |
| `OutageMode` decision | `providers/outage.rs` | Strict / CachedRead / DegradedDev — pure function + controller validator |

### Admission policy (Helm-shipped)

| Resource | Purpose | Chart path |
|---|---|---|
| `null-provider` VAP | Reject `provider: null|noop|disabled` on non-dev tenants (mirrors `ci/no-null-provider-prod.sh`) | `deploy/helm/azureclaw/templates/policies/` |

### Compat / conformance harnesses

| Harness | Path | Specs |
|---|---|---|
| Compat suite | `tests/compat/` (vitest) | `operator-tui.spec.ts` — TUI render + scripted key sequence + outgoing-CR-payload assertions |
| Conformance corpus | `tests/conformance/` (vitest) | `signal-x3dh`, `signal-knock`, `signal-negative` (Phase 0); `oauth21-bcp`, `mcp-streamable-http`, `a2a-agent-card`, `ap2-commerce`, `sandbox-isolation` (Phase 1) |
| Fuzz | `inference-router/fuzz/fuzz_targets/` | 5 targets: `fuzz_a2a_jws`, `fuzz_a2a_base64url`, `fuzz_deserialize_state`, `fuzz_sanitize_chat`, `fuzz_parse_streaming_pf` |

### Foundation docs

`agt-vendored-patch-audit.md`, `security-reviewers.md`,
`sigs-agent-sandbox-compat.md`, `security-audits/_template.md` + dated audit
docs.

### Hotspot decomposition pass #1

| File | Before | After | Note |
|---|---|---|---|
| `inference-router/src/routes.rs` | 4890 | split → `routes/{mod,inference,handoff,governance,mesh,egress}.rs` | Byte-level equivalence proven by `tools/drift/drift.py` |
| `controller/src/reconciler.rs` | 2326 | 1464 (under Phase 1 1500 cap) | `reconciler/mod.rs` + `reconciler/tests.rs` |
| `controller/src/mesh_peer.rs` | 1970 | 1170 (under Phase 1 1200 cap) | `mesh_peer/{mod,offload,pair}.rs` |

### CLI

| Command | Status |
|---|---|
| `azureclaw convert` | **Skeleton** (Phase 0, exit-3) — translates between `Native` and `Sandbox` (sigs/agent-sandbox) shapes; full converter in Phase 2 |
| `cli/src/commands/operator/keymap.ts` | Extracted from `operator.ts`; isolated keymap + status-bar |

---

## Phase 1 — Protocol freshness

Goal (per implementation-plan §7): router speaks MCP 2026 + A2A 1.0.0 + AP2 +
OAuth 2.1; AGT Rust-SDK-backed providers production-parity (not replacement);
compat-mode `TranslateMode` available behind a flag; minimum CRD surface to
configure the new router capabilities.

### Provider migration (in-tree AGT-backed impls)

| What | Code | Audit doc |
|---|---|---|
| `PolicyDecisionProvider` impl on `Governance` | `providers/policy_impl.rs` | `2026-04-24-phase1-policy-provider-in-tree.md` |
| `AuditSink` impl on `Governance` | `providers/audit_impl.rs` | `2026-04-24-phase1-audit-sink-in-tree.md` |
| `SigningProvider` impl on `Governance` | `providers/signing_impl.rs` | `2026-04-24-phase1-signing-provider-in-tree.md` |
| Migrate `handoff.rs` audit calls (13 sites) → trait | `routes/handoff/`, `handoff/` | `2026-04-25-phase1-audit-sink-migrate-handoff.md` |
| Migrate `inference.rs` policy calls (3 sites) → trait | `routes/inference.rs` | `2026-04-25-phase1-policy-provider-migrate-inference.md` |
| `MeshProvider` clarified as plugin-side only | trait file doc-comment | `2026-04-25-phase1-mesh-seam-clarification.md` |

### MCP 2026 (`inference-router/src/mcp/`)

| Module | Purpose |
|---|---|
| `error.rs` | JSON-RPC + transport error mapping |
| `jsonrpc.rs` | JSON-RPC 2.0 framing |
| `streamable_http.rs` | MCP 2026 Streamable HTTP transport (replaces SSE) — `Mcp-Session-Id` semantics + batch JSON-RPC |
| `oauth.rs` | OAuth 2.1 access-token verifier (RFC 8725 BCP) — PKCE, audience, expiry, resource-indicator, scope |
| `oauth_layer.rs` | `tower::Layer` wiring OAuth 2.1 verification onto `protected_mcp_route` |
| `initialize.rs` | `initialize` JSON-RPC handler |
| `pipeline.rs` | End-to-end request pipeline |
| `tools.rs` | `tools/list` + `tools/call` dispatch |

Route binding: `POST /mcp` → `routes/mcp.rs` (sub-router state).

Conformance: `tests/conformance/specs/mcp-streamable-http.spec.ts`,
`oauth21-bcp.spec.ts`. Negative-only edge cases:
`2026-04-25-phase1-mcp-negative-edge-cases.md`.

CRD: `McpServer` (schema-only Phase 1; reconciler in Phase 2). CEL admission:
`productionMode: true ⇒ oauth.issuer set` (`controller/src/crd_validations.rs`).

OWASP MCP Top 10 controls matrix: `docs/security-mcp-top10.md`.

### A2A 1.0.0 (`inference-router/src/a2a/` — 14 modules)

A2A spec **finalised at v1.0.0** (not 1.2 as the implementation-plan
originally drafted). Scaffold mirrors v1.0.0 schema from
<https://a2a-protocol.org/v1.0.0/specification>; protobuf at
`spec/a2a.proto` is the normative source.

| Module | Purpose |
|---|---|
| `signature.rs` | Ed25519 detached JWS primitives (uses `SigningProvider`) |
| `agent_card.rs` | `AgentCard` schema + serialisation |
| `card_signing.rs` | Sign outbound AgentCards |
| `card_server.rs` | Build + serve `/.well-known/agent.json` |
| `card_verifier.rs` | Inbound AgentCard verification (signature + expiry + issuer) |
| `error.rs` | A2A error mapping |
| `jsonrpc_dispatch.rs` | A2A JSON-RPC method dispatch (`message/send`, `tasks/get`, `tasks/cancel`) |
| `agent_projection.rs` | `A2AAgent` CRD spec → `TrustAnchor` projection |
| `trust_store.rs` | Snapshot trust-anchor cache (`kid → VerifyingKey`) |
| `snapshot_rebuild.rs` | Trust-store rebuild orchestrator (hot-reload) |
| `mandate_signing.rs` | AP2 IntentMandate detached-JWS sign / verify |
| `mandate_trust_store.rs` | Type-safe AP2 mandate-issuer trust store wrapper |
| `ap2.rs` | AP2 commerce mandate validation kernel |
| `message_send_ap2.rs` | Wire AP2 verification into A2A `message/send` |

Route binding: `POST /a2a` + `/.well-known/agent.json` → `routes/a2a.rs`.

Ingress posture (ADR-0001): A2A is **gateway-only, surgical opt-in** — see
`docs/adr/0001-a2a-ingress-front-edge.md`. The router does not expose A2A on
the public ingress unless `ClawSandbox.spec.a2a.expose: true`. A "no public
router exposure" VAP + a Cilium L7 CCNP enforce this at admission and at the
network layer.

CI: `ci/a2a-module-isolation.sh` + `forbid(unsafe_code)` on `a2a/`.

CLI: `azureclaw a2a list-exposed`, `azureclaw a2a schema` (Phase 1 scaffold).

Conformance: `tests/conformance/specs/a2a-agent-card.spec.ts`,
`ap2-commerce.spec.ts` — 14 wire-format fixtures.

Fuzz: `fuzz_a2a_jws`, `fuzz_a2a_base64url`.

### Identity provider seam — Entra agent identity

| What | Code | Notes |
|---|---|---|
| Microsoft Graph client for Entra agent identity | `controller/src/...` (Graph client landed in `phase1/identity-provider-seam-entra-agent-id` at `2114bf2`) | `POST /beta/servicePrincipals/microsoft.graph.agentIdentity` + `POST /beta/servicePrincipals/{id}/federatedIdentityCredentials` + `DELETE /beta/servicePrincipals/{id}` |

Endpoints verified against learn.microsoft.com.

### CRDs

| CRD | Status | File | Reconciler? |
|---|---|---|---|
| `ClawSandbox` | v1alpha1 — full | `controller/src/crd.rs` | Yes (`reconciler/mod.rs`) |
| `ClawPairing` | v1alpha1 — full | `controller/src/pairing.rs` | Yes (`pairing_reconciler.rs`) |
| `McpServer` | **Phase 1 schema-only** | `controller/src/mcp_server.rs` | **No** — reconciler is a Phase 2 deliverable |
| `ToolPolicy` | **Phase 1 schema-only** | `controller/src/tool_policy.rs` | **No** — reconciler is a Phase 2 deliverable |

CEL `x-kubernetes-validations` on `McpServer` + `ToolPolicy`:
`controller/src/crd_validations.rs` (post-processes `<Type>::crd()` to inject
rules — kube-rs#1557 prevents derive-time emission).

### Status subresource — KEP-1623

`ClawSandbox.status` exposes `observedGeneration` + `Conditions[]`; the
controller stamps `Degraded=True` / `Ready=False` with `observedGeneration` on
the three validation-failure exits. Code: `controller/src/status/conditions.rs`.

### VAP / MAP set (`deploy/helm/azureclaw/templates/policies/`)

| Policy | Type | Effect |
|---|---|---|
| Pod exec ban | VAP | Deny `pods/exec\|attach\|portforward` on sandbox namespaces |
| Sandbox posture lock | VAP | Deny spec mutations that downgrade isolation, remove seccomp, or flip `readOnlyRootFilesystem: false` |
| Dev-only label removal lock | VAP | Once `azureclaw.azure.com/dev-only` is applied, it cannot be removed |
| Null-provider deny | VAP | Mirrors `ci/no-null-provider-prod.sh` at runtime |
| A2A "no public router exposure" | VAP | Deny exposing A2A on public ingress unless explicitly opted in |
| Router-sidecar inject | MAP | Auto-inject the router sidecar into pods labelled `azureclaw.azure.com/inject-router=true` |
| Seccomp auto-stamp | MAP | Auto-set seccomp to `azureclaw-strict` on sandbox pods that lack it |

**Kubernetes version & feature-gate caveat.** VAP is GA in Kubernetes
≥ 1.30 and ships unconditionally. CRD CEL validations are GA in ≥ 1.29.
**MAP is beta in Kubernetes ≥ 1.32** and requires the kube-apiserver
flags `--feature-gates=MutatingAdmissionPolicy=true` and
`--runtime-config=admissionregistration.k8s.io/v1beta1=true`. On AKS this
is currently only available on preview channels. The two MAP policies
(router-sidecar inject, seccomp auto-stamp) are therefore gated behind
the Helm flag `controller.mutatingAdmissionPolicy.enabled` (default
`false`); when disabled, the controller's reconciler performs the
equivalent injection/stamping deterministically before pod creation, so
the end-state is identical on stable AKS channels. MAP becomes the
default once it is GA on the AKS stable channel.

### Hot reload

| What | Code | Notes |
|---|---|---|
| `policy-envelope` hot-reload core (pure transition + container) | `inference-router/src/policy_envelope.rs` | Audit: `2026-04-25-phase1-policy-envelope-hot-reload.md` |
| A2A trust-store hot-reload integration | `a2a/snapshot_rebuild.rs` | Audit: `2026-04-25-phase1-trust-store-hot-reload-integration.md` |
| AP2 mandate-issuer trust store | `a2a/mandate_trust_store.rs` | Audit: `2026-04-25-phase1-ap2-mandate-trust-store.md` |

### Outage modes

`providers/outage.rs` — `Strict` (default prod, fail-closed), `CachedRead`
(allow if cached decision < TTL else fail-closed), `DegradedDev` (dev only,
fail-open with warning label). Per-`ClawSandbox` via
`spec.agt.outageMode`.

### Telemetry

OTel GenAI SemConv 1.x constants + typed attribute bag in
`inference-router/src/telemetry/`. Audit:
`2026-04-24-phase1-otel-genai-semconv.md`.

### Federated-credential reaper

`controller/src/fedcred_reaper.rs` — periodic GC of orphan federated identity
credentials. Three layers of defence:

1. Name prefix (`azureclaw-`).
2. Subject pattern.
3. System allowlist.

Tunable via `FEDCRED_REAPER_INTERVAL_SECS` (default 600s; `0` disables).
Five unit tests. Wired as the 4th `tokio::select!` arm in `controller/src/main.rs`.

### Hotspot decomposition pass #2

| File | Before | After | Cap met |
|---|---|---|---|
| `handoff/mod.rs` | 2075 | 1770 (after `crypto.rs` 121 + `auth.rs` 184 extractions) | Phase 1 1800 |
| `governance.rs` (router) | 1252 | 837 (after `rate_limiter.rs` + `behavior_monitor.rs` + `governance/trust_ops.rs` extractions) | Phase 1 900 |
| `routes/handoff/` payload handlers | 1570 | 872 | Phase 1 800 |
| `spawn/docker.rs` | 1199 | 762 | Phase 1 900 |
| `inference.rs` | extracted translate helpers | shrinking toward 1500 cap | In progress |

### CLI additions

- `azureclaw a2a list-exposed`, `azureclaw a2a schema` — Phase 1 scaffold for A2A surface inspection.
- `azureclaw convert` — Phase 0 skeleton (exit-3); full converter in Phase 2.
- Operator TUI: `commands/operator/keymap.ts` extracted.

### Compat-mode flag

`ClawSandbox.spec.upstreamCompatibility` — `Native | translate` (schema-only
in Phase 1; full `TranslateMode` reconciler emission in Phase 2; `OverlayMode`
in Phase 2 also). See `docs/sigs-agent-sandbox-compat.md`.

---

## Production fixes (top of dev, since the last main baseline)

These are the recent commits that are not phase-bucketed but ship in PR #44
because they're already on dev.

| Commit | What | Impact |
|---|---|---|
| `4762aee` | Periodic fedcred reaper for orphan GC (controller) | Closes a fedcred-quota leak previously seen at 22/20 cap |
| `14de655` | Retroactive audit doc for `image_generate` loopback unblock | Closes `ci/security-audit-required.sh` blockage |
| `31a80ee` | Extract `core/router-client.ts` from `plugin.ts` (225 LOC) | Phase 3 budget delta on `plugin.ts` |
| `97ef710` | Unblock built-in `image_generate` provider on the loopback router | Production sandbox image generation works |
| `c2d78de` | Gate heavy runtime side-effects on `registrationMode==full` | Prevents duplicate-render UI bug on plugin double-load |
| `299206c` | Plumb `OPENCLAW_GATEWAY_TOKEN` via `secretKeyRef` | Token no longer in plain env-var on pod spec |
| `85b99bf` | Surface `kubectl` stderr on port-forward failure in `azureclaw connect` | Better operator diagnosis when port already bound |
| `016b2df` | Sub-agent `mesh_send` / `discover` go through router-proxied registry | Removes direct registry call from sub-agents |
| `33bef5d` | Resend mesh task on reply timeout if peer AMID changed | Fixes stale-AMID race after sub-agent identity rotation |
| `9bee8e3` | Refresh stale AMID after sub-agent identity rotation | Pairs with `33bef5d` on the cache side |
| `b7ac128` | Break controller reconcile loop, register controller mesh peer, fix postgres deadlock | Controller stability under reconcile pressure |
| `bdb5fd8` | Controller mesh-peer exponential reconnect backoff + jitter | Mitigates 30-cycles-in-5-min reconnect storms in AKS |
| `cdb9728` | Pre-stage OpenClaw bundled-runtime-deps + sub-agent npm 403 fix | Sub-agents come up reliably |
| `4c3094a` | Mirror staged deps to writable tmpfs for RO-rootfs pods | RO-rootfs sub-agents can resolve `node_modules` |
| `164359b` | `AGT_SKIP_ENTRA` toggle + multi-arch Azure Linux base | Enables sandbox base on arm64 + skips Entra in dev |

---

## What is NOT shipped in PR #44

These are deliberately deferred per implementation-plan §0.2 #11
("Branching: dev only until end-of-plan uplift"):

- **Reconcilers for `McpServer` and `ToolPolicy`** — Phase 2.
- **`OverlayMode` for `sigs/agent-sandbox` compat** — Phase 2.
- **`InferencePolicy`, `A2AAgent`, `ClawEval`, `ClawMemory`, `ClawFleet`, `TrustGraph`, `ClawAgentIdentity`, `WasmTool`** — Phase 2/3/4.
- **AGT mesh provider (`AgtMeshProvider`)** — awaits AGT AgentMesh delivery; vendored mesh stays.
- **Cosign image-signing admission** — Phase 3.
- **Kata + SEV-SNP confidential controller** — Phase 4.
- **K8s AI Conformance v1.35+ public certification** — post-OSS.

---

## Provenance

Every claim in this document maps to `git log main..HEAD` on dev. The audit
reviewer for PR #44 should sample any 5 rows and verify with:

```bash
git log --oneline main..dev <path>          # commits that touched that file
ls docs/security-audits/                     # find the dated audit doc
cat ci/<gate>.sh                             # confirm the CI gate runs
cargo test --package <crate>                 # confirm tests pass
```

Last verified against `dev@4762aee` (2026-04-27).
