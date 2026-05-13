// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `ClusterDataSource` — pluggable abstraction hiding whether `ClusterState`
 * comes from live Kube list-watch or a fixture used by tests.
 *
 * Used by the operator TUI panels framework (S14) and (future) by the
 * `kubectl claw attest <name>` read surface (S11).
 */
import { execa } from "execa";
import {
  type ClusterState,
  type CrdItem,
  type CrdCondition,
  type McpServerItem,
  type ToolPolicyItem,
  type InferencePolicyItem,
  type A2AAgentItem,
  type ClawMemoryItem,
  type ClawEvalItem,
  type ClawPairingItem,
  type ProviderState,
  type ProviderStatusSnapshot,
  emptyClusterState,
} from "./types.js";
import { fetchSandboxes } from "../fetchers/sandboxes.js";
import { kctl } from "../helpers.js";

export interface ClusterDataSource {
  fetch(): Promise<ClusterState>;
}

/** Test-only data source: always returns the stored snapshot. */
export class FixtureDataSource implements ClusterDataSource {
  constructor(private snapshot: ClusterState) {}
  async fetch(): Promise<ClusterState> {
    return this.snapshot;
  }
}

// ── Kubectl-backed implementation ──────────────────────────────────

interface KubeListItem {
  metadata?: {
    name?: string;
    namespace?: string;
    creationTimestamp?: string;
    labels?: Record<string, string>;
  };
  spec?: Record<string, unknown>;
  status?: Record<string, unknown>;
}

async function listCrd(plural: string, kubeContext?: string): Promise<KubeListItem[]> {
  try {
    const { stdout } = await execa(
      "kubectl",
      kctl(["get", plural, "-A", "-o", "json"], kubeContext),
      { stdio: "pipe", timeout: 15000 },
    );
    const data = JSON.parse(stdout);
    return Array.isArray(data.items) ? data.items : [];
  } catch {
    return [];
  }
}

function baseItem(it: KubeListItem): CrdItem {
  const conds = ((it.status as { conditions?: CrdCondition[] } | undefined)?.conditions) ?? [];
  return {
    name: it.metadata?.name ?? "",
    namespace: it.metadata?.namespace ?? "",
    age: it.metadata?.creationTimestamp,
    conditions: conds,
  };
}

function asString(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}
function asBool(v: unknown): boolean | undefined {
  return typeof v === "boolean" ? v : undefined;
}
function asNumber(v: unknown): number | undefined {
  return typeof v === "number" ? v : undefined;
}

export class KubectlDataSource implements ClusterDataSource {
  constructor(private kubeContext?: string) {}

  async fetch(): Promise<ClusterState> {
    const out: ClusterState = emptyClusterState();
    try {
      out.sandboxes = await fetchSandboxes(this.kubeContext);
    } catch { /* leave empty */ }

    const [
      pairings, mcpServers, toolPolicies, inferencePolicies,
      a2aAgents, clawMemories, clawEvals,
    ] = await Promise.all([
      listCrd("clawpairings", this.kubeContext),
      listCrd("mcpservers", this.kubeContext),
      listCrd("toolpolicies", this.kubeContext),
      listCrd("inferencepolicies", this.kubeContext),
      listCrd("a2aagents", this.kubeContext),
      listCrd("clawmemories", this.kubeContext),
      listCrd("clawevals", this.kubeContext),
    ]);

    out.pairings = pairings.map<ClawPairingItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const status = (it.status ?? {}) as Record<string, unknown>;
      return {
        ...baseItem(it),
        agentA: asString(spec.agentA),
        agentB: asString(spec.agentB),
        trust: asString(status.trustState) ?? asString(status.trust),
        state: asString(status.phase) ?? asString(status.state),
      };
    });

    out.mcpServers = mcpServers.map<McpServerItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const allowedTools = Array.isArray(spec.allowedTools) ? spec.allowedTools.length : 0;
      return {
        ...baseItem(it),
        url: asString(spec.url),
        productionMode: asBool(spec.productionMode),
        // jwks presence requires a sibling kubectl get of the Secret;
        // unknown until we have it (plan §0.2 #10 — don't guess).
        jwksSecretPresent: "unknown",
        jwksSecretReason: "JWKS Secret presence not probed by data source",
        allowedToolCount: allowedTools,
      };
    });

    out.toolPolicies = toolPolicies.map<ToolPolicyItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const applies = (spec.appliesTo ?? {}) as Record<string, unknown>;
      const commerce = (spec.commerce ?? {}) as Record<string, unknown>;
      const rateLimit = (spec.rateLimit ?? {}) as Record<string, unknown>;
      const rules = Array.isArray(spec.rules) ? spec.rules.length : 0;
      return {
        ...baseItem(it),
        appliesToSandbox: asString(applies.sandboxName),
        commerce: {
          mandates: asBool(commerce.requireMandates),
          floorUsd: asNumber(commerce.floorUsd),
        },
        rateLimitPerMin: asNumber(rateLimit.perMinute),
        ruleCount: rules,
      };
    });

    out.inferencePolicies = inferencePolicies.map<InferencePolicyItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const applies = (spec.appliesTo ?? {}) as Record<string, unknown>;
      const tokenBudget = (spec.tokenBudget ?? {}) as Record<string, unknown>;
      const guardrails = (spec.guardrails ?? {}) as Record<string, unknown>;
      // Post-S10/S13 InferencePolicy.modelPreference is an OBJECT
      // ({ primary: { deployment, provider }, fallback: [...] }), not the
      // legacy string[] some fixtures still use. Support both so the
      // operator panel renders correct deployment names regardless of
      // which CRD revision the cluster is running.
      const mpRaw: unknown = spec.modelPreference;
      let modelPref: string[] = [];
      if (Array.isArray(mpRaw)) {
        modelPref = (mpRaw as unknown[]).filter((m): m is string => typeof m === "string");
      } else if (mpRaw && typeof mpRaw === "object") {
        const mp = mpRaw as Record<string, unknown>;
        const primary = (mp.primary ?? {}) as Record<string, unknown>;
        const primaryDep = typeof primary.deployment === "string" ? primary.deployment : "";
        const fb = Array.isArray(mp.fallback) ? (mp.fallback as unknown[]) : [];
        const fbDeps: string[] = [];
        for (const f of fb) {
          if (f && typeof f === "object") {
            const dep = (f as Record<string, unknown>).deployment;
            if (typeof dep === "string" && dep) fbDeps.push(dep);
          } else if (typeof f === "string") {
            fbDeps.push(f);
          }
        }
        modelPref = primaryDep ? [primaryDep, ...fbDeps] : fbDeps;
      }
      return {
        ...baseItem(it),
        appliesToSandbox: asString(applies.sandboxName),
        dailyTokens: asNumber(tokenBudget.dailyTokens),
        perRequestTokens: asNumber(tokenBudget.perRequestTokens),
        guardrailFloor: asString(guardrails.severityFloor),
        modelPreference: modelPref,
      };
    });

    out.a2aAgents = a2aAgents.map<A2AAgentItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const status = (it.status ?? {}) as Record<string, unknown>;
      const caps = Array.isArray(spec.capabilities)
        ? (spec.capabilities as unknown[]).filter((c): c is string => typeof c === "string")
        : [];
      const agentCardPhase = asString(status.agentCardPhase);
      const cardPub = agentCardPhase === "Published" ? "published"
                    : agentCardPhase === "Pending" ? "pending"
                    : agentCardPhase === "Failed" ? "failed"
                    : "unknown";
      return {
        ...baseItem(it),
        endpointUrl: asString(spec.endpointUrl),
        productionMode: asBool(spec.productionMode),
        agentCardPublished: cardPub as A2AAgentItem["agentCardPublished"],
        agentCardReason: asString(status.agentCardReason),
        capabilities: caps,
      };
    });

    out.clawMemories = clawMemories.map<ClawMemoryItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const status = (it.status ?? {}) as Record<string, unknown>;
      const sbRef = (spec.sandboxRef ?? {}) as Record<string, unknown>;
      const phase = asString(status.phase);
      // The ClawMemory reconciler emits phase="Ready" once the binding has
      // compiled and published successfully (the post-S7 honesty fix flips
      // it to "Pending" with reason=AwaitingFoundryProvisioning until the
      // upstream Foundry store actually exists). Treat "Ready" as bound,
      // "Pending"/"Degraded" as pending, "Failed" as failed.
      const bound = phase === "Ready" || phase === "Bound" ? "bound"
                  : phase === "Pending" || phase === "Degraded" ? "pending"
                  : phase === "Failed" ? "failed"
                  : "unknown";
      return {
        ...baseItem(it),
        sandboxRef: asString(sbRef.name),
        storeName: asString(spec.storeName),
        scope: asString(spec.scope),
        retentionDays: asNumber(spec.retentionDays),
        rbacScopeSummary: asString(status.rbacScopeSummary),
        foundryBound: bound as ClawMemoryItem["foundryBound"],
      };
    });

    out.clawEvals = clawEvals.map<ClawEvalItem>((it) => {
      const spec = (it.spec ?? {}) as Record<string, unknown>;
      const status = (it.status ?? {}) as Record<string, unknown>;
      const sbRef = (spec.sandboxRef ?? {}) as Record<string, unknown>;
      return {
        ...baseItem(it),
        sandboxRef: asString(sbRef.name),
        suite: asString(spec.suite),
        schedule: asString(spec.schedule),
        lastRunAt: asString(status.lastRunAt),
        lastScore: asString(status.lastScore),
        nextScheduledAt: asString(status.nextScheduledAt),
      };
    });

    out.providers = await this.fetchProviderStatus(out);
    return out;
  }

  /** Best-effort provider probe. Anything we can't observe → "unknown". */
  async fetchProviderStatus(state: ClusterState): Promise<ProviderStatusSnapshot> {
    const perSandbox = new Map<string, ProviderState[]>();
    for (const sb of state.sandboxes) {
      perSandbox.set(sb.name, await this.probeSandboxProviders(sb.name, sb.namespace));
    }
    const cluster = await this.probeClusterProviders();
    return { perSandbox, cluster };
  }

  async probeSandboxProviders(sandbox: string, namespace: string): Promise<ProviderState[]> {
    const out: ProviderState[] = [];

    // Foundry — proxy through the in-pod inference router /healthz.
    const foundry = await this.probeRouterHealth(namespace, sandbox, "/healthz");
    out.push({
      id: "foundry",
      label: "Foundry",
      ...foundry,
    });

    // AGT relay/registry — same router, /agt/status.
    const agt = await this.probeRouterHealth(namespace, sandbox, "/agt/status");
    out.push({ id: "agt", label: "AGT", ...agt });

    // ACR pull-through — read recent ImagePullBackOff events.
    out.push(await this.probeAcrPullThrough(namespace));

    // Identity provider — federated WI token presence on the SA.
    out.push(await this.probeIdentity(namespace, sandbox));

    return out;
  }

  async probeClusterProviders(): Promise<ProviderState[]> {
    return [await this.probeAgcIngress()];
  }

  private async probeRouterHealth(
    namespace: string,
    sandbox: string,
    path: string,
  ): Promise<Omit<ProviderState, "id" | "label">> {
    try {
      // Sandbox deployments are named after the sandbox itself; address the
      // pod via `deploy/<sandbox>` so we don't depend on a label selector
      // (the legacy `app.kubernetes.io/name=openclaw` label doesn't exist
      // on AzureClaw sandbox pods — they use `azureclaw.azure.com/sandbox`).
      const { stdout } = await execa(
        "kubectl",
        kctl(
          [
            "exec", "-n", namespace, `deploy/${sandbox}`,
            "-c", "inference-router", "--",
            "curl", "-sS", "--max-time", "3",
            `http://127.0.0.1:8443${path}`,
          ],
          this.kubeContext,
        ),
        { stdio: "pipe", timeout: 6000 },
      );
      if (stdout.includes("\"ok\"") || stdout.includes("\"healthy\"") || stdout.length > 0) {
        return { status: "healthy", detail: stdout.substring(0, 80) };
      }
      return { status: "unknown", reason: "empty response" };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // Trim noisy execa wrapper to the actual stderr line.
      const reason = msg.split("\n").find((ln) => ln.trim().length > 0 && !ln.includes("Command failed")) ?? msg;
      return { status: "unknown", reason: reason.substring(0, 80) };
    }
  }

  private async probeAcrPullThrough(namespace: string): Promise<ProviderState> {
    try {
      const { stdout } = await execa(
        "kubectl",
        kctl(
          ["get", "events", "-n", namespace, "--field-selector",
           "reason=Failed", "-o", "json"],
          this.kubeContext,
        ),
        { stdio: "pipe", timeout: 8000 },
      );
      const data = JSON.parse(stdout);
      const items: Array<{ message?: string }> = Array.isArray(data.items) ? data.items : [];
      const pullFailures = items.filter((e) =>
        typeof e.message === "string" &&
        /ImagePullBackOff|ErrImagePull/.test(e.message ?? ""),
      );
      if (pullFailures.length === 0) {
        return { id: "acr", label: "ACR pull-through", status: "healthy" };
      }
      return {
        id: "acr",
        label: "ACR pull-through",
        status: "degraded",
        reason: `${pullFailures.length} ImagePullBackOff event(s)`,
      };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      return {
        id: "acr",
        label: "ACR pull-through",
        status: "unknown",
        reason: msg.substring(0, 80),
      };
    }
  }

  private async probeIdentity(namespace: string, _sandbox: string): Promise<ProviderState> {
    try {
      // The sandbox-side service account is named `sandbox` (literal) by
      // the controller — not `<sandbox>-sa` (legacy from the prototype).
      const { stdout } = await execa(
        "kubectl",
        kctl(
          ["get", "sa", "sandbox", "-n", namespace, "-o", "json"],
          this.kubeContext,
        ),
        { stdio: "pipe", timeout: 6000 },
      );
      const data = JSON.parse(stdout);
      const annotations: Record<string, string> = data.metadata?.annotations ?? {};
      if (annotations["azure.workload.identity/client-id"]) {
        return { id: "identity", label: "Identity (WI)", status: "healthy" };
      }
      return {
        id: "identity",
        label: "Identity (WI)",
        status: "unknown",
        reason: "no workload-identity annotation",
      };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      const reason = msg.split("\n").find((ln) => ln.trim().length > 0 && !ln.includes("Command failed")) ?? msg;
      return {
        id: "identity",
        label: "Identity (WI)",
        status: "unknown",
        reason: reason.substring(0, 80),
      };
    }
  }

  private async probeAgcIngress(): Promise<ProviderState> {
    try {
      const { stdout } = await execa(
        "kubectl",
        kctl(["get", "gateway.gateway.networking.k8s.io", "-A", "-o", "json"], this.kubeContext),
        { stdio: "pipe", timeout: 6000 },
      );
      const data = JSON.parse(stdout);
      const items: KubeListItem[] = Array.isArray(data.items) ? data.items : [];
      if (items.length === 0) {
        return {
          id: "agc",
          label: "AGC ingress",
          status: "unknown",
          reason: "a2a-ingress not enabled",
        };
      }
      const ready = items.filter((it) => {
        const conds = ((it.status as { conditions?: CrdCondition[] } | undefined)?.conditions) ?? [];
        return conds.some((c) => c.type === "Programmed" && c.status === "True");
      });
      return ready.length === items.length
        ? { id: "agc", label: "AGC ingress", status: "healthy" }
        : { id: "agc", label: "AGC ingress", status: "degraded", reason: `${ready.length}/${items.length} Programmed` };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // Gateway API CRDs aren't installed on every cluster (and shouldn't be
      // required just to render this panel). Surface that cleanly instead of
      // dumping the multi-line `kubectl` error.
      if (/doesn't have a resource type|the server could not find/i.test(msg)) {
        return {
          id: "agc",
          label: "AGC ingress",
          status: "unknown",
          reason: "Gateway API not installed",
        };
      }
      const reason = msg.split("\n").find((ln) => ln.trim().length > 0 && !ln.includes("Command failed")) ?? msg;
      return {
        id: "agc",
        label: "AGC ingress",
        status: "unknown",
        reason: reason.substring(0, 80),
      };
    }
  }
}
