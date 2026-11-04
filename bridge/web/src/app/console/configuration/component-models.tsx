// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useActionState, useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import type { ComponentModel } from "@/lib/bff";
import { saveComponentModel } from "./component-model-actions";

function route(model: ComponentModel["desired"]) {
  return model ? `${model.provider}/${model.deployment}` : "No model selected";
}

export function ComponentModels({ components, clusterDefault, models }: {
  components: ComponentModel[];
  clusterDefault: ComponentModel["desired"];
  models: { provider: string; deployment: string }[];
}) {
  const router = useRouter();
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") router.refresh();
    }, 5000);
    return () => clearInterval(timer);
  }, [router]);
  return <div className="grid gap-3 sm:grid-cols-2">
    {(["orchestrator", "sre"] as const).map(component => {
      const policy = components.find(item => item.component === component);
      return policy ? <ComponentModelCard key={policy.uid} policy={policy} clusterDefault={clusterDefault} models={models} />
        : <p key={component} className="text-sm text-muted">{component === "sre" ? "SRE" : "Orchestrator"}: no installed inference policy.</p>;
    })}
  </div>;
}

function ComponentModelCard({ policy, clusterDefault, models }: {
  policy: ComponentModel; clusterDefault: ComponentModel["desired"]; models: { provider: string; deployment: string }[];
}) {
  const [draft, setDraft] = useState<{ selection: string; version: string } | null>(null);
  const [state, action, pending] = useActionState<{ error: string | null; ok: string | null }, FormData>(async (previous, form) => {
    const result = await saveComponentModel(previous, form);
    if (!result.error) setDraft(null);
    return result;
  }, { error: null, ok: null });
  const initial = policy.mode === "inherit" ? "inherit" : policy.primary ? `${policy.primary.provider}::${policy.primary.deployment}` : "";
  const selection = draft?.selection ?? initial;
  const known = models.some(model => `${model.provider}::${model.deployment}` === selection);
  const problem = policy.conditions?.find(condition => condition.type === "Ready" && condition.status !== "True")?.message;
  const title = policy.component === "sre" ? "SRE" : "Orchestrator";
  // React resets action forms even on handled errors; draft state owns this form.
  return <form action={action} onReset={event => event.preventDefault()} className="rounded-xl border border-border bg-surface p-4 space-y-3">
    <h3 className="font-semibold">{title} default model</h3>
    <input type="hidden" name="component" value={policy.component} />
    <input type="hidden" name="uid" value={policy.uid} />
    <input type="hidden" name="resource_version" value={draft?.version ?? policy.resource_version} />
    <label className="block text-sm">Model source
      <select name="selection" value={selection} onChange={event => setDraft({ selection: event.target.value, version: draft?.version ?? policy.resource_version })} disabled={pending || !policy.editable} aria-label={`${title} default model`} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2">
        <option value="inherit" disabled={!clusterDefault}>Inherit cluster default — {route(clusterDefault)}</option>
        {!known && selection !== "inherit" && <option value={selection} disabled>{selection ? `${selection.replace("::", "/")} (not in catalogue)` : "Choose an explicit model"}</option>}
        {models.map(model => <option key={`${model.provider}::${model.deployment}`} value={`${model.provider}::${model.deployment}`}>Override — {route(model)}</option>)}
      </select>
    </label>
    <div role="status" className="text-sm text-muted">
      <p>Desired: {route(policy.desired)}</p>
      <p>{policy.applied ? "Applied — router acknowledged" : "Pending — not yet confirmed"}: {route(policy.primary)}</p>
      {!policy.applied && problem && <p>{problem}</p>}
    </div>
    {!policy.editable && <p className="text-sm text-muted">Managed by another resource or signed bundle; edit that source.</p>}
    {state.error && <p role="alert" className="text-sm text-danger">{state.error}</p>}
    {state.ok && <p role="status" className="text-sm text-muted">{state.ok}</p>}
    <div className="flex flex-wrap gap-2">
      <button type="submit" disabled={pending || !policy.editable} className="btn-primary text-sm">{pending ? "Saving…" : `Save ${title} default`}</button>
      <button type="button" disabled={pending || !draft} onClick={() => setDraft(null)} className="btn-secondary text-sm">Discard unsaved selection</button>
    </div>
  </form>;
}
