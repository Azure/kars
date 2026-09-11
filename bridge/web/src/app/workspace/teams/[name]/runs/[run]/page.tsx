import Link from "next/link";
import { DeliverableBody } from "@/components/deliverable-view";
import { HonestState } from "@/components/honest-state";
import { ExecutionExplorer } from "@/components/execution-explorer";
import { LiveRefresh } from "@/components/live-refresh";
import { TeamRunFlow } from "@/components/team-run-flow";
import { TaskCheckpointPanel } from "@/components/task-checkpoint";
import { TaskApprovalsPanel } from "@/app/tasks/[name]/task-approvals-panel";
import { DeployTimeline } from "@/app/workspace/missions/[name]/deploy-timeline";
import { EgressRequest } from "@/app/workspace/missions/[name]/egress-request";
import { MissionBlockers } from "@/app/workspace/missions/[name]/mission-blockers";
import { authWired, defaultNamespace, operatorIdentity } from "@/lib/config";
import { getArchivedTeamRun, getTask, getTeam, listTaskApprovals } from "@/lib/bff";
import { analyzeTeamRun } from "@/lib/team-run-evidence";
import { currentPrincipal } from "@/lib/session";
import type { ReactNode } from "react";
import { HaltTeamRunButton } from "./halt-button";

export const dynamic = "force-dynamic";

const OUTCOME = {
  paused: {
    label: "Paused",
    detail: "This run was paused with its evidence preserved. It is not executing while the standing team is hibernating.",
    tone: "border-border bg-surface-muted/50 text-foreground-muted",
  },
  running: {
    label: "Running",
    detail: "The principal and role workers are still executing. Outcome classification appears only after the run terminates.",
    tone: "border-sky-500/40 bg-sky-500/[0.07] text-sky-600",
  },
  delivered: {
    label: "Delivered",
    detail: "Every selected role returned a durable handback in the core assignment ledger.",
    tone: "border-signal/40 bg-signal/[0.07] text-signal",
  },
  delivered_with_issues: {
    label: "Delivered with coordination issues",
    detail: "A usable outcome landed, but one or more mesh handoffs or evidence steps failed and required recovery.",
    tone: "border-warning/50 bg-warning/[0.08] text-warning",
  },
  incomplete: {
    label: "Incomplete",
    detail: "The run stopped for a human request or ended without durable evidence from every selected role.",
    tone: "border-warning/50 bg-warning/[0.08] text-warning",
  },
  failed: {
    label: "Failed",
    detail: "The principal did not produce a successful run result.",
    tone: "border-danger/50 bg-danger/[0.08] text-danger",
  },
} as const;

export default async function TeamRunPage({
  params,
  searchParams,
}: {
  params: Promise<{ name: string; run: string }>;
  searchParams: Promise<{ tab?: string }>;
}) {
  const { name, run } = await params;
  const { tab } = await searchParams;
  const ns = defaultNamespace();
  const [team, task, allApprovals, principal] = await Promise.all([
    getTeam(ns, name).catch(() => null),
    getTask(ns, run).catch(() => null),
    listTaskApprovals(ns, run).catch(() => []),
    currentPrincipal(),
  ]);
  if (!team) return <RunUnavailable run={run} />;
  if (!task) {
    const archived = await getArchivedTeamRun(ns, name, run).catch(() => null);
    if (!archived) return <RunUnavailable run={run} />;
    const pullRequests = archivedPullRequests(archived.content ?? "", team.git_write_repos);
    return (
      <div className="space-y-6">
        <nav aria-label="Breadcrumb" className="flex flex-wrap items-center text-sm text-foreground-muted">
          <Link href="/workspace/teams" prefetch={false} className="hover:text-foreground hover:underline">Teams</Link>
          <span className="px-1.5" aria-hidden>/</span>
          <Link href={`/workspace/teams/${encodeURIComponent(name)}`} prefetch={false} className="hover:text-foreground hover:underline">
            {team.display_name ?? team.name}
          </Link>
          <span className="px-1.5" aria-hidden>/</span>
          <span className="text-foreground">Archived run</span>
        </nav>
        <div>
          <p className="font-mono text-xs text-foreground-muted">{run}</p>
          <h1 className="mt-1 text-2xl font-semibold tracking-tight">{archived.title}</h1>
          <p className="mt-1 text-sm text-foreground-muted">
            Completed {new Date(archived.created_at).toLocaleString()} · retained in team shared memory
          </p>
        </div>
        <section className="rounded-xl border border-signal/35 bg-signal/[0.05] px-5 py-4">
          <p className="text-sm font-semibold">Archived team delivery</p>
          <p className="mt-1 text-xs text-foreground-muted">
            The disposable run resources reached their retention limit. The principal synthesis,
            provenance digest, and PR references remain durable here; per-tool telemetry and transient
            worker artifacts are no longer available.
          </p>
        </section>
        {pullRequests.length > 0 && (
          <section className="rounded-xl border border-border bg-surface p-5">
            <h2 className="text-sm font-semibold">Pull request deliverables</h2>
            <ul className="mt-3 flex flex-wrap gap-2">
              {pullRequests.map((pr) => (
                <li key={pr.url}>
                  <a href={pr.url} target="_blank" rel="noreferrer" className="rounded-lg border border-signal/30 bg-signal/5 px-3 py-2 text-sm font-medium text-signal hover:bg-signal/10">
                    {pr.repo} PR #{pr.number} ↗
                  </a>
                </li>
              ))}
            </ul>
          </section>
        )}
        <section className="rounded-xl border border-border bg-surface p-5">
          <h2 className="text-sm font-semibold">Principal synthesis</h2>
          {archived.content ? (
            <div className="mt-3"><DeliverableBody output={archived.content} /></div>
          ) : (
            <HonestState variant="empty" title="Archived content unavailable" detail="The commons index remains, but its retained content was pruned." />
          )}
        </section>
        <dl className="flex flex-wrap gap-x-6 gap-y-2 rounded-xl border border-border bg-surface px-5 py-4 text-xs text-foreground-muted">
          <div><dt>Digest</dt><dd className="font-mono text-foreground">{archived.digest}</dd></div>
          <div><dt>Stored size</dt><dd className="text-foreground">{archived.size_bytes.toLocaleString()} bytes</dd></div>
          <div><dt>Source</dt><dd className="font-mono text-foreground">{archived.source_task}</dd></div>
        </dl>
      </div>
    );
  }
  const approvals = allApprovals.filter(
    (approval) =>
      approval.run_nonce == null
      || task.current_run_nonce == null
      || approval.run_nonce === task.current_run_nonce,
  );
  if (task.team !== name) return <RunUnavailable run={run} />;

  const evidence = analyzeTeamRun(team, task);
  const rosterRuntimes = [...new Set(team.roster.flatMap((role) => role.runtime ? [role.runtime] : []))];
  const rosterModels = [...new Set(team.roster.flatMap((role) => role.model ? [role.model] : []))];
  const principalRuntime = task.composition?.runtime ?? (rosterRuntimes.length === 1 ? rosterRuntimes[0] : null);
  const principalModel = task.composition?.model ?? (rosterModels.length === 1 ? rosterModels[0] : null);
  const synthesizedGraphAgents = evidence.roles
    .filter((role) => role.state !== "skipped")
    .map((role) => ({
      name: role.role.member_task ?? role.role.name,
      namespace: ns,
      phase:
        role.state === "delivered"
          ? "Completed"
          : role.state === "failed"
            ? "Failed"
            : role.state === "working"
              ? "Running"
              : "Ready",
      runtime: role.role.runtime ?? principalRuntime,
      role: role.role.name,
      parent: run,
      logical_agent_id: role.role.name,
      model: role.role.model ?? principalModel,
    }));
  const normalizeRole = (value: string | null) =>
    (value ?? "").toLowerCase().replace(/[^a-z0-9]+/g, "");
  const graphSubAgents = (task.sub_agents.length > 0 ? task.sub_agents : synthesizedGraphAgents)
    .map((agent) => {
      const rosterRole = team.roster.find((role) => {
        const target = normalizeRole(role.name);
        return target === normalizeRole(agent.role) || normalizeRole(agent.name).includes(target);
      });
      return {
        ...agent,
        runtime: agent.runtime ?? rosterRole?.runtime ?? principalRuntime,
        model: agent.model ?? rosterRole?.model ?? principalModel,
      };
    });
  const outcome = OUTCOME[evidence.outcome];
  const awaitingAssignment = Boolean(
    task.current_run_nonce
    && task.assignment?.task_id !== task.current_run_nonce,
  );
  const assignmentInFlight =
    task.assignment?.completed_at == null
    && (
      task.assignment?.state === "Assigned"
      || task.assignment?.state === "Running"
    );
  const running =
    task.launched && !team.paused && (awaitingAssignment || assignmentInFlight);
  const roleDelivered = evidence.roles.filter((r) => r.state === "delivered").length;
  const roleTarget = evidence.roles.filter((r) => r.state !== "skipped").length;
  const lastActivityAt =
    [
      task.assignment?.last_progress_at,
      task.assignment?.completed_at,
      task.result?.finished_at,
    ]
      .filter((value): value is string => value != null)
      .sort()
      .at(-1) ?? null;
  const evidenceArtifacts = task.artifacts.filter(
    (a) =>
      !a.name.endsWith("collaboration.jsonl") &&
      !a.name.endsWith("research-evidence.jsonl") &&
      !a.name.endsWith("subagent-telemetry.jsonl") &&
      !a.name.endsWith("execution-contract.json") &&
      !a.name.endsWith("task-checkpoint.json"),
  );
  const missingSelectedRoles = evidence.roles.filter(
    (role) => role.state === "missing" || role.state === "failed",
  );
  const failedToolCalls = task.activity.filter(
    (event) => event.kind === "tool" && !event.ok,
  );
  const promptShare =
    task.result?.total_tokens && task.result.prompt_tokens
      ? Math.round((task.result.prompt_tokens / task.result.total_tokens) * 100)
      : null;
  const claimedPullRequest =
    task.result?.status === "error" &&
    /github\.com\/[^/\s]+\/[^/\s]+\/pull\/\d+/i.test(task.result.output);

  return (
    <div className="space-y-6">
      <LiveRefresh active={running} />
      <nav aria-label="Breadcrumb" className="flex flex-wrap items-center text-sm text-foreground-muted">
        <Link href="/workspace/teams" prefetch={false} className="hover:text-foreground">Teams</Link>
        <span className="px-1.5" aria-hidden>/</span>
        <Link href={`/workspace/teams/${encodeURIComponent(name)}`} prefetch={false} className="font-medium text-signal hover:underline">
          {team.display_name ?? team.name}
        </Link>
        <span className="px-1.5" aria-hidden>/</span>
        <span className="text-foreground">Run</span>
      </nav>

      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="font-mono text-xs text-foreground-muted">{run}</p>
          <h1 className="mt-1 text-2xl font-semibold tracking-tight">Team run</h1>
          <p className="mt-1 text-sm text-foreground-muted">
            A managed execution of this standing team: assignments, handoffs, decisions, and final result.
          </p>
        </div>
        {running && <HaltTeamRunButton ns={ns} team={name} run={run} />}
      </div>
      <dl className="flex flex-wrap gap-x-6 gap-y-2 rounded-xl border border-border bg-surface px-5 py-3 text-xs text-foreground-muted">
        <div>
          <dt>Started</dt>
          <dd className="font-medium text-foreground">
            {task.created_at ? new Date(task.created_at).toLocaleString() : "Unknown"}
          </dd>
        </div>
        <div>
          <dt>Last activity</dt>
          <dd className="font-medium text-foreground">
            {lastActivityAt
              ? new Date(lastActivityAt).toLocaleString()
              : "No activity recorded"}
          </dd>
        </div>
        <div>
          <dt>Completed</dt>
          <dd className="font-medium text-foreground">
            {task.result?.finished_at ? new Date(task.result.finished_at).toLocaleString() : "In progress"}
          </dd>
        </div>
      </dl>

      <section className={`rounded-xl border px-5 py-4 ${outcome.tone}`}>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 className="font-semibold">{outcome.label}</h2>
            <p className="mt-1 max-w-3xl text-sm text-foreground-muted">{outcome.detail}</p>
          </div>
          <div className="grid grid-cols-3 gap-5 text-right text-xs text-foreground-muted">
            <div><strong className="block text-base text-foreground">{roleDelivered}/{roleTarget}</strong>selected roles</div>
            <div><strong className="block text-base text-foreground">{evidenceArtifacts.length}</strong>artifacts</div>
            <div><strong className="block text-base text-foreground">{(task.result?.total_tokens ?? 0).toLocaleString()}</strong>tokens</div>
          </div>
        </div>
      </section>
      {task.checkpoint && <TaskCheckpointPanel checkpoint={task.checkpoint} />}

      {evidence.evidenceMode !== "ledger" && task.result != null && (
        <section className="rounded-xl border border-warning/40 bg-warning/[0.06] px-5 py-4">
          <h2 className="font-semibold">Historical run — assignment ledger unavailable</h2>
          <p className="mt-1 text-sm text-foreground-muted">
            This run predates durable child lifecycle events. Its retained output and artifacts remain visible, but Bridge will not infer Delivered N/N from them.
          </p>
        </section>
      )}

      {task.result?.blocked && (
        <section className="rounded-xl border border-warning/50 bg-warning/[0.06] px-5 py-4">
          <h2 className="font-semibold">Run stopped before delivery</h2>
          <p className="mt-1 text-sm text-foreground-muted">{task.result.blocked.detail}</p>
        </section>
      )}
      {task.result?.status === "error" && !task.result.blocked && (
        <section className="rounded-xl border border-danger/50 bg-danger/[0.06] px-5 py-4">
          <h2 className="font-semibold">Why this run failed</h2>
          <p className="mt-1 text-sm text-foreground-muted">
            The agent produced a confident narrative, but the retained execution evidence did not
            support a successful outcome.
          </p>
          <ul className="mt-3 space-y-1.5 text-sm">
            {missingSelectedRoles.length > 0 && (
              <li>
                • Missing required handback:{" "}
                {missingSelectedRoles.map((role) => role.role.name).join(", ")}.
              </li>
            )}
            {failedToolCalls.length > 0 && (
              <li>
                • {failedToolCalls.length} tool call{failedToolCalls.length === 1 ? "" : "s"} failed;
                inspect the grouped execution flow for the exact sequence.
              </li>
            )}
            {claimedPullRequest && (
              <li>
                • The raw narrative claimed a pull request, but failed output is not accepted as a
                PR deliverable. No verified PR is attached to this run.
              </li>
            )}
            {promptShare != null && promptShare >= 85 && (
              <li>
                • {promptShare}% of the {(task.result.total_tokens ?? 0).toLocaleString()} tokens
                were prompt/context tokens, indicating repeated context replay rather than useful
                completion.
              </li>
            )}
          </ul>
          <Link
            href={`/workspace/teams/${encodeURIComponent(name)}/runs/${encodeURIComponent(run)}?tab=activity`}
            prefetch={false}
            className="mt-4 inline-flex rounded-lg border border-danger/30 bg-surface px-3 py-2 text-xs font-semibold text-danger hover:bg-danger/[0.06]"
          >
            Open searchable execution flow →
          </Link>
          <details className="mt-4 rounded-lg border border-danger/25 bg-surface/60">
            <summary className="cursor-pointer px-3 py-2 text-xs font-medium">
              Raw agent narrative
              <span className="ml-2 font-normal text-foreground-muted">
                untrusted because the run failed
              </span>
            </summary>
            <p className="max-h-96 overflow-auto whitespace-pre-wrap border-t border-danger/20 px-3 py-3 text-xs text-foreground-muted">
              {task.result.output}
            </p>
          </details>
        </section>
      )}

      <TeamRunTabs
        active={tab}
        basePath={`/workspace/teams/${encodeURIComponent(name)}/runs/${encodeURIComponent(run)}`}
        tabs={[
          {
            id: "overview",
            label: "Overview",
            node: (
              <div className="space-y-5">
                <section className="rounded-xl border border-border bg-surface p-5">
                  <h2 className="text-sm font-semibold">Role delivery</h2>
                  <p className="mt-1 text-xs text-foreground-muted">
                    Who was selected, who completed their assignment, and which outputs were retained.
                  </p>
                  <div className="mt-4 grid gap-3 md:grid-cols-2">
                    {evidence.roles.map((r) => (
                      <div key={r.role.name} className="rounded-lg border border-border bg-surface-muted/30 p-4">
                        <div className="flex items-center justify-between gap-3">
                          <p className="font-medium">{r.role.name.replace(/-/g, " ")}</p>
                          <span className={`rounded-full px-2 py-0.5 text-[11px] font-medium ${
                            r.state === "delivered"
                              ? "bg-signal/10 text-signal"
                              : r.state === "skipped"
                                ? "bg-surface-muted text-foreground-muted"
                               : r.state === "working"
                                 ? "bg-sky-500/10 text-sky-600"
                                 : r.state === "failed"
                                   ? "bg-danger/10 text-danger"
                                   : "bg-warning/10 text-warning"
                          }`}>
                            {r.state === "delivered"
                              ? "Handback received"
                              : r.state === "skipped"
                                ? "Skipped for this task"
                                : r.state === "working"
                                  ? "Working"
                                  : r.state === "failed"
                                    ? "Handback failed"
                                    : "No handback"}
                          </span>
                        </div>
                        <p className="mt-1 text-xs text-foreground-muted">{r.role.system_prompt}</p>
                        <p className="mt-2 text-[11px] text-foreground-muted">
                          {r.artifacts.length} artifact{r.artifacts.length === 1 ? "" : "s"} · {r.handbacks.length} structured handback{r.handbacks.length === 1 ? "" : "s"}
                          {r.artifactAttribution === "inferred" ? " · artifact ownership inferred from retained file names and paths" : ""}
                        </p>
                        {r.handbacks.at(-1)?.at && (
                          <p className="mt-1 text-[11px] text-foreground-muted" suppressHydrationWarning>
                            Latest handback: {new Date(r.handbacks.at(-1)!.at!).toLocaleString()}
                          </p>
                        )}
                      </div>
                    ))}
                  </div>
                </section>

                {evidence.issues.length > 0 && (
                  <section className="rounded-xl border border-warning/50 bg-warning/[0.06] p-5">
                    <h2 className="text-sm font-semibold">Coordination issues</h2>
                    <p className="mt-1 text-xs text-foreground-muted">
                      These statements came from retained worker reports or the principal deliverable; they are not inferred from a green status badge.
                    </p>
                    <ul className="mt-3 space-y-2 text-sm">
                      {evidence.issues.map((issue) => <li key={issue}>• {issue}</li>)}
                    </ul>
                  </section>
                )}
              </div>
            ),
          },
          {
            id: "activity",
            label: "Execution flow",
            badge: evidence.collaboration.length + evidence.research.length,
            node: (
              <div className="space-y-4">
                {task.result == null && <DeployTimeline task={task} />}
                <ExecutionExplorer
                  running={running}
                  activity={task.activity}
                  telemetry={task.telemetry}
                  assignmentEvents={task.assignment_events}
                  approvals={approvals}
                  ns={ns}
                  name={run}
                  agentLabel={team.display_name ?? team.name}
                  agentPhase={team.paused ? "Hibernating" : task.assignment?.state ?? task.execution_phase ?? task.phase}
                  agentRuntime={principalRuntime}
                  agentModel={principalModel}
                  subAgents={graphSubAgents}
                  identity={task.agent_identity}
                  envelopeDigest={task.envelope_digest ?? team.envelope_digest}
                />
                <MissionBlockers
                  ns={ns}
                  task={run}
                  approvals={approvals}
                  activity={task.activity}
                  running={running}
                  decider={principal.name || operatorIdentity()}
                  authWired={authWired()}
                />
                <TaskApprovalsPanel
                  approvals={approvals}
                  decider={principal.name || operatorIdentity()}
                  authWired={authWired()}
                />
                {task.execution_phase === "Running" && <EgressRequest mission={run} />}
                <TeamRunFlow task={task} evidence={evidence} />
              </div>
            ),
          },
          {
            id: "deliverables",
            label: "Deliverables",
            badge: evidenceArtifacts.length + (task.pull_requests?.length ?? 0),
            node: (
              <div className="space-y-5">
                {(task.pull_requests?.length ?? 0) > 0 && (
                  <section className="rounded-xl border border-border bg-surface p-5">
                    <h2 className="text-sm font-semibold">Pull requests</h2>
                    <ul className="mt-3 space-y-2">
                      {task.pull_requests!.map((pr) => (
                        <li key={pr.url}>
                          <a href={pr.url} target="_blank" rel="noreferrer" className="font-medium text-signal hover:underline">
                            {pr.url} ↗
                          </a>
                        </li>
                      ))}
                    </ul>
                  </section>
                )}
                <section className="rounded-xl border border-border bg-surface p-5">
                  <h2 className="text-sm font-semibold">Role artifacts</h2>
                  <div className="mt-3 space-y-3">
                    {evidence.roles.flatMap((r) => r.artifacts.map((a) => (
                      <details key={`${r.role.name}-${a.name}`} className="rounded-lg border border-border bg-surface-muted/30">
                        <summary className="cursor-pointer px-4 py-3">
                          <span className="font-medium">{a.name}</span>
                          <span className="ml-2 text-xs text-foreground-muted">
                            from {r.role.name.replace(/-/g, " ")}
                            {r.artifactAttribution === "inferred" ? " · inferred attribution" : ""}
                          </span>
                        </summary>
                        <div className="border-t border-border px-4 py-3">
                          <ArtifactBody
                            artifact={a}
                            downloadHref={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(run)}/artifact/${encodeURIComponent(a.name)}`}
                          />
                        </div>
                      </details>
                    )))}
                    {evidence.unattributedArtifacts.map((a) => (
                      <details key={`unattributed-${a.name}`} className="rounded-lg border border-border bg-surface-muted/30">
                        <summary className="cursor-pointer px-4 py-3">
                          <span className="font-medium">{a.name}</span>
                          <span className="ml-2 text-xs text-foreground-muted">principal or unattributed</span>
                        </summary>
                        <div className="border-t border-border px-4 py-3">
                          <ArtifactBody
                            artifact={a}
                            downloadHref={`/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(run)}/artifact/${encodeURIComponent(a.name)}`}
                          />
                        </div>
                      </details>
                    ))}
                  </div>
                </section>
                {task.result?.output && task.result.status !== "error" && !task.result.blocked && (
                  <section className="rounded-xl border border-border bg-surface p-5">
                    <h2 className="text-sm font-semibold">Principal synthesis</h2>
                    <div className="mt-3"><DeliverableBody output={task.result.output} /></div>
                  </section>
                )}
              </div>
            ),
          },
          {
            id: "research",
            label: "Research & egress",
            badge: evidence.research.length || null,
            node: evidence.research.length > 0 ? (
              <section className="rounded-xl border border-border bg-surface p-5">
                <h2 className="text-sm font-semibold">External egress attempts</h2>
                <p className="mt-1 text-xs text-foreground-muted">
                  Router-authoritative HTTP egress events include the credential-safe source URL, enforcement outcome, status, and timing. Legacy runs fall back to clearly labelled agent-reported evidence.
                </p>
                <ul className="mt-4 space-y-2">
                  {evidence.research.map((e, i) => (
                    <li key={`${e.url}-${i}`} className="rounded-lg border border-border bg-surface-muted/30 px-4 py-3">
                      <a href={e.url} target="_blank" rel="noreferrer" className="break-all text-sm font-medium text-signal hover:underline">{e.url} ↗</a>
                      <p className="mt-1 font-mono text-[11px] text-foreground-muted">
                        {e.source} · {e.agent ?? "agent"} · {e.outcome ?? "unknown"}{e.status ? ` · HTTP ${e.status}` : ""}{e.digest ? ` · ${e.digest}` : ""}
                      </p>
                    </li>
                  ))}
                </ul>
              </section>
            ) : (
              <HonestState
                variant="empty"
                title="No external research observed"
                detail="This run did not retain governed HTTP source evidence. Connected services and model calls are not presented as external research."
              />
            ),
          },
        ]}
      />
    </div>
  );
}

function RunUnavailable({ run }: { run: string }) {
  return (
    <div className="space-y-6">
      <nav aria-label="Breadcrumb" className="flex flex-wrap items-center text-sm text-foreground-muted">
        <Link href="/workspace/teams" prefetch={false} className="hover:text-foreground hover:underline">Teams</Link>
        <span className="px-1.5" aria-hidden>/</span>
        <span className="text-foreground">Run unavailable</span>
      </nav>
      <HonestState
        variant="not_wired"
        title="This run isn’t available to the current identity"
        detail={`Run ${run} may belong to another team owner, or its disposable record expired before a durable archive was retained. Switch to the identity that owns the team (for local teams, usually “operator”) and try again.`}
      />
    </div>
  );
}

function archivedPullRequests(
  text: string,
  repos: string[],
): Array<{ repo: string; number: number; url: string }> {
  const found = new Map<string, { repo: string; number: number; url: string }>();
  for (const segment of text.split("github.com/").slice(1)) {
    const match = segment.match(/^([^/\s]+)\/([^/\s]+)\/pulls?\/(\d+)/);
    if (!match) continue;
    const repo = `${match[1]}/${match[2]}`;
    const number = Number(match[3]);
    const url = `https://github.com/${repo}/pull/${number}`;
    found.set(url, { repo, number, url });
  }
  if (repos.length === 1) {
    for (const match of text.matchAll(/\bPR\s*#(\d+)\b/gi)) {
      const number = Number(match[1]);
      const repo = repos[0];
      const url = `https://github.com/${repo}/pull/${number}`;
      found.set(url, { repo, number, url });
    }
  }
  return [...found.values()];
}

type RunTab = {
  id: string;
  label: string;
  badge?: number | null;
  node: ReactNode;
};

function TeamRunTabs({
  tabs,
  active,
  basePath,
}: {
  tabs: RunTab[];
  active?: string;
  basePath: string;
}) {
  const current = tabs.find((tab) => tab.id === active) ?? tabs[0];
  return (
    <div>
      <div
        role="tablist"
        aria-label="Team run sections"
        className="sticky top-[57px] z-10 -mx-1 mb-5 flex gap-1 overflow-x-auto rounded-xl border border-border bg-surface/80 p-1 backdrop-blur supports-[backdrop-filter]:bg-surface/70"
      >
        {tabs.map((tab) => {
          const selected = tab.id === current.id;
          return (
            <Link
              key={tab.id}
              href={`${basePath}?tab=${encodeURIComponent(tab.id)}`}
              prefetch={false}
              role="tab"
              aria-selected={selected}
              className={`relative flex shrink-0 items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-sm font-medium transition ${
                selected
                  ? "bg-signal/10 text-foreground"
                  : "text-foreground-muted hover:bg-surface-muted hover:text-foreground"
              }`}
            >
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
      <div role="tabpanel" className="kb-rise space-y-6">
        {current.node}
      </div>
    </div>
  );
}

function ArtifactBody({
  artifact,
  downloadHref,
}: {
  artifact: import("@/lib/types").MissionArtifact;
  downloadHref: string;
}) {
  const actions = (
    <span className="flex flex-wrap gap-2">
      <a
        href={downloadHref}
        target="_blank"
        rel="noreferrer"
        className="inline-flex rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs font-medium text-signal hover:bg-surface-muted"
      >
        Open full artifact ↗
      </a>
      <a
        href={downloadHref}
        download={artifact.name}
        className="inline-flex rounded-lg bg-signal px-2.5 py-1.5 text-xs font-medium text-signal-fg hover:opacity-90"
      >
        Download
      </a>
    </span>
  );
  if (artifact.content == null) {
    return (
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-foreground-muted">
          {artifact.content_truncated ? "Preview omitted to keep this run page responsive." : "Binary artifact retained."}
        </p>
        {actions}
      </div>
    );
  }
  if (artifact.content.length === 0) {
    return (
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-foreground-muted">Empty text artifact retained.</p>
        {actions}
      </div>
    );
  }
  if (artifact.content_truncated) {
    return (
      <div className="space-y-3">
        <p className="text-xs text-foreground-muted">
          Showing a bounded preview of {(artifact.content_bytes ?? artifact.size_bytes ?? 0).toLocaleString()} bytes.
        </p>
        <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-surface-muted/30 p-3 font-mono text-xs">
          {artifact.content}
        </pre>
        {actions}
      </div>
    );
  }
  if (artifact.name.endsWith(".json")) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(artifact.content);
    } catch {
      // Invalid JSON remains inspectable as text instead of disappearing.
      return (
        <div className="space-y-3">
          <DeliverableBody output={artifact.content} />
          {actions}
        </div>
      );
    }
    return (
      <div className="max-h-[42rem] overflow-auto rounded-lg border border-border bg-surface-muted/30 p-3">
        <StructuredJson value={parsed} />
        <div className="mt-3">{actions}</div>
      </div>
    );
  }
  return (
    <div className="space-y-3">
      <DeliverableBody output={artifact.content} />
      {actions}
    </div>
  );
}

function StructuredJson({ value, depth = 0 }: { value: unknown; depth?: number }) {
  if (value === null || typeof value !== "object") {
    return <span className="break-words font-mono text-xs">{String(value)}</span>;
  }
  if (depth >= 3) {
    return (
      <pre className="whitespace-pre-wrap break-words font-mono text-xs">
        {JSON.stringify(value, null, 2)}
      </pre>
    );
  }
  if (Array.isArray(value)) {
    return (
      <ol className="space-y-2">
        {value.map((entry, index) => (
          <li key={index} className="rounded-md border border-border bg-surface px-3 py-2">
            <span className="mb-1 block text-[10px] font-medium uppercase tracking-wide text-foreground-muted">Item {index + 1}</span>
            <StructuredJson value={entry} depth={depth + 1} />
          </li>
        ))}
      </ol>
    );
  }
  return (
    <dl className="divide-y divide-border">
      {Object.entries(value as Record<string, unknown>).map(([key, entry]) => (
        <div key={key} className="grid gap-1 py-2 sm:grid-cols-[12rem_minmax(0,1fr)] sm:gap-3">
          <dt className="break-words font-mono text-[11px] font-medium text-foreground-muted">{key}</dt>
          <dd className="min-w-0"><StructuredJson value={entry} depth={depth + 1} /></dd>
        </div>
      ))}
    </dl>
  );
}
