# Phase 2 — S12.b: Controller Policy Fetcher (status-only, feature-gated)

**Date:** 2026-04-30
**Slice:** S12.b (`phase2-s12-bd-policy-fetcher`)
**Scope:** Controller-only PR. New `controller/src/policy_fetcher.rs`, an
`AllowlistVerified` Condition surfaced on `ClawSandbox.status`, and the
ACR Workload-Identity token-exchange path. **No** behavior change for
existing deployments — entire path is gated on
`AZURECLAW_FEATURE_SIGNED_ALLOWLIST=1`.

## Existing implementation surveyed

Per plan §0.2 #8, every reused seam:

| Touchpoint | Reused / Extended |
|---|---|
| `controller/src/crd.rs` (S12.a) — `OciArtifactRef`, `NetworkPolicyConfig::allowlist_ref` | Read-only consumer; no schema change. |
| `controller/src/status/mod.rs` — `build_running_status_patch`, `running_status_matches` | Added `_with_extras` variants; old API delegates to new. |
| `controller/src/status/conditions.rs` — `preserve_transition_time`, `reason::*` | Added `TYPE_ALLOWLIST_VERIFIED` and `reason::VERIFIED`. Reused `preserve_transition_time` so `lastTransitionTime` survives same-status reconciles. |
| `controller/src/reconciler/mod.rs` — running-phase status patch site | Single new `await` (`maybe_verify_allowlist`) + extras parameter on the existing patch + matcher. No new K8s resource calls. |
| `controller/Cargo.toml` | New deps: `sigstore = "0.13"`, `oci-client = "0.16"`, `idna = "1"`. |
| `deploy/helm/azureclaw/templates/controller-clusterrole.yaml` | Reviewed — **no change required**. The fetcher reads no Kubernetes resources; it consumes an already-mounted Workload-Identity SA token (via `AZURE_FEDERATED_TOKEN_FILE`) and reaches Entra + the OCI registry over HTTPS. |

**Why a new module rather than extending `crd.rs` or
`reconciler/mod.rs`:** the fetcher carries non-trivial cosign + ACR
trust logic that has its own threat surface and is independently
auditable. Per plan §4.2 the reconciler is at the file budget; isolating
the new code in `policy_fetcher.rs` keeps the slice surgical.

## Trust model summary

Per plan.md "Trust model" for S12:

1. **Cryptographic validity** — cosign signature exists, chains to a
   Fulcio root, certificate not expired at signing time.
2. **Authority** — signer identity (Fulcio cert SAN + issuer claim)
   matches an entry in the cluster `SignerPolicy`. Until S12.d ships
   the ConfigMap watcher, the policy is provisionally read from
   `AZURECLAW_SIGNER_FULCIO_ISSUERS` + `AZURECLAW_SIGNER_SAN_PATTERNS`
   env vars. **Both** must be configured; either alone is unsafe and
   `SignerPolicyConfig::is_configured()` returns `false`.
3. **Replay protection** — canonical bytes carry
   `metadata.generation` (positive monotonic integer); compared by the
   reconciler in S12.e.
4. **Canonical form** — re-validated **after** signature check (sigstore
   only attests bytes, not semantics) per
   `docs/policy-canonical-format.md` rules #1–#13. Any deviation →
   `CanonicalFormViolation`.

## Threat surfaces introduced

| Surface | Detail |
|---|---|
| Network egress to OCI registry | Controller now performs HTTPS GETs to `{registry}/v2/{repo}/...`. With NetworkPolicies on the controller namespace this is allowed by default (controller already calls Entra + ARM); no new egress hole. |
| Federated SA token handling | `acr_token_for_pull` reads `AZURE_FEDERATED_TOKEN_FILE` (already mounted by AKS WI webhook for the existing `fedcred.rs` path). Token is sent **only** to `login.microsoftonline.com/{tenant}/oauth2/v2.0/token` — the Entra token endpoint. Never logged. |
| Sigstore Fulcio trust roots | S12.b builds a sigstore `Client` without a trust repository attached; with empty trust roots the embedded cert in `SignatureLayer.certificate_signature` cannot be verified, so `CertSubject*Verifier` constraints fail closed. S12.d injects `ManualTrustRoot` from cluster state. **The S12.b verifier therefore fails closed by design** — it cannot accept any signature until S12.d wires the trust roots. This is reflected in the plan's "status-only" gating and is documented as the expected outcome. |
| Cache | In-memory `Mutex<HashMap>` keyed on `<registry>/<repository>@<digest>`; entries expire after 1 h. Bounded by the unique-digest count seen during a controller's lifetime (single-digit per cluster in practice). No persistence, no leak across pods. |

## Threat surfaces NOT introduced

- ❌ **No K8s API surface change.** No new CRD, no new RBAC.
- ❌ **No router behavior change.** This slice is controller-only.
- ❌ **No effect on inline `allowedEndpoints`.** NetworkPolicy still derives entirely from the existing path.
- ❌ **No new cosign binary in the controller image.** Verification is in-process via `sigstore-rs`.
- ❌ **No long-lived secrets.** ACR access tokens are obtained on demand and dropped after one pull.
- ❌ **No new persistence.** Cache is in-process, no disk writes.

## Mitigations

- **Feature gate.** The reconciler short-circuits `maybe_verify_allowlist` with `feature_enabled() == false`, before any branching that could run sigstore code. With the gate off, this slice is byte-identical at runtime to S12.a.
- **Fail closed.** `SignerPolicyMissing`, `SignatureVerifyFailed`, `IdentityMismatch`, `CanonicalFormViolation`, and `DigestMismatch` all map to `AllowlistVerified=False/<reason>`. Only `Verified` produces `True`. Status-only — no NetworkPolicy effect either way in S12.b.
- **Transient handling preserves last-known-good.** `FetchError::Transient` returns `None` from `maybe_verify_allowlist`'s downstream selection, and the reconciler uses the prior Condition unchanged (no flap on registry blips).
- **Idempotency.** `running_status_matches_with_extras` compares the new condition's `(type, status, reason)` triple against existing status; only the message can change without forcing a re-patch (and only when the rest of the running shape already matched).
- **`unwrap`/`expect`-free.** All fallible call paths return typed `FetchError` variants. The only `unwrap()` calls are inside `#[cfg(test)]` blocks.
- **`tracing`-only.** No `println!`/`eprintln!`. Lifecycle events at `info!`/`warn!`; cache hits at `debug!`.
- **Ref-shape validation precedes any IO.** `validate_ref_shape` rejects malformed registry/repository/digest/artifactType *before* the cache lookup or any network call.
- **ACR-host gate.** WI exchange is only attempted for `*.azurecr.io|.cn|.us` hosts; other registries (ghcr.io, docker.io, kind-cluster local registries) fall through to `Anonymous`. Operators wanting WI-on-ACR get it; everyone else sees no WI behavior.

## Why not authoritative yet (S12.e gating)

S12.b is **status-only** by design:

1. **Trust roots are not yet provisioned.** Without `SignerPolicy` from S12.d, no signature can be elevated to authority. Failing to a status condition (rather than blocking the NetworkPolicy) is the safe interim shape.
2. **No CLI signing path.** S12.c ships `azureclaw egress … --sign`. Until then, no operator-produced artifacts exist; emitting the condition lets us validate the consumer side against signing pilots.
3. **Drift detection requires both sides.** S12.e's `AllowlistDrift` Condition compares verified canonical bytes against inline `allowedEndpoints`. That comparison ships once both flows exist.

## Test coverage

29 new unit tests in `controller/src/policy_fetcher.rs`:

- `feature_disabled_by_default`, `feature_enabled_when_env_one`, `feature_disabled_for_non_one_truthy_values` — env gate.
- `signer_policy_unconfigured_when_env_unset`, `signer_policy_configured_from_env`, `signer_policy_requires_both_lists_for_is_configured` — SignerPolicy reading.
- `canonical_parser_accepts_valid_artifact` and 9 negative variants (unsorted, duplicate, missing/zero generation, uppercase/wildcard host, out-of-range port, swapped keys, missing trailing newline, top-level key out of order, comments forbidden).
- `validate_ref_shape_*` — ref-shape rejection (bad digest, bad artifactType, uppercase digest, valid).
- `reason_for_error_maps_each_variant` — pins the reason string for every `FetchError` variant (the public contract for status reasons).
- `cache_round_trips_and_expires` — TTL behavior.
- `is_acr_host_recognises_global_clouds` — registry classification.
- `fetch_returns_feature_disabled_when_gate_off`, `fetch_returns_signer_policy_missing_when_no_signer_policy`, `fetch_returns_invalid_ref_for_bad_digest` — top-level entry behavior.
- `acr_token_for_pull_requires_federated_token_env` — WI-exchange env contract.

Test counts:

```
$ cargo test --package azureclaw-controller 2>&1 | grep "test result"
test result: ok. 383 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

(354 pre-S12.b → 383 post; +29 new.)

```
$ cargo clippy --all-targets -- -D warnings
(clean across workspace)
```

E2E tests against a live OCI registry are deferred to S12.e where
authoritative-mode + a kind-cluster registry are wired together.

## References

- **Plan §S12.b** (sub-slice scope): `~/.copilot/session-state/13bea069-bbc9-48ae-a25c-36da81c7a0fe/plan.md`
- **sigstore-rs 0.13.0** — <https://github.com/sigstore/sigstore-rs> (Apache-2.0). Crate features used: `cosign` + `verify` + `rustls-tls`.
- **oci-client 0.16.1** — <https://github.com/oras-project/rust-oci-client> (Apache-2.0). Crate features: `rustls-tls`.
- **ACR OAuth2 token exchange** — <https://learn.microsoft.com/en-us/azure/container-registry/container-registry-authentication-oauth2>
- **Entra ID Workload Identity Federation** — <https://learn.microsoft.com/en-us/entra/workload-id/workload-identity-federation>
- **S12.a audit doc** — `docs/security-audits/2026-04-30-phase2-policyref-schema.md`
- **Canonical format spec** — `docs/policy-canonical-format.md`

## Sign-offs

- [ ] Security review:
- [ ] Owner / merger:


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
