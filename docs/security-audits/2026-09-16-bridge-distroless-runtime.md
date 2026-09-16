<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Bridge shipping runtimes: bounded delegated source review

Base: `d01d0e258454f4ed61c6788c7ca3bf07f2fa93ab`.
Reviewed source: `2a99e2a7317877ae9da6fcb6924fe46192eba719`.

Status: **Scoped source-approved for qualification; shipping-image builds,
scans and current-head native acceptance remain required.**
This record does not approve deployment or waive an image finding.

## Observed packaging and qualification gap

The imported shipping BFF and web Dockerfiles retained Debian runtimes while
Kars core used Microsoft Azure Linux 3 distroless. The native BFF lane used a
separate distroless test Dockerfile, so that successful native result did not
qualify the shipping runtime. The source/configuration security job did not
scan the final application images.

Actual September 16 remote builds of the preceding source succeeded and passed
their non-root/read-only smoke. Image qualification then rejected 58 Debian
High/Critical findings in the BFF, the same OS findings plus 11 global-npm
findings in the web image, and separate Alpine/Go findings in the optional
witness image. The web language findings were in global npm, not the traced
application dependency tree. These failed outcomes are retained; no source-CI
success is substituted for a passing final-image scan.

## T1: New capability or attack surface?

No application routes, roles, credential handling or cluster authority change.
The BFF now ships on the same Azure Linux distroless base as the controller and
router. Native BFF qualification builds that shipping Dockerfile rather than a
different test image. Rust toolchains remain in a discarded build stage.

Node services retain Node 22 and the existing application locks. Microsoft
does not publish an Azure Linux distroless Node 22 image. The images therefore
copy the official Node executable and license from the Node 22 build stage,
not npm or Debian libraries, onto the Microsoft distroless base.

The Node executable and native web addon require Microsoft `libstdc++`.
Its complete RPM-owned payload is exported from the official Azure Linux core
image after RPM verification and exact glibc/libgcc inventory matching.
Both original distroless package manifests are preserved and extended with the
added package's actual metadata. Package records are not removed to hide scan
findings. Mutable upstream tags still require resolved-digest qualification.

## T2: Security-control change?

Shipping runtimes retain numeric non-root identity and direct executable
entrypoints. Build-stage checks execute Node 22, parse the Microsoft CA bundle
and exercise native sharp encoding/decoding on the final runtime libraries.
The gateway imports its compiled application in the final runtime.

The actual final-image inspector checks distribution, identity, entrypoint,
shell/package-manager absence and populated CA trust. It retains package
inventory and application dependencies. BFF smoke requires real health and
explicitly unconfigured readiness, not a fabricated cluster connection.

The offline gateway smoke uses deliberately synthetic settings with networking
disabled. It requires both the SDK application listener and the separate health
listener: health alone can hide a caught SDK startup failure. Node version,
identity and CA/TLS-context checks remain required. This is not Teams
authentication, outbound TLS or Kubernetes integration qualification.

Existing mandatory component jobs now build and scan the shipping BFF, web and
gateway images. High/Critical OS and language findings, including unfixed ones,
remain fatal. JSON scan evidence is retained. There are no ignore files,
severity waivers or successful fallbacks for unavailable scans.

## T3: Availability, compatibility and evidence limits

Node major version, application locks, ports, static/standalone content and
web cache behavior are preserved. The Microsoft CA bundle is explicitly
available to Node; no TLS verification is disabled. Native library and
read-only startup behavior still require actual hosted execution.

Sixteen Node packaging contracts and eight image-contract/orchestration tests
passed locally. Workflow YAML parsed with the verified existing locked parser.
The parent also verified the real digest-matched Microsoft base filesystem and
CA bundle. The implementation owner checked amd64 ELF dependency/version closure;
that inspection is not native execution or arm64 qualification.

The updated public Helm guide records the actual fresh-install Helm 4
Namespace conflict and create-only recovery. Its local render/metadata and
shell-syntax checks passed without cluster mutation. It refuses an existing
namespace rather than granting permission to adopt or reset it.

Two independent AI review contexts covered the parent BFF/native/image-gate/
documentation changes and the separate Node composition/gateway-smoke changes.
Both reported no significant issue within their bounded source scope.
They did not establish runtime compatibility or blanket application security.
Actual Docker builds, final-image vulnerability scans and the updated native
lane have not completed for this source. Existing core resources were not
changed by this correction. Optional witness remediation remains separate.

### First hosted outcome and test-only correction

[Bridge CI 35089486444](https://github.com/Azure/kars/actions/runs/35089486444)
at audit-only head `a8d4585c842e5a7e584c3c9b0c8295ece777eba1` subsequently
built and qualified the actual BFF and web images. Both final runtimes were
identified as Azure Linux 3, passed their image/startup contracts, and returned
zero High/Critical image findings with Trivy 0.70.0. Web native sharp execution
also passed. The downloaded scan artifacts' server digests and head identity
were verified; the workflow's synthetic merge tree equals the candidate tree.

The overall component workflow still failed: an older packaging test expected
the literal `USER 10001` rather than the intended explicit `USER 10001:10001`.
The test-only correction requires the exact UID/GID, Microsoft distroless base
and absence of runtime shell instructions. It does not change image contents
or relax non-root behavior. The first failed result remains failed, and the
gateway image steps were not reached. Matching local Vitest dependencies were
unavailable, so the corrected test still requires actual locked hosted execution.
Native acceptance and the complete current-head component gate remain required.

## Delegation and verdict

This uses the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
Implementation and independent review occurred in separate AI contexts, not
two human reviews. The parent owns composition and this record.

Verdict: accept this bounded source correction for qualification. All
current-head technical/security checks and supported installation prerequisites
remain mandatory before merging into `kars-bridge` or deploying images.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
