// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Per-sandbox security state + egress + AGT fetchers.
 *
 * Extracted from `cli/src/commands/operator.ts` (S15.e.3) so the
 * top-level dashboard module can stay under §4.2's 800-line cap.
 *
 * `fetchSecurityState` performs a deep poll of router-internal
 * endpoints + K8s objects (NetworkPolicy, admin token).
 * `fetchAgtQuick` is a lightweight per-cycle refresh that mutates
 * an existing `SecurityState` (the dashboard caches one per sandbox
 * and reuses it across refresh cycles to keep mesh counters alive
 * between full security refreshes).
 * `fetchEgressDomains` returns the merged learned + approved
 * allowlist domains for the egress-management view.
 *
 * No behavioral change vs. the originals — bodies are byte-identical
 * apart from mechanical edits: closure-captured `kubeContext` and
 * `existing: SecurityState | undefined` are now explicit parameters.
 */

import { execa } from "execa";
import type { EgressDomain, SandboxInfo, SecurityState } from "../types.js";
import { kctl, sumPrometheusCounter } from "../helpers.js";

export async function fetchEgressDomains(sb: SandboxInfo, kubeContext?: string): Promise<EgressDomain[]> {
  if (!sb.podName) return [];
  const isDockerAgent = sb.runtime === "docker";
  const routerCurl = isDockerAgent
    ? (path: string) => execa("docker", [
        "exec", sb.podName!,
        "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
      ], { stdio: "pipe" })
    : (path: string) => execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName!,
        "-c", "inference-router", "--",
        "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
      ], kubeContext), { stdio: "pipe", timeout: 10000 });

  try {
    const [learnedRes, allowRes] = await Promise.allSettled([
      routerCurl("/egress/learned"),
      routerCurl("/egress/allowlist"),
    ]);

    const learnedDomains: Set<string> = new Set();
    const approvedDomains: Set<string> = new Set();

    if (learnedRes.status === "fulfilled") {
      const data = JSON.parse((learnedRes.value as any).stdout);
      for (const d of (data.domains || [])) learnedDomains.add(d);
    }
    if (allowRes.status === "fulfilled") {
      const data = JSON.parse((allowRes.value as any).stdout);
      for (const d of (data.domains || [])) approvedDomains.add(d);
    }

    // Merge: domains in allowlist are "approved", rest are "learned" (pending)
    const results: EgressDomain[] = [];
    const allDomains = new Set([...learnedDomains, ...approvedDomains]);
    for (const d of allDomains) {
      results.push({
        domain: d,
        sandbox: sb.name,
        namespace: sb.namespace,
        state: approvedDomains.has(d) ? "approved" : "learned",
      });
    }
    // Sort: pending first, then approved
    results.sort((a, b) => {
      if (a.state !== b.state) return a.state === "learned" ? -1 : 1;
      return a.domain.localeCompare(b.domain);
    });
    return results;
  } catch {
    return [];
  }
}

/** Poll security-relevant endpoints for a single sandbox. */
export async function fetchSecurityState(sb: SandboxInfo, kubeContext?: string): Promise<SecurityState> {
  const state: SecurityState = {
    sandbox: sb.name,
    isolation: sb.isolation,
    runtime: sb.isolation === "confidential" ? "kata-vm" : "runc",
    seccomp: sb.runtime === "docker" ? "kars-strict"
             : sb.isolation === "enhanced" ? "kars-strict" : "RuntimeDefault",
    networkPolicy: false,
    adminAuth: false,
    readyz: false,
    readyzDetail: "unknown",
    egressMode: "unknown",
    learnedDomains: 0,
    allowlistDomains: 0,
    blocklistDomains: 0,
    blocklistLearnMode: false,
    agtEnabled: false,
    agtAuditEntries: 0,
    agtAuditIntegrity: false,
    agtKnownAgents: 0,
    agtTrustThreshold: 0,
    agtRecentAudit: [],
    agtTrustScores: [],
    agtRelayConnected: false,
    agtRegistryAgents: 0,
    agtAmid: "",
    agtMeshSessions: 0,
    agtMeshSent: 0,
    agtMeshReceived: 0,
    agtTrustUpdates: 0,
    agtTotalInteractions: 0,
    agtGovernanceMode: "",
    agtPolicyEvaluations: 0,
    agtPolicyDenials: 0,
    agtPolicyRateLimits: 0,
    agtEvalLatencyUs: 0,
    agtBehaviorAlerts: 0,
    agtBehaviorDetail: [],
    agtContentFlags: 0,
    agtPolicyRules: 0,
    agtReputation: null,
    agtRelayUrl: "",
    agtRegistryUrl: "",
    totalRequests: 0,
    errorRequests: 0,
    inputTokens: 0,
    outputTokens: 0,
    avgLatencyMs: 0,
  };

  if (!sb.podName) return state;

  // Use per-sandbox runtime to choose exec path (not global devMode) so the
  // unified view can query Docker agents via docker-exec and AKS agents via
  // kubectl-exec in the same operator session.
  const isDockerAgent = sb.runtime === "docker";
  const routerExec = isDockerAgent
    ? (path: string) => execa("docker", [
        "exec", sb.podName!,
        "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
      ], { stdio: "pipe" })
    : (path: string) => execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName!,
        "-c", "inference-router", "--",
        "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
      ], kubeContext), { stdio: "pipe", timeout: 10000 });

  // Docker agents don't have K8s resources (NetworkPolicy, admin token secret)
  const k8sCheck = (args: string[]) => isDockerAgent
    ? Promise.reject("docker-agent")
    : execa("kubectl", kctl(args, kubeContext), { stdio: "pipe", timeout: 10000 });

  const checks = await Promise.allSettled([
    // 0: NetworkPolicy (K8s only)
    k8sCheck(["get", "networkpolicy", "sandbox-policy", "-n", sb.namespace, "-o", "name"]),
    // 1: Admin token secret (K8s only)
    k8sCheck(["get", "secret", "router-admin-token", "-n", sb.namespace, "-o", "name"]),
    // 2: /readyz (body, not just status code)
    routerExec("/readyz"),
    // 3: /blocklist/status
    routerExec("/blocklist/status"),
    // 4: /agt/status
    routerExec("/agt/status"),
    // 5: /egress/allowlist
    routerExec("/egress/allowlist"),
    // 6: /metrics (Prometheus text)
    routerExec("/metrics"),
    // 7: /agt/audit (last entries)
    routerExec("/agt/audit"),
    // 8: /agt/reputation (registry + local trust)
    routerExec("/agt/reputation"),
  ]);

  // NetworkPolicy
  if (checks[0].status === "fulfilled") state.networkPolicy = true;

  // Admin token
  if (checks[1].status === "fulfilled") state.adminAuth = true;

  // readyz
  if (checks[2].status === "fulfilled") {
    const body = ((checks[2].value as any).stdout || "").trim();
    state.readyz = body.startsWith("ok");
    state.readyzDetail = body || "ok";
  }

  // blocklist/status
  if (checks[3].status === "fulfilled") {
    try {
      const bl = JSON.parse((checks[3].value as any).stdout);
      state.blocklistDomains = bl.domain_count || 0;
      state.blocklistLearnMode = bl.learn_mode ?? false;
      state.learnedDomains = bl.learned_domains || 0;
      state.egressMode = bl.learn_mode ? "learning" : "enforcing";
    } catch { /* parse fail */ }
  }

  // agt/status — includes trust_states and inbox count
  if (checks[4].status === "fulfilled") {
    try {
      const agt = JSON.parse((checks[4].value as any).stdout);
      state.agtEnabled = agt.enabled ?? false;
      state.agtAuditEntries = agt.audit_entries || 0;
      state.agtAuditIntegrity = agt.audit_integrity ?? false;
      state.agtKnownAgents = agt.known_agents || 0;
      // Trust states from /agt/status response
      const ts = agt.trust_states || [];
      state.agtTrustScores = ts.map((a: any) => ({
        agent: a.agent_id || a.name || "unknown",
        score: a.score ?? 0,
        tier: a.tier || (a.score >= 800 ? "Sovereign" : a.score >= 600 ? "Verified" : a.score >= 400 ? "Known" : a.score >= 200 ? "Observed" : "Anonymous"),
        interactions: a.interactions ?? 0,
        lastSeen: a.last_interaction || "",
      }));
      state.agtRegistryAgents = ts.length;
      // Mesh metrics from router MeshMetrics counters
      state.agtMeshSessions = agt.mesh_sessions || 0;
      state.agtMeshSent = agt.mesh_messages_sent || 0;
      state.agtMeshReceived = agt.mesh_messages_received || 0;
      state.agtTrustUpdates = agt.trust_updates || 0;
      state.agtTotalInteractions = agt.total_interactions || 0;
      // Native governance stats
      state.agtGovernanceMode = agt.governance_mode || "";
      state.agtPolicyEvaluations = agt.policy_evaluations || 0;
      state.agtPolicyDenials = agt.policy_denials || 0;
      state.agtPolicyRateLimits = agt.policy_rate_limits || 0;
      state.agtEvalLatencyUs = agt.eval_latency_avg_us || 0;
      state.agtBehaviorAlerts = agt.behavior_alerts || 0;
      state.agtBehaviorDetail = agt.behavior_alerts_detail || [];
      state.agtContentFlags = agt.content_flags || 0;
      state.agtPolicyRules = agt.policy_rules || 0;
      state.agtRelayUrl = agt.relay_url || "";
      state.agtRegistryUrl = agt.registry_url || "";
      // If no trust states but governance is enabled, show self
      if (state.agtEnabled && ts.length === 0) {
        state.agtTrustScores = [{
          agent: agt.sandbox || sb.name,
          score: 500,
          tier: "Known (self)",
          interactions: 0,
          lastSeen: "",
        }];
        state.agtRegistryAgents = 1;
      }
    } catch { /* parse fail */ }
  }

  // egress/allowlist
  if (checks[5].status === "fulfilled") {
    try {
      const al = JSON.parse((checks[5].value as any).stdout);
      state.allowlistDomains = al.count || 0;
    } catch { /* parse fail */ }
  }

  // /metrics — parse Prometheus text format
  if (checks[6].status === "fulfilled") {
    const metricsText = (checks[6].value as any).stdout || "";
    state.totalRequests = sumPrometheusCounter(metricsText, "kars_inference_requests_total");
    state.errorRequests = sumPrometheusCounter(metricsText, "kars_inference_requests_total", { status: "error" });
    state.inputTokens = sumPrometheusCounter(metricsText, "kars_tokens_total", { direction: "input" });
    state.outputTokens = sumPrometheusCounter(metricsText, "kars_tokens_total", { direction: "output" });

    // Average latency from histogram sum/count
    const latSum = sumPrometheusCounter(metricsText, "kars_inference_latency_seconds_sum");
    const latCount = sumPrometheusCounter(metricsText, "kars_inference_latency_seconds_count");
    state.avgLatencyMs = latCount > 0 ? Math.round((latSum / latCount) * 1000) : 0;
  }

  // /agt/audit — extract last few entries
  if (checks[7].status === "fulfilled") {
    try {
      const audit = JSON.parse((checks[7].value as any).stdout);
      const entries = Array.isArray(audit) ? audit : audit.entries || [];
      state.agtRecentAudit = entries.slice(-5).map((e: any) => {
        const action = e.action || e.type || "unknown";
        const agent = e.agent_id || e.agent || "";
        const tool = e.tool || "";
        const result = e.result || e.decision || "";
        let ts = "";
        if (e.timestamp) {
          const d = new Date(e.timestamp);
          ts = isNaN(d.getTime()) ? "" : d.toLocaleTimeString();
        }
        const parts = [action];
        if (tool) parts.push(`[${tool}]`);
        if (agent) parts.push(agent.substring(0, 16));
        if (result) parts.push(`→ ${result}`);
        return ts ? `${ts} ${parts.join(" ")}` : parts.join(" ");
      });
    } catch { /* parse fail */ }
  }

  // /agt/reputation — registry score + local trust
  if (checks[8].status === "fulfilled") {
    try {
      const rep = JSON.parse((checks[8].value as any).stdout);
      // AMID (cryptographic identity from registry lookup)
      if (rep.amid) state.agtAmid = rep.amid;
      // Registry reputation (from agentmesh-registry Postgres)
      if (rep.registry) {
        const r = rep.registry;
        state.agtReputation = {
          score: r.score ?? 0,
          tier: r.tier || "unknown",
          completionRate: r.completion_rate ?? 0,
          totalSessions: r.total_sessions ?? 0,
          feedbackCount: r.feedback_count ?? 0,
          avgFeedback: r.average_feedback ?? 0,
          tags: (r.tags || []).map((t: any) => ({ tag: t.tag || "", count: t.count || 0 })),
        };
      }
      // Local trust store (from router in-memory)
      const local = rep.local_trust || [];
      if (local.length > 0) {
        state.agtTrustScores = local.map((a: any) => ({
          agent: a.agent_id || a.name || "unknown",
          score: a.score ?? 0,
          tier: a.tier || (a.score >= 800 ? "Sovereign" : a.score >= 600 ? "Verified" : a.score >= 400 ? "Known" : "Anonymous"),
          interactions: a.interactions ?? 0,
          lastSeen: a.last_interaction || "",
        }));
        state.agtRegistryAgents = local.length;
      }
    } catch { /* parse fail */ }
  }

  return state;
}

/**
 * Lightweight per-cycle AGT refresh — only fetches `/agt/status` per
 * sandbox to keep trust scores and mesh counters alive between full
 * security refreshes.
 *
 * Mutates `existing` in place; the dashboard caches one `SecurityState`
 * per sandbox and reuses it across refresh cycles. If `existing` is
 * undefined or AGT is not enabled, returns immediately without an exec.
 */
export async function fetchAgtQuick(
  sb: SandboxInfo,
  existing: SecurityState | undefined,
  kubeContext?: string,
): Promise<void> {
  if (!sb.podName) return;
  if (!existing?.agtEnabled) return;

  try {
    const isDockerAgent = sb.runtime === "docker";
    const { stdout } = isDockerAgent
      ? await execa("docker", [
          "exec", sb.podName,
          "curl", "-s", "--max-time", "2", "http://localhost:8443/agt/status",
        ], { stdio: "pipe" })
      : await execa("kubectl", kctl([
          "exec", "-n", sb.namespace, sb.podName,
          "-c", "inference-router", "--",
          "curl", "-s", "--max-time", "2", "http://localhost:8443/agt/status",
        ], kubeContext), { stdio: "pipe", timeout: 8000 });

    const agt = JSON.parse(stdout);
    const ts = agt.trust_states || [];
    if (ts.length > 0) {
      existing.agtTrustScores = ts.map((a: any) => ({
        agent: a.agent_id || a.name || "unknown",
        score: a.score ?? 0,
        tier: a.tier || (a.score >= 800 ? "Sovereign" : a.score >= 600 ? "Verified" : a.score >= 400 ? "Known" : a.score >= 200 ? "Observed" : "Anonymous"),
        interactions: a.interactions ?? 0,
        lastSeen: a.last_interaction || "",
      }));
      existing.agtRegistryAgents = ts.length;
    }
    existing.agtMeshSessions = agt.mesh_sessions || 0;
    existing.agtMeshSent = agt.mesh_messages_sent || 0;
    existing.agtMeshReceived = agt.mesh_messages_received || 0;
    existing.agtTrustUpdates = agt.trust_updates || 0;
    existing.agtTotalInteractions = agt.total_interactions || 0;
    existing.agtAuditEntries = agt.audit_entries || 0;
    existing.agtAuditIntegrity = agt.audit_integrity ?? false;
    existing.agtGovernanceMode = agt.governance_mode || existing.agtGovernanceMode;
    existing.agtPolicyEvaluations = agt.policy_evaluations || 0;
    existing.agtPolicyDenials = agt.policy_denials || 0;
    existing.agtPolicyRateLimits = agt.policy_rate_limits || 0;
    existing.agtEvalLatencyUs = agt.eval_latency_avg_us || 0;
    existing.agtBehaviorAlerts = agt.behavior_alerts || 0;
    existing.agtBehaviorDetail = agt.behavior_alerts_detail || [];
    existing.agtContentFlags = agt.content_flags || 0;
    existing.agtPolicyRules = agt.policy_rules || existing.agtPolicyRules;
  } catch { /* non-fatal — full refresh on next TIER_DETAIL cycle */ }
}
