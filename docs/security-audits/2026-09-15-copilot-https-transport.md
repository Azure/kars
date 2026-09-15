<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Copilot transport - bounded delegated source review

Base: `3d1ea2ba996d534c5f3cf19cf9ec477e6be6b736`.
Reviewed source: `a8a34dd067119062971690b1dfaf4e7cefad1b53`.
Unchanged transport composition: `59a441337dd1128d900fbc4966e5c7274e1f538e`.

Status: **Scoped source-approved; hosted execution and fresh CodeQL are required.**
This record does not approve merge or deployment and does not dismiss alerts.

## Scope and evidence

An independent AI review context examined the eight-file transport delta:
the shared Copilot transport and tests, login/provider callers and login tests,
operator module registration, the Bridge CI inventory and browser documentation.
It reported no significant issue in the reviewed changes. The parent composed
that delta and checked that all eight files exactly match the reviewed source.

The investigation started from CodeQL check `104452920964` on public repair
head `8952689bf571d1334f0b0c6d053fbcd936f1daa5`. The implementation owner
read Rust SARIF analysis `1780407125`, associated with merge analysis commit
`feb6eea35e143c0a1eb76f0cd26f78995737d7a5`. Alerts 830 and 831 refer to
`providers.rs`; alert 832 refers to `copilot_login.rs`.

The owner reported URL-string taint imprecision in those flows and a separate,
real lack of HTTP/redirect restrictions in provider clients. Neither that
interpretation nor this source review replaces a fresh CodeQL result.
No alert suppression or gate waiver is introduced.

## T1: New capability or attack surface?

The change introduces a shared, constrained client for existing GitHub Copilot
operations, not a new credential broker, provider, storage type or public route.
Production requests use fixed HTTPS endpoints and do not follow redirects.
Login and provider operations share that boundary.

Loopback fixture transport and custom test certificate handling are test-only.
The committed TLS fixtures generate their material using OpenSSL; no external
certificate or private-key file is needed. Test setup is not production trust
configuration.

## T2: Security-control change?

The boundary rejects HTTP rather than relying only on current HTTPS constants.
Redirects cannot replay device codes or credentials to another endpoint.
The existing grant/store preflight and write-time authority, identity, type and
compare-and-swap checks remain required. The transport repair does not create
credential authority, bypass admission or claim that preflight reserves a write.

Existing device-flow expiration, slowdown and ambiguous persistence outcomes
retain their separate semantics. A successful transport request is not itself
proof that a credential was stored.

## T3: Validation and remaining limits

The implementation owner checked formatting, whitespace, source-level module
and test registration, and certificate generation. The independent review was
source-level. Local Rust compilation, actual test inventory and execution,
Clippy and fresh CodeQL remain unverified.

The source contains nine login tests and six transport tests, including HTTP
rejection, TLS requests, no-redirect behavior, untrusted certificates/hostnames
and device-login redirect behavior. The exact names are required in the
existing CI inventory step. All 39 base guards are preserved; the 13 additions
make 52 on this composition. A later witness integration must preserve its five
additional guards, for at least 57, and its producer/consumer parity step.

The hosted locked-dependency run must actually compile, register and execute
these tests. A source-level registration check is not an executed test result.
The documented local disk floor prevented local Cargo qualification; no
alternative profile or silently reduced test scope was used to claim success.

The repaired transport has not yet been exercised through a fresh live Copilot
authorization on the beta cluster. The original user's authorization attempt
was not observed; these findings are not asserted to be its proven cause.

## Delegation and verdict

### Hosted outcome and test-fixture follow-up

[Bridge CI 35023001697](https://github.com/Azure/kars/actions/runs/35023001697)
at candidate `e6908eb5354cfb8c6b372d2291e57c470f28530b` subsequently passed:
all fifteen Copilot tests actually registered and executed, with locked Clippy.
The three original alerts report fixed on the PR merge ref. However, the
separate CodeQL alert verdict failed a new alert 833 despite successful
analysis jobs; that failure is retained, not waived.

Rust SARIF analysis `1782296326`, merge analysis commit
`3770061a827a2bd4cd185bc830ad49ec4de3673f`, traced the new flow from the
heuristically sensitive variable `oauth` through a test-only `SocketAddr` into
URL formatting. The interpolated data were a validated loopback address and
static operation path, not credentials. The branch was already `cfg(test)`-only;
production fixed HTTPS URLs and credential handling were not involved.

Corrective source `213347e80c140f6a2b4fd19eedaa4436f700b3fc` separates fixture
routing from request URLs: operation-specific localhost URLs are fixed, and the
validated loopback socket is supplied only to client DNS configuration.
Loopback/nonzero-port validation, no proxy, bounded timeout and no redirects
remain. There is no query suppression, configuration exclusion or sensitive
variable rename.

An independent review found no significant issue in that four-file delta.
The added network regression covers IPv4 routing, fixed paths/Host and request
semantics; it has not yet run in the new hosted source. IPv6 routing and proxy
isolation were not runtime-validated by that review. Existing production and
credential-source slices remain unchanged.

The parent composed the exact four files at
`b97e1ad99b2dfc0ace9e897ed6abd87bd2eee148` and verified the CI union:
all 57 existing guards remain, plus the new fixture test, for 58. This yields
sixteen Copilot tests in source. Fresh Rust execution and a passing CodeQL
alert verdict are still mandatory; source-level false-positive analysis is not
a dismissal or successful qualification result.

This uses the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The implementation and independent review were separate AI contexts, not two
human reviewers. The parent owns composition and this audit record.

Verdict: accept the bounded source repair for qualification. All final-head
branch requirements, fresh alert analysis and supported deployment prerequisites
remain mandatory.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
