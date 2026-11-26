// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { SetupCoreInventory } from "./core.js";
import type { LocalBetaPorts } from "../dev/kind-beta-local.js";
import type { CoreHookFailureCode } from "../../lib/core-hook-diagnostics.js";

export type SetupProviderKind = "foundry" | "azure-openai" | "openai" | "anthropic" | "github-copilot" | "openai-compatible";
export interface SetupProvider { kind: SetupProviderKind; endpoint: string; model: string; apiKey?: string }
export interface SetupModel { id: string; name: string }
export interface SetupCopilotState {
  phase: "idle" | "starting" | "pending" | "connected" | "failed";
  userCode?: string; verificationUri?: string; expiresAt?: number; message?: string;
  models?: SetupModel[];
  modelsPhase?: "loading" | "ready" | "failed";
  modelsMessage?: string;
}
export interface SetupDraft {
  target: "kind" | "existing";
  clusterName: string;
  context: string;
  bridge: boolean;
  ports: LocalBetaPorts;
  provider: SetupProvider;
  admin: { email: string; password: string };
  consent: { target: boolean; hostTrust: boolean };
}
export interface SetupCheck {
  id: string; label: string; status: "pending" | "running" | "passed" | "failed" | "blocked"; detail?: string;
  startedAt?: number; finishedAt?: number;
  diagnosticCode?: CoreHookFailureCode;
}
export interface SetupObservation {
  status: "live" | "unavailable" | "stopped";
  observedAt?: number;
  message: string;
  resources: SetupCheck[];
}
export interface SetupState {
  phase: "draft" | "prerequisites" | "preflight" | "review" | "installing" | "ready" | "blocked" | "failed" | "closed";
  prerequisitesReady?: boolean;
  prerequisiteSelection?: { bridge: boolean; ports: LocalBetaPorts };
  contexts: Array<{ name: string; cluster: string; current: boolean }>;
  checks: SetupCheck[]; steps: SetupCheck[]; busy: boolean; message?: string; handoffUrl?: string;
  providerConfigured: boolean;
  authorization?: "waiting" | "approved" | "failed";
  authorizationTarget?: "terminal" | "macos";
  core?: SetupCoreInventory;
  copilot?: SetupCopilotState;
  installation?: { clusterName: string; bridge: boolean; ports: LocalBetaPorts; startedAt: number; finishedAt?: number };
  observation?: SetupObservation;
  cleanup?: { status: "running" | "passed" | "failed"; detail: string };
  recovery?: { clusterName?: string; directory: string; workingDirectory: string; receipt: "available" | "absent" | "unreadable"; nodeRecorded?: boolean; hostTrustInstalled?: boolean; cleanupCommand?: string; cleanupAvailable?: boolean };
}
export class SetupError extends Error {}
