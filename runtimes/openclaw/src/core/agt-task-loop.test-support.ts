// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Retain accounting/compatibility regressions for legacy task_request, resume and
// offload. Production Missions must use native-mission, never this test harness.
import { processTaskWithTools, type TaskLoopDeps } from "./agt-task-loop.js";
import { TaskCompletionLedger, TaskExecutionError, type TaskExecutionEvidence } from "./task-completion.js";
import { ReturnedRouterResponses } from "./router-observations.js";
import { TaskPhaseGuard, type TaskPhaseEvidence } from "./task-phase.js";
import { TaskInputFiles, TaskSkillFiles } from "./task-filesystem.js";
import type { MissionArtifacts } from "@kars/mesh/dist/mission-protocol.js";

type Logger = { info: (m: string) => void; warn: (m: string) => void };

export async function executeLegacyTaskForTest(
  taskContent: unknown, deps: TaskLoopDeps, log: Logger, artifactsEnabled = false,
  reviewedPhase?: unknown, inputArtifacts?: unknown, requiredOutputs?: readonly string[],
): Promise<TaskExecutionEvidence & { output: string; artifacts?: MissionArtifacts; phase?: TaskPhaseEvidence }> {
  const model = process.env.OPENCLAW_MODEL || process.env.KARS_MODEL || process.env.MODEL || "gpt-4.1";
  const ledger = new TaskCompletionLedger(model, artifactsEnabled, requiredOutputs);
  let phase: TaskPhaseGuard | undefined;
  try {
    phase = reviewedPhase === undefined ? undefined : new TaskPhaseGuard(reviewedPhase);
    if (ledger.requiredOutputs.length && !phase?.allowsTool("file_write")) {
      ledger.fail("Required named outputs need a reviewed filesystem-write phase with a positive tool-call ceiling");
    }
    if (inputArtifacts !== undefined && !phase?.allowsTool("file_read")) {
      ledger.fail("Immutable inputs require a reviewed filesystem-read phase with a positive tool-call ceiling");
    }
    const inputs = inputArtifacts === undefined ? undefined : new TaskInputFiles(inputArtifacts);
    const skills = TaskSkillFiles.load();
    if (skills?.hasFiles && !phase?.allowsTool("file_read")) {
      ledger.fail("Selected skill packages require a reviewed filesystem-read phase with a positive tool-call ceiling");
    }
    const observations = artifactsEnabled || deps.liveActivityEnabled ? new ReturnedRouterResponses() : undefined;
    const output = await processTaskWithTools(taskContent, deps, log, ledger, phase, observations, inputs, skills);
    const observationArtifact = artifactsEnabled ? await observations?.artifact() : undefined;
    if (observationArtifact && !ledger.attachRouterObservations(observationArtifact)) {
      log.warn("Router observations omitted: negotiated artifact capacity was used by deliverables");
    }
    return { ...ledger.snapshot(), ...ledger.artifactSnapshot(), output, ...(phase ? { phase: phase.snapshot() } : {}) };
  } catch (error) {
    if (error instanceof TaskExecutionError && !phase) throw error;
    throw new TaskExecutionError(error instanceof Error ? error.message : "Task execution failed", ledger.snapshot(), phase?.snapshot());
  }
}
