"use client";

// kars Bridge — Composer reveal. The moment that makes kars legible: after an
// objective is given, the trust envelope is shown being ASSEMBLED facet by facet
// — model, harness, instructions, tools, connected services, egress, isolation,
// memory, autonomy, budget — each with a one-line rationale and (where relevant)
// an efficiency hint drawn from the learned frontier. Staggered entrance so the
// user literally watches the governed package compose itself.

import type { Blueprint, MissionDelegation } from "@/lib/types";
import { Icon } from "@/components/icon";
import { humanizeMcp } from "@/lib/format";

type Facet = { icon: import("@/components/icon").IconName; label: string; value: string; why?: string; tone?: "accent" | "signal" | "muted" };

const TIER_WORD: Record<number, string> = { 1: "Manual", 2: "Shared", 3: "Conditional", 4: "Supervised", 5: "Full" };

export function EnvelopeReveal({
  blueprint,
  tier,
  budgetTokens,
  rationale,
  source,
  recommended,
  modelBasis,
  delegation,
}: {
  blueprint: Blueprint;
  tier: number;
  budgetTokens?: string;
  rationale?: string | null;
  source?: string | null;
  recommended?: string | null;
  modelBasis?: string | null;
  delegation: MissionDelegation;
}) {
  const model = blueprint.model ? `${blueprint.model.deployment}` : "controller default";
  // Prefer the real, data-grounded basis from the orchestrator (efficiency
  // frontier / objective fit); fall back to a heuristic only when absent.
  const modelWhy = modelBasis
    ? modelBasis
    : recommended && blueprint.model?.deployment === recommended
    ? "Top of the efficiency frontier — cheapest per accepted outcome."
    : "Fits the objective's reasoning load.";
  const executionPlan = blueprint.execution_plan;
  const executionWhy = executionPlan
    ? executionPlan.roles
        .map((role) => {
          const phases = role.phases
            .map((phase) => {
              const capabilities = phase.capabilities.length
                ? phase.capabilities.join("+")
                : "reasoning-only";
              return `${phase.name}:${capabilities}/${phase.max_tool_calls}`;
            })
            .join(", ");
          return `${role.name} [${phases}]`;
        })
        .join(" · ")
    : undefined;

  const facets: Facet[] = [
    { icon: "brain", label: "Model", value: model, why: modelWhy, tone: "accent" },
    { icon: "gear", label: "Harness", value: blueprint.runtime ?? "OpenClaw", why: "Verified runtime on this cluster." },
    { icon: "target", label: "Autonomy", value: `Tier ${tier} · ${TIER_WORD[tier] ?? "?"}`, why: tier <= 3 ? "Pauses before anything costly or irreversible." : "Acts autonomously within the envelope.", tone: "signal" },
    {
      icon: "branch",
      label: "Execution strategy",
      value:
        executionPlan
          ? `Principal + ${executionPlan.roles.length} planned workers`
          : delegation.mode === "principal-specialists"
          ? `Principal + ${delegation.roles.length} legacy leaf specialists`
          : "Single agent",
      why:
        executionPlan
          ? `${executionWhy} · up to ${executionPlan.max_parallel} in parallel.`
          : delegation.mode === "principal-specialists"
          ? `${delegation.roles.map((role) => role.name).join(", ")} · up to ${delegation.max_parallel} in parallel.`
          : "Best for small, tightly coupled work where delegation would add overhead.",
    },
    { icon: "wrench", label: "Tool policy", value: blueprint.tool_policy ?? "none — model only", why: blueprint.tool_policy ? "Bounds every tool the agent may call." : "No tools — pure reasoning." },
    { icon: "plug", label: "Connected services", value: blueprint.mcp_servers?.length ? blueprint.mcp_servers.map(humanizeMcp).join(", ") : "none", why: blueprint.mcp_servers?.length ? "MCP servers the agent may reach, bounded by the tool policy." : undefined },
    { icon: "globe", label: "Network egress", value: blueprint.egress?.length ? blueprint.egress.map((e) => e.host + (e.port ? `:${e.port}` : "")).join(", ") : "model path only", why: blueprint.egress?.length ? "Exact host:port destinations allowed at the network boundary; everything else is denied." : "Default-deny — only the model path is reachable." },
    { icon: "shield", label: "Isolation", value: blueprint.isolation ?? "standard", why: "Sandbox hardening level." },
    { icon: "database", label: "Shared memory", value: blueprint.memory ?? "none", why: blueprint.memory ? "Knowledge commons the mission reads + writes." : undefined },
    { icon: "coin", label: "Budget", value: budgetTokens ? `${Number(budgetTokens).toLocaleString()} tokens` : "no cap", why: "Hard ceiling on spend." },
  ];

  return (
    <div className="kb-card overflow-hidden">
      <div className="flex items-center justify-between gap-3 border-b border-border bg-surface-muted/40 px-5 py-3">
        <div className="flex items-center gap-2">
          <span className="grid h-6 w-6 place-items-center rounded-md bg-accent/15 text-[11px] font-semibold text-accent">kb</span>
          <h2 className="text-sm font-semibold">Trust envelope composed</h2>
        </div>
        {source && <span className="rounded-full bg-surface px-2 py-0.5 font-mono text-[11px] text-foreground-muted">{source}</span>}
      </div>

      {rationale && (
        <p className="kb-rise border-b border-border px-5 py-3 text-sm text-foreground-muted">{rationale}</p>
      )}

      <ul className="kb-stagger divide-y divide-border">
        {facets.map((f) => (
          <li key={f.label} className="flex items-start gap-3 px-5 py-3">
            <span className="mt-0.5 text-foreground-muted" aria-hidden><Icon name={f.icon} /></span>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-2">
                <span className="text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">{f.label}</span>
                <span className={`text-sm font-medium ${f.tone === "accent" ? "text-accent" : f.tone === "signal" ? "text-signal" : ""}`}>{f.value}</span>
              </div>
              {f.why && <p className="mt-0.5 text-xs text-foreground-muted">{f.why}</p>}
            </div>
          </li>
        ))}
      </ul>

      <p className="border-t border-border bg-surface-muted/30 px-5 py-3 text-xs text-foreground-muted">
        This is a proposal. Review and edit every field below — the pre-flight validation gate and the explicit Launch step still govern what runs. Nothing has started.
      </p>
    </div>
  );
}
