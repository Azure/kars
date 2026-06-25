# Security Audit — Idempotent AKS RBAC role assignments + Docker preflight for `--release` (v0.1.15)

Date: 2026-06-25
Scope: `deploy/bicep/modules/sandbox-rbac.bicep` (new), `deploy/bicep/modules/aks.bicep`, `deploy/bicep/main.json` (recompiled), `cli/src/commands/up/preflight.ts`.
Gated path: `cli/src/commands/up/preflight.ts`.

## Summary

1. **Idempotent role-assignment names (the `kars up` blocker).** The AKS module's
   five role assignments named themselves `guid(<resource>.id, ...)` — stable
   across an identity rotation. When the AKS **kubelet identity** rotates (it gets
   a fresh objectId whenever the cluster is recreated, e.g. `kars up
   --from-scratch` after a teardown) or the sandbox UAMI is re-created with the
   same name, the GUID stayed constant but the `principalId` changed, so ARM tried
   to **update** an existing assignment's principal and failed the whole deploy
   with `RoleAssignmentUpdateNotPermitted` ("principal ID … not allowed to be
   updated").

   Fix: a new `modules/sandbox-rbac.bicep` receives the principalIds as **string
   parameters** (legal in a roleAssignment `name`, unlike a runtime
   `reference()` — Bicep BCP120) and names each assignment
   `guid(scope, principalId, roleDefId)`. A rotated identity now yields a new name
   → a clean CREATE instead of a conflicting UPDATE. Every **role, principal, and
   scope is preserved exactly** (kubelet AcrPull @ RG; sandbox AcrPull @ ACR;
   OpenAI User @ AOAI account; KV Secrets User @ Key Vault; MI Contributor @ the
   UAMI). Orphaned old assignments (principal deleted) are harmless.

2. **Docker no longer required for `kars up --release`.** Preflight auto-enabled
   `build=true` whenever a repo Dockerfile was present (developer-mode detection),
   without excluding `--release`. So running `kars up --release` from a clone
   demanded Docker even though release mode only `az acr import`s published images
   (server-side, no local build). Fix: skip the auto-build (and its Docker
   requirement) when `--release` is set.

## T1: New capability / attack surface? (NO)
- No new role, principal, or scope is granted. The set of effective RBAC after a
  successful deploy is identical; only the assignment **resource names** (GUIDs)
  change to be idempotent. The grants remain least-privilege and resource-scoped
  exactly as before.
- The preflight change only *relaxes a tooling requirement* for a mode that never
  used Docker; it touches no deployed resource, credential, or policy.

## T2: Security-control change? (NEUTRAL)
- Roles unchanged: AcrPull (`7f951dda…`), Managed Identity Contributor
  (`e40ec5ca…`), Cognitive Services OpenAI User (`5e0bd9bd…`), Key Vault Secrets
  User (`4633458b…`). Scopes unchanged. `principalType` unchanged. No widening
  (e.g. no move from resource scope to RG/subscription scope).
- `--release` Docker skip does not alter any enforcement; image provenance is
  still the signed GHCR images imported via `az acr import`.

## T3: Availability / fail-open risk? (REDUCED)
- Removes a hard deploy failure (`RoleAssignmentUpdateNotPermitted`) on re-deploys
  over a rotated identity — a pure reliability win. Idempotent CREATE semantics
  mean re-runs converge instead of wedging.
- Removes a false-blocking preflight failure for valid `--release` runs without
  Docker.

## Verification
- `az bicep build --file deploy/bicep/main.bicep` compiles clean (validates the
  new module's `existing`/scope wiring and the BCP120-safe names);
  `deploy/bicep/main.json` recompiled in sync.
- CLI `tsc --noEmit` + oxlint clean; 821 vitest tests pass.
- Module file is bundled into the npm package (`dist/deploy/bicep/modules/sandbox-rbac.bicep`)
  so `az deployment … --template-file main.bicep` resolves it for a clean install.

## Verdict
Accept. Makes AKS RBAC re-deploys idempotent (fixes a hard `kars up` failure) with
zero change to the effective permission set, and stops `--release` from demanding
Docker it never uses. No security control is weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
