// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");

function load(path, dependencies = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports, console, Date, Set, Map,
    require: (name) => dependencies[name] ?? require(name),
  });
  return exports;
}

const helpers = load("../src/lib/datapath-witness.ts");
const now = Date.parse("2026-09-15T15:00:00Z");
const report = {
  enabled: true, state: "observed", requested_enabled: true, coverage: "partial",
  generated_at: new Date(now).toISOString(), window_seconds: 15,
  diagnostic: "A partial sample only.", event_count: 1, nodes_targeted: ["node"],
  nodes_with_events: ["node"], sandboxes: [], install_hint: "untrusted old hint",
};

test("old enabled flag and missing reports never prove installation or live observation", () => {
  const old = helpers.witnessPresentation({ enabled: true, sandboxes: [] }, now);
  assert.equal(old.fresh, false);
  assert.equal(old.requested, "Unknown");
  assert.match(old.label, /unknown/i);
  const missing = helpers.witnessPresentation({ state: "missing", enabled: false }, now);
  assert.match(missing.label, /installation unknown/);
  assert.equal(missing.fresh, false);
});

test("freshness and negative report states suppress current evidence", () => {
  assert.equal(helpers.witnessPresentation(report, now).fresh, true);
  assert.equal(helpers.witnessPresentation(report, now + 180_000).fresh, true);
  assert.equal(helpers.witnessPresentation(report, now + 180_001).state, "stale");
  assert.equal(helpers.witnessPresentation(report, now - 30_001).fresh, false);
  for (const state of ["invalid", "unavailable", "legacy", "disabled", "pending", "stale"]) {
    assert.equal(helpers.witnessPresentation({ ...report, state }, now).fresh, false);
  }
  const empty = helpers.witnessPresentation({ ...report, enabled: false, state: "empty" }, now);
  assert.equal(empty.fresh, true);
  assert.match(empty.label, /unproven/);
});

test("copy is only clipboard with explicit context and no privileged web actuator", async () => {
  const documentation = readFileSync(new URL("../../../deploy/ebpf-witness/README.md", import.meta.url), "utf8");
  assert.ok(documentation.includes(helpers.WITNESS_ENABLE_COMMAND));
  assert.ok(documentation.includes(helpers.WITNESS_DISABLE_COMMAND));
  const copied = [];
  const clipboard = { writeText: async (value) => copied.push(value) };
  for (const owner of [undefined, "kars", "kars-datapath-witness"]) {
    for (const action of ["enable", "disable"]) {
      const result = await helpers.copyWitnessCommand(action, clipboard, owner);
      assert.match(result, /No cluster change/);
      assert.match(copied.at(-1), /--kube-context "\$\{KARS_CONTEXT:\?/);
      assert.match(copied.at(-1), /--set (datapathWitness\.)?enabled=(true|false)/);
      assert.ok(documentation.includes(copied.at(-1)));
      assert.equal(copied.at(-1).includes("--set datapathWitness."), owner !== "kars-datapath-witness");
      assert.doesNotMatch(copied.at(-1), /install\.sh|--take-ownership|--force/);
    }
  }
  await assert.rejects(helpers.copyWitnessCommand("enable", undefined), /Clipboard unavailable/);
  await assert.rejects(helpers.copyWitnessCommand("enable", { writeText: async () => { throw Error("denied"); } }), /denied/);
});

test("operator setup requires reviewed images for both AL3 runtime roles", () => {
  const { DatapathSetup } = load("../src/components/datapath-setup.tsx", {
    "@/lib/datapath-witness": helpers,
  });
  const html = renderToStaticMarkup(React.createElement(DatapathSetup));
  assert.match(html, /AL3 runtime digest/);
  assert.match(html, /aggregator\.image/);
  assert.match(html, /gadget\.image/);
  assert.match(html, /both the client and daemon/);
  assert.match(html, /does not promise a prepublished image/);
  assert.match(html, /No third Helm release/);
  assert.match(html, /scope: all/);
  assert.match(html, /sandboxes: \[\]/);
  assert.match(html, /including new Team and Mission workers/);
  assert.match(html, /explicit, reviewed node selection/);
  assert.match(html, /scope: selected/);
  assert.match(html, /up to 50 exact sandbox names/);
  assert.match(html, /Omitting scope preserves selected mode/);
  const standalone = renderToStaticMarkup(React.createElement(DatapathSetup, { ownerRelease: "kars-datapath-witness" }));
  assert.match(standalone, /standalone release/);
  assert.doesNotMatch(standalone, /--set datapathWitness/);
});

async function page(admin, value = report, fails = false) {
  const source = load("../src/app/console/datapath/page.tsx", {
    "@/components/navigation-link": { default: props => React.createElement("a", props) },
    "@/components/ui": {
      PageHeader: ({ title, lead }) => React.createElement("header", null, title, lead),
      Stat: ({ label, value }) => React.createElement("span", null, label, value),
      Badge: ({ children }) => React.createElement("span", null, children),
    },
    "@/components/datapath-setup": {
      DatapathSetup: ({ ownerRelease }) => React.createElement("button", { "data-owner": ownerRelease }, "Copy enable Helm command"),
    },
    "@/components/live-refresh": {
      LiveRefresh: ({ active, intervalMs }) => React.createElement("span", {
        "data-refresh-active": active, "data-refresh-interval": intervalMs,
      }),
    },
    "@/lib/bff": { getDatapathWitness: async () => { if (fails) throw Error("denied"); return value; } },
    "@/lib/datapath-witness": { witnessPresentation: (w) => helpers.witnessPresentation(w, now) },
    "@/lib/session": { canAdminister: async () => admin },
  });
  return renderToStaticMarkup(await source.default());
}

test("server page exposes command controls only to resolved admins; failures stay explicit", async () => {
  assert.match(await page(true), /Copy enable Helm command/);
  for (const owner of ["kars", "kars-datapath-witness"]) {
    assert.ok((await page(true, { ...report, owner_release: owner })).includes(`data-owner="${owner}"`));
  }
  const operator = await page(false);
  assert.doesNotMatch(operator, /Copy enable Helm command/);
  assert.match(operator, /cluster operator outside Bridge/);
  assert.doesNotMatch(operator, /untrusted old hint|Live from the eBPF|Compliant/);
  const failed = await page(false, report, true);
  assert.match(failed, /Couldn&#x27;t reach the cluster/);
  assert.doesNotMatch(failed, /Fresh partial sample/);
  for (const state of ["stale", "invalid", "unavailable", "legacy"]) {
    const html = await page(true, { ...report, state });
    assert.doesNotMatch(html, /Sandboxes in sample/);
  }
});

test("worker lifecycle guidance and destinations remain available without a current sample", async () => {
  for (const state of ["observed", "empty", "unavailable", "stale", "invalid", "missing"]) {
    const html = await page(false, { ...report, state });
    assert.match(html, /Looking for a Team or Mission agent/);
    assert.match(html, /Temporary workers can appear only while their sandboxes exist/);
    assert.match(html, /An unavailable sample does not mean that no agents are running/);
    for (const destination of ["/workspace/agents", "/console/fleet", "/workspace/teams", "/workspace/missions"]) {
      assert.ok(html.includes(`href="${destination}"`));
    }
  }
  const diagnostic = "Sandbox inventory changed during capture. This window was discarded to avoid assigning traffic to the wrong worker. Waiting for the next sample.";
  const html = await page(false, { ...report, state: "unavailable", diagnostic });
  assert.ok(html.includes(diagnostic));
  assert.doesNotMatch(html, /Sandboxes in sample/);
});

test("internal-only samples explain selected scope and empty external-traffic rows", async () => {
  const internalOnly = {
    ...report, event_count: 6,
    sandboxes: [{
      namespace: "kars-selected-agent", sandbox: "selected-agent", egress_mode: "Strict",
      declared_hosts: ["docs.example.com"], observed_dns: [], observed_connects: 0,
      beyond_declared: [], unused_declared: ["docs.example.com"], verdict: "NO-TRAFFIC",
    }],
  };
  const html = await page(false, internalOnly);
  assert.match(html, /Selected sandboxes only/);
  assert.match(html, /not a fleet-wide feed/);
  assert.match(html, /Team and Mission workers/);
  assert.match(html, /In-scope events6/);
  assert.match(html, /internal traffic, DNS replies, and TCP accept\/close events/);
  assert.match(html, /nonzero event count can therefore coexist with empty external-traffic rows/);
  assert.match(html, /selected-agent/);
  assert.match(html, /Observed external DNS/);
  assert.match(html, /none in window/);
  assert.match(html, /No external events \/ unproven/);
  assert.doesNotMatch(html, /Compliant|Observed \(DNS\)/);
  for (const state of ["stale", "invalid", "unavailable"]) {
    assert.doesNotMatch(await page(false, { ...internalOnly, state }), /Selected sandboxes only|selected-agent/);
  }
});

test("all scope renders dynamic workers as a partial reported sample, not fleet inventory", async () => {
  const fleet = {
    ...report, scope: "all", event_count: 60,
    sandboxes: Array.from({ length: 60 }, (_, index) => ({
      namespace: `kars-worker-${index}`, sandbox: `worker-${index}`, sandbox_uid: `cr-uid-${index}`,
      egress_mode: "Strict", declared_hosts: [], observed_dns: [], observed_connects: 1,
      beyond_declared: [], unused_declared: [], verdict: "NO-BEYOND-OBSERVED",
    })),
  };
  const html = await page(false, fleet);
  assert.match(html, /All-sandbox scope \/ dynamic sampling/);
  assert.match(html, /including new Team and Mission workers/);
  assert.match(html, /reported sample, not a live fleet inventory/);
  assert.match(html, /Sandboxes in sample60/);
  assert.match(html, /worker-59/);
  assert.match(html, /CR UID: cr-uid-59/);
  assert.match(html, /href="\/console\/fleet\?q=worker-59"/);
  assert.match(html, /Partial sample, not a live stream/);
  assert.doesNotMatch(html, /Selected sandboxes only|not a fleet-wide feed/);
  const empty = await page(false, { ...fleet, state: "empty", event_count: 0, sandboxes: [] });
  assert.match(empty, /All-sandbox scope/);
  assert.match(empty, /No sandbox records in this sample/);
  assert.match(empty, /does not demonstrate observation coverage/);
  for (const state of ["stale", "invalid", "unavailable", "legacy"]) {
    assert.doesNotMatch(await page(false, { ...fleet, state }), /All-sandbox scope|worker-59/);
  }
  assert.match(await page(false, { ...fleet, scope: "selected" }), /Selected sandboxes only/);
});

test("unavailable baselines retain observations without inventing a mode or comparison", async () => {
  for (const diagnostic of ["baseline_unavailable", "declaration_invalid"]) {
    const value = {
      ...report, scope: "all", event_count: 3,
      sandboxes: [{
        namespace: "kars-new-worker", sandbox: "new-worker", sandbox_uid: "worker-cr-uid",
        egress_mode: null, declaration_diagnostic: diagnostic, verdict: "UNAVAILABLE",
        declared_hosts: [], beyond_declared: [], unused_declared: [],
        observed_dns: ["actual.example.com"], observed_connects: 2,
      }],
    };
    const html = await page(false, value);
    assert.match(html, /Baseline unavailable1/);
    assert.match(html, /Baseline unavailable \/ comparison unknown/);
    assert.match(html, new RegExp(diagnostic));
    assert.match(html, /unavailable \/ not compared/);
    assert.ok(html.split(/[<>]/).some(text => text === "actual.example.com"));
    assert.match(html, /tabular-nums">2</);
    assert.match(html, /Observations are retained; comparison is unavailable/);
    assert.doesNotMatch(html, /empty baseline \(Learn\)|empty baseline \(Strict deny-all\)|No beyond-declared DNS in sample/);
  }
});

test("witness refresh remains active for observed, stale, missing and failed reads", async () => {
  for (const [value, failed] of [
    [report, false], [{ ...report, state: "stale" }, false],
    [null, false], [report, true],
  ]) {
    const html = await page(false, value, failed);
    assert.match(html, /data-refresh-active="true"/);
    assert.match(html, /data-refresh-interval="5000"/);
    assert.doesNotMatch(html, /Reload to read the latest report/);
  }
  assert.match(await page(false), /refreshes automatically while visible; capture remains sampled/);
});
