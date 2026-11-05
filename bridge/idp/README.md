<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Kars Dex security rebuild

Both source-stage recipes explicitly install GNU `patch` before fetching and
applying the hash-verified upstream patches. This is a build-only dependency;
the shipping AL3 distroless runtime contains no package manager or patch utility.

**Current prepublication repair:** genuine Go 1.27.3 resolver output now selects
`golang.org/x/net` 0.62.0 in both root and nested API graphs, including its required
`x/crypto` 0.58.0 and `x/text` 0.43.0 dependency floors. The first executed resolver
rejected the old 0.57.0 crypto request; its failure is retained. The regenerated
locks were reviewed and promoted with unchanged upstream source baselines. Other
module changes follow those dependency requirements; metadata was not hand-edited.
All 47 packaging/source contracts passed, including rejection of stale nested API
network dependencies even when artifact checksums are internally consistent.
The updated graph passed native Linux ARM64 compilation, memory/SQLite refresh
deadlock regressions, session/authflow/SAML and external-probe race tests, and the
maintained upstream root/API/NTLM race-suite commands with compiler networking
disabled. The current ARM64 image
`sha256:d3c81488c6217e16adff93acb326bc5fd5b26439d6e324f1dd5584ca3a256b4f`
passed the maintained AL3 base inventory/trust, native linkage, memory/SQLite OIDC,
refresh rotation and SQLite restart/signing-continuity checks. Trivy 0.70.0, with
its database updated November 4 at 19:02 UTC and freshly downloaded at 23:00 UTC,
reported zero HIGH/CRITICAL vulnerabilities without ignores. This is not
all-severity clearance, AMD64 validation or fresh-install acceptance. The
installed results below apply only to the October 28 graph.

**October 28 status: repaired Linux ARM64 image installed on Kind; Bridge → Headlamp
automatic sign-in, directional sharing and confirmed logout accepted.** These
results do not establish timed expiry, installed SQLite persistence, AMD64 or
Azure/Entra acceptance. On October 28, 2026 the Go 1.27.3 build passed CGO
compilation, memory/SQLite refresh-deadlock
regressions and session/authflow/SAML race tests. The corrected probe and unchanged
upstream root/API/NTLM race-suite commands also passed; environment-dependent
skips do not establish external connector coverage.

The actual AL3 image
`sha256:b8362c2c945b3e8297ba25a7ce6805039d426409268fcf24b730272e09522c75`
passed memory and SQLite login with fresh refresh-token rotation, SQLite
restart/signing continuity, base inventory/trust and native linkage. Trivy
0.70.0 with a freshly downloaded database found zero HIGH/CRITICAL findings,
without ignores, suppression or waivers. All four reviewed distrusted root
fingerprints are absent from its 226-certificate bundle.

The previous image remains failed: SQLite refresh deadlocked while its rotation
callback read storage, and its scan found nine HIGH package findings across
OpenSSL, JOSE, `x/net` and Go x509. The earlier network-isolated discovery failure
and the probe request-order fixture failure also remain failed evidence.
Genuine Go 1.27.3 resolver output has now been reviewed and promoted: only JOSE
4.1.5 → 4.1.6 and `x/net` 0.59.0 → 0.60.0 changed in root-module selections;
API selections and upstream snapshots are unchanged. Generated metadata was
not hand-edited.

The local compiler host reuses a checksum-verified Go distribution in a cached
Rust builder; it does not prove execution of the official Bookworm builder.
`verify-source-toolchain.sh` still rejects requirements newer than the pinned
compiler before resolution or manifest rewriting. Do not lower upstream
requirements or enable automatic toolchain downloads. Shipping builds consume
reviewed locks read-only. The repaired final image must independently pass
fresh rotation in both memory and SQLite, restart/signing continuity, trust,
linkage, inventory and a fresh zero-HIGH/CRITICAL scan before installation.

The lock artifact records the resolver's compiler and architecture. Native
Linux AMD64 and ARM64 builds may consume the same locked module graph, but both
the recorded resolver and actual compiler must use the pinned Go version.
The build preserves resolver provenance in `locks/toolchain.txt` and records
the actual compiler and target in `build-info.txt`. The current generated
artifact records Linux ARM64; final-image runtime qualification remains separate.
For the previous v2.45.1 graph, ARM64 regeneration matched its AMD64 artifact
except for the compiler record and checksum. The successful historical hosted
attempt on September 16, 2026 compiled Dex with CGO,
completed license collection, passed the compatibility cases, completed the
upstream root/API race-suite commands with exit code zero, passed all nine
signing-key-continuity regression subcases under the race detector, and compiled
the probe. Environment-dependent suite skips do not establish external
connector runtime coverage.

That earlier result applies only to the old dependency graph. Its binary
digest `c4b5f8017cffb5e9853788a66cc1a1ce21104b9e4b4ba04901947d7510d5e4f9`
and publication payload expectations must **not** be reused for a rebuilt
NTLM-fixed image. Frozen operations artifacts remain unsubmitted and require
a new approved reference after fresh qualification.

The required `idp` job in public Bridge CI builds the checked-in inputs
and executes the actual final distroless memory/SQLite/OIDC, linkage, inventory
and scan gates. That CI execution is still required; the earlier build/test
success is **not** final-image qualification or permission to deploy. This
wiring does not enable the optional IdP or change chart/default/installation
behavior.

This upgrades the existing optional Dex IdP, not the authentication framework.
The source is **Dex v2.46.1** at
[`75eb19238e10fc4750e7efb05a4031b254b04029`](https://github.com/dexidp/dex/tree/75eb19238e10fc4750e7efb05a4031b254b04029).
The [stable release](https://github.com/dexidp/dex/releases/tag/v2.46.1) was
published **2026-10-21T14:33:21Z**. On October 28, GitHub's annotated tag
`886e3245accf6f914f3fb58cee3c166a26ccd012` resolved to that commit; the bounded
codeload download below supplied the measured archive checksum. The intended
binary version is **v2.46.1-kars.1**. Labels/notices describe the prospective
build, not the still-installed v2.45.1 image.

The release's official-image CA fix is not automatically inherited by this
separate AL3 recipe. Upstream's [backport](https://github.com/dexidp/dex/pull/5095)
updates both Go to 1.27.3 (rejecting the compromised intermediates) and its
Alpine CA package (removing affected roots). This recipe retains AL3 and pins
Go 1.27.3. The updated AL3 payload has OpenSSL 3.3.7-7.azl3 and 226 TLS
certificates; all four published affected root fingerprints, including the
cross-signing roots, are absent. The maintained final-image gate verifies those
fingerprints again against the unchanged AL3 bundle. This is trust-payload
parity; the ARM64 final-image gate and fresh scan also passed. Live external
connector acceptance remains separate.

## Hermetic Google connector unit tests

The November 4 network-isolated root race attempt reached the Google connector
but failed when `Config.Open` fetched real Google discovery. That failed attempt
is retained; its `TestGetGroups` setup panic is not a passing connector result.
The maintained `scripts/upstream-tests.sh` now uses the checksum-pinned
`patches/google-discovery-overlay.json` to add `google_discovery_test.go` virtually
as `connector/google/kars_discovery_test.go`. No upstream test or assertion is
replaced, and no production Go file, dependency lock or `source.sha256` entry
changes. The script verifies the patch bundle and source inventory before tests,
rejects an existing overlay target, and rechecks source after the root suite.

The Google test binary installs its transport once before tests run and restores
it afterward. Only the exact GET to Google's discovery URL is routed to a local
`httptest` server; its document retains the real Google issuer and HTTPS endpoint
identities, so normal OIDC issuer checking remains enabled. Existing numeric HTTP
loopback Directory/GCE fixtures remain reachable without a proxy. Other external
requests, including real token/JWKS requests, fail rather than falling back to
DNS or live services. No TLS verification is disabled. The fixture is not compiled
into the shipping binary; its reviewed bytes are disclosed with the existing
patch bundle. This is unit-test isolation, **not Google login acceptance**.

Run a fresh qualification attempt with the updated packaging bundle at
`/packaging`, the unchanged reviewed source as the working directory, and the
existing dependency cache under mandatory `network=none`:

```sh
sh /packaging/scripts/upstream-tests.sh
```

The script still runs the complete NTLM, root and nested API race suites with
`-count=1`; only the root command gains
`-overlay=/packaging/patches/google-discovery-overlay.json`. Keep its existing
`/out/doc/ntlm-tests.json`, `upstream-tests.json` and `api-tests.json` evidence.
Do not modify or replay a frozen failed attempt. Go race execution and actual
external Google authentication remain separate, pending validation gates.

Focused offline packaging contracts (no Go invocation or network; the shell
orchestration tests use a command-recording shim):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bridge/idp/tests -p 'test_google_discovery.py'
```

## Persistent installed sessions

The Bridge chart supports the existing SQLite backend with
`idp.dex.persistence.enabled=true`. The Kind beta values opt into this setting
when an operator enables the optional IdP; the general compatibility default
remains memory storage. This is not a migration of a running memory database.
Schedule an explicit sign-in cutover when changing an existing installation;
do not restart the current issuer during outstanding token-expiry validation.

The chart mounts `/var/lib/dex/dex.db` from the `dex-data` ReadWriteOnce PVC,
uses UID/fsGroup 1001, and replaces its single Dex Pod with `Recreate` so an
upgrade never introduces a second SQLite writer. It requests 1Gi by default.
`idp.dex.persistence.size` and `storageClass` select the requested capacity and
provisioner; null uses the cluster default, while an empty string requires a
pre-provisioned volume. Kind's `standard` local-path provisioner survives Pod
replacement but not deletion of the Kind cluster or its node disk.

An operator may instead set `idp.dex.persistence.existingClaim` to a writable
claim in the Bridge namespace. The chart neither creates nor adopts that claim.
A chart-created PVC has `helm.sh/resource-policy: keep`: uninstall retains signing
keys and refresh sessions. Treat this data as credentials, retain it with the
issuer's identity and client/session Secrets, and explicitly select the retained
claim on reinstall. Do not delete it as routine rollout cleanup. External backup,
HA and cross-cluster identity migration are not provided by this single-writer
SQLite option.

Rendering tests and the earlier AL3 SQLite runtime checks do not establish an
installed persistence result. Acceptance requires a real login/refresh, actual
Dex Pod replacement with the same bound PVC, then verification that the old
unexpired token and freshly rotated session still work. The current memory-backed
issuer must not be reported as passing this gate.

## Pinned inputs and security changes

Dex release metadata/source, Go builder manifests and the updated AL3 manifests
were checked October 28, 2026. Pins are not a substitute for a fresh final-image
scan; a new advisory can block acceptance.

| Input | Selected pin / reason |
| --- | --- |
| Dex source | `https://codeload.github.com/dexidp/dex/tar.gz/75eb19238e10fc4750e7efb05a4031b254b04029`, SHA-256 `b5b0d6d2ffb0c4596dda7144593e6009cf2c37f0e3b8551805d09d84bbc3fb95` (1,200,632 bytes when checked) |
| Compiler/builder index | `golang:1.27.3-bookworm@sha256:d9b6a1beb9474adc2a1ce842573639b44b2a0dd49b6669f51d3821baf9ec37c9` (CVE-2026-111943) |
| Builder linux/amd64 manifest | `sha256:e482b31b5d30c31f9e495f4b8cbdf41429b2d74955dfbcd9c0a904483d738390` |
| Builder linux/arm64/v8 manifest | `sha256:ae675cdbf1bb782b6de4a56b157c8f91a5c89a594d8fa6475bb71efc4be3cfcb` |
| Runtime | `mcr.microsoft.com/azurelinux/distroless/base:3.0@sha256:6b85a86416a29cdcfd404e9fbd9c5ef49cc0612882cecc702535dcac39c9446c` |
| Runtime amd64 manifest | `sha256:28cdc9c25371b329318d63242084302021b33708c63f59ce72bbc21e50c831fd` |
| Runtime arm64 manifest | `sha256:96c7bfde3e1e5c502033f44446f6e1eea7f5ae60200b39025c4a8107cc124170` |

The Go 1.27.3 index was read from the public Docker Hub Registry API at
`https://registry-1.docker.io/v2/library/golang/manifests/1.27.3-bookworm`;
the AL3 index came from Microsoft's public container registry. Each native
child was fetched by its index descriptor digest. SHA-256 of response bytes
matched `Docker-Content-Digest` and each child descriptor, with exact child
sizes checked. This inspection downloaded manifest metadata only, not image
layers. The recipes pin indexes to retain native AMD64/ARM64 support. Metadata
inspection does not establish compiler execution or image qualification.
`GOTOOLCHAIN=local` forbids automatic replacement; the verifier requires both
resolver and builder to report **go1.27.3** on a supported native Linux target.

`locks/requests.txt` is a **resolution request, not a fake go.sum**:

The requests preserve released dependency versions except for the observed
security fixes: JOSE 4.1.6 (CVE-2026-116587) and `x/net` 0.60.0
(CVE-2026-78669). The regenerated, reviewed locks select both exact versions.
NTLM/OpenPGP guards, exact-version enforcement and zero-HIGH/CRITICAL
scan requirements remain unchanged. The AL3 refresh must remove OpenSSL
CVE-2026-72897, CVE-2026-84782 and CVE-2026-84784 without altering base files.

| Module | v2.46.1 / requested selection |
| --- | --- |
| `github.com/Azure/go-ntlmssp` | 0.1.1 (retains CVE-2026-32952 fix) |
| `github.com/go-jose/go-jose/v4` | 4.1.6 |
| `github.com/russellhaering/goxmldsig` | 1.6.1 |
| `go.opentelemetry.io/otel`, `/metric`, `/trace` | 1.45.0 |
| `golang.org/x/crypto` | 0.57.0 |
| `golang.org/x/mod` | 0.41.0 |
| `golang.org/x/net` | 0.60.0 |
| `golang.org/x/text` | 0.42.0 |
| `google.golang.org/grpc` | 1.84.0 |

The root's released **Go 1.27.0** directive and
`replace github.com/dexidp/dex/api/v2 => ./api/v2` are preserved. The existing
API resolver policy raises its released Go 1.25.0 directive to 1.26.0 and
requests gRPC 1.84.0/crypto 0.57.0. Go must generate the complete root/API graph,
including transitive requirements and checksums; any higher required directive
or unexpected selection requires review. The selected Go 1.27.3 satisfies the
released source requirement; `GOTOOLCHAIN=local` remains enforced.

Only the separate **`Dockerfile.locks`** maintenance recipe runs `go get` with exact
versions, followed by real `go mod tidy`, `download`, `verify` and `list`.
It uses the public checksum database and proxy without private configuration.
The image build only consumes the reviewed artifact with `-mod=readonly`;
it never runs `go get latest`, `tidy`, version selection or source generation.
The recipes are separate files, not just unrelated stages: legacy Docker
builders execute preceding stages even when the selected target does not
depend on them. Their pinned source/compiler stage is checked for consistency.
An unexpectedly higher MVS selection fails validation and needs review, not
a forced downgrade or a scanner waiver.

### NTLM update and package-scoped OpenPGP evidence

The exact NTLM release is
[`bd8579c18d41bf5d91a5f74b1117c958f635b866`](https://github.com/Azure/go-ntlmssp/tree/bd8579c18d41bf5d91a5f74b1117c958f635b866).
Its [module metadata](https://proxy.golang.org/github.com/!azure/go-ntlmssp/@v/v0.1.1.mod)
requires Go 1.24 and declares no module dependencies, compatible with the
selected Go 1.27.3 builder. The hosted upstream-tests stage requires the
actual `TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics`
regression to be registered, then runs `go test -json -count=1 -race
github.com/Azure/go-ntlmssp/...`. The actual JSON is retained as
`/out/doc/ntlm-tests.json` in the test image and printed into the existing
upstream build log. Standard IIS E2E cases retain their upstream
environment-dependent skips; no IIS credentials or new provider prerequisite
is introduced.

[GO-2026-5932](https://vuln.go.dev/ID/GO-2026-5932.json) concerns
`golang.org/x/crypto/openpgp` and its subpackages, not every package in
`golang.org/x/crypto`. The current reviewed graph selects **v0.58.0**, which
still receives the module-level annotation. The exact reviewed maintenance
candidate is **v0.58.1-0.20261009181718-2f1f834330b2** (fixed version metadata
verified through proxy.golang.org). It is not yet the committed resolved graph.
The GitHub-hosted procedure below must return a real resolver artifact before
review and promotion. There is no module suppression, exclusion or waiver.

Each build now retains the actual
`go list -deps -json=ImportPath,Module ./cmd/dex` result as
`/usr/share/doc/dex/runtime-packages.json`. It records runtime package paths
and their selected modules without test-only or tool-only packages. The
build-time notice collector rejects an empty/wrong-target inventory and
fatally rejects `golang.org/x/crypto/openpgp` or any descendant import path
before completing the image. Other crypto packages remain allowed and
scanned. The same inventory supplies license collection; existing `LICENCE`
handling is preserved. This is package-inclusion evidence and a future
introduction guard, **not** a scanner filter or proof that a pending build
has already passed.

## Disclosed source compatibility patches

v2.46.1 moved the authorization parser to `server/authflow/request.go` and
fixed both nonconstant format callers upstream. The old production patch is
removed, not reapplied or bypassed. The regression follows the real parser's
new package, helper and three-result signature. Upstream also renewed the OAM
SAML fixture certificate, so the old 2016 clock patch is removed. No upstream
production or test file is patched; only the two regression files are added.

| Reviewed input | Scope and reason |
| --- | --- |
| `patches/server_compat_test.go` | Installed as `server/authflow/kars_compat_test.go`; exercises the released authorization parser, including literal `%s%[1]s%%` input, the out-of-band redirect error, and unchanged type/state/URI. |
| `patches/saml_compat_test.go` | Installed as `connector/saml/kars_compat_test.go`; the real signature verifier must accept the unchanged signed assertion within certificate validity and reject it before validity and after expiry. |

The released OAM certificate is valid from **2026-07-02T12:04:52Z** through
**2125-07-02T12:04:52Z**; the old patch would now validate it at an invalid
2016 time. The retained regression derives test times from the actual
certificate, independently of the XML IssueInstant. The certificate, signed
XML, upstream tests and production expiry checks remain byte-identical to the
release. There is no global clock override, test skip, `-vet=off`, or waiver.

`patches/SHA256SUMS` pins both regression sources and their installed hashes.
Source fetch records and verifies a complete upstream inventory. It adds the
two regressions only if their target paths do not already exist, then
constructs the **expected** inventory from the unchanged original inventory
plus reviewed addition hashes. The actual tree must match that inventory
exactly, rejecting extra/missing files, tampering, drift and double application.
Every upstream file except the four separately pinned Go lock files is covered.
Obsolete patch files, series and pre-patch hash inventory are removed together
with their apply loop. Expected hashes are never recomputed from arbitrary
output.

Original and patched source inventories and the complete disclosed patch
bundle are included in `/usr/share/doc/dex/`. The British-spelled `LICENCE`
handling in the dependency-notice collector is preserved.

Source-only integrity checks (no Go compilation, bounded public download):

```sh
PYTHONDONTWRITEBYTECODE=1 KARS_DEX_PATCH_NETWORK=1 \
  python3 -m unittest discover -s bridge/idp/tests -p 'test_*.py'
```

Without that opt-in, offline source contracts still run; actual patch-application
tests are reported as skipped. While v2.45.1 locks remain, the active-lock
validation must fail (`resolver input drift: inputs.lock`); with network checks,
the separate upstream-snapshot comparison must also fail. Keep those gates,
do not suppress them to present source preparation as qualification. Neither
mode replaces hosted Go behavior regressions or runtime qualification.

October 28 source-preparation result after the Go 1.27.2 update: the exact
command above ran **41 tests: 39 passed, one failure and one error**, solely
the two stale-lock gates above. Released root/API directives pass the new
compiler preflight; regression coverage still rejects the old compiler and
requirements newer than 1.27.2. The earlier 40-test run had 38 passes and the
same stale-lock gates after removing the obsolete SAML clock patch.
Shell syntax and `git diff --check -- bridge/idp` passed; `git diff --quiet --
bridge/idp/locks/generated` confirmed no generated changes. No Go compilation,
resolver, image pull/build, container, installation or browser acceptance ran.

## Bridge to Headlamp browser sessions

The released `server/session` and `server/authflow/sessionlogin.go` provide the
maintained session implementation; Kars does not add a cookie/token relay.
The parent-owned runtime configuration must enable `DEX_SESSIONS_ENABLED=true`
and supply `sessions` settings. Keep `sessions.ssoSharedWithDefault: "none"`;
on the **Bridge source client**, explicitly set `ssoSharedWith` to the actual
Headlamp client ID. Sharing is one-way and independent of `trustedPeers`.
Do not use `"*"` or global `"all"` as a shortcut. Retain existing client IDs,
redirect URIs, secrets, users, signing keys and persistent SQLite data.

Configure finite session absolute/idle lifetimes and a stable protected
`sessions.cookieEncryptionKey` (16/24/32 bytes); no key or password ships here.
Dex scopes its HttpOnly, SameSite=Lax cookie to the issuer path and sets Secure
for an HTTPS issuer. Bridge and Headlamp must use the same browser-reachable
Dex issuer. A pre-upgrade Bridge login has no Dex session cookie: after the
upgrade, one normal login must establish it before click-through can reuse it.
Expired/revoked sessions, `prompt=login`, consent/MFA and `max_age` must still
require their normal authentication/approval steps.

The `session-tests` target runs the real upstream session/authflow suites with
race detection before the full upstream suite; the source contracts verify
these implementations remain unpatched. The October 28 Kind installation passed
normal Bridge login followed by both Configuration → SRE and Fleet Health →
Headlamp navigation without a second password or Headlamp Sign In click.
Headlamp received its own client audience; a Kubernetes SelfSubjectReview
identified the existing administrator and configured admin group. Reverse
Headlamp → Bridge silent sharing was denied. Bridge Sign out followed by Dex's
normal Log Out confirmation cleared both sessions, and revisiting Bridge
required credentials. This does not claim revocation of already-issued Headlamp
access tokens. The installed test issuer uses memory storage: timed expiry and
installed SQLite/key continuity remain separate checks, not inferred from image
qualification. Bridge uses ordinary top-level links, not a scripted login popup.

## Compatibility and shipping boundary

The entrypoint is `/usr/local/bin/dex`; default arguments are
`serve /etc/dex/config.yaml`, matching the existing chart's direct command.
No config, client secret, test password, unsafe issuer or auth defaults ship.
The operator must mount an explicit configuration. `secretEnv`, bcrypt
`staticPasswords`, memory storage, Authorization Code + PKCE, nonce and JWKS
are unchanged upstream implementations.

**CGO remains enabled for Dex.** No SQLite stub, `CGO_ENABLED=0`, custom
exclusion tags, static-glibc shortcut or connector removal is used to make a
scan pass. The Bookworm builder has an older glibc baseline than Azure Linux 3;
this is a compatibility premise, not proof of the actual native closure.
The shipping image copies **no Debian shared libraries**, compiler, shell,
package manager or build cache. Its loader and libraries must come from the
pinned Azure Linux base; hosted tests verify this and run real SQLite.
Build each platform on a native worker. Initial qualification is linux/amd64;
the multi-arch input indexes do not imply an arm64 qualification.

The complete upstream `web/` tree remains at `/srv/dex/web`, in addition to
the normal compiled web assets. Upstream connectors and storage source remain
unchanged. Only the official image's unused `docker-entrypoint` and `gomplate`
programs are omitted: template expansion through that wrapper is **not**
supported by this package. Direct Dex features are not replaced or removed.

Azure Linux's **14 RPM inventory records**, base files and CA trust are
retained rather than reconstructed. The final image installs the upstream
Apache-2.0 license, this package's MIT license, attribution, original module
snapshots, active patched module files, dependency diff, Go build metadata and ELF metadata under
`/usr/share/doc/dex/`. A build-time collector retains license/notice files
from linked modules (including bundled notices), and fails for missing root
licenses rather than silently shipping incomplete attribution. License review
of the generated graph and bundled native code is still an acceptance gate.

## Active locks versus archival baseline data

The following records the earlier v2.45.1 representation migration. That
artifact remains byte-for-byte preserved during source preparation. A reviewed
v2.46.1 resolver artifact must replace its baselines with the new release's
actual module files while retaining this `.snapshot` layout.

The verified hosted artifact's four historical module baselines are now
explicit **data snapshots**, not active Go manifests. The change is a
deterministic path-only migration of the already-verified artifact: all four
file contents and SHA-256 values are unchanged, and only their four paths
change in `locks/generated/SHA256SUMS`. No Go resolution, dependency update,
compiler/base change, exclusion, waiver or vulnerability suppression is involved.

Under `bridge/idp/locks/generated/upstream/`:

| Original archival path | Current data path |
| --- | --- |
| `go.mod` | `go.mod.snapshot` |
| `go.sum` | `go.sum.snapshot` |
| `api/v2/go.mod` | `api/v2/go.mod.snapshot` |
| `api/v2/go.sum` | `api/v2/go.sum.snapshot` |

The active patched `locks/generated/go.mod`, `go.sum`, `api/v2/go.mod` and
`api/v2/go.sum` retain their normal filenames, original patched bytes and full
dependency/security scanning. Module inventories, graph, dependency diff,
inputs, toolchain record and all other generated artifact contents are
unchanged. The saved historical baselines are used only for byte comparison,
diffing and attribution; the build still copies **only active locks** into
the Go source tree.

This distinction prevents dependency discovery from mistaking archived
upstream dependencies for new active dependencies. Actual runtime dependencies
and their final-image vulnerability scans remain fully enforced. The runtime
metadata copy preserves the same data suffixes under
`/usr/share/doc/dex/locks/upstream/`.

The original raw hosted-generation log remains unchanged outside the repository
as provenance, with its original transport hash and original archive member
names. `dependencies.patch` also keeps its logical `upstream/go.mod` and
`upstream/api/v2/go.mod` source labels; those labels are not stored manifest
files or compile inputs. Future generation emits the new snapshot layout
directly. The importer and validator require that layout and reject legacy or
mixed active-looking baseline names; do not reimport an old-layout log.
Source upgrades require fresh Go generation and reviewed promotion, not
rewriting existing snapshots.

Representation contracts compare baseline digests with the reviewed artifact
manifest, require exactly four normally named active Go locks, and exercise
archive import/rejection. With the verified-archive
source tests enabled, each baseline snapshot is also compared byte-for-byte
with its original file from the pinned upstream tarball.

## Hosted lock generation

### GitHub-hosted crypto fix candidate (prepared, not yet resolved)

`.github/workflows/bridge-idp-locks.yml` runs on path-scoped PR events for
`bridge/idp/**` or that workflow. Publishing these files to the current PR
triggers it without first registering `workflow_dispatch` on the default branch.
After the workflow reaches the default branch, its input-free manual dispatch
also works with an explicitly selected source ref. The job checks out the exact
PR head SHA (not an unrecorded merge result), uses read-only repository permission
and no persisted checkout credentials, secrets, registry login or image publish.
It runs only the existing pinned Linux AMD64 `Dockerfile.locks` resolver through
Buildx's local `lock-artifact` exporter; it does not run the runtime image build.
Heavy builds/resolution stay on GitHub, not the developer machine.

`scripts/hosted-locks.py prepare` first verifies the current reviewed locks,
copies an isolated candidate context under `artifacts/dex-lock-candidate/`, and
changes **only that copy's** crypto request to
`golang.org/x/crypto@v0.58.1-0.20261009181718-2f1f834330b2`. This is a source-reviewed
literal, not a workflow input or shell-interpolated version. The existing resolver
now reads the same quoted crypto request for the root and nested API modules.
The committed `locks/requests.txt` and `locks/generated/` remain coherent and
unchanged until a subsequent artifact review. Normal runtime builds remain
read-only consumers of the committed locks; the OpenPGP inventory rejection
remains mandatory.

Hosted run **41997973603**, attempt **1**, resolved the fixed root crypto version
but retained **v0.58.0** in the nested API graph. The API imports no crypto
packages, so its `go mod tidy` removed the explicit candidate requirement and
exposed the older requirement through `x/net`. Strict sealing correctly failed
with `nested API unreviewed selection: golang.org/x/crypto`. That artifact remains
failed evidence, not an importable or promoted graph. The resolver now reapplies
the same exact crypto request through `go get` **after nested API tidy**, before
download, verification and inventory export. No later tidy removes the policy
pin. The coordinated root/API version checks are unchanged. A shell-orchestration
regression models the observed tidy fallback and verifies re-pinning and failure
propagation without invoking Go; only a fresh hosted attempt can establish real
resolver and subsequent runtime qualification success.

The evidence includes `provenance.json` with exact source/workflow SHA-256 inputs,
source/event SHAs, repository, run ID and attempt; `resolver.log`; the unmodified
`resolved/` Go output; and, on successful validation, `verified.json` binding the
provenance and complete lock checksum manifest. Validation reuses `check_modules`
and also rejects any upstream `.snapshot` byte change. `job-status.txt`,
`source-diff.log` and logs survive failures, with each run/attempt uploaded as a
separate `dex-lock-candidate-RUN_ID-ATTEMPT` artifact (30-day retention). Download
failed attempts for diagnosis, not import. Retain downloaded evidence beyond
GitHub's retention window when keeping qualification history.

After publication, inspect the specific workflow's run and source SHA:

```sh
gh run list --workflow bridge-idp-locks.yml --branch YOUR_PR_BRANCH
gh run view RUN_ID --json conclusion,headSha,event,url,jobs
mkdir -p artifacts/dex-lock-review-RUN_ID
gh run download RUN_ID --name dex-lock-candidate-RUN_ID-ATTEMPT \
  --dir artifacts/dex-lock-review-RUN_ID/evidence
PYTHONDONTWRITEBYTECODE=1 python3 bridge/idp/scripts/import-locks.py \
  --hosted-artifact artifacts/dex-lock-review-RUN_ID/evidence \
  --output artifacts/dex-lock-review-RUN_ID/reviewed
```

Check the GitHub run succeeded and its repository/source/run/attempt match the
downloaded provenance. Checksums detect drift; they are not a substitute for
retrieving the artifact from the trusted GitHub run. The importer revalidates all
current reviewed source/version checksums and the real module graph before
writing a **new** review directory. A changed checkout fails closed: review
against the published input bytes, not a rewritten provenance file. It never
promotes a hosted candidate directly to `locks/generated` or overwrites output.
Review `dependencies.patch`, all actual module selections, both root/API
checksums, toolchain provenance and unchanged source baselines. Only a subsequent
explicit promotion should replace the reviewed generated directory and update
`locks/requests.txt` together from that verified artifact. Then run fresh hosted
runtime, race, inventory and unsuppressed scanner qualification. Preparing this
job or importing locks alone does **not** resolve the advisory or qualify an image.

Lightweight local contracts (no Go/Docker invocation, network or installation):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bridge/idp/tests -p 'test_*.py'
```

### Historical resolver transport and replay

The following ACR/log transport remains readable for retained historical evidence;
use the GitHub job above for this candidate and keep heavy work hosted.

The earlier hosted workflow was selected when local Docker/Go were unavailable
and disk was below its admission floor; that is historical evidence, not a
fresh resource check. The parent owns fresh resource admission and serial heavy
operations. Do not install Go, compile or populate caches during source preparation.
The following task uses an operator-approved ACR's compute but **does not push
an image**, deploy, or touch cluster state.
The context is this public-only directory, not private repository config.

For v2.46.1, the Go 1.27.3 compiler input satisfies the released source
requirement. The commands below remain an alternative hosted resolver path,
not evidence of a hosted run or installed upgrade acceptance. Use the staged
review procedure for future changes; do not overwrite generated locks merely
to make the default importer run.

From the public Kars worktree root, set `SUBSCRIPTION_ID`, `ACR_NAME` and
`ARTIFACT_DIR` to approved operator values. Keep the artifact directory private.

```sh
(
  cd bridge/idp
  az acr build \
    --subscription "$SUBSCRIPTION_ID" --registry "$ACR_NAME" \
    --platform linux/amd64 --no-push --timeout 3600 \
    --target lock-generation --file Dockerfile.locks .
) > "$ARTIFACT_DIR/dex-lock-generation.log" 2>&1
```

`Dockerfile.locks` pins all public execution/source inputs above. This target
does **module metadata/source resolution only**, not Dex compilation or tests.
It uses only worker-owned `/work/mod` and `/work/cache`. Do not add registry
push credentials, Azure credentials, build secrets or private Go settings.
The log contains one public tar.gz artifact between
`KARS_DEX_LOCKS_BASE64_BEGIN` / `KARS_DEX_LOCKS_BASE64_END`, followed by its
transport SHA-256. Retain the ACR run ID and original log as provenance.

```sh
python3 bridge/idp/scripts/import-locks.py "$ARTIFACT_DIR/dex-lock-generation.log"
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bridge/idp/tests -p 'test_*.py'
```

The importer checks the logged transport SHA-256 as well as the member
checksums. It rejects unsafe members, duplicate/incomplete files, oversized
archives, checksum/input drift, module resolver errors, duplicate module records,
unexpected toolchains and unreviewed module selections; it does not
overwrite existing locks. If ACR log delivery prefixes/truncates the artifact,
retrieve the **raw run log** rather than repairing checksums or guessing files.
Alternatively, on an approved hosted BuildKit worker:

```sh
docker buildx build --platform linux/amd64 --target lock-artifact \
  --output "type=local,dest=$ARTIFACT_DIR/dex-locks" \
  --file bridge/idp/Dockerfile.locks bridge/idp
```

Artifact contents are the generated root/API `go.mod` and `go.sum`,
`modules.json`, `api-modules.json`, `graph.txt`, `toolchain.txt`,
`inputs.lock`, `requests.txt`, `dependencies.patch`, the four byte-unchanged
`upstream/` baseline data files (`go.mod.snapshot` / `go.sum.snapshot` at the
root and under `api/v2`), and `SHA256SUMS`. Review and persist these under
`bridge/idp/locks/generated/` before any runtime build. Generated module
checksums are Go's, not fabricated or transcribed from vulnerability reports.
On a second clean **native worker of the same architecture**, replay the
resolver without its cache and compare the entire reviewed artifact:

```sh
docker build --no-cache --target lock-replay \
  --file bridge/idp/Dockerfile.locks bridge/idp
```

The non-shipping artifact and replay stages default to `1001:1001`. Both sets
of replay inputs are copied with that ownership, without changing lock bytes
or applying `chmod` to upstream data. Replay asserts its actual UID/GID before
running the read-only checksum comparisons. In `Dockerfile.locks`, root is
retained only in the source/resolution stages that need build-directory writes.

The replay target verifies both checksum inventories and requires exact
agreement for the root/API locks, selected module graphs, source snapshots,
dependency diff, inputs and toolchain record. Retain the build log with
`KARS_DEX_LOCK_REPLAY_PASSED`. A replay failure is a real reproducibility
blocker, not permission to overwrite the reviewed locks. Use a native Docker worker or an ACR task exposing Docker's `--no-cache`
option for this independent replay; do not assume the quick-build CLI exposes
that option. Neither replay nor generation is reachable from the normal
`Dockerfile`.

### Offline compilation/testing and execution bounds

The lock resolver has **no built-in bounded offline compilation/testing mode**.
`lock-generation` only runs online module resolution/download/verification and
exports the real graph. It does not run Dex compilation or behavioral tests.
Its script sets `GOFLAGS=-mod=mod`; it exposes no concurrency, memory or CPU
build arguments. The documented ACR `--timeout 3600` is a task wall-clock
limit, not a CPU/memory limit. The parent must admit the worker separately.

`lock-replay` compares artifacts but depends on a fresh `lock-generation`;
it is not an offline replay target. The separate runtime recipe has maintained
`compatibility-tests`, `session-tests`, `upstream-tests` and `test-tools`
targets with the real race suites, but currently does not enforce network-off
execution or expose resource-bound test options. A warm cache alone is not
proof of offline execution. Do not disable checksum verification, race checks
or required suites to call the existing path an offline/bounded validator.
Source-only Python contracts remain the small validation path used here.

### Parent-only no-push v2.46.1 lock-generation and review

Set `SUBSCRIPTION_ID`, `ACR_NAME` and an existing `ARTIFACT_DIR` **outside the
source checkout** to approved operator values. The task below only resolves
the exact requested Go graph on the pinned worker image; it does not build or
push a Dex image. Only the parent submits it after fresh worker admission.
Exact current inputs are the v2.46.1 commit/archive above, all eleven
`locks/requests.txt` requests (including the reviewed JOSE and `x/net` updates),
the Go 1.27.3 builder index/platform manifests above, and the maintained patches
and regression bundle. The pinned distroless digest applies to the later
runtime build, not the resolver. Current locks came from the successful local
native Linux ARM64 resolver; these commands do not imply another hosted run.

```sh
(
  cd bridge/idp
  az acr build \
    --subscription "$SUBSCRIPTION_ID" --registry "$ACR_NAME" \
    --platform linux/amd64 --no-push --timeout 3600 \
    --target lock-generation --file Dockerfile.locks .
) > "$ARTIFACT_DIR/dex-v2.46.1-lock-generation.log" 2>&1

PYTHONDONTWRITEBYTECODE=1 python3 bridge/idp/scripts/import-locks.py \
  "$ARTIFACT_DIR/dex-v2.46.1-lock-generation.log" \
  --output "$ARTIFACT_DIR/dex-v2.46.1-reviewed-locks"
```

`--output` validates the complete archive, current request stamp and exact
selected versions in a temporary sibling, then creates a new review
directory. It refuses any existing output and any alternate location inside
the source checkout. Failed/stale artifacts leave both the proposed output
and existing qualified locks untouched.

Before promotion, review the generated root/API manifests, sums, module
inventories, graph and `dependencies.patch`. Confirm NTLM is exactly v0.1.1,
all eleven requested selections match the released source, the approved
compiler is compatible, and original `.snapshot` baselines match v2.46.1's
pinned upstream bytes. The AL3 digest remains unchanged. Keep the old artifact and
raw generation logs outside the repository as historical provenance. Only
after that review may the parent replace `locks/generated/` with the verified
new artifact; this source change does not perform that replacement.

Then rerun source contracts, the hosted notice-guard Go tests, NTLM package
regressions, compatibility/session/root/API/probe race suites, the full actual
distroless runtime qualifier and fresh scans. Require zero HIGH/CRITICAL
findings **and** confirmed resolution of the current NTLM advisory. Retain
the new package inventory and establish a new binary/payload reference.
The prior green checks and old binary hash do not qualify this update.

## Required Bridge CI qualification

[Bridge CI](../../.github/workflows/bridge-ci.yml) adds **Dex IdP runtime
qualification** (`idp`) to the existing **Bridge component acceptance**
aggregate. Missing, failed, cancelled or skipped IdP results fail that
aggregate; existing component checks remain required. The job has only
`contents: read`, builds on native amd64 Ubuntu Docker compute, and does not
log in to a registry, push an image or deploy cloud/cluster resources.

The job runs source contracts with the verified-archive checks enabled and
explicitly validates the actual generated module locks. It builds
`upstream-tests`, `test-tools` and `runtime` from this directory. An owned
temporary container supplies the upstream root/API JSON reports; each report
must be complete and contain no failures. The summary records actual passing
test counts, package results and every skip, including packages with no test
files, without claiming that unconfigured LDAP/cloud integrations ran.

The existing SHA-pinned Trivy action installs **0.70.0** and performs an initial
actual-image scan. The job discovers its executable using `command -v trivy`,
checks its version, and passes that path to `tests/qualify.py`. The harness
executes the real memory/SQLite/static-OIDC and key-continuity checks plus
native linkage, all base inventory/CA checks, and a separate strict scan with
a fresh cache and current DB. It still runs after an initial scan finding to
retain useful diagnostics; **both scan failures remain fatal**, with no
`continue-on-error` or alternate success path.

The always-uploaded `bridge-idp-qualification-<commit>-<attempt>` artifact
contains source/build/probe/harness logs, upstream/API JSON and skip summary,
image provenance, discovered Trivy version/path, per-step outcomes, initial
scan JSON and all runtime evidence produced before success or failure. A failed
build retains its failure log instead of fabricating a completed test report.
Partial artifacts and upstream test success are not runtime qualification;
the harness's `runtime/runtime-passed.json` covers only its named gates, and
the overall job and aggregate must also succeed.

## Hosted acceptance gates

First run the focused compatibility target on approved hosted compute. The
subscription/registry/artifact values remain operator parameters; this example
does not push an image:

```sh
(
  cd bridge/idp
  az acr build \
    --subscription "$SUBSCRIPTION_ID" --registry "$ACR_NAME" \
    --platform linux/amd64 --no-push --timeout 3600 \
    --target compatibility-tests --file Dockerfile .
) > "$ARTIFACT_DIR/dex-compatibility.log" 2>&1
```

That target runs the real server/SAML regressions with race detection and vet
enabled. The earlier hosted attempt already passed those build/test stages;
the commands remain available to reproduce them before running the actual
runtime harness. The narrower target alone is not acceptance. Legacy ACR builders execute
preceding stages on the way to `test-tools`; the compatibility/upstream stages
are intentionally retained rather than reordered to conceal failures.

On a **native Linux Docker worker with sufficient disk**, build the reviewed
context; examples below are local worker tags, never a publication:

```sh
docker build --target compatibility-tests -t kars-dex-compat:latest bridge/idp
docker build --target upstream-tests -t kars-dex-tests:latest bridge/idp
docker build --target test-tools -t kars-dex-tools:latest bridge/idp
docker build --target runtime -t kars-dex:latest bridge/idp
trivy_path="$(command -v trivy)"
test -x "$trivy_path"
"$trivy_path" --version
python3 bridge/idp/tests/qualify.py \
  --image kars-dex:latest --tools-image kars-dex-tools:latest \
  --trivy "$trivy_path" \
  --evidence "$ARTIFACT_DIR/dex-runtime"
```

The runtime harness uses image IDs after resolving tags, private ephemeral
Docker networks/volumes, randomly generated test-only credentials, nonroot
read-only Dex containers, no capabilities and no-new-privileges. It cleans up
only its own uniquely named containers, networks, volumes and temporary files.
Test config and the external probe **never** enter the shipping image.

Required evidence before parent acceptance:

1. **Full upstream root/API suites** with race detection, retained JSON reports
   (`/out/doc/upstream-tests.json`, `/out/doc/api-tests.json` in the test image).
   Review and report every skip and remaining integration limit. Execute the
   configured memory/SQLite/OIDC paths and relevant configured connector
   integrations. Unconfigured external services do not become additional beta
   prerequisites, but a skipped integration is not evidence of runtime
   compatibility. Real code and compile/unit coverage remain; no production
   service implementation is stubbed by this packaging.
2. **Real final-image runtime checks:** discovery, HTML password login, bcrypt,
   `secretEnv` client authentication, S256 PKCE, JWKS RSA signature and
   issuer/audience/nonce/expiry claims, userinfo, incorrect password/client
   secret/verifier rejection, authorization-code replay rejection. Run all
   against memory and SQLite. After SQLite restart, verify the old unexpired
   ID token against restarted JWKS and the saved verified signing-key identity
   before using the earlier refresh token. Retained old keys across legitimate
   rotation must pass; missing/replaced keys must fail even if refresh works.
3. **Native closure and runtime contents:** ELF interpreter from the Azure Linux
   base, its real `--list` output, successful native execution, unchanged base
   file hashes/links and CA trust, all 14 RPM records, and no copied library
   closure or extra tools. Docker's injected hosts/hostname/resolv.conf are the
   only per-container export exclusions.
4. **Trivy 0.70.0, latest DB, zero HIGH/CRITICAL** across the final OS and Dex
   binary. The harness uses a fresh cache, empty ignore/config files, explicit
   vulnerability scanning, no ignored/unfixed exemptions, and requires
   recognized Azure Linux inventory plus Go binary inventory. Persist JSON,
   scanner/DB metadata, immutable image ID and ELF reports. Unknown/unscanned
   payload is failure, not a clean report.
5. **Independent reproducibility and licenses:** rebuild without cache on a
   second clean native worker and compare `/usr/local/bin/dex` SHA-256 plus
   web/license/dependency payloads. Container export timestamps/image IDs alone
   are not the reproducibility criterion. Review all dependency changes,
   licenses and bundled SQLite attribution.
6. **Configured connector and beta integration checks** belong to the parent,
   including genuine auth/native enrollment. Packaging probes do not diagnose
   that flow and do not authorize shared chart/default/workflow changes.

`runtime-passed.json` covers only the runtime harness's named gates, not full
qualification. No acceptance/publication/deployment status is implied by
source contracts, a generated lockfile, a successful compiler exit or a
scanner version floor. Any new vulnerability or compatibility failure blocks
acceptance and needs a real, disclosed correction.
