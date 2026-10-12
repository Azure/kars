<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Kars Bridge web

The Next.js application provides three persona-scoped products:

- `/workspace` for employees;
- `/console` for operators and administrators;
- `/audit` for auditors.

The browser calls same-origin `/api/*` routes. The Next.js server proxies those
requests to the Rust BFF using `BRIDGE_BFF_URL`; browser code never receives a
Kubernetes credential.

## Development

```bash
npm ci
npm run dev
```

Required integration configuration:

| Variable | Purpose |
|---|---|
| `BRIDGE_BFF_URL` | Server-side BFF origin |
| `BRIDGE_OIDC_ISSUER` | Public OIDC issuer (also used to verify ID tokens) |
| `BRIDGE_OIDC_BACKCHANNEL_ISSUER` | Optional internal issuer base for discovery, token exchange and JWKS; automatically wired for bundled Dex by Helm |
| `BRIDGE_OIDC_CLIENT_ID` | OIDC client |
| `BRIDGE_OIDC_CLIENT_SECRET` | OIDC client secret |
| `BRIDGE_SESSION_SECRET` | Signs Bridge sessions |

Use repository-level `make dev` to run the BFF and web application together.

## Session recovery

Expired-session RSC navigation and prefetch requests receive a same-origin,
non-cacheable 401 rather than following the document login redirect into the
external identity provider. Next can fall back to document navigation; ordinary
page requests retain the existing login redirect and return destination.
Team Pause, Resume, and Edit failures with status 401 offer a plain **Sign in
again** link back to the Team. Unsaved edits remain visible until navigation.
Signing in does not replay a rejected mutation: review and retry the change
explicitly. Permission denials (403) remain permission errors, not login loops.

## Operator configuration

Configuration and Agent capabilities must load their backing resources before
offering changes. An API failure is not an empty catalogue or an unconfigured
integration: the page shows an explicit error and retry guidance. Integration
stores require the existing reviewed, UID-bound credential grant; neither the
page nor its error recovery grants broader Secret access.

The provider wizard validates connection URLs and provider tags before advancing.
Switching provider kinds clears entered credentials and model selections.
AI Runway's missing API is an unavailable optional capability, while permission
and transport failures remain errors rather than appearing as an empty cluster.

## Team profile drafts

Using a Ready profile copies its supported settings into an editable, paused-by-default
Team draft; later profile edits do not update that draft. Unsupported envelope
constraints or unavailable skills block copying rather than being silently omitted.
Scoped positive token and optional micro-USD budgets are copied into the lifetime
budget controls and sent unchanged to preflight and creation. A profile's
`toolPolicyRef` selects and pins that same policy in the Team authority envelope;
missing policies or conflicting profile policy settings block copying. Explicit
pinning never substitutes another policy when the selected policy is unavailable.
Reserved egress-allowlist references and unknown envelope fields remain rejected
because the Team controller cannot enforce them. Draft controls remain editable;
copying a template is not authorization to launch or renew a model contract.
A profile does not supply a typed execution plan. The plan JSON editor remains
available for manual authoring, including after its contents are cleared. JSON
syntax is not execution approval: the BFF validates the complete plan and exact
roster match, and launch requires a fresh package check. Plan, roster or budget
changes invalidate the previous check.

The org chart shows the controller's default member tier, bounded by the Team's
explicit authority ceiling. Zero delegation depth is displayed as disabled;
neither the caption nor profile copying increases that depth.
Controller rejection takes precedence over paused/hibernating presentation on
the Team detail, Team list and Workspace home. The detail shows the controller's
reported constraint; pausing or resuming does not repair an invalid envelope.
Unset member model/harness fields select Team defaults, not a separate cluster
route. Saving a partial member route binds its missing model or harness to the
reviewed Team route (including inherited model fallbacks for a harness-only
override). Explicit member routes remain explicit; this does not copy omitted
network or tool capabilities into a member override.

## Team activity evidence

Team summaries distinguish unavailable evidence from measured zero. Token usage
requires an explicitly known, bounded total from the current revision; a genuine
zero remains visible. Execution flow filters activity and assignment events to
that same revision. Missing role-selection records do not turn the standing
roster into selected or running workers.

Reviewed runs also expose their controller-reserved role and phase tree. Bridge
checks the pinned state, root revision and plan, each child UID/specification and
owner, and the current committed phase terminal. The flow shows those phases'
actual start/finish times and links to their own evidence and files; it does not
manufacture assignment-ledger events or attach child files to the root download.
Every phase must complete before its role is shown as completed. Aggregate token
usage includes the principal and phases only when all those totals are known.
Changed or unavailable reservations are reported explicitly.

The graph and drill-down use the principal's and each phase's revision-bound
router observations. Selecting a producer or action filters that producer's
records and links to its own evidence download. The timeline includes recorded
phase starts/completions and router observation timestamps. These timestamps
identify when the router recorded an observation, not exact tool start times.
Older untimestamped records remain readable in each producer's sequence order;
their missing timing is not reconstructed. Completed filesystem calls report
success/failure, duration, byte counts and an explicitly exported artifact name
through the existing router endpoint. Raw arguments, document contents and
credentials are not copied into telemetry. Model proposals and harness-reported
results remain distinct; reported result counts are not independent proof of
successful tool execution. These fields travel through the maintained runtime,
router and Bridge builds; no live-only configuration is required.

Counts describe published records, not unpublished work. Missing records do not
prove inactivity or that a run is historical.
Transport errors and unavailable SSE observations clear the provisional live
graph without discarding retained activity. Native EventSource reconnection
keeps the existing producer and progress pins; opening a connection alone does
not restore observations. Only a validated snapshot can do that. Explicit
completion, revocation, and end frames still close the stream.
Artifacts alone do not establish role handbacks or delivery.
Failed runs expose their retained failure output without inferring a confident
narrative or diagnosing context replay from token ratios. Missing role handbacks
are reported only when current role-lifecycle evidence is available.
Both `failed` and `rejected` runtime terminals are failures, as is legacy `error`.
The separate runtime error is retained alongside any partial output; none is an
accepted deliverable.

Focused checks:

```bash
node --test tests/team-run-evidence.test.mjs tests/mission-run-evidence.test.mjs tests/team-run-links.test.mjs
```

## Build check

```bash
npm run build
```

## Container build

From the repository root:

```bash
docker build -f bridge/web/Dockerfile -t kars-bridge-web:latest bridge/web
```

If your network requires an npm mirror or authenticated registry, supply its
configuration as an optional BuildKit secret:

```bash
docker build --secret id=npmrc,src="$HOME/.npmrc" \
  -f bridge/web/Dockerfile -t kars-bridge-web:latest bridge/web
```

The secret is mounted only for dependency installation; it is not copied into
an image layer. Without it, npm uses its default configuration. Lockfile versions
and integrity checks are unchanged. Do not pass registry credentials as build
arguments or copy them into the build context.

Geist Sans and Geist Mono are self-hosted from the locked `geist` npm package
(SIL Open Font License), using its `next/font/local` loaders and bundled variable
WOFF2 files. The existing `--font-geist-sans` and `--font-geist-mono` variables
are preserved. Production compilation runs with networking disabled after
installing dependencies; it does not fetch fonts from Google. The final image
serves the generated font assets locally.

The `sharp` override requires at least 0.35.5 within the 0.35 series, including
its patched prebuilt librsvg dependency (GHSA-wq5f-xc86-pv6w). Keep its native
packages synchronized through npm rather than editing lockfile integrity data.
Qualify the final image with real image processing and a fresh vulnerability
scan; a successful Next.js build alone is not runtime acceptance.

## Copilot device sign-in

The provider wizard checks the workspace credential grant and enrolled provider
store before requesting a device code, and again before every OAuth token
exchange. These are readiness checks, not a reservation: actual persistence
still enforces the current grant generation, namespace/store UIDs, store type,
and optimistic concurrency. A late storage failure is reported as unconfirmed,
never authorized; an operator must inspect provider state before another sign-in.
There is no token escrow or automatic resume after token consumption.

Polling is serial and stops at the original expiry even if a request hangs.
If a persistence-capable poll is still in flight at that deadline, the UI reports
an unconfirmed outcome and directs the operator to inspect provider state before
retrying. Late responses cannot replace that warning or authorize the UI. A
deadline with no poll in flight, or an explicit upstream expiry received before
the deadline, remains definitive expiry.
GitHub `slow_down` responses increase the delay by at least five seconds,
cumulatively; the advertised interval is also respected. A delay above 900
seconds or an HTTP rate-limit response stops polling with guidance rather than
retrying indefinitely. See [GitHub's device-flow rate limits](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#rate-limits-for-the-device-flow).
Unexpected JSON, HTTP failures, and transport rejections are errors, not approval
pending. Only an explicit `authorization_pending` reason shows “Waiting for approval.”
Both the web-to-BFF device requests and BFF-to-GitHub requests reject redirects
instead of forwarding a credential-bearing request to another endpoint.
Device-code creation, token polling, Copilot eligibility and the Copilot model
catalog now share one BFF transport. Its production client enforces HTTPS-only
requests with normal certificate/hostname verification and never follows any
redirect, including same-origin redirects. Destinations are fixed GitHub HTTPS
URLs selected by operation, not strings from a browser, environment variable or
upstream response. This also protects eligibility/catalog calls outside device
sign-in. Other providers and credential grant/UID/CAS checks are unchanged.

The HTTP fixture override is compiled only under `cfg(test)`, accepts only a
literal loopback IP and nonzero port, disables proxies, and still rejects
redirects. Its URLs use the fixed loopback hostname `localhost`; the validated
loopback socket is pinned in reqwest's DNS configuration when the fixture
client is built, never interpolated into request URLs from the OAuth client.
TLS regressions exercise the production HTTPS/redirect policy with
test-only DNS overrides and an explicitly trusted, freshly generated test
certificate. They require the existing OpenSSL executable; private keys stay
in memory, and no test credentials are sent to real GitHub endpoints.

The three CodeQL alerts on [PR #566](https://github.com/Azure/kars/pull/566)
([830](https://github.com/Azure/kars/security/code-scanning/830),
[831](https://github.com/Azure/kars/security/code-scanning/831),
[832](https://github.com/Azure/kars/security/code-scanning/832)) traced
`oauth.seat`, `oauth.models`, and `oauth.start`/`oauth.poll` into reqwest's URL
argument, not credentials into URLs. At head `8952689b`, those fields were
fixed HTTPS URLs in production and HTTP URLs only in unit tests. Device codes
were in JSON POST bodies, and tokens were in authorization headers. That
reported URL dataflow was a false positive, but the separate eligibility/catalog
client builders did have a genuine transport-policy gap: reqwest 0.12.28
defaults to HTTP allowed and up to ten redirects. Its sensitive-header
stripping checks host and effective port, not scheme; it is not an HTTPS
boundary. The shared transport closes that gap without suppressing the alerts.
The original paths can be reproduced read-only from the check's SARIF:

```bash
gh api --method GET -H 'Accept: application/sarif+json' \
  repos/Azure/kars/code-scanning/analyses/1780407125 \
  --jq '.runs[].results[] | {ruleId, locations, codeFlows}'
```

At head `e6908eb5`, those three alerts are fixed, but check `104563393079`
reports [alert 833](https://github.com/Azure/kars/security/code-scanning/833)
at the test-only HTTP URL formatting expression in `copilot_transport.rs:65`.
Its four SARIF paths start at the `oauth` variable, traverse the client and its
loopback-address field, and end at reqwest's URL argument. No token, device
code or response body enters that expression: it combines a validated
loopback socket with a fixed operation path, and is excluded from production
by `cfg(test)`. This is a URL-dataflow false positive, not a logging/panic sink.
The clarification separates fixture socket routing from fixed request URLs;
it does not rename sensitive identifiers or suppress analysis.

The exact evidence is SARIF analysis `1782296326` on merge commit
`3770061a827a2bd4cd185bc830ad49ec4de3673f`. CodeQL CLI `2.27.0` used
`codeql/rust-queries` `0.1.42` and `codeql/rust-all` `0.2.21`, both at source
`c6baf479093fafc81d4655dc2014dc583360308e`. That version's
`SensitiveVariableAccess` applies the shared `oauth` name heuristic; its
reqwest model marks `Client::request`'s URL argument as a `request-url` sink.
The same read-only SARIF command above with analysis ID `1782296326` reproduces
all four paths. The successful analysis workflow is not alert closure.

The additive poll contract is `{status:"pending", interval, reason}`, where
`reason` is `authorization_pending` or `slow_down`; the next request carries the
current `interval`. Older callers can omit the request interval (default five
seconds). This web client accepts older `{status:"pending"}` responses, retaining
its current delay and displaying “Checking sign-in status” rather than asserting
that approval is still needed. Deploy the BFF and web fix together: older web
clients do not understand backoff metadata.

Cancel, replacement and unmount stop future polls and ignore late UI callbacks;
they cannot revoke GitHub approval or undo a BFF write already in flight. Device
codes live only in the current UI attempt and POST bodies, never URLs. Refreshing
loses that in-memory attempt; expired codes require a new sign-in. These known
source defects do not establish the cause of any particular live attempt without
observing its redacted poll status.

Regression checks (shared synthetic producer/consumer fixtures in
`bridge/contracts/copilot-login.json`):

```bash
node --test tests/*.test.mjs
# From bridge/bff:
cargo test --locked routes::operator::copilot
```

Bridge CI requires all fifteen previously registered Copilot regressions plus
the fixed-URL/pinned-socket fixture regression to appear in `cargo test -- --list`.
The fifteen tests and locked Rust/clippy gates passed on `e6908eb5`; that result
does not qualify the subsequent fixture-boundary clarification. Fresh hosted
Rust/clippy and CodeQL results are required for the new source, with all existing
guards preserved; formatting/source checks do not substitute for those gates.

`npm run lint` is configured but currently reports a known private-preview
React-rule backlog. It is not a green release gate yet.

Do not restore the create-next-app boilerplate or deploy this application
directly to Vercel. It is designed to run next to the BFF and a Kars cluster.
