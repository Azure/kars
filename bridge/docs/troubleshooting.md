<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Troubleshooting

## First checks

```bash
kubectl -n kars-system get pods
kubectl -n kars-system logs deploy/kars-bridge-bff --since=15m
kubectl -n kars-system logs deploy/kars-bridge-web --since=15m
kubectl -n kars-system logs deploy/kars-controller --since=15m
```

## Common symptoms

| Symptom | Likely cause | Action |
|---|---|---|
| `/workspace` redirects unexpectedly | Missing/incorrect OIDC role mapping | Inspect verified claims and `BRIDGE_OIDC_ROLE_MAP` |
| Auditor can enter Workspace | Incorrect role implication | Auditor must not imply user |
| Login follows localhost and fails in managed Playwright | Port-forward Dex split horizon | Use redirect-manual evidence or configure ingress issuer |
| BFF returns Kubernetes 403 | Missing ServiceAccount verb | Test `kubectl auth can-i` as `kars-bridge` |
| Web proxy returns `bad_gateway` (502) | BFF or Dex upstream request failed | Check the fixed web log marker, configured upstream and service endpoints |
| Mission launch returns 422 | Budget or preflight failure | Read the structured error and Console budget/MCP status |
| MCP appears Ready but tools fail | Stale generation/session or auth | Inspect `McpServer.status` and sandbox router logs |
| Playwright opens a blank page mid-run | Session reaped or non-isolated server | Verify managed preset and router keepalive |
| Team reuses another team’s role | Outdated parent-scoped spawn implementation | Upgrade Kars controller/router/runtime |
| Team Diagnostics says a roster ToolPolicy is `<none>` but its parent is bound | Older Bridge producer dropped the policy when writing a runtime/model override | Keep the Team paused, deploy the corrected BFF, then re-review and save its roster. Do not remove the parent policy or weaken controller attenuation checks |
| Team forgets earlier work | No harvested substantive deliverable | Inspect team commons and run health |
| Delete works as admin but fails in Bridge | Developer kubeconfig hid RBAC gap | Test through deployed BFF ServiceAccount |

Web proxy failures deliberately omit exception messages, upstream URLs, query
parameters and stack traces from both the response and the web failure marker.
Use the request timestamp to correlate BFF/Dex logs and Kubernetes events;
do not enable logging of cookies, login codes or signed principal tokens.

## Fleet utilization

Fleet resource samples come from the Kubernetes Metrics API, separately from
observed task activity. A missing sample is unknown, not zero usage. Partial
node or sandbox coverage therefore does not produce a healthy aggregate.
The active-team-run count is not a global admission pool; the controller does
not enforce the previously displayed six-slot cluster limit.

A Metrics API 404 means metrics-server is not installed or its API registration
is missing. A 503 indicates a registered but unavailable service; check its
Pods, endpoints, and kubelet connectivity. A 401 or 403 indicates missing
Bridge authorization for `metrics.k8s.io` node/pod reads. Task-list failures
are reported independently and never become zero active runs.

Core now defaults to an AL3-packaged metrics-server when no external Metrics API
is registered. For a 404, check that the Core metrics prerequisite was included
in the installed chart and that its matching image was acquired. For a 503,
check `kars-metrics-server` readiness and the APIService Available condition.
External Metrics API providers remain externally managed; Core does not take
them over. See the [Core metrics configuration and certificate renewal guide](../../deploy/helm/kars/README.md#cluster-utilization).
An unavailable panel, a rendered chart, or a Ready Pod alone is not evidence
of working resource samples; verify actual node and sandbox usage.

## OpenClaw startup

Gateway and node-host output is forwarded to the `openclaw` container log while
retaining `/tmp/gateway.log` and `/tmp/node-host.log` inside the sandbox. Use
`kubectl -n kars-<sandbox> logs deploy/<sandbox> -c openclaw` to inspect startup
failures without entering the sandbox. If the gateway exits during the startup
check, the container reports its exit status and fails instead of starting the
node host against an already-dead gateway.

The OpenClaw 2026.9.8 image requires the matching entrypoint: identity belongs in
`agents.entries.main.identity`, generated-image routing in
`agents.defaults.mediaModels.image`, and semantic-memory settings in
`memory.search`. The retired device-auth bypass is not emitted; gateway token
auth and normal device pairing remain enabled. Do not run an automatic repair
that replaces operator settings or weakens admission.

## Managed MCP

The catalogue records an installation request, not a completed installation.
Agent capabilities displays the current controller reason and message alongside
the phase. After editing a server, an older generation's result is shown as
Reconciling, not as a current Ready result. A workload reference can be recorded
before the Deployment exists.

If the message says the configured pull Secret is absent, installation is
waiting for an operator prerequisite, not for browser startup. Check the Helm
values `managedMcp.namespace` and `managedMcp.imagePullSecret`. That Secret must
exist in the controller-claimed managed namespace, even when the Playwright
image itself is public: the configured pull-Secret requirement applies to both
managed presets. A same-named Secret in the controller namespace is insufficient.

After a clean reinstall, let the controller create and UID-claim the managed
namespace before restoring its explicitly approved, namespace-local registry
credential through the operator's secret-management process. Do not pre-create
or force-adopt an unclaimed namespace, copy unrelated credentials, remove the
pull requirement to bypass the check, or mark the server Ready manually.
The controller retries automatically; success still requires the owned rollout
and a real MCP `initialize` / `tools/list` probe. This recovery does not require
node-pool, GPU, CNI, or model-serving changes.

A proposed Team retains up to eight distinct installed MCP references in model
order; duplicate or unknown entries do not discard later valid selections.
Selection does not grant access or prove execution: the reviewed bindings,
router policy and resource qualification gates still apply.

```bash
kubectl -n kars-system get mcpserver <name> -o yaml
kubectl -n kars-mcp get deploy,svc,networkpolicy
kubectl -n kars-<sandbox> logs deploy/<sandbox> -c inference-router --since=15m
```

## Fleet Health availability

Fleet Health distinguishes failed reads from real zero counts. Unavailable
insights or egress inventory render a dash and an explicit availability hint;
refresh after the backend recovers to retrieve current values. A failed
orchestrator status read remains visible rather than hiding the compose-engine
section. An absence of Degraded or Failed sandbox phases is not proof that model
calls, tools, or completed work are healthy.

The diagnostics, orchestrator and integration endpoints propagate failed
Kubernetes inventory reads as sanitized upstream errors instead of reporting
empty inventories or missing installations. Orchestrator details show container
readiness, restart count, node and uptime. The newest non-terminating pod is used,
including Pending pods, init-container failures and scheduling failures.

Router candidates are pod-readiness observations, not successful inference
probes or independent capacity guarantees. The composer can use another ready
sandbox router when the standing orchestrator is absent. A configured direct
endpoint is likewise not evidence that its credentials or model calls work;
health polling does not invoke models or tools. During a rolling BFF upgrade,
new detail fields remain unknown until the backend supplies them.

## Configuration availability

Configuration reads each independent source separately. A failed GitHub App or
additional-provider credential read does not hide the verified cluster-default
provider, model catalogue or integration status. Failed sections show their
error rather than claiming that nothing is configured. Provider editing stays
unavailable until all of its required reads succeed; a verified default provider
remains visible as a read-only card in the meantime.

`credential_authority_unavailable` means the affected store cannot be read under
the current credential grant. Follow the [reviewed enrollment flow](governed-credentials.md)
instead of broadening Secret permissions or replacing existing credentials.

## SRE availability

An empty proposal history does not establish that kars-SRE is installed or
working. SRE Actions checks sandbox inventory separately: a missing `sre`
sandbox is shown as not installed, and an inventory failure is shown as unknown.
A present sandbox's phase does not prove diagnostic tool access or a successful
model call. Failed proposal reads remain unavailable, not an empty history.
Historical proposals remain visible even when the sandbox is absent.

Use the [SRE authority installation and enrollment flow](../../docs/how-to/sre-authority.md);
setting a Helm flag or adding a role label alone does not grant SRE authority.

## Evidence bundle

Collect resource names and UIDs, trace IDs, the affected output/receipt, pod
events, BFF/controller/router logs, and exact timestamps. Remove credentials,
cookies, identity seeds, and private keys before sharing.
