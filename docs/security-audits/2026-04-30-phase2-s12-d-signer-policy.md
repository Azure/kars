# Phase 2 — S12.d: SignerPolicy ConfigMap (Fulcio issuer + SAN allowlist)

**Date:** 2026-04-30
**Slice:** S12.d (`phase2-s12-d-signerpolicy`)
**Scope:** Cluster-scoped ConfigMap that pins the cosign signer
identity policy used by the S12.b egress-allowlist fetcher. Replaces
the env-var path with a watched `azureclaw-signer-policy` ConfigMap in
the controller namespace; env vars remain as an emergency-override
fallback when the ConfigMap is **absent**. Malformed ConfigMaps
**do not** silently fall back — they surface as
`SignerPolicyMalformed` on affected `ClawSandbox` resources.

## Threat model

The fetcher accepts a cosign-signed canonical egress allowlist artifact
as authority over what a sandbox may egress to (status-only in S12.b,
authoritative in S12.e). The signature alone is insufficient: any
identity that controls a Fulcio-trusted OIDC issuer could sign a forged
allowlist. The cluster MUST pin which `(issuer, SAN)` tuples are
considered authoritative.

**Threat:** signer-key misconfiguration → forged allowlist accepted.
Concretely:

1. Operator forgets to install a SignerPolicy → fetcher must fail
   closed (no fallback to "any valid sig is fine").
2. Operator installs a partial / malformed SignerPolicy → fetcher must
   fail closed with a *distinct* signal so the operator notices their
   config is broken (vs. interpreting it as "no policy yet").
3. Attacker tampers with the SignerPolicy → ClusterRole-write privilege
   is required (cluster-scoped object in the controller namespace; only
   cluster-admins or a compromised controller SA can mutate).
4. Attacker tampers with the env-var fallback → requires Deployment-write
   on the controller (same blast radius as RCE on the controller
   itself, which already trumps SignerPolicy integrity).

## Existing implementation surveyed

- **`controller/src/policy_fetcher.rs`** (S12.b) — already houses the
  `SignerPolicyConfig { fulcio_issuers, san_patterns }` value type, the
  `FetchError` enum + `reason_for_error` mapping, the cosign verify
  path (`verify_via_sigstore`), the cache, and the
  `maybe_verify_allowlist` reconciler hook. We extend this module
  rather than parallel-implementing — added `SignerPolicyMalformed`
  variant + reason; rewrote `maybe_verify_allowlist` to consult a
  `SharedSignerPolicy` handle while keeping `fetch_and_verify`'s
  signature unchanged so all existing call paths still compile.
- **`controller/src/main.rs` reconciler-spawn pattern** — every other
  reconciler (`pairing_reconciler::run`, `mcp_server_reconciler::run`,
  `mesh_peer::run`, etc.) is launched as a `tokio::spawn(async move
  { module::run(client).await })` and folded into the top-level
  `tokio::select!`. The new `signer_policy::run` follows the same
  shape; failure of the watcher propagates up so the controller pod
  restarts (avoids serving stale policy state forever after a
  transient kube-apiserver disconnect).
- **`controller/src/leader_election.rs`** — established the
  "`POD_NAMESPACE` env via downward API, fall back to literal
  `azureclaw-system`" idiom. Reused verbatim by the new watcher; the
  Helm `controller-deployment.yaml` was extended to wire
  `POD_NAMESPACE` (it was previously only relied on by leader-election
  *if* set, with a hard-coded fallback — now it's the authoritative
  source for both consumers).
- **`controller/src/status/conditions.rs`** — `TYPE_ALLOWLIST_VERIFIED`,
  `preserve_transition_time`, `reason::*`, `status::FALSE`. The new
  `SignerPolicyMalformed` reason is a string literal returned by
  `reason_for_error` (matches the existing pattern used for
  `SignerPolicyMissing`, `IdentityMismatch`, etc. — none of those are
  `pub const`s in `reason::`, they're match-arm strings).
- **`deploy/helm/azureclaw/templates/rbac.yaml`** — the controller
  `ClusterRole` already grants `get/list/watch` on `configmaps` (the
  per-sandbox AgentCard / claweval ConfigMap reconciliation needs it
  cluster-wide). Adding the SignerPolicy ConfigMap watch did NOT
  require broadening RBAC — namespace-scoped role would have been
  preferred for least-privilege, but a cluster-scoped rule the
  controller already holds is strictly broader. We deliberately did
  not introduce a new namespace-scoped Role + RoleBinding because two
  RBAC sources for the same verb on the same resource would be
  confusing audit-trail noise.
- **`deploy/helm/azureclaw/templates/*-configmap.yaml` rendering
  idiom** — `crd-*.yaml`, `cilium-*.yaml`, etc. follow `kind: X` →
  metadata with `app.kubernetes.io/name: azureclaw` + a `component:`
  label and `namespace: {{ .Release.Namespace }}`. Reused.

## Mitigations

- **Cluster-scoped ConfigMap.** Single authoritative source per
  cluster; no per-tenant slicing of trust roots. Tampering requires
  ClusterRole-write on the controller namespace.
- **Controller-only watch.** The watcher is filtered by
  `metadata.name=azureclaw-signer-policy` *and* scoped to the
  controller's own namespace via `Api::namespaced(client, &ns)` —
  even a compromised tenant namespace cannot mint a SignerPolicy that
  the controller would consume.
- **Namespace-scoped RBAC.** No broadening was needed; the controller
  already holds `get/list/watch configmaps` cluster-wide. We
  documented this in the audit so a future least-privilege pass knows
  the SignerPolicy doesn't *require* cluster-scope; if/when sandbox
  ConfigMap ops are scoped down, this watch can be migrated to a
  per-namespace Role on `azureclaw-system`.
- **Distinct malformed-detection condition.** `SignerPolicyMalformed`
  is a separate `Condition.reason` from `SignerPolicyMissing`. A
  malformed ConfigMap does **not** silently fall back to env vars —
  operators get a hard signal (`AllowlistVerified=False/SignerPolicyMalformed`)
  on every affected `ClawSandbox` so the broken cluster config
  surfaces in `kubectl get clawsandbox -o yaml`.
- **Env fallback as emergency override.** `AZURECLAW_SIGNER_FULCIO_ISSUERS`
  / `AZURECLAW_SIGNER_SAN_PATTERNS` remain readable. They activate
  *only* when the ConfigMap is absent — never when it's present-but-broken
  (test `with_handle_malformed_does_not_fall_back_to_env`). The path
  exists for cluster-bootstrap scenarios where the SignerPolicy CRD
  install hasn't landed yet, and for break-glass overrides on
  customer clusters.
- **Fail-closed ordering preserved.** All three S12.b fail-closed
  semantics still hold:
  - SignerPolicy missing → `SignerPolicyMissing`.
  - Empty `fulcio_issuers` AND empty `san_patterns` → still missing
    (parser rejects either-empty up-front; runtime
    `is_configured()` short-circuits on either-empty).
  - Feature gate off → no condition emitted (no behavior change for
    deployments that haven't opted in).
- **Strict parser.** `parse_configmap` rejects: missing `data` block,
  missing either key, empty list after trimming + comment-stripping,
  non-URL issuer entries, whitespace inside SAN entries. Each
  rejection has a unit test.

## Watch semantics

`signer_policy::run` consumes `kube::runtime::watcher::Event<ConfigMap>`
events filtered to `metadata.name=azureclaw-signer-policy`:

- `Init` + `InitApply` + `InitDone` — atomic rebuild on watch restart;
  if the singleton wasn't observed during the init pass, state is
  cleared to `Absent` (so a controller restart against an empty
  cluster never serves a stale policy).
- `Apply` — re-parse + commit. Parse-error → `Malformed(msg)` (no
  fallback).
- `Delete` — clear to `Absent` → env-var fallback re-engages on the
  next reconcile.

## Test coverage

18 new tests in the controller (`controller/src` test count: 383 →
401):

- `controller/src/signer_policy.rs` (13 tests):
  `parse_valid_configmap`, `parse_strips_comments_and_blank_lines`,
  `parse_rejects_missing_data_block`,
  `parse_rejects_missing_fulcio_issuers_key`,
  `parse_rejects_missing_san_patterns_key`,
  `parse_rejects_empty_issuer_list_after_trimming`,
  `parse_rejects_non_url_issuer`,
  `parse_rejects_san_with_whitespace`,
  `shared_apply_then_snapshot_reflects_configmap`,
  `shared_apply_with_malformed_sets_malformed_state`,
  `shared_clear_returns_to_absent`,
  `shared_default_is_absent`,
  `shared_clone_shares_state`.
- `controller/src/policy_fetcher.rs` (5 new tests + 1 augmented
  reason-mapping test):
  `with_handle_returns_none_when_feature_gate_off`,
  `with_handle_malformed_emits_signer_policy_malformed_condition`,
  `with_handle_malformed_does_not_fall_back_to_env`,
  `with_handle_absent_falls_back_to_env_missing`,
  `with_handle_configmap_takes_precedence_over_env`.

The existing env-fallback tests (`signer_policy_unconfigured_when_env_unset`,
`signer_policy_configured_from_env`, `signer_policy_requires_both_lists_for_is_configured`)
were **kept unchanged** — the env path remains a supported fallback
and the tests still document its semantics. New SharedSignerPolicy
tests inject state directly per the slice spec ("pure value injection
is cleaner") rather than wiring fake ConfigMap apparatus.

## §10.5 Compliance

- §10.5 #4 (signed-policy provenance): identity-pinned authority now
  has a cluster-resident, watched, fail-closed configuration source.
  Combined with S12.b (consumer verify) and S12.c (producer sign),
  the cluster has end-to-end coverage of "the bytes the operator
  sealed are the bytes the controller verifies — under an authority
  the cluster operator has explicitly trusted." S12.e now has a
  reliable `SignerPolicy` to flip authority against.
- Per-sub-slice audit doc: this is the fourth (S12.a, S12.b, S12.c,
  S12.d).

## References

- Plan §S12.d: `~/.copilot/session-state/13bea069-bbc9-48ae-a25c-36da81c7a0fe/plan.md`
- Canonical format: `docs/policy-canonical-format.md`
- S12.b audit: `docs/security-audits/2026-04-30-phase2-policy-fetcher.md`
- S12.c audit: `docs/security-audits/2026-04-30-phase2-s12-c-cli-sign.md`
- Sigstore Fulcio: <https://docs.sigstore.dev/certificate_authority/overview/>
- Workload identity federation: <https://learn.microsoft.com/entra/workload-id/workload-identity-federation>

## Sign-offs

- [ ] Author: ____________________
- [ ] Independent reviewer: ____________________
