# Security Audit — phase1/signing-provider-in-tree

**Date:** 2026-04-25
**Branch:** `phase1/signing-provider-in-tree`
**Base:** `origin/dev`
**Author:** Copilot
**Reviewer:** Pál Lakatos-Tóth

## Scope

Wires the `SigningProvider` four-seam contract to a real in-tree
implementation on `Governance`, mirroring the `PolicyDecisionProvider`
and `AuditSink` landings. Introduces `providers/signing_impl.rs`,
`routes/signing_ops.rs`, and migrates the first call-site
(`POST /agt/handoff/succession`) from direct field access
(`state.governance.identity.sign(...)`) to the typed trait.

**Files touched:**

- `inference-router/src/providers/signing_impl.rs` (new, 141 LOC incl. tests)
- `inference-router/src/providers/mod.rs` (re-exports `SigningError`, `DEFAULT_KEY_REF`)
- `inference-router/src/routes/signing_ops.rs` (new, 19 LOC)
- `inference-router/src/routes/mod.rs` (+6 LOC — `signing_provider` field, import,
  module decl, init)
- `inference-router/src/routes/handoff.rs` (+14 LOC — migrates signing call-site
  and adds a real 500-response path when signing fails)
- `inference-router/tests/agt_governance_integration.rs` (+1 field in test helper)
- `docs/security-audits/2026-04-25-phase1-signing-provider-in-tree.md` (this file)

## Threat model

| Threat | Mitigation in this PR |
|--------|-----------------------|
| Key material leaking across the boundary (§1.2) | Trait accepts only an opaque `KeyRef`; the raw `SigningKey` stays private to `agentmesh::AgentIdentity`. |
| Unknown `KeyRef` silently succeeds | `SigningProvider::sign` returns `Err(UnknownKey)` for any `KeyRef` that isn't `"agent:default"` or the agent's own DID. Verified in unit tests. |
| Tampered payload accepted as valid | Relies on upstream `AgentIdentity::verify` (thin wrapper over `ed25519-dalek::Verifier`). Unit test `verify_rejects_tampered_payload` confirms `false` is returned. |
| Signature from a different agent accepted | Covered by `verify_rejects_signature_from_different_identity`. Two `Governance` instances in the test — the second's signature must not verify against the first. |
| Wrong-length signature bytes (64-byte Ed25519) | `AgentIdentity::verify` returns `false` for any non-64-byte input; test `verify_rejects_wrong_length_signature` pins this contract. |
| Signing backend outage causes silent unsigned request | The migrated `handoff:succession` now responds `500 {"error":"signing backend unavailable"}` and logs `tracing::error!`. Failing closed is correct here — an unsigned succession would be rejected downstream by the registry anyway, and falling open would risk replay. |
| Hand-rolled crypto slipping in | `ci/no-custom-crypto.sh` enforces. The new module imports no crypto primitives; all bytes pass through `AgentIdentity::{sign,verify}`. |
| Legacy call-sites bypass the trait | The first site was migrated; the remaining direct `state.governance.identity.sign(...)` call-sites (none today beyond the one migrated) migrate incrementally. §4.3 blast-radius rule is respected. |
| `Arc<dyn SigningProvider>` breaks memory-sharing with other traits | All four of `AppState.governance`, `policy_provider`, `audit_sink`, `signing_provider` are `Arc::clone` views of the same `Arc<Governance>` — same key material, no duplication. Test `arc_dyn_signing_provider_coercion_works` pins the coercion. |

## Tests added

Seven async unit tests in `providers/signing_impl.rs`:

1. `sign_and_verify_round_trip_default_ref`
2. `did_is_also_accepted_as_key_ref`
3. `unknown_key_ref_fails_both_sign_and_verify`
4. `verify_rejects_tampered_payload`
5. `verify_rejects_wrong_length_signature`
6. `verify_rejects_signature_from_different_identity`
7. `arc_dyn_signing_provider_coercion_works`

Workspace totals: **373 tests green** (up from 366).

## CI gates

- [x] `ci/check-loc.sh`
- [x] `ci/no-stubs.sh`
- [x] `ci/no-custom-crypto.sh`
- [x] `ci/no-null-provider-prod.sh`
- [x] `ci/vendored-patch-audit.sh`
- [x] `ci/security-audit-required.sh`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --all`

## Out of scope

- AGT SDK-backed `AgtSigningProvider` (separate branch).
- Multi-key stores / signed-prekey rotation / delegate-child keypairs.
- Verifying signatures produced by *other* agents (would require a
  keystore of peers' `VerifyingKey`s; not needed by any current
  call-site).
- Migrating further `.identity.sign(...)` call-sites (only one exists
  today; if future code adds more, they migrate through
  `routes/signing_ops.rs` helpers).

---

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pál Lakatos-Tóth <pallakatos@microsoft.com>
