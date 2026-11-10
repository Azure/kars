// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
function load(path, dependencies = {}) {
  const context = {
    exports: {}, URL, URLSearchParams, TextEncoder,
    require: name => {
      assert.ok(Object.hasOwn(dependencies, name), `Unexpected import: ${name}`);
      return dependencies[name];
    },
  };
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText, context);
  return context.exports;
}
const router = load("../src/lib/router-activity.ts");
const mission = load("../src/lib/mission-run-evidence.ts", { "./router-activity": router });
const context = { exports: load("../src/lib/team-run-evidence.ts", {
  "./mission-run-evidence": mission, "./router-activity": router,
}) };

const metricsTask = (overrides = {}) => ({
  artifacts: [], activity: [], result: null, launched: true, execution_phase: "Running",
  assignment: null, assignment_events: [], current_run_nonce: "rev-2",
  role_plan: { selected_roles: [], skipped_roles: [] }, collaboration_events: [],
  ...overrides,
});
const terminal = (overrides = {}) => ({
  assignment_nonce: "rev-2", status: "ok", reviewable: true, output: "Checked output",
  blocked: null, usage_known: true, total_tokens: 7178,
  finished_at: "2026-10-08T16:23:34Z", ...overrides,
});
const toolEvent = (overrides = {}) => ({
  kind: "tool", name: "file_write", runNonce: "rev-2", round: 1, ms: 1,
  ts: "2026-10-08T16:23:33Z", args_preview: "handoff.md", result_preview: "Written",
  ok: true, ...overrides,
});

test("system observations remain downloadable evidence, never a role or run output file", () => {
  const task = metricsTask({
    artifacts: [
      { name: "kars-router-observations.json", source_agent: "writer", content: "{}" },
      { name: "writer/report.md", source_agent: "writer", content: "Role output" },
      { name: "checklist.md", source_agent: null, content: "Run output" },
    ],
  });
  const evidence = context.exports.analyzeTeamRun({
    paused: false, roster: [{ name: "writer", system_prompt: "Write" }],
  }, task);
  assert.equal(evidence.roles[0].artifacts.length, 1);
  assert.equal(evidence.roles[0].artifacts[0].name, "writer/report.md");
  assert.equal(evidence.unattributedArtifacts.length, 1);
  assert.equal(evidence.unattributedArtifacts[0].name, "checklist.md");
  assert.equal(task.artifacts.length, 3, "raw retained evidence is not removed");
});

test("missing role, usage and tool evidence stays unknown rather than zero", () => {
  const metrics = context.exports.teamRunMetrics(metricsTask());
  for (const key of ["selectedRoles", "tokens", "toolCalls", "failedToolCalls", "rounds"]) {
    assert.equal(metrics[key], null, key);
  }
  assert.equal(metrics.roleLifecycleKnown, false);
  assert.equal(context.exports.teamRunMetrics(metricsTask({
    role_plan: { selected_roles: [], skipped_roles: ["qa"] },
  })).selectedRoles, null);
});

test("Team run evidence never substitutes the mutable Team envelope for a missing run digest", () => {
  const page = readFileSync(new URL(
    "../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url,
  ), "utf8");
  assert.match(page, /envelopeDigest=\{task\.envelope_digest\}/);
  assert.doesNotMatch(page, /envelopeDigest=\{[^}]*team\.envelope_digest/);
});

test("failed Team page shows retained failure without inventing a narrative or token diagnosis", () => {
  const page = readFileSync(new URL(
    "../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url,
  ), "utf8");
  assert.match(page, /metrics\.roleLifecycleKnown && \(role\.state === "missing"/);
  assert.match(page, /const failedToolCalls = metrics\.activity\.filter/);
  assert.match(page, /<details open[^>]*>[\s\S]*?Retained failure output[\s\S]*?\{missionFailureMessage\(currentResult\)\}/);
  assert.match(page, /currentResult && failed && !currentResult\.blocked/);
  assert.match(page, /currentResult && realDelivery &&/);
  assert.doesNotMatch(page, /produced a confident narrative|indicating repeated context replay|promptShare/);
});

test("only current explicitly known bounded usage is displayed, including genuine zero", () => {
  for (const value of [0, 7178, Number.MAX_SAFE_INTEGER]) {
    assert.equal(context.exports.teamRunMetrics(metricsTask({
      result: terminal({ total_tokens: value }),
    })).tokens, value);
  }
  for (const extra of [
    { usage_known: false }, { usage_known: undefined }, { total_tokens: null },
    { total_tokens: -1 }, { total_tokens: NaN }, { total_tokens: Infinity },
    { total_tokens: 0.5 }, { total_tokens: Number.MAX_SAFE_INTEGER + 1 },
    { assignment_nonce: "rev-1" }, { assignment_nonce: null },
  ]) {
    assert.equal(context.exports.teamRunMetrics(metricsTask({ result: terminal(extra) })).tokens, null);
  }
  assert.equal(context.exports.teamRunMetrics(metricsTask({
    current_run_nonce: null, result: terminal(),
  })).tokens, null);
});

test("Team flow metrics retain only bound current activity and assignment events", () => {
  const current = toolEvent();
  const metrics = context.exports.teamRunMetrics(metricsTask({
    activity: [current, toolEvent({ runNonce: "rev-1" }), toolEvent({ assignment_nonce: "rev-1" })],
    assignment_events: [{ task_id: "rev-1" }, { task_id: "rev-2", child_role: "writer" }],
    result: terminal({ run_evidence: { run_nonce: "rev-2", rounds: 3 } }),
    role_plan: { selected_roles: ["writer", "qa"], skipped_roles: [] },
  }));
  assert.equal(metrics.activity.length, 1);
  assert.equal(metrics.activity[0], current);
  assert.equal(metrics.assignmentEvents.length, 1);
  assert.equal(metrics.assignmentEvents[0].task_id, "rev-2");
  assert.equal(metrics.roleLifecycleKnown, true);
  assert.equal(metrics.toolCalls, 1);
  assert.equal(metrics.failedToolCalls, 0);
  assert.equal(metrics.rounds, 3);
  assert.equal(metrics.selectedRoles, 2);
  assert.equal(context.exports.teamRunMetrics(metricsTask({
    result: terminal({ run_evidence: { run_nonce: "rev-1", rounds: 3 } }),
  })).rounds, null);
  assert.equal(context.exports.teamRunMetrics(metricsTask({
    assignment_events: [{ task_id: "rev-1", child_role: "writer" }],
  })).roleLifecycleKnown, false);
});

const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const graphActivity = load("../src/components/agent-graph/activity.ts", { "@/lib/router-activity": router });
const { buildAgentExecutions } = load("../src/components/agent-graph/execution.ts", {
  "./activity": graphActivity, "@/lib/router-activity": router,
  "@/lib/team-run-evidence": context.exports,
});
const lifetime = load("../src/components/execution-lifetime.tsx", {
  react: React, "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/icon": { Icon: () => null },
  "@/components/live-refresh": { LivePulse: ({ label }) => React.createElement("span", {}, label) },
  "@/components/agent-graph/activity": graphActivity,
  "@/lib/router-activity": router,
  "@/lib/team-run-evidence": context.exports,
});
const { ExecutionLifetime } = lifetime;
const { TeamRunFlow } = load("../src/components/team-run-flow.tsx", {
  react: React, "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/navigation-link": { default: ({ href, children }) => React.createElement("a", { href }, children) },
  "@/lib/mission-run-evidence": mission,
  "@/lib/team-run-evidence": context.exports,
  "@/lib/router-activity": router,
  "@/components/agent-graph/activity": graphActivity,
  "@/components/execution-lifetime": lifetime,
});
const { RouterActivityPanel } = load("../src/components/router-activity-panel.tsx", {
  "react/jsx-runtime": require("react/jsx-runtime"),
  "@/lib/mission-run-evidence": mission, "@/lib/router-activity": router,
});
let streamedPhases = {}, streamedRoot = null, observingStream = false;
const { ExecutionExplorer } = load("../src/components/execution-explorer.tsx", {
  react: React, "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/activity-stream": { ActivityStream: () => React.createElement("p", {}, "Legacy activity") },
  "@/app/workspace/missions/[name]/deploy-timeline": { DeployTimeline: () => null },
  "@/components/agent-graph": { AgentGraph: () => null },
  "@/components/execution-lifetime": { ExecutionLifetime },
  "@/components/use-live-trace": { useLiveTrace: (_ns, _name, observing, events) => {
    observingStream = observing;
    return { events, routerActivity: streamedRoot, phaseActivity: streamedPhases };
  } },
  "@/components/router-activity-panel": { RouterActivityPanel },
  "@/components/team-run-flow": { TeamRunFlow },
  "@/lib/router-activity": router, "@/lib/team-run-evidence": context.exports,
});
const routerFixture = JSON.parse(readFileSync(new URL(
  "../../../inference-router/tests/fixtures/router-observations-v1.json", import.meta.url,
), "utf8"));

function observedPhase(role, index) {
  const nonce = `phase-${index}`;
  return {
    role, phase: `step-${index}`, task_name: `task-${index}`, task_uid: `uid-${index}`,
    run_nonce: nonce, state: "completed", router_activity: structuredClone(routerFixture),
    result: terminal({
      assignment_nonce: nonce, source: "durable_agent", model: "gpt-5.4-mini",
      run_evidence: {
        task_uid: `uid-${index}`, run_nonce: nonce, assignment_id: `assignment-${index}`,
        agent_name: role, agent_did: `did:mesh:${role}`, dispatcher_did: "did:mesh:dispatcher",
        sandbox_uid: `sandbox-${index}`, pod_uid: `pod-${index}`, runtime_boot_id: `boot-${index}`,
        status: "succeeded", started_at: "2026-10-08T18:04:00Z", finished_at: "2026-10-08T18:05:00Z",
      },
    }),
  };
}

test("failed Team root retains recorded router metrics and producer binding, not delivery", () => {
  const root = observedPhase("principal", 0);
  root.result.status = "failed";
  root.result.reviewable = false;
  root.result.output = "";
  root.result.error = "Required exports missing";
  root.result.run_evidence.status = "failed";
  const task = metricsTask({
    current_run_nonce: root.run_nonce, result: root.result, router_activity: root.router_activity,
  });
  const state = mission.missionRunState(task);
  assert.equal(state.realDelivery, false);
  assert.equal(state.failed, true);
  assert.equal(state.routerActivity, root.router_activity);
  const metrics = context.exports.teamRunMetrics(task);
  assert.equal(metrics.reportedToolResults,
    root.router_activity.trace.events.filter(event => event.kind === "tool_result").length);
  assert.equal(metrics.tokens, root.result.total_tokens);
  assert.equal(mission.missionRunState({ ...task, current_run_nonce: "next-revision" }).routerActivity, null);
  assert.equal(context.exports.teamRunMetrics({ ...task, current_run_nonce: "next-revision" }).reportedToolResults, null);
});

function activePhase(role, index) {
  const phase = observedPhase(role, index);
  const live = { ...phase.result.run_evidence, attempt_uid: `attempt-${index}`, progress_sequence: 2,
    observations: phase.router_activity };
  return { ...phase, state: "active", result: null, router_activity: null, live_router_activity: live };
}

test("active specialist observations are display-only with exact phase identity and terminal precedence", () => {
  const phase = activePhase("researcher", 1);
  const binding = context.exports.displayedPhaseRouterActivity(phase);
  assert.equal(binding.provisional, true);
  assert.equal(context.exports.reviewedPhaseRouterActivity(phase), null);
  for (const patch of [{ task_uid: "replaced" }, { run_nonce: "old" }, { state: "completed" },
    { state: "uncertain" }, { state: "unavailable" }, { result: terminal() }]) {
    assert.equal(context.exports.displayedPhaseRouterActivity({ ...phase, ...patch }), null);
  }
  const completed = { ...observedPhase("researcher", 1), live_router_activity: phase.live_router_activity };
  assert.equal(context.exports.displayedPhaseRouterActivity(completed).provisional, false);
  const task = researchTask([phase]);
  const metrics = context.exports.teamRunMetrics(task);
  assert.equal(metrics.tokens, null);
  assert.equal(metrics.toolCalls, null);
  assert.equal(context.exports.analyzeTeamRun({ paused: false, roster: [] }, task).research.length, 0);
});

function stoppedPhase(role = "researcher", index = 1) {
  const phase = activePhase(role, index);
  return { ...phase, state: "stopped", stopped_activity: { ...phase.live_router_activity, runtime: "OpenClaw" },
    live_router_activity: null };
}

test("stopped progress survives reload but not identity changes, result precedence or stream revocation", () => {
  const phase = stoppedPhase();
  const binding = context.exports.displayedPhaseRouterActivity(phase);
  assert.equal(binding.provisional, true);
  assert.equal(binding.stopped, true);
  assert.equal(binding.producer.runtime, "OpenClaw");
  for (const patch of [{ task_uid: "replaced" }, { run_nonce: "old" }, { state: "active" },
    { state: "held" }, { state: "completed" }, { state: "unavailable" }, { result: terminal() },
    { stopped_activity: { ...phase.stopped_activity, runtime: "gpt-6-astra" } },
    { stopped_activity: { ...phase.stopped_activity, observations: { state: "invented" } } }]) {
    assert.equal(context.exports.displayedPhaseRouterActivity({ ...phase, ...patch }), null);
  }
  const completed = { ...observedPhase("researcher", 1), stopped_activity: phase.stopped_activity };
  assert.equal(context.exports.displayedPhaseRouterActivity(completed).provisional, false);
  assert.equal(context.exports.reviewedPhaseRouterActivity(phase), null);
  const task = researchTask([phase], { launched: false, execution_phase: "Idle" });
  const metrics = context.exports.teamRunMetrics(task);
  assert.equal(metrics.tokens, null);
  assert.equal(metrics.toolCalls, null);
  assert.equal(metrics.rounds, null);
  assert.equal(context.exports.hasActiveReviewedPhases(task), false);
  const evidence = context.exports.analyzeTeamRun({ paused: true, roster: [{ name: "researcher" }] }, task);
  assert.equal(evidence.roles[0].state, "stopped");
  assert.equal(evidence.research.length, 0);
});

test("stopped observations preserve actual runtime and actions without live pulses or terminal download authority", () => {
  const phase = stoppedPhase();
  const binding = context.exports.displayedPhaseRouterActivity(phase);
  const agents = buildAgentExecutions({ agentLabel: "Principal", name: "root", agentPhase: "Idle",
    agentModel: null, agentRuntime: null, running: false, events: [], subAgents: [], phases: [phase] });
  assert.equal(agents[1].phase, "Stopped");
  assert.equal(agents[1].runtime, "OpenClaw");
  assert.ok(agents[1].actions.length > 0);
  const html = renderToStaticMarkup(React.createElement(ExecutionLifetime, {
    running: false, activity: [], assignmentEvents: [], approvals: [], phases: [phase],
  }));
  assert.match(html, /stopped, partial/);
  assert.match(html, /did:mesh:researcher/);
  assert.doesNotMatch(html, /Recording live|Download system evidence/);
  const panel = renderToStaticMarkup(React.createElement(RouterActivityPanel, {
    binding, ns: "kars-system", name: phase.task_name,
  }));
  assert.match(panel, /Last recorded partial observations before stop/);
  assert.match(panel, /not final usage accounting/);
  assert.doesNotMatch(panel, /Live, provisional|completed revision|Download system evidence/);
});

test("Explorer reloads stopped root and phase progress without opening a stream or replacing terminal evidence", () => {
  const phase = stoppedPhase();
  const props = { running: false, observing: false, runNonce: phase.run_nonce, activity: [], telemetry: null,
    assignmentEvents: [], approvals: [], ns: "kars-system", name: "root", agentLabel: "Principal" };
  const render = extra => renderToStaticMarkup(React.createElement(ExecutionExplorer, { ...props, ...extra }));
  const root = render({ stoppedActivity: phase.stopped_activity });
  assert.equal(observingStream, false);
  assert.match(root, /Last recorded partial observations before stop/);
  assert.doesNotMatch(root, /Recording live/);
  const child = render({ phases: [phase] });
  assert.match(child, /stopped, partial/);
  assert.doesNotMatch(child, /Recording live/);
  for (const patch of [{ runNonce: "replacement" }, { running: true, observing: true }, { observing: true }]) {
    assert.doesNotMatch(render({ stoppedActivity: phase.stopped_activity, ...patch }), /stopped, partial|Last recorded partial/);
  }
  const terminalPhase = observedPhase("researcher", 1);
  const terminalBinding = context.exports.reviewedPhaseRouterActivity(terminalPhase);
  assert.doesNotMatch(render({ stoppedActivity: phase.stopped_activity, routerActivity: terminalBinding }), /stopped, partial|Last recorded partial/);
});

test("Team scheduling pause preserves current reviewed phase evidence", () => {
  const task = researchTask([activePhase("builder", 1)], { execution_phase: "Preparing" });
  const enabled = context.exports.analyzeTeamRun({ paused: false, roster: [{ name: "builder" }] }, task);
  const paused = context.exports.analyzeTeamRun({ paused: true, roster: [{ name: "builder" }] }, task);
  assert.equal(paused.outcome, "running");
  assert.deepEqual(paused, enabled);
});

test("Team headline includes current reviewed member work without changing principal state", () => {
  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/page.tsx", import.meta.url), "utf8");
  const expression = page.match(/const runRunning = (Boolean\([\s\S]*?\n  \));/);
  assert.ok(expression, "Team page must derive current assignment activity");
  const running = (latestRunTask, runInFlight = true) => runInNewContext(expression[1], {
    latestRunTask, runInFlight, hasActiveReviewedPhases: context.exports.hasActiveReviewedPhases,
  });
  const task = researchTask([activePhase("builder", 1)], { execution_phase: "Preparing" });
  assert.equal(running(task), true);
  assert.equal(task.execution_phase, "Preparing");
  assert.equal(running(task, false), false, "paused, held or terminal work cannot appear Working");
  assert.equal(running(null), false);
  for (const change of [value => { value.current_run_nonce = "old-root"; },
    value => { value.reviewed_execution.issue = "custody changed"; },
    value => { value.reviewed_execution.phases[0].task_uid = null; },
    value => { value.reviewed_execution.phases[0].state = "completed"; },
    value => { value.reviewed_execution.phases[0].state = "pending"; }]) {
    const changed = structuredClone(task); change(changed);
    assert.equal(running(changed), false);
  }
  assert.equal(running({ ...task, reviewed_execution: null, execution_phase: "Running" }), true);
  assert.match(page, /runRunning=\{runRunning\}/);
});

test("active sibling keeps observation alive after failure without claiming the principal is running", () => {
  const phase = activePhase("researcher", 1);
  const task = researchTask([phase, failedObservedPhase()], {
    execution_phase: "Degraded", execution_detail: "reviewed execution held: failed sibling",
  });
  assert.equal(context.exports.hasActiveReviewedPhases(task), true);
  assert.equal(context.exports.analyzeTeamRun({ paused: true, roster: [] }, task).outcome, "failed");
  for (const change of [value => { value.current_run_nonce = "old-root"; },
    value => { value.reviewed_execution.issue = "custody changed"; },
    value => { value.reviewed_execution.phases[0].task_uid = null; },
    value => { value.reviewed_execution.phases[0].run_nonce = null; },
    value => { value.reviewed_execution.phases[0].state = "completed"; },
    value => { value.reviewed_execution.phases[0].result = terminal(); }]) {
    const changed = structuredClone(task); change(changed);
    assert.equal(context.exports.hasActiveReviewedPhases(changed), false);
  }
});

test("provisional phase appears in graph and timeline without fabricated completion or downloads", () => {
  const phase = activePhase("researcher", 1);
  const agents = buildAgentExecutions({ agentLabel: "Principal", name: "root", agentPhase: "Failed",
    agentModel: null, agentRuntime: null, running: false, events: [], subAgents: [], phases: [phase] });
  assert.equal(agents[0].phase, "Failed");
  assert.equal(agents[1].phase, "Running");
  assert.ok(agents[1].actions.length > 0);
  const html = renderToStaticMarkup(React.createElement(ExecutionLifetime, {
    running: false, activity: [], assignmentEvents: [], approvals: [], phases: [phase],
  }));
  assert.match(html, /provisional/);
  assert.match(html, /did:mesh:researcher/);
  assert.doesNotMatch(html, /researcher \/ step-1: completed|Download system evidence/);
  const panel = renderToStaticMarkup(React.createElement(RouterActivityPanel, {
    binding: context.exports.displayedPhaseRouterActivity(phase), ns: "kars-system", name: phase.task_name,
  }));
  assert.match(panel, /provisional/i);
  assert.doesNotMatch(panel, /Download system evidence/);
});

test("Explorer joins current phase frames and observes active siblings independently of root status", () => {
  const phase = activePhase("researcher", 1);
  streamedPhases = { [router.reviewedPhaseKey(phase)]: phase.live_router_activity };
  try {
    const html = renderToStaticMarkup(React.createElement(ExecutionExplorer, {
      running: false, observing: true, runNonce: "rev-2", activity: [], telemetry: null,
      assignmentEvents: [], approvals: [], ns: "kars-system", name: "root", agentLabel: "Principal",
      agentPhase: "Failed", phases: [{ ...phase, live_router_activity: null }],
    }));
    assert.equal(observingStream, true);
    assert.match(html, /provisional/);
    assert.match(html, /did:mesh:researcher/);
  } finally {
    streamedPhases = {};
  }
});

function egressPhase(role, index, failed = false) {
  const phase = failed ? failedObservedPhase() : observedPhase(role, index);
  const identity = { scope_id: phase.router_activity.scope_id, round: 1, call_id: "fetch-1", name: "web_fetch", ms: 1 };
  phase.router_activity.trace.events.push(
    { ...identity, seq: 90, kind: "router_operation", source: "router-policy", operation: "egress_policy",
      flag: "egress_check", allowed: true, http_status: null, complete: null,
      destination_host: "kubernetes.io", destination_port: 443 },
    { ...identity, seq: 91, kind: "router_operation", source: "router", operation: "egress_fetch",
      flag: "response", allowed: null, http_status: 403, complete: true,
      destination_host: "kubernetes.io", destination_port: 443 },
    { ...identity, seq: 92, kind: "router_operation", source: "router-policy", operation: "tool_policy",
      flag: "governance", allowed: true, http_status: null, complete: null,
      destination_host: null, destination_port: null },
  );
  return phase;
}

function researchTask(phases, overrides = {}) {
  return metricsTask({ reviewed_execution: { run_nonce: "rev-2", issue: null, phases }, ...overrides });
}
const research = task => context.exports.analyzeTeamRun({ paused: true, roster: [] }, task).research;

test("Research aggregates bound successful and failed roles, not governance or model calls", () => {
  const good = egressPhase("researcher", 1);
  const failed = egressPhase("reviewer", 2, true);
  const events = research(researchTask([good, failed, good]));
  assert.equal(events.length, 4, "deduplicate exact observations but keep distinct producers");
  assert.deepEqual(Array.from(events, event => event.agent), ["researcher", "researcher", "reviewer", "reviewer"]);
  for (const event of events) {
    assert.equal(event.url, null, "host and port are not a retained URL");
    assert.equal(event.at, null, "do not borrow a phase or response timestamp");
    assert.equal(event.outcome, null, "neither policy permission nor HTTP status is harness success");
    assert.equal(event.host, "kubernetes.io");
  }
  assert.match(router.routerOperationSummary(events[0].operation), /flag: egress_check; decision: allow/);
  assert.match(router.routerOperationSummary(events[1].operation), /upstream HTTP 403.*Not a harness outcome/);
});

test("Research rejects stale, unavailable and mismatched reviewed producer observations", () => {
  for (const change of [
    task => { task.current_run_nonce = "replacement"; },
    task => { task.reviewed_execution.run_nonce = "old"; },
    task => { task.reviewed_execution.issue = "Custody changed"; },
    task => { task.reviewed_execution.phases[0].task_uid = "replacement"; },
    task => { task.reviewed_execution.phases[0].run_nonce = "replacement"; },
    task => { task.reviewed_execution.phases[0].result.assignment_nonce = "old"; },
    task => { task.reviewed_execution.phases[0].result.source = "single_turn"; },
    task => { task.reviewed_execution.phases[0].result.run_evidence.status = "succeeded"; },
    task => { task.reviewed_execution.phases[0].router_activity = { state: "unavailable" }; },
  ]) {
    const task = researchTask([egressPhase("reviewer", 2, true)]);
    change(task);
    assert.equal(research(task).length, 0, change.toString());
  }
});

test("Research preserves legacy HTTP evidence and uses the existing principal revision binding", () => {
  const root = egressPhase("principal", 0);
  const task = researchTask([], {
    current_run_nonce: root.run_nonce, result: root.result, router_activity: root.router_activity,
    activity: [toolEvent({ name: "http_fetch", source: "router", args_preview: "https://docs.rs/kube", result_preview: "HTTP 200" })],
  });
  const events = research(task);
  assert.equal(events.length, 3);
  assert.equal(events[0].url, "https://docs.rs/kube");
  assert.equal(events[0].status, 200);
  assert.equal(events[0].outcome, "success");
  assert.equal(events[1].agent, "principal");
  task.current_run_nonce = "replacement";
  assert.equal(research(task).length, 1, "stale principal observations excluded, legacy behavior unchanged");
});

test("actual drill-down renders root and every phase's recorded observations and lifecycle", () => {
  const root = observedPhase("principal", 0);
  const phases = ["coordinator", "writer", "reviewer"].map((role, index) => observedPhase(role, index + 1));
  const props = {
    running: false, activity: [], telemetry: null, assignmentEvents: [], approvals: [],
    ns: "kars-system", name: root.task_name, runNonce: root.run_nonce, agentLabel: "Team",
    phases, routerActivity: context.exports.reviewedPhaseRouterActivity(root),
  };
  const html = renderToStaticMarkup(React.createElement(ExecutionExplorer, props));
  assert.doesNotMatch(html, /No records available|No timeline records|Legacy activity/);
  for (const phase of phases) {
    assert.ok(html.includes(`${phase.role} / ${phase.phase}: started`));
    assert.ok(html.includes(`${phase.role} / ${phase.phase}: completed`));
    assert.ok(html.includes(phase.result.run_evidence.agent_did));
  }
  assert.match(html, /Router response|Model proposed|Harness reported/);
  assert.match(html, /Timestamp unavailable/);
  assert.match(html, /no ordering between producers is inferred/);
  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url), "utf8");
  assert.match(page, /<ExecutionExplorer\s+teamFlow=\{\{ task, evidence \}\}\s+startupTask=\{task\.result == null \? task : null\}\s+stoppedActivity=\{task\.stopped_activity\}\s+routerActivity=/);
  assert.match(page, /phases=\{metrics\.phases \?\? \[\]\}/);
});

test("graph keeps reviewed phases alongside the recorded principal with exact producer focus", () => {
  const root = observedPhase("principal", 0);
  const phases = ["coordinator", "writer", "reviewer"].map((role, index) => observedPhase(role, index + 1));
  root.result.run_evidence.runtime = "OpenClaw";
  phases.forEach((phase, index) => { phase.result.run_evidence.runtime = index === 2 ? "Hermes" : "OpenClaw"; });
  const agents = buildAgentExecutions({
    agentLabel: "Team", agentModel: null, agentPhase: "succeeded", agentRuntime: null,
    events: [], name: root.task_name, running: false, subAgents: [],
    routerActivity: context.exports.reviewedPhaseRouterActivity(root), phases,
  });
  assert.equal(agents.length, 4);
  assert.equal(agents[0].runtime, "OpenClaw");
  for (const [index, phase] of phases.entries()) {
    const agent = agents[index + 1];
    const binding = context.exports.reviewedPhaseRouterActivity(phase);
    assert.equal(agent.runtime, index === 2 ? "Hermes" : "OpenClaw");
    assert.equal(agent.parentId, "principal");
    assert.equal(agent.isPrincipal, false);
    assert.equal(agent.displayName, `${phase.role} / ${phase.phase}`);
    assert.equal(agent.actions.length, routerFixture.trace.events.length);
    assert.equal(router.matchesRouterFocus(binding, agent.traceQuery), true);
    assert.equal(router.matchesRouterFocus(context.exports.reviewedPhaseRouterActivity(root), agent.traceQuery), false);
    const actionQuery = graphActivity.exactActionQuery(agent, agent.actions[0]);
    assert.equal(router.matchesRouterFocus(binding, actionQuery), true);
    assert.equal(router.matchesRouterFocus(context.exports.reviewedPhaseRouterActivity(root), actionQuery), false);
    const html = renderToStaticMarkup(React.createElement(RouterActivityPanel, {
      binding, ns: "kars-system", name: phase.task_name, focusQuery: actionQuery,
    }));
    assert.ok(html.includes(`/tasks/${phase.task_name}/artifact/`));
    assert.ok(html.includes(`run_nonce=${phase.run_nonce}`));
  }
});

test("phase observations refuse stale UID, nonce, mismatched status and unbound producer without borrowing principal records", () => {
  for (const change of [
    phase => { phase.task_uid = "different"; },
    phase => { phase.run_nonce = "different"; },
    phase => { phase.result.assignment_nonce = "different"; },
    phase => { phase.result.source = "router"; },
    phase => { phase.result.status = "failed"; },
    phase => { phase.result.run_evidence.status = "failed"; },
    phase => { phase.result = null; },
  ]) {
    const phase = observedPhase("writer", 1);
    change(phase);
    assert.equal(context.exports.reviewedPhaseRouterActivity(phase), null);
  }
  const binding = context.exports.reviewedPhaseRouterActivity(observedPhase("writer", 1));
  for (const query of ["phase:uid-1", "observations:ids=bad", "observations:producer=unknown", "observations:ids=[]"]) {
    assert.equal(router.matchesRouterFocus(binding, query), false);
  }
});

function failedObservedPhase() {
  const phase = observedPhase("reviewer", 2);
  phase.state = "failed";
  phase.result.status = "failed";
  phase.result.reviewable = false;
  phase.result.error = "Required exports missing";
  phase.result.output = "Retained failed reviewer output";
  phase.result.run_evidence.status = "failed";
  return phase;
}

test("failed reviewed evidence survives metrics and graph projection without claiming delivery or tool success", () => {
  const phase = failedObservedPhase();
  const task = metricsTask({ result: null, reviewed_execution: {
    run_nonce: "rev-2", issue: null, phases: [phase],
  } });
  const metrics = context.exports.teamRunMetrics(task);
  const binding = context.exports.reviewedPhaseRouterActivity(metrics.phases[0]);
  assert.equal(binding.observations, phase.router_activity);
  assert.equal(binding.producer, phase.result.run_evidence);
  assert.equal(binding.provisional, false);
  assert.equal(metrics.currentResult, null, "principal synthesis never started");
  assert.equal(metrics.reportedToolResults, 1);
  assert.equal(metrics.toolCalls, null, "router reports are not verified execution");
  assert.equal(metrics.rounds, null, "no aggregate principal round count is known");
  assert.match(mission.missionFailureMessage(metrics.phases[0].result), /Required exports missing/);
  assert.match(mission.missionFailureMessage(metrics.phases[0].result), /Retained failed reviewer output/);
  const evidence = context.exports.analyzeTeamRun({ paused: true, roster: [{ name: "reviewer" }] }, task);
  assert.equal(evidence.outcome, "failed");
  assert.equal(evidence.roles[0].state, "failed");
  const agents = buildAgentExecutions({
    agentLabel: "Team", agentPhase: "failed", events: [], name: "root", running: false,
    subAgents: [], phases: metrics.phases,
  });
  assert.equal(agents[0].actions.length, 0, "no observations borrowed for principal");
  assert.equal(agents[1].phase, "failed");
  assert.equal(agents[1].actions.length, routerFixture.trace.events.length);
  assert.equal(agents[1].rounds, 2);
  assert.equal(agents[1].failures, 1);
  assert.equal(router.matchesRouterFocus(binding, agents[1].traceQuery), true);
});

test("failed phase observations still reject missing or mismatched identity and terminal evidence", () => {
  for (const change of [
    phase => { phase.task_uid = "replacement"; },
    phase => { phase.task_uid = null; },
    phase => { phase.run_nonce = "stale"; },
    phase => { phase.run_nonce = null; },
    phase => { phase.result.assignment_nonce = "stale"; },
    phase => { phase.result.source = "single_turn"; },
    phase => { phase.result.run_evidence.status = "succeeded"; },
    phase => { phase.result.status = "ok"; },
    phase => { phase.result.status = "rejected"; },
    phase => { phase.result.status = "error"; },
    phase => { phase.result.run_evidence = null; },
    phase => { phase.result = null; },
    phase => { phase.router_activity = null; },
  ]) {
    const phase = failedObservedPhase();
    change(phase);
    assert.equal(context.exports.reviewedPhaseRouterActivity(phase), null, change.toString());
  }
  for (const change of [
    task => { task.current_run_nonce = "new-root"; },
    task => { task.reviewed_execution.issue = "Custody changed"; },
    task => { task.reviewed_execution.phases[0].task_uid = "replacement"; },
  ]) {
    const task = metricsTask({ reviewed_execution: {
      run_nonce: "rev-2", issue: null, phases: [failedObservedPhase()],
    } });
    change(task);
    assert.equal(context.exports.teamRunMetrics(task).reportedToolResults, null);
  }
  const phase = failedObservedPhase();
  phase.router_activity = { ...routerFixture, state: "unavailable", reason: "Retained observation unavailable" };
  delete phase.router_activity.trace;
  const task = metricsTask({ reviewed_execution: { run_nonce: "rev-2", issue: null, phases: [phase] } });
  assert.equal(context.exports.reviewedPhaseRouterActivity(phase).observations.state, "unavailable");
  assert.equal(context.exports.teamRunMetrics(task).reportedToolResults, null);
});

function reviewedTask() {
  return metricsTask({
    launched: false, result: terminal({ total_tokens: 11475 }),
    reviewed_execution: {
      run_nonce: "rev-2", issue: null,
      phases: ["coordinator", "checklist-writer", "reviewer"].map((role, index) => ({
        role, phase: `phase-${index}`, task_name: `reserved-${index}`, task_uid: `uid-${index}`,
        run_nonce: `phase-nonce-${index}`, state: "completed",
        result: terminal({
          assignment_nonce: `phase-nonce-${index}`, total_tokens: [3298, 7105, 6702][index],
          run_evidence: {
            task_uid: `uid-${index}`, run_nonce: `phase-nonce-${index}`,
            agent_name: role, status: "succeeded", rounds: 2,
            started_at: `2026-10-08T18:0${index}:00Z`, finished_at: `2026-10-08T18:0${index}:30Z`,
          },
        }),
      })),
    },
  });
}

test("canonical phases populate the real Team chronology and aggregate cost without inventing a ledger", () => {
  const task = reviewedTask();
  const metrics = context.exports.teamRunMetrics(task);
  assert.equal(metrics.tokens, 28580);
  assert.equal(metrics.selectedRoles, 3);
  assert.equal(metrics.assignmentEvents.length, 0);
  const result = context.exports.analyzeTeamRun({
    paused: false, roster: task.reviewed_execution.phases.map(({ role }) => ({ name: role })),
  }, task);
  assert.equal(result.outcome, "delivered");
  assert.equal(result.evidenceMode, "reviewed-phases");
  assert.ok(result.roles.every((role) => role.state === "delivered" && role.handbacks.length === 0));
  const html = flowHtml(task);
  for (const text of ["coordinator", "checklist-writer", "reviewer", "phase-0 started",
    "18:00:30 UTC", "28,580 recorded tokens", "/workspace/missions/reserved-0?tab=artifacts"]) {
    assert.ok(html.includes(text), text);
  }
});

test("a current failed phase stops the running claim even before principal assignment or result", () => {
  const task = reviewedTask();
  task.launched = true;
  task.execution_phase = "Degraded";
  task.result = null;
  const phase = task.reviewed_execution.phases[1];
  phase.state = "failed";
  phase.result.status = "failed";
  phase.result.reviewable = false;
  phase.result.output = "";
  phase.result.error = "Governed inference budget /v1/catalog unavailable/denied (HTTP Some(503))";
  phase.result.run_evidence.status = "failed";
  task.reviewed_execution.phases[2].state = "pending";
  task.reviewed_execution.phases[2].result = null;
  const team = { paused: false, roster: task.reviewed_execution.phases.map(({ role }) => ({ name: role })) };
  assert.equal(context.exports.analyzeTeamRun(team, task).outcome, "failed");
  assert.equal(context.exports.analyzeTeamRun({ ...team, paused: true }, task).outcome, "failed");
  assert.match(mission.missionFailureMessage(context.exports.teamRunMetrics(task).phases[1].result), /catalog/);
  for (const mutate of [
    value => { value.reviewed_execution.run_nonce = "stale"; },
    value => { value.reviewed_execution.phases[1].task_uid = "replacement"; },
    value => { value.reviewed_execution.phases[1].result.assignment_nonce = "stale"; },
  ]) {
    const stale = structuredClone(task);
    mutate(stale);
    assert.notEqual(context.exports.analyzeTeamRun(team, stale).outcome, "failed");
  }
});

test("uncommitted failed phase diagnostics render in the timeline without delivery or accounting authority", () => {
  const diagnostic = "Native per-round usage verification failed: count mismatch (assistants=18, calls=15, turns=15)";
  const phase = { ...observedPhase("builder", 1), state: "failed", result: null,
    router_activity: null, failure_diagnostic: diagnostic };
  const task = researchTask([phase], { execution_phase: "Degraded" });
  const team = { paused: false, roster: [{ name: "builder" }] };
  const evidence = context.exports.analyzeTeamRun(team, task);
  const metrics = context.exports.teamRunMetrics(task);
  assert.equal(evidence.outcome, "failed");
  assert.equal(metrics.phases[0].result, null);
  assert.equal(metrics.tokens, null);
  assert.equal(metrics.rounds, null);
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(context.exports.reviewedPhaseRouterActivity(metrics.phases[0]), null);
  const html = renderToStaticMarkup(React.createElement(TeamRunFlow, { task, evidence, phases: metrics.phases }));
  assert.ok(html.includes(diagnostic));
  assert.ok(!html.includes("No current terminal record is available for this phase."));
  const legacy = { ...phase, failure_diagnostic: undefined };
  assert.equal(mission.reviewedPhaseFailureMessage(legacy), mission.missionFailureMessage(null));
  for (const overrides of [
    { current_run_nonce: "another-run" },
    { reviewed_execution: { ...task.reviewed_execution, issue: "custody changed" } },
  ]) assert.equal(context.exports.reviewedPhases({ ...task, ...overrides }), null);
  const stale = { ...phase, result: terminal({ assignment_nonce: "old" }) };
  const sanitized = context.exports.reviewedPhases(researchTask([stale]))[0];
  assert.equal(sanitized.state, "unavailable");
  assert.equal(sanitized.failure_diagnostic, null);
  assert.equal(mission.reviewedPhaseFailureMessage(sanitized), mission.missionFailureMessage(null));

  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url), "utf8");
  assert.match(page, /Why this run failed[\s\S]*?reviewedPhaseFailureMessage\(phase\)/);
  assert.match(page, /\) : phase\.state === "failed" \? \([\s\S]*?reviewedPhaseFailureMessage\(phase\)/);
  assert.doesNotMatch(page, /missionFailureMessage\(phase\.result\)/);
});

function heldTask(state = "uncertain") {
  const task = reviewedTask();
  task.launched = true;
  task.execution_phase = "Degraded";
  task.execution_detail = "reviewed execution held: reviewed phase failed or ambiguous; synthesis prohibited";
  task.result = null;
  const phases = task.reviewed_execution.phases;
  phases[0].role = "builder";
  phases[0].result.output = "Last valid builder handoff";
  phases[1].role = "reviewer";
  phases[1].state = state;
  phases[1].result = null;
  phases[2].role = "integrator";
  phases[2].state = "pending";
  phases[2].result = null;
  return task;
}
const heldTeam = { paused: false, roster: ["builder", "reviewer", "integrator"].map(name => ({ name })) };

test("held reviewed execution overrides launch, stale running ledger and absent principal assignment", () => {
  for (const state of ["uncertain", "active", "unavailable"]) {
    for (const assignment of [null, { task_id: "rev-2", state: "Running" }]) {
      const task = heldTask(state);
      task.assignment = assignment;
      const before = JSON.stringify(task);
      const evidence = context.exports.analyzeTeamRun(heldTeam, task);
      assert.equal(evidence.outcome, "held");
      assert.equal(context.exports.analyzeTeamRun({ ...heldTeam, paused: true }, task).outcome, "held");
      assert.deepEqual(Array.from(evidence.roles, r => r.state), ["delivered", state === "uncertain" ? "uncertain" : state === "active" ? "held" : "missing", "missing"]);
      const metrics = context.exports.teamRunMetrics(task);
      assert.equal(metrics.tokens, null);
      assert.equal(metrics.phases[0].result.output, "Last valid builder handoff");
      assert.equal(task.artifacts.length, 0);
      assert.equal(metrics.phases[1].result, null);
      assert.notEqual(metrics.phases[1].state, "active");
      assert.equal(JSON.stringify(task), before, "projection must not mutate retained records");
    }
  }
});

test("uncertain principal assembly stays held after every specialist has completed", () => {
  for (const assignment of [null, { task_id: "rev-2", state: "Running" }]) {
    const task = reviewedTask();
    task.launched = true;
    task.result = null;
    task.assignment = assignment;
    task.execution_phase = "Degraded";
    task.execution_detail = "reviewed execution held: Final assembly needs attention: the recorded outcome is uncertain.";
    const team = { paused: false, roster: task.reviewed_execution.phases.map(p => ({ name: p.role })) };
    const before = JSON.stringify(task);
    const evidence = context.exports.analyzeTeamRun(team, task);
    assert.equal(evidence.outcome, "held");
    assert.ok(evidence.roles.every(role => role.state === "delivered"));
    assert.ok(context.exports.reviewedPhases(task).every(phase => phase.state === "completed"));
    assert.equal(JSON.stringify(task), before);
  }
});

test("a bound uncertain phase holds a run even before the root status catches up", () => {
  const task = heldTask();
  task.execution_phase = "Running";
  task.execution_detail = null;
  assert.equal(context.exports.analyzeTeamRun(heldTeam, task).outcome, "held");
  task.reviewed_execution.run_nonce = "previous-run";
  assert.equal(context.exports.analyzeTeamRun(heldTeam, task).outcome, "running");
});

test("held status cannot erase a recorded phase failure or claim a final delivery", () => {
  const task = heldTask();
  task.reviewed_execution.phases[1].state = "failed";
  assert.equal(context.exports.analyzeTeamRun(heldTeam, task).outcome, "failed");
  task.reviewed_execution.phases[1].state = "uncertain";
  task.result = terminal();
  task.assignment = { task_id: "rev-2", state: "Completed" };
  assert.equal(context.exports.analyzeTeamRun(heldTeam, task).outcome, "held");
});

test("rendered Team run preserves completed builder evidence while reviewer is uncertain, held or failed", async () => {
  for (const phaseState of ["uncertain", "active", "failed"]) {
    const task = { ...heldTask(phaseState), team: "capacity-review", name: "held-run", sub_agents: [] };
    if (phaseState === "failed") task.reviewed_execution.phases[1] = egressPhase("reviewer", 2, true);
    const empty = () => null;
    const { default: TeamRunPage } = load("../src/app/workspace/teams/[name]/runs/[run]/page.tsx", {
      "react/jsx-runtime": require("react/jsx-runtime"),
      "@/components/navigation-link": { default: ({ href, children }) => React.createElement("a", { href }, children) },
      "@/components/deliverable-view": { DeliverableBody: ({ output }) => React.createElement("div", {}, output) },
      "@/components/retained-team-documents": { RetainedTeamDocuments: empty },
      "@/components/honest-state": { HonestState: ({ title, detail }) => React.createElement("div", {}, title, detail) },
      "@/components/execution-explorer": { ExecutionExplorer },
      "@/components/live-refresh": { LiveRefresh: empty },
      "@/components/team-run-flow": { TeamRunFlow },
      "@/components/task-checkpoint": { TaskCheckpointPanel: empty },
      "@/app/tasks/[name]/task-approvals-panel": { TaskApprovalsPanel: empty },
      "@/app/workspace/missions/[name]/deploy-timeline": { DeployTimeline: empty },
      "@/app/workspace/missions/[name]/egress-request": { EgressRequest: empty },
      "@/app/workspace/missions/[name]/mission-blockers": { MissionBlockers: empty },
      "@/lib/config": { authWired: () => false, defaultNamespace: () => "kars-system", operatorIdentity: () => "operator" },
      "@/lib/bff": { getTask: async () => task, getTeam: async () => ({ ...heldTeam, name: task.team }), listTaskApprovals: async () => [] },
      "@/lib/team-run-evidence": context.exports, "@/lib/router-activity": router,
      "@/lib/format": { formatEvidenceTime: value => value },
      "@/lib/mission-run-evidence": mission, "@/lib/session": { currentPrincipal: async () => ({ name: "operator" }) },
      "./halt-button": { HaltTeamRunButton: empty }, "./artifact-summary": { ArtifactSummary: empty },
      "../../../read-error": { missingTeamResource: error => { throw error; }, TeamReadError: empty },
    });
    const html = renderToStaticMarkup(await TeamRunPage({
      params: Promise.resolve({ name: task.team, run: task.name }), searchParams: Promise.resolve({ tab: "overview" }),
    }));
    if (phaseState === "failed") {
      assert.match(html, />Failed</);
      assert.match(html, /Required exports missing/);
      assert.doesNotMatch(html, /Retained failed reviewer output/);
      assert.equal(task.reviewed_execution.phases[1].result.output, "Retained failed reviewer output");
      const activityHtml = renderToStaticMarkup(await TeamRunPage({
        params: Promise.resolve({ name: task.team, run: task.name }), searchParams: Promise.resolve({ tab: "activity" }),
      }));
      assert.match(activityHtml, /Harness reported: fetch/);
      assert.match(activityHtml, /reviewer \/ step-2: failed/);
      const researchHtml = renderToStaticMarkup(await TeamRunPage({
        params: Promise.resolve({ name: task.team, run: task.name }), searchParams: Promise.resolve({ tab: "research" }),
      }));
      assert.match(researchHtml, /kubernetes.io:443.*URL not retained/);
      assert.match(researchHtml, /flag: egress_check; decision: allow/);
      assert.match(researchHtml, /upstream HTTP 403.*Not a harness outcome/);
      assert.match(researchHtml, /Timestamp unavailable/);
      assert.doesNotMatch(researchHtml, /href="https?:\/\/kubernetes.io|External source reached/);
      task.reviewed_execution.phases[1].router_activity = { state: "unavailable" };
      const unavailableHtml = renderToStaticMarkup(await TeamRunPage({
        params: Promise.resolve({ name: task.team, run: task.name }), searchParams: Promise.resolve({ tab: "research" }),
      }));
      assert.match(unavailableHtml, /External egress evidence unavailable/);
      assert.match(unavailableHtml, /does not establish that no external traffic occurred/);
    } else {
      assert.match(html, />Held</);
      assert.match(html, /synthesis is prohibited/);
      assert.match(html, phaseState === "uncertain" ? /Outcome uncertain/ : /phase-1 · held/);
    }
    const renderDeliverables = async () => renderToStaticMarkup(await TeamRunPage({
      params: Promise.resolve({ name: task.team, run: task.name }), searchParams: Promise.resolve({ tab: "deliverables" }),
    }));
    const files = await renderDeliverables();
    assert.match(files, /No run-level files have been captured/);
    assert.match(files, /Retained specialist work/);
    assert.match(files, /intermediate contributions, not an approved final delivery/);
    assert.match(files, /href="\/workspace\/missions\/reserved-0\?tab=artifacts"/);
    assert.match(files, /Open builder files/);
    assert.doesNotMatch(files, /Open integrator files/);
    if (phaseState === "failed") assert.match(files, /Open reviewer files/);
    else assert.doesNotMatch(files, /Open reviewer files/);
    const builder = task.reviewed_execution.phases[0];
    const count = builder.result.artifact_count;
    builder.result.artifact_count = null;
    assert.match(await renderDeliverables(), /Recorded file count unavailable/);
    builder.result.artifact_count = 1;
    assert.match(await renderDeliverables(), /1 recorded file</);
    builder.result.artifact_count = count;
    const assignmentNonce = builder.result.assignment_nonce;
    builder.result.assignment_nonce = "stale";
    assert.doesNotMatch(await renderDeliverables(), /Open builder files/);
    builder.result.assignment_nonce = assignmentNonce;
    const runNonce = task.reviewed_execution.run_nonce;
    task.reviewed_execution.run_nonce = "stale";
    assert.doesNotMatch(await renderDeliverables(), /Retained specialist work|Open builder files/);
    task.reviewed_execution.run_nonce = runNonce;
    task.execution_phase = "Idle";
    task.launched = false;
    assert.match(await renderDeliverables(), /Open builder files/);
    assert.match(html, /Phase completed/);
    assert.match(html, /Last valid builder handoff/);
    assert.match(html, /1\/3<\/strong>recorded role completions/);
    assert.match(html, />0<\/strong>captured output files/);
    assert.match(html, /Unavailable<\/strong>recorded tokens/);
    assert.match(html, /phase-2 · pending/);
    assert.doesNotMatch(html, />Running<|>Working<|phase-1 · active|>Principal synthesis</);
    if (phaseState === "failed") {
      const diagnostic = "Native per-round usage verification failed: count mismatch (assistants=18, calls=15, turns=15)";
      const failedPhase = task.reviewed_execution.phases[1];
      const renderOverview = async () => renderToStaticMarkup(await TeamRunPage({
        params: Promise.resolve({ name: task.team, run: task.name }), searchParams: Promise.resolve({ tab: "overview" }),
      }));
      failedPhase.result.error = diagnostic;
      failedPhase.result.output = "Completed the interrupted work without restarting it.";
      failedPhase.failure_diagnostic = "Attempt must not override the committed result";
      const retained = JSON.stringify(failedPhase.result);
      const committedHtml = await renderOverview();
      assert.equal(committedHtml.split(diagnostic).length - 1, 2, "committed failure summary and phase details");
      assert.doesNotMatch(committedHtml, /Completed the interrupted work without restarting it\.|Attempt must not override/);
      assert.match(committedHtml, />Failed</);
      const evidence = context.exports.analyzeTeamRun(heldTeam, task);
      assert.equal(evidence.outcome, "failed");
      const flowHtml = renderToStaticMarkup(React.createElement(TeamRunFlow, {
        task, evidence, phases: context.exports.reviewedPhases(task),
      }));
      assert.ok(flowHtml.includes(diagnostic));
      assert.doesNotMatch(flowHtml, /Completed the interrupted work without restarting it\.|Attempt must not override/);
      assert.equal(JSON.stringify(failedPhase.result), retained, "display changes must retain failed evidence");
      assert.equal(failedPhase.result.reviewable, false);
      failedPhase.result.error = null;
      const missingErrorHtml = await renderOverview();
      assert.match(missingErrorHtml, /No failure message was recorded for this revision/);
      assert.doesNotMatch(missingErrorHtml, /Completed the interrupted work without restarting it\.|Attempt must not override/);
      failedPhase.result = null;
      failedPhase.failure_diagnostic = diagnostic;
      const diagnosticHtml = await renderOverview();
      assert.equal(diagnosticHtml.split(diagnostic).length - 1, 2, "failure summary and expandable phase details");
      assert.match(diagnosticHtml, />Failed</);
      assert.doesNotMatch(diagnosticHtml, /No failure message was recorded|Retained failed reviewer output/);
      assert.doesNotMatch(await renderDeliverables(), /Open reviewer files/);
      delete failedPhase.failure_diagnostic;
      assert.match(await renderOverview(), /No failure message was recorded for this revision/);
    }
  }
});

test("stale root, replaced phase, missing terminal and incomplete multi-phase role cannot claim delivery", () => {
  for (const mutate of [
    (task) => { task.reviewed_execution.run_nonce = "rev-1"; },
    (task) => { task.reviewed_execution.issue = "Reservations changed"; },
    (task) => { task.reviewed_execution.phases[0].task_uid = "replacement"; },
    (task) => { task.reviewed_execution.phases[0].result.assignment_nonce = "stale"; },
    (task) => { task.reviewed_execution.phases[0].result = null; },
    (task) => { task.reviewed_execution.phases.push({
      ...task.reviewed_execution.phases[0], phase: "second-phase", state: "pending", result: null,
    }); },
  ]) {
    const task = reviewedTask();
    mutate(task);
    const evidence = context.exports.analyzeTeamRun({
      paused: false, roster: ["coordinator", "checklist-writer", "reviewer"].map((name) => ({ name })),
    }, task);
    assert.equal(evidence.outcome, "incomplete");
  }
});
const flowHtml = (task) => {
  const { routerActivity, currentResult } = mission.missionRunState(task);
  return renderToStaticMarkup(React.createElement(TeamRunFlow, {
    task, evidence: context.exports.analyzeTeamRun({ paused: false, roster: [] }, task),
    routerActivity: routerActivity && currentResult?.run_evidence
      ? { observations: routerActivity, producer: currentResult.run_evidence, provisional: false } : null,
    phases: context.exports.reviewedPhases(task) ?? [],
  }));
};

function timestampedObservations() {
  const observations = structuredClone(routerFixture);
  observations.trace.events.forEach((event, index) => {
    event.observed_at = `2026-10-08T18:04:0${index}.000Z`;
    if (event.kind === "round") {
      event.usage_state = "present";
      event.usage = { prompt_tokens: 7, completion_tokens: 3, total_tokens: 10 };
    }
    if (event.kind === "tool_proposed") event.argument_preview = {
      text: '{"url":"https://example.com/team"}', truncated: false, redacted: false,
    };
  });
  return observations;
}

test("lower Team flow shares router ordering, arguments and observed cost without asserting execution", () => {
  const root = observedPhase("principal", 0);
  root.router_activity = timestampedObservations();
  root.result.usage_known = false;
  const task = metricsTask({
    current_run_nonce: root.run_nonce, result: root.result, router_activity: root.router_activity,
    activity: [toolEvent({ runNonce: root.run_nonce, name: "duplicate_legacy_tool" })],
  });
  const html = flowHtml(task);
  for (const text of ["2 observed router rounds", "20 observed tokens", "1 observed reported tool results",
    "https://example.com/team", "Proposal only; execution not established", "outcome failure",
    "Source: model-proposed", "Assignment: assignment-0", "Producer: did:mesh:principal",
    "Coverage gaps", "returned records only, not complete run totals"]) assert.ok(html.includes(text), text);
  assert.doesNotMatch(html, /Round count unavailable|Token usage unavailable|20 recorded tokens|duplicate_legacy_tool/);
  const list = html.slice(html.indexOf("<ol"));
  assert.ok(list.indexOf("Router response · round 2") < list.indexOf("Harness reported: fetch"));
  assert.ok(list.indexOf("Harness reported: fetch") < list.indexOf("Model proposed: fetch"));
  assert.ok(list.indexOf("Model proposed: fetch") < list.indexOf("Router response · round 1"));
  assert.doesNotMatch(flowHtml({ ...task, current_run_nonce: "replacement" }), /20 observed tokens|Model proposed: fetch/);
});

test("Team flow contains long payloads in collapsed accessible evidence without hiding outcomes", () => {
  const root = observedPhase("principal", 0);
  root.router_activity = timestampedObservations();
  const payload = `payload-${"x".repeat(8000)}-retained-end`;
  root.router_activity.trace.events.find(event => event.kind === "tool_proposed").argument_preview = {
    text: JSON.stringify({ content: payload }), truncated: true, redacted: true,
  };
  const html = flowHtml(metricsTask({ current_run_nonce: root.run_nonce,
    result: root.result, router_activity: root.router_activity }));
  const disclosures = [...html.matchAll(/<details\b([^>]*)>([\s\S]*?)<\/details>/g)];
  assert.ok(disclosures.length > 0);
  assert.ok(disclosures.every(([, attributes]) => !/\bopen\b/.test(attributes)));
  assert.ok(disclosures.some(([, , content]) => content.includes(payload)));
  assert.match(html, /<summary[^>]*>Arguments and provenance<\/summary>/);
  const visible = html.replace(/<details\b[^>]*>[\s\S]*?<\/details>/g, "");
  assert.doesNotMatch(visible, /payload-xxx/);
  assert.match(visible, /Proposal only; execution not established/);
  assert.match(visible, /outcome failure/);
  assert.match(html, /Model argument preview · redacted · truncated \(not execution proof\)/);
  assert.match(html, /\[overflow-wrap:anywhere\]/);
  assert.match(html, /sm:grid-cols-\[6\.25rem_minmax\(0,1fr\)\]/);
  assert.match(html, /aria-label="Search execution flow"/);
});

test("observed usage remains unknown for missing, partial or invalid counts and retains measured zero", () => {
  for (const [state, total] of [["missing", null], ["partial", 10], ["present", -1],
    ["present", Number.MAX_SAFE_INTEGER]]) {
    const root = observedPhase("principal", 0);
    root.router_activity = timestampedObservations();
    Object.assign(root.router_activity.trace.events[0], { usage_state: state,
      usage: { prompt_tokens: null, completion_tokens: null, total_tokens: total } });
    const html = flowHtml(metricsTask({ current_run_nonce: root.run_nonce,
      result: root.result, router_activity: root.router_activity }));
    assert.match(html, /Observed token usage unavailable/);
    assert.doesNotMatch(html, /0 observed tokens/);
  }
  const root = observedPhase("principal", 0);
  root.router_activity = timestampedObservations();
  root.router_activity.trace.events.filter(event => event.kind === "round")
    .forEach(event => { event.usage.total_tokens = 0; });
  assert.match(flowHtml(metricsTask({ current_run_nonce: root.run_nonce,
    result: root.result, router_activity: root.router_activity })), /0 observed tokens/);
});

test("Team flow receives the same live root and phase observations as the shared explorer and clears disconnects", () => {
  const root = activePhase("principal", 0);
  const phase = activePhase("writer", 1);
  root.live_router_activity.observations = timestampedObservations();
  phase.live_router_activity.observations = timestampedObservations();
  assert.ok(router.liveRouterActivity(root.live_router_activity, root.run_nonce));
  assert.ok(router.liveRouterActivity(phase.live_router_activity, phase.run_nonce));
  const task = metricsTask({ current_run_nonce: root.run_nonce,
    reviewed_execution: { run_nonce: root.run_nonce, issue: null, phases: [{ ...phase, live_router_activity: null }] } });
  const props = {
    running: true, runNonce: root.run_nonce, activity: [], telemetry: null, assignmentEvents: [], approvals: [],
    ns: "kars-system", name: "root", agentLabel: "Principal", phases: task.reviewed_execution.phases,
    teamFlow: { task, evidence: context.exports.analyzeTeamRun({ paused: false, roster: [] }, task) },
  };
  const lowerFlow = extra => {
    const html = renderToStaticMarkup(React.createElement(ExecutionExplorer, { ...props, ...extra }));
    return html.slice(html.indexOf(">Execution flow</h2>"));
  };
  streamedRoot = root.live_router_activity;
  streamedPhases = { [router.reviewedPhaseKey(phase)]: phase.live_router_activity };
  try {
    const html = lowerFlow();
    for (const text of ["4 observed router rounds", "40 observed tokens", "Live provisional observations",
      "Producer: did:mesh:principal", "Producer: did:mesh:writer", "https://example.com/team"]) {
      assert.ok(html.includes(text), text);
    }
    assert.doesNotMatch(html, /Outcome delivered|Run completed with issues|40 recorded tokens/);
    const differentRun = renderToStaticMarkup(React.createElement(ExecutionExplorer, { ...props, runNonce: "replacement" }));
    assert.doesNotMatch(differentRun, />Execution flow<\/h2>/);
    streamedRoot = null;
    streamedPhases = {};
    assert.doesNotMatch(lowerFlow(), /observed tokens|Model proposed: fetch|Live provisional observations/);
    assert.match(lowerFlow(), /Token usage unavailable/);
    const stopped = lowerFlow({ running: false, observing: false,
      stoppedActivity: { ...root.live_router_activity, runtime: "OpenClaw" }, phases: [] });
    assert.match(stopped, /Stopped partial observations/);
    assert.doesNotMatch(stopped, /Live provisional observations|Outcome delivered/);
  } finally {
    streamedRoot = null;
    streamedPhases = {};
  }
});

test("both Team entry points use the shared stream rather than mounting a snapshot-only lower flow", () => {
  for (const file of ["../src/app/workspace/teams/[name]/page.tsx", "../src/app/workspace/teams/[name]/runs/[run]/page.tsx"]) {
    const source = readFileSync(new URL(file, import.meta.url), "utf8");
    assert.match(source, /<ExecutionExplorer\s+teamFlow=/);
    assert.doesNotMatch(source, /<TeamRunFlow|import \{ TeamRunFlow \}/);
  }
});

test("legacy Team tools retain different arguments and newest timestamp first", () => {
  const html = flowHtml(metricsTask({ activity: [
    toolEvent({ args_preview: "first.md", ts: "2026-10-08T16:23:32Z" }),
    toolEvent({ args_preview: "second.md", ts: "2026-10-08T16:23:33Z" }),
  ] }));
  assert.ok(html.includes("first.md"));
  assert.ok(html.indexOf("second.md") < html.indexOf("first.md"));
});

test("rendered Team flow does not turn absent evidence into zero usage or verified work", () => {
  const html = flowHtml(metricsTask());
  for (const text of [
    "Role selection unavailable", "Token usage unavailable", "Tool detail unavailable",
    "Round count unavailable", "Activity evidence is not yet available",
  ]) assert.ok(html.includes(text), text);
  assert.doesNotMatch(html, /0 tokens|0 tool calls|all returned|>verified</);
  assert.ok(flowHtml(metricsTask({ result: terminal({ total_tokens: 0 }) })).includes("0 recorded tokens"));
});

test("rendered Team chronology excludes previous-revision activity and outcomes", () => {
  const html = flowHtml(metricsTask({
    result: terminal({ assignment_nonce: "rev-1" }),
    activity: [toolEvent({ runNonce: "rev-1", name: "stale_tool" })],
    assignment_events: [{
      task_id: "rev-1", sequence: 1, event_type: "completed", state: "Completed",
      message: "stale handback", at: "2026-10-07T10:00:00Z",
    }],
  }));
  assert.doesNotMatch(html, /stale_tool|stale handback|final-outcome|Run completed with issues/);
  assert.ok(html.includes("Token usage unavailable"));
});

function analyze(artifacts, overrides = {}, names = ["qa"]) {
  return context.exports.analyzeTeamRun({
    paused: false,
    roster: names.map(name => ({ name, system_prompt: "" })),
  }, {
    artifacts,
    activity: [],
    result: null,
    launched: false,
    execution_phase: null,
    assignment: null,
    assignment_events: [],
    current_run_nonce: null,
    role_plan: { selected_roles: [], skipped_roles: [] },
    collaboration_events: [],
    ...overrides,
  });
}

const artifact = (source_agent, name = "qa-report.md") => ({
  name,
  source_agent,
  source_path: `/sandbox/${name}`,
  content: null,
  content_truncated: false,
});

test("current failed and rejected terminals remain failures without an assignment ledger", () => {
  for (const status of ["failed", "rejected", "error"]) {
    const task = {
      current_run_nonce: "rev-2", launched: false,
      result: terminal({ status, reviewable: false, output: "", usage_known: false }),
    };
    assert.equal(analyze([], task).outcome, "failed", status);
    assert.notEqual(analyze([], { ...task, current_run_nonce: "rev-3" }).outcome, "failed", status);
  }
});

test("explicit principal producer cannot become recorded QA through name or path", () => {
  const input = artifact("principal");
  const evidence = analyze([input]);
  assert.equal(evidence.roles[0].artifactAttribution, "none");
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(evidence.unattributedArtifacts[0], input);
  assert.equal(evidence.roles[0].state, "missing");
  assert.equal(evidence.outcome, "incomplete");
  const withPrincipal = analyze([input], {}, ["qa", "principal"]);
  assert.equal(withPrincipal.roles[0].artifacts.length, 0);
  assert.equal(withPrincipal.roles[1].artifactAttribution, "recorded");
});

test("matching explicit producer is recorded even if its filename names another role", () => {
  const evidence = analyze([artifact("qa", "developer-report.md")], {}, ["developer", "qa"]);
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(evidence.roles[1].artifactAttribution, "recorded");
});

test("filename and path without producer metadata are at most inferred", () => {
  for (const producer of [undefined, null, "", "   "]) {
    const evidence = analyze([artifact(producer)]);
    assert.equal(evidence.roles[0].artifactAttribution, "inferred");
    assert.equal(evidence.roles[0].state, "missing");
    assert.equal(evidence.outcome, "incomplete");
    assert.equal(evidence.collaboration[0].outcome, null);
    assert.match(evidence.collaboration[0].preview, /inferred/);
  }
  const unknown = analyze([artifact(null, "notes.md")]);
  assert.equal(unknown.roles[0].artifactAttribution, "none");
  assert.equal(unknown.unattributedArtifacts.length, 1);
});

test("unknown explicit producer stays unattributed; substrings are not role identities", () => {
  for (const producer of ["qa-supervisor", "principal-qa", "unknown"]) {
    assert.equal(analyze([artifact(producer)]).unattributedArtifacts.length, 1);
  }
});

test("punctuation-normalized role names are explicit matches, not filename proof", () => {
  const evidence = analyze(
    [artifact("Dependency_Security", "qa-report.md")], {}, ["qa", "dependency-security"],
  );
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(evidence.roles[1].artifactAttribution, "recorded");
});

test("mixed role artifacts retain per-file attribution for honest rendering", () => {
  const explicit = artifact("qa");
  const inferred = artifact(null, "qa-notes.md");
  const evidence = analyze([explicit, inferred]);
  assert.equal(evidence.roles[0].artifactAttribution, "inferred");
  assert.equal(context.exports.artifactRoleAttribution(evidence.roles[0].role, explicit), "recorded");
  assert.equal(context.exports.artifactRoleAttribution(evidence.roles[0].role, inferred), "inferred");
});

test("producer evidence does not supersede assignment nonce or partial persistence", () => {
  const input = artifact("qa");
  const stale = analyze([input], {
    launched: true,
    execution_phase: "Running",
    current_run_nonce: "new-run-assignment",
    assignment: { task_id: "old-run-assignment", state: "Completed" },
    result: { assignment_nonce: "old-run-assignment", status: "success" },
    assignment_events: [{
      task_id: "old-run-assignment", stage: "child_handback", child_role: "qa",
      state: "Completed", outcome: "success",
    }],
  });
  assert.equal(stale.outcome, "running");
  assert.equal(stale.roles[0].state, "missing");
  const partial = analyze([input], {
    current_run_nonce: "slot-specific-run-nonce",
    assignment: { task_id: "slot-specific-run-nonce", state: "Completed" },
    assignment_events: [{
      task_id: "slot-specific-run-nonce", stage: "child_handback", child_role: "qa",
      state: "Completed", outcome: "success",
    }],
    result: {
      assignment_nonce: "slot-specific-run-nonce", status: "success",
      artifact_persistence: "partial", artifact_count: 1, declared_artifact_count: 2,
    },
  });
  assert.equal(partial.outcome, "delivered_with_issues");
  assert.equal(partial.roles[0].state, "delivered");
});

test("role artifact renderer labels each file and explicit unassigned producers honestly", () => {
  const page = readFileSync(new URL(
    "../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url,
  ), "utf8");
  assert.match(page, /import \{ ArtifactSummary \} from "\.\/artifact-summary";/);
  assert.match(page, /<ArtifactSummary artifact=\{a\} role=\{r\.role\} \/>/);
  assert.match(page, /<ArtifactSummary artifact=\{a\} \/>/);

  const summary = readFileSync(new URL(
    "../src/app/workspace/teams/[name]/runs/[run]/artifact-summary.tsx", import.meta.url,
  ), "utf8");
  const renderer = {
    exports: {},
    require: (name) => {
      assert.equal(name, "@/lib/team-run-evidence");
      return context.exports;
    },
    element: (tag, props, ...children) => ({ tag, props, children }),
  };
  runInNewContext(ts.transpileModule(summary, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.React,
      jsxFactory: "element",
    },
  }).outputText, renderer);
  const cases = [
    [artifact("qa"), { name: "qa" }, "from qa · recorded producer"],
    [artifact(null), { name: "qa" }, "possibly qa · inferred attribution"],
    [artifact("Dependency_Security"), { name: "dependency-security" }, "from dependency security · recorded producer"],
    [artifact("principal"), undefined, "recorded producer: principal · not attributed to a roster role"],
    [artifact(null, "notes.md"), undefined, "captured for this run · individual producer not recorded"],
    [artifact("   ", "notes.md"), undefined, "captured for this run · individual producer not recorded"],
  ];
  for (const [input, role, label] of cases) {
    const tree = renderer.exports.ArtifactSummary({ artifact: input, role });
    assert.deepEqual(JSON.parse(JSON.stringify(tree)), {
      tag: "summary",
      props: { className: "cursor-pointer px-4 py-3" },
      children: [
        { tag: "span", props: { className: "font-medium" }, children: [input.name] },
        { tag: "span", props: { className: "ml-2 text-xs text-foreground-muted" }, children: [label] },
      ],
    });
  }
});
