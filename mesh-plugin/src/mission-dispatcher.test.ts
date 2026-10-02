// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MissionDispatcher, missionAttemptName, missionContentDigest, type MissionAttemptStore, type MissionCandidate, type StoredMissionAttempt } from "./mission-dispatcher.js";
import type { IMeshTransport, MessageSecurity } from "./transport-interface.js";
import type { MissionAssignment, MissionProbe, MissionReply } from "./mission-protocol.js";

const candidate: MissionCandidate = {
  namespace: "kars-system", taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid",
  runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`,
  agentName: "Briefing writer", content: "Write three benefits, three risks and an approval checklist.",
};
const evidence = { model: "gpt-5.4-mini", rounds: 1, usage: { promptTokens: 20, completionTokens: 30, totalTokens: 50 } };

function harness() {
  let stored: StoredMissionAttempt | null = null;
  let revision = 0;
  const store: MissionAttemptStore = {
    isCurrent: vi.fn(async () => true), get: vi.fn(async () => structuredClone(stored)),
    create: vi.fn(async attempt => {
      if (stored) return null;
      stored = { revision: String(++revision), attempt: structuredClone(attempt) };
      return structuredClone(stored);
    }),
    replace: vi.fn(async (previous, attempt) => {
      if (stored?.revision !== previous.revision) throw new Error("CAS conflict");
      stored = { revision: String(++revision), attempt: structuredClone(attempt) };
      return structuredClone(stored);
    }),
    acknowledge: vi.fn(async () => {}), publish: vi.fn(async () => true),
  };
  const inbox: Array<{ payload: unknown; from: string; security: MessageSecurity }> = [];
  let statuses: MissionReply["status"][] = ["accepted", "running", "succeeded"];
  let alter = (reply: MissionReply): MissionReply => reply;
  const mesh = {
    currentDid: candidate.dispatcherDid, isConnected: true, isPlaintextPeer: vi.fn(() => false),
    sendWithAck: vi.fn(async (_to, probe: MissionProbe, predicate) => {
      const ready = { ...probe, type: "mission:reply", bootId: "boot-1", status: "ready" };
      const match = predicate(ready, candidate.agentDid, "encrypted");
      if (!match) throw new Error("No matching ready reply");
      return match;
    }),
    send: vi.fn(async (_to, assignment: MissionAssignment) => {
      expect(stored?.attempt.phase).toBe("dispatching");
      expect(stored?.attempt.assignment).toEqual(assignment);
      for (const status of statuses) {
        const reply: MissionReply = { ...assignment, type: "mission:reply", status,
          ...(status === "succeeded" ? { output: "Three benefits, three risks, approval checklist.", evidence } : {}),
        };
        inbox.push({ payload: alter(reply), from: candidate.agentDid, security: "encrypted" });
      }
      return "message-id";
    }),
    waitForMessage: vi.fn(async predicate => {
      while (inbox.length) {
        const item = inbox.shift()!;
        const match = predicate(item.payload, item.from, item.security);
        if (match) return match;
      }
      throw new Error("Timed out waiting for a matching reply");
    }),
  } as unknown as IMeshTransport;
  return { store, mesh, inbox, state: () => stored, setStatuses: (next: typeof statuses) => { statuses = next; },
    alterReply: (f: typeof alter) => { alter = f; }, dispatcher: () => new MissionDispatcher(mesh, store, "dispatcher-process", 100),
  };
}

describe("durable encrypted mission dispatcher", () => {
  it("claims before sending once and persists actual output before publishing completion", async () => {
    const h = harness();
    expect(await h.dispatcher().dispatch(candidate)).toBe("succeeded");
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
    expect(h.mesh.sendWithAck).toHaveBeenCalledWith(candidate.agentDid, expect.objectContaining({ type: "mission:probe" }), expect.any(Function), { retries: 0, timeoutMs: 100 });
    expect(h.store.acknowledge).toHaveBeenCalledTimes(2);
    const actual = h.state()!.attempt;
    expect(actual.phase).toBe("succeeded");
    expect(actual.reply?.evidence).toEqual(evidence);
    expect(actual.events.map(e => e.status)).toEqual(["accepted", "running", "succeeded"]);
    expect(h.store.publish).toHaveBeenCalledWith(actual);
    expect(actual.contentDigest).toBe(missionContentDigest(candidate.content));
  });

  it("allows only the winning atomic claim to send under concurrent dispatch", async () => {
    const h = harness();
    const results = await Promise.all([h.dispatcher().dispatch(candidate), h.dispatcher().dispatch(candidate)]);
    expect(results.sort()).toEqual(["already-claimed", "succeeded"]);
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
  });

  it("never resends a run after a process restart, even with a changed Pod binding", async () => {
    const h = harness();
    vi.mocked(h.mesh.send).mockRejectedValueOnce(new Error("Connection lost after enqueue"));
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
    expect(h.state()!.attempt.phase).toBe("uncertain");
    expect(await h.dispatcher().dispatch({ ...candidate, podUid: "replacement-pod" })).toBe("already-claimed");
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
    expect(h.store.acknowledge).not.toHaveBeenCalled();
    expect(h.store.publish).not.toHaveBeenCalled();
  });

  it("does not acknowledge an assignment merely because it was sent", async () => {
    const h = harness(); h.setStatuses([]);
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
    expect(h.store.acknowledge).not.toHaveBeenCalled();
    expect(h.store.publish).not.toHaveBeenCalled();
  });

  it.each(["failed", "rejected"] as const)("persists a real %s reply without manufacturing output or usage", async status => {
    const h = harness(); h.setStatuses([status]);
    expect(await h.dispatcher().dispatch(candidate)).toBe(status);
    expect(h.state()!.attempt.reply?.output).toBeUndefined();
    expect(h.state()!.attempt.reply?.evidence).toBeUndefined();
  });

  it("recovers publication from a persisted terminal reply without another probe or assignment", async () => {
    const h = harness();
    vi.mocked(h.store.publish).mockRejectedValueOnce(new Error("Kubernetes unavailable"));
    await expect(h.dispatcher().dispatch(candidate)).rejects.toThrow("Kubernetes unavailable");
    expect(h.state()!.attempt.phase).toBe("succeeded");
    expect(await h.dispatcher().dispatch(candidate)).toBe("already-claimed");
    expect(h.store.publish).toHaveBeenCalledTimes(2);
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
    expect(h.mesh.sendWithAck).toHaveBeenCalledTimes(1);
  });

  it.each(["plaintext", "unknown"] as const)("does not accept %s readiness", async security => {
    const h = harness();
    vi.mocked(h.mesh.sendWithAck).mockImplementationOnce(async (_to, payload, predicate) => {
      const ready = { ...(payload as MissionProbe), type: "mission:reply", bootId: "boot-1", status: "ready" };
      expect(predicate(ready, candidate.agentDid, security)).toBeNull();
      throw new Error("No encrypted readiness");
    });
    await expect(h.dispatcher().dispatch(candidate)).rejects.toThrow("No encrypted readiness");
    expect(h.store.create).not.toHaveBeenCalled();
    expect(h.mesh.send).not.toHaveBeenCalled();
  });

  it.each(["podUid", "taskUid", "sandboxUid", "runNonce", "assignmentId", "bootId", "dispatcherDid", "agentDid"] as const)("rejects terminal replies with a different %s", async key => {
    const h = harness(); h.setStatuses(["succeeded"]);
    h.alterReply(reply => ({ ...reply, [key]: key.endsWith("Did") ? `did:mesh:${"c".repeat(32)}` : "different" }));
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
    expect(h.store.publish).not.toHaveBeenCalled();
  });

  it("rejects success without measured usage", async () => {
    const h = harness(); h.setStatuses(["succeeded"]);
    h.alterReply(reply => ({ ...reply, evidence: { ...evidence, usage: null } }));
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
  });

  it("accepts a valid final reply when the acceptance frame was lost", async () => {
    const h = harness(); h.setStatuses(["succeeded"]);
    expect(await h.dispatcher().dispatch(candidate)).toBe("succeeded");
    expect(h.store.acknowledge).not.toHaveBeenCalled();
  });

  it("ignores late acceptance after running", async () => {
    const h = harness(); h.setStatuses(["running", "accepted", "succeeded"]);
    expect(await h.dispatcher().dispatch(candidate)).toBe("succeeded");
    expect(h.state()!.attempt.events.map(e => e.status)).toEqual(["running", "succeeded"]);
  });

  it("refuses stale targets before probing and rechecks after probing", async () => {
    const h = harness(); vi.mocked(h.store.isCurrent).mockResolvedValueOnce(false);
    expect(await h.dispatcher().dispatch(candidate)).toBe("stale");
    expect(h.mesh.sendWithAck).not.toHaveBeenCalled();
    vi.mocked(h.store.isCurrent).mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    expect(await h.dispatcher().dispatch(candidate)).toBe("stale");
    expect(h.store.create).not.toHaveBeenCalled();
  });

  it("fences a target changed immediately after claiming without sending", async () => {
    const h = harness();
    vi.mocked(h.store.isCurrent).mockResolvedValueOnce(true).mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
    expect(h.mesh.send).not.toHaveBeenCalled();
  });

  it("refuses plaintext-configured peers and mismatched local identity", async () => {
    const h = harness(); vi.mocked(h.mesh.isPlaintextPeer).mockReturnValueOnce(true);
    await expect(h.dispatcher().dispatch(candidate)).rejects.toThrow("binding");
    await expect(h.dispatcher().dispatch({ ...candidate, dispatcherDid: candidate.agentDid })).rejects.toThrow("binding");
    expect(h.store.create).not.toHaveBeenCalled();
  });

  it("does not send if creating the claim fails", async () => {
    const h = harness(); vi.mocked(h.store.create).mockRejectedValueOnce(new Error("API unavailable"));
    await expect(h.dispatcher().dispatch(candidate)).rejects.toThrow("API unavailable");
    expect(h.mesh.send).not.toHaveBeenCalled();
  });

  it("reports stale publication both immediately and during terminal recovery", async () => {
    const h = harness(); vi.mocked(h.store.publish).mockResolvedValue(false);
    expect(await h.dispatcher().dispatch(candidate)).toBe("stale");
    expect(h.state()!.attempt.phase).toBe("succeeded");
    expect(await h.dispatcher().dispatch(candidate)).toBe("stale");
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
  });

  it("recovers terminal publication without an online or unchanged transport", async () => {
    const h = harness(); await h.dispatcher().dispatch(candidate);
    Object.assign(h.mesh, { isConnected: false, currentDid: "" });
    vi.mocked(h.mesh.isPlaintextPeer).mockReturnValue(true);
    expect(await h.dispatcher().dispatch(candidate)).toBe("already-claimed");
    expect(h.store.publish).toHaveBeenCalledTimes(2);
    expect(h.mesh.sendWithAck).toHaveBeenCalledTimes(1);
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
  });

  it("bounds a hung send and never retries even if it later resolves", async () => {
    const h = harness();
    let finish!: (id: string) => void;
    vi.mocked(h.mesh.send).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    vi.useFakeTimers();
    try {
      const dispatch = h.dispatcher().dispatch(candidate);
      await vi.advanceTimersByTimeAsync(101);
      expect(await dispatch).toBe("uncertain");
      expect(h.state()!.attempt.error).toContain("send deadline");
      finish("late-message");
      await Promise.resolve();
      expect(await h.dispatcher().dispatch(candidate)).toBe("already-claimed");
      expect(h.mesh.send).toHaveBeenCalledTimes(1);
      expect(h.mesh.waitForMessage).not.toHaveBeenCalled();
      expect(h.store.publish).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });

  it.each(["disconnected", "identity", "plaintext"] as const)("does not send after transport changes at claim: %s", async change => {
    const h = harness(); const create = vi.mocked(h.store.create).getMockImplementation()!;
    vi.mocked(h.store.create).mockImplementationOnce(async attempt => {
      const result = await create(attempt);
      if (change === "disconnected") Object.assign(h.mesh, { isConnected: false });
      if (change === "identity") Object.assign(h.mesh, { currentDid: candidate.agentDid });
      if (change === "plaintext") vi.mocked(h.mesh.isPlaintextPeer).mockReturnValue(true);
      return result;
    });
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
    expect(h.mesh.send).not.toHaveBeenCalled();
  });

  it("refuses a disconnected transport before probing", async () => {
    const h = harness(); Object.assign(h.mesh, { isConnected: false });
    await expect(h.dispatcher().dispatch(candidate)).rejects.toThrow("binding");
    expect(h.mesh.sendWithAck).not.toHaveBeenCalled();
    expect(h.store.create).not.toHaveBeenCalled();
  });

  it.each(["sender", "plaintext", "unknown"] as const)("requires actual encrypted terminal receipt, not payload claims: %s", async invalid => {
    const h = harness(); h.setStatuses(["succeeded"]); const send = vi.mocked(h.mesh.send).getMockImplementation()!;
    vi.mocked(h.mesh.send).mockImplementationOnce(async (...args) => {
      const id = await send(...args);
      if (invalid === "sender") h.inbox[0].from = candidate.dispatcherDid;
      else h.inbox[0].security = invalid;
      return id;
    });
    expect(await h.dispatcher().dispatch(candidate)).toBe("uncertain");
    expect(h.store.publish).not.toHaveBeenCalled();
  });

  it.each(["accepted", "running"] as const)("ignores rejection after %s rather than contradicting execution evidence", async status => {
    const h = harness(); h.setStatuses([status, "rejected", "succeeded"]);
    expect(await h.dispatcher().dispatch(candidate)).toBe("succeeded");
    expect(h.state()!.attempt.events.map(e => e.status)).toEqual([status, "succeeded"]);
  });

  it.each(["accepted", "succeeded"] as const)("preserves a committed %s write when the API response is lost", async status => {
    const h = harness(); h.setStatuses([status]); const replace = vi.mocked(h.store.replace).getMockImplementation()!;
    vi.mocked(h.store.replace).mockImplementationOnce(async (previous, attempt) => {
      await replace(previous, attempt);
      throw new Error("Response lost after commit");
    });
    await expect(h.dispatcher().dispatch(candidate)).rejects.toThrow("Response lost");
    expect(h.state()!.attempt.phase).toBe(status);
    expect(h.store.replace).toHaveBeenCalledTimes(1);
    expect(await h.dispatcher().dispatch(candidate)).toBe("already-claimed");
    expect(h.mesh.send).toHaveBeenCalledTimes(1);
  });

  it("uses a bounded, collision-resistant name including the Task UID", () => {
    expect(missionAttemptName(candidate)).toMatch(/^kars-mission-attempt-[a-f0-9]{40}$/);
    expect(missionAttemptName(candidate)).not.toBe(missionAttemptName({ ...candidate, taskUid: "new-task" }));
    expect(missionAttemptName(candidate)).not.toBe(missionAttemptName({ ...candidate, runNonce: "run-2" }));
  });
});
