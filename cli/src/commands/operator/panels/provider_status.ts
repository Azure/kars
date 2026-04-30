// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Provider-status panel (S14) — Foundry, AGT, ACR pull-through, AGC ingress,
 * identity provider.
 *
 * Sources (per plan §S14):
 *   - Foundry: /healthz proxy on the inference-router (per-sandbox).
 *   - AGT relay/registry: existing health probes (per sandbox via router).
 *   - ACR pull-through: ImagePullBackOff event presence in the sandbox NS.
 *   - AGC ingress: status of `Gateway` / `HTTPRoute` if `--enable-a2a-ingress`
 *     was used (cluster-wide).
 *   - Identity provider: presence + freshness of WI federated tokens.
 *
 * If the data source can't observe a provider, the entry surfaces as
 * `unknown` with a verbatim reason — never invented data (plan §0.2 #10).
 */
import type {
  Panel,
  PanelRenderOpts,
  ClusterState,
  ProviderState,
} from "./types.js";
import { EMPTY } from "./util.js";

function statusColor(s: ProviderState["status"]): string {
  return s === "healthy" ? "green" :
         s === "degraded" ? "yellow" :
         s === "down" ? "red" : "gray";
}

function statusGlyph(s: ProviderState["status"]): string {
  return s === "healthy" ? "●" :
         s === "degraded" ? "◐" :
         s === "down" ? "○" : "?";
}

function renderProviders(label: string, items: ProviderState[]): string {
  if (items.length === 0) return `{bold}${label}{/}\n  ${EMPTY}`;
  const lines = [`{bold}${label}{/}`];
  for (const p of items) {
    const c = statusColor(p.status);
    const g = statusGlyph(p.status);
    const reason = p.reason ? ` {gray-fg}— ${p.reason}{/}` : "";
    lines.push(`  {${c}-fg}${g}{/} ${p.label.padEnd(18)} ${p.status}${reason}`);
    if (p.detail) lines.push(`    {gray-fg}${p.detail}{/}`);
  }
  return lines.join("\n");
}

export const providerStatusPanel: Panel = {
  id: "provider_status",
  title: "Providers",
  refreshIntervalMs: 30_000,

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const lines: string[] = [];

    if (opts?.sandbox) {
      const perSb = state.providers.perSandbox.get(opts.sandbox) ?? [];
      lines.push(renderProviders(`Per-sandbox (${opts.sandbox})`, perSb));
    } else {
      // Group by sandbox for readability when --per-sandbox is off.
      if (state.providers.perSandbox.size === 0) {
        lines.push(renderProviders("Per-sandbox", []));
      } else {
        for (const [sb, providers] of state.providers.perSandbox) {
          lines.push(renderProviders(`Per-sandbox: ${sb}`, providers));
        }
      }
    }
    lines.push("");
    lines.push(renderProviders("Cluster-wide", state.providers.cluster));
    return lines.join("\n");
  },
};
