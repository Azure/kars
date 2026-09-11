# Install Kars with Helm

Use the Helm chart when the Kubernetes cluster, image access, inference backend,
and required identity configuration already exist. The chart can manage
AgentMesh or use an existing external deployment.

Before upgrading an existing controller, run `kars namespace preflight` with the
updated CLI and resolve all ownership conflicts. See
[namespace ownership migration and adoption](namespace-ownership.md). The check
does not read Secrets or change namespaces, workloads, or Helm values.

Optional [workspace credential sources](credential-sources.md) require the
matching controller and CRD. Upgrade both before using `--credential-source`;
legacy direct credential Secrets remain the default.

## Required schema-before-admission stage

Use the matching CLI/chart to stage schemas **before** Helm creates or updates
the admission policies:

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars --context my-cluster \
  --values my-values.yaml --timeout 120
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system --create-namespace --kube-context my-cluster \
  --values my-values.yaml
```

Repeat the same values files and `--set`/`--set-string` overrides in both commands.
For a Helm upgrade using `--reuse-values` or `--reset-then-reuse-values`, pass that
same option to `schemas prepare`; the helper reads the appropriate release values
without printing them. `--check` verifies existing owned schemas without writes.
The command uses the bundled chart when `--chart` is omitted. No Azure deployment,
special controller, privileged Job, probe policy or Bridge-specific schema is
involved.

`up`, fast/full upgrade, Helm-owned image push/mesh updates, and SRE Helm
install/upgrade use the same prerequisite automatically. Local render/apply
installation uses `--ownership template`: newly created CRDs receive explicit
`kars-schema-stage` release/namespace ownership, and subsequent apply excludes
CRDs. Direct render/apply callers must likewise omit CRDs from the later payload;
do not reassign their schema fields to another apply manager. SRE authority's
existing narrowly fingerprinted action-schema repair is followed by the same
published-schema gate before its policies are installed.
CLI rollback resolves an explicit previous Helm revision, stages that recorded
revision's schemas through the same gate, and refuses a rollback that would
remove a CRD or whose release history changed during preparation.

Preparation plans all CRDs before writing, refuses foreign/unmarked ownership,
and never adopts resources by matching a name or label alone. Existing schemas
must match the target chart, their recorded staged schema, or the owning Helm
release's previous manifest. Customized schemas, changed ownership, UID/RV races,
SSA field conflicts, and storage-version/identity migrations stop explicitly.
Updates use UID/resourceVersion-fenced server-side apply with no force conflicts.
Unrelated metadata and customer custom resources are not rewritten or deleted.
An interrupted stage retains any already-created owned schemas for a safe retry;
it does not roll back by deleting CRDs.

`Established` is necessary but insufficient. The gate checks the actual served
resource mapping, fetches `/openapi/v3`, follows its server-relative hashed schema
URL, locates each served GVK and resolves local references as KCM does. The
published declaration surface must match the exact live chart CRD; the URL/hash
and CRD identity/schema are rechecked before proceeding. Missing publication or
references remain pending within the bounded deadline. Auth/transport failures,
untrusted URLs and malformed discovery fail rather than counting as readiness.
This is condition-based polling, not a sleep or a negative-cache workaround.

**Existing failed policies are a separate bound.** Kubernetes 1.31's
[status controller](https://github.com/kubernetes/kubernetes/blob/v1.31.0/pkg/controller/validatingadmissionpolicystatus/controller.go)
skips generations already observed and does not enqueue on CRD schema changes.
Its [type checker](https://github.com/kubernetes/kubernetes/blob/v1.31.0/staging/src/k8s.io/apiserver/pkg/admission/plugin/policy/validating/typechecking.go)
omits the `params` declaration when schema resolution fails.
[KCM initialization](https://github.com/kubernetes/kubernetes/blob/v1.31.0/cmd/kube-controller-manager/app/validatingadmissionpolicystatus.go)
uses a definitions resolver plus
[fresh OpenAPI v3 client discovery](https://github.com/kubernetes/kubernetes/blob/v1.31.0/staging/src/k8s.io/apiserver/pkg/cel/openapi/resolver/discovery.go);
there is no KCM negative schema cache for this flow to clear.
An unchanged policy with already-observed warnings/invalid status is therefore
reported as blocked, not repaired by waiting, generation/text toggles, status
patches or deletion/recreation. A genuine policy upgrade or separate
operator/upstream recovery is required. An existing pending unchanged policy
must finish observation cleanly before the prerequisite succeeds.

CRDs stay in Helm's tracked templates so release ownership, subsequent schema
upgrades and resource retention remain visible to Helm. Schema preparation may
persist even when a later workload upgrade fails; it never promises an atomic
rollback of customer schemas/data. Legacy unmarked template installations need
an explicit ownership migration review, not automatic adoption.
Offline render and transport-fixture tests cover these checks, but repeated cold
installs on the actual Kubernetes/control-plane topology remain required native
evidence, especially for multi-apiserver deployments.

## Local kind

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars --values deploy/helm/kars/values-local-dev.yaml
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-local-dev.yaml
```

Load all referenced development images into kind before installation.

## Existing non-AKS Kubernetes cluster

Start with the generic overlay and layer environment-specific values on top:

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars \
  --values deploy/helm/kars/values-generic.yaml --values my-generic-values.yaml
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml
```

The generic overlay leaves AKS defaults untouched. It disables Azure identity
metadata and schedules sandboxes with the portable `kubernetes.io/os=linux`
selector. It still installs `profiles/kars-strict.json` for existing/default
enhanced sandbox CRs; Helm values do not change those CRD defaults. To use the
portable standard/RuntimeDefault posture, declare it explicitly in each CR as
shown below. Supply pullable images, inference
authentication, a NetworkPolicy-capable CNI, and any environment-specific
integrations. The profile deploys the Microsoft AGT AgentMesh relay and
registry; set `agentMesh.enabled=false` only when those services are managed
externally.

The shipped AgentMesh implementation supports the fixed `agentmesh` namespace
and one registry/relay replica each (zero is allowed for deliberate maintenance).
Other namespaces or multiple replicas are rejected rather than accepted with
broken routing or independent in-memory state. Updates use `Recreate`; expect
clients to reconnect during a mesh upgrade.

After creating a suitable `InferencePolicy` in `kars-system`, replace
`existing-inference-policy` with its name and submit a portable sandbox:

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: portable-agent
  namespace: kars-system
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}
  inferenceRef:
    name: existing-inference-policy
  sandbox:
    isolation: standard
    seccompProfile: RuntimeDefault
```

## Existing AKS

Copy the checked-in template that mirrors the values emitted by `kars up`:

```bash
cp deploy/helm/kars/values-existing-aks.yaml my-aks-values.yaml
# Replace every REPLACE_ME value.

kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars --values my-aks-values.yaml
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values my-aks-values.yaml
```

The template includes controller/router/sandbox and runtime images, Foundry
account/project/deployment values, Content Safety, Workload Identity, Key
Vault, kubelet IMDS identity, federated-credential metadata, AgentMesh, and the
release stamp. The referenced Azure resources and role assignments must already
exist.

The template uses the fixed public `ghcr.io/azure/*` image repositories. To
mirror images into a private ACR instead, replace the repository values, import
every referenced image, and grant the AKS kubelet identity `AcrPull`. Keep one
release tag across every component and set `karsRelease` to that tag; mixing
tags can create controller/router/runtime protocol drift.

The minimum Azure-side prerequisites are:

- AKS with OIDC issuer and Workload Identity enabled;
- public GHCR access, or an ACR containing every privately mirrored image
  referenced by your values;
- a federated controller managed identity and its client ID;
- Foundry/Azure OpenAI data-plane access for the identities used by the router;
- Key Vault/CSI permissions when `azure.keyVaultCsi.enabled=true`;
- NetworkPolicy-capable networking and nodes matching `sandbox.nodeSelector`.

After the required operator schema stage and Helm installation, Kars resources
can be submitted directly with `kubectl apply`. One-pass Helm installation alone
is not a deterministic schema-publication barrier for admission type checking.

## Register an existing AKS installation with the CLI

Kubernetes-facing commands recognize any explicit/current kube context.
Azure lifecycle commands also need the deployment metadata normally written by
`kars up`. After a manual Helm installation, register that metadata without
provisioning or changing infrastructure:

```bash
kars config adopt-aks \
  --subscription <subscription-id> \
  --region eastus2 \
  --resource-group <aks-resource-group> \
  --cluster <aks-cluster-name> \
  --context <kubectl-context>
```

The command verifies the Kars Helm release and `KarsSandbox` CRD, then writes
`~/.kars/context.json`. Add `--acr-login-server <registry>.azurecr.io` when
using a private mirror or when enabling `kars push` and the current ACR-based
`kars upgrade` flow. Optional Workload Identity, OIDC, Foundry, identity, and
Key Vault flags enable the corresponding advanced flows.

`kars push --apply` updates the owning image configuration and verifies the
pushed artifact, rather than merely restarting an old image. Explicit sandbox
image pins are preserved. `sandbox-base` is a build dependency, not a deployable
target; rebuild the sandbox image before applying it.

Core upgrades leave external AgentMesh services untouched. Mesh image updates
are limited to the owning Kars Helm release or the standalone Kars deployment
marked `kars.azure.com/mesh-provider=agt`. Matching service/deployment names alone
do not authorize an update. With external mesh, select an explicit core
`kars push --only` target; the CLI will not adopt or replace external workloads.
