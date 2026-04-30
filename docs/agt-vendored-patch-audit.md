# Vendored AgentMesh Patch Audit

**Status:** tracked per AGT SDK / upstream AgentMesh release.
**Enforcer:** `ci/vendored-patch-audit.sh` — fails CI if `vendor/**` or the
AGT SDK version changed without a matching re-audit row below.

This document tracks the patches applied to the vendored AgentMesh components
(`vendor/agentmesh-sdk`, `vendor/agentmesh-relay`, `vendor/agentmesh-registry`).
Per the vendored-patch principle (`ci/vendored-patch-audit.sh`), every patch is
re-audited on each AGT SDK / upstream bump: still required, upstream-absorbed,
or superseded by the `AgtMeshProvider` (once AGT ships AgentMesh).

## Current pins

| Component | Vendored from upstream | Pinned version | Upstream URL |
|---|---|---|---|
| `vendor/agentmesh-sdk` | `amitayks/agentmesh/agentmesh-js` | v0.1.2 | <https://github.com/amitayks/agentmesh/tree/main/agentmesh-js> |
| `vendor/agentmesh-relay` | `amitayks/agentmesh/relay` | v0.3.0 | <https://github.com/amitayks/agentmesh/tree/main/relay> |
| `vendor/agentmesh-registry` | `amitayks/agentmesh/registry` | v0.3.0 | <https://github.com/amitayks/agentmesh/tree/main/registry> |
| `vendor/sandbox-wheels` | (internal Python wheel cache) | N/A | N/A |

`npm`-published equivalent (historical): `@agentmesh/sdk` v0.1.2.
AGT Rust SDK (shipped, consumed by Phase 0): version TBD — roster row to be
added once crate is pinned in Phase 0.

## Patch status — `vendor/agentmesh-sdk`

Source of truth: `vendor/agentmesh-sdk/README.md`. Numbering matches.

| # | Title | Still required? | Notes |
|---|---|---|---|
| 1 | `PrekeyManager.buildBundle()` — empty signature + missing prekeys | **Yes** | Upstream still returns empty sig on v0.1.2. Revalidate on v0.2.x. |
| 2 | `base64Decode` — `x25519:` / `ed25519:` key type prefix crash | **Yes** | Registry still returns prefixed keys; patch needed everywhere we consume keys from the registry. |
| 3 | X3DH → Double Ratchet handoff — missing peer ratchet key | **Yes** | Core Signal-protocol correctness; cannot ship without. |
| 4 | KNOCK protocol — not wired to relay transport | **Yes** | Sessions silently never establish without it. |
| 5 | KNOCK race condition — message before KNOCK accepted | **Yes** | 1–33 ms race window; reproducible under load. |
| 6 | `connect()` prekey/register order — registry requires registration first | **Yes** | Plus sender-side retry in `plugin.ts`. |
| 7 | `submitReputation` — silent error swallowing | **Yes** | Logs only; safe to keep until AGT TrustManager replaces. |
| 8 | `connect()` — stale connected state blocks reconnect | **Yes** | Reconnect-loop correctness; cannot ship without. |

**Known remaining gap** (documented in SDK README): transport `receive`
events are not wired to `AgentMeshClient.onMessage()`. Parent → sub-agent send
works E2E; sub-agent → parent receive needs the sub-agent to drive its own
relay listener in `entrypoint.sh`. Tracked as a `MeshProvider` behaviour to
replicate faithfully under both `VendoredAgentMeshProvider` and the future
`AgtMeshProvider`; the Phase 0 Signal conformance corpus must assert it.

## Patch status — `vendor/agentmesh-relay`

Source of truth: `vendor/agentmesh-relay/README.md`.

| # | Title | Still required? | Notes |
|---|---|---|---|
| 1 | Raw-timestamp signature verification (`chrono::to_rfc3339` `Z` vs `+00:00`) | **Yes** | Interop with SDK's `Date.toISOString()`. Upstream has not adopted. |
| 2 | Session-aware connection management (ghost-connection fix) | **Yes** | Reconnect correctness under network drops. |

## Patch status — `vendor/agentmesh-registry`

Source of truth: `vendor/agentmesh-registry/README.md`.

| # | Title | Still required? | Notes |
|---|---|---|---|
| 1 | Raw-timestamp signature verification (same root cause as relay Patch 1) | **Yes** | Prekey uploads fail 401 without it. |
| 2 | Ghost agent cleanup + heartbeat + search freshness | **Yes** | Sub-agent restart hygiene. |

## Re-audit cadence

- On every AGT SDK version bump.
- On every `vendor/**` file change.
- On every `@agentmesh/sdk` version bump (if we ever upgrade the npm-published
  upstream, which we currently do not, since we overlay the vendored dist).
- At Phase 1 / Phase 2 / Phase 3 close-out, as part of the phase success gate.

On each re-audit: update this file with a new "audited by / date / AGT SDK
version / upstream version" entry at the bottom and mark any patch as
`Upstream-absorbed` or `Superseded by AgtMeshProvider` with a link to the
commit/PR that absorbed it.

## Re-audit history

| Date | AGT SDK | SDK upstream | Relay upstream | Registry upstream | Auditor | Delta |
|---|---|---|---|---|---|---|
| 2026-04-24 | TBD (landing in Phase 0) | v0.1.2 | v0.3.0 | v0.3.0 | *(initial entry — to be co-signed at first Phase 0 PR)* | Baseline; no patches absorbed upstream yet. |
| 2026-04-30 | TBD | v0.1.2 | v0.3.0 | v0.3.0 | Copilot <223556219+Copilot@users.noreply.github.com>, Pal Lakatos-Toth <pallakatos@microsoft.com> | Phase 2 dev → main integration (PR #125). All 8 SDK patches re-verified still required against upstream v0.1.2; no upstream absorption observed. AGT Rust SDK consumption is via path overlays + pinned versions in `Cargo.toml`; no AgentMesh upstream version bump in this integration. Relay + Registry pins unchanged from baseline. |
