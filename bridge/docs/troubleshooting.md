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
| Mission launch returns 422 | Budget or preflight failure | Read the structured error and Console budget/MCP status |
| MCP appears Ready but tools fail | Stale generation/session or auth | Inspect `McpServer.status` and sandbox router logs |
| Playwright opens a blank page mid-run | Session reaped or non-isolated server | Verify managed preset and router keepalive |
| Team reuses another team’s role | Outdated parent-scoped spawn implementation | Upgrade Kars controller/router/runtime |
| Team forgets earlier work | No harvested substantive deliverable | Inspect team commons and run health |
| Delete works as admin but fails in Bridge | Developer kubeconfig hid RBAC gap | Test through deployed BFF ServiceAccount |

## Managed MCP

```bash
kubectl -n kars-system get mcpserver <name> -o yaml
kubectl -n kars-mcp get deploy,svc,networkpolicy
kubectl -n kars-<sandbox> logs deploy/<sandbox> -c inference-router --since=15m
```

## Evidence bundle

Collect resource names and UIDs, trace IDs, the affected output/receipt, pod
events, BFF/controller/router logs, and exact timestamps. Remove credentials,
cookies, identity seeds, and private keys before sharing.
