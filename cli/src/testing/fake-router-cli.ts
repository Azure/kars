// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Standalone CLI entry for the fake router — groundwork for the
 * docker-compose dev stack (plan item T4).
 *
 * Usage:
 *   node dist/testing/fake-router-cli.js --port 8443 \
 *       --fixtures ../inference-router/tests/fixtures/foundry
 *
 * The `--fixtures` dir is the same one the Rust integration tests use.
 * Any file named `<name>.json` in that dir becomes accessible as a canned
 * response via the default route table — see `--help` for details.
 */
import { parseArgs } from "node:util";
import { readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { FakeRouter, type FixtureRoute } from "./fake-router.js";

interface CliOptions {
  port: number;
  fixturesDir: string | undefined;
}

async function defaultRoutes(
  fixturesDir: string | undefined,
): Promise<FixtureRoute[]> {
  if (fixturesDir === undefined) return [];
  const dir = resolve(fixturesDir);
  const entries = await readdir(dir);
  const routes: FixtureRoute[] = [];

  // Map fixture filenames → canonical routes. Extend as new fixtures land.
  const mapping: Record<string, { method: string; path: string; status?: number }> =
    {
      "chat_completion_ok.json": {
        method: "POST",
        path: "/v1/chat/completions",
      },
      "embeddings_ok.json": { method: "POST", path: "/v1/embeddings" },
      "models_list.json": { method: "GET", path: "/v1/models" },
      "responses_ok.json": { method: "POST", path: "/v1/responses" },
      "connections_list.json": { method: "GET", path: "/connections" },
      "memory_stores_empty.json": { method: "GET", path: "/memory_stores" },
    };

  for (const f of entries) {
    const spec = mapping[f];
    if (spec === undefined) continue;
    routes.push({ ...spec, body: { fixtureFile: f } });
  }
  return routes;
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      port: { type: "string", default: "8443" },
      fixtures: { type: "string" },
      help: { type: "boolean", default: false },
    },
  });

  if (values.help === true) {
    console.log(
      [
        "Usage: fake-router-cli --port <port> --fixtures <dir>",
        "",
        "  --port      TCP port (default 8443; use to mirror production)",
        "  --fixtures  Directory of *.json fixtures to auto-route",
        "  --help      Show this message",
      ].join("\n"),
    );
    process.exit(0);
  }

  const opts: CliOptions = {
    port: Number.parseInt(values.port ?? "8443", 10),
    fixturesDir: values.fixtures,
  };

  const routes = await defaultRoutes(opts.fixturesDir);
  const router = await FakeRouter.start({
    port: opts.port,
    fixturesDir: opts.fixturesDir,
    routes,
  });

  console.log(
    `fake-router listening on ${router.baseUrl} with ${routes.length} fixture routes`,
  );

  const shutdown = async (): Promise<void> => {
    console.log("fake-router: shutting down");
    await router.stop();
    process.exit(0);
  };
  process.on("SIGINT", () => void shutdown());
  process.on("SIGTERM", () => void shutdown());
}

void main().catch((err: unknown) => {
  console.error("fake-router failed:", err);
  process.exit(1);
});
