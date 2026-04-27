# Security Audit: `phase1/mcp-server-crd-scaffold`

**Capability:** scaffold for `McpServer` CRD per implementation-plan §7
entry 3. Schema-only — no reconciler logic, no router data-plane changes.

## 1. Summary

- New `controller/src/mcp_server.rs` defining `McpServerSpec`,
  `McpOAuthConfig`, `SandboxSelector`, `McpServerStatus` (KEP-1623 shape).
- Wired into `main.rs` behind `#[allow(dead_code)]` until the reconciler
  lands in `phase1/mcp-2026-streamable-http-routes`.
- Field shape mirrors MCP 2026-01-15 spec exactly.

## 2. Threat model delta

None at this branch — schema only. The future reconciler will add:
- A new outbound network destination (the MCP server URL) gated by
  per-sandbox `allowedSandboxes` selector and `oauth` config.
- The `productionMode: false` path is the dev-only escape hatch and is
  scheduled to be admission-blocked on non-dev tenants by the same
  policy that today blocks `provider: null|noop|disabled`
  (`no-null-provider-prod.sh` + matching VAP).

## 3. Spec sources

- MCP 2026-01-15 spec: <https://modelcontextprotocol.io/specification/2026-01-15>
- OAuth 2.1 BCP: RFC 9700 <https://www.rfc-editor.org/rfc/rfc9700>
- KEP-1623 (Conditions / observedGeneration):
  <https://github.com/kubernetes/enhancements/tree/master/keps/sig-api-machinery/1623-standardize-conditions>

## 4. Tests

- Cargo build clean (controller).
- No new tests in this branch — behavior gates land with reconciler.

## 5. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
