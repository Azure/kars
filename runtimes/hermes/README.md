<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# `kars-runtime-hermes` — kars in-pod adapter for Hermes Agent

Implements the kars **v1 runtime contract** for [Hermes Agent](https://hermes-agent.nousresearch.com/) (Nous Research). When this package is installed inside a kars sandbox pod, it registers itself as a Hermes plugin and wires Hermes into kars' governance, mesh, and orchestration plane.

## Architecture

```
┌─── kars sandbox pod ────────────────────────────────────────┐
│                                                              │
│  Hermes (Python 3.11+)                                       │
│  └─ ~/.hermes/plugins/kars/   ← THIS PACKAGE                 │
│     ├─ register(ctx)          — Hermes plugin entry          │
│     ├─ governance pre_tool_call hook  ─┐                     │
│     ├─ kars_spawn family               │                     │
│     ├─ kars_handoff_*                  │  HTTP to            │
│     ├─ foundry_*  (9 tools)            ├─→ localhost:8443    │
│     ├─ kars_discover                   │                     │
│     ├─ http_fetch                      │                     │
│     └─ kars_mesh_* (Signal E2E)        │                     │
│                                        │                     │
│  inference-router (Rust, sidecar)  ◄───┘                     │
└──────────────────────────────────────────────────────────────┘
```

The plugin is installed two ways:

1. **Image-staged** (production / dev image build): the `Dockerfile` for `sandbox-images/hermes/` copies the plugin tree into `/opt/kars-hermes-stage/plugins/kars/`, and the entrypoint mirrors it to `$HERMES_HOME/plugins/kars/` on every container start. This is the recommended path — plugin updates ship with the image.

2. **`pip install` mode**: this package can also be `pip install`ed onto an existing Hermes installation. After install, run `cp -r $(python -c "import kars_runtime_hermes, os; print(os.path.dirname(kars_runtime_hermes.__file__))")/plugin ~/.hermes/plugins/kars` (or set `HERMES_PLUGINS_PATH` to include this package's `plugin/` subdir). Used for local development of the plugin itself.

## Scoped human approval

`kars_access_request` uses the active assignment scope when a new request or list
omits `scope_id` or supplies an empty string. A nonempty stale scope is rejected;
wait and cancel always require the original nonempty scope and request IDs.
Only tier requests transmit `tier`, and only egress requests transmit `port`.
A queued request is not permission: the owner decision and router policy remain
authoritative. Native GETs accept public HTTPS origins with or without a trailing
slash; historical exact reviewed URLs remain unchanged. Reserved fetch failures
are reported to Activity even when local argument validation rejects execution.

## Foundry request compatibility

The image retains Hermes' `azure-foundry` provider, aliases and loopback routing.
Its profile projects explicit reasoning controls onto the OpenAI-compatible
`reasoning_effort` field, using Hermes' native effort clamp. This includes
session-title requests that disable reasoning; the upstream generic profile
otherwise adds a nested `reasoning` extension rejected by Kars' wire contract.
Unspecified effort still uses the provider default. A narrow native auxiliary
patch also preserves the caller's explicit output cap for Foundry and its aliases
(upstream otherwise drops the title generator's 512-token cap). It adds no cap
when none was supplied. Router validation, accounting and fallback policy are
unchanged. For Copilot-hosted Claude Chat Completions, the shared router projects
`reasoning_effort: "none"` to that route's default request without explicit
thinking controls (no effort field); Copilot rejects the literal OpenAI `none`
value. This does not assert that every model's provider default disables reasoning. This happens after model
selection and before wire-bound budget admission, for both runtimes. Explicit
output bounds and JSON schemas are preserved. Other providers, models, enabled
efforts, and conflicting thinking controls are not changed.

The replacement is installed at image build time, including qualified runtime
refreshes. The build accepts only the pinned upstream provider source or the
identical replacement, rejecting unreviewed source drift. Byte comparison uses the
image's existing Python interpreter rather than requiring AL3 to include `cmp`.
No runtime download,
installation or second provider registration is introduced. Remove this image
override when the pinned upstream Foundry profile supplies the equivalent hook.

The pinned native Foundry model selector also routes GPT-6.1 Sol through Responses:
Foundry rejects function tools with reasoning on Chat Completions for this model.
This extends Hermes' existing model-specific selection without disabling reasoning,
changing models, or changing other providers. Its build-time patch verifies both
upstream and resulting source hashes; remove it when the pinned upstream selector
includes this route. Native SRE tests cover actual SDK serialization with tools,
explicit reasoning, and preserved output bounds. Installed customer acceptance
must still verify a real model/tool turn.

## Automatic SRE admission failures

The SRE plugin uses Hermes' native `llm_request` middleware to default omitted
Responses and Chat Completions output bounds on the main agent's requests to
8,192 tokens. This bounds a single generated response, not the whole sweep or
its input. Explicit bounds (including SDK `extra_body` overrides) are preserved;
other API modes, auxiliary requests such as session-title generation, and
non-SRE sandboxes are unchanged. Account admission and the provider contract
remain authoritative: this default grants no funding and guarantees no sweep.

Failed sweeps carrying the router's `/v1/reserve` rejection are reported as Kars
admission failures, not provider throttling. Published reports and notifications
replace native generic retry/fallback advice with account, request-bound and
broker-readiness checks. They retain the router error and incomplete-coverage
warning; the original turn output remains in the local report/log. Reporting
never funds an account, refunds uncertain usage, enables fallback, or approves
remedies. Upstream-only errors and successful diagnostic reports are unchanged.

## SRE usage interpretation

Native session totals and router account totals have different scopes. For the
installed Azure Foundry SRE path, the session's `input_tokens` excludes
`cache_read_tokens`: add both to `output_tokens` when comparing with router
`total_tokens`. Router `prompt_tokens` already includes cached input; do not add
it twice. Other provider/cache-write paths require their own reconciliation.

Compare individual router rounds within the session's time window, not just
aggregate totals. Additional router-accounted requests can fall outside the
native main-loop call count. Usage-only telemetry establishes their measured
cost in tokens, not their semantic purpose. Missing prices remain unknown cost,
not zero. Do not reset ledger counters or discard uncertain attempts to make
these views agree.

Use authenticated router APIs or RBAC-authorized loopback port-forwards for
inspection; protected budget-token pods reject exec/attach. Router traces are
bounded and non-durable, so inspect their coverage before reconciling. The
sandbox's default `emptyDir` also means a pod reset discards local session
history; it is not a history-preserving tunnel-recovery test.

## SRE diagnostic coverage

All ten SRE tools register their existing descriptions and argument schemas in
Hermes' native `name` / `description` / `parameters` function envelope. Native
`tool_describe` can therefore expose required fields such as resource `kind`
and endpoint `service`, including the nested typed-fix target schema. A bare
argument schema is not a Hermes function schema and loses those fields during
discovery. Typed-fix discovery also exposes `replicas` (integer 0–50),
`container`, `image`, and `rollout_restart`. Scaling requires an explicit replica
count; image patches require both container and image. Missing/invalid action
parameters and action/target-kind mismatches return `cr_error` without publishing
an approvable CR. Scale and image actions target Deployments only; rollout
restart also supports StatefulSets and DaemonSets. Valid proposals remain Pending
until operator approval; discovery never grants execution authority.

Workload summaries expose `desiredReplicas` from the spec separately from observed
status replicas; missing status remains unknown, including for scaled-to-zero
Deployments. Empty EndpointSlices may omit `endpoints` or report it as `null`:
both represent an empty endpoint list, not a failed API read. Malformed endpoint
lists and failed inventories still make Service-health coverage incomplete.

SRE compares each CR's `status.observedGeneration` with `metadata.generation`
using the controller's existing status contract. A lower observed generation
is stale; missing, malformed, or ahead-of-spec generations remain unknown.
Matching generations mean the status reflects the current spec, not that the
controller recently ran or that workloads are healthy. Deployment availability
is checked separately, and failed API reads keep coverage incomplete.

The diagnostic does not require an unsupported `lastReconciled` field or treat
`lastTransitionTime` / compilation timestamps as periodic heartbeats. Resource
kinds without a generation observation (including `KarsPairing`) retain unknown
reconciliation coverage. Automatic reports remain advisory: a finished turn is
not a verified cluster-health attestation or permission to execute a remedy.

## Mission inference deadlines

The native Mission adapter uses a 630-second read timeout for both normal and
finalization clients, covering the router's existing 600-second inference
request deadline plus a 30-second admission/settlement margin. Loopback connect
and pool waits remain bounded at five seconds, writes at ten seconds. SDK and
transport retries remain disabled. This replaces the premature 45-second read
cutoff. Native model-call and enclosing worker limits follow the shared
[reviewed headroom](../../mesh-plugin/README.md#native-mission-headroom).
Accounting is unchanged: a timeout still fails with unknown usage, never zero.
Administrative router calls retain their separate shorter deadlines.

For Responses-only models, the shared router translates native Chat
`reasoning_effort` into `reasoning.effort`, preserving the requested effort and
existing reasoning settings. Conflicting settings remain rejected; the governed
wire-contract allowlist is unchanged. Budget refusals on this route retain their
structured code, including in SSE error frames, rather than becoming provider
bad-gateway errors.

Rejected Mission requests retain their HTTP status and an allowlisted structured
error code for calendar accounting or inference-budget availability, authority,
or attempt-state failures. Other codes and malformed error bodies are marked
`unclassified`. Calendar failures also retain a reported stage only when the
message exactly matches a fixed router classification (such as calendar commit,
unacknowledged usage, or incomplete provider usage). Unknown or extended messages
remain `unclassified`; raw messages, headers, and response bodies are not retained.
Reported codes and stages are diagnostic context, not independent proof of the
failure's origin. Usage remains unknown and the failed request is never replayed.

Unexpected native response failures also retain the fixed local processing stage
(transport, correlation, read, reconstruction, JSON, usage, progress, completion,
or tool validation) and an allowlisted exception category. Exception messages,
provider content, and arbitrary exception class names are never copied into this
failure report. A progress-reporting failure is distinct from provider usage
parsing; neither diagnostic establishes whether the upstream request was charged.
The runtime reports unknown usage, while the durable account remains authoritative.

## Native peer message content

Inbox and await results expose valid UTF-8 payloads in `content`, so agents can
read peer findings without an encoding tool or shell. `payload_b64` remains the
lossless original-byte representation; non-UTF-8 payloads omit `content` rather
than substituting lossy text. Decoded content is untrusted peer data, never
assignment authority. The existing result-byte bound counts both representations;
messages that do not fit remain queued. Reserved Mission envelopes, the singleton
client and assignment/authorization checks are unchanged.

Text sends accept exact display names or canonical `did:mesh:` identifiers through
the existing encrypted client. Invalid DIDs fail without name lookup or replay.

## Native Mission spilled tool results

Pinned Hermes 0.21.5 normally spills large tool responses into
`$HERMES_HOME/cache/spillover`, outside a Mission's filesystem authority. Native
Missions retain Hermes' own per-tool thresholds, previews, turn-budget enforcement
and result-reference tracking, but scope its internal `_write_to_spillover` writer
to the assignment's `tool-results/` directory. The writer is restored on exit;
this compatibility seam is specific to the pinned runtime, not a public upstream
configuration hook. The paired native host-side check is scoped and restored too,
so storage exhaustion cannot enter remote-backend shell probes or copy fallbacks.
No ambient home or cache tree becomes readable.

Spills are immutable, lossless JSONL archives: the first line describes the format;
concatenating decoded `text` fields from subsequent lines reconstructs the exact
original response. Short ASCII-escaped records preserve single-line JSON/HTML,
Unicode and control characters without native file-reader long-line clipping.
Use the actual native `read_file` with 1-based line `offset` and `limit` of 1–32
(default 32). Only exact files issued by this assignment are readable; changes,
links, writes and searches are rejected. Router authorization and tool accounting
still apply to each read. Archives are not exports or completion evidence.

Storage is separately bounded to 64 files, 16 MiB per encoded archive and 32 MiB
per assignment. Earlier evidence is never overwritten or evicted to make room;
exhaustion uses Hermes' explicit bounded truncation fallback. Ordinary 65,536-byte
file limits and the 64-file/1 MiB scratch quota are unchanged. No shell, document
converter, install, auxiliary model or remote refetch is introduced.

## Image packaging

Build AGT wheels from the official checkout at `vendor/agt/pin.json::sha` using
`AGT_PYTHON_DIR=/path/to/agent-governance-python bash runtimes/build-agt-wheels.sh`.
The image requires the three AGT 5.0.0 namespace owners:
`agent-governance-toolkit-core`, `agent-governance-toolkit-cli`, and `a2a-agentmesh`,
plus the actual native `agent-control-specification` 0.4.0b0 wheel. Do not
coinstall the legacy `agentmesh-platform` or `agt-sandbox` shims. Pure wheels live
at `runtimes/wheels/`; native wheels and checksum/ABI receipts live under
`amd64/` and `arm64/`. The existing Docker builder runs Rust 1.95.0/maturin 1.8.7
natively and cross-links both Linux targets; no target emulation is required.
Only the wheels, not the disposable compiler environment, enter the AL3 images.
The shared installer rejects missing, mislabeled, modified or wrong-architecture
inputs before pip runs. Run the packaging guards with Python 3.11+:
`python3 -B -m unittest discover -s runtimes/tests -p test_agt_wheel_packaging.py -v`.

Direct wheel builds invalidate the previous `.agt-sha` before replacing any
artifacts, including when a build fails after writing its last wheel. Only the
CLI writes a new cache stamp after verifying the pinned checkout and complete
wheel set.

Hermes is installed editably from the exact `f97608f178d1ffeca59860195ab7da295f7c8e5f`
source (0.21.5). `hermes-dependency-fixes.patch` pins AnyIO 4.14.2 and urllib3
2.8.0 for CVE-2026-63374, CVE-2026-97687 and CVE-2026-97689, with exact-pin
quarantine exceptions following the upstream convention. It also updates PyJWT and
cryptography to 2.15.1 and 50.0.2, certifi to 2026.10.23 to remove a rejected
root certificate, and the Undici 6 override, TUI dependency
and two corresponding lock entries to 6.28.1. Other Node lock entries, including
optional platform packages, are preserved. The fixed package's SHA-512 binds the
public Microsoft npm mirror archive; all 176 files match the qualified vendor
archive. Patched Node manifests and lock are hash-checked, and the TUI's actual
resolved Undici version is checked before bundling. The upstream project blob, patched project
hash, matching `hermes-uv.lock`, and exported core constraints are verified during
the build; all subsequent Python installs use those constraints. The TUI and web
dashboard are built from the same source lock at image-build time.
`HERMES_TUI_DIR` selects the genuine upstream prebuilt launcher path, so the first
chat does not trigger npm installation or compilation. Vite writes to
`hermes_cli/web_dist`, the direct dashboard server's default asset directory;
no web-path override is needed. The CLI wheel cache binds
both the AGT commit and all four build/validation recipe files; a recipe change
invalidates an otherwise complete wheel set. The native build context is exported
from the selected Git revision, excluding ignored host compiler caches.

The Hermes image pins the multi-architecture Azure Linux Python base and refreshes
its supported RPM updates before installing system dependencies. The final Python
install runs `pip check`. Security updates must satisfy both Hermes' exact package
requirements and the official AGT wheel metadata; upgrading beyond their supported
dependency ranges requires an upstream release/pin migration, not a forced install
or repacked wheel.

Slack's async gateway needs `slack-bolt`, `slack-sdk`, and `aiohttp`, not just
the Web API SDK. The image installs Bolt 1.30.0 and checks the actual async
application, Socket Mode handler, and Web client imports during the build.
This prevents a missing optional package from silently disabling the upstream
Slack adapter; it does not replace an authenticated installed-channel journey.

The TUI uses Node **22.23.3** via `HERMES_NODE=/opt/node22/bin/node`. Its ARM64 and
x64 downloads are verified against `sandbox-images/hermes/node22-SHASUMS256.txt`;
update the version and both official checksums together. Only `bin/node` and the
upstream license are extracted. The system `nodejs`/`nodejs-npm` packages continue
to supply Node/npm on `PATH` for Hermes dependency probes and other tools; the
unused second npm dependency tree is not installed. Run the focused image-input
guards with:

```bash
python3 -B -m unittest discover -s runtimes/hermes/tests -p test_image_dependencies.py -v
```

These guards do not replace native runtime qualification or the image vulnerability
gate after an image rebuild.

The maintained Dockerfile separates the dependency base, pure-Python adapter wheel
build, and final runtime install. Full builds remain the default. For an
adapter-only refresh, `HERMES_RUNTIME_BASE` may reference a previously qualified,
digest-pinned AL3 Hermes image **only when all dependency, upstream harness, AGT,
Node and system-image inputs are unchanged**. Supply a source-bound adapter wheel
through the `adapter-build` named build context, under `opt/kars-adapter-wheels/`,
to package with networking disabled. Do not reuse the base for dependency changes.

The final stage replaces obsolete adapter source and staged plugin files, reinstalls
only the adapter wheel without an index or dependency changes, and runs `pip check`.
Refresh contexts must include the complete adapter source tree, including
`plugin/plugin.yaml`, in both wheel and staged-plugin inputs. An explicit manifest
copy rejects incomplete contexts at build time. `test_sre_native.py` additionally
exercises cold native CLI discovery from the staged tree: enabled SRE exposes only
its ten tools, while disabled plugins and unknown toolsets remain rejected.
The copied entrypoint and both Python import/plugin-discovery paths must be verified
against current source in the resulting image. This is build-time packaging, not
runtime installation; a running sandbox still cannot fetch or install its adapter.

## Default workspace initialization

Hermes keeps `/sandbox/agent` as its working directory, including for entrypoint
and command overrides. A fresh volume mounted at `/sandbox` hides the directory
ownership baked into the image; the container runtime may recreate the missing
working directory as root. Before starting Hermes, the entrypoint replaces an
**empty, unwritable** default workspace using the sandbox identity, then re-enters
it. This requires a writable parent volume and does not elevate privileges.
Existing writable directories, populated directories, read-only mounts, and custom
working directories are preserved; no recursive deletion or permission changes
are performed. Explicitly read-only agent-code mounts remain read-only.

Run the unprivileged filesystem regression tests with:

```bash
python3 -B -m unittest discover -s runtimes/hermes/tests -p test_workspace_startup.py -v
```

## What it does

| Capability | How |
|---|---|
| **AGT policy gate** | `ctx.register_hook("pre_tool_call", ...)` — every tool call POSTs `/agt/evaluate`; denied calls return an error result to the LLM without executing |
| **Sub-agent spawn** | `kars_spawn`, `kars_spawn_status`, `kars_spawn_destroy`, `kars_spawn_list` — HTTP to `/sandbox/*` on the router |
| **Peer discovery** | `kars_discover` — `/agt/registry/v1/discover?capability=<name>&limit=50` name search; `/agt/registry/v1/agents/{did}` direct lookup |
| **Mesh messaging** | `kars_mesh_send`, `_inbox`, `_await`, `_transfer_file` — end-to-end encrypted (Signal Protocol) via the Python AGT MeshClient (`runtimes/agt-mesh-python/`); the router bridges opaque ciphertext only. Exercised by `tests/e2e/interop/hermes_openclaw_bidi.sh` |
| **Handoff** | `kars_handoff_request`, `kars_handoff_confirm`, `kars_handoff_status` |
| **Foundry tools** | `foundry_code_execute`, `foundry_download_file`, `foundry_image_generation`, `foundry_web_search`, `foundry_file_search`, `foundry_memory`, `foundry_conversations`, `foundry_evaluations`, `foundry_deployments`, `foundry_agents` — all proxied through router |
| **Memory binding** | `foundry_memory` uses store name `memory-${SANDBOX_NAME}` per the KarsMemory convention |
| **HTTP fetch** | `http_fetch` — routes through `/egress/fetch` for egress allowlist enforcement |
| **Trust / signing telemetry** | After successful peer interactions: POST `/agt/trust` + `/agt/signing-counter` |
| **MCP** | Hermes' native MCP client consumes the `mcp_servers.*` block the entrypoint translates from `/etc/kars/mcp/<server>/meta.json` — no plugin code needed |

## Contract

The plugin honors [`docs/runtimes/CONTRACT.md`](../../docs/runtimes/CONTRACT.md) v1. Notable items:

- Reads `/etc/kars/secrets/admin-token` at plugin init for admin-scope endpoints
- Honors `KARS_DEV_PROFILE` for relaxed dev-mode behavior
- Skips Foundry tool registration when `KARS_PROVIDER` ∈ {`github-copilot`, `github-models`}
- Pre-seeds trust set from `AGT_TRUSTED_PEERS` (mesh trust bootstrap)
- Fail-closed grace period of 3 consecutive `/agt/evaluate` failures (`KARS_AGT_EVALUATE_FAIL_OPEN_GRACE` env override)

## Mesh parity

`kars_mesh_*` tools run on a Python AgentMesh `MeshClient` (`runtimes/agt-mesh-python/`) at byte-for-byte wire parity with the TypeScript SDK, so a Hermes agent and an OpenClaw agent are first-class encrypted-mesh peers (proven by `tests/e2e/interop/hermes_openclaw_bidi.sh`). The Signal session lives in the agent process; the router only bridges ciphertext.

### Mesh authentication modes

- **Required Entra:** `MESH_AUTH_BACKEND=EntraAgentIdentity` requires a fresh router
  `/v1/mesh-token` request, regardless of any supplied `AGT_OAUTH_TOKEN`. Startup
  preflight and subsequent plugin initialization each mint independently; an
  environment token never bypasses required authentication. Bounded preflight
  retry exhaustion exits startup without lowering trust. `AGT_SKIP_ENTRA=1`
  conflicts with this backend and is rejected.
- **Explicit Kind/no-Entra:** leave the required backend unset and set
  `AGT_SKIP_ENTRA=1`. Startup and the plugin discard stale OAuth tokens, skip
  verification, and use trust threshold zero. Without the required backend or
  explicit skip, the existing optional/best-effort authentication remains.
- **SRE:** mesh remains disabled, including token acquisition and the inbox worker.

The existing singleton uses `$HERMES_HOME/.agt/identity.json`, the exclusive
`.mesh-prekeys.lock`, and the loopback router at `127.0.0.1:8443` (ignoring HTTP
proxy environment settings). Required mode validates the mint response's Bearer
format, integer `expires_in`, and JWT `exp`; JWT decoding only bounds lifetime,
**not** authenticity. Effective expiry is the earlier TTL/JWT expiry minus 30
seconds, enforced by both wall and monotonic clocks. Each socket must complete a
nonce-bound self-routed KNOCK and canonical
`POST /agt/registry/v1/registry/verify` with
`{"amid":"<did>","verification_token":"<token>"}`. Verification requires HTTP
200, the exact DID, `tier: "verified"`, and nonempty `verified_app_id` and
`verified_tenant_id`, not merely a successful HTTP status.

The pinned AGT relay does not emit connect or heartbeat ACKs. The internal
self-KNOCK confirms authenticated relay routing; it is consumed internally and
is **never peer delivery or mission acceptance evidence**. Up to 128 offline
frames / 1 MiB may be buffered before verification, but no application callback
or inbox listener sees them until verified. Handshake/verification is bounded to
10 seconds. Registration/socket establishment necessarily precede canonical
verification, but client publication and application readiness do not.

At 80% of remaining effective lifetime, renewal fresh-mints, reconnects only the
existing WebSocket, and verifies again. Established socket closure likewise
requires a fresh mint/probe/verify before receiving or sending again. Identity,
prekeys, ratchets, the exclusive lock, and sole inbox listener are retained.
**Renewal/reconnect cancels active peer/native mission work and discards old queued
messages.** Authorization epochs fence late admissions, provider progress,
registry lookups, and replies; a retained mission nonce replays its failed terminal
rather than executing again. There is no anonymous fallback or continued work
under stale authorization. Expiry, mint/verification rejection/outage, failed
reauthentication, or send/heartbeat failures revoke readiness and unpublish.

HTTP completion after timeout cannot restore readiness. Teardown cancels tracked
handlers/listeners and closes the relay/registry before releasing the writer
lock. Cleanup waits at most five seconds per attempt; cancellation-resistant
work or incomplete socket closure stays quarantined with the lock held. A later
getter must finish teardown before retrying the same SDK-cached client; it also
restarts a previously failed/completed worker. Plugin listener intent is retained
across failed eager initialization, so the first successful tool retry starts it. No new client or lock bypass is
introduced. Graceful process exit uses `atexit` without replacing native Hermes
signal handlers; this does not promise cleanup under SIGKILL. These guarantees
are locally tested, not live Entra or deployed mission acceptance.

### Installed mission adapter: native and historical reviewed execution

`KARS_MISSION_DISPATCH_ENABLED=true` enables a strict `mission:*` receiver on the
**existing** verified mesh client and inbox worker. The worker routes reserved
mission traffic before generic file transfer or `hermes -z` handling; malformed,
wrapped, unauthorized, or unsupported envelopes never fall through to either.
When disabled, reserved mission traffic is discarded without readiness replies.
In installed mission mode the shared MeshClient partitions authenticated,
decrypted ingress: only the existing worker consumes reserved mission envelopes;
`kars_mesh_inbox` and `kars_mesh_await` consume ordinary peer payloads intact
(including OpenClaw `task_request` envelopes), without auto-responding or saving
files on the worker path. Each partition holds at most 128 messages: a full peer
queue drops its oldest message with a warning rather than blocking mission
custody; the reserved queue applies backpressure. Authorization suspension clears
both queues. Await selects registry-resolved sender DIDs, leaves other senders
queued, and returns `completed: false` on timeout (maximum 300 seconds). Names
not yet registered are resolved again once per second within that same deadline;
already-resolved names remain bound to their original DIDs for the wait. Messages
from late-registering peers stay queued until their name resolves. Without
senders it waits for one peer message. Non-mission worker/file-transfer behavior,
channels, and SRE are unchanged. No additional MeshClient, prekey writer,
scheduler, or mission record store is introduced.

Execution uses the pinned **genuine** `run_agent.AIAgent.run_conversation` in a
clean per-assignment child process, not a custom OpenAI loop or CLI stdout.
The child imports the canonical installed `kars_runtime_hermes` package even when
the receiver was loaded from `$HERMES_HOME/plugins/kars`. It inherits neither the
parent mesh identity/OAuth token nor plugins. The signed phase selects execution
semantics; an absent `executionMode` retains the historical exact-reviewed contract
and canonical digest. It is not silently upgraded.

Native read-only GitHub App access uses the existing `http_fetch` tool, not a
separate GitHub read tool or MCP server. With reviewed `network` capability and an
authorized repository binding, call canonical
`https://api.github.com/repos/{owner}/{repo}` GET URLs without credentials, custom
headers or a body. The router supplies scoped authentication. For metadata-only
work, use repository metadata and the paginated `/branches` list, matching
`default_branch` to `commit.sha`; `/branches/{branch}` and `/git/ref/heads/{branch}`
are unsupported. No additional direct egress, GitHub MCP or `github-pr` capability
is needed. Guidance grants no authority: preserve assignment scope and actual
denials instead of claiming unobserved private reads. Explicitly requested GitHub
MCP retains its separate binding and credential requirements.

For native phases with `network`, the existing subprocess pipes also forward
`kars_mesh_send`, `kars_discover`, `kars_mesh_inbox`, and `kars_mesh_await`.
These are real native registry tools, accounted and authorized alongside existing
tools. Newline-framed stdin stays open for correlated responses; one parent stdout
reader handles progress, tool requests, and terminal results. Requests contain only
an operation, arguments, and sequential ID—not selectable identity or assignment
authority. The parent rechecks active assignment, ledger custody, singleton, verified
auth generation, and canonical router policy before invoking its existing async
mesh handlers and registry connection. Reserved Mission envelopes cannot be sent
or consumed through this bridge. No child mesh client, identity, or responder is loaded.
Name discovery uses AGT's `/v1/discover` capability search and translates its
`results` list to the tool's `agents`/`count` response; DID lookup remains direct.
Both regular and native tools use this protocol through the router. Malformed
search responses report errors rather than silently claiming no peers exist.
Native discovery requires exactly one non-null name or canonical DID. The unused
field may be omitted or explicitly null, including when a provider's strict tool
schema requires both fields. Two populated targets and two absent targets are rejected.
Discovery results alone do not prove that a peer is executing or has replied.
New reviewed-run v2 reservations provide the independently verified v5
`peerContext` mapping to the native prompt. Planned role/phase locators are not
identity, readiness, or permission; existing discovery and parent-owned encrypted
transport remain authoritative. Retained reviewed-run v1 reservations keep their
original v4 contracts. See [shared contract compatibility](../../mesh-plugin/README.md#reviewed-mission-peer-context).

Frames are bounded by the existing wire limit, requests by the signed tool budget,
peer results by 160 KiB, send content by 64 KiB, and waits by 300 seconds within the
reviewed native overall deadline. Oversized receive results remain queued and
report `truncated`; unmatched senders remain queued. EOF, invalid/pipelined frames,
cancellation, and deadlines cancel pending operations and retire the child. Local
regressions exercise genuine Hermes native dispatch in a clean subprocess with
real encrypted parent send/receive; provider/router boundaries are test fixtures.
Image packaging and live cross-runtime Team acceptance are separate qualifications.

With `reviewedPhase.executionMode: "native"`, the actual Hermes registry supplies
`read_file`, `search_files`, and `write_file` schemas and implementations when their
filesystem capabilities are granted. Guards wrap those handlers for router policy,
workspace containment, immutable inputs/skills, and measured results; Hermes retains
its own conversation, dispatch, paging, file-search, and write behavior. The agent
chooses useful tools and their order: there is no prescribed read/fetch/write sequence,
reserved write slots, or forced minimum-success tool selection. Completion checks
actual exports and unchanged input bytes, not whether an artificial read occurred.
The assignment prompt pairs each immutable artifact name with its exact readable
`inputs/<name>` path; the artifact name alone is not a filesystem path. This keeps
handoff provenance distinct from file access without adding aliases or permissions.
Native context-file handling runs in the clean assignment workspace; ambient plugin
loading remains absent. Peer discovery is explicit through the authorized parent bridge. Only approved skill bytes are materialized there.

Native `network` grants agent-selected public HTTPS GET calls through the existing
`http_fetch` router integration, not a fixed URL enumeration. `requiredToolCalls` can
be empty; explicitly supplied calls remain completion obligations. Router auth,
signed host restrictions, SSRF checks, redirect policy and accounting are unchanged.
HTTP status/headers/body and truncation are returned for reasoning, including non-2xx,
empty and up-to-2-MiB bodies; such data alone does not fail the Mission. A required
fetch is credited only for its matching complete 2xx response. JSON envelopes and
native inference request histories are bounded to 16 MiB. Provider replies retain
the existing 256-KiB bound. No opaque CONNECT exception or runtime download/install
bootstrap is introduced.

Currently supported native capabilities are **filesystem-read, filesystem-write,
network, and memory**. Files are UTF-8 text, at most 65,536 bytes each. Writes are confined
to `exports/` and `scratch/`; scratch is at most 64 files / 1 MiB. Inputs and approved
skills remain immutable; paths cannot escape the assignment or traverse symlinks.
The existing native `write_file` also accepts `{path, source_path, sha256}` instead
of `{path, content}` when both filesystem-read and filesystem-write are reviewed.
Use `inputs/<name>` from the immutable assignment manifest or this invocation's
successful write path (for example `scratch/server.py`), and its explicit complete
`sha256:<64 lowercase hex>` digest. Copy to `exports/server.py` to pass through
builder/QA-tested bytes without model transcription. Only retained source bytes
are used, never arbitrary host files, approved skills, spills or runtime receipts.
Additional native writer options, including append, are not accepted for copies.
A matching digest proves byte identity, not that tests passed.

This is the same authorized and correlated writer, not a separate tool or implicit
export. Original source/digest arguments go to governance; resolved content goes
to the native writer and its persisted bytes are verified. Existing path authority,
reserved receipt names, 16-file/128-KiB serialized export envelope, file/scratch
limits and content-write support remain intact. Failed named writes/copies must be
corrected before completion; copied exports are digest-checked again at collection.
Unused alternative fields may be omitted or null for strict-provider schemas;
non-null content cannot be combined with source/digest arguments.

Only flat, valid output names can become exported artifacts. Native document
extraction/OCR, patch (including multi-file edits and syntax subprocesses), shell,
uncontrolled local memory, hosted search and delegation are not granted. Network phases
also expose the existing `kars_mcp_list` and `kars_mcp_call` router bridge tools,
with per-call authorization and outcome reporting. The model chooses when to use
them. Configured server bindings, tool policy and credentials remain router-owned;
a network grant does not enroll a server. Bridge failures do not count as successful
calls or cause automatic call replay. Original registry handlers are restored when
the assignment ends. Compression
and auxiliary model calls remain disabled until they have measured transport and
funding integration; this is not a claim of unrestricted Hermes feature parity.

Native **memory** explicitly exposes only the existing governed `foundry_memory`
client, without loading the full Foundry catalog or granting shell/auxiliary tools.
A network grant alone does not expose memory; a memory-only phase needs no network
capability. Its reviewed schema requires `operation: search | update`. Omit `scope`
or use `null` to select the router's configured bound private scope, including with
providers that require every schema property. Explicit scope strings must be nonempty
and exactly authorized; never invent a mission scope or use the literal `private`.
The schema accepts optional string `text`/`query` and positive safe-integer
`top_k` (at most 9,007,199,254,740,991). Updates require nonempty `text`;
`delete_scope` and extra arguments are rejected. The router owns current scope
binding, provider access, policy and funding. Native memory does not infer access
from the agent's legacy `KARS_PROVIDER` or `FOUNDRY_PROJECT_ENDPOINT` environment:
the selected inference provider is independent of the router's memory backend.
Missing router bindings or provider configuration, router refusal, malformed
arguments and tool failures are reported truthfully; there is no local-file fallback. Every dispatched call uses existing reservation,
authorization and outcome accounting, with cancellation and authority guards intact.
The original memory registration is restored after execution (or removed if this
run introduced it).

A successful memory submission may return **queued**, which counts as a successful
tool call but is **not completed persistence**. The original result, status and
update identity are preserved for both queued and completed outcomes. Failed calls
remain failed evidence; the adapter does not automatically replay or poll updates.
Uncontrolled Hermes local memory remains disabled with `skip_memory=True`.

Without native mode, `memory` is rejected and the registered tools remain `kars_mission_read`,
`kars_mission_write`, and exact-reviewed `http_fetch`. The following sequencing,
64-KiB fetch, and reserved-slot rules describe that historical mode only; accounting,
wire bindings, immutable artifacts and finite ceilings apply to both modes.

Supported contracts and limits:

- Wire contract versions **1–5**, canonical reviewed-phase/input SHA256 binding,
  `text-v1` named UTF-8 artifacts and required output names. Reviewed phases may
  request `filesystem-read` / `filesystem-write` and optional `network`. Network
  requires 1–8 unique `requiredToolCalls` shaped as
  `{"name":"http_fetch","arguments":{"method":"GET","url":"https://example.com/page"}}`.
  Only those exact URLs are executable: lowercase public DNS hostname, explicit
  slash/path, no credentials, port, fragment, dot segments or non-ASCII URL bytes.
  Query strings are bound exactly. No headers, bodies or other methods are accepted.
  The existing `/egress/fetch` route retains blocklist/allowlist checks, redirect
  refusal and its timeout/body limits. The reviewed adapter additionally requires
  outer HTTP 200, inner 2xx, nonempty UTF-8 content at most 65,536 bytes, and
  `truncated:false`, within a 512 KiB router response envelope; oversize, refused,
  failed and redirected responses never count.
  Immutable inputs are read first; all required fetches must succeed before writes.
  Each distinct required URL contributes once to `completedRequiredToolCalls`;
  retries consume attempts without duplicating required-call credit. Exports and
  pending fetches retain reserved slots within the original ceiling. Fetched text
  is untrusted reference data, never authority. No credentials or provider calls
  are added. Hosted `web-search` stays unsupported because finite funding excludes
  hosted search; proposed search requirements are rejected, never deleted or downgraded.
  Synthesis stays filesystem-only. Unsupported capabilities fail before readiness.
- The receiver negotiates `router-observations-v1` activity. It forwards only
  the shared router recorder's selection for scope/round IDs returned by this
  assignment's inference responses. Invalid, expired, or unavailable selections
  are explicitly unavailable, never fabricated empty traces. Named artifact
  delivery also retains the final selection when artifact capacity permits.
  A failed terminal retains the last validated progress selection as diagnostic
  Activity, including when later progress supplies only usage. Failure does not
  publish deliverables, permit result review, or invent observations discarded by
  an older runtime. The shared dispatcher validates negotiation and persists only
  the reserved observation document for such failures.
  Before filesystem access or reviewed fetch, native callbacks require an explicit positive
  `/agt/evaluate` decision, binding the exact tool name and call ID to the
  returned scope when available; fetch authorization includes exact URL and method arguments,
  and native file authorization includes actual arguments. This gate applies even without negotiated
  activity; denial, malformed replies and transport failures prevent access.
  The same authorized call reports its outcome to the shared recorder before the next
  inference call. Returned-response selections retain immediate reports only when the
  exact call ID, tool name and proposing round match an earlier selected proposal.
  This preserves final-tool outcomes and results after request history exceeds the
  recorder's bounded history scanner. If a later selected conversation report exists,
  it replaces the immediate record (including contradictory outcomes), rather than
  counting the same call twice. Neither request-size limits nor execution authority change.
  Historical file tools report read/write byte counts; native writes
  report verified persisted bytes and export names. Native reads/searches do not
  invent whole-file read evidence for paged or filtered results. Bounded failure
  categories are reported without file contents. These are
  explicitly harness-reported observations, not independent enforcement proof;
  missing correlation or a refused recorder write never fabricates a result or
  changes the filesystem operation's outcome.
- At most **8 actual inference HTTP attempts**, including native finalization
  and finalization retries; at most **32 attempted tool invocations** (or the
  smaller reviewed bound), checked across the whole batch before execution.
  Native streaming, context compression, fallback models, and parallel tool
  execution are disabled. Both modes disable parallel tool generation.
  Native requests state the remaining allowance and verified missing phase exports on
  each model request, including finalization retries, without forcing tool choice or order.
  Only successful byte-verified native writes satisfy that progress reminder; final
  completion still validates the actual required files. No allowance is increased.
  Historical exact-reviewed requests state the remaining allowance and missing phase exports.
  The last required write slots are reserved for those exports; exhausted
  allowances remove tool definitions and tool-selection fields and request a
  final answer limited to completed work, never waive missing inputs or exports. The same
  instruction applies to native empty-response retries. Oversized provider
  batches are still rejected before any execution.
  Calls use only the selected model at
  `http://127.0.0.1:8443/v1/chat/completions`.
- Progress and terminal evidence contain measured calls, successful/attempted
  tools, and provider-reported token usage. Missing/invalid usage stays unknown,
  never becomes a token estimate, and cannot establish success. A successful
  result requires known positive usage, the minimum successful tool count, and
  every required nonempty named export. HTTP rejection errors preserve the status
  and, when available, the previous completion's allowlisted finish reason,
  text length, tool count, and refusal flag—not provider text or error bodies.
  Rejected requests remain unmeasured and cannot establish success.
  An SDK transport ACK is not acceptance.
- Inputs are digest-checked, written as read-only files and byte-checked after
  execution. Tools access only named regular files under `inputs/` / `exports/`;
  symlinks, directories and FIFOs are rejected. Limits are 64 KiB per file,
  64 KiB aggregate input envelope, 16 exports, and 128 KiB serialized exports.
  `response.md` is valid as an input but reserved as an export name.
  Python reserves execution-receipt capacity before launching, using the exact
  manifest, arguments and identity plus worst-case full 2048-byte streams and both
  JSON serialization layers. The 64-KiB receipt and 128-KiB export bounds stay unchanged;
  reservation whitespace preserves previous runs without inventing execution results.
  If existing exports
  leave insufficient space, the tool reports that execution has not started and
  guides the agent to shorten exports or execute before exporting large documents.
  Retained inputs need not be republished; no permissions or spending change is needed.
- One active assignment; a non-evicting 128-entry nonce/digest ledger; exact
  retries replay retained progress/terminal replies rather than re-executing.
  Modified replays and stale boot IDs are rejected. Admission authorization
  must succeed again immediately before native execution. The child has a
  reviewed allowance-derived overall deadline and progress-event bound for native
  phases; historical phases retain 600 seconds and 128 events (plus reviewed peer
  requests where applicable). Only its owned workspace is removed on completion/failure.

#### Controller / dispatcher integration contract

Controller and shared dispatcher wiring now select the trusted Task runtime
(`blueprint.runtime`, then `execution.runtime`, defaulting to OpenClaw) and require
a matching sandbox runtime and exactly one selected `agent` container. Reviewed
role and phase reservations preserve execution-only Hermes selection. The existing
filesystem/direct-fetch subset supports immutable handoffs and required synthesis outputs;
unsupported capabilities remain closed. Runtime selection is candidate metadata,
not new assignment-wire authority.

Local controller, dispatcher and cross-language custody tests cover both runtimes,
including actual reconciler-generated fixtures. They do not establish deployed
mission, genuine Entra, or complete customer-journey acceptance. Installed wiring
supplies:

| Environment | Requirement |
|---|---|
| `KARS_MISSION_DISPATCH_ENABLED` | Exactly `true` to enable; SRE still disables mesh |
| `KARS_MISSION_TASK_NAME`, `KARS_MISSION_TASK_UID` | Exact admitted task identity |
| `KARS_MISSION_SANDBOX_UID`, `KARS_MISSION_POD_UID` | Exact sandbox and current pod identities |
| `KARS_MISSION_DISPATCHER_DID` | Expected encrypted sender, `did:mesh:` plus 32 lowercase hex digits |
| `KARS_MISSION_IDENTITY_ROOT` | Secret-backed 64-hex-character root, never agent-visible prompt material |
| `KARS_MISSION_CONTRACT` | Strict JSON contract version 1–5 matching the reviewed assignment; 8 KiB for v1–v4, 32 KiB for v5 including up to 64 peer locators |
| `KARS_MISSION_ADMISSION` | Strict JSON version-1 admission binding generation, authorization digest and run nonce/objective digest |
| `KARS_MODEL` or `AZURE_OPENAI_DEPLOYMENT` | Exact nonempty selected model; must match evidence |
| `MESH_AUTH_BACKEND`, `AGT_OAUTH_TOKEN`, `AGT_SKIP_ENTRA` | Required-Entra / explicit Kind rules above; no implicit downgrade |

The runtime signing seed is HMAC-SHA256(root,
`JSON.stringify(["kars-mission-identity-v1","runtime",sandboxUid,podUid])`),
matching the shared runtime DID derivation. Its identity is stored mode 0600 at
`$HERMES_HOME/.agt/missions/<podUid>/identity.json`; the sole SDK prekey lock is in
that directory. A process-level loopback socket at **127.0.0.1:19791** prevents
another local Hermes process from claiming mission custody. Parent wiring must
select the actual non-OpenClaw container **`agent`** (not `openclaw` or `hermes`),
as defined in `controller/src/reconciler/mod.rs`. Existing image packaging already
installs the canonical runtime package and stages the Hermes plugin; the agent
runs as UID 1000 beside the router on port 8443. No image layout change is needed.

Dispatchers send encrypted `mission:probe` with the complete target, run nonce,
contract and challenge; readiness echoes them with a per-process `bootId` and
the offered activity capability. The shared dispatcher negotiates activity for
both runtimes; older receivers may omit the capability. Router selections are
bounded to 32 KiB and strip prompt, argument, and result contents. Tool outcomes
retain the router's `harness-reported` provenance, not an enforcement claim.
Measured progress and terminal evidence still belong to the exact named attempt.
`mission:assign` must retain that boot ID and carry
its assignment ID, exact admitted objective and optional immutable inputs. Named
exports require `artifactFormat: "text-v1"`. Immediately before acceptance the
receiver calls router `POST /agt/evaluate`, action `task:execute`, with
`task_uid`, `run_nonce`, `dispatcher_did`, `assignment_id`, `phase_digest`,
`reviewed_phase`, and `input_digest`; only bounded HTTP 200 JSON `allowed: true`
authorizes execution. Roles, DAG scheduling, synthesis and immutable input
provenance remain controller/shared-dispatcher responsibilities.

Required phase exports are minimum obligations, not an exclusive output list.
Internal phase guidance tells application builders to write changed source, tests,
and configuration under `exports/` after testing, alongside required handoffs.
Paths, hashes, and execution receipts do not retain those files for downstream
review. Unchanged inputs need not be republished; artifact limits and role authority
remain unchanged, and failed tests must remain reported as failed.

Persisted attempts are recovered before fresh-execution admission. Losing the
original Pod or binding cannot authorize another execution, but terminal evidence
can still be published when Task intent is unchanged. Changing the Task runtime
preserves immutable evidence without completing the changed Task. Exact retries
never send a second assignment.

#### Native API provenance and acceptance boundary

The image recipe now selects genuine Hermes `0.21.5`, Python `>=3.11,<3.14`.
The mission adapter also supports deployed `0.16.0`; both bootstrap
and native execution reject unqualified patch and prerelease versions. Retain
0.16.0 compatibility until the refreshed image passes installed acceptance and replaces the shipping image,
then remove that version from the shared runtime guard.

Hermes 0.21.5 keeps tool schemas in its iteration-limit summary requests for
provider cache reuse. The adapter removes `tools`, `parallel_tool_calls`, and
`tool_choice` while finalizing: Azure OpenAI rejects a tool-selection field when
no tool definitions are supplied. It still rejects tool calls in summary
responses, including otherwise permitted filesystem tools. Normal tool dispatch
still uses the genuine Hermes implementation. Summary attempts and retries share
the same phase-derived measurement boundary as normal calls; legacy phases retain
the eight-request ceiling. Reaching the iteration limit does not become a
successful mission merely because a summary was returned.

The official 0.16.0
[release source](https://github.com/NousResearch/hermes-agent/tree/3c231eb3979ab9c57d5cd6d02f1d577a3b718b43)
(`v2026.6.5`) and the installed 0.16.0 distribution provide these concrete hooks:

| Installed native source | Integration point |
|---|---|
| `run_agent.py:4859` | `AIAgent.run_conversation` delegates to the genuine conversation loop |
| `run_agent.py:3304,3357,3755` | `_ensure_primary_openai_client`, `_create_request_openai_client`, `_interruptible_api_call` |
| `agent/chat_completion_helpers.py:1265,1430,1473` | `handle_max_iterations` uses separate `iteration_limit_summary` / `iteration_limit_summary_retry` clients; the shared guarded HTTP transport bounds both |
| `run_agent.py:4770,4849`; `agent/tool_executor.py:690` | `_execute_tool_calls` and genuine sequential execution; raw response allowlisting also catches tools native filtering would otherwise hide |
| `tools/registry.py:234,390` | Native `register` and `dispatch` for filesystem tools and reviewed `http_fetch` |
| `agent/conversation_loop.py:1879–1994` | Native provider usage handling; adapter measures response usage directly and never uses estimated cost at line 1937 |

Native-mode hook verification additionally uses the exact shipping source
[`f97608f178d1ffeca59860195ab7da295f7c8e5f`](https://github.com/NousResearch/hermes-agent/tree/f97608f178d1ffeca59860195ab7da295f7c8e5f):
`tools/registry.py` (`get_entry`, `get_definitions`, `register`),
`tools/file_tools.py` (the three file handlers), `tools/read_extract.py`
(document/auxiliary side channels), `agent/agent_init.py`,
`agent/tool_executor.py`, and `agent/prompt_builder.py`. Registry/file-handler
compatibility was also checked against the exact 0.16.0 release source above.
The 0.21.5 callsites are:

| Pinned native source | Used API / hook |
|---|---|
| `agent/client_lifecycle.py:308,437` | `_ensure_primary_openai_client(*, reason)`, `_create_request_openai_client(*, reason, api_kwargs=None)` |
| `run_agent.py:1253,1267,1392,1393` | Forwards for `_interruptible_api_call`, `_build_api_kwargs`, `_execute_tool_calls_sequential`, `_handle_max_iterations` |
| `agent/chat_completion_helpers.py:1229,1472,2263,2275` | Actual request/kwargs/iteration-summary implementations; summary uses the guarded primary-client hook |
| `agent/turn_facade.py:22,146` | `run_conversation` and delegation into the genuine conversation implementation |
| `agent/tool_executor.py:991,1648,1784` | Start callback `(call_id, function_name, display_args)`, `model_tools.handle_function_call`, sequential executor |
| `tools/registry.py:182,498,662,837,935,939` | `ToolEntry`, `get_entry`, `register`, `get_definitions`, `get_schema`, `get_toolset_for_tool` |
| `tools/file_tools.py:1290,1295,1333,1371,1372,1396` | Genuine read/write/search handlers and their `file` toolset registrations |
| `agent/agent_init.py:2323–2347` | Constructor options including toolsets, callbacks, context and memory switches |
| `agent/compression_facade.py:221` | `_compress_context`; deliberately halted rather than allowing unmeasured auxiliary execution |

Source inspection is not runtime acceptance. `test_mission_native_guards.py` runs
without Hermes and verifies local authority/measurement boundaries; the native-mode
cases in `test_mission_native.py` must execute against the genuine pinned package
before image or deployed acceptance. Without `run_agent`, that module is skipped,
not counted as passing native execution. No replacement SDK or mock loop is installed.

When the real native package is available, local tests execute it, mocking only the
provider/network boundaries for native execution and connected SDK tests. They
cover real child startup, filesystem custody, actual native tools/finalization,
provider failures/unknown usage, strict contracts, admission denial, replay,
retained sends, and required-Entra teardown/retry. Node tests verify the canonical
digests against shared TypeScript and parse actual receiver replies with the
shared dispatcher parser. Regression coverage includes generic transfers,
auto-responder-disabled file persistence, SRE, package shape and startup auth.

This is **local native/provider-mocked acceptance**, not a claim of image-level,
live encrypted cross-runtime, real Entra, live inference, or deployed mission
acceptance. Those remain required after parent controller/authority integration.

#### Native-mode integration verification (2026-10-17)

Native tool names are exactly `read_file`, `write_file`, `search_files`, and
`http_fetch`, gated by the phase capabilities. Each authorized invocation sends
`POST http://127.0.0.1:8443/agt/evaluate` with:

```json
{"action":"tool:<tool-name>","context":{"tool_call_id":"<native-call-id>","tool":"<tool-name>","arguments":{},"scope_id":"<returned-router-scope>"}}
```

`arguments` is the actual model argument object, not a summary: native paths are
still as supplied before assignment-root resolution; write arguments include
content; fetch arguments contain the explicit `method:"GET"` and selected `url`.
`scope_id` is omitted until a valid returned router scope is known. The action
verbs are `tool:read_file`, `tool:write_file`, `tool:search_files`, and
`tool:http_fetch`. Authorization requires HTTP 200 and `allowed:true`; response
JSON is bounded to 64 KiB. Outcomes are reported separately to `/telemetry/tool`
with `scope_id`, `call_id`, `ok`, bounded `latency_ms`, and `result_summary`.

Native GET uses `http_fetch.fetch_response`, not the standalone plugin's 32-KiB
text-return path. It sends `POST /egress/fetch` with the selected URL, `method:GET`,
empty headers and body. Outer HTTP 200 must contain an envelope with integer
`status` 100–599, object `headers`, string `body` (at most 2 MiB UTF-8), and boolean
`truncated`. The serialized envelope is bounded to 16 MiB; transport timeouts are
connect 5s/read 35s/write 10s/pool 5s. No redirect is followed by the adapter.
Upstream status 403 is returned unchanged as data. Outer router HTTP 403 returns
`{"error":"Fetch refused by router (HTTP 403)"}`. Neither sets a terminal Mission
failure or credits a required fetch. Over-bound/invalid envelopes also return tool
errors without terminating the loop. Empty, large (within bounds), and truncated
sources remain agent-visible data. Only 2xx increments successful tool count;
only exact required-argument matches with nontruncated 2xx satisfy explicit fetch
obligations. Native inference requests can carry up to 16 MiB JSON; the measured
provider response remains bounded to 256 KiB. Native phases derive their inference
ceiling from `maxToolCalls + 2`: sequential tool rounds, a subsequent answer, and
one reserved finalization attempt. The floor remains eight; optional observation
retention is not an execution ceiling. Only the first 32 returned round identifiers
are retained for export; later valid responses continue to authorize tools while
Activity is explicitly unavailable. Scope and strictly increasing round identifiers
remain checked even after the retention window. This is an upper
bound, not a required sequence or guaranteed allowance: router funding, tool
limits and worker deadlines still apply, and exhausted/incomplete conversations
still fail. Transport, native loop and evidence validation use the same limit.

Native `_build_api_kwargs` returns before all legacy read/fetch/write forcing and
minimum-count `tool_choice:"required"` logic. An explicit signed `minToolCalls`
remains checked by canonical completion evidence; proposing zero does not create
a synthetic requirement. No agent actions are forced to meet a floor. Tool
definitions are removed only on finalization or when the actual attempt cap is
exhausted. Normal context files are enabled; uncontrolled Hermes local memory
remains disabled. Explicit governed native memory is documented above.

The original **155 passed / 96 skipped** was the aggregate of these two commands
(from `runtimes/hermes`), not a single native-runtime acceptance run:

```bash
PYTHONPATH=src python3 -m pytest --basetemp=../../.local/hermes-native-tests \
  tests/test_mission_native_guards.py tests/test_mission_native.py \
  tests/test_mission_protocol.py tests/test_foundry_http_fetch.py \
  tests/test_mission_observations.py -q
# 122 passed, 96 skipped
PYTHONPATH=src python3 -m pytest --basetemp=../../.local/hermes-native-tests \
  tests/test_mission_tool_reporting.py -q
# 33 passed
```

After preserving the native router-denial HTTP status and adding its regression,
the exact combined verification was:

```bash
PYTHONPATH=src python3 -m pytest --basetemp=../../.local/hermes-native-tests \
  tests/test_mission_native_guards.py tests/test_mission_native.py \
  tests/test_mission_protocol.py tests/test_foundry_http_fetch.py \
  tests/test_mission_observations.py tests/test_mission_tool_reporting.py -q -rs
# 156 passed, 96 skipped
```

All 96 skips have the **same** reason: `Requires native pinned Hermes` from the
module-level `importlib.util.find_spec("run_agent") is None` marker in
`test_mission_native.py`. This includes the five version-rejection cases covered
by that module marker; missing mesh metadata was not a separate skip reason in
these commands. No genuine native-loop case passed in this environment. The
actual test interpreter was `/opt/homebrew/opt/python@3.14/bin/python3.14`, outside
the pinned Hermes Python range, with neither `run_agent` nor `hermes-agent`
distribution metadata. No retained-runtime interpreter path was discovered or
used in this implementation pass; no dependencies were installed. Genuine pinned-runtime execution, router
policy acceptance of the four action verbs, and guarded Kind acceptance remain
unverified. `HERMES_NATIVE_PYTHON` below is a required caller-provided interpreter,
not an identified retained environment.

### Native governed-memory verification (2026-10-22)

The focused repair was checked from the repository root with:

```bash
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=runtimes/hermes/src python3 -m pytest \
  runtimes/hermes/tests/test_mission_memory.py \
  runtimes/hermes/tests/test_mission_native_guards.py \
  runtimes/hermes/tests/test_mission_protocol.py \
  runtimes/hermes/tests/test_mission_tool_reporting.py \
  runtimes/hermes/tests/test_foundry_http_fetch.py \
  runtimes/hermes/tests/test_foundry_native.py \
  runtimes/hermes/tests/test_mission_native.py \
  --basetemp=runtimes/hermes/.pytest_cache/memory-repair-20261022 -q
# 271 passed, 128 skipped
```

All 128 skips require the genuine pinned Hermes host (`run_agent` is absent locally).
This verifies guards, schema, registration restoration, router-client error/status
preservation and shared TypeScript digest parity, **not** native-host acceptance.
The four new native memory-only cases cover queued, completed, returned-error and
policy-denied outcomes. Genuine pinned-host execution with no skipped native cases
must pass before packaging; no deployed customer acceptance is claimed here.
Historical failure/skipped evidence above remains unchanged.

## Development

```bash
pip install -e runtimes/agt-mesh-python
pip install -e "runtimes/hermes[dev]"
python -m pytest runtimes/hermes/tests
```

Adapter-only environments skip tests requiring the genuine Hermes runtime; those
skips are not native qualification. Bootstrap unit fixtures supply only supported
version metadata, while separate guard tests reject missing or unqualified runtime
metadata. CI additionally installs the source commit selected by the image
Dockerfile, applies the same dependency/Foundry patches and provider override, and
checks the installed version and native module before running the complete suite.
This runs native tests on the hosted runner without a local image build; installed
AL3 image and customer-workflow acceptance remain separate gates.

Focused installed-mission acceptance (from the repository root) requires a Python
3.11–3.13 environment with genuine `hermes-agent==0.16.0` or `hermes-agent==0.21.5`, the shared Python mesh
dependencies, pytest/pytest-asyncio, and Node supporting TypeScript stripping.
Set `HERMES_NATIVE_PYTHON` to that environment's interpreter; no provider or mesh
network access is made by these tests:

```bash
PYTHONDONTWRITEBYTECODE=1 \
PYTHONPATH=runtimes/hermes/src:runtimes/agt-mesh-python/src \
"$HERMES_NATIVE_PYTHON" -m pytest \
  runtimes/hermes/tests/test_mission_*.py \
  runtimes/hermes/tests/test_mesh_auth*.py \
  runtimes/hermes/tests/test_router_client.py \
  runtimes/hermes/tests/test_mesh_worker.py \
  runtimes/hermes/tests/test_file_transfer_unconditional.py \
  runtimes/hermes/tests/test_mesh_transfer_file.py \
  runtimes/hermes/tests/test_sre.py \
  runtimes/hermes/tests/test_package_shape.py \
  --basetemp=runtimes/hermes/.pytest_cache/mission-acceptance -q
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=runtimes/agt-mesh-python/src \
"$HERMES_NATIVE_PYTHON" -m pytest runtimes/agt-mesh-python/tests \
  --basetemp=runtimes/hermes/.pytest_cache/shared-mesh-acceptance -q
PYTHONDONTWRITEBYTECODE=1 python3 -m ruff check \
  runtimes/hermes/src/kars_runtime_hermes/plugin/__init__.py \
  runtimes/hermes/src/kars_runtime_hermes/plugin/mission_*.py \
  runtimes/hermes/src/kars_runtime_hermes/plugin/mesh*.py \
  runtimes/hermes/src/kars_runtime_hermes/plugin/router_client.py \
  runtimes/hermes/tests/test_mission_*.py \
  runtimes/hermes/tests/test_mesh_auth*.py \
  runtimes/hermes/tests/test_router_client.py \
  runtimes/agt-mesh-python/src/kars_agt_mesh/client.py \
  runtimes/agt-mesh-python/src/kars_agt_mesh/relay_transport.py
bash -n sandbox-images/hermes/entrypoint.sh
```

To test against a running Hermes installation:
```bash
mkdir -p ~/.hermes/plugins
ln -s "$(pwd)/src/kars_runtime_hermes/plugin" ~/.hermes/plugins/kars
hermes plugins doctor
```
