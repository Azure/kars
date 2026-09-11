"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { RepoAccess } from "@/components/repo-access";
import type { ExecutionPlan, TeamLifecycleMode } from "@/lib/types";

type McpOption = { name: string; summary?: string | null };
type MemoryOption = { name: string };

function parseEgress(value: string): { host: string; port?: number }[] {
  return value
    .split(/\r?\n|,/)
    .map((entry) => entry.trim())
    .filter(Boolean)
    .flatMap((entry) => {
      const match = entry.match(/^([^:]+?)(?::(\d{1,5}))?$/);
      if (!match) return [];
      const port = match[2] ? Number(match[2]) : undefined;
      if (port != null && (port < 1 || port > 65535)) return [];
      return [{ host: match[1].toLowerCase(), ...(port ? { port } : {}) }];
    });
}

function moveRoute(routes: string[], index: number, delta: number): string[] {
  const next = index + delta;
  if (next < 0 || next >= routes.length) return routes;
  const copy = [...routes];
  [copy[index], copy[next]] = [copy[next], copy[index]];
  return copy;
}

// Day-to-day, non-amplifying launch-package edits. Tier changes still go through
// governed promote; charter, cadence, MCP and network intent stay human-editable.
export function TeamEdit({
  ns,
  name,
  charter,
  paused,
  everyMinutes,
  lifecycleMode,
  warmIdleSeconds,
  mcpServers,
  availableMcp,
  egress,
  egressMode,
  model,
  modelFallbacks,
  models,
  memory,
  availableMemories,
  gitWriteRepos,
  executionPlan,
}: {
  ns: string;
  name: string;
  charter: string;
  paused: boolean;
  everyMinutes: number | null;
  lifecycleMode: TeamLifecycleMode;
  warmIdleSeconds: number | null;
  mcpServers: string[];
  availableMcp: McpOption[];
  egress: string[];
  egressMode: string | null;
  model: string | null;
  modelFallbacks: string[];
  models: { provider: string; deployment: string; is_default?: boolean }[];
  memory: string | null;
  availableMemories: MemoryOption[];
  gitWriteRepos: string[];
  executionPlan: ExecutionPlan | null;
}) {
  const [open, setOpen] = useState(false);
  const [ch, setCh] = useState(charter);
  const [cad, setCad] = useState(everyMinutes ?? 0);
  const [lifecycle, setLifecycle] = useState<TeamLifecycleMode>(lifecycleMode);
  const [warmIdle, setWarmIdle] = useState(warmIdleSeconds ?? 900);
  const [mcp, setMcp] = useState(mcpServers);
  const [networkMode, setNetworkMode] = useState<"learning" | "strict">(
    egressMode?.toLowerCase().startsWith("strict") ? "strict" : "learning",
  );
  const [egressText, setEgressText] = useState(egress.join("\n"));
  const [modelRoute, setModelRoute] = useState(() => {
    const option = model
      ? models.find((entry) =>
          model.includes("::")
            ? `${entry.provider}::${entry.deployment}` === model
            : entry.deployment === model,
        )
      : undefined;
    return option ? `${option.provider}::${option.deployment}` : "";
  });
  const [fallbackRoutes, setFallbackRoutes] = useState(modelFallbacks);
  const [memoryBinding, setMemoryBinding] = useState(memory ?? "");
  const [selectedGitWriteRepos, setSelectedGitWriteRepos] = useState(gitWriteRepos);
  const [executionPlanText, setExecutionPlanText] = useState(
    executionPlan ? JSON.stringify(executionPlan, null, 2) : "",
  );
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const router = useRouter();
  // PATCH with EXACTLY the given body — so Pause/Resume never smuggles an
  // unsaved charter/cadence edit into the request (that was a silent commit).
  const patch = (body: object) => start(async () => {
    setError(null);
    try {
      const res = await fetch(`/api/namespaces/${ns}/teams/${name}`, {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const b = await res.json().catch(() => null);
        throw new Error(b?.error?.message ?? `Save failed (${res.status})`);
      }
      router.refresh();
      setOpen(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Save failed");
    }
  });
  return (
    <div>
      <div className="flex gap-2">
        <button onClick={() => patch({ paused: !paused })} disabled={pending} className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">{paused ? "Resume" : "Pause"}</button>
        <button onClick={() => setOpen(!open)} className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">Edit</button>
      </div>
      {error && <p className="mt-2 text-xs text-danger">{error}</p>}
      {open && (
        <div className="mt-3 space-y-3 rounded-lg border border-border bg-surface-muted/40 p-4">
          <label className="block text-xs text-foreground-muted">Charter / standing prompt
            <textarea value={ch} onChange={(e) => setCh(e.target.value)} rows={4} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs" />
          </label>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="text-xs text-foreground-muted">Cadence minutes (0 = passive)
              <input type="number" min={0} value={cad} onChange={(e) => setCad(Number(e.target.value))} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs" />
            </label>
            <label className="text-xs text-foreground-muted">Runtime lifecycle
              <select value={lifecycle} onChange={(event) => setLifecycle(event.target.value as TeamLifecycleMode)} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs">
                <option value="resourceOptimized">Resource optimized (recommended)</option>
                <option value="persistent">Persistent</option>
                <option value="ephemeral">Ephemeral</option>
              </select>
            </label>
            {lifecycle === "resourceOptimized" && (
              <label className="text-xs text-foreground-muted">Warm idle window (seconds)
                <input type="number" min={0} value={warmIdle} onChange={(event) => setWarmIdle(Number(event.target.value))} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs" />
              </label>
            )}
            <label className="text-xs text-foreground-muted">Egress mode
              <select value={networkMode} onChange={(event) => setNetworkMode(event.target.value as "learning" | "strict")} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs">
                <option value="learning">Learning · observe new public hosts</option>
                <option value="strict">Strict · enforce reviewed hosts only</option>
              </select>
            </label>
            <label className="text-xs text-foreground-muted">Principal/default model
              <select value={modelRoute} onChange={(event) => {
                const route = event.target.value;
                setModelRoute(route);
                setFallbackRoutes((current) => current.filter((fallback) => fallback !== route));
              }} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs">
                <option value="">cluster default</option>
                {models.map((entry) => (
                  <option key={`${entry.provider}::${entry.deployment}`} value={`${entry.provider}::${entry.deployment}`}>
                    {entry.deployment} · {entry.provider}{entry.is_default ? " (default)" : ""}
                  </option>
                ))}
              </select>
            </label>
            <label className="text-xs text-foreground-muted">Qualified fallback routes
              <select
                multiple
                value={fallbackRoutes}
                onChange={(event) => {
                  const selected = new Set(
                    Array.from(event.currentTarget.selectedOptions, (option) => option.value),
                  );
                  setFallbackRoutes((current) => [
                    ...current.filter((route) => selected.has(route)),
                    ...Array.from(selected).filter((route) => !current.includes(route)),
                  ].slice(0, 8));
                }}
                className="mt-1 min-h-20 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs"
              >
                {models
                  .map((entry) => `${entry.provider}::${entry.deployment}`)
                  .filter((route) => route !== modelRoute)
                  .map((route) => <option key={route} value={route}>{route}</option>)}
              </select>
              {fallbackRoutes.map((route, index) => (
                <span key={route} className="mt-1 flex items-center gap-1 rounded border border-border bg-surface px-2 py-1">
                  <span className="min-w-0 flex-1 truncate">{index + 1}. {route}</span>
                  <button type="button" aria-label={`Move ${route} earlier`} disabled={index === 0} onClick={() => setFallbackRoutes((current) => moveRoute(current, index, -1))}>↑</button>
                  <button type="button" aria-label={`Move ${route} later`} disabled={index === fallbackRoutes.length - 1} onClick={() => setFallbackRoutes((current) => moveRoute(current, index, 1))}>↓</button>
                </span>
              ))}
            </label>
            <label className="text-xs text-foreground-muted">Shared memory
              <select value={memoryBinding} onChange={(event) => setMemoryBinding(event.target.value)} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs">
                <option value="">No shared memory binding</option>
                {availableMemories.map((entry) => (
                  <option key={entry.name} value={entry.name}>{entry.name}</option>
                ))}
              </select>
            </label>
          </div>
          <label className="block text-xs text-foreground-muted">External hosts (one host[:port] per line)
            <textarea value={egressText} onChange={(event) => setEgressText(event.target.value)} rows={3} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 font-mono text-xs" />
          </label>
          <fieldset>
            <legend className="text-xs text-foreground-muted">Connected MCP services</legend>
            <div className="mt-1.5 grid gap-2 sm:grid-cols-2">
              {availableMcp.map((server) => (
                <label key={server.name} className="flex items-start gap-2 rounded border border-border bg-surface px-2.5 py-2 text-xs">
                  <input
                    type="checkbox"
                    checked={mcp.includes(server.name)}
                    onChange={(event) =>
                      setMcp((current) =>
                        event.target.checked
                          ? [...new Set([...current, server.name])]
                          : current.filter((entry) => entry !== server.name),
                      )
                    }
                  />
                  <span><span className="font-medium text-foreground">{server.name}</span>{server.summary && <span className="block text-[11px] text-foreground-muted">{server.summary}</span>}</span>
                </label>
              ))}
            </div>
          </fieldset>
          <RepoAccess
            ns={ns}
            initialSelected={gitWriteRepos}
            onSelectionChange={setSelectedGitWriteRepos}
          />
          <label className="block text-xs text-foreground-muted">
            Typed execution plan
            <span className="ml-1 text-[11px]">
              Adjust role token budgets without changing role names or capabilities.
            </span>
            <textarea
              value={executionPlanText}
              onChange={(event) => setExecutionPlanText(event.target.value)}
              rows={14}
              spellCheck={false}
              className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 font-mono text-xs"
            />
          </label>
          <div className="flex justify-end">
            <button
              onClick={() => {
                let parsedExecutionPlan: ExecutionPlan | undefined;
                try {
                  parsedExecutionPlan = executionPlanText.trim()
                    ? JSON.parse(executionPlanText) as ExecutionPlan
                    : undefined;
                } catch {
                  setError("Typed execution plan must be valid JSON.");
                  return;
                }
                patch({
                  charter: ch,
                  cadence_minutes: Number(cad),
                  lifecycle_mode: lifecycle,
                  warm_idle_seconds: lifecycle === "resourceOptimized" ? warmIdle : undefined,
                  mcp_servers: mcp,
                  model: modelRoute,
                  model_fallbacks: fallbackRoutes,
                  memory: memoryBinding,
                  egress_mode: networkMode,
                  egress: parseEgress(egressText),
                  git_write_repos: selectedGitWriteRepos,
                  execution_plan: parsedExecutionPlan,
                });
              }}
              disabled={pending}
              className="rounded bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg"
            >
              {pending ? "Saving…" : "Save launch package"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
