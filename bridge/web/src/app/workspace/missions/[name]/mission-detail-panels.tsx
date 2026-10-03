// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Mission detail server presentation; page fetching and routing stay in ./page.

import Link from "next/link";
import { missionFailureAdvice, revisionArtifactUrl } from "@/lib/mission-run-evidence";
import { DeliverableView, DeliverableBody } from "@/components/deliverable-view";
import { ProvenanceStory } from "@/components/provenance-story";
import type { ReactNode } from "react";
import { egressScope, humanizeMcp } from "@/lib/format";
import { Icon } from "@/components/icon";
import { type Composition, type MissionResult, type MissionArtifact, type AgentIdentity, type TaskDetail } from "@/lib/types";


/** Infer the review kind from the produced artifact set, for typed routing
 *  (§16): code → review as a change, docs → prose, data → values. */
/** True when an artifact is prose (markdown/plain text) that should render as a
 * formatted document rather than a raw monospace dump. Code/data files
 * (json/csv/yaml/source) stay verbatim in <pre>. Extensionless files are
 * treated as prose — agents commonly write briefings with no extension. */
function isProseArtifact(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return true; // no extension → prose
  const ext = name.slice(dot + 1).toLowerCase();
  return ["md", "mdx", "markdown", "txt", "text", "rst", "adoc"].includes(ext);
}

type MissionServerTab = {
  id: string;
  label: string;
  badge?: number | string | null;
  node: ReactNode;
  live?: boolean;
};

export function MissionServerTabs({
  tabs,
  active,
  basePath,
}: {
  tabs: MissionServerTab[];
  active?: string;
  basePath: string;
}) {
  const current = tabs.find((tab) => tab.id === active) ?? tabs[0];
  return (
    <div>
      <div
        role="tablist"
        aria-label="Mission sections"
        className="sticky top-[57px] z-10 -mx-1 mb-5 flex gap-1 overflow-x-auto rounded-xl border border-border bg-surface/80 p-1 backdrop-blur supports-[backdrop-filter]:bg-surface/70"
      >
        {tabs.map((tab) => {
          const selected = tab.id === current.id;
          return (
            <Link
              key={tab.id}
              href={`${basePath}?tab=${encodeURIComponent(tab.id)}`}
              role="tab"
              aria-selected={selected}
              className={`relative flex shrink-0 items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-sm font-medium transition ${
                selected
                  ? "bg-signal/10 text-foreground"
                  : "text-foreground-muted hover:bg-surface-muted hover:text-foreground"
              }`}
            >
              {tab.live && <span className="h-1.5 w-1.5 rounded-full bg-signal kb-pulse" />}
              {tab.label}
              {tab.badge != null && tab.badge !== 0 && (
                <span className={`rounded-full px-1.5 text-[11px] tabular-nums ${
                  selected
                    ? "bg-signal/20 text-signal"
                    : "bg-surface-muted text-foreground-muted"
                }`}>
                  {tab.badge}
                </span>
              )}
            </Link>
          );
        })}
      </div>
      <div role="tabpanel" className="kb-rise space-y-5">
        {current.node}
      </div>
    </div>
  );
}

export function EnvelopeFact({ label, value }: { label: string; value: string }) {
  return (
    <span className="inline-flex items-baseline gap-1.5">
      <span className="text-xs text-foreground-muted">{label}</span>
      <span className="font-medium">{value}</span>
    </span>
  );
}

/** The mission objective, rendered so a multi-step, command-laden brief is
 *  readable instead of collapsing into one wall of text. The header shows a
 *  clamped one/two-line summary (the first meaningful line); the full brief is
 *  behind a native disclosure that preserves line breaks. */
function objectiveSummary(objective: string): string {
  const firstLine = objective
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  return firstLine ?? objective.trim();
}

export function ObjectiveBlock({ objective }: { objective: string }) {
  const trimmed = (objective ?? "").trim();
  if (!trimmed) {
    return <p className="mt-1 text-sm text-foreground-muted">No objective set.</p>;
  }
  const summary = objectiveSummary(trimmed);
  const hasMore = summary.length < trimmed.length;
  return (
    <div className="mt-1">
      <p className="line-clamp-2 text-sm text-foreground-muted">{summary}</p>
      {hasMore && (
        <details className="group mt-1.5">
          <summary className="inline-flex cursor-pointer list-none items-center gap-1 text-xs font-medium text-signal hover:underline [&::-webkit-details-marker]:hidden">
            <span className="transition-transform group-open:rotate-90" aria-hidden>›</span>
            <span className="group-open:hidden">Show full brief</span>
            <span className="hidden group-open:inline">Hide brief</span>
          </summary>
          <pre className="mt-2 max-h-96 overflow-auto whitespace-pre-wrap rounded-lg border border-border bg-surface-muted/50 px-4 py-3 font-mono text-xs leading-relaxed text-foreground-muted">
            {trimmed}
          </pre>
        </details>
      )}
    </div>
  );
}

/** ONE primary, state-driven next step for the mission (audit f11). It tells the
 *  user what to do now in plain language and links to the single relevant place,
 *  rather than presenting every control at once. The full controls live in the
 *  tabs below; this is the signpost, not a duplicate action surface. */
export function NextStep({
  status,
}: {
  status: import("@/components/mission-status").MissionStatus;
}) {
  const map: Record<string, { tone: string; title: string; body: string; cta?: { href: string; label: string } }> = {
    drafting: {
      tone: "border-signal/30 bg-signal/[0.05]",
      title: "Draft — review launch checks",
      body: "Review the model, tools, network, autonomy and budget below. The Execution panel checks this saved package before enabling Launch; a Ready controller phase alone does not establish launch readiness.",
    },
    deploying: {
      tone: "border-signal/30 bg-signal/[0.05]",
      title: "Deploying — the agent is coming online",
      body: "Provisioning status is shown below. A ready sandbox is not proof that the assignment has started.",
    },
    running: {
      tone: "border-signal/30 bg-signal/[0.05]",
      title: "Running",
      body: "Check revision status in Activity. Detailed activity appears only when a revision-bound trace is available; a running sandbox alone does not prove progress.",
    },
    needs_you: {
      tone: "border-warning/40 bg-warning/10",
      title: "This mission needs your decision",
      body: "An actionable approval request is associated with this revision. Inspect its scope before deciding.",
      cta: { href: "/workspace/inbox", label: "Open the inbox →" },
    },
    done: {
      tone: "border-ok/40 bg-ok/10",
      title: "Delivered",
      body: "Review the output, its recorded producer, and attached files. Accept or request a distinct revision in Deliverable. Receipt availability and revision binding are shown separately.",
    },
    failed: {
      tone: "border-danger/40 bg-danger/10",
      title: "This run didn't complete",
      body: "Open Run outcome to inspect the recorded output and available diagnostics. Missing evidence is not proof of the cause or how far execution got.",
    },
  };
  const m = map[status] ?? map.drafting;
  return (
    <div className={`flex flex-wrap items-center justify-between gap-3 rounded-xl border px-5 py-3.5 ${m.tone}`}>
      <div className="min-w-0">
        <p className="text-sm font-semibold">{m.title}</p>
        <p className="mt-0.5 text-xs text-foreground-muted">{m.body}</p>
      </div>
      {m.cta && (
        <Link href={m.cta.href} className="shrink-0 rounded-lg bg-signal px-4 py-2 text-xs font-semibold text-signal-fg hover:opacity-90">
          {m.cta.label}
        </Link>
      )}
    </div>
  );
}

export function RecordedRunPanel({ result }: { result: MissionResult }) {
  const record = result.run_evidence;
  const fmt = (value: number | null | undefined) => value == null ? "Unavailable" : value.toLocaleString("en-US");
  const facts: [string, string][] = [
    ["Revision", result.assignment_nonce ?? "Unavailable"],
    ["Agent", record?.agent_name ?? "Producer unavailable"],
    ["Agent DID", record?.agent_did ?? "Unavailable"],
    ["Assignment", record?.assignment_id ?? "Unavailable"],
    ["Task UID", record?.task_uid ?? "Unavailable"],
    ["Outcome", record?.status ?? result.status ?? "Unavailable"],
    ["Model", result.model ?? "Unavailable"],
    ["Started", record?.started_at ?? "Unavailable"],
    ["Finished", record?.finished_at ?? result.finished_at ?? "Unavailable"],
    ["Model rounds", fmt(record?.rounds)],
    ["Prompt tokens", fmt(result.usage_known === true ? result.prompt_tokens : null)],
    ["Completion tokens", fmt(result.usage_known === true ? result.completion_tokens : null)],
    ["Total tokens", fmt(result.usage_known === true ? result.total_tokens : null)],
  ];
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Recorded run</h2>
      <p className="mt-1 text-xs text-foreground-muted">
        {record
          ? "Producer and whole-run usage from the validated terminal record for this revision, not the current mesh registry. Detailed tool activity is separate and may be unavailable."
          : "No validated producer record is available for this revision. Current registry membership does not establish who produced this output."}
      </p>
      <dl className="mt-4 grid gap-x-6 gap-y-3 sm:grid-cols-2">
        {facts.map(([label, value]) => (
          <div key={label} className="min-w-0">
            <dt className="text-xs text-foreground-muted">{label}</dt>
            <dd className="mt-0.5 break-all text-sm">{value}</dd>
          </div>
        ))}
      </dl>
      {record && (
        <details className="mt-4 text-xs">
          <summary className="cursor-pointer font-medium">Recorded runtime identity</summary>
          <dl className="mt-2 space-y-2">
            {[["Sandbox UID", record.sandbox_uid], ["Pod UID", record.pod_uid], ["Runtime boot ID", record.runtime_boot_id], ["Dispatcher DID", record.dispatcher_did]].map(([label, value]) => (
              <div key={label}><dt className="text-foreground-muted">{label}</dt><dd className="break-all font-mono">{value}</dd></div>
            ))}
          </dl>
        </details>
      )}
    </section>
  );
}

export function AgentIdentityCard({ identity }: { identity: AgentIdentity }) {
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Current mesh registry identity</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        Discovered from the current registry. This may differ from the recorded producer of an
        earlier revision and is not delivery evidence.
      </p>
      <dl className="mt-3 space-y-2 text-sm">
        <div className="flex flex-wrap items-baseline gap-x-2">
          <dt className="text-xs text-foreground-muted">DID</dt>
          <dd className="font-mono text-xs break-all">{identity.did}</dd>
        </div>
        {identity.capabilities.length > 0 && (
          <div>
            <dt className="text-xs text-foreground-muted">Advertised capabilities</dt>
            <dd className="mt-1 flex flex-wrap gap-1.5">
              {identity.capabilities.map((c) => (
                <span key={c} className="rounded-full bg-surface-muted px-2 py-0.5 font-mono text-xs">
                  {c}
                </span>
              ))}
            </dd>
          </div>
        )}
        {identity.last_seen && (
          <div className="flex flex-wrap items-baseline gap-x-2">
            <dt className="text-xs text-foreground-muted">Last seen on the mesh</dt>
            <dd className="text-xs font-medium">{new Date(identity.last_seen).toLocaleString()}</dd>
          </div>
        )}
      </dl>
    </section>
  );
}

export function ArtifactsPanel({ ns, task, runNonce, artifacts, pullRequests, activity, egress, tokens }: { ns: string; task: string; runNonce: string | null; artifacts: MissionArtifact[]; pullRequests: import("@/lib/types").PullRequestRef[]; activity: import("@/lib/types").ActivityEvent[]; egress: string[]; tokens: number | null }) {
  const fmtSize = (n: number | null) =>
    n == null ? "" : n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`;
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Artifacts</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Files explicitly captured for this revision. This is a bounded handoff, not an
            inventory of every file in the agent sandbox.
          </p>
        </div>
        <span className="shrink-0 rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium">
          {artifacts.length} file{artifacts.length === 1 ? "" : "s"}
        </span>
      </div>
      {!runNonce && artifacts.length > 0 && <p className="mt-3 text-xs text-foreground-muted">Downloads unavailable: no current revision is recorded.</p>}
      {/* Output references do not establish PR authorship. */}
      {pullRequests.length > 0 && (
        <div className="mt-4 rounded-lg border border-signal/20 bg-signal/[0.03] p-4">
          <h3 className="text-xs font-semibold">Pull requests referenced in output</h3>
          <ul className="mt-2 flex flex-wrap gap-2">
            {pullRequests.map((pr) => (
              <li key={pr.url}>
                <a
                  href={pr.url}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-2 rounded-lg border border-signal/30 bg-signal/5 px-2.5 py-1.5 hover:bg-signal/10"
                  title={`Pull request on ${pr.repo}`}
                >
                  <Icon name="branch" size={13} className="shrink-0 text-signal" />
                  <span className="text-xs font-medium text-signal">PR #{pr.number}</span>
                  <span className="font-mono text-[11px] text-foreground-muted">{pr.repo}</span>
                  <span aria-hidden className="text-[11px] text-foreground-muted">↗</span>
                </a>
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className="mt-4 rounded-lg border border-border bg-background/40 p-4">
        <h3 className="text-xs font-semibold">Available activity evidence</h3>
        {activity.length > 0
          ? <div className="mt-2"><ProvenanceStory activity={activity} egress={egress} tokens={tokens} /></div>
          : <p className="mt-2 text-xs text-foreground-muted">Detailed revision-bound trace unavailable. See Recorded run for terminal producer and usage evidence.</p>}
      </div>
      <ul className="mt-4 divide-y divide-border rounded-lg border border-border">
        {artifacts.map((a, i) => (
          <li key={a.name}>
            <details open={i === 0} className="group">
              <summary className="flex cursor-pointer items-center justify-between gap-3 px-4 py-2.5 hover:bg-surface-muted/50">
                <span className="flex items-center gap-2 font-mono text-xs">
                  <span aria-hidden className="text-foreground-muted transition-transform group-open:rotate-180">⌄</span>
                  {a.name}
                </span>
                <span className="flex shrink-0 items-center gap-3 text-xs text-foreground-muted">
                  <a
                    href={revisionArtifactUrl(ns, task, a.name, runNonce)}
                    aria-disabled={!runNonce}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-signal hover:underline"
                  >
                    {a.content_truncated ? "Open full" : "Open"}
                  </a>
                  <a
                    href={revisionArtifactUrl(ns, task, a.name, runNonce)}
                    aria-disabled={!runNonce}
                    download={a.name}
                    className="inline-flex items-center gap-1 font-medium text-signal hover:underline"
                  >
                    <Icon name="download" size={12} />
                    Download
                  </a>
                  <span>
                    {a.content == null ? "binary · " : ""}
                    {fmtSize(a.size_bytes)}
                  </span>
                </span>
              </summary>
              {a.content_truncated ? (
                <div className="space-y-3 border-t border-border bg-surface-muted/20 px-4 py-3">
                  <p className="text-xs text-foreground-muted">
                    Showing a bounded preview of {(a.content_bytes ?? a.size_bytes ?? 0).toLocaleString()} bytes.
                  </p>
                  {a.content ? (
                    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-lg border border-border bg-surface-muted/30 p-3 font-mono text-xs leading-relaxed">
                      {a.content}
                    </pre>
                  ) : null}
                  <a
                    href={revisionArtifactUrl(ns, task, a.name, runNonce)}
                    aria-disabled={!runNonce}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="inline-flex text-xs font-medium text-signal hover:underline"
                  >
                    Open full artifact ↗
                  </a>
                  <a
                    href={revisionArtifactUrl(ns, task, a.name, runNonce)}
                    aria-disabled={!runNonce}
                    download={a.name}
                    className="inline-flex items-center gap-1 text-xs font-medium text-signal hover:underline"
                  >
                    <Icon name="download" size={12} />
                    Download artifact
                  </a>
                </div>
              ) : a.content != null ? (
                isProseArtifact(a.name) ? (
                  <div className="max-h-96 overflow-auto border-t border-border bg-surface-muted/20 px-4 py-3">
                    <DeliverableBody output={a.content} />
                  </div>
                ) : (
                  <pre className="max-h-96 overflow-auto whitespace-pre-wrap border-t border-border bg-surface-muted/30 px-4 py-3 font-mono text-xs leading-relaxed">
                    {a.content}
                  </pre>
                )
              ) : (
                <p className="border-t border-border bg-surface-muted/30 px-4 py-3 text-xs text-foreground-muted">
                  Binary artifact — use{" "}
                  <a
                    href={revisionArtifactUrl(ns, task, a.name, runNonce)}
                    aria-disabled={!runNonce}
                    download={a.name}
                    className="text-signal hover:underline"
                  >
                    Download
                  </a>{" "}
                  to fetch the full file.
                </p>
              )}
            </details>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function ResultPanel({ result }: { result: MissionResult }) {
  return (
    <div className="space-y-3">
      {result.source === "single_turn" && (
        <div className="flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/[0.06] px-3 py-2 text-xs text-foreground-muted">
          <span aria-hidden className="mt-0.5 text-amber-600">ℹ</span>
          <span>
            <span className="font-medium text-foreground">Source marked single-turn.</span> This
            legacy classification alone does not establish tool use, delegated work, or producer identity.
          </span>
        </div>
      )}
      <DeliverableView
        output={result.output}
        model={result.model}
        totalTokens={result.total_tokens}
        finishedAt={result.finished_at}
      />
    </div>
  );
}

/** Current diagnostics are separate from the recorded revision's result. */
export function FailureDiagnostic({
  task,
  troubleshoot,
}: {
  task: TaskDetail;
  troubleshoot: import("@/lib/types").Troubleshoot | null;
}) {
  const reason = task.result?.output ?? task.execution_detail ?? "The run ended with an error.";
  const { cause, remedy } = missionFailureAdvice(reason);
  const stages: { label: string; reached: boolean }[] = [
    { label: "Task launch recorded", reached: task.launched },
    { label: "Current sandbox reference", reached: !!task.sandbox },
    { label: "Current registry last-seen value", reached: !!task.agent_identity?.last_seen },
    { label: "Revision-bound activity available", reached: (task.activity?.length ?? 0) > 0 },
  ];

  return (
    <section className="space-y-4 rounded-xl border border-amber-500/40 bg-amber-500/5 p-6">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 text-warning" aria-hidden>
          <Icon name="target" size={18} />
        </span>
        <div>
          <h2 className="text-sm font-semibold">Revision not ready for approval</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            A stop message alone does not establish the root cause.
          </p>
        </div>
      </div>

      <div className="rounded-lg border border-amber-500/30 bg-surface p-4">
        <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">Recorded result</p>
        <p className="mt-1 text-sm">{cause}</p>
        <p className="mt-3 text-xs font-semibold uppercase tracking-wide text-foreground-muted">What to do</p>
        <p className="mt-1 text-sm">{remedy}</p>
      </div>

      {troubleshoot && troubleshoot.evidence.length > 0 && (
        <div className="rounded-lg border border-danger/30 bg-surface p-4">
          <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">Current sandbox diagnostics — revision binding unverified</p>
          <ul className="mt-2 space-y-1">
            {troubleshoot.evidence.map((e, i) => (
              <li key={i} className="rounded bg-danger/5 px-2 py-1 font-mono text-[11px] leading-relaxed text-danger">{e}</li>
            ))}
          </ul>
        </div>
      )}

      {/* Live container status. */}
      {troubleshoot && troubleshoot.containers.length > 0 && (
        <div className="rounded-lg border border-border bg-surface p-4">
          <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">
            Sandbox pod {troubleshoot.pod_summary ? `(${troubleshoot.pod_summary} ready)` : ""}
          </p>
          <ul className="mt-2 grid gap-1.5 sm:grid-cols-2">
            {troubleshoot.containers.map((c) => (
              <li key={c.name} className="flex items-center gap-2 text-xs">
                <span aria-hidden className={c.ready ? "text-ok" : "text-danger"}>{c.ready ? "✓" : "✗"}</span>
                <span className="font-mono">{c.name}</span>
                <span className="text-foreground-muted">
                  {c.state}{c.reason ? ` · ${c.reason}` : ""}{c.restarts > 0 ? ` · ${c.restarts}↻` : ""}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="rounded-lg border border-border bg-surface p-4">
        <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">Available observations</p>
        <p className="mt-1 text-xs text-foreground-muted">Current resources are not historical execution proof. Missing evidence does not identify a stopping point.</p>
        <ul className="mt-2 space-y-1.5">
          {stages.map((s) => (
            <li key={s.label} className="flex items-center gap-2 text-sm">
              <span className="text-foreground-muted">{s.label}: {s.reached ? "present" : "unavailable"}</span>
            </li>
          ))}
        </ul>
      </div>

      <details className="rounded-lg border border-border bg-surface">
        <summary className="cursor-pointer px-4 py-2.5 text-xs font-semibold">Recorded result or execution message</summary>
        <p className="border-t border-border px-4 py-3 font-mono text-xs leading-relaxed text-foreground-muted">{reason}</p>
      </details>
      {troubleshoot && troubleshoot.agent_log_tail.length > 0 && (
        <details className="rounded-lg border border-border bg-surface">
          <summary className="cursor-pointer px-4 py-2.5 text-xs font-semibold">Current agent log tail — revision binding unverified</summary>
          <pre className="max-h-72 overflow-auto border-t border-border px-4 py-3 font-mono text-[10px] leading-relaxed text-foreground-muted">{troubleshoot.agent_log_tail.join("\n")}</pre>
        </details>
      )}

      {/* Actions. */}
      <div className="flex flex-wrap gap-2">
        <Link
          href={`/workspace/new?intent=${encodeURIComponent(task.objective)}`}
          className="rounded-lg bg-signal px-4 py-2 text-xs font-semibold text-signal-fg hover:opacity-90"
        >
          Re-compose from this intent →
        </Link>
      </div>
      {task.result?.finished_at && (
        <p className="text-xs text-foreground-muted">Recorded finish {new Date(task.result.finished_at).toLocaleString()}</p>
      )}
    </section>
  );
}

export function CompositionPanel({ composition, launched, envelopeToolPolicy }: {
  composition: Composition;
  launched: boolean;
  envelopeToolPolicy: string | null;
}) {
  const c = composition;
  const toolPolicy = c.tool_policy?.trim() || (!launched && envelopeToolPolicy?.trim()) || null;
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">How this mission runs</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        {launched
          ? "The effective configuration the running sandbox is using — read from the materialized policy and sandbox (including any defaults the controller applied)."
          : "The planned configuration you composed — what this mission will run with once launched."}
      </p>
      <dl className="mt-4 grid gap-x-8 gap-y-4 sm:grid-cols-2">
        <Fact label="Model" value={c.model} />
        <Fact label="Harness" value={c.runtime} />
        <Fact label="Tool policy" value={toolPolicy ?? (launched ? "Runtime policy not reported" : "Policy not reported")} />
        <Fact label="Isolation" value={c.isolation} />
        <Fact
          label="Connected services"
          value={c.mcp_servers.length ? c.mcp_servers.map(humanizeMcp).join(", ") : "None"}
        />
        <Fact label="Shared memory" value={c.memory ?? "None"} />
      </dl>
      <p className="mt-3 text-xs text-foreground-muted">
        {toolPolicy === "kars-default"
          ? "kars-default permits governed tools, including shell and Foundry tools. Instructions to avoid tools do not disable them."
          : toolPolicy
          ? "The policy name alone does not establish which tools are permitted; review its rules."
          : "Missing policy information is not evidence that tools are disabled."}
      </p>
      <div className="mt-4 border-t border-border pt-4">
        <p className="text-xs font-medium text-foreground-muted">Network egress</p>
        {c.egress.length === 0 ? (
          <p className="mt-1 text-sm">Model path only — all other egress denied at the boundary.</p>
        ) : (
          <>
          <ul className="mt-1.5 flex flex-wrap gap-1.5">
            {c.egress.map((e) => {
              const scope = egressScope(e);
              return (
                <li key={e} className="inline-flex items-center gap-1.5 rounded-full bg-surface-muted px-2.5 py-1 font-mono text-xs">
                  <span
                    className={`h-1.5 w-1.5 rounded-full ${scope === "internal" ? "bg-sky-500" : "bg-amber-500"}`}
                    title={scope === "internal" ? "Internal — in-cluster / private" : "External — public internet"}
                    aria-hidden
                  />
                  {e}
                </li>
              );
            })}
          </ul>
          <p className="mt-1.5 text-[11px] text-foreground-muted">
            <span className="inline-flex items-center gap-1"><span className="h-1.5 w-1.5 rounded-full bg-sky-500" aria-hidden /> internal</span>
            <span className="ml-3 inline-flex items-center gap-1"><span className="h-1.5 w-1.5 rounded-full bg-amber-500" aria-hidden /> external</span>
            <span className="ml-2">— same boundary, labelled for clarity.</span>
          </p>
          </>
        )}
      </div>
      {c.instructions && (
        <div className="mt-4 border-t border-border pt-4">
          <p className="text-xs font-medium text-foreground-muted">Instructions</p>
          <p className="mt-1.5 whitespace-pre-wrap rounded-lg bg-surface-muted px-3 py-2 text-sm leading-relaxed">
            {c.instructions}
          </p>
        </div>
      )}
    </section>
  );
}

function Fact({ label, value }: { label: string; value: string | null }) {
  return (
    <div>
      <dt className="text-xs text-foreground-muted">{label}</dt>
      <dd className="mt-0.5 text-sm font-medium">{value ?? "—"}</dd>
    </div>
  );
}
