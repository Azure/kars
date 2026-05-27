# kars Compatibility Suite (`tests/compat/`)

**Status:** Phase 0 skeleton. Grows with every Phase-0→Phase-4 decomposition.
**Charter:** `internal Phase 1 plan` §5.4.
**Principle:** `internal Phase 1 plan` §0.2 #1 — "Zero regressions on
existing user-facing behaviour."

## Purpose

The compat suite is the wire between existing user-visible flows and the
Phase-0→Phase-4 decomposition plan. Before we split `cli/src/plugin.ts` or
`inference-router/src/handoff.rs` or swap to the `AgtMeshProvider`, every
protected flow must have a baseline compat spec here. After the change, the
same spec must still pass against the refactored code.

## Protected flows (per plan §5.1 — eight)

1. **`kars dev`** — local Docker sandbox lifecycle.
2. **`kars up`** — AKS provisioning preflight → provision → helm.
3. **`kars connect`** — attach to running sandbox.
4. **`kars handoff`** — warm handoff between sibling agents.
5. **`kars offload`** — cloud offload from local to AKS.
6. **`kars operator`** — headless operator TUI (blessed dashboard).
7. **OpenClaw → kars inter-agent** — E2E Signal protocol via router.
8. **Plugin lifecycle** — OpenClaw plugin load + tool registration.

Each flow gets a spec file under `specs/<flow>.spec.ts`.

## Running locally

```bash
cd tests/compat
npm ci
npm test                # runs all specs
npm test -- operator    # filter by name
```

Tests run headlessly — no real AKS, no real Docker, no real kubectl. All
external calls are mocked through the harness in `harness/`.

## Harness

- `harness/blessed-mock.ts` — replaces `blessed`/`blessed-contrib` with a
  headless surface that records render calls, exposes the current screen as
  a serialisable snapshot, and lets specs inject keyboard events.
- `harness/kubectl-mock.ts` *(Phase 0 stub; Phase 1 fills it in)* — intercepts
  `execa("kubectl", ...)` and returns scripted fixtures.
- `harness/scenario.ts` *(Phase 0 stub; Phase 1 fills it in)* — scenario
  runner that composes mocks, executes a command, captures outputs.

## Spec authoring rules

1. **No `.skip` / `.todo` in merged PRs**, except when a spec is intentionally
   staged for a later phase. Each `.todo` must link to the plan section that
   will land it.
2. **Two oracles per spec** where feasible — e.g., snapshot the rendered
   screen AND assert on a specific rendered cell. Catches both regressions
   and mistaken snapshot updates.
3. **No network, no real kubectl, no real Docker.** Specs that need those
   live in the e2e suite (`make test-e2e`), not here.
4. **Pair with security audits.** When a decomposition PR changes a
   protected flow, the security-audit doc references the compat spec by
   path in §9 "Negative-test coverage".

## Phase-0 acceptance

- [x] Package boots (`npm ci && npm test` green locally).
- [x] `operator-tui.spec.ts` — smoke spec: command registers, blessed screen
  creates without crashing, quit key exits cleanly.
- [ ] Phase 1 adds: full operator-TUI render snapshot, kubectl-mock scenarios,
  `kars dev` lifecycle spec.
