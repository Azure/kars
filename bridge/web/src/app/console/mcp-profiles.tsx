// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// Operator MCP profiles — curate named, vetted bundles of McpServers so users
// compose from approved groupings (e.g. "research", "devops") instead of
// wiring servers one by one. A profile may only reference McpServers that
// already exist on the cluster (the BFF enforces this), so a bundle can never
// smuggle in an unvetted server.

import { useActionState, useState } from "react";
import { Icon } from "@/components/icon";
import { saveMcpProfileAction, deleteMcpProfileAction, type McpProfileState } from "./mcp-profile-actions";
import type { McpProfileOption, RefOption } from "@/lib/types";

const init: McpProfileState = { error: null, ok: null };

export function McpProfiles({ profiles, servers }: { profiles: McpProfileOption[]; servers: RefOption[] }) {
  const [creating, setCreating] = useState(false);

  return (
    <div className="space-y-3">
      {profiles.length === 0 ? (
        <p className="text-xs text-foreground-muted">No MCP profiles yet — bundle a vetted set of servers users can add as one.</p>
      ) : (
        <ul className="space-y-2">
          {profiles.map((p) => (
            <ProfileRow key={p.name} profile={p} servers={servers} />
          ))}
        </ul>
      )}
      {servers.length === 0 ? (
        <p className="text-[11px] text-foreground-muted">Register McpServers first — a profile bundles existing servers.</p>
      ) : !creating ? (
        <button type="button" onClick={() => setCreating(true)} className="rounded-md border border-border px-2.5 py-1 text-[11px] font-medium hover:bg-surface-muted">
          + New profile
        </button>
      ) : (
        <ProfileForm servers={servers} onClose={() => setCreating(false)} />
      )}
    </div>
  );
}

function ProfileRow({ profile, servers }: { profile: McpProfileOption; servers: RefOption[] }) {
  const [editing, setEditing] = useState(false);
  const [delState, delAction, delPending] = useActionState(deleteMcpProfileAction, init);
  if (editing) return <ProfileForm servers={servers} initial={profile} onClose={() => setEditing(false)} />;
  return (
    <li className="rounded-lg border border-border bg-surface px-3 py-2 text-sm">
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-1.5">
          <span aria-hidden><Icon name="box" size={14} /></span>
          <span className="font-medium">{profile.name}</span>
          <span className="text-[11px] text-foreground-muted">{profile.servers.length} server{profile.servers.length === 1 ? "" : "s"}</span>
        </span>
        <div className="flex items-center gap-2">
          <button type="button" onClick={() => setEditing(true)} className="text-[11px] text-foreground-muted hover:text-foreground">Edit</button>
          <form action={delAction}>
            <input type="hidden" name="name" value={profile.name} />
            <button type="submit" disabled={delPending} className="text-[11px] text-foreground-muted hover:text-danger disabled:opacity-50">Remove</button>
          </form>
        </div>
      </div>
      {profile.summary && <p className="mt-0.5 text-[11px] text-foreground-muted">{profile.summary}</p>}
      <p className="mt-1 flex flex-wrap gap-1">
        {profile.servers.map((s) => <span key={s} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px]">{s}</span>)}
      </p>
      {delState.error && <span className="text-[11px] text-danger">{delState.error}</span>}
    </li>
  );
}

function ProfileForm({ servers, initial, onClose }: { servers: RefOption[]; initial?: McpProfileOption; onClose: () => void }) {
  const [state, action, pending] = useActionState(saveMcpProfileAction, init);
  const [sel, setSel] = useState<string[]>(initial?.servers ?? []);
  const [name, setName] = useState(initial?.name ?? "");
  const [summary, setSummary] = useState(initial?.summary ?? "");
  if (state.ok) return (
    <div className="flex items-center gap-2 text-[11px]">
      <p role="status" className="text-signal">{state.ok}</p>
      <button type="button" onClick={onClose} className="text-foreground-muted hover:text-foreground">Done</button>
    </div>
  );
  return (
    <form action={action} aria-busy={pending} className="rounded-lg border border-border bg-surface-muted/30 p-3">
      <fieldset disabled={pending} className="space-y-2">
        {initial ? (
          <input type="hidden" name="name" value={initial.name} />
        ) : (
          <input name="name" aria-label="MCP profile name" value={name} onChange={(e) => setName(e.target.value)} required placeholder="profile name (e.g. research)" className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm" />
        )}
        <input name="summary" aria-label="MCP profile description" value={summary} onChange={(e) => setSummary(e.target.value)} placeholder="short description (optional)" className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs" />
        <div className="flex flex-wrap gap-1.5">
          {servers.map((s) => {
            const on = sel.includes(s.name);
            return (
              <label key={s.name} className={`cursor-pointer rounded-full border px-2 py-0.5 text-[11px] focus-within:ring-2 focus-within:ring-signal ${on ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted"}`}>
                <input type="checkbox" name="servers" value={s.name} checked={on} onChange={(e) => setSel((c) => e.target.checked ? [...c, s.name] : c.filter((x) => x !== s.name))} className="sr-only" />
                {on && <span aria-hidden>✓ </span>}{s.name}
              </label>
            );
          })}
        </div>
        <div className="flex items-center gap-2">
          <button type="submit" disabled={pending || sel.length === 0} className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold text-signal disabled:opacity-50">
            {pending ? "Saving…" : "Save profile"}
          </button>
          <button type="button" disabled={pending} onClick={onClose} className="text-[11px] text-foreground-muted hover:text-foreground disabled:opacity-50">Cancel</button>
          {state.error && <span role="alert" className="text-[11px] text-danger">{state.error}</span>}
        </div>
      </fieldset>
    </form>
  );
}
