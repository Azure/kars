// kars Bridge Operator Console — Policies. Inventory of the governance config:
// connected MCP servers, tool policies, inference policies, and temporary
// egress approvals. All real reads via the operator API.

import { PageHeader, Section, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { Icon } from "@/components/icon";
import { AuthorResource } from "../author-resource";
import { InferencePolicyEditor } from "../inference-policy-editor";
import { DeleteResource } from "../delete-resource";
import { McpCatalog } from "../mcp-catalog";
import { PolicyBuilder } from "../policy-builder";
import { InferenceBudgets, InferenceBudgetEdit } from "@/components/inference-budgets";
import { RetentionPolicyPanel } from "@/components/retention-policy";
import { canAdminister } from "@/lib/session";
import {
  listMcpServers,
  listToolPolicies,
  listInferencePolicies,
  listEgress,
  getOptions,
} from "@/lib/bff";
import type {
  McpServer,
  ToolPolicy,
  InferencePolicy,
  EgressApproval,
  ModelOption,
} from "@/lib/types";

export const dynamic = "force-dynamic";

/** Settle a list fetch into data + an error flag, so a backend/RBAC/cluster
 *  failure renders as an explicit "couldn't load" state rather than being
 *  silently indistinguishable from a legitimately empty list. */
async function settle<T>(p: Promise<T[]>): Promise<{ data: T[]; error: boolean }> {
  try {
    return { data: await p, error: false };
  } catch {
    return { data: [], error: true };
  }
}

function countBadge(n: number) {
  return <Badge tone="muted">{n}</Badge>;
}

export default async function PoliciesPage() {
  const isAdmin = await canAdminister();
  const [mcpR, toolsR, inferenceR, egressR, options] = await Promise.all([
    settle<McpServer>(listMcpServers()),
    settle<ToolPolicy>(listToolPolicies()),
    settle<InferencePolicy>(listInferencePolicies()),
    settle<EgressApproval>(listEgress()),
    getOptions().catch(() => null),
  ]);
  const mcp = mcpR.data;
  const tools = toolsR.data;
  const inference = inferenceR.data;
  const egress = egressR.data;
  const models: ModelOption[] = options?.models ?? [];

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Policies"
        lead="The governance configuration agents run under — the services they may reach, the tools they may call, their inference limits, and any temporary egress widenings."
      />

      <Section
        title="Connected services (MCP)"
        action={<div className="flex items-center gap-2">{countBadge(mcp.length)}<AuthorResource kind="McpServer" /></div>}
      >
        <div className="mb-3"><McpCatalog /></div>
        {mcpR.error ? (
          <HonestState variant="not_wired" compact title="Couldn't load MCP servers" detail="The operator API for MCP servers is unreachable — this isn't the same as none being registered." />
        ) : mcp.length === 0 ? (
          <HonestState variant="empty" compact title="No MCP servers registered" detail="Pick one from the catalog above, or register a custom MCP server." />
        ) : (
          <ul className="space-y-2">
            {mcp.map((m) => (
              <li key={`${m.namespace}/${m.name}`} className="rounded-lg border border-border bg-surface px-4 py-3">
                <div className="flex items-center justify-between gap-3">
                  <span className="flex items-center gap-1.5">
                    <span aria-hidden><Icon name="plug" size={14} /></span>
                    <span className="font-mono text-sm font-medium">{m.name}</span>
                  </span>
                  <div className="flex items-center gap-2">
                    {m.production != null && <Badge tone={m.production ? "info" : "muted"}>{m.production ? "production" : "dev"}</Badge>}
                    {m.phase && <span className="text-xs text-foreground-muted">{m.phase}</span>}
                  </div>
                </div>
                {m.url && <p className="mt-1 font-mono text-xs text-foreground-muted">{m.url}</p>}
                {m.allowed_tools.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1">
                    {m.allowed_tools.map((t) => (
                      <span key={t} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[11px] text-foreground-muted">{t}</span>
                    ))}
                  </div>
                )}
                <div className="mt-2 flex items-center gap-3">
                  <AuthorResource kind="McpServer" initialName={m.name} initialSpec={JSON.stringify(m.spec, null, 2)} />
                  <DeleteResource kind="McpServer" name={m.name} label="MCP server" />
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section
        title="Tool policies"
        action={<div className="flex items-center gap-2">{countBadge(tools.length)}<AuthorResource kind="ToolPolicy" /></div>}
      >
        <div className="mb-3"><PolicyBuilder /></div>
        {toolsR.error ? (
          <HonestState variant="not_wired" compact title="Couldn't load tool policies" detail="The operator API for tool policies is unreachable — not the same as none existing." />
        ) : tools.length === 0 ? (
          <HonestState variant="empty" compact title="No tool policies" detail="Tool policies bound which tools an agent may call." />
        ) : (
          <ul className="space-y-2">
            {tools.map((t) => (
              <li key={`${t.namespace}/${t.name}`} className="flex items-start justify-between gap-3 rounded-lg border border-border bg-surface px-4 py-3">
                <div className="min-w-0">
                  <span className="font-mono text-sm font-medium">{t.name}</span>
                  <span className="ml-2 font-mono text-xs text-foreground-muted">{t.namespace}</span>
                  <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                    {t.applies_to && <Badge tone="muted">{t.applies_to}</Badge>}
                    {t.has_governance_profile && <Badge tone="info">AGT governance profile</Badge>}
                    {t.allowed.map((a) => (
                      <span key={a} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[11px] text-foreground-muted">{a}</span>
                    ))}
                  </div>
                  <div className="mt-2 flex items-center gap-3">
                    <AuthorResource kind="ToolPolicy" initialName={t.name} initialSpec={JSON.stringify(t.spec, null, 2)} />
                    <DeleteResource kind="ToolPolicy" name={t.name} label="tool policy" />
                  </div>
                </div>
                {t.phase && <span className="shrink-0 text-xs text-foreground-muted">{t.phase}</span>}
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section title="Inference budgets" subtitle="A hierarchy over inference token spend — cluster and per-workspace caps (editable), plus the per-sandbox policies. Passive alerts; buffer allows headroom then blocks; strict blocks at the limit." action={countBadge(inference.length)}>
        <InferenceBudgets isAdmin={isAdmin} />
        <div className="mt-5">
          <div className="flex items-center justify-between">
            <h4 className="text-xs font-semibold text-foreground-muted">Per-sandbox policies</h4>
            <InferencePolicyEditor models={models} />
          </div>
          <p className="mt-1 text-[11px] text-foreground-muted">
            Most are controller-generated from each mission&rsquo;s budget. You can also author a
            standalone policy (a selector + token budget + content-safety floor), edit a policy&rsquo;s
            daily budget in place, or remove an authored one.
          </p>
          {inferenceR.error ? (
            <HonestState variant="not_wired" compact title="Couldn't load inference policies" detail="The operator API for inference policies is unreachable." />
          ) : inference.length === 0 ? (
            <p className="mt-2 text-[11px] text-foreground-muted">No per-sandbox policies yet.</p>
          ) : (
            <div className="mt-2 overflow-hidden rounded-lg border border-border">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border bg-surface-muted/40 text-left text-xs text-foreground-muted">
                    <th className="px-4 py-2 font-medium">Name</th>
                    <th className="px-3 py-2 font-medium">Sandbox</th>
                    <th className="px-3 py-2 font-medium">Daily token budget</th>
                    <th className="px-3 py-2 font-medium">Content safety</th>
                    <th className="px-4 py-2 font-medium">Phase</th>
                    <th className="px-4 py-2 font-medium"></th>
                  </tr>
                </thead>
                <tbody>
                  {inference.map((p) => (
                    <tr key={`${p.namespace}/${p.name}`} className="border-b border-border last:border-0">
                      <td className="px-4 py-2 font-mono text-xs">{p.name}</td>
                      <td className="px-3 py-2 font-mono text-xs text-foreground-muted">{p.sandbox ?? "—"}</td>
                      <td className="px-3 py-2 tabular-nums">
                        <InferenceBudgetEdit name={p.name} current={p.daily_token_budget ?? null} />
                      </td>
                      <td className="px-3 py-2">{p.content_safety ? <Badge tone="ok">on</Badge> : <span className="text-foreground-muted">—</span>}</td>
                      <td className="px-4 py-2 text-xs text-foreground-muted">{p.phase ?? "—"}</td>
                      <td className="px-4 py-2 text-right">
                        <div className="flex items-center justify-end gap-2">
                          <InferencePolicyEditor initialName={p.name} initialSpec={p.spec} models={models} />
                          <DeleteResource kind="InferencePolicy" name={p.name} label="inference policy" />
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </Section>

      <Section title="Retention" subtitle="Auto-cleanup for delivered missions and team-run records — Kars keeps them by design for audit until this TTL elapses.">
        <RetentionPolicyPanel isAdmin={isAdmin} />
      </Section>

      <Section title="Temporary egress grants" action={countBadge(egress.length)}>
        {egressR.error ? (
          <HonestState variant="not_wired" compact title="Couldn't load egress grants" detail="The operator API for egress approvals is unreachable." />
        ) : egress.length === 0 ? (
          <HonestState
            variant="empty"
            compact
            title="No temporary egress grants"
            detail="Sandboxes run on their signed baseline allowlist. Temporary widenings appear here."
          />
        ) : (
          <ul className="space-y-2">
            {egress.map((e) => (
              <li key={`${e.namespace}/${e.name}`} className="rounded-lg border border-border bg-surface px-4 py-3">
                <div className="flex items-center justify-between gap-3">
                  <span className="font-mono text-sm font-medium">{e.sandbox ?? e.name}</span>
                  <span className="text-xs text-foreground-muted">{e.phase ?? "—"}</span>
                </div>
                {e.hosts.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1">
                    {e.hosts.map((h) => (
                      <span key={h} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[11px] text-foreground-muted">{h}</span>
                    ))}
                  </div>
                )}
                {e.reason && <p className="mt-1.5 text-xs text-foreground-muted">{e.reason}</p>}
                {e.expires_at && <p className="mt-0.5 text-xs text-foreground-muted">expires {new Date(e.expires_at).toLocaleString()}</p>}
                <div className="mt-2"><DeleteResource kind="EgressApproval" name={e.name} label="egress grant" verb="Revoke" /></div>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}
