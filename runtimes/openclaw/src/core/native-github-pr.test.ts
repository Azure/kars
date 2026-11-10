// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { GITHUB_PR_PARAMETERS, githubPrArgumentError, nativeGithubPr } from "./native-github-pr.js";
import { TaskPhaseGuard } from "./task-phase.js";
import { readTaskFile, writeTaskFile } from "./task-filesystem.js";

const inspect = { operation: "inspect", repository: "owner/repo", path: "README.md" };
const create = { ...inspect, operation: "create", expected_base: "a".repeat(40), content: "Reviewed 🌍\n", title: "Docs", body: "Review" };
const phase = (capabilities: string[], executionMode: string | undefined = "native", maxToolCalls = 8) =>
  new TaskPhaseGuard({ name: "docs", objective: "Produce the reviewed documentation PR", capabilities,
    maxToolCalls, ...(executionMode ? { executionMode } : {}) });

describe("explicit reviewed GitHub adapter", () => {
  it("requires native github-pr, not filesystem/network or zero tool authority", () => {
    expect(phase(["github-pr"]).allowsTool("github_pr")).toBe(true);
    expect(phase(["filesystem-read", "filesystem-write", "network"]).allowsTool("github_pr")).toBe(false);
    expect(phase(["github-pr"], "native", 0).allowsTool("github_pr")).toBe(false);
    expect(() => phase(["github-pr"], "")).toThrow();
    expect(phase(["github-pr"]).allowsTool("http_fetch")).toBe(false);
  });
  it("accepts only bounded inspect/create inputs", () => {
    expect(githubPrArgumentError(inspect)).toBeUndefined();
    expect(githubPrArgumentError(create)).toBeUndefined();
    expect(phase(["github-pr"]).argumentError("github_pr", create)).toBeUndefined();
  });
  it("accepts strict-provider null placeholders without making inspect a mutation", () => {
    const args = { ...inspect, expected_base: null, content: null, title: null, body: null };
    expect(githubPrArgumentError(args)).toBeUndefined();
    expect(phase(["github-pr"]).argumentError("github_pr", args)).toBeUndefined();
    expect(githubPrArgumentError({ ...args, branch: null })).toBeDefined();
    expect(githubPrArgumentError({ ...args, operation: "create" })).toBeDefined();
  });
  it.each(["expected_base", "content", "title", "body"] as const)("keeps %s nullable only for inspect", key => {
    expect(GITHUB_PR_PARAMETERS.properties[key].type).toEqual(["string", "null"]);
    expect(githubPrArgumentError({ ...inspect, [key]: null })).toBeUndefined();
    for (const value of ["", "ignored", 0, false, {}, []]) {
      expect(githubPrArgumentError({ ...inspect, [key]: value })).toBeDefined();
    }
    expect(githubPrArgumentError({ ...create, [key]: null })).toBeDefined();
  });
  it.each<Record<string, unknown>>([
    { path: "../x" }, { path: "/x" }, { path: ".github/workflows/test.yml" }, { path: ".GIT/config" },
    { path: "a/.gitmodules" }, { path: "-c" }, { path: "a//b" }, { path: "a\\b" }, { path: ":(glob)*" },
    { repository: "https://github.com/o/r" }, { repository: "owner/repo.git" }, { repository: "o/r\n" },
    { branch: "main" }, { refspec: "HEAD:main" }, { force: true }, { config: "credential.helper=evil" },
    { constructor: "inherited" }, { content: "\ud800" }, { content: "x\0" }, { title: "new\nline" },
    { expected_base: "-c" }, { content: "x".repeat(131073) },
  ])("rejects path, argument, config and encoding bypass %j", invalid => {
    expect(githubPrArgumentError({ ...create, ...invalid })).toBeDefined();
  });
  it("denies native filesystem access to private transaction receipts before any IO", async () => {
    await expect(readTaskFile("/sandbox/.kars-github/receipt.json", 100)).rejects.toThrow("Private Git");
    await expect(writeTaskFile("/sandbox/.kars-github/../receipt.json", "x")).rejects.toThrow("Private Git");
  });
  it("does not launch a helper for a cancelled assignment", async () => {
    const controller = new AbortController();
    controller.abort(new Error("Cancelled"));
    await expect(nativeGithubPr(inspect, { taskUid: "task", runNonce: "run", assignmentId: "assignment" }, controller.signal))
      .rejects.toThrow("Cancelled");
  });
  it("packages exactly the single maintained shared engine", () => {
    expect(readFileSync(new URL("./native-github-pr.py", import.meta.url)))
      .toEqual(readFileSync(new URL("../../../hermes/src/kars_runtime_hermes/plugin/native_github_pr.py", import.meta.url)));
  });
});
