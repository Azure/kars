# Security Audit: cluster-aware memory scope + policy quintet round-out

**Capability:** Land previously-stashed WIP plus broader in-flight session
work (Slice 4d.4.1 outbound static-bearer, MCP CRD lane fixes, dev-flow
MCP prompts, OpenClaw plugin refinements, cluster-qualified memory scope).
PR: #323.

## 1. Summary

This audit covers the cumulative `main..HEAD` diff of PR #323 (~1500 LOC,
29 files). The change set was sliced out of the prior session and lands
as a single squash per session checkpoint 386 ("one squash" plan)
because the changes are tightly interleaved across the controller,
inference-router, sandbox runtime, and CLI — splitting would require
unweaving several intertwined env-var / ConfigMap-mount / policy-echo
seams that all reference each other.

## 2. Scope

### 2.1 Cluster-aware memory scope (headline)

- **Controller** (`reconciler/mod.rs`): `Context.cluster_name: Option<String>`
  populated from `CLUSTER_NAME` env. Injects `SANDBOX_NAME` (always) +
  `CLUSTER_NAME` (when set) onto the openclaw container so the in-process
  memory tool can compute a deterministic, cluster-qualified scope
  (`agent:${CLUSTER_NAME}/${SANDBOX_NAME}`) instead of falling back to
  `HOSTNAME` (pod hash).
- **OpenClaw plugin** (`memory-binding.ts` new): single resolver function
  that the Foundry Memory tool consumes; no new crypto, no new network.
- **CLI dev/local-k8s**: passes `--set meshPeer.clusterName=<kind>` to
  helm install so local kind clusters get the same scoping as AKS.

**Threat model**: Prevents cross-cluster memory bleed when a single
Foundry project is shared between AKS + local kind (developer
convenience). Memory writes were already RBAC-gated on the project MI;
this fix tightens the **scope key** so two agents named "alice" in
two clusters cannot read each other's memories.

### 2.2 MCP CRD lane

- **Controller** (`mcp_server.rs`, `mcp_server_reconciler.rs`): adds
  `bearerFromEnv` field for Slice 4d.4.1 outbound static-bearer; emits
  meta+JWKS ConfigMap always (dev-mode uses empty `{"keys":[]}`).
- **Sandbox reconciler**: dedup `mirrored_mcp_names`; inject
  `AZURECLAW_MCP_SERVERS` env on the openclaw container.
- **Router forwarder** (`mcp/forwarder.rs`): `Accept: application/json,
  text/event-stream` + SSE envelope decoder; outbound bearer fetched
  from per-sandbox env (not from CRD).
- **Sandbox entrypoint**: `commands.mcp:true` + render `mcp.servers.<n>`
  from env.

**Threat model**: Outbound MCP bearer is sourced from a K8s secret
projected as env on the openclaw container only (UID 1000 sandbox
context), not from the CRD itself. Router never logs bearer values
(see redaction tests in `mcp/forwarder.rs`).

### 2.3 Policy-quintet echo

- **`policy_status.rs`**: Memory bundle now echoed at
  `/internal/policy-status` alongside InferencePolicy / AgtProfile /
  EgressAllowlist / EgressApproval. Read-only observability surface.
- **`chat_completions.rs`** (+237): InferencePolicy enforcement seam
  expansion — tool-allowlist filtering on incoming `tools[]` array,
  audit-logging dropped tools. No crypto changes.
- **`egress_approval_compile.rs` / `_reconciler.rs`**: minor reconcile
  shape additions, no semantic change to CEL evaluation.

### 2.4 NetworkPolicy + dev-flow

- Operator-namespace default-deny NetworkPolicy template (defense in
  depth on the controller pod itself).
- `scripts/dev/fast-rebuild.sh`: developer-only helper, not used in
  production builds. Cross-compiles Rust crates in a builder container,
  overlays onto release image. Never executed inside the cluster.

### 2.5 OpenClaw plugin refresh

- `agt-handoff.ts`, `agt-task-loop.ts`, `foundry.ts`, `foundry-discovery.ts`,
  `openclaw.plugin.json`, `index.ts`: dev-flow MCP prompts + handoff
  polish + contracts.tools updates. All flows go through the in-sandbox
  inference-router proxy on 127.0.0.1:8443; no new outbound paths.

## 3. Crypto Surface

- **No new crypto code.** All ratchet/session/X3DH work continues to use
  the existing vendored `@agentmesh/sdk` (libsodium-backed). The 8
  vendored patches are unchanged by this PR.
- `controller/src/policy_fetcher.rs` (cosign + sigstore-rs) is
  unchanged. JWKS handling in `mcp_server_reconciler.rs` is byte-level
  passthrough — dev mode writes an empty literal `{"keys":[]}`,
  production mode passes through bytes fetched from `oauth.issuer`.

## 4. Secrets Handling

- `bearerFromEnv` (Slice 4d.4.1): the **name** of an env var is in the
  McpServer CRD; the **value** lives in a K8s secret mounted via
  `envFrom` on the openclaw container only. Router process reads it
  via `std::env::var()` on the per-sandbox process; never logged.
- `SANDBOX_NAME` / `CLUSTER_NAME`: non-secret identifiers safe to log.

## 5. Test Coverage

- 884 router tests + 770 controller tests + 17 cncf-conformance pass.
- New tests: `forwarder.rs` adds outbound-bearer redaction test; SSE
  envelope decoder round-trip test; `mcp_server_reconciler.rs` empty-
  JWKS smoke test.

## 6. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
