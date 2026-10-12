// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { runPreflight, type UpOptionsForPreflight } from "./preflight.js";

const state = vi.hoisted(() => ({ execute: vi.fn(), prompt: vi.fn(), loggedIn: true, id: "11111111-1111-4111-8111-111111111111" as unknown }));
vi.mock("execa", () => ({ execa: state.execute }));
vi.mock("inquirer", async original => {
  const actual = await original<typeof import("inquirer")>();
  return { ...actual, default: { ...actual.default, prompt: state.prompt } };
});
vi.mock("../../config.js", () => ({ loadContext: () => null }));
vi.mock("../../stepper.js", () => ({ banner: vi.fn(), checkLine: vi.fn() }));
vi.mock("../../preflight.js", () => ({ runPreflightChecks: vi.fn() }));
vi.mock("../../lib/vm-size.js", () => ({ resolveVmSizes: vi.fn() }));
const selected = "22222222-2222-4222-8222-222222222222";
function options(): UpOptionsForPreflight {
  return { name: "agent", model: "model", region: "eastus2", isolation: "standard", resourceGroup: "rg",
    foundryEndpoint: "https://test.services.ai.azure.com/api/projects/test", build: false, release: true,
    sourceAcr: "mirror", dryRun: false, skipInfra: true, skipPreflight: true, yes: true };
}
beforeEach(() => {
  vi.clearAllMocks();
  state.loggedIn = true;
  state.id = "11111111-1111-4111-8111-111111111111";
  vi.spyOn(console, "log").mockImplementation(() => {});
  state.prompt.mockImplementation(async (questions: { name: string }[]) => {
    if (questions.length === 1 && questions[0]!.name === "subId") return { subId: selected };
    return { region: "eastus2", name: "agent", isolation: "standard" };
  });
  state.execute.mockImplementation(async (bin: string, args: string[]) => {
    if (args[0] === "account" && args[1] === "show") {
      if (args.includes("none") && !state.loggedIn) throw new Error("login required");
      return { stdout: JSON.stringify({ id: state.id, name: "Current" }) };
    }
    if (args[0] === "account" && args[1] === "list") return { stdout: JSON.stringify([
      { id: state.id, name: "Current", isDefault: true }, { id: selected, name: "Selected", isDefault: false },
    ]) };
    if (args[0] === "login") { state.loggedIn = true; return { stdout: "" }; }
    if (args[0] === "account" && args[1] === "set") return { stdout: "" };
    if (args.includes("--version") || args[0] === "version") return { stdout: "tool-version" };
    throw new Error(`Unexpected preflight command: ${bin} ${args.join(" ")}`);
  });
});
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("ordinary up subscription selection", () => {
  it("returns the current subscription without prompting in noninteractive mode", async () => {
    await expect(runPreflight(options())).resolves.toEqual({ rg: "rg", subscriptionId: state.id });
    expect(state.prompt).not.toHaveBeenCalled();
    expect(state.execute.mock.calls.some(([, args]) => args[1] === "set")).toBe(false);
  });
  it("reads and returns the subscription after a successful new login", async () => {
    state.loggedIn = false;
    await expect(runPreflight(options())).resolves.toEqual({ rg: "rg", subscriptionId: state.id });
    const login = state.execute.mock.calls.findIndex(([, args]) => args[0] === "login");
    const read = state.execute.mock.calls.findIndex(([, args]) => args[1] === "show" && args.includes("json"));
    expect(login).toBeGreaterThan(-1);
    expect(read).toBeGreaterThan(login);
  });
  it("propagates the interactive selection after login, not the previously active subscription", async () => {
    vi.stubGlobal("process", { ...process, stdin: { isTTY: true } });
    state.loggedIn = false;
    await expect(runPreflight({ ...options(), yes: false })).resolves.toEqual({ rg: "rg", subscriptionId: selected });
    expect(state.execute).toHaveBeenCalledWith("az", ["account", "set", "--subscription", selected], { stdio: "pipe" });
  });
  it.each([undefined, "", "  "])("refuses absent subscription %j", async id => {
    state.id = id;
    await expect(runPreflight(options())).rejects.toThrow("did not resolve a subscription");
  });
  it("does not log in or select a subscription for an unauthenticated dry-run", async () => {
    state.loggedIn = false;
    await expect(runPreflight({ ...options(), dryRun: true })).resolves.toBeNull();
    expect(state.execute.mock.calls.some(([, args]) => args[0] === "login" || args[1] === "set")).toBe(false);
  });
});
