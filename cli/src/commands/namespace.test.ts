// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { execa } from "execa";
import { namespaceCommand } from "./namespace.js";

vi.mock("execa", () => ({ execa: vi.fn() }));
afterEach(() => vi.restoreAllMocks());

describe("namespace commands", () => {
  it("runs a read-only generic-context preflight", async () => {
    vi.mocked(execa).mockResolvedValue({ stdout: '{"items":[]}' } as never);
    vi.spyOn(console, "log").mockImplementation(() => {});
    await namespaceCommand().parseAsync(["preflight"], { from: "user" });
    expect(execa).toHaveBeenCalledWith("kubectl", [
      "get", "karssandboxes", "-A", "--show-managed-fields=true", "-o", "json",
    ], { stdio: "pipe" });
  });

  it("refuses adoption unless the administrator supplies both reviewed UIDs", async () => {
    const command = namespaceCommand().exitOverride().configureOutput({
      writeErr: () => {}, writeOut: () => {},
    });
    for (const child of command.commands) {
      child.exitOverride().configureOutput({ writeErr: () => {}, writeOut: () => {} });
    }
    await expect(command.parseAsync(["adopt", "demo", "--namespace", "workspace-a"], {
      from: "user",
    })).rejects.toThrow("required option");
  });

  it("propagates API failures instead of printing preflight success", async () => {
    vi.mocked(execa).mockRejectedValue(new Error("Forbidden"));
    const output = vi.spyOn(console, "log").mockImplementation(() => {});
    await expect(namespaceCommand().parseAsync(["preflight"], { from: "user" })).rejects.toThrow("Forbidden");
    expect(output).not.toHaveBeenCalled();
  });
});
