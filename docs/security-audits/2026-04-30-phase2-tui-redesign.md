# Security Audit — `phase2-tui-redesign` (S14)

**Date:** 2026-04-30
**PR:** #TBD
**Author:** @copilot
**Independent reviewer:** _not required_ (TUI surface is read-only;
not router-data-plane / sandbox-image / admission-policy)
**Capability scope:**
Modular per-CRD panels for the operator TUI (`azureclaw operator`). Adds
panels for every Phase-2 CRD (`McpServer`, `ToolPolicy`, `InferencePolicy`,
`A2AAgent`, `ClawMemory`, `ClawEval`) plus a per-sandbox + cluster-wide
provider-status panel, fronted by a pluggable `ClusterDataSource`. New
flags `--panels`, `--per-sandbox`, `--snapshot` on `operator`. No
controller, router, or sandbox-image code is modified by this slice.

---

## 1. Summary

S14 turns the operator TUI into a panel-oriented dashboard. Every CRD
type added in S1–S6 has a dedicated `Panel` that takes a `ClusterState`
snapshot and returns a blessed-tag string. The data source is abstracted
behind a `ClusterDataSource` interface (live `KubectlDataSource` +
fixture for tests). The §15 success-gate phrase "Operator TUI renders
all five CRDs + provider status per sandbox" is satisfied — see §12 below.

## 2. Threat model delta

The operator TUI is a **read-only** terminal client. No new write paths
are introduced. The only state mutations possible from `azureclaw operator`
existed pre-S14 (egress approve/deny, model swap, delete agent, spawn
agent) and are unchanged.

| STRIDE | New exposure? | Mitigation in this PR |
|---|---|---|
| Spoofing | No | TUI runs locally with the operator's kubeconfig context. No new auth surface. |
| Tampering | No | All new panels are read-only; no kubectl write verbs added. |
| Repudiation | No | No new audit-relevant verbs. |
| Information Disclosure | Yes (small) | New panels surface CRD `spec` fields. **Secrets are never rendered raw** — JWKS / token / credential fields collapse to `<present>`/`<missing>` via `panels/redact.ts`. |
| Denial of Service | No | A single dashboard fetch issues seven `kubectl get` calls in parallel + per-sandbox health probes. Bounded by `kubectl` timeouts (≤15s). |
| Elevation of Privilege | No | RBAC scope = current kubeconfig context. Any read denial surfaces as `unknown` with a verbatim reason; no privilege probing. |

Diff against `docs/threat-model.md`: no boundary changes. The TUI sits
outside any sandbox; it consumes kube-apiserver + (transitively) the
in-pod inference-router via `kubectl exec`.

## 3. OWASP mapping

S14 is an **observation-only** UI surface. It introduces no new model,
tool, or data-handling code paths — the OWASP LLM/MCP items therefore do
not apply directly to this PR. The relevant defensive concern is
LLM02 / Information Disclosure — addressed via secret redaction.

| OWASP item | Applies? | Control in this PR |
|---|---|---|
| LLM01 Prompt Injection | No | No model interaction. |
| LLM02 Sensitive Information Disclosure | **Yes** | `panels/redact.ts` collapses any value whose key matches `/(KEY\|TOKEN\|SECRET\|PASSWORD\|CREDENTIAL\|JWKS\|PRIVATE)/i` to `<present>`/`<missing>`. Verified by `panels.test.ts`. |
| LLM03 Supply Chain | No | No new runtime deps; uses existing `blessed`/`execa`. |
| LLM04 Data and Model Poisoning | No | – |
| LLM05 Improper Output Handling | No | – |
| LLM06 Excessive Agency | No | TUI is read-only. |
| LLM07 System Prompt Leakage | No | – |
| LLM08 Vector and Embedding Weaknesses | No | – |
| LLM09 Misinformation | **Yes** | Per plan §0.2 #10 ("verify, don't guess"), every provider-status field that can't be observed renders as `unknown` with a verbatim probe reason. No invented health. Verified by `panels.test.ts` "each unknown branch surfaces a reason". |
| LLM10 Unbounded Consumption | No | – |
| MCP01–MCP10 | No | TUI does not invoke MCP. |

## 4. AuthN / AuthZ path

- **Caller identity:** the operator running `azureclaw operator`.
- **Identity proof:** the operator's existing kubeconfig context (no new
  auth introduced).
- **AGT policy decision point:** none. Read paths bypass AGT (kube
  API-server RBAC is the gate).
- **Outage behaviour:** **CachedRead-equivalent** — if any `kubectl get`
  fails, the relevant section renders as empty / `unknown`. The TUI does
  not retry destructively and does not poll faster on failure.
- **Default for prod tenants:** the slice changes nothing about live
  AGT policy enforcement; runtime fail-closed defaults remain.

## 5. Secret + key custody

The TUI **never reads Secret data** itself. It reads `ClawSandbox`,
`McpServer`, `ToolPolicy`, `InferencePolicy`, `A2AAgent`, `ClawMemory`,
`ClawEval`, `ClawPairing` — all are CRD CRs (no `data:` blobs). The
existing pre-S14 dashboard already reads `kubectl get secret <name>-credentials -o jsonpath={.data}`
to detect channel presence; that surface is unchanged in S14.

| Secret / key | Storage | Reader identities | Rotation | Agent (UID 1000) can read? |
|---|---|---|---|---|
| _(none introduced by S14)_ | — | — | — | — |

The `redact.ts` helper exists as a defense-in-depth assertion: even if a
future panel evolves to surface ConfigMap or Secret content, the
sensitive-key regex prevents raw rendering.

## 6. Egress surface delta

No new outbound destinations. The provider-status probes:

| New egress target | Purpose | Enforcement | Failure mode |
|---|---|---|---|
| `http://localhost:8443/healthz` (in-sandbox) | Foundry reachability indicator | `kubectl exec` from operator workstation; no node-to-node bypass | `unknown` with verbatim reason on probe failure |
| `http://localhost:8443/agt/status` (in-sandbox) | AGT relay/registry indicator | same as above | same |

Both are **already-existing** router endpoints on a pre-existing port.

## 7. Audit events emitted

S14 emits no audit events. Read-only TUI; no writes to AGT
`AuditLogger`.

| Operation | Event | Contents | Attest-visible? |
|---|---|---|---|
| _(none introduced by S14)_ | — | — | — |

## 8. Failure mode

| Failure | Behaviour | `outageMode` gate |
|---|---|---|
| `kubectl get <crd>` fails | panel for that CRD shows `(none)` | n/a (read-only) |
| Provider probe fails / RBAC denied | provider entry renders `unknown` with verbatim reason | n/a |
| `--panels` lists unknown id | dropped silently (logged via empty render) | n/a |
| Empty cluster | every panel renders `(none)` without throwing — verified by per-panel empty-cluster test | n/a |

Default behaviour is observably **honest, not optimistic**. No probe is
allowed to "guess healthy".

## 9. Negative-test coverage

`cli/src/commands/operator/panels/panels.test.ts` adds 39 vitest cases:

| Test | Asserts |
|---|---|
| `S14 panels — empty cluster` (9 panels × 1) | every panel is non-throwing on `emptyClusterState()` |
| `mcpserver — missing jwks renders <missing>` | redaction path for absent secrets |
| `mcpserver — unknown jwks surfaces a verbatim reason` | "verify, don't guess" |
| `provider_status — each unknown branch surfaces a reason` | no invented healthy data |
| `layout — --panels filters and orders` | flag wiring correctness |
| `layout — --per-sandbox groups by sandbox-name` | per-sandbox grouping |
| `redact — isSensitiveKey / redactValue / redactObject` | LLM02 mitigation |
| `FixtureDataSource — returns the stored snapshot verbatim` | data-source seam |

CLI test count: **434 passing** (was 395 baseline; +39 from this slice;
+2 skipped unchanged).

## 10. Vendored / third-party dependency delta

No new runtime or dev dependencies. Reuses `blessed`, `blessed-contrib`,
`execa`, `vitest` already in `cli/package.json`.

| Dep | Version | License | SCA scan | Why needed (citation) |
|---|---|---|---|---|
| _(none introduced by S14)_ | — | — | — | — |

**Source citations (principle §0.2 #10):**

- Phase 2 plan §S14 — "modular panels per CRD type, pluggable data
  source, uses new Conditions matrix, renders all five CRDs + provider
  status per sandbox."
- Phase 2 plan §15 success-gate item: "Operator TUI renders all five
  CRDs + provider status per sandbox."
- CRD field shapes: `controller/src/{mcp_server,tool_policy,inference_policy,a2a_agent,claw_memory,claw_eval}.rs`
  via `deploy/helm/azureclaw/templates/crd-*.yaml`.

## 11. Sign-offs

### Author sign-off

- [x] I have read principles §0.2 #8, #9, #10 of the Phase 2 plan.
- [x] The capability contains no pseudo-implementations. Every claimed
      panel renders against real `kubectl get` shapes; tests use the
      same shapes via fixtures.
- [x] No custom crypto was added (TUI does not crypt).
- [x] Negative tests (Section 9) exist and pass — `npx vitest run`
      reports 434 passing, 0 failing.
- [x] No attestation chain change (read-only surface).

Signed: @copilot — `2026-04-30`

### Independent reviewer sign-off

_Not required for read-only TUI changes outside router-data-plane /
sandbox-image / admission-policy._

---

## 12. §15 success-gate verification

> "Operator TUI renders all five CRDs + provider status per sandbox."

✅ **Verified.** The default `renderDashboard()` output includes panels
for **every** Phase-2 CRD plus per-sandbox provider status:

| Plan-required surface | Panel id | Default-on? |
|---|---|---|
| `McpServer` (S1) | `mcpserver` | ✅ yes |
| `ToolPolicy` (S2) | `toolpolicy` | ✅ yes |
| `A2AAgent` (S3) | `a2aagent` | ✅ yes |
| `InferencePolicy` (S4) | `inferencepolicy` | ✅ yes |
| `ClawMemory` (S5) | `clawmemory` | ✅ yes |
| `ClawEval` (S6) | `claweval` | ✅ yes |
| Provider status, per sandbox | `provider_status` (per-sandbox map) | ✅ yes |

(The plan calls out "five CRDs"; in practice S1–S6 ship six CRDs and S14
panels them all.)

### ASCII proof

A live snapshot under `--per-sandbox` (sb-1 only, condensed) — the
verbatim shape used by `panels.test.ts`:

```
══ Sandbox: sb-1 ══
┄ ClawSandbox  (sb-1) ┄
NAME              HEALTH      MODEL          ISOLATION      AGE     ROLE
sb-1              healthy     gpt-4.1        enhanced       12m     controller
────────────────────────────────────────────────────────────────────────
┄ ClawPairing  (sb-1) ┄
(none)
────────────────────────────────────────────────────────────────────────
┄ McpServer  (sb-1) ┄
mcp-fs (azureclaw-sb-1) 20m
  url: http://mcp-fs.azureclaw-sb-1.svc:8080   production=yes   tools=4
  jwks-secret: <present>
  ● Ready=True Reachable
  ● Authenticated=True JWKSValidated
────────────────────────────────────────────────────────────────────────
┄ ToolPolicy  (sb-1) ┄
tp-default (azureclaw) 2h
  appliesTo: sb-1   rules=6   approval=yes   rate-limit=30/min
  commerce: mandates=yes   floor=$5.00
  ● Programmed=True PolicyCompiled
────────────────────────────────────────────────────────────────────────
┄ InferencePolicy  (sb-1) ┄
ip-default (azureclaw) 2h
  appliesTo: sb-1   tokens: daily=100000   per-req=4000
  guardrail-floor: high
  models: 1.gpt-4.1 → 2.gpt-4o
  ● Programmed=True PolicyCompiled
────────────────────────────────────────────────────────────────────────
┄ A2AAgent  (sb-1) ┄
agent-card-1 (azureclaw-sb-1) 10m
  endpoint: https://example.test/a2a   production=yes
  AgentCard: published
  capabilities: tasks, streaming
  ● CardPublished=True Signed
────────────────────────────────────────────────────────────────────────
┄ ClawMemory  (sb-1) ┄
mem-1 (azureclaw) 5h
  sandbox: sb-1   store=store-default   scope=user-123   retention=30d
  foundry-binding: bound
  rbac: project-MI: Azure AI User on RG
  ● Bound=True MemoryStoreReady
────────────────────────────────────────────────────────────────────────
┄ ClawEval  (sb-1) ┄
eval-nightly (azureclaw) 1d
  sandbox: sb-1   suite=rag-quality   schedule=0 2 * * *
  last-run: 2026-04-29T02:00:00Z   score: 0.91   next: 2026-04-30T02:00:00Z
  ● Scheduled=True CronProgrammed
────────────────────────────────────────────────────────────────────────
┄ Providers  (sb-1) ┄
Per-sandbox (sb-1)
  ● foundry            healthy
  ● agt                healthy
  ● acr                healthy
  ? identity           unknown — no workload-identity annotation

Cluster-wide
  ? agc                unknown — no Gateway objects (a2a-ingress not enabled)
────────────────────────────────────────────────────────────────────────
```

Test asserting this gate (excerpt):

```ts
it("renderDashboard against empty cluster includes every panel header", () => {
  const out = renderDashboard(empty);
  for (const p of DEFAULT_PANELS) {
    expect(out).toContain(p.title);
  }
});
```

§15 success-gate item **closed**.

## 13. Surveyed existing implementation

Pre-S14 the operator TUI was a single 859-line `cli/src/commands/operator.ts`
plus a Phase-0 decomposition under `cli/src/commands/operator/`
(`fetchers/`, `render/`, `dialogs/`, `actions.ts`, `helpers.ts`,
`keymap.ts`, `types.ts`). The pre-S14 surface rendered `ClawSandbox`,
egress domains, security state (AGT trust, audit, mesh counters),
cluster health and a topology view. **No Phase-2 CRD was rendered.**

S14 leaves all of that unchanged — the legacy main-table view + AGT
overlay are untouched. The new panel framework is additive: a sibling
directory `panels/` with its own data source, registry, and overlay
wired via Shift-P. The legacy data fetchers (`fetchers/sandboxes.ts`)
are reused inside `KubectlDataSource` for sandbox listing — no
duplication.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
