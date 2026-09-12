// kars Bridge Workspace — Team detail. The standing org's command surface:
// its charter, who it watches and on what cadence, its org chart (principal +
// roster, each a verified subset of the team's authority), and the live
// history of task-force runs the charter loop has generated autonomously.

import Link from "next/link";
import { notFound } from "next/navigation";
import { HonestState } from "@/components/honest-state";
import { TeamEdit } from "./team-edit";
import { LiveRefresh, LivePulse } from "@/components/live-refresh";
import { EnvelopeDigest } from "@/components/envelope-digest";
import {
  getEngineeringSource,
  getGithubConnection,
  getOptions,
  getTask,
  getTeam,
  getTeamChannels,
  getTeamCommons,
  getTeamLedger,
  listTaskApprovals,
} from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { humanizeMcp } from "@/lib/format";
import { formatWarmIdle } from "@/lib/format";
import {
  TIER_LABELS,
  type CommonsResponse,
  type EngineeringSource,
  type GithubConnection,
  type LedgerEvent,
  type TeamChannelsState,
  type TeamDetail,
} from "@/lib/types";
import { WatchingStatus } from "./watching-status";
import { TeamLedger } from "./team-ledger";
import { PromoteControl } from "./promote-control";
import { RunNowControl } from "./run-control";
import { DeleteTeamControl } from "./delete-control";
import { TeamTasks } from "./team-tasks";
import { TeamChannels } from "./team-channels";
import { DeliverableBody, toPlainPreview } from "@/components/deliverable-view";
import { TeamRosterEdit } from "./team-roster-edit";
import { OrgTree } from "@/components/org-tree";
import { DeployTimeline } from "@/app/workspace/missions/[name]/deploy-timeline";
import { JourneyRail, teamBeat } from "@/components/journey-rail";
import { analyzeTeamRun } from "@/lib/team-run-evidence";
import { TeamRunFlow } from "@/components/team-run-flow";
import { ExecutionExplorer } from "@/components/execution-explorer";
import { Access, NowHero, TeamServerTabs } from "./team-detail-panels";
import { EngineeringIntake } from "./engineering-intake";
import { TeamTiming } from "@/components/team-timing";
import {
  RecentTeamOutcomes,
  TeamRunHistory,
  TeamValueSummary,
} from "./team-outcomes";

export const dynamic = "force-dynamic";

function healthLabel(health: string): string {
  return health === "AwaitingReview" ? "Awaiting review" : health;
}

function HealthChip({ health }: { health: string }) {
  const tone: Record<string, string> = {
    Healthy: "bg-emerald-500/10 text-emerald-600 border-emerald-500/30",
    Watching: "bg-sky-500/10 text-sky-600 border-sky-500/30",
    AwaitingReview: "bg-amber-500/10 text-amber-600 border-amber-500/30",
    Unproductive: "bg-amber-500/10 text-amber-600 border-amber-500/30",
    Stalled: "bg-rose-500/10 text-rose-600 border-rose-500/30",
    Hibernating: "bg-surface-muted text-foreground-muted border-border",
  };
  const cls = tone[health ?? ""] ?? "bg-surface-muted text-foreground-muted border-border";
  return (
    <span className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${cls}`}>
      {healthLabel(health)}
    </span>
  );
}

export default async function TeamDetailPage({
  params,
  searchParams,
}: {
  params: Promise<{ name: string }>;
  searchParams: Promise<{ tab?: string }>;
}) {
  const { name } = await params;
  const { tab } = await searchParams;
  const ns = defaultNamespace();
  let team: TeamDetail;
  try {
    team = await getTeam(ns, name);
  } catch (err) {
    const msg = err instanceof Error ? err.message : "";
    if (msg.includes("not_found") || msg.includes("404")) notFound();
    return (
      <HonestState
        variant="not_wired"
        title="This team is unavailable"
        detail="The run environment isn't reachable right now. Try again shortly."
      />
    );
  }

  const active = !team.paused && team.phase === "Active";
  const runtimeWorking = !team.paused && team.runtime_state === "Working";
  const latestRun =
    (team.current_assignment_nonce ? team.principal_task : null)
    ?? [...team.generated_tasks].sort().reverse()[0];

  // The latest task-force run's live detail — so the team page can fold out the
  // SAME "watch it work" experience a mission gets: the deploy timeline, the
  // auto-folding agent graph, and the live per-round / per-tool activity feed
  // for the run executing right now. "In flight" = launched and not yet
  // delivered (materializing OR running); "running" once the agent works.
  const latestRunTask = latestRun
    ? await getTask(ns, latestRun).catch(() => null)
    : null;
  const allLatestRunApprovals = latestRun
    ? await listTaskApprovals(ns, latestRun).catch(() => [])
    : [];
  const latestRunApprovals = allLatestRunApprovals.filter(
    (approval) =>
      approval.run_nonce == null
      || latestRunTask?.current_run_nonce == null
      || approval.run_nonce === latestRunTask.current_run_nonce,
  );
  const awaitingAssignment = Boolean(
    latestRunTask?.current_run_nonce
    && latestRunTask.assignment?.task_id !== latestRunTask.current_run_nonce,
  );
  const assignmentInFlight = Boolean(
    latestRunTask?.assignment?.completed_at == null
    && (
      latestRunTask?.assignment?.state === "Assigned"
      || latestRunTask?.assignment?.state === "Running"
    ),
  );
  const runInFlight = Boolean(
    latestRunTask
    && latestRunTask.launched
    && !team.paused
    && (awaitingAssignment || assignmentInFlight),
  );
  const runRunning = Boolean(
    runInFlight && latestRunTask?.execution_phase === "Running",
  );
  // Creation redirects here before the team reconciler necessarily stamps
  // phase=Active. Poll any unpaused team with no run yet so the kickoff appears
  // without a manual refresh even while admission is still converging.
  const awaitingFirstRun = !team.paused && team.generated_task_count === 0;
  const runActivity = latestRunTask?.activity ?? [];
  const hasRunActivity = runActivity.length > 0;
  const latestRunEvidence = latestRunTask ? analyzeTeamRun(team, latestRunTask) : null;

  let commons: CommonsResponse | null = null;
  try {
    commons = await getTeamCommons(ns, name);
  } catch {
    commons = null;
  }

  let ledger: LedgerEvent[] = [];
  try {
    ledger = await getTeamLedger(ns, name);
  } catch {
    ledger = [];
  }

  let options = null;
  try {
    options = await getOptions();
  } catch {
    options = null;
  }

  let engineeringSource: EngineeringSource | null = null;
  let githubConnection: GithubConnection | null = null;
  let teamChannels: TeamChannelsState | null = null;
  const [sourceResult, connectionResult, channelsResult] = await Promise.allSettled([
    getEngineeringSource(ns, name),
    getGithubConnection(ns),
    getTeamChannels(ns, name),
  ]);
  if (sourceResult.status === "fulfilled") engineeringSource = sourceResult.value;
  if (connectionResult.status === "fulfilled") githubConnection = connectionResult.value;
  if (channelsResult.status === "fulfilled") teamChannels = channelsResult.value;
  const queuedTasks = team.tasks.filter((task) => task.status === "pending").length;
  const activeTasks = team.tasks.filter((task) => task.status === "active").length;
  const awaitingReview = team.health === "AwaitingReview";

  return (
    <div className="space-y-6">
      {/* Refresh aggressively only while a task-force run is changing. Refreshing
          an idle standing team every five seconds resets open edit forms and makes
          the launch package effectively uneditable. */}
      <LiveRefresh active={runInFlight || runtimeWorking || awaitingFirstRun} intervalMs={5000} />

      <div className="flex items-start justify-between gap-4">
        <div>
        <div className="flex items-center gap-3">
          <Link href="/workspace/teams" className="text-xs text-foreground-muted hover:underline">
            ← Teams
          </Link>
          {active && <LivePulse label={awaitingReview ? "Awaiting review" : "On watch"} />}
        </div>
        <h1 className="mt-2 text-2xl font-semibold tracking-tight">
          {team.display_name ?? team.name}
        </h1>
        {team.reporting_to && (
          <p className="mt-1 text-sm text-foreground-muted">Reports to {team.reporting_to}</p>
        )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <RunNowControl team={team.name} paused={team.paused} inFlight={runInFlight} />
          <DeleteTeamControl team={team.name} />
        </div>
      </div>

      {/* Journey spine — same seven beats as a mission; a standing team lives
          mostly in Run, cycling through Build->Run on each cadence tick. */}
      <JourneyRail
        current={teamBeat({ paused: team.paused, everRan: team.generated_task_count > 0 })}
        paused={team.paused && team.generated_task_count > 0}
      />

      {/* "Now" hero — the single answer to "what is this team doing right now". */}
      <NowHero
        teamName={team.name}
        health={team.paused ? "Hibernating" : team.health}
        active={active}
        runRunning={runRunning}
        runInFlight={runInFlight}
        everyMinutes={team.every_minutes ?? null}
        commonsEntries={team.commons_entry_count}
        nextRunAt={team.next_run_at}
        lastRunAt={team.last_run_at}
        delivered={team.runs_succeeded}
        generated={team.generated_task_count}
        latestRun={latestRun}
        latestOutcome={latestRunEvidence?.outcome ?? null}
        lifecycleMode={team.lifecycle_mode}
        runtimeState={team.runtime_state}
        idleDeadlineAt={team.idle_deadline_at}
      />
      <TeamTiming
        lastActivityAt={team.last_activity_at}
        nextActivityAt={team.paused || awaitingReview ? null : team.next_run_at}
        paused={team.paused}
      />

      {/* Budget stop — a standing team whose daily/monthly cap is exhausted mints
          no new runs until the cap is raised. Surface it as an actionable state,
          not a silent stall the operator has to infer from "no recent runs". */}
      {!team.paused && /budget/i.test(team.detail ?? "") && /(exhaust|exceeded|cap)/i.test(team.detail ?? "") && (
        <div className="rounded-xl border border-warning/50 bg-warning/[0.07] px-4 py-3">
          <div className="flex items-start gap-2">
            <span aria-hidden className="text-warning">⏸</span>
            <div className="min-w-0">
              <p className="text-sm font-medium">Team paused on budget — no new runs until the cap is raised.</p>
              <p className="mt-0.5 text-xs text-foreground-muted">
                {team.detail} Raise the team&apos;s token budget in Edit, inspect spend in the ledger below, or
                pause the team if this is expected.
              </p>
            </div>
          </div>
        </div>
      )}

      <TeamServerTabs
        active={tab ?? (latestRunTask && hasRunActivity ? "activity" : "overview")}
        basePath={`/workspace/teams/${encodeURIComponent(team.name)}`}
        tabs={[
          ...(latestRun && latestRunTask && (runInFlight || hasRunActivity)
            ? [
                {
                  id: "activity",
                  label: "Execution flow",
                  live: runInFlight,
                  badge: latestRunEvidence
                    ? latestRunEvidence.collaboration.length + latestRunEvidence.research.length
                    : hasRunActivity ? runActivity.length : null,
                  node: (
                    <div className="space-y-4">
                      <ExecutionExplorer
                        running={runInFlight}
                        activity={latestRunTask.activity}
                        telemetry={latestRunTask.telemetry}
                        assignmentEvents={latestRunTask.assignment_events}
                        approvals={latestRunApprovals}
                        ns={ns}
                        name={latestRun}
                        agentLabel={team.display_name ?? team.name}
                        agentPhase={
                          awaitingAssignment
                            ? "Launching"
                            : team.paused
                              ? "Hibernating"
                              : latestRunTask.assignment?.state ?? latestRunTask.execution_phase ?? latestRunTask.phase
                        }
                        agentRuntime={latestRunTask.composition?.runtime}
                        agentModel={latestRunTask.composition?.model}
                        subAgents={latestRunTask.sub_agents}
                        identity={latestRunTask.agent_identity}
                        envelopeDigest={latestRunTask.envelope_digest}
                      />
                      <div className="rounded-xl border border-border bg-surface-muted/40 px-4 py-3 text-sm">
                        <span className="text-foreground-muted">
                          {runInFlight
                            ? "The team's current task-force run, live — watch it deploy and work, then read the deliverable in Runs."
                            : "The team's most recent task-force run."}
                        </span>{" "}
                        <Link
                          href={`/workspace/teams/${encodeURIComponent(team.name)}/runs/${encodeURIComponent(latestRun)}`}
                          className="font-medium text-signal hover:underline"
                        >
                          Open the full run →
                        </Link>
                      </div>
                      {runInFlight && <DeployTimeline task={latestRunTask} />}
                      {latestRunEvidence && (
                        <TeamRunFlow task={latestRunTask} evidence={latestRunEvidence} />
                      )}
                    </div>
                  ),
                },
              ]
            : []),
          {
            id: "overview",
            label: "Overview",
            node: (
              <div className="space-y-6">
      <TeamValueSummary
        summary={team.recent_outcome_summary}
        generated={team.generated_task_count}
        retained={team.recent_outcomes.length}
        queued={queuedTasks}
        active={activeTasks}
        tokens={team.tokens_spent_total}
      />
      <RecentTeamOutcomes team={team.name} outcomes={team.recent_outcomes} />
      {/* Charter + watching status side by side */}
      <div className="grid gap-4 md:grid-cols-3">
        <section className="rounded-xl border border-border bg-surface p-5 md:col-span-2">
          <div className="flex items-start justify-between gap-3">
            <h2 className="text-sm font-semibold">Charter</h2>
            {team.health && <HealthChip health={team.paused ? "Hibernating" : team.health} />}
          </div>
          <p className="mt-2 text-sm leading-relaxed text-foreground">{team.charter}</p>
          <div className="mt-4 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-foreground-muted">
            <span>
              Tier {team.tier} · {TIER_LABELS[team.tier] ?? "?"}
            </span>
            <span>Grants members up to Tier {team.authority_ceiling}</span>
            <span>Delegation depth {team.delegation_depth}</span>
            <span>
              Runtime{" "}
              {team.lifecycle_mode === "resourceOptimized"
                ? `resource optimized · ${formatWarmIdle(team.warm_idle_seconds)}`
                : team.lifecycle_mode}
            </span>
          </div>
          <div className="mt-4">
            <TeamEdit
              ns={ns}
              name={team.name}
              charter={team.charter}
              paused={team.paused}
              everyMinutes={team.every_minutes}
              lifecycleMode={team.lifecycle_mode}
              warmIdleSeconds={team.warm_idle_seconds}
              mcpServers={team.mcp_servers}
              availableMcp={(options?.mcp_servers ?? []).filter((server) => server.namespace === ns)}
              egress={team.egress}
              egressMode={team.egress_mode}
              model={team.model}
              modelFallbacks={team.model_fallbacks}
              models={options?.models ?? []}
              memory={team.memory}
              availableMemories={(options?.memories ?? []).filter((memory) => memory.namespace === ns)}
              gitWriteRepos={team.git_write_repos}
              executionPlan={team.execution_plan}
            />
          </div>
        </section>
        <WatchingStatus
          nextRunAt={team.next_run_at}
          lastRunAt={team.last_run_at}
          everyMinutes={team.every_minutes}
          lifecycleMode={team.lifecycle_mode}
          warmIdleSeconds={team.warm_idle_seconds}
          runtimeState={team.runtime_state}
          currentAssignment={team.current_assignment_task ?? team.current_assignment_nonce}
          idleDeadlineAt={team.idle_deadline_at}
          memoryEntries={team.commons_entry_count}
          paused={team.paused}
          health={team.health}
        />
      </div>

              </div>
            ),
          },
          {
            id: "work",
            label: "Work queue",
            badge: queuedTasks + activeTasks,
            node: (
              <div className="space-y-6">
                <EngineeringIntake
                  team={team.name}
                  initialSource={engineeringSource}
                  connection={githubConnection}
                />
                <TeamTasks team={team.name} tasks={team.tasks} paused={team.paused} />
              </div>
            ),
          },
          {
            id: "org",
            label: "Org & access",
            badge: team.roster.length,
            node: (
              <div className="space-y-6">
      {/* Accesses — what this team can use and reach, for management at a glance. */}
      <section className="rounded-xl border border-border bg-surface p-6">
        <h2 className="text-sm font-semibold">Accesses</h2>
        <p className="mt-0.5 text-xs text-foreground-muted">What the team is allowed to use and reach. Members get a verified subset; nothing wider. Values marked <span className="font-medium">default</span> are inherited from the cluster, not explicitly set on this team.</p>
        <dl className="mt-4 grid gap-3 sm:grid-cols-2">
          <Access label="Model" value={team.model ?? "controller default"} isDefault={team.model_default} />
          <Access label="Harness" value={team.runtime ?? "OpenClaw"} isDefault={team.runtime_default} />
          <Access
            label="Runtime lifecycle"
            value={
              team.lifecycle_mode === "resourceOptimized"
                ? `resource optimized · suspends ${formatWarmIdle(team.warm_idle_seconds) === "immediately" ? "immediately when idle" : `after ${formatWarmIdle(team.warm_idle_seconds)} idle`}`
                : team.lifecycle_mode
            }
            isDefault={team.lifecycle_mode === "ephemeral"}
          />
          <Access label="Isolation" value={team.isolation ?? "standard"} isDefault={team.isolation === null} />
          <Access label="Tool policy" value={team.tool_policy ?? "none — model only"} isDefault={team.tool_policy_default} />
          <Access label="Shared memory" value={team.knowledge_commons ?? `${team.name} (default)`} isDefault={team.knowledge_commons === null} />
          <Access label="Connected services (MCP)" value={team.mcp_servers.length ? team.mcp_servers.map(humanizeMcp).join(", ") : "none connected"} isDefault={team.mcp_servers.length === 0} />
          <Access
            label="Network egress"
            value={team.egress.length ? team.egress.join(", ") : team.network_posture}
            isDefault={team.egress.length === 0}
          />
          {team.learned_egress.length > 0 && (
            <Access
              label="Domains reached (live)"
              value={team.learned_egress.join(", ")}
            />
          )}
          <Access
            label="Reports via"
            value={team.channels.length ? team.channels.join(", ") : "no channels — Inbox only"}
            isDefault={team.channels.length === 0}
          />
        </dl>
      </section>

      <TeamChannels
        team={team.name}
        enabled={teamChannels?.enabled ?? team.channels}
        statuses={teamChannels?.statuses ?? []}
      />

      {/* Org chart */}
      <section className="rounded-xl border border-border bg-surface p-6">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="text-sm font-semibold">Org chart</h2>
            <p className="mt-0.5 text-xs text-foreground-muted">
              The standing org. Each member&apos;s authority is a verified subset of the team&apos;s —
              the reporting line is the trust boundary.
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {options ? (
              <TeamRosterEdit
                ns={ns}
                name={team.name}
                roster={team.roster}
                options={{
                  ...options,
                  skills: options.skills.filter((skill) => skill.namespace === ns),
                  mcp_servers: options.mcp_servers.filter((server) => server.namespace === ns),
                  memories: options.memories.filter((memory) => memory.namespace === ns),
                }}
              />
            ) : (
              <button
                type="button"
                disabled
                title="Org editing is unavailable — the run environment is unreachable right now."
                className="cursor-not-allowed rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted opacity-60"
              >
                Edit org
              </button>
            )}
            {!team.paused && <PromoteControl team={team.name} currentTier={team.tier} />}
          </div>
        </div>
        <div className="mt-4">
          <OrgTree
            principal={{
              id: "principal",
              title: team.display_name ?? team.name,
              role: `Principal · team lead · Tier ${team.tier} · grants up to Tier ${team.authority_ceiling}`,
              status: "principal",
              chips: [
                {
                  label: team.runtime ?? "OpenClaw",
                  tone: "accent",
                },
                {
                  label: team.model ?? "cluster default model",
                  tone: "muted",
                },
                {
                  label: team.tool_policy ?? "kars-default",
                  tone: "signal",
                },
              ],
            }}
            members={team.roster.map((role) => ({
              id: role.name,
              title: role.name,
              role: role.tier != null ? `Tier ${role.tier} · ${TIER_LABELS[role.tier] ?? "?"}` : undefined,
              detail: role.system_prompt || undefined,
              status: role.member_task ? "verified" : "pending",
              chips: [
                { label: role.runtime ?? "team default", tone: "accent" as const },
                { label: role.model ?? "team default" },
                ...role.skills.map((s) => ({ label: s, tone: "muted" as const })),
              ],
            }))}
          />
          {team.roster.length === 0 && (
            <p className="mt-2 px-3 py-2 text-center text-xs text-foreground-muted">
              No member roles — the team operates through its charter loop alone.
            </p>
          )}
        </div>
      </section>

              </div>
            ),
          },
          {
            id: "runs",
            label: "Outcomes",
            badge: team.recent_outcomes.length,
            node: (
              <div className="space-y-6">
                <TeamRunHistory
                  team={team.name}
                  runs={[
                    ...new Set([
                      ...team.recent_outcomes.map((outcome) => outcome.run),
                      ...team.generated_tasks,
                    ]),
                  ]}
                  outcomes={team.recent_outcomes}
                  generated={team.generated_task_count}
                />

              </div>
            ),
          },
          {
            id: "knowledge",
            label: "Memory",
            badge: commons?.count ?? 0,
            node: (
              <div className="space-y-6">
      {/* Shared memory — the team's knowledge commons */}
      <section className="rounded-xl border border-border bg-surface p-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-sm font-semibold">Shared memory</h2>
            <p className="mt-0.5 text-xs text-foreground-muted">
              The team&apos;s knowledge commons — what each run learned, accumulated with
              provenance. New runs build on this instead of starting cold.
            </p>
          </div>
          <span className="rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium text-foreground-muted">
            {commons?.count ?? 0} entries
          </span>
        </div>
        {!commons || commons.entries.length === 0 ? (
          <p className="mt-4 text-xs text-foreground-muted">
            No shared knowledge yet. The first standing-operation run to complete will deposit what
            it learned here.
          </p>
        ) : (
          <>
          {commons.count > commons.entries.length && (
            <p className="mt-3 text-[11px] text-foreground-muted">
              Showing the {commons.entries.length} most recent of {commons.count} entries.
            </p>
          )}
          <ul className="mt-4 space-y-3">
            {commons.entries.map((e) => (
              <li key={e.id}>
                <details className="group rounded-lg border border-border bg-surface-muted/40">
                  <summary className="cursor-pointer list-none p-4">
                    <div className="flex items-start justify-between gap-3">
                      <p className="text-sm font-medium">{e.title}</p>
                      <span className="shrink-0 font-mono text-[10px] text-foreground-muted">
                        {e.digest}
                      </span>
                    </div>
                    {e.content && (
                      <p className="mt-1 line-clamp-2 text-xs text-foreground-muted group-open:hidden">
                        {toPlainPreview(e.content)}
                      </p>
                    )}
                    <p className="mt-2 text-[11px] text-foreground-muted">
                      Learned by{" "}
                      <Link
                        href={`/workspace/teams/${encodeURIComponent(team.name)}/runs/${encodeURIComponent(e.source_task)}`}
                        className="font-mono hover:underline"
                      >
                        {e.author}
                      </Link>{" "}
                      · {new Date(e.created_at).toLocaleString()} · click to read
                    </p>
                  </summary>
                  {e.content && (
                    <div className="border-t border-border bg-surface px-4 py-4">
                      <DeliverableBody output={e.content} />
                    </div>
                  )}
                </details>
              </li>
            ))}
          </ul>
          </>
        )}
      </section>

              </div>
            ),
          },
          {
            id: "ledger",
            label: "Diagnostics",
            node: (
              <div className="space-y-6">
      {/* Continuous ledger (§14) — the streaming record of everything done */}
      <section className="rounded-xl border border-border bg-surface p-6">
        <h2 className="text-sm font-semibold">Diagnostic ledger</h2>
        <p className="mt-0.5 text-xs text-foreground-muted">
          Low-level deliveries, failures, knowledge writes, token evidence, and digests. Use the
          Outcomes tab for the customer-facing record.
        </p>
        {ledger.length === 0 ? (
          <p className="mt-4 text-xs text-foreground-muted">No activity recorded yet.</p>
        ) : (
          <TeamLedger team={team.name} ledger={ledger} />
        )}
      </section>

      {/* Provenance */}
      <section className="rounded-xl border border-border bg-surface p-6">
        <h2 className="text-sm font-semibold">Provenance</h2>
        <dl className="mt-3 grid gap-3 sm:grid-cols-2">
          <div>
            <dt className="text-xs text-foreground-muted">Trust envelope</dt>
            <dd className="mt-1">
              {team.envelope_digest ? (
                <EnvelopeDigest digest={team.envelope_digest} />
              ) : (
                <span className="text-xs text-foreground-muted">pending</span>
              )}
            </dd>
          </div>
          <div>
            <dt className="text-xs text-foreground-muted">Knowledge commons</dt>
            <dd className="mt-1 text-sm">
              {team.knowledge_commons ?? (
                <span className="text-foreground-muted">{team.name} (default)</span>
              )}
            </dd>
          </div>
        </dl>
        {team.detail && <p className="mt-4 text-xs text-foreground-muted">{team.detail}</p>}
      </section>
              </div>
            ),
          },
        ]}
      />
    </div>
  );
}
