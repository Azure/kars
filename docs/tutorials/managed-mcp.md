# Managed MCP workloads

Kars can deploy the reviewed `playwright` and `everything` MCP presets. An
operator authors the `McpServer`; an access request alone never creates or
grants a server. This infrastructure does not deliver assignments, acquire skill
packages or memory, or implement aggregate task budgets.

**Publication gate:** this slice depends on the qualified SRE-authority and
governed-services prerequisites. Their local source integration is not a
substitute for completed real-API acceptance or the required reviews. No image
release or deployment is performed by publishing this source.

## Register a managed server

After installing the matching controller/chart, create a server in the same
workspace as its authorized Sandboxes:

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: McpServer
metadata:
  name: browser
  namespace: kars-system
spec:
  managed:
    preset: playwright
  allowedTools:
    - browser_navigate
    - browser_snapshot
  allowedSandboxes:
    matchLabels:
      team: research
```

Reference `browser` in the Sandbox's `spec.governance.mcpServerRefs` and ensure
its labels match the selector. Keep the existing `inferenceRef` and runtime
configuration. The legacy singular `mcpServerRef` remains supported.

Managed server names must be DNS labels because their names become tool-name
prefixes. The `managed` object accepts only a reviewed `preset`; it cannot select
arbitrary images, commands, volumes, privileges, URLs or authentication sources.
It is mutually exclusive with `url`, `bundleRef`, `oauth`, `productionMode`,
`scopes` and `bearerFromEnv`. External URL and genuinely signed bundle authoring
paths remain separate; a managed image is not presented as a signed policy
bundle.

Wait for `Ready` and inspect `status.endpoint`, `workloadRef`,
`managedNamespaceUid`, `workloadGeneration`, `workloadImage`,
`discoveredTools` and `toolSchemaDigest`. A TCP socket alone is insufficient:
the exact Deployment generation/image must become available and pass bounded
MCP initialization and `tools/list` discovery. The probe closes its session and
never invokes a tool. Empty `allowedTools` grants no tools.

Unavailable images, invalid ownership, failed protocol negotiation and stale
rollouts remain Pending/Degraded; they are not advertised as working resources.

## Invoke from either runtime

OpenClaw and Hermes register two provider-independent tools:

- `kars_mcp_list`, optionally with `server: "browser"`.
- `kars_mcp_call` with the exact returned tool name, optional server scope, and
  an `arguments` object.

The namespaced name is `browser.browser_navigate`; hyphens in server names become
underscores. The router re-evaluates its live AGT policy and per-tool rate limit
for `tool:browser.browser_navigate` before forwarding. Configure the bounding
ToolPolicy accordingly. A bridge cannot grant a denied capability, and direct
HTTP calls do not bypass the same router policy check.

The optional `X-Kars-Mcp-Server` header narrows discovery and invocation to one
mounted server; it never selects a workspace or an arbitrary upstream URL.
The unauthenticated lane requires an actual loopback socket peer; missing
connection context, network peers and spoofed forwarding/Sandbox/session headers
are rejected before policy evaluation or invocation. Configured Sandbox identity
is not evidence about an incoming caller. Legacy OAuth routes retain their
verified caller identity for policy/audit and do not expose managed Sandbox
catalogs or browser sessions to remote OAuth clients.
Unqualified or unknown scopes do not invoke a tool. Bridge calls are not
automatically replayed, and HTTP-accepted upstream RPC errors—including errors
mentioning a session—are not retried as tool calls. Semantic `isError` survives
the bridge. Protocol errors and diagnostic logs do not copy API bodies.

Registry changes are applied by Sandbox reconciliation and rolling restart,
not by an instant in-process revocation mechanism. Sandboxes referencing MCP
servers reconcile periodically as well as on normal resource events. A
credential/configuration revision change restarts their cached catalogs.

## Isolation and ownership

The default managed namespace is `kars-mcp`, separate from the controller.
Kars CREATEs and UID-claims it; it does not force-adopt an existing unclaimed
namespace. Namespace replacement is rejected rather than silently rebound.
Use a fresh namespace through `managedMcp.namespace` when necessary; retire
existing managed resources before changing their namespace.

Each Deployment, Service and NetworkPolicy is bound to the full source CR UID
and namespace UID. UID-derived names and selectors prevent same-named servers
in different workspaces from sharing pods. Writes and deletion use API UID/RV
preconditions; status `workloadRef` is only a display hint.

Pods run non-root with RuntimeDefault seccomp, dropped capabilities, a read-only
root filesystem, bounded resources and scratch storage, and no automounted
Kubernetes token. The managed path creates no signing key or endpoint credential.
NetworkPolicy permits incoming controller probes and authorized referring
Sandbox namespaces. Browser egress is restricted to public HTTP(S) plus DNS;
private, loopback and link-local IPv4 destinations are excluded. This is not a
claim that a browser's internal network activity inherits the agent's signed
hostname allowlist: granting a browser tool grants that preset's separately
documented network capability. ToolPolicy must bound the tool surface.

Deleting a server waits for its owned Deployment to disappear, then removes its
owned Service and NetworkPolicy. The shared namespace and operator-managed pull
Secrets remain. Unbound legacy auxiliary Secrets/ConfigMaps are retained for
explicit operator cleanup instead of being deleted by name.

## Image acquisition

Playwright defaults to the pinned official MCR image. Image overrides are
operator-level Helm values:

- `managedMcp.playwrightImage`
- `managedMcp.everythingImage`
- `managedMcp.imagePullSecret`: an existing dockerconfigjson Secret **in the
  claimed managed namespace**, not a credential copied from another namespace.

The Everything image has a checked-in Dockerfile and complete npm lockfile.
Its default GHCR reference is not evidence that a new release was published.
Build and publish it through an explicitly authorized operator workflow:

```sh
kars push --only mcp-everything --apply
```

That command binds the owning controller configuration to the pushed manifest
and waits for eligible Everything servers to qualify the actual image. It does
not overwrite MCP CR specs or force-recreate Services/selectors. With no eligible
servers, only the owning default is updated. No release automation is added by
this slice.

## Bounds and limitations

Discovery is bounded to eight pages and 256 tools per server. Router response
bodies are bounded to 2 MiB; readiness probes use a smaller 256 KiB response
limit and a total deadline. Each bridge admits at most eight pending calls.
Concurrent calls that could race one upstream session are rejected rather than
mutating that session concurrently. Long-running calls can time out; a timeout
does not prove that an upstream side effect was cancelled.

There is no new outbound OAuth acquisition flow. Existing explicitly configured
external bearer authentication and inbound OAuth verification retain their
separate contracts. The router does not claim durable receipts, shared budget
enforcement, workload attestation or autonomous resource approval.
