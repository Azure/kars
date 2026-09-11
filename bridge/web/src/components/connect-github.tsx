"use client";

// Each signed-in user connects an installation of the admin-configured shared
// GitHub App. The connection and selected repos are isolated to that principal.

import { useEffect, useState } from "react";

type AppInfo = { configured: boolean; slug: string | null; install_url: string | null };
type Connection = { connected: boolean; account: string | null; repos: string[] };

export function ConnectGithub({ ns }: { ns: string }) {
  const [app, setApp] = useState<AppInfo | null>(null);
  const [conn, setConn] = useState<Connection | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      fetch("/api/github/app").then((r) => r.json()),
      fetch(`/api/namespaces/${ns}/github/connection`).then((r) => r.json()),
    ]).then(
      ([a, c]) => {
        if (!cancelled) {
          setApp(a);
          setConn(c);
        }
      },
      () => {
        if (!cancelled) setError("Could not load GitHub connection state.");
      },
    );
    return () => {
      cancelled = true;
    };
  }, [ns]);

  async function connect() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch(`/api/namespaces/${ns}/github/connect`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({}),
      });
      const body = await res.json().catch(() => null);
      if (!res.ok) throw new Error(body?.error?.message ?? `Connect failed (${res.status})`);
      setConn(body);
    } catch (e) {
      setError(
        e instanceof Error
          ? `${e.message}. Install the app on your repos first (button above), then Connect.`
          : "Connect failed",
      );
    } finally {
      setBusy(false);
    }
  }

  async function disconnect() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch(`/api/namespaces/${ns}/github/connection`, { method: "DELETE" });
      const body = await res.json().catch(() => null);
      if (!res.ok) throw new Error(body?.error?.message ?? `Disconnect failed (${res.status})`);
      setConn(body);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Disconnect failed");
    } finally {
      setBusy(false);
    }
  }

  if (app && !app.configured) {
    return (
      <p className="text-xs text-foreground-muted">
        The kars GitHub App isn&rsquo;t set up on this platform yet. Ask an operator to configure it
        once (Console → Configuration); then you can connect your repos here.
      </p>
    );
  }

  return (
    <div className="space-y-4">
      <p className="max-w-2xl text-xs text-foreground-muted">
        Let your agents open pull requests on <strong className="text-foreground">your</strong> repos —
        without ever handling a credential. Install the kars GitHub App on the repositories you want,
        then Connect. This connection is private to your signed-in user. The router mints a short-lived,
        repo-scoped token per mission; the agent never sees it. Disconnect any time to remove your grant.
      </p>

      <div className="flex flex-wrap items-center gap-2">
        {app?.install_url && (
          <a
            href={app.install_url}
            target="_blank"
            rel="noreferrer"
            className="rounded-lg border border-border bg-surface-muted px-3 py-1.5 text-xs font-medium hover:bg-surface"
          >
            1 · Install the app on your repos ↗
          </a>
        )}
        <button
          type="button"
          onClick={connect}
          disabled={busy}
          className="cursor-pointer rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {busy ? "Connecting…" : conn?.connected ? "2 · Re-sync repos" : "2 · Connect"}
        </button>
        {conn?.connected && (
          <button
            type="button"
            onClick={disconnect}
            disabled={busy}
            className="cursor-pointer rounded-lg border border-rose-500/40 px-3 py-1.5 text-xs font-medium text-rose-600 hover:bg-rose-500/5 disabled:opacity-50"
          >
            Disconnect
          </button>
        )}
      </div>

      {conn?.connected ? (
        <div className="rounded-lg border border-ok/30 bg-ok/[0.04] p-3">
          <p className="text-xs font-medium text-ok">
            ✓ Connected as <span className="font-mono">{conn.account}</span>
          </p>
          {conn.repos.length > 0 ? (
            <ul className="mt-2 flex flex-wrap gap-1.5">
              {conn.repos.map((r) => (
                <li key={r} className="rounded-full border border-border bg-surface-muted px-2 py-0.5 font-mono text-[11px]">
                  {r}
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-1 text-[11px] text-foreground-muted">
              No repositories selected in the installation yet — add some on GitHub, then Re-sync.
            </p>
          )}
        </div>
      ) : (
        <p className="text-xs text-foreground-muted">Not connected. Install the app, then Connect.</p>
      )}

      {error && <p className="text-xs text-rose-600">{error}</p>}
    </div>
  );
}
