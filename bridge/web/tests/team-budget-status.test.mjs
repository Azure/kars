// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const { renderToStaticMarkup } = require("react-dom/server");
const source = readFileSync(new URL("../src/app/workspace/teams/[name]/team-budget-status.tsx", import.meta.url), "utf8");
const exports = {};
runInNewContext(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText, { exports, require });
const budget = { scope: "GovernedInference", tokens: 5000000, usd_micros: 10000 };
const status = {
  state: "available", observed_at: "2026-10-20T12:00:00Z", account_phase: "Active",
  unbounded_tokens: false, effective_tokens: "6000000", effective_usd_micros: "10000",
  settled: { tokens: "5400000", usd_micros: "700" },
  reserved: { tokens: "200", usd_micros: "20" },
  uncertain: { tokens: "100", usd_micros: "10" }, price_coverage_complete: true,
};
function render(budget_status, original = budget) {
  return renderToStaticMarkup(exports.TeamBudgetStatus({ team: { budget: original, budget_status } }));
}

test("unbounded allowance replaces effective ceiling, not original history or monetary cap", () => {
  const html = render({ ...status, unbounded_tokens: true, effective_tokens: null });
  assert.match(html, /Unlimited · non-expiring token allowance/);
  assert.match(html, /10,000 micro-USD \(still enforced\)/);
  assert.match(html, /Original lifetime inference grant \(historical\)/);
  assert.match(html, /Tokens: 5000000/);
  assert.match(html, /5,400,000 tokens · 700 micro-USD/);
  assert.match(html, /Reserved \(not settled\)/);
  assert.match(html, /Uncertain charges/);
  assert.match(html, /not a remaining balance/);
  assert.doesNotMatch(html, /kars budget|<button/);
});

test("finite top-ups show effective tokens independently from settled, reserved and uncertain amounts", () => {
  const html = render(status);
  assert.match(html, /6,000,000 tokens/);
  assert.match(html, /200 tokens · 20 micro-USD/);
  assert.match(html, /100 tokens · 10 micro-USD/);
  assert.doesNotMatch(html, /Unlimited|non-expiring/);
});

test("absent caps and tier defaults never imply an authorized unlimited allowance", () => {
  const monetaryOnly = render({ ...status, effective_tokens: null });
  assert.match(monetaryOnly, /No token ceiling on this account/);
  assert.match(monetaryOnly, /10,000 micro-USD \(still enforced\)/);
  assert.doesNotMatch(monetaryOnly, /Unlimited|non-expiring/);
  const defaultTeam = render(undefined, null);
  assert.match(defaultTeam, /unavailable/);
  assert.match(defaultTeam, /Tier defaults are not an unlimited allowance/);
  assert.doesNotMatch(defaultTeam, /Settled usage|Unlimited ·/);
});

test("unreadable, missing and legacy observations show unavailable without synthetic balances", () => {
  for (const observation of [undefined, null,
    { state: "unavailable", reason: "A fresh controller-verified lifetime account observation is unavailable." },
    { state: "unavailable", reason: "The bound lifetime account is missing." },
    { state: "unavailable", reason: "The ledger could not be verified." },
  ]) {
    const html = render(observation);
    assert.match(html, /Effective budget and account usage unavailable/);
    assert.doesNotMatch(html, /Settled usage|0 tokens|Unlimited ·/);
    if (observation) assert.ok(html.includes(observation.reason));
  }
});

test("large exact counters, closed accounts and unknown prices remain honest", () => {
  const html = render({ ...status, account_phase: "Closed", unbounded_tokens: true,
    effective_tokens: null, effective_usd_micros: null, price_coverage_complete: false,
    settled: { tokens: "9007199254740993", usd_micros: "0" } });
  assert.match(html, /9,007,199,254,740,993 tokens/);
  assert.match(html, /Recorded account phase: Closed/);
  assert.match(html, /Observed at/);
  assert.doesNotMatch(html, /Read at/);
  assert.match(html, /No monetary ceiling on this account/);
  assert.match(html, /actual monetary cost is unknown/);
  assert.match(html, /not a remaining balance\s+or proof of live account readiness/);
});

test("Team detail mounts the status projection instead of delegating inspection to the CLI", () => {
  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/page.tsx", import.meta.url), "utf8");
  assert.match(page, /<TeamBudgetStatus team=\{team\} \/>/);
  assert.doesNotMatch(page, /tokenTopUps with kars budget status/);
});
