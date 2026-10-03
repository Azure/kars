// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { createServer, type Server, type ServerResponse } from "node:http";
import { mkdtemp, readFile, rm, access } from "node:fs/promises";
import { executeTaskWithEvidence, processTaskWithTools, type TaskLoopDeps } from "./agt-task-loop.js";
import { TaskCompletionLedger, TaskExecutionError } from "./task-completion.js";

const log = { info: vi.fn(), warn: vi.fn() };
const deps: TaskLoopDeps = { meshClient: () => null, isInterruptRequested: () => false, interruptReason: () => "", setInterrupt: () => {} };
const usage = { prompt_tokens: 7, completion_tokens: 4, total_tokens: 11 };
const final = (content: unknown = "A useful briefing", finish_reason = "stop", measured: unknown = usage) => ({
  choices: [{ finish_reason, message: { role: "assistant", content } }], usage: measured,
});
let server: Server | undefined;
let requests: Record<string, any>[];
let policyRequests: Record<string, any>[];
const tempDirectories: string[] = [];
async function workspace() {
  const directory = await mkdtemp("/tmp/kars-mission-artifacts-");
  tempDirectories.push(directory);
  return directory;
}
async function router(responses: unknown[], policyStatus = 200, policyBody: unknown = { allowed: false }) {
  requests = []; policyRequests = [];
  let index = 0;
  server = createServer((req, res) => {
    let body = "";
    req.on("data", (data) => { body += data; });
    req.on("end", () => {
      if (req.url === "/agt/evaluate") {
        policyRequests.push(JSON.parse(body));
        res.writeHead(policyStatus, { "content-type": "application/json" });
        res.end(typeof policyBody === "string" ? policyBody : JSON.stringify(policyBody));
      } else {
        requests.push(JSON.parse(body));
        const response = responses[Math.min(index++, responses.length - 1)];
        if (typeof response === "function") response(res);
        else {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify(response));
        }
      }
    });
  });
  await new Promise<void>((resolve) => server!.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing test listener");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
  vi.stubEnv("OPENCLAW_MODEL", "");
  vi.stubEnv("KARS_MODEL", "gpt-5.4-mini");
}

afterEach(async () => {
  vi.unstubAllEnvs();
  if (server) await new Promise<void>((resolve, reject) => server!.close((error) => error ? reject(error) : resolve()));
  server = undefined;
  await Promise.all(tempDirectories.splice(0).map(path => rm(path, { recursive: true, force: true })));
});

const tool = (name = "mesh_inbox", args = {}) => ({
  choices: [{ finish_reason: "tool_calls", message: { role: "assistant", content: null, tool_calls: [{ id: "call-1", type: "function", function: { name, arguments: JSON.stringify(args) } }] } }], usage,
});

describe("explicit mission attachments", () => {
  it("delivers exact written text after policy authorization with a strict negotiated schema", async () => {
    const directory = await workspace();
    const args = { path: `${directory}/briefing.md`, content: "# Briefing\r\nCafé — 日本語 🌍\n", artifact_name: "briefing.md" };
    await router([tool("file_write", args), final("Attached briefing")], 200, { allowed: true });
    vi.stubEnv("KARS_STRICT_TOOLS", "1");
    const result = await executeTaskWithEvidence("Write a briefing", deps, log, true);
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(await readFile(args.path, "utf8")).toBe(args.content);
    expect(policyRequests).toContainEqual({ action: "tool:file_write", context: { tool: "file_write", tool_call_id: "call-1", arguments: args } });
    const schema = requests[0].tools.find((entry: any) => entry.function.name === "file_write").function;
    expect(schema.strict).toBe(true);
    expect(schema.parameters.properties.artifact_name).toMatchObject({ type: ["string", "null"] });
    expect(schema.parameters.required).toContain("artifact_name");
    expect(requests[1].messages.at(-1).content).toContain("attached as briefing.md");
  });
  it.each([null, undefined])("keeps scratch writes local when artifact name is %j", async artifact_name => {
    const directory = await workspace();
    const args = { path: `${directory}/scratch.md`, content: "private scratch", artifact_name };
    await router([tool("file_write", args), final()], 200, { allowed: true });
    expect(await executeTaskWithEvidence("Write", deps, log, true)).not.toHaveProperty("artifacts");
    expect(await readFile(args.path, "utf8")).toBe(args.content);
    expect(requests[1].messages.at(-1).content).toContain("local only");
  });
  it("does not write or attach denied content", async () => {
    const path = `${await workspace()}/denied.md`;
    await router([tool("file_write", { path, content: "denied", artifact_name: "denied.md" }), final()]);
    expect(await executeTaskWithEvidence("Write", deps, log, true)).not.toHaveProperty("artifacts");
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
  });
  it.each(["response.md", "../secret.md", "__proto__"])("does not write invalid attachment %s", async artifact_name => {
    const path = `${await workspace()}/invalid.md`;
    await router([tool("file_write", { path, content: "draft", artifact_name }), final()], 200, { allowed: true });
    expect(await executeTaskWithEvidence("Write", deps, log, true)).not.toHaveProperty("artifacts");
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests[1].messages.at(-1).content).toContain("file_write error:");
  });
  it.each(["  \n", "🌍".repeat(33 * 1024)])("rejects invalid attachment content before writing (%#)", async content => {
    const path = `${await workspace()}/invalid.md`;
    await router([tool("file_write", { path, content, artifact_name: "invalid.md" }), final()], 200, { allowed: true });
    expect(await executeTaskWithEvidence("Write", deps, log, true)).not.toHaveProperty("artifacts");
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests[1].messages.at(-1).content).toContain("file_write error:");
  });
  it("rejects the seventeenth attachment before writing while retaining the first sixteen", async () => {
    const directory = await workspace();
    await router([...Array.from({ length: 17 }, (_, i) => tool("file_write", {
      path: `${directory}/${i}.md`, content: `document ${i}`, artifact_name: `${i}.md`,
    })), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true);
    expect(result.artifacts).toEqual(Object.fromEntries(Array.from({ length: 16 }, (_, i) => [`${i}.md`, `document ${i}`])));
    expect(await readFile(`${directory}/15.md`, "utf8")).toBe("document 15");
    await expect(access(`${directory}/16.md`)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests[17].messages.at(-1).content).toContain("file_write error:");
  });
  it("keeps the last successful explicitly attached write, not failed or scratch writes", async () => {
    const directory = await workspace();
    const path = `${directory}/briefing.md`;
    await router([
      tool("file_write", { path, content: "first", artifact_name: "briefing.md" }),
      tool("file_write", { path, content: "second 🌍", artifact_name: "briefing.md" }),
      tool("file_write", { path: directory, content: "failed overwrite", artifact_name: "briefing.md" }),
      tool("file_write", { path, content: "private scratch", artifact_name: null }), final(),
    ], 200, { allowed: true });
    expect((await executeTaskWithEvidence("Write", deps, log, true)).artifacts).toEqual({ "briefing.md": "second 🌍" });
    expect(await readFile(path, "utf8")).toBe("private scratch");
    expect(requests[3].messages.at(-1).content).toContain("file_write error:");
  });
  it("isolates artifacts between executions and excludes them from failed evidence", async () => {
    const path = `${await workspace()}/briefing.md`;
    await router([tool("file_write", { path, content: "staged text", artifact_name: "briefing.md" }), final("Truncated", "length"), final()], 200, { allowed: true });
    const error = await executeTaskWithEvidence("Write", deps, log, true).catch(error => error);
    expect(error).toBeInstanceOf(TaskExecutionError);
    expect(error.evidence).toEqual({ model: "gpt-5.4-mini", rounds: 2, usage: { promptTokens: 14, completionTokens: 8, totalTokens: 22 } });
    expect(error).not.toHaveProperty("artifacts");
    expect(await executeTaskWithEvidence("New revision", deps, log, true)).not.toHaveProperty("artifacts");
  });
  it("does not advertise or accept attachments without negotiation", async () => {
    const path = `${await workspace()}/briefing.md`;
    await router([tool("file_write", { path, content: "draft", artifact_name: "briefing.md" }), final()], 200, { allowed: true });
    expect(await executeTaskWithEvidence("Write", deps, log)).not.toHaveProperty("artifacts");
    expect(requests[0].tools.find((entry: any) => entry.function.name === "file_write").function.parameters.properties).not.toHaveProperty("artifact_name");
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
  });
  it("preserves the legacy write result and schema", async () => {
    const path = `${await workspace()}/legacy.md`;
    await router([tool("file_write", { path, content: "legacy" }), final()]);
    expect(await processTaskWithTools("Write", deps, log)).toBe("A useful briefing");
    expect(requests[0].tools.find((entry: any) => entry.function.name === "file_write").function.parameters.properties).not.toHaveProperty("artifact_name");
    expect(requests[1].messages.at(-1).content).toBe(`OK: wrote 6 bytes to ${path}`);
    expect(policyRequests).toEqual([]);
  });
  it("offers inline or corrected-JSON delivery after malformed mission arguments, not peer transfer", async () => {
    const malformed = tool("file_write"); malformed.choices[0].message.tool_calls[0].function.arguments = "{";
    await router([malformed, final()]);
    await executeTaskWithEvidence("Write", deps, log, true);
    expect(requests[1].messages.at(-1).content).toContain("return the complete deliverable in the final response");
    expect(requests[1].messages.at(-1).content).not.toContain("mesh_transfer_file");
    expect(policyRequests).toEqual([]);
  });
  it("projects only cloned accounting evidence into execution errors", () => {
    const result = { model: "model", rounds: 1, output: "private draft", artifacts: { "a.md": "private" }, usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3, private: "data" } };
    const error = new TaskExecutionError("failed", result);
    result.usage.totalTokens = 9;
    expect(error.evidence).toEqual({ model: "model", rounds: 1, usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3 } });
    expect(new TaskExecutionError("failed", { ...result, usage: null }).evidence.usage).toBeNull();
  });
  it("stages isolated snapshots without committing invalid maps", () => {
    const ledger = new TaskCompletionLedger("model", true);
    const staged = ledger.prepareArtifact("a.md", "first");
    expect(ledger.artifactSnapshot()).toEqual({});
    ledger.commitArtifacts(staged); staged["a.md"] = "mutated";
    const snapshot = ledger.artifactSnapshot(); snapshot.artifacts!["a.md"] = "mutated again";
    expect(ledger.artifactSnapshot()).toEqual({ artifacts: { "a.md": "first" } });
    expect(() => ledger.commitArtifacts({ "response.md": "invalid" })).toThrow();
    expect(() => new TaskCompletionLedger("model").commitArtifacts({ "a.md": "text" })).toThrow();
    expect(ledger.artifactSnapshot()).toEqual({ artifacts: { "a.md": "first" } });
  });
});

describe("measured task execution through the local router", () => {
  it("returns real output and aggregate provider usage across tool rounds", async () => {
    await router([tool(), final()]);
    const onEvidence = vi.fn();
    expect(await executeTaskWithEvidence("Write a briefing", { ...deps, onEvidence }, log)).toEqual({
      output: "A useful briefing", model: "gpt-5.4-mini", rounds: 2,
      usage: { promptTokens: 14, completionTokens: 8, totalTokens: 22 },
    });
    expect(requests).toHaveLength(2);
    expect(requests[0]).toMatchObject({ model: "gpt-5.4-mini", max_completion_tokens: 8192 });
    expect(onEvidence).toHaveBeenCalledTimes(2);
  });

  it.each([null, undefined, "Checking the inbox"])("serializes only request fields in a tool follow-up (content %j)", async (content) => {
    const response = tool();
    const message = response.choices[0].message;
    await router([{ ...response, choices: [{ finish_reason: "tool_calls", message: {
      ...message, content, refusal: null, annotations: [], provider_metadata: { trace: "response-only" },
      tool_calls: message.tool_calls.map((call) => ({ ...call, index: 0,
        function: { ...call.function, response_metadata: "response-only" } })),
    } }] }, final()]);
    await executeTaskWithEvidence("Write", deps, log);
    expect(requests[1].messages[2]).toEqual({
      role: "assistant", content: content ?? null, refusal: null, tool_calls: message.tool_calls,
    });
    expect(requests[1].messages[3]).toMatchObject({ role: "tool", tool_call_id: "call-1" });
  });

  it.each([
    { role: "user" }, { role: undefined }, { content: { text: "invalid" } },
    { content: [{ type: "audio", data: "unsupported" }] }, { refusal: {} },
  ])("rejects unsupported assistant tool messages before side effects: %j", async (fields) => {
    const response = tool("exec_command", { command: "printf SHOULD_NOT_EXECUTE" });
    Object.assign(response.choices[0].message, fields);
    await router([response, final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({
      name: "TaskExecutionError", message: expect.stringContaining("Invalid assistant tool message"),
      evidence: { rounds: 1, usage: { totalTokens: 11 } },
    });
    expect(requests).toHaveLength(1);
  });

  it.each(["length", "content_filter", "tool_calls", "unknown"])("does not deliver a %s response", async (reason) => {
    await router([final("Partial answer", reason)]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({ name: "TaskExecutionError", evidence: { rounds: 1, usage: { totalTokens: 11 } } });
  });

  it.each(["", "  ", null, { text: "not a string" }])("rejects empty or malformed output %j", async (content) => {
    await router([final(content)]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toThrow("no final deliverable");
  });

  it.each([undefined, null, {}, { ...usage, total_tokens: 12 }, { ...usage, prompt_tokens: -1 }, { ...usage, total_tokens: 1.5 }])("never invents missing or invalid usage %j", async (measured) => {
    await router([{ ...final(), usage: measured }]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({ evidence: { usage: null } });
  });

  it("keeps aggregate usage unknown when one intermediate response is unmetered", async () => {
    await router([{ ...tool(), usage: undefined }, final()]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({ evidence: { rounds: 2, usage: null } });
  });

  it("reports round exhaustion as failure with measured usage, not a deliverable", async () => {
    await router([tool()]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({ message: expect.stringContaining("maximum tool-calling rounds"), evidence: { rounds: 25, usage: { totalTokens: 275 } } });
    expect(requests).toHaveLength(25);
  });

  it.each([[503, { allowed: true }], [200, {}], [200, null], [200, "invalid-json"], [200, { allowed: "true" }]])("blocks shell execution without an explicit successful policy response (%s, %j)", async (status, body) => {
    await router([tool("exec_command", { command: "printf SHOULD_NOT_EXECUTE" }), final()], status as number, body);
    await executeTaskWithEvidence("Write", deps, log);
    expect(requests[1].messages.at(-1).content).toContain("Blocked by policy");
    expect(requests[1].messages.at(-1).content).not.toContain("SHOULD_NOT_EXECUTE");
  });

  it.each([
    (res: ServerResponse) => { res.writeHead(503); res.end("unavailable"); },
    (res: ServerResponse) => { res.end("invalid-json"); },
    (res: ServerResponse) => { res.writeHead(200, { "Content-Length": 1000 }); res.write("{"); res.flushHeaders(); setImmediate(() => res.destroy()); },
  ])("marks execution usage unknown after an unmeasured later HTTP attempt %#", async (failure) => {
    await router([tool(), failure]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({ name: "TaskExecutionError", evidence: { rounds: 1, usage: null } });
  });

  it("fails an interrupted mission without claiming resumability or calling the model", async () => {
    await router([final()]);
    const setInterrupt = vi.fn();
    await expect(executeTaskWithEvidence("Write", { ...deps, isInterruptRequested: () => true, setInterrupt }, log)).rejects.toMatchObject({ evidence: { rounds: 0, usage: null } });
    expect(setInterrupt).toHaveBeenCalledWith(false, "");
    expect(requests).toHaveLength(0);
  });

  it("fails closed when the required evidence callback fails", async () => {
    await router([final()]);
    await expect(executeTaskWithEvidence("Write", { ...deps, onEvidence: () => { throw new Error("evidence unavailable"); } }, log)).rejects.toMatchObject({ message: "evidence unavailable", evidence: { usage: { totalTokens: 11 } } });
  });

  it("does not execute malformed tool-call envelopes", async () => {
    const response = tool("exec_command", { command: "printf SHOULD_NOT_EXECUTE" });
    response.choices[0].finish_reason = "stop";
    await router([response]);
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toThrow("Invalid tool-call");
    expect(requests).toHaveLength(1);
  });

  it("never routes unknown strict tool names to the shell fallback", async () => {
    await router([tool("invented_tool", { command: "printf SHOULD_NOT_EXECUTE" }), final()], 200, { allowed: true });
    await executeTaskWithEvidence("Write", deps, log);
    expect(requests[1].messages.at(-1).content).toContain("Blocked by policy");
  });

  it("executes an explicitly authorized harmless shell command", async () => {
    await router([tool("exec_command", { command: "printf TASK_POLICY_ALLOWED" }), final()], 200, { allowed: true });
    await executeTaskWithEvidence("Write", deps, log);
    expect(requests[1].messages.at(-1).content).toBe("TASK_POLICY_ALLOWED");
  });

  it("preserves the legacy string return and 2048 output bound", async () => {
    await router([{ ...final(), usage: undefined }]);
    expect(await processTaskWithTools("Write", deps, log)).toBe("A useful briefing");
    expect(requests[0].max_completion_tokens).toBe(2048);
  });
});

describe("task completion accounting", () => {
  it("does not expose mutable ledger state", () => {
    const ledger = new TaskCompletionLedger("model");
    expect(ledger.snapshot().usage).toBeNull();
    ledger.record(final());
    ledger.snapshot().usage!.totalTokens = 0;
    expect(ledger.snapshot().usage!.totalTokens).toBe(11);
  });
  it("rejects aggregate integer overflow", () => {
    const ledger = new TaskCompletionLedger("model");
    const response = final("answer", "stop", { prompt_tokens: Number.MAX_SAFE_INTEGER, completion_tokens: 0, total_tokens: Number.MAX_SAFE_INTEGER });
    ledger.record(response);
    ledger.record(response);
    expect(ledger.snapshot().usage).toBeNull();
    expect(() => ledger.finish(final().choices[0])).toThrow(TaskExecutionError);
  });
});
