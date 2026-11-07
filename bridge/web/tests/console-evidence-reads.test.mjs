// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const { jsx, jsxs } = require("react/jsx-runtime");
const { renderToStaticMarkup } = require("react-dom/server");
class BffError extends Error {
  constructor() { super("PRIVATE_DIAGNOSTIC_DETAIL"); this.code = "upstream_error"; }
}
function load(file, imports) {
  const context = { exports: {}, URL, require(name) {
    assert.ok(Object.hasOwn(imports, name), `Unexpected dependency ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const components = {
  "react/jsx-runtime": { jsx, jsxs },
  "@/components/icon": { Icon: () => null },
  "@/components/wiring-badge": { WiringBadge: ({ status }) => jsx("span", { children: status }) },
  "@/components/bar-chart": { BarChart: () => null },
  "@/components/live-refresh": { LiveRefresh: ({ intervalMs }) => jsx("span", { "data-refresh-ms": intervalMs }) },
  "@/components/ui": {
    PageHeader: ({ title, lead }) => jsxs("header", { children: [jsx("h1", { children: title }), jsx("p", { children: lead })] }),
    Section: ({ title, subtitle, children }) => jsxs("section", { children: [jsx("h2", { children: title }), subtitle, children] }),
    Stat: ({ label, value }) => jsxs("p", { children: [label, value] }),
  },
  "@/lib/config": { authWired: () => true, operatorIdentity: () => "operator",
    headlampUrl: () => null, headlampSsoEnabled: () => false },
  "@/lib/headlamp": load("lib/headlamp.ts", {}),
  "@/lib/efficiency": load("lib/efficiency.ts", {}),
};
components["@/components/console-load-error"] = load("components/console-load-error.tsx", {
  ...components, "@/lib/bff": { BffError },
});
components["@/components/honest-state"] = load("components/honest-state.tsx", components);

const system = { namespace: "kars-system", controller_reachable: true,
  crds: [{ name: "karssandboxes", installed: true }],
  pipeline: [{ id: "controller", name: "Controller wiring", status: "live", description: "Observed controller", detail: "Running" }] };
const insights = { missions_by_phase: [{ name: "Completed", count: 5 }],
  missions_by_tier: [], decisions: [], launched: 0, receipts_issued: 4, inclusion_log_size: 4,
  amplification_rejections: 0, runtime_metrics_available: false };
const noRuns = { routes: [], total_runs: 0, priced: false };
const artifacts = { missions: [{ summary: "Retained output", status: "completed", review_status: "approved" }] };

function page(name, overrides = {}, config = {}) {
  const bff = {
    BffError, getSystem: async () => system,
    getDiagnostics: async () => ({ healthy: true, issues: [], scanned_pods: 8, scanned_sandboxes: 3 }),
    getInsights: async () => insights, getEfficiency: async () => noRuns,
    getArtifacts: async () => artifacts, getInferenceBudgets: async () => null, ...overrides,
  };
  return load(`app/console/${name}/page.tsx`, { ...components, "@/lib/bff": bff,
    "@/lib/config": { ...components["@/lib/config"], ...config } }).default;
}
const outage = async () => { throw new BffError(); };

test("failed diagnostics stay visible beside independently read cluster wiring", async () => {
  const html = renderToStaticMarkup(await page("troubleshooting", { getDiagnostics: outage })());
  assert.match(html, /Could not load active issues \(upstream_error\)/);
  assert.match(html, /does not mean no issues exist/);
  assert.match(html, /Controller wiring/);
  assert.match(html, /data-refresh-ms="8000"/);
  assert.doesNotMatch(html, /All clear|No failing pods|PRIVATE_DIAGNOSTIC_DETAIL/);
});

test("failed system read exposes a retrying error instead of healthy wiring", async () => {
  const html = renderToStaticMarkup(await page("troubleshooting", { getSystem: outage })());
  assert.match(html, /Could not load system status \(upstream_error\)/);
  assert.match(html, /not evidence that the cluster is healthy/);
  assert.match(html, /data-refresh-ms="8000"/);
  assert.doesNotMatch(html, /Pipeline stages live|Controller wiring|PRIVATE_DIAGNOSTIC_DETAIL/);
});

test("fully wired inventory is not presented as successful end-to-end execution", async () => {
  const html = renderToStaticMarkup(await page("troubleshooting")());
  assert.match(html, /Stages reporting wired/);
  assert.match(html, /1 of 1 stages report wiring or inventory evidence/);
  assert.match(html, /not a successful end-to-end test/);
  assert.match(html, /do not prove that an agent can finish your task/);
  assert.match(html, /use its run activity and delivered results/);
  assert.doesNotMatch(html, /stages are live end-to-end|nothing implied to work that doesn/);
});

test("a successful empty diagnostic read remains genuinely healthy", async () => {
  const html = renderToStaticMarkup(await page("troubleshooting")());
  assert.match(html, /All clear — scanned 8 pods and 3 sandboxes/);
  assert.match(html, /No failing pods/);
  assert.doesNotMatch(html, /role="alert"/);
});

test("diagnostics retry replaces an unavailable state with real recovered issues", async () => {
  let failed = true;
  const Page = page("troubleshooting", { getDiagnostics: async () => {
    if (failed) return outage();
    return { healthy: false, scanned_pods: 8, scanned_sandboxes: 3, issues: [{
      severity: "critical", subject: "kars-sre/sre", kind: "SandboxDegraded",
      reason: "Degraded", detail: "API unavailable", remedy: "Inspect the registered API endpoint",
    }] };
  } });
  assert.match(renderToStaticMarkup(await Page()), /Could not load active issues/);
  failed = false;
  const html = renderToStaticMarkup(await Page());
  assert.match(html, /Inspect the registered API endpoint/);
  assert.match(html, /href="\/console\/fleet\?q=sre"/);
  assert.doesNotMatch(html, /Could not load active issues|All clear/);
});

for (const sso of [false, true]) {
  test(`pod diagnostics link to the actual pod rather than sandbox search (SSO ${sso})`, async () => {
    const html = renderToStaticMarkup(await page("troubleshooting", {
      getDiagnostics: async () => ({ healthy: false, scanned_pods: 1, scanned_sandboxes: 0, issues: [{
        severity: "warning", kind: "HighRestarts", subject: "agentmesh/registry-abc · registry",
        reason: "7 restarts", remedy: "Inspect container logs",
      }] }),
    }, { headlampUrl: () => "http://localhost:3905", headlampSsoEnabled: () => sso })());
    assert.match(html, /Inspect pod in Kubernetes dashboard/);
    assert.match(html, /target="_blank" rel="noreferrer"/);
    assert.match(html, sso ? /return=%2Fpods%2Fagentmesh%2Fregistry-abc/ : /href="http:\/\/localhost:3905\/c\/main\/pods\/agentmesh\/registry-abc"/);
    assert.doesNotMatch(html, /console\/fleet\?q=/);
  });
}

test("pod diagnostics without a configured dashboard provide an honest next step", async () => {
  const html = renderToStaticMarkup(await page("troubleshooting", {
    getDiagnostics: async () => ({ healthy: false, scanned_pods: 1, scanned_sandboxes: 0, issues: [{
      severity: "warning", kind: "HighRestarts", subject: "agentmesh/registry-abc · registry",
      reason: "7 restarts", remedy: "Inspect container logs",
    }] }),
  })());
  assert.match(html, /Ask your Kars administrator to inspect this pod and its container logs/);
  assert.doesNotMatch(html, /console\/fleet\?q=|Inspect pod in Kubernetes dashboard/);
});

test("artifact outage cannot become zero fallback deliveries or approvals", async () => {
  const html = renderToStaticMarkup(await page("insights", { getArtifacts: outage })());
  assert.match(html, /Could not load deliverable evidence/);
  assert.match(html, /Missing evidence is not zero outcomes/);
  assert.match(html, /—<\/p><p[^>]*>Deliverables<\/p>/);
  assert.match(html, /—<\/p><p[^>]*>Accepted<\/p>/);
  assert.doesNotMatch(html, /Outcome funnel|PRIVATE_DIAGNOSTIC_DETAIL/);
});

test("artifact outage preserves independently measured run outcomes", async () => {
  const html = renderToStaticMarkup(await page("insights", {
    getArtifacts: outage,
    getEfficiency: async () => ({ total_runs: 2, priced: false, recommended: null, routes: [{
      route: "measured", harness: "Hermes", runs: 2, verified_runs: 2, delivered: 1, accepted: 1,
      measured_token_runs: 0, measured_tokens: null, tokens_per_outcome: null,
      success_rate: 0.5, acceptance_rate: 1, reliability_rate: null,
      avg_wall_ms: null, p95_wall_ms: null, cache_hit_rate: null, tool_fail_rate: null,
      avg_rounds: null, avg_tool_calls: null,
    }] }),
  })());
  assert.match(html, /Could not load deliverable evidence/);
  assert.match(html, /1<\/p><p[^>]*>Deliverables<\/p>/);
  assert.match(html, /1<\/p><p[^>]*>Accepted<\/p>/);
  assert.match(html, /Outcome funnel/);
});

test("budget outage remains visible without hiding valid outcomes", async () => {
  const html = renderToStaticMarkup(await page("insights", { getInferenceBudgets: outage })());
  assert.match(html, /Could not load budget utilization/);
  assert.match(html, /does not mean no budgets exist or no tokens were spent/);
  assert.match(html, /1<\/p><p[^>]*>Accepted<\/p>/);
  assert.doesNotMatch(html, /PRIVATE_DIAGNOSTIC_DETAIL/);
});

test("independent Insights reads start together instead of serially awaiting each endpoint", async () => {
  let release;
  const barrier = new Promise(resolve => { release = resolve; });
  const started = [];
  const data = { getInsights: insights, getEfficiency: noRuns, getArtifacts: artifacts, getInferenceBudgets: null };
  const Page = page("insights", Object.fromEntries(Object.entries(data).map(([name, value]) => [
    name, async () => { started.push(name); await barrier; return value; },
  ])));
  const pending = Page();
  try { assert.deepEqual(started, Object.keys(data)); }
  finally { release(); }
  assert.match(renderToStaticMarkup(await pending), /Insights/);
});
