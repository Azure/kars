# Security audit — THIRD_PARTY_NOTICES.txt for vendored upstream (FILE-NOTICE)

**Date:** 2026-04-30
**Finding:** FILE-NOTICE (OSPO 2026-04-28-Azure-azureclaw.md)
**Capability:** Compliance artifact — no capability-introducing code changes.
**Branch:** `phase3/s24-third-party-notices`
**Plan section:** internal Phase 1 plan §0.2 #9 / OSPO finding FILE-NOTICE

## 1. Summary

Generated `THIRD_PARTY_NOTICES.txt` at the repository root to satisfy the
OSPO FILE-NOTICE finding: the repository ships four bundled upstream packages
under `vendor/` with no third-party notice file.

Files added/modified:

- `THIRD_PARTY_NOTICES.txt` — full upstream license texts, copyright notices,
  and patch summaries for all four vendored projects (9 449 lines / ~513 KB).
- `README.md` — added `## Third-Party Notices` heading (4 lines) pointing to
  the file.

No production code paths were modified. The `vendor/` directory itself is
unchanged; only the compliance artifact is new.

## 2. Vendor inventory

### 2.1 agentmesh-registry

| Field | Value |
|---|---|
| Upstream | https://github.com/amitayks/agentmesh/tree/main/registry |
| Version | v0.3.0 |
| Vendored at | `vendor/agentmesh-registry/` |
| License | MIT |
| Patch count | **4** (see `vendor/agentmesh-registry/README.md`) |

Patch summary:
1. Raw timestamp signature verification — keep `timestamp` as `String`; fixes 401 on prekey upload.
2. Ghost agent cleanup + heartbeat + search freshness — replace-on-register, `POST /v1/registry/heartbeat`, 5-minute search window.
3. `feedback_count` always 0 — wrong table name in reputation queries; switched to `sqlx::query_as`.
4. Operational hardening — graceful SIGTERM, stale-agent cleanup task, honest 503 health, input validation caps, TOCTOU fix, startup panic fix.

### 2.2 agentmesh-relay

| Field | Value |
|---|---|
| Upstream | https://github.com/amitayks/agentmesh/tree/main/relay |
| Version | v0.3.0 |
| Vendored at | `vendor/agentmesh-relay/` |
| License | MIT |
| Patch count | **4** (see `vendor/agentmesh-relay/README.md`) |

Patch summary:
1. Raw timestamp signature verification — keep `Connect.timestamp` as `String`; verify raw bytes.
2. Session-aware connection management — `ConnectionEntry` with `session_id`; ghost connection fix.
3. HTTP health endpoint on port 8766 — eliminates K8s tcpSocket probe WebSocket warnings.
4. Explicit close reason error codes — `SESSION_REPLACED` and `PING_TIMEOUT` stop reconnect storms.

### 2.3 agentmesh-sdk (@agentmesh/sdk)

| Field | Value |
|---|---|
| Upstream | https://github.com/amitayks/agentmesh/tree/main/agentmesh-js |
| Version | v0.1.2 |
| Vendored at | `vendor/agentmesh-sdk/` |
| License | MIT |
| Patch count | **11** (see `vendor/agentmesh-sdk/README.md`) |

Patch summary:
1. `PrekeyManager.buildBundle()` — fixed empty signature and missing one-time prekeys.
2. `base64Decode` key-type prefix crash — strip `x25519:`/`ed25519:` prefix before `atob()`.
3. X3DH→Double Ratchet handoff — pass `peerBundle.signedPrekey` as initial ratchet key.
4. KNOCK protocol not wired to relay transport — send KNOCK via `transport.send()`.
5. KNOCK race condition — `knockPendingPeers` Map awaits KNOCK resolution before message processing.
6. `connect()` prekey/register order — reverted to `register()→uploadPrekeys()`; sender-side retry added.
7. `submitReputation` silent error swallowing — log HTTP status + body on non-200.
8. `connect()` stale connected state — check `transport.connect()` return; `disconnect()` before retry.
9. `bytesToBase64` stack overflow on large payloads — `Buffer.toString('base64')` replaces spread.
10. `initiateSession` "Active session already exists" crash — return `{reused:true}` for existing sessions.
11. `wsFactory` + `plaintextPeers` extensibility hooks — proxy-aware WebSocket injection; Signal bypass for legacy peers.

### 2.4 sandbox-wheels (Python wheel cache)

| Field | Value |
|---|---|
| Source | Various upstream PyPI packages |
| Vendored at | `vendor/sandbox-wheels/` |
| Total wheel files | 131 |
| Unique distributions | **104** |
| Unique license groups | **92** |

License distribution:

| License type | Package count |
|---|---|
| MIT (various) | ~40 |
| Apache-2.0 (various) | ~15 |
| BSD-2/3-Clause (various) | ~15 |
| PSF-2.0 | 2 |
| MPL-2.0 | 1 |
| ISC | 2 |
| GPL-2.0 | 1 (Unidecode) |
| Other/mixed | ~28 |

All 104 packages are listed with their individual license texts (or a
reference where the wheel does not bundle the LICENSE file) in
`THIRD_PARTY_NOTICES.txt` section 4.

## 3. Threat model delta

| STRIDE | Applies? | Notes |
|---|---|---|
| Spoofing | No | Compliance text file; no code or auth changes. |
| Tampering | No | No data processing paths modified. |
| Repudiation | No | License attribution is additive only. |
| Information Disclosure | No | No secrets or sensitive data added. |
| Denial of Service | No | No runtime code changed. |
| Elevation of Privilege | No | No permissions or RBAC changes. |

## 4. LOC budget impact

`THIRD_PARTY_NOTICES.txt` is a `.txt` file. `ci/check-loc.sh` only enforces
budgets on `.rs` and `.ts` new files — this file is outside the scope of the
LOC cap mechanism. No budget amendment required.

The four-line README addition touches a budgeted file; the change is
purely additive documentation and does not affect any code path.

## 5. Review notes

- No vendored source files were modified; the `vendor/` tree is unchanged.
- License bodies are reproduced verbatim from wheel `.dist-info/LICENSE`
  files or from upstream METADATA declarations where the wheel omits the
  LICENSE file.
- The MIT license text for `agentmesh-registry` and `agentmesh-relay` is
  the standard MIT template; these repos declare `license = "MIT"` in
  `Cargo.toml` but do not bundle a `LICENSE` file in the tree vendored here.

---

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
