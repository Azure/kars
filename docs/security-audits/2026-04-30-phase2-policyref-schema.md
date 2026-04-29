# Phase 2 — S12.a: Policy Reference Schema + Canonical Egress Allowlist Format

**Date:** 2026-04-30
**Slice:** S12.a (`phase2-s12-a-policyref-schema`)
**Scope:** Pure schema PR. CRD addition + canonical format spec. **No** controller, CLI, or runtime behavior change.

## Summary

Adds the `OciArtifactRef` shape to `controller/src/crd.rs` and surfaces a new optional field `ClawSandbox.spec.networkPolicy.allowlistRef` of that shape. Defines the byte-stable canonical format for the v1 egress allowlist artifact in `docs/policy-canonical-format.md`.

This is the foundation for the S12 supply-chain-grade policy work; it ships before any code that produces or consumes signed artifacts so that:

1. The schema lands first and stabilizes (no churn in subsequent slices).
2. The canonical format is reviewable in isolation, before signing tooling is built on top of it.
3. Existing CRs without `allowlistRef` round-trip unchanged (`skip_serializing_if = "Option::is_none"`).

## Files Changed

| File | Change |
|---|---|
| `controller/src/crd.rs` | New `OciArtifactRef` struct (camelCase, `JsonSchema`, `PartialEq + Eq`); new optional `allowlist_ref` field on `NetworkPolicyConfig`. |
| `controller/src/crd.rs` (tests) | New `allowlist_ref_round_trips_through_camel_case_json` and `allowlist_ref_omitted_when_none` tests; existing `default_network_policy_denies_all` extended. |
| `deploy/helm/azureclaw/templates/crd.yaml` | New `allowlistRef` sub-schema under `networkPolicy`. Required-field validation, digest regex pattern. |
| `docs/policy-canonical-format.md` | New. Defines canonicalization rules, signing/verification flow, and forward-compatibility strategy for the egress allowlist v1 artifact. |

## Threat-Model Implications

S12.a alone is informational — the field is read by no consumer yet. The threat surface is therefore limited to:

- **Schema spoofing:** an operator could populate `allowlistRef` with a bogus reference. Today this is harmless because no code reads it; once S12.b ships behind `AZURECLAW_FEATURE_SIGNED_ALLOWLIST` the controller will surface verification failures as `AllowlistVerified=False`. Authority is enforced in S12.d (identity pinning), not here.
- **Replay protection:** the canonical document carries `metadata.generation` (positive monotonic integer). Replay protection at the controller side (S12.e) compares incoming `generation` to last-observed; never relies on Rekor freshness. The schema does not yet enforce this — S12.b validators will.
- **Drift / wire format:** rules #1–#13 of the canonical-format doc eliminate equivalence ambiguity. Two operators producing the same logical allowlist always produce the same digest. Rule #7 (IDNA 2008 + lowercase) prevents homograph-style policy spoofing.

## What This Slice Does NOT Do

By design, none of these are in S12.a:

- ❌ No CLI flag, no signing.
- ❌ No controller fetch or verification.
- ❌ No `SignerPolicy` ConfigMap.
- ❌ No router behavior change (blocked-domain capture is S12.f).
- ❌ No effect on existing `allowedEndpoints` precedence.

These land in S12.b–S12.g, each with its own audit doc.

## Test Evidence

```
$ cargo test --package azureclaw-controller -- crd::
running 36 tests
....................................
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 318 filtered out

$ cargo clippy --package azureclaw-controller --all-targets -- -D warnings
(clean)
```

The new tests (`allowlist_ref_round_trips_through_camel_case_json`, `allowlist_ref_omitted_when_none`) lock in:

- camelCase JSON wire format (`artifactType`, not `artifact_type`)
- Backwards-compat: `NetworkPolicyConfig::default()` does not emit `allowlistRef`

## Migration / Rollback

- **In-place v1alpha1 schema edit** (we are pre-release per S13's plan). No conversion webhook needed.
- **Rollback** is trivial: revert this PR; no live data references the field yet.
- **Forward compat** for v2: future artifactTypes bump the `+yaml` suffix; consumers select codec by media-type (see canonical-format doc §"Forward compatibility").

## §10.5 Compliance

- §10.5 #4 (signed-policy provenance): foundation laid; full coverage delivered across S12.a–S12.g.
- Per-sub-slice audit docs are mandatory; this is the first.

## References

- Plan §S12 (post-critique decomposition): `~/.copilot/session-state/.../plan.md`
- ADR-0001 step #4 (gateway component) — independent; this slice doesn't touch the public-edge path.
- Rubber-duck critique 2026-04-30: trust model + decomposition adopted; in-sandbox cosign rejected as out-of-scope (controller-side verification only).
