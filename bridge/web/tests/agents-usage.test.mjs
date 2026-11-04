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
const source = readFileSync(new URL("../src/app/workspace/agents/page.tsx", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText;

async function render(agents, error = false) {
  const imports = {
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/navigation-link": { default: ({ children, ...props }) => React.createElement("a", props, children) },
    "@/components/honest-state": { HonestState: ({ title, detail }) => React.createElement("p", null, title, detail) },
    "@/components/live-refresh": { LiveRefresh: () => null, LivePulse: () => null },
    "@/components/fleet-live": { FleetLive: () => null },
    "@/lib/bff": {
      listAgents: async () => { if (error) throw new Error("Unavailable"); return agents; },
      getFleetTelemetry: async () => null,
    },
    "@/lib/format": { formatUptime: String },
    "@/lib/types": { TIER_LABELS: {} },
  };
  const context = { exports: {}, require(name) {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency ${name}`);
    return imports[name];
  } };
  runInNewContext(compiled, context);
  return renderToStaticMarkup(await context.exports.default());
}

const run = (overrides = {}) => ({
  sandbox: "run-1", namespace: "kars-system", task: "run-1", team: "team-1",
  tokens: null, budget_tokens: null, live: false, status: "error", phase: "Errored",
  rounds: null, tool_calls: null, ...overrides,
});
const usageSection = (html) => html.match(/<section\b[^>]*>[\s\S]*?<\/section>/)?.[0] ?? "";

test("root receipts are scoped separately from specialist and lifetime accounting", async () => {
  const html = await render([run({ tokens: 993479 }), run({ sandbox: "run-2", tokens: 1000 }), run({ sandbox: "run-3" })]);
  assert.match(html, /Recorded root-task usage/);
  assert.match(html, /receipts available for 2\/3 listed runs/);
  assert.match(html, /994,479/);
  assert.match(html, /Specialist phases and unavailable receipts are excluded/);
  assert.match(html, /not full run consumption or settled account totals/);
  assert.match(html, /993k root tok/);
  assert.match(html, /href="\/workspace\/teams\/team-1\/runs\/run-1"/);
  assert.doesNotMatch(html, /tokens spent|Spend &amp; budget|of budget used/);
});

test("missing receipts with a known cap do not become zero usage or zero percent", async () => {
  const html = await render([run({ budget_tokens: 1000 })]);
  assert.match(html, /receipts available for 0\/1 listed runs/);
  assert.match(html, /Usage unavailable · 1\.0k cap/);
  assert.match(usageSection(html), />Unavailable<\/p>/);
  assert.doesNotMatch(html, /width:|0%|0\/1\.0k/);
});

test("missing uncapped receipts remain unavailable rather than disappearing", async () => {
  const html = await render([run()]);
  assert.match(html, /Usage unavailable/);
  assert.match(usageSection(html), />Unavailable<\/p>/);
  assert.doesNotMatch(html, /0 root tok|tokens spent|width:/);
});

test("measured zero retains an actual zero gauge", async () => {
  const html = await render([run({ tokens: 0, budget_tokens: 1000 })]);
  assert.match(html, /receipts available for 1\/1 listed runs/);
  assert.match(html, /0\/1\.0k root\/cap/);
  assert.match(html, /width:0%/);
  assert.match(usageSection(html), />0%<\/p>/);
  assert.doesNotMatch(html, /Usage unavailable|width:2%/);
});

test("partial capped receipt coverage cannot claim a combined utilization or healthy budget", async () => {
  const html = await render([
    run({ tokens: 100, budget_tokens: 2000 }),
    run({ sandbox: "run-2", budget_tokens: 1000 }),
  ]);
  const section = usageSection(html);
  assert.match(section, /receipts available for 1\/2 listed runs/);
  assert.match(section, /root receipts above cap/);
  assert.doesNotMatch(section, /%|width:|text-ok|>0<\/p>/);
  assert.match(html, /100\/2\.0k root\/cap/);
});

test("a declared zero cap is not shown as an absent cap", async () => {
  const section = usageSection(await render([run({ tokens: 1, budget_tokens: 0 })]));
  assert.match(section, />0<\/p><p[^>]*>budgeted \(1\/1 capped\)/);
  assert.match(section, />1<\/p><p[^>]*>root receipts above cap/);
});

test("live cards label root usage rather than full reviewed-run usage", async () => {
  const html = await render([run({ live: true, tokens: 100, rounds: 1 })]);
  assert.match(html, /100 root tokens/);
});

test("unreachable fleet data does not render an empty spending total", async () => {
  const html = await render([], true);
  assert.match(html, /Run environment unreachable/);
  assert.doesNotMatch(html, /Recorded root-task usage|root tokens recorded/);
});
