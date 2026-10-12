<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

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

The CLI accepts this manifest through `kars mcp apply browser --namespace
kars-system --from-file browser.yaml`. Do not add `url` or endpoint
authentication fields to a managed preset: the controller derives the endpoint.
CLI validation rejects unsupported presets and workload overrides before
submitting the resource; API admission and controller qualification still apply.

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

In the Operator Console, the catalog links existing services to their actual
status instead of recording a local "installed" flag. Cancel never registers a
service. A successful install submission means only that the request was
accepted; watch the service's deployment and protocol probe result separately.
While saving, the form and catalog navigation are disabled. A failed or
unconfirmed request retains the entered settings for inspection and retry.
The Capabilities and Policies views use the same current-generation readiness
presentation.

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
Direct MCP POST clients must advertise
`Accept: application/json, text/event-stream`; JSON-only requests are rejected
with HTTP 406 even when the managed server is Ready. The Kind acceptance fixture
uses this same contract. Catalog timeouts report only the last HTTP status,
numeric RPC error code, tool count, expected-tool presence and transport state,
before teardown—not response bodies, headers or credentials.
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

The AL3 Playwright candidate is maintained in `sandbox-images/mcp-playwright`.
It keeps MCP `0.0.78` and its exact Playwright browser revision, installs only at
build time, and uses native Azure Linux 3 libraries with the vendor's Ubuntu 24
Chromium binary. The architecture-specific download label is not a claim of
upstream AL3 support: its image test must pass natively for each shipped CPU.
That test checks UID 1000, Node 24, rendering, clicking, PNG capture and isolated
contexts. The official default remains unchanged until the candidate passes
container and routed installed qualification.
CI runs this image test on native AMD64 and ARM64 runners with networking
disabled, a read-only root, dropped capabilities and bounded scratch/resources.
The existing guarded push/apply path also accepts
`kars push --only mcp-playwright --apply`; it updates only the owning browser
image value and verifies the matching managed preset, leaving Everything and
MCP source specs unchanged.
This changes the controller's owning image configuration and is refused while
protected private-root qualification or retirement evidence exists. Preserve
that evidence; do not bypass the guard by patching the managed Deployment.
An OS/Node package scan does not establish vulnerability coverage for the
downloaded Chromium executables or dependencies bundled inside compiled packages.

The Everything image has a checked-in Dockerfile and complete npm lockfile.
It uses the digest-pinned Azure Linux 3 Node.js 24 base for both ARM64 and AMD64,
installs dependencies only during the build, and runs as UID 1000.
The reference server remains at `2026.7.4`; its compatible MCP SDK dependency
is pinned to `1.31.0`, addressing CVE-2026-104850 in the SDK's optional OAuth
client code. This is not a claim that the server-only preset exposed that client
path. Overrides also pin the audited fixes in fast-uri `3.1.8`, Hono `4.13.7`,
ip-address `10.7.1`, and proxy-addr `2.0.8`.
Run `npm ci --omit=dev --ignore-scripts && npm test` from
`sandbox-images/mcp-everything` to exercise the real reference server and SDK:
initialization, discovery, echo, HTTP 406 for JSON-only clients, and session
termination/reuse rejection. The fixture binds only loopback. These host checks
do not establish container isolation, a clean image scan, or routed Kind/AKS
acceptance.
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
