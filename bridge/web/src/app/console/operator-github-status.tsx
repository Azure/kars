// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge Operator Console — GitHub App self-service setup. Operators
// configure the SHARED kars GitHub App once (the platform identity); the
// credentials are verified against the real GitHub API before being stored,
// and individual users then connect their own repos from their Workspace.
// Private storage requires explicit operator enrollment before this form.

import { useActionState, useState } from "react";
import type { GithubApp } from "@/lib/types";
import {
  putGithubAppAction,
  disconnectGithubAppAction,
  startGithubAppRegistrationAction,
  type GithubAppRegistrationState,
  type GithubAppState,
} from "./configuration/github-app-actions";

const init: GithubAppState = { error: null, ok: null };
const registrationInit: GithubAppRegistrationState = { error: null, registration: null };
const inputClass = "w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal";
const servicePermissions = "Contents + Issues + Pull requests: read/write; Actions + Metadata + Checks + Commit statuses: read";

export function OperatorGithubStatus({
  configured,
  slug,
  storage_ready,
}: Pick<GithubApp, "configured" | "slug" | "storage_ready">) {
  const [setupState, setupAction, settingUp] = useActionState(putGithubAppAction, init);
  const [disconnectState, disconnectAction, disconnecting] = useActionState(disconnectGithubAppAction, init);
  const [confirmingDisconnect, setConfirmingDisconnect] = useState(false);

  if (!storage_ready) {
    return (
      <div className="max-w-2xl space-y-2" role="status">
        <p className="text-xs font-medium text-warning">GitHub App storage requires operator enrollment</p>
        <p className="text-xs text-foreground-muted">
          A credential-free workspace does not authorize private-key storage. Before submitting an App key,
          an operator must explicitly enroll the <code className="font-mono">kars-github-app</code> store
          with purpose <code className="font-mono">github-app</code> and the Bridge writer through
          the reviewed <code className="font-mono">kars credentials grant bootstrap-store</code>,
          {" "}<code className="font-mono">preview</code> and <code className="font-mono">apply</code> flow.
          Bootstrap only if the store is missing; preserve all existing grant authority.
        </p>
        <p className="text-xs text-foreground-muted">
          Set the Bridge Helm option <code className="font-mono">workspaceEnrollment.enabled=false</code>
          {" "}before expanding the grant. Complete private activation and wait for current grant readiness,
          then refresh this page. See the installation guide&rsquo;s GitHub App storage enrollment steps.
          No App ID or private key is needed until storage is ready.
        </p>
      </div>
    );
  }

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
        <p className="max-w-2xl text-xs text-foreground-muted">
          Required repository permissions: {servicePermissions}. After changing App permissions,
          the installation owner must approve the updated grant in GitHub. Credential verification
          and repository discovery do not prove these permissions are granted; a missing permission
          can prevent all router GitHub requests, including read-only tasks.
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
        Use an existing App’s PEM file or register a new App with GitHub. Kars verifies the signing
        credential before saving it to the enrolled store; key contents are never displayed here.
        Registration does not grant repository access. Users then choose their repositories through
        Workspace → Connections and approve installation on GitHub.
      </p>
      <details open className="max-w-2xl rounded-lg border border-border p-3">
        <summary className="cursor-pointer text-sm font-semibold">Use an existing GitHub App</summary>
        <p className="my-2 text-xs text-foreground-muted">
          In the App’s GitHub settings, find its App ID and choose Generate a private key.
          Upload the downloaded .pem file below (up to 16 KiB); do not paste its contents.
          Required repository permissions: {servicePermissions}. Existing installations must approve
          permission changes. No webhook is needed.
        </p>
      <form action={setupAction} className="space-y-2">
        <label className="block space-y-1 text-xs">
          <span>App ID</span>
          <input name="app_id" placeholder="Numeric App ID" required inputMode="numeric" pattern="[0-9]{1,20}" maxLength={20} className={inputClass} />
        </label>
        <label className="block space-y-1 text-xs">
          <span>Private-key PEM file</span>
          <input type="file" name="private_key_file" accept=".pem" required className={inputClass} />
        </label>
        <button
          type="submit"
          disabled={settingUp}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50"
        >
          {settingUp ? "Verifying with GitHub…" : "Verify & connect"}
        </button>
        {setupState.error && <p role="alert" className="text-xs text-danger">{setupState.error}</p>}
      </form>
      </details>
      <details className="max-w-2xl rounded-lg border border-border p-3">
        <summary className="cursor-pointer text-sm font-semibold">Register a new GitHub App</summary>
        <GithubAppRegistration />
      </details>
    </div>
  );
}

function GithubAppRegistration() {
  const [state, action, pending] = useActionState(startGithubAppRegistrationAction, registrationInit);
  const [owner, setOwner] = useState("personal");
  if (state.registration) {
    return (
      <div className="mt-3 space-y-3">
        <p className="text-xs text-foreground-muted">
          Continue to GitHub to review and create the App under your chosen owner. You can cancel there.
          Return in this same browser within one hour. GitHub sends a temporary code back to Kars;
          Kars exchanges it server-side and stores the private key without displaying it.
          This does not install the App or grant access to repositories.
        </p>
        <form action={state.registration.url} method="post">
          <input type="hidden" name="manifest" value={state.registration.manifest} />
          <button type="submit" className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg">
            Continue to GitHub
          </button>
        </form>
        <a href="/console/configuration" className="inline-block text-xs text-signal underline">Cancel or change choices</a>
      </div>
    );
  }
  return (
    <form action={action} className="mt-3 space-y-3">
      <p className="text-xs text-foreground-muted">
        Requires an administrator signed in through SSO. You own the App on GitHub; no PEM upload is needed.
        Repository permissions: {servicePermissions}. Webhook delivery and installation-time OAuth are disabled.
      </p>
      <label className="block space-y-1 text-xs">
        <span>App name</span>
        <input name="name" required maxLength={34} placeholder="Your unique Kars App name" className={inputClass} />
      </label>
      <label className="block space-y-1 text-xs">
        <span>App owner</span>
        <select name="owner" value={owner} onChange={event => setOwner(event.target.value)} className={inputClass}>
          <option value="personal">My personal GitHub account</option>
          <option value="organization">A GitHub organization</option>
        </select>
      </label>
      {owner === "organization" && <label className="block space-y-1 text-xs">
        <span>Organization login</span>
        <input name="organization" required maxLength={39} placeholder="organization-name" className={inputClass} />
      </label>}
      <label className="block space-y-1 text-xs">
        <span>Who may install this App?</span>
        <select name="visibility" defaultValue="private" className={inputClass}>
          <option value="private">Only its owner (private App)</option>
          <option value="public">Other accounts too (public App)</option>
        </select>
      </label>
      <label className="flex items-start gap-2 text-xs text-foreground-muted">
        <input type="checkbox" name="security_signals" className="mt-0.5" />
        <span>Also request read access to Dependabot alerts, code scanning alerts and secret scanning alerts for engineering intake signals.</span>
      </label>
      <button type="submit" disabled={pending} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
        {pending ? "Preparing registration…" : "Prepare GitHub registration"}
      </button>
      {state.error && <p role="alert" className="text-xs text-danger">{state.error}</p>}
    </form>
  );
}
