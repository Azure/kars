<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Genuine pinned OpenClaw host verification

This opt-in test starts the **actual installed OpenClaw gateway**. Its ordinary
plugin loader supplies `PluginRuntime`, starts a registered service, and dispatches
production `createNativeMissionExecutor` tools through `runtime.agent.runEmbeddedAgent`.
There are no native runtime, hook, tool, filesystem, session, or policy-function doubles.
Router/model **HTTP boundaries** and vendor HTTPS content are deterministic fixtures.
The HTTP model fixture emits a
deterministic sequence of decisions; the real OpenClaw engine must execute them and
return each real tool result before it receives the next decision.

## Requirements

- An **already-built, fully installed** OpenClaw **2026.9.8**, commit
  `fc23bc864e4553c2d215e479eeec47b67a0bf943`. Set `OPENCLAW_NATIVE_HOST_ROOT` to its
  package directory containing `openclaw.mjs`, `dist`, and installed dependencies.
  Actual `dist/build-info.json` must contain that version and full commit. Present
  build/postbuild stamps must agree. An asserted environment commit is not accepted.
- **Node >=24.16.0 <25 or >=26.1.0**, with the pinned host's SQLite capability probe
  passing. This is stricter than this repository's general Node requirement.
  The runner refuses unsupported Node, unfinished package lifecycle, or missing
  build artifacts instead of installing, building, or recovering them.
- A writable, physical, non-symlink checkout under **`/sandbox/`**, including these
  tests and, in source mode, `src/core`, `src/core/task-filesystem.py`, and the
  current `npm run build` output in `dist/core` for the pre-gateway initializer. Installed
  mode uses the candidate's `/opt/kars-plugin/core` instead. The actual file helper uses
  **`/usr/bin/python3`** and descriptor-relative filesystem checks. No `/tmp` path
  or filesystem override is used.
- The runtime's existing dependencies, including resolvable **`@kars/mesh` with
  its native-phase distribution already built**. OpenClaw's normal TypeScript
  plugin loader loads the source fixture. No additional runner dependencies are
  required: the runner uses Node's built-in test library, not Vitest or tsx.
- Permission to launch the pinned local host, bind ephemeral loopback ports and
  the sandbox proxy port **8444**. Provide `KARS_NATIVE_HOST_TLS_DIR` containing a
  disposable local `cert.pem` and `key.pem` for DNS name `native-host.invalid`.
  The child trusts only that additional test CA through `NODE_EXTRA_CA_CERTS`;
  TLS verification stays enabled. The test proxy rejects every CONNECT with the
  finite-governed router's 403 response. Native retrieval must then use the registered
  `kars-router` provider and `/egress/fetch`; the HTTP router fixture maps only the
  exact authorized URL to local TLS with certificate verification. No public DNS or
  external service is contacted.
  This test does not execute in, build, pull, or configure containers itself.
  The operator owns the authorized execution environment.

## Run serially in the prepared environment

```sh
cd /sandbox/kars/runtimes/openclaw
OPENCLAW_NATIVE_HOST_ROOT=/path/to/already-installed/openclaw \
  KARS_NATIVE_HOST_TLS_DIR=/path/to/disposable-test-ca \
  node --test --test-concurrency=1 tests/native-host/native-host.test.mjs
```

The test fails, rather than skips or falls back to doubles, when prerequisites or
native integration are missing. Host execution is bounded at 120 seconds, with a
180-second outer test deadline. Only its own child is signalled on shutdown.
A fresh `.runs/<uuid>/` stores its assignment, isolated configuration/state/home,
real input/output files, `receipt.json`, `http-evidence.json`, and host logs for
inspection. These generated files are ignored by Git and retained as evidence.
The test does not reuse user configuration, credentials, proxy settings, plugins,
or sessions. Its fixture explicitly grants `hooks.allowConversationAccess` because
the real non-bundled-plugin `agent_end` hook requires that permission. It also grants
`hooks.allowPromptInjection`, matching the installed Kars plugin, and registers the
production configured-identity prompt hook. Native Mission sessions add that identity
without replacing the host-generated phase, tool, and required-output instructions;
ordinary sessions retain configured-system-prompt replacement behavior.

## Assertions and scope

- Execute the production pre-gateway capacity initializer against the router's
  `/v1/models` HTTP boundary. It replaces stale 32K context and 1K output metadata
  before OpenClaw captures its prepared generation; the pinned runner ignores
  per-run configuration replacements. The native executor verifies that the
  prepared capacity still matches the router before inference. The actual pinned
  provider request must use the governed 128K output maximum.
- Execute the production entrypoint's provider/auth initialization against the
  isolated state directory before starting the host. Installed mode reads the
  packaged `/usr/local/bin/entrypoint.sh`; source mode reads the repository file.
  The test changes only the model HTTP destination and model metadata for its
  fixture. Provider auth settings remain production settings. In particular,
  OpenClaw 2026.9.8 must run without legacy auth-profile seeds that require
  credential migration; the loopback provider already supplies its routing marker.
  `bootstrap-sha256.txt` records the exact entrypoint tested.
- Real UTF-8 file read and write, actual output-file reread, and exact `report.md`
  attachment bytes, not a fabricated tool response or an acceptance claim.
- A governed GET returns an upstream 503. Its actual status and response body reach the next native
  model request. The model requests another GET, which succeeds, before writing
  the report. The native `web_fetch` is then denied before any CONNECT or HTTPS
  request. A separately authorized native invocation extracts a large HTML page,
  excludes scripts, wraps untrusted content, and spills full extracted content
  to an actual local file. The next model request receives bounded content, not
  the raw page. Exact input copying rejects a wrong digest before accepting the
  correct digest. Eight tool attempts, five successes.
- The public `before_agent_finalize` hook requests one revision of an initial
  text-only draft, before tools run. The host correctly refuses revisions after
  potential side effects; the fixture never bypasses that safeguard. The real
  pinned host must start a second attempt and reuse
  its first call ID; no runtime or lifecycle event is fabricated by the test.
- Actual model streaming usage totals across both attempts: **1055 prompt / 155
  completion / 1210 total** tokens, including the provider parser's cached-input
  accounting. Ten native start events, nine distinct call IDs, two finalization
  snapshots and one successful end must corroborate all ten rounds. The host
  suppresses the rejected draft's `agent_end` and removes it from terminal history;
  its measured usage must still be included via the observation-only finalization hook.
  This exercises an explicit finalization revision, not every provider retry or
  context-compaction recovery path.
- Exact model-visible names on `/agt/evaluate`:
  `tool:kars_mission_file_read`, `tool:kars_mission_file_write`,
  `tool:kars_mission_http_fetch`, and `tool:web_fetch`. Authorization matches genuine tool-call IDs,
  assignment identity, native run/session IDs, router-issued scope, and model.
- Router provider `named-router-verification-provider` deliberately differs from
  native transport provider `azure-openai`; authorization must preserve both via
  `router_provider` and `native_provider`.
- `/telemetry/cursor` starts at a nonzero cursor (40). Correlation requests to
  `/telemetry/trace?since=...` advance to just before the previously matched proposing
  round, retaining sibling-call context; Activity export still starts at 40. The
  fixture checks this exact advancement and rejects cursors outside its observed
  high-water mark or missing the proposing round. Responses contain
  `{scope_id,cursor,high_water,has_more,dropped_events,coverage,durable,events}`,
  with `coverage:"router-observed-only"`. Requests send `x-kars-service-scope`;
  `x-kars-telemetry-scope` is an inference-response header, not the trace request
  header. The HTTP fixture rejects a missing or stale service scope with 409,
  matching the router. **No `truncated` field** is returned by that cursor API. Interleaved unrelated calls share scope/model and are never
  authorized or attributed to this Mission.
- `/telemetry/tool` is accepted only after matching authorization, once per call.
  Read summary: `file_read` with real bytes/returned_bytes/truncated. Write summary:
  `file_write` with actual byte count and artifact_name. Failed GET summary:
  `failure` / `tool_execution_failed`. Successful GET has no invented typed summary.
- Negotiated live Activity and the reserved `kars-router-observations.json`
  attachment contain only individually correlated tool proposals, their proposing
  rounds, and genuine immediate harness reports. Coverage is explicitly
  `native-tool-correlated-only`, with a correlated-tool count rather than an
  invented returned-response count. The final text-only round and interleaved
  unrelated calls are excluded. The policy-denied proposal has no fabricated
  completion. No raw arguments, file contents or URLs are exported. The ordinary
  attachment remains the actual report. These are runtime-forwarded observations,
  not independent attestation or complete inference-round coverage.

This is a real **native-host integration** test, not a real-router, controller
assignment/replay, deployed-image, or business-acceptance test. The operator's
2026-10-17 r9 source-overlay run passed the earlier five-round file/raw-GET fixture on the immutable
AL3 / OpenClaw 2026.9.8 / Node 24.21.0 host, with networking disabled and real
filesystem confinement. The subsequent `openclaw-native-packaged-20261017-r1`
run passed the same assertions against immutable candidate
`sha256:7bc708b1d5fdb11e1907a8e247026c5d4eeff6b34bd7f5c9b752434cc5d3e47c`,
using only its compiled adapter, helper and installed mesh dependencies. Per-file
hashes recorded the exact adapter bytes; no production source or mesh overlay was
mounted. That candidate predates the native fetch response-transparency repair;
its prior fixture checked failure visibility rather than the actual 503 response
body. The updated fixture requires both status and body. Native fetch now exposes
bounded upstream responses (including errors, redirects, empty and truncated
content); only 2xx responses count as successful and truncated responses cannot
satisfy complete-fetch obligations. The router's existing 2 MiB body cap and its
truncation annotation are preserved, not enlarged. Historical reviewed execution
retains its original complete-page requirements. This is text fetch, not a
binary-download or unrestricted-shell claim. Installed customer acceptance remains
a separate requirement. Earlier failed attempts remain retained.

The updated seven-round native-web fixture passed in source mode
(`native-web-host-20261017-r3`) and compiled installed-adapter mode
(`native-web-packaged-20261017-r2`) on 2026-10-17. The latter tested immutable
candidate `sha256:96f85fd8aec516561e2ec29d1fd24dea3200384a5b2fa2e5c8e5706767fba11f`
without a production source overlay, including the packaged entrypoint, actual
TLS proxy routing, denied network effects, and the native spill file's contents.
The preceding packaged attempt was cancelled without final diagnostics. Cleanup
now stops the child and closes proxy tunnels before the HTTPS and router servers,
so failure-path cleanup does not strand open tunnel connections. These integration
results do not establish useful customer deliverables or real-router acceptance.
The subsequent installed `ixxac` Mission demonstrated a missing constraint in that
fixture: finite governed inference deliberately rejects **all opaque CONNECT**
tunnels. Its permissive fixture proxy did not test this restriction. The maintained
fixture now rejects CONNECT and requires the native provider to fetch through
`/egress/fetch`. It proves provider registration/configuration and real host extraction,
not the router's host/SSRF implementation or finite-budget ledger. Those production
controls are unchanged and require separate installed acceptance.

The corrected finite-mode fixture passed on 2026-10-17 in source mode
(`native-mediated-web-host-20261017-r2`) and packaged mode
(`native-mediated-web-packaged-20261017-r1`), testing immutable candidate
`sha256:8d5112fb1301ab5f8c6bea82576c02687a70c45f48aa316c51f86c40e7667765`.
The pinned host late-binds web tools to global configuration, replacing the
executor's per-run configuration. Therefore the production entrypoint selects
`kars-router` globally; provider execution still requires the exact invocation
signal from a current, policy-authorized native call. Missing, foreign, replayed,
and ambiguous admissions cannot fetch. No run identifier in global configuration
is an authority grant. The packaged test uses no production source overlay and
requires CONNECT denial, one mediated TLS retrieval, real extraction, bounded
model output, and a real spill file. This is not customer Mission acceptance.

## Confirmed r1 production boundary failure

The genuine loader started this fixture's service, but production
`createNativeMissionExecutor` called `runtime.agent.session.createSessionEntry`
with `initialEntry: { agentHarnessId: "openclaw", modelSelectionLocked: true }`.
The host rejected it:

```text
Plugin "kars-native-host-verification" must register agent harness "openclaw" before it can create its sessions.
```

The receipt reported zero rounds, unknown usage, and zero attempted/successful
tools. This is **not a missing fixture harness registration**. At the pinned
commit, `src/plugins/registry-runtime-session-ownership.ts`'s `assertOwnedHarness`
requires both an existing registry harness and the calling plugin's ownership.
`src/plugins/runtime/types-core.ts` restricts `createSessionEntry.initialEntry`
to harness-owned, CLI-backed, or ACP-backed initialization. That API is not the
ordinary built-in OpenClaw session constructor. Neither the fixture nor Kars owns
the built-in runtime by merely naming it. Do not register a dummy harness, edit
session storage, remove ownership enforcement, or turn this failure into an
expected-success test.

### Ordinary plugin native session integration

A second genuine-host attempt disproved the proposed in-process Gateway shortcut:
`runtime.gateway.request` rejects plugins that are neither bundled nor trusted
official plugins. Kars must not impersonate either class. The r2 failure is
retained separately from r1; neither attempt reached model inference.

The supported ordinary-plugin path is `api.runtime.agent.runEmbeddedAgent`,
documented in pinned `docs/plugins/sdk-runtime/agent.md` as the neutral helper
for normal OpenClaw turns. The host runner owns session creation through
`resolveAgentRunSessionTarget` with `missingSessionKey: "create"`; Kars does not
write the session store or register a replacement harness.

Production rejects an existing assignment-derived session key using the public
read-only `getSessionEntry` API, supplies a fresh session ID, and invokes the
runner with `agentHarnessRuntimeOverride: "openclaw"`. Run-local model locking,
explicit provider/model, disabled fallbacks, tool allowlists, replay fencing,
abort signals, and native accounting remain required.

The r3 genuine host reached inference through this path, then failed the exact
model-visible tool assertion: OpenClaw enables `tool_search`, `tool_describe`,
and `tool_call` by default. Pinned `docs/tools/tool-search.md` documents
`tools.toolSearch: false` for direct schemas. The r4 attempt proved that changing
only the cloned run config was ineffective: prepared model runtime binding uses
the activated host configuration. The maintained sandbox entrypoint and this
fixture now set the option in startup configuration. Tool permissions remain
unchanged.

The r5/r6 attempts exposed a fixture packaging error: imported Kars source lived
outside the admitted plugin package, so module capture omitted its adjacent
Python helper. The fixture now stages the unmodified source and helper inside
one plugin package, matching the installed adapter's package boundary. No helper
path override, filesystem replacement, or capture exemption is used. Provider
fixture IDs are alphanumeric so OpenClaw's transcript normalization preserves
the same exact IDs across model requests, tools, policy, and outcomes.

The r7/r8 attempts completed all five rounds, four calls, and real file effects,
but exposed an incorrect production completion check. Pinned replay metadata
sets `replayInvalid` when completed side effects make another execution unsafe;
it does not by itself indicate a failed first execution. Kars no longer treats
that flag as failure. Existing-session rejection, one native invocation, failed
terminal-state checks, exact accounting, and final-output validation remain.
Regression tests cover a successful non-replayable write and failing terminal
states with replay prohibited. The r9 genuine-host run passed, including the
503 recovery and exact 580-token accounting.

Verified pinned sources (all at the commit in Requirements):

- `docs/plugins/sdk-runtime/agent.md`: ordinary plugin embedded-run helper.
- `src/agents/embedded-agent-runner/run-orchestrator.ts` and
  `src/agents/run-session-target.ts`: host-owned session target resolution.
- `src/plugins/registry-runtime-session-ownership.ts`: harness ownership checks.
- `src/agents/embedded-agent-runner/run/params.ts`: run-local model locking and
  native runtime override semantics.
- `src/plugins/runtime/runtime-embedded-agent.runtime.ts`: plugin-scoped host
  admission and rejection of caller-supplied authority.
- `docs/tools/tool-search.md`: default deferred schemas and direct-schema option.

- `docs/plugins/architecture.md`: admitted package inputs and exclusion of external
  non-module resources from on-demand module capture.
- `src/agents/embedded-agent-runner/replay-state.ts` and
  `run/attempt-normalization.ts`: side-effect replay metadata propagation.

Retain r1–r8 as failures and r9 as a source-overlay integration pass; do not
overwrite their receipts or describe them as installed customer acceptance.

### Compiled-candidate verification

Set `KARS_NATIVE_HOST_ADAPTER_MODE=installed` when running this same fixture in a
newly packaged candidate. It requires the adapter at `/opt/kars-plugin`, stages
its compiled `core` files and Python companion byte-for-byte inside the admitted
test plugin, and records every copied file's SHA-256 in `adapter-inputs.json`.
No source adapter fallback is allowed. The candidate's own `/mesh-plugin` and
production dependencies must remain unmodified; mount only this fixture and its
writable run directory, not a source adapter or mesh overlay. All native execution,
real file, recovery, lifecycle, and usage assertions are identical in both modes.
The default `source` mode remains available for development. Neither mode proves
the installed Kubernetes customer journey.

Static checks (do not start a host):

```sh
node --check tests/native-host/native-host.test.mjs
./node_modules/.bin/tsc --noEmit -p tests/native-host/tsconfig.json
```

Pinned boundary references used for this fixture: `src/plugins/services.ts`,
`src/plugins/runtime/runtime-embedded-agent.runtime.ts`,
`src/plugins/loader-module-runtime.ts`, `src/config/zod-schema.root-support.ts`,
`src/cli/gateway-cli/run-command.ts`, `openclaw.mjs`, and `node-sqlite.mjs` at the
commit above. The gateway, plugin service, and public runtime facade are used
instead of fabricating host admission or importing a separate private registry.
