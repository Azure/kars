// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

const require = createRequire(import.meta.url);
const ts = require("typescript");
function load(path, imports = {}, globals = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: name => name in imports ? imports[name] : require(name), ...globals });
  return exports;
}

function refreshHarness(active, visible = true) {
  const window = new EventTarget();
  const document = Object.assign(new EventTarget(), { visibilityState: visible ? "visible" : "hidden" });
  const intervals = new Map(), timeouts = new Map(), instances = new Set();
  let next = 0, refreshes = 0, navigationPending = false, current;
  const router = { refresh: () => { refreshes++; } };
  const { LiveRefresh } = load("../src/components/live-refresh.tsx", {
    react: { ...React,
      useRef(value) { const index = current.cursor++; return current.slots[index] ??= { current: value }; },
      useTransition() { return [current.pending, current.start]; },
      useEffect(effect, deps) {
        const instance = current, index = instance.cursor++, old = instance.slots[index];
        if (!old || !deps || deps.some((value, i) => !Object.is(value, old.deps[i]))) {
          instance.effects.push(() => {
            old?.cleanup?.(); instance.slots[index] = { deps, cleanup: effect() };
          });
        }
      },
    },
    "next/navigation": { useRouter: () => router },
    "@/components/navigation-link": { hasPendingNavigation: () => navigationPending },
  }, {
    window, document,
    setInterval: fn => { intervals.set(++next, fn); return next; },
    clearInterval: id => intervals.delete(id),
    setTimeout: fn => { timeouts.set(++next, fn); return next; },
    clearTimeout: id => timeouts.delete(id),
  });
  function render(instance) {
    current = instance; instance.cursor = 0;
    LiveRefresh({ active: instance.active });
    while (instance.effects.length) instance.effects.shift()();
  }
  function mount(active) {
    const instance = { active, cursor: 0, pending: false, slots: [], effects: [] };
    instance.start = fn => { instance.pending = true; fn(); };
    instances.add(instance); render(instance);
    return { cleanup() {
      for (const slot of instance.slots) slot?.cleanup?.();
      instances.delete(instance);
    } };
  }
  function complete() {
    for (const instance of instances) { instance.pending = false; render(instance); }
  }
  function coolDown() {
    for (const [id, fn] of timeouts) { timeouts.delete(id); fn(); }
  }
  mount(active);
  return {
    window, document, intervals, timeouts, mount, complete, coolDown,
    count: () => refreshes,
    navigate: pending => { navigationPending = pending; },
    settle: () => { complete(); coolDown(); },
    poll: () => {
      for (const fn of intervals.values()) fn();
      for (const instance of instances) render(instance);
    },
    cleanup: () => {
      for (const instance of instances) for (const slot of instance.slots) slot?.cleanup?.();
      instances.clear();
    },
  };
}

test("idle pages refresh on return without continuous polling", () => {
  const h = refreshHarness(false);
  assert.equal(h.count(), 0);
  assert.equal(h.intervals.size, 0);
  h.window.dispatchEvent(new Event("focus"));
  h.document.dispatchEvent(new Event("visibilitychange"));
  assert.equal(h.count(), 1, "focus and visibility events are coalesced");
  h.settle();
  h.window.dispatchEvent(new Event("focus"));
  assert.equal(h.count(), 2);
  h.cleanup();
});

test("active and inactive hidden pages wait until visible", () => {
  for (const active of [true, false]) {
    const h = refreshHarness(active, false);
    h.poll();
    h.window.dispatchEvent(new Event("focus"));
    assert.equal(h.count(), 0);
    h.document.visibilityState = "visible";
    h.document.dispatchEvent(new Event("visibilitychange"));
    assert.equal(h.count(), 1);
    h.cleanup();
  }
});

test("active polling preserves immediate refresh and throttles duplicate signals", () => {
  const h = refreshHarness(true);
  assert.equal(h.count(), 1);
  h.poll();
  h.window.dispatchEvent(new Event("focus"));
  assert.equal(h.count(), 1);
  h.settle();
  h.poll();
  assert.equal(h.count(), 2);
  h.cleanup();
});

test("slow server refreshes remain single-flight after the timer cooldown expires", () => {
  const h = refreshHarness(true);
  h.coolDown();
  for (let i = 0; i < 8; i++) {
    h.poll();
    h.window.dispatchEvent(new Event("focus"));
    h.document.dispatchEvent(new Event("visibilitychange"));
  }
  assert.equal(h.count(), 1, "elapsed time is not refresh completion");
  h.complete(); h.poll();
  assert.equal(h.count(), 2);
  h.cleanup();
});

test("lifecycle and general polling share one outstanding refresh", () => {
  const h = refreshHarness(true);
  const second = h.mount(true);
  h.coolDown(); h.poll();
  assert.equal(h.count(), 1);
  h.complete(); h.poll();
  assert.equal(h.count(), 2);
  second.cleanup();
  h.coolDown(); h.poll();
  assert.equal(h.count(), 2, "unmounting a non-owner cannot release another refresh");
  h.complete(); h.poll();
  assert.equal(h.count(), 3);
  h.cleanup();
});

test("cleanup removes polling, cooldowns, and both event listeners", () => {
  const h = refreshHarness(true);
  assert.equal(h.timeouts.size, 1);
  h.cleanup();
  assert.equal(h.intervals.size, 0);
  assert.equal(h.timeouts.size, 0);
  h.window.dispatchEvent(new Event("focus"));
  h.document.dispatchEvent(new Event("visibilitychange"));
  assert.equal(h.count(), 1);
});

test("pending navigation suppresses polling and focus refresh until completion or cancellation", () => {
  for (const active of [true, false]) {
    const h = refreshHarness(active);
    h.settle();
    const before = h.count();
    h.navigate(true);
    h.poll();
    h.window.dispatchEvent(new Event("focus"));
    h.document.dispatchEvent(new Event("visibilitychange"));
    assert.equal(h.count(), before);
    h.navigate(false);
    h.window.dispatchEvent(new Event("focus"));
    assert.equal(h.count(), before + 1);
    h.cleanup();
  }
});

test("link transitions independently release their refresh suppression on cleanup", () => {
  const cleanups = [];
  const navigation = load("../src/components/navigation-link.tsx", {
    react: { ...React, useLayoutEffect: fn => { cleanups.push(fn()); } },
    "next/link": { default: () => null, useLinkStatus: () => ({ pending: false }) },
  });
  assert.equal(navigation.hasPendingNavigation(), false);
  navigation.usePendingNavigation(false);
  assert.equal(navigation.hasPendingNavigation(), false);
  navigation.usePendingNavigation(true);
  navigation.usePendingNavigation(true);
  assert.equal(navigation.hasPendingNavigation(), true);
  cleanups[1]();
  assert.equal(navigation.hasPendingNavigation(), true);
  cleanups[2]();
  assert.equal(navigation.hasPendingNavigation(), false);
  cleanups[2]();
  assert.equal(navigation.hasPendingNavigation(), false);
});

test("navigation links preserve Next props and expose pending state without disabling the link", () => {
  let pending = false;
  const navigation = load("../src/components/navigation-link.tsx", {
    react: { ...React, useLayoutEffect: () => {} },
    "next/link": { default: () => null, useLinkStatus: () => ({ pending }) },
  });
  const onNavigate = () => {};
  const ref = { current: null };
  const props = { href: "/console/insights", prefetch: false, replace: true,
    scroll: false, target: "_self", ref, onNavigate, "aria-current": "page",
    className: "existing-style", children: "Insights" };
  const element = navigation.default(props);
  for (const [key, value] of Object.entries(props)) {
    if (!["children", "className"].includes(key)) assert.equal(element.props[key], value);
  }
  assert.match(element.props.className, /existing-style/);
  assert.match(element.props.className, /cursor-progress/);
  assert.equal(element.props.children[0], "Insights");
  const Status = element.props.children[1].type;
  assert.equal(Status(), null);
  pending = true;
  const html = renderToStaticMarkup(Status());
  assert.match(html, /role="status"/);
  assert.match(html, /Opening page/);
  assert.doesNotMatch(html, /disabled/);
});

test("application links share navigation tracking rather than bypassing refresh suppression", () => {
  const root = new URL("../src/", import.meta.url);
  const direct = readdirSync(root, { recursive: true }).filter(name =>
    /\.(tsx?|jsx?)$/.test(name) &&
    /from ["']next\/link["']/.test(readFileSync(new URL(name, root), "utf8")));
  assert.deepEqual(direct, ["components/navigation-link.tsx"]);
});

test("Console and Workspace sidebars do not prefetch their live-data destinations", () => {
  for (const [file, name, count] of [
    ["console-nav", "ConsoleNav", 13], ["workspace-nav", "WorkspaceNav", 8],
  ]) {
    const component = load(`../src/components/${file}.tsx`, {
      "next/navigation": { usePathname: () => "/console" },
      "@/components/navigation-link": { default: ({ href, children, prefetch }) =>
        React.createElement("a", { href, "data-prefetch": String(prefetch) }, children) },
    })[name];
    const html = renderToStaticMarkup(React.createElement(component));
    assert.equal((html.match(/data-prefetch="false"/g) ?? []).length, count);
    assert.doesNotMatch(html, /data-prefetch="undefined"|data-prefetch="true"/);
  }
});

const details = load("../src/components/approval-detail.tsx");
const phases = load("../src/components/approval-phase-badge.tsx");
const decision = () => React.createElement("button", {}, "Act on approval");
const { TaskApprovalsPanel } = load("../src/app/tasks/[name]/task-approvals-panel.tsx", {
  "@/components/approval-detail": details,
  "@/components/approval-decision": { ApprovalDecision: decision },
  "@/components/approval-phase-badge": phases,
  "@/lib/types": { TIER_LABELS: {} },
});
const approval = (phase, actionable) => ({
  name: phase, task: "task", team: null, milestone: null, action_kind: "checkpoint",
  phase, actionable, summary: `${phase} request`, decider: actionable ? null : "reviewer",
  decided_at: null, requested_tier: null, resource_version: "9", bound_envelope_digest: null,
});

test("recorded decisions are visibly nonterminal and cannot be submitted again", () => {
  const html = renderToStaticMarkup(React.createElement(TaskApprovalsPanel, {
    approvals: [approval("DecisionRecorded", false)], decider: "reviewer", authWired: true,
  }));
  assert.match(html, /Decision recorded by reviewer/);
  assert.match(html, /Awaiting controller reconciliation; not a terminal outcome yet/);
  assert.doesNotMatch(html, /<button|Approved|Denied|1 awaiting/);
});

test("undecided approvals still expose controls and terminal badges remain unchanged", () => {
  const html = renderToStaticMarkup(React.createElement(TaskApprovalsPanel, {
    approvals: [approval("Pending", true)], decider: "reviewer", authWired: true,
  }));
  assert.match(html, /<button/);
  assert.match(html, /1 awaiting/);
  for (const phase of ["Approved", "Denied", "Expired", "Stale"]) {
    assert.equal(phases.approvalPhaseLabel(phase), phase);
  }
});

test("Inbox separates reconciliation from actionable requests and resolved history", async () => {
  const { default: Inbox } = load("../src/app/workspace/inbox/page.tsx", {
    "@/components/navigation-link": { default: ({ href, children }) => React.createElement("a", { href }, children) },
    "@/components/approval-decision": { ApprovalDecision: decision },
    "@/components/approval-detail": details,
    "@/components/clarification-answer": { ClarificationAnswer: () => null },
    "@/components/approval-phase-badge": phases,
    "@/components/honest-state": { HonestState: ({ title }) => React.createElement("p", {}, title) },
    "@/components/icon": { Icon: () => null },
    "@/components/live-refresh": { LiveRefresh: () => null },
    "@/lib/bff": {
      BffError: class extends Error {},
      getDigests: async () => [], listTeams: async () => [],
      listApprovals: async () => [approval("Pending", true), approval("DecisionRecorded", false), approval("Approved", false)],
    },
    "@/lib/config": { authWired: () => true, defaultNamespace: () => "kars-system", operatorIdentity: () => "reviewer" },
    "@/lib/session": { currentPrincipal: async () => ({ name: "reviewer" }) },
  });
  const html = renderToStaticMarkup(await Inbox({ searchParams: Promise.resolve({}) }));
  assert.match(html, /aria-label="1 items waiting"/);
  assert.match(html, /Decision recorded · reconciling/);
  assert.match(html, /Your decision is saved/);
  assert.equal((html.match(/Act on approval/g) ?? []).length, 1);
  const history = html.split("<details")[1];
  assert.match(history, /Approved request/);
  assert.doesNotMatch(history, /DecisionRecorded request|Pending request/);
});

const accessDetail = (overrides = {}) => JSON.stringify({
  source: "router-access-request", kind: "egress", target: "www.debian.org", port: 443,
  reason: "Egress policy blocked this destination", expiresAtUnix: 1793553169,
  identity: { task: { uid: "retained-task-uid" } }, ...overrides,
});
const renderDetail = (detail, actionKind = "egress") => renderToStaticMarkup(
  React.createElement(details.ApprovalDetail, { detail, actionKind }),
);

test("access requests explain destination, reason and review expiry without exposing raw identity by default", () => {
  const html = renderDetail(accessDetail());
  const visible = html.split("<details>")[0];
  assert.match(visible, /Destination:/);
  assert.ok(visible.split(/[<>]/).some(text => text === "www.debian.org · port 443"));
  assert.match(visible, /not currently allowed/);
  assert.match(visible, /Request expires:/);
  assert.match(visible, /<time dateTime="2026-11-01T17:12:49.000Z">2026-11-01 17:12:49 UTC/);
  assert.match(visible, /other access rules still apply/);
  assert.match(visible, /not permission for every future run/);
  assert.doesNotMatch(visible, /retained-task-uid|router-access-request/);
  assert.match(html, /<details><summary[^>]*>Technical details<\/summary>/);
  assert.match(html, /retained-task-uid/);
  assert.doesNotMatch(html, /<details open|<a /);
});

test("ordinary details and other approval types preserve the original content", () => {
  for (const detail of ["Please confirm the maintenance window.", "{incomplete JSON", "[1,2]", "null"]) {
    assert.doesNotMatch(renderDetail(detail), /<details>/);
    assert.ok(renderDetail(detail).includes(detail));
  }
  assert.doesNotMatch(renderDetail(accessDetail(), "clarification"), /<details>/);
  assert.equal(renderDetail(null), "");
  assert.equal(renderDetail(undefined), "");
});

test("incomplete access records never fabricate destination or expiry and preserve technical evidence", () => {
  for (const change of [{ target: "" }, { port: 0 }, { port: 65536 }, { port: "443" }, { reason: null }, { kind: "tier" }]) {
    const html = renderDetail(accessDetail(change));
    assert.match(html, /details are incomplete/);
    assert.doesNotMatch(html.split("<details>")[0], /Destination:/);
    assert.match(html, /Technical details/);
  }
  for (const expiresAtUnix of [null, -1, "tomorrow", 9007199254740991]) {
    assert.match(renderDetail(accessDetail({ expiresAtUnix })), /Not recorded/);
  }
  const html = renderDetail(accessDetail({ target: "<script>alert(1)</script>", reason: "<img src=x onerror=alert(1)>" }));
  assert.doesNotMatch(html, /<script\b|<img\b/i);
  assert.match(html, /&lt;script&gt;/);
});

test("task-scoped approval uses the same presentation without changing actionable controls", () => {
  for (const actionable of [true, false]) {
    const html = renderToStaticMarkup(React.createElement(TaskApprovalsPanel, {
      approvals: [{ ...approval(actionable ? "Pending" : "Expired", actionable), action_kind: "egress", detail: accessDetail() }],
      decider: "reviewer", authWired: true,
    }));
    assert.match(html, /Destination:/);
    assert.ok(html.split(/[<>]/).some(text => text === "www.debian.org · port 443"));
    assert.equal(html.includes("Act on approval"), actionable);
    assert.match(html, /<details><summary/);
  }
});
