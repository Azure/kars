// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { validMissionArtifacts, type MissionArtifacts } from "@kars/mesh/dist/mission-protocol.js";
import type { TaskPhaseEvidence } from "./task-phase.js";

export interface TaskUsage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface TaskExecutionEvidence {
  model: string;
  rounds: number;
  usage: TaskUsage | null;
}

export class TaskExecutionError extends Error {
  readonly evidence: TaskExecutionEvidence;
  readonly phase?: TaskPhaseEvidence;
  constructor(message: string, evidence: TaskExecutionEvidence, phase?: TaskPhaseEvidence) {
    super(message);
    this.name = "TaskExecutionError";
    if (phase) this.phase = { ...phase };
    this.evidence = {
      model: evidence.model, rounds: evidence.rounds,
      usage: evidence.usage ? {
        promptTokens: evidence.usage.promptTokens, completionTokens: evidence.usage.completionTokens, totalTokens: evidence.usage.totalTokens,
      } : null,
    };
  }
}

/** Missing usage in any round makes the aggregate unknown, never zero. */
export class TaskCompletionLedger {
  private rounds = 0;
  private awaitingResponse = false;
  private usage: TaskUsage | null = { promptTokens: 0, completionTokens: 0, totalTokens: 0 };

  private artifacts: MissionArtifacts = {};

  constructor(private readonly model: string, readonly artifactsEnabled = false) {}

  prepareArtifact(name: unknown, content: unknown): MissionArtifacts {
    if (!this.artifactsEnabled) throw new Error("Dispatcher does not support named artifacts; return the full content inline");
    if (typeof name !== "string" || typeof content !== "string") throw new Error("Artifact name and content must be strings");
    const next = { ...this.artifacts, [name]: content };
    if (!validMissionArtifacts(next)) throw new Error("Artifact requires a safe, nonreserved filename, nonempty UTF-8 text, at most 16 files and 128 KiB serialized total");
    return next;
  }

  commitArtifacts(artifacts: MissionArtifacts): void {
    if (!this.artifactsEnabled || !validMissionArtifacts(artifacts)) throw new Error("Invalid or unnegotiated mission artifacts");
    this.artifacts = { ...artifacts };
  }

  artifactSnapshot(): { artifacts?: MissionArtifacts } {
    return Object.keys(this.artifacts).length ? { artifacts: { ...this.artifacts } } : {};
  }

  beginRequest(): void {
    this.awaitingResponse = true;
  }

  record(response: unknown): void {
    this.awaitingResponse = false;
    this.rounds++;
    const raw = (response as { usage?: Record<string, unknown> } | null)?.usage;
    const counts = [raw?.prompt_tokens, raw?.completion_tokens, raw?.total_tokens];
    if (!this.usage || !counts.every((n) => Number.isSafeInteger(n) && (n as number) >= 0)) {
      this.usage = null;
      return;
    }
    const [prompt, completion, total] = counts as number[];
    const next = {
      promptTokens: this.usage.promptTokens + prompt,
      completionTokens: this.usage.completionTokens + completion,
      totalTokens: this.usage.totalTokens + total,
    };
    if (prompt + completion !== total || !Object.values(next).every(Number.isSafeInteger)) {
      this.usage = null;
      return;
    }
    this.usage = next;
  }

  snapshot(): TaskExecutionEvidence {
    return { model: this.model, rounds: this.rounds, usage: this.rounds && !this.awaitingResponse && this.usage ? { ...this.usage } : null };
  }

  fail(message: string): never {
    throw new TaskExecutionError(message, this.snapshot());
  }

  finish(choice: { finish_reason?: unknown; message?: { content?: unknown } }): string {
    if (choice.finish_reason !== "stop") this.fail(`Incomplete model response (${String(choice.finish_reason)})`);
    const content = choice.message?.content;
    if (typeof content !== "string" || !content.trim()) this.fail("Model returned no final deliverable");
    if (!this.usage || this.usage.totalTokens === 0) this.fail("Provider usage is unavailable or invalid");
    return content;
  }
}
