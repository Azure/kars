// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { resolve } from "node:path";

export interface TaskPhaseEvidence {
  name: string;
  attemptedToolCalls: number;
  successfulToolCalls: number;
  minToolCalls: number;
  maxToolCalls: number;
}

interface FilesystemPhase {
  name: string;
  objective: string;
  capabilities: string[];
  requiredToolCalls: never[];
  minToolCalls: number;
  maxToolCalls: number;
  freshContext: boolean;
}

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
    const keys = ["name", "objective", "capabilities", "requiredToolCalls", "minToolCalls", "maxToolCalls", "freshContext"];
    if (!record(input) || Object.keys(input).some(key => !keys.includes(key))) {
      throw new Error("Invalid reviewed phase object");
    }
    const { name, objective, maxToolCalls } = input;
    const capabilities = input.capabilities === undefined ? [] : input.capabilities;
    const requiredToolCalls = input.requiredToolCalls === undefined ? [] : input.requiredToolCalls;
    const minToolCalls = input.minToolCalls === undefined ? 0 : input.minToolCalls;
    const freshContext = input.freshContext === undefined ? false : input.freshContext;
    if (typeof name !== "string" || !/^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/.test(name)
      || typeof objective !== "string" || Buffer.byteLength(objective.trim()) < 20 || Buffer.byteLength(objective) > 1200
      || !Number.isSafeInteger(minToolCalls) || !Number.isSafeInteger(maxToolCalls)
      || (minToolCalls as number) < 0 || (maxToolCalls as number) > 32 || (maxToolCalls as number) < (minToolCalls as number)
      || typeof freshContext !== "boolean") {
      throw new Error("Invalid reviewed phase name, objective, or call bounds");
    }
    if (!Array.isArray(capabilities) || capabilities.some(capability => capability !== "filesystem-read" && capability !== "filesystem-write")
      || new Set(capabilities).size !== capabilities.length) {
      throw new Error("Unsupported reviewed phase capabilities: only filesystem-read and filesystem-write are executable");
    }
    if (!Array.isArray(requiredToolCalls) || requiredToolCalls.length !== 0) {
      throw new Error("Required tool-call contracts are not supported by the filesystem phase executor");
    }
    if ((minToolCalls as number) > 0 && capabilities.length === 0) {
      throw new Error("Reviewed phase requires successful calls but grants no supported tools");
    }
    this.phase = { name, objective, capabilities: [...capabilities], requiredToolCalls: [], minToolCalls: minToolCalls as number, maxToolCalls: maxToolCalls as number, freshContext };
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
