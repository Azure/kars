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
const source = readFileSync(new URL("../src/app/console/configuration/provider-wizard.tsx", import.meta.url), "utf8");
const props = {
  hasDefaultProvider: true,
  defaultProvider: { id: "azure-foundry", label: "Azure AI Foundry", note: "Configured", models: ["gpt-4.1"] },
  additionalProviders: [], localInferenceStatus: null, localDeployments: [],
  foundryStatus: { connected: false },
};

function load(states, action, discover = async () => assert.fail("Unexpected discovery")) {
  let cursor = 0;
  const actions = [];
  const exports = {};
  runInNewContext(ts.transpileModule(source + "\nexport { ProviderWizardSession };", {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports, URL,
    require(name) {
      if (name === "react") return { ...React,
        useState(initial) {
          const i = cursor++;
          if (!(i in states)) states[i] = initial;
          return [states[i], value => { states[i] = typeof value === "function" ? value(states[i]) : value; }];
        },
        useTransition: () => [false, fn => fn()],
        useActionState: (fn, initial) => { actions.push(fn); return [initial, fn, false]; },
      };
      if (name === "./provider-discover-actions") return { discoverModelsAction: discover };
      if (name === "./provider-actions") return { onboardProviderAction: action };
      if (name === "./additional-provider-actions") return { addAdditionalProviderAction: action };
      if (name.startsWith(".") || name.startsWith("@/")) return new Proxy({}, {
        get: () => ({ children }) => React.createElement("span", null, children),
      });
      return require(name);
    },
  });
  return { actions, render(name, values) { cursor = 0; return exports[name](values); } };
}

function review() {
  return [true, 3, "github-copilot", "additional", "github-copilot", "", "workload", "", "",
    [{ id: "gpt-4.1", label: "GPT 4.1" }], new Set(["gpt-4.1"]), null, true];
}

test("Copilot review describes the remaining model save and uses a neutral button", () => {
  const f = load(review(), async () => assert.fail("Rendering cannot save"));
  const html = renderToStaticMarkup(f.render("ProviderWizardSession", { ...props, onClose() {}, onSaved() {} }));
  assert.match(html, /Save model selection/);
  assert.match(html, /GitHub sign-in is complete/);
  assert.match(html, /GitHub sign-in \(stored on cluster\)/);
  assert.match(html, /This does not change the cluster default/);
  assert.doesNotMatch(html, />Connect provider<\/button>/);
  const submit = html.match(/<button type="submit"[^>]*>/)?.[0];
  assert.ok(submit);
  assert.doesNotMatch(submit, /bg-signal|text-signal-fg/);
});

test("Next reaches model review without activating the replacement submit button", () => {
  const states = review();
  states[1] = 2;
  const f = load(states, async () => assert.fail("Navigation cannot save"));
  const values = { ...props, onClose() {}, onSaved() { assert.fail("Navigation cannot complete"); } };
  const buttons = tree => {
    if (!React.isValidElement(tree)) return [];
    return [ ...(tree.type === "button" ? [tree] : []),
      ...React.Children.toArray(tree.props.children).flatMap(buttons) ];
  };
  const next = buttons(f.render("ProviderWizardSession", values)).find(button => button.props.children === "Next →");
  assert.equal(next.props.type, "button");
  assert.equal(next.props.disabled, false);
  let prevented = false;
  next.props.onClick({ preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  assert.equal(states[1], 3);
  const save = buttons(f.render("ProviderWizardSession", values)).find(button => button.props.type === "submit");
  assert.equal(save.props.children, "Save model selection");
  assert.ok(next.key);
  assert.ok(save.key);
  assert.notEqual(next.key, save.key, "React must not reuse the clicked Next DOM node as a submit button");
});

test("successful save completes the wizard; failed and unconfirmed saves do not discard selection", async () => {
  for (const outcome of ["success", "denied", "transport"]) {
    const states = review();
    const saved = [];
    const f = load(states, async (_previous, form) => {
      assert.equal(form.get("models"), "gpt-4.1");
      if (outcome === "transport") throw new Error("PRIVATE");
      return outcome === "success" ? { error: null, ok: "Selection saved." } : { error: "Denied.", ok: null };
    });
    f.render("ProviderWizardSession", { ...props, onClose() {}, onSaved: message => saved.push(message) });
    const form = new FormData();
    form.set("models", "gpt-4.1");
    const result = await f.actions[1]({ error: null, ok: null }, form);
    assert.equal(saved.length, outcome === "success" ? 1 : 0);
    assert.equal(states[1], 3);
    assert.deepEqual([...states[10]], ["gpt-4.1"]);
    if (outcome === "transport") {
      assert.match(result.error, /Couldn't confirm/);
      assert.doesNotMatch(result.error, /PRIVATE/);
    }
  }
});

test("completion remounts wizard state and shows the saved outcome outside the form", () => {
  const f = load([], async () => assert.fail("No save while rendering"));
  let tree = f.render("ProviderWizard", props);
  const initial = tree.props.children[1];
  initial.props.onSaved("Selection saved.");
  tree = f.render("ProviderWizard", props);
  assert.notEqual(tree.props.children[1].key, initial.key);
  assert.equal(tree.props.children[0].props.role, "status");
  assert.equal(tree.props.children[0].props.children, "Selection saved.");
  tree.props.children[1].props.onClose();
  tree = f.render("ProviderWizard", props);
  assert.equal(tree.props.children[0], null);
});

function elements(tree, type) {
  if (!React.isValidElement(tree)) return [];
  return [...(tree.type === type ? [tree] : []), ...React.Children.toArray(tree.props.children).flatMap(child => elements(child, type))];
}

function clickNext(fixture, values) {
  const next = elements(fixture.render("ProviderWizardSession", values), "button").find(button => button.props.children === "Next →");
  assert.equal(next.props.disabled, false);
  next.props.onClick({ preventDefault() {} });
}

test("GitHub Models submits its displayed endpoint and keeps post-install changes off bootstrap", () => {
  const states = [];
  const f = load(states, async () => assert.fail("Navigation cannot save"));
  const values = { ...props, onClose() {}, onSaved() {} };
  elements(f.render("ProviderWizardSession", values), "button").find(button => button.props.children === "+ Connect a provider").props.onClick();
  elements(f.render("ProviderWizardSession", values), "button").find(button => renderToStaticMarkup(button).includes("GitHub Models")).props.onClick();
  assert.equal(states[5], "https://models.github.ai/inference");
  let tree = f.render("ProviderWizardSession", values);
  assert.equal(elements(tree, "input").find(input => input.props.type === "radio" && input.props.checked === false).props.disabled, true);
  clickNext(f, values);
  tree = f.render("ProviderWizardSession", values);
  assert.equal(elements(tree, "button").find(button => button.props.children === "Next →").props.disabled, true, "new token authentication needs a token");
  states[7] = "test-only-token";
  clickNext(f, values);
  tree = f.render("ProviderWizardSession", values);
  assert.equal(elements(tree, "input").find(input => input.props.name === "endpoint").props.value, "https://models.github.ai/inference");
});

test("existing Azure connection edits retain its tag, endpoint, models and blank credential", () => {
  const states = [];
  const f = load(states, async () => assert.fail("Opening editor cannot save"));
  const provider = { tag: "azure-openai", endpoint: "https://azure.example", has_key: true, models: ["astra", "mini"] };
  const values = { ...props, additionalProviders: [provider], onClose() {}, onSaved() {} };
  let tree = f.render("ProviderWizardSession", values);
  tree.props.children[0].props.children[1][0].props.onEditModels();
  assert.equal(states[13], provider.tag);
  assert.equal(states[5], provider.endpoint);
  assert.equal(states[7], "");
  assert.deepEqual([...states[10]], provider.models);
  tree = f.render("ProviderWizardSession", values);
  assert.equal(elements(tree, "input").find(input => input.props.value === provider.tag && input.props.type !== "hidden").props.readOnly, true);
  assert.doesNotMatch(renderToStaticMarkup(tree), /Already connected/);
  clickNext(f, values);
  assert.match(renderToStaticMarkup(f.render("ProviderWizardSession", values)), /leave blank to keep the stored credential/);
  clickNext(f, values);
  assert.equal(states[1], 3);
});

test("Azure refresh previews new deployments without saving or dropping existing selections", async () => {
  for (const failure of [false, true]) {
    const states = [];
    const calls = [];
    const f = load(states, async () => assert.fail("Refresh must not save"), async request => {
      calls.push(request);
      return failure ? { models: null, error: "Discovery unavailable" } : { models: [{ id: "new-model" }], error: null };
    });
    const provider = { tag: "azure-openai", endpoint: "https://resource.openai.azure.com/", has_key: true, models: ["saved-model"] };
    const values = { ...props, additionalProviders: [provider], onClose() {}, onSaved() { assert.fail("Refresh cannot complete a save"); } };
    const tree = f.render("ProviderWizardSession", values);
    assert.match(renderToStaticMarkup(tree), /Foundry models &amp; services/);
    tree.props.children[0].props.children[1][0].props.onRefreshModels();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(calls.length, 1);
    assert.equal(calls[0].kind, "azure-openai");
    assert.equal(calls[0].endpoint, provider.endpoint);
    assert.equal(calls[0].key, undefined);
    assert.equal(states[1], 3);
    assert.deepEqual([...states[10]], ["saved-model"]);
    assert.ok(states[9].some(model => model.id === "saved-model"));
    if (failure) assert.equal(states[11], "Discovery unavailable");
    else {
      assert.ok(states[9].some(model => model.id === "new-model"));
      assert.match(states[9].find(model => model.id === "saved-model").label, /not returned/);
    }
    const refresh = elements(f.render("ProviderWizardSession", values), "button").find(button => button.props.children === "Refresh models");
    assert.equal(refresh.props.disabled, false, "Saved-key refresh must not require key reentry");
  }
});

test("new additional Azure Workload Identity connection does not require an API key", () => {
  const states = review();
  states[1] = 2;
  states[2] = "azure-openai";
  states[4] = "azure-openai";
  states[5] = "https://azure.example";
  states[12] = false;
  const f = load(states, async () => assert.fail("Navigation cannot save"));
  clickNext(f, { ...props, onClose() {}, onSaved() {} });
  assert.equal(states[1], 3);
  assert.equal(states[7], "");
});

test("connected Copilot models can be edited without another login and without hiding Foundry", () => {
  const states = [];
  const f = load(states, async () => assert.fail("Opening the editor cannot save"));
  const values = { ...props,
    defaultProvider: { id: "github-copilot", label: "GitHub Copilot", note: "Configured", models: ["gpt-4.1"] },
    additionalProviders: [
      { tag: "github-copilot", endpoint: "https://api.githubcopilot.com", has_key: true, models: ["gpt-4.1"] },
      { tag: "azure-foundry", endpoint: "https://example.cognitiveservices.azure.com", has_key: true, models: ["gpt-4.1"] },
    ],
    onClose() {}, onSaved() {},
  };
  const tree = f.render("ProviderWizardSession", values);
  const list = tree.props.children[0];
  const defaultRow = list.props.children[0];
  assert.equal(list.props.children[1][0].props.p.tag, "azure-foundry");
  defaultRow.props.onEditModels();
  assert.equal(states[0], true);
  assert.equal(states[1], 3);
  assert.equal(states[2], "github-copilot");
  assert.equal(states[7], "");
  assert.equal(states[12], true);
  assert.deepEqual([...states[10]], ["gpt-4.1"]);
});
