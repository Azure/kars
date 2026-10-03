// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { missionKnockHandler } from "./mission-admission.js";
const target = {
  taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid",
  agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`,
};
describe("controller-bound mission session admission", () => {
  it("admits only the exact dispatcher without a registry/reputation dependency", async () => {
    const fallback = vi.fn(async () => { throw new Error("registry unavailable"); });
    expect(await missionKnockHandler(target, fallback)(target.dispatcherDid, {})).toEqual({ accept: true });
    expect(fallback).not.toHaveBeenCalled();
  });
  it.each([target.agentDid, "did:mesh:", `${target.dispatcherDid}x`, target.dispatcherDid.toUpperCase()])("preserves general policy for %s", async from => {
    const fallback = vi.fn(async () => ({ accept: false })); const request = { intent: { capability: "task:execute" } };
    expect(await missionKnockHandler(target, fallback)(from, request)).toEqual({ accept: false });
    expect(fallback).toHaveBeenCalledExactlyOnceWith(from, request);
  });
  it("does not create dispatch authority when mission delivery is disabled", async () => {
    const fallback = vi.fn(async () => ({ accept: false }));
    expect(await missionKnockHandler(null, fallback)(target.dispatcherDid, {})).toEqual({ accept: false });
  });
  it("does not turn general policy failures into admission", async () => {
    const fallback = vi.fn(async () => { throw new Error("policy unavailable"); });
    await expect(missionKnockHandler(target, fallback)(target.agentDid, {})).rejects.toThrow("policy unavailable");
  });
});
