// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({ execa: vi.fn(), review: vi.fn(), apply: vi.fn(), verify: vi.fn(), journal: vi.fn(), save: vi.fn() }));
vi.mock("execa", () => ({ execa: mocks.execa }));
vi.mock("../lib/dispatcher-image.js", () => ({ reviewDispatcherImage: mocks.review, applyDispatcherImage: mocks.apply, verifyDispatcherImage: mocks.verify }));
vi.mock("../lib/operator-journal.js", () => ({ withOperatorJournal: mocks.journal }));
import { dispatcherImageCommand } from "./dispatcher-image.js";
import { PrivateCommandFailure } from "../lib/private-activation-command-diagnostics.js";

const receipt = { version: 1, phase: "reviewed", review: { context: "kind-reviewed" }, reviewDigest: "digest" };
beforeEach(() => {
  vi.resetAllMocks();
  mocks.execa.mockResolvedValue({ stdout: "{}" });
  mocks.journal.mockImplementation(async (_file, _create, action) => action({ save: mocks.save }, receipt));
  mocks.review.mockResolvedValue(receipt);
  mocks.apply.mockResolvedValue({ ...receipt, phase: "verified" });
  mocks.verify.mockResolvedValue({ ...receipt, phase: "verified" });
});

describe("dispatcher-image command", () => {
  it("creates a new durable review bound to the explicit context", async () => {
    await dispatcherImageCommand().parseAsync(["review", "--context", "kind-selected", "--namespace", "kars-system", "--release", "kars", "--image", "immutable", "--receipt", "review.jsonl"], { from: "user" });
    expect(mocks.journal).toHaveBeenCalledWith("review.jsonl", true, expect.any(Function));
    expect(mocks.review).toHaveBeenCalledWith(expect.any(Function), "kind-selected", "kars-system", "kars", "immutable");
    expect(mocks.save).toHaveBeenCalledWith(receipt);
    await mocks.review.mock.calls[0]![0](["get", "namespaces"]);
    expect(mocks.execa).toHaveBeenCalledWith("kubectl", ["--context", "kind-selected", "get", "namespaces"], { stdio: "pipe" });
  });

  it.each(["apply", "verify"])("%s reopens the existing journal and uses its bound context", async operation => {
    await dispatcherImageCommand().parseAsync([operation, "review.jsonl"], { from: "user" });
    expect(mocks.journal).toHaveBeenCalledWith("review.jsonl", false, expect.any(Function));
    const called = operation === "apply" ? mocks.apply : mocks.verify;
    expect(called).toHaveBeenCalledWith(expect.any(Function), receipt, { save: mocks.save });
    await called.mock.calls[0]![0](["get", "deployment"]);
    expect(mocks.execa.mock.calls[0]![1]).toEqual(["--context", "kind-reviewed", "get", "deployment"]);
    expect(operation === "apply" ? mocks.verify : mocks.apply).not.toHaveBeenCalled();
  });

  it("passes the exact patch on stdin without shell expansion or exposing it in argv", async () => {
    await dispatcherImageCommand().parseAsync(["apply", "review.jsonl"], { from: "user" });
    const args = ["patch", "deployment", "kars-mission-dispatcher", "-n", "kars-system", "--type=json", "--patch-file=/dev/stdin", "-o", "json"];
    const patch = JSON.stringify([{ op: "test", path: "/spec", value: { protected: "BODY_CANARY" } }]);
    await mocks.apply.mock.calls[0]![0](args, patch);
    expect(mocks.execa).toHaveBeenCalledWith("kubectl", ["--context", "kind-reviewed", ...args], { stdio: "pipe", input: patch });
    expect(JSON.stringify(mocks.execa.mock.calls[0]![1])).not.toContain("BODY_CANARY");
  });

  it.each(["Conflict", "Invalid", "Forbidden", "Unknown"])("retains sanitized %s command facts, not captured templates or credentials", async serverReason => {
    await dispatcherImageCommand().parseAsync(["apply", "review.jsonl"], { from: "user" });
    mocks.execa.mockRejectedValue(Object.assign(new Error("MESSAGE_CANARY"), {
      stderr: `Error from server (${serverReason}): STDERR_CANARY`, stdout: "STDOUT_CANARY", exitCode: 1,
      cause: new Error("CAUSE_CANARY"), command: "ARGV_CANARY",
    }));
    const error = await mocks.apply.mock.calls[0]![0](["patch", "deployment", "NAME_CANARY"], "BODY_CANARY").catch((error: unknown) => error);
    expect(error).toBeInstanceOf(PrivateCommandFailure);
    expect(error.message).toContain("no automatic retry");
    expect(error.message).toContain(serverReason);
    expect(error.facts).toEqual({ version: 1, phase: "Unscoped", operation: "patch", resourceKind: "Deployment", serverReason, exitCode: 1 });
    expect(`${JSON.stringify(error)} ${error.stack}`).not.toContain("CANARY");
    expect(error).not.toHaveProperty("cause");
  });

  it("safely reports an unstructured local subprocess failure", async () => {
    await dispatcherImageCommand().parseAsync(["apply", "review.jsonl"], { from: "user" });
    mocks.execa.mockRejectedValue(new Error("SECRET_CANARY"));
    const error = await mocks.apply.mock.calls[0]![0](["patch", "deployment"], "BODY_CANARY").catch((error: unknown) => error);
    expect(error.facts.serverReason).toBe("Unknown");
    expect(error.facts.exitCode).toBeNull();
    expect(`${JSON.stringify(error)} ${error.stack}`).not.toContain("CANARY");
  });
});
