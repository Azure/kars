# In-process fake router (CLI-side)

Groundwork for the local dev-loop plan (plan items T1 / T4 / T5).

## Library — `fake-router.ts`

Used from vitest tests. Binds on an ephemeral port, serves canned JSON,
records every request for later assertion.

```ts
import { FakeRouter } from "./fake-router.js";

const router = await FakeRouter.start({
  routes: [
    { method: "POST", path: "/v1/chat/completions", body: {...} },
  ],
});
try {
  process.env.KARS_ROUTER_URL = router.baseUrl;
  // ... exercise code under test ...
  expect(router.log).toHaveLength(1);
} finally {
  await router.stop();
}
```

## Standalone — `fake-router-cli.ts`

Used from `docker-compose.dev.yml` (T4) and the scenario runner (T5).
Binds on a fixed port (default 8443, matching the hardcoded router
address the plugin uses) and auto-routes any `*.json` file in the
fixtures dir.

```bash
node dist/testing/fake-router-cli.js --port 8443 \
  --fixtures ../inference-router/tests/fixtures/foundry
```

Shares fixtures with the Rust integration tests under
`inference-router/tests/fixtures/foundry/` — a single source of truth
for sanitized Azure responses.

## YAML scenario runner — `scenario.ts` + `scenario-runner-cli.ts` (T5)

A declarative harness for component-level HTTP round-trips. Each file
in `scenarios/*.yaml` starts a FakeRouter, fires scripted requests, and
asserts on status / body / headers / recorded request log.

```bash
# Run every shipped scenario (uses tsx — no build required)
make scenario

# Run a specific file
make scenario SCENARIO=src/testing/scenarios/01-chat-completion-happy-path.yaml
```

Scenario schema (trimmed):

```yaml
name: "scenario title"
router:
  fixtures_dir: ../../../../inference-router/tests/fixtures/foundry
  routes:
    - { method: GET, path: /v1/models, body: { fixtureFile: models_list.json } }
steps:
  - name: "fetch models"
    request: { method: GET, path: /v1/models }
    expect:
      status: 200
      body_contains: ["gpt-4o"]
      log: { total_requests: 1, last_method: GET, last_path: /v1/models }
```

The runner is also invokable programmatically from vitest — see
`scenario.test.ts`. Three seed scenarios ship: chat happy path,
content-filter propagation, and 429 rate-limit passthrough.

## docker-compose dev stack — `docker-compose.dev.yml` (T4)

Runs `fake-router-cli` on `127.0.0.1:8443` inside an Azure Linux
(`mcr.microsoft.com/azurelinux/base/nodejs:24`) container with the
fixtures volume-mounted read-only. Azure Linux is the repo-wide base
image family; Node 24 is chosen because the Azure Linux image registry
only publishes 20 and 24 tags (no 22). The fake-router is stdlib-only
so the version bump is a no-op for behavior. Use this to point any
Kars client at a local router without building the sandbox image:

```bash
cd cli && npm ci                      # one-time, for tsx resolution
make dev-compose-up                    # start
KARS_ROUTER_URL=http://127.0.0.1:8443 kars …
make dev-compose-down                  # stop
```

The compose stack deliberately does **not** include the sandbox, the
real router, or the relay. The point is to skip the rebuild loop. Real
router integration tests live under `inference-router/tests/` and
spin everything up in-process.

## Known limitation

`cli/src/plugin.ts` has ~33 hardcoded `http://127.0.0.1:8443/...` call
sites; only two places (lines 3340 + 4698) honour
`KARS_ROUTER_URL`. Plugin-level in-process testing against an
ephemeral-port fake router therefore requires either:

- running the standalone CLI on port 8443 (conflicts with a real router);
- or completing the plugin-URL centralization work (plan.md Q-items).

The standalone mode is the intended path for the compose/scenario work
(T4/T5); the library mode already unlocks any future code that uses
`KARS_ROUTER_URL` correctly.
