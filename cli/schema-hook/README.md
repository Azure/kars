<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Core Helm schema bootstrap

The opt-in `schemaHook` publishes Core's CRDs from the exact pending Helm release
before Helm installs its ordinary resources. This removes the separate
`kars schemas prepare` command **for an original first install**. It is part of
Core, not a third product release or an alternative agent runtime.

**Status:** source integration for the Helm-only beta. The installed helper image,
Helm lifecycle, admission/discovery and CNI enforcement must still be qualified
together. Rendering or unit tests do not establish a working Kind/AKS installation
or a Bridge customer journey. No published, qualified helper image is supplied by
this change. Existing CLI schema preparation remains available for existing
installations; it is not the Helm-only beta acceptance path.

## Supported first-install inputs

- Outer installer: Helm **4.1.3**, including its build-metadata suffix.
- Cluster API: Kubernetes **1.35**, with enforced Kubernetes NetworkPolicy.
- Original revision-one Core install into a dedicated namespace, with no existing
  release history. Do not use replacement, upgrade, rollback or automatic
  failure-uninstall flags with this hook.
- An operator-provided `kars-system` namespace must have restricted Pod Security
  enforce/audit/warn labels plus `app.kubernetes.io/name=kars` and
  `app.kubernetes.io/component=system`. The chart validates these labels without
  adopting the namespace. Sandbox ingress selects those Kars labels for router
  policy confirmation; missing labels leave policies unready even when routers
  are healthy. Set them only on the verified, dedicated Core namespace, retaining
  its UID, existing labels and operator ownership.
- Architecture: `amd64` or `arm64`, matching the reviewed helper image digest.
- Exact helper image repository, `latest` tag and `sha256` digest. Never use a
  digest from a different build or architecture.
- One to eight explicit IPv4 `/32` and TCP-port pairs for the cluster's API
  service/backend, according to its CNI's pre/post-DNAT enforcement. No DNS,
  internet or IMDS allowance is granted to the helper. IPv6 is not supported by
  this entrypoint.
- On Cilium, the chart also creates a helper-scoped `CiliumNetworkPolicy` when
  that API is advertised. Cilium excludes cluster nodes from ordinary CIDR
  selectors, so this policy permits the `kube-apiserver` identity on only the
  reviewed TCP ports. The `/32` NetworkPolicy remains in place for external API
  endpoints; no cluster-wide CIDR matching setting or broad host/cluster allowance
  is enabled. Offline Cilium renders must include
  `--api-versions cilium.io/v2/CiliumNetworkPolicy`.
- `policyPrototype.enabled=false` and `entraSidecar.enabled=false`. Identity
  activation is a separate reviewed upgrade of the same Core release after
  schema establishment; turn `schemaHook.enabled` back off for that upgrade.

An AKS cluster's compatibility with these versions and network requirements must
be established before installation; this is not a claim that every AKS cluster
is supported.

## Build and package from matching source

From the repository root:

```bash
npm --prefix cli run build
npm --prefix cli run check:schema-hook
helm lint deploy/helm/kars
helm package deploy/helm/kars
```

The CLI build generates `deploy/helm/kars/files/schema-hook.mjs` and its dependency
NOTICE before bundling deployment assets. Generate it with the committed CLI
lockfile: the bundler refuses an installed `yaml` version that differs from that
lockfile, including mismatched shared development dependencies. Restore dependencies
in a separate temporary build directory rather than modifying another checkout's
shared `node_modules`. `check:schema-hook` refuses stale or missing generated output
without rewriting it. The chart mounts the exact bundle
bytes from an immutable ConfigMap; the helper image supplies only Node and the
Helm/kubectl clients. Preserve `schema-hook.NOTICE` when distributing the chart.
The entrypoint resolves ConfigMap projection symlinks before checking whether it
is the main module. A successful Job alone is not staging evidence: retain its
JSON result and verify schema publication occurred before Helm continued.

Run the entrypoint and enrollment regressions together:

```bash
npm --prefix cli test -- src/schema-hook-process.test.ts src/schema-hook.test.ts src/schema-hook-executor.test.ts src/lib/core-helm-sre-hook.test.ts
```

Process tests cover both `schemas` and `sre` phases through compiled and bundled
ConfigMap projections, including credential refusal, no local-cluster fallback,
redacted failures and import safety. These checks do not prove successful
in-cluster enrollment; qualify the mounted bundle on the exact toolchain image
and the complete first-install lifecycle separately.

The image recipe is `cli/schema-hook/Dockerfile`, built with the repository root
as context. Its Dockerfile-specific ignore file includes only the recipe, its
ignore file and `build-tools.sh`. It supplies Node 22, Helm 4.3.0+kars.1 and
kubectl 1.35.9+kars.1. The **in-container Helm client only reads stored releases**;
the separately pinned outer installer
is Helm 4.1.3. Both clients are compiled from checksum-pinned upstream release
sources on a native amd64 or arm64 builder, using Go 1.27.2 and `golang.org/x/net`
0.60.0 to fix the HTTP/2 and Go standard-library advisories in the upstream
binaries. Their `kars.1` build metadata and dirty tree state identify this
dependency-only rebuild; the upstream CLI versions are not upgraded. The build
runs each client's version-package tests and preserves both upstream licenses.
The final image remains AL3 distroless, with no compiler or package installer.
Build/publish and record the immutable image digest before enabling the hook;
fresh scanning and installed lifecycle qualification are still required.
Source builds report unversioned main modules and Kubernetes local staging
replacements in Go build metadata. Preserve their pinned source identities
alongside scan results; a clean package scan does not establish advisory coverage
for those unversioned modules or the separately copied Node binary.

## First install

Put the reviewed settings in the same Core values file as the other customer
configuration:

- `schemaHook.enabled: true`
- `schemaHook.architecture`: the image's architecture
- `schemaHook.image.repository` and `.digest`: the reviewed published image
- `schemaHook.apiDestinations`: the exact API destination pairs

Then use a single Core Helm install, without prior CLI staging:

```bash
helm install kars deploy/helm/kars \
  --namespace kars-system --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values reviewed-core-values.yaml \
  --wait --wait-for-jobs --timeout 10m
```

This example is the non-Entra generic cluster path. AKS uses its reviewed AKS
values instead. Bridge remains a separate Helm release; this command does not
install Bridge or qualify agent execution, encrypted handback or deliverables.

## Integrated SRE first-install lifecycle

When `schemaHook.enabled=true` and `sre.enabled=true`, the same Core install now
has three ordered phases; no extra release or post-install activation command:

1. The existing schema-only pre-install Job establishes discovery/admission.
2. Ordinary Helm resources create the controller, budget catalog and retained,
   immutable `kars-sre-first-install` ConfigMap. SRE custom resources are encoded
   there, not in the cold manifest whose REST mappings Helm resolves before hooks.
3. A **different**, bounded post-install identity creates the two policies and
   SRE source, waits for the controller's genuine namespace claim, and creates
   canonical registration with the original intent UID/digest and funding policy
   resolved to the real source/workspace/cluster UIDs. Registration authority
   validates this intent before minting private credentials; the existing budget
   consumer validates it again before creating/binding the authoritative ledger.
   The Job waits for current authority and the matching active ledger. For native
   unbounded funding it additionally uses the existing account `fund` and exact
   catalog CAS authorization to publish the standard UID/digest-bound allowance,
   then waits for controller consumption. Runtime creation and dispatch remain
   blocked until that receipt exists. The Job neither calls a model nor proves
   an installed SRE customer journey.

Kind beta selects exactly `sre.firstInstallFunding: {unboundedTokens: true}`:
no artificial initial cap, funding expiry or renewal, with actual usage retained.
Non-beta installs can instead supply `sre.firstInstallFunding.tokens` (positive
safe integer) and `expiresAt` (future UTC `YYYY-MM-DDTHH:MM:SSZ`). Mixed policies
are rejected. The ordinary finite hook retains its read/create-only permissions;
only the explicit unbounded hook receives `fund` and exact-catalog patch rights.
Helm adoption preserves the exact still-authorized consumed cold receipt without
rewriting values or restoring a revoked catalog entry.
Both policies require an enabled governed broker, real provider
contracts/endpoint/auth references and broker TLS configuration.
`sre.provider`, `sre.model` and policy token ceilings remain operator-selected;
policy ceilings do not themselves authorize spending. Controller, Hermes, router
and helper inputs require approved immutable image identities. No existing SRE
catalog grant may compete with the first-install authorization. The immutable
intent binds all values by digest, the rendered source/policies, catalog, limits
and expiry; it excludes TLS private material from its plaintext payload.
The shared SRE renderer explicitly includes API-defaulted sandbox security
settings (including writable paths) and policy defaults, so live API defaulting
does not change the authorized spec. SRE explicitly requests an empty SELinux
context rather than inheriting the CRD's literal escaped-string default. The
controller still compares the full typed spec exactly, excluding only its own
budget binding; it does not normalize away live security or policy changes.
The helper compares controller environment literals using Kubernetes' empty-value
default: an omitted `EnvVar.value` is empty only when `valueFrom` is also absent.
Nonempty values, references and missing environment entries still fail the
rendered-intent check, and the full live controller spec stays pinned throughout
enrollment.

The post-install Job has a 600-second program bound, a 660-second Pod deadline,
zero retries and API-only network access. It can CREATE the three named-intent
resource kinds and registration, but cannot patch/delete registration, mint
credentials, alter a ledger, update namespaces or publish CRDs. As with Kubernetes
CRD creation, CREATE RBAC cannot restrict names; the packaged caller validates
its exact intended writes. Namespace-wide Helm Secret reads require the same
trusted dedicated namespace as the schema phase.

A conflict, ambiguous CREATE response, changed manifest/controller/catalog,
foreign occupant, missing/expired intent or interrupted attempt fails closed.
No read/adopt recovery, replay funding, grant change, reset or automatic cleanup
is performed. Preserve failed hook evidence and objects for explicit operator
review; do not rerun an interrupted install as a fresh attempt.

A later ordinary-resource ownership transition must disable `schemaHook` while
preserving every other original value, the original live authority/source UIDs,
unchanged unexpired funding and the existing source budget binding. The same
release then includes the inference and tool policies in its ordinary Helm
inventory. The funded source remains controller-owned and outside that inventory:
Helm 4 may send an UPDATE even when its rendered spec is unchanged, which the
runtime-binding admission policy correctly refuses. The transition verifies the
live source configuration and intended labels without writing to that source.
Before transitioning an older installation, verify that its latest deployed
release does not already inventory the source; removing an inventoried source
requires a separately reviewed retention migration, not this cold-install path.
After all authority checks, the transition records the original intent UID and
a values hash excluding only `controller.image` in the immutable Helm-owned
`kars-sre-helm-adoption` ConfigMap. It does not annotate or modify the funded
source: runtime metadata and configuration remain controller-only. Subsequent
ordinary Core upgrades validate this release-owned receipt and the same live
identities, rendered SRE source/policies, catalog, budget binding and unexpired
finite grant. This permits a qualified controller image refresh while keeping
all other values pinned; it does not permit arbitrary Core settings changes.
The first transition still permits only disabling `schemaHook`; perform it
separately before changing other settings.
Missing receipts require that original transition again; foreign receipts fail
closed. Re-enabling the first-install hook is refused.

The intent remains retained, registration stays authority-owned, and qualified
Core upgrade guards still apply. This bounded transition is not a general
provider/funding migration; changing those inputs needs separately reviewed
migration rather than relaxing the immutable grant.

This integrates the previously missing SRE caller. It does **not** supply the
mandatory Kind beta operations values/vanilla-machine bootstrap or qualify the
combined witness, OIDC, provider and two-release installed lifecycle.

## Authority and failure handling

The pre-install hook reads the exact latest pending Helm revision and pins its
stored manifest. It rechecks release history, manifests and controller safety
before each schema write and after admission/discovery verification. It does not
rerender, create customer custom resources, run another Helm install, force
ownership, reconstruct private authority or retry failed writes. CRDs remain
ordinary retained Helm-managed resources.

Kubernetes cannot restrict CRD `create` permission by resource name. The helper
therefore validates its writes against the pinned release manifest; its `get`
and `patch` permissions are restricted to the chart's CRD inventory. Helm history
also requires namespace-wide Secret `get/list`, so use a dedicated Core namespace
and treat the image and its digest as privileged installation inputs.

Each attempt has fresh helper names, a no-retry Job and a bounded execution
deadline. Job, RBAC, service account, program and both network policies are deliberately
retained on both success and failure. Helm hook resources are not automatically
removed by release uninstall. A failed watch is **not** evidence that the helper
has stopped: verify Pod termination before retiring that exact attempt's
credentials, policy and other hook resources. Do not delete CRDs or retained
customer data as cleanup, or blindly rerun a failed release as a new install.

Operator egress is separated from namespace-wide default deny so the helper
cannot inherit controller DNS, mesh, API-port or IMDS permissions. Customer-added
NetworkPolicies are additive too and must not grant the helper broader access.
