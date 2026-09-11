// kars Bridge Workspace — Home. Action-led, not a stat dashboard: the single
// "Start a mission" entry point + your in-flight missions + anything waiting on
// you. Honest empty state on a fresh cluster (no zeroed cards that imply
// measurement).

import Link from "next/link";
import { IntentEntry } from "@/components/intent-entry";
import { HowItWorksSteps } from "@/components/how-it-works";
import { Stat } from "@/components/ui";
import { MissionStatusBadge, missionStatus } from "@/components/mission-status";
import {
  BffError,
  getArtifacts,
  getDigests,
  getTask,
  getTeam,
  listApprovals,
  listTasks,
  listTeams,
} from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import {
  TIER_LABELS,
  type TaskSummary,
  type Approval,
  type TeamSummary,
  type Digest,
  type MissionArtifacts,
} from "@/lib/types";
import { analyzeTeamRun } from "@/lib/team-run-evidence";

export const dynamic = "force-dynamic";

function teamHealthDot(health: string | null, paused: boolean): string {
  const h = paused ? "Hibernating" : (health ?? "");
  if (h === "Healthy") return "bg-emerald-500";
  if (h === "Stalled") return "bg-rose-500";
  if (h === "Unproductive") return "bg-amber-500";
  if (h === "Watching") return "bg-sky-500";
  return "bg-foreground-muted";
}

function completedHeadline(item: MissionArtifacts): string {
  const pr = item.pull_requests[0];
  if (pr) return `Change proposed · ${pr.repo} PR #${pr.number}`;
  return item.excerpt || item.display_name || item.objective || "Completed outcome";
}

export default async function WorkspaceHome() {
  const ns = defaultNamespace();
  let tasks: TaskSummary[] = [];
  let approvals: Approval[] = [];
  let teams: TeamSummary[] = [];
  let digests: Digest[] = [];
  let artifacts: MissionArtifacts[] = [];
  let backendError: string | null = null;
  try {
    [tasks, approvals, teams, digests, artifacts] = await Promise.all([
      listTasks(ns),
      listApprovals(ns, { pending: true }).catch(() => []),
      listTeams(ns).catch(() => []),
      getDigests().catch(() => []),
      getArtifacts()
        .then((i) => i.missions)
        .catch(() => []),
    ]);
  } catch (err) {
    backendError =
      err instanceof BffError ? err.code : err instanceof Error ? err.message : "unknown";
  }

  const waiting = approvals.filter((a) => a.actionable);
  // Standalone missions only — team machinery (principal/members/standing runs)
  // lives on the Team surface, not in the user's mission list.
  const standaloneTasks = tasks.filter((t) => !t.team);
  // A run minted by a standing team's charter loop (`<team>-run-<epoch>`) is
  // autonomous — it must NOT appear in the user's personal review queue. Also
  // exclude anything whose task name is owned by a team (any `<team>-…` object),
  // so team machinery never inflates the user's "To review" KPI (audit f2).
  const isTeamOwned = (task: string) =>
    teams.some((t) => task === t.name || task.startsWith(`${t.name}-`));
  const isStandingRun = (task: string) =>
    teams.some((t) => task.startsWith(`${t.name}-run-`));
  // Deliverables from the user's own missions that have landed and still need a
  // human decision (§16) — excludes autonomous standing-team output.
  const toReview = artifacts.filter(
    (m) =>
      m.summary &&
      m.status !== "error" &&
      m.review_status !== "approved" &&
      !isStandingRun(m.task) &&
      !isTeamOwned(m.task),
  );
  // One card per team — the latest digest — so a stalled team doesn't flood the
  // home with repeated identical messages.
  const latestDigestByTeam = Array.from(
    digests
      .reduce((acc, d) => {
        const prev = acc.get(d.team);
        if (!prev || (d.at ?? "") > (prev.at ?? "")) acc.set(d.team, d);
        return acc;
      }, new Map<string, Digest>())
      .values(),
  ).sort((a, b) => (b.at ?? "").localeCompare(a.at ?? ""));
  const completedTeam = (mission: MissionArtifacts) =>
      mission.team
        ? teams.find((team) => team.name === mission.team)
        : teams.find((team) => mission.task.startsWith(`${team.name}-run-`));
  const completedCandidates = artifacts
      .filter((m) => m.status !== "error" && m.finished_at != null)
      .sort(
        (a, b) =>
          (Number(Boolean(completedTeam(b))) * 2 +
            Number(b.review_status === "approved")) -
            (Number(Boolean(completedTeam(a))) * 2 +
              Number(a.review_status === "approved")) ||
          (b.finished_at ?? "").localeCompare(a.finished_at ?? ""),
      )
      .slice(0, 12);
  const teamOutcomeByTask = new Map<string, ReturnType<typeof analyzeTeamRun>["outcome"]>();
  const teamDetails = new Map(
    await Promise.all(
      Array.from(
       new Set(
         completedCandidates
           .map((candidate) => completedTeam(candidate)?.name)
           .filter((name): name is string => Boolean(name)),
       ),
      ).map(async (teamName) => [
       teamName,
       await getTeam(ns, teamName).catch(() => null),
      ] as const),
    ),
  );
  await Promise.all(
    completedCandidates.flatMap((candidate) => {
      const team = completedTeam(candidate);
      if (!team) return [];
      return [getTask(ns, candidate.task).catch(() => null).then((task) => {
       const detail = teamDetails.get(team.name);
       if (detail && task) {
         teamOutcomeByTask.set(candidate.task, analyzeTeamRun(detail, task).outcome);
       }
      })];
    }),
  );
  const recentCompleted = completedCandidates
    .filter((candidate) => {
      const team = completedTeam(candidate);
      if (!team) return true;
      const outcome = teamOutcomeByTask.get(candidate.task);
      return outcome === "delivered" || outcome === "delivered_with_issues";
    })
    .slice(0, 6);
  const completedHref = (mission: MissionArtifacts) => {
      const team = completedTeam(mission);
      if (!team) return `/workspace/missions/${encodeURIComponent(mission.task)}`;
      return mission.evidence_key && mission.evidence_key !== mission.task
        ? `/workspace/teams/${encodeURIComponent(team.name)}?tab=runs`
        : `/workspace/teams/${encodeURIComponent(team.name)}/runs/${encodeURIComponent(mission.task)}`;
  };

  return (
    <div className="space-y-8">
      {/* Hero: one intent → the orchestrator routes it. */}
      <section className="relative overflow-hidden rounded-3xl border border-border bg-gradient-to-br from-surface via-surface to-surface-muted/50 p-8 shadow-lg sm:p-10 kb-canvas">
        <div aria-hidden className="pointer-events-none absolute -right-20 -top-24 h-72 w-72 rounded-full bg-signal/15 blur-3xl" />
        <div aria-hidden className="pointer-events-none absolute -bottom-24 -left-16 h-64 w-64 rounded-full bg-accent/10 blur-3xl" />
        <div className="relative">
          <span className="inline-flex items-center gap-1.5 rounded-full border border-signal/25 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold uppercase tracking-wide text-signal">
            <span className="kb-pulse inline-block h-1.5 w-1.5 rounded-full bg-signal" />
            AI orchestrator
          </span>
          <h1 className="mt-3 text-3xl font-semibold tracking-tight sm:text-[2.4rem] sm:leading-[1.1]">What do you want done?</h1>
          <p className="mt-2 max-w-xl text-[15px] leading-relaxed text-foreground-muted">
            Describe an outcome. Bridge decides whether it&rsquo;s a one-off mission or a standing team,
            composes a governed agent — or a whole org — you review the plan, then get back verifiable
            work with a signed receipt.
          </p>
          <div className="mt-6 sm:max-w-2xl">
            <IntentEntry />
          </div>
          <details className="mt-4 sm:max-w-2xl">
            <summary className="cursor-pointer text-xs text-foreground-muted hover:text-foreground">
              Or start from scratch
            </summary>
            <div className="mt-3 grid gap-3 sm:grid-cols-2">
              <Link href="/workspace/new" className="kb-card kb-card-hover group flex items-start gap-3 p-4">
                <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-signal/15 text-signal">
                  <svg viewBox="0 0 16 16" className="h-4 w-4" fill="currentColor" aria-hidden><path d="M8 2.5v11M2.5 8h11" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" /></svg>
                </span>
                <div>
                  <p className="text-sm font-semibold">Start a mission</p>
                  <p className="mt-0.5 text-xs text-foreground-muted">A one-off task — compose, review, launch, done.</p>
                </div>
              </Link>
              <Link href="/workspace/teams/new" className="kb-card kb-card-hover group flex items-start gap-3 p-4">
                <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-accent/15 text-accent">
                  <svg viewBox="0 0 20 20" className="h-4 w-4" fill="currentColor" aria-hidden><path d="M7 9a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Zm6 0a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Zm-6 1.5c-2.5 0-4.5 1.4-4.5 3.2V16h9v-2.3c0-1.8-2-3.2-4.5-3.2Zm6 0c-.6 0-1.2.08-1.7.23 1 .8 1.7 1.9 1.7 3V16h4.5v-2.3c0-1.8-2-3.2-4.5-3.2Z" /></svg>
                </span>
                <div>
                  <p className="text-sm font-semibold">Set up a team</p>
                  <p className="mt-0.5 text-xs text-foreground-muted">A standing org that works continuously under a charter.</p>
                </div>
              </Link>
            </div>
          </details>
          <details className="mt-2 sm:max-w-2xl">
            <summary className="cursor-pointer text-xs text-foreground-muted hover:text-foreground">
              How it works
            </summary>
            <HowItWorksSteps />
          </details>
        </div>
      </section>

      {/* Live stat strip — what's alive right now (only when there's signal). */}
      {(standaloneTasks.length > 0 || teams.length > 0 || waiting.length > 0 || toReview.length > 0) && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Stat label="Missions" value={standaloneTasks.length} accent={standaloneTasks.length > 0} />
          <Stat label="Standing teams" value={teams.length} />
          <Stat label="Waiting on you" value={waiting.length} accent={waiting.length > 0} />
          <Stat label="To review" value={toReview.length} accent={toReview.length > 0} />
        </div>
      )}

      {/* First-run orientation (audit f1): when the workspace is empty, explain
          the three-step flow in plain language before the user has any data. */}
      {standaloneTasks.length === 0 && teams.length === 0 && waiting.length === 0 && toReview.length === 0 && !backendError && (
        <section className="rounded-2xl border border-border bg-surface p-6">
          <h2 className="text-sm font-semibold">New here? How it works</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">Three steps from an idea to verifiable, governed work.</p>
          <HowItWorksSteps withExamples />
        </section>
      )}

      {backendError && (
        <div role="alert" className="rounded-xl border border-danger/30 bg-danger/10 px-4 py-3 text-sm text-danger">
          The run environment isn&apos;t reachable right now. Your missions will appear here once it
          reconnects. Nothing is lost.
        </div>
      )}

      {/* Waiting on you. */}
      {waiting.length > 0 && (
        <section>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-semibold">Waiting on you</h2>
            <Link href="/workspace/inbox" className="text-xs font-medium text-signal hover:underline">
              Open inbox →
            </Link>
          </div>
          <ul className="space-y-2">
            {waiting.slice(0, 3).map((a) => (
              <li key={a.name}>
                <Link
                  href="/workspace/inbox"
                  className="flex items-center justify-between gap-3 rounded-xl border border-warning/30 bg-warning/5 px-4 py-3 transition hover:bg-warning/10"
                >
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium">{a.summary}</p>
                    <p className="text-xs text-foreground-muted">Mission {a.task}</p>
                  </div>
                  <span className="shrink-0 rounded-full bg-warning/15 px-2.5 py-0.5 text-xs font-medium text-warning">
                    Decide
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}

      {recentCompleted.length > 0 && (
        <section>
          <div className="mb-3 flex items-center justify-between">
            <div>
              <h2 className="text-sm font-semibold">Recently completed</h2>
              <span className="text-xs text-foreground-muted">
                Latest delivered outcomes across your missions and team runs
              </span>
            </div>
            <Link href="/workspace/artifacts" className="text-xs font-medium text-signal hover:underline">
              All completed work →
            </Link>
          </div>
          <ul className="grid gap-3 sm:grid-cols-2">
            {recentCompleted.map((m) => (
              <li key={m.evidence_key ?? m.task} className="min-w-0">
                <Link
                  href={completedHref(m)}
                  className="kb-card kb-card-hover block p-4"
                >
                  <div className="flex items-start justify-between gap-3">
                    <p className="min-w-0 truncate text-sm font-semibold">
                      {completedTeam(m)?.display_name ??
                        completedTeam(m)?.name ??
                        m.display_name ??
                        m.objective ??
                        m.task}
                    </p>
                    <span className="shrink-0 rounded-full border border-ok/30 bg-ok/10 px-2 py-0.5 text-[11px] font-medium text-ok">
                      {m.pull_requests.length > 0
                        ? "Change proposed"
                        : completedTeam(m)
                        ? teamOutcomeByTask.get(m.task) === "delivered_with_issues"
                          ? "Outcome with issues"
                          : "Outcome"
                        : m.review_status === "approved"
                          ? "Approved"
                          : "Delivered"}
                    </span>
                  </div>
                  <p className="mt-1.5 line-clamp-2 text-sm font-medium text-foreground">
                    {completedHeadline(m)}
                  </p>
                  <p className="mt-2 text-[11px] text-foreground-muted">
                    {completedTeam(m) ? "Standing team run · " : ""}
                    {m.model ?? "Model not reported"}
                    {m.finished_at ? ` · ${new Date(m.finished_at).toLocaleString()}` : ""}
                  </p>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* Standing teams — the command center's persistent operations. */}
      {teams.length > 0 && (
        <section>
          <div className="mb-3 flex items-center justify-between">
            <div>
              <h2 className="text-sm font-semibold">Standing teams</h2>
              <span className="text-xs text-foreground-muted">Long-running orgs that work continuously</span>
            </div>
            <Link href="/workspace/teams" className="text-xs font-medium text-signal hover:underline">
              All teams →
            </Link>
          </div>
          <ul className="grid gap-3 sm:grid-cols-2">
            {teams.slice(0, 4).map((t) => (
              <li key={t.name} className="min-w-0">
                <Link
                  href={`/workspace/teams/${encodeURIComponent(t.name)}`}
                  className="kb-card kb-card-hover group block p-4"
                >
                  <div className="flex items-center justify-between gap-3">
                    <p className="min-w-0 truncate text-sm font-semibold">
                      {t.display_name ?? t.name}
                    </p>
                    <span className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border bg-surface-muted/60 px-2 py-0.5 text-[11px] font-medium text-foreground-muted">
                      <span className={`h-1.5 w-1.5 rounded-full ${teamHealthDot(t.health, t.paused)}`} />
                      {t.paused ? "Hibernating" : (t.health ?? t.phase)}
                    </span>
                  </div>
                  <p className="mt-1.5 line-clamp-1 text-xs text-foreground-muted">{t.charter}</p>
                  <p className="mt-2 text-xs text-foreground-muted">
                    {t.member_count} members · recent {t.retained_delivered} delivered /{" "}
                    {t.retained_failed} failed · {t.generated_task_count} checks
                    {t.every_minutes != null ? ` · every ${t.every_minutes}m` : ""}
                  </p>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* Latest from your teams — the autonomous digest stream (§20). */}
      {latestDigestByTeam.length > 0 && (
        <section>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-semibold">Latest from your teams</h2>
            <Link href="/workspace/inbox" className="text-xs font-medium text-signal hover:underline">
              All digests →
            </Link>
          </div>
          <ul className="space-y-2">
            {latestDigestByTeam.slice(0, 3).map((d, i) => (
              <li
                key={`${d.team}-${d.at}-${i}`}
                className="flex items-center justify-between gap-3 rounded-xl border border-border bg-surface px-4 py-3"
              >
                <div className="min-w-0">
                  <p className="truncate text-sm">
                    <Link href={`/workspace/teams/${encodeURIComponent(d.team)}`} className="font-medium hover:underline">{d.team}</Link>{" "}
                    <span className="text-foreground-muted">{d.summary}</span>
                  </p>
                </div>
                <span className="shrink-0 text-xs text-foreground-muted">
                  {new Date(d.at).toLocaleTimeString()}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* Deliverables awaiting review (§16). */}
      {toReview.length > 0 && (
        <section>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-semibold">Deliverables to review</h2>
            <Link href="/workspace/artifacts" className="text-xs font-medium text-signal hover:underline">
              All artifacts →
            </Link>
          </div>
          <ul className="space-y-2">
            {toReview.slice(0, 4).map((m) => (
              <li key={m.evidence_key ?? m.task}>
                <Link
                  href={completedHref(m)}
                  className="flex items-center justify-between gap-3 rounded-xl border border-border bg-surface px-4 py-3 transition hover:border-signal/40 hover:bg-surface-muted"
                >
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium">{m.display_name ?? m.objective ?? m.task}</p>
                    {m.excerpt && <p className="truncate text-xs text-foreground-muted">{m.excerpt}</p>}
                  </div>
                  <span
                    className={`shrink-0 rounded-full border px-2.5 py-0.5 text-xs font-medium ${
                      m.review_status === "changes_requested"
                        ? "border-amber-500/30 bg-amber-500/10 text-amber-600"
                        : "border-signal/30 bg-signal/10 text-signal"
                    }`}
                  >
                    {m.review_status === "changes_requested" ? "Revising" : "Review"}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* Your missions — only when there are some; the intent entry above is the
          prompt when there are none (no redundant empty block on the home). */}
      {standaloneTasks.length > 0 && (
        <section>
          <div className="mb-3 flex items-baseline justify-between">
            <h2 className="text-sm font-semibold">Your missions</h2>
            <span className="text-xs text-foreground-muted">One-off tasks — start, review, done</span>
          </div>
          <ul className="grid gap-3 sm:grid-cols-2">
            {standaloneTasks.map((t) => {
              const st = missionStatus(t.phase, null, { delivered: t.delivered });
              return (
                <li key={t.name} className="min-w-0">
                  <Link
                    href={`/workspace/missions/${encodeURIComponent(t.name)}`}
                    className="block rounded-xl border border-border bg-surface p-4 transition hover:border-signal/40 hover:shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
                  >
                    <div className="flex items-start justify-between gap-3">
                      <p className="min-w-0 truncate text-sm font-medium">
                        {t.display_name ?? t.objective}
                      </p>
                      <MissionStatusBadge status={st} />
                    </div>
                    <p className="mt-1.5 line-clamp-2 text-xs text-foreground-muted">
                      {t.objective}
                    </p>
                    <p className="mt-2 text-xs text-foreground-muted">
                      Tier {t.tier} · {TIER_LABELS[t.tier] ?? "?"}
                    </p>
                  </Link>
                </li>
              );
            })}
          </ul>
        </section>
      )}
    </div>
  );
}
