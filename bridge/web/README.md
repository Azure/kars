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
| `BRIDGE_OIDC_ISSUER` | OIDC issuer |
| `BRIDGE_OIDC_CLIENT_ID` | OIDC client |
| `BRIDGE_OIDC_CLIENT_SECRET` | OIDC client secret |
| `BRIDGE_SESSION_SECRET` | Signs Bridge sessions |

Use repository-level `make dev` to run the BFF and web application together.

## Build check

```bash
npm run build
```

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
