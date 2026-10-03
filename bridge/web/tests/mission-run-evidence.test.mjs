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
const evidence = load("../src/lib/mission-run-evidence.ts");
const { missionRunState, activityForRevision, mergeRevisionActivity, revisionArtifactUrl, revisionStreamUrl } = evidence;
const event = (nonce = "rev-2", extra = {}) => ({
  kind: "round", runNonce: nonce, round: 1, seq: 1, ms: 30,
  ts: "2026-10-03T06:47:45Z", prompt_tokens: 6940, completion_tokens: 485,
  total_tokens: 7425, tool_calls: 1, finish_reason: "stop", ...extra,
});
const result = (extra = {}) => ({ assignment_nonce: "rev-2", status: "ok", output: "Useful briefing", reviewable: true, blocked: null, ...extra });

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

function hookHarness() {
  const slots = [], pending = [], frames = new Map(), connections = [];
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
      if (!same(old?.deps, deps)) pending.push(() => {
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
    signal(name) { this.listeners.get(name)?.({}); }
  }
  const { useLiveTrace } = load("../src/components/use-live-trace.ts", {
    require: name => {
      if (name === "react") return react;
      if (name === "@/lib/mission-run-evidence") return evidence;
      throw new Error(`Unexpected import: ${name}`);
    }, EventSource,
    requestAnimationFrame: callback => { frames.set(++nextFrame, callback); return nextFrame; },
    cancelAnimationFrame: id => frames.delete(id),
  });
  return {
    connections,
    render: function useRender(nonce, seed = [], running = true) { cursor = 0; return useLiveTrace("ns", "task", running, seed, nonce); },
    effects() { for (const callback of pending.splice(0)) callback(); },
    hydrate() { for (const callback of frames.values()) callback(); frames.clear(); },
    unmount() { for (const slot of slots) slot?.cleanup?.(); },
  };
}

test("live hook preserves hydration seed and hides old-revision events before cleanup", () => {
  const h = hookHarness();
  assert.equal(h.render("rev-2", [event()]).length, 1);
  h.effects();
  const old = h.connections[0];
  old.message(event("rev-2", { seq: 2 }));
  assert.equal(h.render("rev-2", [event()]).length, 1);
  h.hydrate();
  assert.equal(h.render("rev-2", [event()]).length, 2);
  assert.equal(h.render("rev-3", [event("rev-3")]).length, 1);
  h.effects();
  assert.equal(old.closed, true);
  old.message(event("rev-2", { seq: 3 }));
  const current = h.connections[1];
  current.message(event("rev-2", { seq: 4 }));
  current.onmessage({ data: "malformed" });
  current.message(event("rev-3", { seq: 2 }));
  assert.equal(h.render("rev-3", [event("rev-3")]).length, 2);
  assert.ok(h.render("rev-3").every(e => e.runNonce === "rev-3"));
  h.unmount();
  assert.equal(current.closed, true);
  current.message(event("rev-3", { seq: 3 }));
  assert.equal(h.render("rev-3").length, 1);
});

test("live hook closes on terminal/end but not unavailable, and never opens without nonce", () => {
  for (const signal of ["done", "end"]) {
    const h = hookHarness(); h.render("rev-2"); h.effects(); h.hydrate();
    const source = h.connections[0];
    source.signal("unavailable"); assert.equal(source.closed, false);
    source.message(event()); assert.equal(h.render("rev-2").length, 1);
    source.signal(signal); assert.equal(source.closed, true);
    source.message(event("rev-2", { seq: 2 })); assert.equal(h.render("rev-2").length, 1);
    h.unmount();
  }
  for (const [nonce, running] of [[null, true], ["", true], ["rev-2", false]]) {
    const h = hookHarness(); h.render(nonce, [], running); h.effects();
    assert.equal(h.connections.length, 0); h.unmount();
  }
});
