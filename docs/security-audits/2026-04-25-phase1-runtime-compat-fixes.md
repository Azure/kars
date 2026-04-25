# Security audit — Phase 1 · Runtime-compat fixes (plugin ESM, preflight, policy dir, admin-token migration)

Audit ID: `2026-04-25-phase1-runtime-compat-fixes`  
Scope reference: `docs/implementation-plan.md` §7 (Phase 1 hotspot decomposition + AGT provider production-parity); follow-up to live `azureclaw dev --build` validation against OpenClaw 2026.4.24.

## What landed

Five surgical fixes to make the Phase 0 + Phase 1 cumulative work run end-to-end
against the current OpenClaw release on the `dev` path. No new capability is
introduced; each fix restores a code path that regressed against an upstream
change or surfaces a previously-silent failure.

1. **`cli/src/plugin.ts` — top-level `createRequire` ESM shim** (lines 14–39).
   OpenClaw 2026.4.24's plugin loader (`src/plugins/loader.ts:2766`) calls
   `getJiti(safeSource)(safeImportSource)`. Because `cli/package.json` declares
   `"type":"module"`, jiti loads our compiled `dist/plugin.js` as native ESM,
   in which `require` is undefined. Five legacy bare `require()` call-sites
   threw `ReferenceError: require is not defined` at register time and the
   plugin failed to load. The shim is an IIFE that prefers
   `module.createRequire(import.meta.url)` (real ESM path) and falls back to
   `globalThis.require` (vitest CJS-like execution). All five call-sites
   continue to work with no further change.

2. **`cli/src/plugin.ts` — admin-token attachment migration**.
   * Line 313: legacy `POST /agt/trust` site switched
     `headers["x-azureclaw-admin"] = adminToken` →
     `headers["Authorization"] = \`Bearer ${adminToken}\``.
   * New `_readAdminTokenSync()` helper (~line 3514) reads the same
     priority-ordered token files as the existing async `_readAdminToken`
     (`/tmp/.agt-admin-token`, `/etc/azureclaw/secrets/admin-token`,
     `/run/secrets/admin-token`, then `process.env.ADMIN_TOKEN`). Result is
     cached at module scope to avoid hot-path file reads.
   * `routerCall` (~line 4815): when the target path matches
     `/^\/(agt\/trust|agt\/handoff|admin)\b/` and the caller did not already
     supply an `Authorization` header, the helper auto-attaches
     `Authorization: Bearer <token>`. This fixes the observed `401 denied:
     missing or invalid admin token` from `azureclaw_spawn_destroy` cleanup
     hitting `DELETE /agt/trust/<peer>`. The router's `extract_admin_token`
     (`inference-router/src/routes/mod.rs:220–240`) already supports both
     Bearer and the legacy `x-azureclaw-admin` header (with a one-shot WARN
     via `DEPRECATED_ADMIN_HEADER_WARNED`); this commit migrates the last
     remaining caller-side use of the legacy header.

3. **`cli/src/preflight.ts` — `az rest --url-parameters ""` removed**
   (lines 130–139). Azure CLI 2.78
   (`azure-cli/core/util.py:986`) splits each `--url-parameters` entry on
   `=`; an empty-string entry crashes with `ValueError: not enough values to
   unpack`. The flag is optional. Removing the spurious empty-string arg
   restores the RBAC effective-permissions check; live verification confirmed
   the preflight now correctly identifies missing
   `Microsoft.Authorization/roleAssignments/write`.

4. **Policy directory relocated to `/etc/azureclaw/policies/`**
   (`sandbox-images/openclaw/Dockerfile`, `entrypoint.sh`,
   `inference-router/src/governance/mod.rs`).
   OpenClaw 2026.4.x re-locks `~/.openclaw/` to mode `0700` (UID 1000 only)
   at config-write time. The router runs as UID 1001 (`runuser -u router`),
   so once OpenClaw rewrites the dir the router's `read_dir` returns
   EACCES and our policy hot-reload silently dropped from 10 rules to 0.
   Fix: move policies to a router-readable location owned by `root:root`,
   following the same pattern as the existing `/etc/azureclaw/blocklist/`.
   * `Dockerfile` — pre-creates `/etc/azureclaw/policies` and
     `/etc/azureclaw/blocklist` with mode `0755` before `COPY entrypoint.sh`.
   * `entrypoint.sh` — copies AGT policy YAMLs to `/etc/azureclaw/policies/`,
     `chown root:root`, `chmod 444`. Drops the previous
     `chmod o+x "$OPENCLAW_DIR"` workaround (no longer needed). Removes a
     duplicate hardening block now collapsed into the inline copy.
   * `governance/mod.rs` — defaults in `reload_policies()` and
     `spawn_policy_watcher()`: `/sandbox/.openclaw/policies` →
     `/etc/azureclaw/policies`. `load_policies_from_dir` was
     `if let Ok(entries)` (silent early-return on Err); now `match` with an
     explicit `Err(e) => tracing::warn!(... "Policy reload: read_dir
     failed (perms?)")`. Closes the silent-no-op regression class — every
     future EACCES on the policy dir now surfaces in router logs.

5. **`controller/src/reconciler.rs` defaults & manifests** are unchanged in
   this commit; the policy-dir env override is performed in the sandbox
   entrypoint (`AGT_POLICY_DIR=/etc/azureclaw/policies`).

## STRIDE

| Category | Applies | Note |
|---|---|---|
| **Spoofing** | Positive | Migrating `routerCall` to `Authorization: Bearer` brings the last caller onto the standardised admin path; the router's existing `extract_admin_token` already validates the bearer. The deprecated `x-azureclaw-admin` header still works (one-shot WARN) for any out-of-tree caller, but no in-tree code uses it now. |
| **Tampering** | Positive | The policy-dir relocation moves AGT YAML policies from a UID-1000-writable location (`~/.openclaw/policies`, owned by the agent) to root-owned `/etc/azureclaw/policies/` (`chown root:root`, files `0444`). Agent (UID 1000) can no longer rewrite policy files at runtime. This *raises* the floor; previously the OpenClaw user owned the policies directory between entrypoint chmod and OpenClaw's re-lock. |
| **Repudiation** | Positive | The `read_dir` failure is now logged, not silent. Operators can correlate "policy hot-reloaded rules:0" with a concrete EACCES message and remediate, rather than silently running with empty policies. |
| **Information Disclosure** | N/A | No new outbound surface; no new attribute emission; no PII path touched. The `_readAdminTokenSync()` helper reads the same files as the async variant, no broader read surface. |
| **Denial of Service** | N/A | Cached token read avoids per-call file IO on the hot privileged path. Policy hot-reload remains eventually-consistent. |
| **Elevation of Privilege** | **Yes — mitigated**. Pre-fix: an attacker that compromised the agent (UID 1000) could rewrite `~/.openclaw/policies/*.yaml` between OpenClaw's directory re-lock and the router's next reload, achieving policy bypass. Post-fix: policy files are root-owned `0444`; agent compromise cannot tamper with them. The router only reads them. |

## OWASP mapping

| Item | Control |
|---|---|
| **OWASP LLM Top 10 — LLM06 Insecure Plugin Design** | Plugin's privileged calls now uniformly carry an admin bearer token; no caller relies on a deprecated header that could be silently dropped by a future router refactor. |
| **OWASP LLM Top 10 — LLM07 Insecure Output Handling** | The `read_dir` warn surfaces silent-no-op governance, which previously could allow tool calls to bypass policy without operator awareness. |
| **OWASP MCP Top 10 — M03 Tool Permission Bypass** | Policy hot-reload restored to 10 rules (verified live by re-`chmod`-ing the dir during the dev run). Without this fix, after OpenClaw's `~/.openclaw/` re-lock the router ran with empty policies. |
| **OWASP MCP Top 10 — M07 Integrity** | Policy files moved to root-owned read-only dir; agent cannot tamper. |

## AuthN / AuthZ path

* Plugin → router admin endpoints: `Authorization: Bearer <token>` is the
  one-and-only path used in-tree. Token files are read with priority order
  documented above; cached read on the sync side. Router validates via
  `extract_admin_token` (Bearer preferred, legacy header still accepted with
  a one-shot WARN — used only by potential out-of-tree callers).
* No outage-mode change. AGT/relay auth path untouched.

## Secret + key custody

* Token files (`/tmp/.agt-admin-token`, `/etc/azureclaw/secrets/admin-token`,
  `/run/secrets/admin-token`) — locations unchanged. Permissions and
  ownership unchanged. Only difference: the sync helper now reads them via
  the createRequire shim's `node:fs`.
* Cached-in-memory: yes, in the plugin process scope. The plugin process is
  the OpenClaw agent (UID 1000), which already has read access to the token
  file by definition. No new exposure surface.
* Policy YAMLs: now root-owned `0444`; agent (UID 1000) cannot read-write,
  only read. Router (UID 1001) reads. **Agent loses any ability to modify
  governance policy at runtime — net positive.**

## Egress surface delta

None. No new outbound destinations. No DNS/IP changes.

## Audit events emitted

* `tracing::warn!("Policy reload: read_dir failed (perms?)")` — new event,
  emitted on every failed `read_dir` of the policy directory. Router span;
  no PII; receipt-id semantics unchanged.
* No change to AGT AuditLogger event shape.

## Failure mode

* Policy reload: was *silent allow-zero* (catastrophic); now *logged
  warn + rules counter unchanged on failure*. The reload function returns
  the prior count instead of replacing with 0 — previously the early-exit
  silently overwrote the rule set with empty.
* Admin-token cache: failed reads return `null`; caller falls back to
  attempting the request without auth (existing behaviour). No fail-open
  introduced — the router still returns 401 if no token.
* createRequire shim: failure to obtain a `require` falls back to
  `globalThis.require` (vitest path); if both are unavailable, the IIFE
  throws at module import time, which is louder than a runtime
  `ReferenceError` deep in a callback. **Fail-loud, not fail-silent.**

## Negative-test coverage

* `cli/src/plugin.test.ts` — 85 tests pass post-fix; the suite already
  exercises the `routerCall` privileged-path branch via
  `AZURECLAW_ROUTER_URL=http://127.0.0.1:19876` (immediate ECONNREFUSED) so
  the auth-attach branch is hit.
* `inference-router` test suite — 595 + 15 + 15 + 6 + 26 + 2 + 5 + 3 = 667
  tests pass post-fix (governance, mcp, ap2, proxy edge cases all green).
* Live verification on `azureclaw-dev-agent` container before commit:
  * `/etc/azureclaw/policies/` exists with `root:root` `0444` files.
  * Router log shows `Policy hot-reloaded rules:10` (was 0 pre-fix).
  * No `read_dir failed` warn after the relocation.
  * `azureclaw_spawn_destroy` `DELETE /agt/trust/<peer>` returns 200 (was
    401 pre-fix).
  * Plugin loads on OpenClaw 2026.4.24 (was `ReferenceError: require is
    not defined` pre-fix).

## Vendored / third-party dependency delta

* No new crate or npm package.
* No change to vendored AgentMesh patches; the eight patches in
  `vendor/agentmesh-*/` are untouched. `docs/agt-vendored-patch-audit.md`
  needs no update.
* Upstream OpenClaw `2026.4.24` plugin loader source confirmed via GitHub
  MCP (`openclaw/openclaw` `src/plugins/loader.ts:2766`).

## Principle mapping

* §0.2 #1 — zero regressions: 85 plugin tests + 667 router tests all green.
* §0.2 #2 — AGT boundary: no policy-eval, ratchet, audit-chain, or signing
  code added. Only data-plane / packaging changes.
* §0.2 #4 — LOC: no hotspot grew. `plugin.ts` net change is small
  (~30 lines for the shim, ~25 for the sync helper, ~10 for the
  routerCall branch) and is offset by the planned hotspot decomposition
  in subsequent commits.
* §0.2 #8 — fail-closed: silent-no-op `read_dir` replaced with logged warn
  + retain-prior-rules; admin-token absence still returns 401, not bypass.
* §0.2 #9 — this audit doc.
* §0.2 #10 — references pinned: OpenClaw `src/plugins/loader.ts:2766`
  (verified via GitHub MCP `openclaw/openclaw`); Azure CLI
  `azure-cli/core/util.py:986`; router `inference-router/src/routes/mod.rs:220–240`.
* §0.2 #11 — direct commit on `dev`, not main, per user directive
  ("expand the existing PR — currently we are on dev").

## Re-audit triggers

* OpenClaw plugin loader changes from jiti to a different ESM transform
  → re-verify the createRequire shim is still needed.
* Router's `extract_admin_token` removes legacy header support →
  un-needed; the in-tree migration here makes that removal safe.
* AGT policy schema gains a runtime-mutable field → re-evaluate the
  root-owned-readonly stance on `/etc/azureclaw/policies/`.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
