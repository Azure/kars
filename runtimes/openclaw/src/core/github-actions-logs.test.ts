// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import * as http from "node:http";
import { once } from "node:events";
import {
  fetchGitHubActionsJobLogs, normalizeGitHubJobLogRequest, tailLogText,
} from "./github-actions-logs.js";
import { registerGitHubActionsTool } from "./agt-tools/github-actions.js";

const servers: http.Server[] = [];
async function server(handler: http.RequestListener): Promise<string> {
  const instance = http.createServer(handler);
  servers.push(instance);
  instance.listen(0, "127.0.0.1");
  await once(instance, "listening");
  const address = instance.address() as { port: number };
  return `http://127.0.0.1:${address.port}`;
}

afterEach(async () => {
  vi.unstubAllEnvs();
  for (const instance of servers.splice(0)) {
    instance.closeAllConnections();
    await new Promise<void>((resolve) => instance.close(() => resolve()));
  }
});

describe("GitHub Actions bounded keyless client", () => {
  it("validates complete path segments and positive bounded job IDs before HTTP", () => {
    expect(normalizeGitHubJobLogRequest("Owner", "repo", "42", undefined).tailLines).toBe(250);
    expect(normalizeGitHubJobLogRequest("Owner", "repo", "42", 9000).tailLines).toBe(2000);
    expect(normalizeGitHubJobLogRequest("Owner", "repo", "42", -1).tailLines).toBe(1);
    for (const repo of ["..", ".", "%2e%2e", "repo/other", "repo?token=x", "repo\\other", "x".repeat(101), {}]) {
      expect(() => normalizeGitHubJobLogRequest("owner", repo, "42", undefined)).toThrow();
    }
    for (const id of ["", "0", "-1", "1e2", "../logs", "18446744073709551616", 42]) {
      expect(() => normalizeGitHubJobLogRequest("owner", "repo", id, undefined)).toThrow();
    }
    for (const lines of [NaN, Infinity, "20", null]) {
      expect(() => normalizeGitHubJobLogRequest("owner", "repo", "42", lines)).toThrow();
    }
    expect(tailLogText("a\r\nb\r\nc", 2)).toBe("b\nc");
  });

  it("retrieves actual HTTP log bytes and truncation metadata without any credentials", async () => {
    let requests = 0;
    const base = await server((req, res) => {
      requests++;
      expect(req.url).toBe("/gh-api/repos/Owner/repo/actions/jobs/42/logs");
      expect(req.headers.authorization).toBeUndefined();
      expect(req.headers.cookie).toBeUndefined();
      res.writeHead(200, { "content-type": "text/plain", "x-kars-log-truncated": "true" });
      res.end("line one\nline two\nline three");
    });
    vi.stubEnv("KARS_ROUTER_URL", base);
    const response = JSON.parse(await fetchGitHubActionsJobLogs("Owner", "repo", "42", 2));
    expect(response).toMatchObject({
      repository: "Owner/repo", job_id: "42", http_status: 200,
      tail_lines: 2, truncated_before_tail: true, log: "line two\nline three",
    });
    expect(requests).toBe(1);
  });

  it("returns actionable status but never upstream error bodies or redirect URLs", async () => {
    let sinkRequests = 0;
    const sink = await server((_req, res) => { sinkRequests++; res.end("leaked"); });
    const base = await server((_req, res) => {
      res.writeHead(302, { location: `${sink}/?sig=signed-secret` });
      res.end("sensitive-upstream-body");
    });
    vi.stubEnv("KARS_ROUTER_URL", base);
    await expect(fetchGitHubActionsJobLogs("owner", "repo", "42", undefined))
      .rejects.toThrow("GitHub Actions job log request returned HTTP 302");
    expect(sinkRequests).toBe(0);
  });

  it("bounds even a single oversized chunk and rejects incomplete streams", async () => {
    const base = await server((_req, res) => res.end(Buffer.alloc(2 * 1024 * 1024 + 1, "a")));
    vi.stubEnv("KARS_ROUTER_URL", base);
    await expect(fetchGitHubActionsJobLogs("owner", "repo", "42", undefined))
      .rejects.toThrow("exceeds the service limit");
    const partial = await server((_req, res) => {
      res.writeHead(200, { "content-length": "100" });
      res.write("partial");
      res.flushHeaders();
      setImmediate(() => res.destroy());
    });
    vi.stubEnv("KARS_ROUTER_URL", partial);
    await expect(fetchGitHubActionsJobLogs("owner", "repo", "42", undefined))
      .rejects.toThrow(/interrupted|failed/);
  });

  it("does not accept remote, credential-bearing or non-HTTP router configuration", async () => {
    for (const base of ["https://127.0.0.1:8443", "http://evil.example", "http://token@127.0.0.1:8443"]) {
      vi.stubEnv("KARS_ROUTER_URL", base);
      await expect(fetchGitHubActionsJobLogs("owner", "repo", "42", undefined))
        .rejects.toThrow("loopback HTTP router");
    }
  });

  it("registers a working tool with a real HTTP execution and precise errors", async () => {
    const base = await server((_req, res) => res.end("CI diagnostic"));
    vi.stubEnv("KARS_ROUTER_URL", base);
    const registerTool = vi.fn();
    registerGitHubActionsTool({ registerTool });
    expect(registerTool).toHaveBeenCalledTimes(1);
    const tool = registerTool.mock.calls[0][0];
    expect(tool.name).toBe("github_actions_job_logs");
    const result = await tool.execute("call", { owner: "owner", repo: "repo", job_id: "42" });
    expect(JSON.parse(result.content[0].text).log).toBe("CI diagnostic");
    const failure = await tool.execute("call", { owner: "owner", repo: "..", job_id: "42" });
    expect(failure.isError).toBe(true);
    expect(failure.content[0].text).toContain("safe GitHub path segments");
  });

  it("wires the real plugin through governance without logging CI content", async () => {
    let allowed = false;
    let logRequests = 0;
    const base = await server((req, res) => {
      if (req.url === "/agt/evaluate") {
        req.resume();
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify({ allowed, reason: "test policy", matched_rule: "github-policy" }));
      } else {
        logRequests++;
        res.end("workflow-sensitive-output");
      }
    });
    vi.stubEnv("KARS_ROUTER_URL", base);
    vi.stubEnv("AGT_SKIP_INIT", "1");
    type RegisteredTool = {
      execute(id: string, params: Record<string, unknown>): Promise<{
        content: Array<{ text: string }>;
      }>;
    };
    const tools = new Map<string, RegisteredTool>();
    const log = vi.fn();
    const plugin = (await import("../index.js")).default;
    plugin.register({
      id: "kars", name: "kars", version: "test", registrationMode: "discovery",
      config: {}, pluginConfig: {}, logger: { info: log, warn: log, error: log },
      registerTool: (tool: RegisteredTool & { name: string }) => { tools.set(tool.name, tool); },
      registerCommand: vi.fn(), registerProvider: vi.fn(), registerCli: vi.fn(),
      resolvePath: (path: string) => path,
    });
    const tool = tools.get("github_actions_job_logs");
    expect(tool).toBeDefined();
    const params = { owner: "owner", repo: "repo", job_id: "42" };
    const denied = await tool!.execute("denied", params);
    expect(denied.content[0].text).toContain("Blocked by AGT policy");
    expect(logRequests).toBe(0);
    allowed = true;
    const result = await tool!.execute("allowed", params);
    expect(JSON.parse(result.content[0].text).log).toBe("workflow-sensitive-output");
    expect(logRequests).toBe(1);
    expect(JSON.stringify(log.mock.calls)).not.toContain("workflow-sensitive-output");
  });
});
