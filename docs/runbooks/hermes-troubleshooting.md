# Hermes runtime — troubleshooting runbook

A short, scoped runbook for the most common Hermes-specific issues. For the broader kars operator surface (sandboxes, mesh, governance) see [`docs/troubleshooting.md`](../troubleshooting.md) and the [Operator TUI](../operator-tui.md) guide.

> **Healthy baseline.** A working Hermes sandbox shows these lines in `kubectl logs <pod> -c agent` within ~30 seconds of `Started` (timestamps elided):
> ```
> [kars-hermes] Mesh token acquired via auth-sidecar after N attempt(s) — verified-tier registration
> INFO kars_agt_mesh.client: MeshClient connected: name=<sandbox> did=did:mesh:<32hex>
> INFO kars.hermes.mesh: AGT identity verified via OAuth — tier upgraded to 'verified' (id=did:mesh:<32hex>)
> INFO kars.hermes: MeshClient pre-connected at plugin load
> INFO kars.hermes.mesh_worker: mesh_worker: loop started for did=did:mesh:<32hex> (auto-respond mode)
> ```
> If you see all five, the agent is mesh-reachable, Entra-verified, and the auto-responder is live. If any is missing, jump to the matching section below.

---

## "ImagePullBackOff" on a fresh `KarsSandbox kind: Hermes`

**Symptom:** `kubectl describe pod` shows `Failed to pull image "karsacr.azurecr.io/kars-runtime-hermes:latest"`.

**Cause:** The Hermes adapter image was not loaded into your ACR. Older `kars up` runs (before [the productization fix](../internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md)) only imported six adapter images; Hermes was missing.

**Fix:** Re-run `kars push --only runtime-hermes --apply` against the cluster's ACR, or `kars up --skip-infra` to re-trigger the full multi-runtime image bring-up.

```bash
kars push --only runtime-hermes --apply
```

The CLI auto-clones the pinned AGT toolkit (`vendor/agt/pin.json`) and builds the Python wheels (`runtimes/wheels/*.whl`) if either is missing — no manual `git clone` step required.

---

## `MeshClient` doesn't reach the "MeshClient connected" log line

**Symptom:** After `[kars-hermes] Mesh token acquired ...` the next mesh-related log line is `Relay WS dropped` or `Connect frame rejected`, with no `MeshClient connected: name=<sandbox>` line.

**Likely causes (in order of frequency):**

1. **Entra token expired or missing** — the entrypoint logs `Mesh token acquired` only after a successful workload-identity → Entra exchange. If you see this line but no subsequent `verified-tier registration`, the relay is rejecting your connect frame. Check the inference-router log for `auth-sidecar` HTTP 401s:
   ```bash
   kubectl logs <pod> -c inference-router | grep -i 'entra\|auth-sidecar'
   ```
   Fix: ensure `KarsAuthConfig.spec.foundryRbac` is set in your namespace so the controller auto-grants the per-sandbox Entra App the `Azure AI User` role on the resource group. The fix landed in commit [`496cc92`](../../#commit-496cc92).

2. **Registry replica restarted while a stale Hermes pod still holds prekeys** — symptom is `Registry rejected register_self: 409` followed by silent reconnect loop. Restart the Hermes deployment:
   ```bash
   kubectl rollout restart deployment/<sandbox> -n kars-<sandbox>
   ```

3. **Network policy blocks `agentmesh-relay.agentmesh.svc.cluster.local`** — happens when the namespace was created before kars 0.5.x and lacks the auto-applied egress rule. Re-apply the kars NetworkPolicy or label the namespace `app.kubernetes.io/name=kars` so the policy engine picks it up.

---

## "Decrypt failed for did:mesh:..." (silent — no traceback)

**Symptom:** `kubectl logs <pod> -c agent | grep Decrypt` shows lines like
```
WARNING kars_agt_mesh.client: Decrypt failed for did:mesh:8e6549...:
```
with an *empty* exception field and no stack trace.

**Cause: someone called `_get_or_init_client()` from a second Python process.** Most commonly an operator running `kubectl exec ... python3 -c "from kars_runtime_hermes.plugin import mesh; mesh._get_or_init_client(); ..."` for debugging — the secondary process generated fresh X3DH key material and `PUT`-ed it to the registry, clobbering the daemon's bundle. After that point every peer fetches the secondary's public keys but encrypts to the daemon's private key, so AEAD authentication fails with `InvalidTag`.

**Fix in the current codebase:** The second `MeshClient.connect()` now raises
```
MeshTransportError: Another mesh-client process already holds
<HERMES_HOME>/.agt/.mesh-prekeys.lock (pid=<N>). Refusing to start a
second MeshClient for did=<...> — would clobber the running daemon's
prekey bundle ...
```
before it can do any damage. If you still see the old silent `Decrypt failed` log, you're on an older image — run `kars push --only runtime-hermes --apply` to get the patched build (commit shipped as part of the Hermes Act 1 docker-smoke-fixes slice).

**Operator query path (the right way to inspect a live daemon):**

```bash
# 1) From your laptop — read the operator-facing trust store via the router's loopback API.
ADMIN_TOKEN=$(kubectl get secret -n kars-<sandbox> admin-token -o jsonpath='{.data.token}' | base64 -d)
kubectl exec <pod> -c agent -- sh -c "curl -sS -H 'Authorization: Bearer $ADMIN_TOKEN' http://127.0.0.1:8443/agt/trust"

# 2) Inspect the daemon's identity + DID without starting a second client.
kubectl exec <pod> -c agent -- cat /sandbox/.hermes/.agt/identity.json | jq '.did'
```

---

## Hermes' built-in `web_search` returns "tool not registered"

**Cause:** the kars plugin deregisters six Hermes built-ins (`web_search`, `web_fetch`, `code_interpreter`, `image_generation`, `file_search`, `chat_completion`) at startup so the agent cannot bypass kars governance. Use the kars-side replacements:

| Hermes built-in | kars replacement |
|---|---|
| `web_search` | `foundry_web_search` (Foundry Bing Grounding) — no config required |
| `web_fetch` | `http_fetch` (router-gated, egress-allowlisted) |
| `code_interpreter` | `foundry_code_execute` |
| `image_generation` | `foundry_image_gen` |
| `file_search` | `foundry_file_search` |
| `chat_completion` | route via the inference router on `127.0.0.1:8443` |

The full mapping is in [`docs/hermes-plugin.md`](../hermes-plugin.md#denied-hermes-built-ins-6).

---

## No `Verified` tier on the operator panel — Hermes shows `Anonymous`

**Symptom:** `kars operator topology` or the Headlamp mesh page lists the Hermes peer as `tier=anonymous` despite a successful boot.

**Cause:** The entrypoint's `/agt/registry/v1/registry/verify` POST never ran. Two possible reasons:

1. **`AGT_OAUTH_TOKEN` not in PID 1's env.** Validate:
   ```bash
   kubectl exec <pod> -c agent -- sh -c "tr '\0' '\n' < /proc/1/environ | grep -E '^(AGT_OAUTH_TOKEN|MESH_AUTH_BACKEND|AZURE_CLIENT_ID)='"
   ```
   You should see all three. If `AGT_OAUTH_TOKEN` is missing, the workload-identity → Entra exchange failed — check `kubectl logs <pod> -c inference-router | grep auth-sidecar` for the underlying HTTP error.

2. **Registry rejected the token.** Look for `WARNING kars.hermes.mesh: AGT registry rejected OAuth verification: HTTP 4xx` in the agent log. The most common cause is a clock-skew between the registry pod and the Entra issuer — restart the relay/registry deployment.

---

## See also

- **[Hermes plugin reference](../hermes-plugin.md)** — what the plugin registers and how.
- **[Channels & external plugins](../channels-plugins.md)** — credential / env-var contract for Telegram, Slack, Discord, WhatsApp, Brave, Tavily, Exa, Firecrawl, Perplexity.
- **[Cross-runtime mesh AKS audit](../internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md)** *(internal)* — the debugging post-mortem behind the prekey-writer guard.
- **[Mesh plugin](../mesh-plugin.md)** — Hermes-as-mesh-peer symmetry with OpenClaw.
- **End-to-end harnesses:** [`tests/e2e/interop/hermes_openclaw_bidi.sh`](../../tests/e2e/interop/hermes_openclaw_bidi.sh) (local kind) and [`tests/e2e/interop/aks_full_suite.sh`](../../tests/e2e/interop/aks_full_suite.sh) (AKS).
