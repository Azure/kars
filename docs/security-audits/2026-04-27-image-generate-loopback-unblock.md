# Security audit — Image-generate loopback unblock

Audit ID: `2026-04-27-image-generate-loopback-unblock`
Scope reference: `docs/implementation-plan.md` §0.2 #9 (security-audit
sign-off required for any `sandbox-images/*/entrypoint.sh` change). Pairs
commit `97ef710` (`fix(sandbox): unblock built-in image_generate provider
on loopback router`). Audited retroactively as part of the PR #44 readiness
sweep — the original commit landed without the audit doc, which this file
corrects.

## Summary

The OpenClaw 2026.4.x built-in `image_generate` tool (powered by the bundled
OpenAI image-generation provider) refuses to call any provider whose
`baseUrl` resolves to a private / loopback address. AzureClaw's runtime
configures every model — including the image model — to point at the
in-pod inference router on `http://127.0.0.1:8443`. As a result the agent
sees:

```
openai/gpt-image-1: Blocked hostname or private/internal/special-use IP
address
openai/gpt-image-2: Blocked hostname or private/internal/special-use IP
address
```

even though the loopback target is our own authenticated router and not an
attacker-controlled internal endpoint. Upstream OpenClaw provides a narrow,
explicit opt-in env var for exactly this case
(`OPENCLAW_QA_ALLOW_LOCAL_IMAGE_PROVIDER=1`) which is *only* honoured when
`baseUrl` already matches `http://127.0.0.1:` or `http://localhost:` — so
there is no way to weaponise the toggle into a generic SSRF amplifier.

The fix exports that env var from `sandbox-images/openclaw/entrypoint.sh`
right after the gateway-token plumbing (commit `97ef710`). No router or
plugin change.

## Threat model delta

| Asset | Before | After | Notes |
|---|---|---|---|
| Agent → arbitrary internal/private IP via image provider | blocked by upstream SSRF guard | **still blocked** | Upstream gate (`shouldAllowPrivateImageEndpoint`) verifies `baseUrl.startsWith("http://127.0.0.1:")` *before* honouring the env var. |
| Agent → router loopback for image generation | blocked (false positive) | allowed | Router authenticates via Workload Identity, not the agent — caller cannot impersonate. |
| Agent → cloud Foundry image endpoint | unaffected | unaffected | Path: agent → 127.0.0.1:8443 (router) → Foundry; identical to text gen. |
| Egress-guard rules | unchanged | unchanged | UID-1000 still blocked from direct egress; only loopback + DNS allowed at L4. |

STRIDE:

* **Spoofing** — no new surface. Caller of the loopback endpoint is the
  agent process itself; no cross-tenant identity claim is possible.
* **Tampering** — content is forwarded through the same Content-Safety
  guardrail path as `chat/completions`; image prompts traverse Foundry's
  standard prompt-shield pipeline.
* **Repudiation** — image-gen requests log to the same router audit chain
  as inference (`AuditSink` trait, in-tree impl).
* **Information disclosure** — none new. Image responses are bytes
  returned over the existing TLS-to-Foundry path.
* **Denial of service** — capped by existing token-budget + rate-limit
  policy on the router; image-gen consumes from the same budget.
* **Elevation of privilege** — none. UID 1000 cannot escape to UID 1001
  router context; no new syscalls allowed.

## OWASP mapping

OWASP LLM Top 10 v2.0:

* **LLM02 — Insecure Output Handling**: image bytes flow through the
  router's image-content-safety path (already in place for cloud Foundry
  image responses). Unchanged.
* **LLM05 — Supply Chain**: env-var name lifted directly from upstream
  OpenClaw. Re-audit trigger registered (see below) for any rename or
  semantic change.
* **LLM10 — Model DoS**: existing token budget + rate-limit policies
  apply.

OWASP MCP Top 10 (where applicable):

* **MCP04 — Egress controls**: the guard remains in place; this PR only
  un-blocks the *loopback* case which was always intended to be allowed.

## AuthN / AuthZ path

```
agent (UID 1000)
  └─ localhost:8443  (allowed by egress-guard iptables; UID-1000 denied direct egress)
       └─ inference-router (UID 1001, Workload Identity)
            └─ Foundry image endpoint (Entra federated WI, RBAC scoped to AI User)
                 └─ Content Safety + Prompt Shield (Foundry-side)
```

Outage behaviour: identical to text inference. `Strict` mode (prod
default) refuses on AGT/policy outage; `DegradedDev` (dev only) returns
warning-labelled fallback. No image-specific override.

## Secret + key custody

No new secrets. Image-gen reuses the router's existing IMDS token
exchange. Agent (UID 1000) still cannot read `/run/secrets/`,
`/etc/azureclaw/secrets/`, or `/tmp/.agt-admin-token`.

## Egress surface delta

None. The agent-side request stays on the loopback interface; the router
is the sole egress origin. Pre-existing `NetworkPolicy` allow-lists for
Foundry still gate the L4 path.

## Audit events emitted

* `AuditSink` entry on the router for each image-gen request (operation
  type `inference.image`, captured by the existing inference route).
* AGT `PolicyDecisionProvider` evaluation per call (tool name
  `image_generate`).
* No agent-side audit emission (correct — agent never sees the secret
  path).

## Failure mode

* Env var unset: upstream OpenClaw rejects with "Blocked hostname …"
  (the bug we are fixing). Fail-closed.
* Env var set + non-loopback `baseUrl`: still blocked by upstream gate
  (`!baseUrl.startsWith("http://127.0.0.1:") && !baseUrl.startsWith("http://localhost:") → return false`).
  Fail-closed.
* Env var set + loopback `baseUrl` + router unreachable: image-gen
  returns the router's connection-refused error to the agent. Fail-closed.
* Env var set + loopback + router 4xx (policy deny / token budget): same
  failure surface as text inference. Fail-closed.

The narrow gate guarantees there is no fail-open path introduced.

## Negative-test coverage

Pointer to conformance corpus entries that exercise the gate:

* `tests/conformance/seccomp-landlock-egress.spec.ts` — verifies UID-1000
  cannot reach a non-loopback IP regardless of env (covers the upstream
  gate's reject branch in our pod). The seccomp corpus continues to
  enforce the syscall-level egress block.
* No upstream OpenClaw test is duplicated; the upstream provider's gate
  semantics are verified in upstream's own test suite (cited in
  re-audit triggers).

## Vendored / third-party dependency delta

None. The env var is a contract with upstream OpenClaw; no crate or npm
package added or pinned.

## References (Principle 10)

* Upstream provider gate logic (verified via prior session investigation):
  `extensions/openai/image-generation-provider.ts` ·
  `shouldAllowPrivateImageEndpoint(req)` — checks `baseUrl` is loopback
  *and* `process.env.OPENCLAW_QA_ALLOW_LOCAL_IMAGE_PROVIDER === '1'`.
* SSRF helper: `src/infra/net/ssrf.ts` ·
  `isPrivateNetworkOptInEnabled(...)` — separate, broader opt-in (not
  used here).
* Our router image route: `inference-router/src/routes/inference.rs`
  (post-Phase-1 split into `mcp/` + `a2a/`; image gen continues to use
  the inference path).
* Egress-guard iptables policy: `sandbox-images/openclaw/entrypoint.sh`
  egress-guard init container (UID-1000 loopback + DNS only).

## Principle audit

* §0.2 #1 — zero regressions: prior text-only flows untouched; image-gen
  path moves from broken-by-default to working.
* §0.2 #2 — AGT boundary: no policy/audit/signing logic added on our
  side.
* §0.2 #3 — language: bash-only sandbox-images change.
* §0.2 #4 — LOC: 1 export line added; no hotspot impact.
* §0.2 #5 — compatibility: Native mode only; no upstream-CR change.
* §0.2 #6 — non-compete: image gen is a Foundry capability we surface;
  Foundry remains the model provider.
* §0.2 #7 — standards-ready: upstream's env var is the accepted opt-in
  shape.
* §0.2 #8 — solid: no stub, no custom crypto, narrow upstream gate
  trusted by upstream's own tests.
* §0.2 #9 — this audit doc (retroactive).
* §0.2 #10 — refs pinned above.
* §0.2 #11 — committed to `dev`, not `main`.

## Re-audit triggers

* Upstream OpenClaw renames or removes `OPENCLAW_QA_ALLOW_LOCAL_IMAGE_PROVIDER`
  → re-verify env var is still honoured; switch to whatever upstream
  renames it to.
* Upstream OpenClaw widens `shouldAllowPrivateImageEndpoint` (e.g. drops
  the loopback prefix check) → revoke our export immediately and route
  the agent through a router-mediated image path instead.
* Router's image route gains an authenticated-only handler that does not
  share the inference port → re-evaluate whether the loopback gate is
  even needed.
* AzureClaw moves to a non-loopback router endpoint (e.g. UDS) → this
  env var no longer satisfies the upstream gate; switch model
  configuration to the new transport.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
