# Tutorial: managed Playwright and Everything MCP

This tutorial deploys two controller-managed MCP servers:

- **Playwright**: a real browser automation integration.
- **Everything**: a deterministic MCP conformance fixture.

## Prerequisites

- Kars installed in `kars-system`.
- Controller access to the configured managed-MCP images.
- A default `ToolPolicy`.
- A sandbox runtime with MCP support.

## 1. Install the managed MCP resources

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: McpServer
metadata:
  name: playwright
  namespace: kars-system
spec:
  managed:
    preset: playwright
  allowedTools:
    - browser_navigate
    - browser_click
    - browser_snapshot
    - browser_evaluate
---
apiVersion: kars.azure.com/v1alpha1
kind: McpServer
metadata:
  name: everything
  namespace: kars-system
spec:
  managed:
    preset: everything
  allowedTools:
    - echo
    - get-sum
```

```bash
kubectl apply -f managed-mcp.yaml
kubectl -n kars-system get mcpservers
```

Wait for `Ready=True`. Readiness means more than a running pod: the controller
has completed the MCP handshake, listed the tools, recorded the schema digest,
and verified the managed Service.

## 2. Attach both MCPs to a sandbox

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: mcp-demo
  namespace: kars-system
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}
  governance:
    enabled: true
    toolPolicyRef:
      name: kars-default
    mcpServerRefs:
      - name: playwright
      - name: everything
  networkPolicy:
    defaultDeny: true
    egressMode: Strict
```

The controller derives router-to-MCP NetworkPolicy rules from the MCP
registrations. Do not add broad sandbox egress for these Services.

## 3. Run a meaningful proof

Ask the agent to:

1. call `everything.echo` with a unique marker;
2. call `everything.get-sum` with `37` and `5`;
3. navigate to `https://example.com` with Playwright;
4. read the page heading and capture a snapshot.

Expected evidence:

- Everything returns the marker and `42`;
- Playwright returns `Example Domain`;
- router logs contain namespaced `tools/call` events;
- the Playwright session survives navigate → inspect → evaluate.

## 4. Restart recovery

Restart the Everything Deployment while keeping the sandbox pod unchanged.
Repeat echo and sum. The router probes the stale session, reinitializes the MCP
when the old session is proven dead, and retries once.

## What this proves

Everything proves the generic protocol path. Playwright proves a real,
stateful integration. Passing Everything alone does **not** prove browser
automation or a production MCP integration.

## Troubleshooting

See [MCP servers](../mcp.md#troubleshooting) and
[platform troubleshooting](../operations/troubleshooting.md).
