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

If enabling bundled Dex, separately build and qualify the
[curated IdP runtime](../idp/README.md), then set `idp.dex.image` to its approved
immutable registry reference. The historical upstream chart default is not
an approved image merely because it is a default: the September 16 scan of
upstream v2.45.1 found High/Critical issues and blocked its use. Do not deploy
it, suppress those findings, or use the dev-role preview as authentication.
The curated source recipe is not itself a published or runtime-qualified image.
External OIDC remains an alternative when genuinely configured by the operator.

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
set -euo pipefail
umask 077
export KUBECONFIG=/absolute/path/to/operator.kubeconfig
CONTEXT=my-cluster
CORE_VALUES=/absolute/path/to/core-values.yaml
BRIDGE_VALUES=/absolute/path/to/bridge-values.yaml

# Fresh core only: refuse an existing namespace rather than adopting it.
NAMESPACE_EXISTS=$(kubectl --context "$CONTEXT" get namespace kars-system \
  --ignore-not-found -o name)
test -z "$NAMESPACE_EXISTS"
NAMESPACE_SOURCE=$(mktemp)
NAMESPACE_OWNED=$(mktemp)
trap 'rm -f "$NAMESPACE_SOURCE" "$NAMESPACE_OWNED"' EXIT
helm template kars deploy/helm/kars --namespace kars-system \
  --values "$CORE_VALUES" --show-only templates/namespace.yaml > "$NAMESPACE_SOURCE"
kubectl label --local -f "$NAMESPACE_SOURCE" \
  app.kubernetes.io/managed-by=Helm -o yaml |
  kubectl annotate --local -f - meta.helm.sh/release-name=kars \
    meta.helm.sh/release-namespace=kars-system -o yaml > "$NAMESPACE_OWNED"
kubectl --context "$CONTEXT" create --validate=strict --dry-run=server \
  -f "$NAMESPACE_OWNED" -o name
kubectl --context "$CONTEXT" create --validate=strict -f "$NAMESPACE_OWNED"

node cli/dist/index.js schemas prepare \
  --release kars --namespace kars-system \
  --chart deploy/helm/kars --context "$CONTEXT" \
  --values "$CORE_VALUES" --timeout 600 --rollback-on-failure

helm --kubeconfig "$KUBECONFIG" --kube-context "$CONTEXT" \
  upgrade --install kars deploy/helm/kars \
  --namespace kars-system --values "$CORE_VALUES" \
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

### AKS composer proxy connectivity

On AKS with non-host-networked konnectivity agents, the Kubernetes `pods/proxy`
connection reaches the sandbox from `kube-system`, not the Bridge namespace.
Core's sandbox default-deny policy intentionally does not grant that source
access. Healthy Bridge and orchestrator Pods alone do not establish connectivity.

After the initial Bridge installation above, run the following from the same
source checkout (requires Python 3 with PyYAML, Helm and kubectl):

```bash
python3 bridge/deploy/configure-orchestrator-proxy.py \
  --kubeconfig "$KUBECONFIG" --context "$CONTEXT" \
  --namespace kars-system --release kars-bridge --check

python3 bridge/deploy/configure-orchestrator-proxy.py \
  --kubeconfig "$KUBECONFIG" --context "$CONTEXT" \
  --namespace kars-system --release kars-bridge
```

The command waits for the controller-created orchestrator namespace, verifies
its Sandbox UID/namespace claim and the AKS proxy Deployment's selector, and
enables `networkPolicy.orchestratorProxy` in the existing Bridge Helm release.
It captures current private release values without printing them, checks a live
render and server dry-run, and refuses **any** manifest change except the one
proxy NetworkPolicy. It cannot upgrade the BFF image or an enrolled template.
The command also reads every non-policy live release resource, rejects missing
objects or drift in chart-declared fields, and rechecks the complete live
UID/resourceVersion snapshots immediately before applying. Thus a recorded Helm
manifest alone cannot authorize reverting a drifted image or recreating a missing
BFF Deployment. Server-defaulted fields and externally managed metadata are
preserved; original Secret `stringData` is compared to its API `data` encoding.
Post-apply configuration readback excludes only status, resourceVersion and
managedFields, not UIDs, generation, templates or credential data.

These client-side checks are not an atomic lock against concurrent operators.
Do not run other release/resource changes concurrently. Unexpected live changes
or ambiguous upgrade responses fail explicitly, with no invented success,
retry or rollback of protected state.
`--check` completes the same preflight without applying an upgrade or probing.
Existing/reused values keep this option off until explicitly enabled.

The added rule permits **both** namespace `kube-system` **and** Pod label
`app=konnectivity-agent`, only to `bridge-orchestrator` Sandbox Pods in
`kars-bridge-orchestrator`, on TCP **8443**. It does not change core's
`sandbox-policy`, expose gateway ports, grant BFF network-policy permissions,
allow other sandboxes, or alter egress. It uses stable selectors, not Pod IPs.
Host-networked, differently labeled or non-AKS proxy topologies fail explicitly;
they are not silently treated as equivalent.

Helm owns the additive policy and removes it on Bridge uninstall. Running the
same command with `--disable` removes only that allowance; the namespace,
Sandbox, controller, grants and baseline network policy are retained. Recorded
namespace/Sandbox UIDs prevent silently adopting a replacement target on a
later upgrade. Ordinary upgrades must retain these values and use live lookup;
offline rendering with the allowance enabled intentionally refuses unverified
ownership. If another chart change is needed, qualify it separately first.

The final probe uses the actual `pods/proxy` router health path. It consumes no
model tokens and does not claim signed BFF authorization, successful inference
or team composition. Complete the authenticated functional acceptance below.

On the September 16 fresh AKS install, Helm 4's `--create-namespace` conflicted
with the core chart's own Namespace, reporting `original object Namespace with
the name "kars-system" not found`; rollback then reported release-not-found.
The operator checked the failure state before retrying. The successful recovery
used the exact rendered Namespace plus Helm ownership metadata, strict server
dry-run and **CREATE only**, followed by the same schema preparation and Helm
installation without `--create-namespace`. The chart was not patched and no
existing namespace was adopted. The sequence above makes that bootstrap explicit.
If an attempt leaves resources behind, stop and inspect ownership and Helm state;
do not blindly repeat it, delete schemas or relabel an existing namespace.

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

The original retirement proof also binds its reviewed private **consumer**
templates. If `kars-bridge-bff` was included in that original scope, changing its
image is not made safe by leaving the root controller unchanged. The current
continuity code verifies the original consumer/template binding; it does not
provide a reviewed replacement-template migration. Do not deploy a BFF fix to
such a scope through an ordinary Helm image update, edit the proof, or assume
a new same-scope preview accepts the changed template. The proxy-only command
above refuses workload manifest changes and does not remove this limitation.

The BFF model-catalog reader accepts the controller's JSON string-array
`FOUNDRY_DEPLOYMENTS` and legacy CSV catalogs, including `KARS_MODEL_CATALOG`.
Malformed arrays fail explicitly instead of producing quoted deployment names.
That source fix requires a matching qualified BFF image; changing a correct
controller catalog to work around an older BFF is not an upgrade strategy.

The CLI now refuses controller-changing upgrade, image-publication and restart
paths before their first mutation when private qualification or retirement
evidence exists. Missing permissions or malformed evidence do not mean an
unqualified installation. Read-only schema `--check`, genuine root-free
operations and original credential-recovery commands remain available. This
client-side preflight is not an atomic operator lock or the missing migration
protocol; direct Helm or Kubernetes writes must not bypass the restriction.

### Optional datapath witness

The witness is a separate, default-off operator-owned Helm add-on, not part of
the core or Bridge installation transaction. Admin UI commands point to the
[witness installation and guarded removal procedure](../../deploy/ebpf-witness/README.md).
Enabling requires an approved aggregator image, explicit sandbox scope and
privileged kernel-capture approval. A missing report does not prove that no
legacy observer is installed; ownership collisions must be resolved without
adoption or duplicate installation.

Capture can include all namespaces on targeted Linux nodes even though only
selected sandbox aggregates are retained. Keep it off on shared GPU nodes
unless the operator explicitly approves that observation scope. Neither Helm
success nor a fresh report establishes complete kernel coverage or enforcement.

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
