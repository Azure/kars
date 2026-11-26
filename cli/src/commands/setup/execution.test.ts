// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { expect, it, vi } from "vitest";
const execute = vi.hoisted(() => vi.fn(async () => ({ stdout: "fixture" })));
vi.mock("execa", () => ({ execa: execute }));
import { authorizeSetupTerminal, executeSetup } from "./backend.js";

it("requests sudo validation in the inherited terminal, without capturing a password or adding a deadline", async () => {
  const signal = new AbortController().signal;
  const stderr = vi.spyOn(process.stderr, "write").mockReturnValue(true);
  try {
    await authorizeSetupTerminal(signal, true);
    expect(execute).toHaveBeenLastCalledWith("sudo", ["-v"], { stdio: "inherit", timeout: 0, cancelSignal: signal });
  } finally { stderr.mockRestore(); }
});

it("refuses to prompt without a terminal and sanitizes authorization failures", async () => {
  const signal = new AbortController().signal;
  execute.mockClear();
  await expect(authorizeSetupTerminal(signal, false)).rejects.toThrow("interactive terminal");
  expect(execute).not.toHaveBeenCalled();
  const stderr = vi.spyOn(process.stderr, "write").mockReturnValue(true);
  execute.mockRejectedValueOnce(new Error("private-sudo-output"));
  try {
    await expect(authorizeSetupTerminal(signal, true)).rejects.toThrow("Terminal authorization was cancelled, denied, or failed");
  } finally { stderr.mockRestore(); }
});

it("checks sudo version without incompatible flags but keeps actual commands noninteractive", async () => {
  await executeSetup("/usr/bin/sudo", ["--version"]);
  expect(execute).toHaveBeenLastCalledWith("/usr/bin/sudo", ["--version"], expect.objectContaining({ stdio: "pipe" }));
  await executeSetup("/usr/bin/sudo", ["true"]);
  expect(execute).toHaveBeenLastCalledWith("/usr/bin/sudo", ["-n", "true"], expect.objectContaining({ stdio: "pipe" }));
  await executeSetup("/usr/bin/sudo", ["--version", "unexpected"]);
  expect(execute).toHaveBeenLastCalledWith("/usr/bin/sudo", ["-n", "--version", "unexpected"], expect.anything());
});
