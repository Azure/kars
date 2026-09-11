"use client";

// kars Bridge Operator Console — GitHub App self-service setup. Operators
// configure the SHARED kars GitHub App once (the platform identity); the
// credentials are verified against the real GitHub API before being stored,
// and individual users then connect their own repos from their Workspace.
// This used to be a status-only display telling the operator to run
// `kubectl create secret` by hand — now it's a real form.

import { useActionState, useState } from "react";
import {
  putGithubAppAction,
  disconnectGithubAppAction,
  type GithubAppState,
} from "./configuration/github-app-actions";

const init: GithubAppState = { error: null, ok: null };

export function OperatorGithubStatus({
  configured,
  slug,
}: {
  configured: boolean;
  slug: string | null;
}) {
  const [setupState, setupAction, settingUp] = useActionState(putGithubAppAction, init);
  const [disconnectState, disconnectAction, disconnecting] = useActionState(disconnectGithubAppAction, init);
  const [confirmingDisconnect, setConfirmingDisconnect] = useState(false);

  // Once either action reports success, the page revalidates server-side —
  // show a brief confirmation until the fresh `configured` prop lands.
  if (setupState.ok || disconnectState.ok) {
    return <p className="text-xs font-medium text-ok">{setupState.ok ?? disconnectState.ok} Refreshing…</p>;
  }

  if (configured) {
    return (
      <div className="space-y-2">
        <p className="text-xs font-medium text-ok">
          ✓ Shared GitHub App configured{slug ? " — " : ""}
          {slug && <span className="font-mono">{slug}</span>}
        </p>
        <p className="max-w-2xl text-xs text-foreground-muted">
          The platform identity is set. Users don&rsquo;t configure repos here — each user connects
          their own repositories from their <strong className="text-foreground">Workspace → Connections</strong>.
          The App&rsquo;s private key stays in the <code className="font-mono">kars-github-app</code> secret
          and is mounted only to the inference router, never to an agent.
        </p>
        {confirmingDisconnect ? (
          <form action={disconnectAction} className="flex items-center gap-2">
            <span className="text-[11px] text-foreground-muted">Remove the App credential? Workspaces lose the ability to connect repos.</span>
            <button type="submit" disabled={disconnecting} className="rounded-md border border-danger/40 px-2 py-1 text-[11px] font-medium text-danger hover:bg-danger/10 disabled:opacity-50">
              {disconnecting ? "Disconnecting…" : "Confirm disconnect"}
            </button>
            <button type="button" onClick={() => setConfirmingDisconnect(false)} className="text-[11px] text-foreground-muted hover:text-foreground">
              Cancel
            </button>
          </form>
        ) : (
          <button
            type="button"
            onClick={() => setConfirmingDisconnect(true)}
            className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger"
          >
            Disconnect
          </button>
        )}
        {disconnectState.error && <p className="text-xs text-danger">{disconnectState.error}</p>}
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <p className="text-xs font-medium text-warning">GitHub App not configured</p>
      <p className="max-w-2xl text-xs text-foreground-muted">
        Register one shared kars GitHub App on GitHub (Settings → Developer settings → GitHub Apps → New
        GitHub App, permissions: Contents + Pull requests: read/write; Metadata + Checks + Commit
        statuses + Dependabot alerts + Code scanning alerts + Secret scanning alerts: read, no
        webhook needed), then paste its
        App ID and the private key <code className="font-mono">.pem</code> file it generated below.
        Verified against GitHub before saving; once set, users self-serve from their Workspace → Connections.
      </p>
      <form action={setupAction} className="max-w-2xl space-y-2">
        <input
          name="app_id"
          placeholder="App ID (numeric, e.g. 123456)"
          required
          inputMode="numeric"
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm outline-none focus:border-signal"
        />
        <textarea
          name="private_key"
          placeholder={"-----BEGIN RSA PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----"}
          required
          rows={6}
          spellCheck={false}
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed outline-none focus:border-signal"
        />
        <button
          type="submit"
          disabled={settingUp}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50"
        >
          {settingUp ? "Verifying with GitHub…" : "Verify & connect"}
        </button>
        {setupState.error && <p className="text-xs text-danger">{setupState.error}</p>}
      </form>
    </div>
  );
}
