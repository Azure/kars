# AzureClaw Headlamp Plugin

Adds an **AzureClaw** sidebar to the [Headlamp](https://headlamp.dev/) Kubernetes
dashboard with list + detail views for the 9 AzureClaw custom resources:

- ClawSandbox
- InferencePolicy
- ClawMemory
- McpServer
- A2AAgent
- ToolPolicy
- TrustGraph
- ClawPairing
- ClawEval

Detail panes show `.spec`, `.status`, and a typed Conditions table with
status colouring (Ready / Provisioned → green, Degraded / Failed → red,
everything else → amber).

> **Known gaps (v1).** The status chip is **phase-only** — an amber chip
> on a policy-lane CRD does not differentiate "signature failed" from
> "compiled but router-drifted". For `McpServer`, the detail pane
> renders only the **first/primary** server when a sandbox uses
> `governance.mcpServerRefs[]` plural — a fleet panel showing all
> referenced servers is deferred. Both are tracked in
> [`docs/roadmap.md`](../../docs/roadmap.md).

## Build

```bash
npm install
npm run build       # → dist/main.js
```

The CLI's `azureclaw dev --target local-k8s` (Phase 5+) builds this
automatically and side-loads `dist/` into the Headlamp pod via
`kubectl cp` to `/headlamp/plugins/azureclaw/`.

## Standalone install (manual)

```bash
# Build
npm install && npm run build

# Copy into a running Headlamp pod
POD=$(kubectl get pod -n headlamp -l app.kubernetes.io/name=headlamp -o jsonpath='{.items[0].metadata.name}')
kubectl cp dist headlamp/$POD:/headlamp/plugins/azureclaw
kubectl rollout restart deployment/headlamp -n headlamp
```

## Adding a new CRD

The plugin is data-driven. Append one entry to `AZURECLAW_CRDS` in
`src/index.tsx` — list/detail routes and the sidebar entry are
generated automatically. No per-CRD boilerplate required.
