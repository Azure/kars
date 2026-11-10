// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { RepoAccess } from "@/components/repo-access";
import { loginPath } from "@/lib/auth-return";
import type { ExecutionPlan, ExecutionSynthesis } from "@/lib/types";

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
  mcpServers,
  availableMcp,
  egress,
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
  mcpServers: string[];
  availableMcp: McpOption[];
  egress: string[];
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
  const [mcp, setMcp] = useState(mcpServers);
  const [egressText, setEgressText] = useState(egress.join("\n"));
  const [modelRoute, setModelRoute] = useState(() => {
    const option = model
      ? models.find((entry) =>
          model.includes("::")
            ? `${entry.provider}::${entry.deployment}` === model
            : entry.deployment === model,
        )
      : undefined;
    return option ? `${option.provider}::${option.deployment}` : (model ?? "");
  });
  const [fallbackRoutes, setFallbackRoutes] = useState(modelFallbacks);
  const [memoryBinding, setMemoryBinding] = useState(memory ?? "");
  const [selectedGitWriteRepos, setSelectedGitWriteRepos] = useState(gitWriteRepos);
  const [executionPlanText, setExecutionPlanText] = useState(
    executionPlan ? JSON.stringify(executionPlan, null, 2) : "",
  );
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [signInRequired, setSignInRequired] = useState(false);
  const router = useRouter();
  let parsedExecutionPlan: ExecutionPlan | undefined;
  let executionPlanError = false;
  try {
    parsedExecutionPlan = executionPlanText.trim() ? JSON.parse(executionPlanText) as ExecutionPlan : undefined;
  } catch {
    executionPlanError = true;
  }
  const inputPhases = parsedExecutionPlan && Array.isArray(parsedExecutionPlan.roles)
    && parsedExecutionPlan.roles.every((role) => Array.isArray(role?.phases)
      && role.phases.every((phase) => Array.isArray(phase?.capabilities)))
    && Array.isArray(parsedExecutionPlan.synthesis?.capabilities)
    ? [
      ...parsedExecutionPlan.roles.flatMap((role, roleIndex) => role.phases.map((phase, phaseIndex) => ({
        label: `${role.name} / ${phase.name}`, roleIndex, phaseIndex, phase,
      }))),
      { label: "Principal synthesis", roleIndex: -1, phaseIndex: -1, phase: parsedExecutionPlan.synthesis },
    ] : [];
  function updateInputPhase(roleIndex: number, phaseIndex: number, change: Partial<ExecutionSynthesis>) {
    if (!parsedExecutionPlan) return;
    const plan = structuredClone(parsedExecutionPlan);
    const phase = roleIndex < 0 ? plan.synthesis : plan.roles[roleIndex].phases[phaseIndex];
    Object.assign(phase, change);
    setExecutionPlanText(JSON.stringify(plan, null, 2));
  }
  // PATCH with EXACTLY the given body — so Pause/Resume never smuggles an
  // unsaved charter/cadence edit into the request (that was a silent commit).
  const patch = (body: object) => start(async () => {
    setError(null);
    setSignInRequired(false);
    try {
      const res = await fetch(`/api/namespaces/${ns}/teams/${name}`, {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      if (res.status === 401) {
        setSignInRequired(true);
        setError("Your session could not be authenticated. Sign in again, then retry this change.");
        return;
      }
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
      {error && (
        <div role="alert" className="mt-2 text-xs text-danger">
          <p>{error}</p>
          {signInRequired && (
            <a href={loginPath(`/workspace/teams/${encodeURIComponent(name)}`)} className="mt-1 inline-block underline">
              Sign in again
            </a>
          )}
        </div>
      )}
      {open && (
        <div className="mt-3 space-y-3 rounded-lg border border-border bg-surface-muted/40 p-4">
          <label className="block text-xs text-foreground-muted">Charter / standing prompt
            <textarea value={ch} onChange={(e) => setCh(e.target.value)} rows={4} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs" />
          </label>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="text-xs text-foreground-muted">Cadence minutes (0 = passive)
              <input type="number" min={0} value={cad} onChange={(e) => setCad(Number(e.target.value))} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs" />
            </label>
            <p className="text-xs text-foreground-muted">
              The Team persists across isolated runs; agent pods are created on demand.
              Warm and always-on agent runtimes are not currently supported.
            </p>
            <p className="text-xs text-foreground-muted">
              Egress: Strict · reviewed hosts only. New destinations require approved access.
              Datapath observations do not grant access.
            </p>
            <label className="text-xs text-foreground-muted">Principal/default model
              <select value={modelRoute} onChange={(event) => {
                const route = event.target.value;
                setModelRoute(route);
                setFallbackRoutes((current) => current.filter((fallback) => fallback !== route));
              }} className="mt-1 w-full rounded border border-border bg-surface px-2 py-1.5 text-xs">
                <option value="">cluster default</option>
                {modelRoute && !models.some((entry) => `${entry.provider}::${entry.deployment}` === modelRoute) && (
                  <option value={modelRoute}>{modelRoute} (not in current catalog)</option>
                )}
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
            plan={parsedExecutionPlan ?? executionPlan}
            ns={ns}
            initialSelected={gitWriteRepos}
            onSelectionChange={setSelectedGitWriteRepos}
          />
          {inputPhases.length > 0 && (
            <fieldset className="space-y-3 rounded border border-border p-3">
              <legend className="text-xs font-medium">Reading previous work</legend>
              <p className="text-xs text-foreground-muted">
                Revisions and approved Team knowledge supply previous work to every phase, including independent researchers.
                Allow each phase to read previous work and give it enough tool calls. Review only the needed changes.
                If an administrator tool policy blocks reading, ask the administrator to review it; these controls cannot override that policy.
                No host, network or credential access is added. Pause stops new runs only;
                let current work finish before saving because permission changes can stop active work.
              </p>
              {!paused && <p className="text-xs text-warning">Pause the Team before updating retained input permissions.</p>}
              {inputPhases.map(({ label, roleIndex, phaseIndex, phase }) => (
                <div key={`${roleIndex}:${phaseIndex}`} className="space-y-1 text-xs">
                  <p className="font-medium">{label}</p>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      aria-label={`Read previous work for ${label}`}
                      checked={phase.capabilities.includes("filesystem-read")}
                      disabled={!paused || pending}
                      onChange={(event) => updateInputPhase(roleIndex, phaseIndex, {
                        capabilities: event.target.checked
                          ? [...phase.capabilities.filter((cap) => cap !== "filesystem-read"), "filesystem-read"]
                          : phase.capabilities.filter((cap) => cap !== "filesystem-read"),
                      })}
                    />
                    Read previous work
                  </label>
                  <label className="flex items-center gap-2">
                    Maximum tool calls
                    <input
                      type="number" min={0} step={1}
                      aria-label={`Maximum tool calls for ${label}`}
                      value={phase.max_tool_calls}
                      disabled={!paused || pending}
                      onChange={(event) => updateInputPhase(roleIndex, phaseIndex, { max_tool_calls: Number(event.target.value) })}
                      className="w-24 rounded border border-border bg-surface px-2 py-1"
                    />
                  </label>
                </div>
              ))}
              <button type="button" disabled={!paused || pending || executionPlanError}
                onClick={() => patch({ execution_plan: parsedExecutionPlan })}
                className="rounded bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg disabled:opacity-50">
                Save input permissions
              </button>
              <p className="text-xs text-foreground-muted">Saves only the reviewed plan. Resume afterwards, then confirm Run now or allow the reviewed cadence to proceed.</p>
            </fieldset>
          )}
          <label className="block text-xs text-foreground-muted">
            Typed execution plan
            <span className="ml-1 text-[11px]">
              Review phase capabilities and role token budgets. Role names must match the roster.
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
                if (executionPlanError) {
                  setError("Typed execution plan must be valid JSON.");
                  return;
                }
                patch({
                  charter: ch,
                  cadence_minutes: Number(cad),
                  mcp_servers: mcp,
                  model: modelRoute,
                  model_fallbacks: fallbackRoutes,
                  memory: memoryBinding,
                  egress_mode: "strict",
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
