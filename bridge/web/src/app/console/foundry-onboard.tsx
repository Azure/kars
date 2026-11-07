// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// One project URL derives inference and discovers deployments and connections.
// Authentication reuses a same-resource saved key or the cluster's Azure identity.

import { useActionState, useState } from "react";
import { Icon, type IconName } from "@/components/icon";
import { connectFoundryAction, verifyFoundryAction, disconnectFoundryAction, type FoundryState } from "./foundry-actions";
import { OrchestrationCube, type OrchestrationPhase } from "@/components/orchestration-cube";
import type { FoundryConnection, FoundryStatus } from "@/lib/bff";

const init: FoundryState = { error: null, ok: null };

const DISCOVERY_PHASES: OrchestrationPhase[] = [
  { icon: "globe", label: "Resolving the project endpoint", detail: "DNS + reachability" },
  { icon: "shield", label: "Checking access to the project", detail: "saved resource credential or Azure identity" },
  { icon: "brain", label: "Discovering model deployments", detail: "the models this project serves" },
  { icon: "plug", label: "Discovering connected services", detail: "grounding, search, storage…" },
];

export function FoundryOnboard({ status }: { status: FoundryStatus }) {
  const [connectState, connectAction, connectPending] = useActionState(connectFoundryAction, init);
  const [verifyState, verifyAction, verifyPending] = useActionState(verifyFoundryAction, init);
  const [disState, disAction, disPending] = useActionState(disconnectFoundryAction, init);
  const [useKey, setUseKey] = useState(status.auth === "api");
  const [advanced, setAdvanced] = useState(false);
  const [editing, setEditing] = useState(!status.connected);

  const [latestAction, setLatestAction] = useState<"connect" | "verify" | "disconnect" | null>(null);
  const result = latestAction === "connect" ? connectState : latestAction === "verify" ? verifyState : latestAction === "disconnect" ? disState : null;
  const discovering = connectPending || verifyPending;

  return (
    <div className="space-y-3">
      {discovering && (
        <OrchestrationCube title="Connecting & discovering your Foundry project" phases={DISCOVERY_PHASES} active={0} done={false} />
      )}

      {status.connected && (!editing || (latestAction === "connect" && connectState.checks)) && !discovering ? (
        <div className="rounded-lg border border-signal/30 bg-signal/5 p-3 text-sm kb-rise">
          <div className="flex items-center justify-between gap-2">
            <span className="inline-flex items-center gap-1.5 font-medium text-signal">✓ Connected</span>
            <span className="rounded-full bg-surface px-2 py-0.5 text-[11px] font-medium text-foreground-muted">
              {status.auth === "api" ? "API key" : "Azure identity (auto)"}
            </span>
          </div>
          <p className="mt-1.5 break-all font-mono text-[11px] text-foreground-muted">{status.project_endpoint}</p>
          {status.inference_endpoint && <p className="mt-1 break-all text-[11px] text-foreground-muted">Inference: <span className="font-mono">{status.inference_endpoint}</span></p>}
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <form action={verifyAction} onSubmit={() => { setEditing(false); setLatestAction("verify"); }}>
              <button type="submit" disabled={verifyPending} className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold text-signal disabled:opacity-50">
                {verifyPending ? "Refreshing…" : "Refresh models & services"}
              </button>
            </form>
            <button type="button" onClick={() => { setLatestAction(null); setEditing(true); }} className="text-[11px] text-foreground-muted hover:text-foreground">Edit</button>
            <form action={disAction} onSubmit={() => setLatestAction("disconnect")}>
              <button type="submit" disabled={disPending} className="text-[11px] text-foreground-muted hover:text-danger disabled:opacity-50">Disconnect</button>
            </form>
          </div>
        </div>
      ) : !discovering ? (
        <form action={connectAction} onSubmit={() => setLatestAction("connect")} className="space-y-2.5 rounded-lg border border-border bg-surface-muted/30 p-3">
          <label className="block text-sm">
            <span className="font-medium">Foundry project URL</span>
            <input name="project_endpoint" defaultValue={status.project_endpoint ?? ""} required autoFocus placeholder="https://<resource>.services.ai.azure.com/api/projects/<project>" className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-sm" />
            <span className="mt-1 block text-[11px] text-foreground-muted">We derive the inference endpoint from this URL, reuse a matching saved resource key or your cluster&rsquo;s Azure identity, and discover deployed models and connected services. Your default model stays unchanged.</span>
          </label>

          <input type="hidden" name="auth" value={useKey ? "api" : "auto"} />

          <button type="button" onClick={() => setAdvanced((v) => !v)} className="inline-flex items-center gap-1 text-[11px] font-medium text-foreground-muted hover:text-foreground">
            <span aria-hidden className={`inline-block transition-transform ${advanced ? "rotate-180" : ""}`}>⌄</span>
            Advanced (authentication)
          </button>
          {advanced && (
            <div className="space-y-2 rounded-md border border-border bg-surface/50 p-2.5">
              <label className="flex items-center gap-2 text-xs">
                <input type="checkbox" checked={useKey} onChange={(e) => setUseKey(e.target.checked)} className="h-3.5 w-3.5 accent-[var(--signal)]" />
                Supply or reuse an API key
              </label>
              {useKey && (
                <input name="api_key" type="password" placeholder="Leave blank to reuse a matching saved key" className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm" />
              )}
              <p className="text-[11px] text-foreground-muted">
                Memory is <span className="font-medium">per-team</span> — each team&rsquo;s knowledge-commons maps to its own
                Foundry memory store, chosen when you set up the team.
              </p>
            </div>
          )}

          <p className="text-[11px] text-foreground-muted">
            Connecting saves this project for model and service discovery in the governed provider store; it does not change controller settings, default models, or existing team memory. Services listed here are not automatically enabled. Without a matching saved key, discovery uses Azure identity (workload identity on AKS). For dev discovery via your existing
            Azure CLI login, set <span className="font-mono">KARS_FOUNDRY_ALLOW_AZ_CLI=1</span> on the Bridge — it only
            reads an existing <span className="font-mono">az login</span>, never runs one.
          </p>

          <div className="flex items-center gap-2">
            <button type="submit" disabled={connectPending} className="rounded-md bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
              {status.connected ? "Reconnect & discover" : "Connect & discover"}
            </button>
            {status.connected && <button type="button" onClick={() => setEditing(false)} className="text-[11px] text-foreground-muted hover:text-foreground">Cancel</button>}
          </div>
        </form>
      ) : null}

      {status.connected && !result && !discovering && status.connections != null && (
        <FoundryServices connections={status.connections} confirmed savedAt={status.services_discovered_at ?? "unknown"} />
      )}
      {result?.checks && !discovering && (
        <div className="space-y-3 kb-rise">
          <ul className="space-y-1.5 kb-stagger">
            {result.checks.map((c) => (
              <li key={c.label} className="flex items-start gap-2 rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs">
                <span className={c.status === "pass" ? "text-signal" : c.status === "warn" ? "text-warning" : "text-danger"}>
                  {c.status === "pass" ? "✓" : c.status === "warn" ? "!" : "✗"}
                </span>
                <span><span className="font-medium">{c.label}</span><span className="ml-1 text-foreground-muted">{c.detail}</span></span>
              </li>
            ))}
          </ul>
          {result.discovered && (
            <div className="space-y-3">
              {/* Models — verbose: each id + the note that it's now catalogued. */}
              <div className="rounded-lg border border-border bg-surface-muted/30 p-3">
                <p className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
                  <Icon name="brain" size={12} /> Model deployments ({result.discovered.models.length})
                </p>
                {result.discovered.models_discovered !== true ? (
                  <p className="mt-1.5 text-[11px] text-warning">Deployment discovery was not confirmed. Saved models were retained; refresh to retry.</p>
                ) : result.discovered.models.length === 0 ? (
                  <p className="mt-1.5 text-[11px] text-foreground-muted">No model deployments in this project yet. Deploy one in Azure AI Foundry, then refresh.</p>
                ) : (
                  <>
                    <ul className="mt-1.5 space-y-1">
                      {result.discovered.models.map((m) => (
                        <li key={m} className="flex items-center gap-2 text-xs">
                          <Icon name="check" size={11} className="text-signal" />
                          <span className="font-mono">{m}</span>
                          {result.discovered?.models_catalogued === true && <span className="rounded bg-surface px-1.5 py-0.5 text-[10px] text-foreground-muted">→ catalogue · tagged foundry</span>}
                        </li>
                      ))}
                    </ul>
                    <p className="mt-2 text-[11px] text-foreground-muted">{result.discovered.models_catalogued === true ? "The catalogue is refreshed. Choose Set as default in the Model catalogue if you want to change the default; existing explicit routes are unchanged." : "These deployments were discovered, but the catalogue update was not confirmed. Refresh to retry."}</p>
                  </>
                )}
              </div>

              <FoundryServices connections={result.discovered.connections} confirmed={result.discovered.connections_discovered === true} />

              {/* Memory store — surfaced when the project reported one. */}
              {result.discovered.memory_store_found != null && (
                <div className="rounded-lg border border-border bg-surface-muted/30 p-3 text-xs">
                  <p className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
                    <Icon name="database" size={12} /> Agent memory
                  </p>
                  <p className="mt-1.5 text-foreground-muted">
                    {result.discovered.memory_store_found
                      ? "Configured memory store found — team knowledge-commons can bind to it."
                      : "Configured memory store not found in this project. Memory stores are per-team; set one when you create a team."}
                  </p>
                </div>
              )}
            </div>
          )}
        </div>
      )}
      {result?.ok && !discovering && <p role="status" className="text-[11px] text-signal">{result.ok}</p>}
      {result?.error && !discovering && <p role="alert" className="text-[11px] text-danger">{result.error}</p>}
    </div>
  );
}

function FoundryServices({ connections, confirmed, savedAt }: { connections: FoundryConnection[]; confirmed: boolean; savedAt?: string }) {
  return (
    <div className="rounded-lg border border-border bg-surface-muted/30 p-3">
      <p className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
        <Icon name="plug" size={12} /> Connected services ({connections.length})
      </p>
      <p className="mt-1.5 text-[11px] text-foreground-muted">Configured project connections, not evidence that Kars has invoked or validated each service.</p>
      {savedAt && <p className="mt-1.5 text-[11px] text-foreground-muted">Saved discovery · {savedAt}. Refresh to check for changes.</p>}
      {!confirmed ? (
        <p className="mt-1.5 text-[11px] text-warning">Service discovery was not confirmed. Refresh to retry.</p>
      ) : connections.length === 0 ? (
        <p className="mt-1.5 text-[11px] text-foreground-muted">No services (grounding, search, storage, …) are connected to this project.</p>
      ) : (
        <ul className="mt-1.5 space-y-1.5">
          {connections.map((connection) => {
            const service = foundryServiceLabel(connection.category);
            return (
              <li key={connection.name} className="flex items-start gap-2 text-xs">
                <Icon name={service.icon} size={12} className="mt-0.5 text-signal" />
                <span className="min-w-0">
                  <span className="font-medium">{connection.name}</span>
                  <span className="ml-1.5 rounded bg-surface px-1.5 py-0.5 text-[10px] text-foreground-muted">{service.label}</span>
                  {service.enables && <span className="block text-[11px] text-foreground-muted">{service.enables}</span>}
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** Human-friendly label + icon + capability blurb for a raw Foundry
 *  connection category, so discovery says what each service actually IS
 *  rather than dumping an opaque type string. */
function foundryServiceLabel(category: string | null): { label: string; icon: IconName; enables: string | null } {
  const c = (category ?? "").toLowerCase();
  if (c.includes("bing") || c.includes("grounding")) return { label: "Bing grounding", icon: "globe", enables: "Grounded web search for agents." };
  if (c.includes("search")) return { label: "Azure AI Search", icon: "search", enables: "Vector / hybrid retrieval over your indexes." };
  if (c.includes("storage") || c.includes("blob")) return { label: "Azure Storage", icon: "database", enables: "Blob storage for files and artifacts." };
  if (c.includes("openai") || c.includes("aoai")) return { label: "Azure OpenAI", icon: "brain", enables: "Model inference endpoint." };
  if (c.includes("cognitiveservices") || c.includes("aiservices")) return { label: "Azure AI Services", icon: "brain", enables: "Speech, vision, language, and more." };
  if (c.includes("appinsights") || c.includes("monitor")) return { label: "Application Insights", icon: "chart", enables: "Telemetry and tracing." };
  if (c.includes("keyvault")) return { label: "Key Vault", icon: "lock", enables: "Secrets and keys." };
  return { label: category ?? "connection", icon: "link", enables: null };
}
