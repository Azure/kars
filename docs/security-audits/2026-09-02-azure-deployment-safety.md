# Security Audit — Azure deployment safety hardening

Date: 2026-09-02
Scope: `cli/src/commands/{up,add,destroy,upgrade}.ts`,
`cli/src/commands/up/`, `cli/src/commands/mesh/agent_id_setup*.ts`,
`cli/src/preflight.ts`, `cli/src/lib/vm-size.ts`, `deploy/bicep/`.
Gated paths: Azure deployment, identity, rollback, upgrade, and sandbox
lifecycle commands.

## Summary

This change hardens `kars up` against current Azure platform behavior and
partial-deployment hazards reported in Azure/kars#515 through Azure/kars#518.
It removes deprecated ACR Notary v1 configuration, replaces a static AKS
version pin with regional standard-support discovery, preserves existing
resource-group metadata, and moves deterministic name, topology, resource, and
quota failures into preflight.

The change also adds bounded rollback for a cryptographically generated
resource group. Rollback ownership is represented by an Azure
`CanNotDelete` management lock and an internal marker; customer-selected,
cached, default, and pre-existing groups never receive automatic deletion
authority.

## T1: New capability / attack surface? YES

- `--rollback-on-failure` can delete a resource group, but only when the same
  invocation generated a short random group name and created its scoped lease.
  Explicit or existing groups fail closed and cannot acquire rollback
  ownership.
- Azure subscription selection is now carried explicitly through deployment,
  Agent ID setup, image acquisition, sandbox creation, add, upgrade, fast
  upgrade, and destroy. This reduces the pre-existing ambient-subscription
  confused-deputy risk.
- Soft-deleted Key Vault recovery is new. Discovery requires an exact
  subscription, resource-group resource ID, location, and bounded generated-name
  match; ambiguous or malformed results fail closed.
- Management-lock operations add Azure control-plane calls. Lock permissions
  and resource-group delete permission are required only when rollback is
  explicitly enabled.

## T2: Security-control change? YES

- Removed ACR `policies.trustPolicy` because it configures deprecated Notary v1
  Docker Content Trust and is rejected by current Azure. This does not remove
  Kars release signing, cosign verification, admission policy, dependency
  review, secret scanning, or image scanning.
- Existing resource groups are read before creation. Customer tags are neither
  replaced nor merged by normal `kars up`.
- Existing AKS clusters are reuse-only when the control plane is Running, the
  required pools are healthy, the retained deployment succeeded, and its ACR,
  Key Vault, managed identity, and applicable AI resource still exist in the
  selected subscription and resource group.
- Full managed-cluster Bicep mutation is rejected for existing AKS clusters.
  Kubernetes version, support plan, upgrade policy, pool names, pool VM sizes,
  system count, Kata topology, and operator-managed properties cannot be
  silently reset by ancillary recovery.
- Existing pool scaling, creation, repair, SKU migration, and version upgrades
  are directed to dedicated AKS workflows instead of being represented as an
  unsafe full-cluster update.
- A stopped cluster, incomplete topology, ambiguous subscription, unavailable
  SKU, unsupported version, missing quota record, or malformed Azure response
  fails before mutation.
- Rollback leases use random bytes rather than custom cryptography. A concurrent
  adopter establishes a separate delete guard before changing resources; the
  original rollback removes only its own lease, so the adopter's guard prevents
  group deletion.
- Full destroy removes only recognized Kars lease/guard locks. Customer-created
  management locks are not removed.

## T3: Availability / fail-open risk? REDUCED

- Regional AKS version discovery accepts both current Azure CLI response shapes
  (`values` and `valuesProperty`) and rejects LTS-only versions for new
  standard-tier clusters.
- Preflight accounts for total regional vCPU quota and every selected VM-family
  quota, including the additional confidential Kata pool.
- Generated resource and AKS node-resource-group lengths are validated before
  Azure mutation.
- Purge-protected Key Vault tombstones are recovered on retry instead of
  blocking deployment until retention expires.
- Successful rollback clears resumable local context, preventing deleted ACR
  image phases from being skipped.
- Resume, add, upgrade, and destroy resolve a unique deployment subscription;
  zero or multiple matches fail closed.
- Kubernetes-only sandbox destroy remains available when Azure authentication
  has expired; federated-credential cleanup remains best effort.
- The draft is not ready to merge until a full disposable customer deployment
  runs in a subscription granting
  `Microsoft.Authorization/roleAssignments/write`. The available validation
  subscription denied that action after ARM validation; all disposable resource
  groups were removed, and Key Vault recovery was qualified independently.

## Verification

- Full CLI suite: 67 test files; 1,151 passed; 2 intentionally skipped.
- TypeScript typecheck and npm package build passed using the verified local
  dependency cache; no network package installation was used.
- oxlint completed with 0 errors; 28 warnings predate this change.
- Bicep CLI 0.44.1 build and lint passed.
- Generated ARM and npm-bundled deployment assets are deterministic and contain
  neither ACR `trustPolicy` nor a hardcoded Kubernetes `1.33`.
- CI LOC gate passed after decomposing production and test modules below active
  caps; no LOC override was used.
- Live read-only Azure validation covered version/SKU/quota discovery,
  subscription scoping, existing tag preservation, retained resource
  resolution, and existing-cluster fail-closed behavior.
- Live soft-deleted, purge-protected Key Vault discovery and recovery succeeded.
- Dependency review, secret scan, CodeQL language analyses, Trivy, Rust audits,
  container scan, Dockerfile lint, Helm lint, and Bicep validation were green on
  the initial draft revision.
- Multiple independent automated code-review passes were completed. The Copilot
  sign-off below records automated independent review and is not a substitute
  for maintainer approval.

## Verdict

Accept for continued draft review. The controls fail closed and materially
reduce deployment and rollback risk, but the PR must remain draft until the
documented role-assignment-enabled disposable deployment and reporter retest
complete.

Signed-off-by: pallakatos <pallakatos@users.noreply.github.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
