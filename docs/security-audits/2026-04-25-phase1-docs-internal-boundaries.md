# Security Audit: `phase1/docs-internal-boundaries`

**Capability:** docs-only. Adds per-CRD posture rows to
`docs/internal-boundaries.md` for the three new CRDs/extensions
landed this session: `McpServer`, `ToolPolicy`, and
`ClawSandbox.spec.a2a`.

## 1. Summary

Per implementation-plan §3 / `docs/internal-boundaries.md` "Rule for
new CRDs": every new CRD requires a posture-vs-MSFT-product entry.
This branch retroactively adds the four entries that the three
preceding scaffold PRs each technically should have included.

Postures declared:
- `ClawSandbox` — Orthogonal to Foundry agent service.
- `McpServer` — Orthogonal to Foundry MCP hosting.
- `ToolPolicy` — Complementary to Foundry guardrails (Content Safety
  stays model-side; ToolPolicy is sandbox-side).
- `ClawSandbox.spec.a2a` — Orthogonal to Foundry A2A (native).

## 2. Threat model delta

None. Doc-only.

## 3. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
