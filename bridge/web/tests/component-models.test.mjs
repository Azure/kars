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
const calls = [];
const refreshes = [];
class BffError extends Error {}
function load(file, reject = false, react = React) {
  const exports = {};
  const source = readFileSync(new URL(`../src/app/console/configuration/${file}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } });
  runInNewContext(compiled.outputText, { exports, require: name => {
    if (name === "react") return react;
    if (name === "@/lib/bff") return { BffError, setComponentModel: async (...args) => {
      calls.push(JSON.parse(JSON.stringify(args)));
      if (reject) throw new BffError("Policy changed; refresh");
      return { note: "Saved, awaiting router acknowledgement" };
    } };
    if (name === "next/cache") return { revalidatePath: path => refreshes.push(path) };
    if (name === "next/navigation") return { useRouter: () => ({ refresh() {} }) };
    if (name === "./component-model-actions") return { saveComponentModel: async () => assert.fail("Rendering cannot save") };
    return require(name);
  } });
  return exports;
}
const selected = { provider: "azure-openai", deployment: "astra" };
const policy = { component: "sre", policy: "sre-inference", mode: "inherit", desired: selected, primary: selected, applied: false, uid: "identity", resource_version: "7", editable: true, conditions: [] };

test("component selectors expose inheritance and provider-qualified overrides with honest pending state", () => {
  const html = renderToStaticMarkup(React.createElement(load("component-models.tsx").ComponentModels, { components: [policy], clusterDefault: selected, models: [selected, { provider: "other", deployment: "astra" }] }));
  assert.match(html, /Inherit cluster default/);
  assert.match(html, /value="azure-openai::astra"/);
  assert.match(html, /value="other::astra"/);
  assert.match(html, /Pending — not yet confirmed/);
  assert.doesNotMatch(html, /Applied — router acknowledged/);
  assert.match(html, /no installed inference policy/);
  assert.match(html, /name="resource_version" value="7"/);
});

test("applied display comes from current backend evidence and managed policies cannot be edited", () => {
  const html = renderToStaticMarkup(React.createElement(load("component-models.tsx").ComponentModels, { components: [{ ...policy, applied: true, editable: false }], clusterDefault: selected, models: [selected] }));
  assert.match(html, /Applied — router acknowledged/);
  assert.match(html, /signed bundle/);
  assert.match(html, /<select[^>]*disabled/);
  assert.match(html, /<button[^>]*disabled/);
});

test("component forms prevent action resets from replacing controlled drafts", () => {
  const hooks = {
    ...React,
    useEffect() {},
    useState: () => [{ selection: "other::astra", version: "6" }, () => {}],
    useActionState: () => [{ error: "Policy changed; refresh", ok: null }, () => {}, false],
  };
  const grid = load("component-models.tsx", false, hooks).ComponentModels({ components: [policy], clusterDefault: selected, models: [selected] });
  const card = grid.props.children[1];
  const form = card.type(card.props);
  assert.equal(form.type, "form");
  let prevented = false;
  form.props.onReset({ preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  const version = form.props.children.find(child => child?.props?.name === "resource_version");
  assert.equal(version.props.value, "6");
});

test("actions preserve selection mode and concurrency identity and report errors rather than success", async () => {
  const form = new FormData();
  for (const [key, value] of Object.entries({ component: "sre", selection: "inherit", uid: "identity", resource_version: "7" })) form.set(key,value);
  const action = load("component-model-actions.ts").saveComponentModel;
  assert.equal((await action({},form)).error,null);
  assert.deepEqual(calls.at(-1), ["sre",{ mode:"inherit",uid:"identity",resource_version:"7" }]);
  form.set("selection","azure-openai::astra");
  assert.equal((await action({},form)).error,null);
  assert.deepEqual(calls.at(-1)[1].model,selected);
  assert.equal(calls.at(-1)[1].mode,"override");
  assert.ok(refreshes.includes("/console/policies"));
  const failed = await load("component-model-actions.ts",true).saveComponentModel({},form);
  assert.match(failed.error,/Policy changed/); assert.equal(failed.ok,null);
  form.set("component","reviewed-task");
  const before = calls.length;
  assert.ok((await action({},form)).error);
  assert.equal(calls.length,before);
});
