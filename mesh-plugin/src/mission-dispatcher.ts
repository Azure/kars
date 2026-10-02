// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomUUID } from "node:crypto";
import type { IMeshTransport } from "./transport-interface.js";
import { parseMissionMessage, sameMissionTarget, type MissionAssignment, type MissionReply, type MissionTarget } from "./mission-protocol.js";

export interface MissionCandidate extends MissionTarget {
  namespace: string;
  agentName: string;
  content: string;
}
export interface MissionAttempt {
  version: 1;
  candidate: MissionCandidate;
  assignment: MissionAssignment;
  contentDigest: string;
  ownerSession: string;
  startedAt: string;
  updatedAt: string;
  phase: "dispatching" | "accepted" | "running" | "succeeded" | "failed" | "rejected" | "uncertain";
  events: Array<{ at: string; status: MissionReply["status"]; evidence?: MissionReply["evidence"] }>;
  reply?: MissionReply;
  error?: string;
}
export interface StoredMissionAttempt { revision: string; attempt: MissionAttempt }
export interface MissionAttemptStore {
  isCurrent(candidate: MissionCandidate): Promise<boolean>;
  get(candidate: MissionCandidate): Promise<StoredMissionAttempt | null>;
  create(attempt: MissionAttempt): Promise<StoredMissionAttempt | null>;
  replace(previous: StoredMissionAttempt, attempt: MissionAttempt): Promise<StoredMissionAttempt>;
  publish(attempt: MissionAttempt): Promise<boolean>;
  acknowledge(candidate: MissionCandidate): Promise<void>;
}
export type DispatchOutcome = "stale" | "already-claimed" | "succeeded" | "failed" | "rejected" | "uncertain";
export const missionContentDigest = (content: string): string => `sha256:${createHash("sha256").update(content).digest("hex")}`;
export const missionAttemptName = (target: Pick<MissionTarget, "taskUid" | "runNonce">): string =>
  `kars-mission-attempt-${createHash("sha256").update(JSON.stringify([target.taskUid, target.runNonce])).digest("hex").slice(0, 40)}`;
const terminal = (phase: MissionAttempt["phase"]): boolean => ["succeeded", "failed", "rejected"].includes(phase);

/** One durable claim precedes the only assignment send. An ambiguous send is never retried. */
export class MissionDispatcher {
  constructor(
    private readonly mesh: IMeshTransport,
    private readonly store: MissionAttemptStore,
    private readonly ownerSession: string,
    private readonly timeoutMs = 120_000,
    private readonly now = () => new Date().toISOString(),
  ) {
    if (!ownerSession || !Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) throw new Error("Invalid dispatcher session or timeout");
  }

  async dispatch(candidate: MissionCandidate): Promise<DispatchOutcome> {
    const { taskName, taskUid, sandboxUid, podUid, runNonce, agentDid, dispatcherDid, content } = candidate;
    const target: MissionTarget = { taskName, taskUid, sandboxUid, podUid, runNonce, agentDid, dispatcherDid };
    const probe = { ...target, type: "mission:probe" as const, version: 1 as const, challenge: randomUUID() };
    if (!parseMissionMessage(probe) || typeof content !== "string" || !content.trim()
      || typeof candidate.agentName !== "string" || !candidate.agentName.trim()
      || typeof candidate.namespace !== "string" || !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(candidate.namespace)
      || agentDid === dispatcherDid) {
      throw new Error("Invalid or unencrypted mission dispatch binding");
    }
    const existing = await this.store.get(candidate);
    if (existing) {
      if (terminal(existing.attempt.phase)) {
        // Recovery only publishes a previously persisted reply; it never resends an assignment.
        if (!await this.store.publish(existing.attempt)) return "stale";
      }
      return "already-claimed";
    }
    if (dispatcherDid !== this.mesh.currentDid || !this.mesh.isConnected || this.mesh.isPlaintextPeer(agentDid)) {
      throw new Error("Invalid or unencrypted mission dispatch binding");
    }
    if (!await this.store.isCurrent(candidate)) return "stale";
    const ready = await this.mesh.sendWithAck(agentDid, probe, (payload, from, security) => {
      const reply = parseMissionMessage(payload);
      return security === "encrypted" && from === agentDid && reply?.type === "mission:reply"
        && reply.status === "ready" && reply.challenge === probe.challenge && sameMissionTarget(target, reply) ? reply : null;
    }, { retries: 0, timeoutMs: Math.min(this.timeoutMs, 15_000) });
    const assignment: MissionAssignment = {
      ...target, type: "mission:assign", version: 1, bootId: ready.bootId, assignmentId: randomUUID(), content,
    };
    if (!parseMissionMessage(assignment)) throw new Error("Mission assignment exceeds protocol bounds");
    if (!await this.store.isCurrent(candidate)) return "stale";
    const at = this.now();
    let stored = await this.store.create({
      version: 1, candidate, assignment, contentDigest: missionContentDigest(content), ownerSession: this.ownerSession,
      phase: "dispatching", startedAt: at, updatedAt: at, events: [],
    });
    if (!stored) return "already-claimed";
    try {
      if (!await this.store.isCurrent(candidate)) throw new Error("Mission binding changed after the durable claim");
      if (!this.mesh.isConnected || this.mesh.currentDid !== dispatcherDid || this.mesh.isPlaintextPeer(agentDid)) {
        throw new Error("Encrypted transport binding changed after the durable claim");
      }
      const deadline = Date.now() + this.timeoutMs;
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await Promise.race([
          this.mesh.send(agentDid, assignment),
          new Promise<never>((_resolve, reject) => {
            timer = setTimeout(() => reject(new Error("Mission assignment send deadline exceeded")), this.timeoutMs);
          }),
        ]);
      } finally { clearTimeout(timer); }
      while (Date.now() < deadline) {
        const reply = await this.mesh.waitForMessage((payload, from, security) => {
          const message = parseMissionMessage(payload);
          return security === "encrypted" && from === agentDid && message?.type === "mission:reply"
            && message.status !== "ready" && sameMissionTarget(assignment, message)
            && message.bootId === assignment.bootId && message.assignmentId === assignment.assignmentId ? message : null;
        }, Math.max(1, deadline - Date.now()), { consume: true });
        const previous = stored.attempt;
        // A succeeded/failed reply is authoritative even when the earlier acceptance frame was lost.
        if (reply.status === "accepted" && previous.phase === "running") continue;
        if (reply.status === "rejected" && ["accepted", "running"].includes(previous.phase)) continue;
        if (reply.status === "running" && previous.reply?.evidence && reply.evidence
          && reply.evidence.rounds < previous.reply.evidence.rounds) continue;
        if (previous.events.length >= 128) throw new Error("Mission reply event limit exceeded");
        const receivedAt = this.now();
        stored = await this.store.replace(stored, {
          ...previous, phase: reply.status as MissionAttempt["phase"], reply, updatedAt: receivedAt,
          events: [...previous.events, { at: receivedAt, status: reply.status, ...(reply.evidence ? { evidence: reply.evidence } : {}) }],
        });
        if (terminal(stored.attempt.phase)) {
          if (!await this.store.publish(stored.attempt)) return "stale";
          return stored.attempt.phase as "succeeded" | "failed" | "rejected";
        }
        if (reply.status === "accepted" || reply.status === "running") await this.store.acknowledge(candidate);
      }
      throw new Error("Mission reply deadline exceeded");
    } catch (error) {
      // Publication failures leave terminal evidence intact for the next reconciliation.
      if (terminal(stored.attempt.phase)) throw error;
      const latest = await this.store.get(candidate);
      // A failed write may have committed, or another owner may have advanced it.
      // Never turn that newer evidence into uncertainty using a stale revision.
      if (!latest || latest.revision !== stored.revision) throw error;
      await this.store.replace(stored, {
        ...stored.attempt, phase: "uncertain", updatedAt: this.now(),
        error: error instanceof Error ? error.message.slice(0, 2048) : "Mission transport or storage failed",
      });
      return "uncertain";
    }
  }
}
