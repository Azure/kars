<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Access and roles

Bridge has two authorization layers:

1. **Persona authorization** in the web application and BFF controls what an
   authenticated human may do.
2. The **BFF ServiceAccount ClusterRole** limits the aggregate Kubernetes
   actions Bridge can perform.

Neither layer replaces the other.

## Roles

| Capability | User | Auditor | Operator | Admin |
|---|:---:|:---:|:---:|:---:|
| Use Workspace missions and teams | Yes | No | Yes | Yes |
| Manage user/workspace connections | Yes | No | Yes | Yes |
| Read Audit receipts and evidence | No | Yes | No | Yes |
| Use Operator Console | No | No | Yes | Yes |
| Manage policies, MCP, skills, and approvals | No | No | Yes | Yes |
| Administrative configuration | No | No | Limited | Yes |
| Change cluster/workspace/user inference budgets or cluster retention | No | No | No | Yes |

Role implication:

- admin implies operator and auditor;
- operator implies user;
- auditor does **not** imply user.

Workspace, Console, and Audit are self-contained persona surfaces.

## Authentication modes

### OIDC

The production-capable mode. The web application performs Authorization Code +
PKCE, verifies issuer, audience, nonce, and JWKS signature, and issues a signed
Bridge session. Group/role claims map to Bridge roles.

### In-cluster Dex

An optional Helm-managed IdP for private-preview testing. It uses the same OIDC
flow and role mapping as an external provider. Seed users and passwords are not
a production identity source.

### Local development

When OIDC and signed principal propagation are not configured, development
role switching may be available. This is for one trusted developer and must not
be exposed as a multi-user deployment.

See [Identity](identity.md).

## Important limitation

The BFF enforces a dedicated User persona for every non-operator API route.
Auditors therefore cannot bypass the UI with direct Workspace API calls.

The BFF still groups many Console mutation routes under the operator persona.
The UI reserves some configuration actions for admins, but do not treat UI
hiding as a hard admin boundary unless the BFF route itself requires admin.
Before public release, every admin-only operation must have explicit
server-side enforcement and direct API tests.

Inference-budget hierarchy and cluster retention mutations have explicit
admin checks in both BFF middleware and handlers. Their current write routes
are `PUT /api/operator/inference-budgets/cluster`,
`PUT /api/operator/inference-budgets/workspaces/{ns}`,
`PUT /api/operator/inference-budgets/users/{user}`, and
`PUT /api/operator/retention-policy`. This includes setting, lowering, disabling,
and clearing settings, not only increases. Operators retain GET access.
Direct BFF requests without a valid signed principal receive 401 under SSO;
valid non-admin principals receive 403 for these writes.

The web request proxy uses the same signed-session role resolver as server
components. Under SSO, an absent/invalid session yields no roles, and neither
`bridge-role=admin` nor an admin `BRIDGE_ROLES` floor grants access. Local
development fallbacks remain limited to the non-SSO, single-developer mode.

## Kubernetes RBAC

The `kars-bridge` ServiceAccount defines Bridge’s maximum cluster permissions.
Every new BFF write path must update the Helm ClusterRole and be exercised
through the deployed ServiceAccount.

```bash
kubectl auth can-i delete karstasks.kars.azure.com \
  --as system:serviceaccount:kars-system:kars-bridge
```

Do not validate Bridge writes using only a cluster-admin kubeconfig; that hides
missing verbs.

## Separation of duties

- Users submit missions, teams, skills, and requests.
- Operators review operational and governance requests.
- Auditors remain read-only.
- Self-approval is rejected for governed resources where separation is
  required.
- Approval actors are derived from the verified session, never trusted from the
  request body.
