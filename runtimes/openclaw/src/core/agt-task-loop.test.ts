// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { createServer, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { mkdtemp, readFile, rm, access, writeFile, symlink } from "node:fs/promises";
import { executeTaskWithEvidence, processTaskWithTools, type TaskLoopDeps } from "./agt-task-loop.js";
import { TaskCompletionLedger, TaskExecutionError, type TaskExecutionEvidence } from "./task-completion.js";
import { MissionReceiver, type MissionReceiverOptions } from "./mission-receiver.js";
import { missionObjectiveDigest } from "@kars/mesh/dist/mission-admission.js";
import { missionContract, parseMissionMessage, type MissionReply } from "@kars/mesh/dist/mission-protocol.js";

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

const reviewedPhase = (overrides: Record<string, unknown> = {}) => ({
  name: "write-briefing", objective: "Write the reviewed internal briefing document.",
  capabilities: ["filesystem-write"], minToolCalls: 1, maxToolCalls: 2,
  requiredToolCalls: [], freshContext: true, ...overrides,
});
const batch = (...responses: ReturnType<typeof tool>[]) => ({
  usage,
  choices: [{ finish_reason: "tool_calls", message: { role: "assistant", tool_calls: responses.map((response, index) => ({
    ...response.choices[0].message.tool_calls[0], id: `call-${index}`,
  })) } }],
});

describe("reviewed filesystem phase execution", () => {
  it.each([
    null, [], {},
    reviewedPhase({ unexpected: true }),
    reviewedPhase({ name: "Bad Name" }),
    reviewedPhase({ objective: "too short" }),
    reviewedPhase({ objective: "é".repeat(601) }),
    reviewedPhase({ capabilities: null }),
    reviewedPhase({ capabilities: ["filesystem-write", "filesystem-write"] }),
    ...["shell", "network", "web-search", "memory", "mcp"].map(capability => reviewedPhase({ capabilities: [capability] })),
    reviewedPhase({ requiredToolCalls: [{ name: "github_actions_job_logs", arguments: {} }] }),
    reviewedPhase({ requiredToolCalls: null }),
    reviewedPhase({ minToolCalls: null }),
    reviewedPhase({ minToolCalls: -1 }),
    reviewedPhase({ minToolCalls: 3, maxToolCalls: 2 }),
    reviewedPhase({ maxToolCalls: 33 }),
    reviewedPhase({ maxToolCalls: 1.5 }),
    reviewedPhase({ maxToolCalls: "2" }),
    reviewedPhase({ freshContext: null }),
    reviewedPhase({ capabilities: [] }),
  ])("rejects invalid or unsupported phase before inference (%#)", async contract => {
    await router([final()]);
    await expect(executeTaskWithEvidence("task", deps, log, false, contract)).rejects.toMatchObject({ evidence: { rounds: 0, usage: null } });
    expect(requests).toEqual([]);
    expect(policyRequests).toEqual([]);
  });

  it("enforces a tool-free phase", async () => {
    await router([final()]);
    const result = await executeTaskWithEvidence("task", deps, log, false, reviewedPhase({ capabilities: [], minToolCalls: 0, maxToolCalls: 0 }));
    expect(result.phase).toMatchObject({ attemptedToolCalls: 0, successfulToolCalls: 0 });
    expect(requests[0].tools).toEqual([]);
    expect(result.usage).toEqual({ promptTokens: 7, completionTokens: 4, totalTokens: 11 });
  });

  it("counts real authorized writes, retains exact artifacts, and copies the reviewed contract", async () => {
    const path = join(await workspace(), "briefing.md");
    const contract = reviewedPhase({ maxToolCalls: 1 });
    await router([tool("file_write", { path, content: "Useful briefing\r\n✓", artifact_name: "briefing.md" }), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("task", { ...deps, onEvidence: async () => {
      contract.capabilities.length = 0;
      contract.maxToolCalls = 32;
      contract.minToolCalls = 0;
    } }, log, true, contract);
    expect(result.phase).toEqual({ name: "write-briefing", attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 1 });
    expect(result.artifacts).toEqual({ "briefing.md": "Useful briefing\r\n✓" });
    expect(await readFile(path, "utf8")).toBe("Useful briefing\r\n✓");
    expect(result.usage?.totalTokens).toBe(22);
    expect(requests[0].tools.map((t: any) => t.function.name)).toEqual(["file_write"]);
    expect(policyRequests).toHaveLength(1);
  });

  it.each([
    ["file_read", "parent"], ["file_read", "final"],
    ["file_write", "parent"], ["file_write", "final"],
  ])("refuses %s through a %s symlink without successful evidence or artifacts", async (name, kind) => {
    const directory = await workspace();
    const outside = await mkdtemp("/var/tmp/kars-mission-outside-");
    tempDirectories.push(outside);
    const externalPath = join(outside, "document.md");
    const sensitive = "External bytes must neither leak nor change";
    await writeFile(externalPath, sensitive);
    const alias = join(directory, "alias");
    await symlink(kind === "parent" ? outside : externalPath, alias);
    const path = kind === "parent" ? join(alias, "document.md") : alias;
    const args = name === "file_read" ? { path } : { path, content: "Replacement", artifact_name: "document.md" };
    await router([tool(name, args), final()], 200, { allowed: true });
    let failure: unknown;
    try {
      await executeTaskWithEvidence("task", deps, log, true, reviewedPhase({
        capabilities: [name === "file_read" ? "filesystem-read" : "filesystem-write"],
      }));
    } catch (error) { failure = error; }
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure).toMatchObject({ phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
      evidence: { rounds: 2, usage: { totalTokens: 22 } } });
    expect(failure).not.toHaveProperty("artifacts");
    expect(policyRequests).toHaveLength(1);
    expect(requests).toHaveLength(2);
    expect(requests[1].messages.at(-1).content).toContain(`${name} error:`);
    expect(JSON.stringify(requests)).not.toContain(sensitive);
    expect(await readFile(externalPath, "utf8")).toBe(sensitive);
  });

  it("counts an actual read without requiring write permission", async () => {
    const path = join(await workspace(), "source.txt");
    await writeFile(path, "Evidence actually read");
    await router([tool("file_read", { path, max_bytes: 8 }), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("task", deps, log, false, reviewedPhase({ capabilities: ["filesystem-read"] }));
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
    expect(requests[0].tools.map((t: any) => t.function.name)).toEqual(["file_read"]);
    expect(JSON.parse(requests[1].messages.at(-1).content)).toMatchObject({ content: "Evidence", returned_bytes: 8, truncated: true });
  });

  it.each(["file_write", "http_fetch", "exec_command", "memory", "mesh_send", "foundry_code_execute"])("denies ungranted %s before policy or effects", async name => {
    const path = join(await workspace(), "forbidden.txt");
    await router([tool(name, { path, content: "Must not write", cmd: `touch ${path}`, url: "https://example.com" }), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("task", deps, log, false, reviewedPhase({ capabilities: ["filesystem-read"] }))).rejects.toMatchObject({ phase: { attemptedToolCalls: 1, successfulToolCalls: 0 } });
    expect(policyRequests).toEqual([]);
    expect(requests).toHaveLength(2);
    expect(requests[1].messages.at(-1).content).toContain("outside the reviewed phase");
    await expect(access(path)).rejects.toThrow();
  });

  it("rejects a whole overflowing batch before any policy or filesystem effect", async () => {
    const root = await workspace();
    const paths = [join(root, "one"), join(root, "two")];
    await router([batch(...paths.map(path => tool("file_write", { path, content: "Not written" })))], 200, { allowed: true });
    await expect(executeTaskWithEvidence("task", deps, log, false, reviewedPhase({ maxToolCalls: 1 }))).rejects.toMatchObject({
      message: expect.stringContaining("exceeds maxToolCalls"), evidence: { rounds: 1, usage: { totalTokens: 11 } },
      phase: { attemptedToolCalls: 0, successfulToolCalls: 0 },
    });
    expect(policyRequests).toEqual([]);
    for (const path of paths) await expect(access(path)).rejects.toThrow();
  });

  it("caps later batches after success and exports no artifacts on failure", async () => {
    const root = await workspace();
    const first = join(root, "first.txt"), second = join(root, "second.txt");
    await router([
      tool("file_write", { path: first, content: "First evidence", artifact_name: "first.txt" }),
      tool("file_write", { path: second, content: "Must not write" }),
    ], 200, { allowed: true });
    const error = await executeTaskWithEvidence("task", deps, log, true, reviewedPhase({ maxToolCalls: 1 })).catch(error => error);
    expect(error).toBeInstanceOf(TaskExecutionError);
    expect(error.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
    expect(error.evidence.usage.totalTokens).toBe(22);
    expect(error.artifacts).toBeUndefined();
    expect(policyRequests).toHaveLength(1);
    await expect(access(second)).rejects.toThrow();
  });

  it("keeps router denial authoritative and counts denied retries against the ceiling", async () => {
    const path = join(await workspace(), "denied.txt");
    await router([tool("file_write", { path, content: "Denied" })]);
    const error = await executeTaskWithEvidence("task", deps, log, false, reviewedPhase()).catch(error => error);
    expect(error.message).toContain("exceeds maxToolCalls");
    expect(error.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 0 });
    expect(error.evidence.usage.totalTokens).toBe(33);
    expect(policyRequests).toHaveLength(2);
    await expect(access(path)).rejects.toThrow();
  });

  it.each(["null", "[]", "{", '{"path":1,"content":"no"}', '{"path":"/tmp/../../etc/forbidden","content":"no"}', '{"path":"/tmp/a","content":1}', '{"path":"/tmp/a","content":"no","extra":true}'])("does not count invalid arguments as successful (%#)", async args => {
    const response = tool("file_write");
    response.choices[0].message.tool_calls[0].function.arguments = args;
    await router([response, final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("task", deps, log, false, reviewedPhase())).rejects.toMatchObject({
      message: expect.stringContaining("requires 1 successful tool calls"), phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
    });
    expect(policyRequests).toEqual([]);
  });

  it("permits correction within the remaining attempt budget without counting the failed call", async () => {
    const root = await workspace();
    await router([
      tool("file_read", { path: join(root, "missing") }),
      tool("file_write", { path: join(root, "briefing"), content: "New useful evidence" }), final(),
    ], 200, { allowed: true });
    const result = await executeTaskWithEvidence("task", deps, log, false, reviewedPhase({ capabilities: ["filesystem-read", "filesystem-write"] }));
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 1 });
    expect(policyRequests).toHaveLength(2);
    expect(requests[1].messages.at(-1).content).toContain("file_read error");
  });

  it("does not count failed writes or nonregular-file reads as successes", async () => {
    const root = await workspace();
    await router([batch(tool("file_write", { path: root, content: "Fails on directory" }), tool("file_read", { path: root })), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("task", deps, log, false, reviewedPhase({ capabilities: ["filesystem-read", "filesystem-write"] }))).rejects.toMatchObject({ phase: { attemptedToolCalls: 2, successfulToolCalls: 0 } });
    expect(policyRequests).toHaveLength(2);
  });

  it("rejects a final response below the successful-call minimum", async () => {
    await router([final("I completed everything")]);
    await expect(executeTaskWithEvidence("task", deps, log, false, reviewedPhase())).rejects.toMatchObject({
      phase: { attemptedToolCalls: 0, successfulToolCalls: 0 }, evidence: { usage: { totalTokens: 11 } },
    });
  });

  it("preserves successful-call counts and ambiguous spend on later transport failure", async () => {
    const path = join(await workspace(), "briefing");
    await router([tool("file_write", { path, content: "Useful evidence" }), (res: ServerResponse) => res.destroy()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("task", deps, log, false, reviewedPhase())).rejects.toMatchObject({
      phase: { attemptedToolCalls: 1, successfulToolCalls: 1 }, evidence: { rounds: 1, usage: null },
    });
  });

  it.each(["interrupt", "evidence callback"])("keeps phase accounting but exports no artifacts after %s failure", async failure => {
    const path = join(await workspace(), "briefing");
    let observedRounds = 0;
    await router([tool("file_write", { path, content: "Useful evidence", artifact_name: "briefing.md" }), final()], 200, { allowed: true });
    const error = await executeTaskWithEvidence("task", {
      ...deps,
      isInterruptRequested: () => failure === "interrupt" && observedRounds === 1,
      onEvidence: evidence => {
        observedRounds = evidence.rounds;
        if (failure === "evidence callback" && observedRounds === 2) throw new Error("Evidence persistence failed");
      },
    }, log, true, reviewedPhase()).catch(error => error);
    expect(error).toBeInstanceOf(TaskExecutionError);
    expect(error.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
    expect(error.evidence).toMatchObject({ rounds: failure === "interrupt" ? 1 : 2, usage: { totalTokens: failure === "interrupt" ? 11 : 22 } });
    expect(error.artifacts).toBeUndefined();
    expect(policyRequests).toHaveLength(1);
    expect(await readFile(path, "utf8")).toBe("Useful evidence");
  });

  it.each([true, false])("reports response, reservation and every attempted call with policy allowed=%s", async allowed => {
    const path = `${await workspace()}/briefing.md`;
    const malformed = tool("file_write");
    malformed.choices[0].message.tool_calls[0].function.arguments = "{";
    await router([batch(malformed, tool("mesh_inbox"), tool("file_write", {
      path, content: "Verified briefing bytes", artifact_name: "briefing.md",
    })), final()], 200, { allowed });
    const progress: TaskExecutionEvidence[] = [];
    const execution = executeTaskWithEvidence("task", { ...deps, onEvidence: event => { progress.push(event); } }, log, true, reviewedPhase({ maxToolCalls: 3 }));
    if (allowed) expect((await execution).artifacts).toEqual({ "briefing.md": "Verified briefing bytes" });
    else await expect(execution).rejects.toMatchObject({ evidence: { rounds: 2, phase: { attemptedToolCalls: 3, successfulToolCalls: 0 } } });
    const successful = allowed ? 1 : 0;
    expect(progress.map(event => [event.rounds, event.phase?.attemptedToolCalls, event.phase?.successfulToolCalls, event.usage?.totalTokens])).toEqual([
      [1, 0, 0, 11], [1, 3, 0, 11], [1, 3, 0, 11], [1, 3, 0, 11], [1, 3, successful, 11], [2, 3, successful, 22],
    ]);
    expect(policyRequests).toHaveLength(1);
    expect(requests).toHaveLength(2);
    if (allowed) expect(await readFile(path, "utf8")).toBe("Verified briefing bytes");
    else await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    progress.at(-1)!.phase!.successfulToolCalls = 0;
    expect(progress[4].phase?.successfulToolCalls).toBe(successful);
    expect(progress[0].phase?.attemptedToolCalls).toBe(0);
  });

  it.each([0, 1])("retains measured counters if progress persistence fails after reservation with %i successful calls", async successfulToolCalls => {
    const path = `${await workspace()}/briefing.md`;
    await router([tool("file_write", { path, content: "Written before handback", artifact_name: "briefing.md" }), final()], 200, { allowed: true });
    const execution = executeTaskWithEvidence("task", { ...deps, onEvidence: event => {
      if (event.phase?.attemptedToolCalls === 1 && event.phase.successfulToolCalls === successfulToolCalls) throw new Error("Progress persistence failed");
    } }, log, true, reviewedPhase());
    const error = await execution.catch(value => value);
    expect(error).toBeInstanceOf(TaskExecutionError);
    expect(error).toMatchObject({ message: "Progress persistence failed", evidence: {
      rounds: 1, usage: { promptTokens: 7, completionTokens: 4, totalTokens: 11 },
      phase: { attemptedToolCalls: 1, successfulToolCalls },
    } });
    expect(error).not.toHaveProperty("artifacts");
    expect(requests).toHaveLength(1);
    expect(policyRequests).toHaveLength(successfulToolCalls);
    if (successfulToolCalls) expect(await readFile(path, "utf8")).toBe("Written before handback");
    else await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("fits the real maximum-round, maximum-call execution into the receiver journal without replay", async () => {
    const path = `${await workspace()}/briefing.md`;
    const write = (index: number) => tool("file_write", { path, content: `Revision ${index}`, artifact_name: "briefing.md" });
    await router([
      batch(...Array.from({ length: 8 }, (_, index) => write(index))),
      ...Array.from({ length: 24 }, (_, index) => write(index + 8)),
    ], 200, { allowed: true });
    const target = { taskName: "mission", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}` };
    const assignment = { ...target, ...missionContract(reviewedPhase({ maxToolCalls: 32 })), type: "mission:assign", runNonce: "run-1", assignmentId: "assignment-1", bootId: "boot-1", content: "Write the bounded briefing", artifactFormat: "text-v1" };
    const replies: MissionReply[] = [];
    const execute = vi.fn<MissionReceiverOptions["execute"]>(async (content, onEvidence, artifactsEnabled, phase) => executeTaskWithEvidence(content, { ...deps, onEvidence }, log, artifactsEnabled, phase));
    const receiver = new MissionReceiver({ target, admission: { version: 1, state: "run", taskGeneration: 1,
      authorizationDigest: `sha256:${"a".repeat(64)}`, runNonce: assignment.runNonce, objectiveDigest: missionObjectiveDigest(assignment.content) }, expectedContract: missionContract(reviewedPhase({ maxToolCalls: 32 })), bootId: "boot-1", authorize: async () => true, execute,
      send: async (_to, reply) => { replies.push(reply); }, warn: log.warn });
    expect(parseMissionMessage(assignment)).not.toBeNull();
    await receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(replies.at(-1)?.status).toBe("failed"), { timeout: 10_000 });
    expect(requests).toHaveLength(25);
    expect(policyRequests).toHaveLength(32);
    expect(replies).toHaveLength(84);
    expect(replies.filter(reply => reply.status === "running")).toHaveLength(82);
    expect(replies.every(reply => parseMissionMessage(reply) !== null)).toBe(true);
    const terminal = replies.at(-1)!;
    expect(terminal).toMatchObject({ status: "failed", error: expect.stringContaining("maximum tool-calling rounds"), evidence: {
      rounds: 25, usage: { promptTokens: 175, completionTokens: 100, totalTokens: 275 },
      phase: { attemptedToolCalls: 32, successfulToolCalls: 32, maxToolCalls: 32 },
    } });
    expect(terminal).not.toHaveProperty("artifacts");
    expect(terminal).not.toHaveProperty("output");
    expect(await readFile(path, "utf8")).toBe("Revision 31");
    await receiver.handle(target.dispatcherDid, assignment, "encrypted");
    expect(replies.at(-1)).toEqual(terminal);
    expect(execute).toHaveBeenCalledTimes(1);
    expect(requests).toHaveLength(25);
    expect(policyRequests).toHaveLength(32);
  });

  it("does not reuse successful calls across executions", async () => {
    const path = join(await workspace(), "briefing");
    await router([tool("file_write", { path, content: "Useful evidence" }), final(), final()], 200, { allowed: true });
    const contract = reviewedPhase();
    await executeTaskWithEvidence("first task", deps, log, false, contract);
    await expect(executeTaskWithEvidence("second task", deps, log, false, contract)).rejects.toMatchObject({ phase: { attemptedToolCalls: 0, successfulToolCalls: 0 } });
  });
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
