# MCP servers

Bridge manages the Kars `McpServer` catalog and exposes two distinct modes.

## Managed MCP

Bridge creates a typed `McpServer` that selects a controller-owned preset. Kars
deploys the workload, Service, probes, NetworkPolicy, and registry credentials.
The controller does not accept an arbitrary image from the user-facing CR.

### Playwright

The managed Playwright preset provides isolated browser automation. It is a
meaningful integration for navigation, interaction, screenshots, and page
evaluation.

### Everything

Everything is the MCP reference server. It exposes deterministic protocol
features such as:

- echo and sum;
- structured content and annotations;
- resources and links;
- logging and subscriptions;
- long-running operations.

It exists to prove that generic MCP deployment, discovery, schemas,
namespacing, forwarding, and session recovery work. It is **not** a production
business integration.

## External MCP

External mode registers an existing Streamable HTTP endpoint. Bridge does not
deploy it. The operator configures:

- URL and production mode;
- OAuth or router-held bearer authentication;
- allowed tools;
- allowed sandbox scope;
- optional cross-namespace access.

## Readiness

An MCP is ready only when:

1. the workload and Service are ready, if managed;
2. `initialize` succeeds;
3. `notifications/initialized` succeeds;
4. `tools/list` succeeds;
5. the observed generation is current;
6. the tool count and schema digest are recorded.

A running pod is not sufficient.

## Mission and team use

The composer lists only current, namespace-compatible MCP resources. Required
MCP readiness participates in preflight; a missing or stale required server
blocks launch rather than silently dropping tools.

Tools are namespaced as `<server>.<tool>`.

## Security

- Agents do not receive MCP credentials.
- ToolPolicy and MCP allow-lists both apply.
- Server-scoped AGT action verbs are used for governance.
- Strict sandbox egress does not require a broad destination grant; the
  controller derives the router-to-MCP NetworkPolicy path.
- Tool results remain untrusted input to the model.

## Troubleshooting

See [Troubleshooting](troubleshooting.md#managed-mcp).
