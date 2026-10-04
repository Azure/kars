// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const sourceRoot = fileURLToPath(new URL("../src/", import.meta.url));
const cache = new Map();
const traceCalls = [];
const forbiddenMutation = () => assert.fail("Read-only rendering must not mutate a mission");

// Render real components with React; only browser streaming and server actions
// are replaced. The signed-in browser test separately verifies real hydration.
function load(file) {
  const path = resolve(sourceRoot, file);
  if (cache.has(path)) return cache.get(path);
  const exports = {};
  cache.set(path, exports);
  const localRequire = (name) => {
    if (name.endsWith("/use-live-trace")) return {
      useLiveTrace: (...args) => {
        traceCalls.push(args);
        return args[3];
      },
    };
    if (name === "@/components/live-refresh") return { LivePulse: ({ label }) => React.createElement("span", null, label) };
    if (name === "next/navigation") return { useRouter: () => ({ refresh: forbiddenMutation }) };
    if (name === "./review-actions") return { submitReview: forbiddenMutation };
    if (name === "@/app/tasks/[name]/launch-actions") return { setLaunch: forbiddenMutation };
    if (!name.startsWith(".") && !name.startsWith("@/")) return require(name);
    const base = name.startsWith("@/") ? resolve(sourceRoot, name.slice(2)) : resolve(dirname(path), name);
    const dependency = [`${base}.ts`, `${base}.tsx`, `${base}/index.ts`, `${base}/index.tsx`].find(existsSync);
    assert.ok(dependency, `Missing source dependency: ${name}`);
    return load(dependency);
  };
  runInNewContext(ts.transpileModule(readFileSync(path, "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText, { exports, require: localRequire, URLSearchParams, Date });
  return exports;
}

const { formatEvidenceTime } = load("lib/format.ts");
const { ExecutionExplorer } = load("components/execution-explorer.tsx");
const { ExecutionLifetime } = load("components/execution-lifetime.tsx");
const { ReviewPanel } = load("app/workspace/missions/[name]/review-panel.tsx");
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));
const base = {
  running: false, activity: [], telemetry: { rounds: 2, tool_calls: null },
  assignmentEvents: [], approvals: [], ns: "kars-system", name: "briefing",
  runNonce: "rev-2", agentLabel: "Briefing agent", agentPhase: "succeeded",
};
const lifecycle = {
  event_id: "assigned", at: "2026-10-03T06:47:40Z", event_type: "assignment_accepted",
  state: "Running", stage: "executing", child_role: "principal", worker_did: "did:mesh:producer",
  message: "Accepted this revision", outcome: null,
};
const approval = {
  name: "release", phase: "Denied", requested_at: "2026-10-03T06:47:41Z",
  decided_at: "2026-10-03T06:47:42Z", summary: "Release denied", detail: "Needs review", decider: "Reviewer",
};
const round = {
  kind: "round", round: 0, seq: 1, ts: "2026-10-03T06:47:43Z", runNonce: "rev-2",
  ms: 300, prompt_tokens: 3211, completion_tokens: 471, total_tokens: 3682,
  tool_calls: 0, finish_reason: "stop", agent_name: "briefing",
};

test("evidence dates and review history render identically across timezones", () => {
  const previous = process.env.TZ;
  const rendered = [];
  try {
    for (const timezone of ["UTC", "Europe/Budapest", "America/Los_Angeles"]) {
      process.env.TZ = timezone;
      assert.equal(formatEvidenceTime("2026-10-03T06:47:38.353Z"), "2026-10-03 06:47:38 UTC");
      assert.equal(formatEvidenceTime("2026-10-03T06:47:38.353035009+00:00"), "2026-10-03 06:47:38 UTC");
      assert.equal(formatEvidenceTime("2026-10-03T08:47:38+02:00"), "2026-10-03 06:47:38 UTC");
      rendered.push(render(ReviewPanel, {
        task: "briefing", assignmentNonce: "rev-2", kind: "docs", eligible: true,
        initial: { status: "none", revision: 2, history: [{ revision: 1, decision: "request_changes", decided_at: "2026-10-03T06:47:38Z", reviewer: "Reviewer" }] },
      }));
    }
    assert.equal(new Set(rendered).size, 1);
    assert.match(rendered[0], /2026-10-03 06:47:38 UTC/);
  } finally {
    if (previous === undefined) delete process.env.TZ;
    else process.env.TZ = previous;
  }
});

test("missing, malformed and timezone-less dates stay unavailable", () => {
  for (const value of [null, undefined, "", "badZ", "2026-10-03T06:47:38", "2026-10-03", "2026-13-03T06:47:38Z"]) {
    assert.equal(formatEvidenceTime(value), "Time unavailable");
  }
});

test("absent detailed trace shows recorded totals without invented graph or zero-work claims", () => {
  for (const running of [false, true]) {
    traceCalls.length = 0;
    const html = render(ExecutionExplorer, { ...base, running });
    assert.match(html, /Detailed trace unavailable/);
    assert.match(html, /2 model rounds/);
    assert.doesNotMatch(html, /Launch the execution|Awaiting first action|Complete execution lifetime|0\/0|0 tool calls|Drill-down/);
    assert.equal(traceCalls[0][2], running);
    assert.equal(traceCalls[0][4], "rev-2");
    assert.ok(traceCalls.slice(1).every((args) => args[2] === false), "Child must not open a duplicate live stream");
  }
  assert.doesNotMatch(render(ExecutionExplorer, { ...base, telemetry: null }), /Recorded run totals:/);
});

test("missing trace retains genuine assignment and timestamped approval records", () => {
  const html = render(ExecutionExplorer, { ...base, assignmentEvents: [lifecycle], approvals: [approval] });
  assert.match(html, /Detailed trace unavailable/);
  assert.match(html, /Recorded execution timeline/);
  assert.match(html, /Accepted this revision/);
  assert.match(html, /Release denied/);
  assert.doesNotMatch(html, /Complete execution lifetime|Launch the execution/);
});

test("partial round records retain graph and timeline without claiming no work", () => {
  const html = render(ExecutionExplorer, { ...base, activity: [round] });
  assert.match(html, /Drill-down/);
  assert.match(html, /Model round 1/);
  assert.match(html, /3,682 tokens/);
  const { AgentGraph } = load("components/agent-graph.tsx");
  assert.match(render(AgentGraph, base), /No tool actions are retained here/);
  assert.doesNotMatch(html, /Detailed trace unavailable|Launch the execution|completed without retained tool actions/);
});

test("timeline distinguishes unavailable records from an unmatched search", () => {
  const empty = render(ExecutionLifetime, base);
  assert.match(empty, /No timeline records are available here/);
  assert.doesNotMatch(empty, /No retained event matches|0\/0/);
  const filtered = render(ExecutionLifetime, { ...base, assignmentEvents: [lifecycle], focusQuery: "not-present" });
  assert.match(filtered, /No retained event matches this search/);
  assert.doesNotMatch(filtered, /No timeline records are available/);
});

test("rendered approval controls fail closed for unavailable, failed and pending revisions", () => {
  const props = { task: "briefing", assignmentNonce: "rev-2", kind: "docs", initial: null };
  for (const extra of [{}, { eligible: false }, { eligible: true, assignmentNonce: "" }]) {
    assert.match(render(ReviewPanel, { ...props, ...extra }), /<button[^>]*disabled=""[^>]*>Approve deliverable<\/button>/);
  }
  assert.match(render(ReviewPanel, { ...props, eligible: true }), /<button(?![^>]*disabled="")[^>]*>Approve deliverable<\/button>/);
  const pending = render(ReviewPanel, { ...props, eligible: true, initial: { redrive_pending: true } });
  assert.doesNotMatch(pending, />Approve deliverable<\/button>/);
  assert.match(pending, /Revision requested/);
});

const { RouterActivityPanel } = load("components/router-activity-panel.tsx");
const { missionRunState, reviewKind } = load("lib/mission-run-evidence.ts");
const { ROUTER_OBSERVATIONS_ARTIFACT: systemFile } = load("lib/router-activity.ts");
const observed = {
  version: 1, source: "runtime-forwarded-router-observations", coverage: "returned-responses-only",
  responses: 3, scope_id: "scope-a", rounds: [1, 2], state: "observed",
  trace: {
    scope_id: "scope-a", rounds: [1, 2], coverage: "returned-responses-only", durable: false,
    missing_rounds: [2], dropped_events: 4, truncated: true,
    events: [{
      kind: "round", round: 1, seq: 1, scope_id: "scope-a", source: "router-upstream",
      provider: "foundry", model: "gpt-5.4-mini", http_status: 200, accepted: true, outcome: "complete",
      usage: { prompt_tokens: 0, completion_tokens: null, total_tokens: null }, usage_state: "partial",
      finish_reason: "stop", partial_observation: true, ms: 23, tool_calls_observed: 1,
    }],
  },
};
const routerProps = { activity: observed, producer: "Briefing writer", ns: "kars-system", name: "briefing", runNonce: "rev-2" };

test("router observations name the producer and revision without inventing missing usage", () => {
  const html = render(RouterActivityPanel, routerProps);
  for (const text of ["Returned by Briefing writer", "Run: rev-2", "Router round 1", "gpt-5.4-mini", "Provider: foundry", "HTTP: 200",
    "input: 0", "output: Unavailable", "total: Unavailable", "usage: partial", "Partial observation",
    "Responses received: 3", "Correlated router rounds: 2", "missing rounds: 2", "Recorder-wide dropped events: 4", "Selection truncated: yes",
    "not independent proof of tool execution", "Hidden retries and responses without IDs are not covered"]) {
    assert.ok(html.includes(text), text);
  }
  assert.match(html, /href="\/api\/namespaces\/kars-system\/tasks\/briefing\/artifact\/kars-router-observations.json\?run_nonce=rev-2"/);
  const unpinned = render(RouterActivityPanel, { ...routerProps, runNonce: "" });
  assert.doesNotMatch(unpinned, /href=/);
  assert.match(unpinned, /aria-disabled="true"/);
});

test("router proposals and harness outcomes remain distinct and nullable", () => {
  const proposal = { kind: "tool_proposed", scope_id: "scope-a", round: 1, seq: 2, source: "model-proposed", call_id: "call-a", name: "file_write", ok: null };
  const html = render(RouterActivityPanel, { ...routerProps, activity: { ...observed, trace: { ...observed.trace, events: [proposal,
    ...[true, false, null].map((ok, index) => ({ ...proposal, kind: "tool_result", seq: index + 3, source: "harness-reported", ok, reported_in_round: 2 })),
  ] } } });
  for (const text of ["Model proposed: file_write", "Proposal only; execution not established", "Harness reported: file_write",
    "Proposed in router round 1", "Reported in router round 2", "Reported outcome: Success", "Reported outcome: Failure", "Reported outcome: Unavailable"]) {
    assert.ok(html.includes(text), text);
  }
});

test("empty and unavailable router selections do not claim zero work", () => {
  const empty = render(RouterActivityPanel, { ...routerProps, activity: { ...observed, trace: { ...observed.trace, events: [] } } });
  assert.match(empty, /No selected records remain available. This does not establish that no work occurred/);
  const unavailable = render(RouterActivityPanel, { ...routerProps, activity: {
    ...observed, state: "unavailable", reason: "Response correlation changed or was invalid",
  } });
  assert.match(unavailable, /Router observations unavailable: Response correlation changed or was invalid/);
  assert.doesNotMatch(unavailable, /Router round 1|Coverage gaps|0 tool calls/);
});

test("system evidence never changes the useful deliverable review category", () => {
  const artifacts = (...names) => names.map(name => ({ name }));
  for (const [names, expected] of [
    [["briefing.md", systemFile], "docs"], [[systemFile], "output"], [[], "output"],
    [["real.json", systemFile], "code"], [["data.csv", "briefing.md", systemFile], "data"],
    [["main.rs", "data.csv", systemFile], "code"], [["report.MD", systemFile], "docs"],
  ]) assert.equal(reviewKind(artifacts(...names)), expected);
  assert.equal(reviewKind(undefined), "output");
});

test("router projection is shown only for the current successful durable terminal", () => {
  const result = {
    assignment_nonce: "rev-2", status: "ok", source: "durable_agent", reviewable: true, output: "Attached briefing.md",
    blocked: null, run_evidence: { status: "succeeded", run_nonce: "rev-2" },
  };
  const task = { current_run_nonce: "rev-2", result, assignment: null, router_activity: observed };
  assert.equal(missionRunState(task).routerActivity, observed);
  assert.equal(missionRunState(task).realDelivery, true);
  for (const changed of [
    { current_run_nonce: "rev-3" }, { current_run_nonce: null }, { result: null },
    ...[{ assignment_nonce: "rev-1" }, { status: "failed" }, { source: "single_turn" }, { run_evidence: null },
      { run_evidence: { status: "failed", run_nonce: "rev-2" } },
      { run_evidence: { status: "succeeded", run_nonce: "rev-1" } }].map(patch => ({ result: { ...result, ...patch } })),
    { result: { ...result, status: null }, assignment: { task_id: "rev-2", state: "Running", completed_at: null } },
    { router_activity: null }, { router_activity: undefined },
  ]) assert.equal(missionRunState({ ...task, ...changed }).routerActivity, null, JSON.stringify(changed));
});

test("presented router observations suppress only the empty legacy trace and retain timelines and streaming pins", () => {
  traceCalls.length = 0;
  const html = render(ExecutionExplorer, { ...base, routerActivityPresented: true, assignmentEvents: [lifecycle], approvals: [approval] });
  assert.doesNotMatch(html, /Detailed trace unavailable/);
  assert.match(html, /Accepted this revision/);
  assert.match(html, /Release denied/);
  assert.equal(traceCalls[0][4], "rev-2");
  assert.equal(traceCalls[0][2], false);
  assert.match(render(ExecutionExplorer, { ...base, routerActivityPresented: true, activity: [round] }), /Drill-down/);
});

test("artifact list separates downloadable system evidence from output files and preserves legacy absence", () => {
  const { ArtifactsPanel } = load("app/workspace/missions/[name]/mission-detail-panels.tsx");
  const props = {
    ns: "kars-system", task: "briefing", runNonce: "rev-2", pullRequests: [], activity: [], egress: [], tokens: 22,
    artifacts: [{ name: "briefing.md", content: "# Useful briefing", size_bytes: 17 }, { name: systemFile, content: "{}", size_bytes: 2 }],
  };
  const html = render(ArtifactsPanel, { ...props, routerActivityState: "observed" });
  assert.match(html, /1 output file · 1 system evidence file/);
  assert.match(html, /System evidence · not a deliverable/);
  assert.match(html, /Partial router observations are shown in Activity/);
  assert.match(html, /<h1[^>]*>Useful briefing<\/h1>/);
  assert.match(html, /kars-router-observations.json\?run_nonce=rev-2/);
  assert.doesNotMatch(html, /Detailed revision-bound trace unavailable/);
  assert.match(render(ArtifactsPanel, { ...props, routerActivityState: "unavailable" }), /unavailable trace; see Activity for the reason/);
  assert.match(render(ArtifactsPanel, props), /Detailed revision-bound trace unavailable/);
  const unpinned = render(ArtifactsPanel, { ...props, runNonce: null });
  assert.match(unpinned, /Downloads unavailable: no current revision/);
  assert.doesNotMatch(unpinned, /href=/);
});
