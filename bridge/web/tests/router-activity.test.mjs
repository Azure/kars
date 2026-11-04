// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
function load(path, imports = {}, globals = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { ...globals, exports, URLSearchParams, TextEncoder, require: name => {
    if (Object.hasOwn(imports, name)) return imports[name];
    throw new Error(`Unexpected import: ${name}`);
  } });
  return exports;
}
const router = load("../src/lib/router-activity.ts");
const graph = load("../src/components/agent-graph/activity.ts", { "@/lib/router-activity": router });
const mission = load("../src/lib/mission-run-evidence.ts", { "./router-activity": router });
const { RouterActivityPanel } = load("../src/components/router-activity-panel.tsx", {
  "react/jsx-runtime": require("react/jsx-runtime"), "@/lib/router-activity": router, "@/lib/mission-run-evidence": mission,
});
const returned = JSON.parse(readFileSync(new URL("../../../inference-router/tests/fixtures/router-observations-v1.json", import.meta.url), "utf8"));
const producer = { task_uid: "task-uid", run_nonce: "run-1", assignment_id: "assignment-1", agent_name: "worker",
  agent_did: "did:mesh:producer", dispatcher_did: "did:mesh:dispatcher", sandbox_uid: "sandbox-uid", pod_uid: "pod-uid", runtime_boot_id: "boot-1" };
const unavailableReason = "Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)";
function native() {
  const value = structuredClone(returned);
  delete value.responses;
  value.coverage = value.trace.coverage = "native-tool-correlated-only";
  value.correlated_tools = 1;
  value.rounds = [1]; value.trace.rounds = [1];
  value.trace.events.pop();
  value.trace.events[0].tool_calls_observed = 3;
  for (const event of value.trace.events.slice(1)) event.name = "kars_mission_file_write";
  const result = value.trace.events[2];
  delete result.reported_in_round;
  result.ok = true;
  result.seq = 5;
  result.result_summary = { kind: "file_write", bytes: 17, artifact_name: "briefing.md" };
  return value;
}
const frame = (observations = native(), extra = {}) => ({ ...producer, attempt_uid: "attempt-1", progress_sequence: 2, observations, ...extra });
const parse = observations => router.liveRouterActivity(frame(observations), producer.run_nonce);

test("Python execution receipts remain visible without treating process success as acceptance", () => {
  for (const name of ["python_execute", "kars_mission_python_execute"]) {
    for (const reason of ["exited", "timeout", "cancelled", "output_limit", "process_limit", "launch_failed", "cleanup_failed"]) {
      const value = native();
      value.trace.events[1].name = value.trace.events[2].name = name;
      const result = value.trace.events[2];
      result.ok = reason === "exited";
      result.result_summary = { kind: "python_execution", bytes: 512, receipt_sha256: `sha256:${"a".repeat(64)}`, exit_code: result.ok ? 0 : null, reason };
      assert.ok(parse(value));
      assert.match(router.toolResultSummary(result.result_summary), /does not establish application acceptance/);
      assert.match(render(value), /Python execution reported/);
      result.ok = !result.ok;
      assert.equal(parse(value), null);
    }
  }
});

test("malformed or mismatched Python receipts cannot hide inside an accepted trace", () => {
  for (const mutation of [
    s => { s.bytes = 0; }, s => { s.bytes = 65537; },
    s => { s.receipt_sha256 = `sha256:${"A".repeat(64)}`; },
    s => { s.exit_code = 2147483648; }, s => { delete s.exit_code; },
    s => { s.exit_code = 0.5; }, s => { s.reason = "invented"; },
    s => { s.stdout = "unvalidated output"; },
  ]) {
    const value = native();
    value.trace.events[1].name = value.trace.events[2].name = "kars_mission_python_execute";
    const summary = { kind: "python_execution", bytes: 512, receipt_sha256: `sha256:${"a".repeat(64)}`, exit_code: 0, reason: "exited" };
    mutation(summary);
    value.trace.events[2].result_summary = summary;
    assert.equal(parse(value), null);
  }
});

test("live runtime label is optional, validated, and cannot change within a producer stream", () => {
  for (const runtime of [undefined, null, "OpenClaw", "Hermes"]) {
    const value = frame(returned, { runtime });
    assert.ok(router.liveRouterActivity(value, producer.run_nonce));
    assert.equal(router.routerProducerKey(value), router.routerProducerKey(producer));
    assert.ok(router.advancesRouterActivity(value, structuredClone(value)));
  }
  assert.equal(router.liveRouterActivity(frame(returned, { runtime: "gpt-6-astra" }), producer.run_nonce), null);
  const before = frame(returned, { runtime: "Hermes" });
  const after = frame(returned, { runtime: "OpenClaw", progress_sequence: before.progress_sequence + 1 });
  assert.equal(router.advancesRouterActivity(before, after), false);
});
const render = (observations, provisional = true) => renderToStaticMarkup(React.createElement(RouterActivityPanel, {
  binding: { producer, observations, provisional }, ns: "kars-system", name: "mission",
}));

const operation = (value, seq = 3, extra = {}) => ({
  kind: "router_operation", scope_id: value.scope_id, round: 1, seq, source: "router-policy",
  call_id: value.trace.events[1].call_id, name: value.trace.events[1].name,
  operation: "egress_policy", flag: "egress_check", allowed: true, http_status: null, complete: null,
  destination_host: "source.example", destination_port: 443, ms: 2, ...extra,
});

test("argument previews follow exact call correlation into result drill-downs without HTML execution", () => {
  const value = native();
  value.trace.events[1].argument_preview = { text: '{"path":"/sandbox/report.md","content":"<script>example</script>"}', redacted: true, truncated: true };
  assert.ok(parse(value));
  const html = render(value);
  assert.match(html, /Model argument preview · redacted · truncated/);
  assert.match(html, /&lt;script&gt;example&lt;\/script&gt;/);
  assert.doesNotMatch(html, /<script>/);
  const actions = graph.actionsFromRouter({ producer, observations: value });
  assert.match(actions[2].args, /\/sandbox\/report.md/);
  const wrong = { ...value.trace.events[2], call_id: "other-call" };
  assert.equal(router.toolArgumentPreview(wrong, value.trace.events), "Arguments were not recorded for this call.");
  for (const invalid of [null, { text: "x".repeat(2049), truncated: true, redacted: false },
    { text: "🦀".repeat(513), truncated: false, redacted: false },
    { text: "x", truncated: false, redacted: "yes" }, { text: "x", truncated: false, redacted: true, raw: "hidden" }]) {
    value.trace.events[1].argument_preview = invalid;
    assert.equal(parse(value), null);
  }
});

test("router groups and tool drill-downs are newest first without mutating custody sequence", () => {
  const value = expandedActivity(3);
  const original = JSON.stringify(value);
  const html = render(value);
  assert.ok(html.indexOf("Router round 3") < html.indexOf("Router round 2"));
  assert.ok(html.indexOf("Router round 2") < html.indexOf("Router round 1"));
  assert.ok(html.indexOf("Harness reported") < html.indexOf("Model proposed"));
  assert.equal(JSON.stringify(value), original);
  assert.ok(parse(value));
});

test("router policy allow, HTTP 403 and harness success remain separately attributed facts", () => {
  const value = native();
  value.trace.events.splice(2, 0, operation(value), operation(value, 4, {
    source: "router", operation: "egress_fetch", flag: "response", allowed: null, http_status: 403, complete: true,
  }));
  assert.ok(parse(value));
  const html = render(value);
  assert.match(html, /flag: egress_check; decision: allow/);
  assert.match(html, /upstream HTTP 403/);
  assert.match(html, /Not a harness outcome/);
  assert.match(html, /source.example:443/);
  assert.match(html, /Immediate harness report/);
  const actions = graph.actionsFromRouter({ producer, observations: value });
  assert.equal(actions[2].source, "router-policy");
  assert.equal(actions[2].ok, null);
  assert.equal(actions[3].source, "router");
  assert.equal(actions[3].ok, null);
  assert.equal(actions[4].ok, true);
});

test("router deny never fabricates a fetch result; missing flags are explicitly unavailable", () => {
  const value = native(); value.trace.events.pop();
  value.trace.events.push(operation(value, 3, { allowed: false, flag: "private_target" }));
  assert.ok(parse(value));
  assert.match(render(value), /decision: deny/);
  assert.doesNotMatch(render(value), /upstream HTTP/);
  assert.match(render(native()), /unavailable, not allowed or denied/);
});

test("router observations reject incorrect call correlation, raw destinations and fabricated flags", () => {
  const changes = [
    { source: "harness-reported" }, { call_id: "other" }, { name: "other" }, { round: 2 },
    { allowed: "true" }, { flag: "guessed-safe" }, { destination_host: "https://source.example/secret?token=x" },
    { destination_host: "user@source.example" }, { destination_port: 0 }, { destination_port: 65536 },
    { flag: "private_target", allowed: true }, { http_status: 200 }, { complete: true }, { ms: 3_600_001 },
    { operation: "egress_fetch", source: "router", flag: "response", allowed: null, http_status: 403, complete: null },
  ];
  for (const extra of changes) {
    const value = native(); value.trace.events.splice(2, 0, operation(value, 3, extra));
    assert.equal(parse(value), null, JSON.stringify(extra));
  }
  const value = native();
  value.trace.events.splice(1, 0, operation(value, 2));
  assert.equal(parse(value), null, "router observation must follow its proposal");
});

test("native Activity retains actual selected proposals, full round fields and immediate reports", () => {
  const value = native();
  assert.ok(parse(value));
  assert.deepEqual(parse(value).observations, value);
  assert.equal(router.routerActivityCount(value), 1);
  assert.equal(value.trace.events[0].tool_calls_observed, 3, "unselected proposals do not rewrite router counts");
  value.trace.events.pop();
  assert.ok(parse(value), "pending proposal is not a completion claim");
});

test("retained Hermes native trace preserves actual read/write tools, failed search and seven rounds", () => {
  const value = JSON.parse(readFileSync(new URL("../../../inference-router/tests/fixtures/router-observations-hermes-native.json", import.meta.url), "utf8"));
  assert.ok(parse(value));
  assert.deepEqual(parse(value).observations, value);
  assert.equal(router.routerActivityCount(value), 7);
  const html = render(value, false);
  assert.match(html, /write_file/);
  assert.match(html, /read_file/);
  assert.match(html, /handoff.md/);
  assert.match(html, /Tool execution failed/);
});

function expandedActivity(toolRounds, nativeCoverage = true, toolsPerRound = 1) {
  const value = native();
  const [roundTemplate, proposalTemplate, resultTemplate] = structuredClone(value.trace.events);
  value.rounds = Array.from({ length: toolRounds + (nativeCoverage ? 0 : 1) }, (_, i) => i + 1);
  value.trace.rounds = [...value.rounds];
  value.trace.events = [];
  if (nativeCoverage) value.correlated_tools = toolRounds * toolsPerRound;
  else {
    delete value.correlated_tools;
    value.responses = value.rounds.length;
    value.coverage = value.trace.coverage = "returned-responses-only";
  }
  let seq = 0;
  for (const round of value.rounds) {
    const calls = round <= toolRounds ? toolsPerRound : 0;
    value.trace.events.push({ ...roundTemplate, round, seq: ++seq, tool_calls_observed: calls });
    for (let i = 0; i < calls; i++) {
      const call_id = `call-${round}-${i}`;
      value.trace.events.push({ ...proposalTemplate, round, seq: ++seq, call_id });
      value.trace.events.push({
        kind: "router_operation", scope_id: value.scope_id, round, seq: ++seq,
        source: "router-policy", call_id, name: proposalTemplate.name,
        operation: "tool_policy", flag: "governance", allowed: true,
        http_status: null, complete: null, destination_host: null, destination_port: null, ms: 1,
      });
      value.trace.events.push({ ...resultTemplate, round, seq: ++seq, call_id,
        ...(nativeCoverage ? {} : { reported_in_round: round + 1 }),
      });
    }
  }
  return value;
}

test("browser retains growing native Activity beyond the old event and round limits", () => {
  const before = frame(expandedActivity(8, true, 3));
  const after = frame(expandedActivity(28, true, 3), { progress_sequence: 85 });
  assert.ok(after.observations.trace.events.length > 256);
  assert.ok(router.liveRouterActivity(before, producer.run_nonce));
  assert.ok(router.liveRouterActivity(after, producer.run_nonce));
  assert.ok(router.advancesRouterActivity(before, after));
  for (const name of ["kars_mission_file_write", "write_file"]) {
    const value = expandedActivity(128);
    for (const event of value.trace.events) if (event.name) event.name = name;
    assert.ok(parse(value), name);
    assert.equal(graph.actionsFromRouter({ producer, observations: value }).length, value.trace.events.length);
    assert.match(render(value), /briefing.md/);
    value.trace.events.at(-1).call_id = "corrupted-late-result";
    assert.equal(parse(value), null, "late correlation is still checked");
  }
});

test("browser and BFF retain matching bounded Activity for both coverage formats", () => {
  const backend = readFileSync(new URL("../../bff/src/routes/tasks/router_activity.rs", import.meta.url), "utf8");
  const limit = name => Number(backend.match(new RegExp(`const ${name}: usize = (\\d+);`))[1]);
  const maxRounds = limit("MAX_OBSERVATION_ROUNDS");
  const maxEvents = limit("MAX_OBSERVATION_EVENTS");
  for (const nativeCoverage of [true, false]) {
    const value = expandedActivity(maxRounds - (nativeCoverage ? 0 : 1), nativeCoverage);
    assert.ok(parse(value));
    assert.equal(parse(expandedActivity(maxRounds + (nativeCoverage ? 1 : 0), nativeCoverage)), null);
    const policy = value.trace.events.find(event => event.kind === "router_operation");
    while (value.trace.events.length < maxEvents) {
      value.trace.events.push({ ...policy, seq: value.trace.events.at(-1).seq + 1 });
    }
    assert.ok(parse(value), "the full retained event boundary is accepted");
    value.trace.events.push({ ...policy, seq: value.trace.events.at(-1).seq + 1 });
    assert.equal(parse(value), null, "overflow remains rejected");
  }
});

test("native file summaries support new names and legacy aliases with bounded detail", () => {
  for (const name of ["file_read", "read_file", "kars_mission_read", "kars_mission_file_read"]) {
    const value = native();
    value.trace.events[1].name = value.trace.events[2].name = name;
    value.trace.events[2].result_summary = { kind: "file_read", bytes: 17, returned_bytes: 8, truncated: true };
    assert.ok(parse(value), name);
    assert.match(render(value), /Read 8 of 17 bytes \(truncated\)/);
    value.trace.events[2].result_summary.content = "raw file contents";
    assert.equal(parse(value), null);
  }
  for (const name of ["file_write", "write_file", "kars_mission_write", "kars_mission_file_write"]) {
    const value = native(); value.trace.events[1].name = value.trace.events[2].name = name;
    assert.ok(parse(value), name);
  }
});

test("native guard rejects mismatched, duplicated, dropped and invented coverage", () => {
  const changes = [
    v => { v.responses = 1; }, v => { v.responses = null; },
    v => { v.correlated_tools = 0; }, v => { v.correlated_tools = 257; }, v => { v.correlated_tools = 2; },
    v => { v.trace.events[0].accepted = false; }, v => { v.trace.events[0].accepted = null; },
    v => { v.trace.events[2].call_id = "wrong-call"; }, v => { v.trace.events[2].name = "wrong-name"; },
    v => { v.trace.events[2].round = 2; }, v => { v.trace.events[2].reported_in_round = null; },
    v => { v.trace.events[2].reported_in_round = 1; }, v => { v.trace.events[2].reported_in_round = 2; },
    v => { v.trace.events[2].source = "model-proposed"; },
    v => { v.trace.events.push({ ...v.trace.events[1], seq: 6 }); },
    v => { v.trace.events.push({ ...v.trace.events[2], seq: 6 }); },
    v => { v.trace.events.push({ ...v.trace.events[0], seq: 6 }); },
    v => { v.trace.events.shift(); },
    v => { [v.trace.events[1], v.trace.events[2]] = [v.trace.events[2], v.trace.events[1]]; v.trace.events[1].seq = 2; v.trace.events[2].seq = 5; },
    v => { v.trace.truncated = true; }, v => { v.trace.dropped_events = 1; }, v => { v.trace.missing_rounds = [1]; },
    v => { v.rounds.push(2); v.trace.rounds.push(2); v.correlated_tools = 2; },
    v => { v.trace.events[2].result_summary.artifact_name = "../secret"; },
    v => { v.trace.events[2].result_summary.content = "raw contents"; },
    v => { v.trace.coverage = "returned-responses-only"; }, v => { v.trace.scope_id = "other"; },
  ];
  for (const change of changes) { const value = native(); change(value); assert.equal(parse(value), null, String(change)); }
});

test("native multi-round selection binds each result to its actual prior proposal round", () => {
  const value = native();
  value.correlated_tools = 2; value.rounds.push(3); value.trace.rounds.push(3);
  const [round, proposal, result] = structuredClone(value.trace.events);
  Object.assign(round, { round: 3, seq: 8 });
  Object.assign(proposal, { round: 3, seq: 9, call_id: "call-2" });
  Object.assign(result, { round: 3, seq: 10, call_id: "call-2" });
  value.trace.events.push(round, proposal, result);
  assert.ok(parse(value));
  value.trace.events[2].reported_in_round = 3;
  assert.ok(parse(value), "a genuine later selected reporting round is still accepted");
  value.trace.events[5].round = 1;
  assert.equal(parse(value), null, "valid selected round but wrong proposal correlation");
});

test("unavailable native snapshots retain only the fixed reason and bounded selection", () => {
  const value = native();
  delete value.trace;
  Object.assign(value, { state: "unavailable", reason: unavailableReason });
  assert.ok(parse(value));
  assert.ok(parse({ ...value, scope_id: null, rounds: [] }));
  for (const patch of [{ reason: "Response correlation changed or was invalid" }, { correlated_tools: 257 }, { responses: 1 }]) {
    assert.equal(parse({ ...value, ...patch }), null);
  }
  assert.match(render(value), /Router observations unavailable: Native tool-correlated observations unavailable/);
});

test("returned-response coverage accepts immediate correlated results or a later selected reporting round", () => {
  assert.ok(parse(returned));
  assert.equal(router.routerActivityCount(returned), 3);
  const immediate = structuredClone(returned);
  delete immediate.trace.events[2].reported_in_round;
  assert.ok(parse(immediate));
  assert.match(render(immediate), /Immediate harness report/);
  for (const [field, invalid] of [["call_id", "unrelated"], ["name", "unrelated"], ["round", 2], ["ok", null]]) {
    const value = structuredClone(immediate);
    value.trace.events[2][field] = invalid;
    assert.equal(parse(value), null);
  }
  const withoutProposal = structuredClone(immediate);
  withoutProposal.trace.events.splice(1, 1);
  assert.equal(parse(withoutProposal), null);
  for (const reported of [null, 1, 3]) {
    const value = structuredClone(returned);
    value.trace.events[2].reported_in_round = reported;
    assert.equal(parse(value), null);
  }
  assert.match(render(returned), /Responses received: 3/);
  assert.match(render(returned), /Reported in router round 2/);
  assert.doesNotMatch(render(returned), /Immediate harness report|Correlated native tools/);
});

test("returned Activity advances from an immediate outcome to one later contradictory report", () => {
  const initial = structuredClone(returned);
  initial.responses = 1;
  initial.rounds = initial.trace.rounds = [1];
  initial.trace.events.pop();
  const immediate = initial.trace.events[2];
  delete immediate.reported_in_round;
  immediate.ok = true;
  const later = structuredClone(returned);
  later.trace.events[2].seq = 4;
  later.trace.events[2].ok = null;
  later.trace.events[3].seq = 5;
  assert.ok(parse(initial));
  assert.ok(parse(later));
  const before = frame(initial);
  const after = frame(later, { progress_sequence: before.progress_sequence + 1 });
  assert.ok(router.advancesRouterActivity(before, after));
  for (const value of [initial, later]) {
    const actions = graph.actionsFromRouter({ producer, observations: value });
    assert.equal(value.trace.events.filter(event => event.kind === "tool_result").length, 1);
    assert.equal(actions.filter(action => action.source === "harness-reported").length, 1);
  }
  assert.doesNotMatch(render(later), /Immediate harness report/);
  assert.match(render(later), /Reported in router round 2/);
});

test("native UI describes partial tool correlation rather than model-only or final response coverage", () => {
  const value = native();
  const html = render(value);
  for (const label of ["Correlated native tools: 1", "Proposing router rounds: 1", "Immediate harness report",
    "Wrote 17 bytes; exported briefing.md", "Model-only rounds, final responses and uncorrelated tool calls are not covered",
    "not independent proof", "Tool proposals observed: 3"]) assert.ok(html.includes(label), label);
  assert.doesNotMatch(html, /Responses received|returned response IDs|Reported in router round undefined|Download system evidence/);
  assert.match(render(value, false), /Download system evidence/);
  const actions = graph.actionsFromRouter({ producer, observations: value, provisional: true });
  assert.match(actions[2].result, /Immediate harness report; outcome success\. Wrote 17 bytes/);
  assert.equal(actions[2].round, 1);
  assert.equal(actions[2].source, "harness-reported");
});

test("native monotonicity pins coverage, tool count, all producer identities, scope and overlapping records", () => {
  const first = frame();
  assert.equal(router.advancesRouterActivity(first, structuredClone(first)), true);
  assert.equal(router.advancesRouterActivity(first, frame(returned, { progress_sequence: 3 })), false);
  assert.equal(router.advancesRouterActivity(frame(returned), frame(native(), { progress_sequence: 3 })), false);
  for (const field of [...Object.keys(producer), "attempt_uid"]) {
    assert.equal(router.advancesRouterActivity(first, frame(native(), { progress_sequence: 3, [field]: "replacement" })), false, field);
  }
  const selection = native();
  delete selection.trace;
  const unavailable = { ...selection, state: "unavailable", reason: unavailableReason };
  for (const [count, allowed] of [[1, false], [2, true], [3, true]]) {
    const before = frame({ ...unavailable, correlated_tools: 2 });
    const after = frame({ ...unavailable, correlated_tools: count }, { progress_sequence: 3 });
    assert.equal(router.advancesRouterActivity(before, after), allowed);
  }
  assert.equal(router.advancesRouterActivity(first, frame(unavailable, { progress_sequence: 3 })), true);
  assert.equal(router.advancesRouterActivity(first, frame({ ...unavailable, scope_id: null }, { progress_sequence: 3 })), false);
  const changed = frame(native(), { progress_sequence: 3 });
  changed.observations.trace.events[2].ok = false;
  assert.equal(router.advancesRouterActivity(first, changed), false);
  assert.equal(router.liveRouterActivity(first, "different-run"), null);
});

const phaseFrame = (role, activity = frame()) => ({
  run_nonce: "root-run", phase_key: router.reviewedPhaseKey({ role, phase: "research" }), activity,
});

test("reviewed stream binds independent specialists and reconnect never republishes stale siblings", () => {
  const pins = new router.ReviewedActivityPins();
  const first = phaseFrame("researcher");
  const second = phaseFrame("reviewer", frame(returned, { run_nonce: "review-run", task_uid: "review-uid" }));
  assert.equal(pins.observe(first, "root-run"), true);
  assert.equal(pins.observe(second, "root-run"), true);
  assert.equal(Object.keys(pins.current).length, 2);
  pins.clearVisible();
  assert.equal(Object.keys(pins.current).length, 0);
  assert.equal(pins.observe(first, "root-run"), true);
  assert.equal(Object.keys(pins.current).length, 1);
  assert.equal(pins.current[second.phase_key], undefined);
  assert.equal(pins.observe(phaseFrame("researcher", frame(native(), { pod_uid: "replacement" })), "root-run"), false);
  assert.equal(pins.observe(second, "root-run"), true);
});

test("reviewed frames reject wrong roots, malformed keys, changed custody and progress regression", () => {
  const first = phaseFrame("researcher");
  for (const invalid of [null, {}, { ...first, run_nonce: "old-root" },
    { ...first, phase_key: "__proto__" }, { ...first, phase_key: '["researcher"]' },
    { ...first, phase_key: '["researcher", "research"]' },
    { ...first, phase_key: '["researcher",""]' }, { ...first, activity: {} }]) {
    assert.equal(new router.ReviewedActivityPins().observe(invalid, "root-run"), false);
  }
  for (const change of [value => { value.progress_sequence = 1; },
    value => { value.attempt_uid = "replacement"; },
    value => { value.observations.trace.events[2].ok = false; }]) {
    const pins = new router.ReviewedActivityPins();
    assert.equal(pins.observe(first, "root-run"), true);
    const next = structuredClone(first); change(next.activity);
    assert.equal(pins.observe(next, "root-run"), false);
  }
});

test("phase revocation is permanent and does not hide another specialist", () => {
  const pins = new router.ReviewedActivityPins();
  const first = phaseFrame("researcher");
  const second = phaseFrame("reviewer");
  pins.observe(first, "root-run"); pins.observe(second, "root-run");
  assert.equal(pins.observe(phaseFrame("researcher", null), "root-run"), true);
  assert.equal(pins.current[first.phase_key], undefined);
  assert.ok(pins.current[second.phase_key]);
  pins.clearVisible();
  assert.equal(pins.observe(first, "root-run"), false);
  assert.equal(pins.observe(second, "root-run"), true);
});

function traceHook() {
  const states = [], cleanups = [], sources = [];
  let index = 0, first = true;
  class EventSource {
    listeners = new Map();
    closed = false;
    constructor(url) { this.url = url; sources.push(this); }
    addEventListener(name, listener) { this.listeners.set(name, listener); }
    close() { this.closed = true; }
    emit(name, data) { this.listeners.get(name)?.({ data: JSON.stringify(data) }); }
  }
  const { useLiveTrace: renderTrace } = load("../src/components/use-live-trace.ts", {
    react: {
      useState(initial) {
        const slot = index++;
        if (first) states[slot] = initial;
        return [states[slot], next => { states[slot] = typeof next === "function" ? next(states[slot]) : next; }];
      },
      useEffect(effect) { if (first) cleanups.push(effect()); },
      useMemo: compute => compute(),
    },
    "@/lib/router-activity": router, "@/lib/mission-run-evidence": mission,
  }, { EventSource, requestAnimationFrame: callback => { callback(); return 1; }, cancelAnimationFrame() {} });
  const snapshot = () => {
    index = 0;
    const value = renderTrace("kars-system", "team-run", true, [], "root-run");
    first = false;
    return value;
  };
  snapshot();
  return { source: sources[0], snapshot, cleanup: () => cleanups.forEach(cleanup => cleanup?.()) };
}

test("real hook shares phase updates and root unavailability does not erase specialist progress", () => {
  const { source, snapshot, cleanup } = traceHook();
  const first = phaseFrame("researcher");
  source.emit("reviewed-phase-activity", first);
  source.emit("unavailable");
  assert.equal(snapshot().phaseActivity[first.phase_key].task_uid, producer.task_uid);
  assert.equal(snapshot().routerActivity, null);
  source.emit("renew");
  assert.equal(Object.keys(snapshot().phaseActivity).length, 0);
  assert.equal(source.closed, false, "EOF must reconnect without a new hook effect");
  source.emit("reviewed-phase-activity", first);
  assert.equal(Object.keys(snapshot().phaseActivity).length, 1);
  source.emit("error");
  assert.equal(Object.keys(snapshot().phaseActivity).length, 0);
  source.emit("reviewed-phase-activity", phaseFrame("researcher", frame(native(), { pod_uid: "replacement" })));
  assert.equal(source.closed, true, "custody pins survive transport loss");
  assert.equal(Object.keys(snapshot().phaseActivity).length, 0);
  cleanup();
});

test("real hook clears all provisional state on end, done, revocation and cleanup", () => {
  for (const event of ["end", "done", "router-activity-unavailable", "cleanup"]) {
    const { source, snapshot, cleanup } = traceHook();
    source.emit("reviewed-phase-activity", phaseFrame("researcher"));
    source.emit("router-activity", frame(native(), { run_nonce: "root-run" }));
    assert.ok(snapshot().routerActivity);
    assert.equal(Object.keys(snapshot().phaseActivity).length, 1);
    if (event === "cleanup") cleanup(); else source.emit(event);
    assert.equal(source.closed, true, event);
    assert.equal(snapshot().routerActivity, null, event);
    assert.equal(Object.keys(snapshot().phaseActivity).length, 0, event);
    source.emit("reviewed-phase-activity", phaseFrame("reviewer"));
    assert.equal(Object.keys(snapshot().phaseActivity).length, 0);
  }
});
