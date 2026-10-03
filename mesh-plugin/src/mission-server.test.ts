// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { AddressInfo } from "node:net";
import { missionHealthServer, serveMissions } from "./mission-server.js";

function harness() {
  const abort = new AbortController();
  let complete!: () => void;
  const delivery = new Promise<void>(resolve => { complete = resolve; });
  const service = { ready: vi.fn(() => false), run: vi.fn(async () => delivery) };
  const server = missionHealthServer(service);
  const exit = vi.fn();
  const start = (shutdownMs = 1_000) => serveMissions(service, server, abort.signal, { port: 0, shutdownMs }, exit);
  const url = () => `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return { abort, complete, service, server, exit, start, url };
}

describe("mission health and shutdown", () => {
  it("serves live and truthful ready probes while bootstrap or delivery is pending", async () => {
    const h = harness(); const running = h.start();
    try {
      await vi.waitFor(() => expect(h.service.run).toHaveBeenCalledOnce());
      expect((await fetch(`${h.url()}/livez`)).status).toBe(200);
      expect((await fetch(`${h.url()}/readyz`)).status).toBe(503);
      h.service.ready.mockReturnValue(true);
      const ready = await fetch(`${h.url()}/readyz`); expect(ready.status).toBe(200);
      expect(ready.headers.get("cache-control")).toBe("no-store");
      expect((await fetch(`${h.url()}/unknown`)).status).toBe(404);
      expect((await fetch(`${h.url()}/livez`, { method: "POST" })).status).toBe(405);
    } finally { h.abort.abort(); h.complete(); await running; }
    expect(h.server.listening).toBe(false); expect(h.exit).not.toHaveBeenCalled();
  });
  it("forces process retirement when graceful drain exceeds its deadline", async () => {
    const h = harness(); const running = h.start(20);
    try {
      await vi.waitFor(() => expect(h.service.run).toHaveBeenCalledOnce()); h.abort.abort();
      await vi.waitFor(() => expect(h.exit).toHaveBeenCalledOnce());
      expect(h.service.run).toHaveBeenCalledWith(h.abort.signal);
    } finally { h.complete(); await running; }
  });
  it("does not listen or start after an already-aborted signal", async () => {
    const h = harness(); h.abort.abort(); await h.start();
    expect(h.server.listening).toBe(false); expect(h.service.run).not.toHaveBeenCalled();
  });
  it("closes the health listener on service failure", async () => {
    const h = harness(); h.service.run.mockRejectedValueOnce(new Error("SDK retirement failed"));
    await expect(h.start()).rejects.toThrow("SDK retirement failed");
    expect(h.server.listening).toBe(false); expect(h.exit).not.toHaveBeenCalled();
  });
});
