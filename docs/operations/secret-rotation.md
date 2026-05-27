# Secret Rotation Runbook

This runbook covers rotation of every secret kars materialises: per-sandbox credentials, TLS certs, AgentMesh identities, and Azure-side credentials. Rotation never requires recompiling the controller or router.

> **Scope.** Production AKS clusters running kars `v1alpha1`. Local `kars dev` stacks rotate by deleting the ephemeral cluster.

---

## 1. Inventory

| Secret | Where it lives | Owner | Rotation cadence |
|---|---|---|---|
| Sandbox channel/plugin credentials | K8s `Secret` `<name>-credentials` in `kars-<name>` | Tenant operator | On token issuance / leak |
| Inference Router TLS cert | cert-manager `Certificate` (chart default) or external SecretStoreCSI | Cluster admin | 90 days (cert-manager auto) |
| AgentMesh identity (Ed25519 + X25519) | Sandbox in-memory; prekey bundle in registry | Per-sandbox, ephemeral | On each sandbox roll |
| Foundry / Azure RBAC | Workload Identity federated credential | Cluster admin | When the project changes |
| Cosign signing key (if static-key signing is enabled) | Azure Key Vault | Release engineer | 180 days |
| Webhook signing keys (admission webhook) | cert-manager-managed | Cluster admin | 90 days |
| `kars up` operator AAD client (if used) | Azure Key Vault → Helm value | Cluster admin | 90 days |

---

## 2. Per-sandbox credentials

Update a single channel/plugin secret without restarting the sandbox:

```bash
kars credentials update <name> --telegram-token <new-token>
kars credentials update <name> --brave-key      <new-key>
```

The CLI patches the `Secret` and triggers a rolling restart of the sandbox `Deployment`. Pods read the new value via `envFrom` (`optional: true` so the rollout is non-blocking even if the secret is briefly missing).

Verification:

```bash
kubectl -n kars-<name> rollout status deploy/<name>
kubectl -n kars-<name> exec deploy/<name> -c openclaw -- env | grep <CREDENTIAL_PREFIX>
```

If the sandbox has multiple channels/plugins, run `update` for each — the CLI is idempotent.

---

## 3. TLS rotation

cert-manager handles automatic rotation. To force a manual rotation:

```bash
kubectl -n cert-manager annotate certificate kars-router-tls \
    cert-manager.io/issue-temporary-certificate=true --overwrite
```

The controller picks up the renewed `Secret` automatically (no restart needed); the router watches its TLS file and reloads the listener.

---

## 4. AgentMesh identity rotation

Identities are ephemeral per sandbox. To rotate:

```bash
kubectl -n kars-<name> rollout restart deploy/<name>
```

This causes the sandbox to:
1. Generate a fresh Ed25519 + X25519 keypair.
2. Re-register with the AgentMesh registry, uploading new prekeys.
3. Tear down all existing Double-Ratchet sessions; peers re-establish via X3DH on first KNOCK.

Persistent peer-to-peer trust scores survive rotation as long as the agent's DID (registry identifier) is unchanged.

---

## 5. Azure / Foundry credential rotation

Use the federated-credential model (default), not static client secrets. To rotate:

```bash
az identity federated-credential update \
    --name kars-controller \
    --identity-name <controller-uami> \
    --resource-group <rg> \
    --issuer https://<aks-oidc-issuer>/ \
    --subject system:serviceaccount:kars-system:kars-controller
```

Static-secret rotation (deprecated path):

```bash
az ad sp credential reset --id <appId> --years 1
# update K8s secret
kubectl -n kars-system create secret generic kars-azure-creds \
  --from-literal=clientSecret='<new>' --dry-run=client -o yaml | kubectl apply -f -
kubectl -n kars-system rollout restart deploy/kars-controller
```

---

## 6. Cosign signing-key rotation (static-key path only)

If `cosign.signing.mode=keyless` (default), nothing to rotate. For static-key signing:

```bash
az keyvault key rotate --vault-name <kv> --name kars-cosign
# update Helm values to the new key URI
helm upgrade kars deploy/helm/kars \
  --set cosign.signing.keyRef=azurekms://<kv>.vault.azure.net/kars-cosign/<new-version>
```

The next image build uses the new key. Old images stay valid until their tag is overwritten.

---

## 7. Compromise procedure

If a credential is suspected compromised:

1. **Rotate immediately** using the procedure in the relevant section above.
2. **Roll the sandbox** (`kubectl rollout restart deploy/<name>`) so any in-process cache is invalidated.
3. **Audit the AGT receipt chain** for the affected window (`AuditLogger` exposes a query API; see [`docs/security.md`](../security.md) §Audit).
4. **Revoke the AgentMesh identity** if the compromise reaches the agent's keypair: drop the registry record, then roll the sandbox to issue a fresh DID.
5. **File an incident**: see [`SECURITY.md`](../../SECURITY.md) — report through MSRC.

---

## 8. Verification checklist after any rotation

- [ ] Sandbox `Deployment` reaches `Ready=True` within 60s
- [ ] Inference router `/health` returns 200
- [ ] Last AGT audit receipt is signed under the **new** key
- [ ] Channel/plugin smoke test succeeds (e.g. `/start` to Telegram bot)
- [ ] No `policy.fetch.failed` or `auth.imds.failed` events in App Insights for 5 minutes
