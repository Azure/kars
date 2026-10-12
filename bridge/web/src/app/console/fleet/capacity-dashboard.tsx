// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Section } from "@/components/ui";
import type { ClusterCapacity, Sandbox } from "@/lib/types";

function formatCpu(value: number | null): string {
  if (value == null) return "—";
  return value >= 1000 ? `${(value / 1000).toFixed(1)} cores` : `${Math.round(value)}m`;
}

function formatMemory(value: number | null): string {
  if (value == null) return "—";
  return value >= 1024 ** 3
    ? `${(value / 1024 ** 3).toFixed(1)} GiB`
    : `${Math.round(value / 1024 ** 2)} MiB`;
}

function tone(percent: number): string {
  if (percent >= 85) return "var(--color-danger)";
  if (percent >= 70) return "var(--color-warning)";
  return "var(--color-ok)";
}

function total(values: (number | null)[]): number | null {
  if (!values.length || values.some((value) => value == null || !Number.isFinite(value) || value < 0)) return null;
  const sum = values.reduce<number>((sum, value) => sum + (value ?? 0), 0);
  return Number.isFinite(sum) ? sum : null;
}

function percentage(used: number | null, capacity: number | null): number | null {
  return used != null && capacity != null && capacity > 0 ? used / capacity * 100 : null;
}

function Gauge({
  label,
  percent,
  detail,
}: {
  label: string;
  percent: number | null;
  detail: string;
}) {
  const bounded = Math.max(0, Math.min(100, percent ?? 0));
  const color = percent == null ? "var(--color-border)" : tone(percent);
  return (
    <div className="flex items-center gap-4 rounded-xl border border-border bg-surface-muted/30 p-4">
      <div
        className="grid h-24 w-24 shrink-0 place-items-center rounded-full"
        style={{
          background: `conic-gradient(${color} ${bounded * 3.6}deg, color-mix(in srgb, var(--color-border) 65%, transparent) 0deg)`,
        }}
      >
        <div className="grid h-16 w-16 place-items-center rounded-full bg-surface">
          <span className="text-lg font-semibold tabular-nums">{percent == null ? "—" : `${percent.toFixed(1)}%`}</span>
        </div>
      </div>
      <div>
        <p className="font-medium">{label}</p>
        <p className="mt-1 text-xs text-foreground-muted">{detail}</p>
        <p className="mt-2 text-xs font-medium" style={{ color: percent == null ? "var(--color-foreground-muted)" : color }}>
          {percent == null ? "Complete samples unavailable" : percent >= 85 ? "High pressure" : percent >= 70 ? "Watch" : "Healthy headroom"}
        </p>
      </div>
    </div>
  );
}

function Bar({ value, color }: { value: number | null; color: string }) {
  return (
    <div aria-hidden="true" className="h-2 overflow-hidden rounded-full bg-surface-muted">
      <div
        className="h-full rounded-full transition-[width] motion-reduce:transition-none"
        style={{ width: `${Math.max(0, Math.min(100, value ?? 0))}%`, backgroundColor: color }}
      />
    </div>
  );
}

export function CapacityDashboard({
  capacity,
  sandboxes,
}: {
  capacity: ClusterCapacity;
  sandboxes: Sandbox[];
}) {
  const nodes = capacity.nodes.map((node) => ({
    ...node,
    cpu_percent: percentage(node.cpu_usage_millicores, node.cpu_allocatable_millicores),
    memory_percent: percentage(node.memory_usage_bytes, node.memory_allocatable_bytes),
  }));
  const cpuUsed = total(nodes.map((node) => node.cpu_usage_millicores));
  const cpuTotal = total(nodes.map((node) => node.cpu_allocatable_millicores));
  const memoryUsed = total(nodes.map((node) => node.memory_usage_bytes));
  const memoryTotal = total(nodes.map((node) => node.memory_allocatable_bytes));
  const cpuPercent = percentage(cpuUsed, cpuTotal);
  const memoryPercent = percentage(memoryUsed, memoryTotal);
  const activeRuns = capacity.active_team_runs;

  const teams = new Map<string, { cpu: (number | null)[]; memory: (number | null)[]; sandboxes: number; executing: number }>();
  for (const sandbox of sandboxes) {
    const key = sandbox.team ?? (sandbox.parent ? "unattributed sub-agents" : "standalone");
    const current = teams.get(key) ?? { cpu: [], memory: [], sandboxes: 0, executing: 0 };
    current.cpu.push(sandbox.cpu_millicores);
    current.memory.push(sandbox.memory_bytes);
    current.sandboxes += 1;
    if (sandbox.executing) current.executing += 1;
    teams.set(key, current);
  }
  const teamRows = Array.from(teams.entries())
    .map(([name, usage]) => ({
      name, ...usage, cpu: total(usage.cpu), memory: total(usage.memory),
      samples: usage.cpu.filter((cpu, index) => cpu != null && usage.memory[index] != null).length,
    }))
    .sort((a, b) => (b.memory ?? -1) - (a.memory ?? -1));
  const maxTeamCpu = Math.max(1, ...teamRows.map((team) => team.cpu ?? 0));
  const maxTeamMemory = Math.max(1, ...teamRows.map((team) => team.memory ?? 0));

  return (
    <Section className="overflow-hidden">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="text-xs font-medium uppercase tracking-[0.14em] text-foreground-muted">Fleet resources</p>
          <h2 className="mt-1 text-lg font-semibold">Cluster & agent utilization</h2>
          <p className="mt-1 text-xs text-foreground-muted">
            Kubernetes resource samples alongside observed team activity.
          </p>
        </div>
        <div className="flex gap-2 text-xs">
          <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1">
            {activeRuns == null ? "Team activity unavailable" : `${activeRuns} active team runs`}
          </span>
        </div>
      </div>

      {capacity.activity_error && (
        <p role="status" className="mt-5 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning">
          {capacity.activity_error}
        </p>
      )}
      {!capacity.metrics_available && (
        <p role="status" className="mt-5 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning">
          {capacity.metrics_error ?? "Live resource samples are unavailable. Check the Kubernetes Metrics API."}
        </p>
      )}
          <div className="mt-5 grid gap-3 lg:grid-cols-2">
            <Gauge
              label="Cluster CPU"
              percent={cpuPercent}
              detail={`${formatCpu(cpuUsed)} used of ${formatCpu(cpuTotal)}`}
            />
            <Gauge
              label="Cluster memory"
              percent={memoryPercent}
              detail={`${formatMemory(memoryUsed)} used of ${formatMemory(memoryTotal)}`}
            />
          </div>

          <div className="mt-5 grid grid-cols-1 gap-5 xl:grid-cols-[minmax(0,1.05fr)_minmax(0,1fr)]">
            <div className="min-w-0">
              <h3 className="text-sm font-semibold">Node pressure</h3>
              <div className="mt-3 space-y-3">
                {nodes.map((node) => (
                  <div key={node.name} className="rounded-lg border border-border p-3">
                    <p className="font-mono text-xs font-medium [overflow-wrap:anywhere]">{node.name}</p>
                    <div className="mt-3 grid gap-3 sm:grid-cols-2">
                      <div className="min-w-0">
                        <div className="mb-1 flex justify-between text-[11px]">
                          <span className="text-foreground-muted">CPU</span>
                          <span>{node.cpu_percent == null ? "—" : `${node.cpu_percent.toFixed(1)}%`}</span>
                        </div>
                        <Bar value={node.cpu_percent} color={tone(node.cpu_percent ?? 0)} />
                        <p className="mt-1 text-[10px] text-foreground-muted">
                          {formatCpu(node.cpu_usage_millicores)} / {formatCpu(node.cpu_allocatable_millicores)}
                        </p>
                      </div>
                      <div>
                        <div className="mb-1 flex justify-between text-[11px]">
                          <span className="text-foreground-muted">Memory</span>
                          <span>{node.memory_percent == null ? "—" : `${node.memory_percent.toFixed(1)}%`}</span>
                        </div>
                        <Bar value={node.memory_percent} color={tone(node.memory_percent ?? 0)} />
                        <p className="mt-1 text-[10px] text-foreground-muted">
                          {formatMemory(node.memory_usage_bytes)} / {formatMemory(node.memory_allocatable_bytes)}
                        </p>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </div>

            <div>
              <h3 className="text-sm font-semibold">Sandbox footprint by team</h3>
              {!capacity.pod_metrics_available && (
                <p className="mt-3 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
                  {capacity.pod_metrics_error ?? "Pod utilization samples are unavailable."}
                </p>
              )}
              {teamRows.length === 0 && (
                <p className="mt-3 text-xs text-foreground-muted">No sandbox workloads to measure.</p>
              )}
                <div className="mt-3 space-y-3">
                  {teamRows.map((team) => (
                  <div key={team.name} className="rounded-lg border border-border p-3">
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <p className="truncate text-xs font-medium">{team.name}</p>
                        <p className="mt-0.5 text-[10px] text-foreground-muted">
                          {team.sandboxes} sandboxes · {team.executing} confirmed executing
                        </p>
                        <p className="mt-0.5 text-[10px] text-foreground-muted">
                          Complete samples: {team.samples}/{team.sandboxes}
                        </p>
                      </div>
                      <p className="shrink-0 text-[10px] text-foreground-muted">
                        {formatCpu(team.cpu)} · {formatMemory(team.memory)}
                      </p>
                    </div>
                    <div className="mt-2 grid gap-2">
                      <Bar value={percentage(team.cpu, maxTeamCpu)} color="var(--color-accent)" />
                      <Bar value={percentage(team.memory, maxTeamMemory)} color="var(--color-foreground-muted)" />
                    </div>
                  </div>
                  ))}
                </div>
            </div>
          </div>
    </Section>
  );
}
