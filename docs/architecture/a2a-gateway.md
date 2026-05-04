# A2A public-ingress gateway

> Phase 2 S3.5 — closes ADR-0001 #4.

## Why this component exists

Before S3.5 the inference router exposed A2A endpoints on the
cluster-internal mesh only. ADR-0001 #4 required a hardened public
edge so external A2A 1.0.0 callers (other organisations' agents
reaching ours) terminate at a single, narrow surface that:

1. Owns the public TLS certificate (rotated via Application Gateway
   for Containers / cert-manager).
2. Verifies inbound JWS-signed `AgentCard` envelopes against a pinned
   trust store.
3. Enforces per-subject rate limits keyed off the verified JWS
   `sub` claim.
4. Forwards over mTLS to the router on a dedicated port (8445), so
   the router's existing :8443 mesh listener stays unchanged.

## Data flow

```text
                      external A2A 1.0.0 caller
                                │
                       TLS (rotating leaf)
                                │
                                ▼
              ┌──────────────────────────────────────┐
              │  Application Gateway for Containers   │
              │  (Gateway API resource — Helm)        │
              └─────────────────┬────────────────────┘
                                │
                                ▼
              ┌──────────────────────────────────────┐
              │      azureclaw-a2a-gateway            │
              │                                       │
              │  • TLS termination (rustls + ring)    │
              │  • JWS verify (azureclaw-a2a-core)    │
              │  • Replay nonce cache (5 min TTL)     │
              │  • Subject token-bucket (60 burst /   │
              │    5 rps refill, RAM-only in v1)      │
              │  • Drop privs to UID 1002             │
              │  • Distroless static + read-only FS   │
              └─────────────────┬────────────────────┘
                                │
                            mTLS (8445)
                                │
                                ▼
              ┌──────────────────────────────────────┐
              │      azureclaw-inference-router       │
              │      :8445 (mTLS) — gateway path      │
              │      :8443 (mesh) — unchanged         │
              └─────────────────┬────────────────────┘
                                │
                                ▼
                          sandbox pod
```

## Threat model

### In scope (mitigated in S3.5)

| Threat | Mitigation |
|---|---|
| Eavesdropping on the public path | TLS 1.2/1.3 via rustls. |
| Stolen TLS leaf | `notify::Watcher` triggers `Arc<ServerConfig>` swap on cert rotation; old sessions drain. |
| Forged AgentCard | JWS Ed25519 verify in `azureclaw_a2a_core::verify_inbound_card` (library complete & tested) against a pinned trust store; `alg` allow-list is hard-coded to `EdDSA`. **`[GAP-V1]`** the gateway *binary* does not yet run this verifier in its proxy hot path — see "Out of scope in S3.5" below. |
| Replay of a valid envelope | Nonce cache with 5 min TTL and 100k entry cap. |
| Untrusted gateway impersonating router | Router :8445 verifies client cert against the gateway-only CA bundle at `/etc/azureclaw/a2a-gateway-ca.pem`. |
| Burst flood from one subject | Per-subject token bucket (60 burst / 5 rps); over-budget calls return 429. |
| Container escape from the gateway | Distroless static base, read-only root FS, drop ALL caps, UID 1002, seccomp `azureclaw-strict.json`. |

### Out of scope in S3.5

| Concern | Resolution path |
|---|---|
| **`[GAP-V1]` JWS verifier wired as an axum layer** | The verifier is implemented and tested in `azureclaw-a2a-core`; the gateway binary today consumes the verified subject from the `X-A2A-Agent-Subject` header populated by the upstream Gateway API mTLS handshake. Wiring `verify_inbound_card` directly inside the gateway as an opt-in axum layer is a v1.1 task. The unused `azureclaw-a2a-core` workspace dependency in `a2a-gateway/Cargo.toml` is the placeholder. |
| Cross-replica rate-limit sync | Helm value `a2aGateway.rateLimits.sharedRedisUrl` is reserved; impl is `unimplemented!()`. Replicas in v1 enforce in-memory only — the router's downstream limiter is the second line of defence. |
| SAN pinning beyond CA chain on :8445 | The gateway CA is single-purpose (issued only to gateway pods) so chain-of-trust is sufficient for v1. |
| Mandatory mTLS on :8443 | Out of scope — :8443 stays exactly as it is in the dev branch. |

## Surveyed-existing-implementation — what was lifted vs. what is new

The slice deliberately did *not* fork the JWS verifier. Instead:

| Component | Pre-S3.5 location | Post-S3.5 location |
|---|---|---|
| `signature.rs` (RFC 7515 signing-input) | `inference-router/src/a2a/` | `azureclaw-a2a-core/src/` |
| `agent_card.rs` (A2A §5.5 schema) | `inference-router/src/a2a/` | `azureclaw-a2a-core/src/` |
| `card_signing.rs` (Ed25519 sign + verify) | `inference-router/src/a2a/` | `azureclaw-a2a-core/src/` |
| `card_verifier.rs` (inbound caller pin) | `inference-router/src/a2a/` | `azureclaw-a2a-core/src/` |
| `error.rs` (A2A §3.3.2 codes) | `inference-router/src/a2a/` | `azureclaw-a2a-core/src/` |

The router re-exports each module under its original path
(`crate::a2a::signature::*`, etc.) so every existing call site
keeps compiling unchanged. The gateway depends on the new core
crate directly. Both binaries therefore use the same byte-for-byte
verifier.

## Deployment sizing

Default Helm values target a small cluster (≤1k external A2A peers):

| Knob | Default | Rationale |
|---|---|---|
| `replicas` | 2 | HA pair; no shared state required in v1. |
| Per-subject burst | 60 | Comfortably covers card discovery + a few `tasks/send` calls. |
| Per-subject refill | 5 rps | Steady-state ceiling per peer. |
| `maxSubjects` | 50 000 | LRU-evicts when exceeded; bounds RAM at ~10 MB. |
| Resources | 100m / 128Mi → 500m / 256Mi | The gateway is forwarding-only; CPU spikes are TLS handshake-bound. |

For larger surfaces (tens of thousands of distinct peer subjects),
either raise `maxSubjects` or — once available — opt into shared
Redis sync. Both knobs are Helm values so no rebuild is required.

## See also

- `docs/operations/a2a-gateway.md` — operator runbook (enable, cert
  rotation, observability).
- `docs/security-audits/2026-04-30-phase2-a2a-gateway.md` — audit
  doc with explicit tracking of what was lifted from the router.
- `docs/adr/0001-a2a-ingress-front-edge.md` — the ADR.
