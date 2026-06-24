# Security Audit — Entra Agent ID: don't hang on CA-blocked Graph; reach the Bicep fallback OOTB

PR: Azure/kars (branch `fix/entra-graph-ca-bicep-fallback`)

## Scope

Capability-path changes in `cli/src/commands/mesh/agent_id_setup.ts` and
`cli/src/commands/mesh/agent_id_setup_bicep.ts`.

`kars up --mesh-trust=entra` froze at step 7/9 on a Microsoft-corp tenant. Field
trace + a direct `az deployment sub create` test established the exact behaviour:

1. The Graph REST call hits `AADSTS530084` (Conditional Access token-binding
   block on the az CLI's Graph token).
2. The CLI then ran `az login --use-device-code` to "refresh" the Graph token.
   On tenants whose CA *also* requires a compliant/managed device, the device
   code is never authorised (browser completion returns `AADSTS530033`), so
   `az login --use-device-code` **polls for ~15 min** before failing — a hang.
3. The intended fallback — provisioning the Entra objects via a Bicep
   `Microsoft.Graph`-extension deployment, which reaches Graph through ARM's
   deployment engine and is **not** subject to the CA token-binding policy — was
   only reached after that hang, and then failed to even locate its template in
   an npm-installed CLI because `resolveBicepTemplate()` only checked
   repo-relative paths, never the bundled `dist/deploy/bicep/` copy.

A direct test confirmed the Bicep path bypasses the CA block: the deployment
reached Graph server-side (real Graph request-id, no 530084) and failed only on
the corp-tenant `ServiceManagementReference must be a valid GUID` requirement.

## Changes

- **`agent_id_setup.ts`:** on `AADSTS530084`, **do not** attempt a device-code
  re-login (removed `deviceCodeReloginForGraph`). Print a one-line notice and
  propagate immediately so `ensureAgentIdTrustAutoFallback` falls back to Bicep
  without the ~15-min hang.
- **`agent_id_setup_bicep.ts`:** resolve the Bicep template via the shared
  repo-or-bundled resolver (`requireBundledAsset("deploy/bicep/agent-id-trust.bicep")`)
  so the fallback works from an npm-installed CLI (the template is already
  bundled by `scripts/bundle-deploy-assets.mjs`). Detect the
  `ServiceTree`/`ServiceManagementReference` Graph error and surface an
  actionable hint to pass `--service-tree <GUID>`.

## Threat model

### T1: New asset source / attacker-reachable input? (NO)
No new input or network path. The template is the same in-repo/bundled artifact,
now resolved through the existing audited resolver. The Bicep deployment is the
same `az deployment sub create` already shipped.

### T2: Security-control change? (NO — strictly safer)
Removing the device-code re-login only deletes an interactive auth attempt that
could not succeed on these tenants; it does not weaken auth. Provisioning still
goes through ARM + the `Microsoft.Graph` extension with the operator's own
credentials and requires the same `Agent ID Developer` role. On failure the
caller still degrades to the anonymous mesh tier (unchanged), so the cluster is
never left in a more-trusting state than before.

### T3: Fail-open / hang risk? (REDUCED)
The change removes a denial-of-service-shaped hang (15-min poll) and makes the
CA-blocked path fail fast into the declarative Bicep fallback, which is the
tenant-CA-safe provisioning path. No new fail-open behaviour.

## Verdict

Accept. Fixes a hang and an OOTB template-resolution gap in the
`--mesh-trust=entra` provisioning path; no runtime security control is weakened.
Verified: CLI typecheck + lint (0 errors) + 818 tests (48 mesh tests incl. the
CA-block fast-fall path) pass; the Bicep path's CA-bypass and the ServiceTree
requirement were confirmed against the live tenant via `az deployment sub create`.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
