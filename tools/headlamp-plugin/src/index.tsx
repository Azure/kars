// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AzureClaw plugin for the Headlamp dashboard.
 *
 * Targets the operator/SRE managing an AzureClaw cluster. Provides:
 *
 *   1. An **Overview** dashboard with sandbox phase counts, channel
 *      footprint, egress posture summary, and a live recent-sandbox
 *      table — the single page a new shift looks at to triage health.
 *   2. List + detail views for each of the 9 AzureClaw CRDs. The
 *      ClawSandbox detail screen is enhanced with cross-resource links
 *      (inference policy, tool policy, memory) plus a typed Network
 *      Policy card so an operator can see egress posture without
 *      hunting through YAML.
 *
 * Why this matters for AzureClaw specifically: agents have a wide blast
 * radius (sandbox pod, mesh DID, channels, egress, token budget, tool
 * policy). Surfacing those facets in one place reduces the
 * mean-time-to-triage when a customer reports "the agent isn't
 * responding" or "my Telegram bot stopped working" — the operator can
 * confirm phase, channels enabled, egress mode, and pending approvals
 * without leaving Headlamp.
 *
 * Bug fix (the reason the previous version showed
 * "Error loading clawsandboxes"): extending `KubeObject` with bare
 * static fields does NOT register an `apiEndpoint`, so list/get
 * requests fail at runtime. The documented pattern in
 * `@kinvolk/headlamp-plugin@0.13.x` is `makeCustomResourceClass`
 * (see official-plugins/flux/src/kustomizations/Inventory.tsx).
 */

import {
  registerRoute,
  registerSidebarEntry,
} from "@kinvolk/headlamp-plugin/lib";
import { makeCustomResourceClass } from "@kinvolk/headlamp-plugin/lib/lib/k8s/crd";
import type { KubeObject, KubeObjectClass } from "@kinvolk/headlamp-plugin/lib/lib/k8s/KubeObject";
import Secret from "@kinvolk/headlamp-plugin/lib/K8s/secret";
import {
  Link,
  SectionBox,
  SimpleTable,
  StatusLabel,
} from "@kinvolk/headlamp-plugin/lib/CommonComponents";
import * as React from "react";

const GROUP = "azureclaw.azure.com";
const VERSION = "v1alpha1";

interface CrdDescriptor {
  plural: string;
  singular: string;
  kind: string;
  label: string;
  phaseField?: string;
}

const AZURECLAW_CRDS: CrdDescriptor[] = [
  { plural: "clawsandboxes",    singular: "clawsandbox",    kind: "ClawSandbox",     label: "Sandboxes",         phaseField: "phase" },
  { plural: "inferencepolicies", singular: "inferencepolicy", kind: "InferencePolicy", label: "Inference Policies" },
  { plural: "clawmemories",     singular: "clawmemory",     kind: "ClawMemory",      label: "Memories",          phaseField: "phase" },
  { plural: "mcpservers",       singular: "mcpserver",      kind: "McpServer",       label: "MCP Servers",       phaseField: "phase" },
  { plural: "a2aagents",        singular: "a2aagent",       kind: "A2AAgent",        label: "A2A Agents",        phaseField: "phase" },
  { plural: "toolpolicies",     singular: "toolpolicy",     kind: "ToolPolicy",      label: "Tool Policies" },
  { plural: "trustgraphs",      singular: "trustgraph",     kind: "TrustGraph",      label: "Trust Graphs" },
  { plural: "clawpairings",     singular: "clawpairing",    kind: "ClawPairing",     label: "Pairings" },
  { plural: "clawevals",        singular: "claweval",       kind: "ClawEval",        label: "Evals",             phaseField: "phase" },
  { plural: "egressapprovals",  singular: "egressapproval", kind: "EgressApproval",  label: "Egress Approvals",  phaseField: "phase" },
];

const CRD_CLASSES: Record<string, KubeObjectClass> = Object.fromEntries(
  AZURECLAW_CRDS.map(c => [
    c.plural,
    makeCustomResourceClass({
      apiInfo: [{ group: GROUP, version: VERSION }],
      isNamespaced: true,
      singularName: c.singular,
      pluralName: c.plural,
      kind: c.kind,
      customResourceDefinition: undefined as any,
    }),
  ]),
);

const ClawSandboxClass = CRD_CLASSES.clawsandboxes!;

// ──────────────────────────────────────────────────────────────────────
// Sidebar + routes
// ──────────────────────────────────────────────────────────────────────

registerSidebarEntry({
  parent: null,
  name: "azureclaw",
  label: "AzureClaw",
  icon: "mdi:robot-outline",
  url: "/azureclaw",
});

registerSidebarEntry({
  parent: "azureclaw",
  name: "azureclaw-overview",
  label: "Overview",
  url: "/azureclaw",
});

registerRoute({
  path: "/azureclaw",
  sidebar: "azureclaw-overview",
  name: "azureclaw-overview",
  exact: true,
  component: () => <Overview />,
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
    component: () => <CrdList crd={crd} />,
  });

  registerRoute({
    path: `/azureclaw/${crd.plural}/:namespace/:name`,
    sidebar: crd.plural,
    name: `${crd.plural}-detail`,
    exact: true,
    component: () => <CrdDetail crd={crd} />,
  });
}

// ──────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────

type StatusKind = "success" | "error" | "warning" | "";

function phaseToStatus(phase: string | undefined): StatusKind {
  if (!phase) return "";
  if (phase === "Ready" || phase === "Provisioned" || phase === "Active") return "success";
  if (phase === "Degraded" || phase === "Failed" || phase === "Error") return "error";
  // `Compiled` is the crd-well-oiled-machine Slice 0 honesty value:
  // controller wrote the artifact ConfigMap but the router has not
  // echoed a loaded digest yet. Render amber/warning so operators
  // see at a glance that the policy is parsed but NOT live. Falls
  // back to "warning" for any unknown phase (defensive default).
  if (phase === "Compiled" || phase === "Pending") return "warning";
  return "warning";
}

function getStatus(item: KubeObject): Record<string, any> {
  return (item.jsonData?.status ?? {}) as Record<string, any>;
}

function getSpec(item: KubeObject): Record<string, any> {
  return (item.jsonData?.spec ?? {}) as Record<string, any>;
}

// Strip provider prefix(es) from a model identifier so the column reads the
// same regardless of source (inline LiteLLM-style "azure/gpt-5.4" vs.
// InferencePolicy `deployment: gpt-5.4`).
function shortModel(m: string | undefined): string {
  if (!m) return "—";
  const idx = m.lastIndexOf("/");
  return idx >= 0 ? m.slice(idx + 1) : m;
}

function phaseChip(phase: string | undefined) {
  if (!phase) return <span>—</span>;
  const status = phaseToStatus(phase);
  return <StatusLabel status={status as any}>{phase}</StatusLabel>;
}

function urlParams(re: RegExp): RegExpMatchArray | null {
  return window.location.pathname.match(re);
}

// ──────────────────────────────────────────────────────────────────────
// Router policy-status panel (crd-well-oiled-machine Slice 1d.2)
//
// For policy CRDs that participate in the §3 "Ready ⇔ router echo"
// loop, the controller stamps `status.compiledDigest` /
// `status.agtProfileDigest` (the bytes the controller wrote) plus a
// `status.loadedDigest` (the bytes the router actually loaded — only
// populated once every referencing sandbox echoes the digest). The
// `Ready` condition's `reason` carries the live confirmation state
// (`RouterEnforcing` / `AwaitingRouterEnforcement` /
// `NoSandboxesReferencing`). This panel surfaces all three in one
// place so operators don't have to grep `status.conditions`.
//
// Pure read of fields the controller already writes — zero new API
// traffic, no kube-apiserver proxy round-trips, no admin token
// plumbing. Mirrors the data the `azureclaw inspect <sandbox>` CLI
// surfaces (Slice 1d) but on the producer side.
// ──────────────────────────────────────────────────────────────────────

function shortDigest(digest: string | undefined): string {
  if (!digest) return "—";
  const colon = digest.indexOf(":");
  if (colon < 0 || colon + 13 >= digest.length) return digest;
  return `${digest.slice(0, colon + 1)}${digest.slice(colon + 1, colon + 13)}…`;
}

// ──────────────────────────────────────────────────────────────────────
// AllowlistDrift banner (crd-well-oiled-machine Slice 5d)
//
// The controller emits an `AllowlistDrift=True` condition when a
// ClawSandbox's CR carries both `allowedEndpoints` (inline) AND
// `allowlistRef` (signed artifact) and the two diverge. The artifact
// wins and inline is ignored — but operators need to *see* the diff
// so they can either re-sign the bundle to include the new hosts or
// drop the inline override.
//
// The condition message format is:
//   "inline allowedEndpoints differs from verified artifact; artifact wins | drift={JSON}"
// where JSON is `{"added":[...],"removed":[...]}` of `host:port` strings.
// `added` = inline entries missing from the artifact (operator intent
// diverging from authority). `removed` = artifact entries the operator
// did not echo inline. See
// `controller/src/policy_fetcher.rs::DriftSummary` for the canonical
// schema.
// ──────────────────────────────────────────────────────────────────────

interface DriftSummary {
  added: string[];
  removed: string[];
}

function parseDriftFromMessage(message: string | undefined): DriftSummary | null {
  if (!message) return null;
  const idx = message.indexOf(" | drift=");
  if (idx < 0) return null;
  try {
    const parsed = JSON.parse(message.slice(idx + " | drift=".length));
    if (!parsed || typeof parsed !== "object") return null;
    const added = Array.isArray(parsed.added) ? parsed.added.filter((s: unknown) => typeof s === "string") : [];
    const removed = Array.isArray(parsed.removed) ? parsed.removed.filter((s: unknown) => typeof s === "string") : [];
    return { added, removed };
  } catch {
    return null;
  }
}

function AllowlistDriftBanner({ item }: { item: KubeObject }) {
  const status = getStatus(item);
  const conditions = (status.conditions as Array<Record<string, any>> | undefined) ?? [];
  const drift = conditions.find(c => c.type === "AllowlistDrift" && c.status === "True");
  if (!drift) return null;

  const summary = parseDriftFromMessage(drift.message as string | undefined);
  const added = summary?.added ?? [];
  const removed = summary?.removed ?? [];

  return (
    <SectionBox title="⚠ Allowlist drift detected">
      <p style={{ padding: "0.5rem", fontSize: "0.9rem" }}>
        <StatusLabel status={"warning" as any}>artifact wins</StatusLabel>{" "}
        Inline <code>allowedEndpoints</code> diverges from the verified
        signed bundle. The router enforces the bundle; the inline list is
        ignored. Either re-sign the bundle to include the divergent
        hosts, or remove the inline override.
      </p>
      {(added.length > 0 || removed.length > 0) ? (
        <SimpleTable
          data={[
            { side: `Only in inline (operator added, not signed) — ${added.length}`, hosts: added.join(", ") || "—" },
            { side: `Only in bundle (signed, but missing inline) — ${removed.length}`, hosts: removed.join(", ") || "—" },
          ]}
          columns={[
            { label: "Side", getter: (r: any) => r.side },
            { label: "Hosts", getter: (r: any) => <code>{r.hosts}</code> },
          ]}
        />
      ) : (
        <p style={{ padding: "0.5rem", fontSize: "0.85rem", opacity: 0.75 }}>
          {(drift.message as string) ?? "(no diff payload)"}
        </p>
      )}
    </SectionBox>
  );
}

function reasonChip(reason: string | undefined) {
  if (!reason) return <span>—</span>;
  const success = reason === "RouterEnforcing" || reason === "AllDigestsMatch";
  const neutral =
    reason === "NoSandboxesReferencing" || reason === "AsExpected";
  const status: StatusKind = success
    ? "success"
    : neutral
      ? ""
      : reason === "AwaitingRouterEnforcement"
        ? "warning"
        : "error";
  return <StatusLabel status={status as any}>{reason}</StatusLabel>;
}

function RouterPolicyStatusPanel({ crd, item }: { crd: CrdDescriptor; item: KubeObject }) {
  // Only the policy CRDs that the router actually loads carry these
  // fields. ClawSandbox, McpServer, etc. don't participate in the
  // digest-echo loop yet.
  if (
    crd.plural !== "toolpolicies" &&
    crd.plural !== "inferencepolicies" &&
    crd.plural !== "clawmemories"
  ) {
    return null;
  }
  const status = getStatus(item);
  const conditions = (status.conditions as Array<Record<string, any>> | undefined) ?? [];
  const ready = conditions.find(c => c.type === "Ready");
  const compiled =
    crd.plural === "toolpolicies"
      ? (status.agtProfileDigest as string | undefined)
      : (status.compiledDigest as string | undefined);
  const loaded = status.loadedDigest as string | undefined;
  const echo =
    !compiled
      ? "—"
      : loaded && loaded === compiled
        ? "✓ matches"
        : loaded
          ? "≠ mismatched"
          : "(awaiting)";

  return (
    <SectionBox title="Router enforcement (data-plane echo)">
      <SimpleTable
        data={[
          { k: "Compiled digest", v: shortDigest(compiled) },
          { k: "Loaded digest", v: shortDigest(loaded) },
          { k: "Echo", v: echo },
          { k: "Confirmation", v: reasonChip(ready?.reason as string | undefined) },
        ]}
        columns={[
          { label: "Field", getter: (r: any) => r.k },
          { label: "Value", getter: (r: any) => r.v },
        ]}
      />
      <p style={{ padding: "0.5rem", fontSize: "0.85rem", opacity: 0.75 }}>
        The controller polls every referencing sandbox's router and promotes
        <code> phase: Compiled → Ready </code> only when every router echoes
        the exact compiled digest. While{" "}
        <code>AwaitingRouterEnforcement</code>, the policy is parsed but
        <strong> not</strong> live in the data plane.
      </p>
    </SectionBox>
  );
}

// ──────────────────────────────────────────────────────────────────────
// ClawEval status panel (slice 6.4) — surfaces last-run aggregate +
// drift state + corpus reference. Pure read of fields the controller
// already writes; no router admin token in the browser.
// ──────────────────────────────────────────────────────────────────────

function ClawEvalStatusPanel({ crd, item }: { crd: CrdDescriptor; item: KubeObject }) {
  if (crd.plural !== "clawevals") {
    return null;
  }
  const spec = getSpec(item);
  const status = getStatus(item);
  const conditions = (status.conditions as Array<Record<string, any>> | undefined) ?? [];
  const ready = conditions.find(c => c.type === "Ready");
  const drift = conditions.find(c => c.type === "ConformanceDrift");
  const lastResult = status.lastResult as Record<string, any> | undefined;
  const corpus = spec.corpus as Record<string, any> | undefined;
  const corpusLabel = corpus?.builtin
    ? `builtin:${corpus.builtin}`
    : corpus?.bundleRef?.digest
      ? `bundle ${corpus.bundleRef.registry ?? "?"}/${corpus.bundleRef.repository ?? "?"}@${corpus.bundleRef.digest}`
      : "—";

  const passSummary = lastResult
    ? `${lastResult.passedCases ?? 0}/${lastResult.totalCases ?? 0}`
    : "—";
  const driftLabel = lastResult?.drift
    ? <StatusLabel status="error">YES</StatusLabel>
    : lastResult
      ? <StatusLabel status="success">no</StatusLabel>
      : <span style={{ opacity: 0.6 }}>—</span>;

  return (
    <SectionBox title="ClawEval (conformance corpus)">
      <SimpleTable
        data={[
          { k: "Target sandbox", v: (spec.targetSandboxRef as any)?.name ?? "—" },
          { k: "Corpus", v: corpusLabel },
          { k: "Schedule", v: (spec.schedule as string) ?? "(on-demand only)" },
          { k: "Fail sandbox on drift", v: spec.failSandboxOnDrift ? "true" : "false" },
          { k: "Last run", v: (status.lastRunAt as string) ?? "—" },
          { k: "Cases passed", v: passSummary },
          { k: "Drift", v: driftLabel },
          { k: "Ready reason", v: reasonChip(ready?.reason as string | undefined) },
          { k: "Conformance drift reason", v: reasonChip(drift?.reason as string | undefined) },
        ]}
        columns={[
          { label: "Field", getter: (r: any) => r.k },
          { label: "Value", getter: (r: any) => r.v },
        ]}
      />
      <p style={{ padding: "0.5rem", fontSize: "0.85rem", opacity: 0.75 }}>
        ClawEvals replay a signed corpus (or a builtin one) against the target
        sandbox's inference router. The controller stamps each run's verdicts
        on <code>status.lastResult</code> and rolls a history of the most
        recent ones into <code>status.history</code>.
      </p>
    </SectionBox>
  );
}

// ──────────────────────────────────────────────────────────────────────
// Overview dashboard
// ──────────────────────────────────────────────────────────────────────

interface OverviewMetrics {
  sandboxesByPhase: Record<string, number>;
  channelCounts: Record<string, number>;
  egressLearn: number;
  egressStrict: number;
  governanceEnabled: number;
  totalRuntime: Record<string, number>;
}

// Map a credentials-secret data key to a channel name. AzureClaw stores
// channel tokens in <sandbox>-credentials with conventional keys like
// TELEGRAM_BOT_TOKEN / SLACK_BOT_TOKEN. Detecting from secrets is the
// most reliable signal — the openclaw runtime spec itself doesn't
// always carry channels (entrypoint.sh writes them from env at boot).
const CHANNEL_KEY_PATTERNS: Array<[string, RegExp]> = [
  ["telegram", /^TELEGRAM_(BOT_)?TOKEN$/i],
  ["slack",    /^SLACK_(BOT_)?TOKEN$/i],
  ["discord",  /^DISCORD_(BOT_)?TOKEN$/i],
  ["whatsapp", /^WHATSAPP_TOKEN$/i],
];

function channelsFromSecret(s: KubeObject | undefined): Set<string> {
  const found = new Set<string>();
  if (!s) return found;
  const data: Record<string, string> = ((s as any).jsonData?.data ?? {}) as Record<string, string>;
  for (const k of Object.keys(data)) {
    for (const [name, re] of CHANNEL_KEY_PATTERNS) {
      if (re.test(k)) found.add(name);
    }
  }
  return found;
}

function computeMetrics(sandboxes: KubeObject[] | null, secrets: KubeObject[] | null): OverviewMetrics {
  const m: OverviewMetrics = {
    sandboxesByPhase: {},
    channelCounts: {},
    egressLearn: 0,
    egressStrict: 0,
    governanceEnabled: 0,
    totalRuntime: {},
  };
  // Pre-index credentials secrets by namespace/name → set of channels.
  const credsIndex = new Map<string, Set<string>>();
  for (const s of secrets ?? []) {
    const name = s.metadata?.name ?? "";
    const ns = s.metadata?.namespace ?? "";
    if (!name.endsWith("-credentials")) continue;
    const sandboxName = name.replace(/-credentials$/, "");
    credsIndex.set(`${ns}/${sandboxName}`, channelsFromSecret(s));
  }

  for (const sb of sandboxes ?? []) {
    const spec = getSpec(sb);
    const status = getStatus(sb);
    const phase = (status.phase as string) ?? "Unknown";
    m.sandboxesByPhase[phase] = (m.sandboxesByPhase[phase] ?? 0) + 1;

    const np = spec.networkPolicy ?? null;
    // Same default semantics as the controller — absent block ⇒ Learn.
    const isLearn = !np || (np.egressMode ?? "Learn") === "Learn";
    if (isLearn) m.egressLearn += 1;
    else m.egressStrict += 1;

    if (spec.governance?.enabled) m.governanceEnabled += 1;

    const rt = spec.runtime?.kind ?? "Unknown";
    m.totalRuntime[rt] = (m.totalRuntime[rt] ?? 0) + 1;

    const sbName = sb.metadata?.name ?? "";
    const sbNs = sb.metadata?.namespace ?? "";
    // Sandbox pod namespace is azureclaw-<name>; credentials secret
    // lives there with the sandbox's bare name.
    const podNs = `azureclaw-${sbName}`;
    const channels =
      credsIndex.get(`${podNs}/${sbName}`) ??
      credsIndex.get(`${sbNs}/${sbName}`) ??
      new Set<string>();

    // Fallback: also look at runtime spec for explicit channels block.
    const inlineChannels: Record<string, any> = spec.runtime?.openclaw?.config?.channels ?? {};
    for (const k of Object.keys(inlineChannels)) channels.add(k);

    for (const ch of channels) {
      m.channelCounts[ch] = (m.channelCounts[ch] ?? 0) + 1;
    }
  }
  return m;
}

function Overview() {
  const [sandboxes] = (ClawSandboxClass as any).useList() as [KubeObject[] | null];
  const [secrets] = (Secret as any).useList() as [KubeObject[] | null];
  const [inferencePolicies] = (CRD_CLASSES.inferencepolicies as any).useList() as [KubeObject[] | null];
  const [toolPolicies] = (CRD_CLASSES.toolpolicies as any).useList() as [KubeObject[] | null];
  const [memories] = (CRD_CLASSES.clawmemories as any).useList() as [KubeObject[] | null];
  const [mcpServers] = (CRD_CLASSES.mcpservers as any).useList() as [KubeObject[] | null];
  const [a2aAgents] = (CRD_CLASSES.a2aagents as any).useList() as [KubeObject[] | null];
  const metrics = computeMetrics(sandboxes, secrets);
  const total = sandboxes?.length ?? 0;

  const phaseRows = Object.entries(metrics.sandboxesByPhase)
    .sort((a, b) => b[1] - a[1])
    .map(([phase, count]) => ({ phase, count }));

  const runtimeRows = Object.entries(metrics.totalRuntime)
    .sort((a, b) => b[1] - a[1])
    .map(([kind, count]) => ({ kind, count }));

  const channelRows = Object.entries(metrics.channelCounts)
    .sort((a, b) => b[1] - a[1])
    .map(([channel, count]) => ({ channel, count }));

  const recent = (sandboxes ?? [])
    .slice()
    .sort((a, b) => {
      const ta = new Date(a.metadata?.creationTimestamp ?? 0).getTime();
      const tb = new Date(b.metadata?.creationTimestamp ?? 0).getTime();
      return tb - ta;
    })
    .slice(0, 10);

  const policyIndex = new Map<string, KubeObject>();
  for (const p of inferencePolicies ?? []) {
    policyIndex.set(`${p.metadata?.namespace ?? ""}/${p.metadata?.name ?? ""}`, p);
  }
  const recentModel = (sb: KubeObject): string => {
    const spec = getSpec(sb);
    const inline = spec.runtime?.openclaw?.config?.agent?.model ?? spec.agent?.model;
    if (inline) return shortModel(inline);
    const ref = spec.inferenceRef?.name as string | undefined;
    if (!ref) return "—";
    for (const k of [`${sb.metadata?.namespace ?? ""}/${ref}`, `azureclaw-system/${ref}`]) {
      const p = policyIndex.get(k);
      if (p) {
        const ps = getSpec(p);
        const dep = ps.modelPreference?.primary?.deployment;
        if (dep) return shortModel(dep);
      }
    }
    return `(via ${ref})`;
  };
  return (
    <>
      <SectionBox title="AzureClaw — Operator Overview">
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "1rem", padding: "1rem 0" }}>
          <Stat label="Total Sandboxes" value={total} />
          <Stat label="Ready" value={metrics.sandboxesByPhase.Ready ?? 0} tone="success" />
          <Stat label="Degraded" value={metrics.sandboxesByPhase.Degraded ?? 0} tone={metrics.sandboxesByPhase.Degraded ? "error" : ""} />
          <Stat label="Governance ON" value={`${metrics.governanceEnabled} / ${total}`} />
          <Stat label="Egress: Learn / Strict" value={`${metrics.egressLearn} / ${metrics.egressStrict}`} />
        </div>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))", gap: "0.5rem", padding: "0 0 1rem 0" }}>
          <Stat label="Inference Policies" value={inferencePolicies?.length ?? "…"} />
          <Stat label="Tool Policies" value={toolPolicies?.length ?? "…"} />
          <Stat label="Memories" value={memories?.length ?? "…"} />
          <Stat label="MCP Servers" value={mcpServers?.length ?? "…"} />
          <Stat label="A2A Agents" value={a2aAgents?.length ?? "…"} />
        </div>
      </SectionBox>

      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: "1rem" }}>
        <SectionBox title="Sandboxes by Phase">
          <SimpleTable
            data={phaseRows}
            columns={[
              { label: "Phase", getter: (r: any) => phaseChip(r.phase) },
              { label: "Count", getter: (r: any) => r.count },
            ]}
          />
        </SectionBox>
        <SectionBox title="Runtimes">
          <SimpleTable
            data={runtimeRows}
            columns={[
              { label: "Kind", getter: (r: any) => r.kind },
              { label: "Count", getter: (r: any) => r.count },
            ]}
          />
        </SectionBox>
        <SectionBox title="Channels in Use">
          {channelRows.length === 0 ? (
            <p style={{ padding: "1rem" }}>No channels configured.</p>
          ) : (
            <SimpleTable
              data={channelRows}
              columns={[
                { label: "Channel", getter: (r: any) => r.channel },
                { label: "Sandboxes", getter: (r: any) => r.count },
              ]}
            />
          )}
        </SectionBox>
      </div>

      <SectionBox title="Recent Sandboxes">
        <SimpleTable
          data={recent}
          columns={[
            {
              label: "Name",
              getter: (r: KubeObject) => (
                <Link
                  routeName={`clawsandboxes-detail`}
                  params={{
                    namespace: r.metadata?.namespace ?? "",
                    name: r.metadata?.name ?? "",
                  }}
                >
                  {r.metadata?.name}
                </Link>
              ),
            },
            { label: "Namespace", getter: (r: KubeObject) => r.metadata?.namespace ?? "—" },
            { label: "Runtime", getter: (r: KubeObject) => getSpec(r).runtime?.kind ?? "—" },
            { label: "Model", getter: recentModel },
            { label: "Phase", getter: (r: KubeObject) => phaseChip(getStatus(r).phase as string) },
            {
              label: "Egress",
              getter: (r: KubeObject) => {
                const np = getSpec(r).networkPolicy;
                const isLearn = !np || (np.egressMode ?? "Learn") === "Learn";
                return isLearn ? "Learn" : "Strict";
              },
            },
            {
              label: "Age",
              getter: (r: KubeObject) => formatAge(r.metadata?.creationTimestamp as string | undefined),
            },
          ]}
        />
      </SectionBox>
    </>
  );
}

function Stat(props: { label: string; value: React.ReactNode; tone?: StatusKind }) {
  const tone = props.tone ?? "";
  const color = tone === "error" ? "#c62828" : tone === "warning" ? "#ef6c00" : tone === "success" ? "#2e7d32" : "inherit";
  return (
    <div style={{ padding: "1rem", border: "1px solid rgba(127,127,127,0.2)", borderRadius: "6px" }}>
      <div style={{ fontSize: "0.85rem", opacity: 0.7 }}>{props.label}</div>
      <div style={{ fontSize: "1.6rem", fontWeight: 600, color }}>{props.value}</div>
    </div>
  );
}

function formatAge(ts: string | undefined): string {
  if (!ts) return "—";
  const delta = Date.now() - new Date(ts).getTime();
  const s = Math.floor(delta / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

// ──────────────────────────────────────────────────────────────────────
// Generic CRD list
// ──────────────────────────────────────────────────────────────────────

function CrdList({ crd }: { crd: CrdDescriptor }) {
  const cls = CRD_CLASSES[crd.plural]!;
  // Use the same `useList()` pattern that powers the Overview (which
  // works). Passing `resourceClass` to ResourceListView triggers a
  // different internal hook path (TableFromResourceClass) which fails
  // on dynamically-built CR classes — keep things consistent by
  // resolving data ourselves and passing `data`.
  const [items] = (cls as any).useList() as [KubeObject[] | null];

  // Sub-agents do not inline their model in spec — they inherit it
  // from the parent's InferencePolicy via `spec.inferenceRef`. To
  // surface the effective model in the Sandboxes list, we also load
  // InferencePolicies and resolve the deployment lazily.
  const [policies] = (CRD_CLASSES.inferencepolicies as any).useList() as [
    KubeObject[] | null,
  ];
  const policyIndex = React.useMemo(() => {
    const m = new Map<string, KubeObject>();
    for (const p of policies ?? []) {
      m.set(`${p.metadata?.namespace ?? ""}/${p.metadata?.name ?? ""}`, p);
    }
    return m;
  }, [policies]);

  const resolveModel = (sb: KubeObject): string => {
    const spec = getSpec(sb);
    const inline =
      spec.runtime?.openclaw?.config?.agent?.model ?? spec.agent?.model;
    if (inline) return shortModel(inline);
    const ref = spec.inferenceRef?.name as string | undefined;
    if (!ref) return "—";
    // InferencePolicy lives in the operator namespace
    // (azureclaw-system) by convention.
    const candidates = [
      `${sb.metadata?.namespace ?? ""}/${ref}`,
      `azureclaw-system/${ref}`,
    ];
    for (const k of candidates) {
      const p = policyIndex.get(k);
      if (p) {
        const ps = getSpec(p);
        const dep = ps.modelPreference?.primary?.deployment;
        if (dep) return shortModel(dep);
      }
    }
    return `(via ${ref})`;
  };

  const columns: any[] = [
    {
      label: "Name",
      getter: (r: KubeObject) => (
        <Link
          routeName={`${crd.plural}-detail`}
          params={{
            namespace: r.metadata?.namespace ?? "",
            name: r.metadata?.name ?? "",
          }}
        >
          {r.metadata?.name}
        </Link>
      ),
    },
    {
      label: "Namespace",
      getter: (r: KubeObject) => r.metadata?.namespace ?? "—",
    },
  ];
  if (crd.plural === "clawsandboxes") {
    columns.push(
      { label: "Runtime", getter: (r: KubeObject) => getSpec(r).runtime?.kind ?? "—" },
      { label: "Model", getter: resolveModel },
      {
        label: "Egress",
        getter: (r: KubeObject) => {
          const np = getSpec(r).networkPolicy;
          // Controller default: NetworkPolicy block absent OR egressMode
          // unset → Learn mode. Only explicit `egressMode: Strict` → Strict.
          const isLearn = !np || (np.egressMode ?? "Learn") === "Learn";
          return isLearn ? (
            <StatusLabel status="warning">Learn</StatusLabel>
          ) : (
            <StatusLabel status="success">Strict</StatusLabel>
          );
        },
      },
    );
  }
  if (crd.phaseField) {
    columns.push({
      label: "Phase",
      getter: (r: KubeObject) => phaseChip(getStatus(r)[crd.phaseField!] as string),
    });
  }
  columns.push({
    label: "Age",
    getter: (r: KubeObject) => formatAge(r.metadata?.creationTimestamp as string | undefined),
  });

  return (
    <SectionBox title={`AzureClaw — ${crd.label}`}>
      {items === null ? (
        <p style={{ padding: "1rem" }}>Loading…</p>
      ) : items.length === 0 ? (
        <p style={{ padding: "1rem" }}>
          No {crd.label.toLowerCase()} found. Create one with the AzureClaw CLI
          or by applying a CRD manifest.
        </p>
      ) : (
        <SimpleTable data={items} columns={columns} />
      )}
    </SectionBox>
  );
}

// ──────────────────────────────────────────────────────────────────────
// CRD detail view
// ──────────────────────────────────────────────────────────────────────

function CrdDetail({ crd }: { crd: CrdDescriptor }) {
  const match = urlParams(new RegExp(`/azureclaw/${crd.plural}/([^/]+)/([^/]+)`));
  const namespace = match?.[1] ?? "";
  const name = match?.[2] ?? "";
  const cls = CRD_CLASSES[crd.plural]!;
  const [item, error] = (cls as any).useGet(name, namespace);

  if (error) {
    return (
      <SectionBox title={`${crd.kind}: ${name}`}>
        <p>Error: {(error as Error).message}</p>
      </SectionBox>
    );
  }
  if (!item) {
    return <SectionBox title="Loading…">Loading…</SectionBox>;
  }

  const status = getStatus(item);
  const conditions = (status.conditions as Array<Record<string, any>> | undefined) ?? [];

  return (
    <>
      <SectionBox title={`${crd.kind}: ${name}`}>
        <SimpleTable
          data={[
            { k: "Namespace", v: namespace },
            { k: "Phase", v: phaseChip(status.phase as string) },
            { k: "Created", v: item.metadata?.creationTimestamp ?? "—" },
            { k: "UID", v: item.metadata?.uid ?? "—" },
          ]}
          columns={[
            { label: "Field", getter: (r: any) => r.k },
            { label: "Value", getter: (r: any) => r.v },
          ]}
        />
      </SectionBox>

      {crd.plural === "clawsandboxes" && <SandboxExtras item={item} />}

      <AllowlistDriftBanner item={item} />

      <RouterPolicyStatusPanel crd={crd} item={item} />

      <ClawEvalStatusPanel crd={crd} item={item} />

      <SectionBox title="Spec">
        <pre style={{ maxHeight: "400px", overflow: "auto" }}>
          {JSON.stringify(getSpec(item), null, 2)}
        </pre>
      </SectionBox>

      <SectionBox title="Status">
        <pre style={{ maxHeight: "400px", overflow: "auto" }}>
          {JSON.stringify(status, null, 2)}
        </pre>
      </SectionBox>

      {conditions.length > 0 && (
        <SectionBox title="Conditions">
          <SimpleTable
            data={conditions}
            columns={[
              { label: "Type", getter: (c: any) => c.type },
              {
                label: "Status",
                getter: (c: any) => (
                  <StatusLabel status={c.status === "True" ? "success" : "error"}>{c.status}</StatusLabel>
                ),
              },
              { label: "Reason", getter: (c: any) => c.reason ?? "—" },
              { label: "Message", getter: (c: any) => c.message ?? "—" },
            ]}
          />
        </SectionBox>
      )}
    </>
  );
}

// ──────────────────────────────────────────────────────────────────────
// ClawSandbox-specific sections: network policy, channels, related refs.
// These are what an operator most often needs at a glance — phase alone
// doesn't tell you whether Telegram is wired, whether egress is in learn
// mode, or which ToolPolicy is gating tool calls.
// ──────────────────────────────────────────────────────────────────────

function SandboxEgressApprovalsCard({
  sandboxName,
  sandboxNamespace,
}: {
  sandboxName: string;
  sandboxNamespace: string;
}) {
  const [approvals] = (CRD_CLASSES.egressapprovals as any).useList() as [
    KubeObject[] | null,
  ];
  if (!approvals) return null;
  // EgressApprovals live in the same namespace as the ClawSandbox and
  // reference it by spec.sandbox name (sibling, never cross-ns).
  const matching = approvals.filter(a => {
    const ns = a.metadata?.namespace ?? "";
    const spec = getSpec(a);
    return ns === sandboxNamespace && spec.sandbox === sandboxName;
  });
  if (matching.length === 0) return null;

  const rows = matching.map(a => {
    const spec = getSpec(a);
    const status = getStatus(a);
    const hosts: Array<{ host: string; port?: number }> = Array.isArray(spec.hosts)
      ? spec.hosts
      : [];
    const hostSummary = hosts
      .slice(0, 3)
      .map(h => (h.port ? `${h.host}:${h.port}` : h.host))
      .join(", ") + (hosts.length > 3 ? `, +${hosts.length - 3}` : "");
    return {
      name: a.metadata?.name ?? "—",
      phase: status.phase as string | undefined,
      hosts: hostSummary || "—",
      reason: (spec.reason as string | undefined) ?? "—",
      ttl: (spec.ttl as string | undefined) ?? "—",
      expiresAt: status.expiresAt as string | undefined,
      digest: status.mergedDigest as string | undefined,
    };
  });

  return (
    <SectionBox title="Egress Approvals (ephemeral grants)">
      <SimpleTable
        data={rows}
        columns={[
          {
            label: "Name",
            getter: (r: any) => (
              <Link
                routeName="egressapprovals-detail"
                params={{ namespace: sandboxNamespace, name: r.name }}
              >
                {r.name}
              </Link>
            ),
          },
          { label: "Phase", getter: (r: any) => phaseChip(r.phase) },
          { label: "Hosts", getter: (r: any) => r.hosts },
          { label: "TTL", getter: (r: any) => r.ttl },
          { label: "Expires", getter: (r: any) => r.expiresAt ?? "—" },
          { label: "Reason", getter: (r: any) => r.reason },
          { label: "Merged digest", getter: (r: any) => shortDigest(r.digest) },
        ]}
      />
      <p style={{ padding: "0.5rem", fontSize: "0.85rem", opacity: 0.75 }}>
        Grants unioned with the baseline allowlist on the data plane. <code>Active</code>{" "}
        means the router has echoed the merged digest. Grants auto-expire at{" "}
        <code>status.expiresAt</code>; revoke early with <code>azureclaw egress revoke</code>.
      </p>
    </SectionBox>
  );
}

function SandboxExtras({ item }: { item: KubeObject }) {
  const spec = getSpec(item);
  const status = getStatus(item);
  const namespace = item.metadata?.namespace ?? "";
  const name = item.metadata?.name ?? "";

  // Sandbox pod ns is azureclaw-<name>; the credentials Secret lives
  // there with name "<sandbox>-credentials". Channel state lives
  // there (TELEGRAM_BOT_TOKEN, etc.), not in spec.
  const podNs = `azureclaw-${name}`;
  const [credSecret] = (Secret as any).useGet(`${name}-credentials`, podNs) as [
    KubeObject | null,
    Error | null,
  ];

  const npRaw = spec.networkPolicy ?? null;
  const np = npRaw ?? {};
  const isLearn = !npRaw || (np.egressMode ?? "Learn") === "Learn";
  const allowed: Array<{ host?: string; port?: number }> = Array.isArray(np.allowedEndpoints) ? np.allowedEndpoints : [];

  // Detect channels from secret keys (preferred) and from spec inline.
  const detectedChannels = new Set<string>(channelsFromSecret(credSecret ?? undefined));
  const inlineChannels: Record<string, any> = spec.runtime?.openclaw?.config?.channels ?? {};
  for (const k of Object.keys(inlineChannels)) detectedChannels.add(k);
  const channelEntries = Array.from(detectedChannels).map(c => ({
    channel: c,
    enabled: inlineChannels[c]?.enabled !== false,
    source: credSecret && Object.keys((credSecret as any).jsonData?.data ?? {}).some(k =>
      CHANNEL_KEY_PATTERNS.some(([n, re]) => n === c && re.test(k)),
    )
      ? "Secret"
      : "Spec",
  }));

  const inferenceRef = spec.inferenceRef?.name as string | undefined;
  const toolPolicyRef = spec.governance?.toolPolicyRef?.name as string | undefined;
  const memoryRef = spec.memoryRef?.name as string | undefined;
  const mcpRefs: Array<{ name?: string }> = Array.isArray(spec.mcpServerRefs) ? spec.mcpServerRefs : [];

  return (
    <>
      <SectionBox title="Network Policy (Egress)">
        <SimpleTable
          data={[
            { k: "Default Deny", v: String(np.defaultDeny ?? false) },
            { k: "Learn Mode", v: isLearn ? <StatusLabel status="warning">LEARN</StatusLabel> : <StatusLabel status="success">STRICT</StatusLabel> },
            { k: "Allowed Endpoints", v: `${allowed.length}` },
          ]}
          columns={[
            { label: "Field", getter: (r: any) => r.k },
            { label: "Value", getter: (r: any) => r.v },
          ]}
        />
        {allowed.length > 0 && (
          <div style={{ marginTop: "1rem" }}>
            <h4>Allowed Endpoints</h4>
            <SimpleTable
              data={allowed}
              columns={[
                { label: "Host", getter: (r: any) => r.host ?? "—" },
                { label: "Port", getter: (r: any) => r.port ?? "—" },
              ]}
            />
          </div>
        )}
      </SectionBox>

      <SectionBox title="Channels & Integrations">
        {channelEntries.length === 0 ? (
          <p style={{ padding: "0.5rem" }}>
            No channels configured for namespace <code>{podNs}</code>. Use{" "}
            <code>azureclaw credentials set telegram-token …</code> +{" "}
            <code>--channels telegram</code>.
          </p>
        ) : (
          <SimpleTable
            data={channelEntries}
            columns={[
              { label: "Channel", getter: (r: any) => r.channel },
              {
                label: "Status",
                getter: (r: any) =>
                  r.enabled ? <StatusLabel status="success">ENABLED</StatusLabel> : <StatusLabel status="warning">DISABLED</StatusLabel>,
              },
              { label: "Source", getter: (r: any) => r.source },
            ]}
          />
        )}
      </SectionBox>

      <SectionBox title="Related Resources">
        <SimpleTable
          data={[
            ...(inferenceRef ? [{ kind: "InferencePolicy", name: inferenceRef, route: "inferencepolicies-detail" }] : []),
            ...(toolPolicyRef ? [{ kind: "ToolPolicy", name: toolPolicyRef, route: "toolpolicies-detail" }] : []),
            ...(memoryRef ? [{ kind: "ClawMemory", name: memoryRef, route: "clawmemories-detail" }] : []),
            ...mcpRefs.map(r => ({ kind: "McpServer", name: r.name ?? "", route: "mcpservers-detail" })),
          ]}
          columns={[
            { label: "Kind", getter: (r: any) => r.kind },
            {
              label: "Name",
              getter: (r: any) =>
                r.name ? (
                  <Link routeName={r.route} params={{ namespace: "azureclaw-system", name: r.name }}>
                    {r.name}
                  </Link>
                ) : (
                  "—"
                ),
            },
          ]}
        />
      </SectionBox>

      {status.mesh && (
        <SectionBox title="Mesh (AGT)">
          <SimpleTable
            data={[
              { k: "Agent DID", v: status.mesh.did ?? "—" },
              { k: "Registered", v: status.mesh.registered ? <StatusLabel status="success">YES</StatusLabel> : <StatusLabel status="error">NO</StatusLabel> },
              { k: "Trust Score", v: status.mesh.trustScore ?? "—" },
              { k: "Last Heartbeat", v: status.mesh.lastHeartbeat ?? "—" },
            ]}
            columns={[
              { label: "Field", getter: (r: any) => r.k },
              { label: "Value", getter: (r: any) => r.v },
            ]}
          />
        </SectionBox>
      )}

      <SandboxEgressApprovalsCard sandboxName={name} sandboxNamespace={namespace} />

      <SectionBox title="Pod & Workspace">
        <SimpleTable
          data={[
            {
              k: "CR Namespace",
              v: (
                <Link routeName="namespace" params={{ name: namespace }}>
                  {namespace}
                </Link>
              ),
            },
            {
              k: "Sandbox Namespace",
              v: (
                <Link routeName="namespace" params={{ name: podNs }}>
                  {podNs}
                </Link>
              ),
            },
            {
              k: "Pods",
              v: (
                <Link routeName="pods" params={{ namespace: podNs }}>
                  View pods in {podNs}
                </Link>
              ),
            },
            {
              k: "Deployment",
              v: (
                <Link routeName="deployments" params={{ namespace: podNs }}>
                  View deployments in {podNs}
                </Link>
              ),
            },
            {
              k: "Secrets",
              v: (
                <Link routeName="secrets" params={{ namespace: podNs }}>
                  View secrets in {podNs}
                </Link>
              ),
            },
          ]}
          columns={[
            { label: "Field", getter: (r: any) => r.k },
            { label: "Value", getter: (r: any) => r.v },
          ]}
        />
      </SectionBox>
    </>
  );
}
