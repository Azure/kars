"use client";

// kars Bridge — Microsoft Teams channel configuration.
//
// Separate fields for Client ID, Tenant ID, Client Secret, and Allowed Entra
// Subjects (no composite strings). Write-only secrets — the UI only shows
// whether Teams is enabled, never the credentials themselves. Admin-consent
// and Bot resource guidance is surfaced inline.

import { useEffect, useState } from "react";
import { Icon } from "@/components/icon";

export function ConnectTeams({ ns }: { ns: string }) {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [clientId, setClientId] = useState("");
  const [tenantId, setTenantId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [allowedSubjects, setAllowedSubjects] = useState("");

  async function load() {
    try {
      const r = await fetch(`/api/namespaces/${ns}/channels`, { cache: "no-store" });
      const d = await r.json();
      setEnabled((d.enabled ?? []).includes("teams"));
    } catch {
      setError("Couldn't load channel status.");
      setEnabled(false);
    }
  }

  useEffect(() => {
    let active = true;
    fetch(`/api/namespaces/${ns}/channels`, { cache: "no-store" })
      .then((response) => response.json())
      .then((data) => {
        if (active) setEnabled((data.enabled ?? []).includes("teams"));
      })
      .catch(() => {
        if (active) {
          setError("Couldn't load channel status.");
          setEnabled(false);
        }
      });
    return () => {
      active = false;
    };
  }, [ns]);

  async function save() {
    if (!clientId.trim() || !tenantId.trim() || !clientSecret.trim() || !allowedSubjects.trim()) {
      setError("All fields are required.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const r = await fetch(`/api/namespaces/${ns}/channels`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          channel: "teams",
          teams: {
            client_id: clientId.trim(),
            tenant_id: tenantId.trim(),
            client_secret: clientSecret.trim(),
            entra_role_map: allowedSubjects.trim(),
          },
        }),
      });
      if (!r.ok) {
        const b = await r.json().catch(() => null);
        throw new Error(b?.error?.message || "save failed");
      }
      setClientId("");
      setTenantId("");
      setClientSecret("");
      setAllowedSubjects("");
      setOpen(false);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "save failed");
    } finally {
      setBusy(false);
    }
  }

  async function disconnect() {
    setBusy(true);
    setError(null);
    try {
      await fetch(`/api/namespaces/${ns}/channels/teams`, { method: "DELETE" });
      await load();
    } catch {
      setError("Disconnect failed.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="rounded-lg border border-border p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span aria-hidden className="text-base leading-none"><Icon name="message" size={16} /></span>
          <span className="text-sm font-medium">Microsoft Teams</span>
          {enabled && (
            <span className="rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-600">
              connected
            </span>
          )}
        </div>
        {enabled ? (
          <button
            type="button"
            disabled={busy}
            onClick={disconnect}
            className="rounded-md border border-border px-2 py-1 text-[11px] text-foreground-muted transition hover:text-rose-600 disabled:opacity-50"
          >
            Disconnect
          </button>
        ) : (
          <button
            type="button"
            onClick={() => { setOpen(!open); setError(null); }}
            className="rounded-md border border-border px-2 py-1 text-[11px] font-medium transition hover:bg-surface-muted"
          >
            {open ? "Cancel" : "Connect"}
          </button>
        )}
      </div>

      {open && !enabled && (
        <div className="space-y-2">
          <div className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2">
            <p className="text-[11px] text-foreground-muted">
              <strong>Prerequisites:</strong> Register an Entra ID App Registration with <code>BotFramework Channel</code> enabled. Grant admin consent for <code>TeamsActivity.Send</code>. The Bot must be installed in your target Teams channel/chat.
            </p>
          </div>

          <input
            type="text"
            autoComplete="off"
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            placeholder="Client ID (App Registration)"
            className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs outline-none focus:border-signal"
          />
          <input
            type="text"
            autoComplete="off"
            value={tenantId}
            onChange={(e) => setTenantId(e.target.value)}
            placeholder="Tenant ID"
            className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs outline-none focus:border-signal"
          />
          <input
            type="password"
            autoComplete="off"
            value={clientSecret}
            onChange={(e) => setClientSecret(e.target.value)}
            placeholder="Client Secret (write-only, never shown again)"
            className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs outline-none focus:border-signal"
          />
          <textarea
            autoComplete="off"
            value={allowedSubjects}
            onChange={(e) => setAllowedSubjects(e.target.value)}
            placeholder={'[{"entra_subject":"<entra-oid>","bridge_subject":"<bridge-oidc-sub>","roles":["operator","user"],"name":"Alice"}]'}
            rows={3}
            className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs font-mono outline-none focus:border-signal"
          />

          <div className="flex items-center justify-between">
            <span className="text-[10px] text-foreground-muted">
              HITL approvals and proactive updates — no assistant mediation.
            </span>
            <button
              type="button"
              disabled={busy || !clientId.trim() || !tenantId.trim() || !clientSecret.trim() || !allowedSubjects.trim()}
              onClick={save}
              className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
            >
              {busy ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      )}

      {error && <p className="text-xs text-rose-600">{error}</p>}
    </div>
  );
}
