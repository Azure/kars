# OWASP MCP Top 10 (2025) — AzureClaw controls matrix

Internal mapping. Each row answers: what AzureClaw surface takes the hit,
what control applies today, what lands per the implementation plan, and
the code/config reference. Written against the OWASP MCP Top 10 2025
listing (<https://owasp.org/www-project-mcp-top-10/>), re-verified on
2026-04-24.

> **Scope:** this document covers MCP calls originating *from* sandboxed
> agents and MCP servers *hosted in AKS* via the forthcoming `McpServer`
> CRD. Managed SaaS MCP servers published by Foundry are out
> of scope (Foundry hosts those).

## Summary table

| # | OWASP MCP (2025) | Primary surface | Status |
|---|---|---|---|
| 01 | Token Mismanagement & Secret Exposure | Router auth path, ConfigMap/Secret custody, OAuth 2.1 | Partial today; hardens on the roadmap |
| 02 | Privilege Escalation via Scope Creep | `ToolPolicy` CRD, AGT policy profile, scopes-per-tool | Partial today; broadens on the roadmap |
| 03 | Tool Poisoning | Sandbox image supply chain, cosign on pod images | Cosign verification tracked on the roadmap |
| 04 | Supply Chain & Dependency Tampering | SBOM per release, SLSA-v1, vendored-patch audit | Vendored audit today; SLSA on the roadmap |
| 05 | Command Injection & Execution | Seccomp-strict, Landlock, egress-guard | Live today |
| 06 | Intent Flow Subversion (prompt injection) | Foundry Content Safety + `InferencePolicy` guardrails | Content Safety live; CRD broadens on the roadmap |
| 07 | Insufficient Authentication & Authorization | OAuth 2.1 on MCP, AGT `PolicyDecisionProvider` | OAuth 2.1 on the roadmap |
| 08 | Lack of Audit and Telemetry | AGT `AuditLogger`, OTel GenAI SemConv spans | Audit live; SemConv on the roadmap |
| 09 | Shadow MCP Servers | Shadow-MCP admission policy (VAP) | tracked on the roadmap |
| 10 | Context Injection & Over-Sharing | Per-sandbox namespace isolation, `Mcp-Session-Id`, tenancy boundary | Namespace live; session ID on the roadmap |

## Detail

### MCP01 — Token mismanagement & secret exposure

**Threat.** Long-lived bearer tokens baked into MCP clients; secrets
leaking through agent memory, prompt context, or stack traces.

**Today.** The sandbox pod authenticates to Foundry and Azure APIs via
IMDS + Workload Identity; no long-lived tokens in the image.
`<name>-credentials` secret is mounted via `envFrom` into the agent
container; **never mounted into the inference-router**; `UID 1000`
agent code reads its own bindings but not router identity.

**Gap.** Inbound MCP calls today run through the vendored trust path
(handoff-style KNOCK); OAuth 2.1 short-lived token flow is tracked on the roadmap
deliverable.

**Plan.** A planned `inference-router/src/mcp/oauth.rs` module
enforces PKCE, audience indicators (RFC 8707), and resource-indicated
tokens; refresh-token rotation mandatory; no bearer token persists on
disk. Negative tests in `tests/conformance/oauth/` assert replayed
token reject / expired token reject / audience mismatch reject
.

**Refs.** `sandbox-images/openclaw/entrypoint.sh` (credential mount),
`deploy/helm/azureclaw/templates/` (secret shape),
tracked on the roadmap.

### MCP02 — Privilege escalation via scope creep

**Threat.** A tool authorised for "read file" also effectively performs
"exec"; scopes grow to an over-permissioned blob.

**Today.** Sandbox agents are UID 1000 with seccomp-strict (219 syscalls
allowed, 28 explicit denies), Landlock read-only enforced on
`/sandbox/plugins`, `/sandbox/node_modules`, `/sandbox/skills` (see
`sandbox-images/openclaw/entrypoint.sh:713-747`). Egress goes **only**
through `127.0.0.1:8443` via iptables UID filter.

**Today.** `ToolPolicy.spec` carries `appliesTo` selector
and AP2 commerce caps; the roadmap adds `approval` and `rateLimit`
precedence. Policy evaluation is an AGT call
(`PolicyDecisionProvider`) — we surface scopes, AGT decides.

**Refs.** `controller/src/tool_policy_reconciler.rs`.

### MCP03 — Tool poisoning

**Threat.** Image replaced between publish and pull; malicious layer
injected; tool binary swapped post-deploy.

**Today.** Images pushed to the per-deployment ACR via `azureclaw up`;
`imagePullPolicy: Always` used across controller-managed deployments;
ACR content-trust available but not enforced at admission.

**Plan.** Planned (see roadmap): **cosign admission verification** on pod images against the release key, with a Rekor
transparency-log freshness window. Unsigned or stale images →
admission deny. Distinct from CR-spec signing.

### MCP04 — Software supply chain & dependency tampering

**Threat.** Compromised npm/crate dependency; tampered mesh SDK or
relay/registry image; dropped patch integrity.

**Today.** AzureClaw uses Microsoft AGT AgentMesh only. The historical
vendored AgentMesh fork was removed after the gap-closing patches landed
upstream, so dependency review focuses on the AGT SDK
version, AGT relay/registry images, and regular SCA gates rather than local
patch drift.

**Plan.** Planned (see roadmap): SBOM per release and SLSA-v1 provenance on CR
specs. `trivy` + `cosign-verify` + SCA gates enter
permanent CI on the roadmap.

### MCP05 — Command injection & execution

**Threat.** Unvalidated input flows into shell or tool argv; model
coerced to assemble a dangerous call.

**Today (strongest control).** The sandbox pod is the containment
boundary, not input validation. Even a complete code-injection win
in the agent process gives UID 1000 with seccomp-strict and no direct
egress; Landlock blocks writes to plugin/SDK directories; hijacked
code cannot exfiltrate without going through the router's proxy and
Foundry Content Safety. This is the "defense-in-depth" posture summarised
in `docs/security.md` (the layered defenses section).

**Conformance.** `tests/conformance/seccomp-landlock-egress/` (the conformance corpus) asserts that forbidden syscalls return `EPERM`
**not silently succeed** — directly targeting the class of bug where
a control looks present but isn't actually wired.

### MCP06 — Intent flow subversion (prompt injection)

**Threat.** Hostile content manipulates the model's routing, tool
selection, or downstream intent.

**Today.** Inference-router calls Foundry-side Content Safety
(`Microsoft.DefaultV2`); `prompt_filter_results` parsed from response
in `inference-router/src/safety.rs` (module docstring lines 1-14).
There is **no** in-process circuit breaker or cooldown — by design;
Foundry owns the guardrail.

**Plan.** Planned (see roadmap): `InferencePolicy.spec` (minimal) + tracked on the roadmap full
`InferencePolicy` with `guardrails` field expresses *which* Foundry
safety levels apply per-sandbox; a VAP denies
spec mutations that weaken Content Safety below the tenant floor.

### MCP07 — Insufficient authentication & authorization

**Threat.** Unauthenticated MCP endpoint exposed; anonymous callers
assumed trusted.

**Plan.** Planned (see roadmap): `McpServer.spec.productionMode: true` implies
`oauth.issuer` set (enforced by CEL `x-kubernetes-validations`).
Router refuses unauthenticated traffic on any
production-mode `McpServer`. Dev-mode anonymous requires
`azureclaw.azure.com/dev-only=true` label (admission-enforced,
`ci/no-null-provider-prod.sh` is the static mirror).

### MCP08 — Lack of audit and telemetry

**Threat.** Attacker actions go unlogged; incident response has no
trail.

**Today.** AGT governance runs in-process in the Rust router.
`PolicyEngine`, `AuditLogger`, `TrustManager`, `RateLimiter`,
`BehaviorMonitor` implemented in
`inference-router/src/governance/mod.rs` and
`inference-router/src/providers/policy.rs`. Names frozen in
`docs/security.md` §Governance (lines ~153-170) and `CHANGELOG.md`.

**Plan.** Planned (see roadmap): OTel GenAI SemConv 1.x emission on every router
span. `kubectl claw attest <name>` returns fresh
attestation including AGT audit-receipt id.

### MCP09 — Shadow MCP servers

**Threat.** Undocumented or orphaned MCP endpoints running somewhere
in the cluster; no CR surface, no policy binding.

**Plan.** Planned (see roadmap): `controller/src/admission/shadow_mcp.rs` — ValidatingAdmissionPolicy denies any MCP call (observed
by the router) to a server without a matching `McpServer` CR. Signal
sourced from AGT `BehaviorMonitor`. Paired with the
admission block so detection → enforcement is one loop.

### MCP10 — Context injection & over-sharing

**Threat.** State from one sandbox, session, or tenant bleeds into
another; context merges across boundaries.

**Today.** Each sandbox gets its own K8s namespace (`azureclaw-<name>`)
with NetworkPolicy + seccomp + Landlock. `ClawMemory` is a **binding
resource** over Foundry Memory Store; no
in-cluster memory backend shipped, so no cross-sandbox leak surface
to defend.

**Plan.** Planned (see roadmap): MCP Streamable HTTP enforces `Mcp-Session-Id`
semantics — session id is scoped per
`McpServer` CR; `tests/conformance/mcp-2026/` asserts session
confusion attacks fail closed.

## Re-audit triggers

* Upstream OWASP MCP list issues 2026 update — re-verify every row.
* Any new MCP transport lands (HTTP/3, gRPC, alt-stream) → re-map
 MCP05 / MCP07 controls.
* AGT AgentMesh replaces vendored mesh for a tenant → MCP08 telemetry
 path changes; re-check span emission coverage.
* `ClawMemory` backend ever gains in-cluster storage → MCP10 must
 add cross-tenant isolation controls.

## Related docs

* `docs/security.md` — the live security posture (layered defenses,
 syscall counts, component names).
