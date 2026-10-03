// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MissionReceiver, missionTargetFromEnvironment } from "./mission-receiver.js";
import { parseMissionMessage, type MissionAssignment, type MissionReply } from "@kars/mesh/dist/mission-protocol.js";
import { TaskExecutionError } from "./task-completion.js";
import { missionIdentity } from "@kars/mesh/dist/mission-identity.js";

const target = { taskName: "mission", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}` };
const assignment: MissionAssignment = { ...target, type: "mission:assign", version: 1, runNonce: "run-1", assignmentId: "assignment-1", bootId: "boot-1", content: "Produce a useful briefing" };
const evidence = { model: "gpt-5.4-mini", rounds: 1, usage: { promptTokens: 10, completionTokens: 12, totalTokens: 22 } };
function setup(overrides: Partial<ConstructorParameters<typeof MissionReceiver>[0]> = {}) {
  const replies: MissionReply[] = [];
  const execute = vi.fn(async () => ({ ...evidence, output: "A useful briefing" }));
  const authorize = vi.fn(async () => true);
  const receiver = new MissionReceiver({ target, execute, authorize, send: async (_to, reply) => { replies.push(reply); }, warn: vi.fn(), bootId: "boot-1", ...overrides });
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
  it("serializes assignments before asynchronous authorization", async () => {
    let allow!: (value: boolean) => void;
    const s = setup({ authorize: () => new Promise(resolve => { allow = resolve; }) });
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await s.receiver.handle(target.dispatcherDid, assignment, "encrypted");
    await s.receiver.handle(target.dispatcherDid, { ...assignment, runNonce: "run-2", assignmentId: "assignment-2" }, "encrypted");
    expect(s.execute).not.toHaveBeenCalled();
    expect(s.replies.map(r => r.status)).toEqual(["rejected"]);
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
    expect(missionTargetFromEnvironment(derivedDid, environment)).toEqual({ ...target, agentDid: derivedDid });
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
