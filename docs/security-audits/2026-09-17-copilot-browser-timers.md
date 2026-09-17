<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Copilot browser timer receiver correction - delegated source review

Date: 2026-09-17.
Base: `6183b4b80e5d86bd6b37ae67cec41759f301e079`.
Reviewed source: `bb891bb952a4ceebe2060097ae2edc4f77642b31`.
Scope: `bridge/web/src/lib/copilot-login.ts`,
`bridge/web/tests/copilot-login.test.mjs`, and the directly related browser
acceptance section in `bridge/docs/public-beta-helm.md`.

Status: **Source-approved for final-head qualification, not deployed.**

## T1: New capability / attack surface? NO

The default clock now wraps calls to the browser's native `setTimeout` and
`clearTimeout`. Previously those functions were stored unbound and invoked as
clock-object methods, giving the Window APIs an invalid receiver. Real browser
clicks threw `TypeError: Illegal invocation` before a server-action request and
left the UI displaying `Starting...`. A direct successful backend request could
not establish browser functionality.

No endpoint, token, role, provider setting, external destination or dependency
is introduced. No global browser timer replacement ships in production.

## T2: Security-control change? NEUTRAL

Only native timer invocation semantics change. The original expiry, cumulative
poll intervals, generation/cancellation fences, signed session requirement,
provider/seat validation and governed storage confirmation remain unchanged.
An injected test clock remains supported. This does not weaken an error path
or make a cancelled/in-flight authorization appear successful.

## T3: Availability / fail-open risk? REDUCED

The new default-clock regression reproduces the original invalid receiver
instead of supplying the fake clock that masked the defect in earlier tests.
It covers initial code display, polling, cancellation and disposal. It failed
on the old source and passes after both browser timers are wrapped.

All 35 focused login/action/controller contracts passed with cached TypeScript
5.9.3 matching the declared version; no npm installation was performed.
The parent reproduced zero POST requests and the exception in the deployed,
hydrated browser UI. A binding-only diagnostic experiment displayed a code and
returned one successful server action. That attempt was cancelled without
approval or credential storage; no actual codes were printed or retained.

The exact corrected controller was also transpiled and executed in a fresh
Chrome target with unmodified native timers and synthetic test-service replies:
code display, one poll and cancellation worked. This is browser API execution,
not a deployed web-image acceptance result. The isolated diagnostic browser
and its private profile were removed.

## Delegation and verdict

An independent AI context reviewed the exact three-file delta and found no
significant remaining issue. It uses the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306);
it is not represented as a second human review or a waiver of technical gates.

Accept this bounded source for final-head build/image/lifecycle qualification.
The live web image still needs a supported upgrade and genuine browser
acceptance. This review does not qualify the separate protected BFF-template
migration, composer inference or maintenance-team launch.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
