// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, expect, it, vi } from "vitest";
import { createServer, request, type ClientRequest, type Server, type ServerResponse } from "node:http";
import type { TaskLoopDeps } from "./agt-task-loop.js";
import { executeLegacyTaskForTest as executeTaskWithEvidence } from "./agt-task-loop.test-support.js";

vi.mock("node:http", async importOriginal => {
  const actual = await importOriginal<typeof import("node:http")>();
  return { ...actual, request: vi.fn(actual.request) };
});

const deps: TaskLoopDeps = {
  meshClient: () => null, isInterruptRequested: () => false,
  interruptReason: () => "", setInterrupt: () => {},
};
const log = { info: vi.fn(), warn: vi.fn() };
let server: Server | undefined;
let timer: ReturnType<typeof setTimeout> | undefined;
let count = 0;

async function router(reply: (response: ServerResponse) => void) {
  count = 0;
  vi.mocked(request).mockClear();
  server = createServer((req, res) => {
    req.resume();
    req.on("end", () => { count++; reply(res); });
  });
  await new Promise<void>(resolve => server!.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing listener");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
  vi.stubEnv("KARS_MODEL", "gpt-5.4-mini");
  vi.stubEnv("OPENCLAW_MODEL", "");
}

afterEach(async () => {
  if (timer) clearTimeout(timer);
  timer = undefined;
  server?.closeAllConnections();
  if (server) await new Promise<void>(resolve => server!.close(() => resolve()));
  server = undefined;
  vi.unstubAllEnvs();
});

it("waits beyond the former 60-second cutoff and records the eventual response once", async () => {
  await router(res => {
    timer = setTimeout(() => res.end(JSON.stringify({
      choices: [{ finish_reason: "stop", message: { role: "assistant", content: "Completed after reasoning" } }],
      usage: { prompt_tokens: 7, completion_tokens: 4, total_tokens: 11 },
    })), 61_000);
  });
  const result = await executeTaskWithEvidence("task", deps, log, false);
  expect(result.usage?.totalTokens).toBe(11);
  expect(result.rounds).toBe(1);
  expect(count).toBe(1);
  expect(vi.mocked(request).mock.calls[0][1]).toMatchObject({ timeout: 630_000 });
}, 75_000);

it("fails once with unknown usage when the router returns an inference timeout", async () => {
  await router(res => { res.writeHead(504); res.end(JSON.stringify({ error: "upstream deadline" })); });
  await expect(executeTaskWithEvidence("task", deps, log, false)).rejects.toMatchObject({
    message: expect.stringContaining("LLM HTTP 504"), evidence: { rounds: 0, usage: null },
  });
  expect(count).toBe(1);
});

it("destroys a locally timed-out request without retrying or inventing usage", async () => {
  await router(() => {});
  const failure = executeTaskWithEvidence("task", deps, log, false).catch(error => error);
  await vi.waitFor(() => expect(count).toBe(1));
  const req = vi.mocked(request).mock.results[0].value as ClientRequest;
  req.emit("timeout");
  expect(await failure).toMatchObject({ message: "LLM timeout", evidence: { rounds: 0, usage: null } });
  expect(req.destroyed).toBe(true);
  expect(count).toBe(1);
});
