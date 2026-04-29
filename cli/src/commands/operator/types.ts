/**
 * Operator-TUI shared type declarations.
 *
 * Extracted from `cli/src/commands/operator.ts` (S15.e.1) so the
 * top-level dashboard module can stay under §4.2's 800-line cap.
 *
 * No behavioral or shape change — all interfaces are byte-identical
 * to the originals. They are used only inside the operator command
 * (no cross-file consumers as of S15.e.1).
 */

export type HealthState = "healthy" | "degraded" | "down" | "pending" | "unknown" | "dormant";

export interface SandboxInfo {
  name: string;
  namespace: string;
  status: string;
  health: HealthState;
  model: string;
  isolation: string;
  channels: string;
  age: string;
  podName: string;
  restarts: number;
  role: "controller" | "sub-agent";
  parent: string;  // parent agent name (empty if controller)
  handoffState?: "dormant" | "active-successor" | "returning";
  runtime: "docker" | "aks";
}

export interface EgressDomain {
  domain: string;
  sandbox: string;
  namespace: string;
  state: "learned" | "approved";
}

/** Security state polled from a single sandbox's router + k8s objects. */
export interface SecurityState {
  sandbox: string;
  isolation: string;
  runtime: string;         // "runc" | "kata-vm-isolation"
  seccomp: string;         // "azureclaw-strict" | "RuntimeDefault"
  networkPolicy: boolean;
  adminAuth: boolean;
  readyz: boolean;
  readyzDetail: string;    // "ok" | "not ready — ..." | "content safety unreachable"
  egressMode: string;      // "learning" | "enforcing" | "unknown"
  learnedDomains: number;
  allowlistDomains: number;
  blocklistDomains: number;
  blocklistLearnMode: boolean;
  agtEnabled: boolean;
  agtAuditEntries: number;
  agtAuditIntegrity: boolean;
  agtKnownAgents: number;
  agtTrustThreshold: number;
  // AGT detail — populated from /agt/audit, /agt/trust, relay/registry logs
  agtRecentAudit: string[];     // last few audit entries
  agtTrustScores: { agent: string; score: number; tier: string; interactions: number; lastSeen: string }[];
  agtRelayConnected: boolean;
  agtRegistryAgents: number;
  agtAmid: string;  // cryptographic identity from registry
  // Mesh counters (from router MeshMetrics)
  agtMeshSessions: number;
  agtMeshSent: number;
  agtMeshReceived: number;
  agtTrustUpdates: number;
  agtTotalInteractions: number;
  // Native governance stats (from /agt/status when governance_mode === "native")
  agtGovernanceMode: string;     // "native" | ""
  agtPolicyEvaluations: number;
  agtPolicyDenials: number;
  agtPolicyRateLimits: number;
  agtEvalLatencyUs: number;      // average eval latency in microseconds
  agtBehaviorAlerts: number;
  agtBehaviorDetail: Array<{ agent: string; reasons: string[] }>;
  agtContentFlags: number;
  agtPolicyRules: number;
  // Registry reputation (from agentmesh-registry)
  agtReputation: {
    score: number;       // 0.0–1.0 composite
    tier: string;
    completionRate: number;
    totalSessions: number;
    feedbackCount: number;
    avgFeedback: number;
    tags: { tag: string; count: number }[];
  } | null;
  // AGT relay/registry URLs
  agtRelayUrl: string;
  agtRegistryUrl: string;
  // Prometheus metrics
  totalRequests: number;
  errorRequests: number;
  inputTokens: number;
  outputTokens: number;
  avgLatencyMs: number;
}

export interface NodeInfo {
  name: string;
  pool: string;
  status: string;
  version: string;
  cpuCores: string;
  cpuPct: string;
  memBytes: string;
  memPct: string;
  os: string;
  runtime: string;
}

export interface ClusterHealth {
  apiLatencyMs: number;
  apiReachable: boolean;
  nodes: NodeInfo[];
  quotas: { namespace: string; cpuUsed: string; cpuHard: string; memUsed: string; memHard: string }[];
  pvcs: { namespace: string; name: string; phase: string; size: string }[];
  warnings: { time: string; reason: string; object: string; message: string }[];
}

export interface MeshHealth {
  relayReady: boolean;
  registryReady: boolean;
  registryPods: number;
  registryReadyPods: number;
}
