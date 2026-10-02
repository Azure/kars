// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, expect, it, vi } from "vitest";
import { createServer, type Server, type ServerResponse } from "node:http";
import { authorizeTaskAction } from "./task-policy.js";

let server: Server | undefined;
afterEach(async () => {
  vi.unstubAllEnvs();
  if (server) {
    server.closeAllConnections();
    await new Promise<void>(resolve => server!.close(() => resolve()));
  }
  server = undefined;
});

it.each([
  [true, (res: ServerResponse) => { res.end('{"allowed":true}'); }],
  [false, (res: ServerResponse) => { res.end('{"allowed":false}'); }],
  [false, (res: ServerResponse) => { res.writeHead(201); res.end('{"allowed":true}'); }],
  [false, (res: ServerResponse) => { res.writeHead(503); res.end('{"allowed":true}'); }],
  [false, (res: ServerResponse) => { res.end('{"allowed":"true"}'); }],
  [false, (res: ServerResponse) => { res.end('malformed'); }],
  [false, (res: ServerResponse) => { res.end(' '.repeat(65537) + '{"allowed":true}'); }],
  [false, (res: ServerResponse) => { res.writeHead(200, { "Content-Length": 100 }); res.write('{'); res.flushHeaders(); setImmediate(() => res.destroy()); }],
  [false, (_res: ServerResponse) => { /* Deliberately leave the real socket idle to test its timeout. */ }],
] as const)("policy decision %s for response %#", async (allowed, respond) => {
  let payload: unknown;
  server = createServer((req, res) => {
    let body = "";
    req.on("data", chunk => { body += chunk; });
    req.on("end", () => { payload = JSON.parse(body); respond(res); });
  });
  await new Promise<void>(resolve => server!.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing listener");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
  expect(await authorizeTaskAction("task:execute", { run_nonce: "run-1" })).toBe(allowed);
  expect(payload).toEqual({ action: "task:execute", context: { run_nonce: "run-1" } });
});
