# Security Audit — Hermes Act 1 E2E Smoke Fixes

**Date:** 2026-06-04
**Branch:** `hermes/act1-docker-smoke-fixes`
**Reviewers:** Pal Lakatos, Copilot

## Scope

End-to-end smoke run on kind exposed five real bugs blocking Hermes
Act 1 from being functional. Each is fixed here:

1. **`awk` not in Azure Linux 3 base image** — entrypoint's config.yaml
   merge path crashed on every re-run (after first boot, when the
   merge code path actually fires). Replaced awk with a Python merger
   that preserves user-managed YAML sections while replacing the
   entrypoint-owned `plugins:` and `mcp_servers:` blocks.

2. **TUI mode crashes without TTY in K8s pods** — `hermes` with no
   subcommand drops into the interactive chat REPL which exits
   immediately when stdin is not a TTY. Replaced with `hermes gateway
   run --accept-hooks` (the recommended Docker/headless mode per
   `hermes gateway --help`), which stays alive as a daemon waiting
   for messages even when no channels are configured.

3. **`KARS_MCP_SERVERS` env hardcoded to `openclaw` container** —
   `controller/src/reconciler/mod.rs` injected the list of mirrored
   McpServer names only into the container literally named `openclaw`.
   The Hermes container is named `agent` (mirroring pydantic-ai,
   anthropic, langgraph, etc.), so the env was silently dropped and
   the entrypoint had nothing to translate. Fixed by reading
   `agent_container_name` from `runtime_spec.kind` (matches the same
   logic that names the container in the deployment spec ~line 1771).

4. **Hermes entrypoint scanned wrong path for McpServer specs** — the
   original entrypoint expected `/etc/kars/mcp/<name>/meta.json` files
   but the controller only mounts JWKS payloads there (for the
   *router* to verify upstream signatures, not for agents to discover
   URLs). The actual kars contract uses `KARS_MCP_SERVERS` (CSV of
   names) and the loopback router endpoint with an
   `x-kars-mcp-server: <name>` header — agents never see real
   upstream URLs or bearer tokens. Re-aligned the Hermes entrypoint
   to the kars contract.

5. **`hermes config set key=value` syntax wrong** — Hermes 0.15.x
   uses `hermes config set <key> <value>` (two positional args, not
   `key=value`). The channel-translation loop silently failed via
   `|| true`. Fixed to use the correct syntax.

6. **Router rustls CryptoProvider not pre-installed** — sub-agent
   spawn handler crashed with `Could not automatically determine the
   process-level CryptoProvider` when kube-client made its first TLS
   handshake. Our dep graph pulls aws-lc-rs transitively (via
   reqwest+oci-client) but rustls 0.23 refuses to auto-pick when
   multiple provider feature gates resolve. Added an explicit
   `aws_lc_rs::default_provider().install_default()` as the first
   line of `main()`.

## Threat model

### T1: Python merge in entrypoint (NEW CODE PATH, MITIGATED)

The new Python merger reads `$HERMES_CONFIG` and `$MCP_FRAGMENT`
(both files we control) and writes back to `$HERMES_CONFIG`.

- **Path traversal:** Both file paths are constructed by the
  entrypoint from `$HERMES_HOME` (also entrypoint-controlled, set
  before the Python invocation). No user input flows in.
- **Regex DoS:** The strip pattern `^(plugins|mcp_servers):` is a
  literal alternation, no quantifiers, no backtracking. Safe.
- **YAML injection:** We do not parse YAML — we only do line-based
  block deletion + concat. Any malformed YAML the user might write
  is preserved verbatim and surfaces as a Hermes startup error,
  same as before this change. No new escalation path.

### T2: `gateway run` always-on (BEHAVIOUR CHANGE)

Previously the pod would exit immediately on re-run (no TTY +
TUI). Now it stays alive as a Hermes gateway daemon listening on a
loopback socket. The gateway only accepts messages from configured
channels — without `channels.telegram.token` (etc.) it has no input
source other than future `hermes config set` calls from inside the
pod.

Net effect: the pod is now stably long-lived, which is what kars
needs for sub-agent spawn + mesh delivery. No new ingress surface
(NetworkPolicy still bans inbound traffic to UID 1000 except via
the inference-router sidecar).

### T3: `KARS_MCP_SERVERS` for non-openclaw runtimes (FIX, NO NEW RISK)

The controller now injects the same env into the `agent` container
for hermes/pydantic-ai/anthropic/langgraph/byo pods that it already
injected into the `openclaw` container for OpenClaw pods. The list
of names is derived from the *mirrored* McpServer ConfigMaps the
controller already authored — no new sources of trust.

The agent still cannot reach the real upstream URLs directly: it
only knows the name, and the router resolves+signs+forwards. No
secrets cross into the agent.

### T4: Hermes entrypoint reads `KARS_MCP_SERVERS` env (REPLACED FILESYSTEM SCAN)

Switched from filesystem scan of `/etc/kars/mcp/*/meta.json` (which
never existed) to env-var split of `KARS_MCP_SERVERS`. Reduces
attack surface: no more `jq` parsing of attacker-controlled JSON,
no more `meta.json` files needed in the agent's view. The
loopback-router-only routing is more constrained than the prior
"emit upstream URL into agent config" path.

### T5: `hermes config set` syntax fix (TYPO CLASS, NO NEW RISK)

Same data path; just the right CLI invocation. No security
implication.

### T6: rustls CryptoProvider install (CRYPTO HARDENING)

aws-lc-rs is a BoringSSL-derived, FIPS-eligible provider. Pinned
via workspace `rustls = { … features = ["aws-lc-rs"] }`. We were
already linking aws-lc-rs transitively via reqwest+oci-client; this
just makes the install explicit so kube-client (and any future
rustls user) doesn't panic. No new crypto code; no new providers
in our dep graph.

## Verification

End-to-end smoke run on kind cluster `kars-dev`:

```
[T1]  Pod state                                              2/2 Running ✅
[T2]  Plugin discovery: kars                                 10 tools + 2 hooks ✅
[T3]  Router /healthz                                        200 ok ✅
[T4]  Router /agt/evaluate                                   200 {allowed:true} ✅
[T5]  http_fetch via router /egress/fetch                    200, 528 bytes ✅
[T6]  kars_spawn_list via router /sandbox/list               200 {count:0} ✅
[T7]  foundry_memory store name = memory-<sandbox>           ✅
[T8]  KarsMemory CR                                          phase=Compiled ✅
[T9]  McpServer CR translated → mcp_servers.smoke-mcp        present ✅
[T10] Channel: TELEGRAM_BOT_TOKEN → channels.telegram.token  present ✅
[T11] Mesh stubs return clear Act 2 error                    ✅
[T12] pre_tool_call hook fires + decision=allow              ✅
```

All 834 controller + 932 router Rust tests still pass.
`cargo clippy -- -D warnings` clean. `cargo fmt --all` applied.

## Decision

**Approved.**

Five bug fixes plus one new code path (Python YAML merger).
Threat surface is unchanged or reduced relative to the broken
prior state. No new external trust sinks. The Hermes runtime is
now functionally equivalent to OpenClaw for everything except mesh
(deferred to Act 2 as planned).

Signed-off-by: Pal Lakatos <pal.lakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
