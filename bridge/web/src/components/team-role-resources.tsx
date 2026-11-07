// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import type { TeamRoleResources } from "@/lib/types/teams";

const destination = (entry: TeamRoleResources["egress"][number]) =>
  `${entry.host.trim().toLowerCase()}:${entry.port ?? 443}`;

export function TeamRoleResourcePicker({ name, value, available, onChange }: {
  name: string;
  value?: TeamRoleResources | null;
  available: TeamRoleResources;
  onChange: (resources: TeamRoleResources) => void;
}) {
  const selected = value ?? { mcp_servers: [], egress: [] };
  const memories = [...new Set([available.memory, selected.memory].filter((memory): memory is string => Boolean(memory)))];
  const servers = [...new Set([...available.mcp_servers, ...selected.mcp_servers])];
  const hosts = [...new Map([...available.egress, ...selected.egress].map((entry) => [destination(entry), entry])).values()];
  return (
    <fieldset className="mt-3 rounded-lg border border-border p-2 text-xs">
      <legend className="px-1 font-medium">{name || "Member"} resources</legend>
      <p className="text-[11px] text-foreground-muted">
        Explicit member memory, MCP and Strict egress selections, limited to the reviewed Team resources. Native phases also need memory or network permission respectively. Skills remain separately reviewed.
      </p>
      {!value && <p className="mt-1 text-[11px] text-foreground-muted">
        Legacy selection unchanged: a runtime/model override has no inherited memory binding, MCP or egress; without an override the whole Team blueprint is inherited.
        <button type="button" className="ml-1 underline" onClick={() => onChange({ mcp_servers: [], egress: [] })}>Choose explicit resources</button>
      </p>}
      {servers.map((server) => {
        const checked = selected.mcp_servers.includes(server);
        const allowed = available.mcp_servers.includes(server);
        return <label key={server} className="mt-1 flex items-center gap-2">
          <input type="checkbox" checked={checked} onChange={(event) => onChange({ ...selected,
            mcp_servers: event.target.checked ? [...selected.mcp_servers, server] : selected.mcp_servers.filter((entry) => entry !== server),
          })} />
          MCP: {server}{!allowed && <span className="text-danger">not selected for Team — remove or restore Team selection</span>}
        </label>;
      })}
      {hosts.map((entry) => {
        const key = destination(entry);
        const checked = selected.egress.some((host) => destination(host) === key);
        const allowed = available.egress.some((host) => destination(host) === key);
        return <label key={key} className="mt-1 flex items-center gap-2">
          <input type="checkbox" checked={checked} onChange={(event) => onChange({ ...selected,
            egress: event.target.checked ? [...selected.egress, entry] : selected.egress.filter((host) => destination(host) !== key),
          })} />
          Egress: {key}{!allowed && <span className="text-danger">not selected for Team — remove or restore Team selection</span>}
        </label>;
      })}
      {memories.length > 0 && <label className="mt-2 block">
        Memory binding
        <select aria-label={`${name || "Member"} memory binding`} value={selected.memory ?? ""}
          className="ml-2 rounded border border-border bg-surface p-1"
          onChange={(event) => onChange({ ...selected, memory: event.target.value || null })}>
          <option value="">Default private runtime memory</option>
          {memories.map((memory) => <option key={memory} value={memory} disabled={memory !== available.memory}>{memory}</option>)}
        </select>
        {selected.memory && selected.memory !== available.memory &&
          <span className="text-danger">not selected for Team — remove or restore Team selection</span>}
        <p className="mt-1 text-[11px] text-foreground-muted">Select the Team binding explicitly to share it with this member. No binding uses default backend selection; it does not disable memory.</p>
      </label>}
      {value && !selected.memory && selected.mcp_servers.length === 0 && selected.egress.length === 0 &&
        <p className="mt-1 text-[11px] text-foreground-muted">No explicit memory binding, MCP or egress destinations selected for this member.</p>}
      {servers.length === 0 && hosts.length === 0 && memories.length === 0 &&
        <p className="mt-1 text-[11px] text-foreground-muted">Select Team resources under governance before assigning them to members.</p>}
    </fieldset>
  );
}
