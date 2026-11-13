<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Install the public Kars + Bridge beta branch with Helm

The public integration source is
[`Azure/kars`, branch `kars-bridge`](https://github.com/Azure/kars/tree/kars-bridge).
It is not the normal release channel. Publishing this branch does **not**
publish its images or qualify an arbitrary existing cluster.

This guide separates the **unqualified Helm-only first-install preview** from
functional acceptance and the older CLI-assisted compatibility procedure.
Kars core is independently usable; Bridge is an optional second Helm release.
The beta gate requires exactly those two product releases, without raw-manifest
product bootstrap or a separate `kars schemas prepare` command. Passing existing
CLI-assisted installation checks does not satisfy that gate.

Do not uninstall an existing customer installation to follow a fresh-install
example. Read the [core upgrade and schema rules](../../docs/how-to/helm-installation.md)
and [namespace ownership rules](../../docs/how-to/namespace-ownership.md) first.

## Release artifacts and the two Helm commands

The existing `release-public-interim.yml` workflow packages Core (`kars`) and
Bridge (`kars-bridge`) charts, the matching CLI tarball, release profiles,
image receipts, checksums and signatures. It builds the maintained AL3 image
recipes, including the Bridge services, curated IdP, schema helper, dispatcher,
monitoring, managed MCP and datapath witness. Core's locked subcharts are bundled;
customers do not need to rebuild images or restore chart dependencies after
matching release artifacts have actually been published.

**Source packaging changes are not publication or installed acceptance.** A release
operator must select an unused explicit SemVer from the protected `kars-bridge`
source. No beta version is selected by this guide. Do not substitute a historical
stable release or GitHub's `/latest/` URL for the selected beta's artifacts.
Prerelease versions use GitHub prereleases and npm's `next` channel and do not
advance stable GitHub, npm or container `latest` pointers.

Before installing, satisfy the detailed private-values and cold lifecycle
requirements below, including:

- Helm **4.1.3** and Kubernetes **1.35** for the currently reviewed cold schema
  lifecycle; `schemaHook.enabled: true`, the release-bound helper image, and
  the exact API destination address/port pairs. Other versions require separate
  qualification, not a claim of compatibility with literally every Kubernetes.
- Enforced compatible CNI/NetworkPolicy, reviewed node isolation/seccomp and
  witness/kernel capability, storage, DNS and sufficient non-GPU agent capacity.
  The packaged Core, Bridge, OpenClaw and Hermes image jobs target amd64/arm64;
  other Python adapter jobs currently publish **amd64 only**. Confirm the chosen
  architecture and installed behavior before rollout. `langgraph-ts` is not
  published by this workflow; do not select it without an independently qualified
  operator image. Optional external Entra sidecars and standalone Inspektor Gadget
  also require operator-qualified images; upstream witness OCI gadget digests
  and the external Microsoft AL3 seccomp base are not rebuilt by this release.
- Genuine HTTPS OIDC, browser-reachable DNS/TLS and redirect URIs, RBAC/role
  mapping, principal verification, provider credentials and private enrollment.
  Packaged IdP/Bridge images are not an identity configuration or dev-auth bypass.
- A compatible authorized inference provider with honest capacity metadata and
  actual usage accounting. **Confirmed limitation:** stock Ollama `/v1/models`
  lacks capacity metadata required by both native runtimes; native Missions fail
  before execution unless an exact live operator `ModelContract` provides those
  bounds. No Ollama fix or universal provider acceptance is claimed. Model
  capacity bounds are not a Kars spending-cap requirement.
- For Kind beta, retain unbounded, non-expiring SRE funding and record-only
  spending policy from the matching beta profile. Do not substitute finite grants
  or disable authentication, isolation, secret protection or accounting. Production
  spending enforcement remains an explicit separate policy.

Prepare private `core.yaml` and `bridge.yaml` using the sections below. Download
and verify the selected release's `SHA256SUMS`, `SHA256SUMS.cosign.bundle`, chart
archives and profiles before installing; verify GitHub provenance against
`Azure/kars` and the release workflow/source. The release's image overrides must
be applied last so an older local values file cannot silently restore stale
`latest` images. For a compatible **fresh generic cluster**, the installation
itself is these two commands (replace the version placeholder first):

```bash
KARS_VERSION='<selected-semver-without-v>'
KARS_RELEASE="https://github.com/Azure/kars/releases/download/v${KARS_VERSION}"

helm install kars "${KARS_RELEASE}/kars-${KARS_VERSION}.tgz" \
  --namespace kars-system --create-namespace \
  --values "${KARS_RELEASE}/core-values-generic.yaml" \
  --values core.yaml --values "${KARS_RELEASE}/core-release-images.yaml" \
  --wait --timeout 20m
helm install kars-bridge "${KARS_RELEASE}/kars-bridge-${KARS_VERSION}.tgz" \
  --namespace kars-system \
  --values bridge.yaml --values "${KARS_RELEASE}/bridge-release-images.yaml" \
  --wait --timeout 20m
```

For Kind beta, add `--values "${KARS_RELEASE}/core-values-kind-beta.yaml"`
after the generic profile and before `core.yaml`, and add
`--values "${KARS_RELEASE}/bridge-values-kind-beta.yaml"` before `bridge.yaml`.
Review the resulting values against the beta requirements below; private overrides
must not undo the beta funding/isolation contract. The ordinary source defaults
stay unchanged; only CI's packaged charts/profiles bind to the published digests.

These are **install**, not upgrade/reinstall/rollback, commands. Do not add
`--atomic`/`--rollback-on-failure` to the retained cold-helper path. Existing Core
owners must follow the identity-preserving upgrade rules and install only Bridge
when appropriate. Wait/Ready success is not customer workflow acceptance.

The matching CLI is optional for these two Helm installs. Its authoritative
artifact is `${KARS_RELEASE}/kars-runtime-cli-${KARS_VERSION}.tgz`, installable
with `npm i -g <that-url>`. `release-manifest.json` and `npm-publication.json`
record the explicit npm channel and actual outcome: an existing registry version
is **unverified**, not a successful publication of the new tarball. SemVer build
metadata is preserved in the downloadable charts/CLI but skipped for npm
publication because npm normalizes it. A failed npm attempt does not remove the
signed downloadable CLI.

### Release operator checks still required

The workflow checks source ancestry, refuses overwriting an existing release,
restores locked Helm dependencies before CLI bundling, and checks anonymous GHCR
access against each image receipt before publishing charts. Newly created GHCR
packages may initially be private: set all required packages (including
`openclaw-sandbox`) public through the organization package settings, then retry
the failed packaging job. Configure the npm OIDC trusted publisher for
`Azure/kars` and this exact workflow; token-based fallback is not assumed.

A failed-job rerun uses the original workflow revision. If packaging needs a
workflow repair, publish that repair first, then use the **same unpublished
version** with the original failed run ID:

```bash
gh workflow run release-public-interim.yml --repo Azure/kars --ref kars-bridge \
  -f version=v0.2.0-beta.6 -f recovery_run_id=43395564436
```

This optional recovery mode validates the original successful producers and
skips every image, binary, wheel and manifest build. It reuses retained receipts,
binaries and SBOMs by immutable artifact IDs and verified archive SHA256 hashes;
only validation, chart/CLI packaging and publication run again. It refuses
expired/missing artifacts, incomplete producers, source/version mismatch and
product changes outside the packaging-repair allowlist. Protected-source
ancestry, anonymous image access, lockfile checks and signing remain required.
An existing release is never overwritten. Omit `recovery_run_id` for a normal
full release; do not omit it when recovering successful producer work.

In recovery releases, `release-manifest.json` distinguishes `producer` from
`packaging`. The checksum-covered `release-recovery.json` records original run,
source, attempt, artifact IDs and archive hashes. After a packaging-only rerun,
recovery distinguishes the latest failed packaging attempt from the original
producer attempts and job IDs; GitHub's copied successful job rows are not new
builds. Images and original binaries retain their original producer provenance;
newly assembled chart/CLI tarballs and checksum attestations identify the recovery
workflow commit/run instead.

Run `node --test cli/scripts/release-artifacts.test.mjs` for the lightweight
packaging regression tests (Node, locked CLI dependencies and Helm required).
Add `KARS_RELEASE_NETWORK_TESTS=1` to reproduce the missing-repository error in an
isolated empty Helm configuration, then verify that the actual fixed workflow
step restores the exact locked dependencies. This does not change your Helm
configuration. Run
`PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s ci/tests -p release_recovery_test.py`
for the artifact-recovery checks. These checks cover versioning, receipts,
recovery scheduling/provenance, chart rendering/packaging and bundled
dependencies, not full image builds or cluster acceptance. Public
multiarchitecture builds/scans/signatures, registry visibility, artifact download
verification and the actual Helm/customer journeys still need successful release
and acceptance evidence. Until then, this remains the unqualified preview.

## 1. Pin source and qualify the environment

Use this source-based procedure when matching published artifacts are not yet
available, or when qualifying a source change rather than a released package.

Use an existing cluster with operator-approved registry access, identity, CNI,
inference and placement. These steps do not provision, resize or deallocate
AKS, node pools or GPUs.

```bash
git clone --branch kars-bridge --single-branch https://github.com/Azure/kars.git
cd kars
git checkout --detach
git rev-parse HEAD

# Restore exact Chart.lock dependencies before CLI asset bundling.
helm dependency build deploy/helm/kars

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
| Headlamp | Core `headlamp.image` must be built/qualified from `sandbox-images/headlamp`; configure `headlamp.oidc` for real SSO and Bridge `headlamp.url` for its browser-reachable URL; no manual bearer-token step in the Kind beta path |
| Datapath witness | Core-managed `datapathWitness` enabled with qualified AL3 images, `scope: all`, `sandboxes: []`, reviewed node selection and a kernel-compatible capture mode; no third product release |
| Hermes SRE | Core `sre.enabled: true`, the qualified Hermes runtime, a real routed `sre.provider` / `sre.model`, governed credentials and unbounded non-expiring beta funding; selection alone does not provision a provider |

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

### Integrated Kind command

The source CLI wires the maintained cold Helm lifecycle into one Kind start command:

```bash
npm install --global <verified-beta-cli-tarball-url>
kars dev --release --target local-k8s --beta-config
```

This is source integration, **not a claim that the published npm package/images already contain it**. A matching packaged CLI, qualified images and actual clean-machine/customer acceptance remain release prerequisites. No additional product installer, grant-renewal command or preview IDP is involved.

With no filename, setup prompts for initial inference through the maintained provider helper and a real new local email/password. It provisions release-bound packaged Dex as a separate prerequisite inside the fresh owned Kind node, after Cilium and before Core. Full Bridge continues to consume verified external-style HTTPS OIDC and reject legacy bundled Dex. The actual email claim maps to Bridge admin and prefixed Kubernetes User readers: built-in `view` plus explicit read-only Kars dashboard/SRE resources, nodes/namespaces and metrics. No Secrets reads, writes or wildcard grants are added; static-password groups are not assumed. Separate client secrets and registered callback URLs are generated for Bridge and Headlamp. Each application can authenticate with the same account; automatic shared-browser-session SSO is not enabled by this setup.

The unique `.kars.test` issuer is routed through loopback HTTPS443 to a gateway-restricted NodePort, with matching in-cluster DNS and API-server host-network routing. Setup explicitly asks permission for one marked hosts entry and its constrained one-year CA trust (macOS login keychain or Linux `update-ca-certificates`). It does not weaken TLS verification, install a shared signing authority, or silently modify trust. Non-root Dex uses a prebound local PVC owned by that exact node. The fresh catalogue is explicitly empty: pricing is unknown, while real authorized usage continues to be recorded under non-expiring unbounded Kind-beta allowances.

Private `.kars-kind-beta-<name>` state retains exact runtime endpoint, immutable node custody and failures. Run `kars dev down --cluster-name <printed-name>` from the same directory to delete only that recorded node and reverse owned hostname/trust entries. Ctrl+C only stops owned tunnels. Fresh attempts must not reuse old directories, funding or execution identities. This implementation has unit/render evidence, not live customer acceptance.

Explicit-file external OIDC remains available with `kars dev --release --target local-k8s --beta-config ./kind-beta.yaml --cluster-name kars-beta`. Prepare `kind-beta.yaml` alongside the reviewed Core/Bridge values and credential/RBAC resource files. The version-one input shape is:

```yaml
version: 1
nodeImage: "kindest/node:v1.35.5@sha256:<qualified-node-digest>"
ciliumVersion: "<reviewed-compatible-exact-x.y.z>"
coreValues: core.yaml
bridgeValues: bridge.yaml
resources:
  - credentials.yaml
  - oidc-readers.yaml
oidc:
  usernameClaim: email
  usernamePrefix: "oidc:"
  groupsClaim: groups
  groupsPrefix: "oidc:"
  # Optional public PEM CA bundle, trusted by API server, Headlamp and Bridge:
  # caFile: oidc-ca.pem
```

Replace the bracketed image/version inputs with genuinely qualified values; they are not executable defaults. Paths resolve relative to this config. Keep these files private and outside source control. The issuer and API-server audience come from Core `headlamp.oidc.issuerURL` and `clientID`. The external issuer must expose verified HTTPS discovery/JWKS before Kind starts and be reachable from the host, node and pods; configuring an external identity provider/application is an actual environment prerequisite. Register both callback URLs, issue the selected username/group claims and supply the reviewed Bridge role map. The CLI does not create users, fabricate claims or mint provider credentials.

For automatic Bridge-to-Headlamp entry, use the packaged Headlamp OIDC navigation repair and set `BRIDGE_HEADLAMP_SSO_ENABLED=true` in Bridge `web.extraEnv`. This starts Headlamp's own authorization flow and returns to the selected cluster view after verification; it does not share Bridge tokens. The common IdP must support browser-session SSO between the separately registered clients. For Dex 2.46.1, enable `DEX_SESSIONS_ENABLED=true`, retain `sessions.ssoSharedWithDefault: none`, and explicitly list the Headlamp client under the Bridge client's `ssoSharedWith`. One fresh legitimate IdP login is needed to create that session after upgrading from Dex 2.45.1. Expired sessions still require sign-in. Keep this flag off for unpatched external dashboards or token-only access.

For the default local URLs, Core sets `headlamp.oidc.callbackURL: http://localhost:4466/oidc-callback`; Bridge sets `headlamp.url: http://localhost:4466` and `BRIDGE_OIDC_REDIRECT_URI: http://localhost:3000/auth/callback` in `web.extraEnv`. Other explicit, distinct unprivileged loopback ports are accepted. Bridge and Headlamp must share the issuer. Core `headlamp.oidc.clientSecret.sourceName` must reference the provided source Secret in `kars-system`; its `name`/`key` select the maintained projection into `headlamp`. Supply the budget TLS Secret (`tls.crt`/`tls.key`) and Bridge's `BRIDGE_OIDC_CLIENT_SECRET` Secret reference in the resource files. For prepared inference authentication, also supply `inferenceRouter.azure.openai.credentials.existingSecret`/`key` and its Secret. Set `auth.principalSecretName`; if that Secret is absent from the inputs the CLI generates a cryptographically random shared session secret once and creates it directly in Kubernetes, never in a values file or command argument.

`resources` accepts only namespaced Secrets/ConfigMaps in `kars-system`, explicit read-only ClusterRoles, and OIDC User/Group RoleBindings/ClusterRoleBindings targeting the built-in `view` role or those supplied reader roles. Subjects must use the configured prefixes; anonymous/system/service-account grants, `cluster-admin`, Secrets reads, wildcard/subresource grants and reader writes are rejected. For example, pair a built-in `view` binding with a supplied reader role listing the intended `kars.azure.com` resources and `metrics.k8s.io` pods/nodes, all with `get/list/watch`. This is Kubernetes dashboard authorization; Bridge's explicit role map remains independently enforced. The CLI does not grant write access just to load a dashboard.

For Headlamp's SRE Console, include `karssandboxes` **and** `karssreactions` in the intended dashboard reader's `kars.azure.com` rule with `get/list/watch`. The console lists these resources across namespaces; a namespace-only binding does not authorize that view. The built-in `view` role alone does not include these custom resources. Bind this access only to the intended operators; SRE action approval/patch permissions are separate and are not required to read the console. A successful OIDC login with a missing read grant displays **SRE state unavailable**, not evidence that SRE is absent or unhealthy.

Core owns the inference configuration. Set `controller.inferenceDefaults.provider` and `.model` for the bootstrap route, using `foundry.endpoint` for `foundry` or `inferenceRouter.azure.openai.endpoint` for `azure-openai`. This cold profile currently qualifies only these HTTPS, API-key-backed routes, not every standalone `kars dev` provider. Explicit SRE overrides remain independent; absent Core defaults can use explicitly supplied SRE defaults for compatibility. Keep the selected model consistent with the reviewed broker contracts and actual provider availability.

For unbounded Kind beta, set `inferenceBudget.calendarPolicy: record-only` and exactly `sre.firstInstallFunding: {unboundedTokens: true}`. With both explicit policies, `inferenceBudget.contracts: []` is supported; the catalogue version, existing budget TLS Secret/CA and pinned router digest remain required. This avoids inventing prices or model bounds during first setup. Enrollment still binds fresh account/execution identities, and the existing broker authorizes record-only requests and records actual usage. Missing price information remains unknown, not zero or free. Empty catalogues do not enable unbounded authority by themselves: finite or monetary budgets still need genuine matching contracts, and non-beta chart profiles reject an empty catalogue. Do not copy old funding records into the fresh inputs.

When the provider Secret reference is omitted, the command reuses compatible saved Foundry credentials or asks interactively for the missing endpoint, API key and default model through the existing CLI credential prompts. Supplied endpoint/model values stay fixed. The shared prompt saves credentials in the existing private CLI configuration; installation sends the key directly to Kubernetes as a Core Secret, never as a Helm value or command argument. Prepared Secret references instead require their matching explicit endpoint/model and never consult laptop credentials. Noninteractive runs without either compatible saved configuration or complete prepared inputs fail clearly. Bridge receives no copied provider, model or inference credentials: it inherits the live Core configuration.

**Existing Core is not a cold-install target.** Install only the separate Bridge Helm release with the reviewed Bridge identity/enrollment values, leaving the independently functional Core release, SRE, provider credentials and workloads untouched. Do not rerun the Core steps below, adopt its namespace, or use the cold Kind command as an upgrade/attachment command. Discovery failures do not authorize a fresh install. Bridge removal must target only its own release, never delete the shared namespace or uninstall Core. Preserve external identity and credential resources that Core or other consumers still use; inspect release ownership before any removal.

The command performs these steps, without touching an existing cluster:

1. Validate reviewed inputs, both bundled charts, issuer discovery and available local ports. Refuse an existing target cluster before prompting; resolve missing Core inference settings after these checks. Require Helm 4.1.3 before cluster creation.
2. Create fresh Kind using the pinned node, kubeadm v1beta3 map patches for OIDC settings (converted by Kind to its kubeadm version), optional read-only public CA and disabled default CNI. Use a dedicated kubeconfig, never change the ambient context, and retain failed Kind nodes for inspection.
3. Install the pinned upstream Cilium chart (Kubernetes IPAM, kube-proxy retained), wait for its DaemonSet/CRD and node readiness, then derive exact API Service/EndpointSlice `/32` + port pairs for the existing schema-hook policy. Select only the new control-plane node for the witness and sandbox.
4. Render Core's maintained restricted namespace and create the reviewed Secrets/ConfigMaps/RBAC without adoption or overwrite. The existing Core namespace template explicitly supports this unowned operator namespace.
5. Install Core `kars` with generic + Kind beta + reviewed + discovered-environment values, using its real schema/SRE hooks. After Core succeeds, install Bridge `kars-bridge` with its maintained Kind beta profile. No raw AgentMesh deployment, compatibility schema staging, rollback or identity replay is used.
6. Start loopback-only Bridge/Headlamp tunnels, check HTTP reachability and open Bridge. Keep the command in the foreground; Ctrl+C or a failed tunnel stops only its own forwards, not the cluster.

The private `.kars-kind-beta-<cluster>/` directory in the invocation directory retains the dedicated kubeconfig, Kind configuration and non-secret generated overrides. Its own `.gitignore` excludes all contents from ordinary Git staging; do not force-add it. Rerunning first install refuses an existing cluster or attempt directory. After closing the foreground command, reconnect with ordinary access commands in separate terminals, using the same reviewed callback ports:

```bash
kubectl --kubeconfig .kars-kind-beta-kars-beta/kubeconfig --context kind-kars-beta \
  -n kars-system port-forward --address 127.0.0.1 service/kars-bridge-web 3000:3000
kubectl --kubeconfig .kars-kind-beta-kars-beta/kubeconfig --context kind-kars-beta \
  -n headlamp port-forward --address 127.0.0.1 service/headlamp 4466:80
```

These are optional later access commands, not additional setup. Retain and inspect failed installs; a deliberate new attempt uses a new cluster name rather than reusing consumed identities. Only `--cluster-name` may accompany the beta route: standalone provider/build/image/mesh/ephemeral flags are rejected rather than silently ignored. Cilium readiness and reachable login pages still require real policy/authentication/workflow acceptance; neither this route nor the Helm recipe establishes AKS parity.

### Helm-only first-install preview — the beta acceptance path

Use the [Core schema bootstrap guide](../../cli/schema-hook/README.md) for the
full first-install contract. This path is **not yet an installed or released
beta**: a matching built helper image and the actual Helm/admission/CNI lifecycle
remain to be qualified. No qualified helper image digest is supplied by this
source change.

For the first, non-Entra Kind gate:

- Use Helm **4.1.3**, Kubernetes **1.35**, and an enforced NetworkPolicy-capable
  CNI. The existing Kubernetes 1.31 CI fixtures do not qualify this new path.
- Use a fresh, dedicated `kars-system` namespace and original revision-one Core
  release, not an existing release, replacement or upgrade.
- In the reviewed Core values, enable `schemaHook`, supply its exact image
  repository/digest/architecture and API destination pairs, and leave
  `entraSidecar.enabled` and `policyPrototype.enabled` false.
- Layer Core `values-kind-beta.yaml` over `values-generic.yaml`, then the private
  operator values; layer Bridge `values-kind-beta.yaml` before its operator values.
  These are inputs to the existing Helm caller, not another installer/release.
  Both charts execute fail-closed profile validation during rendering.
- Configure Bridge for `namespace: kars-system`, `core.namespace: kars-system`
  and `createNamespace: false`. This bounded cold profile uses genuine external
  HTTPS OIDC available **before Core**; bundled Dex and development-role fallback
  are not supported in this profile. Other existing profiles remain unchanged.
- Include Core-managed witness, Hermes SRE and tokenless Headlamp in the initial
  two-release configuration. These are mandatory for Kind beta acceptance,
  although the generic Core chart keeps witness and SRE opt-in. The final
  bootstrap must supply their reviewed images, capture prerequisites, OIDC
  integration, routed SRE provider/model and unbounded non-expiring funding without subsequent
  product-install or feature-activation commands. The legacy Core-only
  `kars dev --target local-k8s` flow does not yet meet this contract.
- The Core first-install lifecycle now wires SRE through a separate bounded
  post-install caller when both `schemaHook.enabled` and `sre.enabled` are set.
  The Kind beta overlay selects exactly `sre.firstInstallFunding: {unboundedTokens: true}`,
  with no token cap or expiry. Supply an explicit broker catalogue (empty is supported
  only with the record-only unbounded policy above) and immutable
  controller/Hermes/router/helper images. Cold Helm manifests retain immutable intent
  instead of premature SRE CRs; the caller binds that policy to the controller-created
  source and namespace UIDs in canonical registration, publishes the existing
  UID/digest-bound account allowance and waits for controller consumption before
  completing. Actual usage is retained. Explicit finite funding remains available in
  non-beta profiles. Existing authority and budget paths enforce this without model
  calls, replay funding or automatic failure recovery.
  See [the lifecycle and failure contract](../../cli/schema-hook/README.md#integrated-sre-first-install-lifecycle).
  This source integration is not installed acceptance or a completed vanilla
  bootstrap: genuine inputs, matching image builds and combined two-chart
  qualification remain required.

#### Mandatory operations inputs and cold ordering

The maintained beta profiles enable witness, unbounded Hermes SRE, Headlamp OIDC and
Core AgentMesh together without changing generic chart defaults. Missing or
conflicting inputs fail in the actual Helm caller, before resource creation:

- Supply `kindBeta.cni: calico` or `cilium`, with the corresponding CNI APIs
  already installed and `cilium.enabled` matching. API discovery is only a
  prerequisite, **not proof of policy enforcement**. Configure Kind API-server
  OIDC issuer/client/CA trust at cluster creation; Helm cannot supply that trust.
  Provision the operator's reviewed Kubernetes user/group RBAC before installation
  as well: successful OIDC login does not grant access to cluster resources.
  Headlamp readers need explicit read permissions for the intended Kubernetes,
  `kars.azure.com` and `metrics.k8s.io` resources. Bind the API server's actual
  prefixed OIDC principals, not Headlamp's shared ServiceAccount; do not grant
  Secrets access or writes merely to make the dashboard load. Verify these
  permissions and denials through a real signed-in user session.
- Supply one exact approved Kind node in `datapathWitness.nodes`, the same
  `sandbox.nodeSelector.kubernetes.io/hostname`, and explicit kernel-compatible
  `datapathWitness.gadget.hookMode`. Verify the Kind node's hostname label matches
  its name. Namespace scope is exactly `[sre]`; the daemon has required node-name
  affinity, and each DNS/TCP collection passes exact `--node` and `--namespace`
  selectors. No `--all-namespaces` capture or unrelated-node scheduling is used.
- Supply qualified immutable controller/router/OpenClaw/Hermes, mesh, Headlamp,
  witness and seccomp-installer images, plus images for enabled monitoring and
  metrics components. The controller tag must match its separately approved
  cold-install digest. Digest syntax is not provenance/runtime qualification;
  AL3, OpenClaw 2026.9.8/Node24 and capture compatibility remain build gates.
- Select `sre.provider: azure-openai` with
  `inferenceRouter.azure.openai.endpoint`, or `sre.provider: foundry` with
  `foundry.endpoint`, and an explicit `sre.model`. This bounded profile reuses
  those existing HTTPS key-auth routes through
  `inferenceRouter.azure.openai.credentials.existingSecret`/`key`; inline keys,
  no-auth local routes and a shadowing `kars-inference-providers` Secret are
  rejected. Other named provider routes remain supported outside this profile
  but require qualification before inclusion here. Supply real matching budget
  contracts and the existing budget TLS Secret/CA. Kind beta uses the unbounded,
  non-expiring first-install funding policy above; finite ceilings remain an
  explicit non-beta policy. Chart checks do not manufacture provider contracts.
- Supply Core Headlamp issuer, client ID/Secret reference and browser callback;
  supply Bridge OIDC issuer/client/Secret, redirect, role claim/map and shared
  principal Secret through its existing fields. Secrets must be available via
  the operator's established secret-management process, including automatic
  namespace-local projection when a namespace is created during installation;
  no post-install credential activation is supplied. Bridge validates the live
  Core owner/profile, current UID-bound SRE enrollment, and Headlamp issuer/URL.
  Cilium deployments must enable Bridge `networkPolicy.routerApiServerIngress`;
  Calico deployments leave that Cilium-specific policy off.

#### Optional Core-owned Headlamp OIDC projection

By default, `headlamp.oidc.clientSecret.name`/`key` and `caConfigMap` remain
references to operator-managed objects **in `headlamp`**; Core copies nothing.
For a cold installation, where Core creates the `headlamp` namespace, explicitly
opt in to Core-managed projection using source objects already available in the
**Core release namespace** (`kars-system` for Kind beta):

```yaml
headlamp:
  oidc:
    enabled: true
    issuerURL: https://issuer.example.invalid
    clientID: headlamp-client
    callbackURL: http://localhost:3903/oidc-callback
    clientSecret:
      sourceName: operator-headlamp-oidc
      name: headlamp-oidc
      key: client-secret
    # Both CA fields are optional; omit them for a publicly trusted issuer.
    caSourceConfigMap: operator-oidc-ca
    caConfigMap: headlamp-oidc-ca
```

The operator supplies an `Opaque` source Secret with nonempty `data[client-secret]`
through their established secret-management process, not through chart values.
An operator-provided `kars-system` namespace containing these inputs remains
operator-owned: Core does not adopt it or include it in the release manifest.
It must already have `pod-security.kubernetes.io/enforce`, `audit` and `warn`
labels set to `restricted`. Terminating namespaces and incomplete or conflicting
Helm ownership are rejected. A namespace already owned by this exact Core release
remains chart-managed. This does not permit precreating the Headlamp or SRE namespaces.
If CA projection is selected, the source ConfigMap must contain public PEM
certificates in `data["ca.crt"]`, never private keys. The chart copies **only** the
selected Secret key and `ca.crt`, with no source metadata or other data keys.
The same `clientSecret.key` selects the source key and names the destination key.
Secret and CA projection are independently opt-in; reference-only CA remains
supported alongside a projected Secret.

These are ordinary Helm resources in the **same Core release** as the namespace,
not hooks, Jobs or another release. Do not precreate or adopt the Headlamp
namespace. Exact-name `lookup` is confined to the Core release namespace for
sources and `headlamp` for destinations. The Helm caller needs permission to read
those exact objects and to create/manage destinations; unrelated existing targets,
terminating objects, source/destination identity collisions and missing or invalid
source data fail closed. Existing projected targets must retain this release's
normal Helm ownership labels/annotations. Re-run the existing Core Helm upgrade
with the source references to refresh copies after rotation; this is not a live
secret synchronizer. Disabling projection removes the managed copies on upgrade
under normal Helm lifecycle rules; arrange replacement before switching back to
reference-only operation. An offline `helm template` cannot resolve opted-in sources;
use a cluster-connected caller/server dry-run, protecting its output.

**Security: projection stores a copy of the OIDC client secret in Helm release
history, including older revisions after rotation. Protect release-history read
access as credential access**, as well as destination Secret access. Do not expose
rendered manifests, `helm get manifest`, debug/dry-run output or release storage in
logs or test artifacts. Keep actual credentials out of values and checked-in
manifests; only synthetic fixtures belong in tests. Projection preserves PKCE and
TLS verification; PEM envelope validation is not certificate/issuer qualification.

For an external HTTPS issuer signed by an operator CA, optionally set Bridge
`web.oidc.caConfigMap` to an **existing ConfigMap in the Bridge namespace**, with
public PEM certificate(s) in `data["ca.crt"]`. The operator's established
projection process must provide it before Bridge installs. The chart neither
creates this ConfigMap nor copies Core Headlamp's CA across namespaces. For example,
`web: {oidc: {caConfigMap: bridge-external-oidc-ca}}` references that existing
object; keep `idp.enabled=false` and configure the genuine issuer/client settings
through the existing `web.extraEnv` fields.

The chart mounts only `ca.crt`, read-only at `/etc/bridge-oidc-ca/ca.crt`, and owns
`NODE_EXTRA_CA_CERTS` pointing to it. This **adds trust process-wide in Node**, not
just for OIDC; use only reviewed public CA certificates, never private keys.
It preserves TLS verification, the public issuer and normal HTTPS transport;
there is no backchannel or verification bypass. With this option enabled, manual
`NODE_EXTRA_CA_CERTS`, `NODE_TLS_REJECT_UNAUTHORIZED`, `NODE_OPTIONS`,
`BRIDGE_OIDC_BACKCHANNEL_ISSUER` and `DEX_UPSTREAM_URL` overrides are rejected.
The kind-beta environment allowlist and HTTPS port-443 requirement are unchanged.

The kind-beta caller also requires the named ConfigMap to exist, not be
terminating, and contain PEM certificate envelopes only. This is a structural
check, **not X.509 chain/expiry or live OIDC qualification**; the operator must
verify those before installation. Generic offline rendering validates the
reference and external HTTPS settings but cannot establish ConfigMap existence;
the non-optional volume/key blocks Pod startup if either is missing. Node loads
extra CAs at process startup, so CA rotation requires a reviewed Web rollout;
changing the ConfigMap alone does not reload trust. Empty `caConfigMap` preserves
default system trust and bundled Dex behavior. This is chart-only wiring: no Web
image rebuild, baked-in private CA, or post-install first-use activation is needed.

During Core's ordinary-resource wait, the beta aggregator's `--process-health`
probe requires fresh successful publication, including an honest `unavailable`
report while the SRE source/baseline is absent. Original `--health` still means
successful capture/publication; it remains false without a valid baseline.
Neither probe makes missing data qualified. Publication UID/resourceVersion and
node-Pod identity fences are unchanged.

The existing bounded SRE post-install Job (weight `-90`) creates/enrolls the
source and waits for verified authority/funding. Only afterward do the exact
observer Role (`-80`) and RoleBinding (`-79`) enter controller-owned `kars-sre`.
Helm creates no runtime namespace and grants only GET on its compiled baseline.
Failure stops later hooks; retained Jobs, credentials, intent and any completed
RBAC require inspection, not adoption, automatic retry or replenishment. A later
ordinary-resource transition uses the existing original-intent/UID guard and
changes only `schemaHook.enabled`; it is **not needed to activate operations**.
Generic/standalone witness readiness and existing ownership guards are unchanged.

With genuine private values and an explicit qualified context prepared, the
maintained caller is still exactly these two Helm operations. This is a source
integration recipe, **not final accepted vanilla-machine commands**:

```bash
set -euo pipefail
umask 077
export KUBECONFIG=/absolute/path/to/operator.kubeconfig
CONTEXT=my-fresh-kind-cluster
CORE_VALUES=/absolute/path/to/core-values.yaml
BRIDGE_VALUES=/absolute/path/to/bridge-values.yaml

helm --kubeconfig "$KUBECONFIG" --kube-context "$CONTEXT" \
  install kars deploy/helm/kars \
  --namespace kars-system --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values deploy/helm/kars/values-kind-beta.yaml --values "$CORE_VALUES" \
  --wait --wait-for-jobs --timeout 20m

helm --kubeconfig "$KUBECONFIG" --kube-context "$CONTEXT" \
  install kars-bridge bridge/deploy/helm/kars-bridge \
  --namespace kars-system \
  --values bridge/deploy/helm/kars-bridge/values-kind-beta.yaml --values "$BRIDGE_VALUES" \
  --wait --wait-for-jobs --timeout 15m
```

Do not prepend the compatibility Namespace CREATE or schema-preparation commands
below. Do not add automatic failure-uninstall/rollback flags to the first-install
hook. A failed install requires inspection of that exact attempt's Helm state,
retained resources and helper termination, not blind replacement or CRD deletion.
In particular, the historical Namespace conflict below must be checked against
this exact installer/chart combination; its raw-manifest workaround is not a
Helm-only beta solution.

After both releases install, complete the functional acceptance below. Ready Pods
are not agent delivery. The AKS gate additionally requires genuine Entra Agent ID
and the same customer journeys. Core schema establishment with Entra disabled
alone does not satisfy it; identity activation is a separate reviewed operation
on the **same Core release**, with `schemaHook.enabled` turned off. Do not enable
the first-install hook and Entra sidecar simultaneously or count an anonymous
AgentMesh installation as Entra qualification.

### CLI-assisted compatibility procedure — not the beta acceptance path

The following preserves the older Helm 4 procedure and historical recovery
context for existing installation tooling. It requires raw Namespace creation
and CLI schema staging, so it is **not** evidence of the Helm-only beta gate.
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

### Protected Core upgrades remain restricted

Completing private enrollment seals the controller's reviewed Pod template into
its retirement binding. The [reviewed private-root migration](../../docs/operations/upgrades.md)
supports a narrowly bounded immutable-image transition; it is not a general Core
Helm publication, trust-rotation or interrupted-publication recovery mechanism.
Keeping the same namespace and Deployment UIDs is insufficient. Even a rollout
restart changes a template annotation; a healthy Deployment after that restart
does not prove private qualification continuity.

Set the intended controller images, sandbox placement and isolation prerequisites
**before initial enrollment**. If enrollment has already started or completed,
do not change that template through Helm or `kars upgrade` and assume another
preview/apply will requalify it. The admitted-Pod recovery repair handles legitimate
AKS admission changes; it does not remove these separate upgrade restrictions.
Do not clear qualification metadata, replace the sealed binding, or delete grants
as a workaround. Full-Core publication must also preserve the exact budget TLS
identity/revision bound into qualification. A failed Helm status after cancellation
is not proof that its publisher has stopped writing; do not retry based only on
that status. Unsupported transitions remain blocked pending a reviewed lifecycle.
The lifecycle work is tracked in
[Azure/kars#567](https://github.com/Azure/kars/issues/567).

The CLI now refuses controller-changing upgrade, image-publication and restart
paths before their first mutation when private qualification or retirement
evidence exists. Missing permissions or malformed evidence do not mean an
unqualified installation. Read-only schema `--check`, genuine root-free
operations and original credential-recovery commands remain available. This
client-side preflight is not an atomic operator lock or the missing migration
protocol; direct Helm or Kubernetes writes must not bypass the restriction.

### Core-managed datapath witness

The maintained witness chart is included as Core's `datapathWitness` dependency;
Kind beta acceptance requires it enabled from the initial Core installation,
without a third Helm release or a later activation command. The standalone
`kars-datapath-witness` release remains a compatibility path for existing
operators, not the two-release beta bootstrap. See the
[witness installation and guarded removal procedure](../../deploy/ebpf-witness/README.md).
Enabling requires approved AL3 gadget/aggregator images, an explicit reviewed
kube context and node selection, and privileged kernel-capture approval. Set
`datapathWitness.scope: all` with `datapathWitness.sandboxes: []` in the reviewed
Core values. The collector independently rediscovers all KarsSandbox CRs in
`kars-system` each sample, bounds capture to their namespaces and rechecks the
snapshot identity afterward. New Team and Mission workers enter subsequent
samples without maintaining a static sandbox list. A missing report does not
prove that no legacy observer is installed; ownership collisions must be
resolved without adoption or duplicate installation.

Existing selected installations remain supported with `scope: selected` and up
to 50 exact configured sandbox names; absent scope retains that legacy behavior.
All scope requires an empty configured list (no implicit filter) and does not
impose a 50-row fleet cap; the 512 KiB report limit remains. Settings/report scope
and configuration digest must match. All-scope rows carry the sandbox CR UID,
not just a reusable namespace name.

Bridge reads only the intent and report ConfigMaps, not privileged collector or
fleet inventory APIs. Its console labels the reported sample, never full fleet
coverage. Rows with unavailable or invalid controller declarations retain actual
DNS/TCP observations, but show an unavailable comparison, not Learn or Strict
deny-all. `declaration_diagnostic` distinguishes `baseline_unavailable` from
`declaration_invalid`; these rows have null `egress_mode`, verdict `UNAVAILABLE`
and empty declaration/comparison arrays. Available rows omit that diagnostic.

Keep capture off shared GPU nodes unless the operator explicitly approves the
observation scope. Neither Helm success nor all scope, a fresh report, empty
traffic or ready pods establishes complete kernel coverage or enforcement.

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
