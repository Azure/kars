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
GitHub `slow_down` responses increase the delay by at least five seconds,
cumulatively; the advertised interval is also respected. A delay above 900
seconds or an HTTP rate-limit response stops polling with guidance rather than
retrying indefinitely. See [GitHub's device-flow rate limits](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#rate-limits-for-the-device-flow).
Unexpected JSON, HTTP failures, and transport rejections are errors, not approval
pending. Only an explicit `authorization_pending` reason shows “Waiting for approval.”
Both the web-to-BFF device requests and BFF-to-GitHub requests reject redirects
instead of forwarding a credential-bearing request to another endpoint.

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
cargo test copilot_login
```

`npm run lint` is configured but currently reports a known private-preview
React-rule backlog. It is not a green release gate yet.

Do not restore the create-next-app boilerplate or deploy this application
directly to Vercel. It is designed to run next to the BFF and a Kars cluster.
