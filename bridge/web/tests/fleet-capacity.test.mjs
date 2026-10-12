// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const { renderToStaticMarkup } = require("react-dom/server");
const source = readFileSync(new URL("../src/app/console/fleet/capacity-dashboard.tsx", import.meta.url), "utf8");
const formatContext = { exports: {} };
runInNewContext(ts.transpileModule(
  readFileSync(new URL("../src/lib/format.ts", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } },
).outputText, formatContext);
const imports = {
  react: require("react"),
  "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/ui": { Section: ({ children }) => children, Badge: ({ children }) => children },
  "@/lib/format": formatContext.exports,
};
const context = {
  exports: {},
  require(name) {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
    return imports[name];
  },
};
runInNewContext(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText, context);
const { CapacityDashboard } = context.exports;
const fleetContext = { ...context, exports: {} };
runInNewContext(ts.transpileModule(
  readFileSync(new URL("../src/app/console/fleet/fleet-list.tsx", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } },
).outputText, fleetContext);
const { FleetList } = fleetContext.exports;
const honestContext = { ...context, exports: {} };
runInNewContext(ts.transpileModule(
  readFileSync(new URL("../src/components/honest-state.tsx", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } },
).outputText, honestContext);

const node = {
  name: "worker", cpu_usage_millicores: 0, cpu_allocatable_millicores: 2000,
  memory_usage_bytes: 0, memory_allocatable_bytes: 1024 ** 3,
  cpu_percent: 0, memory_percent: 0,
};
const capacity = {
  metrics_available: true, metrics_error: null, nodes: [node],
  active_team_runs: 2, activity_error: null,
  pod_metrics_available: true, pod_metrics_error: null,
};
function render(overrides = {}, sandboxes = []) {
  return renderToStaticMarkup(CapacityDashboard({ capacity: { ...capacity, ...overrides }, sandboxes }));
}

test("metrics outages retain activity and never invent global admission slots", () => {
  const html = render({
    metrics_available: false, metrics_error: "Install metrics-server.",
    nodes: [{ ...node, cpu_usage_millicores: null, memory_usage_bytes: null }],
    pod_metrics_available: false,
  });
  assert.match(html, /2 active team runs/);
  assert.match(html, /Install metrics-server/);
  assert.match(html, /Complete samples unavailable/);
  assert.doesNotMatch(html, /Healthy headroom|global slots|slots free|0\.0%/);
});

test("partial node coverage does not masquerade as a low cluster total", () => {
  const html = render({
    metrics_available: false,
    nodes: [node, { ...node, name: "unsampled", cpu_usage_millicores: null, memory_usage_bytes: null }],
  });
  assert.equal((html.match(/Complete samples unavailable/g) ?? []).length, 2);
  assert.doesNotMatch(html, /Healthy headroom/);
  assert.match(html, /unsampled/);
});

test("real zero samples render zero-width bars and healthy headroom", () => {
  const html = render();
  assert.match(html, /0\.0%/);
  assert.match(html, /width:0%/);
  assert.doesNotMatch(html, /width:1%/);
  assert.match(html, /Healthy headroom/);
});

test("activity read failures remain unknown instead of zero runs", () => {
  const html = render({ active_team_runs: null, activity_error: "Task activity could not be read." });
  assert.match(html, /Team activity unavailable/);
  assert.match(html, /Task activity could not be read/);
  assert.doesNotMatch(html, /0 active team runs/);
});

test("team footprint reports sample coverage and safely renders names", () => {
  const html = render({}, [
    { team: "<team>", parent: null, cpu_millicores: 100, memory_bytes: 1024, executing: true },
    { team: "<team>", parent: null, cpu_millicores: null, memory_bytes: null, executing: null },
  ]);
  assert.match(html, /&lt;team&gt;/);
  assert.match(html, /Complete samples: 1\/2/);
  assert.match(html, /1 confirmed executing/);
  assert.doesNotMatch(html, /100m|1 MiB/);
});

test("over-capacity values are not capped in the displayed percentage", () => {
  const html = render({ nodes: [{ ...node, cpu_usage_millicores: 3000, cpu_percent: 150 }] });
  assert.match(html, /150\.0%/);
  assert.match(html, /High pressure/);
  assert.doesNotMatch(html, /#[0-9a-f]{6}/i);
});

const sandbox = {
  name: "bridge-orchestrator-with-a-long-name", namespace: "kars-system",
  runtime_namespace: "kars-bridge-orchestrator", phase: "Running", parent: null,
  runtime: "OpenClaw", governed: true, created: null,
  cpu_millicores: null, memory_bytes: null,
  conditions: [{ type_: "RuntimeReady", status: "True", reason: "Reconciled", message: "adapter <OpenClaw> reconciled" }],
};
function renderFleet(initialQuery = "") {
  return renderToStaticMarkup(imports["react/jsx-runtime"].jsx(FleetList, { sandboxes: [sandbox], initialQuery, observedAt: Date.now() }));
}

function renderTimedFleet(clock, localText) {
  class EnvironmentDate extends Date {
    static now() { return clock; }
    toLocaleString() { return localText; }
  }
  const timed = { ...context, exports: {}, Date: EnvironmentDate };
  runInNewContext(ts.transpileModule(
    readFileSync(new URL("../src/app/console/fleet/fleet-list.tsx", import.meta.url), "utf8"),
    { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } },
  ).outputText, timed);
  return renderToStaticMarkup(imports["react/jsx-runtime"].jsx(timed.exports.FleetList, {
    sandboxes: [{ ...sandbox, created: "2026-10-09T12:00:00Z" }],
    observedAt: Date.parse("2026-10-09T12:00:59Z"),
  }));
}

test("Fleet creation evidence is identical across server and browser time zones", () => {
  const clock = Date.parse("2026-10-09T12:00:59Z");
  const server = renderTimedFleet(clock, "12:00 PM");
  assert.equal(renderTimedFleet(clock, "2:00 PM"), server);
  assert.match(server, /2026-10-09 12:00:00 UTC/);
});

test("Fleet age uses the server observation across a hydration minute boundary", () => {
  const server = renderTimedFleet(Date.parse("2026-10-09T12:00:59Z"), "same zone");
  const client = renderTimedFleet(Date.parse("2026-10-09T12:01:00Z"), "same zone");
  assert.equal(client, server);
  assert.match(client, /59s.*?old/);
});

async function renderPage({ inventoryFails = false, capacityFails = false } = {}) {
  const jsx = imports["react/jsx-runtime"].jsx;
  const pageImports = {
    ...imports,
    "@/components/ui": {
      ...imports["@/components/ui"],
      PageHeader: ({ title }) => jsx("h1", { children: title }),
      Stat: ({ label, value }) => jsx("p", { children: `${label}: ${value}` }),
    },
    "@/components/honest-state": honestContext.exports,
    "@/components/live-refresh": { LiveRefresh: () => null },
    "@/lib/bff": {
      listSandboxes: async () => {
        if (inventoryFails) throw new Error("private inventory failure detail");
        return [sandbox];
      },
      getClusterCapacity: async () => {
        if (capacityFails) throw new Error("private capacity failure detail");
        return capacity;
      },
    },
    "./fleet-list": { FleetList },
    "./mesh-topology": { MeshTopology: () => null },
    "./capacity-dashboard": { CapacityDashboard },
  };
  const pageContext = { exports: {}, require(name) {
    assert.ok(Object.hasOwn(pageImports, name), `unexpected dependency: ${name}`);
    return pageImports[name];
  } };
  runInNewContext(ts.transpileModule(
    readFileSync(new URL("../src/app/console/fleet/page.tsx", import.meta.url), "utf8"),
    { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } },
  ).outputText, pageContext);
  return renderToStaticMarkup(await pageContext.exports.default({}));
}

test("failed capacity reads leave a visible recovery state and the actual fleet intact", async () => {
  const html = await renderPage({ capacityFails: true });
  assert.match(html, /Cluster &amp; agent utilization unavailable/);
  assert.match(html, /retries automatically/);
  assert.match(html, /bridge-orchestrator-with-a-long-name/);
  assert.doesNotMatch(html, /private capacity failure detail|Healthy headroom/);
});

test("failed sandbox inventory never becomes an empty healthy footprint", async () => {
  const html = await renderPage({ inventoryFails: true });
  assert.match(html, /Sandbox inventory is unavailable/);
  assert.match(html, /Cluster unreachable/);
  assert.doesNotMatch(html, /No sandboxes|Healthy headroom|private inventory failure detail/);
});

test("complete Fleet read failures remain explicit and recover to real capacity", async () => {
  const failed = await renderPage({ inventoryFails: true, capacityFails: true });
  assert.match(failed, /Cluster &amp; agent utilization unavailable/);
  assert.match(failed, /Cluster unreachable/);
  const recovered = await renderPage();
  assert.match(recovered, /Healthy headroom/);
  assert.match(recovered, /bridge-orchestrator-with-a-long-name/);
  assert.doesNotMatch(recovered, /utilization unavailable|Cluster unreachable/);
});

test("Fleet preserves full condition details with labelled status, reason and message", () => {
  const html = renderFleet();
  assert.match(html, /RuntimeReady/);
  assert.match(html, /Status: <\/span>True/);
  assert.match(html, /Reason: <\/span>Reconciled/);
  assert.match(html, /Message: <\/span>adapter &lt;OpenClaw&gt; reconciled/);
  assert.doesNotMatch(html, /<table/);
  assert.match(html, /bridge-orchestrator-with-a-long-name/);
});

test("Fleet filters expose their selected state and a labelled search field", () => {
  const html = renderFleet();
  assert.match(html, /aria-label="Search sandboxes"/);
  assert.equal((html.match(/aria-pressed="true"/g) ?? []).length, 1);
  assert.equal((html.match(/aria-pressed="false"/g) ?? []).length, 4);
});

test("Fleet initial search matches namespace and reports an honest empty result", () => {
  assert.match(renderFleet("kars-system"), /RuntimeReady/);
  const html = renderFleet("no-matching-sandbox");
  assert.match(html, /No sandbox matches these filters/);
  assert.doesNotMatch(html, /RuntimeReady/);
});
