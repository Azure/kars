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
  for (const action of ["enable", "disable"]) {
    const result = await helpers.copyWitnessCommand(action, clipboard);
    assert.match(result, /No cluster change/);
    assert.match(copied.at(-1), /--kube-context "\$\{KARS_CONTEXT:\?/);
    assert.match(copied.at(-1), /--set enabled=(true|false)/);
    assert.doesNotMatch(copied.at(-1), /install\.sh|--take-ownership|--force/);
  }
  await assert.rejects(helpers.copyWitnessCommand("enable", undefined), /Clipboard unavailable/);
  await assert.rejects(helpers.copyWitnessCommand("enable", { writeText: async () => { throw Error("denied"); } }), /denied/);
});

async function page(admin, value = report, fails = false) {
  const source = load("../src/app/console/datapath/page.tsx", {
    "@/components/ui": {
      PageHeader: ({ title, lead }) => React.createElement("header", null, title, lead),
      Stat: ({ label, value }) => React.createElement("span", null, label, value),
      Badge: ({ children }) => React.createElement("span", null, children),
    },
    "@/components/datapath-setup": { DatapathSetup: () => React.createElement("button", null, "Copy enable Helm command") },
    "@/lib/bff": { getDatapathWitness: async () => { if (fails) throw Error("denied"); return value; } },
    "@/lib/datapath-witness": { witnessPresentation: (w) => helpers.witnessPresentation(w, now) },
    "@/lib/session": { canAdminister: async () => admin },
  });
  return renderToStaticMarkup(await source.default());
}

test("server page exposes command controls only to resolved admins; failures stay explicit", async () => {
  assert.match(await page(true), /Copy enable Helm command/);
  const operator = await page(false);
  assert.doesNotMatch(operator, /Copy enable Helm command/);
  assert.match(operator, /cluster operator outside Bridge/);
  assert.doesNotMatch(operator, /untrusted old hint|Live from the eBPF|Compliant/);
  const failed = await page(false, report, true);
  assert.match(failed, /Couldn&#x27;t reach the cluster/);
  assert.doesNotMatch(failed, /Fresh partial sample/);
  for (const state of ["stale", "invalid", "unavailable", "legacy"]) {
    const html = await page(true, { ...report, state });
    assert.doesNotMatch(html, /Sandboxes in scope/);
  }
});
