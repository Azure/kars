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
    exports: {}, URL, URLSearchParams,
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
const { TeamRunFlow } = load("../src/components/team-run-flow.tsx", {
  react: React, "react/jsx-runtime": require("react/jsx-runtime"),
  "next/link": { default: ({ href, children }) => React.createElement("a", { href }, children) },
  "@/lib/team-run-evidence": context.exports,
});

const graphActivity = load("../src/components/agent-graph/activity.ts", { "@/lib/router-activity": router });
const { buildAgentExecutions } = load("../src/components/agent-graph/execution.ts", {
  "./activity": graphActivity, "@/lib/router-activity": router,
  "@/lib/team-run-evidence": context.exports,
});
const { ExecutionLifetime } = load("../src/components/execution-lifetime.tsx", {
  react: React, "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/icon": { Icon: () => null },
  "@/components/live-refresh": { LivePulse: () => null },
  "@/components/agent-graph/activity": graphActivity,
  "@/lib/team-run-evidence": context.exports,
});
const { RouterActivityPanel } = load("../src/components/router-activity-panel.tsx", {
  "react/jsx-runtime": require("react/jsx-runtime"),
  "@/lib/mission-run-evidence": mission, "@/lib/router-activity": router,
});
const { ExecutionExplorer } = load("../src/components/execution-explorer.tsx", {
  react: React, "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/activity-stream": { ActivityStream: () => React.createElement("p", {}, "Legacy activity") },
  "@/components/agent-graph": { AgentGraph: () => null },
  "@/components/execution-lifetime": { ExecutionLifetime },
  "@/components/use-live-trace": { useLiveTrace: (_ns, _name, _running, events) => ({ events, routerActivity: null }) },
  "@/components/router-activity-panel": { RouterActivityPanel },
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
  assert.match(page, /<ExecutionExplorer\s+routerActivity=/);
  assert.match(page, /phases=\{metrics\.phases \?\? \[\]\}/);
});

test("graph keeps reviewed phases alongside the recorded principal with exact producer focus", () => {
  const root = observedPhase("principal", 0);
  const phases = ["coordinator", "writer", "reviewer"].map((role, index) => observedPhase(role, index + 1));
  const agents = buildAgentExecutions({
    agentLabel: "Team", agentModel: null, agentPhase: "succeeded", agentRuntime: null,
    events: [], name: root.task_name, running: false, subAgents: [],
    routerActivity: context.exports.reviewedPhaseRouterActivity(root), phases,
  });
  assert.equal(agents.length, 4);
  for (const [index, phase] of phases.entries()) {
    const agent = agents[index + 1];
    const binding = context.exports.reviewedPhaseRouterActivity(phase);
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

test("phase observations refuse stale UID, nonce, failure and unbound producer without borrowing principal records", () => {
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
const flowHtml = (task) => renderToStaticMarkup(React.createElement(TeamRunFlow, {
  task, evidence: context.exports.analyzeTeamRun({ paused: false, roster: [] }, task),
}));

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
