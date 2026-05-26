# Security Audit — operator multi-cluster + headlamp + plugin DID fix + RBAC + netpol

**Scope**: PR #354 — `fix/operator-headlamp-bugs`. Five commits across the operator TUI, OpenClaw plugin, K8s RBAC, and sandbox NetworkPolicy.

Capability-introducing paths touched:

- `controller/src/reconciler/mod.rs` (sandbox NetworkPolicy egress rule)
- `cli/src/commands/connect.ts`, `list.ts`, `headlamp.ts` and operator/* (new + edited CLI commands; cli/src/commands/ is in the capability list)
- `runtimes/openclaw/src/index.ts` (the OpenClaw plugin's KNOCK/onMessage handlers)

All changes are either **bug fixes** for incorrect identifier propagation or **policy fixes** that loosen overly-restrictive defaults. No capability is added or weakened in a way that grants new authority to the agent.

## 1. What changed

### 1a. Sandbox NetworkPolicy now allows mesh egress to agentmesh ns

`controller/src/reconciler/mod.rs` egress rule selecting the agentmesh namespace previously matched the label `app.kubernetes.io/managed-by=azureclaw`, which the AGT bundle in `deploy/agentmesh-agt.yaml` never set (it labels with `azureclaw.azure.com/managed: "true"` instead). The mismatch meant every sandbox lost mesh on kindnet. **Also**: kindnet evaluates NetworkPolicy post-DNAT, so the ports the policy must allow are the Service `targetPort` values (relay 8083, registry 8082), not the service-side ports (8765 / 8080).

Both fixed. The selector now matches by canonical ns name (`kubernetes.io/metadata.name: agentmesh`); the allow-list includes both service ports and target ports.

**Capability impact**: this is a fix, not a weakening. The pre-fix policy was unintentionally blocking required infrastructure traffic. The new policy still **only** allows egress to the agentmesh namespace and only on the four AGT-specific ports. No new network reachability.

### 1b. Controller RBAC: `events.k8s.io` group added

`deploy/helm/kars/templates/rbac.yaml` previously granted `create/patch` on the legacy core `events` ("" apiGroup) but not on `events.k8s.io`. kube-rs's `Recorder` writes to the modern API group on Kubernetes ≥1.19, so every reconcile spammed a 403 warning. Both groups now granted. Strictly additive — the controller can finally publish events the modern API expects.

### 1c. OpenClaw plugin: peer DID truncation

`runtimes/openclaw/src/index.ts` lines 580/658/710/731 fell back to `fromAmid.slice(0, 12)` when name resolution returned empty. That truncated literal — exactly `did:agentmes` for every AGT DID — was passed as the **trust-store key** to `pushTrustToRouter()` and as the inbox `from_agent` identifier, collapsing every distinct peer into a single key.

Fixed: the 12-char short form stays in log strings (humans can read it), but identifier sites now fall back to the **full** AMID so distinct peers get distinct trust entries.

**Capability impact**: tightens, doesn't loosen. Previously every unresolved peer inherited the same trust score; now each peer is scored independently and KNOCK threshold rejection works correctly.

### 1d. Operator multi-cluster

`cli/src/commands/list.ts` and `cli/src/commands/operator/fetchers/sandboxes.ts` now probe every kube context in the user's kubeconfig and query each in parallel (3s probe timeout). Each sandbox is tagged with its origin `kubeContext` so per-sandbox follow-up fetches (router probes for security/egress/agt-quick) target the correct cluster rather than silently falling back to whichever context is current.

**Capability impact**: zero. The operator only reads the kubeconfig the user already has access to; cross-cluster *writes* still require explicit `--context`. The new `kars headlamp` command port-forwards Headlamp + optionally installs the chart on a single context.

### 1e. CLI agent table 'Cluster' column

`cli/src/commands/operator.ts` gained a `Cluster` column rendering the origin context via `clusterOriginTag(s)` (extracted into `cli/src/commands/operator/helpers.ts` for the §4.2 LOC budget). Purely display.

## 2. Streaming / hot path

Not touched. Edits are confined to non-streaming control-plane handlers (KNOCK, onMessage, NetworkPolicy compile, table render).

## 3. CR/LF / header-injection safety

`runtimes/openclaw/src/index.ts` falls back to the full AMID as `from_agent`; AMIDs are validated DID strings (`did:agentmesh:<name>`) with no CR/LF. Even if a malicious agent registered a registry entry with a name containing CR/LF, the AMID resolution layer enforces character class via the registry's input validation. No new sanitization needed.

## 4. Defensive parsing

All new code paths in `cli/src/commands/list.ts` and `sandboxes.ts` use `Promise.allSettled` with short timeouts — an unreachable AKS context does not block listing the local one. JSON parsing is wrapped in try/catch with no-op fallthrough.

## 5. Crypto Surface

No change. Mesh envelopes continue to be X3DH + Double Ratchet from the upstream `@microsoft/agent-governance-sdk`. This PR touches neither identity, key material, nor envelope layout.

## 6. Secrets Handling

Headlamp service-account tokens are minted via `kubectl create token --duration=24h` and printed to stdout for the user to paste into the Headlamp login UI. They are never written to disk by the CLI. Same TTL + provisioning pattern as `kars dev --target local-k8s` (PR #338).

## 7. Test Coverage

- `cargo test --package kars-controller` — 1 PASS (integration; the controller crate has no `--lib` target)
- `cd cli && npm test` — **769/769 PASS**
- `cd runtimes/openclaw && npm test` — **118/118 PASS**
- Live verification on kind-kars-dev: exec-brief e2e harness **9/9 verification checks PASS** with the patched NetworkPolicy + plugin + RBAC

## 8. Network / NetworkPolicy review

The only network policy change is in §1a (sandbox → agentmesh egress). No new ingress allowance; no new external egress allowance. The fix is strictly within the same control-plane allow list the policy already documented.

## 9. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
