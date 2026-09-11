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
