// kars Bridge Operator Console — Configuration. The hub for the CLUSTER this
// runs on: the inference provider, Azure AI Foundry connection, the models it
// serves, cluster add-ons (SRE agent, Headlamp), and the platform GitHub App.
// Agent-facing building blocks (skills, team profiles, MCP, credentials) live
// on their own page — Console → Agent capabilities. Reads are live cluster
// facts; writes are operator-gated.

import { getOptions, getFoundry, getIntegrations, getGithubApp, listAdditionalProviders, getLocalInferenceStatus, getLocalInferenceCatalog, listLocalModelDeployments } from "@/lib/bff";
import { PageHeader, Section, Badge } from "@/components/ui";
import { ProviderWizard } from "./provider-wizard";
import { ModelCatalogue } from "./model-catalogue";
import { OperatorGithubStatus } from "../operator-github-status";
import { Icon } from "@/components/icon";
import type { Options } from "@/lib/types";

export const dynamic = "force-dynamic";

export default async function ConfigurationPage() {
  const options: Options | null = await getOptions().catch(() => null);
  const foundry = await getFoundry().catch(() => null);
  const integrations = await getIntegrations().catch(() => null);
  const githubApp = await getGithubApp().catch(() => ({ configured: false, slug: null, install_url: null }));
  const localInferenceStatus = await getLocalInferenceStatus().catch(() => null);
  const localInferenceCatalog = await getLocalInferenceCatalog().catch(() => []);
  // Discover every AI Runway ModelDeployment cluster-wide. Bridge-managed
  // deployments are also auto-wired by this call; externally managed ones are
  // read-only discoveries and still appear in the provider summary.
  const localDeployments = await listLocalModelDeployments().catch(() => []);
  const additionalProviders = await listAdditionalProviders().catch(() => []);

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Configuration"
        lead="What this cluster runs on — provider, Foundry, models — set once at setup, inherited by every team. Agent-facing capabilities (skills, team profiles, MCP, credentials) live under Console → Agent capabilities."
      />

      {/* Inference provider — the inherited fact that drives every mission and
          every composed envelope. ONE wizard + ONE unified list: the cluster
          default and every additional provider render as uniform rows (the
          default just carries a badge), so nothing "looks bigger" than the
          rest. See provider-wizard.tsx for why default vs additional are still
          distinct resources under the hood. */}
      <Section
        title="Inference provider"
        subtitle="The service(s) this cluster authenticates to for model calls. One default every mission inherits, plus any additional providers an InferencePolicy can route specific sandboxes to."
      >
        <ProviderWizard
          hasDefaultProvider={!!options?.provider}
          defaultProvider={
            options?.provider
              ? {
                  id: options.provider.id,
                  label: options.provider.label,
                  note: options.provider.note,
                  models: (options.models ?? []).filter((m) => m.provider === options.provider!.id).map((m) => m.deployment),
                }
              : null
          }
          additionalProviders={additionalProviders}
          localInferenceStatus={localInferenceStatus}
          localDeployments={localDeployments}
          foundryStatus={foundry ?? { connected: false, project_endpoint: null, inference_endpoint: null, memory_store_id: null, auth: null, has_api_key: false }}
        />
      </Section>

      <ModelCatalogue
        models={options?.models ?? []}
        localStatus={localInferenceStatus}
        localCatalog={localInferenceCatalog}
        localDeployments={localDeployments}
      />

      {/* Cluster integrations — the real kars add-ons (SRE agent + Headlamp
          plugin), with live status and either deep-links or activation. */}
      <Section
        title="Integrations"
        subtitle="kars add-ons for this cluster — the SRE agent and the Headlamp dashboard plugin. Activate or open them here."
      >
        <div className="grid gap-3 sm:grid-cols-2">
          {/* kars-SRE agent */}
          <div className="rounded-xl border border-border bg-surface p-4">
            <div className="flex items-center justify-between">
              <p className="flex items-center gap-1.5 text-sm font-semibold"><Icon name="wrench" size={14} /> kars-SRE agent</p>
              <Badge tone={integrations?.sre_present ? "ok" : "muted"} dot={!!integrations?.sre_present}>
                {integrations?.sre_present ? (integrations.sre_phase ?? "present") : "not enabled"}
              </Badge>
            </div>
            {integrations?.sre_present ? (
              <p className="mt-2 text-xs text-foreground-muted">
                The SRE sandbox is running{integrations.sre_ready ? ` (${integrations.sre_ready} ready)` : ""} — it triages cluster health and proposes operator-approved fixes (KarsSREAction).
                {integrations.headlamp_url && (
                  <> Open its console: <a className="text-signal hover:underline" href={`${integrations.headlamp_url}/kars/sre`} target="_blank" rel="noreferrer">Headlamp → /kars/sre ↗</a></>
                )}
              </p>
            ) : (
              <div className="mt-2">
                <p className="text-xs text-foreground-muted">Not enabled on this cluster. Activate the SRE agent (Hermes runtime, scoped apiserver access, read-only diagnostics + approved apply-fix):</p>
                <code className="mt-2 block overflow-x-auto rounded-lg border border-border bg-surface-muted/40 px-3 py-2 font-mono text-[11px]">{integrations?.sre_activate_cmd ?? "kars sre install"}</code>
              </div>
            )}
          </div>

          {/* Headlamp plugin */}
          <div className="rounded-xl border border-border bg-surface p-4">
            <div className="flex items-center justify-between">
              <p className="flex items-center gap-1.5 text-sm font-semibold"><Icon name="compass" size={14} /> Headlamp plugin</p>
              <Badge tone={integrations?.headlamp_deployed ? (integrations.headlamp_url ? "ok" : "warn") : "muted"} dot={!!integrations?.headlamp_deployed}>
                {integrations?.headlamp_deployed ? (integrations.headlamp_url ? "linked" : "deployed") : "not found"}
              </Badge>
            </div>
            {integrations?.headlamp_url ? (
              <div className="mt-2">
                <p className="text-xs text-foreground-muted">The kars Headlamp plugin — deep dashboard views for kars resources:</p>
                <div className="mt-2 flex flex-wrap gap-2">
                  {integrations.headlamp_paths.map((l) => (
                    <a key={l.path} href={`${integrations.headlamp_url}${l.path}`} target="_blank" rel="noreferrer" className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-medium text-signal hover:bg-signal/15">
                      {l.label} ↗
                    </a>
                  ))}
                </div>
              </div>
            ) : integrations?.headlamp_deployed ? (
              <p className="mt-2 text-xs text-foreground-muted">
                Headlamp is deployed but not linked here. Install the kars plugin and set <code className="font-mono">BRIDGE_HEADLAMP_URL</code> to deep-link its <code className="font-mono">/kars/*</code> views. <span className="text-foreground-muted">{integrations.headlamp_install_hint}</span>
              </p>
            ) : (
              <p className="mt-2 text-xs text-foreground-muted">No Headlamp deployment detected. Install Headlamp + the kars plugin (tools/headlamp-plugin), then set <code className="font-mono">BRIDGE_HEADLAMP_URL</code>.</p>
            )}
          </div>
        </div>
      </Section>

      {/* GitHub App — a platform-level integration like SRE/Headlamp above,
          not part of the provider→Foundry→models inference chain. Kept here,
          grouped with the other cluster add-ons, instead of wedged between
          Inference provider and Azure AI Foundry. */}
      <Section
        title="GitHub App (platform identity)"
        subtitle="Configure the one shared kars GitHub App that lets workspaces open pull requests. Operators set this up once; users then connect their own repos from their Workspace → Connections."
      >
        <OperatorGithubStatus configured={githubApp.configured} slug={githubApp.slug} />
      </Section>
    </div>
  );
}
