# Security Audit — gate Entra Agent ID Graph preflight on `--mesh-trust=entra`, non-interactively

PR: Azure/kars (branch `fix/preflight-entra-graph-gating`)

## Scope

Capability-path changes in `cli/src/commands/mesh/agent_id_setup.ts` and
`cli/src/commands/up/preflight.ts` (plus the non-capability `cli/src/preflight.ts`):

The `kars up` preflight ran the **Microsoft Graph** "Entra Agent ID directory
role" check **unconditionally**, and on a Conditional-Access Graph block
(`AADSTS530084`) the shared Graph helper launched an **interactive
`az login --use-device-code`** re-login. On tenants whose CA also requires a
managed device for the device-code flow, that prompt can never complete, so
`kars up` **hung in preflight — even with the default `--mesh-trust=anonymous`,
which needs no Graph access at all.**

Two fixes:

1. **Gate the check on trust mode.** Thread `meshTrust` into `PreflightOptions`
   and only run the Entra Agent ID Graph check when `meshTrust === "entra"`.
   The default `anonymous` mode now makes **zero** Graph calls.
2. **Non-interactive in preflight.** Thread an `interactive` flag through
   `azGraphRest`/`azGraphRestWithRetry` and `checkAgentIdRole` /
   `detectExistingBlueprint`. The preflight passes `interactive: false`, so a
   `AADSTS530084` (or `65001/65002`) block propagates to the existing graceful
   handler → a **soft warning**, never a blocking device-code prompt. Default
   callers (the real Entra *setup* phase) keep `interactive: true`, so the
   device-code re-login still happens there, where interactivity is expected.

## Threat model

### T1: Does skipping the Graph check for `anonymous` weaken trust? (NO)
`anonymous` mesh trust never used Entra Agent IDs — the preflight Graph call was
purely advisory and, by design, only relevant to `entra`. Skipping it removes an
unnecessary, hang-prone auth round-trip; it changes **no** runtime trust control.
The actual `entra` setup phase (unchanged) still enforces Agent-ID issuance.

### T2: Does non-interactive preflight hide a real misconfiguration? (NO)
A CA block / missing role still surfaces — as a clearly-worded **warning** with
remediation (`az login --scope https://graph.microsoft.com//.default`, or grant
`Agent ID Developer`). The preflight was already declared "soft-fail (warning,
not blocking)"; this makes it actually behave that way instead of blocking on an
interactive login. The authoritative enforcement remains the setup phase.

### T3: Does the new `interactive` flag change the setup-phase auth path? (NO)
`interactive` defaults to `true`; every existing caller (the `ensureAgentId*`
setup path) is unchanged and still attempts the one-shot device-code re-login on
`AADSTS530084`. Only the two read-only preflight callers opt into
`interactive: false`. A regression test asserts that with `interactive: false`
exactly one `az` call is made (no device-code re-login).

## What this audit does NOT cover

- The Entra Agent ID **issuance / federation** logic (`ensureAgentIdTrust*`) —
  unchanged here; covered by prior Agent-ID audits.
- Tenant Conditional-Access policy itself (operator/tenant responsibility).

## Verdict

Accept. The change removes a preflight hang, makes no Graph calls in the default
`anonymous` mode, and downgrades an `entra` CA-block from a blocking interactive
prompt to a documented warning — without altering the authoritative Entra setup
path. Verified by the full 804-test suite incl. a new non-interactive regression
test; typecheck + lint clean.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
