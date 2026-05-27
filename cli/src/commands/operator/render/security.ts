// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Security + AGT render helpers — extracted from operator.ts startDashboard
// closure (S15.e.5b) so the closure stays under the §4.2 800-LOC cap.
// Bodies byte-identical to the originals; closure-captured `agentTable`,
// `sandboxes`, `securityStates`, `securityBox`, and `agtPanel` become
// an explicit context object.

import type { SandboxInfo, SecurityState } from "../types.js";

interface BlessedBox {
  setContent(content: string): void;
}

interface AgentTable {
  rows?: { selected?: number };
}

export interface SecurityRenderContext {
  agentTable: AgentTable;
  sandboxes: SandboxInfo[];
  securityStates: Map<string, SecurityState>;
  securityBox: BlessedBox;
  agtPanel: BlessedBox;
}

const ok = (v: boolean) => v ? "{green-fg}●{/}" : "{red-fg}●{/}";

export function renderSecurity(ctx: SecurityRenderContext): void {
  const { agentTable, sandboxes, securityStates, securityBox } = ctx;
  const idx = agentTable.rows?.selected ?? 0;
  const sb = sandboxes[idx];
  if (!sb) {
    securityBox.setContent("{gray-fg}No agent selected{/}");
    return;
  }

  const sec = securityStates.get(sb.name);
  if (!sec) {
    securityBox.setContent(`{bold}${sb.name}{/}\n\n{gray-fg}Polling...{/}`);
    return;
  }

  const seccompLabel = sec.seccomp === "kars-strict"
    ? "{green-fg}strict (~219){/}" : `{yellow-fg}${sec.seccomp}{/}`;

  const egressLabel = sec.egressMode === "learning"
    ? `{yellow-fg}learning{/} (${sec.learnedDomains} found)`
    : sec.egressMode === "enforcing"
    ? `{green-fg}enforcing{/} (${sec.allowlistDomains} allowed)`
    : "{gray-fg}unknown{/}";

  const blLabel = sec.blocklistDomains > 0
    ? `{green-fg}${sec.blocklistDomains.toLocaleString()}{/} domains`
    : "{yellow-fg}not loaded{/}";

  const readyzLabel = sec.readyz
    ? `{green-fg}${sec.readyzDetail}{/}`
    : `{red-fg}${sec.readyzDetail}{/}`;

  // Token formatting
  const fmtTokens = (n: number) =>
    n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` :
    n >= 1_000 ? `${(n / 1_000).toFixed(1)}K` :
    `${n}`;

  const totalTokens = sec.inputTokens + sec.outputTokens;
  const errRate = sec.totalRequests > 0
    ? `${((sec.errorRequests / sec.totalRequests) * 100).toFixed(1)}%`
    : "-";
  const errColor = sec.errorRequests > 0 ? "red" : "green";

  const lines: string[] = [
    `{bold}${sb.name}{/}`,
    "",
    `{bold}{underline}Infrastructure{/}`,
    ` Isolation     ${sec.isolation} (${sec.runtime})`,
    ` Seccomp       ${seccompLabel}`,
    ` NetworkPolicy ${ok(sec.networkPolicy)} ${sec.networkPolicy ? "active" : "missing"}`,
    ` iptables      ${ok(true)} redirect → proxy`,
    ` Fwd Proxy     ${ok(true)} localhost:8444`,
    ` Admin Auth    ${ok(sec.adminAuth)} ${sec.adminAuth ? "token set" : "disabled"}`,
    ` Router Ready  ${ok(sec.readyz)} ${readyzLabel}`,
    "",
    `{bold}{underline}Egress Control{/}`,
    ` Mode          ${egressLabel}`,
    ` Blocklist     ${blLabel}`,
    ` Allowlist     ${sec.allowlistDomains} domain(s)`,
    "",
    `{bold}{underline}Token Usage{/}`,
    ` Requests      ${sec.totalRequests} total  {${errColor}-fg}${sec.errorRequests} err (${errRate}){/}`,
    ` Tokens In     ${fmtTokens(sec.inputTokens)}`,
    ` Tokens Out    ${fmtTokens(sec.outputTokens)}`,
    ` Total         {bold}${fmtTokens(totalTokens)}{/}`,
    ` Avg Latency   ${sec.avgLatencyMs > 0 ? `${sec.avgLatencyMs}ms` : "-"}`,
  ];

  if (sec.agtEnabled) {
    const modeLabel = sec.agtGovernanceMode === "native"
      ? "{green-fg}native{/}" : sec.agtGovernanceMode || "unknown";
    const denyRate = sec.agtPolicyEvaluations > 0
      ? `${((sec.agtPolicyDenials / sec.agtPolicyEvaluations) * 100).toFixed(1)}%`
      : "0%";
    const registryMode = sec.agtRegistryUrl
      ? (sec.agtRegistryUrl.includes("localhost") || sec.agtRegistryUrl.includes("agentmesh-registry")
        ? "{yellow-fg}local{/}" : `{green-fg}global{/}`)
      : "{gray-fg}none{/}";
    lines.push(
      "",
      `{bold}{underline}AGT Governance{/}`,
      ` Mode       ${modeLabel}  ${sec.agtPolicyRules} rules`,
      ` Registry   ${registryMode}`,
      ` Evals      ${sec.agtPolicyEvaluations}  deny ${denyRate}  RL ${sec.agtPolicyRateLimits}`,
      ` Latency    ${sec.agtEvalLatencyUs > 0 ? `${sec.agtEvalLatencyUs}µs` : "<1µs"}`,
      sec.agtBehaviorAlerts > 0 && sec.agtBehaviorDetail.length > 0
        ? ` {red-fg}⚠ ${sec.agtBehaviorDetail.map((a: { agent: string; reasons: string[] }) => `${a.agent}: ${a.reasons[0]}`).join("; ")}{/}`
        : ` Safety    {green-fg}✓{/}  flags ${sec.agtContentFlags}`,
      ` {gray-fg}[g] full detail{/}`,
    );
  }

  securityBox.setContent(lines.join("\n"));
}

/** Full AGT detail — used in the overlay panel. */
export function renderAGTFull(
  sb: SandboxInfo,
  sandboxes: SandboxInfo[],
  securityStates: Map<string, SecurityState>,
): string {
  const sec = securityStates.get(sb.name);
  if (!sec) return `{bold}${sb.name}{/}\n{gray-fg}Polling...{/}`;
  if (!sec.agtEnabled) return "{gray-fg}AGT not enabled{/}\n{gray-fg}Use --governance flag{/}";

  const activePeerCount = sec.agtTrustScores.filter((t: any) =>
    t.agent !== sb.name && sandboxes.some((s) => s.name === t.agent) && (t.interactions > 0 || t.lastSeen)
  ).length;

  const lines: string[] = [
    `{bold}${sb.name}{/}` + (sec.agtAmid ? ` {gray-fg}${sec.agtAmid}{/}` : ""),
    ` Mode    ${sec.agtGovernanceMode === "native" ? "{green-fg}native{/}" : sec.agtGovernanceMode || "unknown"}  ${sec.agtPolicyRules} policy rules`,
    ` Chain   ${sec.agtAuditEntries} entries ${ok(sec.agtAuditIntegrity)} ${sec.agtAuditIntegrity ? "valid" : "BROKEN"}`,
    ` Agents  ${sec.agtRegistryAgents > 0 ? sec.agtRegistryAgents : activePeerCount} known`,
    ` Mesh    ${sec.agtMeshSessions} sessions  ↑${sec.agtMeshSent} ↓${sec.agtMeshReceived}  ${sec.agtTrustUpdates} trust updates`,
  ];

  // Native governance policy stats
  if (sec.agtPolicyEvaluations > 0 || sec.agtGovernanceMode === "native") {
    const denyRate = sec.agtPolicyEvaluations > 0
      ? `${((sec.agtPolicyDenials / sec.agtPolicyEvaluations) * 100).toFixed(1)}%`
      : "0%";
    const rlColor = sec.agtPolicyRateLimits > 0 ? "yellow" : "green";
    const contentColor = sec.agtContentFlags > 0 ? "yellow" : "green";
    lines.push(
      "",
      `{bold}Policy Engine{/}  ${sec.agtPolicyRules} rules loaded`,
      ` Evals    ${sec.agtPolicyEvaluations}  deny ${denyRate}  {${rlColor}-fg}${sec.agtPolicyRateLimits} rate-limited{/}`,
      ` Latency  ${sec.agtEvalLatencyUs > 0 ? `${sec.agtEvalLatencyUs}µs` : "<1µs"} avg`,
    );
    // Behavior: show reasons when alerts fire, ✓ when clean
    if (sec.agtBehaviorAlerts > 0 && sec.agtBehaviorDetail.length > 0) {
      for (const alert of sec.agtBehaviorDetail) {
        const why = alert.reasons.join(", ");
        lines.push(` {red-fg}⚠ ${alert.agent}: ${why}{/}`);
      }
    } else {
      lines.push(` Safety   {green-fg}✓ behavior{/}  {${contentColor}-fg}${sec.agtContentFlags > 0 ? `⚠ ${sec.agtContentFlags} content` : "✓ content"}{/}`);
    }
  }

  if (sec.agtReputation) {
    const r = sec.agtReputation;
    const pct = (r.score * 100).toFixed(0);
    const c = r.score >= 0.7 ? "green" : r.score >= 0.5 ? "yellow" : "red";
    // Reputation score bar (10 blocks)
    const filled = Math.round(r.score * 10);
    const bar = "█".repeat(filled) + "░".repeat(10 - filled);
    lines.push("", `{bold}Reputation{/} {gray-fg}(registry){/}`);
    lines.push(` {${c}-fg}${bar}{/} ${pct}% ${r.tier}  ${r.totalSessions} sessions  ${r.feedbackCount} reviews`);
    if (r.totalSessions > 0) {
      // Star rating from avg feedback (0.0–1.0 → 0–5 stars)
      const stars = Math.round(r.avgFeedback * 5);
      const starStr = "★".repeat(stars) + "☆".repeat(5 - stars);
      lines.push(` {yellow-fg}${starStr}{/} ${r.avgFeedback.toFixed(2)}  completion ${(r.completionRate * 100).toFixed(0)}%`);
    }
    // Tag badges
    if (r.tags.length > 0) {
      const tagStr = r.tags.map((t) => `{cyan-fg}#${t.tag}{/}×${t.count}`).join("  ");
      lines.push(` ${tagStr}`);
    }
  } else {
    lines.push("", `{gray-fg}Reputation  awaiting first session{/}`);
  }

  if (sec.agtTrustScores.length > 0) {
    const self = sec.agtTrustScores.find((t) => t.agent === sb.name);
    // Show peers that are either known sandboxes OR recently active.
    // Sub-agents are spawned dynamically and may not appear in the
    // sandbox list; cloud-offload parents are identified only by AMID
    // prefix (no CR name) so they also don't match. For AMID-only peers
    // we require BOTH recent lastSeen AND interactions > 0 so stale
    // ghosts (failed KNOCKs, aged-out sessions) stay hidden.
    const recentThreshold = Date.now() - 30 * 60_000;
    const peers = sec.agtTrustScores.filter((t) => {
      if (t.agent === sb.name) return false;
      if (sandboxes.some((s) => s.name === t.agent)) return true;
      if (!t.lastSeen || t.interactions <= 0) return false;
      const seen = new Date(/^\d+Z$/.test(t.lastSeen) ? Number(t.lastSeen.slice(0, -1)) * 1000 : t.lastSeen);
      return !isNaN(seen.getTime()) && seen.getTime() > recentThreshold;
    });

    if (peers.length > 0) {
      lines.push("", `{bold}Mesh Traffic{/}`);
      for (const t of peers) {
        const c = t.score >= 600 ? "green" : t.score >= 400 ? "yellow" : "red";
        const filled = Math.round(t.score / 100);
        const bar = "█".repeat(filled) + "░".repeat(10 - filled);
        let ago = "";
        if (t.lastSeen) {
          const d = new Date(/^\d+Z$/.test(t.lastSeen) ? Number(t.lastSeen.slice(0, -1)) * 1000 : t.lastSeen);
          const ms = Date.now() - d.getTime();
          if (!isNaN(ms) && ms >= 0) {
            if (ms < 60_000) ago = `${Math.round(ms / 1000)}s ago`;
            else if (ms < 3_600_000) ago = `${Math.round(ms / 60_000)}m ago`;
            else ago = `${Math.round(ms / 3_600_000)}h ago`;
          }
        }
        const name = t.agent;
        lines.push(` {${c}-fg}${bar}{/} ${t.score} ${name}`);
        lines.push(`   ${t.tier} · ${t.interactions} msg${t.interactions !== 1 ? "s" : ""}${ago ? ` · ${ago}` : ""}`);
      }
      const selfName = self?.agent || sb.name;
      lines.push("");
      for (const t of peers) {
        const peerName = t.agent;
        const arrow = t.interactions > 0 ? `═══⟐ ${t.interactions} msg${t.interactions !== 1 ? "s" : ""} ⟐═══` : `─── idle ───`;
        lines.push(` {cyan-fg}${selfName}{/} ${arrow} {green-fg}${peerName}{/}`);
      }
    } else if (self) {
      lines.push("", `{gray-fg}No peer agents yet{/}`);
    }
  }

  if (sec.agtRecentAudit.length > 0) {
    lines.push("", "{bold}Audit{/}");
    for (const entry of sec.agtRecentAudit) {
      lines.push(` {gray-fg}${entry}{/}`);
    }
  } else {
    lines.push("", "{gray-fg}No audit entries yet{/}");
  }

  return lines.filter(Boolean).join("\n");
}

/** Compact AGT summary for the small panel. */
export function renderAGT(ctx: SecurityRenderContext): void {
  const { agentTable, sandboxes, securityStates, agtPanel } = ctx;
  const idx = agentTable.rows?.selected ?? 0;
  const sb = sandboxes[idx];
  if (!sb) {
    agtPanel.setContent("{gray-fg}No agent selected{/}");
    return;
  }

  const sec = securityStates.get(sb.name);
  if (!sec) {
    agtPanel.setContent(`{bold}${sb.name}{/}\n{gray-fg}Polling...{/}`);
    return;
  }

  if (!sec.agtEnabled) {
    agtPanel.setContent("{gray-fg}AGT not enabled{/}\n{gray-fg}Use --governance flag{/}");
    return;
  }

  const mode = sec.egressMode === "enforcing" ? "{green-fg}enforcing{/}" : "{yellow-fg}learning{/}";
  // Same filter as the full panel: include known-sandbox peers
  // unconditionally; include AMID-only peers (e.g. cloud-offload parents)
  // only if they have real traffic + recent activity, to avoid showing
  // stale ghosts from failed KNOCKs or aged-out sessions.
  const recentThreshold = Date.now() - 30 * 60_000;
  const peers = sec.agtTrustScores.filter((t) => {
    if (t.agent === sb.name) return false;
    if (sandboxes.some((s) => s.name === t.agent)) return t.interactions > 0 || !!t.lastSeen;
    if (!t.lastSeen || t.interactions <= 0) return false;
    const seen = new Date(/^\d+Z$/.test(t.lastSeen) ? Number(t.lastSeen.slice(0, -1)) * 1000 : t.lastSeen);
    return !isNaN(seen.getTime()) && seen.getTime() > recentThreshold;
  });

  const lines: string[] = [
    `{bold}${sb.name}{/}` + (sec.agtAmid ? ` {gray-fg}${sec.agtAmid.substring(0, 12)}…{/}` : ""),
    ` ${mode}  ${sec.agtMeshSessions} sessions  ↑${sec.agtMeshSent} ↓${sec.agtMeshReceived}`,
    ` ${peers.length} peer${peers.length !== 1 ? "s" : ""}`,
  ];

  for (const t of peers) {
    const c = t.score >= 600 ? "green" : t.score >= 400 ? "yellow" : "red";
    const filled = Math.round(t.score / 100);
    const bar = "█".repeat(filled) + "░".repeat(4 - Math.min(filled, 4));
    lines.push(` {${c}-fg}${bar}{/} ${t.score} ${t.agent}`);
  }

  if (peers.length === 0) {
    lines.push(` {gray-fg}no peers yet{/}`);
  }

  lines.push(`{gray-fg}[g] full detail{/}`);

  agtPanel.setContent(lines.join("\n"));
}
