"use client";

// kars Bridge — structured AGT tool-policy builder. Toggle capability presets,
// add custom allow/deny rules, set approval — and it generates a valid ToolPolicy
// (no hand-written YAML). A collapsible live preview shows the generated policy
// for the curious; submit goes through the same SSA path as manual authoring.

import { useActionState, useMemo, useState } from "react";
import { Icon } from "@/components/icon";
import { applyGovernanceAction, type GovState } from "./governance-actions";
import {
  POLICY_PRESETS,
  buildToolPolicySpec,
  generateAgtProfile,
  type CustomRule,
} from "./policy-builder-data";

const init: GovState = { error: null, ok: null };
let RID = 1;

export function PolicyBuilder() {
  const [open, setOpen] = useState(false);
  const [state, action, pending] = useActionState(applyGovernanceAction, init);

  const [name, setName] = useState("");
  const [labelKey, setLabelKey] = useState("kars.azure.com/system-default");
  const [labelValue, setLabelValue] = useState("true");
  const [toolGlob, setToolGlob] = useState("*");
  const [presets, setPresets] = useState<Set<string>>(
    () => new Set(POLICY_PRESETS.filter((p) => p.defaultOn).map((p) => p.id)),
  );
  const [custom, setCustom] = useState<CustomRule[]>([]);
  const [approvalMode, setApprovalMode] = useState<"none" | "always" | "aboveThreshold">("none");
  const [approvalThreshold, setApprovalThreshold] = useState("USD 25.00");
  const [showYaml, setShowYaml] = useState(false);

  const spec = useMemo(
    () =>
      JSON.stringify(
        buildToolPolicySpec({
          agent: name,
          sandboxLabelKey: labelKey,
          sandboxLabelValue: labelValue,
          toolGlob,
          presetIds: presets,
          custom,
          approvalMode,
          approvalThreshold,
        }),
        null,
        2,
      ),
    [name, labelKey, labelValue, toolGlob, presets, custom, approvalMode, approvalThreshold],
  );
  const yaml = useMemo(() => generateAgtProfile(name, presets, custom), [name, presets, custom]);

  const togglePreset = (id: string) =>
    setPresets((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-lg border border-signal/40 bg-signal/10 px-3 py-1.5 text-xs font-medium text-signal hover:bg-signal/15"
      >
        <Icon name="wrench" size={14} className="inline mr-1" /> Build a policy
      </button>
    );
  }

  if (state.ok) {
    return (
      <div className="mt-2 rounded-xl border border-ok/30 bg-ok/5 p-4 text-sm">
        <p className="font-medium text-ok">Policy created.</p>
        <p className="mt-0.5 text-xs text-foreground-muted">{state.ok}</p>
        <button type="button" onClick={() => setOpen(false)} className="mt-2 rounded-md border border-border px-2 py-1 text-xs hover:bg-surface-muted">Done</button>
      </div>
    );
  }

  return (
    <form action={action} className="mt-2 space-y-4 rounded-xl border border-border bg-surface p-4">
      <input type="hidden" name="kind" value="ToolPolicy" />
      <input type="hidden" name="name" value={name} />
      <input type="hidden" name="spec" value={spec} />

      <div className="flex items-center justify-between">
        <div>
          <p className="text-sm font-semibold">Build a tool policy</p>
          <p className="text-xs text-foreground-muted">Pick what agents may do — no YAML. The cluster compiles and enforces it.</p>
        </div>
        <button type="button" onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">Close</button>
      </div>

      {/* Identity + scope */}
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="text-xs text-foreground-muted">
          Policy name
          <input value={name} onChange={(e) => setName(e.target.value)} required placeholder="repo-agents"
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm" />
        </label>
        <label className="text-xs text-foreground-muted">
          Applies to tool (glob · <span className="font-mono">*</span> = all tools)
          <input value={toolGlob} onChange={(e) => setToolGlob(e.target.value)}
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm" />
        </label>
        <label className="text-xs text-foreground-muted">
          Sandbox selector — label key
          <input value={labelKey} onChange={(e) => setLabelKey(e.target.value)} placeholder="kars.azure.com/team"
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm" />
        </label>
        <label className="text-xs text-foreground-muted">
          Label value
          <input value={labelValue} onChange={(e) => setLabelValue(e.target.value)} placeholder="true"
            className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-sm" />
        </label>
      </div>

      {/* Capability presets */}
      <div>
        <p className="text-xs font-semibold">Capabilities</p>
        <p className="text-[11px] text-foreground-muted">Toggle what this policy allows or blocks. These become priority-ordered rules.</p>
        <ul className="mt-2 grid gap-2 sm:grid-cols-2">
          {POLICY_PRESETS.map((p) => {
            const on = presets.has(p.id);
            return (
              <li key={p.id}>
                <button
                  type="button"
                  onClick={() => togglePreset(p.id)}
                  className={`flex w-full items-start gap-2 rounded-lg border p-2.5 text-left ${on ? "border-signal/40 bg-signal/5" : "border-border hover:bg-surface-muted/50"}`}
                >
                  <span className={`mt-0.5 grid h-4 w-4 shrink-0 place-items-center rounded border text-[10px] ${on ? "border-signal bg-signal text-signal-fg" : "border-border"}`}>{on ? "✓" : ""}</span>
                  <span className="min-w-0">
                    <span className="flex items-center gap-1.5 text-xs font-medium">
                      {p.label}
                      <span className={`rounded-full px-1.5 text-[10px] ${p.effect === "deny" ? "bg-danger/10 text-danger" : "bg-ok/10 text-ok"}`}>{p.effect}</span>
                    </span>
                    <span className="mt-0.5 block text-[11px] text-foreground-muted">{p.description}</span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      </div>

      {/* Custom rules */}
      <div>
        <div className="flex items-center justify-between">
          <p className="text-xs font-semibold">Custom rules (advanced)</p>
          <button type="button" onClick={() => setCustom((c) => [...c, { id: RID++, name: "", effect: "allow", actions: "", priority: 60 }])}
            className="rounded-md border border-border px-2 py-1 text-[11px] hover:bg-surface-muted">+ Add rule</button>
        </div>
        {custom.length > 0 && (
          <ul className="mt-2 space-y-2">
            {custom.map((c) => (
              <li key={c.id} className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-surface-muted/40 p-2">
                <input value={c.name} onChange={(e) => setCustom((cs) => cs.map((x) => x.id === c.id ? { ...x, name: e.target.value } : x))} placeholder="rule name"
                  className="w-32 rounded border border-border bg-surface px-2 py-1 text-xs" />
                <select value={c.effect} onChange={(e) => setCustom((cs) => cs.map((x) => x.id === c.id ? { ...x, effect: e.target.value as "allow" | "deny" } : x))}
                  className="rounded border border-border bg-surface px-2 py-1 text-xs">
                  <option value="allow">allow</option><option value="deny">deny</option>
                </select>
                <input value={c.actions} onChange={(e) => setCustom((cs) => cs.map((x) => x.id === c.id ? { ...x, actions: e.target.value } : x))} placeholder="action patterns, e.g. tool:github_* shell:make"
                  className="min-w-40 flex-1 rounded border border-border bg-surface px-2 py-1 font-mono text-xs" />
                <input type="number" value={c.priority} onChange={(e) => setCustom((cs) => cs.map((x) => x.id === c.id ? { ...x, priority: Number(e.target.value) } : x))}
                  className="w-16 rounded border border-border bg-surface px-2 py-1 text-xs" title="priority (higher first)" />
                <button type="button" onClick={() => setCustom((cs) => cs.filter((x) => x.id !== c.id))} className="text-xs text-foreground-muted hover:text-danger">✕</button>
              </li>
            ))}
          </ul>
        )}
      </div>

      {/* Approval gate */}
      <div className="flex flex-wrap items-center gap-3">
        <label className="text-xs text-foreground-muted">
          Human approval
          <select value={approvalMode} onChange={(e) => setApprovalMode(e.target.value as typeof approvalMode)}
            className="ml-2 rounded border border-border bg-surface px-2 py-1 text-xs">
            <option value="none">not required</option>
            <option value="always">always</option>
            <option value="aboveThreshold">above a spend threshold</option>
          </select>
        </label>
        {approvalMode === "aboveThreshold" && (
          <input value={approvalThreshold} onChange={(e) => setApprovalThreshold(e.target.value)} placeholder="USD 25.00"
            className="w-32 rounded border border-border bg-surface px-2 py-1 font-mono text-xs" />
        )}
      </div>

      {/* Live preview */}
      <div>
        <button type="button" onClick={() => setShowYaml((v) => !v)} className="inline-flex items-center gap-1 text-[11px] font-medium text-foreground-muted hover:text-foreground">
          <span aria-hidden className={`inline-block transition-transform ${showYaml ? "rotate-180" : ""}`}>⌄</span>
          {showYaml ? "Hide" : "Show"} generated policy
        </button>
        {showYaml && (
          <pre className="mt-2 max-h-64 overflow-auto rounded-lg border border-border bg-surface-muted/40 p-3 font-mono text-[10px] leading-relaxed">{yaml}</pre>
        )}
      </div>

      <div className="flex items-center gap-3">
        <button type="submit" disabled={pending || !name.trim()} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Creating…" : "Create policy"}
        </button>
        {state.error && <p className="text-xs text-danger">{state.error}</p>}
      </div>
    </form>
  );
}
