"use client";

import { Icon } from "@/components/icon";
import { MEMBER_ARCHETYPES } from "@/lib/member-archetypes";
import type { GovernancePanelInput, OrgPanelInput } from "./team-composer-panel-types";

function moveFallback(routes: string[], index: number, delta: number): string[] {
  const next = index + delta;
  if (next < 0 || next >= routes.length) return routes;
  const copy = [...routes];
  [copy[index], copy[next]] = [copy[next], copy[index]];
  return copy;
}

export function renderGovernancePanel({ name, options, mcp, setMcp, toolPolicy, setToolPolicy, commons, setCommons, memory, setMemory, selectedMemoryOption, runtime, setRuntime, model, setModel, modelFallbacks, setModelFallbacks, egressMode, setEgressMode, egressText, setEgressText }: GovernancePanelInput) {
  return (
<details className="mt-3">
          <summary className="cursor-pointer text-xs font-medium text-foreground-muted hover:text-foreground">
            Advanced governance &amp; access{mcp.length > 0 ? ` · ${mcp.length} MCP selected` : ""}
          </summary>
          <fieldset className="mt-3 rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="shield" size={13} /> Governance & access</legend>
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="text-xs text-foreground-muted">
                Tool policy
                <select aria-label="Tool policy" value={toolPolicy} onChange={(e) => setToolPolicy(e.target.value)} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  <option value="">cluster default (kars-default)</option>
                  {options.tool_policies.map((tp) => <option key={tp.name} value={tp.name}>{tp.name}{tp.summary ? ` — ${tp.summary}` : ""}</option>)}
                </select>
              </label>
              <label className="text-xs text-foreground-muted">
                Knowledge commons name
                <input
                  value={commons}
                  onChange={(e) => setCommons(e.target.value)}
                  placeholder={`${name || "<team>"} (default)`}
                  className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
                />
                <span className="mt-1 block text-[11px] text-foreground-muted">
                  The team&apos;s durable shared archive and backlog namespace. Leave blank to use the
                  team default.
                </span>
              </label>
              <label className="text-xs text-foreground-muted">
                Runtime memory backend
                <select
                  aria-label="Team runtime memory"
                  value={memory}
                  onChange={(e) => setMemory(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
                >
                  <option value="">None — rely on the team&apos;s own commons</option>
                  {options.memories.map((entry) => (
                    <option key={entry.name} value={entry.name}>
                      {entry.name}
                      {entry.summary ? ` · ${entry.summary}` : ""}
                       {entry.qualified_routes?.length ? " · qualified" : " · unqualified"}
                    </option>
                  ))}
                </select>
                {selectedMemoryOption && (
                  <span className="mt-1 block text-[11px] text-foreground-muted">
                    {selectedMemoryOption.backend ?? "unknown backend"}
                    {selectedMemoryOption.compiled_digest ? ` · digest ${selectedMemoryOption.compiled_digest}` : ""}
                    {selectedMemoryOption.readiness ? ` · ${selectedMemoryOption.readiness}` : ""}
                    {selectedMemoryOption.qualified_routes?.length
                      ? ` · qualified on ${selectedMemoryOption.qualified_routes.join(", ")}`
                      : " · not resource-qualified"}
                  </span>
                )}
              </label>
              <label className="text-xs text-foreground-muted">
                Harness (runtime for every run)
                <select aria-label="Team harness" value={runtime} onChange={(e) => setRuntime(e.target.value)} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  <option value="">sandbox default (OpenClaw)</option>
                  {options.runtimes.filter((rt) => rt.wired).map((rt) => <option key={rt.kind} value={rt.kind}>{rt.label}</option>)}
                </select>
              </label>
              <label className="text-xs text-foreground-muted">
                Principal/default model
                <select aria-label="Team principal model" value={model} onChange={(event) => {
                  const route = event.target.value;
                  setModel(route);
                  setModelFallbacks((current) => current.filter((fallback) => fallback !== route));
                }} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  <option value="">cluster default</option>
                  {options.models.map((option) => (
                    <option key={`${option.provider}::${option.deployment}`} value={`${option.provider}::${option.deployment}`}>
                      {option.deployment} · {option.provider}{option.is_default ? " (default)" : ""}
                    </option>
                  ))}
                </select>
              </label>
              <label className="text-xs text-foreground-muted sm:col-span-2">
                Qualified fallback routes
                <select
                  multiple
                  aria-label="Team model fallback routes"
                  value={modelFallbacks}
                  onChange={(event) => {
                    const selected = new Set(
                      Array.from(event.currentTarget.selectedOptions, (option) => option.value),
                    );
                    setModelFallbacks((current) => [
                      ...current.filter((route) => selected.has(route)),
                      ...Array.from(selected).filter((route) => !current.includes(route)),
                    ].slice(0, 8));
                  }}
                  className="mt-1 min-h-24 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
                >
                  {options.models
                    .map((option) => `${option.provider}::${option.deployment}`)
                    .filter((route) => route !== model)
                    .map((route) => (
                      <option key={route} value={route}>
                        {route}
                      </option>
                    ))}
                </select>
                {modelFallbacks.map((route, index) => (
                  <span key={route} className="mt-1 flex items-center gap-1 rounded border border-border bg-surface px-2 py-1">
                    <span className="min-w-0 flex-1 truncate">{index + 1}. {route}</span>
                    <button type="button" aria-label={`Move ${route} earlier`} disabled={index === 0} onClick={() => setModelFallbacks((current) => moveFallback(current, index, -1))}>↑</button>
                    <button type="button" aria-label={`Move ${route} later`} disabled={index === modelFallbacks.length - 1} onClick={() => setModelFallbacks((current) => moveFallback(current, index, 1))}>↓</button>
                  </span>
                ))}
                <span className="mt-1 block text-[11px]">
                  Bridge accepts a fallback only when retained evidence proves the complete Team plan and resources on that route.
                </span>
              </label>
              <fieldset className="sm:col-span-2">
                <legend className="text-xs text-foreground-muted">Connected services (MCP)</legend>
                {options.mcp_servers.length === 0 ? (
                  <p className="mt-1.5 text-xs text-foreground-muted">No MCP servers are installed.</p>
                ) : (
                  <div className="mt-1.5 grid gap-2 sm:grid-cols-2">
                    {options.mcp_servers.map((server) => {
                      const checked = mcp.includes(server.name);
                      return (
                        <label key={server.name} className="flex items-start gap-2 rounded-lg border border-border px-3 py-2 text-sm">
                          <input
                            type="checkbox"
                            checked={checked}
                            disabled={!checked && mcp.length >= 8}
                            onChange={(event) =>
                              setMcp((current) =>
                                event.target.checked
                                  ? [...new Set([...current, server.name])]
                                  : current.filter((name) => name !== server.name),
                              )
                            }
                            className="mt-0.5 h-3.5 w-3.5 rounded border-border"
                          />
                          <span>
                            <span className="font-medium text-foreground">{server.name}</span>
                            {server.summary && <span className="block text-[11px] text-foreground-muted">{server.summary}</span>}
                            <span className="block text-[11px] text-foreground-muted">
                              {server.mode ? `mode ${server.mode}` : "mode unknown"}
                              {server.discovered_tools?.length ? ` · tools ${server.discovered_tools.slice(0, 4).join(", ")}` : ""}
                              {server.tool_schema_digest ? ` · schema ${server.tool_schema_digest}` : " · schema missing"}
                              {server.qualified_routes?.length
                                ? ` · qualified ${server.qualified_routes.join(", ")}`
                                : " · not resource-qualified"}
                            </span>
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </fieldset>
              <label className="text-xs text-foreground-muted">
                Egress mode
                <select value={egressMode} onChange={(event) => setEgressMode(event.target.value as "learning" | "strict")} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  <option value="learning">Learning · observe new public hosts</option>
                  <option value="strict">Strict · enforce reviewed hosts only</option>
                </select>
              </label>
              <label className="text-xs text-foreground-muted sm:col-span-2">
                External hosts (one host[:port] per line)
                <textarea value={egressText} onChange={(event) => setEgressText(event.target.value)} rows={3} placeholder={"api.example.com:443\nstatus.example.com:443"} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
                <span className="mt-1 block text-[11px] text-foreground-muted">Public DNS only. Private/internal targets stay fail-closed; expose those through an approved in-cluster MCP or service integration.</span>
              </label>
            </div>
            <p className="mt-2 text-[11px] text-foreground-muted">Every team run is bounded by a tool policy — leave unset to inherit the cluster default. Selected MCP services and the runtime memory backend are inherited by every run and validated before launch. The knowledge commons remains the team&apos;s durable shared archive. The harness is the runtime every run executes on (a chat-only adapter is corrected to OpenClaw).</p>
          </fieldset>
        </details>
  );
}

export function renderOrgPanel({ name, tier, roles, options, addFromArchetype, addRole, addNote, patchRole, removeRole }: OrgPanelInput) {
  return (
<div className="kb-card p-5 sm:p-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-sm font-semibold">Org chart</h2>
            <p className="mt-0.5 text-xs text-foreground-muted">Each role&rsquo;s authority is a verified subset of the team&rsquo;s — members can run different harnesses & models.</p>
          </div>
          <div className="flex items-center gap-2">
            <select
              value=""
              onChange={(e) => { if (e.target.value) addFromArchetype(e.target.value); e.target.value = ""; }}
              className="rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs font-medium text-foreground-muted hover:bg-surface-muted"
              title="Add a pre-defined member archetype (e.g. Rust Engineer, Financial Analyst)"
            >
              <option value="">+ Add from archetype…</option>
              {MEMBER_ARCHETYPES.map((a) => (
                <option key={a.id} value={a.id}>{a.icon} {a.title}</option>
              ))}
            </select>
            <button type="button" onClick={addRole} className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">+ Add role</button>
          </div>
          {addNote && (
            <p role="status" aria-live="polite" className="mt-1.5 text-right text-[11px] font-medium text-signal">{addNote}</p>
          )}
        </div>

        {/* Principal node */}
        <div className="mt-4 rounded-xl border border-signal/40 bg-signal/[0.05] p-3">
          <div className="flex items-center justify-between">
            <p className="text-sm font-semibold">{name || "this team"} <span className="font-normal text-foreground-muted">· Principal</span></p>
            <span className="text-xs text-foreground-muted">Tier {tier} · grants members up to Tier {Math.max(1, tier - 1)}</span>
          </div>
        </div>

        {/* Role nodes — connected to the principal as a visual org tree. */}
        <div className="relative mt-3 space-y-3 kb-stagger sm:pl-6">
          <span aria-hidden className="pointer-events-none absolute left-3 top-0 hidden h-full w-px bg-border sm:block" />
          {roles.map((r) => (
            <div key={r.id} className="relative rounded-xl border border-border bg-surface p-3">
              <span aria-hidden className="pointer-events-none absolute -left-3 top-6 hidden h-px w-3 bg-border sm:block" />
              <div className="flex items-center gap-2">
                <input value={r.name} onChange={(e) => patchRole(r.id, { name: e.target.value })} placeholder="role name (e.g. triager)" className="flex-1 rounded-lg border border-border bg-surface px-2.5 py-1.5 text-sm font-medium" />
                <span className="text-[11px] text-foreground-muted">Member</span>
                <button type="button" onClick={() => removeRole(r.id)} className="text-xs text-foreground-muted hover:text-danger">Remove</button>
              </div>
              <textarea value={r.system_prompt} onChange={(e) => patchRole(r.id, { system_prompt: e.target.value })} rows={2} placeholder="what this role does…" className="mt-2 w-full resize-y rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs" />
              <div className="mt-2 grid gap-2 sm:grid-cols-2">
                <select aria-label="Role model" value={r.model} onChange={(e) => patchRole(r.id, { model: e.target.value })} className="rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs">
                  <option value="">model: team default</option>
                  {options.models.map((m) => <option key={`${m.provider}::${m.deployment}`} value={`${m.provider}::${m.deployment}`}>{m.deployment}</option>)}
                </select>
                <select aria-label="Role harness" value={r.runtime} onChange={(e) => patchRole(r.id, { runtime: e.target.value })} className="rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs">
                  <option value="">harness: OpenClaw</option>
                  {options.runtimes.filter((rt) => rt.wired).map((rt) => <option key={rt.kind} value={rt.kind}>{rt.label}</option>)}
                </select>
              </div>
              {/* Per-role skills — a real picker from the attested KarsSkills the
                  cluster offers, so "Skills" isn't a taught concept with no control. */}
              {options.skills.length > 0 ? (
                <div className="mt-2">
                  <p className="text-[11px] text-foreground-muted">Skills (attested capability bundles this role acquires)</p>
                  <div className="mt-1 flex flex-wrap gap-1.5">
                    {options.skills.map((sk) => {
                      const on = r.skills.includes(sk.name);
                      return (
                        <button
                          key={sk.name}
                          type="button"
                          title={[
                            sk.summary,
                            sk.version ? `version ${sk.version}` : null,
                            sk.version_digest ? `digest ${sk.version_digest}` : null,
                            sk.recipe ? `recipe ${sk.recipe}` : null,
                            sk.qualified_routes?.length
                              ? `qualified ${sk.qualified_routes.join(", ")}`
                              : "not resource-qualified",
                          ].filter(Boolean).join(" · ") || undefined}
                          onClick={() =>
                            patchRole(r.id, {
                              skills: on ? r.skills.filter((x) => x !== sk.name) : [...r.skills, sk.name],
                            })
                          }
                          className={`rounded-full border px-2 py-0.5 text-[11px] font-medium ${
                            on ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"
                          }`}
                        >
                          {on ? "✓ " : ""}{sk.name}
                        </button>
                      );
                    })}
                  </div>
                </div>
              ) : (
                r.skills.length > 0 && (
                  <p className="mt-2 text-[11px] text-foreground-muted">Skills: {r.skills.join(", ")}</p>
                )
              )}
            </div>
          ))}
          {roles.length === 0 && <p className="text-xs text-foreground-muted">No roles — add at least one, or the team runs as a single principal.</p>}
        </div>
      </div>
  );
}
