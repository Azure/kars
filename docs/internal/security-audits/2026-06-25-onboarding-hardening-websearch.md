# Security Audit — OOTB onboarding hardening + keyless web search (v0.1.16)

Date: 2026-06-25
Scope:
- `cli/src/commands/up.ts` (pre-deploy ARM validate gate, `--skip-validate`, `--yes`, stepper count)
- `cli/src/commands/up/preflight.ts` (non-interactive mode; Docker preflight false-negative)
- `cli/src/commands/up/images.ts` (customer-mode mesh `-agt` tag + required-failure gate)
- `cli/src/commands/up/agentmesh_deploy.ts` (surface mesh-readiness timeout instead of swallowing)
- `cli/src/commands/up/sandbox_bringup.ts` (free-port WebUI forward, token-read retry, dangling-Bing-connection self-heal)
- `cli/src/commands/dev.ts` (free host port for the dev container WebUI)
- `cli/src/commands/mesh/agent_id_setup_bicep.ts` (service-tree error classifier)
- `cli/src/stepper.ts` (numerator clamp)
- `runtimes/openclaw/src/core/agt-tools/foundry.ts`, `runtimes/openclaw/src/core/agt-task-loop.ts`
  (`foundry_web_search`: classic key-based `bing_grounding` → keyless managed `web_search`)
- `ci/bicep-rbac-idempotency.py` (new), `.github/workflows/ci.yml` (static RBAC gate)
- `deploy/bicep/modules/acr-pull-assignment.bicep` (new), `deploy/bicep/standalone/controller-acrpull.bicep` (+recompiled `.json`)

Gated paths (per CI `security-audit-required`): `cli/src/commands/*`, `runtimes/openclaw/src/core/*`.

## Summary

A batch of first-run ("out-of-the-box") reliability fixes plus a credentials-posture
improvement for web search. None grants a new role, scope, or principal; the only
identity/credential change **removes** an API-key code path in favour of the keyless,
Entra/IMDS-authenticated managed tool.

1. **Pre-deploy ARM validate gate.** `kars up` runs `az deployment group validate`
   before creating resources. Read-only; fails fast on template/param/RBAC issues
   with no side effects. `--skip-validate` opts out.

2. **Non-interactive `--yes` / no-TTY.** Preflight skips `inquirer` prompts when
   `--yes` or no TTY. Only affects prompting; all values still come from flags +
   defaults. No control is bypassed.

3. **Docker preflight false-negative.** `--release` (Docker not required) now probes
   `docker --version` (binary) instead of `docker info` (needs a running daemon).
   `--build` still uses `docker info`. No security impact — Docker is a local
   tooling check, not a deployed control.

4. **Customer-mode image import correctness + fail-closed.** Mesh images now import
   with the `-agt` suffix the manifest references (was silently wrong → ImagePullBackOff),
   and a **required**-image import failure now aborts instead of being swallowed.
   This *strengthens* integrity (no partial/mismatched deploy reported as success).
   Image provenance is unchanged (`az acr import` of the same sources).

5. **Mesh-readiness timeout surfaced.** `agentmesh_deploy` previously swallowed a
   180s readiness timeout and reported success; now it warns. Pure observability;
   no control change.

6. **WebUI port-forward free-port + token retry (`kars up`/`kars dev`).** Auto-pick a
   free localhost/host port instead of hard-binding 18789; retry the gateway-token
   read with backoff. Loopback/localhost only; no new exposure (same per-pod gateway,
   same token gate).

7. **Dangling Grounding-with-Bing connection self-heal.** When `--foundry-endpoint`
   is set, `kars up` removes project `GroundingWithBingSearch` connections whose
   backing `Microsoft.Bing/accounts` resource no longer resolves (e.g. its RG was
   deleted). Advisory, idempotent, never aborts the deploy, and never touches a
   connection whose Bing resource is still alive. Removing a connection only deletes
   a stale pointer (and its now-orphaned stored key reference) — it grants nothing.

8. **service-tree error classifier.** Only improves the Entra Bicep-fallback error
   message (no longer tells the user to pass `--service-tree` when they already did).
   No behavioural change to provisioning.

9. **Keyless web search (credentials posture — the notable change).**
   `foundry_web_search` previously sent `tools=[{type:"bing_grounding",
   bing_grounding:{search_configurations:[{project_connection_id}]}}]` and
   auto-discovered a `GroundingWithBingSearch` connection — a path that depends on an
   **API key** stored in a Foundry connection. It now sends `tools=[{type:"web_search"}]`,
   the Microsoft-**managed** Bing tool that authenticates with the router's existing
   Entra/IMDS token (`ai.azure.com` audience) and needs **no Bing API key, no
   user-created Bing resource, and no project connection**. This *reduces* the
   credential surface and aligns with kars's no-API-keys principle.

## T1: New capability / attack surface? (NO / REDUCED)
- No new RBAC role, principal, or scope. The effective permission set after a deploy
  is unchanged (RBAC items here are the already-audited v0.1.15 set; only the
  `controller-acrpull` standalone helper is re-expressed via the idempotent module).
- The web-search change **removes** a key-based code path; nothing new is reachable.
- The Bing self-heal only **deletes** a dead connection; it cannot create resources,
  roles, or connections.

## T2: Security-control change? (NEUTRAL / IMPROVED)
- Content Safety, Prompt Shields, egress allowlist, NetworkPolicy, seccomp, token
  budget, AGT governance — all unchanged. `web_search` still flows through the same
  router inference path and governance gates as before (the router posts to the same
  Foundry endpoint with the same Entra/IMDS auth).
- Required-image fail-closed and managed-Bing keyless auth are net integrity/credential
  improvements.

## T3: Availability / fail-open risk? (REDUCED)
- Validate gate, free-port forwards, token retry, mesh-readiness warning, customer-mode
  fail-closed, and the Bing self-heal all remove confusing OOTB failure modes or make
  them explicit. The validate gate and self-heal are read-mostly and never block on
  their own optional failures (`.catch` advisory).
- `--yes`/no-TTY makes `kars up` scriptable without weakening any check.

## Verification
- `python3 ci/bicep-rbac-idempotency.py` → OK (8 role assignments / 12 files, all
  include a principalId). `az bicep build` of `main.bicep`, the standalone helper,
  and the new module all compile; `controller-acrpull.json` recompiled in sync.
- CLI: `tsc --noEmit` clean, oxlint 0 errors, **821 vitest tests pass**.
- Runtime (`runtimes/openclaw`): `tsc --noEmit` clean, oxlint 0 errors, **244 tests pass**.
- Keyless `web_search` proven against a live Foundry project (`/openai/v1/responses`
  and `/openai/responses?api-version=2025-11-15-preview`): HTTP 200 with real,
  cited results and a `web_search_call` step — with no Bing connection present.

## Verdict
Accept. A batch of OOTB reliability fixes with no expansion of roles, scopes, or
network exposure; the one credential-relevant change **removes** an API-key path in
favour of keyless Entra/IMDS-authenticated managed web search. Net posture: neutral-to-improved.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
