"use client";

// kars Bridge — edit a standing team's org post-create (REQ13 "edit everything").
// The org chart is not frozen at creation: you can add/remove member roles and
// change each member's prompt, harness, model, and skills, then save. PATCHes
// the team's spec.roster; the controller reconciles member tasks to match.
// Mirrors the team composer's role grammar so create and edit feel identical.

import { useState, useTransition } from "react";
import { MEMBER_ARCHETYPES } from "@/lib/member-archetypes";
import { useRouter } from "next/navigation";
import type { Options, TeamRole } from "@/lib/types";
import { ViewportPortal } from "@/components/viewport-portal";

type EditRole = {
  id: number;
  name: string;
  system_prompt: string;
  runtime: string;
  model: string;
  skills: string[];
};

let RID = 1;

export function TeamRosterEdit({
  ns,
  name,
  roster,
  options,
}: {
  ns: string;
  name: string;
  roster: TeamRole[];
  options: Options;
}) {
  const [open, setOpen] = useState(false);
  const [roles, setRoles] = useState<EditRole[]>(() =>
    roster.map((r) => {
      const opt = r.model
        ? options.models.find((m) =>
            r.model?.includes("::")
              ? `${m.provider}::${m.deployment}` === r.model
              : m.deployment === r.model,
          )
        : undefined;
      return {
        id: RID++,
        name: r.name,
        system_prompt: r.system_prompt ?? "",
        runtime: r.runtime ?? "",
        model: opt ? `${opt.provider}::${opt.deployment}` : "",
        skills: r.skills,
      };
    }),
  );
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const router = useRouter();

  const patch = (id: number, p: Partial<EditRole>) =>
    setRoles((rs) => rs.map((r) => (r.id === id ? { ...r, ...p } : r)));
  const add = () =>
    setRoles((rs) => [...rs, { id: RID++, name: "", system_prompt: "", runtime: "", model: "", skills: [] }]);
  const remove = (id: number) => setRoles((rs) => rs.filter((r) => r.id !== id));
  const availableSkills = new Set((options.skills ?? []).map((s) => s.name));
  const addFromArchetype = (aid: string) => {
    const a = MEMBER_ARCHETYPES.find((x) => x.id === aid);
    if (!a) return;
    setRoles((rs) => [
      ...rs,
      { id: RID++, name: a.id, system_prompt: a.system_prompt, runtime: "", model: "", skills: a.suggestedSkills.filter((s) => availableSkills.has(s)) },
    ]);
  };

  const save = () =>
    start(async () => {
      setError(null);
      const payload = roles
        .filter((r) => r.name.trim())
        .map(({ name, system_prompt, runtime, model, skills }) => ({
          name,
          system_prompt,
          runtime: runtime || undefined,
          model: model || undefined,
          skills,
        }));
      try {
        const res = await fetch(`/api/namespaces/${ns}/teams/${name}`, {
          method: "PATCH",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ roles: payload }),
        });
        if (!res.ok) {
          const body = await res.json().catch(() => null);
          throw new Error(body?.error?.message ?? `Save failed (${res.status})`);
        }
        router.refresh();
        setOpen(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : "Save failed");
      }
    });

  const wired = options.runtimes.filter((r) => r.wired);

  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted"
      >
        Edit org
      </button>
    );
  }

  return (
    <ViewportPortal onClose={() => setOpen(false)}>
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Edit team org"
        className="fixed inset-0 z-[100] overflow-auto bg-surface p-5"
      >
      <div className="flex items-center justify-between">
        <div>
          <p className="text-base font-semibold">Edit team org</p>
          <p className="text-xs text-foreground-muted">Full-width role, harness, model, prompt, and skill editor</p>
        </div>
        <button onClick={() => setOpen(false)} className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">
          Close
        </button>
      </div>
      <p className="mt-0.5 text-xs text-foreground-muted">
        A visual org: the team leads; each card is a member reporting to it. Edit a card in place, add
        from an archetype, or remove. Saving reconciles the running org.
      </p>

      {/* Visual org tree — principal on top, a spine, then member cards. */}
      <div className="mt-4 flex flex-col items-center">
        <div className="w-full max-w-xs rounded-xl border border-signal/40 bg-signal/[0.08] px-4 py-2.5 text-center shadow-sm">
          <p className="truncate text-sm font-semibold">{name}</p>
          <p className="text-[10px] font-semibold uppercase tracking-wide text-signal">Principal · team lead</p>
        </div>
        {roles.length > 0 && <div className="h-4 w-px bg-border" aria-hidden />}
        {roles.length > 1 && <div className="h-px w-4/5 bg-border" aria-hidden />}

        <ul className="mt-0 flex w-full max-w-full flex-nowrap items-start justify-start gap-3 overflow-x-auto pb-2 sm:justify-center">
          {roles.map((r) => (
            <li key={r.id} className="flex flex-col items-center">
              <div className="h-3 w-px bg-border" aria-hidden />
              <div className="w-60 rounded-xl border border-border bg-surface p-3 shadow-sm">
                <div className="flex items-center gap-2">
                  <input
                    value={r.name}
                    onChange={(e) => patch(r.id, { name: e.target.value })}
                    placeholder="role name (e.g. researcher)"
                    className="flex-1 rounded-lg border border-border bg-surface px-2 py-1 text-sm font-medium"
                  />
                  <button
                    onClick={() => remove(r.id)}
                    title="Remove member"
                    className="rounded-lg border border-border px-1.5 py-1 text-xs text-foreground-muted hover:bg-surface-muted"
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  value={r.system_prompt}
                  onChange={(e) => patch(r.id, { system_prompt: e.target.value })}
                  rows={2}
                  placeholder="what this member does"
                  className="mt-2 w-full resize-y rounded-lg border border-border bg-surface px-2 py-1 text-[11px]"
                />
                <div className="mt-2 grid grid-cols-1 gap-1.5">
                  <select
                    value={r.model}
                    onChange={(e) => patch(r.id, { model: e.target.value })}
                    className="rounded-lg border border-border bg-surface px-2 py-1 text-[11px]"
                  >
                    <option value="">model: team default</option>
                    {options.models.map((m) => (
                      <option key={`${m.provider}::${m.deployment}`} value={`${m.provider}::${m.deployment}`}>{m.deployment}</option>
                    ))}
                  </select>
                  <select
                    value={r.runtime}
                    onChange={(e) => patch(r.id, { runtime: e.target.value })}
                    className="rounded-lg border border-border bg-surface px-2 py-1 text-[11px]"
                  >
                    <option value="">harness: team default</option>
                    {wired.map((rt) => (
                      <option key={rt.kind} value={rt.kind}>{rt.label}</option>
                    ))}
                  </select>
                  {r.skills.length > 0 && (
                    <p className="text-[10px] text-foreground-muted">skills: {r.skills.join(", ")}</p>
                  )}
                </div>
              </div>
            </li>
          ))}
          {/* Add-member node, visually part of the tree. */}
          <li className="flex flex-col items-center">
            {roles.length > 0 && <div className="h-3 w-px bg-border" aria-hidden />}
            <div className="grid w-60 place-items-center gap-2 rounded-xl border border-dashed border-border bg-surface-muted/30 p-3">
              <button onClick={add} className="w-full rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">
                + Add member
              </button>
              <select
                value=""
                onChange={(e) => { if (e.target.value) addFromArchetype(e.target.value); e.target.value = ""; }}
                className="w-full rounded-lg border border-border bg-surface px-2 py-1.5 text-xs text-foreground-muted"
                title="Add a pre-defined member archetype"
              >
                <option value="">+ from archetype…</option>
                {MEMBER_ARCHETYPES.map((a) => (
                  <option key={a.id} value={a.id}>{a.icon} {a.title}</option>
                ))}
              </select>
            </div>
          </li>
        </ul>
      </div>

      <div className="mt-4 flex items-center">
        <button
          onClick={save}
          disabled={pending}
          className="ml-auto rounded-lg bg-signal px-4 py-1.5 text-xs font-semibold text-signal-fg disabled:opacity-50"
        >
          {pending ? "Saving…" : "Save org"}
        </button>
      </div>
      {error && <p className="mt-2 text-xs text-danger">{error}</p>}
      </div>
    </ViewportPortal>
  );
}
