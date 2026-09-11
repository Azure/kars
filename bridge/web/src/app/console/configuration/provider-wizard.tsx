"use client";

// kars Bridge Operator Console — the ONE inference-provider wizard. Merges
// what used to be two separate flows on this page (the plain "Add or switch
// a provider" form for the cluster's single default, and a second
// "Additional providers" form for everything beyond it) into a single
// step-by-step experience: pick a provider kind, decide whether it's the
// cluster's default or an additional one, authenticate, then pick/discover
// its models and review before connecting.
//
// The two are still genuinely different resources under the hood — the
// default provider patches the controller's own env (every sandbox's single
// inherited endpoint; existing missions, envelope generation, and the
// orchestrator all read it) via `onboardProviderAction`, while an additional
// provider is an entry in the `kars-inference-providers` Secret mirrored into
// every sandbox's router via `addAdditionalProviderAction` (only reachable
// when an InferencePolicy names its tag) — but the OPERATOR shouldn't have
// to know two different forms to use either one.

import { useActionState, useEffect, useRef, useState, useTransition, type ElementType } from "react";
import { onboardProviderAction, type ProviderState } from "./provider-actions";
import { addAdditionalProviderAction, removeAdditionalProviderAction, type AdditionalProviderState } from "./additional-provider-actions";
import { discoverModelsAction } from "./provider-discover-actions";
import { undeployLocalModelAction, type LocalInferenceState } from "./local-inference-actions";
import { copilotLoginStartAction, copilotLoginPollAction } from "./copilot-login-actions";
import { disconnectFoundryAction, type FoundryState } from "../foundry-actions";
import { FoundryOnboard } from "../foundry-onboard";
import { Icon, type IconName } from "@/components/icon";
import { HonestState } from "@/components/honest-state";
import { Badge } from "@/components/ui";
import type { AdditionalProvider, DiscoveredModel } from "@/lib/types";
import type {
  LocalInferenceStatus,
  LocalModelDeployment,
  FoundryStatus,
  CopilotLoginStart,
} from "@/lib/bff";

const init: ProviderState = { error: null, ok: null };

type Kind = "github-copilot" | "github-models" | "azure-openai" | "foundry" | "custom" | "local";
type Target = "default" | "additional";

const PRESETS: { id: Kind; label: string; icon: IconName; tagHint: string; endpointEditable: boolean; supportsDiscovery: boolean; hint: string; defaultCapable: boolean }[] = [
  {
    id: "github-copilot",
    label: "GitHub Copilot",
    icon: "link",
    tagHint: "github-copilot",
    endpointEditable: false,
    supportsDiscovery: true,
    hint: "Verifies your Copilot seat live, then lists the exact models GitHub currently serves it — a real query against your seat's model catalog (gpt-5.6, Claude Opus 4.8, Gemini 3.1 Pro, …), with the flagship 'powerful' tier pre-selected.",
    // The cluster-default onboarding form only wires an Azure-style
    // endpoint/key onto the controller — it has no path to a Copilot JWT
    // exchange. Copilot is real and proven, but only as an additional
    // provider (per-mission via InferencePolicy model preference).
    defaultCapable: false,
  },
  {
    id: "github-models",
    label: "GitHub Models",
    icon: "box",
    tagHint: "github-models",
    endpointEditable: false,
    supportsDiscovery: true,
    hint: "Public catalog, no auth needed to discover — a GitHub PAT is only needed for actual inference.",
    // Same reason as GitHub Copilot: the default form has no endpoint field
    // for this kind, so there's nothing for the router's host check to key
    // off. Works today as an additional provider.
    defaultCapable: false,
  },
  {
    id: "azure-openai",
    label: "Azure OpenAI",
    icon: "database",
    tagHint: "azure-openai",
    endpointEditable: true,
    supportsDiscovery: true,
    hint: "Discovers real deployments from the entered endpoint. Workload Identity is preferred on AKS; an API key is for development only.",
    defaultCapable: true,
  },
  {
    id: "foundry",
    label: "Azure AI Foundry",
    icon: "database",
    tagHint: "foundry",
    endpointEditable: true,
    supportsDiscovery: true,
    hint: "Connect a Foundry project — discovers ALL its services (grounding, storage, connections) and deployed models, adding the models to the catalogue tagged foundry. Identity on AKS; API key in dev.",
    defaultCapable: true,
  },
  {
    id: "custom",
    label: "Custom (advanced)",
    icon: "gear",
    tagHint: "",
    endpointEditable: true,
    supportsDiscovery: false,
    hint: "Any OpenAI-compatible endpoint.",
    defaultCapable: true,
  },
  {
    id: "local",
    label: "Local model (in-cluster)",
    icon: "box",
    tagHint: "local",
    endpointEditable: false,
    supportsDiscovery: false,
    hint: "Deploy a model that runs entirely inside this cluster — no external API, no per-token billing. Needs AI Runway + KAITO installed once by an operator (docs/local-inference.md).",
    defaultCapable: true,
  },
];

const STEPS = ["Provider", "Where it applies", "Authentication", "Models & review"] as const;

function StepRail({ step }: { step: number }) {
  return (
    <ol className="flex flex-wrap items-center gap-1.5">
      {STEPS.map((label, i) => (
        <li key={label} className="flex items-center gap-1.5">
          <span
            className={`flex h-6 min-w-6 items-center justify-center rounded-full px-1.5 text-[11px] font-semibold ${
              i === step ? "bg-signal text-signal-fg" : i < step ? "bg-signal/15 text-signal" : "bg-surface-muted text-foreground-muted"
            }`}
            title={label}
          >
            {i < step ? <Icon name="check" size={12} /> : i + 1}
          </span>
          <span className={`hidden text-[11px] sm:inline ${i === step ? "font-medium text-foreground" : "text-foreground-muted"}`}>{label}</span>
          {i < STEPS.length - 1 && <span className="h-px w-4 bg-border" aria-hidden />}
        </li>
      ))}
    </ol>
  );
}

function ProviderCard({ p }: { p: AdditionalProvider }) {
  const isLocal = p.tag.startsWith("local-");
  const isFoundry = p.tag === "foundry";
  const [removeState, removeAction, removePending] = useActionState(removeAdditionalProviderAction, { error: null, ok: null } as AdditionalProviderState);
  const [undeployState, undeployAction, undeployPending] = useActionState(undeployLocalModelAction, { error: null, ok: null } as LocalInferenceState);
  const [disconnectState, disconnectAction, disconnectPending] = useActionState(disconnectFoundryAction, { error: null, ok: null } as FoundryState);
  // Removal is provider-kind-specific: a local model deletes its
  // ModelDeployment CR; Foundry disconnects the project (clearing the
  // connection + its catalogue models); everything else drops its secret keys.
  const removeForm = isLocal ? undeployAction : isFoundry ? disconnectAction : removeAction;
  const state = isLocal ? undeployState : isFoundry ? disconnectState : removeState;
  const pending = isLocal ? undeployPending : isFoundry ? disconnectPending : removePending;
  return (
    <li className="rounded-lg border border-border bg-surface px-3 py-2.5 text-sm">
      <div className="flex items-center justify-between gap-3">
        <span className="flex min-w-0 items-center gap-2">
          <Icon name={isLocal ? "box" : isFoundry ? "database" : "link"} size={13} />
          <span className="font-medium">{p.tag}</span>
          {isLocal && <Badge tone="muted">in-cluster</Badge>}
          {isFoundry && <Badge tone="muted">Foundry</Badge>}
          {p.has_key && <Badge tone="muted">key stored</Badge>}
        </span>
        <span className="flex items-center gap-1.5">
          <form action={removeForm}>
            {!isFoundry && <input type="hidden" name={isLocal ? "name" : "tag"} value={isLocal ? p.tag.replace(/^local-/, "") : p.tag} />}
            <button type="submit" disabled={pending} className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger disabled:opacity-50">
              {pending ? "Removing…" : isFoundry ? "Disconnect" : "Remove"}
            </button>
          </form>
        </span>
      </div>
      {p.endpoint && <p className="mt-1 truncate font-mono text-[11px] text-foreground-muted">{p.endpoint}</p>}
      {p.models.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {p.models.map((m) => (
            <span key={m} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px] text-foreground-muted">{m}</span>
          ))}
        </div>
      )}
      {state.error && <p className="mt-1 text-[11px] text-danger">{state.error}</p>}
    </li>
  );
}

/** The cluster's DEFAULT inference provider, rendered as a uniform row (same
 *  shape as an additional ProviderCard) but carrying the "default" badge —
 *  so nothing looks visually bigger/special. Every mission inherits this. */
function DefaultProviderCard({ p }: { p: { id: string; label: string; note: string; models: string[] } }) {
  const icon: IconName = p.id === "github-copilot" ? "link" : p.id.startsWith("local") ? "box" : "database";
  return (
    <li className="rounded-lg border border-signal/30 bg-signal/[0.04] px-3 py-2.5 text-sm">
      <div className="flex items-center justify-between gap-3">
        <span className="flex min-w-0 items-center gap-2">
          <Icon name={icon} size={13} />
          <span className="font-medium">{p.label}</span>
          <Badge tone="info">default</Badge>
        </span>
        <span className="text-[11px] text-foreground-muted">Every mission inherits this</span>
      </div>
      <p className="mt-1 max-w-xl text-[11px] text-foreground-muted">{p.note}</p>
      {p.models.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {p.models.slice(0, 12).map((m) => (
            <span key={m} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px] text-foreground-muted">{m}</span>
          ))}
          {p.models.length > 12 && <span className="rounded bg-surface-muted px-1.5 py-0.5 text-[10px] text-foreground-muted">+{p.models.length - 12} more</span>}
        </div>
      )}
    </li>
  );
}

/** The local (in-cluster) inference provider — AI Runway/KAITO — as ONE line,
 *  regardless of how many models are deployed on it. Models themselves are
 *  added / removed / set-as-default in the Model catalogue below. */
function AiRunwayCard({
  models,
  available,
  isDefault,
}: {
  models: Array<{ name: string; connected: boolean }>;
  available: boolean;
  isDefault: boolean;
}) {
  return (
    <li className={`rounded-lg border px-3 py-2.5 text-sm ${isDefault ? "border-signal/30 bg-signal/[0.04]" : "border-border bg-surface"}`}>
      <div className="flex items-center justify-between gap-3">
        <span className="flex min-w-0 items-center gap-2">
          <Icon name="box" size={13} />
          <span className="font-medium">AI Runway</span>
          <Badge tone="muted">in-cluster</Badge>
          {available && <Badge tone="muted">detected</Badge>}
          {isDefault && <Badge tone="info">default</Badge>}
        </span>
        <span className="text-[11px] text-foreground-muted">
          {isDefault
            ? "Every mission inherits this"
            : models.length === 0
              ? "no models detected"
              : `${models.length} model${models.length === 1 ? "" : "s"}`}
        </span>
      </div>
      <p className="mt-1 text-[11px] text-foreground-muted">
        Models running entirely inside this cluster. Add, remove, or set a default in the Model catalogue below.
      </p>
      {models.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {models.map((model) => (
            <span key={model.name} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px] text-foreground-muted">
              {model.name}
              {!model.connected && <span className="ml-1 font-sans">· detected only</span>}
            </span>
          ))}
        </div>
      )}
    </li>
  );
}

function normalizeLocalModelName(model: string): string {
  return model.split("/").at(-1) ?? model;
}

function deploymentModelNames(deployment: LocalModelDeployment): string[] {
  const modelName = deployment.model_id
    ? normalizeLocalModelName(deployment.model_id)
    : null;
  return [deployment.name, modelName].filter((value): value is string => Boolean(value));
}

function isAiRunwayProvider(
  provider: AdditionalProvider,
  deployments: LocalModelDeployment[],
): boolean {
  if (provider.tag === "airunway" || provider.tag.startsWith("local-")) return true;
  if (!provider.endpoint?.includes(".svc.cluster.local")) return false;
  return deployments.some((deployment) => {
    const names = new Set(deploymentModelNames(deployment));
    return names.has(provider.endpoint!.split("://").at(-1)!.split(".")[0])
      || provider.models.some((model) => names.has(model));
  });
}

function isAiRunwayDefault(
  provider: { id: string; label: string; models: string[] } | null,
): boolean {
  return Boolean(
    provider
      && (provider.id === "local-inference"
        || provider.id === "airunway"
        || provider.id.startsWith("local-")),
  );
}

/** GitHub Copilot device-flow sign-in, inline in the wizard. Starts the flow,
 *  shows the user code + verification link, polls until approved, then hands
 *  the seat's live models to the parent. The token is minted + stored
 *  server-side — the browser never handles it. */
function CopilotSignIn({
  signedIn,
  onAuthorized,
}: {
  signedIn: boolean;
  onAuthorized: (models: DiscoveredModel[]) => void;
}) {
  const [starting, startStarting] = useTransition();
  const [flow, setFlow] = useState<CopilotLoginStart | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Poll once a flow is active.
  useEffect(() => {
    if (!flow) return;
    let cancelled = false;
    const started = Date.now();
    const tick = async () => {
      if (cancelled) return;
      if (Date.now() - started > flow.expires_in * 1000) {
        setError("The sign-in code expired. Start again.");
        setFlow(null);
        return;
      }
      const r = await copilotLoginPollAction(flow.device_code);
      if (cancelled) return;
      if (r.status === "authorized") {
        if (pollRef.current) clearInterval(pollRef.current);
        setFlow(null);
        onAuthorized(r.models);
      } else if (r.status === "error") {
        if (pollRef.current) clearInterval(pollRef.current);
        setError(r.error);
        setFlow(null);
      }
    };
    pollRef.current = setInterval(() => void tick(), Math.max(flow.interval, 3) * 1000);
    return () => {
      cancelled = true;
      if (pollRef.current) clearInterval(pollRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [flow]);

  if (signedIn) {
    return (
      <p className="flex items-center gap-1.5 text-xs text-signal">
        <Icon name="check" size={13} /> Signed in to GitHub Copilot — your seat&rsquo;s models are listed in the next step.
      </p>
    );
  }

  function begin() {
    setError(null);
    startStarting(async () => {
      const r = await copilotLoginStartAction();
      if (r.ok) setFlow(r.data);
      else setError(r.error);
    });
  }

  if (!flow) {
    return (
      <div className="space-y-2">
        <p className="text-xs text-foreground-muted">
          Sign in with your GitHub account to verify your Copilot seat and load the exact models it serves. No token to paste — it&rsquo;s minted and stored securely on the cluster.
        </p>
        <button type="button" onClick={begin} disabled={starting} className="inline-flex items-center gap-1.5 rounded-lg bg-signal px-3 py-2 text-xs font-semibold text-signal-fg disabled:opacity-50">
          <Icon name="link" size={13} /> {starting ? "Starting…" : "Sign in with GitHub"}
        </button>
        {error && <p className="text-[11px] text-danger">{error}</p>}
      </div>
    );
  }

  return (
    <div className="space-y-2.5">
      <p className="text-xs text-foreground-muted">Finish signing in on GitHub:</p>
      <ol className="space-y-2 text-xs">
        <li className="flex items-center gap-2">
          <span className="grid h-5 w-5 place-items-center rounded-full bg-signal/15 text-[10px] text-signal">1</span>
          <span>Open <a href={flow.verification_uri} target="_blank" rel="noreferrer" className="font-medium text-signal hover:underline">{flow.verification_uri} ↗</a></span>
        </li>
        <li className="flex items-center gap-2">
          <span className="grid h-5 w-5 place-items-center rounded-full bg-signal/15 text-[10px] text-signal">2</span>
          <span className="flex items-center gap-2">
            Enter code
            <code className="rounded border border-border bg-surface-muted px-2 py-0.5 font-mono text-sm tracking-widest">{flow.user_code}</code>
            <button
              type="button"
              onClick={() => { navigator.clipboard?.writeText(flow.user_code); setCopied(true); setTimeout(() => setCopied(false), 1500); }}
              className="rounded border border-border px-1.5 py-0.5 text-[10px] font-medium text-foreground-muted hover:text-signal"
            >
              {copied ? "copied" : "copy"}
            </button>
          </span>
        </li>
      </ol>
      <p className="flex items-center gap-1.5 text-[11px] text-foreground-muted">
        <span className="h-2 w-2 animate-pulse rounded-full bg-signal" /> Waiting for approval…
      </p>
      {error && <p className="text-[11px] text-danger">{error}</p>}
    </div>
  );
}


export function ProviderWizard({
  hasDefaultProvider,
  defaultProvider,
  additionalProviders,
  localInferenceStatus,
  localDeployments,
  foundryStatus,
}: {
  hasDefaultProvider: boolean;
  defaultProvider: { id: string; label: string; note: string; models: string[] } | null;
  additionalProviders: AdditionalProvider[];
  localInferenceStatus: LocalInferenceStatus | null;
  localDeployments: LocalModelDeployment[];
  foundryStatus: FoundryStatus;
}) {
  const [open, setOpen] = useState(false);
  const [step, setStep] = useState(0);
  const [kind, setKind] = useState<Kind>(hasDefaultProvider ? "github-models" : "azure-openai");
  const [target, setTarget] = useState<Target>(hasDefaultProvider ? "additional" : "default");
  const [tag, setTag] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [authMode, setAuthMode] = useState<"workload" | "agentid" | "api">("workload");
  const [key, setKey] = useState("");
  const [models, setModels] = useState("");
  const [discovered, setDiscovered] = useState<DiscoveredModel[] | null>(null);
  const [selectedModels, setSelectedModels] = useState<Set<string>>(new Set());
  const [discoverError, setDiscoverError] = useState<string | null>(null);
  const [discovering, startDiscovering] = useTransition();
  // GitHub Copilot device-flow sign-in (mints a Copilot-authorized token,
  // stored server-side). Once signed in, the seat's live models are populated
  // and the connect step needs no pasted key.
  const [copilotSignedIn, setCopilotSignedIn] = useState(false);

  // Local (in-cluster) inference: the wizard's "Local" kind connects the AI
  // Runway PROVIDER (one line in the list). Deploying / removing / setting a
  // default among individual local models happens in the Model catalogue
  // (see model-catalogue.tsx + local-model-deploy.tsx), so no deploy state
  // lives here anymore.

  const [defaultState, defaultAction, defaultPending] = useActionState(onboardProviderAction, init);
  const [additionalState, additionalAction, additionalPending] = useActionState(addAdditionalProviderAction, init);
  const state = target === "default" ? defaultState : additionalState;
  const pending = target === "default" ? defaultPending : additionalPending;
  const action = target === "default" ? defaultAction : additionalAction;

  const cfg = PRESETS.find((p) => p.id === kind)!;
  const isLocal = kind === "local";
  const isFoundry = kind === "foundry";
  const usedTags = new Set(additionalProviders.map((p) => p.tag));

  function selectKind(k: Kind) {
    setKind(k);
    const c = PRESETS.find((p) => p.id === k)!;
    // GitHub Copilot / GitHub Models have no wired path to become the
    // cluster's default from this form (see PRESETS comments) — steer to
    // "additional", which is fully wired end-to-end.
    if (!c.defaultCapable && target === "default") setTarget("additional");
    setTag(c.tagHint);
    setEndpoint("");
    setDiscovered(null);
    setSelectedModels(new Set());
    setDiscoverError(null);
    setCopilotSignedIn(false);
  }

  function reset() {
    setOpen(false);
    setStep(0);
    setTag("");
    setEndpoint("");
    setAuthMode("workload");
    setKey("");
    setModels("");
    setDiscovered(null);
    setSelectedModels(new Set());
    setCopilotSignedIn(false);
  }

  function toggleModel(id: string) {
    setSelectedModels((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  const canDiscover =
    cfg.supportsDiscovery &&
    (kind === "github-models" ||
      (kind === "azure-openai" && endpoint.trim() && (authMode !== "api" || key.trim())));

  function runDiscover() {
    setDiscoverError(null);
    startDiscovering(async () => {
      const result = await discoverModelsAction({ kind, endpoint: endpoint || undefined, key: key || undefined });
      if (result.models === null) {
        setDiscoverError(result.error);
        setDiscovered(null);
      } else {
        setDiscovered(result.models);
        // Pre-select the recommended pick (currently only Copilot's curated
        // catalog carries this) so the operator isn't forced to hunt for it —
        // mirrors `kars dev`'s picker defaulting to the starred model.
        const recommended = result.models.filter((m) => m.recommended).map((m) => m.id);
        setSelectedModels(new Set(recommended));
      }
    });
  }

  const modelsValue = Array.from(new Set([...Array.from(selectedModels), ...models.split(",").map((s) => s.trim()).filter(Boolean)])).join(",");

  const step1Valid = isLocal || isFoundry ? true : target === "default" ? cfg.defaultCapable : tag.trim().length > 0 && !usedTags.has(tag.trim());
  const step2Valid = target === "default" ? authMode !== "api" || key.trim().length > 0 : cfg.id === "github-copilot" ? copilotSignedIn : cfg.id === "github-models" || cfg.id === "custom" ? true : key.trim().length > 0;
  const canGoNext = [true, step1Valid, step2Valid, true][step];

  if (!open) {
    // Unified list: the cluster default (badged) + every additional provider,
    // all uniform rows. Dedupe an additional whose tag matches the default id
    // (e.g. Copilot signed in via the wizard is also the cluster default) so
    // it shows once, as the default.
    // Collapse the per-model local-<name> entries into ONE "AI Runway
    // (in-cluster)" line — the local inference PROVIDER, not each model.
    // Individual local models are managed in the Model catalogue below.
    const defaultIsAiRunway = isAiRunwayDefault(defaultProvider);
    const localProviders = additionalProviders.filter((provider) =>
      isAiRunwayProvider(provider, localDeployments),
    );
    const additionalToShow = additionalProviders.filter(
      (provider) =>
        provider.tag !== defaultProvider?.id
        && !isAiRunwayProvider(provider, localDeployments),
    );
    const connectedLocalModels = new Set([
      ...(defaultIsAiRunway ? defaultProvider?.models ?? [] : []),
      ...localProviders.flatMap((provider) => provider.models),
    ].map(normalizeLocalModelName));
    const localModelStates = new Map<string, boolean>();
    for (const model of connectedLocalModels) localModelStates.set(model, true);
    for (const deployment of localDeployments.filter((item) => item.phase === "Running")) {
      for (const model of deploymentModelNames(deployment)) {
        localModelStates.set(
          model,
          Boolean(localModelStates.get(model)) || deployment.managed,
        );
      }
    }
    const localModels = Array.from(localModelStates, ([name, connected]) => ({
      name,
      connected,
    }));
    const showAiRunway =
      defaultIsAiRunway
      || localProviders.length > 0
      || localDeployments.length > 0
      || !!localInferenceStatus?.available;
    const anyConfigured = !!defaultProvider || additionalToShow.length > 0 || showAiRunway;
    return (
      <div className="mt-4 space-y-3">
        {anyConfigured ? (
          <ul className="space-y-2">
            {defaultProvider && !defaultIsAiRunway && <DefaultProviderCard p={defaultProvider} />}
            {additionalToShow.map((p) => <ProviderCard key={p.tag} p={p} />)}
            {showAiRunway && (
              <AiRunwayCard
                models={localModels}
                available={!!localInferenceStatus?.available}
                isDefault={defaultIsAiRunway}
              />
            )}
          </ul>
        ) : (
          <HonestState
            variant="empty"
            compact
            title="No provider detected"
            detail="This cluster has no inference provider configured yet. Connect one below to serve models to your teams."
          />
        )}
        <button type="button" onClick={() => setOpen(true)} className="rounded-lg border border-signal/40 bg-signal/10 px-3 py-1.5 text-xs font-medium text-signal hover:bg-signal/15">
          + Connect a provider
        </button>
      </div>
    );
  }

  // For Local / Foundry the step embeds its OWN forms (FoundryOnboard,
  // deploy) — so the wrapper must NOT be a <form> (nested forms are invalid
  // HTML and silently break the inner submit). Those kinds self-submit; only
  // the endpoint+key kinds use the outer form's action.
  const selfContained = isLocal || isFoundry;
  const Wrapper = (selfContained ? "div" : "form") as ElementType;
  const wrapperProps = selfContained ? {} : { action };

  return (
    <div className="mt-4">
      <Wrapper {...wrapperProps} className="rounded-lg border border-border bg-surface p-4">
        {/* Fields for the "default provider" action (onboardProviderAction). */}
        <input type="hidden" name="kind" value={kind} />
        <input type="hidden" name="auth" value={authMode} />
        {target === "default" && !isLocal && <input type="hidden" name="endpoint" value={endpoint} />}
        {target === "default" && !isLocal && <input type="hidden" name="key" value={key} />}
        {/* Fields for the "additional provider" action (addAdditionalProviderAction). */}
        {target === "additional" && !isLocal && <input type="hidden" name="tag" value={tag} />}
        {target === "additional" && !isLocal && <input type="hidden" name="endpoint" value={endpoint} />}
        {target === "additional" && !isLocal && <input type="hidden" name="api_key" value={key} />}
        {!isLocal && <input type="hidden" name="models" value={modelsValue} />}

        <div className="flex items-center justify-between border-b border-border pb-3">
          <StepRail step={step} />
          <button type="button" onClick={reset} className="text-xs text-foreground-muted hover:text-foreground">Cancel</button>
        </div>

        {/* Step 1: pick a provider kind. */}
        {step === 0 && (
          <div className="pt-4">
            <p className="text-xs font-medium text-foreground-muted">Which provider do you want to connect?</p>
            <div className="mt-2 grid gap-2 sm:grid-cols-2">
              {PRESETS.map((p) => {
                const selected = kind === p.id;
                return (
                  <button
                    key={p.id}
                    type="button"
                    onClick={() => { selectKind(p.id); setStep(1); }}
                    className={`flex items-start gap-2.5 rounded-lg border p-3 text-left transition ${
                      selected ? "border-signal bg-signal/[0.06]" : "border-border bg-surface hover:bg-surface-muted"
                    }`}
                  >
                    <Icon name={p.icon} size={16} className={selected ? "text-signal" : "text-foreground-muted"} />
                    <span>
                      <span className="block text-sm font-medium">{p.label}</span>
                      <span className="block text-[11px] text-foreground-muted">{p.hint}</span>
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {/* Step 2 (Local kind only): connect the AI Runway provider — one line
            in the list. Deploying / removing individual models happens in the
            Model catalogue, so this step is just detect + acknowledge. */}
        {isLocal && step === 1 && (
          <div className="space-y-3 pt-4">
            {localInferenceStatus?.available ? (
              <div className="rounded-lg border border-signal/30 bg-signal/[0.04] p-3">
                <p className="flex items-center gap-1.5 text-sm font-medium text-signal">
                  <Icon name="check" size={15} /> AI Runway detected
                </p>
                <p className="mt-1 text-xs text-foreground-muted">
                  In-cluster inference (AI Runway + KAITO) is installed and ready. It appears as a single <span className="font-medium">AI Runway (in-cluster)</span> provider in the list.
                  {localInferenceStatus.gpu_node_count > 0
                    ? ` ${localInferenceStatus.gpu_node_count} GPU node(s) detected — GPU-tier models are available.`
                    : " No GPU nodes detected — CPU-tier models only."}
                </p>
                <p className="mt-2 text-xs text-foreground-muted">
                  Deploy, remove, or set a default among individual local models in the <span className="font-medium">Model catalogue</span> below.
                </p>
              </div>
            ) : (
              <HonestState
                variant="empty"
                compact
                title="AI Runway isn't installed on this cluster yet"
                detail="Local inference needs AI Runway + KAITO installed once by an operator (real helm/kubectl — see docs/local-inference.md; kars doesn't install it for you). Once it's in, this step detects it and it shows up as a provider automatically."
              />
            )}
          </div>
        )}

        {/* Step 2 (Foundry kind only): connect + discover ALL services & models
            inline — the single Foundry surface (the old standalone section is
            gone). Reuses the full FoundryOnboard flow (connect → verify →
            discover services + models → auto-populate the catalogue). */}
        {isFoundry && step === 1 && (
          <div className="pt-4">
            <FoundryOnboard status={foundryStatus} />
          </div>
        )}

        {/* Step 2: where it applies (default vs additional) + connection details. */}
        {!isLocal && !isFoundry && step === 1 && (
          <div className="space-y-3 pt-4">
            <fieldset className="rounded-lg border border-border p-3">
              <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="target" size={13} /> Where it applies</legend>
              <div className="grid gap-2 sm:grid-cols-2">
                <label
                  className={`flex items-start gap-2 rounded-lg border p-2.5 text-xs ${
                    !cfg.defaultCapable ? "cursor-not-allowed border-border opacity-50" : target === "default" ? "cursor-pointer border-signal bg-signal/[0.05]" : "cursor-pointer border-border"
                  }`}
                >
                  <input type="radio" name="_target" checked={target === "default"} disabled={!cfg.defaultCapable} onChange={() => setTarget("default")} className="mt-0.5" />
                  <span>
                    <span className="block font-medium text-foreground">The cluster&rsquo;s default provider</span>
                    <span className="block text-foreground-muted">
                      {cfg.defaultCapable
                        ? "Every mission inherits this unless an InferencePolicy says otherwise. Replaces the current default, if any."
                        : cfg.id === "github-copilot"
                          ? "Connect Copilot here (sign in below) — it becomes an available provider. Then use \u201cSet as default\u201d on it in the provider list to make every mission inherit it."
                          : `${cfg.label} is added as an available provider here. After connecting, use \u201cSet as default\u201d on it in the provider list to make it the cluster default.`}
                    </span>
                  </span>
                </label>
                <label className={`flex cursor-pointer items-start gap-2 rounded-lg border p-2.5 text-xs ${target === "additional" ? "border-signal bg-signal/[0.05]" : "border-border"}`}>
                  <input type="radio" name="_target" checked={target === "additional"} onChange={() => setTarget("additional")} className="mt-0.5" />
                  <span>
                    <span className="block font-medium text-foreground">An additional provider</span>
                    <span className="block text-foreground-muted">Available to every sandbox, but only used by a sandbox whose InferencePolicy names its tag.</span>
                  </span>
                </label>
              </div>
            </fieldset>

            {target === "additional" && (
              <fieldset className="rounded-lg border border-border p-3">
                <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="link" size={13} /> Connection</legend>
                <div className="grid gap-3 sm:grid-cols-2">
                  <label className="block text-xs text-foreground-muted">
                    Tag
                    <input value={tag} onChange={(e) => setTag(e.target.value)} readOnly={cfg.id !== "custom"} placeholder="e.g. foundry-eu" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs read-only:opacity-70" />
                    {usedTags.has(tag.trim()) && <span className="mt-1 block text-[11px] text-danger">Already connected — pick a different tag.</span>}
                  </label>
                  {cfg.endpointEditable ? (
                    <label className="block text-xs text-foreground-muted">
                      Endpoint
                      <input value={endpoint} onChange={(e) => setEndpoint(e.target.value)} placeholder="https://your-resource.services.ai.azure.com" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
                    </label>
                  ) : (
                    <label className="block text-xs text-foreground-muted">
                      Endpoint
                      <input value={cfg.id === "github-copilot" ? "https://api.githubcopilot.com" : cfg.id === "github-models" ? "https://models.github.ai/inference" : ""} readOnly className="mt-1 w-full rounded-lg border border-border bg-surface-muted/50 px-3 py-2 font-mono text-xs opacity-70" />
                    </label>
                  )}
                </div>
              </fieldset>
            )}
            {target === "default" && cfg.endpointEditable && (
              <fieldset className="rounded-lg border border-border p-3">
                <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="link" size={13} /> Connection</legend>
                <label className="block text-xs text-foreground-muted">
                  Endpoint
                  <input value={endpoint} onChange={(e) => setEndpoint(e.target.value)} placeholder="https://your-resource.services.ai.azure.com" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
                </label>
              </fieldset>
            )}
            <p className="text-[11px] text-foreground-muted">{cfg.hint}</p>
          </div>
        )}

        {/* Step 3: authentication. */}
        {!isLocal && step === 2 && (
          <div className="pt-4">
            {target === "default" ? (
              <fieldset className="rounded-lg border border-border p-3">
                <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="lock" size={13} /> Authentication</legend>
                <label className="block text-xs text-foreground-muted">
                  How it authenticates
                  <select value={authMode} onChange={(e) => setAuthMode(e.target.value as typeof authMode)} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm">
                    <option value="workload">Workload identity (recommended)</option>
                    <option value="agentid">Agent id</option>
                    <option value="api">API key (development only)</option>
                  </select>
                </label>
                {authMode === "api" && (
                  <label className="mt-2 block text-xs text-foreground-muted">
                    API key
                    <input value={key} onChange={(e) => setKey(e.target.value)} type="password" placeholder="required" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
                  </label>
                )}
              </fieldset>
            ) : cfg.id === "github-copilot" ? (
              <fieldset className="rounded-lg border border-border p-3">
                <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="lock" size={13} /> Sign in to GitHub Copilot</legend>
                <CopilotSignIn
                  signedIn={copilotSignedIn}
                  onAuthorized={(models) => {
                    setCopilotSignedIn(true);
                    setKey(""); // token is stored server-side, never in the browser
                    setDiscovered(models);
                    setSelectedModels(new Set(models.filter((m) => m.recommended).map((m) => m.id)));
                  }}
                />
              </fieldset>
            ) : (
              <fieldset className="rounded-lg border border-border p-3">
                <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="lock" size={13} /> Authentication</legend>
                <label className="block text-xs text-foreground-muted">
                  {cfg.id === "github-models" ? "GitHub token" : "API key / token (optional)"}
                  <input value={key} onChange={(e) => setKey(e.target.value)} type="password" placeholder={cfg.id === "github-models" ? "required" : "leave blank to use Workload Identity"} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs" />
                </label>
              </fieldset>
            )}
          </div>
        )}

        {/* Step 4: models + review. */}
        {!isLocal && step === 3 && (
          <div className="space-y-3 pt-4">
            <fieldset className="rounded-lg border border-border p-3">
              <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="brain" size={13} /> Models to serve</legend>
              {cfg.supportsDiscovery && cfg.id !== "github-copilot" && (
                <div className="mb-2 flex items-center justify-between">
                  <span className="text-[11px] text-foreground-muted">Discover the real catalog instead of typing ids.</span>
                  <button type="button" onClick={runDiscover} disabled={!canDiscover || discovering} className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-signal-fg disabled:opacity-40">
                    {discovering ? "Discovering…" : "Discover models"}
                  </button>
                </div>
              )}
              {cfg.id === "github-copilot" && (
                <p className="mb-2 text-[11px] text-foreground-muted">Your Copilot seat&rsquo;s live models — the flagship tier is pre-selected. Adjust below.</p>
              )}
              {discoverError && <p className="mb-2 text-[11px] text-danger">{discoverError}</p>}
              {discovered && (
                <div className="mb-2 max-h-40 space-y-1 overflow-y-auto rounded-lg border border-border bg-surface-muted/30 p-2">
                  {discovered.length === 0 ? (
                    <p className="text-[11px] text-foreground-muted">No models found.</p>
                  ) : (
                    discovered.map((m) => (
                      <label key={m.id} className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-xs hover:bg-surface-muted">
                        <input type="checkbox" checked={selectedModels.has(m.id)} onChange={() => toggleModel(m.id)} />
                        <span className="font-mono">{m.id}</span>
                        {m.label && <span className="text-foreground-muted">— {m.label}</span>}
                        {m.recommended && <span className="text-signal" title="Recommended">★</span>}
                      </label>
                    ))
                  )}
                </div>
              )}
              <input
                value={models}
                onChange={(e) => setModels(e.target.value)}
                placeholder={discovered ? "Add more by id (comma-separated, optional)" : "e.g. gpt-4.1, gpt-4o-mini (comma-separated)"}
                className="w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
              />
              <p className="mt-1.5 text-[11px] text-foreground-muted">
                {target === "default"
                  ? "Available to every mission and to the orchestrator."
                  : "These appear in the InferencePolicy model picker tagged with this provider, alongside the cluster's default models."}
              </p>
            </fieldset>

            <div className="rounded-lg border border-dashed border-border bg-surface-muted/30 p-3 text-xs">
              <p className="font-medium text-foreground-muted">Review</p>
              <dl className="mt-1.5 grid gap-1 sm:grid-cols-2">
                <div><dt className="inline text-foreground-muted">Provider: </dt><dd className="inline font-medium">{cfg.label}</dd></div>
                <div><dt className="inline text-foreground-muted">Applies to: </dt><dd className="inline font-medium">{target === "default" ? "Cluster default" : "Additional"}</dd></div>
                {target === "additional" && <div><dt className="inline text-foreground-muted">Tag: </dt><dd className="inline font-mono">{tag || "—"}</dd></div>}
                <div className="sm:col-span-2"><dt className="inline text-foreground-muted">Endpoint: </dt><dd className="inline font-mono">{endpoint || (cfg.endpointEditable ? "—" : "built-in")}</dd></div>
                <div><dt className="inline text-foreground-muted">Auth: </dt><dd className="inline">{target === "default" ? authMode : key ? "key/token provided" : "none"}</dd></div>
                <div><dt className="inline text-foreground-muted">Models: </dt><dd className="inline font-mono">{modelsValue || "—"}</dd></div>
              </dl>
            </div>
          </div>
        )}

        <div className="mt-4 flex items-center gap-3 border-t border-border pt-3">
          {step > 0 && (
            <button type="button" onClick={() => setStep((s) => s - 1)} className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted">← Back</button>
          )}
          {step < ((isLocal || isFoundry) ? 1 : STEPS.length - 1) ? (
            <button type="button" onClick={() => setStep((s) => s + 1)} disabled={!canGoNext} className="rounded-lg bg-signal px-4 py-1.5 text-xs font-semibold text-signal-fg disabled:opacity-50">Next →</button>
          ) : isFoundry ? (
            // Foundry self-submits via the embedded FoundryOnboard flow —
            // the wizard only offers a way out once it's connected/discovered.
            <button type="button" onClick={reset} className="rounded-lg border border-signal/40 bg-signal/10 px-4 py-2 text-sm font-medium text-signal hover:bg-signal/15">Done</button>
          ) : isLocal ? (
            // Local = connect AI Runway (detect only). Model deploy/remove is
            // in the Model catalogue, so the wizard just needs a way out.
            <button type="button" onClick={reset} className="rounded-lg border border-signal/40 bg-signal/10 px-4 py-2 text-sm font-medium text-signal hover:bg-signal/15">Done</button>
          ) : (
            <button type="submit" disabled={pending || !modelsValue} className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
              {pending ? "Connecting…" : target === "default" ? "Set as default provider" : "Connect provider"}
            </button>
          )}
          {isLocal || isFoundry ? null : (
            <>
              {state.error && <p className="text-xs text-danger">{state.error}</p>}
              {state.ok && <p className="text-xs text-ok">{state.ok}</p>}
            </>
          )}
        </div>
      </Wrapper>
    </div>
  );
}
