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
    const dependency = [`${base}.ts`, `${base}.tsx`].find(existsSync);
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
