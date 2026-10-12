<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Optional datapath witness: operator-owned Helm switch

**Off by default. No installation is required for core Kars or Bridge.**
This add-on takes bounded kernel DNS/TCP samples through upstream
[Inspektor Gadget v0.53.2](https://github.com/inspektor-gadget/inspektor-gadget/releases/tag/v0.53.2)
and compares explicitly selected sandboxes with the controller's compiled
egress baselines. It observes; it does not enforce policy, sign attestations,
or establish complete kernel coverage.

Core includes the maintained witness chart as its local `datapathWitness`
dependency: use `datapathWitness.enabled=true` / `false`, without a third release.
The standalone `kars-datapath-witness` release remains supported for existing
operators. **A real cluster operator runs Helm.**
Bridge only reads two fixed ConfigMaps and offers admin-only copy controls. A
copied command has not run, created a release, or enabled observation. There is
no privileged BFF actuator or web-request shell execution.

## Prerequisites (before requesting on)

- Reviewed public Kars source containing this chart and runtime. No private
  repository, SDK, storage service, PVC, host-installed gadget client, or
  `install.sh --continuous` is required. That old console command was not part
  of the public source; it is not a supported installation path.
- Helm 3.18+ or Helm 4, an **explicit reviewed kube context**, existing
  `kars-system`, and existing `KarsSandbox` resources and `kars-<name>` namespaces.
  The fixed release name is `kars` (Core) or `kars-datapath-witness` (standalone),
  stored in `kars-system`.
  Do not use `--create-namespace`, `--take-ownership`, `--force`, `--force-replace`,
  client-only dry runs as preflight, or another release name.
- An operator with Helm lifecycle authority for this release's resources,
  including namespaced RBAC and the IG read-only ClusterRole/Binding. Server-side
  preflight needs GETs on managed identities plus cluster-wide **read-only**
  DaemonSet/Deployment lists to detect known observers. A denied read fails
  preflight; it is not treated as absence. This is the operator's authority,
  **not an additional Bridge permission**.
  The native capture client uses a read-only kubeconfig referencing its existing
  projected service-account token file and cluster CA; no token is copied into
  a ConfigMap or accepted without TLS verification. The observer has GET-only
  access to standard apps/batch workload owners for pod attribution, not workload
  writes or Secret access.
- Linux amd64/arm64 nodes with readable, nonempty `/sys/kernel/btf/vmlinux`
  (BTF v1), compatible eBPF/fanotify support, and a supported containerd setup.
  The default `gadget.hookMode=fanotify+ebpf` requires
  `CONFIG_FANOTIFY_ACCESS_PERMISSIONS=y`; additional capabilities cannot enable
  a compiled-out kernel feature. On a reviewed kernel without permission
  events (including some Docker Desktop Kind hosts), explicitly select
  `datapathWitness.gadget.hookMode=podinformer` in Core, or
  `gadget.hookMode=podinformer` for the standalone chart. This reuses upstream
  Kubernetes container discovery without installing host hooks or changing
  the kernel. DNS/TCP capture still uses the same signed eBPF gadgets.
  Container discovery is asynchronous and may miss startup/short-lived traffic:
  coverage remains **partial**, never complete. Automatic fallback remains
  disabled. The selected mode is bound into the runtime configuration digest;
  changing it rolls the observer and invalidates old report/config matching.
  Every scheduled IG pod checks the mounted host BTF header in an unprivileged
  init container. Node readiness/list access is not a BTF test. BTF presence
  alone does not prove that either gadget can load or cover all traffic.
- Admission approval for **elevated IG** in the dedicated `kars-witness-gadget`
  namespace (PSS enforce `privileged`, audit/warn `restricted`). The upstream
  capability-based daemon runs as root with SYS_ADMIN, SYS_PTRACE, SYSLOG,
  SYS_RESOURCE, IPC_LOCK, NET_RAW, NET_ADMIN, unconfined AppArmor, host runtime
  sockets, proc, debugfs and bpffs mounts. It is a privileged observer even
  though it does not set `securityContext.privileged: true`. Its host access is
  powerful; "observational" does **not** mean unprivileged or read-only host
  access. In particular, host proc symlinks/runtime sockets remain sensitive.
  No changes to core PSS, CNI, model resources, node pools, VM/VMSS or AKS are
  made by this chart. It never automatically opts in GPU/H100 nodes.
- A **built and published AL3 witness runtime image by digest**, pullable by all selected
  node architectures. There is no implied prepublished Kars aggregator image,
  `:dev` fallback, or automatic ACR build/push. Private pull secrets, if needed,
  must already exist in the dedicated namespace; provision that namespace with
  this chart's exact ownership metadata only after operator review, not by
  adopting an unrelated namespace. Otherwise use a registry accessible through
  the cluster's existing registry integration or a public operator image.
  Set both `aggregator.image` and `gadget.image` to the qualified runtime digest.
  Both are required when enabled; there is no automatic Debian-daemon fallback.
  The chart neither reads nor creates Secrets for registry credentials.

## Build / image and package provenance

Run from a reviewed public source revision in an environment with a functioning
container builder and network access. **These commands are operator actions;
they are not executed by Bridge.** The maintained recipe uses separate
digest-pinned Azure Linux 3 Python 3.12 build and distroless runtime bases,
matching Kars's AgentMesh packaging. The Go 1.27.2 SDK is copied from the official
toolchain image, but compilation and upstream CLI/OCI/API-helper tests execute in the AL3
builder. No Debian runtime files, shell or package manager enter the final
image. The builder prepares a runtime tree with searchable directories,
read-only source/provenance and both static executables; the final stage
copies that tree without a runtime shell or permission-changing command.
Record the source revision and publish to your own authorized registry:

```sh
export WITNESS_IMAGE='YOUR_REGISTRY/kars-datapath-witness-aggregator:latest'
docker buildx build --platform linux/amd64,linux/arm64 \
  --build-arg SOURCE_REVISION="$(git rev-parse HEAD)" \
  --file deploy/ebpf-witness/aggregator/Dockerfile \
  --tag "$WITNESS_IMAGE" --provenance=mode=max --sbom=true \
  --metadata-file witness-image-metadata.json --push .
```

Use the published **index digest** from the build output/metadata in your values
file, not the mutable tag or a single-architecture digest for a mixed cluster.
Retain the metadata, SBOM and source revision; apply your registry's image
signing/review policy before installation. A Dockerfile/chart reference is not
evidence that this image was built, pushed, or qualified.

If refreshing bases, override `PYTHON_BUILD_BASE` and `PYTHON_BASE` with reviewed
digest-pinned Azure Linux `base/python` and `distroless/python` indexes,
respectively; mutable tags and other distributions are refused before source
extraction and compilation. Both base references are retained in `/opt/witness/build.json`.
The same runtime contains the upstream daemon at `/usr/bin/gadgettracermanager`,
also accessible through AL3's `/bin` symlink. Packaging preserves this usrmerge layout.
It is built without the client's `withoutebpf` tag and retains upstream embedded
BPF objects from the checksum-verified source archive. No host-hook installers
or global cleanup executable are added. The chart explicitly selects this
daemon entrypoint; the image default remains the unprivileged aggregator.
The daemon's elevated host access and the separately pinned OCI gadget artifacts
still require operator review and actual kernel-capture qualification.

The Dockerfile builds the real upstream `kubectl-gadget` v0.53.2 source at
commit `7a314380ff61534ea70df9fb086505f958b2dff9`, rather than embedding its
vulnerable release binary. It verifies the source archive before extraction,
uses reviewed `client.go.mod`/`client.go.sum` locks with read-only module
resolution, and retains upstream `withoutebpf` client and OCI verification tests.
The security rebuild identifies itself as `v0.53.2-kars-security`.
`/opt/witness/build.json` records the source archive/commit, architecture, Go
version and binary/build-info/module-lock/license hashes; the actual Go build
information for both binaries and module locks are retained under `/opt/witness/provenance`.
This is source-build provenance, not the checksum of the superseded stock
binary archive. No kubectl/Python packages are downloaded at runtime.
Upstream image indexes and gadget artifacts are digest-pinned in `values.yaml`;
IG verifies the two OCI gadgets with the upstream public key and restricts the
server to those two artifacts. The artifacts were resolved from the official
v0.53.2 release, not inferred from aliases:

| Artifact | SHA-256 |
| --- | --- |
| `gadget/trace_dns` | `751684c8bf45731ffb412e608d9fb71f9e896debe1d138d524f850cadf54a0f7` |
| `gadget/trace_tcp` | `b6a4f4563430effa4ddde01898e628f082f2db4b638b18922eb55e4bd4aaab89` |
| Client source archive (both architectures) | `934060feae4ecab32644704c4e272e06b7c0da5a4f67429ba04b0113751946e4` |

To package without publication:

```sh
helm lint deploy/helm/kars-datapath-witness --namespace kars-system
helm package deploy/helm/kars-datapath-witness --destination ./dist/charts
shasum -a 256 ./dist/charts/kars-datapath-witness-0.1.0.tgz
```

The standalone chart has no dependencies. Core vendors this local dependency;
after changing its maintained sources, regenerate the Core dependency with
`helm dependency update deploy/helm/kars --skip-refresh` and retain `Chart.lock`
and the generated chart archive together. Rendering performs no network downloads. The
daemon template is adapted from the official v0.53.2 chart, not the entire
general-purpose upstream RBAC bundle; see `THIRD_PARTY_NOTICES.md`. The reduced
integration grants no IG Secret, trace-CRD, seccomp-profile, or workload writes.
It fixes `fanotify+ebpf` without a pod-informer fallback, does not install
CRI-O/NRI host hooks, and does not run the upstream global `/cleanup` that could
remove shared hooks/pins. Bounded foreground RPCs end their probes; no headless
instances or persistent event buffer is created/recreated.

## Core switch: no third Helm release

For Core, nest the image, timing and sandbox values shown below under
`datapathWitness` in `WITNESS_VALUES`. Preserve all unrelated Core values and
review the entire upgrade against the intended cluster:

```sh
helm upgrade kars ./deploy/helm/kars --namespace kars-system --kube-context "${KARS_CONTEXT:?Set the reviewed kube context}" --reuse-values --values "${WITNESS_VALUES:?Set the reviewed Core witness values file}" --set datapathWitness.enabled=true --wait --timeout 10m
helm upgrade kars ./deploy/helm/kars --namespace kars-system --kube-context "${KARS_CONTEXT:?Set the reviewed kube context}" --reuse-values --set datapathWitness.enabled=false --wait --timeout 10m
```

Never uninstall Core to stop capture. Default off retains intent metadata;
it does not establish that a legacy observer has stopped. Core refuses to adopt
standalone resources automatically. When deliberately retaining an existing
standalone release, set Core's `datapathWitness.managed=false` and
`datapathWitness.enabled=false`. This renders no Core witness resources and
leaves external ownership intact. It cannot detach already Core-owned objects;
use the guarded off upgrade instead. Ownership migration needs explicit operator
review and is not automated.

The producer verifies its configured release against both the DaemonSet and
report object. Older standalone runtime configurations without `release_name`
retain their standalone owner. Bridge accepts Core or standalone reports only
when intent and report have the same verified owner; its copy controls select
the corresponding chart rather than proposing a second observer.

## Standalone enable: existing-owner compatibility

Create an operator-owned values file outside source control if it contains
environment-specific registry details:

```yaml
aggregator:
  image: YOUR_REGISTRY/kars-datapath-witness-aggregator@sha256:YOUR_PUBLISHED_INDEX_DIGEST
  windowSeconds: 15
  intervalSeconds: 30
gadget:
  image: YOUR_REGISTRY/kars-datapath-witness-aggregator@sha256:YOUR_PUBLISHED_INDEX_DIGEST
sandboxes:
  - demo
```

The schema rejects empty scope, either missing runtime image, mutable tags, and unknown values
when enabled. This does not prove registry reachability; image pull failures,
missing BTF, failed capture or missing/invalid baselines keep the aggregator
unready and `--wait` fails instead of reporting successful enablement.

```sh
export KARS_CONTEXT='YOUR_REVIEWED_CONTEXT'
export WITNESS_VALUES='/absolute/path/to/reviewed-witness-values.yaml'
helm upgrade --install kars-datapath-witness ./deploy/helm/kars-datapath-witness --namespace kars-system --kube-context "${KARS_CONTEXT:?Set the reviewed kube context}" --values "${WITNESS_VALUES:?Set the reviewed witness values file}" --set enabled=true --wait --timeout 10m
```

Repeat the same command to update scope/settings. It does not adopt a legacy
installation or create another release. IG identity and its pod template stay
stable on no-op upgrades; the single aggregator restarts to bind its report to
the new release revision. A Helm success means rollout readiness, **not complete
kernel capture qualification**. A timeout leaves a failed/pending release and
possibly running resources; inspect it or use the guarded off command, not a
success-shaped retry or an automatic install of another observer.

## Disable / remove safely

```sh
helm upgrade kars-datapath-witness ./deploy/helm/kars-datapath-witness --namespace kars-system --kube-context "${KARS_CONTEXT:?Set the reviewed kube context}" --reuse-values --set enabled=false --wait --timeout 10m
```

This runs ownership preflight even for identities and sandbox Role/Bindings
being removed. It removes this release's IG DaemonSet, aggregator, ServiceAccounts,
RBAC, config and witness report. It does not issue broad namespace/label deletion.
**Verify termination**, including lingering/terminating pods, using the same
explicit context:

```sh
kubectl --context "$KARS_CONTEXT" -n kars-witness-gadget get daemonset,deployment,pods -l app.kubernetes.io/instance=kars-datapath-witness
```

Wait for **zero** such workloads/pods before saying this release's observation
has stopped. Errors are errors, not empty success. Helm deletion is asynchronous
and cannot assert that another operator's observer has stopped.

Default-off rendering creates **one nonprivileged operator-intent ConfigMap**,
`kars-system/kars-datapath-witness-settings`, and Helm records its own release
history/Secrets. Off after a prior enable also retains the dedicated namespace
with its privileged-PSS labels (`helm.sh/resource-policy: keep`), protecting any
subsequently added resources. None of these retained objects performs capture.
The chart never owns or removes `kars-system`, core CRDs, core data, model/CNI
resources, or unrelated observer installations.

After a successful guarded disable and termination check, optionally run
`helm uninstall kars-datapath-witness --namespace kars-system --kube-context "$KARS_CONTEXT" --wait`.
That removes intent/release metadata, not the retained namespace. **Do not skip
the guarded disable**: Helm uninstall does not render preflight and cannot
fence a resource replaced under the same name. Do not use Helm rollback to an
enabled revision (rollback also skips this preflight); enable only by upgrade
with the reviewed chart. Operator administration must be serialized: no
client-side preflight can atomically prevent another operator replacing an
object between lookup and Helm's write/delete.

## Legacy/shared observer conflict

An absent `kars-system/kars-datapath-witness` ConfigMap is **not** "not installed".
Raw IG and legacy aggregators can keep running with no report. Enable preflight
refuses other IG DaemonSets identified by the upstream label/image, recognized
legacy aggregator deployments, or any managed identity with different/missing
Helm ownership. Existing cluster-wide IG installs are not silently reused,
renamed, relabeled, upgraded, or removed. Detection cannot identify arbitrarily
renamed third-party forks; the operator must also inventory nonstandard observers.

There is intentionally no automatic legacy migration. Preserve the existing
UIDs and configurations, establish who owns the installation and whether it is
shared, and plan removal/migration separately. Do not install a second DaemonSet
just to make Bridge display "on". This switch is not permission to mutate a live
cluster, including any H100 installation.

## Data, authority and honest interpretation

The unprivileged aggregator lists pods and GETs the one IG DaemonSet **only in
the dedicated observer namespace**. The pinned client uses an explicit
`--gadget-namespace` and exact ready node list; Kubernetes port-forward creation
is confined to that namespace (RBAC cannot constrain rotating pod names by
label). Treat membership of that namespace as an operator security boundary.
It GETs only configured KarsSandbox names in `kars-system` and each exact
`karssandbox-<name>-egress-allowlist` ConfigMap in `kars-<name>`. No global
ConfigMap/pod enumeration is granted to the aggregator.

The elevated IG identity separately requires read-only cluster node/pod/namespace
and service discovery plus `nodes/proxy:get` for upstream runtime enrichment.
Its ConfigMap informer can read only the dedicated namespace, with no writer
rights. Both foreground gadgets sample all namespaces on targeted Linux nodes;
only configured sandbox aggregates are retained. Raw events are bounded to
temporary files (8 MiB per stream) and discarded after computation. Published
fields contain hostnames, counts and node names, never credentials or raw logs.

The sole write grant is **GET/UPDATE with resourceNames on the precreated
`kars-system/kars-datapath-witness` ConfigMap**. No CREATE, PATCH, or unrestricted
writer is granted. Publication checks Helm ownership, pins the ConfigMap UID
for the process lifetime, and uses a resourceVersion-conditional PUT. Replaced,
deleted, conflicted or forbidden objects fail explicitly. Loss of write access
cannot refresh old evidence; Bridge eventually shows stale or read-unavailable.

Capture exit failures, stderr warnings/errors (including upstream dropped-event
warnings), invalid JSON/schema/attribution, output/deadline limits, readiness
gaps, changing node sets, and missing/invalid/changing baselines produce an
`unavailable` report and fail readiness. A clean but empty capture produces
`empty`, not compliant. Strict deny-all with no traffic stays `NO-TRAFFIC`;
Learn comes from the actual CRD mode, not a missing ConfigMap. Baseline wildcard
comparison covers DNS names, not approved runtime overlays or port policy.
TCP counts include failed **connect attempts**, excluding accepts/closes and
nonpublic destinations. DNS intent is not correlated with TCP success.

Bridge retains the legacy DTO fields, adding requested intent, diagnostic
state, freshness (180 seconds, 30-second future clock-skew tolerance), publisher/
revision validation and explicit partial coverage. `enabled=true` now means
only a fresh validated nonempty sample, never installed/enforcing. API 404,
pending publication, malformed data, stale data, legacy reports, explicit off,
and API/transport/permission failure are distinct. Installation remains
**unknown** because Bridge reads reports, not Helm/workload inventory.

The witness is sampled, not continuous: by default 15-second foreground DNS/TCP
windows separated by 30 seconds plus control-plane work. It does not cover all
UDP, encrypted/cached DNS, attribution gaps, unscheduled/unsupported nodes, or
all times. Ready DaemonSets, clean empty windows and no beyond-baseline DNS do
not prove complete kernel coverage. This data is not bound into Kars receipts.

## Verification

Local, no-cluster checks:

```sh
python3 -m pip install -r deploy/ebpf-witness/tests/requirements.txt
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s deploy/ebpf-witness/aggregator
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s deploy/ebpf-witness/tests -p test_chart.py
cd bridge/web && node --test tests/datapath-witness.test.mjs
```

The core Helm gate runs render/package/runtime and loopback fake-API ownership
tests. Bridge's existing gates run the registered BFF read-only/negative tests,
web contracts and type/build checks, plus the disposable Kind lifecycle and
image-build checks. Lifecycle fixtures and image build are **not kernel proof**.

Before production qualification an operator must run the real built image and
pinned IG on a disposable compatible cluster, generate known DNS/TCP allowed,
beyond-baseline, deny-all and empty-window traffic on **every** target node,
verify attribution/window boundaries, simulate stream/node/API failures, and
prove capture ends after disable. Repeat for the actual kernel/containerd/
architecture/admission configuration. No such live kernel qualification or
production enablement is asserted by the source, rendered manifests, mocked
tests, or Bridge UI.
