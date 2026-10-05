// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";
import { missionAttemptName, missionContentDigest, type MissionAttempt, type MissionCandidate, type StoredMissionAttempt } from "./mission-dispatcher.js";
import { missionContract, missionEvidenceAdvances, parseMissionMessage, sameMissionContract, sameMissionTarget, type MissionReply } from "./mission-protocol.js";
import { singleControllerOwner, type MissionMetadata } from "./mission-workload.js";

export interface MissionConfigMap {
  apiVersion: "v1"; kind: "ConfigMap"; metadata: MissionMetadata; data: Record<string, string>; immutable?: boolean;
}
export type MissionRecordIdentity = Pick<MissionCandidate, "namespace" | "taskName" | "taskUid" | "runNonce">;
export const missionRecordOwner = (candidate: Pick<MissionCandidate, "taskName" | "taskUid">) =>
  ({ apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: candidate.taskName, uid: candidate.taskUid, controller: true });
export const missionRecordOwned = (metadata: MissionMetadata, candidate: Pick<MissionCandidate, "taskName" | "taskUid">): boolean =>
  !metadata.deletionTimestamp && singleControllerOwner(metadata, "kars.azure.com/v1alpha1", "KarsTask", candidate.taskName, candidate.taskUid);

export function unpackMissionAttempt(cm: MissionConfigMap, candidate: MissionRecordIdentity): StoredMissionAttempt {
  if (!missionRecordOwned(cm.metadata, candidate) || !cm.metadata.resourceVersion || cm.metadata.namespace !== candidate.namespace
    || cm.metadata.name !== missionAttemptName(candidate)) throw new Error("Mission attempt ownership or revision mismatch");
  if (!cm.data?.["attempt.json"] || Buffer.byteLength(cm.data["attempt.json"]) > 900 * 1024) throw new Error("Mission attempt exceeds storage bounds");
  const attempt = JSON.parse(cm.data["attempt.json"]) as MissionAttempt;
  if (!attempt || attempt.version !== 1 || attempt.candidate?.taskUid !== candidate.taskUid || attempt.candidate?.runNonce !== candidate.runNonce
    || attempt.candidate?.taskName !== candidate.taskName || attempt.candidate?.namespace !== candidate.namespace
    || parseMissionMessage(attempt.assignment)?.type !== "mission:assign"
    || !sameMissionTarget(attempt.candidate, attempt.assignment)
    || !sameMissionContract(missionContract(attempt.candidate.reviewedPhase, attempt.candidate.inputArtifacts), attempt.assignment)
    || attempt.contentDigest !== missionContentDigest(attempt.candidate.content)
    || attempt.assignment.content !== attempt.candidate.content || !Array.isArray(attempt.events) || attempt.events.length > 128
    || !["dispatching", "accepted", "running", "succeeded", "failed", "rejected", "uncertain"].includes(attempt.phase)
    || typeof attempt.candidate.agentName !== "string" || !attempt.candidate.agentName.trim() || Buffer.byteLength(attempt.candidate.agentName) > 512
    || typeof attempt.ownerSession !== "string" || !attempt.ownerSession || attempt.ownerSession.length > 253
    || typeof attempt.startedAt !== "string" || typeof attempt.updatedAt !== "string"
    || !Number.isFinite(Date.parse(attempt.startedAt)) || !Number.isFinite(Date.parse(attempt.updatedAt))
    || Date.parse(attempt.updatedAt) < Date.parse(attempt.startedAt)
    || (attempt.error !== undefined && (typeof attempt.error !== "string" || attempt.error.length > 2048))
    || (attempt.phase === "dispatching" && (attempt.events.length !== 0 || attempt.reply !== undefined || attempt.error !== undefined))
    || (attempt.phase === "uncertain" && (!attempt.error || (attempt.reply && !["accepted", "running"].includes(attempt.reply.status))))
    || (attempt.reply && (parseMissionMessage(attempt.reply)?.type !== "mission:reply"
      || !sameMissionTarget(attempt.assignment, attempt.reply) || !sameMissionContract(attempt.assignment, attempt.reply)
      || attempt.reply.bootId !== attempt.assignment.bootId
      || attempt.reply.assignmentId !== attempt.assignment.assignmentId
      || (attempt.reply.artifactFormat !== undefined && attempt.reply.artifactFormat !== attempt.assignment.artifactFormat)))
    || (["accepted", "running", "succeeded", "failed", "rejected"].includes(attempt.phase) && attempt.reply?.status !== attempt.phase)) {
    throw new Error("Malformed durable mission attempt");
  }
  let lastTime = Date.parse(attempt.startedAt);
  let lastStatus: string | undefined;
  let lastEvidence: MissionReply["evidence"];
  for (const event of attempt.events) {
    if (!event || typeof event.at !== "string" || !Number.isFinite(Date.parse(event.at))
      || Date.parse(event.at) < lastTime || Date.parse(event.at) > Date.parse(attempt.updatedAt)
      || !["accepted", "running", "succeeded", "failed", "rejected"].includes(event.status)
      || (lastStatus === "running" && event.status === "accepted")
      || (["accepted", "running"].includes(lastStatus ?? "") && event.status === "rejected")
      || ["succeeded", "failed", "rejected"].includes(lastStatus ?? "")
      || (attempt.assignment.version !== 1 && !missionEvidenceAdvances(lastEvidence, event.evidence))
      || !parseMissionMessage({ ...attempt.assignment, inputArtifacts: undefined, type: "mission:reply", status: event.status,
        evidence: event.evidence, ...(event.status === "succeeded" ? { output: attempt.reply?.output } : {}) })) {
      throw new Error("Malformed mission event journal");
    }
    lastTime = Date.parse(event.at);
    lastStatus = event.status;
    lastEvidence = event.evidence;
  }
  if ((attempt.reply?.status !== lastStatus) || !isDeepStrictEqual(attempt.reply?.evidence, attempt.events.at(-1)?.evidence)) {
    throw new Error("Mission reply does not match event journal");
  }
  return { revision: cm.metadata.resourceVersion, attempt };
}

/** One projection contract shared by publication and retained-result readers. */
export function missionResultMaps(attempt: MissionAttempt, role: "canonical" | "current" = "canonical"):
  { output: MissionConfigMap; artifacts: MissionConfigMap } {
  const { candidate, reply, assignment } = attempt;
  if (!reply || !["succeeded", "failed", "rejected"].includes(attempt.phase) || reply.status !== attempt.phase
    || parseMissionMessage(reply)?.type !== "mission:reply" || !sameMissionTarget(assignment, reply) || !sameMissionContract(assignment, reply)
    || assignment.assignmentId !== reply.assignmentId || assignment.bootId !== reply.bootId
    || (reply.artifactFormat !== undefined && reply.artifactFormat !== assignment.artifactFormat)) throw new Error("Mission has no valid terminal reply");
  const prefix = "kars.azure.com/";
  const key = missionAttemptName(candidate).replace("kars-mission-attempt-", "");
  const suffix = role === "canonical" ? key : candidate.taskName;
  const metadata = (label: string): MissionMetadata => ({
    name: `kars-${label}-${suffix}`, namespace: candidate.namespace, ownerReferences: [missionRecordOwner(candidate)],
    labels: { [`${prefix}${label}`]: key },
    annotations: { [`${prefix}mission-evidence-key`]: key, [`${prefix}mission-evidence-role`]: role,
      [`${prefix}mission-principal-name`]: candidate.taskName, [`${prefix}mission-task-uid`]: candidate.taskUid,
      [`${prefix}mission-run-nonce`]: candidate.runNonce },
  });
  const success = reply.status === "succeeded";
  const artifacts: Record<string, string> = success ? { "response.md": reply.output!, ...reply.artifacts } : {};
  const data: Record<string, string> = {
    taskName: candidate.taskName, taskUid: candidate.taskUid, assignmentNonce: candidate.runNonce,
    assignmentId: assignment.assignmentId, agentName: candidate.agentName, agentDid: candidate.agentDid,
    dispatcherDid: candidate.dispatcherDid, sandboxUid: candidate.sandboxUid, podUid: candidate.podUid,
    runtimeBootId: assignment.bootId, status: success ? "ok" : reply.status, output: success ? reply.output! : "",
    error: reply.error ?? "", startedAt: attempt.startedAt, finishedAt: attempt.updatedAt,
    model: reply.evidence?.model ?? "",
    ...(reply.evidence?.usage ? { totalTokens: String(reply.evidence.usage.totalTokens) } : {}),
    usageKnown: String(reply.evidence?.usage !== null && reply.evidence?.usage !== undefined),
    artifactCount: String(Object.keys(artifacts).length), "evidence.json": JSON.stringify(reply),
  };
  return {
    output: { apiVersion: "v1", kind: "ConfigMap", metadata: metadata("mission-output"), data },
    artifacts: { apiVersion: "v1", kind: "ConfigMap", metadata: metadata("mission-artifacts"), data: artifacts },
  };
}
