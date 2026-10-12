# OpenClaw runtime: required Entra mesh admission

Set `MESH_AUTH_BACKEND=EntraAgentIdentity` to require Entra admission through the existing Kars inference router and official AGT transport. This setting is incompatible with `AGT_SKIP_INIT=1`, `AGT_SKIP_ENTRA=1`, and `AGT_SKIP_VERIFY=1`; conflicts stop startup rather than downgrade authentication.

## Local validation

Run `npm run build`, `npm test`, and `npm run typecheck` from this directory.
Vitest discovers the maintained `src/**/*.test.ts` suites, matching the mesh
plugin and CLI. Generated tests under `dist/` are not a second test suite:
TypeScript can leave old outputs there after a source test is removed or renamed.
Source-test success does not establish image or installed-runtime acceptance.

## Reviewed Mission outputs

Required output names are passed from the installed Mission contract into the
executor, not inferred from the task's prose. Each phase exports its own named
handoff or final deliverables with `file_write` and an explicit `artifact_name`.
Scratch files and inline answers never satisfy a required export. A premature
final answer receives corrective feedback only within the original tool-call
and model-round limits; exhausted limits fail the phase and block synthesis.
Required-output executions request one tool call at a time. When only enough
calls remain to export the missing outputs, the model is offered `file_write`
with those exact non-null filenames; after the ceiling is used it is asked for
a final answer without tools. Tool authorization and completion validation
remain independent, including when a provider ignores these request constraints.
Legacy assignments without required outputs retain their inline-delivery path.

The shared controller retires a completed reviewed phase's sandbox and inference
policy only after verifying its current grant and immutable successful handoff.
The Task, lifetime budget lineage and publication records remain available to
later phases and synthesis. Failed or ambiguous evidence never authorizes this
completion cleanup; stopping the root retains the existing owned-compute cleanup.

## Startup and credentials

The sandbox entrypoint performs a bounded `GET /v1/mesh-token` readiness preflight. It clears any inherited `AGT_OAUTH_TOKEN` and discards the preflight credential. `MESH_TOKEN_MAX_WAIT` controls the total preflight budget (1–300 seconds, default 60). The router URL follows `KARS_ROUTER_URL`, then `ROUTER_LOCAL_URL`, then the local router on port 8443.

The plugin independently mints a fresh token from that same endpoint. The response must contain a nonempty `access_token`, a Bearer `token_type`, and numeric `expires_in` greater than 60 seconds and no greater than one day. Each auth HTTP request has an eight-second deadline and a 64 KiB response limit. Token values and auth response bodies are not logged.

The single existing transport registers its identity and connects using the fresh credential. Before publishing its client or identity, the plugin posts `{ amid, verification_token }` to `/agt/registry/v1/registry/verify`. Successful admission requires the current response to name the connected canonical `did:mesh:<32 lowercase hex>` identity, report `tier: verified`, and include nonempty `verified_app_id` and `verified_tenant_id`. Explicit negative/error fields reject admission even with HTTP 200. An old verified registry record is not sufficient.

The request is `POST /agt/registry/v1/registry/verify` with JSON `{ "amid": "<connected canonical DID>", "verification_token": "<fresh router token>" }`. The transport must remain connected with that same current DID and lifecycle through the POST response; a matching response for an identity that is no longer current is rejected. The token is the same one sent in the relay's `connect.token` frame. Required renewal and reconnect repeat the complete mint/connect/verify sequence.

The selected registry implementation is [`verify_identity`, `agentmesh/registry/app.py`](https://github.com/microsoft/agent-governance-toolkit/blob/f68f2cf312c7e1366d6fd5654c51d8380c815222/agent-governance-python/agent-mesh/src/agentmesh/registry/app.py). Local enforcement is in `src/core/mesh-auth.ts`, `validateMeshVerification` and `RequiredMeshAuth.authenticate`: it requires a matching DID, verified tier, application ID and tenant ID, not merely HTTP success or a timestamp. The AGT 5 SDK is checksum-bound under `vendor/agt`; mesh servers, Python owners and Rust dependencies use the same source revision. Local compatibility does not establish deployed Entra acceptance.

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
