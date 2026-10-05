// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MissionReceiver, missionTargetFromEnvironment, type MissionReceiverOptions } from "./mission-receiver.js";
import { MAX_MISSION_MESSAGE_BYTES, missionContract, parseMissionMessage, type MissionAssignment, type MissionReply } from "@kars/mesh/dist/mission-protocol.js";
import { missionObjectiveDigest, type MissionAdmission } from "@kars/mesh/dist/mission-admission.js";
import { TaskExecutionError } from "./task-completion.js";
import { missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import { missionInputContentDigest } from "@kars/mesh/dist/mission-inputs.js";

const target = { taskName: "mission", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}` };
const assignment: MissionAssignment = { ...target, type: "mission:assign", version: 1, runNonce: "run-1", assignmentId: "assignment-1", bootId: "boot-1", content: "Produce a useful briefing" };
const admission: MissionAdmission = { version: 1, state: "run", taskGeneration: 1, authorizationDigest: `sha256:${"a".repeat(64)}`,
  runNonce: assignment.runNonce, objectiveDigest: missionObjectiveDigest(assignment.content) };
const evidence = { model: "gpt-5.4-mini", rounds: 1, usage: { promptTokens: 10, completionTokens: 12, totalTokens: 22 } };
function setup(overrides: Partial<ConstructorParameters<typeof MissionReceiver>[0]> = {}) {
  const replies: MissionReply[] = [];
  const execute = vi.fn(async () => ({ ...evidence, output: "A useful briefing" }));
  const authorize = vi.fn(async () => true);
  const receiver = new MissionReceiver({ target, admission, execute, authorize, send: async (_to, reply) => { replies.push(reply); }, warn: vi.fn(), bootId: "boot-1", ...overrides });
  return { receiver, replies, execute, authorize };
}

describe("encrypted mission receiver", () => {
  it.each(["plaintext", "unknown"] as const)("rejects %s even when payload claims trusted identities", async security => {
    const s = setup();
    expect(await s.receiver.handle(target.dispatcherDid, assignment, security)).toBe(true);
    expect(s.authorize).not.toHaveBeenCalled();
    expect(s.replies).toEqual([]);
  });
  it("rejects a different cryptographically authenticated sender", async () => {
    const s = setup();
    await s.receiver.handle(target.agentDid, assignment, "encrypted");
    expect(s.execute).not.toHaveBeenCalled();
  });
  it.each(["taskUid", "sandboxUid", "podUid", "taskName", "runNonce", "bootId", "agentDid", "dispatcherDid"])("rejects an invalid %s binding", async field => {
    const s = setup();
    await s.receiver.handle(target.dispatcherDid, { ...assignment, [field]: field === "runNonce" ? "" : "mismatch" }, "encrypted");
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.replies).toEqual([]);
  });
  it("answers a correlated probe without executing", async () => {
    const s = setup();
    await s.receiver.handle(target.dispatcherDid, { ...target, type: "mission:probe", version: 1, challenge: "probe-1", runNonce: "run-1" }, "encrypted");
    expect(s.replies).toHaveLength(1);
    expect(s.replies[0]).toMatchObject({ type: "mission:reply", status: "ready", challenge: "probe-1", bootId: "boot-1" });
    expect(s.execute).not.toHaveBeenCalled();
  });
  it("delivers once and replays the exact terminal response without executing twice", async () => {
    const s = setup();
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(s.replies.map(r => r.status)).toEqual(["accepted", "succeeded"]);
    expect(s.replies[1]).toMatchObject({ ...target, runNonce: "run-1", assignmentId: "assignment-1", output: "A useful briefing", evidence });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    expect(s.execute).toHaveBeenCalledTimes(1);
    expect(s.replies[2]).toEqual(s.replies[1]);
    await s.receiver.handle(target.dispatcherDid, { ...assignment, content: "Different content" }, "encrypted");
    expect(s.execute).toHaveBeenCalledTimes(1);
    expect(s.replies).toHaveLength(3);
  });
  it("negotiates attachments, preserves bytes and replays without execution or mutation", async () => {
    const artifacts = { "briefing.md": "# Briefing\r\nCafé — 日本語 🌍\n" };
    const execute = vi.fn(async () => ({ ...evidence, output: "Briefing attached", artifacts }));
    const s = setup({ execute });
    const negotiated = { ...assignment, artifactFormat: "text-v1" as const };
    await s.receiver.handle(target.dispatcherDid, negotiated, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(execute).toHaveBeenCalledWith(assignment.content, expect.any(Function), true, undefined, undefined);
    const terminal = structuredClone(s.replies.at(-1)!);
    expect(terminal.artifacts).toEqual(artifacts);
    artifacts["briefing.md"] = "changed executor state";
    await s.receiver.handle(target.dispatcherDid, negotiated, "encrypted");
    expect(s.replies.at(-1)).toEqual(terminal);
    const count = s.replies.length;
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    expect(s.replies).toHaveLength(count);
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("keeps old assignments on the inline executor path", async () => {
    const s = setup();
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(s.execute).toHaveBeenCalledWith(assignment.content, expect.any(Function), false, undefined, undefined);
    expect(s.replies.at(-1)).not.toHaveProperty("artifacts");
    expect(s.replies.at(-1)).not.toHaveProperty("artifactFormat");
  });
  it.each([new Date(), { "response.md": "overwrite" }, { "a.md": "x".repeat(128 * 1024) }, { "a.md": " " }])("fails invalid raw attachments without leaking content in failure evidence %#", async artifacts => {
    const s = setup({ execute: async () => ({ ...evidence, output: "Private draft", artifacts: artifacts as unknown as Record<string, string> }) });
    await s.receiver.handle(target.dispatcherDid, { ...assignment, artifactFormat: "text-v1" }, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    const failed = s.replies.at(-1)!;
    expect(failed).not.toHaveProperty("artifacts");
    expect(failed).not.toHaveProperty("output");
    expect(failed.evidence).toEqual(evidence);
    expect(parseMissionMessage(failed)).not.toBeNull();
  });
  it("rejects executor attachments when the assignment did not negotiate them", async () => {
    const s = setup({ execute: async () => ({ ...evidence, output: "Draft", artifacts: { "a.md": "text" } }) });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.at(-1)?.evidence).toEqual(evidence);
  });
  it("consumes unsupported attachment formats without executing", async () => {
    const s = setup();
    expect(await s.receiver.handle(target.dispatcherDid, { ...assignment, artifactFormat: "text-v2" }, "encrypted")).toBe(true);
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.replies).toEqual([]);
  });
  it("serializes assignments before asynchronous authorization", async () => {
    let allow!: (value: boolean) => void;
    const s = setup({ authorize: () => new Promise(resolve => { allow = resolve; }) });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await s.receiver.handle(target.dispatcherDid, { ...assignment, runNonce: "run-2", assignmentId: "assignment-2" }, "encrypted");
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.replies).toEqual([]);
    allow(true);
    await vi.waitFor(() => expect(s.execute).toHaveBeenCalledTimes(1));
  });
  it("fails closed on policy unavailability before acceptance", async () => {
    const s = setup({ authorize: async () => { throw new Error("policy unavailable"); } });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.replies).toHaveLength(1);
  });
  it("reports failure and unknown usage without converting it into success", async () => {
    const s = setup({ execute: async () => { throw new TaskExecutionError("response aborted", { ...evidence, usage: null }); } });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.at(-1)).toMatchObject({ evidence: { usage: null }, error: "response aborted" });
  });
  it("does not hand back oversized or unmetered output as delivered", async () => {
    const s = setup({ execute: async () => ({ ...evidence, output: "x".repeat(192 * 1024) }) });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.some(r => r.status === "succeeded")).toBe(false);
    expect(s.replies.at(-1)?.evidence).toEqual(evidence);
    expect(parseMissionMessage(s.replies.at(-1))).not.toBeNull();
  });
  it("retains the terminal response when sending fails", async () => {
    let available = false;
    const sent: MissionReply[] = [];
    const s = setup({ send: async (_to, reply) => { if (!available) throw new Error("offline"); sent.push(reply); } });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.execute).toHaveBeenCalledTimes(1));
    available = true;
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(sent.at(-1)?.status).toBe("succeeded"));
    expect(s.execute).toHaveBeenCalledTimes(1);
  });
  it("consumes malformed reserved messages without passing them to legacy handlers", async () => {
    const s = setup();
    expect(await s.receiver.handle(target.dispatcherDid, { type: "mission:future" }, "encrypted")).toBe(true);
    expect(await s.receiver.handle(target.dispatcherDid, { type: "task_request" }, "encrypted")).toBe(false);
  });
});

describe("installed run admission", () => {
  const probe = { ...target, type: "mission:probe", version: 1, challenge: "probe-1", runNonce: assignment.runNonce };
  it.each([undefined, null, {}, { ...admission, taskGeneration: 0 }])("rejects invalid constructor admission %#", value => {
    expect(() => setup({ admission: value as MissionAdmission })).toThrow("Invalid installed mission admission");
  });
  it("keeps an idle runtime silent without authorizing or executing", async () => {
    const s = setup({ admission: { version: 1, state: "idle", taskGeneration: 1, authorizationDigest: admission.authorizationDigest } });
    for (const message of [probe, assignment]) expect(await s.receiver.handle(target.dispatcherDid, message, "encrypted")).toBe(true);
    expect(s.replies).toEqual([]);
    expect(s.authorize).not.toHaveBeenCalled();
    expect(s.execute).not.toHaveBeenCalled();
  });
  it("drops uninstalled runs and altered objectives before they can consume replay capacity", async () => {
    const s = setup();
    for (let i = 0; i < 129; i++) {
      for (const message of [{ ...probe, runNonce: `other-${i}` }, { ...assignment, runNonce: `other-${i}` },
        { ...assignment, content: `${assignment.content} ${i}` }]) {
        await s.receiver.handle(target.dispatcherDid, message, "encrypted");
      }
    }
    expect(s.replies).toEqual([]);
    expect(s.authorize).not.toHaveBeenCalled();
    expect(s.execute).not.toHaveBeenCalled();
    await s.receiver.handle(target.dispatcherDid, probe, "encrypted");
    expect(s.replies.at(-1)?.status).toBe("ready");
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    const terminal = s.replies.at(-1);
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    expect(s.replies.at(-1)).toEqual(terminal);
    expect(s.authorize).toHaveBeenCalledTimes(1);
    expect(s.execute).toHaveBeenCalledTimes(1);
  });
  it("captures admission across asynchronous authorization and keeps exact objective bytes", async () => {
    const content = "\ufeff Briefing\r\nCafé — 日本語 🌍 \n";
    const installed = { ...admission, objectiveDigest: missionObjectiveDigest(content) };
    let allow!: (allowed: boolean) => void;
    const s = setup({ admission: installed, authorize: () => new Promise(resolve => { allow = resolve; }) });
    await s.receiver.handle(target.dispatcherDid, { ...assignment, content: content.trim() }, "encrypted");
    expect(s.replies).toEqual([]);
    await s.receiver.handle(target.dispatcherDid, { ...assignment, content }, "encrypted");
    installed.runNonce = "other-run"; installed.objectiveDigest = missionObjectiveDigest("replacement");
    await s.receiver.handle(target.dispatcherDid, { ...probe, runNonce: "other-run" }, "encrypted");
    expect(s.replies).toEqual([]);
    allow(true);
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(s.execute).toHaveBeenCalledWith(content, expect.any(Function), false, undefined, undefined);
    await s.receiver.handle(target.dispatcherDid, { ...assignment, content }, "encrypted");
    expect(s.execute).toHaveBeenCalledTimes(1);
  });
});

describe("immutable input receiver", () => {
  const phase = { name: "review-draft", objective: "Review the supplied immutable document", capabilities: ["filesystem-read"], minToolCalls: 1, maxToolCalls: 2 };
  const content = "Upstream document\r\n✓ 日本語 🌍";
  const inputs = [{ name: "draft.md", content, sha256: missionInputContentDigest(content), source: {
    namespace: "kars-system", taskName: "writer", taskUid: "upstream-task", sandboxUid: "upstream-sandbox",
    podUid: "upstream-pod", runNonce: "upstream-run", assignmentId: "upstream-assignment", agentDid: target.agentDid, artifactName: "draft.md",
  } }];
  const contract = missionContract(phase, inputs);
  const constrained = { ...assignment, ...contract, inputArtifacts: inputs, artifactFormat: "text-v1" as const };
  const measured = { ...evidence, phase: { name: phase.name, minToolCalls: 1, maxToolCalls: 2, attemptedToolCalls: 1, successfulToolCalls: 1 } };

  it("captures inputs before asynchronous authorization and replays only the exact terminal", async () => {
    let allow!: (value: boolean) => void;
    const execute = vi.fn<MissionReceiverOptions["execute"]>(async (_content, progress) => {
      progress(measured); return { ...measured, output: "Review complete", artifacts: { "review.md": "A separate useful review" } };
    });
    const s = setup({ expectedContract: contract, execute, authorize: () => new Promise(resolve => { allow = resolve; }) });
    await s.receiver.handle(target.dispatcherDid, { ...target, ...contract, runNonce: assignment.runNonce, type: "mission:probe", challenge: "input-probe" }, "encrypted");
    expect(s.replies[0]).toMatchObject({ ...contract, status: "ready" });
    const caller = structuredClone(constrained);
    await s.receiver.handle(target.dispatcherDid, caller, "encrypted");
    caller.inputArtifacts[0].content = "mutated"; caller.inputArtifacts[0].source.runNonce = "mutated";
    caller.inputArtifacts.length = 0;
    allow(true);
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(execute).toHaveBeenCalledWith(assignment.content, expect.any(Function), true, contract.reviewedPhase, inputs);
    for (const value of [execute.mock.calls[0][4], execute.mock.calls[0][4]![0], execute.mock.calls[0][4]![0].source]) expect(Object.isFrozen(value)).toBe(true);
    expect(s.replies.map(r => r.status)).toEqual(["ready", "accepted", "running", "succeeded"]);
    const terminal = s.replies.at(-1)!;
    expect(terminal).toMatchObject({ inputDigest: contract.inputDigest, artifacts: { "review.md": "A separate useful review" } });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    expect(s.replies.at(-1)).toEqual(terminal);
    expect(execute).toHaveBeenCalledTimes(1);
    for (const response of s.replies) {
      expect(response).not.toHaveProperty("inputArtifacts");
      expect(JSON.stringify(response)).not.toContain("Upstream document");
      expect(parseMissionMessage(response)).not.toBeNull();
    }
  });

  it("rejects self-consistent but uninstalled input contracts before authorization or replay reservation", async () => {
    const execute = vi.fn<MissionReceiverOptions["execute"]>(async () => ({ ...measured, output: "Review complete" }));
    const s = setup({ expectedContract: contract, execute });
    const changed = structuredClone(inputs); changed[0].source.runNonce = "another-run";
    const alternative = { ...constrained, ...missionContract(phase, changed), inputArtifacts: changed };
    for (let i = 0; i < 129; i++) await s.receiver.handle(target.dispatcherDid, { ...alternative, assignmentId: `rejected-${i}` }, "encrypted");
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    expect(s.authorize).not.toHaveBeenCalled(); expect(execute).not.toHaveBeenCalled(); expect(s.replies).toEqual([]);
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(s.authorize).toHaveBeenCalledTimes(1); expect(execute).toHaveBeenCalledTimes(1);
  });

  it("does not execute or return input bytes when policy denies the admitted assignment", async () => {
    const s = setup({ expectedContract: contract, authorize: async () => false });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.replies.at(-1)).not.toHaveProperty("inputArtifacts");
    expect(JSON.stringify(s.replies)).not.toContain("Upstream document");
  });
});

describe("reviewed filesystem phase receiver", () => {
  const contract = missionContract({ name: "write-briefing", objective: "Write a useful bounded briefing", capabilities: ["filesystem-write"], minToolCalls: 1, maxToolCalls: 2 });
  const constrained: MissionAssignment = { ...assignment, ...contract, artifactFormat: "text-v1" };
  const measured = (attemptedToolCalls = 1, successfulToolCalls = 1): NonNullable<MissionReply["evidence"]> => ({
    ...evidence, usage: { ...evidence.usage },
    phase: { name: "write-briefing", attemptedToolCalls, successfulToolCalls, minToolCalls: 1, maxToolCalls: 2 },
  });
  it("negotiates the exact contract and forwards an immutable phase after asynchronous authorization", async () => {
    let allow!: (value: boolean) => void;
    const execute = vi.fn<MissionReceiverOptions["execute"]>(async (_content, progress) => {
      const counters = measured(); progress(counters);
      counters.phase!.successfulToolCalls = 0;
      return { ...measured(), output: "Written briefing", artifacts: { "briefing.md": "Real result bytes\r\n" } };
    });
    const s = setup({ expectedContract: contract, execute, authorize: () => new Promise(resolve => { allow = resolve; }) });
    await s.receiver.handle(target.dispatcherDid, { ...target, ...contract, type: "mission:probe", challenge: "phase-probe", runNonce: "run-1" }, "encrypted");
    expect(s.replies[0]).toMatchObject({ ...contract, status: "ready" });
    const input = structuredClone(constrained);
    await s.receiver.handle(target.dispatcherDid, input, "encrypted");
    input.reviewedPhase!.maxToolCalls = 32; input.content = "Changed after authorization started";
    allow(true);
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    expect(execute).toHaveBeenCalledWith(assignment.content, expect.any(Function), true, contract.reviewedPhase, undefined);
    expect(Object.isFrozen(execute.mock.calls[0][3])).toBe(true);
    expect(s.replies.map(reply => reply.status)).toEqual(["ready", "accepted", "running", "succeeded"]);
    expect(s.replies[2].evidence?.phase?.successfulToolCalls).toBe(1);
    const terminal = s.replies.at(-1)!;
    expect(terminal.evidence).toEqual(measured());
    expect(Object.isFrozen(terminal.evidence!.phase)).toBe(true);
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    expect(s.replies.at(-1)).toEqual(terminal);
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it.each(["missing", "regressing", "unknown-to-known", "swallowed"])("fails closed on %s progress without erasing validated counters", async variant => {
    const s = setup({ expectedContract: contract, execute: async (_content, progress) => {
      progress(variant === "unknown-to-known" ? { ...measured(), usage: null } : measured());
      const invalid = variant === "missing" ? evidence : variant === "regressing" ? measured(0, 0) : measured();
      if (variant === "swallowed") {
        try { progress(measured(0, 0)); } catch { /* Deliberately ignore the rejected callback. */ }
        return { ...measured(), output: "Must not succeed" };
      }
      progress(invalid);
      return { ...measured(), output: "Must not succeed" };
    } });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.map(reply => reply.status)).toEqual(["accepted", "running", "failed"]);
    expect(s.replies.at(-1)?.evidence).toEqual({ ...measured(), usage: null });
    expect(s.replies.every(reply => parseMissionMessage(reply))).toBe(true);
  });
  it("refuses success below the minimum but retains exact bounded accounting", async () => {
    const s = setup({ expectedContract: contract, execute: async () => ({ ...measured(1, 0), output: "Prose is not a successful file operation" }) });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.at(-1)?.evidence).toEqual(measured(1, 0));
  });
  it.each(["artifacts", "oversized-output", "interrupted"])("retains measured phase counts after %s failure", async variant => {
    const s = setup({ expectedContract: contract, execute: async (_content, progress) => {
      progress(measured(1, 0));
      if (variant === "interrupted") throw new TaskExecutionError("Interrupted", measured());
      return { ...measured(), output: variant === "oversized-output" ? "x".repeat(192 * 1024) : "Result",
        ...(variant === "artifacts" ? { artifacts: { "response.md": "Invalid" } } : {}) };
    } });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.at(-1)?.evidence).toEqual(measured());
    expect(s.replies.at(-1)).not.toHaveProperty("artifacts");
    expect(s.replies.at(-1)).not.toHaveProperty("output");
  });
  it("ignores malformed error evidence while preserving prior counters and unknown final usage", async () => {
    const s = setup({ expectedContract: contract, execute: async (_content, progress) => {
      progress(measured()); throw new TaskExecutionError("Untrusted failure counts", measured(0, 0));
    } });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.at(-1)?.evidence).toEqual({ ...measured(), usage: null });
  });
  it("caps journal events and seals callbacks after execution", async () => {
    let callback!: (value: NonNullable<MissionReply["evidence"]>) => void;
    const s = setup({ expectedContract: contract, execute: async (_content, progress) => {
      callback = progress;
      for (let i = 0; i < 127; i++) progress(measured());
      return { ...measured(), output: "Unreachable" };
    } });
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies).toHaveLength(128);
    const terminal = structuredClone(s.replies.at(-1));
    expect(() => callback(measured())).toThrow("closed");
    await s.receiver.handle(target.dispatcherDid, constrained, "encrypted");
    expect(s.replies.at(-1)).toEqual(terminal);
  });
  it.each([1, 2])("answers a maximum-size padded v%i probe with only the negotiated reply fields", async version => {
    const readyContract = missionContract(version === 2 ? contract.reviewedPhase : undefined);
    const input = { ...target, ...readyContract, type: "mission:probe", challenge: "phase-probe", runNonce: "run-1", padding: "" };
    input.padding = "x".repeat(MAX_MISSION_MESSAGE_BYTES - Buffer.byteLength(JSON.stringify(input)));
    expect(Buffer.byteLength(JSON.stringify(input))).toBe(MAX_MISSION_MESSAGE_BYTES);
    expect(parseMissionMessage(input)).not.toBeNull();
    const s = setup({ expectedContract: readyContract });
    await s.receiver.handle(target.dispatcherDid, input, "encrypted");
    expect(s.replies).toHaveLength(1);
    expect(s.replies[0]).toMatchObject({ ...target, ...readyContract, status: "ready", challenge: "phase-probe", runNonce: "run-1", bootId: "boot-1" });
    expect(Object.isFrozen(s.replies[0])).toBe(true);
    expect(s.replies[0]).not.toHaveProperty("padding");
    expect(parseMissionMessage(s.replies[0])).not.toBeNull();
    expect(Buffer.byteLength(JSON.stringify(s.replies[0]))).toBeLessThan(4096);
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.authorize).not.toHaveBeenCalled();
  });

  it.each([true, false])("bounds and freezes a padded assignment failure with measured=%s and replays it unchanged", async measuredFailure => {
    const boundedContract = missionContract({ name: "a".repeat(48), objective: "é".repeat(600), capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 32, maxToolCalls: 32, requiredToolCalls: [], freshContext: true });
    const input = { ...assignment, ...boundedContract, artifactFormat: "text-v1", padding: "" };
    input.padding = "x".repeat(MAX_MISSION_MESSAGE_BYTES - Buffer.byteLength(JSON.stringify(input)));
    expect(Buffer.byteLength(JSON.stringify(input))).toBe(MAX_MISSION_MESSAGE_BYTES);
    expect(parseMissionMessage(input)).not.toBeNull();
    const counters = { model: "m".repeat(253), rounds: 25, usage: { promptTokens: 175, completionTokens: 100, totalTokens: 275 },
      phase: { name: "a".repeat(48), attemptedToolCalls: 32, successfulToolCalls: 32, minToolCalls: 32, maxToolCalls: 32 } };
    const execute = vi.fn<MissionReceiverOptions["execute"]>(async (_content, progress) => {
      progress(counters);
      if (measuredFailure) throw new TaskExecutionError("🔥".repeat(4096), counters);
      throw new Error("🔥".repeat(4096));
    });
    const s = setup({ expectedContract: boundedContract, execute });
    await s.receiver.handle(target.dispatcherDid, input, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("failed"));
    expect(s.replies.map(reply => reply.status)).toEqual(["accepted", "running", "failed"]);
    const failed = s.replies.at(-1)!;
    const retained = structuredClone(failed);
    expect(failed).toMatchObject({ ...boundedContract, error: "🔥".repeat(1024), evidence: { ...counters, usage: measuredFailure ? counters.usage : null } });
    for (const reply of s.replies) {
      expect(reply).not.toHaveProperty("padding");
      expect(reply).not.toHaveProperty("content");
      expect(reply).not.toHaveProperty("output");
      expect(reply).not.toHaveProperty("artifacts");
      expect(parseMissionMessage(reply)).not.toBeNull();
      expect(Buffer.byteLength(JSON.stringify(reply))).toBeLessThan(MAX_MISSION_MESSAGE_BYTES);
    }
    expect(Object.isFrozen(failed)).toBe(true);
    expect(Object.isFrozen(failed.evidence)).toBe(true);
    expect(Object.isFrozen(failed.evidence!.phase)).toBe(true);
    expect(Object.isFrozen(failed.reviewedPhase!.capabilities)).toBe(true);
    expect(() => { failed.evidence!.phase!.successfulToolCalls = 0; }).toThrow(TypeError);
    counters.phase.successfulToolCalls = 0;
    counters.usage.totalTokens = 0;
    await s.receiver.handle(target.dispatcherDid, input, "encrypted");
    expect(s.replies.at(-1)).toEqual(retained);
    expect(execute).toHaveBeenCalledTimes(1);
    expect(s.authorize).toHaveBeenCalledTimes(1);
    await s.receiver.handle(target.dispatcherDid, { ...input, content: "Changed content" }, "encrypted");
    expect(s.replies).toHaveLength(4);
  });

  it("copies error phase evidence independently with explicit phase precedence", () => {
    const original = measured();
    const error = new TaskExecutionError("Failed", original, measured(2, 1).phase);
    original.phase!.attemptedToolCalls = 0;
    expect(error.phase).toEqual(measured(2, 1).phase);
    expect(error.evidence.phase).toEqual(error.phase);
    error.phase!.attemptedToolCalls = 1;
    expect(error.evidence.phase!.attemptedToolCalls).toBe(2);
  });
});

describe("installed mission receiver binding", () => {
  const contract = missionContract({ name: "write-briefing", objective: "Write a useful bounded briefing", capabilities: ["filesystem-write"], minToolCalls: 1, maxToolCalls: 2 });
  const other = missionContract({ ...contract.reviewedPhase, maxToolCalls: 3 });
  it.each([null, { version: 3 }, { version: 1, extra: undefined },
    { ...contract, phaseDigest: "sha256:" + "0".repeat(64) }, { ...contract, extra: undefined },
  ])("refuses invalid installed contracts before receiving messages %#", expectedContract => {
    expect(() => setup({ expectedContract: expectedContract as MissionReceiverOptions["expectedContract"] })).toThrow("Invalid installed mission contract");
  });
  it.each([null, {}, { ...target, taskUid: "" }, { ...target, extra: undefined },
    { ...target, agentDid: "invalid" }, { ...target, runNonce: "injected" },
  ])("refuses invalid installed targets %#", installedTarget => {
    expect(() => setup({ target: installedTarget as MissionReceiverOptions["target"] })).toThrow();
  });
  it.each(["", "invalid boot id"])("refuses invalid boot identity %#", bootId => {
    expect(() => setup({ bootId })).toThrow("Invalid installed mission target or boot identity");
  });
  it("rejects accessor-backed target and contract without invoking getters", () => {
    const get = vi.fn(() => "task-uid");
    const installedTarget = Object.defineProperty({ ...target }, "taskUid", { get, enumerable: true });
    expect(() => setup({ target: installedTarget })).toThrow();
    const expectedContract = Object.defineProperty({ ...contract }, "reviewedPhase", { get, enumerable: true });
    expect(() => setup({ expectedContract })).toThrow();
    expect(get).not.toHaveBeenCalled();
  });
  it.each([
    { installed: undefined, incoming: contract },
    { installed: missionContract(), incoming: contract },
    { installed: contract, incoming: missionContract() },
    { installed: contract, incoming: other },
  ])("rejects mismatched probes and assignments before authorization or nonce reservation %#", async ({ installed, incoming }) => {
    const expected = installed ?? missionContract();
    const phase = expected.reviewedPhase ? { name: expected.reviewedPhase.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 } : undefined;
    const execute = vi.fn(async () => ({ ...evidence, ...(phase ? { phase } : {}), output: "A useful briefing" }));
    const s = setup({ expectedContract: installed, execute });
    for (let i = 0; i < 129; i++) {
      const runNonce = i === 0 ? assignment.runNonce : `rejected-${i}`;
      expect(await s.receiver.handle(target.dispatcherDid, { ...target, ...incoming, type: "mission:probe", challenge: "probe-1", runNonce }, "encrypted")).toBe(true);
      expect(await s.receiver.handle(target.dispatcherDid, { ...assignment, ...incoming, runNonce }, "encrypted")).toBe(true);
    }
    expect(s.replies).toEqual([]);
    expect(s.authorize).not.toHaveBeenCalled();
    expect(execute).not.toHaveBeenCalled();
    const accepted = { ...assignment, ...expected };
    await s.receiver.handle(target.dispatcherDid, accepted, "encrypted");
    await vi.waitFor(() => expect(s.replies.at(-1)?.status).toBe("succeeded"));
    const terminal = s.replies.at(-1);
    await s.receiver.handle(target.dispatcherDid, accepted, "encrypted");
    expect(s.replies.map(reply => reply.status)).toEqual(["accepted", "succeeded", "succeeded"]);
    expect(s.replies.at(-1)).toBe(terminal);
    expect(s.authorize).toHaveBeenCalledTimes(1);
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("captures options independently and executes the installed immutable phase, not caller-owned data", async () => {
    const replies: MissionReply[] = [];
    const expectedContract = structuredClone(contract);
    const installedTarget = { ...target };
    let authorized: MissionAssignment | undefined;
    const execute = vi.fn<MissionReceiverOptions["execute"]>(async (_content, _progress, _artifacts, phase) => {
      expect(phase).toEqual(contract.reviewedPhase);
      expect(phase).not.toBe(expectedContract.reviewedPhase);
      expect(phase).not.toBe(authorized?.reviewedPhase);
      expect(Object.isFrozen(phase)).toBe(true);
      expect(Object.isFrozen(phase?.capabilities)).toBe(true);
      expect(() => { phase!.maxToolCalls = 32; }).toThrow(TypeError);
      return { ...evidence, phase: { name: phase!.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 }, output: "A useful briefing" };
    });
    const authorize = vi.fn<MissionReceiverOptions["authorize"]>(async incoming => { authorized = incoming; return true; });
    const options: MissionReceiverOptions = { target: installedTarget, admission, expectedContract, execute, authorize,
      send: async (_to, reply) => { replies.push(reply); }, warn: vi.fn(), bootId: "boot-1" };
    const receiver = new MissionReceiver(options);
    installedTarget.taskUid = "mutated-task";
    expectedContract.reviewedPhase!.maxToolCalls = 32;
    expectedContract.phaseDigest = other.phaseDigest;
    options.target = { ...target, podUid: "other-pod" };
    options.expectedContract = other;
    options.bootId = "other-boot";
    options.authorize = vi.fn(async () => false);
    options.execute = vi.fn(async () => { throw new Error("replacement executor"); });
    options.send = vi.fn(async () => undefined);
    await receiver.handle(target.dispatcherDid, { ...target, ...contract, type: "mission:probe", challenge: "probe-1", runNonce: assignment.runNonce }, "encrypted");
    expect(replies.at(-1)).toMatchObject({ ...target, ...contract, status: "ready", bootId: "boot-1" });
    await receiver.handle(target.dispatcherDid, { ...assignment, ...contract }, "encrypted");
    await vi.waitFor(() => expect(replies.at(-1)?.status).toBe("succeeded"));
    expect(execute).toHaveBeenCalledTimes(1);
    expect(authorize).toHaveBeenCalledTimes(1);
    expect(options.authorize).not.toHaveBeenCalled();
    expect(options.execute).not.toHaveBeenCalled();
    expect(options.send).not.toHaveBeenCalled();
  });
});

describe("mission wire and environment validation", () => {
  const environment = {
    KARS_MISSION_DISPATCH_ENABLED: "true", KARS_MISSION_IDENTITY_ROOT: "c".repeat(64), KARS_MISSION_TASK_NAME: target.taskName,
    KARS_MISSION_TASK_UID: target.taskUid, KARS_MISSION_SANDBOX_UID: target.sandboxUid, KARS_MISSION_POD_UID: target.podUid,
    KARS_MISSION_DISPATCHER_DID: target.dispatcherDid,
  };
  const derivedDid = missionIdentity(environment.KARS_MISSION_IDENTITY_ROOT, "runtime", target.sandboxUid, target.podUid).did;
  it("requires a complete binding and independently secret root", () => {
    expect(missionTargetFromEnvironment(target.agentDid, {})).toBeNull();
    expect(() => missionTargetFromEnvironment(target.agentDid, { KARS_MISSION_DISPATCH_ENABLED: "true" })).toThrow("Secret-backed");
    const captured = missionTargetFromEnvironment(derivedDid, environment);
    expect(captured).toEqual({ ...target, agentDid: derivedDid });
    expect(Object.isFrozen(captured)).toBe(true);
  });
  it.each(["KARS_MISSION_SANDBOX_UID", "KARS_MISSION_POD_UID", "KARS_MISSION_IDENTITY_ROOT"])("rejects a DID from a different %s", field => {
    const value = field === "KARS_MISSION_IDENTITY_ROOT" ? "d".repeat(64) : "different-uid";
    expect(() => missionTargetFromEnvironment(derivedDid, { ...environment, [field]: value })).toThrow("does not match");
  });
  it("does not accept a legacy signing seed or public Entra app ID as the mission root", () => {
    const { KARS_MISSION_IDENTITY_ROOT: _root, ...binding } = environment;
    expect(() => missionTargetFromEnvironment(derivedDid, {
      ...binding, KARS_IDENTITY_SEED: "c".repeat(64), PINNED_AGENT_IDENTITY_APP_ID: "public-app-id",
    })).toThrow("Secret-backed");
  });
  it("rejects invalid or absent success evidence", () => {
    const reply = { ...assignment, type: "mission:reply", status: "succeeded", output: "answer", evidence };
    expect(parseMissionMessage(reply)).not.toBeNull();
    expect(parseMissionMessage({ ...reply, evidence: undefined })).toBeNull();
    expect(parseMissionMessage({ ...reply, evidence: { ...evidence, usage: null } })).toBeNull();
    expect(parseMissionMessage({ ...reply, evidence: { ...evidence, usage: { ...evidence.usage, totalTokens: 0 } } })).toBeNull();
  });
});
