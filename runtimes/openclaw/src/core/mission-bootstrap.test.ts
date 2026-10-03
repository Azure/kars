// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { acquireMissionWriter, missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import { initializeMissionIdentity } from "./mission-bootstrap.js";

vi.mock("@kars/mesh/dist/mission-identity.js", async importOriginal => {
  const actual = await importOriginal<typeof import("@kars/mesh/dist/mission-identity.js")>();
  return { ...actual, acquireMissionWriter: vi.fn(), missionIdentity: vi.fn(actual.missionIdentity) };
});

const writerKey = Symbol.for("kars-mission-prekey-writer");
const state = process as typeof process & { [writerKey]?: ReturnType<typeof acquireMissionWriter> };
const env = {
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

describe("mission runtime identity bootstrap", () => {
  it("leaves disabled legacy initialization unchanged and acquires no writer", async () => {
    expect(await initializeMissionIdentity({})).toBeNull();
    expect(acquireMissionWriter).not.toHaveBeenCalled();
    expect(missionIdentity).not.toHaveBeenCalled();
  });
  it("waits for exclusive writer custody before deriving any key", async () => {
    let acquired!: (value: typeof writer) => void;
    vi.mocked(acquireMissionWriter).mockReturnValue(new Promise(resolve => { acquired = resolve; }));
    const pending = initializeMissionIdentity(env);
    expect(missionIdentity).not.toHaveBeenCalled();
    expect(state[writerKey]).toBeDefined();
    acquired(writer);
    const identity = await pending;
    expect(identity?.signingPrivateKey.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
    await expect(state[writerKey]).resolves.toBe(writer);
    expect(writer.close).not.toHaveBeenCalled();
  });
  it("prevents parallel contexts from starting a second acquisition", async () => {
    let acquired!: (value: typeof writer) => void;
    vi.mocked(acquireMissionWriter).mockReturnValue(new Promise(resolve => { acquired = resolve; }));
    const pending = initializeMissionIdentity(env);
    await expect(initializeMissionIdentity(env)).rejects.toThrow("already attempted");
    expect(acquireMissionWriter).toHaveBeenCalledTimes(1);
    acquired(writer);
    await pending;
  });
  it("rejects a lock collision before identity construction and forbids retries", async () => {
    vi.mocked(acquireMissionWriter).mockRejectedValue(new Error("EADDRINUSE"));
    await expect(initializeMissionIdentity(env)).rejects.toThrow("EADDRINUSE");
    expect(missionIdentity).not.toHaveBeenCalled();
    await expect(initializeMissionIdentity(env)).rejects.toThrow("already attempted");
    expect(acquireMissionWriter).toHaveBeenCalledTimes(1);
  });
  it("retains writer custody after binding validation fails", async () => {
    await expect(initializeMissionIdentity({ ...env, KARS_MISSION_TASK_UID: "" })).rejects.toThrow("complete controller-owned binding");
    await expect(state[writerKey]).resolves.toBe(writer);
    expect(writer.close).not.toHaveBeenCalled();
    await expect(initializeMissionIdentity(env)).rejects.toThrow("already attempted");
  });
  it("does not fall back to legacy seed or Entra identity when the root is missing", async () => {
    await expect(initializeMissionIdentity({ ...env, KARS_MISSION_IDENTITY_ROOT: "",
      KARS_IDENTITY_SEED: "d".repeat(64), PINNED_AGENT_IDENTITY_APP_ID: "public-app-id" })).rejects.toThrow("Secret root");
    expect(writer.close).not.toHaveBeenCalled();
  });
  it("captures the reviewed binding before waiting for the lock", async () => {
    let acquired!: (value: typeof writer) => void;
    vi.mocked(acquireMissionWriter).mockReturnValue(new Promise(resolve => { acquired = resolve; }));
    const mutable = { ...env };
    const pending = initializeMissionIdentity(mutable);
    mutable.KARS_MISSION_POD_UID = "other-pod";
    acquired(writer);
    const identity = await pending;
    expect(identity?.signingPrivateKey.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
  });
  it("ignores legacy key material and does not rewrite the environment", async () => {
    const configured = { ...env, KARS_IDENTITY_SEED: "d".repeat(64), PINNED_AGENT_IDENTITY_APP_ID: "public-app-id" };
    const before = { ...configured };
    const identity = await initializeMissionIdentity(configured);
    expect(identity?.signingPrivateKey.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
    expect(configured).toEqual(before);
  });
});
