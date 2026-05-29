# Migration guide — per-pod sidecar → shared sidecar

For operators upgrading from an earlier `feat/entra-agent-id` head (where every
sandbox pod ran its own auth-sidecar container) to the shared-sidecar
architecture (one Deployment in `kars-system`).

If you have never deployed `feat/entra-agent-id` before, skip this doc.

## What changed

| | Before (per-pod) | After (shared) |
|---|---|---|
| Sidecar deployment | One per sandbox pod | One Deployment (2 replicas) in `kars-system` |
| Reachable at | `http://127.0.0.1:8080` from the router | `http://entra-auth-sidecar.kars-system.svc:5000` |
| Network policy | iptables UID isolation (in-pod) | NetworkPolicy ingress (cross-namespace) |
| Resource footprint (10 sandboxes) | ~1.28 GB | ~160 MB |
| MSAL cache | Per-pod | Per replica (still per-sandbox via `?AgentIdentity=` query param) |

The router's fail-closed contract is unchanged: when `AUTH_SIDECAR_URL` is set,
no WI / IMDS / API-key fallback. Per-sandbox attribution still flows via the
`PINNED_AGENT_IDENTITY_APP_ID` env var injected by the controller.

## Migration steps

### 1. Pull the latest

```bash
git checkout feat/entra-agent-id
git pull
```

### 2. Re-deploy the Helm release with the shared sidecar enabled

Read your existing `KarsAuthConfig/default` to get the blueprint client ID and
tenant ID:

```bash
kubectl get karsauthconfig default -o jsonpath='{.spec.agentId.blueprintClientId}'
kubectl get karsauthconfig default -o jsonpath='{.spec.tenant.tenantId}'
```

Then upgrade:

```bash
helm upgrade kars deploy/helm/kars -n kars-system \
  --reset-then-reuse-values \
  --set entraSidecar.enabled=true \
  --set entraSidecar.blueprintClientId=<from above> \
  --set entraSidecar.tenantId=<from above>
```

Verify the shared sidecar is up:

```bash
kubectl get deploy entra-auth-sidecar -n kars-system
kubectl get svc entra-auth-sidecar -n kars-system
```

### 3. Restart controller + sandboxes

The controller rolls out new pods using the shared sidecar (no per-pod
container). Existing sandbox pods need to be restarted to pick up:
- The new router image (sidecar mode)
- The new env vars (`AUTH_SIDECAR_URL`, `PINNED_AGENT_IDENTITY_APP_ID`,
  `EXPECTED_TENANT_ID`)

```bash
kubectl rollout restart deployment kars-controller -n kars-system
for ns in $(kubectl get karssandbox -A -o jsonpath='{range .items[*]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'); do
  k_ns=$(echo "$ns" | cut -d/ -f1)
  k_name=$(echo "$ns" | cut -d/ -f2)
  # Sandbox namespaces are kars-${name}; the deployment is named ${name}
  kubectl rollout restart deployment "$k_name" -n "kars-${k_name}" 2>/dev/null
done
```

### 4. Verify

```bash
# All routers booted in sidecar mode?
for pod in $(kubectl get pods -A -l kars.azure.com/component=sandbox -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{"\n"}{end}'); do
  ns=$(echo "$pod" | cut -d/ -f1)
  name=$(echo "$pod" | cut -d/ -f2)
  kubectl logs -n "$ns" "$name" -c inference-router 2>/dev/null \
    | grep 'Sidecar auth mode enabled' | head -1 \
    | jq -r '"\(.timestamp[:19])  \(.fields.pinned_agent_id)"' 2>/dev/null
done
```

### 5. Grant per-agent Azure RBAC (manual step until Phase 5b)

Each KarsSandbox's agent identity needs `Cognitive Services OpenAI User` AND
`Azure AI User` on the Foundry account it talks to.

The conservative way (works even when the operator account is CA-blocked from
`az role assignment create`):

```bash
FOUNDRY_SCOPE="/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.CognitiveServices/accounts/<foundry-name>"
COGSVC_OPENAI=/subscriptions/<sub>/providers/Microsoft.Authorization/roleDefinitions/5e0bd9bd-7b93-4f28-af87-19fc36ad61bd
AZURE_AI_USER=/subscriptions/<sub>/providers/Microsoft.Authorization/roleDefinitions/53ca6127-db72-4b80-b1b0-d745d6d5456d

for APPID in $(kubectl get karssandbox -A -o jsonpath='{range .items[*]}{.status.agentIdentity.appId}{"\n"}{end}'); do
  [[ -z "$APPID" ]] && continue
  for ROLE in "$COGSVC_OPENAI" "$AZURE_AI_USER"; do
    ASSIGN=$(uuidgen)
    az rest --method PUT \
      --url "https://management.azure.com${FOUNDRY_SCOPE}/providers/Microsoft.Authorization/roleAssignments/${ASSIGN}?api-version=2022-04-01" \
      --body "{\"properties\":{\"roleDefinitionId\":\"${ROLE}\",\"principalId\":\"${APPID}\",\"principalType\":\"ServicePrincipal\"}}" -o none
  done
done
```

After the grant, restart the sandbox + sidecar pods to flush any cached
PermissionDenied state:

```bash
kubectl rollout restart deployment entra-auth-sidecar -n kars-system
for ns in $(kubectl get karssandbox -A -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{"\n"}{end}' | tr '/' ' '); do
  set -- $ns
  kubectl rollout restart deployment "$2" -n "kars-$2" 2>/dev/null
done
```

## Phase 5b will eliminate step 5

The controller will assign the role on the Foundry RG (or per-resource scope
declared in `KarsAuthConfig.spec.downstreamRbac`) every time it creates a new
agent identity. This requires granting the controller MI
`Microsoft.Authorization/roleAssignments/write` on the Foundry RG once at
`kars up` time. Until that ships, the manual grant above is the workaround.

## Rollback

If you need to fall back to the per-pod sidecar branch:

```bash
git checkout <earlier-sha>
helm upgrade kars deploy/helm/kars -n kars-system \
  --reset-then-reuse-values \
  --set entraSidecar.enabled=false
```

Then rebuild + push images, restart controller, restart sandboxes. The
per-pod sidecar will be re-injected as before.

## Known caveats

- **HostFiltering**: the Microsoft Entra SDK sidecar rejects non-`localhost`
  Host headers regardless of `AllowedHosts=*`. The router works around this by
  overriding `Host: localhost:5000` on every sidecar call. This is in code
  (`inference-router/src/sidecar_client.rs`) — no operator action needed.
- **MSAL cache per replica**: each of the two sidecar replicas maintains its own
  MSAL cache. After a role grant, both replicas may serve a stale-RBAC token
  until cache expiry (~24h). Restart the sidecar Deployment to force fresh
  tokens.
- **Foundry Bing Grounding**: `foundry_web_search` requires a Bing Grounding
  connection set up in the Foundry project (`project_connection_id` field). If
  missing, the tool returns 400 — orthogonal to auth, fix in the Foundry portal.
