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
