// Mission detail server presentation; page fetching and routing stay in ./page.

import Link from "next/link";
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
      title: "Ready to launch",
      body: "Review the composed plan below — model, tools, network, autonomy, budget — then launch it in the Execution panel when you're happy.",
    },
    deploying: {
      tone: "border-signal/30 bg-signal/[0.05]",
      title: "Deploying — the agent is coming online",
      body: "Each provisioning step below is a real, verified event. This page updates itself live; no need to refresh.",
    },
    running: {
      tone: "border-signal/30 bg-signal/[0.05]",
      title: "Running",
      body: "Watch the agent work in the Activity tab. If it needs a decision it will ask you here and in your Inbox.",
    },
    needs_you: {
      tone: "border-warning/40 bg-warning/10",
      title: "This mission needs your decision",
      body: "It paused for your approval before a priced, external, or irreversible step.",
      cta: { href: "/workspace/inbox", label: "Open the inbox →" },
    },
    done: {
      tone: "border-ok/40 bg-ok/10",
      title: "Delivered",
      body: "The deliverable is ready. Review it and accept or request changes in the Deliverable tab; the signed receipt is in the Receipt tab.",
    },
    failed: {
      tone: "border-danger/40 bg-danger/10",
      title: "This run didn't complete",
      body: "Open the Run failed tab below for a full diagnosis — the likely cause, how far it got, the runtime's exact reason, and one-click ways to re-compose or re-run.",
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

export function AgentIdentityCard({ identity }: { identity: AgentIdentity }) {
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Agent mesh identity</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        The running agent&apos;s real, harness-neutral identity on the encrypted agent mesh —
        discovered live from the registry. This is how work is delivered and verified across any
        runtime.
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

export function ArtifactsPanel({ ns, task, artifacts, pullRequests, activity, egress, tokens }: { ns: string; task: string; artifacts: MissionArtifact[]; pullRequests: import("@/lib/types").PullRequestRef[]; activity: import("@/lib/types").ActivityEvent[]; egress: string[]; tokens: number | null }) {
  const fmtSize = (n: number | null) =>
    n == null ? "" : n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`;
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Artifacts</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            The complete set of files the agent produced through its native loop over the mesh —
            captured by the controller into a durable, cluster-native record.
          </p>
        </div>
        <span className="shrink-0 rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium">
          {artifacts.length} file{artifacts.length === 1 ? "" : "s"}
        </span>
      </div>
      {/* Pull requests are a first-class delivery type — a PR the agent opened is
          an artifact, shown here as a chip (not only in the deliverable prose). */}
      {pullRequests.length > 0 && (
        <div className="mt-4 rounded-lg border border-signal/20 bg-signal/[0.03] p-4">
          <h3 className="text-xs font-semibold">Pull requests opened</h3>
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
      {/* How this was made — the plain-language provenance story over the real trace. */}
      <div className="mt-4 rounded-lg border border-border bg-background/40 p-4">
        <h3 className="text-xs font-semibold">How this was made</h3>
        <div className="mt-2"><ProvenanceStory activity={activity} egress={egress} tokens={tokens} /></div>
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
                    href={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/artifact/${encodeURIComponent(a.name)}`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-signal hover:underline"
                  >
                    {a.content_truncated ? "Open full" : "Open"}
                  </a>
                  <a
                    href={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/artifact/${encodeURIComponent(a.name)}`}
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
                    href={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/artifact/${encodeURIComponent(a.name)}`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="inline-flex text-xs font-medium text-signal hover:underline"
                  >
                    Open full artifact ↗
                  </a>
                  <a
                    href={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/artifact/${encodeURIComponent(a.name)}`}
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
                    href={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/artifact/${encodeURIComponent(a.name)}`}
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
            <span className="font-medium text-foreground">Single-turn completion.</span> The full
            agent loop (tools + sub-agents) was unavailable on this run, so this is one model turn —
            the Activity tab will show no tool calls. Re-run to try the full loop again.
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

/** Analyse a run failure reason into a plain-language cause + a specific remedy,
 *  and (when relevant) flag that the harness itself is the problem. */
function analyzeFailure(reason: string, harness: string | null): { cause: string; remedy: string; harnessIssue: boolean } {
  const r = (reason || "").toLowerCase();
  const chatGateway = !!harness && /hermes|gateway|channel/.test(harness.toLowerCase());
  if (r.includes("did not come online") || r.includes("not yet discoverable") || r.includes("mesh registry") || r.includes("not discoverable")) {
    return {
      cause: chatGateway
        ? `The agent never registered on the encrypted mesh. The “${harness}” harness is a chat-gateway — it waits for inbound channel messages and does not execute a one-shot mission on its own, so it never came online to do autonomous work.`
        : "The agent sandbox didn't register on the encrypted mesh within the startup window. This is usually a slow container image pull or node pressure delaying the pod — occasionally a crashed agent container.",
      remedy: chatGateway
        ? "Re-compose this mission on the OpenClaw harness (built for autonomous missions), or drive this one through its channel."
        : "Re-run it — a fresh sandbox often comes up cleanly. If it repeats, an operator can inspect the sandbox for image-pull or crash errors.",
      harnessIssue: chatGateway,
    };
  }
  if (r.includes("no progress heartbeat") || r.includes("timed out") || r.includes("timeout")) {
    return {
      cause: "The agent started but stopped making progress, so the controller timed the run out after a period with no heartbeat.",
      remedy: "Re-run it. If it stalls repeatedly, narrow the objective or raise the token/time budget in the envelope.",
      harnessIssue: false,
    };
  }
  if (r.includes("content safety") || r.includes("jailbreak") || r.includes("blocked by")) {
    return {
      cause: "A content-safety policy blocked the run before it could deliver.",
      remedy: "Adjust the objective to avoid the flagged content, or ask an operator about the content-safety floor.",
      harnessIssue: false,
    };
  }
  if (r.includes("budget") || r.includes("token cap") || r.includes("out of tokens")) {
    return {
      cause: "The run hit its token budget before producing a deliverable.",
      remedy: "Re-run with a higher token budget in the envelope.",
      harnessIssue: false,
    };
  }
  return {
    cause: "The run ended with an error before producing a deliverable.",
    remedy: "Re-run it, or re-compose with a different harness or model.",
    harnessIssue: false,
  };
}

/** Real, actionable troubleshooting for a failed run: what happened, how far the
 *  provisioning got (which stage it stopped at), and what to do next. When live
 *  cluster evidence is available (pod/container status + the agent's own log
 *  tail), it uses the evidence-derived diagnosis and SHOWS the proof; otherwise
 *  it falls back to analysing the recorded reason. */
export function FailureDiagnostic({
  task,
  troubleshoot,
}: {
  task: TaskDetail;
  troubleshoot: import("@/lib/types").Troubleshoot | null;
}) {
  const reason = task.result?.output ?? task.execution_detail ?? "The run ended with an error.";
  const harness = task.composition?.runtime ?? null;
  // Prefer the live, evidence-derived diagnosis from the cluster; fall back to
  // the local reason analysis when the troubleshoot endpoint is unavailable.
  const local = analyzeFailure(reason, harness);
  const cause = troubleshoot?.cause ?? local.cause;
  const remedy = troubleshoot?.remedy ?? local.remedy;
  const harnessIssue = troubleshoot?.harness_issue ?? local.harnessIssue;
  const meshAcknowledged = task.assignment_events.some(
    (event) => event.event_type === "acknowledged" || event.state === "Running",
  );

  // How far provisioning got — the same stages the deploy timeline tracks. The
  // first un-reached stage is where it stopped.
  const stages: { label: string; reached: boolean }[] = [
    { label: "Launch approved", reached: task.launched },
    { label: "Sandbox provisioned", reached: !!task.sandbox },
    { label: "Agent online on the mesh", reached: meshAcknowledged || !!task.agent_identity?.last_seen },
    { label: "First activity (model round / tool call)", reached: (task.activity?.length ?? 0) > 0 },
  ];
  const stoppedAt = stages.findIndex((s) => !s.reached);

  return (
    <section className="space-y-4 rounded-xl border border-amber-500/40 bg-amber-500/5 p-6">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 text-warning" aria-hidden>
          <Icon name="target" size={18} />
        </span>
        <div>
          <h2 className="text-sm font-semibold">Run did not complete</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {troubleshoot
              ? "Diagnosed from the sandbox's live pod status and the agent's own logs."
              : "Here's what happened and how to fix it."}
          </p>
        </div>
      </div>

      {/* Likely cause + remedy. */}
      <div className="rounded-lg border border-amber-500/30 bg-surface p-4">
        <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">Likely cause</p>
        <p className="mt-1 text-sm">{cause}</p>
        <p className="mt-3 text-xs font-semibold uppercase tracking-wide text-foreground-muted">What to do</p>
        <p className="mt-1 text-sm">{remedy}</p>
      </div>

      {/* The real smoking-gun evidence pulled from the agent's logs. */}
      {troubleshoot && troubleshoot.evidence.length > 0 && (
        <div className="rounded-lg border border-danger/30 bg-surface p-4">
          <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">Evidence — from the agent&apos;s own logs</p>
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

      {/* How far it got. */}
      <div className="rounded-lg border border-border bg-surface p-4">
        <p className="text-xs font-semibold uppercase tracking-wide text-foreground-muted">How far it got</p>
        <ol className="mt-2 space-y-1.5">
          {stages.map((s, i) => {
            const isStop = i === stoppedAt;
            return (
              <li key={s.label} className="flex items-center gap-2 text-sm">
                <span aria-hidden className={s.reached ? "text-ok" : isStop ? "text-danger" : "text-foreground-muted"}>
                  {s.reached ? "✓" : isStop ? "✗" : "•"}
                </span>
                <span className={s.reached ? "" : isStop ? "font-medium text-danger" : "text-foreground-muted"}>
                  {s.label}
                  {isStop && <span className="ml-1.5 text-xs font-normal text-danger">— stopped here</span>}
                </span>
              </li>
            );
          })}
        </ol>
      </div>

      {/* The raw agent log tail — the exact evidence, for the record. */}
      <details className="rounded-lg border border-border bg-surface">
        <summary className="cursor-pointer px-4 py-2.5 text-xs font-semibold">
          {troubleshoot && troubleshoot.agent_log_tail.length > 0 ? "Agent log tail (live)" : "Runtime's exact reason"}
        </summary>
        {troubleshoot && troubleshoot.agent_log_tail.length > 0 ? (
          <pre className="max-h-72 overflow-auto border-t border-border px-4 py-3 font-mono text-[10px] leading-relaxed text-foreground-muted">{troubleshoot.agent_log_tail.join("\n")}</pre>
        ) : (
          <p className="border-t border-border px-4 py-3 font-mono text-xs leading-relaxed text-foreground-muted">{reason}</p>
        )}
      </details>

      {/* Actions. */}
      <div className="flex flex-wrap gap-2">
        <Link
          href={`/workspace/new?intent=${encodeURIComponent(task.objective)}`}
          className="rounded-lg bg-signal px-4 py-2 text-xs font-semibold text-signal-fg hover:opacity-90"
        >
          {harnessIssue ? "Re-compose on OpenClaw →" : "Re-compose from this intent →"}
        </Link>
      </div>
      {task.result?.finished_at && (
        <p className="text-xs text-foreground-muted">Failed {new Date(task.result.finished_at).toLocaleString()}</p>
      )}
    </section>
  );
}

export function CompositionPanel({ composition, launched }: { composition: Composition; launched: boolean }) {
  const c = composition;
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
        <Fact label="Tool policy" value={c.tool_policy ?? "None — model only"} />
        <Fact label="Isolation" value={c.isolation} />
        <Fact
          label="Connected services"
          value={c.mcp_servers.length ? c.mcp_servers.map(humanizeMcp).join(", ") : "None"}
        />
        <Fact label="Shared memory" value={c.memory ?? "None"} />
      </dl>
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
