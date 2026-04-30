// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * McpServer panel (S1) — list + Conditions + JWKS Secret presence.
 *
 * The JWKS secret backs OAuth 2.1 verification; we display "<present>" /
 * "<missing>" only — never raw values (plan §0.2 "no secret rendering").
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader } from "./util.js";

export const mcpServerPanel: Panel = {
  id: "mcpserver",
  title: "McpServer",
  refreshIntervalMs: 30_000,

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    // McpServer is namespace-scoped to a sandbox indirectly via
    // `allowedSandboxes` selectors. With a sandbox filter we surface
    // every McpServer in the same namespace as the sandbox (best we can
    // do without resolving label selectors here).
    const items = opts?.sandbox
      ? state.mcpServers.filter((m) => m.namespace === `azureclaw-${opts.sandbox}`)
      : state.mcpServers;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const m of items) {
      lines.push(renderItemHeader(m));
      const url = m.url ?? "(no url)";
      const prod = m.productionMode === undefined ? "?" : (m.productionMode ? "yes" : "no");
      lines.push(`  url: {cyan-fg}${url}{/}   production=${prod}   tools=${m.allowedToolCount ?? 0}`);
      const jwksColor =
        m.jwksSecretPresent === "present" ? "green" :
        m.jwksSecretPresent === "missing" ? "red" : "yellow";
      const jwksValue = m.jwksSecretPresent ?? "unknown";
      const jwksReason = m.jwksSecretReason ? ` {gray-fg}(${m.jwksSecretReason}){/}` : "";
      lines.push(`  jwks-secret: {${jwksColor}-fg}<${jwksValue}>{/}${jwksReason}`);
      lines.push(formatConditions(m.conditions));
    }
    return lines.join("\n");
  },
};
