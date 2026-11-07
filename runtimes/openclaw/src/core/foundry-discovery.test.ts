// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { discoverFoundryProject } from "./foundry-discovery.js";

const files = vi.hoisted(() => new Map<string, string>());
vi.mock("node:fs", () => ({
  mkdirSync: vi.fn(),
  readFileSync: (path: string) => files.get(path) || "",
  writeFileSync: (path: string, content: string) => files.set(path, content),
  renameSync: (from: string, to: string) => { files.set(to, files.get(from) || ""); files.delete(from); },
  unlinkSync: (path: string) => files.delete(path),
}));

beforeEach(() => files.clear());

describe("Foundry discovery memory recall", () => {
  it("keeps static and contextual recall distinct and merges duplicate memories", async () => {
    const router = vi.fn(async (_method: string, _path: string) => ({ data: [] }));
    const memory = vi.fn()
      .mockResolvedValueOnce({ memories: [{ memory_id: "profile", content: "concise replies" }] })
      .mockResolvedValueOnce({ memories: [
        { memory_id: "profile", content: "concise replies" },
        { memory_id: "context", content: "review the handoff" },
      ] });
    const log = { info: vi.fn(), warn: vi.fn() };
    await discoverFoundryProject(router, log, memory);
    expect(memory.mock.calls).toEqual([
      [{ operation: "search" }],
      [{ operation: "search", text: "key facts, user preferences, prior context, recent work, handoff history", top_k: 10 }],
    ]);
    expect(router.mock.calls.every(([, path]) => !path.includes("memory_stores"))).toBe(true);
    const content = files.get("/sandbox/.openclaw/workspace/MEMORY.md") || "";
    expect(content.match(/concise replies/g)).toHaveLength(1);
    expect(content).toContain("review the handoff");
    expect(log.info).toHaveBeenCalledWith("Foundry memory: recalled 2 memories on startup");
  });

  it("retains contextual recall when static recall fails", async () => {
    const router = vi.fn(async () => ({ data: [] }));
    const memory = vi.fn().mockRejectedValueOnce(new Error("unavailable"))
      .mockResolvedValueOnce({ memories: [{ content: "prior context" }] });
    await discoverFoundryProject(router, { info: vi.fn(), warn: vi.fn() }, memory);
    expect(memory).toHaveBeenCalledTimes(2);
    expect(files.get("/sandbox/.openclaw/workspace/MEMORY.md")).toContain("prior context");
  });
});
