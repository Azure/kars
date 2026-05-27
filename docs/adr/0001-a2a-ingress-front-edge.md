# ADR 0001: A2A 1.0 ingress — single gateway, router never publicly exposed

**Status:** Accepted
**Date:** 2026-04-25
**Deciders:** Pal Lakatos-Toth, Copilot (drafter)
**Context PRs:** `phase1/a2a-1.0.0-scaffold` (data model + JWS), this ADR
**Implementation plan:** §7 (Phase 1) — A2A 1.0 protocol support

## Context

The A2A 1.0 (Agent2Agent) protocol is a public-facing, peer-to-peer
agent interop protocol with no central registry: each agent serves
its own signed AgentCard at `/.well-known/agent.json` (RFC 7515 JWS,
EdDSA per RFC 8037). To interoperate with foreign runtimes (LangChain,
Google ADK, OpenAI Agents, AWS Bedrock Agents), Kars must offer
a publicly-reachable A2A endpoint.

The sandbox inference router is the most privileged process in the
sandbox: it holds IMDS tokens, Foundry credentials, runs the policy
chain, and is the sole network interface for the agent. Exposing it
to the internet — directly per-sandbox — would multiply the attack
surface (one publicly-reachable service per sandbox) and put a
brand-new parser (JWS, JSON-RPC) one memory-corruption-bug away from
secret material.

A2A protocol logic must live in the router (not the plugin), because
the plugin (UID 1000) cannot accept inbound traffic in any
hardening-preserving way: egress-guard iptables wall it to localhost,
Landlock blocks bind() outside loopback, no `CAP_NET_BIND_SERVICE`.
Routing inbound A2A through the router lets us reuse the existing
policy / Content Safety / audit chain. This decision is non-negotiable.

The remaining question is: **what sits in front of the router so the
router itself is never on the public internet?**

## Decision

### D1. Single shared gateway component owns A2A public ingress

A new component `kars-a2a-gateway` is the **only** public TLS
endpoint for A2A. All foreign agents that talk to Kars resolve a
single hostname (e.g. `a2a.kars.example`); routing to the
target sandbox happens after JWS verification, by sandbox-id in the
URL path or JWS `sub` claim.

This mirrors the role `agentmesh-relay` plays for mesh: one shared,
hardened, audited public-edge component instead of N×sandbox-named
services.

**Rejected alternative:** per-sandbox public hostnames. Multiplies the
attack surface, requires per-sandbox cert provisioning, and forces
duplicated AuthN/AuthZ logic into every sandbox.

### D2. Router never has a public-internet ingress

The sandbox router gains exactly one new bind: `0.0.0.0:8445` for A2A
inbound. The two existing binds (`127.0.0.1:8443` main,
`127.0.0.1:8444` forward proxy) stay loopback-only. Reachability of
`8445` is gated by:

- A `Service` of type `ClusterIP` only — never `LoadBalancer`.
- A `CiliumNetworkPolicy` on the sandbox namespace permitting TCP 8445
  **only from the gateway's ServiceAccount** (`fromEndpoints` with
  identity selector), with L7 rules enforcing path/method/body-cap
  redundantly.
- mTLS termination by the router's listener: presents the sandbox's
  Workload Identity cert; requires the peer cert to chain to the
  gateway's Workload Identity issuer.

A ValidatingAdmissionPolicy (`phase1/a2a-vap-no-public-router-exposure`)
rejects any sandbox-namespace `Ingress`, `LoadBalancer` Service, or
`NetworkPolicy.ingress.from.ipBlock`. The "router is never publicly
exposed" invariant is hard-encoded into the cluster admission path.

### D3. Defense-in-depth at the network plane (Cilium L7)

The gateway sits behind a `CiliumNetworkPolicy` with L7 HTTP rules
that drop malformed traffic **before** it ever reaches Rust code:

- **Method allow-list:** `POST`, `GET`, `OPTIONS` only.
- **Path regex pinning:**
  - `^/.well-known/agents/[a-z0-9-]{1,63}/agent\.json$` for discovery
  - `^/a2a/v1/[a-z0-9-]{1,63}/(send|get|cancel|stream)$` for RPC
- **Required headers:** `Content-Type: application/json` for POST.
- **Body cap:** 4 MiB at the L7 layer (matches
  `mcp::streamable_http::MAX_FRAME_BYTES`).
- **Per-source-IP rate limit** and **connection-count limit** for
  flood resistance.
- **Connection / read / write timeouts** at conservative values (30s
  read, 60s response, 5min idle for SSE).

A second `CiliumNetworkPolicy` on the sandbox namespace re-asserts
method/path/body-cap on the cluster-internal hop (gateway → router).
Three independent gates between the internet and router process
memory.

### D4. Router-internal module isolation for A2A handler

A single router process handles A2A. To raise the bar against any
hypothetical memory-corruption in the JWS or JSON-RPC parser:

- A2A inbound code lives in a dedicated module
  `inference-router/src/routes/a2a/ingress.rs`.
- The module is `forbid(unsafe_code)` at the file level.
- The module is **structurally prohibited** from importing the types
  that hold secret material:
  - `crate::auth::ImdsToken`
  - `crate::auth::FoundryCredentials`
  - any concrete `*Credential*` / `*Token*` from `crate::auth`
- Enforcement is by a CI gate (`ci/a2a-module-isolation.sh`) that
  greps for forbidden imports in the A2A module subtree.
- Policy chain calls happen via the existing `PolicyDecisionProvider`
  trait — i.e. through indirection that does not carry credentials.
- A fuzz target (`fuzz/fuzz_targets/a2a_jws.rs` + `a2a_jsonrpc.rs`)
  is added at module landing and run in CI on every change.

This is **not** process isolation — same heap, same allocator. It
raises the difficulty of an exploit from "find a use-after-free" to
"find a use-after-free *and* heap-scan for IMDS token bytes without
having any type information about where they live." Combined with
Rust's existing memory safety, the residual risk is judged
acceptable for the threat model.

### D5. Sidecar process isolation deferred (not rejected)

A separate `a2a-handler` container running as UID 1002 with no IMDS
access, no Foundry creds, and a Workload Identity cert that can call
only `/a2a/internal/dispatch` on the router was considered. It would
provide structural process isolation against memory-corruption
exploits.

It is **deferred**, not rejected. Reasons for deferring:

- Adds a fourth container per sandbox pod, a third image to build /
  push / patch, a third NetworkPolicy slice, mTLS bootstrap between
  containers, and operational surface area.
- Rust memory safety + `forbid(unsafe_code)` + fuzz targets +
  module-level secret isolation already substantially reduce the
  attack class this would defend against.
- Can be added later without changing the gateway, the protocol, or
  the agent-side experience. Forward-compatible.

We will revisit this decision if (a) a CVE in any router dep makes a
sandbox attack credible, or (b) production traffic patterns warrant
the additional isolation.

### D6. Surgical, opt-in, revocable per-sandbox exposure

A2A inbound exposure is **never** automatic. Every step from
"sandbox exists" to "sandbox reachable from the internet for A2A"
requires explicit, auditable opt-in:

**1. Cluster-wide default: A2A inbound disabled.**
The gateway component is deployable but ships with an empty
`spec.a2a.allowedSandboxes` allow-list. No sandbox is reachable
through the gateway by default, even if it has A2A code.

**2. Per-sandbox CRD opt-in.** The `KarsSandbox` CRD gains an
optional `spec.a2a` block:

```yaml
spec:
  a2a:
    enabled: false                # default false; required true to expose
    allowedCallers:               # required when enabled; empty = deny-all
      - subject: "did:web:example.com:agents:planner"
        thumbprint: "sha256:abcd..."   # JWS issuer key thumbprint
        expiresAt: "2026-07-01T00:00:00Z"
    minimumTrustScore: 700        # default 700; AGT score below → reject
    advertisedSkills:             # explicit skill allow-list
      - "search.web"
      - "summarize.text"
    rateLimit:
      perCallerRpm: 30            # per-caller requests per minute
      globalRpm: 300              # overall ceiling for this sandbox
    bodyCapBytes: 1048576         # default 1 MiB; 4 MiB hard ceiling
    sessionMaxSeconds: 60         # max time any single inbound RPC may hold
    allowStreaming: false         # SSE off by default
    expiresAt: "2026-05-01T00:00:00Z"  # MUST be set; max 30d in future
```

Every field is **required to be set explicitly when `enabled: true`**.
There are no implicit defaults that broaden exposure. Validation:

- `enabled: true` with empty `allowedCallers` → admission rejects.
- `expiresAt` absent or > 30d in the future → admission rejects.
- `advertisedSkills` empty when `enabled: true` → admission rejects.
- `minimumTrustScore < 500` → admission rejects unless namespace has
  label `kars.io/a2a-low-trust=acknowledged` and a
  matching `Acknowledgement` CR exists (sign-off path; not a flag).

**3. Time-bounded exposure.** `expiresAt` is mandatory and capped at
30 days. Once it passes, the controller transitions
`status.a2a.state` to `Expired` and removes the gateway routing
entry within one reconcile loop. Re-exposing requires updating the
CRD with a new `expiresAt` (audited as a fresh opt-in event).

**4. Caller pinning by JWS thumbprint, not just subject.**
`allowedCallers[].thumbprint` pins the caller's exact AgentCard
signing key. Subject (DID/URL) alone is not sufficient — a foreign
agent that re-keys cannot impersonate the previous caller without
the operator updating the CRD.

**5. Skill allow-list at the gateway.** The gateway maps each
inbound JSON-RPC method/skill-id against `advertisedSkills` and
rejects unknown calls with `A2aErrorCode::UnsupportedOperation`.
A foreign agent cannot probe for unadvertised methods.

**6. Per-sandbox NetworkPolicy is generated, not hand-written.**
The controller emits the `CiliumNetworkPolicy` admitting gateway →
router (TCP 8445) **only when `spec.a2a.enabled: true`** and only
for the duration `expiresAt` permits. When `enabled` flips to
`false` (or `expiresAt` passes), the controller deletes the
NetworkPolicy and the Service before any other reconciliation; the
sandbox returns to "zero inbound" within one reconcile.

**7. Gateway routing table is controller-owned, not free-form.** The
gateway reads its routing table from a `ConfigMap` that **only the
controller writes**. RBAC: gateway SA has `get/watch` on the
ConfigMap, never `update/patch`. An exploit in the gateway cannot
add new sandbox routes or extend an exposure window — it can only
serve traffic the controller has explicitly authorised.

**8. Revoke-now is a single field flip.** Setting
`spec.a2a.enabled: false` (or deleting the `KarsSandbox`) triggers
the controller to (a) remove the gateway ConfigMap entry, (b)
delete the NetworkPolicy, (c) delete the ClusterIP Service for
8445, (d) emit an audit event — all within one reconcile loop.
Time-to-revoke target: < 30 seconds end-to-end. No restart of
the gateway, no human in the loop.

**9. Continuous attestation in the audit log.** Every inbound A2A
call emits an audit event containing: caller subject, caller
thumbprint, caller AGT trust score at call time, target sandbox-id,
RPC method, payload SHA-256, gateway-side latency, router-side
latency, decision (allow / deny / reason). Audit is append-only via
`AuditSink` and rotated to AGT.

**10. Operator-facing "what is currently exposed?" command.**
`kars a2a list-exposed` queries all `KarsSandbox` resources
across the cluster and prints a single table: namespace, sandbox-id,
allowed callers, advertised skills, expiry, current trust threshold.
Operators get a single source of truth for live A2A exposure
posture without grep'ing CRDs by hand. A `--json` flag enables
scripted compliance checks.

**11. Blast-radius bounds.** Even when `enabled: true`, a
compromised foreign caller cannot:

- Reach any sandbox other than the one their thumbprint is pinned
  to (gateway routing is per-route).
- Call any tool not in `advertisedSkills` (skill allow-list at
  gateway, re-checked at router).
- Exceed `rateLimit` (token-bucket per caller + per sandbox).
- Sustain a session beyond `sessionMaxSeconds` (router-side
  watchdog).
- Exceed `bodyCapBytes` (Cilium L7 cap → router L7 cap → in-process
  cap, three independent gates).
- Persist past `expiresAt` (controller revocation).
- Trigger any tool outside the existing `ToolPolicy` envelope
  (policy chain runs unchanged).
- Consume IMDS/Foundry tokens via memory disclosure (module-level
  isolation in the router; D4).

### D7. Agent-side A2A egress unchanged from any other egress

Outbound A2A from the sandbox (agent calls foreign agent) flows
exactly like any other egress today:

```
agent → plugin → 127.0.0.1:8444 forward proxy → policy chain → foreign agent
```

No gateway involvement. No new exposure. Plugin holds no A2A signing
key in this direction; the router signs outbound AgentCards via
`SigningProvider`.

### D8. AgentCard custody

The router signs each sandbox's AgentCard via `SigningProvider`. The
gateway **fetches and caches** signed cards from the per-sandbox
router on demand (cluster-internal mTLS), serving them at the public
well-known URI. The gateway never holds a per-sandbox signing key.

## Architecture

**Pod layout reminder:** the router and the agent run as **two
separate sidecar containers** in the sandbox pod, sharing only the
pod's network namespace. Different images (`kars-inference-router`
vs `openclaw`), different UIDs (1001 vs 1000), different filesystems,
different seccomp profiles, different Landlock policies. They
communicate over `127.0.0.1` inside the pod — i.e. kernel-routed
loopback between two distinct user-space processes in two distinct
containers. This is already container-grade isolation; A2A inherits it.

```
Internet
    │
    ▼  TLS (cert-manager, public hostname)
┌──────────────────────────────────────────────────────────────┐
│  Cilium L7 ingress (kars-system ns)                     │
│   - Method allow-list, path regex, body cap, rate limit      │
└──────────────────────────────────────────────────────────────┘
    │
    ▼
┌──────────────────────────────────────────────────────────────┐
│  kars-a2a-gateway (Rust binary, shared)                 │
│   - Public TLS terminator                                    │
│   - Verify caller AgentCard JWS via SigningProvider          │
│   - AGT trust score gate                                     │
│   - Per-caller rate limit, audit event                       │
│   - Routes by sandbox-id (controller-owned ConfigMap)        │
│   - NO Foundry creds, NO IMDS access                         │
└──────────────────────────────────────────────────────────────┘
    │
    ▼  cluster-internal mTLS (Workload Identity, peer-pinned)
┌──────────────────────────────────────────────────────────────┐
│  CiliumNetworkPolicy (sandbox ns) — only emitted by the      │
│  controller when KarsSandbox.spec.a2a.enabled is true        │
│   - Permit TCP 8445 from gateway SA only                     │
│   - L7 re-validate method/path/body-cap                      │
└──────────────────────────────────────────────────────────────┘
    │
    ▼  TCP 8445 (sandbox pod IP)
┌─ Sandbox pod (single pod, two sidecar containers) ───────────┐
│                                                              │
│  Container: inference-router (UID 1001, separate image)      │
│   - 127.0.0.1:8443 main (loopback, unchanged)                │
│   - 127.0.0.1:8444 forward proxy (loopback, unchanged)       │
│   - 0.0.0.0:8445   A2A inbound (NEW)                         │
│                                                              │
│   routes/a2a/ingress.rs                                      │
│    - forbid(unsafe_code)                                     │
│    - module-isolation: cannot import auth::ImdsToken etc.    │
│    - re-verifies JWS, body cap, JSON-RPC parse               │
│    - calls policy chain via trait                            │
│                                                              │
│   ↓  127.0.0.1:18789 (loopback, separate container)  ↓       │
│                                                              │
│  Container: openclaw (UID 1000, separate image)              │
│   - Receives plaintext A2A request via OpenClaw gateway      │
│   - Plugin holds AgentCard signing key (SigningProvider)     │
│   - Cannot bind inbound (egress-guard iptables enforce)      │
└──────────────────────────────────────────────────────────────┘
```

## Consequences

### Positive

- Sandbox's "zero public ingress" posture is preserved.
- Single public TLS surface (`kars-a2a-gateway`) — same
  one-shared-component model as `agentmesh-relay`.
- Three independent gates (Cilium L7 → gateway → router L7 + mTLS)
  before any user-controlled bytes reach Rust parsers in the router.
- Router-internal module isolation makes the A2A code path
  structurally unable to name secret-holding types — CI-enforced.
- Outbound A2A inherits the existing policy chain unchanged.
- Architecture is forward-compatible with later sidecar isolation
  (D5) if needed.

### Negative

- One new component to build, deploy, operate
  (`kars-a2a-gateway`).
- New cluster-internal ingress path to each sandbox (gateway →
  router on TCP 8445). NetworkPolicy + mTLS make this safe but it is
  a delta from "zero ingress" today.
- Two-hop latency for inbound A2A (gateway → router → plugin).
- Memory-corruption in the A2A handler is not fully isolated from
  IMDS tokens (same process). Mitigated by Rust safety + module
  discipline + fuzzing; not eliminated. Acceptable for current threat
  model.

### Neutral

- Outbound A2A semantics unchanged.
- The merged scaffold (`phase1/a2a-1.0.0-scaffold`) data model and
  JWS builder are reused unchanged by both the gateway (verifying
  inbound) and the router (signing its own card before it is cached).

## Implementation phases

(Reflected as new entries in §7 of the implementation plan.)

1. `phase1/a2a-1.0.0-routes-internal` — mount router-internal A2A
   handlers on TCP 8445 behind a new ClusterIP Service. mTLS-pinned
   to gateway SA. No public exposure. Includes module-isolation CI
   gate (`ci/a2a-module-isolation.sh`).
2. `phase1/a2a-karssandbox-spec` — extend `KarsSandbox` CRD with
   the full `spec.a2a` block (D6); admission validators reject
   unsafe combinations (empty `allowedCallers` with `enabled: true`,
   missing `expiresAt`, expiry > 30d, `minimumTrustScore < 500`
   without acknowledgement CR).
3. `phase1/a2a-controller-revocation` — controller logic to emit
   and delete `Service` + `CiliumNetworkPolicy` + gateway ConfigMap
   entry on opt-in / opt-out / expiry. Reconcile target:
   < 30s end-to-end revocation.
4. `phase1/a2a-gateway-component` — new `kars-a2a-gateway`
   Rust binary, Helm chart, deployment. Verifies inbound JWS,
   forwards to per-sandbox router. Routing table sourced from
   controller-owned ConfigMap (gateway SA: get/watch only).
5. `phase1/a2a-cilium-l7-policies` — Cilium L7 rules for both the
   gateway-ingress and gateway→router edges.
6. `phase1/a2a-vap-no-public-router-exposure` — VAP rejecting any
   sandbox-namespace `Ingress`, `LoadBalancer` Service, or
   `NetworkPolicy.ingress.from.ipBlock`.
7. `phase1/a2a-cli-list-exposed` — `kars a2a list-exposed`
   subcommand (D6 #10) plus `--json` for compliance scripting.
8. `phase1/a2a-egress-from-sandbox` — outbound A2A path through the
   existing forward proxy.
9. `phase1/a2a-fuzz-targets` — `fuzz/fuzz_targets/a2a_jws.rs` and
   `a2a_jsonrpc.rs`.

## Alternatives considered

**A. Expose router directly per-sandbox.** Rejected: posture change is
extreme; N×sandboxes on the internet; each publicly-named.

**B. Outbound-only A2A (no inbound).** Acceptable fallback if the
gateway component is delayed. Ships a usable subset.

**C. Put A2A in the plugin (mirror mesh).** Rejected for the inbound
path: plugin cannot accept inbound traffic without breaking sandbox
hardening. Outbound-A2A-in-plugin was considered but rejected for
code-locality and policy-chain reuse reasons.

**D. Run the gateway as a sidecar in each sandbox pod.** Rejected:
multiplies the public attack surface, duplicates AuthN logic.

**E. Sidecar container `a2a-handler` (UID 1002) for process
isolation.** Deferred per D5, not rejected. Adds genuine memory
isolation but costs operational complexity that current threat model
does not yet demand.

## References

- A2A 1.0 specification: <https://a2a-protocol.org/v1.0.0/specification>
- RFC 7515 (JWS): <https://www.rfc-editor.org/rfc/rfc7515>
- RFC 8037 (JOSE EdDSA): <https://www.rfc-editor.org/rfc/rfc8037>
- RFC 8725 (JWT BCP — alg confusion): <https://www.rfc-editor.org/rfc/rfc8725>
- Kars implementation plan §1.2.1 (SigningProvider trait), §7
  (Phase 1 protocol scope), §0.2 #11 (dev-only branching).

## Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
