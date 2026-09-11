"use client";

import { useActionState, useState } from "react";
import { putCredentialAction, type CredState } from "./credential-actions";
import { credentialReviewMatches } from "@/lib/credential-review";

const init: CredState = { error: null, ok: null, review: null, pending: null };

// Source authoring is governed by the core workspace grant, not runtime Secret access.
export function CredentialForm() {
  const [state, action, pending] = useActionState(putCredentialAction, init);
  const [target, setTarget] = useState("");
  const [key, setKey] = useState("");
  const [kind, setKind] = useState("");
  const [namespace, setNamespace] = useState("kars-system");
  const [targetUid, setTargetUid] = useState("");
  const t = target.trim().toLowerCase();
  const k = key.trim();
  const selectedKind = kind === "KarsSandbox" || kind === "KarsTask" || kind === "KarsTeam" ? kind : null;
  const reviewed = selectedKind && state.review && credentialReviewMatches(state.review, {
    kind: selectedKind, namespace: namespace.trim(), target: t, key: k,
    ...(targetUid.trim() ? { targetUid: targetUid.trim() } : {}),
  }) ? state.review : null;
  return (
    <form action={action} className="space-y-3">
      <div className="grid gap-2 sm:grid-cols-3">
        <select name="kind" value={kind} onChange={(e) => setKind(e.target.value)} required
          aria-label="Credential target kind"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm">
          <option value="">Select target kind</option>
          <option value="KarsSandbox">Sandbox</option>
          <option value="KarsTask">Mission (KarsTask)</option>
          <option value="KarsTeam">Standing team</option>
        </select>
        <input name="namespace" value={namespace} onChange={(e) => setNamespace(e.target.value)}
          aria-label="Credential workspace namespace" required
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm" />
        <input name="targetUid" value={targetUid} onChange={(e) => setTargetUid(e.target.value)}
          placeholder="Expected target UID (optional for first review)"
          aria-label="Reviewed target UID"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm" />
      </div>
      <div className="grid gap-2 sm:grid-cols-3">
        <input name="target" value={target} onChange={(e) => setTarget(e.target.value)} placeholder="target name (e.g. repo-watch)" required
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
        <input name="key" value={key} onChange={(e) => setKey(e.target.value)} placeholder="env var (e.g. GITHUB_TOKEN)" required
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
        <input name="value" type="password" placeholder={reviewed?.continuation ? "re-enter the same value" : "value"}
          required={reviewed !== null} autoComplete="new-password"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
      </div>
      {(t || k) && (
        <p className="text-[11px] text-foreground-muted">
          Stores a governed source in workspace <span className="font-mono text-foreground">{namespace}</span>;
          no runtime namespace is created. {k && <>The operator grant must permit <span className="font-mono text-foreground">{k}</span>.</>}
          {" "}Delivery waits for captured target/source UIDs and controller acknowledgement.
        </p>
      )}
      {state.pending?.receipt.source && (
        <p className="rounded-lg border border-border p-3 text-xs text-foreground-muted">
          Acknowledged source write: <code>{state.pending.receipt.source.name}</code>,
          UID <code>{state.pending.receipt.source.uid}</code>, version <code>{state.pending.receipt.source.version}</code>.
          No source deletion or automatic resubmission is permitted. Refresh and review before resuming.
        </p>
      )}
      {state.pending && !state.pending.receipt.source && (
        <p className="rounded-lg border border-border p-3 text-xs text-foreground-muted">
          No source write was attempted. A fresh review may advance status-only versions, but cannot accept
          a changed target, grant, source or intent.
        </p>
      )}
      {reviewed && (
        <fieldset key={`${reviewed.expiresAt}:${reviewed.submission}:${reviewed.metadata.target.version}`}
          className="space-y-2 rounded-lg border border-border p-3 text-xs">
          <legend className="px-1 font-semibold">Current metadata review</legend>
          <dl className="grid gap-1 break-all">
            <div>Target UID: <code>{reviewed.metadata.target.uid ?? "not created - unbound staging"}</code></div>
            <div>Generation / version: <code>{reviewed.metadata.target.generation ?? "-"}</code> / <code>{reviewed.metadata.target.version ?? "-"}</code></div>
            <div>Full target intent: <code>{reviewed.metadata.target.intent ?? "no existing target"}</code></div>
            <div>Grant UID / generation: <code>{reviewed.metadata.grant.uid}</code> / <code>{reviewed.metadata.grant.generation}</code></div>
            <div>Grant authority: <code>{reviewed.metadata.grant.intent}</code></div>
            <div>Source UID / version: <code>{reviewed.metadata.source.uid ?? "not inventoried; exclusive CREATE only"}</code> / <code>{reviewed.metadata.source.version ?? "-"}</code></div>
            <div>Credential key: <code>{reviewed.metadata.key}</code>; submission {reviewed.submission} of 3</div>
          </dl>
          <label className="flex items-start gap-2">
            <input type="checkbox" name="confirmed" required />
            <span>I reviewed this target, complete intent fingerprint, grant and source identity.
              {reviewed.continuation && " Keep the same credential value."}
              {reviewed.bindingOnly && " Resume binding only this acknowledged source write."}</span>
          </label>
        </fieldset>
      )}
      <div className="flex flex-wrap gap-2">
        <button type="submit" name="operation" value="review" formNoValidate disabled={pending}
          className="rounded-lg border border-border px-4 py-2 text-sm font-semibold disabled:opacity-50">
          {state.pending ? "Refresh and review current metadata" : "Review credential metadata"}
        </button>
        <button type="submit" name="operation" value="store" disabled={pending || !reviewed || !!state.pending}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Working…" : reviewed?.bindingOnly ? "Confirm and resume binding" : "Confirm and store credential"}
        </button>
        <button type="submit" name="operation" value="reset" formNoValidate disabled={pending}
          className="rounded-lg border border-border px-4 py-2 text-sm disabled:opacity-50">
          Start a new change
        </button>
      </div>
      {state.error && <p className="text-xs text-danger">{state.error}</p>}
      {state.ok && <p className="text-xs text-ok">{state.ok}</p>}
    </form>
  );
}
