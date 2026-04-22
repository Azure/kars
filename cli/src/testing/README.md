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
  process.env.AZURECLAW_ROUTER_URL = router.baseUrl;
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

## Known limitation

`cli/src/plugin.ts` has ~33 hardcoded `http://127.0.0.1:8443/...` call
sites; only two places (lines 3340 + 4698) honour
`AZURECLAW_ROUTER_URL`. Plugin-level in-process testing against an
ephemeral-port fake router therefore requires either:

- running the standalone CLI on port 8443 (conflicts with a real router);
- or completing the plugin-URL centralization work (plan.md Q-items).

The standalone mode is the intended path for the compose/scenario work
(T4/T5); the library mode already unlocks any future code that uses
`AZURECLAW_ROUTER_URL` correctly.
