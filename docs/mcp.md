# MCP servers in kars

The [Model Context Protocol](https://modelcontextprotocol.io) (MCP) is how a
kars agent reaches tools it doesn't ship with — a hosted search API, a wiki
reader, a headless browser, your internal services. kars treats every MCP
server as an **untrusted upstream**: the agent never holds its credentials and
never opens a socket to it directly. Everything goes through the per-pod
**inference router**, which discovers the MCP's tools, enforces governance on
every call, and is the only network path to the server.

This guide covers adding an MCP, how it's reached, authentication, and the two
mechanisms that make MCP support **work out of the box**: egress
auto-derivation and session keepalive.

> Looking for a runnable example? See
> [`examples/playwright-mcp/`](../examples/playwright-mcp/) — a browser agent on
> the official Playwright MCP, end to end.

## The model: `McpServer` CR + `mcpServerRefs`

Two pieces, both declarative:

1. An **`McpServer`** custom resource describes the server: its URL, the tools
   you allow, which sandboxes may use it, and (for hosted servers) its OAuth
   config.
2. A sandbox **opts in** by naming that CR in
   `spec.governance.mcpServerRefs`.

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: McpServer
metadata:
  name: playwright
  namespace: kars-system            # same namespace as the sandbox(es)
spec:
  url: "http://playwright-mcp.kars-mcp.svc.cluster.local:8931/mcp"
  allowedTools: [browser_navigate, browser_click, browser_snapshot, browser_evaluate]
  allowedSandboxes:
    matchLabels: { kars.azure.com/sandbox: browser }
  productionMode: false
  displayName: "Playwright (headless Chromium)"
---
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: browser
  namespace: kars-system
  labels: { kars.azure.com/sandbox: browser }
spec:
  runtime: { kind: OpenClaw, openclaw: {} }
  governance:
    enabled: true
    mcpServerRefs:
      - name: playwright           # ← the only MCP-specific line
  networkPolicy:
    defaultDeny: true
    egressMode: Strict
```

You can also create the CR imperatively:

```bash
kars mcp apply playwright \
  --namespace kars-system \
  --url http://playwright-mcp.kars-mcp.svc.cluster.local:8931/mcp \
  --allowed-tool browser_navigate --allowed-tool browser_click \
  --allowed-sandbox-label kars.azure.com/sandbox=browser \
  --display-name "Playwright (headless Chromium)"

kars mcp list -n kars-system
kars mcp get playwright -n kars-system -o yaml
kars mcp delete playwright -n kars-system
```

### Key `McpServer.spec` fields

| Field | Meaning |
|---|---|
| `url` | Streamable-HTTP MCP endpoint. In-cluster Service DNS or a hosted `https://` URL. |
| `allowedTools` | Allow-list of tool names. Empty = none; `["*"]` = all (then gate with `ToolPolicy`). Pin explicitly so an upstream change can't widen the surface. |
| `allowedSandboxes.matchLabels` | Which sandboxes may use this MCP. Empty = same-namespace only. |
| `productionMode` | `true` requires HTTPS + OAuth 2.1. |
| `oauth` | OAuth issuer/audience/resource for `productionMode`. The router mints tokens; the agent never sees them. |
| `bearerFromEnv` | Static outbound bearer from a named env var, for MCPs that use a long-lived API token. |
| `crossNamespaceAllowed` | Allow sandboxes in other namespaces to reference this CR. |

The full schema is in the [CRD reference](api/crd-reference.md).

## How a tool call flows

```
agent (UID 1000) ──127.0.0.1:8443──▶ inference-router (UID 1001) ──▶ MCP server
        tools/call "playwright.browser_navigate"      │
                                                        ├─ trust + ToolPolicy check
                                                        ├─ audit event
                                                        ├─ token budget
                                                        └─ outbound auth (OAuth / bearer)
```

The agent calls a **namespaced** tool (`<server>.<tool>`, e.g.
`playwright.browser_navigate`) on loopback. The router authorises it, dispatches
to the MCP, and returns the result. The agent has no ambient network reach and
no credentials.

## Out-of-the-box egress

Because the router is the only path to the MCP, the sandbox's default-deny
NetworkPolicy has to admit the **router → MCP** hop. kars does this for you:
the controller parses the `McpServer.spec.url` and emits the right egress rule
automatically — you do **not** add the MCP host to
`networkPolicy.allowedEndpoints` by hand.

- **In-cluster Service** (`*.svc.cluster.local`): the controller emits a
  `namespaceSelector` rule for the MCP's namespace. This matters under the
  Cilium CNI, where a K8s NetworkPolicy `ipBlock` (even `0.0.0.0/0`) only
  matches the reserved `world` entity and never an in-cluster pod — so an
  `ipBlock` rule would silently fail to admit traffic to another pod.
- **External host, non-443**: a coarse port-level rule; the router's CONNECT
  allowlist enforces the exact host.
- **External host, 443**: already covered by the router's blanket HTTPS path —
  no extra rule needed.

Verify it after applying a sandbox:

```bash
kubectl -n kars-<sandbox> get networkpolicy -o yaml | grep -A4 namespaceSelector
```

## Reliable sessions (no `about:blank` mid-task)

Stateful MCP servers — Playwright is one — keep per-session state (your live
browser page) and run a **server-side heartbeat**: they send the client a
JSON-RPC `ping` every few seconds and destroy the session if no `pong` comes
back within a short window (Playwright's default is 5s). A naive request/response
client never answers those pings, so the server reaps the session; the next
tool call gets `404 Session not found`, the client re-initialises, and the work
lands on a **fresh, blank** page — the agent sees `about:blank` mid-task.

kars's router is a **well-formed MCP client**: for every stateful session it
holds the standalone SSE stream open and answers the server's `ping`s with
`pong`s, keeping the session — and the agent's live page — alive. Multi-step
flows (`navigate → click → snapshot → evaluate`) therefore stay on one page.
This is automatic for any heartbeating MCP; there's nothing to configure.

## Authentication

The agent never holds MCP credentials. Two outbound modes, both handled by the
router:

- **OAuth 2.1** (`productionMode: true` + `oauth:`): the controller wires JWKS
  rotation and the router presents a signed bearer token to the MCP.

  ```yaml
  spec:
    url: "https://mcp.example.com/mcp"
    productionMode: true
    oauth:
      issuer: "https://login.microsoftonline.com/<tenant>/v2.0"
      audience: "api://your-mcp"
  ```

- **Static bearer** (`bearerFromEnv`): for MCPs that authenticate with a
  long-lived API token, stored in the sandbox's `<name>-credentials` secret and
  injected by name. The token stays in the router; the agent only sees tools.

## Tool governance

`allowedTools` on the `McpServer` is the coarse gate. For per-tool rules
(arguments, rate limits, approval), bind a [`ToolPolicy`](api/crd-reference.md)
via `governance.toolPolicyRef`. MCP tools are subject to the same AGT governance
as built-in tools — trust scoring, audit, and policy all apply.

For the MCP-specific threat model (tool poisoning, confused-deputy, prompt
injection through tool output), see the
[MCP security top-10](security-mcp-top10.md).

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Agent says the tool doesn't exist | Tool not in `allowedTools`, or sandbox label doesn't match `allowedSandboxes` | Add the tool / fix the label; re-apply the CR. |
| `404 Session not found`, page resets to `about:blank` | Router not keeping the session alive (pre-0.1.24) | Upgrade the router; keepalive is automatic. |
| Calls time out to an in-cluster MCP | Egress not admitted (e.g. `ipBlock` under Cilium) | Use the MCP's Service DNS `url` so the controller derives a `namespaceSelector` rule; check `kubectl -n kars-<sandbox> get networkpolicy`. |
| `403`/`401` from a hosted MCP | OAuth/bearer misconfigured | Check `oauth.issuer`/`audience` or the `bearerFromEnv` secret. |

## See also

- [`examples/playwright-mcp/`](../examples/playwright-mcp/) — runnable end-to-end example.
- [CRD reference](api/crd-reference.md) — full `McpServer` / `KarsSandbox` schema.
- [MCP security top-10](security-mcp-top10.md) — the MCP threat model.
- [Architecture diagrams](architecture-diagrams.md) — the MCP data path.
