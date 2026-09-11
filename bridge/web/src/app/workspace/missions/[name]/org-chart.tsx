"use client";

// kars Bridge Workspace — the org chart: principal + delegated roles, and an
// "add a role" composer constrained to the principal's authority (§12).

import { useState } from "react";
import Link from "next/link";
import { MissionStatusBadge, missionStatus } from "@/components/mission-status";
import { addRole, type AddRoleInput } from "./role-actions";
import { TIER_LABELS, type BlueprintEgress, type TaskDetail, type TaskSummary } from "@/lib/types";

function parseEgress(items: string[]): BlueprintEgress[] {
  return items.map((s) => {
    const [host, port] = s.split(":");
    const p = port ? Number(port) : null;
    return { host, port: Number.isFinite(p as number) ? (p as number) : null };
  });
}

export function OrgChart({ task }: { task: TaskDetail }) {
  const [adding, setAdding] = useState(false);
  const principalEgress = parseEgress(task.composition?.egress ?? []);
  const ceiling = task.envelope.authority_ceiling;
  const canDelegate = task.envelope.delegation_depth > 0;

  // Roles share a long `<team>-<role>` naming prefix (e.g.
  // `kars-repo-health-ci-reporter`); strip it so nodes read as `ci-reporter`
  // instead of a wall of truncated "Kars r…". Derived from the principal name.
  const teamPrefix = task.name.replace(/-principal$/, "");
  const shorten = (n: string): string => {
    const s = n.startsWith(`${teamPrefix}-`) ? n.slice(teamPrefix.length + 1) : n;
    return s.length > 0 ? s : n;
  };

  // Auto-fold: a legible org chart shows a bounded set of roles; the rest
  // collapse into a "+N more" node rather than an endless grid.
  const MAX_ROLES = 6;
  const roles = task.children;
  const shownRoles = roles.slice(0, MAX_ROLES);
  const foldedRoles = Math.max(0, roles.length - shownRoles.length);

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Org chart</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            The team working on this mission. Each role&apos;s authority is a verified subset of the
            mission&apos;s — the reporting line is the trust boundary.
          </p>
        </div>
        {canDelegate && !adding && (
          <button
            type="button"
            onClick={() => setAdding(true)}
            className="shrink-0 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted"
          >
            + Add a role
          </button>
        )}
      </div>

      {/* Visual org tree: principal on top, a connector spine, then the
          delegated roles as a connected row of detail nodes. */}
      <div className="mt-5 flex flex-col items-center">
        <PrincipalNode
          name={task.display_name ?? "Lead"}
          tier={task.envelope.tier}
          model={task.composition?.model ?? null}
          harness={task.composition?.runtime ?? null}
          phase={task.phase}
          ceiling={ceiling}
          canDelegate={canDelegate}
        />

        {(roles.length > 0 || task.sub_agents.length > 0) && (
          <div className="h-5 w-px bg-border" aria-hidden />
        )}

        {roles.length > 0 && (
          <div className="relative w-full">
            {/* horizontal bus connecting the children */}
            {shownRoles.length + (foldedRoles > 0 ? 1 : 0) > 1 && (
              <div className="mx-auto mb-0 h-px bg-border" style={{ width: "80%" }} aria-hidden />
            )}
            <ul className="flex flex-wrap items-stretch justify-center gap-4">
              {shownRoles.map((c) => (
                <li key={c.name} className="flex flex-col items-center">
                  <div className="h-4 w-px bg-border" aria-hidden />
                  <RoleNode role={c} label={shorten(c.display_name ?? c.name)} />
                </li>
              ))}
              {foldedRoles > 0 && (
                <li className="flex flex-col items-center">
                  <div className="h-4 w-px bg-border" aria-hidden />
                  <div className="grid w-52 place-items-center rounded-xl border border-dashed border-border bg-surface-muted/40 px-3 py-2.5 text-xs text-foreground-muted">
                    +{foldedRoles} more role{foldedRoles === 1 ? "" : "s"}
                  </div>
                </li>
              )}
            </ul>
          </div>
        )}

        {roles.length === 0 && task.sub_agents.length === 0 && (
          <p className="mt-2 px-1 text-center text-xs text-foreground-muted">
            {canDelegate
              ? "No reports yet. Add a role to delegate part of this mission — its authority will be a verified subset of the mission's."
              : "This mission can't delegate further (no delegation budget left)."}
          </p>
        )}

        {/* Runtime agents/sub-agents — the agent actually running + any it
            spawned over the mesh (distinct from the governed roles above). */}
        {task.sub_agents.length > 0 && (
          <div className="mt-5 w-full border-t border-border pt-4">
            <p className="text-center text-xs font-medium text-foreground-muted">Running agents</p>
            <p className="text-center text-[11px] text-foreground-muted">
              The agent in flight and the sub-agents it spawned to help.
            </p>
            <ul className="mt-3 flex flex-wrap items-center justify-center gap-3">
              {task.sub_agents.map((a) => (
                <li
                  key={`${a.namespace}/${a.name}`}
                  className="flex items-center gap-2 rounded-lg border border-signal/30 bg-signal/[0.04] px-3 py-1.5"
                >
                  <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${a.phase === "Running" ? "bg-ok kb-pulse" : "bg-foreground-muted"}`} aria-hidden />
                  <span className="truncate text-sm font-medium">{a.name}</span>
                  <span className="text-xs text-foreground-muted">
                    {a.runtime ?? "agent"} · {a.phase ?? "—"}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>

      {adding && (
        <AddRoleForm
          principal={task.name}
          principalTier={task.envelope.tier}
          principalCeiling={ceiling}
          principalDelegationDepth={task.envelope.delegation_depth}
          toolPolicy={task.composition?.tool_policy ?? null}
          principalEgress={principalEgress}
          onClose={() => setAdding(false)}
        />
      )}
    </section>
  );
}

function PrincipalNode({
  name,
  tier,
  model,
  harness,
  phase,
  ceiling,
  canDelegate,
}: {
  name: string;
  tier: number;
  model: string | null;
  harness: string | null;
  phase: string;
  ceiling: number;
  canDelegate: boolean;
}) {
  return (
    <div className="w-full max-w-sm rounded-xl border border-signal/40 bg-signal/[0.06] px-4 py-3 shadow-sm">
      <div className="flex items-center justify-between gap-2">
        <p className="truncate text-sm font-semibold">{name}</p>
        <span className="rounded-full bg-signal/15 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-signal">
          Principal
        </span>
      </div>
      <div className="mt-2 flex flex-wrap gap-1.5">
        <NodeChip label={`Tier ${tier} · ${TIER_LABELS[tier] ?? "?"}`} tone="signal" />
        {harness && <NodeChip label={harness} tone="accent" />}
        {model && <NodeChip label={model} />}
        <NodeChip label={canDelegate ? `grants up to Tier ${ceiling}` : "operates alone"} />
      </div>
      {phase === "Degraded" && (
        <p className="mt-2 text-[11px] font-medium text-danger">authority rejected — degraded</p>
      )}
    </div>
  );
}

function RoleNode({ role, label }: { role: TaskSummary; label: string }) {
  const degraded = role.phase === "Degraded";
  return (
    <Link
      href={`/workspace/missions/${encodeURIComponent(role.name)}`}
      title={role.display_name ?? role.name}
      className="block w-52 rounded-xl border border-border bg-surface px-3 py-2.5 shadow-sm transition hover:-translate-y-0.5 hover:border-signal/40 hover:shadow-md"
    >
      <div className="flex items-start justify-between gap-2">
        <p className="min-w-0 truncate text-sm font-medium">{label}</p>
        <MissionStatusBadge status={missionStatus(role.phase)} />
      </div>
      <div className="mt-1.5 flex flex-wrap gap-1.5">
        <NodeChip label={`Tier ${role.tier} · ${TIER_LABELS[role.tier] ?? "?"}`} />
        {degraded && <NodeChip label="authority rejected" tone="danger" />}
      </div>
    </Link>
  );
}

function NodeChip({ label, tone = "muted" }: { label: string; tone?: "muted" | "signal" | "accent" | "danger" }) {
  const cls = {
    muted: "border-border bg-surface-muted text-foreground-muted",
    signal: "border-signal/30 bg-signal/10 text-signal",
    accent: "border-accent/30 bg-accent/10 text-accent",
    danger: "border-danger/30 bg-danger/10 text-danger",
  }[tone];
  return (
    <span className={`rounded border px-1.5 py-0.5 text-[10px] font-medium ${cls}`}>{label}</span>
  );
}

function AddRoleForm({
  principal,
  principalTier,
  principalCeiling,
  principalDelegationDepth,
  toolPolicy,
  principalEgress,
  onClose,
}: {
  principal: string;
  principalTier: number;
  principalCeiling: number;
  principalDelegationDepth: number;
  toolPolicy: string | null;
  principalEgress: BlueprintEgress[];
  onClose: () => void;
}) {
  const [roleName, setRoleName] = useState("");
  const [objective, setObjective] = useState("");
  const [tier, setTier] = useState(Math.min(principalCeiling, 2));
  const [instructions, setInstructions] = useState("");
  const [egress, setEgress] = useState<string[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const egressKey = (e: BlueprintEgress) => `${e.host}${e.port ? `:${e.port}` : ""}`;

  async function submit() {
    setPending(true);
    setError(null);
    const input: AddRoleInput = {
      principal,
      principalTier,
      principalCeiling,
      principalDelegationDepth,
      toolPolicy,
      roleName,
      objective,
      tier,
      instructions,
      egress: principalEgress.filter((e) => egress.includes(egressKey(e))),
    };
    const res = await addRole(input);
    setPending(false);
    if (res.error) {
      setError(res.error);
      return;
    }
    onClose();
  }

  return (
    <div className="mt-4 space-y-3 rounded-xl border border-border bg-surface-muted/40 p-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">Add a role</h3>
        <button type="button" onClick={onClose} className="text-xs text-foreground-muted hover:text-foreground">
          Cancel
        </button>
      </div>
      <p className="text-xs text-foreground-muted">
        This role reports to the mission. Its authority is bounded by the mission: at most Tier{" "}
        {principalCeiling}
        {toolPolicy ? `, the same tool policy (${toolPolicy})` : ""}, and only the network
        destinations the mission already holds.
      </p>

      <div className="grid gap-3 sm:grid-cols-2">
        <label className="block">
          <span className="text-xs font-medium text-foreground-muted">Role name</span>
          <input
            value={roleName}
            onChange={(e) => setRoleName(e.target.value)}
            placeholder="e.g. Test-coverage engineer"
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          />
        </label>
        <label className="block">
          <span className="text-xs font-medium text-foreground-muted">Autonomy (≤ Tier {principalCeiling})</span>
          <select
            value={tier}
            onChange={(e) => setTier(Number(e.target.value))}
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          >
            {Array.from({ length: principalCeiling }, (_, i) => i + 1).map((t) => (
              <option key={t} value={t}>
                Tier {t} · {TIER_LABELS[t] ?? "?"}
              </option>
            ))}
          </select>
        </label>
      </div>

      <label className="block">
        <span className="text-xs font-medium text-foreground-muted">What this role does</span>
        <textarea
          value={objective}
          onChange={(e) => setObjective(e.target.value)}
          rows={2}
          placeholder="e.g. Raise test coverage in the CLI package and open PRs."
          className="mt-1 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
      </label>

      <label className="block">
        <span className="text-xs font-medium text-foreground-muted">Instructions (optional)</span>
        <textarea
          value={instructions}
          onChange={(e) => setInstructions(e.target.value)}
          rows={2}
          className="mt-1 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
      </label>

      {principalEgress.length > 0 && (
        <div>
          <span className="text-xs font-medium text-foreground-muted">
            Network access (subset of the mission&apos;s)
          </span>
          <ul className="mt-1 space-y-1">
            {principalEgress.map((e) => {
              const k = egressKey(e);
              return (
                <li key={k}>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={egress.includes(k)}
                      onChange={(ev) =>
                        setEgress((cur) => (ev.target.checked ? [...cur, k] : cur.filter((x) => x !== k)))
                      }
                      className="h-4 w-4 accent-[var(--signal)]"
                    />
                    <span className="font-mono text-xs">{k}</span>
                  </label>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      {error && (
        <p role="alert" className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          {error}
        </p>
      )}

      <div className="flex items-center justify-end gap-2">
        <button
          type="button"
          disabled={pending}
          onClick={submit}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
        >
          {pending ? "Adding…" : "Add role"}
        </button>
      </div>
    </div>
  );
}
