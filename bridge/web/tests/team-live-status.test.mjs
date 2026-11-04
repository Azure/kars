// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

const require = createRequire(import.meta.url);
const ts = require("typescript");
function load(path, imports = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports,
    require(name) {
      if (name in imports) return imports[name];
      if (name === "@/components/navigation-link") return {
        default: ({ href, children }) => React.createElement("a", { href }, children),
      };
      return require(name);
    }
  });
  return exports;
}
const { NowHero } = load("../src/app/workspace/teams/[name]/team-detail-panels.tsx");
const { WatchingStatus } = load("../src/app/workspace/teams/[name]/watching-status.tsx", {
  "@/lib/format": load("../src/lib/format.ts"),
});
const defaults = {
  teamName: "clinic", phase: "Active", health: "Healthy", paused: false, active: true,
  runRunning: false, runInFlight: false, everyMinutes: null, commonsEntries: 2,
  nextRunAt: null, lastRunAt: null, delivered: 2, generated: 16,
  latestRun: null, latestOutcome: null, lifecycleMode: "ephemeral",
  runtimeState: null, idleDeadlineAt: null, warmIdleSeconds: null,
  currentAssignment: null, memoryEntries: 2,
};
const render = (component, props) => renderToStaticMarkup(React.createElement(component, { ...defaults, ...props }));

for (const lifecycleMode of ["ephemeral", "resourceOptimized", "persistent"]) {
  for (const health of ["Healthy", "Stalled", "Unproductive"]) {
    test(`${lifecycleMode}/${health}: an in-flight run never presents as idle or queued`, () => {
      const props = { lifecycleMode, health, runInFlight: true };
      for (const component of [NowHero, WatchingStatus]) {
        const html = render(component, props);
        assert.match(html, /Run in progress/);
        assert.doesNotMatch(html, /On watch|Standing by|Idle|no assignment active|waiting for queued work/);
      }
      for (const component of [NowHero, WatchingStatus]) {
        const html = render(component, { ...props, runRunning: true });
        assert.match(html, /Working/);
        assert.doesNotMatch(html, /Standing by|On watch|Starting —/);
      }
    });
  }
}

test("review remains distinct from scheduling pause and draining work", () => {
  for (const component of [NowHero, WatchingStatus]) {
    const props = { health: "AwaitingReview", runInFlight: true, runRunning: true, runtimeState: "Working" };
    const html = render(component, props);
    assert.match(html, /Awaiting your review/);
    assert.doesNotMatch(html, /Working|Standing by|Current assignment in progress/);
    const paused = render(component, { ...props, paused: true, active: false });
    assert.match(paused, /Scheduling paused — current run is finishing/);
    assert.match(paused, /still uses tokens/);
    assert.doesNotMatch(paused, /Hibernating|principal runtime is suspended|No assignment runtime is allocated/);
  }
});

test("held reviewed runs override stale in-flight and runtime Working signals", () => {
  for (const component of [NowHero, WatchingStatus]) {
    const html = render(component, {
      latestOutcome: "held", runHeld: true, runInFlight: true, runRunning: true,
      runtimeState: "Working", latestRun: "held-run",
    });
    assert.match(html, /Run held/);
    assert.doesNotMatch(html, /Working|Run in progress|Current assignment in progress|Starting —/);
  }
});

const { RunNowControl } = load("../src/app/workspace/teams/[name]/run-control.tsx", {
  "next/navigation": { useRouter: () => ({ refresh() {} }) },
  "@/components/live-refresh": { LiveRefresh: () => null },
  "./run-actions": { runTeamNow: () => { throw new Error("Rendering must not submit work"); } },
});

test("held work requires an explicit stop and Team resume before offering a fresh run", () => {
  for (const paused of [false, true]) {
    const held = render(RunNowControl, { team: "clinic", held: true, stopped: false, paused, inFlight: false, runKey: "held-run" });
    assert.match(held, /Run held — stop this run/);
    assert.doesNotMatch(held, /<button/);
  }
  const paused = render(RunNowControl, { team: "clinic", held: true, stopped: true, paused: true, inFlight: false, runKey: "held-run" });
  assert.match(paused, /Scheduling paused/);
  assert.doesNotMatch(paused, /<button/);
  const stopped = render(RunNowControl, { team: "clinic", held: true, stopped: true, paused: false, inFlight: false, runKey: "held-run" });
  assert.match(stopped, /Run now/);
  assert.doesNotMatch(stopped, /Run in progress|Run held/);
  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/page.tsx", import.meta.url), "utf8");
  assert.match(page, /held=\{runHeld\}/);
  assert.match(page, /stopped=\{latestRunTask\?\.launched === false && Boolean\(latestRunTask.halted\)\}/);
});

test("idle teams retain on-demand status without claiming active work", () => {
  assert.match(render(NowHero, {}), /waiting for queued work or Run now/);
  assert.match(render(WatchingStatus, {}), /Idle · ready/);
  assert.match(render(WatchingStatus, {}), /Standing by/);
  assert.match(render(WatchingStatus, { runtimeState: "Working" }), /Current assignment in progress/);
});

test("controller rejection remains explicit despite in-flight flags", () => {
  const html = render(NowHero, { phase: "Degraded", detail: "Invalid trust envelope", runInFlight: true });
  assert.match(html, /Team blocked by controller validation/);
  assert.match(html, /Invalid trust envelope/);
  assert.doesNotMatch(html, /Run in progress|On watch/);
});

const { TeamsList } = load("../src/app/workspace/teams/teams-list.tsx", {
  "@/lib/format": load("../src/lib/format.ts"),
  "@/lib/types": { TIER_LABELS: { 2: "Shared" } },
  "@/components/team-timing": { TeamTiming: () => null },
});

test("Team list distinguishes paused scheduling from runtime hibernation", () => {
  const team = {
    name: "clinic", charter: "Review the monitoring application", tier: 2,
    phase: "Active", health: "Healthy", paused: true, runtime_state: "Working",
    lifecycle_mode: "ephemeral", member_count: 4, generated_task_count: 11,
    retained_delivered: 3, retained_no_action: 0, retained_failed: 1,
  };
  const list = (changes = {}) => renderToStaticMarkup(React.createElement(TeamsList, { teams: [{ ...team, ...changes }] }));
  assert.match(list(), /Scheduling paused/);
  assert.doesNotMatch(list(), /Hibernating/);
  assert.match(list({ paused: false }), /Working/);
  assert.doesNotMatch(list({ paused: false }), /Scheduling paused/);
  assert.match(list({ paused: false, runtime_state: "Hibernating", lifecycle_mode: "resourceOptimized", warm_idle_seconds: 600 }), /Hibernating/);
  assert.match(list({ phase: "Degraded" }), /Degraded/);
  assert.doesNotMatch(list({ phase: "Degraded" }), /Scheduling paused/);
});

test("paused Team summaries do not overwrite the current run's observed phase", () => {
  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/page.tsx", import.meta.url), "utf8");
  const runPhase = page.match(/agentPhase=\{([\s\S]*?)\n\s*\}/)?.[1];
  assert.ok(runPhase);
  assert.doesNotMatch(runPhase, /team\.paused|Hibernating/);
  assert.match(runPhase, /latestRunTask\.assignment\?\.state/);
  assert.match(page, /health=\{team\.health\}/);
  for (const source of [page, readFileSync(new URL("../src/app/workspace/page.tsx", import.meta.url), "utf8")]) {
    assert.match(source, /paused \? "Scheduling paused"/);
    assert.doesNotMatch(source, /paused \? "Hibernating"/);
  }
});

test("execution graph descriptions do not deny visible destinations or imply complete coverage", () => {
  const source = readFileSync(new URL("../src/components/agent-graph.tsx", import.meta.url), "utf8");
  assert.match(source, /destination nodes are not proof of complete traffic coverage/);
  assert.doesNotMatch(source, /destinations are not recorded/);
});
