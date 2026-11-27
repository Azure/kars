// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("./server.js", () => ({ launchSetupBrowser: vi.fn(async () => {}) }));
import { launchSetupBrowser } from "./server.js";
import { upCommand } from "../up.js";

beforeEach(() => vi.clearAllMocks());
describe("up browser routing", () => {
  it("launches only the concierge with default up options", async () => {
    await upCommand().parseAsync(["--setup-browser"], { from: "user" });
    expect(launchSetupBrowser).toHaveBeenCalledOnce();
  });
  it("passes an explicit failed cluster only to the cleanup concierge", async () => {
    await upCommand().parseAsync(["--setup-browser", "failed-unit"], { from: "user" });
    expect(launchSetupBrowser).toHaveBeenCalledExactlyOnceWith("failed-unit");
  });
  it.each(["--yes", "--release", "--cluster-name=other", "--dry-run", "--mesh-peer"])("rejects conflicting option %s before launching", async arg => {
    await expect(upCommand().exitOverride().parseAsync(["--setup-browser", arg], { from: "user" })).rejects.toThrow("standalone");
    expect(launchSetupBrowser).not.toHaveBeenCalled();
  });
});
