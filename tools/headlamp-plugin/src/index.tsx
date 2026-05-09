// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AzureClaw plugin for the Headlamp dashboard.
 *
 * Adds:
 *   - A top-level "AzureClaw" sidebar entry with one sub-entry per CRD.
 *   - List + detail views for the 9 AzureClaw custom resources.
 *
 * The CRD ResourceClasses are declared statically (one per CRD) instead
 * of via a `makeCustomResourceClass` factory call — that helper does not
 * exist in `@kinvolk/headlamp-plugin@^0.13.x`. Extending `KubeObject`
 * directly with `static kind / apiName / apiVersion / isNamespaced` is
 * the documented pattern (see headlamp-k8s/plugins/flux/src/common/Resources.tsx).
 */

import {
  registerRoute,
  registerSidebarEntry,
} from "@kinvolk/headlamp-plugin/lib";
import { KubeObject } from "@kinvolk/headlamp-plugin/lib/k8s/cluster";
import {
  Link,
  SectionBox,
  SimpleTable,
  StatusLabel,
} from "@kinvolk/headlamp-plugin/lib/CommonComponents";
import { ResourceListView } from "@kinvolk/headlamp-plugin/lib/components/common/Resource";
import * as React from "react";

const GROUP = "azureclaw.azure.com";
const VERSION = "v1alpha1";
const API_VERSION = `${GROUP}/${VERSION}`;

class ClawSandbox extends KubeObject {
  static kind = "ClawSandbox";
  static apiName = "clawsandboxes";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class InferencePolicy extends KubeObject {
  static kind = "InferencePolicy";
  static apiName = "inferencepolicies";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class ClawMemory extends KubeObject {
  static kind = "ClawMemory";
  static apiName = "clawmemories";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class McpServer extends KubeObject {
  static kind = "McpServer";
  static apiName = "mcpservers";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class A2AAgent extends KubeObject {
  static kind = "A2AAgent";
  static apiName = "a2aagents";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class ToolPolicy extends KubeObject {
  static kind = "ToolPolicy";
  static apiName = "toolpolicies";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class TrustGraph extends KubeObject {
  static kind = "TrustGraph";
  static apiName = "trustgraphs";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class ClawPairing extends KubeObject {
  static kind = "ClawPairing";
  static apiName = "clawpairings";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}
class ClawEval extends KubeObject {
  static kind = "ClawEval";
  static apiName = "clawevals";
  static apiVersion = API_VERSION;
  static isNamespaced = true;
}

interface CrdDescriptor {
  plural: string;
  kind: string;
  label: string;
  ResourceClass: typeof KubeObject;
  phaseField?: string;
}

const AZURECLAW_CRDS: CrdDescriptor[] = [
  { plural: "clawsandboxes", kind: "ClawSandbox", label: "Sandboxes", ResourceClass: ClawSandbox, phaseField: "phase" },
  { plural: "inferencepolicies", kind: "InferencePolicy", label: "Inference Policies", ResourceClass: InferencePolicy },
  { plural: "clawmemories", kind: "ClawMemory", label: "Memories", ResourceClass: ClawMemory, phaseField: "phase" },
  { plural: "mcpservers", kind: "McpServer", label: "MCP Servers", ResourceClass: McpServer, phaseField: "phase" },
  { plural: "a2aagents", kind: "A2AAgent", label: "A2A Agents", ResourceClass: A2AAgent, phaseField: "phase" },
  { plural: "toolpolicies", kind: "ToolPolicy", label: "Tool Policies", ResourceClass: ToolPolicy },
  { plural: "trustgraphs", kind: "TrustGraph", label: "Trust Graphs", ResourceClass: TrustGraph },
  { plural: "clawpairings", kind: "ClawPairing", label: "Pairings", ResourceClass: ClawPairing },
  { plural: "clawevals", kind: "ClawEval", label: "Evals", ResourceClass: ClawEval, phaseField: "phase" },
];

// Top-level AzureClaw entry. URL points at the first sub-entry so the
// parent itself is clickable (Headlamp doesn't auto-route parent items
// without a registerRoute, so we just defer to the first child route).
registerSidebarEntry({
  parent: null,
  name: "azureclaw",
  label: "AzureClaw",
  icon: "mdi:robot-outline",
  url: `/azureclaw/${AZURECLAW_CRDS[0]!.plural}`,
});

for (const crd of AZURECLAW_CRDS) {
  registerSidebarEntry({
    parent: "azureclaw",
    name: crd.plural,
    label: crd.label,
    url: `/azureclaw/${crd.plural}`,
  });

  registerRoute({
    path: `/azureclaw/${crd.plural}`,
    sidebar: crd.plural,
    name: crd.plural,
    exact: true,
    component: () =>
      React.createElement(ResourceListView as any, {
        title: `AzureClaw — ${crd.label}`,
        resourceClass: crd.ResourceClass,
        columns: buildColumns(crd),
      }),
  });

  registerRoute({
    path: `/azureclaw/${crd.plural}/:namespace/:name`,
    sidebar: crd.plural,
    name: `${crd.plural}-detail`,
    exact: true,
    component: () => React.createElement(DetailView, { crd }),
  });
}

function buildColumns(crd: CrdDescriptor) {
  const cols: any[] = [
    "name",
    {
      label: "Namespace",
      getter: (r: KubeObject) =>
        React.createElement(
          Link,
          {
            routeName: "namespace",
            params: { name: r.metadata?.namespace ?? "" },
          },
          r.metadata?.namespace,
        ),
    },
  ];
  if (crd.phaseField) {
    cols.push({
      label: "Phase",
      getter: (r: KubeObject) => {
        const phase = (r.jsonData?.status as Record<string, unknown> | undefined)?.[
          crd.phaseField!
        ] as string | undefined;
        if (!phase) return "—";
        const status =
          phase === "Ready" || phase === "Provisioned"
            ? "success"
            : phase === "Degraded" || phase === "Failed"
              ? "error"
              : "warning";
        return React.createElement(StatusLabel, { status }, phase);
      },
    });
  }
  cols.push("age");
  return cols;
}

interface DetailViewProps {
  crd: CrdDescriptor;
}

function DetailView({ crd }: DetailViewProps) {
  const params = (window.location.pathname.match(
    new RegExp(`/azureclaw/${crd.plural}/([^/]+)/([^/]+)`),
  ) ?? []) as string[];
  const namespace = params[1];
  const name = params[2];
  const [item, error] = (crd.ResourceClass as any).useGet(name, namespace);

  if (error) {
    return React.createElement(
      SectionBox,
      { title: `${crd.kind}: ${name}` },
      `Error: ${(error as Error).message}`,
    );
  }
  if (!item) {
    return React.createElement(SectionBox, { title: "Loading…" }, "Loading…");
  }

  const status = (item.jsonData?.status ?? {}) as Record<string, unknown>;
  const conditions =
    (status.conditions as Array<Record<string, unknown>> | undefined) ?? [];

  return React.createElement(
    React.Fragment,
    null,
    React.createElement(
      SectionBox,
      { title: `${crd.kind}: ${name}` },
      React.createElement("pre", null, JSON.stringify(item.jsonData?.spec ?? {}, null, 2)),
    ),
    React.createElement(
      SectionBox,
      { title: "Status" },
      React.createElement("pre", null, JSON.stringify(status, null, 2)),
    ),
    conditions.length > 0
      ? React.createElement(
          SectionBox,
          { title: "Conditions" },
          React.createElement(SimpleTable, {
            data: conditions,
            columns: [
              { label: "Type", getter: (c: Record<string, unknown>) => c.type as string },
              {
                label: "Status",
                getter: (c: Record<string, unknown>) =>
                  React.createElement(
                    StatusLabel,
                    { status: c.status === "True" ? "success" : "error" },
                    c.status as string,
                  ),
              },
              { label: "Reason", getter: (c: Record<string, unknown>) => (c.reason as string) ?? "—" },
              { label: "Message", getter: (c: Record<string, unknown>) => (c.message as string) ?? "—" },
            ],
          }),
        )
      : null,
  );
}
