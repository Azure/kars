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
  if (percent >= 85) return "#ef4444";
  if (percent >= 70) return "#f59e0b";
  return "#10b981";
}

function Gauge({
  label,
  percent,
  detail,
}: {
  label: string;
  percent: number;
  detail: string;
}) {
  const bounded = Math.max(0, Math.min(100, percent));
  const color = tone(bounded);
  return (
    <div className="flex items-center gap-4 rounded-xl border border-border bg-surface-muted/30 p-4">
      <div
        className="grid h-24 w-24 shrink-0 place-items-center rounded-full"
        style={{
          background: `conic-gradient(${color} ${bounded * 3.6}deg, color-mix(in srgb, var(--color-border) 65%, transparent) 0deg)`,
        }}
      >
        <div className="grid h-16 w-16 place-items-center rounded-full bg-surface">
          <span className="text-lg font-semibold tabular-nums">{bounded.toFixed(1)}%</span>
        </div>
      </div>
      <div>
        <p className="font-medium">{label}</p>
        <p className="mt-1 text-xs text-foreground-muted">{detail}</p>
        <p className="mt-2 text-xs font-medium" style={{ color }}>
          {bounded >= 85 ? "High pressure" : bounded >= 70 ? "Watch" : "Healthy headroom"}
        </p>
      </div>
    </div>
  );
}

function Bar({ value, color }: { value: number; color: string }) {
  return (
    <div className="h-2 overflow-hidden rounded-full bg-surface-muted">
      <div
        className="h-full rounded-full transition-[width]"
        style={{ width: `${Math.max(1, Math.min(100, value))}%`, backgroundColor: color }}
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
  const cpuUsed = capacity.nodes.reduce((sum, node) => sum + (node.cpu_usage_millicores ?? 0), 0);
  const cpuTotal = capacity.nodes.reduce((sum, node) => sum + (node.cpu_allocatable_millicores ?? 0), 0);
  const memoryUsed = capacity.nodes.reduce((sum, node) => sum + (node.memory_usage_bytes ?? 0), 0);
  const memoryTotal = capacity.nodes.reduce((sum, node) => sum + (node.memory_allocatable_bytes ?? 0), 0);
  const cpuPercent = cpuTotal > 0 ? (cpuUsed / cpuTotal) * 100 : 0;
  const memoryPercent = memoryTotal > 0 ? (memoryUsed / memoryTotal) * 100 : 0;
  const activeRuns = capacity.active_team_runs;
  const admissionPercent = capacity.global_active_runs_limit > 0
    ? (activeRuns / capacity.global_active_runs_limit) * 100
    : 0;

  const teams = new Map<string, { cpu: number; memory: number; sandboxes: number; executing: number }>();
  for (const sandbox of sandboxes) {
    const key = sandbox.team ?? (sandbox.parent ? "unattributed sub-agents" : "standalone");
    const current = teams.get(key) ?? { cpu: 0, memory: 0, sandboxes: 0, executing: 0 };
    current.cpu += sandbox.cpu_millicores ?? 0;
    current.memory += sandbox.memory_bytes ?? 0;
    current.sandboxes += 1;
    if (sandbox.executing) current.executing += 1;
    teams.set(key, current);
  }
  const teamRows = Array.from(teams.entries())
    .map(([name, usage]) => ({ name, ...usage }))
    .sort((a, b) => b.memory - a.memory);
  const maxTeamCpu = Math.max(1, ...teamRows.map((team) => team.cpu));
  const maxTeamMemory = Math.max(1, ...teamRows.map((team) => team.memory));

  return (
    <Section className="overflow-hidden">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="text-xs font-medium uppercase tracking-[0.14em] text-foreground-muted">Capacity control</p>
          <h2 className="mt-1 text-lg font-semibold">Cluster & agent utilization</h2>
          <p className="mt-1 text-xs text-foreground-muted">
            Live Metrics API usage correlated with teams, runs, and admission slots.
          </p>
        </div>
        <div className="flex gap-2 text-xs">
          <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1">
            {capacity.team_max_concurrent_runs} runs / team
          </span>
          <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1">
            {activeRuns} / {capacity.global_active_runs_limit} global slots
          </span>
        </div>
      </div>

      {!capacity.metrics_available ? (
        <p className="mt-5 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning">
          Live utilization unavailable: {capacity.metrics_error ?? "metrics API is not available"}.
        </p>
      ) : (
        <>
          <div className="mt-5 grid gap-3 lg:grid-cols-3">
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
            <Gauge
              label="Team-run admission"
              percent={admissionPercent}
              detail={`${activeRuns} executing leads · ${Math.max(0, capacity.global_active_runs_limit - activeRuns)} slots free`}
            />
          </div>

          <div className="mt-5 grid gap-5 xl:grid-cols-[1.05fr_1fr]">
            <div>
              <h3 className="text-sm font-semibold">Node pressure</h3>
              <div className="mt-3 space-y-3">
                {capacity.nodes.map((node) => (
                  <div key={node.name} className="rounded-lg border border-border p-3">
                    <p className="truncate font-mono text-xs font-medium">{node.name}</p>
                    <div className="mt-3 grid gap-3 sm:grid-cols-2">
                      <div>
                        <div className="mb-1 flex justify-between text-[11px]">
                          <span className="text-foreground-muted">CPU</span>
                          <span>{node.cpu_percent?.toFixed(1) ?? "—"}%</span>
                        </div>
                        <Bar value={node.cpu_percent ?? 0} color={tone(node.cpu_percent ?? 0)} />
                        <p className="mt-1 text-[10px] text-foreground-muted">
                          {formatCpu(node.cpu_usage_millicores)} / {formatCpu(node.cpu_allocatable_millicores)}
                        </p>
                      </div>
                      <div>
                        <div className="mb-1 flex justify-between text-[11px]">
                          <span className="text-foreground-muted">Memory</span>
                          <span>{node.memory_percent?.toFixed(1) ?? "—"}%</span>
                        </div>
                        <Bar value={node.memory_percent ?? 0} color={tone(node.memory_percent ?? 0)} />
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
              {!capacity.pod_metrics_available ? (
                <p className="mt-3 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
                  Pod utilization unavailable: {capacity.pod_metrics_error ?? "no pod metrics"}.
                </p>
              ) : (
                <div className="mt-3 space-y-3">
                  {teamRows.map((team) => (
                  <div key={team.name} className="rounded-lg border border-border p-3">
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <p className="truncate text-xs font-medium">{team.name}</p>
                        <p className="mt-0.5 text-[10px] text-foreground-muted">
                          {team.sandboxes} sandboxes · {team.executing} executing
                        </p>
                      </div>
                      <p className="shrink-0 text-[10px] text-foreground-muted">
                        {formatCpu(team.cpu)} · {formatMemory(team.memory)}
                      </p>
                    </div>
                    <div className="mt-2 grid gap-2">
                      <Bar value={(team.cpu / maxTeamCpu) * 100} color="#38bdf8" />
                      <Bar value={(team.memory / maxTeamMemory) * 100} color="#a78bfa" />
                    </div>
                  </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        </>
      )}
    </Section>
  );
}
