// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomUUID } from "node:crypto";
import { isMissionMessage, missionContract, missionEvidenceAdvances, parseMissionContract, parseMissionMessage, sameMissionContract, sameMissionTarget, snapshotMissionData, validMissionArtifacts, type MissionAssignment, type MissionContract, type MissionReply, type MissionTarget } from "@kars/mesh/dist/mission-protocol.js";
import type { FilesystemPhase } from "@kars/mesh/dist/mission-phase.js";
import type { MissionInput } from "@kars/mesh/dist/mission-inputs.js";
import { missionAdmissionAllows, parseMissionAdmission, type MissionAdmission } from "@kars/mesh/dist/mission-admission.js";
import type { MessageSecurity } from "@kars/mesh/dist/transport-interface.js";
import { missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import { TaskExecutionError, type TaskExecutionEvidence } from "./task-completion.js";

export interface MissionReceiverOptions {
  target: Omit<MissionTarget, "runNonce">;
  expectedContract?: MissionContract;
  admission: MissionAdmission;
  authorize: (assignment: MissionAssignment) => Promise<boolean>;
  execute: (content: string, progress: (evidence: TaskExecutionEvidence) => void, artifactsEnabled: boolean, reviewedPhase?: FilesystemPhase, inputArtifacts?: readonly MissionInput[]) => Promise<TaskExecutionEvidence & Pick<MissionReply, "artifacts"> & { output: string }>;
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
  return Object.freeze(target);
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
  private readonly options: MissionReceiverOptions & { expectedContract: MissionContract };
  constructor(options: MissionReceiverOptions) {
    const targetKeys = Reflect.ownKeys(options.target);
    const target = snapshotMissionData(options.target) as MissionReceiverOptions["target"];
    const expectedContract = parseMissionContract(options.expectedContract === undefined ? missionContract() : options.expectedContract);
    if (!expectedContract) throw new Error("Invalid installed mission contract");
    this.bootId = options.bootId ?? randomUUID();
    if (!target || targetKeys.length !== 6 || Object.keys(target).sort().join(",") !== "agentDid,dispatcherDid,podUid,sandboxUid,taskName,taskUid"
      || !parseMissionMessage({ ...target, ...expectedContract, type: "mission:reply", status: "ready",
        challenge: "validation", runNonce: "validation", bootId: this.bootId })) {
      throw new Error("Invalid installed mission target or boot identity");
    }
    const admission = parseMissionAdmission(options.admission);
    if (!admission) throw new Error("Invalid installed mission admission");
    this.options = Object.freeze({ ...options, target, expectedContract, admission });
  }

  async handle(from: string, value: unknown, security: MessageSecurity): Promise<boolean> {
    if (!isMissionMessage(value)) return false;
    const message = parseMissionMessage(value);
    if (!message || security !== "encrypted" || from !== this.options.target.dispatcherDid
      || !sameMissionTarget(message, { ...this.options.target, runNonce: message.runNonce })
      || !sameMissionContract(message, this.options.expectedContract)
      || !missionAdmissionAllows(this.options.admission, message.runNonce,
        message.type === "mission:assign" ? message.content : undefined)) return true;
    if (message.type === "mission:probe") {
      const ready = parseMissionMessage({
        type: "mission:reply", status: "ready", version: message.version,
        taskName: message.taskName, taskUid: message.taskUid, sandboxUid: message.sandboxUid,
        podUid: message.podUid, runNonce: message.runNonce, agentDid: message.agentDid,
        dispatcherDid: message.dispatcherDid, challenge: message.challenge, bootId: this.bootId,
        ...(message.reviewedPhase ? { reviewedPhase: message.reviewedPhase, phaseDigest: message.phaseDigest } : {}),
        ...(message.inputDigest ? { inputDigest: message.inputDigest } : {}),
      });
      if (ready?.type === "mission:reply") await this.send(ready);
      else this.options.warn("Invalid bounded mission readiness reply");
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
    return {
      type: "mission:reply", status, version: assignment.version,
      taskName: assignment.taskName, taskUid: assignment.taskUid, sandboxUid: assignment.sandboxUid,
      podUid: assignment.podUid, runNonce: assignment.runNonce, agentDid: assignment.agentDid,
      dispatcherDid: assignment.dispatcherDid, bootId: assignment.bootId, assignmentId: assignment.assignmentId,
      ...(assignment.artifactFormat ? { artifactFormat: assignment.artifactFormat } : {}),
      ...(assignment.reviewedPhase ? { reviewedPhase: assignment.reviewedPhase, phaseDigest: assignment.phaseDigest } : {}),
      ...(assignment.inputDigest ? { inputDigest: assignment.inputDigest } : {}),
      ...extra,
    };
  }

  private async send(reply: MissionReply): Promise<void> {
    try { await this.options.send(this.options.target.dispatcherDid, reply); }
    catch { this.options.warn("Encrypted mission handback unavailable; retained for correlated retry"); }
  }

  private async run(execution: Execution): Promise<void> {
    let progressSends = Promise.resolve();
    let latest: MissionReply["evidence"];
    let progressCount = 0;
    let finished = false;
    let progressError: Error | undefined;
    const validateEvidence = (evidence: TaskExecutionEvidence, status: "running" | "failed"): MissionReply => {
      const reply = parseMissionMessage(this.reply(execution.assignment, status, { evidence }));
      if (!reply || reply.type !== "mission:reply"
        || (execution.assignment.version !== 1 && !missionEvidenceAdvances(latest, reply.evidence))) {
        throw new Error("Invalid or regressing mission execution evidence");
      }
      latest = reply.evidence;
      return reply;
    };
    try {
      if (!await this.options.authorize(execution.assignment)) throw new Error("Mission policy denied or evaluation unavailable");
      execution.reply = this.reply(execution.assignment, "accepted");
      await this.send(execution.reply);
      const result = await this.options.execute(execution.assignment.content, (evidence) => {
        try {
          if (finished || progressError || progressCount >= 126) throw new Error("Mission progress is closed or exceeds the journal bound");
          const progress = validateEvidence(evidence, "running");
          progressCount++;
          execution.reply = progress;
          progressSends = progressSends.then(() => this.send(progress));
        } catch (error) {
          progressError = error instanceof Error ? error : new Error("Invalid mission progress");
          throw progressError;
        }
      }, execution.assignment.artifactFormat === "text-v1", this.options.expectedContract.reviewedPhase, execution.assignment.inputArtifacts);
      finished = true;
      if (progressError) throw progressError;
      const descriptors = Object.getOwnPropertyDescriptors(result);
      const measured: Record<string, unknown> = {};
      for (const key of ["model", "rounds", "usage", "phase"] as const) {
        const field = descriptors[key];
        if (field && (!field.enumerable || !("value" in field))) throw new Error("Invalid mission evidence field");
        if (field) measured[key] = field.value;
      }
      validateEvidence(measured as unknown as TaskExecutionEvidence, "failed");
      if (result.artifacts !== undefined && (execution.assignment.artifactFormat !== "text-v1" || !validMissionArtifacts(result.artifacts))) {
        throw new TaskExecutionError("Invalid or unnegotiated mission artifacts", latest!);
      }
      const reply = parseMissionMessage(this.reply(execution.assignment, "succeeded", {
        output: result.output, ...(result.artifacts ? { artifacts: result.artifacts } : {}), evidence: latest,
      }));
      if (!reply || reply.type !== "mission:reply") throw new TaskExecutionError("Final deliverable exceeds the mission envelope or lacks valid evidence", latest!);
      execution.reply = reply;
    } catch (error) {
      let measuredFailure = false;
      if (error instanceof TaskExecutionError) {
        try { validateEvidence(error.evidence, "failed"); measuredFailure = true; }
        catch { /* Retain the last validated counters, not malformed terminal evidence. */ }
      }
      // An unmeasured failure cannot certify that no further inference consumed tokens.
      if (!measuredFailure && latest) latest = { ...latest, usage: null };
      const failed = parseMissionMessage(this.reply(execution.assignment, "failed", {
        error: error instanceof Error ? error.message.slice(0, 2048) : "Mission execution failed",
        ...(latest ? { evidence: latest } : {}),
      }));
      if (!failed || failed.type !== "mission:reply") throw new Error("Invalid bounded mission failure");
      execution.reply = failed;
    } finally {
      finished = true;
      await progressSends;
      this.active = null;
    }
    if (execution.reply) await this.send(execution.reply);
  }
}
