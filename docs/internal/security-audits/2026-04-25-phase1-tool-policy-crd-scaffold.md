# Security Audit: `phase1/tool-policy-crd-scaffold`

**Capability:** scaffold for `ToolPolicy` CRD per implementation-plan §7
entry 4. Schema-only — no reconciler logic, no router data-plane.

## 1. Summary

- New `controller/src/tool_policy.rs` defining `ToolPolicySpec`,
  `AppliesToSelector`, `CommercePolicy` (AP2 caps), `RateLimitPolicy`,
  `ApprovalPolicy`, `ToolPolicyStatus` (KEP-1623 shape).
- Wired into `main.rs` behind `#[allow(dead_code)]`.

## 2. Threat model

**Maps directly to OWASP MCP Top 10 controls:**
- MCP-04 (Excessive Agency) — `commerce.dailyCap` / `monthlyCap` /
  `perTransferCap`.
- MCP-08 (Counterparty Trust) — `commerce.counterpartyAllowlist`
  (empty = deny-all, fail-closed).
- MCP-06 (Rate Limiting) — `rateLimit` block.
- MCP-09 (Human-in-the-loop) — `approval.mode`.

**Fail-closed defaults are central:**
- Empty `counterpartyAllowlist` → deny all transfers.
- Missing or malformed cap string → policy compile rejects → AGT
  PolicyEngine returns Deny.
- Conformance corpus row "AP2 cap exceeded → refuse" enforces this.

## 3. Spec sources

- AP2: <https://ap2-protocol.org/>
- KEP-1623: <https://github.com/kubernetes/enhancements/tree/master/keps/sig-api-machinery/1623-standardize-conditions>
- OWASP MCP Top 10 (2025): tracked internally in `docs/security-mcp-top10.md`
  (Phase 1 §7 entry 11 deliverable).

## 4. Tests

- Cargo build clean.
- Behavior coverage will land with the reconciler PR.

## 5. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
