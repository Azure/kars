// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Vitest fixtures for operator-TUI panels tests (S14).
 */
import type {
  ClusterState,
  ClawPairingItem,
  McpServerItem,
  ToolPolicyItem,
  InferencePolicyItem,
  A2AAgentItem,
  ClawMemoryItem,
  ClawEvalItem,
  ProviderState,
} from "./types.js";
import type { SandboxInfo } from "../types.js";
import { emptyClusterState } from "./types.js";

export function fixtureSandbox(name = "sb-1"): SandboxInfo {
  return {
    name,
    namespace: `azureclaw-${name}`,
    status: "Running (1/1)",
    health: "healthy",
    model: "gpt-4.1",
    isolation: "enhanced",
    channels: "TG,SL",
    age: "5m",
    podName: `${name}-pod`,
    restarts: 0,
    role: "controller",
    parent: "",
    runtime: "aks",
  };
}

export function fixturePairing(): ClawPairingItem {
  return {
    name: "alice-bob",
    namespace: "azureclaw",
    age: "1h",
    agentA: "alice",
    agentB: "bob",
    state: "Active",
    trust: "Verified",
    conditions: [
      { type: "Paired", status: "True", reason: "HandshakeComplete", message: "X3DH ok" },
    ],
  };
}

export function fixtureMcp(): McpServerItem {
  return {
    name: "mcp-fs",
    namespace: "azureclaw-sb-1",
    age: "20m",
    url: "http://mcp-fs.azureclaw-sb-1.svc:8080",
    productionMode: true,
    jwksSecretPresent: "present",
    allowedToolCount: 4,
    conditions: [
      { type: "Ready", status: "True", reason: "Reachable" },
      { type: "Authenticated", status: "True", reason: "JWKSValidated" },
    ],
  };
}

export function fixtureToolPolicy(): ToolPolicyItem {
  return {
    name: "tp-default",
    namespace: "azureclaw",
    age: "2h",
    appliesToSandbox: "sb-1",
    commerce: { mandates: true, floorUsd: 5 },
    approvalRequired: true,
    rateLimitPerMin: 30,
    ruleCount: 6,
    conditions: [
      { type: "Programmed", status: "True", reason: "PolicyCompiled" },
    ],
  };
}

export function fixtureInferencePolicy(): InferencePolicyItem {
  return {
    name: "ip-default",
    namespace: "azureclaw",
    age: "2h",
    appliesToSandbox: "sb-1",
    dailyTokens: 100_000,
    perRequestTokens: 4_000,
    guardrailFloor: "high",
    modelPreference: ["gpt-4.1", "gpt-4o"],
    conditions: [{ type: "Programmed", status: "True", reason: "PolicyCompiled" }],
  };
}

export function fixtureA2AAgent(): A2AAgentItem {
  return {
    name: "agent-card-1",
    namespace: "azureclaw-sb-1",
    age: "10m",
    endpointUrl: "https://example.test/a2a",
    productionMode: true,
    agentCardPublished: "published",
    capabilities: ["tasks", "streaming"],
    conditions: [{ type: "CardPublished", status: "True", reason: "Signed" }],
  };
}

export function fixtureClawMemory(): ClawMemoryItem {
  return {
    name: "mem-1",
    namespace: "azureclaw",
    age: "5h",
    sandboxRef: "sb-1",
    storeName: "store-default",
    scope: "user-123",
    retentionDays: 30,
    rbacScopeSummary: "project-MI: Azure AI User on RG",
    foundryBound: "bound",
    conditions: [{ type: "Bound", status: "True", reason: "MemoryStoreReady" }],
  };
}

export function fixtureClawEval(): ClawEvalItem {
  return {
    name: "eval-nightly",
    namespace: "azureclaw",
    age: "1d",
    sandboxRef: "sb-1",
    suite: "rag-quality",
    schedule: "0 2 * * *",
    lastRunAt: "2026-04-29T02:00:00Z",
    lastScore: "0.91",
    nextScheduledAt: "2026-04-30T02:00:00Z",
    conditions: [{ type: "Scheduled", status: "True", reason: "CronProgrammed" }],
  };
}

export function fixtureProviderHealthy(id = "foundry"): ProviderState {
  return { id, label: id, status: "healthy" };
}

export function fixtureProviderUnknown(id = "agc", reason = "Gateway not found"): ProviderState {
  return { id, label: id, status: "unknown", reason };
}

export function fullFixture(): ClusterState {
  const state = emptyClusterState();
  state.sandboxes = [fixtureSandbox("sb-1"), fixtureSandbox("sb-2")];
  state.pairings = [fixturePairing()];
  state.mcpServers = [fixtureMcp()];
  state.toolPolicies = [fixtureToolPolicy()];
  state.inferencePolicies = [fixtureInferencePolicy()];
  state.a2aAgents = [fixtureA2AAgent()];
  state.clawMemories = [fixtureClawMemory()];
  state.clawEvals = [fixtureClawEval()];
  state.providers.perSandbox.set("sb-1", [
    fixtureProviderHealthy("foundry"),
    fixtureProviderHealthy("agt"),
    fixtureProviderHealthy("acr"),
    fixtureProviderUnknown("identity", "no workload-identity annotation"),
  ]);
  state.providers.cluster = [fixtureProviderUnknown("agc", "no Gateway objects")];
  return state;
}
