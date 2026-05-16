# Security Audit — comment-only stale-SDK-name scrub in openclaw runtime

**Scope**: PR #329 — single-line comment edit in
`runtimes/openclaw/src/index.ts` (header block immediately above the
`AGT_POLICY` table).

## 1. What changed

Pure comment text. One line. The header above the policy table used to
say `// AGT SDK — AgentMesh (amitayks/agentmesh)`, which had been stale
since the mesh-provider vendor branches were retired in Phase 5.2 (commit
`14a43f5`). The runtime has not imported `@agentmesh/sdk` since then;
the AGT transport is `@microsoft/agent-governance-sdk` on the
TypeScript side and the `agentmesh` crate (v3.x) on the Rust side. The
comment now names the correct upstream.

Diff:

```diff
-// AGT SDK — AgentMesh (amitayks/agentmesh)
+// AGT (Microsoft Agent Governance Toolkit) — `@microsoft/agent-governance-sdk`
 // Full E2E encrypted inter-agent communication via self-hosted relay/registry.
```

## 2. Capability Surface

None added, removed, or changed. The edited bytes are inside a `//`
comment block; TypeScript emits nothing for comments. The trailing
`AGT_POLICY` map and every code path that consumes it (KNOCK handler,
tool-allow/deny gating) are byte-identical to `main` pre-change.

The `security-audit-required` CI gate matched on the path
(`runtimes/openclaw/src/index.ts`) which is part of the
capability-introducing path list, not on the diff content. This audit
documents that no capability was introduced.

## 3. Crypto Surface

No change. The Signal Protocol / X3DH / Double-Ratchet primitives are
all sourced from `@microsoft/agent-governance-sdk` (TypeScript plugin)
and the `agentmesh` crate (Rust router). No custom crypto, no key
material moved, no algorithm changes.

## 4. Secrets Handling

No change. No new env vars, no new secret reads, no new logging.

## 5. Test Coverage

Not required — the change emits no JavaScript and is unreachable. All
existing runtime/router/controller tests continue to pass; CI build was
green on PR #329 except for this audit-gate trip itself.

## 6. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
