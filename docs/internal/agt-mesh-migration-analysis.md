# AGT-Mesh Migration — Gap Analysis

**Question:** Can AzureClaw move off the vendored `amitayks/agentmesh` stack
(TypeScript SDK + Rust relay + Rust registry) and onto AGT's
`microsoft/agent-governance-toolkit` agent-mesh for inter-agent E2E messaging?

**Short answer:** **Not today.** Governance is already on AGT (router uses
`agentmesh` Rust crate v3.1.0). The messaging transport is not yet a viable
swap — AGT's TS SDK has no E2E crypto, there is no store-and-forward relay,
and the identity/wire formats differ. A migration is strategically attractive
but currently requires AGT investment we don't control.

---

## 1. Scope — what are we actually replacing?

Three vendored components and their AzureClaw integrations:

| Vendored | Upstream | Our usage | In scope? |
|---|---|---|---|
| `@agentmesh/sdk` v0.1.2 (TS) | `amitayks/agentmesh` | `cli/src/plugin.ts`, `mesh-plugin/src/connection.ts` | ✅ Yes |
| `agentmesh-relay` v0.3.0 (Rust) | `amitayks/agentmesh` | Deployed in-cluster (`deploy/agentmesh.yaml`) | ✅ Yes |
| `agentmesh-registry` v0.3.0 (Rust) | `amitayks/agentmesh` | Deployed in-cluster | ✅ Yes |
| `agentmesh` Rust crate v3.1.0 | `microsoft/agent-governance-toolkit` | `inference-router/src/governance.rs` (PolicyEngine, TrustManager, AuditLogger, MCP redactor, rate limiter) | ❌ **Already on AGT** |

**Clarification:** we already consume AGT for policy, trust, audit, MCP
governance, and rate limiting in the Rust router. This analysis is **only**
about the TS messaging stack.

## 2. What the vendored stack actually provides

From [`vendor/agentmesh-sdk`](../vendor/agentmesh-sdk/),
[`vendor/agentmesh-relay`](../vendor/agentmesh-relay/),
[`vendor/agentmesh-registry`](../vendor/agentmesh-registry/):

### Capabilities AzureClaw depends on

| # | Capability | Where used | Criticality |
|---|---|---|---|
| 1 | **Signal Protocol E2E (TypeScript)** — X3DH + Double Ratchet in the agent process | `cli/src/plugin.ts`, `mesh-plugin/src/connection.ts` | ⛔ Blocking |
| 2 | **Store-and-forward relay** — inbox persistence so offline sub-agents get messages on reconnect (72h TTL — `vendor/agentmesh-relay/src/connection.rs` uses `Duration::hours(72)`) | All offload + handoff flows | ⛔ Blocking |
| 3 | **Registry discovery** — AMID→pubkey lookup, capability search, 5-min freshness window | `mesh_discover`, handoff, sub-agent spawn | ⛔ Blocking |
| 4 | **Prekey upload/download** — X3DH bundle repository | Part of session init | ⛔ Blocking |
| 5 | **KNOCK protocol** — intent-carrying handshake with auto-accept policy hook | All sessions | ⛔ Blocking |
| 6 | **Session cache & reuse** | Performance | 🟡 Important |
| 7 | **Heartbeat + presence** | Peer visibility, last-seen | 🟡 Important |
| 8 | **Plaintext peers escape hatch** | Rust controller speaks to TS agents | 🟡 Important |
| 9 | **Reputation feedback** (`submitReputation`) | Post-session trust signal | 🟢 Nice-to-have |
| 10 | **DID document resolution** | Not actively consumed | ⚪ Unused |
| 11 | **Organization DNS verification** | Not actively consumed | ⚪ Unused |

### 19 vendored patches we depend on

11 SDK + 4 relay + 4 registry. Summary categories:
- **Correctness fixes** (empty signatures, wrong table names, prekey/register ordering, base64 prefix stripping)
- **Signature verification alignment** (raw-timestamp signing to avoid chrono re-serialization mismatch)
- **Connection lifecycle** (session-aware supersede, stale-state reconnect)
- **Performance** (`bytesToBase64` stack overflow on >100KB)
- **Extensibility hooks** (`wsFactory` for HTTPS_PROXY CONNECT tunnels, `plaintextPeers` for the Rust controller)

Full list in each [`vendor/*/README.md`](../vendor/).

## 3. What AGT agent-mesh provides today

From [`agent-governance-toolkit/packages/agent-mesh/`](https://github.com/microsoft/agent-governance-toolkit/tree/main/packages/agent-mesh):

### Layers

1. **Identity & zero-trust** — Ed25519 DIDs (`did:mesh:<hex>`), SPIFFE/SVID, sponsor accountability, 15-min ephemeral credentials.
2. **Trust & protocol bridge** — 5-dimension trust scoring (0–1000), TrustBridge adapters to A2A, MCP, IATP, ACP.
3. **Governance & compliance** — YAML/JSON + OPA/Rego policy engine, hash-chain audit, EU AI Act / SOC2 / HIPAA / GDPR compliance mapping.
4. **Reward & learning** — decay model, 5-class anomaly detection (rapid-fire, drift, frequency, degradation, time-of-day).

### SDKs & parity

| Capability | Python | TypeScript (`@microsoft/agentmesh-sdk` 3.2.0) | Rust (`agentmesh` 3.1.0) |
|---|---|---|---|
| Identity, trust, policy, audit | ✅ | ✅ | ✅ |
| Lifecycle | ✅ | ✅ | ❌ |
| **X3DH + Double Ratchet** | ✅ | ❌ | ❌ |
| WebSocket / gRPC transport | ✅ | ❌ | ❌ |
| Framework integrations | ✅ (6+) | ❌ | ❌ |

The TS and Rust SDKs are **governance-only subsets** of the Python SDK. E2E encryption (`encryption/channel.py`, `encryption/x3dh.py`, `encryption/ratchet.py`) lives **only in Python**.

### Services

API gateway, trust engine, policy server, audit collector, agent registry — all stateful **governance** services.

### Critical absence

**There is no store-and-forward relay.** The transport layer assumes agents are directly reachable (WebSocket or gRPC). No offline inbox, no message TTL, no delivery-on-reconnect.

## 4. Capability overlap matrix

| # | Vendored capability | AGT status | Gap |
|---|---|---|---|
| 1 | TS X3DH + Double Ratchet | ❌ Python-only | **Blocking** — agent runtime is Node.js |
| 2 | Store-and-forward relay | ❌ Not provided | **Blocking** — offload/handoff require it |
| 3 | Agent registry with last-seen | ✅ Equivalent (different schema: `did:mesh` vs `amid`) | Identity re-key required |
| 4 | Prekey upload/download | ✅ In Python SDK only | Needs TS surface |
| 5 | KNOCK intent protocol | ❌ Not in AGT | Would need implementation |
| 6 | Reputation feedback | 🟡 Via trust engine signals, different API | Rewrite |
| 7 | Plaintext peers hook | ❌ | Would need to be added |
| 8 | `wsFactory` HTTPS_PROXY hook | ❌ | Would need to be added |
| 9 | 5-dimension trust scoring | ✅ **Better than amitayks** | Already on AGT in router |
| 10 | Hash-chain audit + compliance export | ✅ **Better than amitayks** | Already on AGT in router |
| 11 | Policy engine with OPA/Rego | ✅ **Better than amitayks** | Already on AGT in router |
| 12 | DID identity | ✅ Exceeds | Format change |

**Legend:** ✅ equal-or-better · 🟡 partial · ❌ missing · ⛔ blocking

## 5. Blocking gaps (why we can't migrate today)

### Gap A — No TypeScript E2E crypto

Signal Protocol (X3DH + Double Ratchet) exists only in `agentmesh-platform`'s
**Python** package
([`src/agentmesh/encryption/`](https://github.com/microsoft/agent-governance-toolkit/tree/main/packages/agent-mesh/src/agentmesh/encryption)).

`@microsoft/agentmesh-sdk` exports only `AgentIdentity`, `IdentityRegistry`,
`TrustManager`, `PolicyEngine`, `PolicyConflictResolver`, `AuditLogger`,
`AgentMeshClient`, `GovernanceMetrics`, `McpSecurityScanner`,
`LifecycleManager` — no `SecureChannel`, no `X3DHKeyManager`, no
`DoubleRatchet`. AGT's own `AgentMeshClient` in TS is a local governance
wrapper, not a transport client.

AzureClaw agents run under Node.js via OpenClaw. Options to close this gap:

1. **Wait for AGT to port crypto to TS.** Cleanest, zero engineering for us.
   AGT has active work items for TS crypto/prekey support, but no committed
   delivery date.
2. **Port X3DH/Double Ratchet to TS ourselves and upstream.** 3–6 weeks of
   careful work; subtle crypto; ongoing maintenance burden. **Highest-leverage
   contribution we could make to AGT if we want this migration accelerated.**
3. **Python sidecar in the sandbox (viable — Python is already present).**
   Sandbox base image ([`sandbox-images/openclaw/Dockerfile`](../sandbox-images/openclaw/Dockerfile))
   already includes Python. A local helper process can own X3DH + Double
   Ratchet state and expose a localhost RPC (stdio or Unix socket) to the
   Node agent. Avoids reimplementing crypto in TS. **But:** cipher
   incompatibility (section 4) means this isn't a drop-in bridge to the
   current relay — it only works as part of a coordinated cutover where the
   other side also speaks AGT's ChaCha20-Poly1305 envelope.
4. **Use the existing vendored TS SDK as the crypto core, swap only relay/registry for AGT equivalents.** Non-starter: AGT has no relay (Gap B) and AGT registry uses a different identity/wire format (Gap C).

### Gap B — No store-and-forward relay

AGT services (API gateway, trust, policy, audit, registry) are all stateful
governance services. There is no WebSocket relay with an offline inbox.

**Nuance:** our current offload flow is largely *proactive* — the sandbox
sends `offload_hello` once ready, so the parent-to-child boot-time rendezvous
can often be handled by controller state + retry + ack rather than a durable
relay queue. But general offline peer messaging (any agent sends to any
agent, whenever) still needs durable delivery.

Options if we had to do without a relay:

- **Application-level retry** (we already do some: 8×2s / 5×1s in SDK patch
  6). Extending this far enough to cover any plausible downtime adds
  complexity and still has a delivery-upper-bound problem.
- **External message queue** (Azure Service Bus / Redis Streams / NATS,
  optionally with KEDA). AKS has the primitives, but this is **not cheap
  in AzureClaw's threat model**: the sandbox egress-guard blocks direct
  egress from UID 1000, so the MQ has to be router-proxied; multi-tenant
  queue isolation, per-sandbox RBAC, DLQ/retention/residency, and ciphertext-
  at-rest compliance all become new operational surface.

### Gap C — Identity and wire-protocol divergence

| Axis | Vendored (amitayks) | AGT |
|---|---|---|
| Identity | `amid` = base58(sha256(ed25519\_pub)[:20]) | Python SDK: `did:mesh:<unique-id>`; TS SDK: `did:agentmesh:<agentId>:<fingerprint>` (**AGT itself has internal divergence**) |
| Key discovery | Registry `/v1/registry/prekeys/{amid}` returns X3DH bundle | Registry returns agent metadata; no prekey endpoint at parity |
| Relay frame | WebSocket + tagged JSON (`Connect`/`Send`/`Receive`/`Knock`/…) | n/a (no relay) |
| Session cipher | **XSalsa20-Poly1305 (NaCl SecretBox, no AAD)** | **ChaCha20-Poly1305 with AAD** |
| Auth to registry | Ed25519 signature over raw ISO-8601 timestamp | SPIFFE/SVID mTLS + bearer tokens |
| Session proto | X3DH + Double Ratchet (Signal-style) | X3DH + Double Ratchet (Signal-style) — same family, incompatible envelope/cipher |

**The cipher difference is not academic.** Vendored uses NaCl SecretBox
(no AAD field); AGT uses ChaCha20-Poly1305 with AAD binding agent DIDs into
the ciphertext. There is no wire-level interop path even between matching
SDK versions — any bridging requires re-encryption at a translator, or a
coordinated cutover with both sides on the same cipher.

Migrating means **all deployed agent identities rotate** (AMID→DID),
**every encrypted session restarts** (no in-flight rollover), the relay
client is rewritten to speak AGT's transport, registry calls move to a
different auth model, and AGT's own TS/Python DID inconsistency needs to
converge (or be papered over by us).

## 6. What a migration would actually cost

Two scopes, very different numbers:

### Scope A — Bridge-only prototype (Python sidecar for crypto, keep current relay)

| Workstream | Effort |
|---|---|
| Python helper in sandbox (X3DH + Ratchet state, localhost RPC) | 1–2 weeks |
| TS adapter + chunking glue | 1 week |
| Interop tests (against AGT Python peer) | 1–2 weeks |
| Operator health checks + supervision | <1 week |
| **Total** | **~2–6 weeks** for a hardened pilot |

This is a spike, not a migration. It proves the Python-bridge path works but
doesn't retire anything.

### Scope B — Full migration (retire amitayks SDK + relay + registry)

| Workstream | Engineering | Risk |
|---|---|---|
| TS SDK: port / wrap E2E crypto (if not shipped upstream) | 3–6 weeks | High (crypto correctness) |
| Introduce internal transport/crypto abstraction in AzureClaw first | 1–2 weeks | Low (enables everything below) |
| TS SDK: KNOCK + prekey + session-cache plumbing on top of AGT transport | 2–3 weeks | Medium |
| Identity translation + dual-write (AMID ↔ DID) during cutover | 1–2 weeks | Medium |
| Replace relay: AGT registry + external MQ wiring (router-proxied from UID 1000, RBAC, DLQ) | 3–4 weeks | Medium-high (new operational surface) |
| Replace registry (schema + identity rotation for existing agents + external home agents) | 2–3 weeks | Medium |
| Plugin rewrite: `cli/src/plugin.ts` + `mesh-plugin/src/connection.ts` | 2 weeks | Medium |
| Router: adjust `mesh.rs` fallback HTTP path | <1 week | Low |
| Crypto/protocol interop testing (cipher change, AAD binding, replay/ordering) | 2 weeks | Medium-high |
| Security review of bridge + transport glue | 1 week | Medium |
| E2E + load tests + patch-equivalence coverage | 2 weeks | Medium |
| Home-agent / external-peer migration tooling | 1 week | Medium (blast radius) |
| Documentation + operator migration runbook | 1 week | Low |
| **Total engineering** | **~22–30 engineering-weeks** | — |
| **Calendar time** (with review cycles, dual-stack period, rollback planning) | **~4–6 months** | — |

The earlier version of this section estimated 13–18 weeks; that undercounted
dual-stack cutover, external agent migration, MQ operational hardening, and
the security-review lane. Plus ongoing risk that AGT's TS/transport/relay
story is still "Public Preview" and may drift under us during implementation.

## 7. What migrating would buy us

- **Retire 19 vendored patches** (we maintain them today).
- **Single upstream** for the whole governance + messaging stack. Today we
  couple AGT (router governance) with amitayks (TS messaging) — two
  upstreams to track.
- **Stronger identity story** — SPIFFE/SVID, sponsor accountability, 15-min
  credentials are features we'd otherwise have to build.
- **Compliance artefacts** (EU AI Act / SOC2 / HIPAA / GDPR mapping) are
  first-class in AGT but bolted-on for us.
- **Closer alignment with Microsoft's public agent-governance posture** —
  same rationale as the [Upstream Alignment](upstream-alignment.md)
  argument for OpenClaw.

## 8. What migrating would cost us

- Significant engineering (section 6).
- **New operational surface** — an external MQ (Redis / NATS / Service Bus)
  to replace store-and-forward.
- **Loss of patches we rely on today** unless upstreamed or reimplemented
  (e.g., `wsFactory` for Node.js 22 HTTPS_PROXY — none of AGT's transports
  has a public equivalent).
- **Identity rotation** for any deployed agents.
- **Risk of upstream churn** during implementation (3.x → 4.x moves,
  Public-Preview-to-GA breakages).

## 9. Readiness scorecard

| Capability | AGT readiness | Blocker? |
|---|---|---|
| Governance (router, Rust crate) | ✅ **GA-ready for us — already shipped** | No |
| Trust scoring | ✅ Richer than amitayks | No |
| Audit & compliance | ✅ Exceeds | No |
| E2E encryption (TS runtime) | ❌ Python-only | **Yes** |
| Store-and-forward relay | ❌ Not in product | **Yes** |
| Registry (TS client) | 🟡 Exists, different schema/auth | Partial |
| KNOCK-style intent protocol | ❌ Not in product | Partial |
| Operator UX parity (heartbeat/presence visible to our controller) | 🟡 Possible via AGT, requires adapter | Partial |

**Overall: not yet drop-in replaceable.** The messaging layer is the soft
spot.

## 10. Recommendation

**Stay on the vendored amitayks fork for messaging. Keep AGT for governance.**
Revisit when AGT publishes — on a **milestone basis, not a timer**:

1. A TypeScript SDK with `SecureChannel` / X3DH / Double Ratchet at parity
   with the Python SDK, **and**
2. Either a store-and-forward relay service, or a reference architecture
   for pairing AGT mesh with an external MQ, **and**
3. A reconciled DID format across Python and TS (today AGT Python uses
   `did:mesh:...` while AGT TS uses `did:agentmesh:...:...`).

### Strategic framing

- **Case against migrating now:** AGT messaging is exactly the piece we
  need most, and is also AGT's *least* mature area. A migration today
  means running AGT governance + a Python crypto helper + an external MQ
  + identity translation + a custom cipher-envelope adapter. That is
  *more* architectural sprawl than status quo, not less.
- **Case for medium-term migration:** single upstream for the whole
  governance + messaging stack; retire 19 vendored patches; stronger
  identity story (SPIFFE/SVID, sponsor accountability, 15-min credentials);
  first-class compliance artefacts.

### Highest-leverage work to do now

- **Upstream our 19 patches to `amitayks/agentmesh`.** The repo is active;
  most patches are bug fixes that stand on their own merits. Eliminating
  the fork without migrating technology is a much cheaper win than full
  AGT adoption, and it's purely additive.
- **Contribute TS crypto to AGT** if we want to accelerate AGT readiness.
  A TS port of AGT's Python X3DH/Ratchet is the single highest-leverage
  contribution to make future migration viable. If we do this, we own
  the contribution that unblocks ourselves.
- **Harden the vendored relay** against the two known pain points (AKS
  deployment quirks, HTTPS_PROXY CONNECT tunnelling) rather than rewriting.

### Phased path if/when migration becomes viable

Reordered from the original draft — **registry-first is the wrong starting
point** because current session establishment is tightly coupled to the
current registry/prekey behavior, and identity/cipher changes are unresolved.

1. **Phase 0 — prep (already done):** governance on AGT via Rust crate.
2. **Phase 1 — internal abstraction:** introduce a transport/crypto
   interface in AzureClaw's TS code so the concrete SDK can be swapped
   without touching plugin call sites. Behavior-preserving refactor.
3. **Phase 2 — crypto spike:** prototype Node↔Python crypto bridge (or TS
   port) and prove cipher/envelope interop against a real AGT peer.
4. **Phase 3 — identity translation:** dual-write AMID ↔ DID so existing
   deployments keep working during cutover. Rotate home-agent and
   external-peer identities on a schedule, not a flag day.
5. **Phase 4 — registry cutover** behind the abstraction, running both
   registries in parallel.
6. **Phase 5 — relay replacement** (AGT transport + external MQ for offline
   delivery, router-proxied so UID 1000 can reach it).
7. **Phase 6 — cleanup:** remove `vendor/agentmesh-*`, delete the 19 patches.

## 11. Risk register — things to watch during any migration

- **Crypto interop risk.** XSalsa20-Poly1305 (vendored) vs ChaCha20-Poly1305
  (AGT) with AAD means no wire-level rollover. Any cutover is a hard flip
  per session, with a translator if we need to span both worlds during
  transition.
- **External-peer blast radius.** Identity rotation isn't just in-cluster —
  it affects home agents (`~/.azureclaw/mesh-identity.json`), stored
  pairings, and any third-party peer that has our AMID pinned. Rekey
  tooling + a grace period with both IDs valid are prerequisites.
- **Ciphertext-at-rest compliance.** If we replace the relay with an
  external MQ, even though payloads are E2E-encrypted, durable ciphertext
  at rest creates retention / purge / residency / DSR obligations the
  current store-and-forward model sidesteps (relay TTL + in-cluster only).
- **Python bridge ops burden.** A sandbox-local Python helper adds
  supervision (restart policy), health checks, state persistence for
  session keys, seccomp/capability carve-outs, and a second debuggable
  surface per sandbox.
- **AGT upstream churn.** AGT agent-mesh is Public Preview; version 3.2.0
  is the current TS SDK. Breaking changes remain possible pre-GA. Pinning
  is mandatory during any migration window.
- **AGT's internal DID divergence.** Python `did:mesh:...` vs TS
  `did:agentmesh:...` is unresolved upstream. Pick one and translate —
  but be ready to retranslate if AGT converges on the other.

## 12. If we could influence AGT — proposed upstream roadmap

Since AGT is also Microsoft, this section assumes we have design input.
The question flips: instead of "how do we bend to AGT?", it becomes "what
should AGT ship so that AzureClaw and every other AGT-mesh adopter wins?"

The core observation driving this section: **AGT's strengths are
governance, trust, identity chain-of-custody, and compliance mapping.
Its weaknesses are exactly where amitayks/agentmesh + AzureClaw are
strong — TS runtime, E2E crypto, relay transport, and operator UX.**
A merge of the two codebases is genuinely more capable than either alone.

### 12.1 Guiding principles for a merged design

1. **Protocol first, SDK second.** Ship a wire spec (envelope, cipher,
   prekey bundle, KNOCK frame) before a second SDK. Today both amitayks
   and AGT ship code without a stable protocol document — that's why the
   Python/TS divergence inside AGT was possible.
2. **One identity, everywhere.** Pick a single DID format and retire the
   others. Don't paper over divergence in SDKs.
3. **Crypto in TS and Rust is non-negotiable for enterprise adoption.**
   Python-only blocks every Node/Rust host runtime — which is most of
   them (OpenClaw, MCP servers, VS Code extensions, Copilot clients,
   Cloudflare Workers, Deno, Bun).
4. **Relay is table stakes for multi-agent systems.** Agents go offline.
   Agents boot slowly. Agents crash. A serverless store-and-forward
   layer is not an add-on — it's a core service.
5. **Separate governance decisions from transport bytes.** Governance
   layers (policy, trust, audit) should be consumable *around* the
   transport, not folded into it.

### 12.2 Concrete proposals — where AGT should move toward us

> **Framing note.** The amitayks `agentmesh` repo and our `vendor/`
> fork of it are external community code under different ownership.
> "Adopt" below means *adopt the design intent + published behavior*,
> not *paste the source*. Everywhere it matters, the deliverable is a
> clean-room AGT implementation written against the D1 wire spec, with
> amitayks + our patches as prior art, reference behavior, and interop
> test corpus.

| # | Proposal | Rationale | Implementation path |
|---|---|---|---|
| 1 | **Adopt AMID as the canonical agent ID** (`amid` = base58(sha256(ed25519\_pub)[:20])) and deprecate `did:mesh:...` / `did:agentmesh:...:...` | Shorter, pubkey-bound, self-verifying (anyone can recompute from the key, no registry round-trip), already implemented in two languages, already has a DID-document endpoint (`/v1/registry/did/{amid}`) for W3C interop when needed | AGT adds AMID derivation to identity module; emits `amid:` URIs everywhere; existing DID strings become a legacy compatibility shim behind a feature flag. |
| 2 | **Ship first-party AGT relay + registry services** — either a clean-room re-implementation informed by the amitayks design, or a fresh implementation that improves on it. Use `amitayks/agentmesh` + our 4+4 vendored patches as *reference material* and a known-working test vector suite, not a code-adoption target. | AGT can't unilaterally adopt a third-party community repo under different ownership. But there is a clear design gap (no first-party transport) and a battle-tested reference implementation to learn from. A clean-room re-implementation can (a) be native to AGT's governance hooks, (b) avoid carrying the legacy wire quirks that forced our 4+4 patches, (c) ship under a license AGT controls. | New crates `microsoft/agent-governance-toolkit/packages/agentmesh-relay/` + `agentmesh-registry/` written fresh against the D1 wire spec; amitayks + AzureClaw patches cited as prior art and used as interop tests. |
| 3 | **Ship a first-party AGT TS crypto SDK** (`@microsoft/agentmesh-sdk-crypto`) with full X3DH + Double Ratchet + KNOCK + prekey management — written against the D1 wire spec. Our 11 vendored patches + bug write-ups become reference documentation and interop tests, not a code-adoption target. | Closes the single biggest gap today (AGT TS has *no* crypto). A clean-room implementation aligned with the D1 spec can be: shipped under a license AGT controls, free of the layering quirks that forced our 11 patches, and interop-tested against both the amitayks SDK and our vendored one. | New package in `microsoft/agent-governance-toolkit/sdks/typescript-crypto/`; the current governance-only TS SDK can be renamed `@microsoft/agentmesh-sdk-governance`. AzureClaw contributes every patch rationale as an interop test case. |
| 4 | **Publish a stable wire protocol spec** (`AGENTMESH-WIRE-1.0.md`) covering: envelope tags (`Connect`/`Send`/`Receive`/`Knock`…), cipher suite, AAD binding, prekey bundle format, KNOCK intent schema, sequence/replay rules | Today there is *no* published wire spec. Makes polyglot implementation tractable and interop testable. | Write-once; stabilize; version via header field. |
| 5 | **Unify on a single cipher suite with AAD** | Vendored uses XSalsa20-Poly1305 (no AAD); AGT Python uses ChaCha20-Poly1305+AAD. ChaCha+AAD is stronger. This is a breaking wire change — do it once as part of the D1 spec cut. | Spec v1.0 = ChaCha20-Poly1305 + AAD with AAD = `sha256(from_amid ‖ to_amid ‖ session_id)`. The new TS crypto SDK (proposal 3) implements this natively. One-time migration, then stable. |
| 6 | **Unify registry schema** at the feature-union of (a) the amitayks REST surface (prekey bundles, reputation, DID docs, succession, revocation, org verification) and (b) AGT's SPIFFE/SVID + lifecycle metadata | The amitayks registry's surface is a good superset to start from — prekey + reputation are table stakes. AGT's contribution: SPIFFE/SVID binding and hash-chain audit on top. | New D2 registry implements the feature-union; existing `did:mesh:` entries migrate via a succession endpoint. |
| 7 | **Keep Ed25519-signed-timestamp auth as the default** (amitayks style) **and make SPIFFE/SVID an enterprise add-on** | Ed25519 + timestamp works everywhere (browser, Node, Rust, Go). SPIFFE requires PKI plumbing that most adopters don't have. | `auth_mode: "ed25519" \| "svid" \| "hybrid"` on both relay and registry. |
| 8 | **Ship a minimal reference-architecture bundle** (Helm chart + Docker Compose) with: relay, registry, AGT policy server, AGT audit collector, optional Postgres, optional Redis — deployable in 5 minutes | Today AGT gives you governance microservices; amitayks gives you transport. Nobody has published the combined package. AzureClaw's `deploy/agentmesh.yaml` is close. | `microsoft/agent-governance-toolkit/packages/agent-mesh/deployments/bundle/`. |
| 9 | **Rust SDK parity** (crypto + transport) so the inference-router lane is TS-free end to end | Today we have to reach into TS to do E2E crypto from Rust. A Rust parity SDK collapses that. | Port via `agentmesh` crate 4.x (already in workspace). |

### 12.3 Where AzureClaw moves toward AGT

In exchange (and because they're good ideas anyway):

- **Adopt AGT's 5-dimension trust scoring as the canonical model.** Retire any
  ad-hoc trust math in the router/plugin.
- **Adopt AGT's hash-chain audit** as the canonical audit format for mesh events,
  with CloudEvents export.
- **Adopt AGT's OPA/Rego policy expression** alongside the existing YAML
  profiles.
- **Adopt AGT's SPIFFE/SVID binding** as the recommended enterprise auth
  mode (keep Ed25519-signed-timestamp as the default).
- **Use AGT's `LifecycleManager` + `CredentialRotator`** for the 15-minute
  ephemeral credential model on workload-identity sandboxes.

### 12.4 Deliverables — AGT-side, in dependency order

| # | Deliverable | Owner (AGT team) | Depends on |
|---|---|---|---|
| D1 | Publish `AGENTMESH-WIRE-1.0.md` spec (covers envelope, cipher, AAD, KNOCK, prekey bundle, identity, registry REST) | Protocol TL | — |
| D2 | Ship first-party AGT relay + registry services (clean-room implementation written against D1 wire spec). Use amitayks codebase + our 4+4 vendored patches as reference + interop test corpus. Release as `agentmesh-relay` / `agentmesh-registry` crates inside `agentmesh-platform`. | Services TL | D1 |
| D3 | Ship first-party `@microsoft/agentmesh-sdk-crypto` (TS). Clean-room implementation against D1 wire spec; use our vendored SDK + 11 patch rationales as reference + interop tests. | SDK TL | D1 |
| D4 | Rust SDK: add `agentmesh::transport` + `agentmesh::crypto` modules at parity with D3 | SDK TL | D1, D3 |
| D5 | Identity convergence: introduce AMID derivation and DID compatibility shim; update all AGT services to accept `amid:` URIs | Identity TL | D1 |
| D6 | Registry schema unification (prekey bundles, reputation, DID, SPIFFE/SVID, lifecycle) + migration tooling | Identity/Services | D2, D5 |
| D7 | Cipher unification: ChaCha20-Poly1305 + AAD across Python / TS / Rust SDKs | Crypto TL | D1, D3 |
| D8 | Auth mode selector on relay + registry (`ed25519` / `svid` / `hybrid`) | Services | D1, D2 |
| D9 | Reference deployment bundle (Helm + Compose) — relay, registry, policy, audit, observability | DevEx | D2, D6 |
| D10 | Governance-in-transport integration: policy hooks in relay (e.g., rate-limit by trust tier, deny by policy) | Governance + Services | D2, D6 |
| D11 | Operator UX: presence panel, trust panel, peer inspector driven by AGT's `GovernanceMetrics` | DevEx | D9, D10 |
| D12 | `agentmesh` CLI (Python) gets `relay start` / `registry start` / `agent up` shortcuts pointing at D2/D9 | DevEx | D9 |

### 12.5 Suggested ordering (AGT's engineering lanes)

```
   D1 (spec) ──┬─── D2 (first-party relay/registry) ──┬── D6 (schema) ──┬── D9 (bundle) ── D11 (UX) ── D12 (CLI)
               │                                    │               │
               ├─── D3 (TS crypto SDK) ─── D7 (cipher unified)       │
               │                                                    │
               └─── D5 (AMID)  ──────────────────── D8 (auth mode) ──┘
                                                    D10 (policy-in-transport) ── D11
                                                    D4 (Rust parity)  ────────── D11
```

### 12.6 Minimum viable merge (MVM)

If AGT can only do a subset, the smallest coherent set that unblocks
AzureClaw — and every Node-hosted multi-agent project — is:

- **D1** (wire spec)
- **D2** (ship first-party relay + registry)
- **D3** (ship first-party TS crypto SDK)

That's three deliverables. Everything else (identity convergence, cipher
unification, Rust parity, UX) is improvement-over-time on top.

### 12.7 What AzureClaw commits in return

If AGT takes up this roadmap, AzureClaw commits to:

- **Contributing interop tests + patch rationales** — our 11 SDK patches,
  4 relay patches, and 4 registry patches become documented reference
  behaviors and test vectors for the first-party AGT implementations.
  We don't expect our code to be adopted verbatim; the *knowledge* is
  what transfers.
- **Writing the wire spec's first draft** — we have de-facto implementations
  on both sides and the clearest picture of what breaks in practice.
- **Running interop conformance** against both AGT's first-party services
  and amitayks's OSS implementation during the transition window.
- **Adopting AGT's governance + identity + lifecycle primitives fully** and
  retiring any parallel implementations on our side.
- **Being a reference deployment for AGT agent-mesh v4.x** — production
  evidence that the new stack works end to end under real isolation
  constraints (seccomp, NetworkPolicy, workload identity).

---

## 13. References

- Vendored stack: [`vendor/agentmesh-sdk`](../vendor/agentmesh-sdk/),
  [`vendor/agentmesh-relay`](../vendor/agentmesh-relay/),
  [`vendor/agentmesh-registry`](../vendor/agentmesh-registry/) (each with
  README listing patches)
- Patch inventory: 11 SDK + 4 relay + 4 registry (section 2 + each README)
- Router usage of AGT: [`inference-router/src/governance.rs`](../inference-router/src/governance.rs)
- AGT mesh: https://github.com/microsoft/agent-governance-toolkit/tree/main/packages/agent-mesh
- AGT TS SDK surface: `sdks/typescript/src/index.ts` (no encryption exports)
- AGT Python crypto: `src/agentmesh/encryption/{channel,x3dh,ratchet}.py`
- Upstream amitayks: https://github.com/amitayks/agentmesh
- In-cluster deployment: [`deploy/agentmesh.yaml`](../deploy/agentmesh.yaml)
