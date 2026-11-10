# Security Audit — retired URLhaus feed, opt-in extra blocklist feeds

Date: 2026-11-09
Scope: `inference-router/src/blocklist.rs`, `inference-router/src/routes/mod.rs`,
`controller/src/reconciler/mod.rs`, `controller/src/reconciler/tests.rs`,
`controller/src/config_hash.rs`, `.github/workflows/blocklist-refresh.yml`,
`docs/egress-proxy.md`, `docs/security-validation.md`, related doc mentions.
Gated paths: `inference-router/src/routes/mod.rs`.

## Summary

abuse.ch retired the keyless URLhaus downloads on 2026-10-21; the hostfile URL
answers `410 Gone`, so every refresh logged a failure and the live blocklist
had been OISD plus the seed file since then. This change removes the dead feed
from the router, the refresh CronJob and the seed workflow, and adds an opt-in
`BLOCKLIST_EXTRA_FEEDS` setting (comma-separated `hosts:<https-url>` /
`domains:<https-url>`, empty by default) so operators can add feeds of their
own choice. The gated change in `routes/mod.rs` only parses that setting at
router startup and passes the accepted feeds to the existing refresh task.

## T1: New capability / attack surface? (YES, opt-in, bounded)

- With the setting unset (the default) the router fetches only OISD, as
  before; no new endpoint, credential, Kubernetes permission or runtime path
  is added.
- When an operator sets it, the router makes outbound HTTPS GET requests to
  up to 8 operator-chosen URLs on the existing refresh cycle. The value comes
  from the controller environment (Helm `controller.extraEnv`), which only a
  cluster operator can change; it is not settable from a sandbox, a CRD field
  or a request.
- Entries are validated at startup: `https` scheme only, a host is required,
  no user info or password in the URL, duplicates dropped, at most 8 feeds.
  Invalid entries are logged by position (never by URL) and skipped.
- Redirects are refused, and a response from any URL other than the
  configured one is an error, so a feed cannot be bounced to plain HTTP or to
  another host.

## T2: Security-control change? (NEUTRAL to STRENGTHENED)

- Feed entries are deny-only: nothing in a feed can allow a domain or weaken
  other controls. A successful refresh replaces that source's last-good set,
  so a feed update can add or remove deny entries; it cannot turn a deny into
  an allow.
- Fetch errors never carry the feed URL (`reqwest::Error::without_url()`),
  and logs name a feed by host only, so a token in a feed query string does
  not reach the logs (test `test_fetch_error_does_not_log_secret_query`).
- The controller forwards the value verbatim and lists
  `BLOCKLIST_EXTRA_FEEDS` as a config-hash input, so a change rolls the
  routers instead of leaving them on stale configuration.
- The refresh CronJob and the seed ConfigMap stay OISD-only (1 MiB
  ConfigMap limit).

## T3: Availability / fail-open risk? (REDUCED)

- Every HTTP feed (OISD and each extra feed) has a 60 s timeout and a 50 MB
  cap enforced while the body streams (a larger declared `Content-Length` is
  refused up front). The seed source is a local file from the mounted
  ConfigMap, read without a network fetch; its size is bounded by the
  Kubernetes ConfigMap limit (1 MiB), not by these HTTP bounds.
- Every source, the seed file included, keeps at most 500,000 domains, and
  `MAX_TOTAL_DOMAINS` (1,000,000) bounds all sources together, so a hostile
  or broken feed cannot exhaust router memory.
- Each source keeps the entries of its last successful load, so a failing
  feed (including OISD) no longer empties what it supplied before; a source
  that never loaded adds nothing and is logged.
- Removing the 410 feed ends a refresh failure on every cycle.

## Verification

- New unit tests: extra-feed parsing (unset, accepted, rejected, duplicates
  and count cap), format dispatch, fetch-error redaction, redirect refusal,
  declared-length and streamed-body caps, last-known-good entries of a failed
  feed, and the router env built by the controller.
- CI on b1dac06: Rust Build & Test (fmt, clippy `-D warnings`, tests), E2E
  (Kind), Chaos tier and the other gates pass; only this gate failed, which
  this report addresses.

## Verdict

Accept. The default behaviour is OISD only; the new outbound fetches exist only
when a cluster operator configures them, are HTTPS-only, non-redirecting,
size- and count-capped, and can only add domains to the deny set.

Signed-off-by: Slava Olesik <looksof@gmail.com>
