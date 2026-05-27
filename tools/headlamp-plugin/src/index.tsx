// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * kars plugin for the Headlamp dashboard.
 *
 * Targets the operator/SRE managing an kars cluster. Provides:
 *
 *   1. An **Overview** dashboard with sandbox phase counts, channel
 *      footprint, egress posture summary, and a live recent-sandbox
 *      table — the single page a new shift looks at to triage health.
 *   2. List + detail views for each of the 9 kars CRDs. The
 *      KarsSandbox detail screen is enhanced with cross-resource links
 *      (inference policy, tool policy, memory) plus a typed Network
 *      Policy card so an operator can see egress posture without
 *      hunting through YAML.
 *
 * Why this matters for kars specifically: agents have a wide blast
 * radius (sandbox pod, mesh DID, channels, egress, token budget, tool
 * policy). Surfacing those facets in one place reduces the
 * mean-time-to-triage when a customer reports "the agent isn't
 * responding" or "my Telegram bot stopped working" — the operator can
 * confirm phase, channels enabled, egress mode, and pending approvals
 * without leaving Headlamp.
 *
 * Bug fix (the reason the previous version showed
 * "Error loading karssandboxes"): extending `KubeObject` with bare
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
import { useTheme } from "@mui/material/styles";
import * as React from "react";

const GROUP = "kars.azure.com";
const VERSION = "v1alpha1";

interface CrdDescriptor {
  plural: string;
  singular: string;
  kind: string;
  label: string;
  phaseField?: string;
}

const KARS_CRDS: CrdDescriptor[] = [
  { plural: "karssandboxes",    singular: "karssandbox",    kind: "KarsSandbox",     label: "Sandboxes",         phaseField: "phase" },
  { plural: "inferencepolicies", singular: "inferencepolicy", kind: "InferencePolicy", label: "Inference Policies" },
  { plural: "karsmemories",     singular: "karsmemory",     kind: "KarsMemory",      label: "Memories",          phaseField: "phase" },
  { plural: "mcpservers",       singular: "mcpserver",      kind: "McpServer",       label: "MCP Servers",       phaseField: "phase" },
  { plural: "a2aagents",        singular: "a2aagent",       kind: "A2AAgent",        label: "A2A Agents",        phaseField: "phase" },
  { plural: "toolpolicies",     singular: "toolpolicy",     kind: "ToolPolicy",      label: "Tool Policies" },
  { plural: "trustgraphs",      singular: "trustgraph",     kind: "TrustGraph",      label: "Trust Graphs" },
  { plural: "karspairings",     singular: "karspairing",    kind: "KarsPairing",     label: "Pairings" },
  { plural: "karsevals",        singular: "karseval",       kind: "KarsEval",        label: "Evals",             phaseField: "phase" },
  { plural: "egressapprovals",  singular: "egressapproval", kind: "EgressApproval",  label: "Egress Approvals",  phaseField: "phase" },
];

const CRD_CLASSES: Record<string, KubeObjectClass> = Object.fromEntries(
  KARS_CRDS.map(c => [
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

const KarsSandboxClass = CRD_CLASSES.karssandboxes!;

// ──────────────────────────────────────────────────────────────────────
// Sidebar + routes
// ──────────────────────────────────────────────────────────────────────

registerSidebarEntry({
  parent: null,
  name: "kars",
  label: "kars",
  icon: "mdi:robot-outline",
  url: "/kars",
});

registerSidebarEntry({
  parent: "kars",
  name: "kars-overview",
  label: "Overview",
  url: "/kars",
});

registerRoute({
  path: "/kars",
  sidebar: "kars-overview",
  name: "kars-overview",
  exact: true,
  component: () => <Overview />,
});

registerSidebarEntry({
  parent: "kars",
  name: "kars-mesh",
  label: "Mesh Topology",
  url: "/kars/mesh",
});

registerRoute({
  path: "/kars/mesh",
  sidebar: "kars-mesh",
  name: "kars-mesh",
  exact: true,
  component: () => <MeshTopology />,
});

for (const crd of KARS_CRDS) {
  registerSidebarEntry({
    parent: "kars",
    name: crd.plural,
    label: crd.label,
    url: `/kars/${crd.plural}`,
  });

  registerRoute({
    path: `/kars/${crd.plural}`,
    sidebar: crd.plural,
    name: crd.plural,
    exact: true,
    component: () => <CrdList crd={crd} />,
  });

  registerRoute({
    path: `/kars/${crd.plural}/:namespace/:name`,
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

// Reasons that mean "the policy is broken; needs human attention, not patience".
// Sourced from `controller/src/status/conditions.rs` (`reason::*`) — kept in
// sync by audit (`docs/internal/crd-well-oiled-machine/honest-audit.md`).
// Distinguishing these from `Awaiting*` reasons is the difference between an
// amber chip (working as designed, just settling) and a red chip (operator
// action required). The plain phase enum can't tell them apart on its own.
const HARD_FAILURE_REASONS = new Set<string>([
  "SignatureMismatch",
  "BundleVerifyFailed",
  "AuthMisconfigured",
  "MemoryStoreMissing",
  "RuntimeAdapterMissing",
  "AdapterMissing",
  "ShapeInvalid",
  "AllowlistDrift",
  "PolicyCompileFailed",
]);

const SOFT_PENDING_REASONS = new Set<string>([
  "AwaitingRouterEnforcement",
  "AwaitingFoundryProvisioning",
  "NoSandboxesReferencing",
  "Pending",
]);

function readyReason(item: KubeObject): string | undefined {
  const conds = (getStatus(item).conditions as Array<Record<string, any>> | undefined) ?? [];
  const ready = conds.find(c => c.type === "Ready");
  return ready?.reason as string | undefined;
}

function phaseToStatus(phase: string | undefined, reason?: string): StatusKind {
  // Reason wins when present — a `Degraded` phase with reason
  // `AwaitingRouterEnforcement` is amber (transient), but a `Pending` phase
  // with reason `SignatureMismatch` is red (operator action required).
  if (reason && HARD_FAILURE_REASONS.has(reason)) return "error";
  if (reason && SOFT_PENDING_REASONS.has(reason)) return "warning";
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

function phaseChip(phase: string | undefined, reason?: string) {
  if (!phase) return <span>—</span>;
  const status = phaseToStatus(phase, reason);
  // When the reason carries actionable signal, append it as a secondary
  // muted label so operators see both at a glance. e.g.
  //   [Degraded] AllowlistDrift   — red, action required
  //   [Pending]  AwaitingRouterEnforcement — amber, settling
  const showReason = reason && (HARD_FAILURE_REASONS.has(reason) || SOFT_PENDING_REASONS.has(reason));
  return (
    <span>
      <StatusLabel status={status as any}>{phase}</StatusLabel>
      {showReason && (
        <span style={{ marginLeft: "0.4rem", fontSize: "0.85em", color: "#888" }}>
          {reason}
        </span>
      )}
    </span>
  );
}

function chipForItem(item: KubeObject, phaseField: string) {
  const phase = (getStatus(item) as any)[phaseField] as string | undefined;
  return phaseChip(phase, readyReason(item));
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
// plumbing. Mirrors the data the `kars inspect <sandbox>` CLI
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
// KarsSandbox's CR carries both `allowedEndpoints` (inline) AND
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
  // fields. KarsSandbox, McpServer, etc. don't participate in the
  // digest-echo loop yet.
  if (
    crd.plural !== "toolpolicies" &&
    crd.plural !== "inferencepolicies" &&
    crd.plural !== "karsmemories"
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
// KarsEval status panel (slice 6.4) — surfaces last-run aggregate +
// drift state + corpus reference. Pure read of fields the controller
// already writes; no router admin token in the browser.
// ──────────────────────────────────────────────────────────────────────

function KarsEvalStatusPanel({ crd, item }: { crd: CrdDescriptor; item: KubeObject }) {
  if (crd.plural !== "karsevals") {
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
    <SectionBox title="KarsEval (conformance corpus)">
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
        KarsEvals replay a signed corpus (or a builtin one) against the target
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

// Map a credentials-secret data key to a channel name. kars stores
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
    // Sandbox pod namespace is kars-<name>; credentials secret
    // lives there with the sandbox's bare name.
    const podNs = `kars-${sbName}`;
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
  const [sandboxes] = (KarsSandboxClass as any).useList() as [KubeObject[] | null];
  const [secrets] = (Secret as any).useList() as [KubeObject[] | null];
  const [inferencePolicies] = (CRD_CLASSES.inferencepolicies as any).useList() as [KubeObject[] | null];
  const [toolPolicies] = (CRD_CLASSES.toolpolicies as any).useList() as [KubeObject[] | null];
  const [memories] = (CRD_CLASSES.karsmemories as any).useList() as [KubeObject[] | null];
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
    for (const k of [`${sb.metadata?.namespace ?? ""}/${ref}`, `kars-system/${ref}`]) {
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
      <SectionBox title="kars — Operator Overview">
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
                  routeName={`karssandboxes-detail`}
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
            { label: "Phase", getter: (r: KubeObject) => phaseChip(getStatus(r).phase as string, readyReason(r)) },
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

      <TokenBudgetOverview sandboxes={sandboxes ?? []} inferencePolicies={inferencePolicies ?? []} />
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
    // (kars-system) by convention.
    const candidates = [
      `${sb.metadata?.namespace ?? ""}/${ref}`,
      `kars-system/${ref}`,
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
  if (crd.plural === "karssandboxes") {
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
      getter: (r: KubeObject) => phaseChip(getStatus(r)[crd.phaseField!] as string, readyReason(r)),
    });
  }
  columns.push({
    label: "Age",
    getter: (r: KubeObject) => formatAge(r.metadata?.creationTimestamp as string | undefined),
  });

  return (
    <SectionBox title={`kars — ${crd.label}`}>
      {items === null ? (
        <p style={{ padding: "1rem" }}>Loading…</p>
      ) : items.length === 0 ? (
        <p style={{ padding: "1rem" }}>
          No {crd.label.toLowerCase()} found. Create one with the kars CLI
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
  const match = urlParams(new RegExp(`/kars/${crd.plural}/([^/]+)/([^/]+)`));
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
            { k: "Phase", v: phaseChip(status.phase as string, readyReason(item)) },
            { k: "Created", v: item.metadata?.creationTimestamp ?? "—" },
            { k: "UID", v: item.metadata?.uid ?? "—" },
          ]}
          columns={[
            { label: "Field", getter: (r: any) => r.k },
            { label: "Value", getter: (r: any) => r.v },
          ]}
        />
      </SectionBox>

      {crd.plural === "karssandboxes" && <SandboxExtras item={item} />}
      {crd.plural === "inferencepolicies" && <InferencePolicyMetricsCard policyName={item.metadata.name} />}
      {crd.plural === "toolpolicies" && <ToolPolicyMetricsCard policyName={item.metadata.name} />}
      {crd.plural === "trustgraphs" && <TrustGraphMetricsCard />}

      <AllowlistDriftBanner item={item} />

      <RouterPolicyStatusPanel crd={crd} item={item} />

      <KarsEvalStatusPanel crd={crd} item={item} />

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
// KarsSandbox-specific sections: network policy, channels, related refs.
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
  // EgressApprovals live in the same namespace as the KarsSandbox and
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
        <code>status.expiresAt</code>; revoke early with <code>kars egress revoke</code>.
      </p>
    </SectionBox>
  );
}

function McpServerFleetCard({ refs }: { refs: Array<{ name?: string }> }) {
  // Per-MCP-server live status on the KarsSandbox detail. Each server gets a
  // row showing phase + reason chip, JWKS digest (router-echoed), and tool
  // count. Previously the operator had to click each server to see drift —
  // now you see the whole referenced fleet at a glance.
  const [servers] = (CRD_CLASSES.mcpservers as any).useList() as [
    KubeObject[] | null,
  ];
  if (refs.length === 0) return null;
  const byName = new Map<string, KubeObject>();
  (servers ?? []).forEach(s => {
    const n = s.metadata?.name;
    if (n) byName.set(n, s);
  });
  const rows = refs.map(r => {
    const obj = r.name ? byName.get(r.name) : undefined;
    const status = obj ? getStatus(obj) : {};
    const spec = obj ? getSpec(obj) : {};
    const tools = Array.isArray(spec.tools) ? spec.tools.length : (status.toolCount ?? 0);
    return {
      name: r.name ?? "—",
      phase: (status.phase as string | undefined),
      reason: obj ? readyReason(obj) : undefined,
      digest: (status.jwksDigest as string | undefined) ?? (status.bundleDigest as string | undefined),
      tools,
      missing: !obj,
    };
  });
  return (
    <SectionBox title={`MCP Servers (${rows.length})`}>
      <SimpleTable
        data={rows}
        columns={[
          {
            label: "Name",
            getter: (r: any) =>
              r.missing ? (
                <span>
                  {r.name} <StatusLabel status="error">MISSING</StatusLabel>
                </span>
              ) : (
                <Link
                  routeName="mcpservers-detail"
                  params={{ namespace: "kars-system", name: r.name }}
                >
                  {r.name}
                </Link>
              ),
          },
          { label: "Phase", getter: (r: any) => phaseChip(r.phase, r.reason) },
          { label: "Tools", getter: (r: any) => r.tools },
          { label: "JWKS digest", getter: (r: any) => shortDigest(r.digest) },
        ]}
      />
    </SectionBox>
  );
}

function SandboxExtras({ item }: { item: KubeObject }) {
  const spec = getSpec(item);
  const status = getStatus(item);
  const namespace = item.metadata?.namespace ?? "";
  const name = item.metadata?.name ?? "";

  // Sandbox pod ns is kars-<name>; the credentials Secret lives
  // there with name "<sandbox>-credentials". Channel state lives
  // there (TELEGRAM_BOT_TOKEN, etc.), not in spec.
  const podNs = `kars-${name}`;
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
            <code>kars credentials set telegram-token …</code> +{" "}
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
            ...(memoryRef ? [{ kind: "KarsMemory", name: memoryRef, route: "karsmemories-detail" }] : []),
            ...mcpRefs.map(r => ({ kind: "McpServer", name: r.name ?? "", route: "mcpservers-detail" })),
          ]}
          columns={[
            { label: "Kind", getter: (r: any) => r.kind },
            {
              label: "Name",
              getter: (r: any) =>
                r.name ? (
                  <Link routeName={r.route} params={{ namespace: "kars-system", name: r.name }}>
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

      <McpServerFleetCard refs={mcpRefs} />

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

      <SandboxBudgetCard sandboxName={name} inferenceRefName={spec.inferenceRef?.name} />
      <SandboxMetricsCard sandboxName={name} />
    </>
  );
}

// ──────────────────────────────────────────────────────────────────────
// Grafana iframe embed for per-sandbox metrics.
// Anonymous viewer + allow_embedding enabled on the kube-prometheus-stack
// Grafana so the panel renders without auth. The Grafana base URL is
// configurable via window.KARS_GRAFANA_URL — defaults to the local
// port-forward used during dev (http://127.0.0.1:3000).
// ──────────────────────────────────────────────────────────────────────
function SandboxMetricsCard({ sandboxName }: { sandboxName: string }) {
  const theme = useTheme();
  const grafanaTheme = theme.palette.mode === "dark" ? "dark" : "light";
  const grafanaBase =
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (typeof window !== "undefined" && (window as any).KARS_GRAFANA_URL) ||
    "http://127.0.0.1:3000";
  const url =
    `${grafanaBase}/d/kars-ops?kiosk=tv&refresh=10s&theme=${grafanaTheme}` +
    `&var-sandbox=${encodeURIComponent(sandboxName)}`;
  return (
    <SectionBox title={`Metrics (Grafana) — ${sandboxName}`}>
      <div style={{ marginBottom: 8 }}>
        <a href={url} target="_blank" rel="noopener noreferrer">
          Open full dashboard in Grafana ↗
        </a>
      </div>
      <iframe
        src={url}
        title={`Grafana metrics for ${sandboxName}`}
        style={{ width: "100%", height: "720px", border: "0" }}
        loading="lazy"
      />
    </SectionBox>
  );
}

// ──────────────────────────────────────────────────────────────────────
// MeshTopology — native SVG visualization of the AGT mesh with
// parent→sub-agent hierarchy. Uses KarsSandbox CRs (label
// `kars.azure.com/parent`) to build the tree, and Prometheus
// queries to overlay live token/activity stats and inter-agent
// communication. Configurable via window.KARS_PROMETHEUS_URL.
// ──────────────────────────────────────────────────────────────────────
interface MeshNodeData {
  name: string;
  parent: string;     // "" if controller
  knownPeers: number;
  meshSent: number;       // mesh messages sent (5m increase)
  meshRecv: number;       // mesh messages received (5m increase)
  meshSentLife: number;   // mesh messages sent (lifetime counter value)
  meshRecvLife: number;   // mesh messages received (lifetime counter value)
}

async function promQuery(base: string, q: string): Promise<{metric: Record<string,string>, value: number}[]> {
  const url = `${base}/api/v1/query?query=${encodeURIComponent(q)}`;
  const r = await fetch(url);
  if (!r.ok) throw new Error(`prom ${r.status}`);
  const j = await r.json();
  return (j?.data?.result || []).map((row: {metric: Record<string,string>, value: [number, string]}) => ({
    metric: row.metric || {},
    value: Number(row.value?.[1] || 0),
  }));
}

function usePromBase(): string {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (typeof window !== "undefined" && (window as any).KARS_PROMETHEUS_URL) || "http://127.0.0.1:19091";
}

function usePromPoll<T>(initial: T, loader: (base: string) => Promise<T>, intervalMs = 5000): { data: T; err: string } {
  const base = usePromBase();
  const [data, setData] = React.useState<T>(initial);
  const [err, setErr] = React.useState("");
  const [tick, setTick] = React.useState(0);
  React.useEffect(() => {
    let cancelled = false;
    loader(base).then((d) => { if (!cancelled) { setData(d); setErr(""); } }).catch((e) => { if (!cancelled) setErr(String(e)); });
    const id = setInterval(() => setTick((t) => t + 1), intervalMs);
    return () => { cancelled = true; clearInterval(id); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base, tick]);
  return { data, err };
}

function MeshTopology() {
  const theme = useTheme();
  const isDark = theme.palette.mode === "dark";
  const bg = isDark ? "#1e1e1e" : "#fafafa";
  const muted = isDark ? "#aaa" : "#555";
  const stroke = isDark ? "#cfd8dc" : "#37474f";
  const textOnNode = "#fff";

  const [sandboxes] = (KarsSandboxClass as any).useList() as [KubeObject[] | null];

  const { data: live, err } = usePromPoll(
    { peers: [] as { metric: Record<string,string>; value: number }[],
      sentLife: [] as { metric: Record<string,string>; value: number }[],
      recvLife: [] as { metric: Record<string,string>; value: number }[],
      sentRate: [] as { metric: Record<string,string>; value: number }[],
      recvRate: [] as { metric: Record<string,string>; value: number }[],
      relayConn: 0,
      relayRouted: 0,
      relayStored: 0,
      relayDelivered: 0,
      relayMsgsPerSec: 0 },
    async (base) => {
      const [peers, sentLife, recvLife, sentRate, recvRate, relayConn, relayRouted, relayStored, relayDelivered, relayMsgs] = await Promise.all([
        promQuery(base, 'kars_agt_known_agents'),
        promQuery(base, 'kars_mesh_messages_sent_total'),
        promQuery(base, 'kars_mesh_messages_received_total'),
        promQuery(base, 'sum by (sandbox) (increase(kars_mesh_messages_sent_total[5m]))'),
        promQuery(base, 'sum by (sandbox) (increase(kars_mesh_messages_received_total[5m]))'),
        promQuery(base, 'sum(agentmesh_relay_connected_agents)'),
        promQuery(base, 'sum(agentmesh_relay_messages_routed_total)'),
        promQuery(base, 'sum(agentmesh_relay_messages_stored_total)'),
        promQuery(base, 'sum(agentmesh_relay_messages_delivered_total)'),
        promQuery(base, 'sum(rate(agentmesh_relay_messages_routed_total[5m]))'),
      ]);
      return {
        peers, sentLife, recvLife, sentRate, recvRate,
        relayConn: relayConn[0]?.value || 0,
        relayRouted: relayRouted[0]?.value || 0,
        relayStored: relayStored[0]?.value || 0,
        relayDelivered: relayDelivered[0]?.value || 0,
        relayMsgsPerSec: relayMsgs[0]?.value || 0,
      };
    }
  );

  // Index Prometheus results by sandbox name.
  const peerByName = Object.fromEntries(live.peers.map((p) => [p.metric.sandbox || "", p.value]));
  const sentLifeByName = Object.fromEntries(live.sentLife.map((p) => [p.metric.sandbox || "", p.value]));
  const recvLifeByName = Object.fromEntries(live.recvLife.map((p) => [p.metric.sandbox || "", p.value]));
  const sentRateByName = Object.fromEntries(live.sentRate.map((p) => [p.metric.sandbox || "", p.value]));
  const recvRateByName = Object.fromEntries(live.recvRate.map((p) => [p.metric.sandbox || "", p.value]));

  // Build hierarchy from CR labels.
  const nodes: MeshNodeData[] = (sandboxes || []).map((sb) => {
    const name = sb.metadata.name;
    const parent = (sb.metadata.labels || {})["kars.azure.com/parent"] || "";
    return {
      name,
      parent,
      knownPeers: peerByName[name] || 0,
      meshSent: sentRateByName[name] || 0,
      meshRecv: recvRateByName[name] || 0,
      meshSentLife: sentLifeByName[name] || 0,
      meshRecvLife: recvLifeByName[name] || 0,
    };
  });

  const controllers = nodes.filter((n) => !n.parent).sort((a, b) => a.name.localeCompare(b.name));
  const childrenByParent: Record<string, MeshNodeData[]> = {};
  for (const n of nodes) {
    if (!n.parent) continue;
    childrenByParent[n.parent] = childrenByParent[n.parent] || [];
    childrenByParent[n.parent].push(n);
  }

  // Layout: relay at top center, controllers in a row below, each controller's
  // children fanned out below it.
  const W = 1100;
  const ctrlGap = Math.max(220, W / Math.max(1, controllers.length));
  const relayX = W / 2;
  const relayY = 70;
  const ctrlRowY = 220;
  const childRowY = 400;
  const nodeR = 36;
  const relayR = 50;

  // Pre-compute positions.
  const ctrlPos: Record<string, { x: number; y: number; n: MeshNodeData }> = {};
  controllers.forEach((c, i) => {
    const x = ctrlGap * (i + 0.5) + (W - ctrlGap * controllers.length) / 2;
    ctrlPos[c.name] = { x, y: ctrlRowY, n: c };
  });

  const childPos: Record<string, { x: number; y: number; n: MeshNodeData; parent: string }> = {};
  for (const c of controllers) {
    const kids = childrenByParent[c.name] || [];
    const parentX = ctrlPos[c.name].x;
    const kidGap = 130;
    kids.forEach((k, i) => {
      const offset = (i - (kids.length - 1) / 2) * kidGap;
      childPos[k.name] = { x: parentX + offset, y: childRowY, n: k, parent: c.name };
    });
  }

  // Orphans (parent missing from CR list).
  const orphans = nodes.filter((n) => n.parent && !ctrlPos[n.parent]);

  // Drive sizing / edge styling off mesh message activity instead of tokens.
  const traffic = (n: MeshNodeData) => n.meshSent + n.meshRecv;
  const maxTraffic = Math.max(0.001, ...nodes.map(traffic));
  const maxLife = Math.max(1, ...nodes.map((n) => n.meshSentLife + n.meshRecvLife));
  const H = (orphans.length > 0 ? 600 : 520);

  function nodeFill(n: MeshNodeData): string {
    const t = traffic(n);
    if (t > 5) return "#43a047";
    if (t > 0.5) return "#9ccc65";
    if (t > 0) return "#ffd54f";
    if (n.knownPeers > 0) return "#90caf9";
    return isDark ? "#555" : "#bdbdbd";
  }
  function nodeSize(n: MeshNodeData): number {
    return nodeR + Math.min(14, ((n.meshSentLife + n.meshRecvLife) / maxLife) * 14);
  }
  function edgeWidth(t: number): number {
    return 1 + (t / maxTraffic) * 5;
  }
  function edgeOpacity(t: number): number {
    return 0.3 + (t / maxTraffic) * 0.7;
  }
  function pulseDur(t: number): number {
    return t > 0 ? Math.max(0.6, 3 - (t / maxTraffic) * 2.4) : 0;
  }

  return (
    <SectionBox title="🕸️ Mesh Topology (live)">
      <div style={{ marginBottom: 12, fontSize: 13, color: muted }}>
        Tree view of the AGT mesh: AGT Relay (top), controllers (mid row), sub-agents (bottom row).
        Polled from Prometheus every 5s. Edge thickness & pulse speed ∝ mesh messages
        in/out (5m). Node size ∝ lifetime mesh-message volume. <b>children</b> = sub-agent
        CRs labeled <code>kars.azure.com/parent=&lt;name&gt;</code>; <b>trust</b> = peers in
        this router's local AGT trust graph (only populated after live traffic; resets on pod restart).
        {err && <div style={{ color: "#ef5350", marginTop: 6 }}>Prometheus unreachable: {err} (configure window.KARS_PROMETHEUS_URL)</div>}
      </div>
      <div style={{ display: "flex", gap: 16, marginBottom: 12, flexWrap: "wrap" }}>
        <StatusLabel status="">🔗 Relay connected: <b>{live.relayConn}</b></StatusLabel>
        <StatusLabel status="">📨 Relay msg/s (5m): <b>{live.relayMsgsPerSec.toFixed(2)}</b></StatusLabel>
        <StatusLabel status="">📬 Routed total: <b>{Math.round(live.relayRouted).toLocaleString()}</b></StatusLabel>
        <StatusLabel status="">📦 Stored (offline): <b>{Math.round(live.relayStored).toLocaleString()}</b></StatusLabel>
        <StatusLabel status="">✉️ Delivered (after reconnect): <b>{Math.round(live.relayDelivered).toLocaleString()}</b></StatusLabel>
        <StatusLabel status="">🤖 Sandboxes: <b>{nodes.length}</b></StatusLabel>
        <StatusLabel status="">👨‍👩‍👧 Controllers: <b>{controllers.length}</b></StatusLabel>
        <StatusLabel status="">🧒 Sub-agents: <b>{Object.keys(childPos).length}</b></StatusLabel>
      </div>
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", maxWidth: W, background: bg, borderRadius: 8 }}>
        <defs>
          <radialGradient id="relayGrad" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="#fff59d" />
            <stop offset="100%" stopColor="#fbc02d" />
          </radialGradient>
          <filter id="glow" x="-50%" y="-50%" width="200%" height="200%">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge><feMergeNode in="blur" /><feMergeNode in="SourceGraphic" /></feMerge>
          </filter>
        </defs>

        {/* relay → controller edges */}
        {controllers.map((c) => {
          const p = ctrlPos[c.name];
          const t = traffic(c);
          return (
            <g key={`r-${c.name}`}>
              <line x1={relayX} y1={relayY} x2={p.x} y2={p.y}
                stroke="#42a5f5" strokeWidth={edgeWidth(t)}
                strokeOpacity={edgeOpacity(t)} />
              {/* Outbound pulse: relay → sandbox (recv direction for the sandbox) */}
              {c.meshRecv > 0 && (
                <circle r="4" fill="#81d4fa" filter="url(#glow)">
                  <animateMotion dur={`${pulseDur(c.meshRecv)}s`} repeatCount="indefinite"
                    path={`M${relayX},${relayY} L${p.x},${p.y}`} />
                </circle>
              )}
              {/* Inbound pulse: sandbox → relay (sent direction) */}
              {c.meshSent > 0 && (
                <circle r="4" fill="#ffeb3b" filter="url(#glow)">
                  <animateMotion dur={`${pulseDur(c.meshSent)}s`} repeatCount="indefinite"
                    path={`M${p.x},${p.y} L${relayX},${relayY}`} />
                </circle>
              )}
              <text x={(relayX + p.x) / 2} y={(relayY + p.y) / 2 - 4} textAnchor="middle"
                fontSize="10" fill={muted} style={{ pointerEvents: "none" }}>
                ↑{Math.round(c.meshSent * 60 / 5) || 0} ↓{Math.round(c.meshRecv * 60 / 5) || 0} /min
              </text>
            </g>
          );
        })}

        {/* controller → child edges (logical parent relationship; mesh traffic
            still flows via the relay, so we pulse based on the child's traffic) */}
        {Object.values(childPos).map((cp) => {
          const par = ctrlPos[cp.parent];
          if (!par) return null;
          const t = traffic(cp.n);
          return (
            <g key={`pc-${cp.n.name}`}>
              <line x1={par.x} y1={par.y} x2={cp.x} y2={cp.y}
                stroke="#7e57c2" strokeWidth={edgeWidth(t)}
                strokeOpacity={edgeOpacity(t)} strokeDasharray="6,4" />
              {pulseDur(t) > 0 && (
                <circle r="3" fill="#ce93d8" filter="url(#glow)">
                  <animateMotion dur={`${pulseDur(t)}s`} repeatCount="indefinite"
                    path={`M${par.x},${par.y} L${cp.x},${cp.y}`} />
                </circle>
              )}
            </g>
          );
        })}

        {/* AGT Relay (top) */}
        <g>
          <circle cx={relayX} cy={relayY} r={relayR} fill="url(#relayGrad)" stroke="#f57f17" strokeWidth="3" filter="url(#glow)" />
          <text x={relayX} y={relayY - 8} textAnchor="middle" fontSize="13" fontWeight="bold" fill="#212121">AGT Relay</text>
          <text x={relayX} y={relayY + 6} textAnchor="middle" fontSize="10" fill="#212121">{live.relayConn} connected</text>
          <text x={relayX} y={relayY + 20} textAnchor="middle" fontSize="10" fill="#212121">{live.relayMsgsPerSec.toFixed(2)} msg/s</text>
          <text x={relayX} y={relayY + 34} textAnchor="middle" fontSize="9" fill="#212121">
            {Math.round(live.relayRouted).toLocaleString()} routed
          </text>
        </g>

        {/* Controllers */}
        {controllers.map((c) => {
          const p = ctrlPos[c.name];
          const sz = nodeSize(c);
          const childCount = (childrenByParent[c.name] || []).length;
          return (
            <g key={`c-${c.name}`}>
              <circle cx={p.x} cy={p.y} r={sz} fill={nodeFill(c)} stroke={stroke} strokeWidth="2.5" />
              <text x={p.x} y={p.y - 8} textAnchor="middle" fontSize="13" fontWeight="bold" fill={textOnNode}>{c.name}</text>
              <text x={p.x} y={p.y + 4} textAnchor="middle" fontSize="9" fill={textOnNode}>controller</text>
              <text x={p.x} y={p.y + 18} textAnchor="middle" fontSize="10" fill={textOnNode}>
                ↑{Math.round(c.meshSentLife).toLocaleString()} ↓{Math.round(c.meshRecvLife).toLocaleString()}
              </text>
              <text x={p.x} y={p.y + 30} textAnchor="middle" fontSize="9" fill={textOnNode}>
                {childCount} child{childCount === 1 ? "" : "ren"} · {c.knownPeers} trust
              </text>
            </g>
          );
        })}

        {/* Sub-agents */}
        {Object.values(childPos).map((cp) => {
          const n = cp.n;
          const sz = nodeSize(n) - 6;
          return (
            <g key={`s-${n.name}`}>
              <circle cx={cp.x} cy={cp.y} r={sz} fill={nodeFill(n)} stroke={stroke} strokeWidth="1.5" />
              <text x={cp.x} y={cp.y - 6} textAnchor="middle" fontSize="11" fontWeight="bold" fill={textOnNode}>{n.name}</text>
              <text x={cp.x} y={cp.y + 6} textAnchor="middle" fontSize="9" fill={textOnNode}>sub-agent</text>
              <text x={cp.x} y={cp.y + 20} textAnchor="middle" fontSize="10" fill={textOnNode}>
                ↑{Math.round(n.meshSentLife).toLocaleString()} ↓{Math.round(n.meshRecvLife).toLocaleString()}
              </text>
            </g>
          );
        })}

        {/* Orphan sub-agents (parent missing from CR list) */}
        {orphans.length > 0 && (
          <g>
            <text x={W / 2} y={H - 80} textAnchor="middle" fontSize="11" fill={muted}>— Orphan sub-agents (parent CR not found) —</text>
            {orphans.map((o, i) => {
              const x = (W / (orphans.length + 1)) * (i + 1);
              return (
                <g key={`o-${o.name}`}>
                  <circle cx={x} cy={H - 40} r={nodeR - 8} fill={isDark ? "#616161" : "#9e9e9e"} stroke={isDark ? "#9e9e9e" : "#616161"} strokeWidth="1.5" strokeDasharray="3,3" />
                  <text x={x} y={H - 44} textAnchor="middle" fontSize="11" fontWeight="bold" fill={textOnNode}>{o.name}</text>
                  <text x={x} y={H - 30} textAnchor="middle" fontSize="9" fill={textOnNode}>parent:{o.parent}</text>
                </g>
              );
            })}
          </g>
        )}
      </svg>
      <div style={{ marginTop: 12 }}>
        <SimpleTable
          data={nodes
            .map((n) => ({
              name: n.name,
              kind: n.parent ? `sub-agent ← ${n.parent}` : "controller",
              peers: n.knownPeers,
              sent5m: Math.round(n.meshSent),
              recv5m: Math.round(n.meshRecv),
              sentLife: Math.round(n.meshSentLife),
              recvLife: Math.round(n.meshRecvLife),
            }))
            .sort((a, b) => (b.sent5m + b.recv5m) - (a.sent5m + a.recv5m))}
          columns={[
            { label: "Sandbox", getter: (r: { name: string }) => r.name },
            { label: "Role", getter: (r: { kind: string }) => r.kind },
            { label: "Peers", getter: (r: { peers: number }) => r.peers },
            { label: "↑ Sent (5m)", getter: (r: { sent5m: number }) => r.sent5m },
            { label: "↓ Recv (5m)", getter: (r: { recv5m: number }) => r.recv5m },
            { label: "↑ Sent (life)", getter: (r: { sentLife: number }) => r.sentLife.toLocaleString() },
            { label: "↓ Recv (life)", getter: (r: { recvLife: number }) => r.recvLife.toLocaleString() },
          ]}
        />
      </div>
    </SectionBox>
  );
}

// ──────────────────────────────────────────────────────────────────────
// Per-CRD Prometheus metric cards. These embed a focused Grafana
// dashboard view (filtered by the relevant entity) instead of duplicating
// the Grafana queries client-side. The router metrics
// `kars_inference_*` and `kars_agt_policy_evaluations_total`
// surface model/policy/decision labels that map naturally to the
// InferencePolicy and ToolPolicy CRDs.
// ──────────────────────────────────────────────────────────────────────
function grafanaBaseUrl(): string {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (typeof window !== "undefined" && (window as any).KARS_GRAFANA_URL) || "http://127.0.0.1:3000";
}

function InferencePolicyMetricsCard({ policyName }: { policyName: string }) {
  const theme = useTheme();
  const grafanaTheme = theme.palette.mode === "dark" ? "dark" : "light";
  const muted = theme.palette.text.secondary;
  const { data, err } = usePromPoll(
    { byModel: [] as { metric: Record<string,string>; value: number }[],
      bySandbox: [] as { metric: Record<string,string>; value: number }[],
      reqRate: [] as { metric: Record<string,string>; value: number }[],
      latency: 0 },
    async (base) => {
      const [byModel, bySandbox, reqRate, latency] = await Promise.all([
        promQuery(base, 'sum by (model, direction) (increase(kars_tokens_total[1h]))'),
        promQuery(base, 'sum by (sandbox) (increase(kars_tokens_total[1h]))'),
        promQuery(base, 'sum by (model, status) (rate(kars_inference_requests_total[5m]))'),
        promQuery(base, 'histogram_quantile(0.95, sum by (le) (rate(kars_inference_latency_seconds_bucket[5m])))'),
      ]);
      return { byModel, bySandbox, reqRate, latency: latency[0]?.value || 0 };
    }
  );
  const url = `${grafanaBaseUrl()}/d/kars-ops?kiosk=tv&refresh=10s&theme=${grafanaTheme}`;
  const modelRows = data.byModel.map((r) => ({
    model: r.metric.model || "?",
    direction: r.metric.direction || "?",
    tokens: Math.round(r.value).toLocaleString(),
  })).sort((a, b) => Number(b.tokens.replace(/,/g,"")) - Number(a.tokens.replace(/,/g,"")));
  const sandboxRows = data.bySandbox.map((r) => ({
    sandbox: r.metric.sandbox || "?",
    tokens: Math.round(r.value).toLocaleString(),
  })).sort((a, b) => Number(b.tokens.replace(/,/g,"")) - Number(a.tokens.replace(/,/g,"")));
  return (
    <SectionBox title={`📊 Inference Metrics (policy: ${policyName})`}>
      <div style={{ marginBottom: 8, fontSize: 13, color: muted }}>
        Live aggregates across all sandboxes routed through this policy class. {err && <span style={{color:"#ef5350"}}>{err}</span>}
      </div>
      <div style={{ display: "flex", gap: 12, marginBottom: 12, flexWrap: "wrap" }}>
        <StatusLabel status="">⏱ p95 latency (5m): <b>{(data.latency * 1000).toFixed(0)} ms</b></StatusLabel>
        <StatusLabel status="">🧮 Models active: <b>{new Set(data.byModel.map((r) => r.metric.model)).size}</b></StatusLabel>
        <StatusLabel status="">🤖 Sandboxes consuming: <b>{sandboxRows.length}</b></StatusLabel>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
        <div>
          <h4 style={{ margin: "4px 0" }}>Tokens by model (1h)</h4>
          <SimpleTable data={modelRows} columns={[
            { label: "Model", getter: (r: { model: string }) => r.model },
            { label: "Dir", getter: (r: { direction: string }) => r.direction },
            { label: "Tokens", getter: (r: { tokens: string }) => r.tokens },
          ]} />
        </div>
        <div>
          <h4 style={{ margin: "4px 0" }}>Top consumers (1h)</h4>
          <SimpleTable data={sandboxRows.slice(0, 10)} columns={[
            { label: "Sandbox", getter: (r: { sandbox: string }) => r.sandbox },
            { label: "Tokens", getter: (r: { tokens: string }) => r.tokens },
          ]} />
        </div>
      </div>
      <div style={{ marginTop: 12 }}>
        <a href={url} target="_blank" rel="noopener noreferrer">Open full Grafana dashboard ↗</a>
      </div>
    </SectionBox>
  );
}

function ToolPolicyMetricsCard({ policyName }: { policyName: string }) {
  const theme = useTheme();
  const muted = theme.palette.text.secondary;
  const { data, err } = usePromPoll(
    { decisions: [] as { metric: Record<string,string>; value: number }[],
      bySandbox: [] as { metric: Record<string,string>; value: number }[],
      latencyP95: 0 },
    async (base) => {
      const [decisions, bySandbox, latP95] = await Promise.all([
        promQuery(base, 'sum by (decision) (increase(kars_agt_policy_evaluations_total[1h]))'),
        promQuery(base, 'sum by (sandbox, decision) (increase(kars_agt_policy_evaluations_total[1h]))'),
        promQuery(base, 'histogram_quantile(0.95, sum by (le) (rate(kars_agt_eval_latency_seconds_bucket[5m])))'),
      ]);
      return { decisions, bySandbox, latencyP95: latP95[0]?.value || 0 };
    }
  );
  const total = data.decisions.reduce((s, r) => s + r.value, 0) || 1;
  const decisionRows = data.decisions.map((r) => ({
    decision: r.metric.decision || "?",
    count: Math.round(r.value).toLocaleString(),
    pct: ((r.value / total) * 100).toFixed(1) + "%",
  }));
  const sandboxRows = data.bySandbox.map((r) => ({
    sandbox: r.metric.sandbox || "?",
    decision: r.metric.decision || "?",
    count: Math.round(r.value).toLocaleString(),
  })).sort((a, b) => Number(b.count.replace(/,/g,"")) - Number(a.count.replace(/,/g,"")));
  return (
    <SectionBox title={`🛡️ Policy Evaluations (policy: ${policyName})`}>
      <div style={{ marginBottom: 8, fontSize: 13, color: muted }}>
        AGT policy evaluation counters scoped to all sandboxes referencing this policy. {err && <span style={{color:"#ef5350"}}>{err}</span>}
      </div>
      <div style={{ display: "flex", gap: 12, marginBottom: 12, flexWrap: "wrap" }}>
        <StatusLabel status="">⏱ p95 eval latency (5m): <b>{(data.latencyP95 * 1e6).toFixed(0)} µs</b></StatusLabel>
        <StatusLabel status="">📊 Total evals (1h): <b>{Math.round(total).toLocaleString()}</b></StatusLabel>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 2fr", gap: 16 }}>
        <div>
          <h4 style={{ margin: "4px 0" }}>Decision mix (1h)</h4>
          <SimpleTable data={decisionRows} columns={[
            { label: "Decision", getter: (r: { decision: string }) => r.decision },
            { label: "Count", getter: (r: { count: string }) => r.count },
            { label: "Share", getter: (r: { pct: string }) => r.pct },
          ]} />
        </div>
        <div>
          <h4 style={{ margin: "4px 0" }}>Top deniers/allowers (1h)</h4>
          <SimpleTable data={sandboxRows.slice(0, 15)} columns={[
            { label: "Sandbox", getter: (r: { sandbox: string }) => r.sandbox },
            { label: "Decision", getter: (r: { decision: string }) => r.decision },
            { label: "Count", getter: (r: { count: string }) => r.count },
          ]} />
        </div>
      </div>
    </SectionBox>
  );
}

function TrustGraphMetricsCard() {
  const theme = useTheme();
  const muted = theme.palette.text.secondary;
  const { data, err } = usePromPoll(
    { peers: [] as { metric: Record<string,string>; value: number }[],
      auditEntries: [] as { metric: Record<string,string>; value: number }[],
      bundleHealth: [] as { metric: Record<string,string>; value: number }[] },
    async (base) => {
      const [peers, audit, bundle] = await Promise.all([
        promQuery(base, 'kars_agt_known_agents'),
        promQuery(base, 'kars_agt_audit_entries_total'),
        promQuery(base, 'kars_policy_bundle_healthy'),
      ]);
      return { peers, auditEntries: audit, bundleHealth: bundle };
    }
  );
  const peerRows = data.peers.map((r) => ({
    sandbox: r.metric.sandbox || "?",
    knownPeers: r.value,
  })).sort((a, b) => b.knownPeers - a.knownPeers);
  const totalPeers = data.peers.reduce((s, r) => s + r.value, 0);
  const totalAudit = data.auditEntries.reduce((s, r) => s + r.value, 0);
  return (
    <SectionBox title="🔐 Trust Graph Metrics">
      <div style={{ marginBottom: 8, fontSize: 13, color: muted }}>
        AGT trust graph: peers known per sandbox + tamper-evident audit log size. {err && <span style={{color:"#ef5350"}}>{err}</span>}
      </div>
      <div style={{ display: "flex", gap: 12, marginBottom: 12, flexWrap: "wrap" }}>
        <StatusLabel status="">🤝 Total known peers: <b>{totalPeers}</b></StatusLabel>
        <StatusLabel status="">📜 Audit entries: <b>{Math.round(totalAudit).toLocaleString()}</b></StatusLabel>
        <StatusLabel status="">📦 Healthy bundles: <b>{data.bundleHealth.filter((r) => r.value > 0).length}/{data.bundleHealth.length}</b></StatusLabel>
      </div>
      <SimpleTable data={peerRows} columns={[
        { label: "Sandbox", getter: (r: { sandbox: string }) => r.sandbox },
        { label: "Known peers", getter: (r: { knownPeers: number }) => r.knownPeers },
      ]} />
    </SectionBox>
  );
}

// ──────────────────────────────────────────────────────────────────────
// Token budget panels — overview-wide aggregate + per-sandbox utilization.
// Budgets live on InferencePolicy.spec.tokenBudget.dailyTokens; consumption
// is tracked by the router metric `kars_tokens_total{sandbox,direction}`.
// Each sandbox references its policy via spec.inferenceRef.name (same ns).
// ──────────────────────────────────────────────────────────────────────
function utilizationTone(pct: number): StatusKind {
  if (pct >= 90) return "error";
  if (pct >= 70) return "warning";
  if (pct > 0) return "success";
  return "";
}

function formatTokens(n: number): string {
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return Math.round(n).toLocaleString();
}

function BudgetBar({ used, total, height = 14 }: { used: number; total: number; height?: number }) {
  const theme = useTheme();
  const isDark = theme.palette.mode === "dark";
  const trackBg = isDark ? "#333" : "#eee";
  const textColor = isDark ? "#eee" : "#333";
  const pct = total > 0 ? Math.min(100, (used / total) * 100) : 0;
  const color = pct >= 90 ? "#c62828" : pct >= 70 ? "#ef6c00" : "#2e7d32";
  return (
    <div style={{ background: trackBg, borderRadius: 4, height, overflow: "hidden", position: "relative" }}>
      <div style={{ background: color, height: "100%", width: `${pct}%`, transition: "width .3s ease" }} />
      <div style={{ position: "absolute", inset: 0, display: "flex", alignItems: "center", justifyContent: "center",
        fontSize: 11, fontWeight: 600, color: pct > 50 ? "#fff" : textColor }}>
        {pct.toFixed(1)}%
      </div>
    </div>
  );
}

function TokenBudgetOverview({ sandboxes, inferencePolicies }: { sandboxes: KubeObject[]; inferencePolicies: KubeObject[] }) {
  const theme = useTheme();
  const muted = theme.palette.text.secondary;
  // Per-sandbox consumption from Prometheus (24h window aligns with dailyTokens budget).
  const { data, err } = usePromPoll(
    [] as { metric: Record<string,string>; value: number }[],
    async (base) => promQuery(base, 'sum by (sandbox) (increase(kars_tokens_total[24h]))'),
    10000
  );
  const consumedBySandbox: Record<string, number> = {};
  for (const row of data) consumedBySandbox[row.metric.sandbox || "?"] = row.value;

  // Resolve each sandbox → its InferencePolicy daily budget.
  const ipByName: Record<string, KubeObject> = {};
  for (const ip of inferencePolicies) ipByName[ip.metadata.name] = ip;

  const rows = sandboxes.map((sb) => {
    const spec = (sb as any).jsonData?.spec || (sb as any).spec || {};
    const refName: string = spec.inferenceRef?.name || "";
    const ip = ipByName[refName];
    const dailyBudget = ((ip as any)?.jsonData?.spec || (ip as any)?.spec || {})?.tokenBudget?.dailyTokens || 0;
    const used = consumedBySandbox[sb.metadata.name] || 0;
    return {
      name: sb.metadata.name,
      policy: refName || "—",
      budget: dailyBudget,
      used,
      pct: dailyBudget > 0 ? (used / dailyBudget) * 100 : 0,
    };
  });

  const fleetBudget = rows.reduce((s, r) => s + r.budget, 0);
  const fleetUsed = rows.reduce((s, r) => s + r.used, 0);
  const fleetPct = fleetBudget > 0 ? (fleetUsed / fleetBudget) * 100 : 0;
  const atRisk = rows.filter((r) => r.pct >= 70).length;
  const over = rows.filter((r) => r.pct >= 100).length;

  return (
    <SectionBox title="💰 Token Budget (24h)">
      <div style={{ marginBottom: 12, fontSize: 13, color: muted }}>
        Aggregate daily budget across all InferencePolicy CRs vs. actual consumption pulled from
        Prometheus. {err && <span style={{ color: "#ef5350" }}>{err}</span>}
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))", gap: "1rem", marginBottom: 16 }}>
        <Stat label="Fleet budget (24h)" value={formatTokens(fleetBudget)} />
        <Stat label="Fleet consumed (24h)" value={formatTokens(fleetUsed)} tone={utilizationTone(fleetPct)} />
        <Stat label="Fleet utilization" value={`${fleetPct.toFixed(1)}%`} tone={utilizationTone(fleetPct)} />
        <Stat label="Sandboxes ≥70% used" value={atRisk} tone={atRisk > 0 ? "warning" : ""} />
        <Stat label="Sandboxes over budget" value={over} tone={over > 0 ? "error" : ""} />
      </div>
      <div style={{ marginBottom: 8, fontSize: 13, fontWeight: 600 }}>Fleet utilization</div>
      <BudgetBar used={fleetUsed} total={fleetBudget} height={20} />
      <div style={{ marginTop: 16 }}>
        <SimpleTable
          data={rows.sort((a, b) => b.pct - a.pct).map((r) => ({
            name: r.name,
            policy: r.policy,
            budget: formatTokens(r.budget),
            used: formatTokens(r.used),
            bar: r,
          }))}
          columns={[
            { label: "Sandbox", getter: (r: { name: string }) => r.name },
            { label: "Policy", getter: (r: { policy: string }) => r.policy },
            { label: "Budget", getter: (r: { budget: string }) => r.budget },
            { label: "Used", getter: (r: { used: string }) => r.used },
            { label: "Utilization", getter: (r: { bar: { used: number; budget: number } }) => (
              <div style={{ width: 160 }}><BudgetBar used={r.bar.used} total={r.bar.budget} /></div>
            )},
          ]}
        />
      </div>
    </SectionBox>
  );
}

function SandboxBudgetCard({ sandboxName, inferenceRefName }: { sandboxName: string; inferenceRefName?: string }) {
  const theme = useTheme();
  const muted = theme.palette.text.secondary;
  // Pull both the bound InferencePolicy CR (for the budget) and the live counter.
  const [policies] = (CRD_CLASSES.inferencepolicies as any).useList() as [KubeObject[] | null];
  const policy = (policies || []).find((p) => p.metadata.name === inferenceRefName);
  const spec = (policy as any)?.jsonData?.spec || (policy as any)?.spec || {};
  const dailyBudget = spec?.tokenBudget?.dailyTokens || 0;
  const perRequestCap = spec?.tokenBudget?.perRequestTokens || 0;

  const { data: used24h } = usePromPoll(
    0,
    async (base) => {
      const r = await promQuery(base, `sum(increase(kars_tokens_total{sandbox="${sandboxName}"}[24h]))`);
      return r[0]?.value || 0;
    },
    10000
  );
  const { data: split } = usePromPoll(
    [] as { metric: Record<string,string>; value: number }[],
    async (base) => promQuery(base, `sum by (direction) (increase(kars_tokens_total{sandbox="${sandboxName}"}[24h]))`),
    10000
  );

  const pct = dailyBudget > 0 ? (used24h / dailyBudget) * 100 : 0;
  const remaining = Math.max(0, dailyBudget - used24h);
  const inputTok = split.find((r) => r.metric.direction === "input")?.value || 0;
  const outputTok = split.find((r) => r.metric.direction === "output")?.value || 0;

  return (
    <SectionBox title={`💰 Token Budget — ${sandboxName}`}>
      {!inferenceRefName && (
        <div style={{ color: muted, fontSize: 13 }}>No <code>inferenceRef</code> set on this sandbox; no enforced budget.</div>
      )}
      {inferenceRefName && !policy && (
        <div style={{ color: "#ef6c00", fontSize: 13 }}>InferencePolicy <code>{inferenceRefName}</code> not found.</div>
      )}
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: "0.75rem", marginBottom: 12 }}>
        <Stat label="Daily budget" value={dailyBudget > 0 ? formatTokens(dailyBudget) : "unlimited"} />
        <Stat label="Consumed (24h)" value={formatTokens(used24h)} tone={utilizationTone(pct)} />
        <Stat label="Remaining" value={dailyBudget > 0 ? formatTokens(remaining) : "—"} tone={utilizationTone(pct)} />
        <Stat label="Per-request cap" value={perRequestCap > 0 ? formatTokens(perRequestCap) : "unlimited"} />
        <Stat label="Input tokens" value={formatTokens(inputTok)} />
        <Stat label="Output tokens" value={formatTokens(outputTok)} />
      </div>
      {dailyBudget > 0 && (
        <div>
          <div style={{ marginBottom: 6, fontSize: 13, fontWeight: 600 }}>Utilization</div>
          <BudgetBar used={used24h} total={dailyBudget} height={22} />
        </div>
      )}
      {inferenceRefName && (
        <div style={{ marginTop: 12, fontSize: 12, color: muted }}>
          Policy: <Link routeName="inferencepolicies-detail" params={{ namespace: (policy as any)?.metadata?.namespace || "default", name: inferenceRefName }}>
            {inferenceRefName}
          </Link>
        </div>
      )}
    </SectionBox>
  );
}