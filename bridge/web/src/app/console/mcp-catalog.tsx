// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge — MCP catalog picker. Managed entries create a typed McpServer
// preset that the controller materializes as a real Deployment + Service.
// Hosted/external entries register a real operator-reviewed URL. The UI never
// substitutes a fake internal endpoint or calls registration "deployment".

import { useActionState, useMemo, useState } from "react";
import { Icon } from "@/components/icon";
import { applyGovernanceAction, type GovState } from "./governance-actions";
import { MCP_CATALOG, MCP_CATEGORIES, type McpCatalogEntry } from "./mcp-catalog-data";
import type { McpServer } from "@/lib/types";
import { mcpStatus } from "./mcp-status";

const init: GovState = { error: null, ok: null };

function AddForm({ entry, onDone, onBusy }: {
  entry: McpCatalogEntry;
  onDone: () => void;
  onBusy: (busy: boolean) => void;
}) {
  const [state, action, pending] = useActionState(async (previous: GovState, form: FormData) => {
    onBusy(true);
    try {
      return await applyGovernanceAction(previous, form);
    } catch {
      return { error: "Couldn't confirm the request. Check the service list before retrying; your input has been retained.", ok: null };
    } finally {
      onBusy(false);
    }
  }, init);
  const [name, setName] = useState(entry.name);
  const [url, setUrl] = useState(entry.url);
  const [tools, setTools] = useState(entry.allowedTools.join(", "));
  // Default to dev mode so one-click connect always succeeds; production (OAuth)
  // is opt-in and reveals a required issuer field (the CRD rejects
  // productionMode without spec.oauth.issuer).
  const [production, setProduction] = useState(false);
  const [issuer, setIssuer] = useState("");
  const managed = entry.hosting === "managed";

  // Build the McpServer spec from the friendly fields — the operator never sees
  // JSON. `allowedTools` is comma-separated; "*" means all (governed by policy).
  const spec = useMemo(() => {
    const allowedTools = tools
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
    const s: Record<string, unknown> = { displayName: entry.label };
    if (managed && entry.managedPreset) {
      s.managed = { preset: entry.managedPreset };
    } else {
      s.url = url.trim();
    }
    if (allowedTools.length) s.allowedTools = allowedTools;
    if (!managed && production && issuer.trim()) {
      s.productionMode = true;
      s.oauth = { issuer: issuer.trim() };
    }
    if (!managed && entry.bearerFromEnv) s.bearerFromEnv = entry.bearerFromEnv;
    return JSON.stringify(s, null, 2);
  }, [url, tools, production, issuer, entry.label, entry.managedPreset, entry.bearerFromEnv, managed]);

  const blocked = (!managed && !url.trim()) || (!managed && production && issuer.trim() === "");

  if (state.ok) {
    return (
      <div role="status" className="rounded-lg border border-border bg-surface p-3 text-xs">
        <p className="font-medium">
          {entry.label} {managed ? "installation requested." : "registered."}
        </p>
        <p className="mt-0.5 text-foreground-muted">{state.ok}</p>
        {managed && <p className="mt-2 text-foreground-muted">This is not a readiness result. Follow deployment and MCP probe status in the service list below.</p>}
        <button type="button" onClick={onDone} className="mt-2 rounded-md border border-border px-2 py-1 text-[11px] hover:bg-surface-muted">Done</button>
      </div>
    );
  }

  return (
    <form action={action} aria-busy={pending} className="rounded-lg border border-border bg-surface-muted/60 p-3">
      <fieldset disabled={pending} className="space-y-2">
      <input type="hidden" name="kind" value="McpServer" />
      <input type="hidden" name="spec" value={spec} />
      <div className="flex items-center gap-2">
        <span className="text-base" aria-hidden>{entry.icon}</span>
        <p className="text-xs font-semibold">{managed ? "Install" : "Register"} {entry.label}</p>
        <a href={entry.docs} target="_blank" rel="noopener noreferrer" className="text-[11px] text-signal hover:underline">docs ↗</a>
      </div>
      <label className="block text-[11px] text-foreground-muted">
        Name
        <input name="name" value={name} onChange={(e) => setName(e.target.value)} required
          className="mt-0.5 w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs" />
      </label>
      {managed ? (
        <div className="rounded-md border border-border bg-surface px-2.5 py-2 text-[11px] text-foreground-muted">
          <span className="font-medium text-foreground">Managed on this cluster.</span>{" "}
          The Kars controller deploys the reviewed <span className="font-mono">{entry.managedPreset}</span> preset,
          creates its Service and NetworkPolicy, probes <span className="font-mono">initialize → tools/list</span>,
          and only then marks it Ready.
        </div>
      ) : (
        <label className="block text-[11px] text-foreground-muted">
          Endpoint URL {entry.hosting === "external" && <span className="text-warning">· deploy it first, then enter its real URL</span>}
          <input value={url} onChange={(e) => setUrl(e.target.value)} required
            placeholder={entry.hosting === "external" ? "https://your-real-mcp.example/mcp" : undefined}
            className="mt-0.5 w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs" />
        </label>
      )}
      <label className="block text-[11px] text-foreground-muted">
        Allowed tools (comma-separated · <span className="font-mono">*</span> = all, governed by a tool policy)
        <input value={tools} onChange={(e) => setTools(e.target.value)}
          className="mt-0.5 w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs" />
      </label>
      {!managed && <label className="flex items-center gap-2 text-[11px] text-foreground-muted">
        <input type="checkbox" checked={production} onChange={(e) => setProduction(e.target.checked)} />
        Production — require OAuth-authenticated inbound calls {entry.oauth && <span className="text-foreground">(recommended for {entry.label})</span>}
      </label>}
      {!managed && production && (
        <label className="block text-[11px] text-foreground-muted">
          OAuth issuer URL <span className="text-warning">· required for production</span>
          <input value={issuer} onChange={(e) => setIssuer(e.target.value)} placeholder="https://issuer.example.com"
            className="mt-0.5 w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs" />
        </label>
      )}
      <p className="text-[11px] text-foreground-muted">
        {managed
          ? "The endpoint is derived from the controller-owned Service; agents never see or choose it."
          : production
            ? "The router will reject calls not bearer-authenticated against this issuer."
            : entry.bearerFromEnv
              ? `Outbound authentication uses the router-only ${entry.bearerFromEnv} credential; the agent never sees it.`
              : "The endpoint is registered as supplied; Kars does not pretend an external server was deployed."}{" "}
        Applied through the Bridge with Server-Side Apply; the cluster validates it.
      </p>
      <div className="flex items-center gap-3">
        <button type="submit" disabled={pending || blocked} className="rounded-md bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg disabled:opacity-50">
          {pending ? (managed ? "Installing…" : "Registering…") : (managed ? "Install on cluster" : "Register endpoint")}
        </button>
        <button type="button" disabled={pending} onClick={onDone} className="text-xs text-foreground-muted hover:text-foreground disabled:opacity-50">Cancel</button>
        {blocked && <span className="text-[11px] text-foreground-muted">
          {!url.trim() ? "Enter the real endpoint URL." : "Add an issuer URL, or turn off production."}
        </span>}
        {state.error && <p role="alert" className="text-xs text-danger">{state.error}</p>}
      </div>
      </fieldset>
    </form>
  );
}

export function McpCatalog({ servers }: { servers: McpServer[] }) {
  const [open, setOpen] = useState(false);
  const [cat, setCat] = useState<string>("All");
  const [q, setQ] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return MCP_CATALOG.filter((e) => {
      if (cat !== "All" && e.category !== cat) return false;
      if (!needle) return true;
      return e.label.toLowerCase().includes(needle) || e.description.toLowerCase().includes(needle);
    });
  }, [cat, q]);

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-lg border border-signal/40 bg-signal/10 px-3 py-1.5 text-xs font-medium text-signal hover:bg-signal/15"
      >
        <Icon name="bolt" size={14} className="inline mr-1" /> Add from catalog
      </button>
    );
  }

  return (
    <div className="mt-2 rounded-xl border border-border bg-surface p-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div>
          <p className="text-sm font-semibold">Add an MCP service</p>
          <p className="text-xs text-foreground-muted">Managed presets are deployed on this cluster; hosted/external servers register a real endpoint.</p>
        </div>
        <button type="button" disabled={busy} onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground disabled:opacity-50">Close</button>
      </div>

      <div className="mb-3 flex flex-wrap items-center gap-2">
        <input
          value={q}
          disabled={busy}
          aria-label="Search MCP services"
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search services…"
          className="min-w-40 flex-1 rounded-lg border border-border bg-surface px-3 py-1.5 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        <div className="flex flex-wrap gap-1">
          {["All", ...MCP_CATEGORIES].map((c) => (
            <button
              key={c}
              type="button"
              disabled={busy}
              onClick={() => setCat(c)}
              className={`rounded-lg border px-2 py-1 text-[11px] font-medium ${cat === c ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"}`}
            >
              {c}
            </button>
          ))}
        </div>
      </div>

      <ul className="grid gap-2 sm:grid-cols-2">
        {filtered.map((e) => {
          const existing = servers.find((server) => server.name === e.name);
          const status = existing ? mcpStatus(existing) : null;
          return (
          <li key={e.id} className="rounded-lg border border-border bg-surface-muted/40 p-3">
            <div className="flex items-start justify-between gap-2">
              <div className="flex items-start gap-2">
                <span className="text-lg leading-none" aria-hidden>{e.icon}</span>
                <div className="min-w-0">
                  <p className="text-sm font-medium">{e.label}</p>
                  <p className="mt-0.5 line-clamp-2 text-xs text-foreground-muted">{e.description}</p>
                  <div className="mt-1 flex items-center gap-2">
                    <span className="rounded-full bg-surface-muted px-1.5 py-0.5 text-[10px] text-foreground-muted">{e.category}</span>
                    <span className="text-[10px] text-foreground-muted">
                      {e.hosting === "managed" ? "managed · installs on cluster" : e.hosting === "hosted" ? "hosted endpoint" : "external · bring endpoint"}
                    </span>
                  </div>
                </div>
              </div>
              {existing && status && selected !== e.id ? (
                <a href={`#mcp-${existing.namespace}-${existing.name}`}
                  className="shrink-0 rounded-md border border-border px-2 py-1 text-[11px] font-medium hover:bg-surface-muted">
                  View service · {status.label}
                </a>
              ) : (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => setSelected(selected === e.id ? null : e.id)}
                  className="shrink-0 rounded-md border border-border px-2 py-1 text-[11px] font-medium hover:border-signal/40 hover:text-signal"
                >
                  {selected === e.id ? "Cancel" : "Add"}
                </button>
              )}
            </div>
            {selected === e.id && (
              <div className="mt-3">
                <AddForm
                  entry={e}
                  onBusy={setBusy}
                  onDone={() => {
                    setSelected(null);
                  }}
                />
              </div>
            )}
          </li>
        ); })}
      </ul>
    </div>
  );
}
