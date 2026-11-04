// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
function load(path, imports = {}) {
  const context = { exports: {}, require(name) {
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const api = load("lib/efficiency.ts");

function assertResolution(models, history, model, reason) {
  const actual = api.resolveRecommendedModel(models, history);
  assert.equal(actual.model, model);
  assert.equal(actual.reason, reason);
}

test("catalogue recommendations cannot attribute ambiguous history to a provider", () => {
  const foundry = { provider: "azure-foundry", deployment: "gpt-5.4-mini", is_default: true };
  const openai = { provider: "azure-openai", deployment: "gpt-5.4-mini", is_default: false };
  const history = { recommended: "gpt-5.4-mini", total_runs: 8 };
  for (const models of [[foundry, openai], [openai, foundry]]) {
    assertResolution(models, history, null, "ambiguous");
  }
  for (const model of [foundry, openai]) {
    assertResolution([model], history, model, "selected");
  }
  assertResolution([foundry, { ...foundry }], history, foundry, "selected");
});

test("catalogue recommendation fallback distinguishes unavailable, empty and unqualified history", () => {
  const history = { recommended: "gpt-5.4-mini", total_runs: 8 };
  assertResolution([], null, null, "unavailable");
  assertResolution([], undefined, null, "unavailable");
  assertResolution([], { ...history, total_runs: 0 }, null, "empty");
  assertResolution([], { ...history, recommended: null }, null, "unqualified");
  assertResolution([], history, null, "missing");
});
const row = (harness, overrides = {}) => ({
  route: "same-model", harness, runs: 2, verified_runs: 2, delivered: 2, accepted: 1,
  success_rate: 1, acceptance_rate: 0.5, measured_token_runs: 1, measured_tokens: 100,
  avg_tokens: 100, tokens_per_outcome: 100, avg_rounds: null, avg_tool_calls: null,
  avg_prompt_tokens: null, avg_completion_tokens: null, tool_fail_rate: null,
  avg_wall_ms: null, p95_wall_ms: null, avg_ttfa_ms: null,
  reliability_rate: null, reliability_k: null, reliability_samples: 0,
  usd_per_outcome: null, cache_hit_rate: null, top_fault: "", ...overrides,
});
const efficiency = routes => ({
  routes, recommended: "same-model", recommended_harness: "Hermes",
  recommended_basis: "Best available measured route", recommended_low_confidence: true,
  total_runs: routes.reduce((sum, route) => sum + route.runs, 0), priced: false,
});

function render(node) {
  if (Array.isArray(node)) return node.map(render);
  if (!node || typeof node !== "object") return node;
  if (typeof node.type === "function") return render(node.type(node.props));
  return { type: node.type, ...node.props, children: render(node.props?.children) };
}

test("unknown measurements are absent, genuine zero stays zero and cannot beat an unknown", () => {
  for (const unknown of [null, undefined, NaN, Infinity, -1]) {
    assert.equal(api.metric(unknown), "—");
    assert.equal(api.lowerWins(unknown, 1), null);
    assert.equal(api.lowerWins(1, unknown), null);
  }
  assert.equal(api.metric(0), "0");
  assert.equal(api.metric(0, 1), "0.0");
  assert.equal(api.lowerWins(0, 1), true);
});

test("recommendation consumers select both exact model and harness", () => {
  const openclaw = row("OpenClaw");
  const hermes = row("Hermes", { accepted: 2 });
  assert.equal(api.recommendedStats(efficiency([openclaw, hermes])), hermes);
  assert.equal(api.recommendedStats(efficiency([openclaw])), null);
  assert.equal(api.recommendedStats({ ...efficiency([hermes]), recommended: "same" }), null);
  assert.equal(api.recommendedStats({ ...efficiency([openclaw]), recommended_harness: null }), null);
});

test("delivered-token subtotal uses measured totals, not averages multiplied by all deliveries", () => {
  assert.equal(api.measuredDeliveredTokens(efficiency([row("OpenClaw")])), 100);
  assert.equal(api.measuredDeliveredTokens(efficiency([row("OpenClaw", { measured_tokens: null })])), null);
  assert.equal(api.measuredDeliveredTokens(null), null);
  assert.equal(api.measuredDeliveredTokens(efficiency([row("OpenClaw", { measured_tokens: 0 })])), 0);
});

test("frontier omits unknown cost without excluding genuine measured zero", () => {
  const known = row("OpenClaw", { tokens_per_outcome: 0 });
  const unknown = row("Hermes", { tokens_per_outcome: null });
  const rows = api.measuredRoutes(efficiency([known, unknown]));
  assert.equal(rows.length, 1);
  assert.equal(rows[0], known);
});

test("Insights renders nullable measurements, exact harness and measured population safely", async () => {
  const jsx = (type, props) => ({ type, props });
  const data = efficiency([row("OpenClaw"), row("Hermes", {
    avg_rounds: 2, avg_tool_calls: 0, tool_fail_rate: null, cache_hit_rate: 0,
    measured_token_runs: 0, measured_tokens: null, tokens_per_outcome: null,
  })]);
  const page = load("app/console/insights/page.tsx", {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/components/bar-chart": { BarChart: () => null },
    "@/components/honest-state": { HonestState: () => null },
    "@/components/live-refresh": { LiveRefresh: () => null },
    "@/components/console-load-error": { ConsoleLoadError: () => null },
    "@/lib/efficiency": api,
    "@/lib/bff": {
      getEfficiency: async () => data,
      getInsights: async () => ({ missions_by_phase: [], missions_by_tier: [], decisions: [],
        launched: 0, receipts_issued: 0, inclusion_log_size: 0, amplification_rejections: 0,
        runtime_metrics_available: true }),
      getArtifacts: async () => ({ missions: [] }),
      getInferenceBudgets: async () => null,
    },
  });
  const rendered = render(await page.default());
  const text = JSON.stringify(rendered);
  assert.match(text, /Measured delivered tokens/);
  assert.match(text, /Hermes/);
  assert.match(text, /Runs \/ verified/);
  assert.match(text, /missing measurements cannot win/i);
  assert.match(text, /—/);
  assert.doesNotMatch(text, /NaN|Infinity|cheaper-per-accepted-outcome/);
});

test("Insights exposes failed evidence reads without falling back to outcome counts", async () => {
  const jsx = (type, props) => ({ type, props });
  const page = load("app/console/insights/page.tsx", {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/components/bar-chart": { BarChart: () => null },
    "@/components/honest-state": { HonestState: props => jsx("status", props) },
    "@/components/live-refresh": { LiveRefresh: props => jsx("refresh", props) },
    "@/components/console-load-error": { ConsoleLoadError: () => null },
    "@/lib/efficiency": api,
    "@/lib/bff": {
      getEfficiency: async () => { throw new Error("upstream unavailable"); },
      getInsights: async () => ({ missions_by_phase: [{ name: "Completed", count: 5 }] }),
      getArtifacts: async () => ({ missions: [
        { summary: "Retained output", status: "completed", review_status: "approved" },
      ] }),
      getInferenceBudgets: async () => null,
    },
  });
  const text = JSON.stringify(render(await page.default()));
  assert.match(text, /Efficiency evidence unavailable/);
  assert.match(text, /not empty history/);
  assert.match(text, /"intervalMs":8000/);
  assert.doesNotMatch(text, /Measured delivered tokens|Accepted|outcome-funnel/);
});
