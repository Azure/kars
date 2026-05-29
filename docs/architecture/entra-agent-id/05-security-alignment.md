# Conditional Access + custom security attributes (Phase 5)

> **Audience**: operators rolling out kars in tenants where Entra
> Conditional Access policies are required.
>
> **Scope**: tenant-admin bootstrap that makes per-sandbox agent
> identities targetable by attribute-driven CA policies. One-time
> per tenant; subsequent kars deployments inherit.

## Why this matters

Per Microsoft's [Entra Agent ID design patterns][design-patterns],
autonomous agent identities are first-class principals: each has its
own appId, can be granted RBAC, can hold Conditional Access policies,
and can carry custom security attributes that drive **attribute-based
CA**. Without those attributes, CA policies cannot target the agent
fleet as a class — operators would have to enumerate individual appIds,
which doesn't scale and rots as sandboxes come and go.

kars Phase 5 wires three pieces:

1. **Custom security attribute set** declared on the tenant. kars'
   recommended baseline is the `AgentGovernance` set with four
   attributes: `AgentClassification`, `DataSensitivity`,
   `ProductOwner`, `ManagedBy`.
2. **Per-sandbox attribute values** declared in
   `KarsSandbox.spec.meshAuth.customSecurityAttributes`. The controller
   PATCHes these onto each per-sandbox agent identity SP at
   provisioning time.
3. **Baseline Conditional Access policy** targeting agents with
   `ManagedBy=kars-controller`, blocking sign-ins flagged by Entra
   risk detection at the `high` level (default; configurable).

[design-patterns]: https://learn.microsoft.com/en-us/entra/agent-id/concept-agent-id-design-patterns

## Bootstrap order

```text
┌──────────────────────────┐    one-time, tenant-admin
│ custom-security-         │ ─→ creates AttributeSet + 4 attributes
│ attributes.sh            │
└──────────────────────────┘
            │
            ▼ (next)
┌──────────────────────────┐    one-time, CA admin
│ conditional-access-      │ ─→ creates / updates the baseline policy
│ baseline.sh              │
└──────────────────────────┘
            │
            ▼ (then per kars cluster)
┌──────────────────────────┐
│ kars up / kars mesh      │ ─→ KarsSandbox CRs reference the attrs;
│ setup-trust              │     controller PATCHes them onto each
└──────────────────────────┘     new agent identity
```

## Step 1: declare the attribute set

```bash
./deploy/bicep/standalone/custom-security-attributes.sh
```

Idempotent. Required role: `Attribute Definition Administrator`.

The script creates:

| Attribute Set     | Attribute             | Type   | Values                                            |
|-------------------|-----------------------|--------|---------------------------------------------------|
| AgentGovernance   | `AgentClassification` | String | `Standard` \| `Restricted` \| `Confidential`      |
| AgentGovernance   | `DataSensitivity`     | String | `Public` \| `Internal` \| `Confidential`          |
| AgentGovernance   | `ProductOwner`        | String | free-form (Entra user OID)                        |
| AgentGovernance   | `ManagedBy`           | String | free-form (kars-controller writes `kars-controller`) |

Override the set name via `ATTRIBUTE_SET_NAME=...` env var if your
governance team prefers a different namespace.

## Step 2: apply the baseline CA policy

```bash
./deploy/bicep/standalone/conditional-access-baseline.sh
```

Defaults: state = `enabledForReportingButNotEnforced` (collect
telemetry without blocking; flip to `enabled` after 7 days of clean
telemetry), risk levels = `high`, targets = the `AgentGovernance`
attribute set.

Override:
```bash
POLICY_STATE=enabled \
SIGN_IN_RISK_LEVELS=medium,high \
./deploy/bicep/standalone/conditional-access-baseline.sh
```

Required role: `Conditional Access Administrator` or `Security
Administrator`.

## Step 3: reference the attributes in KarsSandbox

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: data-extractor
  namespace: my-namespace
spec:
  # ...other fields...
  meshAuth:
    mode: AgentId
    customSecurityAttributes:
      AgentGovernance:
        # Values MUST match the predefined-values list in
        # custom-security-attributes.sh (AgentClassification and
        # DataSensitivity are usePreDefinedValuesOnly=true).
        AgentClassification: "Restricted"
        DataSensitivity: "Confidential"
        # ProductOwner accepts any string — kars treats it as opaque.
        ProductOwner: "a6132685-1079-4d60-b5d9-0b3e172ff3c4"
        # ManagedBy MUST be exactly "kars-controller" for the
        # baseline CA policy filter to match.
        ManagedBy: "kars-controller"
```

The controller PATCHes these onto the per-sandbox agent identity SP
on the FIRST successful reconcile after the CR is created or its
`customSecurityAttributes` field is edited. Subsequent reconciles are
no-ops on Graph (PATCH is idempotent).

## Failure modes

| Symptom                                                      | Cause                                                           | Fix                                                     |
|--------------------------------------------------------------|-----------------------------------------------------------------|---------------------------------------------------------|
| KarsSandbox `phase=Degraded reason=...PatchFailed: 400`      | Attribute set not declared in the tenant                        | Run `custom-security-attributes.sh`                     |
| KarsSandbox `phase=Degraded reason=...PatchFailed: 403`      | Controller identity lacks `CustomSecAttributeAssignment` perm   | Grant the controller MI / blueprint SP the role         |
| Agent signing in despite policy `state=enabled`              | Policy filter does not match agent's attributes                 | Verify `ManagedBy=kars-controller` was PATCHed (Graph)  |
| Policy blocks a legitimate sign-in                           | Risk level threshold too low                                    | Inspect risk reason in audit log; raise threshold       |

## Scale-out invariant

When a KarsSandbox scales to N replicas, all replicas share ONE agent
identity (and one set of custom security attributes). The scale-out
attribute fan-out anti-pattern — where each pod gets its own identity
with its own attributes — is enforced architecturally in
`controller/src/agent_id_provisioning.rs` (the agent identity is keyed
on `KarsSandbox.metadata.uid`, never on pod ordinal or replica index).
Verified by `tag_layout_excludes_per_pod_attributes` unit test.

## Foundry RBAC inheritance

The companion Bicep template
`deploy/bicep/standalone/foundry-rbac.bicep` grants `Azure AI User`
on a Foundry resource to the **blueprint SP** (not per-agent). All
derived agent identities inherit access — eliminating per-agent
role-assignment churn. Per the research/critique recommendation R5.
