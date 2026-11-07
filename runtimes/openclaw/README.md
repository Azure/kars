<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# OpenClaw runtime: required Entra mesh admission

Set `MESH_AUTH_BACKEND=EntraAgentIdentity` to require Entra admission through the existing Kars inference router and official AGT transport. This setting is incompatible with `AGT_SKIP_INIT=1`, `AGT_SKIP_ENTRA=1`, and `AGT_SKIP_VERIFY=1`; conflicts stop startup rather than downgrade authentication.

## Local validation

Run `npm run build`, `npm test`, and `npm run typecheck` from this directory.
Vitest discovers the maintained `src/**/*.test.ts` suites, matching the mesh
plugin and CLI. Generated tests under `dist/` are not a second test suite:
TypeScript can leave old outputs there after a source test is removed or renamed.
Source-test success does not establish image or installed-runtime acceptance.

## Scoped human approval

`kars_access_request` uses the active assignment scope when a new request or list
omits `scope_id` or supplies an empty string. A nonempty stale scope is rejected;
wait and cancel always require the original nonempty scope and request IDs.
Only tier requests transmit `tier`, and only egress requests transmit `port`.
A queued request is not permission: the owner decision and router policy remain
authoritative.

## Inference request lifetime

The task-loop client waits up to 630 seconds of socket inactivity for a model
response: the router's existing 600-second upstream deadline plus 30 seconds
for admission, response handling and settlement. It must not abandon a valid
reasoning request at the old 60-second client deadline. This does not extend
the router deadline, add retries, increase task limits or forgive unresolved
usage. A local timeout, aborted response or router error still fails the attempt;
unknown provider consumption remains unknown.

## Native Mission execution

MissionReceiver uses the registered plugin's `runtime.agent.session.createSessionEntry`
and `runtime.agent.runEmbeddedAgent` APIs from OpenClaw **2026.9.8**, pinned to
`fc23bc864e4553c2d215e479eeec47b67a0bf943`. There is one native run, not a CLI
subprocess, a Kars chat-completion loop, or a fallback executor. Assignment identity
selects an isolated native session; receiver admission, boot/run binding and terminal
replay protection remain controller-owned. The legacy task loop still serves
`task_request`, handoff resume and offload, not Missions.

An explicit reviewed phase is required. With `executionMode:"native"`, the model
chooses which granted tools to use and in what order; network capability need not
specify URLs or a mandatory fetch. Explicit `requiredToolCalls`, when supplied,
remain completion obligations. No input/fetch/write sequence, forced export slots
or follow-up model loop is imposed. Missing mode never widens an old grant.
Historical tool-bearing phases currently fail **before model execution**: their
whole-batch admission guarantee cannot be implemented from the pinned API's
individual tool-start events. They require explicit review as native phases, not
an automatic mode upgrade or a scripted-loop fallback. Historical compute-only
phases remain supported. The historical guard retains its exact GET, complete-input
and fetch-before-write restrictions. Assignments without a capability phase fail
rather than receiving ambient agent authority.

Run-bound v2 adapters expose `kars_mission_file_read`, `kars_mission_file_write`
and, when network is granted, `kars_mission_http_fetch`, `kars_mission_mcp_list`
and `kars_mission_mcp_call`. MCP uses the existing router bridge at `/mcp`, with
configured server bindings and router-enforced tool policy and credential scope;
network capability alone does not enroll a server. In both OpenClaw and Hermes,
listing with an omitted or empty-string `server` uses the existing qualified
catalogue (`tools/list`), not a tool invocation. Nonempty scopes still narrow the
catalogue; malformed scopes are refused. Invocation still requires an exact tool
name and rejects an explicitly empty server scope. Listing does not grant execution
authority. The model chooses discovery and invocation, rather than following a
prescribed MCP sequence. Cancellation
propagates through the bridge queue and HTTP request, with no automatic call replay.
Native read-only GitHub App access uses `kars_mission_http_fetch`, not a separate
GitHub read tool or MCP server. With reviewed `network` capability and an authorized
repository binding, call canonical `https://api.github.com/repos/{owner}/{repo}`
GET URLs without credentials, custom headers or a body. The router supplies scoped
authentication. Metadata-only work uses repository metadata and the paginated
`/branches` list (matching `default_branch` to `commit.sha`), not unsupported
`/branches/{branch}` or `/git/ref/heads/{branch}` routes. No additional direct egress,
GitHub MCP or `github-pr` capability is needed. Runtime guidance does not grant access:
keep assignment scope and actual denials; never claim unobserved private reads.
An explicitly requested GitHub MCP integration retains its own credential scope.

Explicit native `memory` capability independently exposes
`kars_mission_foundry_memory` for search/update through the existing `/platform/mcp`
`foundry.memory` service; network alone does not grant memory. The router retains
binding, exact scope and shared-scope authorization, Foundry credentials and update
status handling. Omitting scope or supplying `null` selects the bound private scope,
including with providers that require every schema property. Never invent a mission
scope or use the literal `private`; explicit strings require exact grants. Deletion and store
selection are not exposed. Calls use the same observed admission, policy and outcome
reporting as other native tools, and cancellation aborts the HTTP request without
replay. `queued`/`in_progress` does not establish persistence; even `completed` does
not approve knowledge. Read back when the assignment requires persistence evidence.
The configured memory binding and Foundry project must be available independently
of the selected inference provider.

Native network phases also expose `kars_mission_mesh_send`, `kars_mission_discover`,
`kars_mission_mesh_inbox` and `kars_mission_mesh_await` through the same observed
admission, policy and outcome path. They reuse the singleton AGT transport and
registry, not generic interactive messaging tools or another client/listener.
Send accepts `to_agent` (exact name or canonical DID) and ordinary `content` up to
65,536 UTF-8 bytes. Discovery accepts exactly one `name` or `did`, returns at most
16 fresh registrations, and requires a unique fresh registration to resolve a name.
Send/discovery have a 30-second invocation deadline; registry requests and results
are bounded. There is no automatic send/prekey retry. Await accepts up to 16 unique
`senders` and a positive `timeout_seconds` up to 300; results are capped at 160 KiB.
Timeout leaves partial selections queued and returns `completed:false`.

Native inbox eligibility is captured at live socket arrival for the exact active
assignment, then checked again before insertion and consumption. Pre-assignment,
replayed/backlogged, plaintext, chunk-reassembled and reserved protocol messages
(including JSON-encoded Mission envelopes) are not native ordinary inbox messages.
A name filter resolves to an authenticated DID; a payload's claimed sender name
is not authority. Ordinary messages themselves have no Team/run identifier, so
this is receiver-lifetime partitioning, not proof of a sender's Team membership.
Abort, stale authority and deadline expiry veto subsequent request/encryption/relay
effects without canceling unrelated invocations. An already-issued frame cannot be
recalled: send results report unconfirmed delivery, and an ambiguous failure must
not be replayed automatically. The scoped transport capability must be present;
an older built `@kars/mesh` fails closed rather than ignoring per-call custody.
Rebuild/package the shared mesh and runtime together before installed acceptance.

Native model and execution allowlists
exclude ambient shell, browser, generic messaging and delegation tools. Each effect checks
host invocation authority, the assignment lease, session/model correlation, the
reviewed ceiling and fail-closed AGT policy. The selected model must belong to the
finite `azure-openai` catalog using the local router `/v1` endpoint; provider/model
selection is locked and model/auth fallbacks are disabled.

Native discovery requires exactly one non-null name or canonical DID. The unused
field may be omitted or explicitly null, including when a provider's strict tool
schema requires both fields. Two populated targets and two absent targets are rejected.

HTTP retrieval tools use `/egress/fetch`, never direct public sockets. Native peer
tools use the existing router-mediated AGT relay and registry paths. Native phases may
select HTTPS URLs, methods, string headers and a text body, subject to the router's
actual policy. Successful fetches require outer HTTP 200, inner 2xx, `truncated:false`
and complete nonempty UTF-8 text up to 65,536 bytes (1 MiB envelope bound).
Failures, redirects and truncation do not discharge explicit required calls. This
is bounded text retrieval, **not** support for arbitrary binary downloads or opaque
CONNECT/browser/shell egress. Hosted search and additional capabilities are not
implicitly granted. Retrieved content is untrusted data, never new authority.

Completed rounds come from native `assistantTurns`. Usage requires matching
per-round assistant messages and model-call observations, with complete valid token
counts. Model-call starts are counted across attempts even when the pinned host
restarts its call IDs. An observation-only `before_agent_finalize` hook preserves
measured drafts before the host prunes a rejected draft and suppresses its `agent_end`.
It never requests revision or bypasses the host's side-effect rewind protection.
Assistant evidence is retained across finalization and attempt-end snapshots,
without charging repeated history twice; ambiguous or missing evidence still fails
reconciliation. Sparse cumulative totals, missing observations and unmeasured attempts
remain unknown and cannot succeed. Usage failures report mismatched observation counts
or the first invalid round and field category, without transcript content or provider
payloads; explicit native terminal errors retain precedence. Native tool-start observations reserve attempts,
including failed calls; an adapter with missing or repeated correlation cannot act.
Before a tool-enabled native run, `GET /telemetry/cursor` supplies the router-issued
scope and an ephemeral search cursor. Before authorization, the adapter reads
`GET /telemetry/trace?since=<cursor>` with `x-kars-telemetry-scope` and requires one
matching proposed tool call ID/name and its accepted round's model. Missing,
ambiguous, stale, truncated or dropped correlation fails before effects; the cursor
window is never treated as this run's complete inference history. OpenClaw's local
`azure-openai` provider names the configured router transport, not the upstream
selected by router policy. The correlated round's actual provider ID is retained
as `router_provider` in authorization context, separately from `native_provider`;
no provider alias is inferred and native context still requires the configured
local transport. Router inference policy and funding authorize the upstream.

`/agt/evaluate` receives the exact model-visible `tool:kars_mission_*` action and
`context.tool`, plus `scope_id`, `tool_call_id` and frozen assignment/native identity.
Only matched, positively authorized calls send `/telemetry/tool` outcomes, once per
call. Read/write summaries contain measured byte counts (and optional output name),
never paths or contents; fetch success has no invented typed summary. Failures use
a bounded `failure` summary. A rejected outcome receipt aborts the run rather than
repeating an effect or claiming its telemetry was recorded. Denied, invalid or
unobserved calls do not send completion reports.

Tool proposals alone cannot identify every inference round, especially the final
text-only response. When `activityFormat: "router-observations-v1"` is negotiated,
Activity exports only individually correlated proposals, their actual proposing
rounds and immediate harness-reported results. Correlated router operations also
retain tool-policy decisions, egress-policy decisions and upstream HTTP status/body
completeness, separately from harness success. Native fetches forward the authorized
scope and call ID; unmatched operations are not inferred from timing or tool names.
Only the destination host/port is retained, never URL paths, queries, headers or bodies.
Absent router flags mean unavailable, not allowed or denied. The shared AgentMesh
message validator preserves this distinction in live progress. Its distinct
`native-tool-correlated-only` coverage records `correlated_tools`, never invented
returned-response identifiers/counts. Unrelated cursor events and later transcript
echoes are excluded. Safe summaries omit arguments, URLs, paths and content.
The router's `github_service` flag is retained only as a denied egress decision;
generic GitHub HTTP remains blocked and publication uses the reviewed native adapter.
Completed tool reports publish serialized live progress. Rejected progress aborts the
native run before further effects, preserving the original reporting error rather than
masking it as journal exhaustion. A reporting failure does not undo a completed effect
or authorize replay. Successful completion also attempts the existing reserved
system-artifact attachment within the artifact limits.
Reset, expiry, truncation, lost events or refused reads yield explicitly unavailable
Activity without failing otherwise successful work. Export remains bounded to 130
rounds, 1,024 events and a separate 384 KiB Activity envelope; ordinary file limits
are unchanged. These forwarded observations do not prove
complete inference usage, final text-only rounds or independent tool attestation.
Native measured usage still comes from the separately reconciled host evidence.
Explicit native phases use `max(8, maxToolCalls + 2)` model rounds and the shared
[reviewed time/event headroom](../../mesh-plugin/README.md#native-mission-headroom),
not the historical 25-round/eight-minute limits. Tool correlation pages the existing
router cursor endpoint through a captured high-water mark and advances its cursor
after matching, retaining the proposing round for sibling calls. Each page remains
bounded; missing current events or changed scope still fail. Historical dropped
events outside that advancing window do not revoke current valid work. Activity
export stays explicitly unavailable beyond its retention limits, including empty
round lists when needed, without becoming an execution limit. Incomplete, yielded,
asynchronous, refused or uncorrelated completion still fails.

## Reviewed Mission outputs

Required filenames come from the controller contract, never inferred from prose.
The native user prompt quotes the overall Mission and role as reference context,
then states the current reviewed phase and its exact `/sandbox/<filename>` path
and `artifact_name` for each required export. These phase instructions also remain
in the extra system prompt, rather than relying on system context alone to resolve
conflicting final-deliverable requests in the overall Mission. The agent writes its own substantive findings, including
unknowns; the runtime does not create missing handoffs or retry the agent loop.
Root deliverables and final chat text do not substitute for phase handoffs.
Required names are minimum obligations, not an exclusive export list. Internal
phase guidance tells application builders to export changed source, tests, and
configuration with `artifact_name` after testing, alongside required handoffs.
Paths, hashes, and execution receipts do not retain those files for downstream
review. Unchanged inputs need not be republished; artifact limits and role authority
remain unchanged, and failed tests must remain reported as failed.
For byte-preserving native handoffs, the existing `kars_mission_file_write` accepts
`{path, source_path, sha256, artifact_name?}` **instead of** `{path, content, artifact_name?}`.
Both filesystem-read and filesystem-write must be reviewed. `source_path` must be
an exact `/sandbox/.kars-inputs/<name>` manifest path or a successful write from
this invocation; `sha256` is the complete `sha256:<64 lowercase hex>` digest from
the manifest or write result. Retained bytes are copied, not reread from ambient
host files or transcribed by the model. Skills, input bundles and arbitrary files
are not copy sources. Builders can export their tested scratch writes; principals
can copy retained builder/QA inputs to the original customer filenames. A matching
digest preserves byte identity, not independent proof that tests passed.

The destination uses the same guarded filesystem writer, explicit artifact
selection, required-output tracking, reserved receipt names and serialized limits.
Copy arguments receive the same tool correlation and governance authorization;
there is no implicit export or new capability. A digest mismatch fails before the
write, a failed named copy must be corrected before completion, and collected copy
bytes must still match the expected digest. Content writes remain supported.
Unused alternative fields may be omitted or null for strict-provider schemas;
non-null content cannot be combined with source/digest arguments.

Successful file-write results report the selected `artifact_name` and
`required_outputs_remaining`, so the native loop can see outstanding phase exports
without a scripted tool sequence or another run. This is write feedback, not final
file verification; read-only inputs and differently named deliverables do not clear
an outstanding export.
Explicit `artifact_name` exports remain supported. Native mode also collects a
successful write whose basename exactly matches a required filename. Other scratch
files, pre-existing files and inline answers cannot satisfy required outputs.
The runtime rereads the actual written files as strict UTF-8 before returning them;
model-supplied write arguments are not file-retrieval evidence. Missing, unreadable,
truncated, invalid or unresolved exports fail, with the existing 16-file/128 KiB
serialized artifact bound. Before each export write, the runtime checks the complete
pending export set, including aliases of the same normalized path and real Python
execution receipts. Writes and Python receipt reservations are serialized per
assignment so concurrent calls cannot overbook that envelope. An oversized write
is rejected before changing the file; the native loop can shorten its output and
retry while retaining the failed attempt. Python checks space for its execution receipt
before starting. Its request-sized worst-case reservation includes the exact source
manifest, arguments and identity, both full 2048-byte streams and JSON escaping at
both serialization layers. The 64-KiB receipt and 128-KiB export bounds are unchanged;
reservation whitespace contains only previously retained runs, never synthetic results.
A capacity refusal reports that no execution occurred and tells the
agent to shorten existing exports or execute before exporting large documents; retained
inputs can be read directly without republishing them. This is not an authorization or
spending failure. Final disk rereads remain mandatory.
File delivery is not evidence of business acceptance.

Downstream reviewed phases and the principal receive all exported peer documents,
not only handoff summaries, with deterministic aliases and original content/hash
and producer custody. Team retention reconstructs that same complete input set
from verified completed records, including sealed approved memory, after compute
retirement. It does not require a retired sandbox to become Ready again or accept
unbound replacement files. Historical summary-only assignments remain readable
for their retention lifetime only when their sealed inputs exactly match approved
memory plus all verified handoffs; this is not evidence of full-file peer review.
Partial full-export sets, substituted content, and changed custody are rejected.
Response text and router telemetry are not peer inputs.

Native accounting reconciles assistant measurements across attempt snapshots using
provider, model, timestamp and exact token fields, preserving repeated occurrences.
Transcript redaction or replay-only body changes are not new model calls. Changed
measurements and ambiguous collisions across pruned attempts fail the strict
assistant/call/turn count check; missing usage is never replaced with zero or
cumulative totals. Messages without usable timestamps retain exact-message matching.

Focused tests use native API doubles only at test boundaries. Installed validation
must still verify the pinned host's plugin lifecycle permissions, v2 tool bindings,
model-call/tool-start events, per-round usage, real router mediation and final file
retrieval. Passing local tests is not installed-runtime or live Mission acceptance.

The shared controller retires a terminal reviewed phase's sandbox and inference
policy only after verifying its current grant and immutable terminal records.
Verified failed or rejected phases can retire compute without authorizing later
phases or synthesis. An empty failed artifact ConfigMap may omit `data`, as the
Kubernetes API does; unexpected artifacts or missing nonempty outputs still fail
custody verification. The Task, lifetime budget lineage and publication records
remain available. Ambiguous evidence does not authorize terminal cleanup;
stopping the root retains the existing owned-compute cleanup.

## Startup and credentials

The sandbox entrypoint performs a bounded `GET /v1/mesh-token` readiness preflight. It clears any inherited `AGT_OAUTH_TOKEN` and discards the preflight credential. `MESH_TOKEN_MAX_WAIT` controls the total preflight budget (1–300 seconds, default 60). The router URL follows `KARS_ROUTER_URL`, then `ROUTER_LOCAL_URL`, then the local router on port 8443.

The plugin independently mints a fresh token from that same endpoint. The response must contain a nonempty `access_token`, a Bearer `token_type`, and numeric `expires_in` greater than 60 seconds and no greater than one day. Each auth HTTP request has an eight-second deadline and a 64 KiB response limit. Token values and auth response bodies are not logged.

The single existing transport registers its identity and connects using the fresh credential. Before publishing its client or identity, the plugin posts `{ amid, verification_token }` to `/agt/registry/v1/registry/verify`. Successful admission requires the current response to name the connected canonical `did:mesh:<32 lowercase hex>` identity, report `tier: verified`, and include nonempty `verified_app_id` and `verified_tenant_id`. Explicit negative/error fields reject admission even with HTTP 200. An old verified registry record is not sufficient.

The request is `POST /agt/registry/v1/registry/verify` with JSON `{ "amid": "<connected canonical DID>", "verification_token": "<fresh router token>" }`. The transport must remain connected with that same current DID and lifecycle through the POST response; a matching response for an identity that is no longer current is rejected. The token is the same one sent in the relay's `connect.token` frame. Required renewal and reconnect repeat the complete mint/connect/verify sequence.

The selected registry implementation is [`verify_identity`, `agentmesh/registry/app.py`](https://github.com/microsoft/agent-governance-toolkit/blob/17595e649abdfa703962c2f8c241826f68318109/agent-governance-python/agent-mesh/src/agentmesh/registry/app.py). Local enforcement is in `src/core/mesh-auth.ts`, `validateMeshVerification` and `RequiredMeshAuth.authenticate`: it requires a matching DID, verified tier, application ID and tenant ID, not merely HTTP success or a timestamp. The AGT 5 SDK is checksum-bound under `vendor/agt`; mesh servers, Python owners and Rust dependencies use the same source revision. Local compatibility does not establish deployed Entra acceptance.

## Renewal, readiness, and teardown

One lease wraps the existing transport; it does not create a second mesh client, SDK, identity, or prekey writer. The process-wide `Symbol.for` initialization lock remains synchronous. Concurrent plugin registrations share that initialization or join an in-progress renewal.

The client, mission authorization/execution, callbacks, offload entry points, and task-loop readiness are gated on an unexpired verified lease. Socket callbacks retain their originating lifecycle and arrival admission through asynchronous work, preventing stale callbacks from acting through a newly admitted client. Messages queued in the transport before initial admission are drained before publication.

Renewal starts before expiry (five minutes early, or halfway through the remaining lease for shorter tokens). Renewal and reconnect immediately withdraw publication and readiness, retire the old socket, mint a fresh router credential, reconnect the same transport, and reverify before republishing. An embedded JWT expiry may shorten, but never extend, the router lease. Required mode does not use the optional twelve-hour environment-token reverify loop.

Mint, verification, connection, expiry, or renewal failures stop required-mode usability and terminate the process after bounded cleanup. Connection and retirement bounds are 35 and 10 seconds respectively. Failed retirement quarantines the lease: it cannot reopen, and process singleton/prekey custody remains held until restart. SIGINT/SIGTERM also retire the required lease before exit.

Without the required backend setting, existing optional OAuth verification remains best-effort and explicit development/skip behavior remains available. Required-mode failures never select that fallback.

## Authoritative agent instructions

The sandbox targets OpenClaw **2026.9.8** on Node.js **24.21.0**. Its entrypoint retains the existing provider-specific Kars identity, welcome, tool, and channel instructions, but stores them in `plugins.entries.kars.config.systemPrompt` rather than the removed `agents.defaults.systemPromptOverride` setting.

The trusted Kars plugin returns that exact string from `before_prompt_build` during full registration. It does not read conversation messages or append instructions across turns. Workspace bootstrap rewrites therefore do not remove the configured instructions. Discovery/setup/CLI-metadata registration does not install the live hook; standalone use without a configured prompt leaves OpenClaw's prompt unchanged. A non-string prompt or a configured prompt on a host without the hook API fails plugin registration.

The entrypoint explicitly grants `plugins.entries.kars.hooks.allowConversationAccess` and `allowPromptInjection`, which the non-bundled plugin needs for this hook. These permissions are not granted to other plugins. Prompt text is bound with `jq --rawfile` after placeholder substitutions, preserving quotes, newlines, Unicode, and shell-sensitive characters. Empty or missing generated instructions and invalid configuration stop entrypoint setup without replacing the original configuration.

Upgrade the base image and sandbox adapter together. Local regressions verify serialization and plugin registration, not actual model consumption or compatibility of every bundled channel/provider. Before deployment, inspect `openclaw plugins inspect kars --runtime --json` in the candidate image and verify a real agent turn uses the instructions. The dependency scan and installed beta acceptance remain separate gates; selecting a newer release is not evidence that either passed. Hermes keeps its separate runtime/Node requirements.

## Offline external plugins

OpenClaw 2026.9.8 ships Discord, Slack, WhatsApp, Brave, Tavily, Exa,
Firecrawl and Perplexity as external packages. The existing AL3 base recipe
installs the eight version-matched packages through OpenClaw's installer at
build time, checking the registry integrity values in
`sandbox-images/openclaw/external-plugins.json`. Their code remains image-owned
and read-only under `/opt/openclaw-managed`; startup never downloads or installs
packages.

The final image also binds each installer project's `node_modules/openclaw`
peer to the image's host. Native dependencies can be hoisted outside the
plugin's own dependency directory (for example, WhatsApp's Sharp binary), where
the installer's per-plugin peer link is not visible. This build-time binding
preserves OpenClaw's retained-native host check under a read-only root; it does
not disable capture, change installer records, or create links at runtime.
Conflicting existing peers fail the build instead of being replaced.

The installer database is exported as a checked SQLite seed. Before starting
OpenClaw, the entrypoint initializes a fresh `/sandbox/.openclaw/state` from that
seed as the sandbox user. Restarts with the same image preserve the live database
and its contents. An existing database without the current seed binding fails
explicitly without overwriting data; recreate the sandbox pod when upgrading the
image, as `/sandbox` is ephemeral. No new persistent volume is introduced.

Plugin capture and generated code need writable scratch independently of the
read-only installed packages. The controller already supplies disk-backed
`/sandbox` and memory-backed `/tmp`; a 256 MiB scratch cap is insufficient for
all eight external plugins. An isolated native ARM64 preparation proof loaded
all eight in two fresh states within an 832 MiB diagnostic `/sandbox` budget
and the existing 4 GiB/no-swap memory limit. This is not a production storage
limit, complete sandbox-image qualification, channel delivery or beta acceptance.

## Node.js archive integrity

Both base-image stages verify the selected Node.js archive before extraction using `sandbox-images/openclaw/node24-SHASUMS256.txt`. Its ARM64 and x64 hashes come from the official Node.js 24.21.0 `SHASUMS256.txt`. Changing `NODE_VERSION` also requires refreshing these pinned hashes; missing, corrupted, unlisted-version and malformed-checksum inputs fail closed. Unsupported architectures are rejected rather than silently downloading x64.

Run the archive guards and offline-wheel regressions together from the repository root:

```bash
python3 -B -m unittest discover -s runtimes/tests -p 'test_openclaw_*.py' -v
```

These tests exercise the real checksum command and architecture selection, but do not replace rebuilt-image execution or dependency scanning.

## Offline Node dependency updates

The selected OpenClaw and Node releases remain unchanged. The final base-image
stage applies four pinned package updates at the six vulnerable installed
locations recorded in `vendor/sandbox-node/security-updates.json`: MCP SDK
1.31.0, brace-expansion 5.0.11, and the compatible Undici 6.28.1 and 7.29.1 lines.
Global npm 12.2.0 and the three skill CLIs are pinned to the scanned baseline;
the update does not remove tools or reinstall OpenClaw.

These archives contain the complete, unmodified published files and licenses,
corroborated against matching jsDelivr and UNPKG SHA256/size inventories. They
were repacked with `npm pack --ignore-scripts`; their recorded checksums are
**not** the original npm registry tarball checksums. The manifest retains both
inventory URLs and hashes. A refresh must compare every published file before
repacking and verify that the resulting archive has exactly that file set.

The build-only updater verifies every archive and installed package identity,
Node engine, existing dependency/peer range and parent declaration before
replacing code. For npm's hoisted bundled dependencies, it checks every declaring
consumer that actually resolves to the selected package, not npm's top-level
manifest or consumers of a separate nested copy. It uses npm's offline archive extractor, executes no package
scripts, preserves existing nested dependencies, and updates an exact parent
pin only when it matches the recorded old version. Missing/incompatible
dependencies, unexpected versions and symlinked targets fail the build rather
than triggering network installation. The temporary updater/archives are
removed from the final image; plugin seed, host-peer binding and runtime startup
remain unchanged.

The regressions exercise actual offline extraction, rejection paths and both
vendored Undici implementations against loopback HTTP. They do not qualify the
complete OpenClaw/MCP behavior on AL3. Rebuild, run the unchanged native plugin
acceptance, and scan the complete image before deployment. Known unresolved
1Password, braces and http-cache-semantics findings are not suppressed or waived.

## Go skill tools

The base image cross-compiles all nine Go skill tools on the build worker's
native CPU, including the CGO/SQLite-backed `wacli`. This avoids running the Go
compiler under ARM64 emulation. The disposable, digest-pinned Go 1.26.6
Bookworm builder is not shipped; the runtime remains Azure Linux 3.

BuildKit supplies `BUILDPLATFORM` and `TARGETARCH`. Legacy builders must supply
both explicitly, for example `--build-arg BUILDPLATFORM=linux/amd64
--build-arg TARGETARCH=arm64` for an AMD64 worker producing an ARM64 image.
Missing or unsupported target architectures fail before compilation.
`--target go-builder` builds just this first stage without installing OpenClaw.

The build retains all eight CGO-free tools plus `wacli`, using a target C
compiler for its bundled SQLite and pure-Go DNS/user lookups. Each executable
must have the requested ELF machine and no dynamic loader or shared-library
dependency before it is copied into the runtime. These build checks do not
replace native execution of the finished image.

## 1Password CLI integrity

The AL3 base includes 1Password CLI **2.40.0** without changing the selected
OpenClaw release. Both ARM64 and AMD64 archives are pinned by SHA256 in the
recipe and checked before extraction; unsupported architectures fail closed.
The pinned archives contain binaries whose detached signatures were verified
against 1Password's published signing fingerprint
`3FEF9748469ADBE15DA7CA80AC2D62742012EA22`.

When updating this tool, authenticate both downloaded binaries using the
[publisher's verification instructions](https://www.1password.dev/cli/verify/)
before changing the archive hashes. Signature verification and recipe tests do
not establish a clean dependency scan or rebuilt-image runtime acceptance.

## Offline Python dependencies

The base image installs the committed `vendor/sandbox-wheels` payload without accessing PyPI. `SHA256SUMS` binds every wheel, and the image verifies it before installation and runs `pip3 check` afterward. Keep the ARM64 and x86_64 CPython 3.12 payloads together; remove superseded wheels so pip cannot select an unintended version.

`security-updates.json` records official PyPI URLs, SHA256 digests, sizes, and versions for the eight refreshed distributions: aiohttp 3.14.3, anyio 4.14.2, cryptography 50.0.2, Pillow 12.3.0, pyasn1 0.6.4, pypdf 6.19.0, soupsieve 2.8.4, and urllib3 2.8.0. When refreshing these packages, retain upstream hash evidence, update both supported architectures, regenerate the complete checksum manifest, and run the wheel regressions from the repository root:

```bash
python3 -B -m unittest discover -s runtimes/tests -p test_openclaw_wheels.py -v
```

The regressions check inventory, bytes, actual wheel metadata, removal of superseded versions, and image verification ordering. Cross-platform pip dry runs are additional resolution checks, not actual Linux installation/import tests. The rebuilt image still needs its runtime checks and fresh dependency scan before deployment.

## Local validation

From this directory:

```bash
npm test -- src/core/mesh-auth.test.ts src/core/mesh-auth-entrypoint.test.ts src/core/system-prompt-entrypoint.test.ts src/index.mesh-auth.test.ts src/index.test.ts src/core/agt-heartbeat.test.ts src/core/mission-receiver.test.ts
npm run typecheck
./node_modules/.bin/oxlint src/core/mesh-auth.ts src/core/mesh-auth.test.ts src/core/mesh-auth-entrypoint.test.ts src/index.mesh-auth.test.ts src/index.ts
bash -n ../../sandbox-images/openclaw/entrypoint.sh
```

The auth regressions use controlled transports and local HTTP servers. Entrypoint tests execute bounded authentication, prompt-serialization, and credential-driven plugin-registration blocks with controlled commands and temporary files, not the complete sandbox entrypoint. They do not deploy resources, prove model consumption, or establish live Entra/AgentMesh acceptance.
