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

The runtime pins Next.js 16.4.1 for the image-optimization and cache-function
remote-code-execution fixes (GHSA-2464-fhr4-w9xw and GHSA-673q-mjm6-r5xr),
along with the related Server Actions and image-optimization fixes. Keep
`eslint-config-next` aligned. The existing sharp override requires 0.35.5 or
later for GHSA-wq5f-xc86-pv6w. Package the current build's traced standalone
dependencies; an older image's `node_modules` can reintroduce vulnerabilities
even when the source lockfile is patched. Dependency installation uses the
configured approved npm registry; production builds and runtime execution
need no package downloads.

## Session recovery

Expired-session RSC navigation and prefetch requests receive a same-origin,
non-cacheable 401 rather than following the document login redirect into the
external identity provider. Next can fall back to document navigation; ordinary
page requests retain the existing login redirect and return destination.
The login endpoint also rejects RSC requests before creating OIDC state, so
background refresh or prefetch cannot overwrite a document login's PKCE/state
cookie. Callback state, nonce and token validation remain unchanged.
Team Pause, Resume, and Edit failures with status 401 offer a plain **Sign in
again** link back to the Team. Unsaved edits remain visible until navigation.
Signing in does not replay a rejected mutation: review and retry the change
explicitly. Permission denials (403) remain permission errors, not login loops.

Same-origin RSC GETs use fetch `keepalive` through Next's client instrumentation
entry point. Firefox otherwise cancels an in-flight refresh before `pagehide`,
and Next 16.3.6 can navigate back to the old page while handling that cancellation.
This changes only the lifetime of those finite reads; API calls, mutations,
credentials, explicit cancellation and real error responses are unchanged.
Remove the workaround when the framework handles document-unload cancellation
without replacing the destination, retaining reload/hard-navigation regression
coverage alongside ordinary navigation and session recovery.

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

Troubleshooting routes sandbox warnings to the sandbox inventory and pod warnings
to the configured Kubernetes dashboard's pod details, preserving its SSO flow.
Without a usable dashboard destination, it gives administrator guidance instead
of sending pod names to an empty sandbox search. Opening diagnostics grants no
additional Kubernetes access.

## SRE action review

The SRE Actions console shows each persisted proposal's namespace/name and exact
JSON action parameters separately from the agent's diagnosis and rationale.
Operators can review replica counts, image/container changes, and target scope
before approving; null values are preserved and missing parameters are explicitly
unavailable, not defaulted. Bridge records the decision only. The controller owns
the single execution attempt, short-lived scoped credentials, temporary permission
cleanup, and execution outcome.

## Mission composition

The reviewed loop's goal, success criteria and context are sent to the composer
alongside the plain intent, so route, scheduling and budget proposals can reflect
those requirements. They also remain in the execution instructions, while the
Mission objective, title and slug retain the plain intent. Back-navigation keeps
the reviewed context; skipping the loop removes it from composition and execution.
Composition still does not authorize launch or waive pre-flight validation.

## Activity evidence references

The execution graph reports available evidence references, not verified controls.
An envelope digest or DID does not prove a signature was checked, and a
non-principal graph entry may be a planned phase or observed producer rather than
a spawned sandbox. Receipt presence is distinct from signature verification;
use the Auditor receipt-verification tools for cryptographic checks. Missing
references do not establish success, failure, or that a run is historical.
Team run views use the run's own envelope digest, never the mutable Team digest
as a substitute.

## Mission execution presentation

Mission detail, Execution, and the journey distinguish the principal's controller
phase from revision-bound reviewed child attempts. An active reviewed attempt is
Running even while the principal remains Launching. Retained completed phases
keep the journey at Run during handoff gaps, shown as Awaiting next phase rather
than Deploying; they never establish Mission delivery. Current root results,
failed phases, held/uncertain execution, and stop state take precedence over
active children. Missing or stale reviewed evidence cannot assert progress.
Inconsistent current snapshots show Execution evidence unavailable and keep
polling rather than claiming the agent is coming online again.
This projection is display-only: readiness and dispatch controls still use the
principal's actual state. Mission list and workspace-home cards use the same
projection through the summary DTO's compact `reviewed_progress` and current run
nonce. The BFF reuses detail's reservation, ownership, attempt and result checks,
but reuses already-listed outputs and never loads artifacts or tool traces for
summaries. Reviewed reads share a 256-GET / five-second budget across the entire
list (newest first, at most 64 roles and 64 phases per mission); exhausted or
changed snapshots explicitly report evidence unavailable, not Deploying or
Running. A final task-list read fences authorization, replacement and reruns.
List filters/counts keep waiting/stopping under Active, unavailable evidence in
its own filter, and stopped runs separate from drafts. This requires the matching
BFF summary DTO; frontend-only deployment cannot supply missing child evidence.

Team and Mission startup signals share the Activity explorer's revision-pinned
stream, including live principal and reviewed-worker router observations. Mission
Overview uses the same validated stream hook. Disconnected or replaced live
observations are cleared rather than retained as current progress. Observed work
does not establish sandbox readiness, access verification, or successful delivery;
those signals and terminal evidence remain separate.

## Deliverable links

Shared Mission and Team markdown links resolve only against the selected run's
captured artifacts and download with its `run_nonce`. Native `exports/<filename>`
links also resolve when the capture has a flat filename without source-path
metadata. Explicit source paths are not overridden, arbitrary directories are
not reduced to basenames, and ambiguous or unbound export links stay unavailable.

## Mission qualification runs

New Mission and Team qualification choices require both an operator/admin role
and `/api/options` reporting `per_run_qualification_available: true`. Required
mode offers the choice; global validation mode, invalid mode configuration and
missing options metadata do not. This is presentation metadata, not an admission
grant: the BFF still checks designation requests independently. Both composers
preserve safe package-validation rejections instead of labelling them orchestrator
outages; unexpected and upstream failures retain generic fallback messages.

Operators and administrators can explicitly select **Qualification run** while
composing or reviewing a new Mission, including after choosing manual review when
composition is unavailable. The default is off. This designates one reviewed run
for onboarding a configured harness/provider; it is not a global testing window
and does not certify that route as qualified. Standing Teams are unchanged.
Changing the selection clears previous validation and the launch selection.

The Mission client passes the optional `qualification_run` boolean outside the
blueprint to compose, package validation (`/api/namespaces/{ns}/validate`), and
creation. Server actions check the signed principal's role independently of UI
visibility; the BFF remains authoritative. The compose response does not own this
selection, so manual-review fallback preserves it. Only prior generic route
evidence may be waived: security, resources, approvals and finite budgets remain
binding to the reviewed package.

Mission detail and Activity retain the approver, subject, approval time,
reviewed-package digest and admission deadline from the saved task. The deadline
is 45 minutes after create approval, not a cancellation timer for admitted work.
Unlaunched drafts still use the existing **Launch** action with fresh server
validation; the server decides whether admission is still permitted. Designated
Missions cannot be replicated, revised or run again as normal Missions. Another
attempt requires a newly reviewed, explicitly designated Mission. An unfinished
durable request with a recorded run nonce can still be resumed or awaited after
admission expiry; **Resume / await run** uses the existing run endpoint, whose
server authorization decides whether work can continue. A stopped pending request
still needs fresh server validation before its sandbox can be launched again.
Completed attempts never expose that resume control. Existing eligible output
approvals and sandbox cleanup remain available.

## Audit dates

Receipt summary dates use the same explicit English locale and UTC timezone on
server and browser, avoiding hydration mismatches and timezone-dependent day
changes. The visible date is labelled UTC; the full recorded timestamp remains
available in the tooltip and semantic `time` element on both audit surfaces.
Team activity ledgers likewise use the shared UTC evidence formatter and explicit
English token grouping, so browser locale/timezone changes do not replace their
server-rendered rows. Unusable timestamps display `Time unavailable`.

## Team profile drafts

Workspace → Teams lists profiles from the current workspace. **Use <profile>**
opens the existing profile-backed composer; only controller-Ready profiles are
selectable. Inventory failures remain visible without hiding existing teams or
charter-based creation. Selecting a profile does not create or launch a Team.

The launch editor preserves an existing model route if it is absent from the
current options catalog, including when loading the catalog fails. The retained
route is shown explicitly; saving another setting must not silently clear it.
Selecting `cluster default` explicitly clears the assignment. Backend route
validation remains authoritative; retaining a route does not qualify it for use.

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
changes invalidate the previous check. Preflight sends an explicit delegation depth
unchanged and checks it against the backend's reviewed Team activation contract;
an omitted depth uses the same server default as creation. An insufficient depth
is reported before launch without raising the grant. Paused drafts remain
editable, and activation still independently enforces the reviewed depth.

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

The lower Team Execution flow consumes the shared Activity explorer's current
principal and reviewed-phase observations, including live and stopped evidence;
it does not open a second stream. It reuses the shared action descriptions and
newest-first timestamp/producer/sequence ordering. Tool arguments, proposal-only
status, reported outcomes, producer identity and coverage gaps remain visible.
Observed round and token counts describe only returned records, not complete run
totals; missing/partial token usage is never filled with zero. Terminal totals
remain separately qualified by current-revision evidence.

Reviewed runs also expose their controller-reserved role and phase tree. Bridge
checks the pinned state, root revision and plan, each child UID/specification and
owner, and the current committed phase terminal. The flow shows those phases'
actual start/finish times and links to their own evidence and files; it does not
manufacture assignment-ledger events or attach child files to the root download.
Every phase must complete before its role is shown as completed. Aggregate token
usage includes the principal and phases only when all those totals are known.
Changed or unavailable reservations are reported explicitly.

The graph and drill-down use the principal's and each phase's revision-bound
router observations. Nodes and inspectors show runtime (OpenClaw or Hermes)
separately from model. Runtime comes from the custody-validated retained attempt,
including live progress, never today's roster or a model-name guess. Historical
records without that metadata remain explicitly unknown; runtime labels do not
change producer identity or observation coverage.
Selecting a producer or action filters that producer's
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
