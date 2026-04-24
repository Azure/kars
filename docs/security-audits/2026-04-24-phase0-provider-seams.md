# Security audit — Phase 0 provider-seam scaffolds

**Date:** 2026-04-24
**Capability:** provider trait/interface scaffolds (Rust router, Rust controller, TS CLI)
**Branch:** `phase0/provider-trait-scaffolds`
**Companion plan section:** `docs/implementation-plan.md` §6 items 4 + §1.2 + §1.4

## 1. Summary

Land the four provider contracts as trait/interface definitions:

- **Rust router** (`inference-router/src/providers/`): `MeshProvider`,
  `PolicyDecisionProvider`, `AuditSink`, `SigningProvider` — async traits
  with canonical request/response types, `thiserror`-based error types, and
  module-level `#![allow(dead_code)]` until Phase 1 wires call-sites.
- **Rust controller** (`controller/src/providers/`): `ProviderKind`,
  `ProviderSelection`, `field_managers` — for CRD-spec parsing and SSA
  field-manager discipline per plan §6 #4.
- **TS CLI** (`cli/src/providers.ts`): `ProviderKind`, `ProviderSelection`,
  `parseProviderKind`, `selectionHasNull`, `selectionToEnv`,
  `parseOutageMode`, `DEV_ONLY_LABEL_{KEY,VALUE}`.

No runtime behaviour change; no implementation code added; no call-sites
migrated. This PR is pure type scaffolding.

## 2. Threat model

| STRIDE | Applies? | Notes |
|---|---|---|
| Spoofing | No | No network-facing code added. |
| Tampering | No | No persistence / SSA / CRD writes added. |
| Repudiation | No | No audit-emission code added. |
| Information disclosure | No | No secret handling added. |
| Denial of service | No | No new endpoints / reconciler loops. |
| Elevation of privilege | No | No admission / RBAC changes. |

**OWASP LLM Top 10:** N/A — no runtime path touched.
**OWASP MCP Top 10:** N/A — no MCP handler modified.

## 3. AuthN / AuthZ path

None added.

## 4. Secret custody

None added. `SigningProvider` contract *documents* the invariant that key
material never crosses the boundary — enforcement comes when
`VendoredSigningProvider` lands in Phase 1 under a subsequent audit.

## 5. Egress delta

None. No new HTTP clients, WebSocket connections, or DNS lookups.

## 6. Audit events

None added. `AuditSink` trait is defined but no `append()` calls added.

## 7. Failure mode

`cargo check`: clean.
`cargo test --package azureclaw-controller`: 102 passed / 0 failed (including
5 new tests for `ProviderKind::from_spec`, `allowed_in_prod`,
`ProviderSelection::default`, `has_null`, and `field_managers` constants).
`cargo test --package azureclaw-inference-router`: 0 passed / 0 failed
(no new tests — traits with no impls).
`cargo clippy --all-targets -- -D warnings`: clean after two fixes
(doc-list overindent, struct-update syntax).
`cli/` `npm run typecheck`: clean.
`cli/` `npx vitest run src/providers.test.ts`: 11 passed.
`tests/compat/` `npm test`: 11 passed / 8 todo (unchanged).
All six CI gates: green against `main`.

## 8. Negative-test coverage

- `provider_kind_parses_all_aliases` — rejects `""`, `"vendoreed"`, etc.
- `null_is_not_allowed_in_prod` — `Null.allowed_in_prod() == false`.
- `has_null_detects_any_null_field` — detects a `Null` in any of the 4 fields.
- TS-side: `parseProviderKind` rejects `noop`/`disabled` (admission-side
  aliases are deliberately CLI-strict to catch typos early).

## 9. Dependency delta

Added one workspace-level crate, one crate-level adoption, zero TS deps:

- `async-trait = "0.1"` — added to workspace `[workspace.dependencies]` and
  consumed by `inference-router/Cargo.toml`. Source: docs.rs/async-trait
  (version 0.1.89 resolved from crates.io). Needed because `async fn` in
  traits still returns an opaque `impl Future` that breaks dyn dispatch —
  we need `dyn MeshProvider` at call sites in Phase 1. No native-Rust
  replacement is stable as of 2026-04-24 (RFC 3668 merged but not yet
  stabilised for object-safety on async traits). **Principle #10 source**:
  <https://crates.io/crates/async-trait/0.1.89> (Apache-2.0/MIT, 500M+
  downloads, maintained by dtolnay).

## 10. Internal-boundary posture

N/A — scaffolding only. No new surface vs. MSFT products. When
`AgtPolicyProvider`/`AgtAuditSink`/`AgtSigningProvider` land in Phase 1
they'll be `Consume` against AGT per `docs/internal-boundaries.md`.

## 11. Sign-offs

- Author sign-off (documented below):
  `Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>`

- Reviewer sign-off: pending user review per the local-only workflow rule.
  Second `Signed-off-by:` line to be added before this branch pushes
  upstream.

---

### Re-audit triggers

- Any implementation (`VendoredMeshProvider`, `AgtMeshProvider`, …) lands →
  separate audit doc.
- `async-trait` version changes → re-audit dep delta.
- The traits evolve (method added / return type changed) → re-audit.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
