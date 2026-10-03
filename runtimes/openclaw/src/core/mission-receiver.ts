// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomUUID } from "node:crypto";
import { isMissionMessage, parseMissionMessage, sameMissionTarget, validMissionArtifacts, type MissionAssignment, type MissionReply, type MissionTarget } from "@kars/mesh/dist/mission-protocol.js";
import type { MessageSecurity } from "@kars/mesh/dist/transport-interface.js";
import { missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import { TaskExecutionError, type TaskExecutionEvidence } from "./task-completion.js";

export interface MissionReceiverOptions {
  target: Omit<MissionTarget, "runNonce">;
  authorize: (assignment: MissionAssignment) => Promise<boolean>;
  execute: (content: string, progress: (evidence: TaskExecutionEvidence) => void, artifactsEnabled: boolean) => Promise<TaskExecutionEvidence & Pick<MissionReply, "artifacts"> & { output: string }>;
  send: (to: string, reply: MissionReply) => Promise<unknown>;
  warn: (message: string) => void;
  bootId?: string;
}
export function missionTargetFromEnvironment(agentDid: string, env = process.env): Omit<MissionTarget, "runNonce"> | null {
  if (env.KARS_MISSION_DISPATCH_ENABLED !== "true") return null;
  const target = {
    taskName: env.KARS_MISSION_TASK_NAME!, taskUid: env.KARS_MISSION_TASK_UID!,
    sandboxUid: env.KARS_MISSION_SANDBOX_UID!, podUid: env.KARS_MISSION_POD_UID!,
    dispatcherDid: env.KARS_MISSION_DISPATCHER_DID!, agentDid,
  };
  if (!/^[a-fA-F0-9]{64}$/.test(env.KARS_MISSION_IDENTITY_ROOT ?? "") || !parseMissionMessage({
    ...target, type: "mission:probe", version: 1, challenge: "validation", runNonce: "validation",
  })) throw new Error("Mission dispatch requires a complete controller-owned binding and Secret-backed identity root");
  if (missionIdentity(env.KARS_MISSION_IDENTITY_ROOT!, "runtime", target.sandboxUid, target.podUid).did !== agentDid) {
    throw new Error("Mission identity does not match its controller-owned Sandbox and Pod binding");
  }
  return target;
}

interface Execution {
  digest: string;
  assignment: MissionAssignment;
  reply?: MissionReply;
}

/** One receiver per existing mesh singleton; it never creates another SDK client. */
export class MissionReceiver {
  private readonly bootId: string;
  private readonly executions = new Map<string, Execution>();
  private active: string | null = null;
  constructor(private readonly options: MissionReceiverOptions) {
    this.bootId = options.bootId ?? randomUUID();
  }

  async handle(from: string, value: unknown, security: MessageSecurity): Promise<boolean> {
    if (!isMissionMessage(value)) return false;
    const message = parseMissionMessage(value);
    if (!message || security !== "encrypted" || from !== this.options.target.dispatcherDid
      || !sameMissionTarget(message, { ...this.options.target, runNonce: message.runNonce })) return true;
    if (message.type === "mission:probe") {
      const { type: _type, ...target } = message;
      await this.send({ ...target, type: "mission:reply", bootId: this.bootId, status: "ready" });
      return true;
    }
    if (message.type !== "mission:assign" || message.bootId !== this.bootId) return true;
    const digest = createHash("sha256").update(JSON.stringify(message)).digest("hex");
    const previous = this.executions.get(message.runNonce);
    if (previous) {
      if (previous.digest === digest && previous.reply) await this.send(previous.reply);
      // A mutated assignment must not overwrite or terminate the original run.
      return true;
    }
    if (this.active || this.executions.size >= 128) {
      await this.send(this.reply(message, "rejected", { error: this.active ? "Runtime has an active mission" : "Runtime replay ledger is full; no execution started" }));
      return true;
    }
    this.active = message.runNonce;
    const execution = { digest, assignment: message };
    this.executions.set(message.runNonce, execution);
    void this.run(execution).catch(() => this.options.warn("Mission receiver failed before handback"));
    return true;
  }

  private reply(assignment: MissionAssignment, status: MissionReply["status"], extra: Partial<MissionReply> = {}): MissionReply {
    const { content: _content, type: _type, ...binding } = assignment;
    return { ...binding, type: "mission:reply", status, ...extra };
  }

  private async send(reply: MissionReply): Promise<void> {
    try { await this.options.send(this.options.target.dispatcherDid, reply); }
    catch { this.options.warn("Encrypted mission handback unavailable; retained for correlated retry"); }
  }

  private async run(execution: Execution): Promise<void> {
    let progressSends = Promise.resolve();
    try {
      if (!await this.options.authorize(execution.assignment)) throw new Error("Mission policy denied or evaluation unavailable");
      execution.reply = this.reply(execution.assignment, "accepted");
      await this.send(execution.reply);
      const result = await this.options.execute(execution.assignment.content, (evidence) => {
        execution.reply = this.reply(execution.assignment, "running", { evidence });
        const progress = execution.reply;
        progressSends = progressSends.then(() => this.send(progress));
      }, execution.assignment.artifactFormat === "text-v1");
      if (result.artifacts !== undefined && (execution.assignment.artifactFormat !== "text-v1" || !validMissionArtifacts(result.artifacts))) {
        throw new TaskExecutionError("Invalid or unnegotiated mission artifacts", result);
      }
      const reply = this.reply(execution.assignment, "succeeded", {
        output: result.output, ...(result.artifacts ? { artifacts: { ...result.artifacts } } : {}),
        evidence: { model: result.model, rounds: result.rounds, usage: result.usage },
      });
      if (!parseMissionMessage(reply)) throw new TaskExecutionError("Final deliverable exceeds the mission envelope or lacks valid evidence", result);
      execution.reply = reply;
    } catch (error) {
      execution.reply = this.reply(execution.assignment, "failed", {
        error: error instanceof Error ? error.message.slice(0, 2048) : "Mission execution failed",
        ...(error instanceof TaskExecutionError ? { evidence: error.evidence } : {}),
      });
    } finally {
      await progressSends;
      this.active = null;
    }
    if (execution.reply) await this.send(execution.reply);
  }
}
