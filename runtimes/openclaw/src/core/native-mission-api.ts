// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/** Structural subset verified against OpenClaw fc23bc864e4553c2d215e479eeec47b67a0bf943.
 * Keep this boundary aligned with plugin-api.types.ts, runtime/types-core.ts,
 * tool-types.ts, hook-types.ts and agents/embedded-agent-runner/{types,run/params}.ts.
 * No private runtime admission or CLI APIs are used.
 */
export interface NativeToolContext {
  sessionKey?: string;
  sessionId?: string;
  toolBindings?: Readonly<Record<string, unknown>>;
  activeModel?: { provider?: string; modelId?: string };
  assertInvocationCurrent: () => void;
}
export interface NativeTool {
  name: string;
  label: string;
  description: string;
  parameters: Record<string, unknown>;
  execute: (id: string, args: Record<string, unknown>, signal?: AbortSignal) => Promise<{
    content: { type: "text"; text: string }[];
    details: unknown;
    isError?: boolean;
  }>;
}
export interface NativeToolFactory {
  contextVersion: 2;
  create: (ctx: NativeToolContext) => NativeTool[] | null;
}
export interface NativeToolHookContext {
  runId?: string;
  sessionKey?: string;
  sessionId?: string;
  toolName: string;
  toolCallId?: string;
  abortSignal?: AbortSignal;
}
export interface NativeToolHookEvent {
  runId?: string;
  toolCallId?: string;
  toolName: string;
  params: Record<string, unknown>;
}
export interface NativeRunResult {
  payloads?: { text?: string; isError?: boolean; isReasoning?: boolean; isCommentary?: boolean }[];
  meta: {
    aborted?: boolean;
    error?: unknown;
    agentMeta?: { sessionId: string; provider: string; model: string; assistantTurns?: number; usage?: unknown };
    completion?: { finishReason?: string; stopReason?: string; refusal?: boolean };
    replayInvalid?: boolean;
    yielded?: boolean;
    continuationPending?: true;
    pendingToolCalls?: unknown[];
    failureSignal?: unknown;
    terminalToolFailure?: unknown;
  };
  didSendViaMessagingTool?: boolean;
  acceptedSessionSpawns?: unknown[];
  asyncWorkStarted?: true;
  requesterContinuationSettled?: true;
}
export interface NativeRunParams {
  sessionId: string;
  sessionKey: string;
  agentId: string;
  workspaceDir: string;
  agentDir: string;
  sessionPersistence: "durable";
  prompt: string;
  extraSystemPrompt: string;
  timeoutMs: number;
  runId: string;
  config: Record<string, unknown>;
  provider: string;
  model: string;
  modelSelectionLocked: true;
  modelFallbacksOverride: string[];
  allowAuthProfileFallback: false;
  agentHarnessRuntimeOverride: "openclaw";
  toolsAllow: string[];
  toolExecutionAllow: string[];
  toolBindings: Readonly<Record<string, unknown>>;
  disableMessageTool: true;
  abortSignal: AbortSignal;
  onAgentEvent: (event: { stream: string; data: Record<string, unknown>; sessionKey?: string }) => void;
}
export interface NativeWebFetchProvider {
  id: string;
  label: string;
  hint: string;
  envVars: string[];
  placeholder: string;
  signupUrl: string;
  credentialPath: string;
  requiresCredential: false;
  getCredentialValue: (config?: Record<string, unknown>) => unknown;
  setCredentialValue: (config: Record<string, unknown>, value: unknown) => void;
  createTool: (ctx: { config?: Record<string, unknown>; fetchConfig?: Record<string, unknown> }) => {
    description: string;
    parameters: Record<string, unknown>;
    execute: (args: Record<string, unknown>, context?: { signal?: AbortSignal }) => Promise<Record<string, unknown>>;
  } | null;
}
export interface NativeMissionApi {
  id: string;
  registerWebFetchProvider: (provider: NativeWebFetchProvider) => void;
  runtime: {
    version: string;
    config: { current: () => Record<string, unknown> };
    agent: {
      session: {
        getSessionEntry: (params: { agentId: string; sessionKey: string; readConsistency: "latest" }) => unknown;
      };
      resolveAgentDir: (cfg: Record<string, unknown>, agentId: string) => string;
      resolveAgentWorkspaceDir: (cfg: Record<string, unknown>, agentId: string) => string;
      runEmbeddedAgent: (params: NativeRunParams) => Promise<NativeRunResult>;
    };
  };
  registerTool: (factory: NativeToolFactory, options: { names: string[]; optional: true }) => void;
  on: {
    (name: "before_tool_call", handler: (event: NativeToolHookEvent, ctx: NativeToolHookContext) => Promise<{
      block: true; blockReason: string;
    } | void>): void;
    (name: "after_tool_call", handler: (event: NativeToolHookEvent & {
      result?: unknown; error?: string; durationMs?: number;
    }, ctx: NativeToolHookContext) => Promise<void>): void;
    (name: "before_agent_finalize", handler: (event: { runId?: string; sessionId: string; messages?: unknown[] }, ctx: {
      runId?: string; sessionKey?: string; sessionId?: string;
    }) => void): void;
    (name: "agent_end", handler: (event: { runId?: string; messages: unknown[]; success: boolean }, ctx: {
      runId?: string; sessionKey?: string; sessionId?: string;
    }) => void): void;
    (name: "model_call_started", handler: (event: { runId: string; callId: string; provider: string; model: string }, ctx: {
      sessionKey?: string; sessionId?: string;
    }) => void): void;
  };
}
