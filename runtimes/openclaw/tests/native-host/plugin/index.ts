// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFile, writeFile, rename } from "node:fs/promises";
import { join } from "node:path";
import { createNativeMissionExecutor } from "../../../src/core/native-mission.js";
import type { NativeMissionApi } from "../../../src/core/native-mission-api.js";
import type { MissionAssignment } from "@kars/mesh/dist/mission-protocol.js";
import { createSystemPromptHook } from "../../../src/core/system-prompt.js";

type HostApi = NativeMissionApi & {
  on: NativeMissionApi["on"] & ((name: "before_prompt_build", handler: ReturnType<typeof createSystemPromptHook>) => void)
    & ((name: "before_agent_finalize", handler: (event: { messages?: unknown[]; sessionId: string }) => { action: "continue" | "revise"; reason?: string;
      retry?: { instruction: string; idempotencyKey: string; maxAttempts: number } }) => void);
  registrationMode?: string;
  registerService(service: {
    id: string;
    start(): Promise<void>;
    stop(): void;
  }): void;
};

export default {
  id: "kars-native-host-verification",
  register(api: HostApi) {
    if (api.registrationMode !== "full") return;
    // The real loader supplies the API. Never replace runtime, tools, hooks, or filesystem access.
    api.on("before_prompt_build", createSystemPromptHook("Kars native-host configured identity"));
    const execute = createNativeMissionExecutor(api);
    let revised = false;
    const finalizations: unknown[] = [];
    api.on("before_agent_finalize", event => {
      finalizations.push({ sessionId: event.sessionId, messages: structuredClone(event.messages) });
      if (revised) return { action: "continue" };
      revised = true;
      return { action: "revise", reason: "Verify accounting across a genuine native attempt boundary",
        retry: { instruction: "Complete the assigned tools and required exports before returning the final confirmation.",
          idempotencyKey: "native-host-accounting-revision", maxAttempts: 1 } };
    });
    const starts: unknown[] = [];
    const ends: unknown[] = [];
    api.on("model_call_started", (event, ctx) => {
      starts.push({ event: { runId: event.runId, callId: event.callId, provider: event.provider, model: event.model },
        ctx: { sessionKey: ctx.sessionKey, sessionId: ctx.sessionId } });
    });
    api.on("agent_end", (event, ctx) => {
      ends.push({ event: { runId: event.runId, messages: structuredClone(event.messages), success: event.success },
        ctx: { runId: ctx.runId, sessionKey: ctx.sessionKey, sessionId: ctx.sessionId } });
    });
    let stopped = false;
    let started = false;
    api.registerService({
      id: "native-mission-host-verification",
      async start() {
        if (started) throw new Error("Verification service started twice");
        started = true;
        const root = process.env.KARS_NATIVE_HOST_RUN_DIR;
        if (!root?.startsWith("/sandbox/")) throw new Error("Verification requires an owned /sandbox checkout directory");
        const assignment = JSON.parse(await readFile(join(root, "assignment.json"), "utf8")) as MissionAssignment;
        const progress: unknown[] = [];
        let result: unknown;
        let failure: unknown;
        try {
          result = await execute(assignment, (evidence, routerObservations) => {
            progress.push({ ...structuredClone(evidence), ...(routerObservations ? { routerObservations } : {}) });
          }, () => {
            if (stopped) throw new Error("Verification service stopped during execution");
          });
        } catch (error) {
          failure = error instanceof Error
            ? { ...error, name: error.name, message: error.message, stack: error.stack }
            : { message: String(error) };
        }
        const receipt = {
          runtimeVersion: api.runtime.version,
          registrationMode: api.registrationMode,
          result, failure, starts, ends, finalizations, progress,
        };
        await writeFile(join(root, "receipt.pending.json"), JSON.stringify(receipt, null, 2));
        await rename(join(root, "receipt.pending.json"), join(root, "receipt.json"));
      },
      stop() { stopped = true; },
    });
  },
};
