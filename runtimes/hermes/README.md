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
cryptography to 2.15.1 and 50.0.2, and the Undici 6 override, TUI dependency
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
The copied entrypoint and both Python import/plugin-discovery paths must be verified
against current source in the resulting image. This is build-time packaging, not
runtime installation; a running sandbox still cannot fetch or install its adapter.

## What it does

| Capability | How |
|---|---|
| **AGT policy gate** | `ctx.register_hook("pre_tool_call", ...)` — every tool call POSTs `/agt/evaluate`; denied calls return an error result to the LLM without executing |
| **Sub-agent spawn** | `kars_spawn`, `kars_spawn_status`, `kars_spawn_destroy`, `kars_spawn_list` — HTTP to `/sandbox/*` on the router |
| **Peer discovery** | `kars_discover` — `/agt/registry/v1/agents/{did}` lookup |
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

### Installed mission adapter: constrained native subset

`KARS_MISSION_DISPATCH_ENABLED=true` enables a strict `mission:*` receiver on the
**existing** verified mesh client and inbox worker. The worker routes reserved
mission traffic before generic file transfer or `hermes -z` handling; malformed,
wrapped, unauthorized, or unsupported envelopes never fall through to either.
When disabled, reserved mission traffic is discarded without readiness replies.
Ordinary peer messages, automatic file transfers, channels, and SRE retain their
normal paths. In installed mission mode only, passive `kars_mesh_inbox` and
`kars_mesh_await` calls reject without draining messages: the worker must be the
sole inbox consumer. No additional MeshClient, prekey writer, scheduler, or
mission record store is introduced.

Execution uses the pinned **genuine** `run_agent.AIAgent.run_conversation` in a
clean per-assignment child process, not a custom OpenAI loop or CLI stdout.
The child imports the canonical installed `kars_runtime_hermes` package even when
the receiver was loaded from `$HERMES_HOME/plugins/kars`. It inherits neither the
parent mesh identity/OAuth token nor plugins. Native registered tools are limited
to `kars_mission_read` and `kars_mission_write`; no terminal, network, delegation,
MCP, memory, or interactive tools are authorized.

Supported contracts and limits:

- Wire contract versions **1–4**, canonical reviewed-phase/input SHA256 binding,
  `text-v1` named UTF-8 artifacts and required output names. Reviewed phases may
  request only `filesystem-read` / `filesystem-write`, with no named
  `requiredToolCalls`. Unsupported capabilities fail before readiness.
- The receiver negotiates `router-observations-v1` activity. It forwards only
  the shared router recorder's selection for scope/round IDs returned by this
  assignment's inference responses. Invalid, expired, or unavailable selections
  are explicitly unavailable, never fabricated empty traces. Named artifact
  delivery also retains the final selection when artifact capacity permits.
- At most **8 actual inference HTTP attempts**, including native finalization
  and finalization retries; at most **32 attempted tool invocations** (or the
  smaller reviewed bound), checked across the whole batch before execution.
  Native streaming, context compression, fallback models, and parallel tool
  execution are disabled. Calls use only the selected model at
  `http://127.0.0.1:8443/v1/chat/completions`.
- Progress and terminal evidence contain measured calls, successful/attempted
  tools, and provider-reported token usage. Missing/invalid usage stays unknown,
  never becomes a token estimate, and cannot establish success. A successful
  result requires known positive usage, the minimum successful tool count, and
  every required nonempty named export. An SDK transport ACK is not acceptance.
- Inputs are digest-checked, written as read-only files and byte-checked after
  execution. Tools access only named regular files under `inputs/` / `exports/`;
  symlinks, directories and FIFOs are rejected. Limits are 64 KiB per file,
  64 KiB aggregate input envelope, 16 exports, and 128 KiB serialized exports.
  `response.md` is valid as an input but reserved as an export name.
- One active assignment; a non-evicting 128-entry nonce/digest ledger; exact
  retries replay retained progress/terminal replies rather than re-executing.
  Modified replays and stale boot IDs are rejected. Admission authorization
  must succeed again immediately before native execution. The child has a
  600-second overall deadline and a 128-event output bound; only its owned
  workspace is removed on completion/failure.

#### Controller / dispatcher integration contract

Controller and shared dispatcher wiring now select the trusted Task runtime
(`blueprint.runtime`, then `execution.runtime`, defaulting to OpenClaw) and require
a matching sandbox runtime and exactly one selected `agent` container. Reviewed
role and phase reservations preserve execution-only Hermes selection. The existing
filesystem subset supports immutable handoffs and required synthesis outputs;
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
| `KARS_MISSION_CONTRACT` | Strict JSON contract version 1–4 matching the reviewed assignment |
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
provider cache reuse. The adapter removes `tools`, `tool_choice`, and
`parallel_tool_calls` while finalizing, and rejects tool calls in summary
responses, including otherwise permitted filesystem tools. Normal tool dispatch
still uses the genuine Hermes implementation. Summary attempts and retries share
the existing eight-request measurement/budget boundary; reaching the iteration
limit does not become a successful mission merely because a summary was returned.

The official 0.16.0
[release source](https://github.com/NousResearch/hermes-agent/tree/3c231eb3979ab9c57d5cd6d02f1d577a3b718b43)
(`v2026.6.5`) and the installed 0.16.0 distribution provide these concrete hooks:

| Installed native source | Integration point |
|---|---|
| `run_agent.py:4859` | `AIAgent.run_conversation` delegates to the genuine conversation loop |
| `run_agent.py:3304,3357,3755` | `_ensure_primary_openai_client`, `_create_request_openai_client`, `_interruptible_api_call` |
| `agent/chat_completion_helpers.py:1265,1430,1473` | `handle_max_iterations` uses separate `iteration_limit_summary` / `iteration_limit_summary_retry` clients; the shared guarded HTTP transport bounds both |
| `run_agent.py:4770,4849`; `agent/tool_executor.py:690` | `_execute_tool_calls` and genuine sequential execution; raw response allowlisting also catches tools native filtering would otherwise hide |
| `tools/registry.py:234,390` | Native `register` and `dispatch` for the two real filesystem tools |
| `agent/conversation_loop.py:1879–1994` | Native provider usage handling; adapter measures response usage directly and never uses estimated cost at line 1937 |

Local tests install and execute the real native package, mocking only the
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

## Development

```bash
cd runtimes/hermes
pip install -e ".[dev]"
pytest
```

Focused installed-mission acceptance (from the repository root) requires a Python
3.11–3.13 environment with genuine `hermes-agent==0.16.0`, the shared Python mesh
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
