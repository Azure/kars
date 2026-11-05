<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Kars Helm chart

This chart installs the Kars CRDs, controller, RBAC, admission controls,
policies, and optional operational components into an existing Kubernetes
cluster. It does not provision the cluster, registry, inference backend, or cloud
identity. For no-Entra development, it can store an operator-supplied initial API
key using the [Helm inference bootstrap](../../../docs/how-to/helm-inference-bootstrap.md).
Read that guide's release-history and router workload-specification exposure
boundaries before supplying credentials. The generic profile also deploys the
Microsoft AGT AgentMesh relay and registry from Kars's Azure Linux 3 distroless
release images: `ghcr.io/azure/kars-agentmesh-relay:latest` and
`ghcr.io/azure/kars-agentmesh-registry:latest`. These run the unchanged upstream
modules from the shared AGT source pin; the maintained recipe is
[`sandbox-images/agentmesh/Dockerfile`](../../../sandbox-images/agentmesh/Dockerfile).
Before installation, verify the selected release images are published and
qualified, or supply independently qualified images from your own registry.
Operator-provided `agentMesh.*.image` references are preserved, including
`tag: latest@sha256:<digest>` for an independently qualified immutable image.
Saved values containing older repositories are not silently migrated; update
those explicit image values when upgrading. The chart preserves the fixed
service endpoints, `/health` probes and single-replica in-memory storage.
Upstream PoP is enabled by default; do not enable its unauthenticated-DID bypass.
Encrypted interoperability and invalid-auth refusals alone do not qualify
accepted Entra tokens or the complete Team journey.

## Support status

| Environment | Status |
|---|---|
| Local kind | Tested with `values-local-dev.yaml` |
| AKS | Primary tested deployment |
| EKS, GKE, and other Kubernetes | Chart-render tested with `values-generic.yaml`; runtime support depends on operator-provided integrations |

These statuses describe existing installation profiles, not qualification of the
new Helm-only first-install preview or the Core + Bridge beta customer journeys.

## Existing non-AKS cluster

### Default inference model

Set the existing provider's default model before installing and privately
activating the controller:

```yaml
controller:
  inferenceDefaults:
    provider: azure-openai
    model: gpt-5.4-mini
```

Both fields are optional together. Leaving them empty preserves existing
behavior. These typed values set the controller's existing Task/agent default
model and provider; they do not register an endpoint, credentials or funding.
Configure those independently through the existing operator inputs. Explicit
per-agent models remain independent. SRE inherits the Core defaults unless
`sre.provider` or `sre.model` explicitly overrides the corresponding field.
Enabling SRE requires a configured route; no provider or model is invented.
Existing releases with explicit SRE values retain their pinned route. In `kind-beta`,
the default provider must match its configured SRE provider route, but the
models may differ; arbitrary `controller.extraEnv` overrides remain forbidden.
A qualified private controller's pod template is sealed: these values are not
an in-place settings-migration or seal-bypass mechanism. Reconciled operator
settings retain their existing authority over bootstrap defaults.

With the inference broker enabled, conversational contracts for the default
provider also populate the controller's model catalogue. An explicit operator
catalogue takes precedence; model availability does not establish successful
inference. After SRE adoption, operators can append contracts for new models
under a new catalogue version without changing SRE's pinned policy or funding.
Original contracts must remain in place with their bounds unchanged (only the
existing validity/version renewal is allowed). Additions require unique routes,
finite bounds and unexpired validity; router admission still validates the full
contract and reserves against the requesting agent's existing budget.

### Headlamp user sign-in

Core owns the AL3 Headlamp deployment and bundled Kars plugin. Its service
account has no dashboard resource permissions. Token-free sign-in uses
Headlamp's native OIDC flow with PKCE and the individual user's ID token,
not a shared service-account token.

The Kubernetes API server must already trust the same HTTPS issuer and client
ID, and its username/group mappings must have appropriate Kubernetes RBAC.
Register the exact browser callback with the identity provider. These are
cluster/identity-provider prerequisites: enabling the chart setting does not
change API-server authentication, grant roles, install a host CA or configure
Bridge's Dex issuer.

For a read-only operator dashboard, adapt the group in
[`deploy/headlamp-reader-rbac.yaml`](../../headlamp-reader-rbac.yaml) to the
**API-server-mapped** OIDC group (including its configured prefix), then have a
cluster administrator apply that prerequisite. It grants explicit workload,
node, CRD, metrics and Kars-resource reads rather than inheriting local
aggregation or unrelated changes to built-in roles. It does not bind the
Headlamp ServiceAccount, grant Secret access, permit pod exec/service proxy, or
allow approving SRE actions. Monitoring queries and remediation decisions need
separately reviewed permissions.

Verify authorization with the actual signed-in identity, not just a successful
OIDC callback: open Nodes and the Kars SRE console and confirm live reads. Before
installing, an administrator can also use `kubectl auth can-i list nodes
--as=<mapped-user> --as-group=<mapped-group>` and the equivalent check for
`karssandboxes.kars.azure.com`. Keep negative checks for Secret reads, pod exec,
proxy access and action updates. For subresources, use `create pods
--subresource=exec` or `get services --subresource=proxy` with the same identity
flags; `services/proxy` instead checks a Service named `proxy`. These
impersonation checks complement, not replace, browser acceptance.

```yaml
headlamp:
  oidc:
    enabled: true
    issuerURL: https://identity.example.test/dex
    clientID: kars-headlamp
    callbackURL: https://headlamp.example.test/oidc-callback
    clientSecret:
      name: headlamp-oidc
      key: client-secret
    scopes: "profile,email,groups"
    caConfigMap: headlamp-oidc-ca
```

The referenced Secret must exist in `headlamp`; secret contents are not copied
into Helm values or rendered manifests. For a private issuer CA, provide the
optional ConfigMap in the same namespace with a PEM `ca.crt`. Otherwise omit
`caConfigMap` to use the image's system trust. Missing inputs fail Pod startup;
invalid CA contents fail Headlamp configuration rather than disabling TLS.
Rotate the client Secret or CA and restart the owned Deployment through the
normal reviewed operational path to reload them.

The issuer must use HTTPS, including on Kind. An HTTP callback is accepted only
on `localhost`, `127.0.0.1` or `::1` for local port-forward use; this does not
relax issuer TLS. Core allows the issuer's explicit TCP port in the existing
Headlamp NetworkPolicy. Discovery, token and JWKS endpoints must be reachable
through that policy. Keep OIDC disabled until trust, credentials, callbacks
and per-user RBAC are configured. Rendering these settings is not installed
SSO acceptance.

Bridge can register this client in its existing Dex instance and expose a
native HTTPS listener; see [sharing Dex with Headlamp](../../../bridge/deploy/helm/kars-bridge/README.md#sharing-dex-with-core-headlamp).
For local split-horizon DNS, `headlamp.hostAliases` accepts ordinary Kubernetes
hostname/IP mappings. This affects only the Headlamp Pod: the API server and
browser still need their own resolution and certificate trust for the same
issuer. It does not change host DNS, host trust, or API-server authentication.

### Adopted datapath witness upgrades

An already enabled, Core-managed witness can change `datapathWitness.scope`,
`sandboxes`, and the reviewed digest-pinned references in `aggregator.image` /
`gadget.image` (including repository and tag changes) after SRE adoption. Supply `sre.datapathWitnessBaseline`: the **entire original
effective `datapathWitness` object** from `helm get values kars -n kars-system
--all -o json` before the first witness transition. Keep that same baseline on
subsequent image refreshes; never replace it with the last upgrade's values.
A baseline is not permission to change arbitrary settings: only the named
fields are projected back, and the full comparison must reproduce the original
immutable adoption digest. SRE identity, original receipts, funding, approved
nodes, hook mode, DNS/TCP artifact digests, resources, authentication and all
unrelated configuration remain sealed. Both original and selected runtime
references must be valid images pinned to a full lowercase SHA-256 digest.

For fleet discovery, select `datapathWitness.scope=all`,
`datapathWitness.sandboxes=[]`, and qualified digest-pinned collector/daemon
images. Qualified images may move repositories; retain the original baseline
unchanged and preserve every other installed value.
Place the baseline under `sre`, not `datapathWitness` (the dependency rejects
unknown configuration fields). Baselines have no default and affect receipt
comparison only, not the deployed witness configuration.

Older sealed values omitted `scope`; that absence means legacy `selected`.
Preserve the absence in the baseline. An unchanged legacy selection must pass
`datapathWitness.scope=selected` explicitly because the current dependency
default is `all`; an all scope with a nonempty sandbox list is invalid. The
Kind beta profile requires fleet scope, so its next upgrade must select `all`
and an empty list rather than retain selected scope.

The maintained local Core Helm caller (`installLocalCoreChart`, used by
`kars dev --target local-k8s`) captures the original effective witness baseline
when adoption exists, retains a saved baseline on later upgrades, and passes it
to both schema preflight and the final Helm invocation. It preserves the saved
scope (or legacy selected scope) unless an overlay or assignment explicitly
selects another scope. Cold installs and protected migration publication retain
their separate existing paths. Direct Helm callers must supply the baseline and
explicit scope themselves; no skip flag or receipt rewrite is supported.

### Prometheus monitoring

Core installs the pinned `kube-prometheus-stack` dependency in `monitoring`;
it is part of the Core release, not a third Helm release or a separate CLI
installation. Run `helm dependency build deploy/helm/kars` before installing
from source or bundling the CLI when the locked dependency archives are absent.
The CLI build requires matching, exactly pinned `Chart.yaml`/`Chart.lock`
dependencies and their nonempty regular archives under `charts/`; it fails
before replacing the bundled chart if these inputs are incomplete. Rebundling
replaces the chart directory so removed templates and old dependencies do not
remain in the npm package. The chart includes the operator, admission certificate Job,
Prometheus, kube-state-metrics, node exporter, sandbox PodMonitor and AgentMesh
JSON exporter. Default runtime images use the maintained Kars AL3 packaging.
As with the other Core images, supply qualified images from your registry
when the selected release images are not published. Do not substitute upstream
runtime images to work around missing artifacts.

The stable Service is `monitoring/kps-kube-prometheus-stack-prometheus:9090`.
Headlamp queries it through the authenticated Kubernetes Service proxy.
`monitoring.querySubjects` can grant named Kubernetes users or groups GET access
to that exact proxy, without granting Secrets, writes, other Service proxies or
permissions to Headlamp's ServiceAccount:

```yaml
monitoring:
  querySubjects:
    - kind: Group
      name: kars:operators
```

These are the API server's mapped subjects, not merely Bridge role names.
No binding is created by default. An unauthorised user sees an explicit
telemetry error, not fabricated zeroes.

After completed SRE adoption, changing these readers requires
`monitoring.querySubjectsBaseline`: the exact original `monitoring.querySubjects`
array sealed by the adoption receipt (including `[]`). Set `querySubjects` to
the selected User/Group list; `[]` revokes the entire query binding. Both lists
require unique objects containing only `kind` and a nonempty `name` without
surrounding whitespace or newlines. ServiceAccounts are never accepted.

Refreshing Headlamp independently requires `headlamp.imageBaseline`, the exact
original `repository:tag` string (`:latest` or `:latest@sha256:<64 lowercase hex>`).
Select the qualified image through `headlamp.image.repository` and
`headlamp.image.tag`; the selected tag must be `latest@sha256:<64 lowercase hex>`.
The registry is operator-selected, not environment-specific. Supply each baseline
only for its corresponding transition and retain the original baseline across
subsequent upgrades, rather than replacing it with the last selected value.
These optional keys have no defaults, so existing sealed values remain valid.
Only those image fields and the reader list are restored for receipt comparison;
the selected values are emitted and the original immutable receipt is retained.
OIDC/authentication, ServiceAccounts, credentials, monitoring endpoints, SRE
configuration and funding are not relaxed by these transitions.

Enabling, disabling or refreshing Core-owned Grafana after SRE adoption requires
`monitoring.grafanaBaseline`: the entire original effective
`monitoring.grafana` object sealed by the adoption receipt (including chart
defaults, not only user-supplied overrides). Select the new switch and image
`registry`/`repository`/`tag` tuples through the existing Grafana values; both
selected tags must be immutable `latest@sha256:<64 lowercase hex>` references.
Retain the original baseline on later upgrades, including disable/re-enable
operations. Treat baseline snapshots as private if they contain credentials.
The baseline has no default and changes only receipt comparison. Helm adds
Grafana dependency defaults when enabling it; comparison normalizes both sides
against `files/monitoring/grafana-defaults.json`, verified by tests against the
locked dependency archive. Refresh that generated JSON when upgrading the
Grafana chart. Authentication, networking, resource limits, provisioning and
SRE authority remain sealed. Alongside the switch and image tuples, the only
permitted RBAC change is narrowing legacy `rbac.namespaced=false` to `true`;
enabled Grafana must use namespace-scoped access. The maintained default is
namespace-scoped, so older sealed releases also need their original baseline
when adopting this default. Additional namespaces or permission changes remain
rejected; Grafana must not receive cluster-wide access to tenant Secrets.
Use the owning Core release, not a competing Grafana installation, and keep the
ordinary-upgrade schema hook disabled. Kind beta still excludes Grafana during
cold installation; after completed adoption it permits this qualified optional
Grafana transition without enabling Alertmanager or duplicate Inspektor Gadget.

`monitoring.enabled=false` retains an external monitoring installation without
adopting its namespace. A foreign `monitoring` namespace causes installation to
fail rather than taking ownership. Core creates its namespace before admission
hooks and retains it on uninstall. `monitoring.prometheus.enabled=false` disables
Prometheus and the Kars scrape resources; disable `monitoring.enabled` to omit
the entire stack. The CLI does not overwrite Core-owned monitoring and directs
older Core installations to upgrade instead of creating an out-of-band stack.
The standalone, non-Core CLI path uses the same pinned chart, image defaults
and Kars templates. It installs the upstream resources with Helm so certificate
creation and webhook patch hooks retain their required order; it does not apply
hook Jobs as ordinary manifests.

Helm installs dependency CRDs before a first installation. Helm upgrades do not
upgrade or newly install CRDs automatically. Before enabling monitoring on an
existing release, review the pinned dependency's CRDs, apply only the approved
definitions with server-side apply, and wait for `Established` before upgrading
Core. Existing CRD ownership and field conflicts must be resolved explicitly;
do not use force-conflicts or adopt another operator's CRDs implicitly. The
upstream BusyBox/kubectl CRD-upgrade Job stays disabled.

The CLI's shared schema preflight handles these dependency definitions
separately from Kars CRDs: it creates missing definitions only after server
preview, reuses exact existing schemas without changing ownership, and checks
publication before applying their instances. Changed existing dependency
schemas require an explicit reviewed upgrade; Kars's schema ownership and
rollback guards are unchanged. Private-root qualification/retirement protections
can block a Core upgrade even when adding monitoring; the CLI does not remove
those protections or override the refusal.

`kars dev --target local-k8s` uses the fixed Core release name `kars` and Helm's
hook lifecycle as well. Older render/apply installations have template-owned
schemas and are not silently adopted; use a new development cluster or an
explicitly reviewed ownership migration for those installations.

Prometheus defaults to two-day retention with a 512 MB block-retention target
and a 1 GiB disk-backed `emptyDir`. This is bounded ephemeral telemetry, **not
durable audit evidence**; use an operator-provided
`monitoring.prometheus.prometheusSpec.storageSpec` PVC for required durability. Node exporter mounts
proc/sys read-only, without the full host root, host PID or host networking.

Prometheus and optional Grafana prefer nodes without the standard `control-plane`
or legacy `master` role labels, leaving control-plane headroom when workers are
available. This is a scheduling preference, not a requirement or an eviction rule:
single-node Kind remains supported, existing node selectors and taints still apply,
and already-running Pods do not move until replaced. Override their respective
`monitoring.prometheus.prometheusSpec.affinity` and `monitoring.grafana.affinity`
values for dedicated monitoring pools, or set them to `null` to remove the preference.
This does not increase resource limits or establish stability under load.

Grafana remains optional (`monitoring.grafana.enabled=false`). Enabling it
requires independently qualified Kars Grafana and sidecar images; successful
Prometheus installation does not qualify Grafana or its signed plugins.
Dashboard JSON lives in `files/monitoring`; Core renders its ConfigMaps directly,
without a second generated copy. The sidecar reads monitoring ConfigMaps, not
Secrets. No anonymous Grafana login, fixed admin password, runtime plugin
download, root chown helper or upstream test image is enabled by these defaults.

Sandbox filters and scraped-router counts use Prometheus target discovery, so
idle sandboxes remain selectable before their first inference request. A
successful scrape is not workload readiness. Error percentages retain their
meaning below one request per second; no traffic remains no data. Token and
behavior counters can reset on router restart, audit counts describe retained
router entries, and peer sums can include duplicates. Illustrative prices and
display thresholds are not governed budgets or invoices.

The Alertmanager datasource is disabled with the default Alertmanager-free
stack. To use an independently installed Alertmanager, explicitly enable
`monitoring.grafana.sidecar.datasources.alertmanager.enabled` and configure its
`url`. Grafana prunes entries removed from its provisioned datasource files;
manually created datasources are not removed.

SQLite write-ahead logging is enabled to avoid readers blocking background
writes and session rotation during dashboard refreshes. This does not switch
database engines or expand resource limits. Grafana storage remains ephemeral
unless `monitoring.grafana.persistence` is configured; provisioned dashboards
and datasources are recreated, but local sessions and manual edits do not
survive Pod replacement.

Grafana uses `GOGC=50` to collect its Go heap earlier, leaving memory for mapped
executable pages and other non-heap allocations within the existing 512 MiB
container limit. The upstream chart sets `GOMEMLIMIT` to the entire container
limit; that alone does not reserve this headroom. Earlier collection trades
some CPU for a smaller heap. Requalify sustained dashboard refreshes when
overriding `monitoring.grafana.env.GOGC` or its resource limits.

Kind beta gives kube-state-metrics a 256 MiB memory limit and 500m CPU limit
through `values-kind-beta.yaml`; production defaults remain unchanged. The
retained beta workload exhausted its 250m CPU allowance and approached the
128 MiB memory limit during startup, with probe-triggered restarts and substantial
memory page faults. This bounded profile adjustment keeps both existing health
probes, collectors, and the AL3 image unchanged. Apply it through the Core release,
not a competing Deployment patch, and verify sustained readiness and real metrics
queries after installation; rendering the profile alone does not prove recovery.

### Metrics API

Core defaults to installing `kars-metrics-server` when the cluster has no
registered Metrics API. Its immutable upstream metrics-server binary is packaged
on Azure Linux 3 distroless, not the upstream runtime base. The required image is
included in the CLI image acquisition, source builds, and release inventory.
Source installation requires that matching image to be available in the selected
registry; rendering the chart alone does not qualify installed utilization.

An existing external `v1beta1.metrics.k8s.io` registration is preserved rather
than replaced, including a platform-managed provider. Set
`metricsServer.enabled=false` to opt out explicitly. `kars push --only
metrics-server --apply` refuses disabled or externally owned installations;
a combined image push reports that component as build-only while updating the
other selected components. Reconcile the Core chart first if a legacy,
template-installed cluster has no metrics prerequisite.

The aggregated API uses a generated serving certificate and a verified CA
bundle. The kubelet connection verifies the cluster CA by default; configure
`metricsServer.kubelet.caBundle` if kubelets use a different CA. The local Kind
overlay explicitly permits self-signed kubelet certificates, but does not
disable serving TLS verification on the aggregated API. Insecure kubelet TLS
is rejected outside the explicit `KARS_DEV_PROFILE=true` profile.

For kubelet certificates with hostname SANs but no IP SANs, set
`metricsServer.kubelet.preferredAddressTypes: [Hostname]` and supply the kubelet
CA. If those node names are not resolvable in cluster DNS, configure
`metricsServer.hostAliases` with Kubernetes `ip`/`hostnames` entries for those
nodes. Keep mappings and trust bundles current when node addresses or kubelet
CAs change. Neither option disables certificate or hostname verification.

Serving certificates last one year and are retained across upgrades. Renew
before expiry through the normal reviewed Core upgrade, adding
`--set-string metricsServer.tls.rotationRevision=<new-unique-revision>`.
Reusing that revision retains the renewed certificate; changing it generates
a new certificate, updates the API CA, and rolls the metrics Pod. Allow for a
brief metrics interruption during this single-replica rotation. Do not delete
or adopt another owner's serving Secret to force a renewal.

Cluster-connected upgrade rendering is required to retain serving keys and
external ownership: `helm template --is-upgrade --dry-run=server`.
The CLI local installer distinguishes first install from upgrade and uses the
selected Kind context for these lookups. Image refreshes wait for both the
owned Deployment and its API registration; Fleet still treats absent or
partial node/pod samples as unknown, never zero.

### Safety evaluations

Core includes the KarsEval API and controller. `evals.runnerImage` configures
the existing AL3 conformance runner (`ghcr.io/azure/kars-conformance-runner:latest`);
the maintained release publishes both AMD64 and ARM64 images. Helm does not
launch adversarial tests automatically: an operator selects a sandbox and
corpus or explicitly schedules an evaluation.

The local Kind installer builds or acquires and verifies the runner alongside
the other required images; `values-local-dev.yaml` selects
`kars-conformance-runner:latest`. Non-Linux source builds use the Linux
multistage recipe rather than copying a macOS executable. `--no-build` still
requires the caller to have loaded all configured images.

The console leaves the image override blank by default, so a new eval inherits
the Core setting. An explicit per-eval image still overrides that setting.
Existing `controller.extraEnv` entries for `KARS_CONFORMANCE_RUNNER_IMAGE`
retain precedence without duplicate environment entries; migrate that entry
to `evals.runnerImage` when next reviewing the installation values.
Image acquisition and chart rendering are not proof of a completed safety
run. Failed inventory reads and zero-case or errored reports are not shown
as successful evaluations. The console offers all five shipped builtin
corpora and supports keyboard submission; form fields remain fixed while
a launch request is in flight.

### Preserving console-managed inference settings

Fresh installations use the chart's inference bootstrap values. After an
administrator changes providers or model defaults in the console, upgrades
render the credential authority's current configuration instead of restoring
stale bootstrap settings. Explicit removals remain removed, and credentials
remain Secret references; their contents are not copied into the release.
Existing bootstrap Secrets are retained because enrolled providers may still
reference them.

This requires cluster-connected rendering: use `helm upgrade` and
`helm template --is-upgrade --dry-run=server` (also `--validate` on Helm 3).
Client-only `helm template` cannot discover operator configuration and is not an
upgrade manifest for a console-configured installation. The installing identity
needs read access to the controller Deployment, credential grant, and settings
Secret. If their UID bindings or reconciliation revision are no longer current,
rendering fails rather than resetting provider settings. Resolve the credential
authority condition before retrying; do not force ownership to bypass it.

### Generic overlay

The generic overlay disables Azure Workload Identity metadata and Azure-only
chart settings, uses RuntimeDefault seccomp, and replaces the AKS-specific
sandbox pool selector with the standard Linux node label.

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml

helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml
```

`my-generic-values.yaml` must provide pullable images, an inference endpoint
and router-side authentication, and any environment-specific secret-store,
policy, monitoring, ingress, and signing integrations. A NetworkPolicy-capable
CNI is required.

The overlay is opt-in and does not change existing AKS defaults.

With `schemaHook.enabled=false`, the schema preparation step is required before
the first admission installation; an ordinary Helm install does not wait for
asynchronous CRD OpenAPI publication ahead of policy type checking. The opt-in
[first-install preview](#helm-only-first-install-preview) instead performs that
stage inside Core's Helm invocation and is the intended Helm-only beta path.
For CLI-assisted preparation, use the same chart, release, namespace, context and
values as the following Helm operation. The public helper installs only exact
owned CRDs and verifies Established, resource discovery, hashed OpenAPI v3
documents and resolvable declared types. It does not install policies or grant
writer authority. See [the schema lifecycle](../../../docs/how-to/helm-installation.md#required-schema-before-admission-stage)
for ownership, upgrade and already-failed-policy bounds.

To use an externally managed AgentMesh deployment instead, set:

```yaml
agentMesh:
  enabled: false
```

## Helm-owned Entra identity configuration

**Qualification status:** this identity path has local Helm-render coverage, not
installed lifecycle or token-exchange qualification. Rust schema/environment
parity, API-server defaulting, ownership, rollout, rollback, and genuine
per-agent Entra authentication remain release gates. The compatibility CRD
overlay is not freshly generated Rust schema; the support table above does not
qualify this new path.

### Customer prerequisites and credential modes

`entraSidecar.enabled` defaults to `false`. Enabling it requires Core in
`kars-system`, an established supported `KarsAuthConfig` CRD, and reviewed
`entraSidecar.authConfig` values. The cluster-scoped singleton is named `default`;
a second Core release cannot independently own it.

The chart does **not** create a tenant, blueprint application, typed blueprint
principal, managed identity, federated identity credential, or Graph consent.
These are explicit customer prerequisites, not a hidden requirement to run
`kars mesh setup-trust`. A blueprint application alone does not prove its typed
principal exists. See [Agent Identity](../../../docs/agent-identity.md).

Choose an explicit `authConfig.controller.credentialMode`; authentication
failure never causes a mode switch:

| Mode | Required external trust and connectivity | Sidecar projection |
|---|---|---|
| `ManagedIdentityImds` | `managedIdentityClientId`, that identity available on the nodes hosting the shared sidecar, reachable IMDS, and reviewed blueprint federation | `SignedAssertionFromManagedIdentity`; no sidecar Workload Identity annotations or label |
| `WorkloadIdentity` | Cluster OIDC/Workload Identity, blueprint federation matching the issuer, audience, and `system:serviceaccount:kars-system:entra-auth-sidecar`, plus a working webhook/token projection | Blueprint client/tenant annotations, Workload Identity Pod label, and `SignedAssertionFilePath` at `/var/run/secrets/azure/tokens/azure-identity-token` |

The controller's `azure.workloadIdentity` setting is independent. Neither mode
proves downstream grants, per-agent identity issuance, or verified AgentMesh
membership. Review tenant authority, DNS/egress, and the shared sidecar's cluster
access boundary separately.

### Explicit activation in the same Core release

Identity activation requires the supported CRD **before** rendering the enabled
sidecar. Helm cannot use a CRD created later in the same operation to resolve an
ordinary `KarsAuthConfig`. The first-install schema hook does not support
same-operation identity activation: establish Core with the sidecar disabled,
then disable `schemaHook` for the reviewed identity upgrade. The default
ToolPolicy is a post-install/post-upgrade hook, not an exception for ordinary
custom resources.

1. Establish Core with the sidecar disabled using the reviewed installation or
   schema-transition procedure for the target cluster. Cold installation and
   schema migration still require native qualification.
2. Verify the live CRD has `Established=True` and matches the supported full
   schema. Discovery alone, an empty lookup, stale schemas, missing or unready
   establishment status, `Terminating=True`, and a CRD deletion timestamp are
   refused. The chart does not silently repair an incompatible or retiring live
   CRD during identity activation.
3. Explicitly upgrade the **same Core release** with complete reviewed Core and
   identity values. After prerequisites hold and the intended context is selected:

   ```bash
   helm upgrade kars ./deploy/helm/kars --namespace kars-system \
     -f reviewed-core-values.yaml -f reviewed-identity-values.yaml \
     --wait --wait-for-jobs --timeout 10m
   ```

Do not substitute `upgrade --install` for the prerequisite bootstrap or silently
reuse old identity settings. If integrating with a chart that has a schema hook,
`schemaHook.enabled=true` with an enabled sidecar is explicitly refused.
Activation is an additional Core operation, not a third product release. It does
not create a Bridge owner or authorize dispatch/merge. The chart's AgentMesh
components remain controlled independently by `agentMesh.enabled`; identity
activation does not establish their readiness. The two-product Core/Bridge beta
remains unqualified.

### Canonical reviewed values

`entraSidecar.authConfig` is the canonical `KarsAuthConfig/default.spec`:

- `tenant.tenantId` and optional HTTPS `tenant.authorityHost` (default
  `https://login.microsoftonline.com/`).
- `agentId.blueprintClientId`, `agentId.blueprintObjectId`, and optional sponsor
  object IDs. Client and object IDs are not interchangeable.
- `controller.credentialMode`, with `managedIdentityClientId` required for IMDS.
- `downstreamApis`: each API needs an HTTPS `baseUrl` and nonempty unique `scopes`;
  `requestAppToken` defaults to `true` and preserves explicit `false`.
- Optional `foundryRbac` scopes/role-definition IDs for the controller's separate
  per-agent assignment flow. Recording them does not prove grant authority.
- `meshAuthBackend` (default `Anonymous`) and optional `meshAuthAudience`.
  `EntraAgentIdentity` requires a correspondingly configured and qualified
  relay/registry/runtime path; choosing it is not a readiness assertion.

Deprecated `entraSidecar.tenantId` and `blueprintClientId` must be blank or match
the canonical IDs ignoring case. Conflicting anchors, unknown nested fields,
malformed GUIDs, implicit credential modes, invalid downstream API names, and
case-insensitive duplicate API names are refused. HTTP URLs and malformed legacy
identity values are rejected; review compatibility before migrating saved values.

An explicit `AgentMesh` downstream API entry wins case-insensitively. Otherwise,
Entra mesh mode emits the reconciler's synthetic `https://agentmesh.invalid/`
base URL with the trimmed audience (default `api://agentmesh/.default`). That
sentinel is not a reachable relay or connectivity evidence.

Enabled offline `helm template` cannot inspect live schema/ownership and fails
closed even with `--api-versions`. Tests substitute lookup fixtures in disposable
charts; they do not prove native admission. A reviewed server-side dry run can
inspect the intended cluster, but does not prove installed behavior or tokens.

### Managed AgentMesh Entra verifier

When `agentMesh.enabled=true` and canonical
`entraSidecar.authConfig.meshAuthBackend=EntraAgentIdentity`, the chart requires
`entraSidecar.enabled=true` and the same established-schema, release-ownership
and reviewed-spec-hash prerequisites described above. Both registry and relay
receive **both** `AGENTMESH_ENTRA_AUDIENCE` and `AGENTMESH_ENTRA_TENANT_ID` in their
initial Pod templates, before startup. Changing these values changes both Pod
templates and requests a rollout; there is no later verifier-configuration patch.
Partial verifier configuration is not supported.

When requesting Entra through `kars up` (including `up --upgrade`), complete the
reviewed **same-Core-release identity activation before running `up`**. Do not
rely on `up` to provision identities or a raw `KarsAuthConfig`, repair tenant
trust, fall back to anonymous mode, or patch verifier environment after startup.
Preserve the reviewed Core Helm values. Installed checks must compare the Helm
release's verifier pair against both live registry/relay Pod templates and
readiness; these configuration checks still do not establish JWT acceptance.

Verifier settings come only from the canonical sidecar's **effective** environment:

- An explicit `downstreamApis.AgentMesh` entry wins ignoring name casing; duplicate
  API names ignoring case are rejected. Its `requestAppToken` defaults to `true`;
  explicit `false` is refused for managed Entra mesh, not overwritten.
- Without that entry, the trimmed `meshAuthAudience` supplies the scope. The shared
  fallback `api://agentmesh/.default` remains an OAuth scope, not a JWT audience,
  and is refused for managed Entra mesh because its resource app ID is ambiguous.
- Exactly one app-token scope is supported: `RESOURCE_APP_GUID/.default` or
  `api://RESOURCE_APP_GUID/.default`. The verifier's v2 JWT audience is the
  lowercase **resource application client-ID GUID**, without `api://` or
  `/.default`. It is not inferred from `agentId.blueprintClientId`; the blueprint
  authenticates the agent-identity exchange and need not be the resource app.
- The canonical tenant must be a GUID (projected lowercase to the verifier), and
  `tenant.authorityHost` must be `https://login.microsoftonline.com/` (default;
  optional final slash, case-insensitive host). The official verifier pins the
  public-cloud tenant v2 issuer. Sovereign/custom authorities, tenant paths,
  ports, delegated scopes, multiple scopes and ambiguous resource URIs are
  rejected rather than guessed.

For example, within the otherwise complete reviewed `entraSidecar.authConfig`:

```yaml
meshAuthBackend: EntraAgentIdentity
downstreamApis:
  AgentMesh:
    baseUrl: https://agentmesh.invalid/
    scopes: ["api://eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee/.default"]
    requestAppToken: true
```

Replace that illustrative GUID with the **actual resource application's client
ID**. The base URL here is the same sentinel used by the token-only fallback,
not evidence of a reachable mesh endpoint. On that actual resource application,
explicitly configure **`api.requestedAccessTokenVersion=2`** and the corresponding
Application ID URI, application permissions/app roles, consent and **per-agent
grants**. Blueprint-wide consent alone does not establish each agent's resource
access. The chart does not create or inspect these tenant-side prerequisites;
using a v2 token endpoint alone does not guarantee a v2 access token.

Anonymous managed mesh receives no Entra verifier pair. `agentMesh.enabled=false`
leaves external mesh management unchanged; the operator remains responsible for
its complete verifier configuration and compatibility. Upstream PoP remains
at its enabled default: the chart never sets
`AGENTMESH_RELAY_ALLOW_UNAUTHED_DID`. The ignored upstream `REGISTRY_URL` and
`REQUIRE_REGISTRATION` environment entries are not emitted and must not be
interpreted as registry membership enforcement.

**Acceptance remains unverified:** actual Helm renders with offline lookup
fixtures check configuration and fail-closed lifecycle gates only. A successful
render, Pod rollout or health check does not prove an accepted JWT. Qualification
still requires genuine resource-issued v2 tokens accepted by **both** official
services, rejection of wrong-tenant/audience or invalid tokens, and **distinct
per-agent identity and proof-of-possession** with the intended grants. No live
token exchange, verified membership or end-to-end Team/Bridge journey is claimed.

### Initial SRE adoption and accounting policy

The first ordinary upgrade disables `schemaHook.enabled` while preserving the
sealed installation values and live SRE authority. An evaluation-runner image
and `evals.runnerImageBaseline` already sealed during installation are retained;
adding or changing them before adoption is not an initial ownership transition.
The adopted receipt continues to bind subsequent runner upgrades to that baseline.

Older sealed values without `inferenceBudget.calendarPolicy` mean `enforce`.
An explicit null is rejected if Helm preserves it, or falls back to `enforce`
if Helm removes it during values coalescing; null never selects `record-only`.
After adoption, operators may select `enforce` or `record-only` without replacing
the original SRE intent or receipt. The catalog update still requires the native
`karsbudgetaccounts/fund` permission. Record-only retains actual usage and pending
liabilities; it does not reset funding, identities, or unresolved requests.

### Runtime image refresh after SRE adoption

An adopted release can refresh the OpenClaw and Hermes image digests without
replacing SRE authority or its immutable adoption receipt. Set
`sre.runtimeImageBaseline.openclaw` and `sre.runtimeImageBaseline.hermes` to the
full original image references from the values sealed at adoption; retain this
baseline on subsequent upgrades. Select the qualified replacements through the
existing `sandbox.image.tag` and `runtimes.hermes.image` values.

Both original and selected references must use `:latest@sha256:<64 hex digits>`.
Ordinary refreshes retain the same repositories. After a reviewed migration,
Core publication may retain different repositories only when **both** selected
references exactly match unique literal `SANDBOX_IMAGE` and
`HERMES_RUNTIME_IMAGE` declarations on the same live, original-UID controller.
Missing, duplicate, indirect or mismatched declarations fail closed; this cannot
select a new repository or image that is not already installed. The chart
substitutes only the two full original image references and removes the baseline
metadata before checking the **entire** sealed values digest. An invented
baseline cannot authorize other configuration changes.
Initial adoption, router changes, policies, runtime settings, credentials,
identities and funding retain their existing checks. The existing pull-coverage
and Kind dispatcher transitions remain compatible; the original receipt is not
rewritten.

Review a server dry-run and qualify the selected images before upgrading. The
controller reconciles the new defaults into agent Deployments: replacing a Pod
with an `emptyDir` sandbox loses its temporary files. Preserve necessary data
first or explicitly treat those test-agent files as disposable. Verify actual
runtime image IDs, durable records and budget ledgers after rollout; Helm success
alone is neither data-continuity proof nor beta acceptance.

### Kind provider connections after SRE adoption

Cold installation still rejects a populated `kars-inference-providers` Secret.
An adopted upgrade can retain connections added through Bridge only when the
live, Ready, generation-current workspace credential grant enrolls that exact
Opaque Secret UID for `providers`, with its grant annotation intact. Deleted,
replaced, disabled or unreconciled authority fails closed. Only the maintained
named-provider keys and legacy Copilot token are allowed; arbitrary router
environment overrides are rejected.

A named alias for the sealed SRE provider must use the identical bootstrap
endpoint and credential from the existing configured Secret reference. Other
provider connections do not replace that route. Credentials are compared in
memory, never rendered into manifests, changed or removed by this check. The
full SRE adoption and private-continuity checks still apply.

### Kind mission dispatcher after SRE adoption

An adopted `kind-beta` release can activate the separate mission dispatcher from
its sealed disabled defaults with `missionDispatcher.enabled=true`, explicit
`authMode=development`, a nonempty image repository and immutable SHA-256 digest.
Relay and registry overrides must remain empty, selecting Core's managed mesh.
The original SRE adoption receipt is retained: all other values, funded source,
policies, live identities and finite funding must still match. This exception
cannot change dispatcher settings already sealed into that receipt and cannot
be combined with the initial SRE ownership transition. Review a server dry-run
before upgrading; activation is not proof of model execution or Team acceptance.
Entra activation remains a separate reviewed identity transition.

Mission-enabled task deployments use `Recreate` to avoid overlapping replicas
sharing the runtime identity. For an existing owned `RollingUpdate` deployment,
the controller first removes API-defaulted `rollingUpdate` settings with a
UID/resource-version-fenced strategy-only merge patch, then rechecks task and
source authority before applying the mission template. Server-side apply alone
can retain these settings and fail with HTTP 422. A conflict is not permission
to adopt a replacement; operators should not remove the isolation strategy or
manually manufacture mission bindings to bypass a failed transition.

### Entra mission dispatcher

The existing dispatcher is opt-in: set `missionDispatcher.enabled=true` and
`missionDispatcher.authMode=entra` only with the reviewed identity activation
above. Its prerequisites are checked before rendering:

- `entraSidecar.enabled=true` and `azure.entraAuth.enabled=true`.
- Canonical `entraSidecar.authConfig.meshAuthBackend=EntraAgentIdentity`.
- Lowercase GUIDs for canonical `tenant.tenantId`, `agentId.blueprintClientId`
  and `controller.managedIdentityClientId`.
- Nonempty `agentId.sponsorUserObjectIds`, all lowercase GUIDs.

The controller's identity provisioner needs `managedIdentityClientId` even when
**the sidecar** uses `WorkloadIdentity`; their credential paths are independent.
Provisioning uses the existing MI-to-blueprint Graph exchange. On AKS, verify
that the configured MI is assigned to the node-pool VMSS, controller IMDS egress
is permitted, blueprint federation/consent is configured, and the
BlueprintPrincipal exists. Helm cannot establish these external prerequisites.

The chart projects the tenant and blueprint from the same canonical AuthConfig;
it does not normalize IDs or accept a second set of dispatcher identity values.
The controller generates and pins the root and distinct Agent Identity binding.
Dispatcher RBAC adds GET of only `kars-mission-dispatcher-entra` beside its own
named root, never Secret list/write or runtime-root access. No public endpoint
or additional release is introduced. Missing, replaced or ambiguous identity
custody keeps dispatch unavailable rather than selecting anonymous transport.

Explicit `authMode=development` remains available for no-Entra qualification and
does not receive the Entra binding grant. Local render and unit tests do not
prove Graph provisioning, token refresh, verified peers or useful delivery;
those remain required installed acceptance checks.

### Ownership, rollout, retention, and recovery

Helm owns the singleton and an immutable, content-addressed environment ConfigMap
`kars-auth-env-<48 hex characters>`, generated from the same canonical spec. The
sidecar's required `envFrom` references that exact version. Environment changes
change the name and Pod-template hash, requesting a rollout; sponsor-only changes
do not. The controller's legacy mutable `kars-auth-sidecar-env` projection remains
separate and is not consumed by this Helm-managed sidecar.

An existing singleton must have this release's exact Helm ownership and a
`kars.azure.com/helm-auth-spec-sha256` annotation matching its live spec. Foreign
ownership, missing hashes, and detected out-of-band edits are refused. A singleton
with a deletion timestamp is refused even when ownership and hash match; complete
explicit retirement/recovery first. Do not add ownership/hash annotations to
bypass review of an existing CR. Status and server metadata are excluded from the
hash, but deletion state is checked separately. Render-time lookup is observational,
not atomic admission or UID/resource-version fencing; concurrent changes and
API-server defaulting/pruning still require installed testing.

The singleton and versioned ConfigMaps use `helm.sh/resource-policy: keep`.
Disabling the sidecar removes its Deployment/Service/ServiceAccount but retains
identity configuration; it does not stop controller identity processing or revoke
agent identities and tenant grants. Uninstall is not revocation either. Retained
objects may prevent adoption by another release.

Keep old ConfigMaps while draining Pods or retained release revisions reference
them. Explicit decommissioning must account for active agents, controller
behavior, federations/grants, and retained resources. Rollback compatibility and
failed-upgrade recovery require native qualification. A Ready Deployment,
`ConfigReady`, successful render, or Helm exit zero does not establish Agent ID
tokens, scoped authorization, verified mesh identity, or the Bridge workflow.

## Helm-only first-install preview

The opt-in `schemaHook` moves first-install CRD publication into Core's own Helm
invocation, without `kars schemas prepare` or a third product release. See the
[schema bootstrap guide](../../../cli/schema-hook/README.md) for the exact
Helm/Kubernetes versions, image build and digest, API-only networking, installation
command and retained-helper lifecycle. It remains disabled by default until the
image and native installation are qualified. It does not yet qualify Bridge,
Kind/AKS journeys or Helm-only upgrades. The CLI-assisted procedures in this
guide remain available for compatibility, not as the accepted beta install path.

## Validate

```bash
helm lint deploy/helm/kars
helm template kars deploy/helm/kars \
  --namespace kars-system \
  --values deploy/helm/kars/values-generic.yaml >/tmp/kars.yaml
```

CRDs remain in the chart's tracked templates, not Helm's install-only `crds/`
directory. Pre-created CRDs carry the exact intended Helm ownership, and the
following Helm operation records/manages them normally. Later upgrades use the
same schema preflight; they do not delete CRDs or customer resources.
Every CRD is retained with `helm.sh/resource-policy: keep`. Automatic rollback
also requires a compatible, retained previous successful release: pass the
following operation's `--atomic`/`--rollback-on-failure` option to `schemas prepare`.
An ownership match alone never permits removing fields or changing retained
schema validation. Older releases without complete retention require a reviewed
retention transition before atomic upgrades.

For an existing AKS cluster, run `kars config adopt-aks` after Helm installation
to write the local deployment context used by `kars upgrade`, `kars push`, and
mesh lifecycle commands. Kubernetes-only commands already use the selected
kube context directly.

Start from `values-existing-aks.yaml`; it mirrors the Helm values written by
`kars up` after provisioning and marks every customer-specific value with
`REPLACE_ME`. It uses the fixed public GHCR repositories. When a private mirror
is required, replace those repositories, import every image into the selected
ACR, and grant the AKS kubelet identity `AcrPull`. Stamp `karsRelease` with the
exact common release tag before installation.
