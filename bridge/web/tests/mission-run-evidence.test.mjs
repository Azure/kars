// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const ts = require("typescript");
function load(path, globals = {}) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8");
  const exports = {};
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, { exports, ...globals });
  return exports;
}
const router = load("../src/lib/router-activity.ts");
const evidence = load("../src/lib/mission-run-evidence.ts", { require: name => {
  if (name === "./router-activity") return router;
  throw new Error(`Unexpected evidence import: ${name}`);
} });
const observations = JSON.parse(readFileSync(new URL("../../../inference-router/tests/fixtures/router-observations-v1.json", import.meta.url), "utf8"));
const liveFrame = (extra = {}) => ({
  task_uid: "task-uid", run_nonce: "rev-2", assignment_id: "assignment-id",
  agent_name: "worker", agent_did: "did:mesh:producer", dispatcher_did: "did:mesh:dispatcher",
  sandbox_uid: "sandbox-uid", pod_uid: "pod-uid", runtime_boot_id: "boot-id",
  attempt_uid: "attempt-uid", progress_sequence: 2,
  observations: structuredClone(observations), ...extra,
});
const { missionRunState, activityForRevision, mergeRevisionActivity, revisionArtifactUrl, revisionStreamUrl } = evidence;
const event = (nonce = "rev-2", extra = {}) => ({
  kind: "round", runNonce: nonce, round: 1, seq: 1, ms: 30,
  ts: "2026-10-03T06:47:45Z", prompt_tokens: 6940, completion_tokens: 485,
  total_tokens: 7425, tool_calls: 1, finish_reason: "stop", ...extra,
});
const result = (extra = {}) => ({ assignment_nonce: "rev-2", status: "ok", output: "Useful briefing", reviewable: true, blocked: null, ...extra });

test("standalone Mission lists use parent and Team identity rather than generated names", () => {
  assert.equal(evidence.isStandaloneMission({ name: "rp-real-root", parent: null, team: null }), true);
  assert.equal(evidence.isStandaloneMission({ name: "ordinary-name", parent: "root", team: null }), false);
  assert.equal(evidence.isStandaloneMission({ name: "ordinary-name", parent: null, team: "team" }), false);
});

test("failure messages retain separate runtime errors and partial output without invented causes", () => {
  const error = "Governed inference /v1/session unavailable (HTTP 503)";
  assert.equal(evidence.missionFailureMessage(result({ error, output: "" })), error);
  assert.equal(evidence.missionFailureMessage(result({ error, output: "Partial notes" })), `${error}\n\nPartial notes`);
  assert.equal(evidence.missionFailureMessage(result({ error, output: error })), error);
  assert.equal(evidence.missionFailureMessage(result({ output: "Legacy error" })), "Legacy error");
  assert.equal(evidence.missionFailureMessage(null), "No failure message was recorded for this revision.");
});

test("failure advice does not turn admission errors or generic messages into proven causes", () => {
  const admission = evidence.missionFailureAdvice("Governed inference budget wire request bounds unavailable/denied");
  assert.match(admission.cause, /does not establish token-budget exhaustion/);
  for (const reason of ["budget unavailable", "Hermes not discoverable in mesh registry", "timeout", "blocked by policy"]) {
    const advice = evidence.missionFailureAdvice(reason);
    assert.match(advice.cause, /does not establish the underlying cause/);
    assert.doesNotMatch(advice.remedy, /higher|OpenClaw|Re-run/);
  }
  assert.match(evidence.missionFailureAdvice("Daily token budget exceeded (23131/20000)").cause, /not independently verified/);
});

test("exact terminal evidence survives stale, missing, or in-flight assignment projections", () => {
  for (const assignment of [null, { task_id: "rev-1", state: "Running" }, { task_id: "rev-2", state: "Running" }]) {
    const state = missionRunState({ current_run_nonce: "rev-2", result: result(), assignment });
    assert.equal(state.realDelivery, true);
    assert.equal(state.assignmentInFlight, false);
    assert.equal(state.awaitingAssignment, false);
  }
});

test("only eligible useful success in the exact revision is ready for review", () => {
  for (const invalid of [
    { reviewable: false }, { reviewable: undefined }, { output: "  " },
    { status: "failed" }, { status: "rejected" }, { status: "error" },
    { status: null }, { status: "running" }, { status: "unknown" },
    { assignment_nonce: "rev-1" }, { blocked: { reason: "token_budget" } },
  ]) {
    assert.equal(missionRunState({ current_run_nonce: "rev-2", result: result(invalid), assignment: null }).realDelivery, false);
  }
  for (const nonce of [null, "", "rev-3"]) {
    const state = missionRunState({ current_run_nonce: nonce, result: result(), assignment: null });
    assert.equal(state.currentResult, null);
    assert.equal(state.realDelivery, false);
  }
  const pending = missionRunState({ current_run_nonce: "rev-3", result: result(), assignment: { task_id: "rev-2", state: "Completed" } });
  assert.equal(pending.awaitingAssignment, true);
  assert.equal(pending.assignmentInFlight, true);
});

test("activity accepts only well-formed exact-revision frames", () => {
  assert.equal(activityForRevision(event(), "rev-2"), true);
  for (const extra of [
    { runNonce: "rev-1" }, { assignmentNonce: "rev-1" }, { assignment_nonce: null },
    { seq: -1 }, { seq: 1.5 }, { seq: Number.MAX_SAFE_INTEGER + 1 },
    { round: -1 }, { ms: Infinity }, { ts: "invalid" }, { ts: 1 },
    { total_tokens: -1 }, { prompt_tokens: 1.5 }, { completion_tokens: NaN },
    { tool_calls: "1" }, { agent: null }, { agentRole: "admin" }, { finish_reason: null },
  ]) assert.equal(activityForRevision(event("rev-2", extra), "rev-2"), false, JSON.stringify(extra));
  const unbound = event(); delete unbound.runNonce;
  for (const invalid of [null, [], {}, unbound, "frame"]) assert.equal(activityForRevision(invalid, "rev-2"), false);
  assert.equal(activityForRevision(event(), ""), false);
  const tool = event("rev-2", { kind: "tool", name: "file_write", args_preview: "...", result_preview: "saved", ok: true, source: "harness" });
  assert.equal(activityForRevision(tool, "rev-2"), true);
  for (const extra of [{ source: "unknown" }, { ok: "true" }, { name: null }, { args_preview: {} }])
    assert.equal(activityForRevision({ ...tool, ...extra }, "rev-2"), false);
});

test("revision activity deduplication preserves distinct producers without summing snapshots", () => {
  const seed = event("rev-2", { agentInstance: "pod-a" });
  const latest = event("rev-2", { agentInstance: "pod-a", ms: 40 });
  const other = event("rev-2", { agentInstance: "pod-b" });
  const merged = mergeRevisionActivity([seed, event("old")], [latest, other], "rev-2");
  assert.equal(merged.length, 2);
  assert.equal(merged[0].ms, 40);
  assert.equal(merged[0].total_tokens, 7425);
  assert.equal(mergeRevisionActivity([seed], [latest], null).length, 0);
  const unsequenced = event(); delete unsequenced.seq;
  assert.equal(mergeRevisionActivity([unsequenced], [unsequenced], "rev-2").length, 1);
});

test("stream and artifact URLs require explicit identity and encode every component", () => {
  for (const nonce of [undefined, null, ""]) {
    assert.equal(revisionStreamUrl("ns", "task", nonce), null);
    assert.equal(revisionArtifactUrl("ns", "task", "briefing.md", nonce), undefined);
  }
  assert.equal(revisionStreamUrl("", "task", "rev"), null);
  assert.equal(revisionArtifactUrl("ns", "task", "", "rev"), undefined);
  assert.equal(revisionArtifactUrl("n/s", "t ?", "briefing #.md", "r&2"), "/api/namespaces/n%2Fs/tasks/t%20%3F/artifact/briefing%20%23.md?run_nonce=r%262");
  assert.equal(revisionStreamUrl("n/s", "t ?", "r&2"), "/api/namespaces/n%2Fs/tasks/t%20%3F/stream?run_nonce=r%262");
});

test("router snapshots require exact revision, bounded identities and truthful raw shape", () => {
  assert.ok(router.liveRouterActivity(liveFrame(), "rev-2"));
  for (const nonce of ["", "rev-1"]) assert.equal(router.liveRouterActivity(liveFrame(), nonce), null);
  for (const field of ["task_uid", "run_nonce", "assignment_id", "agent_name", "agent_did", "dispatcher_did",
    "sandbox_uid", "pod_uid", "runtime_boot_id", "attempt_uid"]) {
    for (const value of [null, "", 12, "x".repeat(2049)]) {
      assert.equal(router.liveRouterActivity(liveFrame({ [field]: value }), "rev-2"), null, field);
    }
  }
  for (const value of [null, [], {}, "bad", liveFrame({ progress_sequence: 0 }), liveFrame({ progress_sequence: 1.5 })]) {
    assert.equal(router.liveRouterActivity(value, "rev-2"), null);
  }
  const invalid = [
    a => { a.version = 2; }, a => { a.source = "untrusted"; }, a => { a.responses = 0; },
    a => { a.rounds = [0]; }, a => { a.rounds = [2, 1]; }, a => { a.rounds = [1, 1]; },
    a => { a.rounds = Array.from({ length: 33 }, (_, i) => i + 1); a.responses = 33; },
    a => { a.trace.scope_id = "other"; }, a => { a.trace.durable = true; },
    a => { a.trace.missing_rounds = [3]; }, a => { a.trace.dropped_events = -1; },
    a => { a.trace.events[0].seq = 0; }, a => { a.trace.events[1].seq = 1; },
    a => { a.trace.events[0].round = 3; }, a => { a.trace.events[0].scope_id = "other"; },
    a => { a.trace.events[0].usage.total_tokens = -1; }, a => { a.trace.events[0].http_status = 600; },
    a => { a.trace.events[0].ms = NaN; }, a => { a.trace.events[1].ok = true; },
    a => { a.trace.events[2].ok = "false"; }, a => { a.trace.events[2].reported_in_round = 1; },
    a => { a.trace.events[2].reported_in_round = 3; },
  ];
  for (const mutate of invalid) {
    const frame = liveFrame(); mutate(frame.observations);
    assert.equal(router.liveRouterActivity(frame, "rev-2"), null, String(mutate));
  }
});

test("router progression pins every producer field, attempt, scope and overlapping records", () => {
  const first = liveFrame();
  assert.equal(router.advancesRouterActivity(null, first), true);
  assert.equal(router.advancesRouterActivity(first, structuredClone(first)), true);
  for (const field of ["task_uid", "run_nonce", "assignment_id", "agent_name", "agent_did", "dispatcher_did",
    "sandbox_uid", "pod_uid", "runtime_boot_id", "attempt_uid"]) {
    assert.equal(router.advancesRouterActivity(first, liveFrame({ progress_sequence: 3, [field]: "changed" })), false, field);
  }
  assert.equal(router.advancesRouterActivity(first, liveFrame({ progress_sequence: 1 })), false);
  const changed = liveFrame(); changed.observations.trace.truncated = true;
  assert.equal(router.advancesRouterActivity(first, changed), false, "equal sequence cannot change");
  changed.progress_sequence = 3;
  assert.equal(router.advancesRouterActivity(first, changed), true);
  changed.observations.responses = 2;
  assert.equal(router.advancesRouterActivity(first, changed), false);
  changed.observations.responses = 3;
  changed.observations.trace.events[0].ms++;
  assert.equal(router.advancesRouterActivity(first, changed), false, "overlapping event cannot change");
  const expired = liveFrame({ progress_sequence: 3, observations: {
    ...observations, state: "unavailable", reason: "Router observations unavailable after execution (reset, expiry or read failure)",
  } });
  assert.ok(router.liveRouterActivity(expired, "rev-2"));
  assert.equal(router.advancesRouterActivity(first, expired), true);
  assert.equal(router.advancesRouterActivity(expired, liveFrame({ progress_sequence: 4 })), true);
  const replaced = liveFrame({ progress_sequence: 4 });
  replaced.observations.scope_id = "different";
  assert.equal(router.advancesRouterActivity(expired, replaced), false, "unavailable does not release the scope pin");
  expired.observations.scope_id = "different";
  assert.equal(router.advancesRouterActivity(first, expired), false);
});

test("unavailable snapshots cannot erase immutable observed records", () => {
  const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
  const source = h.connections[0]; source.signal("router-activity", liveFrame());
  const unavailable = { ...observations };
  delete unavailable.trace;
  source.signal("router-activity", liveFrame({ progress_sequence: 3, observations: {
    ...unavailable, state: "unavailable", reason: "Router observations unavailable after execution (reset, expiry or read failure)",
  } }));
  assert.equal(h.render("rev-2").routerActivity.observations.state, "unavailable");
  const changed = liveFrame({ progress_sequence: 4 }); changed.observations.trace.events[0].ms++;
  source.signal("router-activity", changed);
  assert.equal(source.closed, true);
  assert.equal(h.render("rev-2").routerActivity, null);
  h.unmount();
});

function hookHarness() {
  const slots = [], pending = new Map(), frames = new Map(), connections = [];
  let cursor = 0, nextFrame = 0;
  const same = (a, b) => a && a.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  const react = {
    useState(initial) {
      const index = cursor++;
      slots[index] ??= { value: initial };
      return [slots[index].value, value => { slots[index].value = typeof value === "function" ? value(slots[index].value) : value; }];
    },
    useEffect(effect, deps) {
      const index = cursor++;
      const old = slots[index];
      if (!same(old?.deps, deps)) pending.set(index, () => {
        old?.cleanup?.();
        slots[index] = { deps, cleanup: effect() };
      });
    },
    useMemo(compute, deps) {
      const index = cursor++;
      if (!same(slots[index]?.deps, deps)) slots[index] = { deps, value: compute() };
      return slots[index].value;
    },
  };
  class EventSource {
    listeners = new Map(); closed = false;
    constructor(url) { this.url = url; connections.push(this); }
    close() { this.closed = true; }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    message(value) { this.onmessage?.({ data: JSON.stringify(value) }); }
    signal(name, value) { this.listeners.get(name)?.({ data: JSON.stringify(value) }); }
  }
  const { useLiveTrace } = load("../src/components/use-live-trace.ts", {
    require: name => {
      if (name === "react") return react;
      if (name === "@/lib/mission-run-evidence") return evidence;
      if (name === "@/lib/router-activity") return router;
      throw new Error(`Unexpected import: ${name}`);
    }, EventSource,
    requestAnimationFrame: callback => { frames.set(++nextFrame, callback); return nextFrame; },
    cancelAnimationFrame: id => frames.delete(id),
  });
  return {
    connections,
    render: function useRender(nonce, seed = [], running = true) { cursor = 0; return useLiveTrace("ns", "task", running, seed, nonce); },
    effects() { for (const callback of pending.values()) callback(); pending.clear(); },
    hydrate() { for (const callback of frames.values()) callback(); frames.clear(); },
    unmount() { for (const slot of slots) slot?.cleanup?.(); },
  };
}

test("live router snapshots replace expired records and preserve pins across reconnect", () => {
  const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
  const source = h.connections[0];
  source.signal("router-activity", liveFrame());
  assert.equal(h.render("rev-2").routerActivity.observations.trace.events.length, 4);
  source.signal("error"); source.signal("open");
  source.signal("router-activity", liveFrame());
  assert.equal(source.closed, false);
  const next = liveFrame({ progress_sequence: 3 });
  next.observations.trace.events = next.observations.trace.events.slice(-1);
  next.observations.trace.missing_rounds = [1];
  source.signal("router-activity", next);
  assert.equal(h.render("rev-2").routerActivity.observations.trace.events.length, 1);
  source.signal("router-activity", liveFrame({ assignment_id: "replacement", progress_sequence: 4 }));
  assert.equal(source.closed, true);
  assert.equal(h.render("rev-2").routerActivity, null);
  source.signal("router-activity", next);
  assert.equal(h.render("rev-2").routerActivity, null);
  h.unmount();
});

test("stream interruptions hide provisional observations until a validated snapshot recovers", () => {
  for (const interruption of ["error", "unavailable"]) {
    const h = hookHarness(); h.render("rev-2", [event()]); h.effects(); h.hydrate();
    const source = h.connections[0];
    source.message(event("rev-2", { seq: 2 }));
    source.signal("router-activity", liveFrame());
    assert.notEqual(h.render("rev-2").routerActivity, null);
    source.signal(interruption);
    assert.equal(h.render("rev-2", [event()]).routerActivity, null, interruption);
    assert.equal(h.render("rev-2", [event()]).events.length, 2, "retained activity survives");
    assert.equal(source.closed, false, "native EventSource recovery remains enabled");
    source.signal("open");
    assert.equal(h.render("rev-2").routerActivity, null, "a transport connection is not evidence");
    source.signal("router-activity", liveFrame());
    assert.notEqual(h.render("rev-2").routerActivity, null, "an identical pinned snapshot can recover");
    assert.equal(h.connections.length, 1, "recovery must not open a duplicate connection");
    h.unmount();
  }
});

test("stream interruptions do not release producer or observed-record pins", () => {
  for (const interruption of ["error", "unavailable"]) {
    for (const mutate of [
      frame => { frame.assignment_id = "replacement"; },
      frame => { frame.progress_sequence = 1; },
      frame => { frame.observations.trace.events[0].ms++; },
    ]) {
      const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
      const source = h.connections[0];
      source.signal("router-activity", liveFrame());
      source.signal(interruption);
      assert.equal(h.render("rev-2").routerActivity, null);
      source.signal("open");
      const invalid = liveFrame({ progress_sequence: 3 }); mutate(invalid);
      source.signal("router-activity", invalid);
      assert.equal(source.closed, true);
      assert.equal(h.render("rev-2").routerActivity, null);
      h.unmount();
    }
  }
});

test("malformed or regressed router frames clear the provisional graph and close", () => {
  for (const invalid of ["not-json-object", {}, liveFrame({ progress_sequence: 1 }), liveFrame({ run_nonce: "rev-1" })]) {
    const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
    const source = h.connections[0]; source.signal("router-activity", liveFrame());
    assert.notEqual(h.render("rev-2").routerActivity, null);
    source.signal("router-activity", invalid);
    assert.equal(source.closed, true);
    assert.equal(h.render("rev-2").routerActivity, null);
    h.unmount();
  }
  const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
  const source = h.connections[0]; source.signal("router-activity", liveFrame());
  source.listeners.get("router-activity")({ data: "{" });
  assert.equal(source.closed, true);
  assert.equal(h.render("rev-2").routerActivity, null);
  h.unmount();
});

test("stopping and restarting the same revision cannot revive provisional observations", () => {
  const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
  h.connections[0].signal("router-activity", liveFrame());
  assert.notEqual(h.render("rev-2").routerActivity, null);
  assert.equal(h.render("rev-2", [], false).routerActivity, null); h.effects();
  assert.equal(h.connections[0].closed, true);
  assert.equal(h.render("rev-2", [], true).routerActivity, null);
  h.effects();
  assert.equal(h.connections.length, 2);
  assert.equal(h.render("rev-2").routerActivity, null);
  h.connections[1].signal("router-activity", liveFrame());
  assert.notEqual(h.render("rev-2").routerActivity, null);
  h.unmount();
});

test("live hook preserves hydration seed and hides old-revision events before cleanup", () => {
  const h = hookHarness();
  assert.equal(h.render("rev-2", [event()]).events.length, 1);
  h.effects();
  const old = h.connections[0];
  old.message(event("rev-2", { seq: 2 }));
  old.signal("router-activity", liveFrame());
  assert.equal(h.render("rev-2", [event()]).events.length, 1);
  assert.equal(h.render("rev-2").routerActivity, null);
  h.hydrate();
  assert.equal(h.render("rev-2", [event()]).events.length, 2);
  assert.equal(h.render("rev-2").routerActivity.assignment_id, "assignment-id");
  assert.equal(h.render("rev-3", [event("rev-3")]).events.length, 1);
  assert.equal(h.render("rev-3").routerActivity, null);
  h.effects();
  assert.equal(old.closed, true);
  old.message(event("rev-2", { seq: 3 }));
  old.signal("router-activity", liveFrame({ progress_sequence: 3 }));
  const current = h.connections[1];
  current.message(event("rev-2", { seq: 4 }));
  current.onmessage({ data: "malformed" });
  current.message(event("rev-3", { seq: 2 }));
  assert.equal(h.render("rev-3", [event("rev-3")]).events.length, 2);
  assert.ok(h.render("rev-3").events.every(e => e.runNonce === "rev-3"));
  assert.equal(h.render("rev-3").routerActivity, null);
  h.unmount();
  assert.equal(current.closed, true);
  current.message(event("rev-3", { seq: 3 }));
  assert.equal(h.render("rev-3").events.length, 1);
});

test("live hook closes on terminal/revocation but not initial unavailable, and requires nonce", () => {
  for (const signal of ["done", "end", "router-activity-unavailable"]) {
    const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
    const source = h.connections[0];
    source.signal("unavailable"); assert.equal(source.closed, false);
    source.message(event()); assert.equal(h.render("rev-2").events.length, 1);
    source.signal("router-activity", liveFrame());
    assert.notEqual(h.render("rev-2").routerActivity, null);
    source.signal(signal); assert.equal(source.closed, true);
    source.message(event("rev-2", { seq: 2 })); assert.equal(h.render("rev-2").events.length, 1);
    source.signal("router-activity", liveFrame({ progress_sequence: 3 }));
    assert.equal(h.render("rev-2").routerActivity, null);
    h.unmount();
  }
  for (const [nonce, running] of [[null, true], ["", true], ["rev-2", false]]) {
    const h = hookHarness(); h.render(nonce, [], running); h.effects();
    assert.equal(h.connections.length, 0); h.unmount();
  }
});
