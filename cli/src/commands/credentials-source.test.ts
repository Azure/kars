// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { credentialsCommand } from "./credentials.js";
import { addCommand } from "./add.js";

const mocks = vi.hoisted(() => ({
  source: vi.fn(), direct: vi.fn(), execute: vi.fn(),
  spinner: { start: vi.fn(), succeed: vi.fn(), fail: vi.fn(), warn: vi.fn() },
}));
vi.mock("execa", () => ({ execa: mocks.execute }));
vi.mock("ora", () => ({ default: () => mocks.spinner }));
vi.mock("../config.js", async importOriginal => ({
  ...await importOriginal<typeof import("../config.js")>(),
  resolveSecret: (_value: string | undefined) => undefined,
  loadContext: () => undefined,
}));
vi.mock("../lib/credential-source.js", async importOriginal => ({
  ...await importOriginal<typeof import("../lib/credential-source.js")>(),
  updateCredentialSource: mocks.source, updateDirectCredentials: mocks.direct,
}));

beforeEach(() => {
  vi.clearAllMocks();
  mocks.spinner.start.mockReturnValue(mocks.spinner);
  mocks.execute.mockResolvedValue({ stdout: "" });
  mocks.direct.mockResolvedValue(undefined);
  mocks.source.mockResolvedValue({ kind: "source", reference: { name: "source", uid: "source-uid" } });
  vi.spyOn(console, "log").mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

describe("credentials command source integration", () => {
  it("routes opt-in flags to the workspace and never prints source values or restarts directly", async () => {
    await credentialsCommand().parseAsync([
      "update", "demo", "--namespace", "workspace-a", "--use-source", "--telegram-token", "SENSITIVE-INPUT",
    ], { from: "user" });
    expect(mocks.source).toHaveBeenCalledWith(mocks.execute, "demo", "workspace-a", {
      updates: { TELEGRAM_BOT_TOKEN: "SENSITIVE-INPUT" }, remove: [],
      useSource: true, disableSource: undefined, restart: true,
    });
    expect(mocks.direct).not.toHaveBeenCalled();
    expect(mocks.execute).not.toHaveBeenCalled();
    expect(vi.mocked(console.log).mock.calls.flat().join(" ")).not.toContain("SENSITIVE-INPUT");
  });

  it("supports remote removal and explicit source disable using existing credentials update", async () => {
    await credentialsCommand().parseAsync(["update", "demo", "--remove", "telegram-token"], { from: "user" });
    expect(mocks.source.mock.calls[0][3].remove).toEqual(["TELEGRAM_BOT_TOKEN"]);
    await credentialsCommand().parseAsync(["update", "demo", "--disable-source"], { from: "user" });
    expect(mocks.source.mock.calls[1][3].disableSource).toBe(true);
  });

  it("keeps legacy flags and --no-restart working on the direct path", async () => {
    mocks.source.mockResolvedValue(undefined);
    await credentialsCommand().parseAsync([
      "update", "demo", "--openai-api-key", "legacy-provider", "--no-restart",
    ], { from: "user" });
    expect(mocks.direct).toHaveBeenCalledWith(mocks.execute, "demo", { OPENAI_API_KEY: "legacy-provider" }, []);
    expect(mocks.execute).not.toHaveBeenCalled();
  });
});

describe("source-mode add flags", () => {
  it.each(["openclaw", "openai-agents", "microsoft-agent-framework", "langgraph", "anthropic", "pydantic-ai", "hermes"])(
    "permits agent credential environment flags for %s without publishing a fake UID", async runtime => {
      await addCommand().parseAsync([
        "demo", "--runtime", runtime, "--credential-source", "--telegram-token", "SENSITIVE-INPUT", "--dry-run",
      ], { from: "user" });
      expect(mocks.execute).not.toHaveBeenCalled();
      const output = vi.mocked(console.log).mock.calls.flat().join(" ");
      expect(output).toContain("No runnable source-bound manifest");
      expect(output).not.toContain("SENSITIVE-INPUT");
    },
  );
});
