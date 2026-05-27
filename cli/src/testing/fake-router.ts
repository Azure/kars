// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * In-process fake Kars inference router.
 *
 * Groundwork for the local compose stack (plan item T4) and the scenario
 * runner (T5). Bind this on any loopback port and point the plugin at it via
 * `KARS_ROUTER_URL` (currently honoured at plugin.ts:3340 and
 * plugin.ts:4698; remaining ~30 hardcoded call sites are the subject of a
 * separate quality PR — see plan.md Q-items).
 *
 * Zero runtime dependencies — uses only `node:http`. Safe to import from
 * vitest, `scripts/`, and the future compose image.
 *
 * Usage (in tests):
 *
 *   const router = await FakeRouter.start({
 *     routes: [
 *       { method: "POST", path: "/v1/chat/completions", body: {...} },
 *     ],
 *   });
 *   try {
 *     process.env.KARS_ROUTER_URL = router.baseUrl;
 *     // ... exercise code under test ...
 *     expect(router.log).toHaveLength(1);
 *   } finally {
 *     await router.stop();
 *   }
 */
import http, {
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from "node:http";
import { readFile } from "node:fs/promises";
import { resolve as resolvePath } from "node:path";

export interface FixtureRoute {
  method: string;
  /** Path must match exactly (starts-with). */
  path: string;
  /** HTTP status (default 200). */
  status?: number;
  /** Body. Either an inline value, a `() => Value`, or `{ fixtureFile: string }`. */
  body?: unknown | (() => unknown) | { fixtureFile: string };
  /** Additional response headers. */
  headers?: Record<string, string>;
}

export interface RecordedRequest {
  method: string;
  path: string;
  query: string;
  headers: Record<string, string>;
  body: Buffer;
}

export interface StartOptions {
  /** Port to bind. Defaults to 0 (ephemeral). Pass 8443 to mirror production. */
  port?: number;
  /** Initial route table. */
  routes?: FixtureRoute[];
  /** Optional base dir for `{fixtureFile: "..."}` — resolved relative to cwd otherwise. */
  fixturesDir?: string;
}

export class FakeRouter {
  private constructor(
    private readonly server: Server,
    public readonly port: number,
    public readonly routes: FixtureRoute[],
    public readonly log: RecordedRequest[],
    private readonly fixturesDir: string | undefined,
  ) {}

  get baseUrl(): string {
    return `http://127.0.0.1:${this.port}`;
  }

  static async start(opts: StartOptions = {}): Promise<FakeRouter> {
    const routes: FixtureRoute[] = opts.routes ? [...opts.routes] : [];
    const log: RecordedRequest[] = [];
    const fixturesDir = opts.fixturesDir;

    const server = http.createServer(
      (req: IncomingMessage, res: ServerResponse) =>
        void handle(req, res, routes, log, fixturesDir),
    );

    return await new Promise<FakeRouter>((resolve, reject) => {
      server.once("error", reject);
      server.listen(opts.port ?? 0, "127.0.0.1", () => {
        const addr = server.address();
        if (addr == null || typeof addr === "string") {
          reject(new Error("fake router: unexpected address"));
          return;
        }
        resolve(new FakeRouter(server, addr.port, routes, log, fixturesDir));
      });
    });
  }

  /** Add or replace a route at runtime (useful for per-test setup). */
  route(r: FixtureRoute): this {
    this.routes.push(r);
    return this;
  }

  clearLog(): void {
    this.log.length = 0;
  }

  async stop(): Promise<void> {
    await new Promise<void>((resolve) => this.server.close(() => resolve()));
  }
}

async function handle(
  req: IncomingMessage,
  res: ServerResponse,
  routes: FixtureRoute[],
  log: RecordedRequest[],
  fixturesDir: string | undefined,
): Promise<void> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  const body = Buffer.concat(chunks);

  const url = new URL(req.url ?? "/", "http://127.0.0.1");
  const recHeaders: Record<string, string> = {};
  for (const [k, v] of Object.entries(req.headers)) {
    recHeaders[k.toLowerCase()] = Array.isArray(v) ? v.join(",") : (v ?? "");
  }
  log.push({
    method: req.method ?? "GET",
    path: url.pathname,
    query: url.search,
    headers: recHeaders,
    body,
  });

  const match = routes.find(
    (r) =>
      r.method.toUpperCase() === (req.method ?? "GET").toUpperCase() &&
      url.pathname.startsWith(r.path),
  );

  if (!match) {
    res.statusCode = 404;
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        error: {
          code: "fake_router_no_fixture",
          message: `no fixture for ${req.method} ${url.pathname}`,
        },
      }),
    );
    return;
  }

  let payload: unknown;
  if (
    match.body != null &&
    typeof match.body === "object" &&
    "fixtureFile" in (match.body as Record<string, unknown>)
  ) {
    const file = (match.body as { fixtureFile: string }).fixtureFile;
    const dir = fixturesDir ?? process.cwd();
    const raw = await readFile(resolvePath(dir, file), "utf8");
    payload = JSON.parse(raw);
  } else if (typeof match.body === "function") {
    payload = (match.body as () => unknown)();
  } else {
    payload = match.body ?? {};
  }

  res.statusCode = match.status ?? 200;
  res.setHeader("content-type", "application/json");
  for (const [k, v] of Object.entries(match.headers ?? {})) res.setHeader(k, v);
  res.end(JSON.stringify(payload));
}
