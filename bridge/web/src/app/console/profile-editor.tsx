// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge Operator Console — visual KarsProfile (team profile) editor.
// Replaces the raw-JSON textarea (AuthorResource) for this one resource kind
// with real fields matching the actual CRD schema
// (controller/src/kars_profile.rs): displayName, domain, charterTemplate, a
// roles[] roster (each with systemPrompt + skills), defaultEnvelope
// (tier/authorityCeiling/delegationDepth/toolPolicyRef), the team's default
// toolPolicy, and knowledgeCommons. Submits through the SAME
// applyGovernanceAction the textarea used (name + a JSON "spec" string), so
// the backend contract is unchanged; only the authoring experience is
// visual. An "Edit as JSON" escape hatch stays available, mirroring
// InferencePolicyEditor.

import { useActionState, useState } from "react";
import { applyGovernanceAction, type GovState } from "./governance-actions";
import { useSpecEditor } from "./use-spec-editor";
import { GovernanceDialog } from "./governance-dialog";
import { Icon } from "@/components/icon";
import type { RefOption } from "@/lib/types";

const init: GovState = { error: null, ok: null };

const TIERS = [1, 2, 3, 4, 5] as const;

type Role = { name: string; systemPrompt: string; skills: string[] };

interface FormShape {
  displayName: string;
  domain: string;
  charterTemplate: string;
  tier: number;
  authorityCeiling: number;
  delegationDepth: number;
  envelopeToolPolicy: string;
  toolPolicy: string;
  knowledgeCommons: string;
  roles: Role[];
}

function parseSpec(spec: Record<string, unknown> | undefined): FormShape {
  const envelope = (spec?.defaultEnvelope as Record<string, unknown>) ?? {};
  const toolPolicyRef = (envelope.toolPolicyRef as Record<string, unknown>) ?? {};
  const roles = (spec?.roles as Record<string, unknown>[] | undefined) ?? [];
  return {
    displayName: (spec?.displayName as string) ?? "",
    domain: (spec?.domain as string) ?? "",
    charterTemplate: (spec?.charterTemplate as string) ?? "",
    tier: (envelope.tier as number) ?? 3,
    authorityCeiling: (envelope.authorityCeiling as number) ?? 2,
    delegationDepth: (envelope.delegationDepth as number) ?? 1,
    envelopeToolPolicy: (toolPolicyRef.name as string) ?? "",
    toolPolicy: (spec?.toolPolicy as string) ?? "",
    knowledgeCommons: (spec?.knowledgeCommons as string) ?? "",
    roles: roles.map((r) => ({
      name: (r.name as string) ?? "",
      systemPrompt: (r.systemPrompt as string) ?? "",
      skills: (r.skills as string[] | undefined) ?? [],
    })),
  };
}

function buildSpec(f: FormShape): Record<string, unknown> {
  const spec: Record<string, unknown> = {
    domain: f.domain.trim(),
    charterTemplate: f.charterTemplate.trim(),
  };
  if (f.displayName.trim()) spec.displayName = f.displayName.trim();
  if (f.toolPolicy.trim()) spec.toolPolicy = f.toolPolicy.trim();
  if (f.knowledgeCommons.trim()) spec.knowledgeCommons = f.knowledgeCommons.trim();

  const envelope: Record<string, unknown> = {
    tier: f.tier,
    authorityCeiling: f.authorityCeiling,
    delegationDepth: f.delegationDepth,
  };
  if (f.envelopeToolPolicy.trim()) envelope.toolPolicyRef = { name: f.envelopeToolPolicy.trim() };
  spec.defaultEnvelope = envelope;

  spec.roles = f.roles
    .map((r) => ({
      name: r.name.trim(),
      ...(r.systemPrompt.trim() ? { systemPrompt: r.systemPrompt.trim() } : {}),
      ...(r.skills.length > 0 ? { skills: r.skills } : {}),
    }));

  return spec;
}

export function ProfileEditor({
  initialName,
  initialSpec,
  toolPolicies,
  skills,
}: {
  initialName?: string;
  initialSpec?: Record<string, unknown>;
  toolPolicies: RefOption[];
  skills: RefOption[];
}) {
  const [open, setOpen] = useState(false);
  const [state, action, pending] = useActionState(applyGovernanceAction, init);
  const [name, setName] = useState(initialName ?? "");
  const [force, setForce] = useState(false);
  const editing = Boolean(initialName);
  const { f, setF, advanced, rawSpec, setRawSpec, specJson, modeError, toggleMode } =
    useSpecEditor(initialSpec, parseSpec, buildSpec);

  const patch = (p: Partial<FormShape>) => setF((prev) => ({ ...prev, ...p }));
  const patchRole = (i: number, p: Partial<Role>) => setF((prev) => ({ ...prev, roles: prev.roles.map((r, j) => (j === i ? { ...r, ...p } : r)) }));
  const envelopeError = !advanced && f.authorityCeiling > f.tier
    ? "Authority ceiling must not exceed tier. Review the envelope before saving."
    : null;

  return (
    <>
      <button
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
        className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
      >
        {editing ? `Edit ${initialName}` : "+ Add team profile"}
      </button>
      {open && <GovernanceDialog title={editing ? "Edit team profile" : "New team profile"} pending={pending} onClose={() => setOpen(false)}>
    <form action={action} aria-busy={pending} className="w-full min-w-0 bg-surface-muted p-4 [&_fieldset]:min-w-0">
      <fieldset disabled={pending} className="min-w-0 space-y-4">
      <input type="hidden" name="kind" value="KarsProfile" />
      <input type="hidden" name="spec" value={specJson} />
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium">{editing ? "Edit team profile" : "New team profile"}</p>
        <div className="flex items-center gap-3">
          <button type="button" onClick={toggleMode} className="text-xs text-foreground-muted hover:text-foreground">
            {advanced ? "Use visual form" : "Edit as JSON"}
          </button>
          <button type="button" disabled={pending} onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">Cancel</button>
        </div>
      </div>

      {modeError && <p role="alert" className="text-xs text-danger">{modeError}</p>}
      <input
        name="name"
        aria-label="team profile name"
        value={name}
        onChange={(event) => setName(event.target.value)}
        readOnly={editing}
        placeholder="name (lowercase-with-hyphens)"
        required
        className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm read-only:opacity-70"
      />

      {advanced ? (
        <textarea
          aria-label="Team profile specification JSON"
          value={rawSpec}
          onChange={(e) => setRawSpec(e.target.value)}
          rows={16}
          spellCheck={false}
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed"
        />
      ) : (
        <div className="space-y-4">
          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="person" size={13} /> Identity</legend>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Display name (optional)">
                <input value={f.displayName} onChange={(e) => patch({ displayName: e.target.value })} placeholder="e.g. Engineering maintainer" className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" />
              </Field>
              <Field label="Domain">
                <input required value={f.domain} onChange={(e) => patch({ domain: e.target.value })} placeholder="e.g. eng, finance, docs, soc, legal" className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" />
              </Field>
            </div>
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="note" size={13} /> Charter</legend>
            <textarea
              required
              aria-label="Profile charter"
              value={f.charterTemplate}
              onChange={(e) => patch({ charterTemplate: e.target.value })}
              rows={3}
              placeholder="The standing mandate a team instantiated from this profile adopts."
              className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm"
            />
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="shield" size={13} /> Default envelope</legend>
            <div className="grid gap-3 sm:grid-cols-3">
              <Field label="Tier">
                <select value={f.tier} onChange={(e) => patch({ tier: Number(e.target.value) })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  {TIERS.map((t) => <option key={t} value={t}>{t}</option>)}
                </select>
              </Field>
              <Field label="Authority ceiling">
                <select value={f.authorityCeiling} onChange={(e) => patch({ authorityCeiling: Number(e.target.value) })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  {TIERS.map((t) => <option key={t} value={t}>{t}</option>)}
                </select>
              </Field>
              <Field label="Delegation depth">
                <input type="number" required min={0} max={2147483647} step={1} value={f.delegationDepth} onChange={(e) => patch({ delegationDepth: Number(e.target.value) })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm tabular-nums" />
              </Field>
            </div>
            <div className="mt-3">
              <Field label="Envelope tool policy (optional)">
                <select value={f.envelopeToolPolicy} onChange={(e) => patch({ envelopeToolPolicy: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  <option value="">none</option>
                  {toolPolicies.map((tp) => <option key={tp.name} value={tp.name}>{tp.name}{tp.summary ? ` — ${tp.summary}` : ""}</option>)}
                </select>
              </Field>
            </div>
            <p className="mt-1.5 text-[11px] text-foreground-muted">Authority ceiling must be ≤ tier. Saving the profile does not confirm controller validation.</p>
            {envelopeError && <p role="alert" className="mt-2 text-xs text-danger">{envelopeError}</p>}
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="gear" size={13} /> Team defaults</legend>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Members' bounding tool policy (optional)">
                <select value={f.toolPolicy} onChange={(e) => patch({ toolPolicy: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  <option value="">cluster default (kars-default)</option>
                  {toolPolicies.map((tp) => <option key={tp.name} value={tp.name}>{tp.name}{tp.summary ? ` — ${tp.summary}` : ""}</option>)}
                </select>
              </Field>
              <Field label="Knowledge commons (optional)">
                <input value={f.knowledgeCommons} onChange={(e) => patch({ knowledgeCommons: e.target.value })} placeholder="team's own commons (default)" className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" />
              </Field>
            </div>
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="handshake" size={13} /> Roles</legend>
            <div className="space-y-2">
              {f.roles.map((r, i) => (
                <div key={i} className="rounded-lg border border-border bg-surface p-2.5">
                  <div className="flex items-center gap-2">
                    <input required aria-label={`Role ${i + 1} name`} value={r.name} onChange={(e) => patchRole(i, { name: e.target.value })} placeholder="role name (e.g. triager)" className="min-w-0 flex-1 rounded-lg border border-border bg-surface px-2.5 py-1.5 text-sm font-medium" />
                    <button type="button" aria-label={`Remove role ${r.name || i + 1}`} onClick={() => patch({ roles: f.roles.filter((_, j) => j !== i) })} className="shrink-0 text-foreground-muted hover:text-danger">
                      <Icon name="cross" size={13} />
                    </button>
                  </div>
                  <textarea
                    aria-label={`Role ${r.name || i + 1} instructions`}
                    value={r.systemPrompt}
                    onChange={(e) => patchRole(i, { systemPrompt: e.target.value })}
                    rows={2}
                    placeholder="standing instructions for this role…"
                    className="mt-2 w-full resize-y rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs"
                  />
                  {skills.length > 0 && (
                    <div className="mt-2 flex flex-wrap gap-1.5">
                      {skills.map((sk) => {
                        const on = r.skills.includes(sk.name);
                        return (
                          <button
                            key={sk.name}
                            type="button"
                            title={sk.summary ?? undefined}
                            aria-pressed={on}
                            onClick={() => patchRole(i, { skills: on ? r.skills.filter((s) => s !== sk.name) : [...r.skills, sk.name] })}
                            className={`rounded-full border px-2 py-0.5 text-[11px] font-medium ${on ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"}`}
                          >
                            {sk.name}
                          </button>
                        );
                      })}
                    </div>
                  )}
                </div>
              ))}
              <button type="button" onClick={() => patch({ roles: [...f.roles, { name: "", systemPrompt: "", skills: [] }] })} className="text-xs text-signal hover:underline">
                + Add role
              </button>
            </div>
          </fieldset>
        </div>
      )}

      <p className="text-[11px] text-foreground-muted">
        Saved with <span className="font-mono">kubectl apply</span> semantics (field manager <span className="font-mono">kars-bridge</span>). The API checks the schema; the controller validates the saved profile asynchronously. Check its status before creating a team.
      </p>
      <label className="flex items-center gap-2 text-[11px] text-foreground-muted">
        <input type="checkbox" name="force" checked={force} onChange={(event) => setForce(event.target.checked)} /> Force — take ownership of fields another manager owns (only on a conflict)
      </label>
      <div className="flex flex-wrap items-center gap-3">
        <button type="submit" disabled={pending || envelopeError !== null} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Applying…" : editing ? "Save changes" : "Create team profile"}
        </button>
        {state.error && <p role="alert" className="text-xs text-danger">{state.error}</p>}
        {state.ok && <p role="status" className="text-xs text-foreground-muted">{state.ok}</p>}
      </div>
      </fieldset>
    </form>
      </GovernanceDialog>}
    </>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block text-xs text-foreground-muted">
      {label}
      <div className="mt-1">{children}</div>
    </label>
  );
}
