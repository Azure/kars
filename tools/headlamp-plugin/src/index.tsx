// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AzureClaw plugin for the Headlamp dashboard.
 *
 * Adds:
 *   - A top-level "AzureClaw" sidebar entry with one sub-entry per CRD.
 *   - List + detail views for the 9 AzureClaw custom resources.
 *
 * The plugin is data-driven: every CRD is described by a single
 * descriptor object (group, version, plural, kind), and the framework
 * generates list/detail routes + sidebar entries from it. Adding a new
 * CRD means appending one entry to AZURECLAW_CRDS — no per-CRD
 * boilerplate.
 */

import {
  registerRoute,
  registerSidebarEntry,
} from "@kinvolk/headlamp-plugin/lib";
import {
  KubeObject,
  makeCustomResourceClass,
} from "@kinvolk/headlamp-plugin/lib/k8s/cluster";
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

interface CrdDescriptor {
  /** lower-case plural — used in routes and as sidebar id. */
  plural: string;
  /** PascalCase kind. */
  kind: string;
  /** Singular shown in the sidebar. */
  label: string;
  /** Optional .status.phase field path. */
  phaseField?: string;
}

const AZURECLAW_CRDS: CrdDescriptor[] = [
  { plural: "clawsandboxes", kind: "ClawSandbox", label: "Sandboxes", phaseField: "phase" },
  { plural: "inferencepolicies", kind: "InferencePolicy", label: "Inference Policies" },
  { plural: "clawmemories", kind: "ClawMemory", label: "Memories", phaseField: "phase" },
  { plural: "mcpservers", kind: "McpServer", label: "MCP Servers", phaseField: "phase" },
  { plural: "a2aagents", kind: "A2AAgent", label: "A2A Agents", phaseField: "phase" },
  { plural: "toolpolicies", kind: "ToolPolicy", label: "Tool Policies" },
  { plural: "trustgraphs", kind: "TrustGraph", label: "Trust Graphs" },
  { plural: "clawpairings", kind: "ClawPairing", label: "Pairings" },
  { plural: "clawevals", kind: "ClawEval", label: "Evals", phaseField: "phase" },
];

// Top-level AzureClaw entry.
registerSidebarEntry({
  parent: null,
  name: "azureclaw",
  label: "AzureClaw",
  icon: "mdi:robot-outline",
  url: `/azureclaw/${AZURECLAW_CRDS[0]?.plural ?? "clawsandboxes"}`,
});

for (const crd of AZURECLAW_CRDS) {
  const ResourceClass = makeCustomResourceClass({
    apiInfo: [{ group: GROUP, version: VERSION, isNamespaced: true }],
    pluralName: crd.plural,
    singularName: crd.kind,
  });

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
      React.createElement(ResourceListView, {
        title: `AzureClaw — ${crd.label}`,
        resourceClass: ResourceClass,
        columns: buildColumns(crd),
      }),
  });

  registerRoute({
    path: `/azureclaw/${crd.plural}/:namespace/:name`,
    sidebar: crd.plural,
    name: `${crd.plural}-detail`,
    exact: true,
    component: () => React.createElement(DetailView, { crd, ResourceClass }),
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
        return React.createElement(
          StatusLabel,
          { status },
          phase,
        );
      },
    });
  }
  cols.push("age");
  return cols;
}

interface DetailViewProps {
  crd: CrdDescriptor;
  ResourceClass: ReturnType<typeof makeCustomResourceClass>;
}

function DetailView({ crd, ResourceClass }: DetailViewProps) {
  const params = (window.location.pathname.match(
    new RegExp(`/azureclaw/${crd.plural}/([^/]+)/([^/]+)`),
  ) ?? []) as string[];
  const namespace = params[1];
  const name = params[2];
  const [item, error] = ResourceClass.useGet(name, namespace);

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
