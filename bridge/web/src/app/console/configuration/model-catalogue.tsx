"use client";

// Model catalogue — every model across all connected providers, tagged with
// the provider that serves it. This is ALSO where local (in-cluster) models
// are managed: deploy a new one, remove one, and set any model as the cluster
// default. Managed-provider models (Copilot / Foundry / Azure) are read-only
// here — you connect/remove those as providers above.

import { useActionState, useState } from "react";
import { Icon } from "@/components/icon";
import { Badge, Section } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { LocalModelDeploy } from "./local-model-deploy";
import { setDefaultModelAction, type SetDefaultModelState } from "./set-default-model-actions";
import { undeployLocalModelAction, type LocalInferenceState } from "./local-inference-actions";
import type { ModelOption } from "@/lib/types";
import type {
  LocalInferenceStatus,
  CuratedLocalModel,
  LocalModelDeployment,
} from "@/lib/bff";

function isLocalProvider(provider: string) {
  return provider === "airunway"
    || provider === "local-inference"
    || provider.startsWith("local-");
}

function ModelRow({
  m,
  localDeployments,
}: {
  m: ModelOption;
  localDeployments: LocalModelDeployment[];
}) {
  const local = isLocalProvider(m.provider);
  const managedDeployment = localDeployments.find((deployment) => {
    if (!deployment.managed) return false;
    const modelName = deployment.model_id?.split("/").at(-1);
    return deployment.name === m.deployment
      || modelName === m.deployment
      || m.provider === `local-${deployment.name}`;
  });
  const [defState, setDefault, defPending] = useActionState(setDefaultModelAction, { error: null, ok: null } as SetDefaultModelState);
  const [rmState, remove, rmPending] = useActionState(undeployLocalModelAction, { error: null, ok: null } as LocalInferenceState);
  return (
    <li className="flex flex-col gap-1 rounded-lg border border-border bg-surface px-3 py-2.5 text-sm">
      <div className="flex items-center justify-between gap-3">
        <span className="flex min-w-0 flex-col gap-0.5">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate font-mono text-xs">{m.deployment}</span>
            <Badge tone="muted">{local ? "AI Runway" : m.provider}</Badge>
            {m.is_default && <Badge tone="info">default</Badge>}
          </span>
          {m.detail && <span className="truncate text-[11px] text-foreground-muted">{m.detail}</span>}
        </span>
        <span className="flex shrink-0 items-center gap-1.5">
          {!m.is_default && (
            <form action={setDefault}>
              <input type="hidden" name="deployment" value={m.deployment} />
              <input type="hidden" name="provider" value={m.provider} />
              <button type="submit" disabled={defPending} className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-signal/40 hover:text-signal disabled:opacity-50">
                {defPending ? "Setting…" : "Set as default"}
              </button>
            </form>
          )}
          {managedDeployment && (
            <form action={remove}>
              <input type="hidden" name="name" value={managedDeployment.name} />
              <button type="submit" disabled={rmPending} className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger disabled:opacity-50">
                {rmPending ? "Removing…" : "Remove"}
              </button>
            </form>
          )}
        </span>
      </div>
      {defState.error && <p className="text-[11px] text-danger">{defState.error}</p>}
      {rmState.error && <p className="text-[11px] text-danger">{rmState.error}</p>}
    </li>
  );
}

export function ModelCatalogue({
  models,
  localStatus,
  localCatalog,
  localDeployments,
}: {
  models: ModelOption[];
  localStatus: LocalInferenceStatus | null;
  localCatalog: CuratedLocalModel[];
  localDeployments: LocalModelDeployment[];
}) {
  const [deploying, setDeploying] = useState(false);
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const filtered = q
    ? models.filter((m) =>
        m.deployment.toLowerCase().includes(q) ||
        m.provider.toLowerCase().includes(q) ||
        (m.provider.startsWith("local-") && "ai runway in-cluster".includes(q)) ||
        (m.detail ?? "").toLowerCase().includes(q),
      )
    : models;
  return (
    <Section
      title="Model catalogue"
      subtitle="Every model across all connected providers, tagged with the provider that serves it — what missions reason with and what the orchestrator may propose. Set any model as the cluster default right here; local (in-cluster) models are also added and removed here."
    >
      <div className="mb-3 flex flex-wrap items-center gap-2">
        {models.length > 0 && (
          <div className="relative flex-1 min-w-[200px]">
            <span className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-foreground-muted">
              <Icon name="search" size={13} />
            </span>
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search models — id, provider, vendor…"
              className="w-full rounded-lg border border-border bg-surface py-1.5 pl-8 pr-3 text-xs"
            />
          </div>
        )}
        {localStatus?.available && !deploying && (
          <button type="button" onClick={() => setDeploying(true)} className="inline-flex items-center gap-1.5 rounded-lg border border-signal/40 bg-signal/10 px-3 py-1.5 text-xs font-medium text-signal hover:bg-signal/15">
            <Icon name="box" size={13} /> Deploy a local model
          </button>
        )}
      </div>
      {localStatus?.available && deploying && (
        <div className="mb-4">
          <LocalModelDeploy status={localStatus} catalog={localCatalog} onClose={() => setDeploying(false)} />
        </div>
      )}
      {models.length === 0 ? (
        <HonestState
          variant="empty"
          compact
          title="No models configured"
          detail="Connect a provider above and its models appear here — or deploy a local one."
        />
      ) : filtered.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border bg-surface-muted/30 px-3 py-4 text-center text-xs text-foreground-muted">No models match &ldquo;{query}&rdquo;.</p>
      ) : (
        <ul className="grid gap-2 sm:grid-cols-2">
          {filtered.map((m) => (
            <ModelRow
              key={`${m.provider}::${m.deployment}`}
              m={m}
              localDeployments={localDeployments}
            />
          ))}
        </ul>
      )}
    </Section>
  );
}
