// kars Bridge Workspace — Mission detail (the live mission canvas).
//
// The user projection of a governed task: the objective, an always-visible
// governance envelope strip, the role tree (delegated children), the live
// activity stream, the decisions waiting on the user, the efficiency scorecard,
// and the Governance Receipt. No Kubernetes vocabulary surfaces.

import Link from "next/link";
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
import { EgressRequest } from "./egress-request";
import { DeleteMissionControl } from "./delete-control";
import { HonestState } from "@/components/honest-state";
import { PageHeader } from "@/components/ui";
import { TaskApprovalsPanel } from "@/app/tasks/[name]/task-approvals-panel";
import { ExecutionPanel } from "@/app/tasks/[name]/execution-panel";
import { BffError, getReceipt, getReview, getScorecard, getTroubleshoot, getTask, getCompliancePack, listTaskApprovals } from "@/lib/bff";
import { authWired, defaultNamespace, operatorIdentity } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import { egressScope } from "@/lib/format";
import { Icon } from "@/components/icon";
import { HaltButton } from "./halt-button";
import { TIER_LABELS, type MissionArtifact } from "@/lib/types";
import { MissionServerTabs, EnvelopeFact, ObjectiveBlock, NextStep, AgentIdentityCard, ArtifactsPanel, ResultPanel, FailureDiagnostic, CompositionPanel } from "./mission-detail-panels";


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
