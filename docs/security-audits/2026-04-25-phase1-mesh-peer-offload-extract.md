# Phase 1 — `controller/src/mesh_peer.rs` decomposition

**Date:** 2026-04-25
**Slug:** `phase1-mesh-peer-offload-extract`
**Branch:** `phase1/mesh-peer-offload-extract`
**Capability author:** Pal Lakatos-Toth
**Independent reviewer:** Pal Lakatos-Toth (Phase 1 single-reviewer carry-over;
see `docs/security-audits/2026-04-25-phase1-spawn-docker-extract.md` §11)

## 1. Summary

Decomposes `controller/src/mesh_peer.rs` (1970 LOC) — controller-side
AgentMesh relay peer that handles pair / offload requests — into a module
directory `controller/src/mesh_peer/` with three files:

- `mod.rs` (1170 LOC) — identity (Ed25519), lease acquisition, run loop,
  WebSocket connect/listen, message dispatch (`handle_message`,
  `handle_peer_message`), helpers (`hex_sha256`, `send_to_peer`,
  `enqueue_outbound`, `serialize_and_send_outbound`), unit tests.
- `offload.rs` (718 LOC) — offload orchestration: `handle_offload_request`,
  `handle_offload_cleanup`, `watch_sandbox_ready`, `annotate_ready_sent`,
  `resume_pending_offload_watchers`, `validate_pairing_for_offload`.
- `pair.rs` (124 LOC) — pair-request handling: `handle_pair_request`,
  `pair_error`.

After the move `mod.rs` is **under the Phase 1 LOC cap of 1200** (1170/1200);
`offload.rs` is **under the new-file cap of 800** (718/800); `pair.rs`
trivially under cap. The Phase 0 monotonic-decrease budget on
`controller/src/mesh_peer*` is honoured: 1970 → 1170 (-800).

This change is **structure-only**. No protocol behaviour, no handler signature,
no exported symbol, no message wire-format, no K8s API call site, no audit
event, and no security boundary is altered. Tests, clippy, and all six CI
gates pass.

## 2. Threat model delta

None. STRIDE surface unchanged: same Ed25519 identity material in the same
secret namespace; same WebSocket peer; same pairing-token verification path;
same offload-request validation against `ClawPairing` CRDs; same outbound
send. The split keeps the public module surface (`pub async fn run`,
`pub async fn load_or_create_identity`) identical; submodule items are
`pub(super)` only and do not widen visibility outside `mesh_peer`.

Rust's "descendants see their ancestors' private items" rule lets `offload.rs`
and `pair.rs` use `MeshPeerState`, `FederationMessage`, `IDENTITY_NAMESPACE`,
`hex_sha256`, `pair_error`, `send_to_peer`, `enqueue_outbound` directly via
`use super::{...}` without elevating any visibility. Crate-level callers
(`controller::main`) see exactly the same surface they did before.

## 3. OWASP mapping

- **MCP-Top10 / Tool-Poisoning:** unchanged — pair-request still rejects
  unknown / expired / consumed tokens with `pair_error(...)` in `pair.rs`.
- **MCP-Top10 / Excessive-Trust:** unchanged — offload-request still
  validates `ClawPairing` phase, AMID binding, capabilities, slot/budget caps
  in `offload.rs` (`validate_pairing_for_offload`).
- **LLM-Top10 / LLM02 Insecure-Output / LLM07 Insecure-Plugin:** N/A
  (controller plane, not inference path).

## 4. AuthN / AuthZ path

Unchanged. Pair: out-of-band pairing token (SHA-256-hashed, compared against
`ClawPairing.spec.tokenHash`). Offload: requires bound AMID + active pairing
(`pair::handle_pair_request` → `offload::validate_pairing_for_offload`).
AGT outage modes are unaffected (no provider call paths touched).

## 5. Secret + key custody

Controller Ed25519 secret (`mesh-peer-identity` in `azureclaw-system`) is
loaded only inside `mod.rs::load_or_create_identity` exactly as before.
Submodules receive a borrowed `&MeshPeerState` and never access the secret
directly. Agent (UID 1000) cannot read the secret — namespace + RBAC
unchanged.

## 6. Egress surface delta

None. Outbound traffic still leaves the controller pod via the same
WebSocket to the AgentMesh relay. No new endpoints, no new DNS, no new IP
pinning.

## 7. Audit events emitted

Unchanged. Same `tracing::warn!` / `tracing::error!` events at the same call
sites with the same fields (`pairing`, `phase`, `from`, `amid`,
`display_name`).

## 8. Failure mode

Fail-closed preserved at every previously-fail-closed site. Pair-request:
unknown / expired / non-pending token → `pair_error(...)`. Offload-request:
invalid pairing → reject with explicit error. K8s API failure on
`patch_status` → still `pair_error("Internal error — could not bind
identity")` (mod.rs → pair.rs identical text).

## 9. Negative-test coverage

Five existing unit tests (in-tree, lifted from the bottom of the original
`mesh_peer.rs` to the bottom of `mod.rs` so they continue to exercise crate-
private items): `derive_amid_is_deterministic`,
`derive_amid_is_base58`, `different_keys_produce_different_amids`,
`sign_timestamp_produces_valid_base64`, `public_key_b64_is_32_bytes`,
`hex_sha256_matches`, `pair_error_response`. All 136 controller-bin tests
pass post-split. Conformance corpus (Signal/X3DH) is not invoked from this
controller code path; it sits on the agent-side `MeshProvider` in plugin/
inference-router.

## 10. Vendored / third-party dependency delta

None. No new crates added. No vendored patch touched. Same
`ed25519_dalek`, `tokio_tungstenite`, `kube`, `k8s_openapi`, `chrono`,
`sha2`, `base64`, `serde_json`. `ci/vendored-patch-audit.sh` clean.

## 11. Sign-offs

Per Phase 0 carry-over policy (single-reviewer permitted for pure structural
moves with zero behaviour delta and zero new attack surface).

Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
