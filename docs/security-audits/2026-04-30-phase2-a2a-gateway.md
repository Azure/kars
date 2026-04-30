# Security audit — Phase 2 S3.5 A2A public-ingress gateway

- **Slice**: `phase2-a2a-gateway-component`
- **ADR**: ADR-0001 #4 — A2A ingress front edge.
- **Date**: 2026-04-30
- **Components touched**: new crate `a2a-gateway`, new crate
  `azureclaw-a2a-core`, `azureclaw-inference-router`, Helm chart,
  image-cache CI workflow.
- **CRDs added**: none. Configuration is purely Helm + env (per the
  slice DO-NOT list).

## What changed

1. **Workspace restructure (lift)**. `signature.rs`, `agent_card.rs`,
   `card_signing.rs`, `card_verifier.rs`, and `error.rs` were moved
   from `inference-router/src/a2a/` into a new library-only workspace
   member `azureclaw-a2a-core`. The router re-exports them at their
   original paths (`crate::a2a::signature::*`, etc.) so call sites
   are unaffected. The gateway depends on the new crate directly.

2. **New binary `azureclaw-a2a-gateway`**. axum + rustls + tokio.
   Modules: `tls`, `mtls`, `verify`, `proxy`, `rate_limit`,
   `metrics`, `health`. Distroless static image (musl).

3. **Router-side mTLS port (additive)**. New module
   `inference-router/src/a2a_mtls.rs`. The new port (default 8445)
   is **opt-in via `A2A_MTLS_ENABLED=1`**; default off, so the
   existing :8443 path is byte-for-byte unchanged.

4. **Helm template**. New `templates/a2a-gateway-deployment.yaml` +
   ServiceAccount + Service. Off by default
   (`a2aGateway.enabled: false`).

5. **CI / image cache**. New matrix entry in
   `.github/workflows/image-cache-publish.yml` for the
   `azureclaw-a2a-gateway` image; `a2a-gateway/**` and
   `azureclaw-a2a-core/**` paths added to the trigger filter.

## Surveyed existing implementation — extraction map

| Symbol | Pre-S3.5 path | Post-S3.5 path | Notes |
|---|---|---|---|
| `SignatureInput`, `build_signing_input`, `base64url_*` | `inference-router::a2a::signature` | `azureclaw_a2a_core::signature` | RFC 7515 helper; no behaviour change. |
| `AgentCard`, `AgentCardSignature`, `AgentSkill`, … | `inference-router::a2a::agent_card` | `azureclaw_a2a_core::agent_card` | A2A §5.5 schema; no behaviour change. |
| `sign_card`, `verify_card`, `TrustedKeys`, `CardSignError` | `inference-router::a2a::card_signing` | `azureclaw_a2a_core::card_signing` | EdDSA-only; the `alg` allow-list lock is preserved. |
| `verify_inbound_card`, `CardVerifierConfig`, `VerifiedCallerIdentity`, `CardVerifyError` | `inference-router::a2a::card_verifier` | `azureclaw_a2a_core::card_verifier` | Caller-pin + thumbprint check. |
| `A2aErrorCode`, `A2aError` | `inference-router::a2a::error` | `azureclaw_a2a_core::error` | A2A §3.3.2 codes. |

Everything else under `inference-router/src/a2a/` (`ap2.rs`,
`jsonrpc_dispatch.rs`, `mandate_signing.rs`, `mandate_trust_store.rs`,
`message_send_ap2.rs`, `card_server.rs`, `agent_projection.rs`,
`snapshot_rebuild.rs`, `trust_store.rs`) **stayed in the router**.
These modules either depend on router-private trait seams (provider
SPI) or are consumed only by router routes; lifting them is out of
scope for S3.5 and not necessary for the gateway.

The CI guard `ci/a2a-module-isolation.sh` continues to apply to the
files still under `inference-router/src/a2a/`. The lifted files,
now in `azureclaw-a2a-core`, never imported `crate::auth::*` types
to begin with — moving them out actually *strengthens* the
isolation invariant (they are now mechanically prohibited from
seeing the router's auth types since the auth module is on the
other side of the crate boundary).

## Threat model

| Threat | Mitigation |
|---|---|
| Untrusted external A2A caller forges an `AgentCard` | JWS Ed25519 verify (`azureclaw_a2a_core::card_verifier`); `alg` is hard-coded to `EdDSA`, eliminating RFC 8725 §3.1 alg confusion. |
| Replay of a valid signed envelope | `verify::ReplayCache` — 5-min TTL, 100k entry cap. Cap eviction is by oldest-expiry so the freshest replays are caught even under cache pressure. |
| Burst flood by a single peer | `rate_limit::SubjectLimiter` — token bucket per verified subject (60 burst / 5 rps). |
| Distinct-subject spray | Subject map cap (`maxSubjects=50_000`) with most-stale eviction. |
| Stolen TLS leaf | `tls::spawn_reloader` swaps `Arc<ServerConfig>` on `notify` event; rotation does not require pod restart. |
| Stolen gateway → router client cert | Router :8445 chains to the gateway-only CA bundle. CA can be rotated in lockstep with the leaf. |
| Container escape from the gateway pod | Distroless static base (no shell, no libc), read-only root FS, drop `ALL` caps, run as UID 1002, seccomp profile `azureclaw-strict.json`. |
| Gateway compromise → reach beyond router | Cilium ClusterwideNetworkPolicy `azureclaw-a2a-gateway-to-router` already restricts the gateway SA to router :8445 / POST `/a2a/.+` only. |
| Default-on regression | Helm value `a2aGateway.enabled: false` and router env `A2A_MTLS_ENABLED` defaults to disabled — turning the slice on requires two explicit opt-ins. |

## Cryptography surface

- Sign / verify: `ed25519-dalek` (workspace dep, allow-listed in
  `ci/no-custom-crypto.sh`).
- TLS: `rustls 0.23` with the `ring` provider — workspace-wide
  constraint per the DO-NOT list (no second TLS implementation).
- PEM parsing: `rustls-pemfile`.
- Base64 / hex: `base64`, `hex` (workspace deps).
- Hashing: `sha2` (transitive via `ed25519-dalek`).

No custom crypto framing introduced. The JWS protected/payload byte
construction was moved verbatim from the router.

## Test coverage delta

| Crate | Tests before | Tests after | Delta |
|---|---|---|---|
| `azureclaw-controller` | 399 | 399 | 0 |
| `azureclaw-inference-router` | 623 | 624 | +1 (lib `a2a_mtls` config) |
| `azureclaw-a2a-core` (new) | — | 73 | +73 (lifted from router; round-trip / replay / wrong-issuer / expired-token coverage retained) |
| `azureclaw-a2a-gateway` (new) | — | 31 | +31 (TLS load, mTLS load, replay cache, rate limiter, metrics, health, proxy URL builder) |
| **Workspace total** | **1022** | **1127** | **+105** |

## Residual risks

- v1 of the gateway ships without cross-replica rate-limit sync.
  Mitigated by the router's downstream limiter — a peer that times
  the burst exactly between two replicas can briefly exceed
  per-replica budget, but cannot escape the router-side ceiling.
- `notify` on macOS uses kqueue; the cert-reload test asserts only
  that no panic occurs there (Linux/inotify is reliable).
- Configuration is via env vars rather than a CRD — operators must
  keep the gateway's mTLS Secret and the router's CA bundle in
  sync manually. A future slice could add a CRD if this becomes a
  routine source of misconfiguration.

## Verification commands

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace                 # 1127 tests across 4 crates
helm lint deploy/helm/azureclaw
hadolint a2a-gateway/Dockerfile        # if installed
```

## Sign-off

This audit covers S3.5. The next ADR-0001 follow-up (cross-replica
limiter sync, automatic CA rotation) is tracked in the slice
backlog and explicitly **out of scope** here.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
