"use client";

// Operator Foundry onboarding — URL-first + guided, animated discovery.
// Paste the Foundry project URL; the Bridge connects using your Azure identity
// (workload identity on AKS, your az login in dev) and immediately discovers the
// project's models, connected services, and memory store — shown live. An API
// key and extra endpoints are optional, tucked under Advanced.

import { useActionState, useState } from "react";
import { Icon, type IconName } from "@/components/icon";
import { connectFoundryAction, verifyFoundryAction, disconnectFoundryAction, type FoundryState } from "./foundry-actions";
import { OrchestrationCube, type OrchestrationPhase } from "@/components/orchestration-cube";
import type { FoundryStatus } from "@/lib/bff";

const init: FoundryState = { error: null, ok: null };

const DISCOVERY_PHASES: OrchestrationPhase[] = [
  { icon: "globe", label: "Resolving the project endpoint", detail: "DNS + reachability" },
  { icon: "shield", label: "Authenticating with your Azure identity", detail: "workload identity (AKS) or your az login (dev)" },
  { icon: "brain", label: "Discovering model deployments", detail: "the models this project serves" },
  { icon: "plug", label: "Discovering connected services", detail: "grounding, search, storage…" },
  { icon: "database", label: "Checking the memory store", detail: "for team knowledge-commons" },
];

export function FoundryOnboard({ status }: { status: FoundryStatus }) {
  const [connectState, connectAction, connectPending] = useActionState(connectFoundryAction, init);
  const [verifyState, verifyAction, verifyPending] = useActionState(verifyFoundryAction, init);
  const [disState, disAction, disPending] = useActionState(disconnectFoundryAction, init);
  const [useKey, setUseKey] = useState(status.auth === "api");
  const [advanced, setAdvanced] = useState(false);
  const [editing, setEditing] = useState(!status.connected);

  const result = connectState.checks ? connectState : verifyState.checks ? verifyState : null;
  const discovering = connectPending || verifyPending;

  return (
    <div className="space-y-3">
      {discovering && (
        <OrchestrationCube title="Connecting & discovering your Foundry project" phases={DISCOVERY_PHASES} active={0} done={false} />
      )}

      {status.connected && !editing && !discovering ? (
        <div className="rounded-lg border border-signal/30 bg-signal/5 p-3 text-sm kb-rise">
          <div className="flex items-center justify-between gap-2">
            <span className="inline-flex items-center gap-1.5 font-medium text-signal">✓ Connected</span>
            <span className="rounded-full bg-surface px-2 py-0.5 text-[11px] font-medium text-foreground-muted">
              {status.auth === "api" ? "API key" : "Azure identity (auto)"}
            </span>
          </div>
          <p className="mt-1.5 break-all font-mono text-[11px] text-foreground-muted">{status.project_endpoint}</p>
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <form action={verifyAction}>
              <button type="submit" disabled={verifyPending} className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold text-signal disabled:opacity-50">
                {verifyPending ? "Discovering…" : "Re-discover"}
              </button>
            </form>
            <button type="button" onClick={() => setEditing(true)} className="text-[11px] text-foreground-muted hover:text-foreground">Edit</button>
            <form action={disAction}>
              <button type="submit" disabled={disPending} className="text-[11px] text-foreground-muted hover:text-danger disabled:opacity-50">Disconnect</button>
            </form>
          </div>
          {disState.error && <p className="mt-1 text-[11px] text-danger">{disState.error}</p>}
        </div>
      ) : !discovering ? (
        <form action={connectAction} className="space-y-2.5 rounded-lg border border-border bg-surface-muted/30 p-3">
          <label className="block text-sm">
            <span className="font-medium">Foundry project URL</span>
            <input name="project_endpoint" defaultValue={status.project_endpoint ?? ""} required autoFocus placeholder="https://<resource>.services.ai.azure.com/api/projects/<project>" className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-sm" />
            <span className="mt-1 block text-[11px] text-foreground-muted">That&rsquo;s all we need. We&rsquo;ll authenticate with your Azure identity and discover the rest.</span>
          </label>

          <input type="hidden" name="auth" value={useKey ? "api" : "auto"} />

          <button type="button" onClick={() => setAdvanced((v) => !v)} className="inline-flex items-center gap-1 text-[11px] font-medium text-foreground-muted hover:text-foreground">
            <span aria-hidden className={`inline-block transition-transform ${advanced ? "rotate-180" : ""}`}>⌄</span>
            Advanced (API key, inference endpoint, memory store)
          </button>
          {advanced && (
            <div className="space-y-2 rounded-md border border-border bg-surface/50 p-2.5">
              <label className="flex items-center gap-2 text-xs">
                <input type="checkbox" checked={useKey} onChange={(e) => setUseKey(e.target.checked)} className="h-3.5 w-3.5 accent-[var(--signal)]" />
                Use an API key instead of my Azure identity (dev)
              </label>
              {useKey && (
                <input name="api_key" type="password" placeholder="Foundry project API key (stored write-only)" className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm" />
              )}
              <div className="grid gap-2 sm:grid-cols-2">
                <label className="block text-[11px] text-foreground-muted">
                  Inference endpoint (optional)
                  <input name="inference_endpoint" defaultValue={status.inference_endpoint ?? ""} placeholder="https://<res>.openai.azure.com/" className="mt-1 w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs" />
                </label>
              </div>
              <p className="text-[11px] text-foreground-muted">
                Memory is <span className="font-medium">per-team</span> — each team&rsquo;s knowledge-commons maps to its own
                Foundry memory store, chosen when you set up the team. There is no cluster-wide store.
              </p>
            </div>
          )}

          <p className="text-[11px] text-foreground-muted">
            Discovery uses your Azure identity: workload identity on AKS. For dev discovery via your existing
            Azure CLI login, set <span className="font-mono">KARS_FOUNDRY_ALLOW_AZ_CLI=1</span> on the Bridge — it only
            reads an existing <span className="font-mono">az login</span>, never runs one.
          </p>

          <div className="flex items-center gap-2">
            <button type="submit" disabled={connectPending} className="rounded-md bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
              {status.connected ? "Reconnect & discover" : "Connect & discover"}
            </button>
            {status.connected && <button type="button" onClick={() => setEditing(false)} className="text-[11px] text-foreground-muted hover:text-foreground">Cancel</button>}
            {connectState.error && <span className="text-[11px] text-danger">{connectState.error}</span>}
          </div>
        </form>
      ) : null}

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
                {result.discovered.models.length === 0 ? (
                  <p className="mt-1.5 text-[11px] text-foreground-muted">No model deployments in this project yet. Deploy one in Azure AI Foundry, then re-discover.</p>
                ) : (
                  <>
                    <ul className="mt-1.5 space-y-1">
                      {result.discovered.models.map((m) => (
                        <li key={m} className="flex items-center gap-2 text-xs">
                          <Icon name="check" size={11} className="text-signal" />
                          <span className="font-mono">{m}</span>
                          <span className="rounded bg-surface px-1.5 py-0.5 text-[10px] text-foreground-muted">→ catalogue · tagged foundry</span>
                        </li>
                      ))}
                    </ul>
                    <p className="mt-2 text-[11px] text-foreground-muted">These are now available to missions and teams, and to the orchestrator when it proposes a model.</p>
                  </>
                )}
              </div>

              {/* Services — verbose: name + friendly type + what it enables. */}
              <div className="rounded-lg border border-border bg-surface-muted/30 p-3">
                <p className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
                  <Icon name="plug" size={12} /> Connected services ({result.discovered.connections.length})
                </p>
                {result.discovered.connections.length === 0 ? (
                  <p className="mt-1.5 text-[11px] text-foreground-muted">No services (grounding, search, storage, …) are connected to this project.</p>
                ) : (
                  <ul className="mt-1.5 space-y-1.5">
                    {result.discovered.connections.map((c) => {
                      const svc = foundryServiceLabel(c.category);
                      return (
                        <li key={c.name} className="flex items-start gap-2 text-xs">
                          <Icon name={svc.icon} size={12} className="mt-0.5 text-signal" />
                          <span className="min-w-0">
                            <span className="font-medium">{c.name}</span>
                            <span className="ml-1.5 rounded bg-surface px-1.5 py-0.5 text-[10px] text-foreground-muted">{svc.label}</span>
                            {svc.enables && <span className="block text-[11px] text-foreground-muted">{svc.enables}</span>}
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                )}
              </div>

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
      {(connectState.ok || verifyState.ok) && !discovering && <p className="text-[11px] text-signal">{connectState.ok || verifyState.ok}</p>}
      {verifyState.error && <p className="text-[11px] text-danger">{verifyState.error}</p>}
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
