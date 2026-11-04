// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => typeof type === "function" ? type(props) : { type, props };
const jsxRuntime = { jsx, jsxs: jsx, Fragment: "fragment" };
function load(path, imports = {}, globals = {}) {
  const context = { exports: {}, ...globals, require(name) {
    if (name === "react/jsx-runtime") return jsxRuntime;
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const result = (status = "pass", ok = status !== "fail") => ({ ok,
  checks: [{ id: "budget", label: "Budget", status, detail: "Budget check detail" }] });
const tick = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function actions(fetch) {
  return load("app/tasks/[name]/launch-actions.ts", {
    "@/lib/config": { defaultNamespace: () => "workspace/a" },
    "@/lib/bff": { authenticatedBffFetch: fetch },
  });
}

test("stored validation uses the authenticated, encoded, uncached POST and preserves warnings", async () => {
  for (const status of ["pass", "warn", "fail"]) {
    let request;
    const api = actions(async (...args) => {
      request = args;
      return { ok: true, json: async () => result(status) };
    });
    const actual = await api.validateTask("mission/a");
    assert.equal(actual.ok, status !== "fail");
    assert.equal(actual.checks[0].status, status);
    assert.equal(request[0], "/api/namespaces/workspace%2Fa/tasks/mission%2Fa/validate");
    assert.equal(request[1].method, "POST");
    assert.equal(request[1].cache, "no-store");
  }
});

test("transport, HTTP, and unreadable responses fail closed without exposing upstream secrets", async () => {
  for (const fetch of [
    async () => { throw new Error("private credential"); },
    async () => ({ ok: false, status: 403 }),
    async () => ({ ok: true, json: async () => { throw new Error("private payload"); } }),
  ]) {
    const actual = await actions(fetch).validateTask("draft");
    assert.equal(actual.ok, false);
    assert.equal(actual.checks[0].status, "fail");
    assert.doesNotMatch(JSON.stringify(actual), /private/);
  }
});

test("launch preserves actionable validation reasons and never exposes unknown upstream errors", async () => {
  const message = "TypedPlanExecutionUnavailable: use fresh filesystem-read/write phases.";
  for (const [status, code] of [[400, "bad_request"], [409, "conflict"], [422, "rejected"]]) {
    let request;
    const actual = await actions(async (...args) => {
      request = args;
      return { ok: false, status, json: async () => ({ error: { code, message } }) };
    }).setLaunch("mission/a", true);
    assert.equal(actual.error, message);
    assert.equal(request[0], "/api/namespaces/workspace%2Fa/tasks/mission%2Fa/launch");
    assert.equal(request[1].body, '{"launch":true}');
  }
  for (const fetch of [
    async () => { throw new Error("private credential"); },
    async () => ({ ok: false, status: 500, json: async () => ({ error: { code: "bad_request", message: "private credential" } }) }),
    async () => ({ ok: false, status: 400, json: async () => ({ error: { code: "unknown", message: "private credential" } }) }),
    async () => ({ ok: false, status: 422, json: async () => { throw new Error("private credential"); } }),
    async () => ({ ok: false, status: 400, json: async () => null }),
  ]) {
    const actual = await actions(fetch).setLaunch("draft", true);
    assert.ok(actual.error);
    assert.doesNotMatch(actual.error, /private/);
  }
  assert.equal((await actions(async () => ({ ok: true })).setLaunch("draft", true)).error, null);
});

test("malformed, empty, coerced-status and inconsistent successful responses cannot enable launch", async () => {
  for (const payload of [null, [], {}, { ok: true, checks: [] },
    { ok: "true", checks: result().checks }, { ok: true, checks: [null] },
    { ok: true, checks: [{ ...result().checks[0], status: ["pass"] }] },
    { ok: true, checks: [{ ...result().checks[0], detail: null }] },
    result("fail", true), result("pass", false),
  ]) {
    const actual = await actions(async () => ({ ok: true, json: async () => payload })).validateTask("draft");
    assert.equal(actual.ok, false, JSON.stringify(payload));
  }
});

const statusModule = load("components/mission-status.tsx");
const validationChecks = load("components/validation-checks.tsx", {
  react: { useState: initial => [initial, () => {}] },
  "@/lib/budget-review": load("lib/budget-review.ts"),
});
const missionState = load("lib/mission-run-evidence.ts", {
  "./router-activity": load("lib/router-activity.ts"),
});
const presentation = load("lib/mission-presentation.ts", {
  "../components/mission-status": statusModule,
  "./mission-run-evidence": missionState,
  "./team-run-evidence": load("lib/team-run-evidence.ts", {
    "./mission-run-evidence": missionState,
    "./router-activity": load("lib/router-activity.ts"),
  }),
});
const draft = { name: "draft", namespace: "workspace", created_at: "2026-10-02T00:00:00Z",
  phase: "Ready", launched: false, envelope: { budget: { tokens: 20000 } },
  envelope_digest: "digest", observed_generation: 1, result: null, activity: [],
  assignment: null, current_run_nonce: null, execution_phase: null };
function text(node) {
  if (Array.isArray(node)) return node.map(text).join("");
  if (node && typeof node === "object") return text(node.props?.children);
  return node == null || typeof node === "boolean" ? "" : String(node);
}
function nodes(node, predicate) {
  if (Array.isArray(node)) return node.flatMap(child => nodes(child, predicate));
  if (!node || typeof node !== "object") return [];
  return [...(predicate(node) ? [node] : []), ...nodes(node.props?.children, predicate)];
}

test("budget failures explain the administrator path without changing readiness or offering a bypass", () => {
  const check = { id: "budget_contracts", status: "fail", label: "Operator budget contracts",
    detail: 'Role "benchmark-researcher": No current operator budget contract covers model "gpt-6.1-sol".' };
  const before = JSON.stringify(check);
  const tree = validationChecks.ValidationChecks({ checks: [check] });
  const content = text(tree);
  assert.ok(content.includes(check.detail));
  assert.match(content, /not a legal agreement, provider login, or token allowance/);
  assert.match(content, /blank shared token limit does not clear a finite role limit/);
  assert.match(content, /inferenceBudget\.contracts/);
  assert.match(content, /inferenceBudget\.catalogVersion/);
  assert.match(content, /no browser contract editor/);
  assert.match(content, /Do not remove an intended limit/);
  assert.equal(nodes(tree, node => node.type === "details").length, 1);
  assert.equal(nodes(tree, node => ["form", "input"].includes(node.type)).length, 0);
  assert.deepEqual(nodes(tree, node => node.type === "button").map(text), ["Copy support request"]);
  assert.match(content, /Spending setup needs attention/);
  assert.match(content, /This check has not started any work/);
  assert.match(content, /person who manages Kars/);
  assert.equal(nodes(tree, node => node.type === "details")[0].props.open, undefined);
  assert.equal(JSON.stringify(check), before);
  for (const changed of [{ ...check, status: "warn" }, { ...check, status: "pass" }, { ...check, id: "model" }]) {
    assert.doesNotMatch(text(validationChecks.ValidationChecks({ checks: [changed] })), /no browser contract editor/);
  }
});

// Run the production component with controllable hook commits and network replies.
function panel(initial = draft, { launchResponse, runResponse } = {}) {
  let task = initial, cursor = 0, tree, pending = false;
  const slots = [], effects = [], requests = [], launches = [], runs = [], transitions = [];
  let refreshes = 0, intervalId = 0;
  const intervals = new Map();
  const router = { refresh() { refreshes++; } };
  const react = {
    useState(initialValue) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = initialValue;
      return [slots[index], value => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }];
    },
    useRef(value) { const index = cursor++; return slots[index] ??= { current: value }; },
    useEffect(effect, deps) {
      const index = cursor++, old = slots[index];
      if (!old || deps.some((value, i) => !Object.is(value, old.deps[i]))) {
        effects.push(() => { old?.cleanup?.(); slots[index] = { deps, cleanup: effect() }; });
      }
    },
    useTransition() { return [pending, fn => {
      pending = true;
      transitions.push(Promise.resolve(fn()).finally(() => { pending = false; }));
    }]; },
  };
  const { ExecutionPanel } = load("app/tasks/[name]/execution-panel.tsx", {
    react, "next/navigation": { useRouter: () => router },
    "@/components/live-refresh": { LiveRefresh: ({ active }) => {
      intervals.clear();
      if (active) intervals.set(++intervalId, () => router.refresh());
      return null;
    } },
    "@/components/status-badge": { StatusBadge: ({ label }) => jsx("span", { children: label }) },
    "@/lib/mission-presentation": presentation,
    "@/components/validation-checks": validationChecks,
    "@/components/mission-qualification": load("components/mission-qualification.tsx", {
      "@/components/navigation-link": { default: props => jsx("a", props) },
    }),
    "@/lib/mission-run-evidence": missionState,
    "@/lib/run-mission-client": { runMissionClient: async (...args) => { runs.push(args); return runResponse ?? { error: null }; } },
    "./launch-actions": {
      validateTask: name => { const request = { name, ...deferred() }; requests.push(request); return request.promise; },
      setLaunch: async (...args) => { launches.push(args); return launchResponse ?? { error: null }; },
    },
  }, { window: {
    setTimeout() {},
    setInterval(callback) { intervals.set(++intervalId, callback); return intervalId; },
    clearInterval(id) { intervals.delete(id); },
  } });
  const harness = {
    requests, launches, runs,
    poll() { for (const callback of intervals.values()) callback(); },
    refreshes: () => refreshes,
    intervalCount: () => intervals.size,
    render(next = task) {
      task = next; cursor = 0; tree = ExecutionPanel({ task });
      while (effects.length) effects.shift()();
      return tree;
    },
    holdRefresh(value = true) { pending = value; harness.render(); },
    button(label) {
      const props = nodes(tree, n => n.type === "button" && text(n) === label)[0]?.props;
      if (!props) return undefined;
      return { ...props, onClick: (...args) => {
        const reply = props.onClick(...args);
        if (reply?.then) transitions.push(reply);
        return reply;
      } };
    },
    text: () => text(tree),
    async settle(index, value = result()) { requests[index].resolve(value); await tick(); harness.render(); },
    async finish() { await Promise.all(transitions); harness.render(); },
    unmount() { for (const slot of slots) slot?.cleanup?.(); },
  };
  harness.render();
  return harness;
}

test("permanently halted Missions offer a new Mission instead of impossible launch recovery", () => {
  for (const halted of ["Halted by administrator", ""]) {
    for (const launched of [false, true]) {
      const ui = panel({ ...draft, halted, launched });
      assert.equal(ui.requests.length, 0);
      assert.equal(ui.launches.length, 0);
      assert.equal(ui.runs.length, 0);
      assert.match(ui.text(), /cannot restart/);
      assert.match(ui.text(), /no launch checks to fix/);
      assert.match(ui.text(), /Start a new Mission/);
      assert.equal(ui.button("Launch"), undefined);
      assert.equal(ui.button("Recheck launch"), undefined);
      assert.equal(ui.button("Run again"), undefined);
      assert.equal(ui.intervalCount(), 0);
    }
  }
});

test("a completed reviewed run explains retained output instead of offering impossible relaunch", async () => {
  const ui = panel({ ...draft, execution_phase: "Idle" });
  await ui.settle(0, { ok: false, checks: [{ id: "execution_activation",
    label: "Stored execution package cannot be activated", status: "fail",
    detail: "CompletedReviewedRun: this run is complete and its runtime is stopped. Review retained output or request a new revision.",
  }] });
  assert.match(ui.text(), /runtime is stopped/);
  assert.match(ui.text(), /Request changes/);
  assert.match(ui.text(), /retained/);
  assert.doesNotMatch(ui.text(), /Launch blocked|resolve the failing checks|until the failing checks are resolved/);
  assert.equal(ui.button("Launch"), undefined);
  assert.equal(ui.button("Recheck launch"), undefined);
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.runs.length, 0);
  ui.unmount();
});

test("completion recovery requires the authoritative activation refusal and does not claim cleanup early", async () => {
  const completed = { id: "execution_activation", label: "Cannot activate", status: "fail",
    detail: "CompletedReviewedRun: recorded completion" };
  const stopping = panel({ ...draft, execution_phase: "Stopping" });
  await stopping.settle(0, { ok: false, checks: [completed] });
  assert.match(stopping.text(), /Sandbox shutdown was requested/);
  assert.doesNotMatch(stopping.text(), /runtime is stopped/);
  assert.equal(stopping.button("Launch"), undefined);
  stopping.unmount();
  for (const check of [{ ...completed, id: "budget" }, { ...completed, status: "warn" },
    { ...completed, detail: "Another activation failure" }]) {
    const ui = panel(draft);
    await ui.settle(0, { ok: false, checks: [check] });
    assert.ok(ui.button("Launch"));
    assert.match(ui.text(), /Launch blocked/);
    assert.doesNotMatch(ui.text(), /Completed revision/);
    ui.unmount();
  }
});

test("a Ready draft starts checking, blocks failed validation, and explicitly rechecks", async () => {
  const ui = panel();
  assert.equal(ui.requests.length, 1);
  assert.equal(ui.button("Launch").disabled, true);
  assert.match(ui.text(), /Checking this saved package/);
  assert.doesNotMatch(ui.text(), /Ready to launch/);
  await ui.settle(0, result("fail"));
  assert.equal(ui.button("Launch").disabled, true);
  assert.match(ui.text(), /Launch blocked/);
  ui.button("Recheck launch").onClick(); ui.render();
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(1, result("warn"));
  assert.equal(ui.button("Launch").disabled, false);
  assert.match(ui.text(), /completed with warnings/);
});

test("only a server-identified saved keyless draft offers setup recovery through validated Launch", async () => {
  for (const pending of [undefined, false, true]) {
    const ui = panel({ ...draft, phase: "Degraded", execution_phase: "Idle", credential_setup_pending: pending });
    assert.equal(ui.button("Launch").disabled, true);
    await ui.settle(0);
    assert.equal(ui.button("Launch").disabled, pending !== true);
    if (pending === true) {
      assert.match(ui.text(), /verify the existing empty source/);
      ui.button("Launch").onClick();
      assert.equal(ui.launches.length, 0);
      await ui.settle(1); await ui.finish();
      assert.deepEqual(ui.launches, [["draft", true]]);
    }
  }
});

test("saved keyless setup cannot bypass failed validation or retain stale recovery eligibility", async () => {
  const saved = { ...draft, phase: "Degraded", execution_phase: "Idle", credential_setup_pending: true };
  const ui = panel(saved);
  await ui.settle(0, result("fail"));
  assert.equal(ui.button("Launch").disabled, true);
  ui.button("Recheck launch").onClick(); ui.render();
  await ui.settle(1);
  assert.equal(ui.button("Launch").disabled, false);
  ui.render({ ...saved, credential_setup_pending: false });
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(2);
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.launches.length, 0);
});

test("a rejected saved-source recovery remains visible and does not claim launch", async () => {
  const ui = panel({ ...draft, phase: "Degraded", credential_setup_pending: true },
    { launchResponse: { error: "Draft credential source is not acknowledged; no source was created or changed" } });
  await ui.settle(0);
  ui.button("Launch").onClick();
  await ui.settle(1); await ui.finish();
  assert.match(ui.text(), /Draft credential source is not acknowledged/);
  assert.ok(ui.button("Launch"));
  assert.equal(ui.button("Stop"), undefined);
});

test("validation puts every failure first and retains warnings and passing details behind native disclosures", () => {
  const checks = [
    { id: "dns", status: "pass", label: "Host resolves", detail: "Not a tool execution proof" },
    { id: "budget", status: "fail", label: "Budget contract missing", detail: "Enroll supported bounds" },
    { id: "runtime", status: "warn", label: "Runtime unqualified", detail: "No retained execution proof" },
  ];
  const tree = validationChecks.ValidationChecks({ checks });
  assert.ok(text(tree).indexOf("Budget contract missing") < text(tree).indexOf("Runtime unqualified"));
  const disclosures = nodes(tree, node => node.type === "details");
  assert.equal(disclosures.length, 2);
  assert.ok(disclosures.every(node => !node.props.open));
  assert.match(text(disclosures[0]), /1 warning to review.*No retained execution proof/);
  assert.match(text(disclosures[1]), /1 check passed.*Not a tool execution proof/);
  assert.ok(disclosures.every(node => !text(node).includes("Budget contract missing")));
});

test("a held reviewed run never claims it is running or keeps polling for a missing root assignment", () => {
  const ui = panel({ ...draft, launched: true, current_run_nonce: "held",
    execution_phase: "Degraded", execution_detail: "A reviewed phase failed; synthesis prohibited." });
  assert.match(ui.text(), /Execution blocked/);
  assert.match(ui.text(), /A reviewed phase failed; synthesis prohibited/);
  assert.doesNotMatch(ui.text(), /Running the mission|waiting for readiness or dispatch/);
  assert.equal(ui.intervalCount(), 0);
  assert.equal(ui.button("Stop").disabled, false);
});

test("execution blockage uses its diagnostic, never a successful admission condition", () => {
  assert.equal(statusModule.missionBlockReason({
    phase: "Ready", execution_phase: "Degraded",
    status_message: "trust envelope validated and digested", execution_detail: "Missing required handoff.",
  }), "Missing required handoff.");
  assert.equal(statusModule.missionBlockReason({
    phase: "Ready", execution_phase: "Degraded", status_message: "trust envelope validated and digested",
  }), "Execution is blocked; no diagnostic detail has been recorded.");
  assert.equal(statusModule.missionBlockReason({
    phase: "Degraded", execution_phase: "Idle", status_message: "Authority rejected.",
  }), "Authority rejected.");
  assert.equal(statusModule.missionBlockReason({ phase: "Ready", execution_phase: "Running" }), null);
});

test("a halted mission's unfinished nonce is history, not an active request or polling authority", () => {
  const ui = panel({ ...draft, current_run_nonce: "stopped-run",
    assignment: { task_id: "stopped-run", state: "Running", completed_at: null } });
  assert.doesNotMatch(ui.text(), /awaiting a current terminal result/);
  assert.match(ui.text(), /Review the trust envelope/);
  assert.equal(ui.intervalCount(), 0);
  assert.equal(ui.requests.length, 1);
});

test("a readiness wait does not blame or recommend replacing a harness without evidence", () => {
  const ui = panel({ ...draft, launched: true });
  assert.match(ui.text(), /waiting for readiness or dispatch/);
  assert.doesNotMatch(ui.text(), /chat-gateway|re-compose|Hermes|OpenClaw/);
});

test("admission must also be Ready and unrelated activity does not retrigger validation", async () => {
  const ui = panel({ ...draft, phase: "Pending" });
  await ui.settle(0);
  assert.equal(ui.button("Launch").disabled, true);
  ui.render({ ...draft, phase: "Pending", activity: [{ message: "updated" }] });
  assert.equal(ui.requests.length, 1);
  ui.render(draft);
  assert.equal(ui.requests.length, 2);
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(1);
  assert.equal(ui.button("Launch").disabled, false);
});

test("replaced package validation cannot enable Launch with a stale success", async () => {
  const ui = panel();
  ui.render({ ...draft, observed_generation: 2, envelope_digest: "changed" });
  await ui.settle(0);
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(1, result("fail"));
  assert.equal(ui.button("Launch").disabled, true);
});

test("Launch revalidates and does not mutate when the new validation fails", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick(); ui.render();
  assert.equal(ui.launches.length, 0);
  await ui.settle(1, result("fail")); await ui.finish();
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Launch").disabled, true);
});

test("validated Launch uses the original action only after the fresh check passes", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick();
  await ui.settle(1); await ui.finish();
  assert.deepEqual(ui.launches, [["draft", true]]);
  assert.ok(ui.button("Stop"));
});

test("a launch still awaiting its response never labels the stop control as stopping", async () => {
  const response = deferred();
  const ui = panel(draft, { launchResponse: response.promise });
  await ui.settle(0);
  ui.button("Launch").onClick();
  await ui.settle(1);
  ui.render({ ...draft, launched: true, observed_generation: 2 });
  assert.equal(ui.button("Stop")?.disabled, true);
  assert.doesNotMatch(ui.text(), /Stopping/);
  assert.deepEqual(ui.launches, [["draft", true]]);
  response.resolve({ error: null });
  await ui.finish();
});

test("a stop still awaiting its response never labels the launch control as launching", async () => {
  const response = deferred();
  const ui = panel({ ...draft, launched: true }, { launchResponse: response.promise });
  ui.button("Stop").onClick();
  ui.render({ ...draft, launched: false, observed_generation: 2 });
  assert.equal(ui.button("Launch")?.disabled, true);
  assert.doesNotMatch(ui.text(), /Validating \/ launching/);
  assert.deepEqual(ui.launches, [["draft", false]]);
  response.resolve({ error: null });
  await ui.finish();
});

test("an acknowledged launch keeps Stop usable while the page refresh remains pending", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick();
  await ui.settle(1); await ui.finish();
  ui.holdRefresh();
  assert.equal(ui.button("Stop").disabled, false);
  await ui.button("Stop").onClick();
  await ui.finish();
  assert.deepEqual(ui.launches, [["draft", true], ["draft", false]]);
  assert.equal(ui.requests.length, 3, "Stop only triggers the subsequent idle validation, not action preflight");
  ui.unmount();
});

test("an in-flight lifecycle mutation is not submitted twice before a render", async () => {
  const response = deferred();
  const ui = panel({ ...draft, launched: true }, { launchResponse: response.promise });
  const stop = ui.button("Stop");
  stop.onClick(); stop.onClick();
  assert.equal(ui.launches.length, 1);
  response.resolve({ error: "Unavailable; inspect status before retrying" });
  await ui.finish();
  assert.equal(ui.button("Stop").disabled, false);
  ui.unmount();
});

test("Stop refreshes through controller acknowledgement, then releases the optimistic launch override", async () => {
  const initial = { ...draft, phase: "Degraded", launched: true, observed_generation: 2 };
  const ui = panel(initial);
  ui.button("Stop").onClick(); await ui.finish();
  ui.render({ ...initial, launched: false });
  assert.equal(ui.intervalCount(), 1);
  ui.poll();
  assert.equal(ui.refreshes(), 1);
  ui.render({ ...draft, observed_generation: 3 });
  ui.render({ ...draft, observed_generation: 3 });
  assert.equal(ui.intervalCount(), 0);
  ui.render({ ...draft, launched: true, observed_generation: 4 });
  assert.ok(ui.button("Stop"), "later server state must not be masked by the old stop");
  ui.unmount();
});

test("Stop keeps refreshing after acknowledgement until the sandbox and execution drain", async () => {
  const initial = { ...draft, launched: true, observed_generation: 2, execution_phase: "Running",
    sandbox: "mission-sandbox", current_run_nonce: "unfinished-run",
    execution_detail: "The governed agent is running in its sandbox." };
  const ui = panel(initial);
  ui.button("Stop").onClick(); await ui.finish();
  assert.match(ui.text(), /Stopping/);
  assert.doesNotMatch(ui.text(), /governed agent is running/);
  for (const state of [
    { execution_phase: "Running", sandbox: "mission-sandbox" },
    { execution_phase: "Stopping", sandbox: "mission-sandbox", execution_detail: "Removing sandbox" },
    { execution_phase: "Idle", sandbox: "mission-sandbox" },
  ]) {
    const draining = { ...initial, ...state, launched: false, observed_generation: 3 };
    ui.render(draining); ui.render(draining);
    await ui.settle(ui.requests.length - 1);
    assert.equal(ui.intervalCount(), 1);
    assert.equal(ui.button("Launch").disabled, true);
    await ui.button("Launch").onClick();
    assert.equal(ui.launches.length, 1, "draining cannot submit another launch even through the handler");
    assert.match(ui.text(), /shutdown is still being confirmed/);
    assert.doesNotMatch(ui.text(), /governed agent is running/);
  }
  const stopped = { ...initial, launched: false, observed_generation: 3,
    execution_phase: "Idle", sandbox: null, execution_detail: "Runtime stopped" };
  ui.render(stopped); ui.render(stopped);
  assert.equal(ui.intervalCount(), 0);
  assert.match(ui.text(), /Stopped/);
  assert.doesNotMatch(ui.text(), /mission-sandbox|shutdown is still being confirmed/);
  ui.unmount();
});

test("package change while action validation is in flight discards the launch attempt", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick();
  ui.render({ ...draft, envelope_digest: "new" });
  await ui.settle(1); await ui.finish();
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(2);
  assert.equal(ui.button("Launch").disabled, false);
});

test("initial and action transport failures are recoverable and keep Launch disabled", async () => {
  const ui = panel();
  ui.requests[0].reject(new Error("offline")); await tick(); ui.render();
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.button("Recheck launch").disabled, false);
  ui.button("Recheck launch").onClick(); ui.render(); await ui.settle(1);
  ui.button("Launch").onClick(); ui.requests[2].reject(new Error("action offline")); await ui.finish();
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.button("Recheck launch").disabled, false);
});

test("reviewed execution changes presentation without enabling principal dispatch", () => {
  const task = { ...draft, launched: true, execution_phase: "Launching", current_run_nonce: "root-run",
    qualification_run: { approved_at: "now" }, run_requested: true,
    reviewed_execution: { run_nonce: "root-run", issue: null, phases: [{
      role: "research", phase: "research", task_name: "child", task_uid: "child-uid",
      run_nonce: "child-run", state: "active", result: null,
    }] } };
  const ui = panel(task);
  assert.match(ui.text(), /Running the mission/);
  assert.doesNotMatch(ui.text(), /Deploying|Coming online|waiting for readiness/);
  assert.equal(ui.button("Resume / await run").disabled, true);
  assert.equal(ui.requests.length, 0);
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.runs.length, 0);
  ui.render({ ...task, reviewed_execution: { ...task.reviewed_execution, phases: [{
    ...task.reviewed_execution.phases[0], state: "failed",
  }] } });
  assert.match(ui.text(), /Run failed/);
  assert.doesNotMatch(ui.text(), /Running the mission|Coming online/);
});

test("Stop and delivered reruns remain independent of draft validation", async () => {
  const running = panel({ ...draft, launched: true, execution_phase: "Running" });
  assert.equal(running.requests.length, 0);
  running.button("Stop").onClick(); await running.finish();
  assert.deepEqual(running.launches, [["draft", false]]);
  const delivered = panel({ ...draft, launched: true, execution_phase: "Running",
    current_run_nonce: "run-1",
    result: { assignment_nonce: "run-1", status: "ok", reviewable: true, finished_at: "now", output: "Useful work" } });
  assert.match(delivered.text(), /Delivered/);
  assert.equal(delivered.requests.length, 0);
  delivered.button("Run again").onClick(); await delivered.finish();
  assert.deepEqual(delivered.runs, [["workspace", "draft"]]);
  assert.match(delivered.text(), /run is requested and awaiting a current terminal result/);
});

test("current terminal evidence ends pending status even without a matching live assignment", () => {
  const task = { ...draft, launched: true, execution_phase: "Running", current_run_nonce: "run-2",
    assignment: { task_id: "old-run", state: "Running", completed_at: null },
    result: { assignment_nonce: "run-2", status: "ok", reviewable: true, output: "Revised work", finished_at: "later" } };
  const ui = panel(task);
  assert.match(ui.text(), /Delivered/);
  assert.doesNotMatch(ui.text(), /awaiting a current terminal result/);
  ui.render({ ...task, current_run_nonce: "run-3" });
  assert.match(ui.text(), /awaiting a current terminal result/);
  ui.render({ ...task, launched: false, execution_phase: null, assignment: null });
  assert.match(ui.text(), /Delivered/);
  assert.doesNotMatch(ui.text(), /awaiting a current terminal result/);
});

const qualification = { approved_by: "Operator", approved_by_sub: "subject-1",
  approved_at: "2026-10-16T06:00:00Z", expires_at: "2026-10-16T06:45:00Z", package_sha256: "a".repeat(64) };

test("an unused qualification draft can launch after server validation; expiry rechecks fail closed", async () => {
  const ui = panel({ ...draft, qualification_run: qualification, run_requested: false });
  await ui.settle(0, result("warn"));
  assert.equal(ui.button("Launch").disabled, false);
  ui.button("Launch").onClick();
  await ui.settle(1, { ok: false, checks: [{ id: "qualification", label: "Admission expired",
    status: "fail", detail: "Create a new explicitly reviewed qualification Mission." }] });
  await ui.finish();
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.launches.length, 0);
  assert.match(ui.text(), /Admission expired/);
});

test("an unused qualification draft preserves the first Launch path without a client expiry timer", async () => {
  const ui = panel({ ...draft, qualification_run: qualification, run_requested: false });
  await ui.settle(0);
  assert.equal(ui.button("Launch").disabled, false);
  ui.button("Launch").onClick();
  await ui.settle(1); await ui.finish();
  assert.deepEqual(ui.launches, [["draft", true]]);
  assert.ok(ui.button("Stop"));
});

test("qualification evidence changes invalidate saved-task preflight", async () => {
  const ui = panel(draft);
  await ui.settle(0);
  ui.render({ ...draft, qualification_run: qualification });
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.requests.length, 2);
  await ui.settle(1);
  assert.equal(ui.button("Launch").disabled, false);
});

test("an expired pending qualification request can resume or await its existing nonce", async () => {
  const expired = { ...qualification, expires_at: "2000-01-01T00:45:00Z" };
  for (const assignment of [null,
    { task_id: "run-1", state: "Running", completed_at: null },
    { task_id: "run-1", state: "Completed", completed_at: "now" },
  ]) {
    const pending = { ...draft, qualification_run: expired, run_requested: true,
      current_run_nonce: "run-1", launched: true, execution_phase: "Running", assignment };
    const ui = panel(pending);
    assert.equal(ui.requests.length, 0, "awaiting admitted work does not rerun draft admission");
    assert.equal(ui.button("Resume / await run").disabled, false);
    assert.equal(ui.button("Run again"), undefined);
    ui.button("Resume / await run").onClick(); await ui.finish();
    assert.deepEqual(ui.runs, [["workspace", "draft"]]);
    assert.equal(ui.launches.length, 0);
    assert.match(ui.text(), /awaiting a current terminal result/);
    assert.equal(ui.intervalCount(), 1);
    ui.render({ ...pending, result: { assignment_nonce: "run-1", status: "ok",
      finished_at: "2026-10-16T07:00:00Z", output: "Retained output" } });
    assert.equal(ui.button("Resume / await run"), undefined);
    assert.equal(ui.button("Run again"), undefined);
    assert.equal(ui.intervalCount(), 0);
    assert.equal(ui.button("Stop sandbox").disabled, false);
    ui.unmount();
  }
});

test("qualification resume surfaces server completion rejection without launching a new attempt", async () => {
  const ui = panel({ ...draft, qualification_run: qualification, run_requested: true,
    current_run_nonce: "run-1", launched: true, execution_phase: "Running" }, {
    runResponse: { error: "This qualification run has completed; create a fresh reviewed Mission for another attempt" },
  });
  ui.button("Resume / await run").onClick(); await ui.finish();
  assert.match(ui.text(), /qualification run has completed/);
  assert.equal(ui.runs.length, 1);
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Run again"), undefined);
  ui.unmount();
});

test("a stopped pending qualification request keeps server-validated Launch after expiry", async () => {
  const ui = panel({ ...draft, qualification_run: { ...qualification, expires_at: "2000-01-01T00:45:00Z" },
    run_requested: true, current_run_nonce: "run-1" });
  await ui.settle(0);
  assert.equal(ui.button("Launch").disabled, false);
  ui.button("Launch").onClick();
  assert.equal(ui.launches.length, 0);
  await ui.settle(1); await ui.finish();
  assert.deepEqual(ui.launches, [["draft", true]]);
});

test("qualification resume requires an unfinished durable nonce, never merely a missing result", async () => {
  for (const state of [
    { current_run_nonce: null },
    { current_run_nonce: " " },
    ...["ok", "error", "failed", "rejected"].map(status => ({ current_run_nonce: "run-1",
      result: { assignment_nonce: "run-1", status, finished_at: "now", output: "Recorded outcome" } })),
  ]) {
    const task = { ...draft, qualification_run: qualification, run_requested: true, ...state };
    const stopped = panel(task);
    await stopped.settle(0);
    assert.equal(stopped.button("Launch").disabled, true);
    stopped.button("Launch").onClick();
    assert.equal(stopped.launches.length, 0);
    const launched = panel({ ...task, launched: true, execution_phase: "Running" });
    assert.equal(launched.button("Resume / await run"), undefined);
    assert.equal(launched.button("Run again"), undefined);
    assert.equal(launched.runs.length, 0);
    stopped.unmount(); launched.unmount();
  }
});

test("a completed qualification Mission cannot relaunch or rerun, but Stop remains available after expiry", async () => {
  const used = { ...draft, qualification_run: qualification, run_requested: true, current_run_nonce: "run-1",
    result: { assignment_nonce: "run-1", status: "ok", finished_at: "now", output: "Useful work" } };
  const stopped = panel(used);
  await stopped.settle(0);
  assert.equal(stopped.button("Launch").disabled, true);
  stopped.button("Launch").onClick();
  assert.equal(stopped.launches.length, 0);
  const delivered = panel({ ...used, launched: true, execution_phase: "Running" });
  assert.equal(delivered.button("Run again"), undefined);
  assert.match(delivered.text(), /create a new Mission/);
  assert.equal(delivered.button("Stop sandbox").disabled, false);
  delivered.button("Stop sandbox").onClick(); await delivered.finish();
  assert.deepEqual(delivered.launches, [["draft", false]]);
  assert.equal(delivered.runs.length, 0);
});

test("draft naming does not override blocked, failed, delivered or active execution precedence", () => {
  assert.equal(text(statusModule.MissionStatusBadge({ status: "drafting" })), "Draft");
  assert.equal(statusModule.missionStatus("Ready", "Running", { failed: true }), "failed");
  assert.equal(statusModule.missionStatus("Degraded", "Running", { delivered: true }), "blocked");
  assert.equal(statusModule.missionStatus("Ready", "Running", { delivered: true }), "done");
  assert.equal(statusModule.missionStatus("Ready", "Running"), "running");
});
