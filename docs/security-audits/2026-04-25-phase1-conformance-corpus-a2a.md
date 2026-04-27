# Security Audit: `phase1/conformance-corpus-a2a`

**Capability:** scaffolds Phase 1 conformance-corpus harnesses for
A2A 1.0.0 AgentCard JWS, MCP 2026-01-15 Streamable HTTP, AP2 commerce,
and OAuth 2.1 BCP.

## 1. Summary

Adds four new vitest specs under `tests/conformance/specs/`:
- `a2a-agent-card.spec.ts` (18 todos)
- `mcp-streamable-http.spec.ts` (~22 todos)
- `ap2-commerce.spec.ts` (9 todos)
- `oauth21-bcp.spec.ts` (6 todos)

All cases are `it.todo` until the corresponding routes/reconcilers
land. The harness shape locks in the negative-class coverage
required by implementation-plan §5.4 so the future routes branches
are pure wiring steps.

## 2. Threat model — what this defends

The vendored AgentMesh production-bug class — "endpoint returned 200
but never called the crypto step" — would have been caught by exactly
this style of behavioural negative-test corpus. Pre-authoring the
negative cases here, before routes exist, prevents the routes branch
from accidentally shipping a 200-without-verify path.

OWASP MCP Top 10 cases covered:
- MCP-01 (Auth bypass) — OAuth corpus
- MCP-02 (Tool poisoning) — Streamable-HTTP method allowlist
- MCP-04 (Excessive Agency) — AP2 caps
- MCP-08 (Counterparty Trust) — empty allowlist deny-all
- MCP-10 (Shadow MCP) — shadow detection signal

## 3. Tests

- 8 spec files run under `tests/conformance/`.
- 111 todos across the corpus.
- All skipped (no code yet); harness compiles + collects cleanly.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
