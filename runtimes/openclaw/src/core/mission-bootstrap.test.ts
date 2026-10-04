// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { acquireMissionWriter, missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import { missionContract } from "@kars/mesh/dist/mission-protocol.js";
import { initializeMissionBinding } from "./mission-bootstrap.js";

vi.mock("@kars/mesh/dist/mission-identity.js", async importOriginal => {
  const actual = await importOriginal<typeof import("@kars/mesh/dist/mission-identity.js")>();
  return { ...actual, acquireMissionWriter: vi.fn(), missionIdentity: vi.fn(actual.missionIdentity) };
});

const writerKey = Symbol.for("kars-mission-prekey-writer");
const state = process as typeof process & { [writerKey]?: ReturnType<typeof acquireMissionWriter> };
const admission = { version: 1, state: "idle", taskGeneration: 1, authorizationDigest: `sha256:${"a".repeat(64)}` };
const env = {
  KARS_MISSION_ADMISSION: JSON.stringify(admission),
  KARS_MISSION_DISPATCH_ENABLED: "true", KARS_MISSION_IDENTITY_ROOT: "a".repeat(64),
  KARS_MISSION_TASK_NAME: "mission", KARS_MISSION_TASK_UID: "task-uid",
  KARS_MISSION_SANDBOX_UID: "sandbox-uid", KARS_MISSION_POD_UID: "pod-uid",
  KARS_MISSION_DISPATCHER_DID: `did:mesh:${"b".repeat(32)}`,
};
let writer: Awaited<ReturnType<typeof acquireMissionWriter>>;
beforeEach(() => {
  delete state[writerKey];
  vi.clearAllMocks();
  writer = { close: vi.fn(async () => {}) };
  vi.mocked(acquireMissionWriter).mockResolvedValue(writer);
});
afterEach(() => { delete state[writerKey]; });

describe("mission runtime binding bootstrap", () => {
  const contract = missionContract({ name: "write-briefing", objective: "Write a useful bounded briefing", capabilities: ["filesystem-write"], minToolCalls: 1, maxToolCalls: 2 });
  it("leaves disabled legacy initialization unchanged and acquires no writer", async () => {
    expect(await initializeMissionBinding({ KARS_MISSION_CONTRACT: "invalid", KARS_MISSION_ADMISSION: "invalid" })).toBeNull();
    expect(acquireMissionWriter).not.toHaveBeenCalled();
    expect(missionIdentity).not.toHaveBeenCalled();
  });
  it("waits for exclusive writer custody before deriving any key", async () => {
    let acquired!: (value: typeof writer) => void;
    vi.mocked(acquireMissionWriter).mockReturnValue(new Promise(resolve => { acquired = resolve; }));
    const pending = initializeMissionBinding(env);
    expect(missionIdentity).not.toHaveBeenCalled();
    expect(state[writerKey]).toBeDefined();
    acquired(writer);
    const binding = await pending;
    expect(binding?.identity.signingPrivateKey.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
    expect(binding?.contract).toEqual({ version: 1 });
    await expect(state[writerKey]).resolves.toBe(writer);
    expect(writer.close).not.toHaveBeenCalled();
  });
  it("prevents parallel contexts from starting a second acquisition", async () => {
    let acquired!: (value: typeof writer) => void;
    vi.mocked(acquireMissionWriter).mockReturnValue(new Promise(resolve => { acquired = resolve; }));
    const pending = initializeMissionBinding(env);
    await expect(initializeMissionBinding(env)).rejects.toThrow("already attempted");
    expect(acquireMissionWriter).toHaveBeenCalledTimes(1);
    acquired(writer);
    await pending;
  });
  it("rejects a lock collision before identity construction and forbids retries", async () => {
    vi.mocked(acquireMissionWriter).mockRejectedValue(new Error("EADDRINUSE"));
    await expect(initializeMissionBinding(env)).rejects.toThrow("EADDRINUSE");
    expect(missionIdentity).not.toHaveBeenCalled();
    await expect(initializeMissionBinding(env)).rejects.toThrow("already attempted");
    expect(acquireMissionWriter).toHaveBeenCalledTimes(1);
  });
  it("retains writer custody after binding validation fails", async () => {
    await expect(initializeMissionBinding({ ...env, KARS_MISSION_TASK_UID: "" })).rejects.toThrow("complete controller-owned binding");
    await expect(state[writerKey]).resolves.toBe(writer);
    expect(writer.close).not.toHaveBeenCalled();
    await expect(initializeMissionBinding(env)).rejects.toThrow("already attempted");
  });
  it("does not fall back to legacy seed or Entra identity when the root is missing", async () => {
    await expect(initializeMissionBinding({ ...env, KARS_MISSION_IDENTITY_ROOT: "",
      KARS_IDENTITY_SEED: "d".repeat(64), PINNED_AGENT_IDENTITY_APP_ID: "public-app-id" })).rejects.toThrow("Secret root");
    expect(writer.close).not.toHaveBeenCalled();
  });
  it("captures identity, target and installed contract before waiting for the lock", async () => {
    let acquired!: (value: typeof writer) => void;
    vi.mocked(acquireMissionWriter).mockReturnValue(new Promise(resolve => { acquired = resolve; }));
    const mutable = { ...env, KARS_MISSION_CONTRACT: JSON.stringify(contract) };
    const pending = initializeMissionBinding(mutable);
    mutable.KARS_MISSION_IDENTITY_ROOT = "d".repeat(64);
    mutable.KARS_MISSION_POD_UID = "other-pod";
    mutable.KARS_MISSION_TASK_UID = "other-task";
    mutable.KARS_MISSION_DISPATCHER_DID = `did:mesh:${"e".repeat(32)}`;
    mutable.KARS_MISSION_CONTRACT = JSON.stringify(missionContract());
    mutable.KARS_MISSION_ADMISSION = JSON.stringify({ ...admission, taskGeneration: 2 });
    acquired(writer);
    const binding = await pending;
    expect(binding?.identity.signingPrivateKey.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
    expect(binding?.target).toMatchObject({ podUid: "pod-uid", taskUid: "task-uid", dispatcherDid: env.KARS_MISSION_DISPATCHER_DID });
    expect(binding?.contract).toEqual(contract);
    expect(binding?.admission).toEqual(admission);
    expect(Object.isFrozen(binding?.admission)).toBe(true);
  });
  it.each([missionContract(), contract])("installs an explicit immutable contract %#", async expected => {
    const binding = await initializeMissionBinding({ ...env, KARS_MISSION_CONTRACT: JSON.stringify(expected) });
    expect(binding?.contract).toEqual(expected);
    expect(Object.isFrozen(binding)).toBe(true);
    expect(Object.isFrozen(binding?.target)).toBe(true);
    expect(Object.isFrozen(binding?.contract)).toBe(true);
    if (binding?.contract.reviewedPhase) {
      expect(Object.isFrozen(binding.contract.reviewedPhase)).toBe(true);
      expect(Object.isFrozen(binding.contract.reviewedPhase.capabilities)).toBe(true);
      expect(Object.isFrozen(binding.contract.reviewedPhase.requiredToolCalls)).toBe(true);
    }
  });
  it.each(["", " ", "null", "[]", "{", '{"version":3}', '{"version":1,"extra":true}',
    JSON.stringify({ ...contract, phaseDigest: "sha256:" + "0".repeat(64) }),
    JSON.stringify(missionContract()).padEnd(8193, " "),
    " ".repeat(8191) + "é",
  ])("rejects invalid present configuration without legacy fallback or custody release %#", async value => {
    await expect(initializeMissionBinding({ ...env, KARS_MISSION_CONTRACT: value })).rejects.toThrow("Invalid KARS_MISSION_CONTRACT");
    await expect(state[writerKey]).resolves.toBe(writer);
    expect(writer.close).not.toHaveBeenCalled();
    await expect(initializeMissionBinding(env)).rejects.toThrow("already attempted");
    expect(acquireMissionWriter).toHaveBeenCalledTimes(1);
  });
  it("accepts the exact configuration byte bound", async () => {
    const value = JSON.stringify(contract).padEnd(8192, " ");
    expect(Buffer.byteLength(value)).toBe(8192);
    expect((await initializeMissionBinding({ ...env, KARS_MISSION_CONTRACT: value }))?.contract).toEqual(contract);
  });
  it.each([undefined, "", "null", "[]", "{", '{"version":1}', JSON.stringify({ ...admission, extra: true }),
    JSON.stringify(admission).padEnd(8193, " "), " ".repeat(8191) + "é",
  ])("rejects missing or invalid admission and retains exclusive custody %#", async value => {
    await expect(initializeMissionBinding({ ...env, KARS_MISSION_ADMISSION: value })).rejects.toThrow("Invalid KARS_MISSION_ADMISSION");
    await expect(state[writerKey]).resolves.toBe(writer);
    expect(writer.close).not.toHaveBeenCalled();
    await expect(initializeMissionBinding(env)).rejects.toThrow("already attempted");
    expect(acquireMissionWriter).toHaveBeenCalledTimes(1);
  });
  it.each([admission, { ...admission, state: "run", runNonce: "rev-1", objectiveDigest: `sha256:${"b".repeat(64)}` }])("captures admission at the exact UTF-8 byte bound %#", async expected => {
    const json = JSON.stringify(expected);
    const value = json + " ".repeat(8192 - Buffer.byteLength(json));
    expect(Buffer.byteLength(value)).toBe(8192);
    const binding = await initializeMissionBinding({ ...env, KARS_MISSION_ADMISSION: value });
    expect(binding?.admission).toEqual(expected);
    expect(Object.isFrozen(binding?.admission)).toBe(true);
  });
  it("ignores legacy key material and does not rewrite the environment", async () => {
    const configured = { ...env, KARS_IDENTITY_SEED: "d".repeat(64), PINNED_AGENT_IDENTITY_APP_ID: "public-app-id" };
    const before = { ...configured };
    const binding = await initializeMissionBinding(configured);
    expect(binding?.identity.signingPrivateKey.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
    expect(configured).toEqual(before);
  });
});
