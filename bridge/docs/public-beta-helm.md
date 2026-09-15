<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Install the public Kars + Bridge beta branch with Helm

The public integration source is
[`Azure/kars`, branch `kars-bridge`](https://github.com/Azure/kars/tree/kars-bridge).
It is not the normal release channel. Publishing this branch does **not**
publish its images or qualify an arbitrary existing cluster.

This guide separates a fresh Helm installation from functional acceptance.
Kars core is independently usable; Bridge is an optional second Helm release.
Do not uninstall an existing customer installation to follow a fresh-install
example. Read the [core upgrade and schema rules](../../docs/how-to/helm-installation.md)
and [namespace ownership rules](../../docs/how-to/namespace-ownership.md) first.

## 1. Pin source and qualify the environment

Use an existing cluster with operator-approved registry access, identity, CNI,
inference and placement. These steps do not provision, resize or deallocate
AKS, node pools or GPUs.

```bash
git clone --branch kars-bridge --single-branch https://github.com/Azure/kars.git
cd kars
git checkout --detach
git rev-parse HEAD

# Build the matching CLI from the committed dependency lock.
cd cli
npm ci
npm run build
cd ..
```

Retain that commit ID with the installation record. Do not use an unrelated
globally installed CLI or a different chart revision for schema preparation.

Build the selected components from that source into an operator-controlled
registry, using their checked-in Dockerfiles and lockfiles. Record the actual
source revision, platform and immutable digest for each artifact. A retained
image is usable only with verified complete build-input equivalence; changing
its label is not a rebuild. Do not overwrite normal release repositories with
beta images or assume historical chart image defaults are publicly available.
See [Bridge image builds and deployment](deployment.md#install).

Before changing an existing installation, inventory Helm ownership, CRD/schema
compatibility, workload placement and external credentials. Back up sensitive
configuration privately with restricted file permissions, outside the checkout.
Do not copy old controller identities, receipt keys, qualification proofs or
Ready status into a fresh installation.

## 2. Prepare two private values files

Keep credential values out of Git, shell command arguments and published logs.
Use the chart's Secret references and the deployment operator's secret-management
process. Core and Bridge have separate values contracts:

| Surface | Required operator decisions |
| --- | --- |
| Core images | `controller.image`, `inferenceRouter.image`, `sandbox.image`, and each selected `runtimes.*.image` |
| AgentMesh | Existing deployment, or `agentMesh.enabled: true` with qualified registry/relay images and namespace ownership |
| Scheduling | `sandbox.nodeSelector` must match a usable pool with adequate capacity and compatible taints |
| Sandbox isolation | Install and qualify the selected seccomp/isolation prerequisites; a Running controller is not proof of node assets |
| Managed MCP | Qualified `managedMcp.*Image` references and, if configured, a namespace-local `managedMcp.imagePullSecret` |
| Bridge namespace | `namespace` and `core.namespace`; use `createNamespace: false` when joining the core namespace |
| Bridge images | `bff.image`, `web.image`, and optional gateway image; `global.imagePullSecrets` refers to existing Secrets in the Bridge namespace |
| Authentication | Genuine Dex/external OIDC configuration, shared principal-verification secret, correct redirect URI and role mapping |
| Inference | Reachable, authorized backend; preserve Content Safety and Prompt Shields |
| Governed credentials | Matching CRDs/controller and the reviewed private-enrollment prerequisites |
| Finite budgets | Genuine provider contracts, bounds, accounting and operator-owned budget TLS/CA |

For image objects with separate repository and tag fields, keep the repository
separate and use an immutable `latest@sha256:...` tag value. Fully qualified
runtime image fields take the complete reference. Check the rendered manifests
and actual Pod imageIDs, not just values-file text.

On an existing GPU cluster, do not point ordinary agents at a GPU-only pool
merely because inference runs there. Select an approved CPU pool through
`sandbox.nodeSelector`; do not change GPU taints, node labels or the model
deployment to make an agent schedule. An empty selector retains Kars's existing
isolation-pool selection; it does not mean "any node."

Core image access must work through the installation's supported registry
configuration before rollout. Bridge's `global.imagePullSecrets` does not
configure the core controller or every other namespace. Do not substitute an
untracked ServiceAccount patch for a qualified, reproducible installation.

## 3. Install core, then Bridge

The following is the Helm 4 command sequence used for the beta installation.
For Helm 3, use the documented `--atomic` counterpart consistently in both the
schema stage and Helm; see the [core Helm guide](../../docs/how-to/helm-installation.md).
Choose explicit absolute file paths and the intended context:

```bash
umask 077
export KUBECONFIG=/absolute/path/to/operator.kubeconfig
CONTEXT=my-cluster
CORE_VALUES=/absolute/path/to/core-values.yaml
BRIDGE_VALUES=/absolute/path/to/bridge-values.yaml

node cli/dist/index.js schemas prepare \
  --release kars --namespace kars-system \
  --chart deploy/helm/kars --context "$CONTEXT" \
  --values "$CORE_VALUES" --timeout 600 --rollback-on-failure

helm --kubeconfig "$KUBECONFIG" --kube-context "$CONTEXT" \
  upgrade --install kars deploy/helm/kars \
  --namespace kars-system --create-namespace --values "$CORE_VALUES" \
  --rollback-on-failure --wait=legacy --timeout 15m

node cli/dist/index.js schemas prepare \
  --release kars --namespace kars-system \
  --chart deploy/helm/kars --context "$CONTEXT" \
  --values "$CORE_VALUES" --timeout 600 --rollback-on-failure --check

kubectl --context "$CONTEXT" -n kars-system \
  rollout status deployment/kars-controller --timeout=600s

helm --kubeconfig "$KUBECONFIG" --kube-context "$CONTEXT" \
  upgrade --install kars-bridge bridge/deploy/helm/kars-bridge \
  --namespace kars-system --values "$BRIDGE_VALUES" \
  --rollback-on-failure --wait=legacy --timeout 15m

kubectl --context "$CONTEXT" -n kars-system \
  rollout status deployment/kars-bridge-bff --timeout=600s
kubectl --context "$CONTEXT" -n kars-system \
  rollout status deployment/kars-bridge-web --timeout=600s
```

Use exactly the same core values, overrides and rollback mode for preparation
and Helm. A schema/ownership refusal is not permission to force adoption, delete
CRDs, strip finalizers, clear private qualification, or edit Helm history.
Keep-retention protects CRDs; it does not make all configuration changes atomic.

For local UI access, use a loopback-only tunnel:

```bash
kubectl --context "$CONTEXT" --request-timeout=0 -n kars-system \
  port-forward --address 127.0.0.1 service/kars-bridge-web 3000:3000
```

Open `http://localhost:3000` and authenticate normally. An `ok` health response
or an authentication redirect proves reachability, not a usable compose engine.

## 4. Complete functional acceptance

First complete [governed credential enrollment](../../docs/how-to/governed-credential-grants.md)
with the matching CLI, original reviewed scope and genuine retirement evidence.
If recovery is in progress, do not change the sealed controller template to
work around it. Fix/review the cause and resume through the supported workflow.
Do not consume provider device authorization before the credential store is ready.
An authorization token already consumed before a storage failure has no guaranteed
resume path.

### Qualified controller upgrades are not yet supported

In this beta, completing private enrollment seals the controller's reviewed Pod
template into its retirement binding. A subsequent image, environment or other
template change does not have a supported root-template migration sequence.
Keeping the same namespace and Deployment UIDs is insufficient. Even a rollout
restart changes a template annotation; a healthy Deployment after that restart
does not prove private qualification continuity.

Set the intended controller images, sandbox placement and isolation prerequisites
**before initial enrollment**. If enrollment has already started or completed,
do not change that template through Helm or `kars upgrade` and assume another
preview/apply will requalify it. The admitted-Pod recovery repair handles legitimate
AKS admission changes; it does not remove this separate upgrade limitation.
Do not clear qualification metadata, replace the sealed binding, or delete grants
as a workaround. An existing installation needing a controller-template change
must wait for a reviewed migration path.
The lifecycle work is tracked in
[Azure/kars#567](https://github.com/Azure/kars/issues/567).

### Verify managed tools and the user journey

For managed MCPs, the controller first creates and claims its separate namespace.
Only then provision or restore the specifically approved local registry
credential if `managedMcp.imagePullSecret` is set. A same-named Secret elsewhere
does not satisfy it. Do not pre-create/force-adopt the namespace. Installation
remains Pending until the credential, owned rollout and real protocol discovery
are ready; see [Managed MCP troubleshooting](troubleshooting.md#managed-mcp).

Verify the actual orchestrator Pod and router, not only the Sandbox CR phase.
Then exercise authenticated Home intent, proposal review, Team launch, work,
attributable agent activity/artifacts/deliverables and feedback/revision. Use a
controlled test repository; merging a resulting PR still needs explicit authority.
Finite-budget acceptance additionally requires the actual
[budget contract and TLS prerequisites](../../docs/governed-inference-budgets.md),
not guessed prices or an unbounded substitute.

## Executed beta baseline and known limits

The September 15, 2026 clean lab installation used public commit
`3bc7ff58d8d2994e474be39fd2af201d750fc5c2`, Kubernetes 1.35.5 and Helm 4.1.3.
Both new Helm releases reached deployed revision 1; the controller, Bridge and
AgentMesh workloads were Ready, with 21 Established CRDs and 43 observed clean
admission policies. The existing GPU model and cluster infrastructure were retained.

That was **not** a flawless or end-to-end accepted installation. Actual follow-up
found missing registry access, incomplete seccomp-installer reinstatement, a
GPU-pool selector that prevented orchestrator scheduling, AKS admitted-Pod
enrollment incompatibility, provider device-polling defects, and a missing
managed-MCP pull Secret. Preserve these failures alongside their individual
recovery outcomes. Passing component CI is not a substitute for live acceptance.

The managed-MCP incident was subsequently recovered on the same installed public
source. After strict server dry-run and fresh ownership/identity checks, only the
original approved, configured pull credential was restored into the newly claimed
MCP namespace using CREATE-only semantics. The existing controller then reconciled
Playwright to current-generation Ready, discovered 24 tools and produced a Ready
Pod using the configured immutable image. Independent follow-up confirmed those
observations and unchanged protected model/cluster namespace identities. No
controller, ServiceAccount, MCP spec or readiness override was needed. This
recovery does not qualify general node registry access, the other unresolved
installation prerequisites or the complete Home-to-Team journey.

In a later, separately authorized model-only operation, the GPT-OSS
ModelDeployment was removed through AI Runway's normal provider lifecycle to
free the H100 for other work. Its serving workloads were removed, GPU memory
usage was zero and no compute processes remained. The existing nodes, pools,
drivers and operators were retained. The local GPT-OSS inference path is
therefore intentionally unavailable; subsequent beta acceptance must use an
available, explicitly configured inference provider rather than recreate the
removed model or claim the old local path still works.

The destructive cleanup of old, explicitly disposable lab Teams and duplicate
Helm releases was specific to that reset. It is not a customer upgrade procedure,
and no session-local cleanup helper is a product installation prerequisite.
