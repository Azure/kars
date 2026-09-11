// kars Bridge Workspace — Inbox. The fleet-wide decision queue. Every card
// answers what/by-which-role/why/impact before the buttons (no rubber-stamping).

import Link from "next/link";
import { ApprovalDecision } from "@/components/approval-decision";
import { ClarificationAnswer } from "@/components/clarification-answer";
import { actionLabel } from "@/components/approval-phase-badge";
import { HonestState } from "@/components/honest-state";
import { Icon } from "@/components/icon";
import { BffError, getDigests, listApprovals, listTeams } from "@/lib/bff";
import { authWired, defaultNamespace, operatorIdentity } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import { type Approval, type Digest, type TeamSummary } from "@/lib/types";

export const dynamic = "force-dynamic";

function digestTone(health: string): string {
  switch (health) {
    case "Healthy":
      return "border-emerald-500/30";
    case "Stalled":
      return "border-rose-500/30";
    case "Unproductive":
      return "border-amber-500/30";
    default:
      return "border-border";
  }
}

function DigestCard({ d, priorCount = 0 }: { d: Digest; priorCount?: number }) {
  return (
    <li className={`rounded-xl border ${digestTone(d.health)} bg-surface p-5`}>
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <Link
            href={`/workspace/teams/${encodeURIComponent(d.team)}`}
            className="text-sm font-semibold text-signal hover:underline"
          >
            {d.team}
          </Link>
          <p className="mt-1 text-sm">{d.summary}</p>
        </div>
        <span className="shrink-0 rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium text-foreground-muted">
          {d.health}
        </span>
      </div>
      <div className="mt-3 flex flex-wrap gap-x-5 gap-y-1 text-xs text-foreground-muted">
        <span>{d.runs_generated} runs</span>
        <span>{d.runs_delivered} delivered</span>
        <span>{d.tokens_spent.toLocaleString()} tokens</span>
        <span>{d.knowledge_entries} knowledge entries</span>
        <span>{new Date(d.at).toLocaleString()}</span>
        {priorCount > 0 && (
          <Link href={`/workspace/teams/${encodeURIComponent(d.team)}`} className="hover:underline" title="Earlier digests from this team">
            +{priorCount} earlier
          </Link>
        )}
        {d.channel && (
          <span className="inline-flex items-center gap-1 rounded-full bg-surface-muted px-2 py-0.5 font-mono text-[11px]" title="Verified reporting channel — reports travel only this declared line">
            {d.gated ? <Icon name="lock" size={10} className="inline mr-0.5" /> : null}{d.channel}
          </span>
        )}
      </div>
    </li>
  );
}

function isTeamMilestoneReview(a: Approval): boolean {
  return a.action_kind === "checkpoint" && a.team != null && a.milestone != null;
}

function impactLine(a: Approval): string {
  switch (a.action_kind) {
    case "egress":
      return "Reaches an external system · review the destination";
    case "tierRaise":
      return a.requested_tier != null
        ? `Raises this role's authority to Tier ${a.requested_tier} · grants more independence`
        : "Raises this role's authority";
    case "budgetRaise":
      return "Raises the governed token ceiling · tools and authority stay unchanged";
    case "clarification":
      return "The agent is asking you a question · your answer resumes its active run";
    case "irreversible":
      return "Cannot be undone · review carefully";
    case "toolCall":
      return "Calls an external tool · may cost money";
    case "checkpoint":
      return isTeamMilestoneReview(a)
        ? "Review a Team outcome · approve it or return exact changes to the same work item"
        : "Checkpoint sign-off · confirms progress";
    default:
      return "Needs your decision";
  }
}

function DecisionCard({
  a,
  decider,
  auth,
  team,
}: {
  a: Approval;
  decider: string;
  auth: boolean;
  team: TeamSummary | null;
}) {
  const decided = !a.actionable;
  const teamMilestoneReview = isTeamMilestoneReview(a);
  return (
    <li className={`rounded-xl border p-5 ${decided ? "border-border bg-surface" : "border-warning/30 bg-warning/5"}`}>
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5 text-xs font-medium text-foreground-muted">
              {actionLabel(a.action_kind)}
            </span>
            <Link
              href={
                team
                  ? `/workspace/teams/${encodeURIComponent(team.name)}/runs/${encodeURIComponent(a.task)}`
                  : `/workspace/missions/${encodeURIComponent(a.task)}`
              }
              className="text-xs text-signal hover:underline"
            >
              {team ? "Team run" : "Mission"} {a.task}
            </Link>
          </div>
          <p className="mt-2 text-sm font-medium">{a.summary}</p>
          {a.detail && <p className="mt-0.5 text-xs text-foreground-muted">{a.detail}</p>}
          <p className="mt-2 inline-flex items-center gap-1.5 text-xs text-foreground-muted">
            <svg viewBox="0 0 16 16" className="h-3.5 w-3.5 text-warning" fill="currentColor" aria-hidden>
              <path d="M8 1.5 1 14h14L8 1.5Zm0 5a.75.75 0 0 1 .75.75v3a.75.75 0 0 1-1.5 0v-3A.75.75 0 0 1 8 6.5ZM8 11.5a.9.9 0 1 0 0 1.8.9.9 0 0 0 0-1.8Z" />
            </svg>
            {impactLine(a)}
          </p>
        </div>
        {decided && (
          <span className="shrink-0 text-right text-xs text-foreground-muted">
            <span className="block">{a.phase}{a.decider ? ` by ${a.decider}` : ""}</span>
            {a.decided_at && <span className="block">{new Date(a.decided_at).toLocaleString()}</span>}
          </span>
        )}
      </div>
      {a.actionable && (
        <div className="mt-4 border-t border-warning/20 pt-3">
          {a.action_kind === "clarification" ? (
            <ClarificationAnswer
              name={a.name}
              decider={decider}
              authWired={auth}
              resourceVersion={a.resource_version}
              boundEnvelopeDigest={a.bound_envelope_digest}
            />
          ) : (
            <ApprovalDecision
              name={a.name}
              decider={decider}
              authWired={auth}
              resourceVersion={a.resource_version}
              boundEnvelopeDigest={a.bound_envelope_digest}
              approveLabel={teamMilestoneReview ? "Approve outcome" : "Approve"}
              denyLabel={teamMilestoneReview ? "Request changes" : "Deny"}
              requireDenyReason={teamMilestoneReview}
              reasonPlaceholder={
                teamMilestoneReview
                  ? "Describe exactly what the Team must change before you review this work again."
                  : undefined
              }
            />
          )}
        </div>
      )}
    </li>
  );
}

export default async function WorkspaceInbox({
  searchParams,
}: {
  searchParams: Promise<{ history?: string }>;
}) {
  const { history } = await searchParams;
  const ns = defaultNamespace();
  const principal = await currentPrincipal();
  const decider = principal.name || operatorIdentity();
  const auth = authWired();

  let approvals: Approval[] = [];
  let teams: TeamSummary[] = [];
  let error: string | null = null;
  try {
    [approvals, teams] = await Promise.all([listApprovals(ns), listTeams(ns).catch(() => [])]);
  } catch (err) {
    error = err instanceof BffError ? err.code : "unknown";
  }

  let digests: Digest[] = [];
  try {
    digests = await getDigests();
  } catch {
    digests = [];
  }

  const pending = approvals.filter((a) => a.actionable);
  const decided = approvals.filter((a) => !a.actionable);
  const visibleDecided = history === "all" ? decided : decided.slice(0, 10);
  const teamFor = (approval: Approval) =>
    teams.find(
      (team) =>
        team.name === approval.team
        || approval.task === `${team.name}-principal`
        || approval.task.startsWith(`${team.name}-run-`),
    ) ?? null;
  // One card per team — the latest digest — with a count of how many it stands
  // for, so a chatty/stalled team doesn't flood the inbox with identical cards.
  const digestGroups = Array.from(
    digests
      .reduce((acc, d) => {
        const g = acc.get(d.team);
        if (!g) acc.set(d.team, { latest: d, count: 1 });
        else {
          g.count += 1;
          if ((d.at ?? "") > (g.latest.at ?? "")) g.latest = d;
        }
        return acc;
      }, new Map<string, { latest: Digest; count: number }>())
      .values(),
  ).sort((a, b) => (b.latest.at ?? "").localeCompare(a.latest.at ?? ""));

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Inbox</h1>
        <p className="mt-1 text-sm text-foreground-muted">
          Decisions your missions and standing teams are waiting on. Approve, deny, or adjust —
          every choice is recorded in the work&apos;s Governance Receipt.
        </p>
      </div>

      {error ? (
        <HonestState variant="not_wired" title="Inbox unavailable" detail="The run environment isn't reachable right now." />
      ) : (
        <>
          <section>
            <h2 className="mb-3 text-sm font-semibold">
              Waiting on you
              {pending.length > 0 && (
                <span
                  aria-label={`${pending.length} items waiting`}
                  className="ml-2 rounded-full bg-warning/15 px-2 py-0.5 text-xs font-medium text-warning"
                >
                  {pending.length}
                </span>
              )}
            </h2>
            {pending.length === 0 ? (
              <HonestState variant="empty" compact title="No approvals pending" detail="No mission or team run is waiting on a human decision right now." />
            ) : (
              <ul className="mt-4 space-y-3">
                {pending.map((a) => (
                  <DecisionCard
                    key={a.name}
                    a={a}
                    decider={decider}
                    auth={auth}
                    team={teamFor(a)}
                  />
                ))}
              </ul>
            )}
          </section>

          {decided.length > 0 && (
            <details className="rounded-xl border border-border bg-surface p-5">
              <summary className="cursor-pointer list-none text-sm font-semibold">
                Decision history
                <span className="ml-2 rounded-full bg-surface-muted px-2 py-0.5 text-xs font-medium text-foreground-muted">
                  {decided.length}
                </span>
                <span className="ml-2 text-xs font-normal text-foreground-muted">
                  resolved items are collapsed by default
                </span>
              </summary>
              <ul className="space-y-3">
                {visibleDecided.map((a) => (
                  <DecisionCard
                    key={a.name}
                    a={a}
                    decider={decider}
                    auth={auth}
                    team={teamFor(a)}
                  />
                ))}
              </ul>
              {decided.length > visibleDecided.length && (
                <Link
                  href="/workspace/inbox?history=all"
                  className="mt-3 inline-block text-xs font-medium text-signal hover:underline"
                >
                  Show all {decided.length} resolved decisions →
                </Link>
              )}
            </details>
          )}

          {digestGroups.length > 0 && (
            <section>
              <h2 className="mb-3 text-sm font-semibold">
                Team digests
                <span className="ml-2 rounded-full bg-surface-muted px-2 py-0.5 text-xs font-medium text-foreground-muted">
                  {digestGroups.length}
                </span>
              </h2>
              <p className="mb-3 text-xs text-foreground-muted">
                Your standing teams reporting in — the latest from each. No action needed.
              </p>
              <ul className="space-y-3">
                {digestGroups.map((g) => (
                  <DigestCard key={g.latest.team} d={g.latest} priorCount={g.count - 1} />
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </div>
  );
}
