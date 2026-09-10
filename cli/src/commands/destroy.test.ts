// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { destroyCommand } from "./destroy.js";

const { execute, target, guard } = vi.hoisted(() => ({
  execute: vi.fn(), target: vi.fn(), guard: vi.fn(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../lib/azure-destroy-target.js", () => ({ withAzureDestroyTarget: target }));
vi.mock("../lib/sre-authority.js", () => ({ assertDestroySafe: guard }));
vi.mock("ora", () => ({ default: () => ({ start() { return this; }, succeed: vi.fn(), fail: vi.fn() }) }));
afterEach(() => { vi.restoreAllMocks(); vi.clearAllMocks(); });

describe("destroy --all cannot bypass target-bound retirement", () => {
  it.each([["--local"], ["--local", "--cloud"], ["sre", "--local"], ["anything", "--local"]].map(flags => ({ flags })))(
    "rejects Azure --all with local flags: $flags", async ({ flags }) => {
      await expect(destroyCommand().parseAsync(["node", "destroy", "--all", "--yes", ...flags])).rejects.toThrow("cannot be combined");
      expect(execute).not.toHaveBeenCalled();
      expect(target).not.toHaveBeenCalled();
    },
  );
  it.each([[], ["sre"], ["unrelated"], ["--cloud"]].map(flags => ({ flags })))("always binds Azure deletion for $flags", async ({ flags }) => {
    const azure = vi.fn().mockResolvedValue({ stdout: "" });
    target.mockImplementation(async (_exec, _rg, _sub, _context, destroy) => destroy(azure));
    await destroyCommand().parseAsync(["node", "destroy", "--all", "--yes", "--resource-group", "B",
      "--subscription", "subscription-id", "--context", "A", ...flags]);
    expect(target).toHaveBeenCalledWith(execute, "B", "subscription-id", "A", expect.any(Function));
    expect(guard).not.toHaveBeenCalled();
    expect(execute).not.toHaveBeenCalled();
    expect(azure.mock.calls.map(([, args]) => args.slice(0, 2))).toEqual([
      ["group", "delete"], ["cognitiveservices", "account"], ["keyvault", "purge"],
    ]);
  });
  it("never starts deletion when target proof fails", async () => {
    target.mockRejectedValue(new Error("target mismatch"));
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit 1"); });
    await expect(destroyCommand().parseAsync(["node", "destroy", "--all", "--yes"])).rejects.toThrow("exit 1");
    expect(execute).not.toHaveBeenCalled();
  });
  it("does not fetch credentials or mutate anything merely to show --all confirmation", async () => {
    vi.spyOn(console, "log").mockImplementation(() => {});
    await destroyCommand().parseAsync(["node", "destroy", "--all"]);
    expect(target).not.toHaveBeenCalled();
    expect(execute).not.toHaveBeenCalled();
  });
});
