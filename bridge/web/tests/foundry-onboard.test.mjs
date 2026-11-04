// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const empty = { error: null, ok: null };

function compile(file, dependencies) {
  const source = readFileSync(new URL(`../src/app/console/${file}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { reportDiagnostics: true, compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } });
  assert.deepEqual(compiled.diagnostics.filter(d => d.category === ts.DiagnosticCategory.Error), []);
  const exports = {};
  runInNewContext(compiled.outputText, { exports, require: name => dependencies[name] ?? require(name) });
  return exports;
}

function elements(tree, type) {
  if (!React.isValidElement(tree)) return [];
  return [...(tree.type === type ? [tree] : []), ...React.Children.toArray(tree.props.children).flatMap(child => elements(child, type))];
}

function component(results, states = [false, false, false, "verify"]) {
  let stateCursor = 0, actionCursor = 0;
  const componentModule = compile("foundry-onboard.tsx", {
    react: { ...React,
      useState(initial) { const i = stateCursor++; if (!(i in states)) states[i] = initial; return [states[i], value => { states[i] = typeof value === "function" ? value(states[i]) : value; }]; },
      useActionState() { return [results[actionCursor++] ?? empty, () => {}, false]; },
    },
    "@/components/icon": { Icon: () => null },
    "@/components/orchestration-cube": { OrchestrationCube: () => null },
    "./foundry-actions": {},
  });
  return status => { stateCursor = 0; actionCursor = 0; return componentModule.FoundryOnboard({ status }); };
}

const status = { connected: true, project_endpoint: "https://resource.services.ai.azure.com/api/projects/p", auth: "api" };

test("refresh failure replaces an earlier successful connection result", () => {
  const render = component([
    { ...empty, ok: "Old connection success", checks: [{ label: "OLD CHECK", status: "pass", detail: "old" }] },
    { ...empty, error: "Refresh unavailable" },
  ], [true, false, false, "connect"]);
  let tree = render(status);
  assert.match(renderToStaticMarkup(tree), /OLD CHECK/);
  elements(tree, "form")[0].props.onSubmit();
  tree = render(status);
  const html = renderToStaticMarkup(tree);
  assert.match(html, /Refresh unavailable/);
  assert.doesNotMatch(html, /OLD CHECK|Old connection success/);
});

test("refresh after initial connection or reconnect stays in the connected view", () => {
  for (const refresh of [
    { ...empty, ok: "Foundry verified.", checks: [] },
    { ...empty, error: "Refresh unavailable" },
  ]) {
    const render = component([
      { ...empty, ok: "Connected to Foundry.", checks: [] }, refresh,
    ], [true, false, true, "connect"]);
    let tree = render(status);
    assert.match(renderToStaticMarkup(tree), /Refresh models &amp; services/);
    elements(tree, "form")[0].props.onSubmit();
    tree = render(status);
    const html = renderToStaticMarkup(tree);
    assert.match(html, /Refresh models &amp; services/);
    assert.doesNotMatch(html, /Reconnect &amp; discover|Foundry project URL/);
    assert.match(html, new RegExp(refresh.ok ?? refresh.error));
    elements(tree, "button").find(button => button.props.children === "Edit").props.onClick();
    assert.match(renderToStaticMarkup(render(status)), /Reconnect &amp; discover/);
  }
});

test("only complete discovery can claim an empty project or catalogue publication", () => {
  for (const confirmed of [false, true]) {
    const render = component([empty, { ...empty, checks: [], discovered: {
      models: [], connections: [], models_discovered: confirmed, connections_discovered: confirmed,
    } }]);
    const html = renderToStaticMarkup(render(status));
    if (confirmed) assert.match(html, /No model deployments in this project/);
    else {
      assert.match(html, /Deployment discovery was not confirmed/);
      assert.match(html, /Service discovery was not confirmed/);
      assert.doesNotMatch(html, /No model deployments|No services \(/);
    }
    assert.match(html, /not evidence that Kars has invoked or validated each service/);
    assert.doesNotMatch(html, /catalogue · tagged foundry/);
  }
});

test("project onboarding requests one endpoint and explains credential reuse and memory effects", () => {
  const render = component([], [false, false, true, null]);
  const tree = render({ connected: false });
  assert.deepEqual(elements(tree, "input").map(input => input.props.name), ["project_endpoint", "auth"]);
  const html = renderToStaticMarkup(tree);
  assert.match(html, /matching saved resource key/);
  assert.match(html, /default model stays unchanged/);
  assert.match(html, /does not change controller settings, default models, or existing team memory/);
  assert.match(html, /Services listed here are not automatically enabled/);
});

test("saved services survive a fresh render and show their discovery age", () => {
  const render = component([], [true, false, false, null]);
  const html = renderToStaticMarkup(render({ ...status,
    inference_endpoint: "https://resource.services.ai.azure.com/",
    connections: [{ name: "saved-search", category: "CognitiveSearch" }],
    services_discovered_at: "2026-11-03T00:00:00Z",
  }));
  assert.match(html, /Inference:/);
  assert.match(html, /saved-search/);
  assert.match(html, /Azure AI Search/);
  assert.match(html, /Saved discovery · 2026-11-03T00:00:00Z/);
  assert.match(html, /Refresh to check for changes/);
  assert.match(html, /not evidence that Kars has invoked or validated each service/);
});

test("connection and refresh actions revalidate Configuration and preserve discovery failures", async () => {
  const paths = [], connections = [];
  let fail = false;
  const actions = compile("foundry-actions.ts", {
    "next/cache": { revalidatePath: path => paths.push(path) },
    "@/lib/bff": {
      BffError: class extends Error {},
      connectFoundry: async input => connections.push(input),
      verifyFoundry: async () => {
        if (fail) throw new Error("private diagnostic");
        return { checks: [{ status: "warn", label: "Not complete" }], discovered: { models: [] } };
      },
    },
  });
  const form = new FormData();
  form.set("project_endpoint", status.project_endpoint);
  form.set("auth", "api");
  const first = await actions.connectFoundryAction(empty, form);
  assert.equal(connections[0].api_key, "");
  assert.equal(connections[0].inference_endpoint, undefined);
  assert.match(first.ok, /issues/);
  const refresh = await actions.verifyFoundryAction(empty, form);
  assert.match(refresh.ok, /issues/);
  fail = true;
  const failed = await actions.connectFoundryAction(empty, form);
  assert.match(failed.error, /connection was saved/);
  assert.doesNotMatch(failed.error, /private diagnostic/);
  assert.deepEqual(paths, Array(3).fill("/console/configuration"));
});
