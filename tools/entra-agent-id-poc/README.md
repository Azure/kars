# Entra Agent ID Provisioning POC

Measures end-to-end wall-clock for provisioning an Entra Agent ID via
Bicep's `Microsoft.Graph` extension, so we can decide whether to inline
the call in `kars up` / sandbox-spawn or keep it pre-provisioned.

## What this POC answers

1. **Do I have the privileges?** `--probe` lists your tenant role
   assignments and tells you up-front whether the deployment will
   succeed before you try.
2. **How fast?** Times each phase: Bicep compile → deploy (app + SP +
   federated credential) → token acquisition → cleanup.
3. **What claims does the token carry?** Decoded after acquisition so
   we can verify what kars' AGT registry would see.

## Required privileges

The deployment principal needs **one** of:

| Role | Notes |
|---|---|
| Application Administrator | Minimal viable, recommended |
| Cloud Application Administrator | Equivalent |
| Global Administrator | Overkill but works |
| Custom role | Must include `microsoft.directory/applications/createAsOwner` + `microsoft.directory/servicePrincipals/createAsOwner` + `microsoft.directory/applications/credentials/update` |

If your tenant has Token Binding Conditional Access (the `AADSTS530084`
error from a stale token), re-auth with:

```
az login --tenant <tenant-id> --scope https://graph.microsoft.com//.default
```

## Usage

```sh
# Probe role assignments without deploying
./drive.sh --probe

# Full provision + measure + cleanup
./drive.sh

# Provision and leave in place for portal inspection
./drive.sh --keep

# Cleanup a --keep-ed run:
#   az ad app delete --id <appObjectId printed by drive.sh>
```

## What the Bicep template creates

`main.bicep` declares three Microsoft.Graph resources:

1. **`Microsoft.Graph/applications@v1.0`** — the Entra "agent identity",
   tagged with `EntraAgentId` so it surfaces in the GA portal section
2. **`Microsoft.Graph/servicePrincipals@v1.0`** — tenant-scoped instance
   of the app, gives it a stable `oid` per tenant
3. **`Microsoft.Graph/applications/federatedIdentityCredentials@v1.0`** —
   maps a synthetic K8s service-account JWT to this app (the
   per-sandbox unit-of-work that kars' `controller/src/fedcred.rs`
   does today via ARM REST)

## How this maps to kars

| Phase | What kars does today | What changes with Entra Agent ID |
|---|---|---|
| Bicep compile | n/a (controller uses raw ARM calls) | n/a |
| App + SP create | `kars mesh setup-trust` (one-time, tenant-wide) | Per-sandbox if we want fine-grained identity, OR one-time if we keep sandbox-as-fedcred |
| Federated credential | `fedcred.rs` ARM call on sandbox create | Same call but via the new Graph endpoint |
| Token acquisition | `entrypoint.sh:158-235` with `api://agentmesh/.default` | New scope per Entra Agent ID GA |
| Cleanup | `fedcred_reaper.rs` on sandbox delete | Same |

## Integration plan (if POC numbers are good)

1. **Phase 1 (~1 week):** swap the scope string in
   `sandbox-images/openclaw/entrypoint.sh` from
   `api://agentmesh/.default` to the GA Entra Agent ID resource ID.
   Update `cli/src/commands/mesh/setup-trust.ts` to register the app
   with the `EntraAgentId` tag.
2. **Phase 2 (~2-3 weeks):** new `KarsAgentIdentity` CRD declaring
   each sandbox's logical agent role + owner + sponsor. Controller
   reconciles to Microsoft.Graph/applications via the deployment SDK.
3. **Phase 3 (later):** Conditional Access, OBO, Defender / Purview
   wire-up (out of scope for the POC).

## Files

- `main.bicep` — the template (84 lines)
- `drive.sh` — wall-clock measurement driver (200 lines)
- `README.md` — this file

Both files are POC scaffolding; nothing in this directory is wired
into the production `kars` deployment path.
