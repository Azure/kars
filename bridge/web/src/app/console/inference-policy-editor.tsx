"use client";

// kars Bridge Operator Console — visual InferencePolicy editor. Replaces the
// raw-JSON textarea (AuthorResource) for this one resource kind with real
// fields matching the actual CRD schema (controller/src/inference_policy.rs):
// appliesTo (sandbox selector), tokenBudget (daily/monthly/per-request caps),
// contentSafety (four severity floors + Prompt Shields), and modelPreference
// (primary + fallback chain, picked from the cluster's real wired models —
// not typed by hand). Submits through the SAME applyGovernanceAction the
// textarea used (name + a JSON "spec" string), so the backend contract is
// unchanged; only the authoring experience is visual. An "Edit as JSON"
// escape hatch stays available for the one case a form can't express
// (bundleRef — a signed OCI policy bundle).

import { useActionState, useState } from "react";
import { applyGovernanceAction, type GovState } from "./governance-actions";
import { Icon } from "@/components/icon";
import type { ModelOption } from "@/lib/types";

const init: GovState = { error: null, ok: null };

const SEVERITIES = ["", "Safe", "Low", "Medium", "High"] as const;
const ACTIONS = ["", "chat", "responses", "image", "embeddings", "*"] as const;

type ModelRef = { provider: string; deployment: string };

interface FormShape {
  displayName: string;
  sandboxName: string;
  labels: { key: string; value: string }[];
  action: string;
  dailyTokens: string;
  monthlyTokens: string;
  perRequestTokens: string;
  hate: string;
  selfHarm: string;
  sexual: string;
  violence: string;
  requirePromptShields: boolean;
  primary: string; // "provider::deployment" or ""
  fallback: string[];
}

function modelKey(m: ModelRef): string {
  return `${m.provider}::${m.deployment}`;
}

function parseSpec(spec: Record<string, unknown> | undefined): FormShape {
  const appliesTo = (spec?.appliesTo as Record<string, unknown>) ?? {};
  const sandboxMatchLabels = (appliesTo.sandboxMatchLabels as Record<string, string>) ?? {};
  const tokenBudget = (spec?.tokenBudget as Record<string, unknown>) ?? {};
  const contentSafety = (spec?.contentSafety as Record<string, unknown>) ?? {};
  const modelPreference = (spec?.modelPreference as Record<string, unknown>) ?? {};
  const primary = modelPreference.primary as ModelRef | undefined;
  const fallback = (modelPreference.fallback as ModelRef[] | undefined) ?? [];
  return {
    displayName: (spec?.displayName as string) ?? "",
    sandboxName: (appliesTo.sandboxName as string) ?? "",
    labels: Object.entries(sandboxMatchLabels).map(([key, value]) => ({ key, value })),
    action: (appliesTo.action as string) ?? "",
    dailyTokens: tokenBudget.dailyTokens != null ? String(tokenBudget.dailyTokens) : "",
    monthlyTokens: tokenBudget.monthlyTokens != null ? String(tokenBudget.monthlyTokens) : "",
    perRequestTokens: tokenBudget.perRequestTokens != null ? String(tokenBudget.perRequestTokens) : "",
    hate: (contentSafety.hate as string) ?? "",
    selfHarm: (contentSafety.selfHarm as string) ?? "",
    sexual: (contentSafety.sexual as string) ?? "",
    violence: (contentSafety.violence as string) ?? "",
    requirePromptShields: contentSafety.requirePromptShields === true,
    primary: primary ? modelKey(primary) : "",
    fallback: fallback.map(modelKey),
  };
}

function buildSpec(f: FormShape): Record<string, unknown> {
  const spec: Record<string, unknown> = {};
  if (f.displayName.trim()) spec.displayName = f.displayName.trim();

  const sandboxMatchLabels: Record<string, string> = {};
  for (const { key, value } of f.labels) {
    if (key.trim()) sandboxMatchLabels[key.trim()] = value.trim();
  }
  const appliesTo: Record<string, unknown> = { sandboxMatchLabels };
  if (f.sandboxName.trim()) appliesTo.sandboxName = f.sandboxName.trim();
  if (f.action) appliesTo.action = f.action;
  spec.appliesTo = appliesTo;

  const tokenBudget: Record<string, unknown> = {};
  if (f.dailyTokens.trim()) tokenBudget.dailyTokens = Number(f.dailyTokens);
  if (f.monthlyTokens.trim()) tokenBudget.monthlyTokens = Number(f.monthlyTokens);
  if (f.perRequestTokens.trim()) tokenBudget.perRequestTokens = Number(f.perRequestTokens);
  if (Object.keys(tokenBudget).length > 0) spec.tokenBudget = tokenBudget;

  const contentSafety: Record<string, unknown> = {};
  if (f.hate) contentSafety.hate = f.hate;
  if (f.selfHarm) contentSafety.selfHarm = f.selfHarm;
  if (f.sexual) contentSafety.sexual = f.sexual;
  if (f.violence) contentSafety.violence = f.violence;
  if (f.requirePromptShields) contentSafety.requirePromptShields = true;
  if (Object.keys(contentSafety).length > 0) spec.contentSafety = contentSafety;

  if (f.primary) {
    const [provider, deployment] = f.primary.split("::");
    const modelPreference: Record<string, unknown> = { primary: { provider, deployment } };
    const fb = f.fallback.filter(Boolean).map((k) => {
      const [provider, deployment] = k.split("::");
      return { provider, deployment };
    });
    if (fb.length > 0) modelPreference.fallback = fb;
    spec.modelPreference = modelPreference;
  }

  return spec;
}

export function InferencePolicyEditor({
  initialName,
  initialSpec,
  models,
}: {
  initialName?: string;
  initialSpec?: Record<string, unknown>;
  models: ModelOption[];
}) {
  const [open, setOpen] = useState(false);
  const [state, action, pending] = useActionState(applyGovernanceAction, init);
  const editing = Boolean(initialName);
  const [advanced, setAdvanced] = useState(false);
  const [f, setF] = useState<FormShape>(() => parseSpec(initialSpec));
  const [rawSpec, setRawSpec] = useState(() => JSON.stringify(initialSpec ?? {}, null, 2));

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
      >
        {editing ? `Edit ${initialName}` : "+ Add inference policy"}
      </button>
    );
  }

  const specJson = advanced ? rawSpec : JSON.stringify(buildSpec(f));
  const patch = (p: Partial<FormShape>) => setF((prev) => ({ ...prev, ...p }));

  return (
    <form action={action} className="mt-2 space-y-4 rounded-lg border border-border bg-surface-muted p-4">
      <input type="hidden" name="kind" value="InferencePolicy" />
      <input type="hidden" name="spec" value={specJson} />
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium">{editing ? `Edit inference policy` : "New inference policy"}</p>
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => setAdvanced((v) => !v)}
            className="text-xs text-foreground-muted hover:text-foreground"
          >
            {advanced ? "Use visual form" : "Edit as JSON"}
          </button>
          <button type="button" onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">Cancel</button>
        </div>
      </div>

      <input
        name="name"
        defaultValue={initialName}
        readOnly={editing}
        placeholder="name (lowercase-with-hyphens)"
        required
        className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm read-only:opacity-70"
      />

      {advanced ? (
        <textarea
          value={rawSpec}
          onChange={(e) => setRawSpec(e.target.value)}
          rows={14}
          spellCheck={false}
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed"
        />
      ) : (
        <div className="space-y-4">
          <Field label="Display name">
            <input
              value={f.displayName}
              onChange={(e) => patch({ displayName: e.target.value })}
              placeholder="e.g. Research team — standard budget"
              className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
            />
          </Field>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="target" size={13} /> Applies to</legend>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Exact sandbox (optional)">
                <input
                  value={f.sandboxName}
                  onChange={(e) => patch({ sandboxName: e.target.value })}
                  placeholder="leave blank to match by label"
                  className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
                />
              </Field>
              <Field label="Inference action">
                <select
                  value={f.action}
                  onChange={(e) => patch({ action: e.target.value })}
                  className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
                >
                  {ACTIONS.map((a) => <option key={a} value={a}>{a === "" ? "Any" : a}</option>)}
                </select>
              </Field>
            </div>
            <div className="mt-2">
              <p className="mb-1 text-xs text-foreground-muted">Sandbox labels (AND)</p>
              {f.labels.map((row, i) => (
                <div key={i} className="mb-1.5 flex items-center gap-2">
                  <input
                    value={row.key}
                    onChange={(e) => patch({ labels: f.labels.map((r, j) => (j === i ? { ...r, key: e.target.value } : r)) })}
                    placeholder="kars.azure.com/team"
                    className="w-1/2 rounded-lg border border-border bg-surface px-2.5 py-1.5 font-mono text-xs"
                  />
                  <input
                    value={row.value}
                    onChange={(e) => patch({ labels: f.labels.map((r, j) => (j === i ? { ...r, value: e.target.value } : r)) })}
                    placeholder="value"
                    className="w-1/2 rounded-lg border border-border bg-surface px-2.5 py-1.5 font-mono text-xs"
                  />
                  <button type="button" onClick={() => patch({ labels: f.labels.filter((_, j) => j !== i) })} className="shrink-0 text-foreground-muted hover:text-danger">
                    <Icon name="cross" size={13} />
                  </button>
                </div>
              ))}
              <button
                type="button"
                onClick={() => patch({ labels: [...f.labels, { key: "", value: "" }] })}
                className="text-xs text-signal hover:underline"
              >
                + Add label
              </button>
            </div>
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="coin" size={13} /> Token budget</legend>
            <div className="grid gap-3 sm:grid-cols-3">
              <Field label="Daily cap">
                <input type="number" min={0} value={f.dailyTokens} onChange={(e) => patch({ dailyTokens: e.target.value })} placeholder="no cap" className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm tabular-nums" />
              </Field>
              <Field label="Monthly cap">
                <input type="number" min={0} value={f.monthlyTokens} onChange={(e) => patch({ monthlyTokens: e.target.value })} placeholder="no cap" className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm tabular-nums" />
              </Field>
              <Field label="Per-request cap">
                <input type="number" min={0} value={f.perRequestTokens} onChange={(e) => patch({ perRequestTokens: e.target.value })} placeholder="no cap" className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm tabular-nums" />
              </Field>
            </div>
            <p className="mt-1.5 text-[11px] text-foreground-muted">Monthly must be ≥ daily and ≥ per-request — the cluster validates this on save.</p>
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="shield" size={13} /> Content safety floor</legend>
            <div className="grid gap-3 sm:grid-cols-4">
              <Field label="Hate">
                <select value={f.hate} onChange={(e) => patch({ hate: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  {SEVERITIES.map((s) => <option key={s} value={s}>{s || "Governance default"}</option>)}
                </select>
              </Field>
              <Field label="Self-harm">
                <select value={f.selfHarm} onChange={(e) => patch({ selfHarm: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  {SEVERITIES.map((s) => <option key={s} value={s}>{s || "Governance default"}</option>)}
                </select>
              </Field>
              <Field label="Sexual">
                <select value={f.sexual} onChange={(e) => patch({ sexual: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  {SEVERITIES.map((s) => <option key={s} value={s}>{s || "Governance default"}</option>)}
                </select>
              </Field>
              <Field label="Violence">
                <select value={f.violence} onChange={(e) => patch({ violence: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                  {SEVERITIES.map((s) => <option key={s} value={s}>{s || "Governance default"}</option>)}
                </select>
              </Field>
            </div>
            <label className="mt-2 flex items-center gap-2 text-xs text-foreground-muted">
              <input type="checkbox" checked={f.requirePromptShields} onChange={(e) => patch({ requirePromptShields: e.target.checked })} />
              Require Prompt Shields (jailbreak / indirect-injection detection)
            </label>
          </fieldset>

          <fieldset className="rounded-lg border border-border p-3">
            <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="brain" size={13} /> Model preference</legend>
            <Field label="Primary route">
              <select value={f.primary} onChange={(e) => patch({ primary: e.target.value })} className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                <option value="">Inherit controller default</option>
                {models.map((m) => <option key={modelKey(m)} value={modelKey(m)}>{m.provider} :: {m.deployment}{m.is_default ? " (default)" : ""}</option>)}
              </select>
            </Field>
            {f.primary && (
              <div className="mt-2">
                <p className="mb-1 text-xs text-foreground-muted">Fallback chain (tried in order on primary failure)</p>
                {f.fallback.map((val, i) => (
                  <div key={i} className="mb-1.5 flex items-center gap-2">
                    <select
                      value={val}
                      onChange={(e) => patch({ fallback: f.fallback.map((v, j) => (j === i ? e.target.value : v)) })}
                      className="w-full rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs"
                    >
                      <option value="">— select a model —</option>
                      {models.map((m) => <option key={modelKey(m)} value={modelKey(m)}>{m.provider} :: {m.deployment}</option>)}
                    </select>
                    <button type="button" onClick={() => patch({ fallback: f.fallback.filter((_, j) => j !== i) })} className="shrink-0 text-foreground-muted hover:text-danger">
                      <Icon name="cross" size={13} />
                    </button>
                  </div>
                ))}
                <button type="button" onClick={() => patch({ fallback: [...f.fallback, ""] })} className="text-xs text-signal hover:underline">
                  + Add fallback route
                </button>
              </div>
            )}
          </fieldset>
        </div>
      )}

      <p className="text-[11px] text-foreground-muted">
        Applied with <span className="font-mono">kubectl apply</span> semantics (field manager <span className="font-mono">kars-bridge</span>). The cluster validates it — invalid specs are rejected with the API server&apos;s own message.
      </p>
      <label className="flex items-center gap-2 text-[11px] text-foreground-muted">
        <input type="checkbox" name="force" /> Force — take ownership of fields another manager owns (only on a conflict)
      </label>
      <div className="flex items-center gap-3">
        <button type="submit" disabled={pending} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Applying…" : editing ? "Save changes" : "Create inference policy"}
        </button>
        {state.error && <p className="text-xs text-danger">{state.error}</p>}
        {state.ok && <p className="text-xs text-ok">{state.ok}</p>}
      </div>
    </form>
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
