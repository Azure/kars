# kars Example: Browser-automation agent on a Playwright MCP

A sandboxed OpenClaw agent that drives a **real headless Chromium** through the
official [Playwright MCP](https://github.com/microsoft/playwright-mcp) server —
navigating pages, clicking, typing, taking snapshots — entirely under kars
governance and isolation.

This is the canonical "**bring an MCP server and it just works**" example. The
only MCP-specific line in the whole sandbox spec is a single
`governance.mcpServerRefs` entry; kars does the rest.

## What it shows

| Capability | How |
|---|---|
| **Zero-config MCP wiring** | One `McpServer` CR + one `mcpServerRefs` entry. The controller mirrors config into the sandbox and exposes the allow-listed `browser_*` tools on the agent's loopback. |
| **Out-of-the-box egress** | The controller parses the MCP's `url`, sees it's an in-cluster Service, and **auto-derives** the default-deny NetworkPolicy egress rule (`namespaceSelector` for `kars-mcp`, port 8931). You never hand-write that rule. |
| **Reliable multi-step browsing** | The per-pod router holds the MCP session alive by answering Playwright's heartbeat pings, so `navigate → click → snapshot → evaluate` keeps the **same live page** instead of resetting to `about:blank` mid-task. |
| **No credentials in the agent** | The agent calls tools on `127.0.0.1`; the router is the only path to the MCP. Trust, policy, audit and token budget run on every call. |
| **Default kars hardening** | seccomp `kars-strict`, read-only rootfs, drop-ALL caps, non-root, enhanced isolation. |

## Architecture

```
┌─ namespace: kars-system ───────────────────────────────────────────┐
│  KarsSandbox "browser"  (OpenClaw, enhanced isolation)              │
│   ├── openclaw container (UID 1000) ── tools on 127.0.0.1:8443 ──┐  │
│   └── inference-router  (UID 1001) ─────────────────────────────┼─ │
│         • holds MCP session alive (heartbeat pong)              │ │
│         • governance on every tool call                         │ │
└─────────────────────────────────────────────────────────────────┼─┘
                  derived egress (namespaceSelector kars-mcp:8931)  │
┌─ namespace: kars-mcp ─────────────────────────────────────────────▼┐
│  Deployment/Service "playwright-mcp"  (mcr.microsoft.com/playwright/mcp)
│   • real headless Chromium, Streamable-HTTP MCP on :8931           │
└────────────────────────────────────────────────────────────────────┘
```

## Prerequisites

A running kars control plane (`kars up`) on an AKS cluster, and `kubectl`
pointed at it.

## Deploy

```bash
# 1. The Playwright MCP server (in its own namespace).
kubectl apply -f 00-playwright-mcp.yaml
kubectl -n kars-mcp rollout status deploy/playwright-mcp

# 2. Register the MCP with kars + the browser agent.
kubectl apply -f 01-mcpserver.yaml
kubectl apply -f 02-karssandbox.yaml
kubectl -n kars-system wait --for=jsonpath='{.status.phase}'=Running \
  karssandbox/browser --timeout=180s
```

## Use it

```bash
kars connect browser
```

Then ask it to do something on the web, e.g.:

> Navigate to https://example.com, take a snapshot, and tell me the page heading.

> Go to https://playwright.dev, click "Get started", and summarise the page.

Multi-step flows work because the router keeps the browser session (and its
live page) alive across calls.

## Verify the out-of-the-box egress (optional)

The controller derives the egress rule from the MCP URL — confirm it's there
without you having written it:

```bash
kubectl -n kars-browser get networkpolicy -o yaml | grep -A4 namespaceSelector
# → a rule selecting kubernetes.io/metadata.name: kars-mcp on TCP 8931
```

## How it works

1. **`McpServer` CR** (`01-mcpserver.yaml`) declares the endpoint, the
   allow-listed tools, and which sandboxes may use it.
2. **`mcpServerRefs`** in the sandbox (`02-karssandbox.yaml`) opts the agent in.
   The controller mirrors the MCP's config into the sandbox namespace, mounts
   it into the router, and — because the URL is an in-cluster Service —
   **auto-derives the NetworkPolicy egress** to `kars-mcp:8931`. (Cilium only
   honours a `namespaceSelector` for in-cluster pod destinations, never an
   `ipBlock`; the controller emits the correct form.)
3. **The inference router** discovers the MCP's tools, namespaces them, and
   exposes them to the agent on loopback. It **acts as a well-formed MCP
   client**: it holds the standalone SSE stream open and answers the server's
   heartbeat `ping`s with `pong`s, so Playwright never reaps the session. That
   is what keeps a browsing flow on one live page.

## Going hosted

To point at a **hosted** MCP instead of an in-cluster one, change just the URL:

```yaml
spec:
  url: "https://mcp.example.com/mcp"
  productionMode: true
  oauth:
    issuer: "https://login.microsoftonline.com/<tenant>/v2.0"
    audience: "api://your-mcp"
```

External `https://` hosts on 443 are already covered by the router's HTTPS
path; the controller adds a coarse port rule for non-443 hosts and the router
enforces the exact host. The agent still never sees a token.

## Clean up

```bash
kubectl delete -f 02-karssandbox.yaml -f 01-mcpserver.yaml -f 00-playwright-mcp.yaml
```

See [`docs/mcp.md`](../../docs/mcp.md) for the full MCP guide and
[`docs/api/crd-reference.md`](../../docs/api/crd-reference.md) for the
`McpServer` / `KarsSandbox` schemas.
