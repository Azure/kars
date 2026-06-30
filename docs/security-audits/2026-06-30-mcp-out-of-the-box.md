# Security Audit — MCP out-of-the-box: session keepalive, egress auto-derive, CLI update (v0.1.24)

Date: 2026-06-30
Scope: `inference-router/src/mcp/forwarder.rs`, `controller/src/reconciler/mod.rs`, `controller/src/reconciler/mcp_egress.rs`, `cli/src/commands/update.ts`, `cli/src/lib/update-check.ts`, `cli/src/commands/upgrade.ts`.
Gated paths: `inference-router/src/mcp/forwarder.rs`, `controller/src/reconciler/...`, `cli/src/commands/...`.

## Summary

This change makes MCP servers work out of the box and adds a CLI self-update
notice. Four capability-touching pieces:

1. **Router MCP session keepalive** — the forwarder now holds the standalone
   `GET /mcp` SSE stream open and answers a heartbeating MCP server's `ping`s
   with `pong`s so the upstream doesn't reap the session mid-task.
2. **Controller MCP egress auto-derivation** — the controller parses each
   referenced `McpServer.spec.url` and emits the matching default-deny
   NetworkPolicy egress rule (namespaceSelector for in-cluster, coarse ipBlock
   for external non-443, nothing for external 443).
3. **CLI `kars update` + automatic update notice** — best-effort npm version
   check + optional `npm install -g @kars-runtime/cli@latest`.
4. **`kars upgrade` pre-flight** — server-side-apply conflict detection before
   the atomic Helm upgrade (read-only dry-run).

## T1: New capability / attack surface? (YES — bounded)

- **Keepalive (router→MCP):** opens one long-lived GET to an MCP endpoint the
  router was *already* authorised to call (same URL, headers, session id as the
  tools/call path). It only ever **reads** SSE frames and **writes** a JSON-RPC
  `pong` (`{"id":<n>,"result":{}}`) — no new destinations, no new credentials,
  no agent-reachable surface (the agent still only sees loopback tools). The
  pong responder replies solely to server-initiated `ping`s; any other inbound
  frame is ignored. The task is bounded by a per-request timeout and is
  aborted/replaced on session re-init.
- **Egress auto-derivation:** *narrows* rather than widens — it adds precisely
  the rule needed to reach a *referenced* `McpServer` and nothing else. The
  derivation is driven only by `governance.mcpServerRefs` (operator-authored)
  and the referenced CR's `url`; an attacker cannot inject a rule without RBAC
  to create an `McpServer` and to reference it from a sandbox. In-cluster URLs
  yield a `namespaceSelector` scoped to the MCP's namespace + exact port;
  external non-443 yields a coarse port rule with host enforcement still at the
  router; external 443 adds nothing. Unparseable/missing referents are skipped
  (fail-closed: no rule added).
- **CLI update check:** introduces two outbound HTTPS calls from the operator's
  workstation (npm registry `latest` + GitHub release notes), each bounded by a
  1.5s timeout, cached ≤24h in `~/.kars/update-check.json`, off in CI/non-TTY
  and via `KARS_NO_UPDATE_CHECK=1`. It never auto-executes an install: the
  passive notice only prints text; the actual `npm install -g` runs only on an
  explicit `kars update` (interactive confirm, or `--yes`). No tokens, no
  cluster access, no code executed from the network response (only a version
  string and a changelog line are read).
- **Upgrade pre-flight:** a `helm upgrade --dry-run=server` — read-only,
  mutates nothing — whose output is parsed for field-manager conflicts.

## T2: Security-control change? (NEUTRAL→IMPROVED)

- The router remains the sole network path to MCP servers; governance (trust,
  ToolPolicy, audit, token budget) on tool calls is unchanged. Keepalive adds
  no tool surface.
- Egress stays **default-deny**; auto-derivation produces the *same* rules an
  operator would have hand-written, but correctly (namespaceSelector under
  Cilium, where an ipBlock silently fails for in-cluster pods). This removes a
  foot-gun that previously led operators to widen egress by hand.
- The session-loss classifier was tightened to fire only on genuine 4xx hard
  signals, so a healthy 2xx tool result that merely mentions "session" no
  longer triggers a destructive re-init.

## T3: Availability / fail-open risk? (REDUCED)

- Keepalive **reduces** an availability bug: stateful MCP sessions were being
  reaped after ~5s, breaking multi-step flows. All keepalive failures are
  swallowed and the task simply exits/reconnects; it never blocks a tool call.
- Egress derivation that errors on the API returns a short requeue (no partial
  NetworkPolicy is applied). A missing/unparseable referent is skipped rather
  than failing the whole reconcile.
- The CLI update check is fully best-effort: every error path returns silently;
  it can never block or fail a `kars` command. The upgrade pre-flight is
  additive and only *stops before* a known-bad apply (fail-closed, safer).

## Verification

- Rust: `cargo test --all` green (controller 854 + router 957 + integration);
  `cargo clippy --all-targets -D warnings` clean; `cargo audit` /
  `cargo deny check` clean (anyhow bumped to 1.0.103, RUSTSEC-2026-0190).
- CLI: build + typecheck + `oxlint` (0 errors) + 928 vitest tests green
  (incl. new `update-check` suite).
- Live AKS validation: both sandbox routers hold a persistent GET SSE stream to
  Playwright that survives well past the 5s reaper; zero `session lost` log
  lines. Example CRDs validated with `kubectl apply --dry-run=server`.
- `ci/check-loc.sh`, `ci/no-stubs.sh`, `ci/check-copyright-headers.sh` pass.

## Verdict

Accept — the new surface is bounded, operator-gated, and fail-closed; the net
effect is improved availability (session keepalive), tighter-by-construction
egress, and a non-intrusive, consent-gated CLI updater.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
