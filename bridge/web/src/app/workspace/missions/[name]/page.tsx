// kars Bridge Workspace — Mission detail (the live mission canvas).
//
// The user projection of a governed task: the objective, an always-visible
// governance envelope strip, the role tree (delegated children), the live
// activity stream, the decisions waiting on the user, the efficiency scorecard,
// and the Governance Receipt. No Kubernetes vocabulary surfaces.

import Link from "next/link";
import { DeliverableView, DeliverableBody } from "@/components/deliverable-view";
import { notFound, redirect } from "next/navigation";
import { ExecutionExplorer } from "@/components/execution-explorer";
import { LiveRefresh, LivePulse } from "@/components/live-refresh";
import { MissionAutoRun } from "./mission-autorun";
import { OrgChart } from "./org-chart";
import { MissionMap } from "./mission-map";
import { ReviewPanel } from "./review-panel";
import { ReadinessPanel } from "./readiness-panel";
import { DeployTimeline } from "./deploy-timeline";
import { NetworkMode } from "./network-mode";
import { MissionBlockers } from "./mission-blockers";
import { MissionScorecard } from "@/components/mission-scorecard";
import { MissionStatusBadge, missionStatus } from "@/components/mission-status";
import { JourneyRail, missionBeat } from "@/components/journey-rail";
import { ReceiptPanel } from "@/components/receipt-panel";
import { CompliancePackView } from "@/components/compliance-pack";
import { ReceiptVerifyButton } from "@/components/receipt-verify";
import { ProvenanceOverlay } from "@/components/provenance-overlay";
import { AuditReportDownload } from "@/components/audit-report";
import { ReliabilityRunner } from "./reliability-runner";
import { BudgetRecovery } from "./budget-recovery";
import { PromoteMission } from "./promote-mission";
import { ProvenanceStory } from "@/components/provenance-story";
import { EgressRequest } from "./egress-request";
import { DeleteMissionControl } from "./delete-control";
import { HonestState } from "@/components/honest-state";
import { PageHeader } from "@/components/ui";

import { TaskApprovalsPanel } from "@/app/tasks/[name]/task-approvals-panel";
import { ExecutionPanel } from "@/app/tasks/[name]/execution-panel";
import {
  BffError,
  getReceipt,
  getReview,
  getScorecard,
  getTroubleshoot,
  getTask,
  getCompliancePack,
  listTaskApprovals,
} from "@/lib/bff";
import { authWired, defaultNamespace, operatorIdentity } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import type { ReactNode } from "react";
import { egressScope, humanizeMcp } from "@/lib/format";
import { Icon } from "@/components/icon";
import { HaltButton } from "./halt-button";
import { TIER_LABELS, type Composition, type MissionResult, type MissionArtifact, type AgentIdentity, type TaskDetail } from "@/lib/types";

export const dynamic = "force-dynamic";

/** What each autonomy tier means for how much interaction flies into the
 *  operator's inbox — the legible link between autonomy and steering (REQ17). */
const AUTONOMY_INTERACTION: Record<number, string> = {
  1: "Manual — it proposes every step and acts on nothing on its own; you perform each action.",
  2: "Shared — it acts only on low-risk steps; everything else arrives in your Inbox for approval.",
  3: "Conditional — it acts on its own but pauses for your approval before anything priced, external, or irreversible.",
  4: "Supervised — it runs autonomously with periodic checkpoints you sign off on in your Inbox.",
  5: "Full — it runs to completion within budget; you review the result. Only hard-stops would ask you.",
};

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

function reviewKind(artifacts: MissionArtifact[] | undefined): string {
  if (!artifacts || artifacts.length === 0) return "output";
  const exts = artifacts.map((a) => a.name.split(".").pop()?.toLowerCase() ?? "");
  const code = ["ts", "tsx", "js", "jsx", "rs", "py", "go", "java", "rb", "c", "cpp", "h", "sh", "yaml", "yml", "toml", "json"];
  const docs = ["md", "mdx", "txt", "rst", "adoc", "html"];
  const data = ["csv", "tsv", "parquet", "xlsx"];
  if (exts.some((e) => code.includes(e))) return "code";
  if (exts.some((e) => data.includes(e))) return "data";
  if (exts.some((e) => docs.includes(e))) return "docs";
  return "output";
}

export default async function MissionDetail({
  params,
  searchParams,
}: {
  params: Promise<{ name: string }>;
  searchParams: Promise<{ tab?: string }>;
}) {
  const principal = await currentPrincipal();
  const { name } = await params;
  const { tab } = await searchParams;
  const ns = defaultNamespace();

  let task;
  try {
    task = await getTask(ns, name);
  } catch (err) {
    if (err instanceof BffError && err.code === "not_found") notFound();
    // Any other failure (BFF unreachable, upstream error) renders an honest
    // error state — never a raw crash overlay at the user.
    return (
      <div className="space-y-6">
        <PageHeader eyebrow="Workspace" title="Mission" />
        <HonestState
          variant="not_wired"
          title="This mission is unavailable"
          detail="The run environment isn't reachable right now, so this mission's details couldn't be loaded. Try again shortly."
          action={
            <Link
              href="/workspace/missions"
              className="rounded-lg border border-border px-4 py-2 text-sm font-medium hover:border-signal/40 hover:text-signal"
            >
              ← Back to missions
            </Link>
          }
        />
      </div>
    );
  }

  const [receipt, scorecard, allApprovals, review, compliance] = await Promise.all([
    getReceipt(ns, name).catch(() => null),
    getScorecard(ns, name).catch(() => null),
    listTaskApprovals(ns, name).catch(() => []),
    getReview(ns, name).catch(() => null),
    getCompliancePack(ns, name).catch(() => null),
  ]);
  const currentRunNonce = task.current_run_nonce;
  const approvals = allApprovals.filter(
    (approval) =>
      approval.run_nonce == null
      || currentRunNonce == null
      || approval.run_nonce === currentRunNonce,
  );

  const needsYou = approvals.some((a) => a.actionable);
  const awaitingAssignment = Boolean(
    task.current_run_nonce
    && task.assignment?.task_id !== task.current_run_nonce,
  );
  const assignmentInFlight =
    awaitingAssignment
    || (
      task.assignment?.completed_at == null
      && (task.assignment?.state === "Assigned" || task.assignment?.state === "Running")
    );
  const resultMatchesCurrentRun =
    task.result == null
    || task.current_run_nonce == null
    || task.result.assignment_nonce == null
    || task.result.assignment_nonce === task.current_run_nonce;
  const currentResult = assignmentInFlight || !resultMatchesCurrentRun ? null : task.result;
  const currentReceipt = assignmentInFlight ? null : receipt;
  const displayTask = assignmentInFlight ? { ...task, result: null } : task;
  // A run result with status "error" is a FAILURE, not a deliverable — it must
  // never read as "Delivered"/"Running", and it must not present a receipt as
  // attesting real work (audit f8/f9/f13).
  const blocked = currentResult?.blocked ?? null;
  const assignmentFailed =
    !awaitingAssignment
    && task.assignment?.completed_at != null
    && task.assignment.state === "Failed";
  const failed =
    assignmentFailed || (currentResult?.status === "error" && blocked == null);
  // A run whose output is a capability/limit STOP is not a deliverable — surface
  // it as an actionable state, never the answer.
  // For a failed run, pull the real cluster troubleshooting evidence (pod +
  // container status + the agent's own log tail + an evidence-derived cause).
  const troubleshoot = failed ? await getTroubleshoot(ns, name).catch(() => null) : null;
  const realDelivery =
    !assignmentFailed
    && currentResult != null
    && currentResult.status !== "error"
    && blocked == null;
  const status = missionStatus(task.phase, task.execution_phase, {
    delivered: realDelivery,
    needsYou,
    launched: task.launched,
    failed,
  });
  // Poll through the WHOLE live lifecycle — deploying, running, or awaiting a
  // human decision — not just "Running". Exhibit C: a mission stuck at
  // "Deploying 1/5" must update itself as the sandbox comes up, never require F5.
  const live = status === "deploying" || status === "running" || status === "needs_you";
  const running = status === "running";
  // A newly-created draft can land here before the controller has stamped its
  // Ready condition. Keep polling admission so Launch enables automatically
  // instead of requiring the user to click around or refresh.
  const admissionPending = !task.launched && task.phase === "Pending";
  // Keep the live canvas polling not just while the agent runs, but through the
  // whole review window — a delivered mission whose review isn't yet approved may
  // still re-run (request-changes) or land an approval, and those must appear
  // without an F5. Stops once the deliverable is approved (or the mission ends).
  // Tail while execution is changing. A delivered-but-unreviewed mission is
  // stable until the human acts; background refreshes there reset the selected
  // tab and make Activity/Artifacts feel unclickable.
  const refreshActive = live || admissionPending;
  // Drive the first run automatically once a launched mission's sandbox is up
  // and nothing has run yet — so launching a one-shot mission just starts it.
  // Gate on "no run ever requested" (not "no activity"): the agent emits startup
  // telemetry (MCP init / tool list) before any mission run, which would falsely
  // suppress the kickoff and leave the mission silently idle.
  const firstRunPending =
    task.launched && running && currentResult == null && !task.run_requested;
  const budget = task.envelope.budget;
  // Which tab opens by default. A delivered/failed run opens on its outcome; a
  // freshly LAUNCHED mission drops the operator straight into the live Activity
  // view (the "watch it work" moment) instead of the static Overview; an
  // un-launched draft opens on Overview to review the plan.
  const initialTab = currentResult
    ? "deliverable"
    : task.launched && (running || status === "deploying")
      ? "activity"
      : "overview";

  if (task.team) {
    redirect(
      `/workspace/teams/${encodeURIComponent(task.team)}/runs/${encodeURIComponent(name)}`,
    );
  }

  return (
    <div className="space-y-6">
      {/* Live canvas: while the mission is running, re-fetch on an interval so
          the activity trace, telemetry, and deliverable land without a reload. */}
      <LiveRefresh active={refreshActive} />
      <MissionAutoRun key={name} namespace={ns} name={name} active={firstRunPending} />
      {/* Header + envelope strip */}
      <div>
        <nav aria-label="Breadcrumb" className="flex flex-wrap items-center text-sm text-foreground-muted">
          {task.team ? (
            <>
              <Link href="/workspace/teams" className="rounded hover:text-foreground">
                Teams
              </Link>
              <span className="px-1.5" aria-hidden>/</span>
              <Link
                href={`/workspace/teams/${encodeURIComponent(task.team)}`}
                className="rounded font-medium text-signal hover:underline"
              >
                {task.team}
              </Link>
            </>
          ) : (
            <Link href="/workspace/missions" className="rounded hover:text-foreground">
              Missions
            </Link>
          )}
          {task.lineage.map((ancestor) => (
            <span key={ancestor} className="flex items-center">
              <span className="px-1.5" aria-hidden>/</span>
              <Link
                href={`/workspace/missions/${encodeURIComponent(ancestor)}`}
                className="rounded hover:text-foreground"
              >
                {ancestor}
              </Link>
            </span>
          ))}
          <span className="px-1.5" aria-hidden>/</span>
          <span className="text-foreground">{task.team ? "Run" : (task.display_name ?? name)}</span>
        </nav>
        <div className="mt-2 flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h1 className="text-2xl font-semibold tracking-tight">
              {task.display_name ?? "Mission"}
            </h1>
            <ObjectiveBlock objective={task.objective} />
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {(live) && <LivePulse label={status === "deploying" ? "Deploying" : status === "needs_you" ? "Waiting on you" : "Live"} />}
            {live && !task.halted && <HaltButton ns={ns} task={name} />}
            {/* While live, the animated pulse already names the phase (Deploying/
                Live/Waiting) — a second static status badge beside it just
                duplicates the word. Show the badge only when NOT live (delivered,
                blocked, or a draft), so there's exactly one status label. */}
            {!live && <MissionStatusBadge status={status} />}
            {/* Delete is always reachable from the header (parity with a team) —
                not buried at the bottom of a tab. The fuller explanatory block
                stays in Overview. */}
            <DeleteMissionControl name={name} />
          </div>
        </div>
        {task.parent && (
          <p className="mt-2 text-sm text-foreground-muted">
            A role within{" "}
            <Link href={`/workspace/missions/${encodeURIComponent(task.parent)}`} className="text-signal hover:underline">
              {task.parent}
            </Link>
          </p>
        )}
      </div>

      {/* Journey spine — the same seven beats the compose flow showed, now
          tracking the live mission so the story reads continuously. */}
      <JourneyRail
        current={missionBeat({
          launched: task.launched,
          executionPhase: task.execution_phase,
          hasResult: realDelivery,
          blocked: status === "blocked",
        })}
        blocked={status === "blocked" || status === "failed"}
      />

      {/* Governance envelope strip — always visible. */}
      <div className="rounded-xl border border-border bg-surface-muted/50 px-5 py-3 text-sm">
        <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
          <EnvelopeFact label="Autonomy" value={`Tier ${task.envelope.tier} · ${TIER_LABELS[task.envelope.tier] ?? "?"}`} />
          <EnvelopeFact
            label="Budget"
            value={budget?.tokens != null ? `${budget.tokens.toLocaleString()} tokens` : "No cap"}
          />
          <EnvelopeFact
            label="External reach"
            value={(() => {
              const ext = (task.composition?.egress ?? []).filter((e) => egressScope(e) === "external");
              if (ext.length === 0) return "Local only — no external services";
              const gated = task.envelope.tier <= 3;
              return `${ext.length} external ${ext.length === 1 ? "service" : "services"} — ${gated ? "gated, asks you first" : "autonomous"}`;
            })()}
          />
          {task.phase === "Degraded" && task.status_message && (
            <span className="text-danger">{task.status_message}</span>
          )}
        </div>
        {/* What this autonomy level actually means for how much flies into your
            inbox — makes the autonomy → interaction link legible (REQ17) — and
            frames the envelope as adjustable guardrails, not a fixed cage: every
            boundary here can be widened in-flow (raise the autonomy tier, add a
            network host, or lift the budget in Overview), each as a governed,
            receipted decision. */}
        <p className="mt-2 border-t border-border/60 pt-2 text-xs text-foreground-muted">
          {AUTONOMY_INTERACTION[task.envelope.tier] ?? "Acts within its envelope; you review outcomes."}
          {task.launched && (
            <>
              {" "}
              <span className="text-foreground-muted/80">
                These are guardrails, not a cage — raise the tier, add a network host, or lift the budget
                anytime in Overview; each change is a governed, receipted decision.
              </span>
            </>
          )}
        </p>
      </div>

      {/* Next step — ONE primary, state-driven call to action, so the mission
          reads as "here's what to do now" instead of ~18 competing buttons
          (audit f11). The detailed controls remain grouped in the tabs below. */}
      {status !== "blocked" && (
        <NextStep status={status} />
      )}

      {/* Blocked banner */}
      {status === "blocked" && task.status_message && (
        <div role="alert" className="flex items-start gap-3 rounded-xl border border-danger/30 bg-danger/10 px-4 py-3">
          <div>
            <p className="text-sm font-medium text-danger">This mission is blocked</p>
            <p className="mt-0.5 text-sm text-foreground-muted">{task.status_message}</p>
          </div>
        </div>
      )}

      {task.halted && (
        <div className="flex items-start gap-2 rounded-xl border border-danger/40 bg-danger/[0.06] px-4 py-3">
          <span aria-hidden className="text-danger">⏹</span>
          <div>
            <p className="text-sm font-medium">Mission halted</p>
            <p className="mt-0.5 text-xs text-foreground-muted">
              {task.halted}. The agent was torn down and removed from the mesh; the deliverable,
              trace, and receipt are retained. This halt is recorded as a governed decision.
            </p>
          </div>
        </div>
      )}
      {task.harness_corrected && (
        <div className="flex items-start gap-2 rounded-xl border border-accent/40 bg-accent/[0.06] px-4 py-3">
          <Icon name="compass" size={16} className="shrink-0 text-accent" />
          <div>
            <p className="text-sm font-medium">Harness capability-corrected</p>
            <p className="mt-0.5 text-xs text-foreground-muted">
              {task.harness_corrected}. Recorded as a governed decision so the mission runs on a
              harness that can actually execute it — never silently idling.
            </p>
          </div>
        </div>
      )}
      {realDelivery && (
        <div className="flex items-center justify-between gap-3 rounded-xl border border-ok/40 bg-ok/[0.06] px-4 py-3">
          <div className="flex items-center gap-2">
            <span aria-hidden className="text-ok">✓</span>
            <p className="text-sm font-medium">Deliverable ready — review it in the Deliverable tab below.</p>
          </div>
          <MissionStatusBadge status="done" />
        </div>
      )}
      {blocked?.reason === "budget" && (
        <div className="rounded-xl border border-warning/50 bg-warning/[0.07] px-4 py-3">
          <div className="flex items-start gap-2">
            <span aria-hidden className="text-warning">⏸</span>
            <div className="min-w-0">
              <p className="text-sm font-medium">
                Run stopped — daily token budget reached
                {blocked.spent != null && blocked.limit != null
                  ? ` (${blocked.spent.toLocaleString()} / ${blocked.limit.toLocaleString()} tokens)`
                  : ""}
                .
              </p>
              <p className="mt-0.5 text-xs text-foreground-muted">
                This isn&apos;t the mission&apos;s answer — the agent hit its budget mid-run. Increase the
                governed budget below to continue in the existing sandbox, narrow the objective, or wait
                for the daily reset.
              </p>
              <BudgetRecovery
                namespace={ns}
                name={name}
                current={task.envelope.budget?.tokens ?? null}
                spent={blocked.spent}
                stoppedLimit={blocked.limit}
                approvalPending={approvals.some(
                  (approval) =>
                    approval.task === name &&
                    approval.action_kind === "budgetRaise" &&
                    approval.phase === "Pending",
                )}
              />
            </div>
          </div>
        </div>
      )}
      {/* No-op run: the run completed ok but produced no real deliverable and it
          isn't a budget stop or a failure — an honest, non-confusing state (the
          agent decided there was nothing new to add) with a clear next step. */}
      {currentResult != null && !realDelivery && blocked == null && !failed && (
        <div className="rounded-xl border border-border bg-surface-muted/50 px-4 py-3">
          <div className="flex items-start gap-2">
            <span aria-hidden className="text-foreground-muted">◦</span>
            <div className="min-w-0">
              <p className="text-sm font-medium">This run produced no new deliverable.</p>
              <p className="mt-0.5 text-xs text-foreground-muted">
                The agent completed but reported nothing material to add for this objective. If you
                expected output, sharpen the objective or widen its tools/network reach in the launch
                package, then run again.
              </p>
            </div>
          </div>
        </div>
      )}

      {/* Body — organised into tabs so the mission is legible, not a 14-panel scroll. */}
      <MissionServerTabs
        active={tab ?? initialTab}
        basePath={`/workspace/missions/${encodeURIComponent(name)}`}
        tabs={[
          {
            id: "overview",
            label: "Overview",
            node: (
              <div className="space-y-5">
                {task.launched && <DeployTimeline task={displayTask} />}
                {task.composition && <CompositionPanel composition={task.composition} launched={task.launched} />}
                {task.composition && (
                  <ReadinessPanel
                    composition={task.composition}
                    launched={task.launched}
                    executionPhase={task.execution_phase}
                    degraded={task.phase === "Degraded"}
                    delivered={realDelivery}
                    failed={failed}
                  />
                )}
                {task.sandbox && (
                  <NetworkMode ns={ns} task={name} mode={task.egress_mode} />
                )}
                {currentResult != null && <ReliabilityRunner ns={ns} name={name} />}
                {task.launched && <PromoteMission ns={ns} name={name} currentTier={task.envelope.tier} />}
                {task.agent_identity && <AgentIdentityCard identity={task.agent_identity} />}
                <MissionMap task={task} />
                {/* Org chart only for a real multi-agent structure — a team run
                    or a mission that delegated/spawned. A single-run task shows
                    no org chart (there's no org). */}
                {(task.children.length > 0 || task.sub_agents.length > 0) && <OrgChart task={task} />}
                <TaskApprovalsPanel approvals={approvals} decider={principal.name || operatorIdentity()} authWired={authWired()} />
                <ExecutionPanel task={task} />
                {running && <EgressRequest mission={name} />}
              </div>
            ),
          },
          {
            id: "activity",
            label: "Activity",
            live: running,
            badge: task.activity?.filter((event) => event.kind === "tool").length ?? null,
            node: (
              <div className="space-y-4">
                {task.launched && currentResult == null && <DeployTimeline task={displayTask} />}
                <ExecutionExplorer
                  running={running}
                  activity={task.activity}
                  telemetry={task.telemetry}
                  assignmentEvents={task.assignment_events}
                  approvals={approvals}
                  ns={ns}
                  name={name}
                  agentLabel={task.display_name ?? name}
                  agentPhase={task.assignment?.state ?? task.execution_phase ?? task.phase}
                  agentRuntime={task.composition?.runtime}
                  agentModel={task.composition?.model}
                  subAgents={task.sub_agents}
                  identity={task.agent_identity ?? null}
                  envelopeDigest={task.envelope_digest ?? null}
                  receipt={currentReceipt}
                />
                <MissionBlockers
                  ns={ns}
                  task={name}
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
                {running && <EgressRequest mission={name} />}
              </div>
            ),
          },
          {
            id: "deliverable",
            label: blocked ? "Run stopped" : currentResult?.status === "error" ? "Run failed" : "Deliverable",
            live: false,
            badge: currentResult != null && currentResult.status !== "error" && blocked == null ? "Ready" : null,
            node: currentResult ? (
              <div className="space-y-5">
                {currentResult.status === "error" ? (
                  <FailureDiagnostic task={displayTask} troubleshoot={troubleshoot} />
                ) : blocked ? (
                  <section className="rounded-xl border border-warning/50 bg-warning/[0.06] p-6">
                    <p className="text-sm font-medium">
                      {blocked.reason === "budget" ? "Run stopped — daily token budget reached" : "Run stopped"}
                      {blocked.spent != null && blocked.limit != null
                        ? ` (${blocked.spent.toLocaleString()} / ${blocked.limit.toLocaleString()} tokens)`
                        : ""}
                    </p>
                    <p className="mt-1 text-sm text-foreground-muted">{blocked.detail}</p>
                    <p className="mt-3 text-xs text-foreground-muted">
                      This is a stop condition, not the mission&apos;s answer — so there&apos;s no deliverable
                      to review. Raise the budget in the launch package (enforced by the sandbox&apos;s
                      inference policy), narrow the objective, or resume after the daily reset, then run again.
                    </p>
                    <details className="mt-3">
                      <summary className="cursor-pointer text-xs text-foreground-muted hover:text-foreground">
                        Show the raw stop message
                      </summary>
                      <pre className="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg border border-border bg-surface-muted/50 p-3 text-[11px] text-foreground-muted">
                        {currentResult.output}
                      </pre>
                    </details>
                  </section>
                ) : (
                  <ResultPanel result={currentResult} />
                )}
                {currentResult.status !== "error" && blocked == null && currentResult.output && <ReviewPanel task={name} assignmentNonce={currentResult.assignment_nonce ?? task.current_run_nonce ?? name} kind={reviewKind(task.artifacts)} initial={review} sandboxLive={task.launched && task.execution_phase === "Running"} />}
              </div>
            ) : null,
          },
          {
            id: "artifacts",
            label: "Artifacts",
            badge: ((task.artifacts?.length ?? 0) + (task.pull_requests?.length ?? 0)) || null,
            node: (task.artifacts && task.artifacts.length > 0) || (task.pull_requests && task.pull_requests.length > 0) ? (
              <ArtifactsPanel ns={ns} task={name} artifacts={task.artifacts} pullRequests={task.pull_requests ?? []} activity={task.activity} egress={task.composition?.egress ?? []} tokens={currentResult?.total_tokens ?? null} />
            ) : realDelivery ? (
              <HonestState
                variant="empty"
                compact
                title="No separate files"
                detail="This mission produced a text deliverable — read it in the Deliverable tab. No discrete file artifacts were captured for this run."
              />
            ) : null,
          },
          {
            id: "receipt",
            label: "Receipt",
            node: (
              <div className="space-y-3">
                {scorecard && <MissionScorecard scorecard={scorecard} />}
                {currentReceipt && realDelivery ? (
                  <>
                    <div className="flex items-center justify-end gap-2">
                      <AuditReportDownload task={name} receipt={currentReceipt} activity={task.activity} egress={task.composition?.egress ?? []} />
                      <ProvenanceOverlay receipt={currentReceipt} deliverableDid={null} activity={task.activity} egress={task.composition?.egress ?? []} />
                    </div>
                    <ReceiptVerifyButton ns={ns} task={name} />
                    <ReceiptPanel receipt={currentReceipt} />
                    {compliance && <CompliancePackView pack={compliance} />}
                  </>
                ) : (
                  <section className="rounded-xl border border-dashed border-border bg-surface-muted/40 p-6 text-center">
                    <p className="text-sm font-medium">No Governance Receipt yet</p>
                    <p className="mt-1 text-xs text-foreground-muted">
                      {status === "failed"
                        ? "This run did not complete successfully — there's no delivered work to attest, so no receipt is presented."
                        : status === "blocked"
                          ? "Blocked missions don't produce a receipt — there's no validated work to attest."
                          : status === "done"
                            ? "Finalising the signed receipt for this mission's delivered work — refresh in a moment. If it remains unavailable, check receipt signing in the Operator Console."
                            : "A signed, verifiable receipt is issued once this mission delivers work — it attests the captured deliverable, token cost, and the policies enforced."}
                    </p>
                  </section>
                )}
              </div>
            ),
          },
        ]}
      />
    </div>
  );
}

type MissionServerTab = {
  id: string;
  label: string;
  badge?: number | string | null;
  node: ReactNode;
  live?: boolean;
};

function MissionServerTabs({
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

function EnvelopeFact({ label, value }: { label: string; value: string }) {
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

function ObjectiveBlock({ objective }: { objective: string }) {
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
function NextStep({
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

function AgentIdentityCard({ identity }: { identity: AgentIdentity }) {
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

function ArtifactsPanel({ ns, task, artifacts, pullRequests, activity, egress, tokens }: { ns: string; task: string; artifacts: MissionArtifact[]; pullRequests: import("@/lib/types").PullRequestRef[]; activity: import("@/lib/types").ActivityEvent[]; egress: string[]; tokens: number | null }) {
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

function ResultPanel({ result }: { result: MissionResult }) {
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
function FailureDiagnostic({
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

function CompositionPanel({ composition, launched }: { composition: Composition; launched: boolean }) {
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
