"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import type {
  EngineeringSignal,
  EngineeringSource,
  GithubConnection,
} from "@/lib/types";
import {
  configureEngineeringSource,
  decideEngineeringReview,
  disconnectEngineeringSource,
  runEngineeringSync,
} from "./engineering-actions";

const INTERVALS = [
  [300, "Every 5 minutes"],
  [900, "Every 15 minutes"],
  [1800, "Every 30 minutes"],
  [3600, "Every hour"],
  [21600, "Every 6 hours"],
] as const;

const STATE_META = {
  disabled: ["Disabled", "border-border bg-surface-muted text-foreground-muted"],
  idle: ["Scheduled", "border-sky-500/30 bg-sky-500/10 text-sky-600"],
  syncing: ["Syncing", "border-sky-500/30 bg-sky-500/10 text-sky-600"],
  ok: ["Healthy", "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"],
  partial: ["Partial", "border-amber-500/30 bg-amber-500/10 text-amber-600"],
  error: ["Error", "border-rose-500/30 bg-rose-500/10 text-rose-600"],
} as const;

const REVIEW_META = {
  ready_for_review: ["Ready for review", "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"],
  waiting_for_ci: ["Waiting for GitHub CI", "border-sky-500/30 bg-sky-500/10 text-sky-600"],
  ci_failed: ["CI failed", "border-rose-500/30 bg-rose-500/10 text-rose-600"],
  blocked: ["Blocked", "border-amber-500/30 bg-amber-500/10 text-amber-600"],
  unknown: ["Unverified", "border-border bg-surface-muted text-foreground-muted"],
} as const;

function when(value: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export function EngineeringIntake({
  team,
  initialSource,
  connection,
}: {
  team: string;
  initialSource: EngineeringSource | null;
  connection: GithubConnection | null;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [source, setSource] = useState(initialSource);
  const [enabled, setEnabled] = useState(initialSource?.enabled ?? false);
  const [autoRun, setAutoRun] = useState(initialSource?.auto_run ?? true);
  const [repos, setRepos] = useState<Set<string>>(
    new Set(initialSource?.repos ?? []),
  );
  const [dependabot, setDependabot] = useState(
    initialSource?.configured
      ? initialSource.signals.includes("dependabot_pr")
      : true,
  );
  const [dependabotAlerts, setDependabotAlerts] = useState(
    initialSource?.signals.includes("dependabot_alert") ?? false,
  );
  const [codeScanning, setCodeScanning] = useState(
    initialSource?.signals.includes("code_scanning_alert") ?? false,
  );
  const [secretScanning, setSecretScanning] = useState(
    initialSource?.signals.includes("secret_scanning_alert") ?? false,
  );
  const [interval, setInterval] = useState(
    initialSource?.poll_interval_seconds ?? 900,
  );
  const [error, setError] = useState<string | null>(null);
  const [feedbackRun, setFeedbackRun] = useState<string | null>(null);
  const [feedback, setFeedback] = useState("");

  const noLongerAuthorized = connection?.connected
    ? source?.repos.filter((repo) => !connection.repos.includes(repo)) ?? []
    : [];
  const status = source?.status;
  const meta = STATE_META[status?.state ?? "disabled"];

  function toggleRepo(repo: string) {
    setRepos((current) => {
      const next = new Set(current);
      if (next.has(repo)) next.delete(repo);
      else next.add(repo);
      return next;
    });
  }

  function save() {
    setError(null);
    startTransition(async () => {
      const signals: EngineeringSignal[] = [
        ...(dependabot ? (["dependabot_pr"] as const) : []),
        ...(dependabotAlerts ? (["dependabot_alert"] as const) : []),
        ...(codeScanning ? (["code_scanning_alert"] as const) : []),
        ...(secretScanning ? (["secret_scanning_alert"] as const) : []),
      ];
      const result = await configureEngineeringSource(team, {
        enabled,
        auto_run: autoRun,
        repos: [...repos],
        signals,
        poll_interval_seconds: interval,
      });
      if (result.error) {
        setError(result.error);
        return;
      }
      setSource(result.source);
      router.refresh();
    });
  }

  function sync() {
    setError(null);
    startTransition(async () => {
      const result = await runEngineeringSync(team);
      if (result.error) {
        setError(result.error);
        return;
      }
      setSource(result.source);
      router.refresh();
    });
  }

  function disconnect() {
    setError(null);
    startTransition(async () => {
      const result = await disconnectEngineeringSource(team);
      if (result.error) {
        setError(result.error);
        return;
      }
      setSource(result.source);
      setEnabled(false);
      setRepos(new Set());
      router.refresh();
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Engineering intake</h2>
          <p className="mt-0.5 max-w-3xl text-xs text-foreground-muted">
            Continuously discover actionable work in repositories from your existing GitHub App
            connection and queue it in this team&apos;s durable backlog. Agents propose changes;
            current checks must still provide CI evidence before anyone claims success.
          </p>
        </div>
        <span className={`rounded-full border px-2.5 py-1 text-xs font-medium ${meta[1]}`}>
          {meta[0]}
        </span>
      </div>

      <div className="mt-4 rounded-lg border border-border bg-surface-muted/20 p-4">
        <p className="text-xs font-semibold">How one engineering item moves through kars</p>
        <ol className="mt-3 grid gap-2 sm:grid-cols-2 xl:grid-cols-6">
          {[
            ["1", "Intake", "GitHub signal is discovered."],
            ["2", "Backlog", "A durable team task is queued."],
            ["3", "Run", "The team mints one governed task force."],
            ["4", "Agents", "Selected roles work and hand back evidence."],
            ["5", "Result", "One principal deliverable plus role artifacts land."],
            ["6", "Readiness", "GitHub CI and mergeability are observed."],
          ].map(([step, label, detail]) => (
            <li key={step} className="rounded-md border border-border bg-surface px-3 py-2">
              <p className="text-[10px] font-semibold uppercase tracking-wide text-signal">
                {step} · {label}
              </p>
              <p className="mt-1 text-[11px] leading-relaxed text-foreground-muted">{detail}</p>
            </li>
          ))}
        </ol>
        <p className="mt-3 text-[11px] text-foreground-muted">
          Activity is the live timeline inside a run. Artifacts are files produced by individual
          agents. The deliverable is the principal&apos;s final synthesis. Engineering intake owns
          discovery and GitHub readiness; it links to the exact run that performed the work.
        </p>
      </div>

      {initialSource === null ? (
        <p className="mt-4 rounded-lg border border-warning/30 bg-warning/[0.05] p-3 text-xs text-foreground-muted">
          Engineering intake status is unavailable while the Bridge integration store cannot be
          reached.
        </p>
      ) : (
        <>
          {!connection?.connected && (
            <p className="mt-4 rounded-lg border border-border bg-surface-muted/30 p-3 text-xs text-foreground-muted">
              Connect the shared GitHub App for your user to configure or sync repositories. This
              team never asks for or exposes a token; you can still inspect status or disconnect
              this intake source.
            </p>
          )}
          <div className="mt-4 grid gap-4 lg:grid-cols-[1.4fr_1fr]">
            <div className="rounded-lg border border-border bg-surface-muted/20 p-4">
              <p className="text-xs font-medium">Authorized repositories</p>
              {connection?.account && (
                <p className="mt-0.5 text-[11px] text-foreground-muted">
                  Connected as <span className="font-mono">{connection.account}</span>
                </p>
              )}
              {!connection?.connected ? (
                <p className="mt-3 text-xs text-foreground-muted">
                  No connected repository list is available for this user.
                </p>
              ) : connection.repos.length === 0 ? (
                <p className="mt-3 text-xs text-foreground-muted">
                  Your installation currently grants no repositories. Update it on GitHub, then
                  re-sync the connection.
                </p>
              ) : (
                <div className="mt-3 grid gap-2 sm:grid-cols-2">
                  {connection.repos.map((repo) => (
                    <label key={repo} className="flex cursor-pointer items-center gap-2 text-xs">
                      <input
                        type="checkbox"
                        checked={repos.has(repo)}
                        onChange={() => toggleRepo(repo)}
                        className="h-3.5 w-3.5 rounded border-border accent-signal"
                      />
                      <span className="font-mono">{repo}</span>
                    </label>
                  ))}
                </div>
              )}
              {noLongerAuthorized.length > 0 && (
                <p className="mt-3 text-[11px] text-warning">
                  No longer authorized: {noLongerAuthorized.join(", ")}. Save a valid selection
                  before the next sync.
                </p>
              )}
            </div>

            <div className="space-y-3 rounded-lg border border-border bg-surface-muted/20 p-4">
              <label className="flex items-center gap-2 text-xs font-medium">
                <input
                  type="checkbox"
                  checked={enabled}
                  onChange={(event) => setEnabled(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Enable continuous intake
              </label>
              <label className="flex items-center gap-2 text-xs font-medium">
                <input
                  type="checkbox"
                  checked={autoRun}
                  onChange={(event) => setAutoRun(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Start the team automatically when new work is queued
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={dependabot}
                  onChange={(event) => setDependabot(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Dependabot pull requests
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={dependabotAlerts}
                  onChange={(event) => setDependabotAlerts(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Dependabot vulnerability alerts
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={codeScanning}
                  onChange={(event) => setCodeScanning(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Code scanning / code-quality alerts
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={secretScanning}
                  onChange={(event) => setSecretScanning(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Secret scanning alerts
              </label>
              <p className="text-[11px] leading-relaxed text-foreground-muted">
                Security signals require the GitHub App&apos;s corresponding read permission and
                the repository feature to be enabled. Missing permissions or unavailable features
                show as Partial/Unavailable; they are never reported as zero findings.
              </p>
              <label className="block text-xs">
                <span className="font-medium">Poll interval</span>
                <select
                  value={interval}
                  onChange={(event) => setInterval(Number(event.target.value))}
                  className="mt-1 block w-full rounded-md border border-border bg-surface px-2 py-1.5 text-xs"
                >
                  {INTERVALS.map(([seconds, label]) => (
                    <option key={seconds} value={seconds}>
                      {label}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          </div>

          <dl className="mt-4 grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-9">
            <div>
              <dt className="text-[11px] text-foreground-muted">Last sync</dt>
              <dd className="mt-0.5 text-xs">{when(status?.last_sync_at ?? null)}</dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Last success</dt>
              <dd className="mt-0.5 text-xs">{when(status?.last_success_at ?? null)}</dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Discovered</dt>
              <dd className="mt-0.5 text-xs tabular-nums">
                {status?.items_discovered ?? 0}
              </dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Newly queued</dt>
              <dd className="mt-0.5 text-xs tabular-nums">{status?.items_queued ?? 0}</dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Total queued</dt>
              <dd className="mt-0.5 text-xs tabular-nums">
                {status?.total_items_queued ?? 0}
              </dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Next poll</dt>
              <dd className="mt-0.5 text-xs">{when(status?.next_poll_at ?? null)}</dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Ready for review</dt>
              <dd className="mt-0.5 text-xs tabular-nums text-emerald-600">
                {status?.ready_for_review ?? 0}
              </dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Waiting for CI</dt>
              <dd className="mt-0.5 text-xs tabular-nums">{status?.waiting_for_ci ?? 0}</dd>
            </div>
            <div>
              <dt className="text-[11px] text-foreground-muted">Red / blocked</dt>
              <dd className="mt-0.5 text-xs tabular-nums text-rose-600">
                {status?.ci_failed ?? 0}
              </dd>
            </div>
          </dl>

          {status?.last_error && (
            <p className="mt-3 rounded-lg border border-rose-500/30 bg-rose-500/[0.05] p-3 text-xs text-rose-600">
              {status.last_error}
            </p>
          )}

          {(status?.signal_results ?? []).length > 0 && (
            <div className="mt-4">
              <h3 className="text-xs font-semibold">Source coverage</h3>
              <p className="mt-0.5 text-[11px] text-foreground-muted">
                One row per repository and selected signal. This is the honest answer to what was
                actually scanned during the last sync.
              </p>
              <ul className="mt-2 grid gap-2 md:grid-cols-2">
                {(status?.signal_results ?? []).map((result) => (
                  <li
                    key={`${result.repo}:${result.signal}`}
                    className="rounded-md border border-border bg-surface-muted/20 px-3 py-2"
                  >
                    <div className="flex items-start justify-between gap-2">
                      <div>
                        <p className="font-mono text-[10px]">{result.repo}</p>
                        <p className="mt-0.5 text-xs font-medium">
                          {result.signal.replaceAll("_", " ")}
                        </p>
                      </div>
                      <span
                        className={`rounded-full border px-2 py-0.5 text-[10px] ${
                          result.state === "ok"
                            ? "border-emerald-500/30 text-emerald-600"
                            : result.state === "unavailable"
                              ? "border-border bg-surface-muted text-foreground-muted"
                            : result.state === "truncated"
                              ? "border-amber-500/30 text-amber-600"
                              : "border-rose-500/30 text-rose-600"
                        }`}
                      >
                        {result.state}
                      </span>
                    </div>
                    <p className="mt-1 text-[11px] text-foreground-muted">{result.detail}</p>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {(status?.review_items ?? []).length > 0 && (
            <div className="mt-4">
              <div>
                <h3 className="text-xs font-semibold">Engineering work items</h3>
                <p className="mt-0.5 text-[11px] text-foreground-muted">
                  Each card joins the intake source to its backlog task, exact run, agent
                  handbacks, deliverable/artifacts, and GitHub-observed readiness.
                </p>
              </div>
              <ul className="mt-3 space-y-2">
                {(status?.review_items ?? []).map((item) => {
                  const reviewMeta = REVIEW_META[item.state];
                  return (
                    <li key={`${item.repo}#${item.pr_number}`} className="rounded-lg border border-border bg-surface-muted/20 p-3">
                      <div className="flex flex-wrap items-start justify-between gap-3">
                        <div className="min-w-0">
                          <a href={item.pr_url} target="_blank" rel="noreferrer" className="text-xs font-semibold text-signal hover:underline">
                            {item.repo} #{item.pr_number} · {item.title} ↗
                          </a>
                          <p className="mt-1 text-[11px] text-foreground-muted">{item.detail}</p>
                          <div className="mt-2 flex flex-wrap gap-1.5 text-[10px]">
                            <span className="rounded-full border border-border px-2 py-0.5">
                              backlog {item.task_status || "unknown"}
                            </span>
                            <span className="rounded-full border border-border px-2 py-0.5">
                              run {item.run_state?.toLowerCase() ?? "unknown"}
                            </span>
                            <span className="rounded-full border border-border px-2 py-0.5">
                              agents {item.delivered_roles.length}/{item.selected_roles.length} delivered
                            </span>
                            <span className="rounded-full border border-border px-2 py-0.5">
                              {item.artifact_count == null
                                ? "artifacts unavailable"
                                : `${item.artifact_count} artifact${item.artifact_count === 1 ? "" : "s"}`}
                            </span>
                          </div>
                          {item.selected_roles.length > 0 && (
                            <p className="mt-2 text-[11px] text-foreground-muted">
                              Agents: {item.selected_roles.map((role) => (
                                <span key={role} className="mr-1.5 inline-flex items-center gap-1">
                                  <span aria-hidden>{item.delivered_roles.includes(role) ? "✓" : "○"}</span>
                                  <span className="font-mono">{role}</span>
                                </span>
                              ))}
                            </p>
                          )}
                          <p className="mt-1 font-mono text-[10px] text-foreground-muted">
                            {item.checks_passed}/{item.checks_total} checks · {item.head_sha.slice(0, 12)} · run {item.run}
                          </p>
                          <div className="mt-2 flex flex-wrap gap-3 text-[11px] font-medium">
                            <Link
                              href={`/workspace/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(item.run)}?tab=activity`}
                              className="text-signal hover:underline"
                            >
                              Open agent activity →
                            </Link>
                            <Link
                              href={`/workspace/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(item.run)}?tab=deliverables`}
                              className="text-signal hover:underline"
                            >
                              Open deliverable & artifacts →
                            </Link>
                          </div>
                          <div className="mt-3 border-t border-border pt-3">
                            {feedbackRun === item.run ? (
                              <div className="space-y-2">
                                <textarea
                                  value={feedback}
                                  onChange={(event) => setFeedback(event.target.value)}
                                  rows={2}
                                  placeholder="Describe exactly what the principal must change before this PR can be reviewed again."
                                  className="w-full rounded-md border border-border bg-surface px-2.5 py-2 text-xs outline-none focus:border-signal"
                                />
                                <div className="flex gap-2">
                                  <button
                                    type="button"
                                    disabled={pending || !feedback.trim()}
                                    onClick={() => {
                                      setError(null);
                                      startTransition(async () => {
                                        const result = await decideEngineeringReview(team, {
                                          decision: "request_changes",
                                          repo: item.repo,
                                          pr_number: item.pr_number,
                                          pr_url: item.pr_url,
                                          head_sha: item.head_sha,
                                          run: item.run,
                                          comment: feedback.trim(),
                                        });
                                        if (result.error) {
                                          setError(result.error);
                                          return;
                                        }
                                        setFeedback("");
                                        setFeedbackRun(null);
                                        router.refresh();
                                      });
                                    }}
                                    className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-signal-fg disabled:opacity-50"
                                  >
                                    Queue changes & run team
                                  </button>
                                  <button
                                    type="button"
                                    onClick={() => {
                                      setFeedback("");
                                      setFeedbackRun(null);
                                    }}
                                    className="rounded-md border border-border px-2.5 py-1 text-[11px]"
                                  >
                                    Cancel
                                  </button>
                                </div>
                              </div>
                            ) : (
                              <div className="flex flex-wrap gap-2">
                                <button
                                  type="button"
                                  onClick={() => setFeedbackRun(item.run)}
                                  className="rounded-md border border-border px-2.5 py-1 text-[11px] font-medium"
                                >
                                  Request changes
                                </button>
                                <a
                                  href={item.pr_url}
                                  target="_blank"
                                  rel="noreferrer"
                                  className="rounded-md border border-emerald-500/40 px-2.5 py-1 text-[11px] font-medium text-emerald-600"
                                >
                                  Review / merge in GitHub ↗
                                </a>
                              </div>
                            )}
                            <p className="mt-2 text-[10px] text-foreground-muted">
                              Bridge can queue review feedback today. Principal merge authorization
                              remains disabled until it is a typed, single-use grant bound to this
                              repository, PR, and exact head SHA.
                            </p>
                          </div>
                        </div>
                        <span className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-medium ${reviewMeta[1]}`}>
                          {reviewMeta[0]}
                        </span>
                      </div>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}

          <div className="mt-4 flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={save}
              disabled={
                pending ||
                !connection?.connected ||
                (enabled &&
                  (repos.size === 0 ||
                    (!dependabot && !dependabotAlerts && !codeScanning && !secretScanning)))
              }
              className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg disabled:opacity-50"
            >
              {pending ? "Working…" : source?.configured ? "Save configuration" : "Configure"}
            </button>
            <button
              type="button"
              onClick={sync}
              disabled={
                pending ||
                !connection?.connected ||
                !source?.configured ||
                !source.enabled
              }
              className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium disabled:opacity-50"
            >
              Sync now
            </button>
            {source?.configured && (
              <button
                type="button"
                onClick={disconnect}
                disabled={pending}
                className="rounded-lg border border-rose-500/30 px-3 py-1.5 text-xs font-medium text-rose-600 disabled:opacity-50"
              >
                Disconnect intake
              </button>
            )}
          </div>
        </>
      )}
      {error && <p className="mt-3 text-xs text-rose-600">{error}</p>}
    </section>
  );
}
