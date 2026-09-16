<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Kars Dex security rebuild

**Status: hosted build/test stages passed; final runtime qualification pending.**
Real Go-generated `locks/generated/` files are present and remain unchanged.
The successful hosted attempt on September 16, 2026 compiled Dex with CGO,
completed license collection, passed the compatibility cases, completed the
upstream root/API race-suite commands with exit code zero, passed all nine
signing-key-continuity regression subcases under the race detector, and compiled
the probe. Environment-dependent suite skips do not establish external
connector runtime coverage.

The required `idp` job in public Bridge CI now builds these checked-in inputs
and executes the actual final distroless memory/SQLite/OIDC, linkage, inventory
and scan gates. That CI execution is still required; the earlier build/test
success is **not** final-image qualification or permission to deploy. This
wiring does not enable the optional IdP or change chart/default/installation
behavior.

This is a packaging/dependency correction for the existing optional Dex IdP,
not another IdP or authentication framework. It starts from **Dex v2.45.1**
application source at
[`11d2eeb52b42e1980e14cb91e69dd9e3faab2076`](https://github.com/dexidp/dex/tree/11d2eeb52b42e1980e14cb91e69dd9e3faab2076).
The [stable release](https://github.com/dexidp/dex/releases/tag/v2.45.1) was
published March 3, 2026. The binary identifies itself as **v2.45.1-kars.1**,
and image labels and installed notices disclose both the dependency rebuild
and the bounded source changes.

## Pinned inputs and security changes

Public metadata was checked September 16, 2026. Pins are not a substitute for
a fresh final-image scan; a new advisory can block acceptance.

| Input | Selected pin / reason |
| --- | --- |
| Dex source | `https://codeload.github.com/dexidp/dex/tar.gz/11d2eeb52b42e1980e14cb91e69dd9e3faab2076`, SHA-256 `18bf92e8ccbf53e86814c2beb39b7d59f28fb07c84639e9f47a9bb5ea764e0b9` (863,553 bytes when checked) |
| Compiler/builder | `golang:1.26.8-bookworm@sha256:9fdc884aacc3bec89b20ffc69f4bb369c78210e3e4f600387b5128b12c199f81` |
| Builder amd64 manifest | `sha256:bc6beb46032d45f421cf400036bf031cdc64f683ba9cdc124e31d063e71670bd` |
| Runtime | `mcr.microsoft.com/azurelinux/distroless/base:3.0@sha256:4377af4aa7a810b7d59f691eae5066895a71aa3eee4cfb4eba527bbebff16479` |
| Runtime amd64 manifest | `sha256:0198b6345e0aeffd6c2e455ebc4c74b1d644ac1dfcb5aa5236972f17fd281f27` |

[Official Go download metadata](https://go.dev/dl/?mode=json&include=all)
selects **go1.26.8**, the latest stable 1.26 patch when checked.
[Release notes](https://go.dev/doc/devel/release#go1.26.8) date it September 1.
The official linux-amd64 Go archive SHA-256 is
`d0f743b33e8d8945e6b1f432edd15785c70507121d6e2a723b21285eddf8b57b`;
the Docker build uses the pinned official builder, not an unchecked download.
`GOTOOLCHAIN=local` forbids automatic replacement of the selected compiler.

`locks/requests.txt` is a **resolution request, not a fake go.sum**:

| Module | Upstream | Requested selection | Authoritative fix / dependency reason |
| --- | --- | --- | --- |
| `github.com/go-jose/go-jose/v4` | 4.1.3 | 4.1.4 | [GO-2026-4945](https://vuln.go.dev/ID/GO-2026-4945.json) |
| `github.com/russellhaering/goxmldsig` | 1.5.0 | 1.6.0 | [GO-2026-4753](https://vuln.go.dev/ID/GO-2026-4753.json) |
| `go.opentelemetry.io/otel`, `/metric`, `/trace` | 1.39.0 | 1.44.0 | [GO-2026-5506](https://vuln.go.dev/ID/GO-2026-5506.json) fixes 1.41; [GO-2026-5158](https://vuln.go.dev/ID/GO-2026-5158.json) and gRPC require 1.44 |
| `golang.org/x/crypto` | 0.48.0 | 0.56.0 | 0.55 fixes [GO-2026-6303](https://vuln.go.dev/ID/GO-2026-6303.json), but September advisories [GO-2026-6354](https://vuln.go.dev/ID/GO-2026-6354.json) / [6355](https://vuln.go.dev/ID/GO-2026-6355.json) require 0.56 |
| `golang.org/x/mod` | 0.32.0 | 0.40.0 | [GO-2026-6179](https://vuln.go.dev/ID/GO-2026-6179.json), [6180](https://vuln.go.dev/ID/GO-2026-6180.json) |
| `golang.org/x/net` | 0.50.0 | 0.58.0 | 0.56 fixes [GO-2026-5942](https://vuln.go.dev/ID/GO-2026-5942.json); gRPC requires 0.58 |
| `golang.org/x/text` | 0.34.0 | 0.41.0 | 0.39 fixes [GO-2026-5970](https://vuln.go.dev/ID/GO-2026-5970.json); gRPC/crypto require 0.41 |
| `google.golang.org/grpc` | 1.79.1 | 1.83.2 | [Stable security release](https://github.com/grpc/grpc-go/releases/tag/v1.83.2), [GO-2026-6443](https://vuln.go.dev/ID/GO-2026-6443.json) explicitly fixes the 1.83 branch in 1.83.2 |

The exact module manifests are public at
`https://proxy.golang.org/<module>/@v/<version>.mod`; their `.info` siblings
record upstream commit and publication metadata.
[gRPC 1.83.2](https://proxy.golang.org/google.golang.org/grpc/@v/v1.83.2.mod)
requires Go 1.25 and raises OTel/net/text.
[crypto 0.56.0](https://proxy.golang.org/golang.org/x/crypto/@v/v0.56.0.mod)
requires **Go 1.26.0**, so both root and nested API module directives are
deliberately raised from upstream's 1.25.0 / 1.24.0 to 1.26.0. The root's
`replace github.com/dexidp/dex/api/v2 => ./api/v2` is retained.
`x/mod` also requires `x/tools` 0.49.0; let Go resolve the full graph rather
than hand-editing transitive versions or checksums.

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

## Disclosed source compatibility patches

The original upstream commit and archive SHA-256 have **not** changed.
No new dependency versions, module locks, compiler pins or base images are
introduced by these compatibility corrections.

| Reviewed input | Scope and reason |
| --- | --- |
| `patches/0001-literal-oauth-error-descriptions.patch` | Exactly two production calls in `server/oauth2.go`, originally lines 477 and 561, pass already-built descriptions as `"%s", description` / `"%s", err`. This satisfies Go vet and preserves literal percent characters instead of formatting the text twice. Error type, state, redirect URI and authorization decisions are unchanged. |
| `patches/0002-saml-fixture-validation-clock.patch` | Test-only change to `connector/saml/saml_test.go`. Only `TestVerifyUnsignedMessageAndSignedAssertionWithRootXmlNs` opts into a validation clock at the signed XML's `2016-12-12T16:54:35Z` IssueInstant. Existing helper callers keep a nil clock, which means the real clock. |
| `patches/server_compat_test.go` | Installed into the fetched source as `server/kars_compat_test.go`; exercises the real authorization parser, including literal `%s%[1]s%%` input and the out-of-band redirect error. |
| `patches/saml_compat_test.go` | Installed as `connector/saml/kars_compat_test.go`; the real signature verifier must accept the unchanged signed assertion at fixture time and reject it before certificate validity and after expiry. |

The OAM fixture certificate is valid from **2016-06-30T04:54:16Z** through
**2026-06-28T04:54:16Z**. Its signed XML's IssueInstant is inside that interval.
This is an XML namespace/signature fixture, not a wall-clock expiry test.
The supported test-clock API is
[`NewFakeClockAt`](https://github.com/russellhaering/goxmldsig/blob/878c8c615feb628064040115d00e105a137fcfa7/clock.go);
[production validation](https://github.com/russellhaering/goxmldsig/blob/878c8c615feb628064040115d00e105a137fcfa7/validate.go)
still enforces the trusted certificate's `NotBefore` and `NotAfter`.
No certificate, signed XML, production SAML code, trust setting or vet setting
is weakened or replaced.

`patches/SHA256SUMS` pins the patch order, both patches, regression sources and
the before/after file-hash manifests. The source stage first verifies the
original archive, inventories and verifies the **original** source, then
applies the reviewed patches with `git apply`. It verifies the reviewed post-patch hashes and
derives the expected `source.sha256` from the original inventory plus only
the four declared changed/added files. It compares that expected inventory
against the entire actual source inventory, rejecting additional files,
missing files, tampered patches, drift and double application.
Only the four dependency manifests are excluded from source inventory, as
before; they are independently checked against the generated Go locks.
The changed source files are **not** excluded or accepted by recomputing
their expected hashes from whatever a patch happens to produce.

Original and patched source inventories and the complete disclosed patch
bundle are included in `/usr/share/doc/dex/`. The British-spelled `LICENCE`
handling in the dependency-notice collector is preserved.

Source-only integrity checks (no Go compilation, bounded public download):

```sh
PYTHONDONTWRITEBYTECODE=1 KARS_DEX_PATCH_NETWORK=1 \
  python3 -m unittest discover -s bridge/idp/tests -p 'test_*.py'
```

Without that opt-in, offline source contracts still run; actual patch-application
tests are reported as skipped. Neither mode is a replacement for the hosted
Go behavior regressions or runtime qualification.

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
Apache-2.0 license, this package's MIT license, attribution, original/patched
module files, dependency diff, Go build metadata and ELF metadata under
`/usr/share/doc/dex/`. A build-time collector retains license/notice files
from linked modules (including bundled notices), and fails for missing root
licenses rather than silently shipping incomplete attribution. License review
of the generated graph and bundled native code is still an acceptance gate.

## Hosted lock generation

Local Docker and Go were unavailable and disk was below the 8.5 GiB floor.
Do not install Go, compile, populate a global cache or run these builds locally.
The following task uses an operator-approved ACR's compute but **does not push
an image**, deploy, or touch cluster state.
The context is this public-only directory, not private repository config.

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
`inputs.lock`, `requests.txt`, `dependencies.patch`, the four unchanged
`upstream/` module files, and `SHA256SUMS`. Review and persist these under
`bridge/idp/locks/generated/` before any runtime build. Generated module
checksums are Go's, not fabricated or transcribed from vulnerability reports.
On a second clean **native worker of the same architecture**, replay the
resolver without its cache and compare the entire reviewed artifact:

```sh
docker build --no-cache --target lock-replay \
  --file bridge/idp/Dockerfile.locks bridge/idp
```

The replay target verifies both checksum inventories and requires exact
agreement for the root/API locks, selected module graphs, source snapshots,
dependency diff, inputs and toolchain record. Retain the build log with
`KARS_DEX_LOCK_REPLAY_PASSED`. A replay failure is a real reproducibility
blocker, not permission to overwrite the reviewed locks. Use a native Docker worker or an ACR task exposing Docker's `--no-cache`
option for this independent replay; do not assume the quick-build CLI exposes
that option. Neither replay nor generation is reachable from the normal
`Dockerfile`.

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
