// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — Configuration. The hub for the CLUSTER this
// runs on: the inference provider, Azure AI Foundry connection, the models it
// serves, cluster add-ons (SRE agent, Headlamp), and the platform GitHub App.
// Agent-facing building blocks (skills, team profiles, MCP, credentials) live
// on their own page — Console → Agent capabilities. Reads are live cluster
// facts; writes are operator-gated.

import { getOptions, getFoundry, getIntegrations, getGithubApp, listAdditionalProviders, getLocalInferenceStatus, getLocalInferenceCatalog, listLocalModelDeployments } from "@/lib/bff";
import { PageHeader, Section, Badge } from "@/components/ui";
import { DefaultProviderCard, ProviderWizard } from "./provider-wizard";
import { ModelCatalogue } from "./model-catalogue";
import { ComponentModels } from "./component-models";
import { getComponentModels } from "@/lib/bff";
import { OperatorGithubStatus } from "../operator-github-status";
import { Icon } from "@/components/icon";
import { ConsoleLoadError } from "@/components/console-load-error";
import { headlampLink } from "@/lib/headlamp";
import { headlampSsoEnabled } from "@/lib/config";

export const dynamic = "force-dynamic";

export default async function ConfigurationPage({ searchParams }: {
  searchParams?: Promise<Record<string, string | string[] | undefined>>;
} = {}) {
  const githubResult = (await searchParams)?.githubApp;
  const githubMessages: Record<string, string> = {
    connected: "GitHub registration returned. Check the current App status below, then connect your repositories from Workspace → Connections.",
    unauthorized: "Administrator access is required to finish GitHub App registration.",
    "session-expired": "Sign in again before registering an App. If GitHub already created it, use its App ID and a newly generated PEM file instead.",
    "invalid-flow": "The GitHub registration link is invalid, expired, or belongs to another browser session. Start again here. If the App already exists, use its App ID and a newly generated PEM file instead.",
    cancelled: "GitHub registration did not return a conversion code. No credential was saved by this callback. You can start again or upload an existing App’s PEM file.",
    "configuration-changed": "App storage or configuration changed during registration. This callback did not replace newer state. Check the current App status below; if GitHub created an App that is not configured here, use its App ID and a newly generated PEM file.",
    "conversion-failed": "Kars could not complete GitHub registration. Do not reuse the callback link: its code may already be consumed. If GitHub created the App, generate a new PEM in its settings and use the existing-App upload below. Check storage readiness before retrying. If another attempt succeeded, the current App status below will show it.",
  };
  const githubMessage = typeof githubResult === "string" && Object.hasOwn(githubMessages, githubResult)
    ? githubMessages[githubResult] : null;
  const [optionsRead, foundryRead, integrationsRead, githubAppRead, localStatusRead,
    localCatalogRead, localDeploymentsRead, additionalProvidersRead, componentModelsRead] = await Promise.allSettled([
      getOptions(), getFoundry(), getIntegrations(), getGithubApp(),
      getLocalInferenceStatus(), getLocalInferenceCatalog(),
      listLocalModelDeployments(), listAdditionalProviders(), getComponentModels(),
  ]);
  const options = optionsRead.status === "fulfilled" ? optionsRead.value : null;
  const catalogueOptions = options ?? await getOptions({ catalogueOnly: true }).catch(() => null);
  const integrations = integrationsRead.status === "fulfilled" ? integrationsRead.value : null;
  const defaultProvider = options?.provider
    ? {
        ...options.provider,
        models: options.models.filter((model) => model.provider === options.provider?.id)
          .map((model) => model.deployment),
      }
    : null;
  const providerReads = [
    ["default provider", optionsRead],
    ["Foundry connection", foundryRead],
    ["local inference", localStatusRead],
    ["local deployments", localDeploymentsRead],
    ["additional providers", additionalProvidersRead],
  ] as const;
  const modelReads = [
    ["model catalogue", optionsRead],
    ["local inference", localStatusRead],
    ["local model catalogue", localCatalogRead],
    ["local deployments", localDeploymentsRead],
  ] as const;
  const headlampUrl = integrations?.headlamp_url;
  const sreContainers = integrations?.sre_ready?.match(/^([1-9]\d*)\/([1-9]\d*)$/);
  const sreReportsReady = integrations?.sre_present
    && (integrations.sre_phase === "Running" || integrations.sre_phase === "Ready")
    && !!sreContainers && sreContainers[1] === sreContainers[2];

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
        {optionsRead.status === "fulfilled" && foundryRead.status === "fulfilled"
          && localStatusRead.status === "fulfilled" && localDeploymentsRead.status === "fulfilled"
          && additionalProvidersRead.status === "fulfilled" ? (
            <ProviderWizard
              hasDefaultProvider={!!defaultProvider}
              defaultProvider={defaultProvider}
              additionalProviders={additionalProvidersRead.value}
              localInferenceStatus={localStatusRead.value}
              localDeployments={localDeploymentsRead.value}
              foundryStatus={foundryRead.value}
            />
          ) : (
            <div className="space-y-3">
              {defaultProvider && <ul><DefaultProviderCard p={defaultProvider} /></ul>}
              {providerReads.map(([title, read]) => read.status === "rejected"
                ? <ConsoleLoadError key={title} inline title={title} error={read.reason} />
                : null)}
            </div>
          )}
      </Section>

      {catalogueOptions && localStatusRead.status === "fulfilled"
        && localCatalogRead.status === "fulfilled" && localDeploymentsRead.status === "fulfilled" ? (
          <ModelCatalogue
            models={catalogueOptions.models}
            localStatus={localStatusRead.value}
            localCatalog={localCatalogRead.value}
            localDeployments={localDeploymentsRead.value}
          />
        ) : (
          <Section title="Model catalogue">
            {modelReads.map(([title, read]) => read.status === "rejected"
              ? <ConsoleLoadError key={title} inline title={title} error={read.reason} />
              : null)}
          </Section>
        )}

      <Section title="Component models" subtitle="Orchestrator and SRE can inherit the saved cluster default or use independent overrides. Changes reconcile automatically; Applied requires a router acknowledgement. Existing reviewed tasks retain their routes.">
        {componentModelsRead.status === "fulfilled" && catalogueOptions ? (
          <ComponentModels components={componentModelsRead.value.components} clusterDefault={componentModelsRead.value.cluster_default} models={catalogueOptions.models} />
        ) : <ConsoleLoadError inline title="Component models" error={componentModelsRead.status === "rejected" ? componentModelsRead.reason : new Error("Model catalogue unavailable")} />}
      </Section>

      {/* Cluster integrations — the real kars add-ons (SRE agent + Headlamp
          plugin), with live status and either deep-links or activation. */}
      <Section
        title="Integrations"
        subtitle="kars add-ons for this cluster — the SRE agent and the Headlamp dashboard plugin. Activate or open them here."
      >
        {integrationsRead.status === "rejected" ? (
          <ConsoleLoadError inline title="Integrations" error={integrationsRead.reason} />
        ) : (
        <div className="grid gap-3 sm:grid-cols-2">
          {/* kars-SRE agent */}
          <div className="rounded-xl border border-border bg-surface p-4">
            <div className="flex items-center justify-between">
              <p className="flex items-center gap-1.5 text-sm font-semibold"><Icon name="wrench" size={14} /> kars-SRE agent</p>
              <Badge tone={sreReportsReady ? "ok" : integrations?.sre_present ? "warn" : "muted"} dot={!!integrations?.sre_present}>
                {integrations?.sre_present ? (integrations.sre_phase ?? "present") : "not enabled"}
              </Badge>
            </div>
            {integrations?.sre_present ? (
              <p className="mt-2 text-xs text-foreground-muted">
                {sreReportsReady ? "The SRE sandbox reports ready" : "The SRE sandbox is present but not ready"}
                {integrations.sre_ready ? ` (${integrations.sre_ready} containers ready)` : " (container readiness unavailable)"}.
                {" "}Readiness alone does not verify a completed diagnosis; review diagnostic evidence and remediation proposals in its console.
                {headlampUrl && (
                  <> Open its console: <a className="text-signal hover:underline" href={headlampLink(headlampUrl, "/kars/sre", headlampSsoEnabled())} target="_blank" rel="noreferrer">Headlamp → SRE console ↗</a></>
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
            {headlampUrl ? (
              <div className="mt-2">
                <p className="text-xs text-foreground-muted">The kars Headlamp plugin — deep dashboard views for kars resources:</p>
                <div className="mt-2 flex flex-wrap gap-2">
                  {integrationsRead.value.headlamp_paths.map((l) => (
                    <a key={l.path} href={headlampLink(headlampUrl, l.path, headlampSsoEnabled())} target="_blank" rel="noreferrer" className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-medium text-signal hover:bg-signal/15">
                      {l.label} ↗
                    </a>
                  ))}
                </div>
              </div>
            ) : integrations?.headlamp_deployed ? (
              <p className="mt-2 text-xs text-foreground-muted">
                Headlamp is deployed but not linked here. Install the kars plugin and set <code className="font-mono">BRIDGE_HEADLAMP_URL</code> to deep-link its <code className="font-mono">/kars/*</code> views. <span className="text-foreground-muted">{integrations?.headlamp_install_hint}</span>
              </p>
            ) : (
              <p className="mt-2 text-xs text-foreground-muted">No Headlamp deployment detected. Install Headlamp + the kars plugin (tools/headlamp-plugin), then set <code className="font-mono">BRIDGE_HEADLAMP_URL</code>.</p>
            )}
          </div>
        </div>
        )}
      </Section>

      {/* GitHub App — a platform-level integration like SRE/Headlamp above,
          not part of the provider→Foundry→models inference chain. Kept here,
          grouped with the other cluster add-ons, instead of wedged between
          Inference provider and Azure AI Foundry. */}
      <Section
        title="GitHub App (platform identity)"
        subtitle="Configure the one shared kars GitHub App that lets workspaces open pull requests. Operators set this up once; users then connect their own repos from their Workspace → Connections."
      >
        {githubMessage && <p role="status" className="mb-3 max-w-2xl rounded-lg border border-border bg-surface p-3 text-xs text-foreground-muted">{githubMessage}</p>}
        {githubAppRead.status === "fulfilled"
          ? <OperatorGithubStatus configured={githubAppRead.value.configured} slug={githubAppRead.value.slug} storage_ready={githubAppRead.value.storage_ready} />
          : <ConsoleLoadError inline title="GitHub App" error={githubAppRead.reason} />}
      </Section>
    </div>
  );
}
