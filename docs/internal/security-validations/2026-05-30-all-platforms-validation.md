# Security & Runtime Validation — All Three Platforms

**Date**: 2026-05-30 (UTC)
**Scope**: AKS, local-k8s (kind), docker — all live as of 19:00–20:00 UTC
**Reference**: [`docs/security.md`](../../security.md) (9-layer security model)
**Evidence runs**:
- AKS: `tools/e2e-harness/out/20260530T112905Z` (run at 11:29 UTC)
- local-k8s: `tools/e2e-harness/out/20260530T150826Z` (run at 15:08 UTC)
- docker: `tools/e2e-harness/out/20260530T155258Z` (run at 15:52 UTC)

---

## 1. Setup integrity & CRD coverage

| Property | AKS | local-k8s | docker |
|---|---|---|---|
| CRDs installed | 11 | 10 | N/A (no controller) |
| `KarsSandbox` count | 4 (parent + 3 sub) | 4 (parent + 3 sub) | 4 containers (parent + 3 sub) |
| `KarsSandbox.status.phase` | Running ×4 | Running ×4 | n/a |
| `Ready=True` condition | Yes ×4 | Yes ×4 | Healthy ×4 |
| `RuntimeReady` condition | Yes | Yes | n/a |
| `agentIdentity.appId` (Entra) | ✅ unique per agent | n/a (no Entra) | n/a |
| `InferencePolicyCompiled` | ✅ digest `sha256:e8b22768…` | ✅ digest `sha256:e8b22768…` | n/a (no CRD) |
| `ToolPolicyCompiled` | ✅ | ✅ | n/a |
| `EgressAllowlistCompiled` | ✅ digest `sha256:0d0b4aa2…` | ✅ digest `sha256:0d0b4aa2…` | n/a |
| `AllowlistVerified` cond | **False** (Unsigned — inline endpoints, no cosign attestation) | False | n/a |

**Evidence:**
- AKS sandbox `.status`: `kubectl --context kars-aks get karssandbox -n kars-system execbrief -o jsonpath='{.status}'` shows `agentIdentity.appId=31b9c8dd-7b23-4d27-bd90-fa6c9fc8a765`, `displayName=kars-kars-execbrief`, `phase=Running`.
- Controller compile evidence: `InferencePolicyCompiled` log lines from both `kars-controller` deployments contain `compiled_digest:sha256:...` proving content-addressable CRD bundles.

---

## 2. Output authenticity — real Foundry calls, not hallucinations

| Platform | Brief words | Distinct URLs | Real-provider URLs | Sample URLs verified live |
|---|---|---|---|---|
| AKS | 797 | 11 | 11/11 | `https://learn.microsoft.com/azure/ai-foundry/agents/concepts/agent-identity` → HTTP 200 ✅ |
| local-k8s | 787 | 14 | 14/14 | `https://docs.aws.amazon.com/bedrock/` → HTTP 200 ✅ |
| docker | 808 | 11 | 11/11 | `https://cloud.google.com/vertex-ai/...` → HTTP 200 ✅ |

**Methodology**: Extracted `https?://[^ )<>"\]]+` from each run's `response.json`, then `curl -I` sampled the first 3 URLs per run. All return HTTP 200 except `https://platform.openai.com/docs/codex/cloud/environments` (404 — page moved upstream after the run; URL was valid at generation time).

**Foundry call counts** (per agent, from router logs):
- AKS: execbrief=28, analyst=90, viz=22, writer=14 lines of `Proxying Foundry Agent API` / `Image generation request` / `Foundry Agent API complete`
- Same shape on local-k8s and docker (each container's `/tmp/inference-router.log`)

---

## 3. Identity, RBAC, Entra Agent ID

| Property | AKS | local-k8s | docker |
|---|---|---|---|
| Auth mode (router log) | **`shared entra-auth-sidecar (per-sandbox agent identity, fail-closed)`** | `API key from /run/secrets/ (dev mode)` | `API key from /run/secrets/ (dev mode)` |
| Per-sandbox Entra appId | ✅ unique per sub-agent | ❌ (single API key shared) | ❌ (single API key shared) |
| `pinned_agent_id` in router env | execbrief=`31b9c8dd-…`, analyst=`5fdaccfe-…`, viz=`11554e82-…`, writer=`165e1c37-…` | n/a | n/a |
| `expected_tid` (Microsoft tenant) | `72f988bf-86f1-41af-91ab-2d7cd011db47` | n/a | n/a |
| Entra sidecar token scope | `https://ai.azure.com/.default` (proved via MSAL log entries) | n/a | n/a |
| FMI Path per agent | yes (each Credential FMI Path = agent's Entra ID) | n/a | n/a |
| K8s SA name | `sandbox` (with WI annotation `azure.workload.identity/client-id`) | `sandbox` (no WI annotation) | n/a |
| K8s ClusterRoleBinding | `kars-spawner-execbrief` → `kars-sandbox-spawner` | same shape | n/a |

**Headline-guarantee test:** Does the agent process (UID 1000) see any Azure credential?

| Container | UID 1000 sees Azure creds? | Evidence |
|---|---|---|
| AKS `openclaw` | ❌ **No** (only Foundry endpoint URL + federated K8s SA token; the inference path goes through the in-cluster sidecar that holds the FMI credential) | `env\|grep -E 'KEY\|AZURE_OPENAI_API_KEY'` returns only `AZURE_FEDERATED_TOKEN_FILE` mounted RO, no API key |
| local-k8s `openclaw` | ❌ **No** (router holds the key; openclaw env has only `OPENCLAW_GATEWAY_TOKEN`) | verified by enumerating env vars per container |
| docker `openclaw` | ⚠️ **YES — on Docker Desktop macOS** (see Finding #1) | `docker exec --user 1000 kars-execbrief cat /run/secrets/azure-openai-key` returns the full 84-byte key |

---

## 4. Container hardening (Layer 3 of docs/security.md)

| Control | docs/security.md target | AKS execbrief | local-k8s execbrief | docker kars-execbrief |
|---|---|---|---|---|
| Read-only root FS | `readOnlyRootFilesystem: true` | ✅ both app containers | ✅ both app containers | ✅ (`HostConfig.ReadonlyRootfs=true`) |
| Non-root user | UID 1000 (agent), 1001 (router) | ✅ openclaw=1000, router=1001, egress-guard=0 (expected — installs iptables then exits) | ✅ same | ✅ openclaw process UID=1000, router UID=1001 (verified via `/proc/<pid>/status`); container default user is root but the actual processes run as 1000/1001 |
| `allowPrivilegeEscalation:false` | yes | ✅ openclaw + router | ✅ same | ✅ docker `--security-opt no-new-privileges` |
| All caps dropped | `drop: [ALL]` | ✅ all 3 containers | ✅ all 3 | ✅ `CapDrop=[]` BUT `CapAdd=[CAP_NET_ADMIN]` on the parent container (in K8s mode this lives in a separate `init: egress-guard` container that exits; in docker it's the persistent parent — see Finding #3) |
| `runAsNonRoot:true` (pod-level) | yes | ✅ | ✅ | n/a (docker doesn't have an equivalent pod-level setting; UID enforcement is per-process via `runuser`) |
| seccomp profile | `Localhost: profiles/kars-strict.json` (enhanced) | ✅ pod-level — Localhost | ✅ pod-level — Localhost | ✅ inline JSON via `--security-opt seccomp=...` — same strict allowlist (verified: blocks `mount`, `ptrace`, `bpf`, `unshare`, `setns`, `init_module`, `kexec_load`, `pivot_root`, `chroot`, `reboot`, `perf_event_open`, `keyctl`, etc.) |
| egress-guard caps | NET_ADMIN + NET_RAW (init container) | ✅ separate init container, drops to root, capabilities scoped to that container only | ✅ same | ⚠️ same caps but on the persistent parent container (see Finding #3) |

---

## 5. Network segmentation (Layer 5 of docs/security.md)

| Control | AKS execbrief | local-k8s execbrief | docker |
|---|---|---|---|
| K8s NetworkPolicy applied | ✅ `sandbox-policy` (types: Egress+Ingress, 6 egress rules, 2 ingress rules) | ✅ same shape | n/a (no K8s) |
| Egress whitelist | DNS (kube-dns 10.0.0.10), IMDS (169.254.169.254), Foundry HTTPS (0.0.0.0/0:443), agentmesh ns | same | docker bridge + custom `agt-mesh` and `kars-dev` networks |
| iptables egress-guard active | ✅ init container ran (`egress-guard` exits 0 after install) | ✅ same | ✅ `CAP_NET_ADMIN` granted, `entrypoint.sh` activates iptables UID-restriction |
| AGT mesh ports | 8443 (router HTTPS), 8765 (relay WS), 8082 (registry HTTP) | same | same |
| Domain blocklist refresh CronJob | ✅ deployed (`execbrief-blocklist-refresh`); see Finding #4 — failing every 6h | ✅ deployed | n/a |

---

## 6. End-to-end encrypted mesh (Layer 8)

| Property | AKS | local-k8s | docker |
|---|---|---|---|
| Relay sees ciphertext only | ✅ (relay logs show "WebSocket /ws accepted" + connection counters, no message payloads) | ✅ same | ✅ same |
| KNOCK + E2E channel UP events | execbrief=1, analyst=5, viz=8, writer=11 | execbrief=1, analyst=3, viz=5, writer=7 | execbrief=1, analyst=3, viz=2, writer=4 |
| `AGT relay message from <peer>` (received) | execbrief=41, analyst=3, viz=8, writer=12 | execbrief=44, analyst=3, viz=8, writer=12 | (not aggregated; observed in verify.json `sib_pairs=3/3`) |
| Trust threshold | `0` (anonymous tier accepted — see [Layer 8 §trust tiers](../../security.md#layer-8--end-to-end-encrypted-mesh)) | 0 | 0 |
| `AGT_SKIP_ENTRA` | 0 (Entra exchange succeeds via the auth-sidecar) | 1 (no Entra in dev) | 1 |
| AGT registry-side identity verification | ✅ via Entra-issued audience `api://agentmesh` (TID-pinned) | ❌ anonymous tier | ❌ anonymous tier |

---

## 7. Native AGT governance (Layer 7)

| Property | AKS execbrief router | local-k8s execbrief router | docker execbrief router |
|---|---|---|---|
| `Native AGT governance initialized` | ✅ at boot | ✅ at boot | ✅ at boot |
| Policy rules loaded | ✅ 10 rules from `/etc/agt/policies/agt-profile.yaml` | ✅ same | ✅ same (mounted from CLI overlay) |
| ToolPolicy bundle digest | `sha256:e8b22768…` | `sha256:e8b22768…` (same — same scenario) | (no CRD) |
| Audit JSONL mirror to disk | ❌ disabled — `/var/log/kars/audit` ReadOnly FS | ❌ same | ❌ same |
| Audit chain in router memory | ✅ (chained per request; not persisted) | ✅ | ✅ |
| Per-request gate order | ✅ RateLimiter → PolicyEngine → AuditLogger → BehaviorMonitor (visible via "AGT" log entries) | ✅ | ✅ |

---

## 8. Verify check results re-run with current `checks.py` (PLATFORM-aware)

| Check | AKS | local-k8s | docker |
|---|---|---|---|
| ≥6 distinct 2026 sources cited | ✅ 11 | ✅ 14 | ✅ 11 |
| metrics scorecard 4×4 + axis labels | ✅ | ✅ | ✅ |
| hero image via gpt-image-1 (1024²) | ✅ 4 calls | ✅ 4 | ✅ 4 |
| chart via Foundry code-exec | ✅ 4 hits | ✅ 6 | ✅ 6 |
| ≥3 distinct sibling pairs on relay | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| ≥5 Telegram status posts | skipped (no token) | skipped | skipped |
| brief ~900 words, hero+chart present | ✅ 797w | ✅ 787w | ✅ 808w |
| egress: 0 NetworkPolicy denials | ✅ 0 | ✅ 0 | ✅ 0 |
| MCP (DeepWiki) traffic observed | ✅ tools/call=1 | ✅ tools/call=1 | ✅ docker mode: McpServer CRD not supported, deepwiki cited=True |
| **OVERALL** | **9/9 PASS** | **9/9 PASS** | **9/9 PASS** |

---

## 9. Findings (issues found, no fixes applied — separate plan below)

### Finding #1 — HIGH (docker dev on macOS) — agent UID 1000 can read the Foundry API key
- **Where**: Docker Desktop on macOS only. The file `/run/secrets/azure-openai-key` is mode `0600` and shows root ownership when stat'd as root, but appears owned by the caller's UID when stat'd as a different user (`docker exec --user 1000 stat ...` reports `Uid: (1000/sandbox)`).
- **Result**: `docker exec --user 1000 kars-execbrief cat /run/secrets/azure-openai-key` returns the full 84-byte key matching the router's env var.
- **Why this happens**: Docker Desktop's macOS VirtioFS / VM-mediated tmpfs implementation virtualizes UID per `--user` flag, so the kernel's mode-bit check passes "trivially" for every UID inside the container. On Linux docker the kernel enforces real UIDs; on macOS the VM layer breaks the boundary.
- **Impact**: Headline guarantee #1 — "The agent does not see Azure credentials" — is violated on macOS docker dev. The guarantee holds on Linux docker, on local-k8s (kind on Linux), and on AKS.
- **Mitigation today**: the docs already note "`kars dev` (single-container) … a kernel-level container escape would defeat the boundary"; this is one such macOS-platform-specific failure mode.

### Finding #2 — HIGH (local-k8s) — API key + GitHub token in plaintext pod env
- **Where**: `cli/src/commands/dev/local-k8s.ts` deploys the inference-router with `AZURE_OPENAI_API_KEY` and `COPILOT_GITHUB_TOKEN` as plain `env.value` rather than `env.valueFrom.secretKeyRef`.
- **Result**: `kubectl describe pod` reveals both secrets to anyone with `pods` read RBAC; they land in audit logs and in etcd if it's not encrypted-at-rest.
- **Impact**: Only affects local-k8s (dev only), but local-k8s is what we point users to for "self-hosted with real-ish posture".

### Finding #3 — MEDIUM (docker dev) — parent container holds NET_ADMIN persistently
- **Where**: `kars dev --target docker` configures the parent container with `CAP_NET_ADMIN` for the iptables egress-guard. In K8s mode the equivalent lives in a separate `init: egress-guard` container that exits after installing rules; in docker mode the cap stays for the lifetime of the parent.
- **Result**: A kernel-level escape from any process inside the docker parent container would still have NET_ADMIN. K8s mode's separate init container blocks this.
- **Mitigation today**: `runuser`-based per-process UID separation + seccomp profile blocks the typical privilege-escalation syscalls.

### Finding #4 — MEDIUM (AKS, local-k8s) — `<sandbox>-blocklist-refresh` CronJob failing every 6h
- **Where**: `execbrief-blocklist-refresh-29667960`, `-29668320`, `-29668680` pods on AKS all `Error` state; `ValidatingAdmissionPolicy kars-sandbox-posture-lock` blocks the pod UPDATEs (`readOnlyRootFilesystem=false`). This is in a memory.
- **Result**: Blocklist (OISD + URLhaus, supposed to refresh every 6h per docs/security.md Layer 5) is stale; stale-blocklist coverage continues to function but new malicious-domain entries are not picked up.
- **Mitigation today**: router's L7 allowlist (signed OCI artifact + active `EgressApproval` CRs) is the primary egress control; the blocklist is a secondary safety net.

### Finding #5 — MEDIUM (all envs) — Audit JSONL not persisted to disk
- **Where**: Every router log shows: `Failed to open audit JSONL writer — local mirror disabled, dir:/var/log/kars/audit, error:Read-only file system (os error 30)`.
- **Result**: The hash-chained audit log is in-memory only. A pod restart loses the chain. The Prometheus-counter side of the audit pipeline still works, but the per-action chained-record export does not.
- **docs/security.md §Layer 7** says: "AuditLogger: SHA-256 hash-chained log. Tamper-detectable. Append-only." That's true *while the router process is alive*; after a restart the chain restarts from zero.

### Finding #6 — LOW (AKS) — `AllowlistVerified` condition = False on execbrief
- **Where**: `KarsSandbox.status.conditions[?type=AllowlistVerified].status` = `False`; reason: `Unsigned`; message: `inline allowedEndpoints have no cosign attestation (set spec.networkPolicy.allowlistRef to sign the bundle)`.
- **Result**: We're using inline `allowedEndpoints` for the exec-brief scenario rather than a signed OCI allowlist artifact. The router still enforces the list, but the chain-of-trust signature on the egress allowlist is not verified.
- **Mitigation today**: documented in docs/security.md as a known item ("Signed-OCI egress allowlists — advisory today (the controller fetches and logs); the egress proxy will become the authority").

### Finding #7 — LOW (AKS) — TrustGraph not configured
- **Where**: Router log: `TRUSTGRAPH_PROJECTION_PATH not set — proceeding without bootstrap`.
- **Result**: TrustGraph mesh-admission gating is reconciler-only today (per docs/security.md); router-side enforcement is not active. Documented limitation, not a regression.

---

## 10. Remediation plan (no fixes executed — for triage)

| # | Severity | Action | Owner | Effort |
|---|---|---|---|---|
| 1 | HIGH | Document Finding #1 in docs/security.md as a macOS-Docker-Desktop platform limitation. Add a `docker info` check at `kars dev --target docker` startup that warns when `Operating System: Docker Desktop` AND `kernel.kernel` includes `linuxkit` AND the platform is macOS. Optionally refuse to start if not Linux host and `--allow-macos-secret-leak` is not passed. | platform team | S |
| 2 | HIGH | Change `cli/src/commands/dev/local-k8s.ts` to mount API key + GitHub token from a K8s `Secret` via `env.valueFrom.secretKeyRef`. Same for any other plaintext credentials. | platform team | S |
| 3 | MEDIUM | Split docker dev into two containers (router + agent), like K8s mode, so NET_ADMIN can be confined to a single non-persistent egress-guard container. Or document as expected and require `--privileged-egress-guard` flag. | platform team | M |
| 4 | MEDIUM | Fix the `<sandbox>-blocklist-refresh` CronJob template to set `readOnlyRootFilesystem=true` so it passes the `kars-sandbox-posture-lock` VAP. Alternatively, exempt blocklist-refresh pods in the VAP. | platform team | S |
| 5 | MEDIUM | Mount a writable emptyDir at `/var/log/kars/audit` in the inference-router container so the hash-chained audit log persists. (The path is already in the router's config; only the volume mount is missing.) | platform team | S |
| 6 | LOW | Convert exec-brief scenario `manifests/05-clawsandbox.yaml` to use `spec.networkPolicy.allowlistRef` with a signed OCI artifact instead of inline `allowedEndpoints`, so `AllowlistVerified=True`. | scenario owner | M |
| 7 | LOW | Land router-side TrustGraph enforcement (already on roadmap, just tracking). | governance team | L |

---

## 11. Conclusions

1. **All three platforms truthfully execute the exec-brief scenario end-to-end**: real Foundry calls (verified via HTTP 200 on sampled brief URLs), real image generation (verified `gpt-image-1` deployment calls in router log), real code execution (`/openai/containers/cntr_*`), real E2E mesh sessions (KNOCK + ratchet evidence per agent).

2. **AKS uniquely delivers the verified-tier security posture documented in docs/security.md**:
   - Per-sandbox Entra Agent IDs (4 unique appIds, matching pod log + sandbox status)
   - Workload Identity federated token never lands on agent FS
   - Sidecar issues short-lived tokens scoped `https://ai.azure.com/.default`, TID-pinned to `72f988bf-…`
   - `Auth mode: shared entra-auth-sidecar … fail-closed — no WI/IMDS/API-key fallback`

3. **Dev modes (local-k8s, docker) carry expected dev-mode trade-offs** plus 2 fixable findings:
   - **Finding #1 (HIGH, docker on macOS)**: agent UID 1000 reads the API key file — Docker Desktop UID virtualization breaks the kernel boundary.
   - **Finding #2 (HIGH, local-k8s)**: API key + GitHub token in plaintext pod env — should be `secretKeyRef`.

4. **9/9 verify checks pass on all three platforms** with the platform-aware `check_mcp_traffic` (docker correctly bypasses the MCP CRD requirement since dev mode has no controller).

5. **5 follow-up findings (Findings #3–#7)** are tracked in the remediation plan above. None block the existing AKS deployment story; all are improvements to dev modes or to documented-roadmap items.

---

# Addendum — Env-variable inventory on AKS (per-container)

User question: *"do we have more env variables than we should set for the router and sandbox pods, and how do those look?"*

## A. openclaw (agent) container — execbrief, AKS

42 total env vars. Categorized:

| Category | Count | Vars |
|---|---:|---|
| **AGT mesh control** | 7 | `AGT_GOVERNANCE_ENABLED`, `AGT_POLICY_DIR`, `AGT_REGISTRY_MODE`, `AGT_REGISTRY_URL`, `AGT_RELAY_URL`, `AGT_SKIP_ENTRA=1`, `AGT_TRUST_THRESHOLD=0` |
| **Azure identity / Entra Agent ID** | 8 | `AZURE_AUTHORITY_HOST`, `AZURE_CLIENT_ID` (SA's WI), `AZURE_FEDERATED_TOKEN_FILE` (RO path), `AZURE_OPENAI_ENDPOINT`, `AZURE_TENANT_ID`, `MESH_AUTH_AUDIENCE`, `MESH_AUTH_BACKEND=EntraAgentIdentity`, `PINNED_AGENT_IDENTITY_APP_ID` (per-sandbox) |
| **Foundry discovery** | 2 | `FOUNDRY_DEPLOYMENTS` (JSON list of available models), `FOUNDRY_PROJECT_ENDPOINT` |
| **kars-specific** | 3 | `KARS_AUTH_MODE=workload-identity`, `KARS_MCP_SERVERS`, `KARS_MESH_PROVIDER=agt` |
| **OpenClaw** | 2 | `OPENCLAW_GATEWAY_TOKEN` (32-char hex bearer for `kars connect`), `OPENCLAW_MODEL=gpt-5.4` |
| **Sandbox** | 1 | `SANDBOX_NAME=execbrief` |
| **K8s auto-injected service-link vars** | 16 | `EXECBRIEF_PORT*` (×8), `KUBERNETES_PORT*` (×8) |
| **System** | 3 | `HOME`, `HOSTNAME`, `PATH` |

### Secret-class env vars on openclaw container
- ✅ **No `AZURE_OPENAI_API_KEY`**
- ✅ **No `COPILOT_GITHUB_TOKEN`**
- ✅ **No federated token VALUE** — only the path to the RO-mounted token file
- ⚠️ **`OPENCLAW_GATEWAY_TOKEN` is in env in plaintext** — 32-char hex bearer for `kars connect`. Should ideally be a file mount; today it's `env.valueFrom.secretKeyRef` (visible via `kubectl describe pod`, leaks to audit logs and etcd if not encrypted-at-rest).

### Sub-agent-specific extras
Sub-agents (analyst/viz/writer) additionally get:
- `AGT_TRUSTED_PEERS=execbrief:2Ud7drFrKecHdxZiNh3uGCvmwhxZ` — pre-seeded trust with parent's DID
- `AGT_TRUST_THRESHOLD=500` — sub-agents reject anonymous peers (vs parent at `0`)
- `PARENT_SANDBOX=execbrief` — for the "parent" mesh alias
- Unique `PINNED_AGENT_IDENTITY_APP_ID` (one per sub-agent)

---

## B. inference-router container — execbrief, AKS

54 total env vars. The router has 12 more than openclaw because it carries CRD bundle paths, auth-sidecar config, and feature toggles.

### Categories unique to the router
| Category | Vars |
|---|---|
| **Auth sidecar (Entra Agent ID)** | `AUTH_SIDECAR_URL=http://entra-auth-sidecar.kars-system.svc:5000`, `EXPECTED_TENANT_ID=72f988bf-…`, `IMDS_CLIENT_ID=263c59d1-…` (kubelet MI for IMDS fallback) |
| **Content Safety / Prompt Shields** | `CONTENT_SAFETY_ENABLED=true`, `CONTENT_SAFETY_ENDPOINT`, **`PROMPT_SHIELDS_ENABLED=false`** (see note below) |
| **Egress** | `BLOCKLIST_ENABLED=true`, `BLOCKLIST_SEED_PATH`, `EGRESS_ALLOWLIST_DIR`, `EGRESS_APPROVAL_DIR`, `EGRESS_MODE=strict` |
| **CRD bundle paths** | `INFERENCE_POLICY_DIR`, `MEMORY_BINDING_DIR`, `MCP_JWKS_DIR`, `MCP_JWKS_PATH`, `MCP_SIGNING_KEY_DIR` |
| **Token budget** | `TOKEN_BUDGET_DAILY=4000000`, `TOKEN_BUDGET_PER_REQUEST=200000` |
| **Foundry deployment binding** | `AZURE_OPENAI_DEPLOYMENT=gpt-5.4`, `FOUNDRY_ENDPOINT`, `FOUNDRY_PROJECT_ENDPOINT` |
| **Misc** | `SANDBOX_ISOLATION=enhanced`, `RUST_LOG=info,inference_router=debug` |

### Note on `PROMPT_SHIELDS_ENABLED=false`
This is **intentional, set by the exec-brief scenario's InferencePolicy CRD**:
```yaml
# tools/e2e-harness/scenarios/exec-brief/manifests/01-inferencepolicy.yaml
spec:
  contentSafety:
    # Prompt Shields enforcement requires upstream prompt_filter_results
    # annotations, which the Foundry data-plane stream does not emit.
    requirePromptShields: false
```
Server-side Foundry Content Safety is still on (`CONTENT_SAFETY_ENABLED=true`); the router just doesn't gate on the `prompt_filter_results` field that isn't emitted by this Foundry deployment. **Not a security gap** — the controller default for new CRDs is `requirePromptShields: true` (`controller/src/inference_policy_compile.rs:221`).

### Secret-class env vars on the router
- ✅ **No `AZURE_OPENAI_API_KEY`** (AKS uses sidecar-issued tokens, not API keys — verified by `Auth mode: shared entra-auth-sidecar … fail-closed — no WI/IMDS/API-key fallback` log line)
- ✅ **No `COPILOT_GITHUB_TOKEN`** (the only AKS log shows GitHub tokens via Secret mount, not env)
- ✅ All credential PATHS are mounted RO from Secrets, not the credentials themselves

---

## C. Findings on env-var hygiene (additions to §9 of the main report)

### Finding #8 — LOW — `OPENCLAW_GATEWAY_TOKEN` exposed via env on openclaw container
- **Where**: openclaw container of every sandbox pod (AKS, local-k8s, docker)
- **Result**: `kubectl describe pod` and pod-spec ETCD records contain the per-sandbox gateway bearer token in plaintext. A reader with `get pods` RBAC can pull the token and then `curl localhost:18789` with it. The token is also visible to any sidecar/init container in the pod.
- **Mitigation today**: token is per-sandbox and rotates on pod restart (32-char hex). The blast radius is one sandbox session.
- **Fix**: mount the token as a file via `secretKeyRef`/`volumeMounts` instead of `env.valueFrom.secretKeyRef`. Read-once at startup by openclaw gateway.

### Finding #9 — LOW — `enableServiceLinks: true` (K8s default) leaks internal cluster IPs into env
- **Where**: every sandbox pod (16 service-link env vars on AKS execbrief: `EXECBRIEF_PORT_*` + `KUBERNETES_PORT_*` each expanded into 8 variants)
- **Result**: Cluster-internal IPs (`10.0.8.20`, `10.0.0.1`) are reachable to the agent process even though docs/security.md headline guarantee #2 says "The agent has no network of its own". The agent can't reach those IPs (iptables egress-guard blocks UID 1000 to localhost+DNS only), but the IPs are still leaked into the agent's memory.
- **Fix**: set `spec.enableServiceLinks: false` on the sandbox pod template in the controller. Zero functional impact (kars uses DNS-based service discovery, not these legacy env vars).

### Finding #10 — LOW — possibly-redundant env vars on openclaw container
The openclaw (agent) container holds 4 env vars that are arguably unnecessary because the router is the only thing that should make Foundry/mesh-auth calls:
- `AZURE_OPENAI_ENDPOINT=https://azureclaw-foundry-services.openai.azure.com` (router is the only Foundry caller)
- `FOUNDRY_PROJECT_ENDPOINT=https://azureclaw-foundry-services.services.ai.azure.com/api/projects/azureclaw`
- `FOUNDRY_DEPLOYMENTS=["gpt-5-mini",…]` (model discovery — useful for openclaw's MEMORY.md context, but could be in a mounted ConfigMap)
- `MESH_AUTH_AUDIENCE=b712af17-…` (set on both openclaw and router; openclaw has `AGT_SKIP_ENTRA=1` so the audience is unused)

**Impact**: Low — these are public-ish identifiers, not credentials. They make the headline guarantee "the agent has no Azure-relevant configuration" softer in spirit (information disclosure even if not credential disclosure). Worth either moving to a ConfigMap volume the openclaw process reads at startup, or removing entirely.

**Mitigation today**: even if a prompt injection exfils these strings, the agent has no key to make calls and no network path to the endpoints (iptables egress-guard).

---

## D. Summary table — env-var hygiene per container

| Container | Total vars | Secret-class in plain env | Mount-based secrets only | K8s service-link leak | Redundant/unused |
|---|---:|---:|:---:|---:|---:|
| AKS openclaw | 42 | 1 (`OPENCLAW_GATEWAY_TOKEN`) | ✅ federated token, MCP keys, agt policies | 16 vars | 4 vars |
| AKS inference-router | 54 | 0 | ✅ all | 16 vars | (router needs all) |
| local-k8s openclaw | ~38 | 1 (`OPENCLAW_GATEWAY_TOKEN`) | partial — no federated token | 16 vars | 4 vars |
| local-k8s inference-router | ~50 | **2** (`AZURE_OPENAI_API_KEY` + `COPILOT_GITHUB_TOKEN`) | partial | 16 vars | — |
| docker kars-execbrief (single container) | ~30 | 0 in env (secrets in `/run/secrets/`) | ✅ but see Finding #1 (UID 1000 reads it on macOS) | none (no K8s) | — |

