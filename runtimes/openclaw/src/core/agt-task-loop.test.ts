// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createHash } from "node:crypto";
import { createServer, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { mkdtemp, readFile, rm, access, writeFile, symlink } from "node:fs/promises";
import { executeTaskWithEvidence, processTaskWithTools, type TaskLoopDeps } from "./agt-task-loop.js";
import { TaskCompletionLedger, TaskExecutionError, type TaskExecutionEvidence } from "./task-completion.js";
import { MissionReceiver, type MissionReceiverOptions } from "./mission-receiver.js";
import { missionObjectiveDigest } from "@kars/mesh/dist/mission-admission.js";
import { missionContract, parseMissionMessage, type MissionReply } from "@kars/mesh/dist/mission-protocol.js";
import { missionInputContentDigest } from "@kars/mesh/dist/mission-inputs.js";
import { TASK_INPUT_ROOT, TASK_SKILL_ROOT, TaskInputFiles, TaskSkillFiles } from "./task-filesystem.js";
import * as taskFilesystem from "./task-filesystem.js";

const log = { info: vi.fn(), warn: vi.fn() };
const deps: TaskLoopDeps = { meshClient: () => null, isInterruptRequested: () => false, interruptReason: () => "", setInterrupt: () => {} };
const usage = { prompt_tokens: 7, completion_tokens: 4, total_tokens: 11 };
const final = (content: unknown = "A useful briefing", finish_reason = "stop", measured: unknown = usage) => ({
  choices: [{ finish_reason, message: { role: "assistant", content } }], usage: measured,
});
let server: Server | undefined;
let requests: Record<string, any>[];
let policyRequests: Record<string, any>[];
let toolReports: Record<string, any>[];
const tempDirectories: string[] = [];
async function workspace() {
  const directory = await mkdtemp("/tmp/kars-mission-artifacts-");
  tempDirectories.push(directory);
  return directory;
}
let observationRequests: { url: string; scope: string | string[] | undefined }[];
async function router(responses: unknown[], policyStatus = 200, policyBody: unknown = { allowed: false }, observations?: unknown) {
  requests = []; policyRequests = []; observationRequests = []; toolReports = [];
  let index = 0;
  server = createServer((req, res) => {
    if (req.method === "GET" && req.url?.startsWith("/telemetry/trace?")) {
      observationRequests.push({ url: req.url, scope: req.headers["x-kars-service-scope"] });
      const selected = typeof observations === "function" ? observations(req.url) : observations;
      res.writeHead(selected ? 200 : 404, { "content-type": "application/json" });
      res.end(JSON.stringify(selected ?? {}));
      return;
    }
    let body = "";
    req.on("data", (data) => { body += data; });
    req.on("end", () => {
      if (req.url === "/agt/evaluate") {
        policyRequests.push(JSON.parse(body));
        res.writeHead(policyStatus, { "content-type": "application/json" });
        res.end(typeof policyBody === "string" ? policyBody : JSON.stringify(policyBody));
      } else if (req.url === "/telemetry/tool") {
        toolReports.push(JSON.parse(body));
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ recorded: true, source: "harness-reported", enforcement_changed: false }));
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

describe("router observations in completed task handoffs", () => {
  const correlated = (round: number, response: unknown) => (res: ServerResponse) => {
    res.writeHead(200, { "content-type": "application/json", "x-kars-telemetry-scope": "scope-a", "x-kars-telemetry-round": String(round) });
    res.end(JSON.stringify(response));
  };
  const selection = {
    scope_id: "scope-a", rounds: [1, 2], coverage: "returned-responses-only", durable: false,
    missing_rounds: [], dropped_events: 0, truncated: false,
    events: [1, 2].map(round => ({ kind: "round", scope_id: "scope-a", round, seq: round,
      source: "router-upstream", provider: "foundry", model: "gpt-5.4-mini", http_status: 200,
      accepted: true, outcome: "complete", usage, usage_state: "present", finish_reason: "stop",
      partial_observation: false, ms: 10, tool_calls_observed: 0 })),
  };
  const filename = "kars-router-observations.json";

  it("emits a selected snapshot while the next response is still pending, without artifact negotiation", async () => {
    let finishResponse: ServerResponse | undefined;
    const snapshots: string[] = [];
    const proposed = { kind: "tool_proposed", scope_id: "scope-a", round: 1, seq: 2,
      source: "model-proposed", call_id: "call-1", name: "mesh_inbox", ok: null };
    const firstSelection = { ...selection, rounds: [1], events: [
      { ...selection.events[0], finish_reason: "tool_calls", tool_calls_observed: 1 }, proposed,
    ] };
    const cumulativeSelection = { ...selection, events: [...firstSelection.events,
      { ...proposed, kind: "tool_result", seq: 3, source: "harness-reported", reported_in_round: 2 },
      { ...selection.events[1], seq: 4 },
    ] };
    await router([
      correlated(1, tool()),
      (res: ServerResponse) => { finishResponse = res; },
    ], 200, { allowed: true }, (url: string) => url === "/telemetry/trace?rounds=1" ? firstSelection : cumulativeSelection);
    let completed = false;
    const execution = executeTaskWithEvidence("task", { ...deps, liveActivityEnabled: true,
      onEvidence: (_evidence, snapshot) => { if (snapshot) snapshots.push(snapshot); },
    }, log, false).then(result => { completed = true; return result; });
    try {
      await vi.waitFor(() => expect(finishResponse).toBeDefined());
      expect(completed).toBe(false);
      expect(snapshots).toHaveLength(1);
      expect(JSON.parse(snapshots[0])).toMatchObject({ state: "observed", rounds: [1], trace: firstSelection });
      expect(observationRequests).toEqual([{ url: "/telemetry/trace?rounds=1", scope: "scope-a" }]);
    } finally {
      if (finishResponse) correlated(2, final())(finishResponse);
    }
    const result = await execution;
    expect(result.artifacts).toBeUndefined();
    expect(result.rounds).toBe(2);
    expect(snapshots).toHaveLength(2);
    expect(JSON.parse(snapshots[1])).toMatchObject({ state: "observed", responses: 2, rounds: [1, 2], trace: cumulativeSelection });
    expect(JSON.parse(snapshots[1]).trace.events.slice(0, 2)).toEqual(JSON.parse(snapshots[0]).trace.events);
    expect(observationRequests).toEqual([
      { url: "/telemetry/trace?rounds=1", scope: "scope-a" },
      { url: "/telemetry/trace?rounds=1,2", scope: "scope-a" },
    ]);
  });

  it("attaches selected existing router records alongside the actual useful file", async () => {
    const path = join(await workspace(), "briefing.md");
    await router([
      correlated(1, tool("file_write", { path, content: "Useful briefing\r\n✓", artifact_name: "briefing.md" })),
      correlated(2, final()),
    ], 200, { allowed: true }, selection);
    const result = await executeTaskWithEvidence("task", deps, log, true, reviewedPhase());
    expect(result.artifacts?.["briefing.md"]).toBe(await readFile(path, "utf8"));
    expect(JSON.parse(result.artifacts![filename])).toEqual({
      version: 1, source: "runtime-forwarded-router-observations", coverage: "returned-responses-only",
      responses: 2, scope_id: "scope-a", rounds: [1, 2], state: "observed", trace: selection,
    });
    expect(observationRequests).toEqual([{ url: "/telemetry/trace?rounds=1,2", scope: "scope-a" }]);
    expect(result.usage?.totalTokens).toBe(22);
    expect(result.rounds).toBe(2);
    expect(toolReports).toHaveLength(1);
    expect(toolReports[0]).toMatchObject({
      scope_id: "scope-a", call_id: "call-1", ok: true,
      result_summary: { kind: "file_write", bytes: Buffer.byteLength("Useful briefing\r\n✓"), artifact_name: "briefing.md" },
    });
    expect(Number.isSafeInteger(toolReports[0].latency_ms)).toBe(true);
    expect(JSON.stringify(toolReports)).not.toContain("Useful briefing");
    expect(policyRequests[0].context.scope_id).toBe("scope-a");
  });

  it("reports a real refused filesystem write without inventing success or copying error contents", async () => {
    const directory = await workspace();
    await router([
      correlated(1, tool("file_write", { path: directory, content: "private-content" })),
      correlated(2, final()),
    ], 200, { allowed: true }, selection);
    await executeTaskWithEvidence("task", deps, log, true, reviewedPhase({ minToolCalls: 0 }));
    expect(toolReports).toHaveLength(1);
    expect(toolReports[0]).toMatchObject({ ok: false,
      result_summary: { kind: "failure", reason: "filesystem_operation_failed" } });
    expect(JSON.stringify(toolReports)).not.toContain(directory);
    expect(JSON.stringify(toolReports)).not.toContain("private-content");
  });

  it("does not fetch or export observations when execution fails after a real write", async () => {
    const path = join(await workspace(), "briefing.md");
    await router([
      correlated(1, tool("file_write", { path, content: "Actual file", artifact_name: "briefing.md" })),
      correlated(2, final("", "length")),
    ], 200, { allowed: true }, selection);
    const failure = await executeTaskWithEvidence("task", deps, log, true, reviewedPhase()).catch(error => error);
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure.artifacts).toBeUndefined();
    expect(await readFile(path, "utf8")).toBe("Actual file");
    expect(observationRequests).toEqual([]);
  });

  it("refuses model-written system evidence before the filesystem write", async () => {
    const path = join(await workspace(), "forged.json");
    await router([tool("file_write", { path, content: "Forged observations", artifact_name: filename }), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("task", deps, log, true, reviewedPhase())).rejects.toBeInstanceOf(TaskExecutionError);
    await expect(access(path)).rejects.toThrow();
    expect(observationRequests).toEqual([]);
  });

  it("preserves all useful files when observation attachment has no remaining capacity", async () => {
    const directory = await workspace();
    const files = Object.fromEntries(Array.from({ length: 16 }, (_, i) => [`part-${i}.md`, `Useful part ${i}`]));
    await router([
      correlated(1, batch(...Object.entries(files).map(([name, content]) => tool("file_write", {
        path: join(directory, name), content, artifact_name: name,
      })))),
      correlated(2, final()),
    ], 200, { allowed: true }, selection);
    const result = await executeTaskWithEvidence("task", deps, log, true, reviewedPhase({ minToolCalls: 16, maxToolCalls: 16 }));
    expect(result.artifacts).toEqual(files);
    expect(result.usage?.totalTokens).toBe(22);
    expect(observationRequests).toHaveLength(1);
    expect(log.warn).toHaveBeenCalledWith("Router observations omitted: negotiated artifact capacity was used by deliverables");
    for (const [name, content] of Object.entries(files)) expect(await readFile(join(directory, name), "utf8")).toBe(content);
  });

  it("does not collect observations without negotiated artifact transport", async () => {
    await router([correlated(1, final())], 200, { allowed: true }, selection);
    const result = await executeTaskWithEvidence("task", deps, log, false);
    expect(result.artifacts).toBeUndefined();
    expect(observationRequests).toEqual([]);
  });
});

describe("selected Skills in measured reviewed execution", () => {
  const path = `${TASK_SKILL_ROOT}/checklist/references/check.md`;
  let restore: () => void;
  beforeEach(async () => {
    const files = JSON.parse(await readFile(new URL("../../../../tests/fixtures/skill-package.json", import.meta.url), "utf8"));
    const raw = Buffer.from(JSON.stringify({ schema: 1, sandboxUid: "sandbox-uid", skills: [{
      name: "checklist", uid: "skill-uid", generation: 2, versionDigest: `sha256:${"a".repeat(32)}`,
      summary: "Review local evidence", recipe: null,
      packageDigest: "sha256:ff733becaecde141e87b4b8223ca99091bb181295398a1ddace419976dd16615", files,
    }] }));
    const skills = new TaskSkillFiles(raw, `sha256:${createHash("sha256").update(raw).digest("hex")}`);
    const spy = vi.spyOn(TaskSkillFiles, "load").mockReturnValue(skills);
    restore = () => spy.mockRestore();
  });
  afterEach(() => restore());

  it.each([undefined, reviewedPhase(), reviewedPhase({ capabilities: ["filesystem-read"], minToolCalls: 0, maxToolCalls: 0 })])(
    "requires an actual reviewed read allowance before model or policy (%#)", async phase => {
      await router([final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("Review", deps, log, true, phase)).rejects.toMatchObject({
        message: expect.stringContaining("Selected skill packages require"), evidence: { rounds: 0 },
      });
      expect(requests).toEqual([]);
      expect(policyRequests).toEqual([]);
    });

  it("reads exact package bytes under policy and exports only the explicit reviewed output", async () => {
    const output = join(await workspace(), "quality-review.md");
    await router([tool("file_read", { path }), tool("file_write", {
      path: output, content: "PASS: Consent evidence inspected.", artifact_name: "quality-review.md",
    }), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Review the selected checklist", deps, log, true,
      reviewedPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 2 }));
    expect(requests[0].messages[0].content).toContain(`${TASK_SKILL_ROOT}/checklist/SKILL.md`);
    expect(requests[0].messages[0].content).not.toContain("Consent: café");
    expect(JSON.parse(requests[1].messages.at(-1).content)).toMatchObject({ content: "Consent: café ✓\n" });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 2 });
    expect(result.artifacts).toEqual({ "quality-review.md": "PASS: Consent evidence inspected." });
    expect(await readFile(output, "utf8")).toBe("PASS: Consent evidence inspected.");
    expect(policyRequests).toHaveLength(2);
  });

  it.each(["file_write", "file_read"])("does not grant writes or unknown files through %s", async name => {
    const args = name === "file_read" ? { path: `${path}.missing` } : { path, content: "Must not mutate" };
    await router([tool(name, args), final()],
      200, { allowed: true });
    await expect(executeTaskWithEvidence("Review", deps, log, true,
      reviewedPhase({ capabilities: ["filesystem-read", "filesystem-write"] }))).rejects.toMatchObject({
      phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
    });
    expect(policyRequests).toHaveLength(1);
  });

  it("reports invalid controller-bound content as measured failure before inference", async () => {
    vi.spyOn(TaskSkillFiles, "load").mockImplementation(() => { throw new Error("Invalid controller-bound skill catalog"); });
    await router([final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Review", deps, log, true,
      reviewedPhase({ capabilities: ["filesystem-read"] }))).rejects.toMatchObject({
      message: "Invalid controller-bound skill catalog", evidence: { rounds: 0 },
    });
    expect(requests).toEqual([]);
  });
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
    expect(observationRequests).toEqual([]);
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
    const rounds = 2;
    expect(failure).toMatchObject({ phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
      evidence: { rounds, usage: { totalTokens: rounds * 11 } } });
    expect(failure).not.toHaveProperty("artifacts");
    expect(policyRequests).toHaveLength(1);
    expect(requests).toHaveLength(rounds);
    if (name === "file_write") expect((failure as TaskExecutionError).message).toContain("Explicit file_write export failed: file_write error:");
    else expect(requests[1].messages.at(-1).content).toContain("file_read error:");
    expect(JSON.stringify(requests)).not.toContain(sensitive);
    expect(await readFile(externalPath, "utf8")).toBe(sensitive);
  });

  describe("immutable input execution", () => {
    const path = `${TASK_INPUT_ROOT}/draft.md`;
    const content = "Untrusted upstream draft\r\n✓ 日本語 🌍";
    const input = () => ({ name: "draft.md", content, sha256: missionInputContentDigest(content), source: {
      namespace: "kars-system", taskName: "writer", taskUid: "upstream-task", sandboxUid: "upstream-sandbox",
      podUid: "upstream-pod", runNonce: "upstream-run", assignmentId: "upstream-assignment", agentDid: `did:mesh:${"a".repeat(32)}`, artifactName: "draft.md",
    } });
    const readPhase = (overrides = {}) => reviewedPhase({ capabilities: ["filesystem-read"], ...overrides });

    it("rejects the installed two-input/two-export starvation before any model call", async () => {
      await router([final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("Synthesize both handoffs", deps, log, true,
        readPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 0 }),
        [input(), { ...input(), name: "second.md" }], ["brief.md", "checklist.md"])).rejects.toMatchObject({
        message: expect.stringContaining("insufficient remaining reviewed tool calls"),
        evidence: { rounds: 0, usage: null }, phase: { attemptedToolCalls: 0, maxToolCalls: 2 },
      });
      expect(requests).toEqual([]);
      expect(policyRequests).toEqual([]);
    });

    it("requires both complete handoffs before exporting within the exact four-call ceiling", async () => {
      const directory = await workspace();
      await router([tool("file_read", { path, max_bytes: 65536 }),
        tool("file_read", { path: `${TASK_INPUT_ROOT}/second.md`, max_bytes: 65536 }),
        tool("file_write", { path: join(directory, "brief.md"), content: "Combined findings", artifact_name: "brief.md" }),
        tool("file_write", { path: join(directory, "checklist.md"), content: "Combined actions", artifact_name: "checklist.md" }),
        final()], 200, { allowed: true });
      const result = await executeTaskWithEvidence("Synthesize both handoffs", deps, log, true,
        readPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 0, maxToolCalls: 4 }),
        [input(), { ...input(), name: "second.md" }], ["brief.md", "checklist.md"]);
      expect(result.phase).toMatchObject({ attemptedToolCalls: 4, successfulToolCalls: 4, maxToolCalls: 4 });
      expect(requests.map(request => request.tool_choice)).toEqual([
        { type: "function", function: { name: "file_read" } },
        { type: "function", function: { name: "file_read" } },
        { type: "function", function: { name: "file_write" } },
        { type: "function", function: { name: "file_write" } }, "none",
      ]);
      expect(requests[1].tools[0].function.parameters.properties.path.enum).toEqual([`${TASK_INPUT_ROOT}/second.md`]);
      expect(requests[0].messages.at(-1).content).toContain(`Next, call file_read with path "${TASK_INPUT_ROOT}" and max_bytes 65536`);
      expect(requests[1].messages.at(-1).content).toContain(`Unread immutable inputs remaining: ["${TASK_INPUT_ROOT}/second.md"]`);
      expect(requests[1].messages.at(-1).content).not.toContain(path);
      expect(requests[1].messages.at(-1).content).toContain("One complete read and 2 required exports must fit within the 3 remaining reviewed tool calls");
      expect(requests[2].messages.at(-1).content).not.toContain("Unread immutable inputs remaining:");
      expect(result.artifacts).toEqual({ "brief.md": "Combined findings", "checklist.md": "Combined actions" });
    });

    it.each([4, 10])("reads eight complete inputs and exports three outputs within %s calls", async maxToolCalls => {
      const directory = await workspace();
      const inputs = Array.from({ length: 8 }, (_, i) => ({ ...input(), name: `input-${i}.md` }));
      const outputs = ["brief.md", "checklist.md", "decisions.md"];
      await router([tool("file_read", { path: TASK_INPUT_ROOT, max_bytes: 65536 }),
        ...outputs.map(name => tool("file_write", {
          path: join(directory, name), content: `Substantive synthesis for ${name}`, artifact_name: name,
        })), final()], 200, { allowed: true });
      const result = await executeTaskWithEvidence("Synthesize complete historical memory and both handoffs", deps, log, true,
        readPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 2, maxToolCalls }),
        inputs, outputs);
      expect(result.phase).toMatchObject({ attemptedToolCalls: 4, successfulToolCalls: 4, maxToolCalls });
      expect(requests).toHaveLength(5);
      expect(policyRequests).toHaveLength(4);
      expect(requests[0].tools[0].function.parameters.properties.path.enum).toEqual([TASK_INPUT_ROOT]);
      const readMessage = requests[1].messages.find((message: { role: string }) => message.role === "tool");
      expect(JSON.parse(JSON.parse(readMessage.content).content)).toEqual(inputs);
      expect(Object.keys(result.artifacts!).sort()).toEqual(outputs.sort());
    });

    it("does not count a policy-denied bundle as consumed evidence", async () => {
      await router([tool("file_read", { path: TASK_INPUT_ROOT, max_bytes: 65536 }), final()], 200, { allowed: false });
      await expect(executeTaskWithEvidence("Read the supplied evidence", deps, log, true,
        readPhase(), [input(), { ...input(), name: "second.md" }])).rejects.toMatchObject({
        message: expect.stringContaining("Unread immutable inputs"),
        phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
      });
      expect(policyRequests).toHaveLength(1);
    });

    it("does not count a partial input read as completed collaboration", async () => {
      await router([tool("file_read", { path, max_bytes: 4 }), final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("Review the entire input", deps, log, true,
        readPhase(), [input()])).rejects.toMatchObject({
        message: expect.stringContaining("Unread immutable inputs"),
        phase: { attemptedToolCalls: 1, successfulToolCalls: 1 },
      });
    });

    it("blocks output before input even when the provider ignores the forced read tool", async () => {
      const output = join(await workspace(), "review.md");
      await router([tool("file_write", { path: output, content: "Unread synthesis", artifact_name: "review.md" }),
        final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("Review supplied evidence", deps, log, true,
        readPhase({ capabilities: ["filesystem-read", "filesystem-write"], maxToolCalls: 4 }),
        [input()], ["review.md"])).rejects.toBeInstanceOf(TaskExecutionError);
      await expect(access(output)).rejects.toThrow();
      expect(requests[1].messages.some((message: { content?: string }) =>
        message.content?.includes("Read all immutable inputs completely"))).toBe(true);
    });

    it("reads captured bytes through the authorized tool, not system instructions, without automatic export", async () => {
      const original = input(); const caller = [structuredClone(original)];
      await router([(res: ServerResponse) => {
        caller[0].content = "Changed while model was running"; caller[0].source.runNonce = "changed"; caller.length = 0;
        res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify(tool("file_read", { path })));
      }, final()], 200, { allowed: true });
      const result = await executeTaskWithEvidence("Review the supplied draft", deps, log, true, readPhase(), caller);
      expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
      expect(result.artifacts).toBeUndefined();
      expect(requests[0].messages[0].content).toContain(path);
      expect(requests[0].messages[0].content).toContain(original.sha256);
      expect(JSON.stringify(requests[0])).not.toContain("Untrusted upstream draft");
      expect(requests[0].tools.map((t: any) => t.function.name)).toEqual(["file_read"]);
      expect(JSON.parse(requests[1].messages.at(-1).content)).toMatchObject({ content, bytes: Buffer.byteLength(content), truncated: false,
        input: { source: original.source, sha256: original.sha256, readOnly: true } });
      expect(policyRequests).toHaveLength(1);
    });

    it.each([undefined, reviewedPhase(), readPhase({ minToolCalls: 0, maxToolCalls: 0 })])("rejects inputs without usable reviewed read permission before model or policy (%#)", async phase => {
      await router([final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("task", deps, log, true, phase, [input()])).rejects.toMatchObject({
        message: expect.stringContaining("Immutable inputs require"), evidence: { rounds: 0 },
      });
      expect(requests).toEqual([]); expect(policyRequests).toEqual([]);
    });

    it("rejects invalid input bytes before model execution and disallows an unmeasured input loop", async () => {
      await router([final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("task", deps, log, true, readPhase(), [{ ...input(), content: "digest mismatch" }])).rejects.toBeInstanceOf(TaskExecutionError);
      await expect(processTaskWithTools("task", deps, log, undefined, undefined, undefined, new TaskInputFiles([input()]))).rejects.toThrow("measured reviewed");
      expect(requests).toEqual([]); expect(policyRequests).toEqual([]);
    });

    it("keeps router denial authoritative and does not reveal contents or count a successful read", async () => {
      await router([tool("file_read", { path }), final()]);
      const failure = await executeTaskWithEvidence("task", deps, log, true, readPhase(), [input()]).catch(error => error);
      expect(failure).toBeInstanceOf(TaskExecutionError);
      expect(failure).toMatchObject({ phase: { attemptedToolCalls: 1, successfulToolCalls: 0 } });
      expect(failure).not.toHaveProperty("artifacts");
      expect(JSON.stringify(requests)).not.toContain("Untrusted upstream draft");
      expect(policyRequests).toHaveLength(1);
    });

    it.each([`${TASK_INPUT_ROOT}/unknown.md`, `${TASK_INPUT_ROOT}/../draft.md`, "/sandbox//.kars-inputs/draft.md"]) ("rejects missing or aliased input path %s without exposure", async alias => {
      await router([tool("file_read", { path: alias }), final()], 200, { allowed: true });
      await expect(executeTaskWithEvidence("task", deps, log, true, readPhase(), [input()])).rejects.toMatchObject({ phase: { attemptedToolCalls: 1, successfulToolCalls: 0 } });
      expect(JSON.stringify(requests)).not.toContain("Untrusted upstream draft");
      expect(requests[1].messages.findLast((message: { role: string }) => message.role === "tool").content).toContain("file_read error");
    });

    it("reads upstream data then attaches only explicitly written downstream output", async () => {
      const output = join(await workspace(), "review.md");
      await router([tool("file_read", { path }), tool("file_write", { path: output, content: "Reviewed draft: one correction", artifact_name: "review.md" }), final()], 200, { allowed: true });
      const result = await executeTaskWithEvidence("Review the supplied draft", deps, log, true,
        readPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 2 }), [input()]);
      expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 2 });
      expect(result.artifacts).toEqual({ "review.md": "Reviewed draft: one correction" });
      expect(await readFile(output, "utf8")).toBe("Reviewed draft: one correction");
      expect(policyRequests).toHaveLength(2);
    });

    it("exports nothing if execution fails after reading input and writing a result", async () => {
      const output = join(await workspace(), "review.md");
      await router([tool("file_read", { path }), tool("file_write", { path: output, content: "Incomplete review", artifact_name: "review.md" }),
        (res: ServerResponse) => res.destroy()], 200, { allowed: true });
      const failure = await executeTaskWithEvidence("task", deps, log, true,
        readPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 2 }), [input()]).catch(error => error);
      expect(failure).toBeInstanceOf(TaskExecutionError);
      expect(failure).toMatchObject({ phase: { attemptedToolCalls: 2, successfulToolCalls: 2 }, evidence: { usage: null } });
      expect(failure).not.toHaveProperty("artifacts");
    });
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

  it("requires useful tools until the measured minimum is met after a failed read and export", async () => {
    const directory = await workspace();
    const path = join(directory, "handoff.md");
    const content = "Verified clinic capacity findings";
    await router([
      tool("file_read", { path: join(directory, "missing.md") }),
      tool("file_write", { path, content, artifact_name: "handoff.md" }),
      tool("file_read", { path }),
      final("Exported and verified handoff.md"),
    ], 200, { allowed: true });
    const result = await executeTaskWithEvidence("The clinic facts are supplied inline.", deps, log, true,
      reviewedPhase({ capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 2, maxToolCalls: 8 }),
      undefined, ["handoff.md"]);
    expect(requests.map(request => request.tool_choice)).toEqual(["required", "required", "required", undefined]);
    expect(requests[2].messages[0].content).toContain("requires 1 successful authorized tool calls; 6 attempts remain");
    expect(requests[0].messages[0].content).toContain("No immutable input files were supplied");
    expect(result.phase).toMatchObject({ attemptedToolCalls: 3, successfulToolCalls: 2, maxToolCalls: 8 });
    expect(result.usage?.totalTokens).toBe(44);
    expect(result.artifacts).toEqual({ "handoff.md": content });
    expect(await readFile(path, "utf8")).toBe(content);
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
      [1, 0, 0, 11], [1, 3, 0, 11], [1, 3, 0, 11], [1, 3, 0, 11], [1, 3, successful, 11],
      [2, 3, successful, 22],
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

describe("required reviewed exports", () => {
  const args = { path: "/sandbox/handoff.md", content: "Measured phase findings — 日本語", artifact_name: "handoff.md" };
  const mockWrite = () => vi.spyOn(taskFilesystem, "writeTaskFile").mockImplementation(async (_path, content) => Buffer.byteLength(content));
  afterEach(() => vi.restoreAllMocks());

  it("passes the typed phase output contract into the system prompt and retains exact exports", async () => {
    mockWrite();
    await router([tool("file_write", args), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Overall task requests final-report.md", deps, log, true, reviewedPhase(), undefined, ["handoff.md"]);
    expect(result.artifacts).toEqual({ "handoff.md": args.content });
    expect(requests[0].messages[0].content).toContain('Required named UTF-8 outputs for this phase: ["handoff.md"]');
    expect(requests[0].messages[0].content).toContain("Execute only this reviewed phase");
    expect(requests[0].messages[0].content).not.toContain("Choose inline-only delivery");
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
  });

  it("permits an explicit export after a scratch-only final answer within unchanged limits", async () => {
    const write = mockWrite();
    await router([tool("file_write", { ...args, artifact_name: null }), final("The handoff is saved locally."),
      tool("file_write", args), final("Handoff attached.")], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Prepare phase findings", deps, log, true, reviewedPhase(), undefined, ["handoff.md"]);
    expect(result.artifacts).toEqual({ "handoff.md": args.content });
    expect(result).toMatchObject({ rounds: 4, usage: { totalTokens: 44 }, phase: { attemptedToolCalls: 2, successfulToolCalls: 2, maxToolCalls: 2 } });
    expect(requests[2].messages.at(-1).content).toContain("Only 1 reviewed tool calls remain");
    expect(write).toHaveBeenCalledTimes(2);
  });

  it("never harvests a private file or increases the ceiling when the export allowance is exhausted", async () => {
    const write = mockWrite();
    await router([tool("file_write", { ...args, artifact_name: null }), final(args.content)], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Prepare phase findings", deps, log, true,
      reviewedPhase({ maxToolCalls: 1 }), undefined, ["handoff.md"])).rejects.toMatchObject({
      message: "Missing required named mission output: handoff.md; insufficient remaining reviewed tool calls",
      evidence: { rounds: 1, usage: { totalTokens: 11 } },
      phase: { attemptedToolCalls: 1, successfulToolCalls: 1, maxToolCalls: 1 },
    });
    expect(requests).toHaveLength(1);
    expect(write).toHaveBeenCalledTimes(1);
  });

  it("reserves the last allowed call for an explicit named export without widening authority", async () => {
    mockWrite();
    await router([tool("file_write", { ...args, artifact_name: null }), tool("file_write", args), final()],
      200, { allowed: true });
    const result = await executeTaskWithEvidence("Prepare phase findings", deps, log, true,
      reviewedPhase({ capabilities: ["filesystem-read", "filesystem-write"] }), undefined, ["handoff.md"]);
    expect(requests.every(request => request.parallel_tool_calls === false)).toBe(true);
    expect(requests[0].tool_choice).toBe("required");
    expect(requests[1].tool_choice).toEqual({ type: "function", function: { name: "file_write" } });
    expect(requests[1].tools.map((entry: any) => entry.function.name)).toEqual(["file_write"]);
    expect(requests[1].tools[0].function.parameters.properties.artifact_name).toMatchObject({
      type: "string", enum: ["handoff.md"],
    });
    expect(requests[2].tools).toEqual([]);
    expect(requests[2].tool_choice).toBe("none");
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 2, maxToolCalls: 2 });
    expect(result.artifacts).toEqual({ "handoff.md": args.content });
  });

  it("narrows the required export choice after each committed output", async () => {
    mockWrite();
    await router([tool("file_write", args),
      tool("file_write", { ...args, path: "/sandbox/checklist.md", artifact_name: "checklist.md" }), final()],
      200, { allowed: true });
    const result = await executeTaskWithEvidence("Prepare two outputs", deps, log, true,
      reviewedPhase(), undefined, ["handoff.md", "checklist.md"]);
    expect(requests[0].tools[0].function.parameters.properties.artifact_name.enum).toEqual(["checklist.md", "handoff.md"]);
    expect(requests[1].tools[0].function.parameters.properties.artifact_name.enum).toEqual(["checklist.md"]);
    expect(Object.keys(result.artifacts ?? {}).sort()).toEqual(["checklist.md", "handoff.md"]);
  });

  it("rejects an impossible output count before model or filesystem access", async () => {
    const write = mockWrite();
    await router([final()]);
    await expect(executeTaskWithEvidence("Prepare two outputs", deps, log, true,
      reviewedPhase({ maxToolCalls: 1 }), undefined, ["handoff.md", "checklist.md"])).rejects.toMatchObject({
      message: expect.stringContaining("insufficient remaining reviewed tool calls"),
      evidence: { rounds: 0, usage: null },
    });
    expect(requests).toHaveLength(0);
    expect(write).not.toHaveBeenCalled();
  });

  it("rejects an output contract without write capability before spending tokens", async () => {
    await router([final()]);
    await expect(executeTaskWithEvidence("Prepare phase findings", deps, log, true,
      reviewedPhase({ capabilities: ["filesystem-read"] }), undefined, ["handoff.md"])).rejects.toMatchObject({
      message: expect.stringContaining("Required named outputs need a reviewed filesystem-write phase"),
      evidence: { rounds: 0, usage: null },
    });
    expect(requests).toHaveLength(0);
  });

  it("bounds repeated non-exporting final answers by the existing model-round limit", async () => {
    await router([final()]);
    await expect(executeTaskWithEvidence("Prepare phase findings", deps, log, true,
      reviewedPhase(), undefined, ["handoff.md"])).rejects.toMatchObject({
      message: expect.stringContaining("maximum tool-calling rounds (25)"),
      evidence: { rounds: 25, usage: { totalTokens: 275 } },
      phase: { attemptedToolCalls: 0, successfulToolCalls: 0 },
    });
    expect(requests).toHaveLength(25);
  });

  it("freezes the output contract and retains final validation as a separate boundary", () => {
    const names = ["handoff.md"];
    const ledger = new TaskCompletionLedger("model", true, names);
    names[0] = "different.md";
    expect(Object.isFrozen(ledger.requiredOutputs)).toBe(true);
    ledger.record(final());
    expect(() => ledger.finish(final().choices[0])).toThrow("Missing required named mission output: handoff.md");
    ledger.commitArtifacts(ledger.prepareArtifact("handoff.md", args.content));
    expect(ledger.finish(final().choices[0])).toBe("A useful briefing");
    expect(() => new TaskCompletionLedger("model", false, ["handoff.md"])).toThrow("negotiated artifact delivery");
    expect(() => new TaskCompletionLedger("model", true, ["../secret"])).toThrow("Invalid required mission output names");
  });
});

describe("explicit export failure completion", () => {
  const path = "/sandbox/.openclaw/workspace/briefing.md";
  const args = { path, content: "Actual briefing 🌍", artifact_name: "briefing.md" };
  const mockWrite = () => vi.spyOn(taskFilesystem, "writeTaskFile").mockImplementation(async (_path, content) => Buffer.byteLength(content));
  afterEach(() => vi.restoreAllMocks());

  it("allows a failed export to succeed after a same-name committed retry", async () => {
    const write = mockWrite().mockRejectedValueOnce(new Error("disk full"));
    await router([tool("file_write", args), tool("file_write", args), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase());
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(result).toMatchObject({ rounds: 3, usage: { totalTokens: 33 }, phase: { attemptedToolCalls: 2, successfulToolCalls: 1 } });
    expect(write).toHaveBeenCalledTimes(2);
    expect(requests[1].messages.at(-1).content).toContain("disk full");
    expect(requests[2].messages.at(-1).content).toContain("attached as briefing.md");
  });

  it("does not clear a failed export when a different named export succeeds", async () => {
    const write = mockWrite().mockRejectedValueOnce(new Error("disk full"));
    await router([tool("file_write", args), tool("file_write", { ...args, artifact_name: "other.md" }), final()], 200, { allowed: true });
    const failure = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase()).catch(error => error);
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure).toMatchObject({ evidence: { rounds: 3, usage: { totalTokens: 33 } }, phase: { attemptedToolCalls: 2, successfulToolCalls: 1 } });
    expect(failure).not.toHaveProperty("artifacts");
    expect(write).toHaveBeenCalledTimes(2);
    expect(requests[2].messages.at(-1).content).toContain("attached as other.md");
  });

  it("delivers only the later committed replacement after an overwrite retry", async () => {
    const write = mockWrite().mockResolvedValueOnce(Buffer.byteLength(args.content)).mockRejectedValueOnce(new Error("write failed"));
    await router([
      tool("file_write", args), tool("file_write", { ...args, content: "Failed replacement" }),
      tool("file_write", { ...args, content: "Repaired replacement 🌍" }), final(),
    ], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ maxToolCalls: 3 }));
    expect(result.artifacts).toEqual({ "briefing.md": "Repaired replacement 🌍" });
    expect(result).toMatchObject({ rounds: 4, phase: { attemptedToolCalls: 3, successfulToolCalls: 2 } });
    expect(write).toHaveBeenCalledTimes(3);
  });

  it("permits a same-name committed retry later in the same batch", async () => {
    const write = mockWrite().mockRejectedValueOnce(new Error("write failed"));
    await router([batch(tool("file_write", args), tool("file_write", args)), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase());
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 1 });
    expect(write).toHaveBeenCalledTimes(2);
    expect(requests).toHaveLength(2);
  });

  it("keeps every failed name pending until each is repaired", async () => {
    const write = mockWrite().mockRejectedValueOnce(new Error("first failed")).mockRejectedValueOnce(new Error("second failed"));
    await router([
      batch(tool("file_write", args), tool("file_write", { ...args, artifact_name: "second.md" })),
      tool("file_write", args), final(),
    ], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({
      message: "Explicit file_write export failed: file_write error: second failed (1 unresolved export(s))",
      evidence: { rounds: 3, usage: { totalTokens: 33 } },
    });
    expect(write).toHaveBeenCalledTimes(3);
  });

  it("permits a denied export to retry when the later policy evaluation allows it", async () => {
    const write = mockWrite();
    const policy = { allowed: false };
    await router([tool("file_write", args), (res: ServerResponse) => {
      policy.allowed = true;
      res.end(JSON.stringify(tool("file_write", args)));
    }, final()], 200, policy);
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase());
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 1 });
    expect(requests[1].messages.at(-1).content).toContain("Blocked by policy");
    expect(policyRequests).toHaveLength(2);
    expect(write).toHaveBeenCalledTimes(1);
  });

  it("permits corrected phase arguments for the same valid export name", async () => {
    const write = mockWrite();
    await router([tool("file_write", { ...args, content: 1 }), tool("file_write", args), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase());
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(requests[1].messages.at(-1).content).toContain("Blocked by reviewed phase");
    expect(policyRequests).toHaveLength(1);
    expect(write).toHaveBeenCalledTimes(1);
  });

  it("clears a failed commit only after a later same-name commit succeeds", async () => {
    const write = mockWrite();
    const commit = vi.spyOn(TaskCompletionLedger.prototype, "commitArtifacts")
      .mockImplementationOnce(() => { throw new Error("commit rejected"); });
    await router([tool("file_write", args), tool("file_write", { ...args, content: "Committed retry" }), final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase());
    expect(result.artifacts).toEqual({ "briefing.md": "Committed retry" });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 1 });
    expect(requests[1].messages.at(-1).content).toContain("commit rejected");
    expect(write).toHaveBeenCalledTimes(2);
    expect(commit).toHaveBeenCalledTimes(2);
  });

  it.each([42, {}, "../secret.md"])("does not clear invalid name %j with a valid different-name commit", async artifact_name => {
    const write = mockWrite();
    await router([
      tool("file_write", { ...args, artifact_name }),
      tool("file_write", { ...args, artifact_name: "42" }), final(),
    ], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed"), evidence: { rounds: 3 },
    });
    expect(write).toHaveBeenCalledTimes(1);
    expect(requests[2].messages.at(-1).content).toContain("attached as 42");
  });

  it.each([null, undefined])("does not clear a pending export with a successful scratch write (%j)", async artifact_name => {
    const write = mockWrite().mockRejectedValueOnce(new Error("export failed"));
    await router([tool("file_write", args), tool("file_write", { ...args, artifact_name }), final(args.content)], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true, reviewedPhase())).rejects.toMatchObject({
      message: expect.stringContaining("export failed"), evidence: { rounds: 3 },
      phase: { attemptedToolCalls: 2, successfulToolCalls: 1 },
    });
    expect(write).toHaveBeenCalledTimes(2);
    expect(requests[2].messages.at(-1).content).toContain("local only");
  });

  it("does not extend the reviewed attempt ceiling to repair exports", async () => {
    const write = mockWrite().mockRejectedValue(new Error("export failed"));
    await router([tool("file_write", args)], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ maxToolCalls: 1 }))).rejects.toMatchObject({
      message: expect.stringContaining("exceeds maxToolCalls (1)"),
      evidence: { rounds: 2, usage: { totalTokens: 22 } },
      phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
    });
    expect(write).toHaveBeenCalledTimes(1);
  });

  it("bounds repeated failed export retries by the existing model round limit", async () => {
    const write = mockWrite().mockRejectedValue(new Error("export failed"));
    await router([tool("file_write", args)], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({
      message: expect.stringContaining("maximum tool-calling rounds (25)"),
      evidence: { rounds: 25, usage: { totalTokens: 275 } },
    });
    expect(write).toHaveBeenCalledTimes(25);
    expect(requests).toHaveLength(25);
  });

  it("isolates unresolved exports between executions", async () => {
    mockWrite().mockRejectedValueOnce(new Error("export failed"));
    await router([tool("file_write", args), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toBeInstanceOf(TaskExecutionError);
    const result = await executeTaskWithEvidence("Inline-only revision", deps, log, true);
    expect(result.output).toBe("A useful briefing");
    expect(result.artifacts).toBeUndefined();
    expect(result.rounds).toBe(1);
  });

  it("fails a requested export even after the phase minimum was satisfied", async () => {
    const write = mockWrite().mockResolvedValueOnce(7).mockRejectedValueOnce(new Error("disk full"));
    await router([
      tool("file_write", { path, content: "scratch", artifact_name: null }),
      tool("file_write", args), final("Attached briefing.md"),
    ], 200, { allowed: true });
    const failure = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase()).catch(error => error);
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure).toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed"),
      evidence: { rounds: 3, usage: { promptTokens: 21, completionTokens: 12, totalTokens: 33 } },
      phase: { attemptedToolCalls: 2, successfulToolCalls: 1 },
    });
    expect(failure.message).toContain("disk full");
    expect(failure).not.toHaveProperty("artifacts");
    expect(failure).not.toHaveProperty("output");
    expect(write).toHaveBeenCalledTimes(2);
    expect(requests).toHaveLength(3);
  });

  it.each([
    { status: 200, policy: { allowed: false }, phase: undefined },
    { status: 200, policy: { allowed: false }, phase: reviewedPhase({ minToolCalls: 0 }) },
    { status: 503, policy: { allowed: true }, phase: undefined },
    { status: 200, policy: "invalid-json", phase: undefined },
  ])("rejects terminal success after unresolved denied or unavailable-policy exports (%#)", async ({ status, policy, phase }) => {
    const write = mockWrite();
    await router([tool("file_write", args), final("Attached briefing.md")], status, policy);
    await expect(executeTaskWithEvidence("Write", deps, log, true, phase)).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed: Blocked by policy"),
      evidence: { rounds: 2, usage: { totalTokens: 22 } },
    });
    expect(write).not.toHaveBeenCalled();
    expect(requests).toHaveLength(2);
    expect(requests[1].messages.at(-1).content).toContain("Blocked by policy");
    expect(policyRequests).toHaveLength(1);
  });

  it.each([
    { capabilities: ["filesystem-read"], minToolCalls: 0 },
    { capabilities: [], minToolCalls: 0, maxToolCalls: 0 },
  ])("cannot disguise an out-of-phase export as inline success (%#)", async overrides => {
    const write = mockWrite();
    await router([tool("file_write", args), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true, reviewedPhase(overrides))).rejects.toBeInstanceOf(TaskExecutionError);
    expect(write).not.toHaveBeenCalled();
    expect(policyRequests).toEqual([]);
    expect(requests).toHaveLength(overrides.maxToolCalls === 0 ? 1 : 2);
  });

  it.each([
    { path: "/outside/briefing.md" }, { artifact_name: "response.md" },
    { artifact_name: "../secret.md" }, { artifact_name: "__proto__" },
    { artifact_name: "kars-router-observations.json" }, { artifact_name: "" },
    { artifact_name: 42 }, { artifact_name: {} },
    { content: "  \n" }, { content: "🌍".repeat(33 * 1024) },
  ])("cannot forget invalid explicit exports (%#)", async override => {
    const write = mockWrite();
    await router([tool("file_write", { ...args, ...override }), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed"),
      evidence: { rounds: 2, usage: { totalTokens: 22 } },
    });
    expect(write).not.toHaveBeenCalled();
    expect(requests).toHaveLength(2);
  });

  it("fails a parsed explicit export rejected by phase argument validation", async () => {
    const write = mockWrite();
    await router([tool("file_write", { ...args, content: 1 }), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ minToolCalls: 0 }))).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed: Blocked by reviewed phase"),
      phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
    });
    expect(write).not.toHaveBeenCalled();
    expect(policyRequests).toEqual([]);
  });

  it("fails unnegotiated explicit exports rather than returning attachment claims", async () => {
    const write = mockWrite();
    await router([tool("file_write", args), final("Attached briefing.md")], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({
      message: expect.stringContaining("Dispatcher does not support named artifacts"),
      evidence: { rounds: 2, usage: { totalTokens: 22 } },
    });
    expect(write).not.toHaveBeenCalled();
    expect(requests).toHaveLength(2);
  });

  it("does not deliver stale committed bytes after an explicit overwrite fails", async () => {
    const write = mockWrite().mockResolvedValueOnce(Buffer.byteLength(args.content)).mockRejectedValueOnce(new Error("write failed"));
    const ledger = new TaskCompletionLedger("gpt-5.4-mini", true);
    await router([tool("file_write", args), tool("file_write", { ...args, content: "Replacement" }), final()], 200, { allowed: true });
    await expect(processTaskWithTools("Write", deps, log, ledger)).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed"),
      evidence: { rounds: 3, usage: { totalTokens: 33 } },
    });
    expect(ledger.artifactSnapshot()).toEqual({ artifacts: { "briefing.md": args.content } });
    expect(write).toHaveBeenCalledTimes(2);
    expect(requests).toHaveLength(3);
  });

  it("fails if written bytes cannot be committed as an export", async () => {
    const write = mockWrite();
    vi.spyOn(TaskCompletionLedger.prototype, "commitArtifacts").mockImplementationOnce(() => { throw new Error("commit rejected"); });
    const progress: TaskExecutionEvidence[] = [];
    await router([tool("file_write", args), final()], 200, { allowed: true });
    const failure = await executeTaskWithEvidence("Write", { ...deps, onEvidence: event => { progress.push(event); } }, log, true, reviewedPhase()).catch(error => error);
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure).toMatchObject({
      message: "Explicit file_write export failed: file_write error: commit rejected (1 unresolved export(s))",
      phase: { attemptedToolCalls: 1, successfulToolCalls: 0 },
      evidence: { rounds: 2, usage: { totalTokens: 22 } },
    });
    expect(failure).not.toHaveProperty("artifacts");
    expect(progress.at(-1)?.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 0 });
    expect(write).toHaveBeenCalledWith(path, args.content);
    expect(requests).toHaveLength(2);
    expect(requests[1].messages.at(-1).content).toContain("commit rejected");
  });

  it("continues a batch after a failed export but rejects unresolved terminal success", async () => {
    const write = mockWrite().mockRejectedValueOnce(new Error("write failed"));
    await router([batch(tool("file_write", args), tool("file_write", { ...args, artifact_name: "second.md" })), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ minToolCalls: 0 }))).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed"),
      phase: { attemptedToolCalls: 2, successfulToolCalls: 1 },
    });
    expect(write).toHaveBeenCalledTimes(2);
    expect(policyRequests).toHaveLength(2);
    expect(requests).toHaveLength(2);
    expect(requests[1].messages.slice(-2).map((message: any) => message.content)).toEqual([
      "file_write error: write failed", expect.stringContaining("attached as second.md"),
    ]);
  });

  it("fails capacity overflow without committing or delivering the seventeenth export", async () => {
    const write = mockWrite();
    const ledger = new TaskCompletionLedger("gpt-5.4-mini", true);
    await router([...Array.from({ length: 17 }, (_, i) => tool("file_write", { ...args, artifact_name: `${i}.md` })), final()], 200, { allowed: true });
    await expect(processTaskWithTools("Write", deps, log, ledger)).rejects.toMatchObject({
      message: expect.stringContaining("Explicit file_write export failed"),
      evidence: { rounds: 18, usage: { totalTokens: 198 } },
    });
    expect(Object.keys(ledger.artifactSnapshot().artifacts!)).toHaveLength(16);
    expect(write).toHaveBeenCalledTimes(16);
    expect(requests).toHaveLength(18);
  });

  it.each([null, undefined])("permits failed private scratch followed by real exports (%j)", async artifact_name => {
    const write = mockWrite().mockRejectedValueOnce(new Error("scratch write failed"));
    await router([
      tool("file_write", { path, content: "private", artifact_name }),
      tool("file_write", args),
      tool("file_write", { ...args, content: "Private revision", artifact_name: null }), final(),
    ], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ maxToolCalls: 3 }));
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 3, successfulToolCalls: 2 });
    expect(result.rounds).toBe(4);
    expect(write).toHaveBeenCalledTimes(3);
  });

  it.each([false, true])("does not infer exports from objective or final prose (negotiated %j)", async enabled => {
    const write = mockWrite();
    await router([final("Full inline content mentioning briefing.md")]);
    const result = await executeTaskWithEvidence("Write a briefing.md deliverable", deps, log, enabled);
    expect(result.output).toBe("Full inline content mentioning briefing.md");
    expect(result.artifacts).toBeUndefined();
    expect(write).not.toHaveBeenCalled();
  });

  it("does not extract export intent from malformed JSON", async () => {
    const write = mockWrite();
    const malformed = tool("file_write");
    malformed.choices[0].message.tool_calls[0].function.arguments = '{"artifact_name":"briefing.md",';
    await router([malformed, final("Full inline content")], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true);
    expect(result.output).toBe("Full inline content");
    expect(result.artifacts).toBeUndefined();
    expect(write).not.toHaveBeenCalled();
    expect(policyRequests).toEqual([]);
  });

  it.each([1, 2, 3])("still commits explicit Unicode exports across wire split %i", async split => {
    const write = mockWrite();
    const response = Buffer.from(JSON.stringify(tool("file_write", args)));
    const boundary = response.indexOf(Buffer.from("🌍")) + split;
    await router([(res: ServerResponse) => {
      res.writeHead(200, { "content-type": "application/json" });
      res.write(response.subarray(0, boundary));
      setTimeout(() => res.end(response.subarray(boundary)), 10);
    }, final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase());
    expect(write).toHaveBeenCalledWith(path, args.content);
    expect(result.artifacts).toEqual({ "briefing.md": args.content });
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
  });
});

describe("explicit mission attachments", () => {
  it.each([false, true])("commits a same-name retry after a real filesystem failure (older commit %j)", async olderCommit => {
    const directory = await workspace();
    const path = `${directory}/briefing.md`;
    const args = { path, content: "Older commit", artifact_name: "briefing.md" };
    const repaired = "# Repaired briefing\r\nCafé — 日本語 🌍\n";
    await router([
      ...(olderCommit ? [tool("file_write", args)] : []),
      tool("file_write", { ...args, path: directory, content: "Cannot replace a directory" }),
      tool("file_write", { ...args, content: repaired }), final("Attached briefing.md"),
    ], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ maxToolCalls: 3 }));
    expect(result.artifacts).toEqual({ "briefing.md": repaired });
    expect(await readFile(path, "utf8")).toBe(repaired);
    expect(result).toMatchObject({ rounds: olderCommit ? 4 : 3, phase: { attemptedToolCalls: olderCommit ? 3 : 2, successfulToolCalls: olderCommit ? 2 : 1 } });
    expect(requests[olderCommit ? 2 : 1].messages.at(-1).content).toContain("file_write error:");
    expect(requests.at(-1)!.messages.at(-1).content).toContain("attached as briefing.md");
  });

  it.each([false, true])("does not repair a real failed export with a different named commit (older commit %j)", async olderCommit => {
    const directory = await workspace();
    const path = `${directory}/briefing.md`;
    const args = { path, content: "Older commit", artifact_name: "briefing.md" };
    const other = { path: `${directory}/other.md`, content: "Another export 🌍", artifact_name: "other.md" };
    await router([
      ...(olderCommit ? [tool("file_write", args)] : []),
      tool("file_write", { ...args, path: directory, content: "Cannot replace a directory" }),
      tool("file_write", other), final("Full inline fallback"),
    ], 200, { allowed: true });
    const failure = await executeTaskWithEvidence("Write", deps, log, true, reviewedPhase({ maxToolCalls: 3 })).catch(error => error);
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure).toMatchObject({ message: expect.stringContaining("Explicit file_write export failed"), evidence: { rounds: olderCommit ? 4 : 3 } });
    expect(failure).not.toHaveProperty("artifacts");
    expect(failure).not.toHaveProperty("output");
    expect(await readFile(other.path, "utf8")).toBe(other.content);
    if (olderCommit) expect(await readFile(path, "utf8")).toBe(args.content);
    else await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests.at(-1)!.messages.at(-1).content).toContain("attached as other.md");
  });

  it.each([1, 2, 3])("preserves UTF-8 artifact bytes split after emoji byte %i on the response wire", async split => {
    const path = `${await workspace()}/briefing.md`;
    const content = "# Briefing\r\n🌍 Café — 日本語\n";
    const response = Buffer.from(JSON.stringify(tool("file_write", { path, content, artifact_name: "briefing.md" })));
    const boundary = response.indexOf(Buffer.from("🌍")) + split;
    await router([(res: ServerResponse) => {
      res.writeHead(200, { "content-type": "application/json" });
      res.write(response.subarray(0, boundary));
      setTimeout(() => res.end(response.subarray(boundary)), 10);
    }, final()], 200, { allowed: true });
    const result = await executeTaskWithEvidence("Write a briefing", deps, log, true);
    expect(result.artifacts).toEqual({ "briefing.md": content });
    expect(await readFile(path, "utf8")).toBe(content);
    expect(policyRequests[0].context.arguments.content).toBe(content);
  });

  it("preserves a split multibyte character in the inline deliverable", async () => {
    const content = "# Briefing\r\nCafé — 日本語 🌍\n";
    const response = Buffer.from(JSON.stringify(final(content)));
    const boundary = response.indexOf(Buffer.from("é")) + 1;
    await router([(res: ServerResponse) => {
      res.writeHead(200, { "content-type": "application/json" });
      res.write(response.subarray(0, boundary));
      setTimeout(() => res.end(response.subarray(boundary)), 10);
    }]);
    expect((await executeTaskWithEvidence("Write a briefing", deps, log)).output).toBe(content);
  });

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
  it("fails without writing or attaching denied content", async () => {
    const path = `${await workspace()}/denied.md`;
    await router([tool("file_write", { path, content: "denied", artifact_name: "denied.md" }), final()]);
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({ message: expect.stringContaining("Explicit file_write export failed") });
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests).toHaveLength(2);
  });
  it.each(["response.md", "../secret.md", "__proto__"])("does not write invalid attachment %s", async artifact_name => {
    const path = `${await workspace()}/invalid.md`;
    await router([tool("file_write", { path, content: "draft", artifact_name }), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({ message: expect.stringContaining("Explicit file_write export failed: file_write error:") });
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests).toHaveLength(2);
  });
  it.each(["  \n", "🌍".repeat(33 * 1024)])("rejects invalid attachment content before writing (%#)", async content => {
    const path = `${await workspace()}/invalid.md`;
    await router([tool("file_write", { path, content, artifact_name: "invalid.md" }), final()], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({ message: expect.stringContaining("Explicit file_write export failed: file_write error:") });
    await expect(access(path)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests).toHaveLength(2);
  });
  it("fails the seventeenth attachment before writing without delivering a partial set", async () => {
    const directory = await workspace();
    await router([...Array.from({ length: 17 }, (_, i) => tool("file_write", {
      path: `${directory}/${i}.md`, content: `document ${i}`, artifact_name: `${i}.md`,
    })), final()], 200, { allowed: true });
    const failure = await executeTaskWithEvidence("Write", deps, log, true).catch(error => error);
    expect(failure).toBeInstanceOf(TaskExecutionError);
    expect(failure.message).toContain("Explicit file_write export failed: file_write error:");
    expect(failure).not.toHaveProperty("artifacts");
    expect(await readFile(`${directory}/15.md`, "utf8")).toBe("document 15");
    await expect(access(`${directory}/16.md`)).rejects.toMatchObject({ code: "ENOENT" });
    expect(requests).toHaveLength(18);
  });
  it("keeps successful writes on disk but fails delivery after an explicit overwrite fails", async () => {
    const directory = await workspace();
    const path = `${directory}/briefing.md`;
    await router([
      tool("file_write", { path, content: "first", artifact_name: "briefing.md" }),
      tool("file_write", { path, content: "second 🌍", artifact_name: "briefing.md" }),
      tool("file_write", { path: directory, content: "failed overwrite", artifact_name: "briefing.md" }),
      tool("file_write", { path, content: "private scratch", artifact_name: null }), final(),
    ], 200, { allowed: true });
    await expect(executeTaskWithEvidence("Write", deps, log, true)).rejects.toMatchObject({ message: expect.stringContaining("Explicit file_write export failed: file_write error:") });
    expect(await readFile(path, "utf8")).toBe("private scratch");
    expect(requests).toHaveLength(5);
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
    await expect(executeTaskWithEvidence("Write", deps, log)).rejects.toMatchObject({ message: expect.stringContaining("Explicit file_write export failed") });
    expect(requests).toHaveLength(2);
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
