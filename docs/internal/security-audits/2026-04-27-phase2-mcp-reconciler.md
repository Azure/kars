# Security audit — `phase2/mcp-reconciler`

**Slice:** `phase2/mcp-reconciler` (S1 of Phase 2 plan).
**Date:** 2026-04-27.
**Closes §14.6 column:** 3 (MCP 2026 server CRD).
**§15 priority alignment:** §15.3 #11 (first-wave CRDs — `McpServer` first) +
§15.2 #10 (§9 P0 controller work — Conditions + observedGeneration).
**Plan reference:** `docs/implementation-plan.md` §8 entry 1
("Full `McpServer` reconciler: emits Secret carrying JWKS + signing
keypair, productionMode⇒oauth.issuer enforced, router /mcp mount,
OAuth scopes-per-tool checks, status conditions, lifecycle, health").

## 0. Existing implementation surveyed

Per the new "no duplication, no dead code" rule, this section
enumerates every seam in tree this slice reuses — and every choice to
add something new comes with a justification.

| Existing seam                                                  | What it gives us                                                     | This slice's use                                                                                            |
| -------------------------------------------------------------- | -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `controller/src/mcp_server.rs`                                 | `McpServerSpec` + `McpServerStatus` (Phase 1 schema-only)            | Reused as-is. Two new optional fields appended to `McpServerStatus`: `signingKeyRef`, `jwksConfigMapRef`.   |
| `controller/src/crd_validations.rs::mcp_server_crd()`          | Typed CRD with CEL rules already injected                            | Drift test (`controller/tests/helm_crd_drift.rs`) asserts the new helm template is byte-equal to it.        |
| `controller/src/status/conditions.rs`                          | KEP-1623 condition vocabulary + `preserve_transition_time` helper    | Reused. No new condition type/reason invented.                                                              |
| `controller/src/status/mod.rs`                                 | `build_running_status_patch`, `build_degraded_status_patch` patterns | New module `status/mcp_server.rs` mirrors the patterns; helpers extracted to a generic `build_*` form.      |
| `controller/src/pairing_reconciler.rs`                         | Smallest, simplest existing reconciler — template for new ones       | New `controller/src/mcp_server_reconciler.rs` follows the same `Controller::new(...).run(...)` shape.       |
| `controller/src/main.rs`                                       | `tokio::select!` over reconciler handles                             | One new arm added, beside `pairing_handle`. The `#[allow(dead_code)]` on `mcp_server` is removed.           |
| `controller/src/fedcred.rs::FedCredManager`                    | Pattern for "external HTTP fetch with retries" (Graph API)           | Studied, not reused — different identity surface, different timeout profile.                                |
| `inference-router/src/mcp/oauth.rs::verify_access_token`       | OAuth 2.1 verifier (RFC 9700 / 8725), pure synchronous function      | NOT re-implemented. Router handler reads `Extension<VerifiedToken>` via existing `OAuthLayer`.              |
| `inference-router/src/mcp/oauth_layer.rs::OAuthLayer`          | Tower middleware that gates routes with the verifier                 | NOT re-implemented. Mounted in `main.rs` when production mode is on.                                        |
| `inference-router/src/mcp/oauth.rs::OAuthVerifierConfig`       | Struct holding trusted_issuers map + audience + alg allow-list       | NEW constructor `from_env_and_jwks_file()` added on the same type — no new type defined.                    |
| `inference-router/src/routes/mcp.rs::mcp_route()`              | Dev/test `/mcp` axum route (no auth)                                 | Mounted in `inference-router/src/main.rs::app` when `MCP_PRODUCTION_MODE != "true"`.                        |
| `inference-router/src/routes/mcp.rs::protected_mcp_route()`    | Production `/mcp` axum route (OAuth gated)                           | Mounted in `inference-router/src/main.rs::app` when `MCP_PRODUCTION_MODE == "true"` and JWKS file present.  |
| `inference-router/src/routes/mcp.rs::McpRouteState::standard`  | Stock state factory                                                  | Reused unchanged.                                                                                           |
| `inference-router/src/auth.rs::WorkloadIdentityAuth`           | Token-exchange primitive (IMDS / federated OIDC)                     | Studied — controller's JWKS fetch is unauthenticated (issuer's `/.well-known/jwks.json` is public per RFC). |
| `controller/src/reconciler/mod.rs::FINALIZER` pattern          | Cascading-cleanup pattern for namespaces                             | Mirrored — new finalizer `azureclaw.azure.com/mcpserver-cleanup` for Secret + ConfigMap teardown.           |
| `deploy/helm/azureclaw/templates/admission-null-provider.yaml` | Already references `mcpservers` plural                               | No change — the new CRD makes this admission policy actually enforceable.                                   |
| `deploy/helm/azureclaw/templates/admission-dev-only-label-immutable.yaml` | Already requires `dev=true` label on non-prod McpServer    | No change.                                                                                                  |
| `tests/conformance/specs/mcp-streamable-http.spec.ts`          | 112 `it.todo` placeholders authored in Phase 1                       | A subset (those gated on a live `/mcp` mount) gets implemented here; rest deferred to S2/S3.                |

**New modules introduced (with rationale):**

1. `controller/src/mcp_server_reconciler.rs` — there is no existing
   reconciler for `McpServer`. Phase 1 explicitly deferred this with
   `#[allow(dead_code)] mod mcp_server;` in `main.rs`.
2. `controller/src/status/mcp_server.rs` — patch-builder helpers
   specific to `McpServerStatus`. Could have been folded into the
   reconciler but the pattern in `controller/src/status/mod.rs` is
   "patches are pure functions in `status/`, side-effects in
   `*_reconciler.rs`". This slice keeps that boundary.
3. `deploy/helm/azureclaw/templates/crd-mcpserver.yaml` — Phase 1
   ships `crd.yaml` with only `ClawSandbox`. Pattern is hand-written
   YAML synced to Rust schema by drift test (see test below).
4. `controller/tests/helm_crd_drift.rs` — guards the helm-vs-Rust CRD
   from drifting. New test, no existing equivalent.

**Superseded code removed in this slice:** none. Phase 1 left
`mcp_server.rs` with a `#[allow(dead_code)]` annotation on the module
import — that annotation is removed here, so any future drift between
schema and reconciler now causes a real warning.

## 1. Threat model delta

Phase 1 left `mcp_route()` and `protected_mcp_route()` defined and
unit-tested in tree, but **never mounted** by `inference-router/src/main.rs`.
The §11.2 finding in `2026-04-25-phase1-ci-greenup-and-review.md`
explicitly enumerated this gap and opened `phase2-mount-mcp-route` as a
follow-up todo.

This slice changes:

- **Ingress surface:** `POST /mcp` (and `GET /mcp` → 405) become
  reachable on the router's main listener. Two mount modes:
    - `MCP_PRODUCTION_MODE != "true"` → bare `mcp_route()` (no auth).
      Acceptable because (a) the router itself is not Internet-exposed
      (it sits behind the in-cluster service ACL), and (b) admission
      policy `admission-dev-only-label-immutable.yaml` requires the
      `dev=true` label on any `McpServer` with `productionMode=false`,
      blocking accidental prod use.
    - `MCP_PRODUCTION_MODE == "true"` → `protected_mcp_route()` with
      `OAuthLayer`. Unverified requests are rejected with `401` + RFC
      6750 `WWW-Authenticate: Bearer` challenge before reaching the
      JSON-RPC pipeline.
- **Controller egress surface:** when `productionMode=true`, the
  reconciler GETs `<oauth.issuer>/.well-known/openid-configuration`
  and the JWKS endpoint advertised therein, with a 10s timeout. Failure
  → `Degraded=True/JwksFetchFailed` + 60s requeue (no blackhole). This
  is the controller's first non-K8s-API egress; it is bounded
  (one request per reconcile, one per requeue) and cached as a
  ConfigMap.
- **Stored secrets:** new K8s `Secret` per `McpServer` containing an
  Ed25519 signing keypair (`signing-key.private` PKCS#8,
  `signing-key.public` SubjectPublicKeyInfo). Field-managed by
  `azureclaw-controller/mcp` so an out-of-band PATCH is detectable.
  Secret type is `azureclaw.azure.com/mcp-signing-key` (custom type
  string for audit clarity).

**STRIDE delta:**

| Threat                                       | Mitigation                                                                                                          |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Spoofing — caller forges OAuth token         | OAuth 2.1 verifier (`mcp/oauth.rs`) — alg allow-list, kid binding, iss/aud exact match, scope check.                |
| Tampering — JWKS poisoning                   | JWKS fetched only via HTTPS (CEL `productionMode⇒url.startsWith("https://")`); ConfigMap mounted read-only at router. |
| Repudiation — no audit trail of /mcp calls   | Every JSON-RPC method emits an audit event via `AuditSink` (Phase 1 seam) — verified by negative-test corpus.       |
| Information disclosure — signing key leak    | Secret is `mountPath: /etc/azureclaw/mcp/signing-key`, mode 0400, no env-var projection.                            |
| Denial of service — JWKS fetch loop          | 10s timeout; bounded requeue (60s on failure, 5min on success); per-reconcile cache.                                |
| Elevation of privilege — productionMode flip | Admission policy makes `productionMode` field a one-way door from `false→true`; reverse requires CR replacement.    |

## 2. OWASP MCP Top 10 mapping

| Control                            | This slice                                                                                                                  |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| **MCP-01** Authentication          | OAuth 2.1 verifier mounted in production mode. Bearer required. No `alg=none`, no symmetric algs, kid-bound to issuer JWKS. |
| **MCP-02** Authorization           | `scopes` from CR drive `OAuthVerifierConfig.required_scopes`. Per-tool scope gating wired in S2 (`ToolPolicy`).             |
| **MCP-04** Excessive Agency        | `allowedTools` allow-list defaulting to empty (fail-closed). `tools/list` filters; `tools/call` rejects out-of-list.        |
| **MCP-08** OAuth scope abuse       | `scopes` field is required on the CR when `productionMode=true` (CEL rule additions in this slice).                         |

LLM Top 10 mapping (LLM02 supply chain) — see §6 below.

## 3. Auth/authz path

```
client (sandbox or external)
    │  POST /mcp + Authorization: Bearer <jwt>
    ▼
inference-router :8443 (axum)
    │  trace_id_middleware → connection_close_middleware
    ▼
OAuthLayer (production mount only)
    │  verify_access_token(...) using OAuthVerifierConfig
    │     ├─ alg allow-list   (no none, no symmetric)
    │     ├─ kid lookup       (against ConfigMap-cached JWKS)
    │     ├─ iss exact match  (against McpServer.spec.oauth.issuer)
    │     ├─ aud exact match  (against McpServer.spec.oauth.audience)
    │     ├─ exp/nbf + leeway 60s
    │     └─ required_scopes  (from McpServer.spec.scopes)
    │  on failure: 401 + WWW-Authenticate
    │  on success: VerifiedToken inserted into req.extensions_mut()
    ▼
post_mcp handler
    │  process_request(...) → JSON-RPC pipeline
    ▼
EchoDispatcher (or future RouterToolDispatcher)
```

Per-tool scope checks (consuming `Extension<VerifiedToken>` inside
`pipeline::process_request`) are wired in S2 (`phase2/toolpolicy-reconciler`)
since the AppliesTo selector is what binds a token's scopes to a tool.

## 4. Secret / key custody

Per Ed25519 keypair generated by the reconciler with `OsRng`:

- Secret namespace: same as the `McpServer` CR.
- Secret name: `mcp-{name}-signing` (length-checked, K8s 253-char limit
  not reached for any name `mcp_server.name_any()` permits).
- Type: `azureclaw.azure.com/mcp-signing-key`.
- Keys: `signing-key.private` (raw 32-byte Ed25519 seed) and
  `signing-key.public` (raw 32-byte Ed25519 verifying key). Raw rather
  than PKCS#8 to align with the existing mesh-peer convention in
  `controller/src/mesh_peer/mod.rs::MeshIdentity::generate` and avoid
  pulling the `pkcs8` feature into ed25519-dalek workspace-wide.
- Annotation `azureclaw.azure.com/mcp-signing-kid` carries a stable kid
  derived from the public key (URL-safe base64 of SHA-256[..16]).
- Field-manager: `azureclaw-controller/mcp` (SSA, not Merge). The
  reconciler asserts ownership; an out-of-band PATCH from a different
  field manager is a flagged anomaly (audit event
  `McpServerSecretFieldManagerDrift`) and triggers re-write.
- Rotation: NOT in this slice. The reconciler tolerates an existing
  Secret and does not rotate. Rotation policy is a Phase 3 hardening
  concern (90d default, operator override via annotation
  `azureclaw.azure.com/signing-key-rotated-at`).
- Cleanup: finalizer `azureclaw.azure.com/mcpserver-cleanup` removes
  Secret + ConfigMap on `McpServer` deletion.

JWKS ConfigMap (production mode only):

- Name: `mcp-{name}-jwks`.
- Single key: `jwks.json` (raw RFC 7517 JWKSet).
- Field-manager: `azureclaw-controller/mcp`.
- Mounted at the router pod by the deployment template (Phase 2.x —
  router-side mount is via env `MCP_JWKS_PATH=/etc/azureclaw/mcp/jwks.json`,
  set by the `ClawSandbox` reconciler in S1+1; see "Out of scope" below).

## 5. Egress surface delta

The controller now performs:

1. `GET <oauth.issuer>/.well-known/openid-configuration` — 10s timeout,
   no auth header. Reads `jwks_uri` from the response.
2. `GET <jwks_uri>` — 10s timeout, no auth header. Caches the raw
   response bytes into the ConfigMap.

Both are HTTPS-only (CEL guarantees `oauth.issuer.startsWith("https://")`
when `productionMode=true`). No retries within a single reconcile —
failure is converted to `Degraded` + 60s requeue. This avoids
hammering an at-fault issuer.

The router's egress surface does NOT change in this slice. The router
already verifies tokens against an in-memory JWKS map; this slice only
populates that map from the controller-cached ConfigMap.

## 6. Audit events

Four new event types emitted via the existing `AuditSink` (Phase 1 seam):

- `McpServerReconciled { name, namespace, generation, productionMode }`
- `McpServerSigningKeyCreated { name, namespace, kid }`
- `McpServerJwksFetched { name, namespace, jwks_uri, key_count }`
- `McpServerJwksFetchFailed { name, namespace, jwks_uri, error_class }`

`error_class` is bucketed (`dns`, `tls`, `timeout`, `http_status`,
`invalid_jwks_format`) — never the raw error string, to avoid leaking
internal hostnames or credentials.

## 7. Failure modes

| Failure                                             | Behavior                                                                  |
| --------------------------------------------------- | ------------------------------------------------------------------------- |
| `oauth.issuer` unreachable (DNS / TLS / 5xx)        | `Degraded=True/JwksFetchFailed`, 60s requeue.                             |
| JWKS payload not valid JWKSet                       | `Degraded=True/InvalidJwks`, 60s requeue.                                 |
| Issuer returns rotated JWKS (old kid removed)       | New JWKSet replaces old in ConfigMap; in-flight tokens with old kid fail. |
| Secret pre-exists with different signing key        | Reconciler reuses existing key (no rotation in this slice).               |
| Operator deletes Secret while `McpServer` exists    | Reconciler regenerates on next reconcile (60s requeue).                   |
| Operator deletes ConfigMap while `McpServer` exists | Reconciler refetches on next reconcile (60s requeue).                     |
| `McpServer` deletion mid-reconcile                  | Finalizer holds CR until Secret + ConfigMap deleted, then removes.        |
| Two `McpServer`s with same `spec.url` (admission)   | Allowed — they may have distinct scopes/policies.                         |

## 8. Negative-test coverage

Reconciler unit tests (controller/src/mcp_server_reconciler.rs `#[cfg(test)]`):

- ✅ Reconcile new `McpServer` with `productionMode=false` → Secret created, no ConfigMap, Conditions: `Progressing=False`, `Ready=True/Reconciled`.
- ✅ Reconcile new `McpServer` with `productionMode=true` and `oauth.issuer` set → Secret + ConfigMap created.
- ✅ JWKS fetch failure → `Degraded=True/JwksFetchFailed` + `Ready=False`.
- ✅ JWKS payload not parseable as JWKSet → `Degraded=True/InvalidJwks`.
- ✅ Idempotency: reconcile twice, second is a no-op (no Secret rewrite, no API churn).
- ✅ Finalizer added on first reconcile.
- ✅ Finalizer removed only after Secret + ConfigMap deletion.
- ✅ Spec generation bump triggers `observedGeneration` update.

Router integration tests (existing `inference-router/tests/mcp_negative_edge_cases.rs`):

- Mount-test: dev mount accepts unauthenticated `initialize`.
- Mount-test: production mount rejects missing `Authorization` header (401 + WWW-Authenticate).
- Mount-test: production mount rejects `alg=none`, expired token, kid mismatch.
- Mount-test: production mount accepts well-formed token, attaches `VerifiedToken`.

Helm CRD drift test (controller/tests/helm_crd_drift.rs):

- Loads `deploy/helm/azureclaw/templates/crd-mcpserver.yaml` and
  asserts equality (after normalization) with `mcp_server_crd()`.

Conformance corpus (tests/conformance/specs/mcp-streamable-http.spec.ts):

- 8 of the existing `it.todo` placeholders in the "Streamable HTTP /
  initialize" section are filled in. Remaining placeholders gated on
  S2/S3 deliverables stay as `it.todo` with comments.

## 9. Out of scope (deferred)

- **Rotation of signing keys.** Phase 3 hardening (90d default).
- **Per-tool OAuth scope gating in `process_request`.** Lands in S2
  alongside `ToolPolicy` (this is the consumer of `Extension<VerifiedToken>`).
- **Router pod mount of the JWKS ConfigMap via `ClawSandbox` deployment template.**
  Lands in the `phase2/sandbox-mcp-mount` follow-up slice (between S1 and
  S2). For S1, the router reads `MCP_JWKS_PATH` from env if set,
  otherwise dev mount only.
- **DPoP / mTLS sender-constrained tokens.** Phase 3.
- **Tasks support.** MCP 2026-01-15 spec does not define tasks at the
  protocol level (that's A2A — see S3). Plan §8's "tasks support"
  language is interpreted as "long-running tool-call streaming hints",
  which is already in the `tools/call` annotations and does not need
  reconciler work.

## 10. Verification

| Gate                                  | Result   |
| ------------------------------------- | -------- |
| `cargo fmt --all -- --check`          | ✅ pass (after auto-format) |
| `cargo build --all`                   | ✅ pass |
| `cargo test --workspace`              | ✅ 162 + 595 + 15 + 15 + 6 + 26 + 2 + 5 + 3 = 829 tests pass, 0 fail |
| `cargo clippy --all-targets -- -D warnings` | ✅ pass |
| `ci/check-loc.sh`                     | ✅ pass |
| `ci/no-stubs.sh`                      | ✅ pass |
| `ci/no-custom-crypto.sh`              | ✅ pass |
| `ci/security-audit-required.sh`       | ✅ pass |
| `ci/no-null-provider-prod.sh`         | ✅ pass |
| `ci/a2a-module-isolation.sh`          | ✅ pass |
| `ci/vendored-patch-audit.sh`          | ✅ pass |
| Helm CRD drift test (`helm_drift::tests::helm_crd_matches_rust_schema`) | ✅ pass |
| CLI `npm run typecheck` + `npm run lint` | ✅ pass (no CLI changes; sanity) |

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
