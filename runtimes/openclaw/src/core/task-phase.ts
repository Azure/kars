// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { resolve } from "node:path";

import { filesystemPhase, type FilesystemPhase, type TaskPhaseEvidence } from "@kars/mesh/dist/mission-phase.js";
export type { TaskPhaseEvidence } from "@kars/mesh/dist/mission-phase.js";

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    && [Object.prototype, null].includes(Object.getPrototypeOf(value));
}

/** A local, single-phase ceiling; it does not negotiate or authorize a mission plan. */
export class TaskPhaseGuard {
  private readonly phase: FilesystemPhase;
  private attempts = 0;
  private successes = 0;

  constructor(input: unknown) {
    this.phase = filesystemPhase(input);
  }

  allowsTool(name: string): boolean {
    if (this.phase.maxToolCalls === 0) return false;
    return (name === "file_read" && this.phase.capabilities.includes("filesystem-read"))
      || (name === "file_write" && this.phase.capabilities.includes("filesystem-write"));
  }

  instructions(): string {
    return `Reviewed phase: ${JSON.stringify(this.phase)}. Every requested tool call, including malformed, denied and failed calls, consumes the attempt ceiling. A batch exceeding the remaining ceiling fails before any call runs. Only successful authorized filesystem operations count toward minToolCalls; a final response before that minimum fails. No other tools or indirect effects are supported.`;
  }

  reserveBatch(count: number): void {
    if (!Number.isSafeInteger(count) || count < 1 || count > this.phase.maxToolCalls - this.attempts) {
      throw new Error(`Reviewed phase ${this.phase.name} exceeds maxToolCalls (${this.phase.maxToolCalls})`);
    }
    this.attempts += count;
  }

  argumentError(name: string, args: unknown): string | undefined {
    if (!this.allowsTool(name)) return "Tool is outside the reviewed phase capabilities";
    const keys = name === "file_write" ? ["path", "content", "artifact_name"] : ["path", "max_bytes"];
    if (!record(args) || Object.keys(args).some(key => !keys.includes(key)) || typeof args.path !== "string") {
      return "Reviewed filesystem calls require a path and only declared arguments";
    }
    const path = resolve(args.path);
    if (!args.path.startsWith("/") || (!path.startsWith("/sandbox/") && !path.startsWith("/tmp/"))) {
      return "Reviewed filesystem path must resolve under /sandbox/ or /tmp/";
    }
    if (name === "file_write" && (typeof args.content !== "string"
      || (args.artifact_name != null && typeof args.artifact_name !== "string"))) {
      return "Reviewed file_write requires string content and an optional string artifact_name";
    }
    if (name === "file_read" && args.max_bytes != null && (!Number.isSafeInteger(args.max_bytes)
      || (args.max_bytes as number) < 1 || (args.max_bytes as number) > 16 * 1024 * 1024)) {
      return "Reviewed file_read max_bytes must be an integer between 1 and 16777216";
    }
    return undefined;
  }

  recordSuccess(): void {
    if (this.successes >= this.attempts) throw new Error("Reviewed phase success has no reserved attempt");
    this.successes++;
  }

  finish(): void {
    if (this.successes < this.phase.minToolCalls) {
      throw new Error(`Reviewed phase ${this.phase.name} requires ${this.phase.minToolCalls} successful tool calls; completed ${this.successes}`);
    }
  }

  snapshot(): TaskPhaseEvidence {
    return { name: this.phase.name, attemptedToolCalls: this.attempts, successfulToolCalls: this.successes, minToolCalls: this.phase.minToolCalls, maxToolCalls: this.phase.maxToolCalls };
  }
}
