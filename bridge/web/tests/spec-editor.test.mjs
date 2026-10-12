// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const root = new URL("../src/app/console/", import.meta.url);

function harness(imports = {}) {
  const states = [];
  let cursor = 0;
  let actionState = { error: null, ok: null };
  const react = { useState(initial) {
    const index = cursor++;
    if (!(index in states)) states[index] = typeof initial === "function" ? initial() : initial;
    return [states[index], value => {
      states[index] = typeof value === "function" ? value(states[index]) : value;
    }];
  }, useActionState() { return [actionState, () => {}, false]; } };
  function load(name, privateFunctions = false) {
    const exports = {};
    const code = ts.transpileModule(readFileSync(new URL(name, root), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText + (privateFunctions ? "\nObject.assign(exports, {parseSpec, buildSpec});" : "");
    runInNewContext(code, { exports, require: name => {
      if (name in imports) return imports[name];
      if (name === "react") return react;
      if (name === "react/jsx-runtime") return require(name);
      if (name === "./use-spec-editor") return { useSpecEditor: hook };
      return {};
    }, console });
    return exports;
  }
  const hook = load("use-spec-editor.ts").useSpecEditor;
  return {
    load,
    setActionState(value) { actionState = value; },
    component(Component, props) {
      cursor = 0;
      return Component(props);
    },
    render(initial, parse, build, jsonOnly = false) {
      cursor = 0;
      return hook(initial, parse, build, jsonOnly);
    },
  };
}

function elements(node) {
  if (Array.isArray(node)) return node.flatMap(elements);
  if (!node || typeof node !== "object" || !node.props) return [];
  return [node, ...elements(node.props.children)];
}

for (const [file, component, label, props] of [
  ["inference-policy-editor.tsx", "InferencePolicyEditor", "inference policy name", { models: [] }],
  ["mcp-server-editor.tsx", "McpServerEditor", "MCP server name", {}],
  ["profile-editor.tsx", "ProfileEditor", "team profile name", { toolPolicies: [], skills: [] }],
]) {
  test(`${file}: rejected saves preserve the name, specification and explicit force choice`, () => {
    const h = harness();
    const Component = h.load(file)[component];
    const render = () => elements(h.component(Component, props));
    render().find(node => node.type === "button").props.onClick();
    let nodes = render();
    const field = name => nodes.find(node => node.type === "input" && node.props.name === name);
    field("name").props.onChange({ target: { value: "beta-policy-retry" } });
    field("force").props.onChange({ target: { checked: true } });
    nodes = render();
    const specification = field("spec").props.value;

    h.setActionState({ error: "The cluster rejected this specification.", ok: null });
    nodes = render();
    assert.equal(field("name").props.value, "beta-policy-retry");
    assert.equal(nodes.find(node => node.props["aria-haspopup"] === "dialog").props["aria-expanded"], true);
    nodes.find(node => node.type === "button" && node.props.children === "Cancel").props.onClick();
    nodes = render();
    assert.equal(nodes.some(node => node.type === "form"), false);
    nodes.find(node => node.props["aria-haspopup"] === "dialog").props.onClick();
    nodes = render();
    assert.equal(field("name").props.value, "beta-policy-retry");
    assert.equal(field("spec").props.value, specification);
    assert.equal(field("name").props.defaultValue, undefined);
    assert.equal(field("name").props["aria-label"], label);
    assert.equal(field("force").props.checked, true);
    assert.equal(field("spec").props.value, specification);
    assert.equal(nodes.find(node => node.props.role === "alert").props.children,
      "The cluster rejected this specification.");

    h.setActionState({ error: null, ok: "Policy applied." });
    nodes = render();
    assert.equal(nodes.find(node => node.props.role === "status").props.children, "Policy applied.");
    assert.equal(field("name").props.value, "beta-policy-retry");
  });

  test(`${file}: editing keeps the existing resource identity read-only`, () => {
    const h = harness();
    const Component = h.load(file)[component];
    const render = () => elements(h.component(Component, { ...props, initialName: "existing-policy" }));
    render().find(node => node.type === "button").props.onClick();
    const input = render().find(node => node.type === "input" && node.props.name === "name");
    assert.equal(input.props.value, "existing-policy");
    assert.equal(input.props.readOnly, true);
  });
}

test("governance dialogs use native modality, restore focus and prevent dismissal while saving", () => {
  const exports = {};
  const effects = [];
  const ref = { current: null };
  const events = [];
  class Element { focus() { events.push("focus-trigger"); } }
  const document = { activeElement: new Element(), body: { style: { overflow: "auto" } } };
  const code = ts.transpileModule(readFileSync(new URL("governance-dialog.tsx", root), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  runInNewContext(code, {
    exports, document, HTMLElement: Element,
    require: name => name === "react"
      ? { useRef: () => ref, useEffect: effect => effects.push(effect) }
      : require(name),
  });
  const props = { title: "Edit inference policy", pending: true, onClose: () => events.push("dismiss") };
  const dialog = exports.GovernanceDialog(props);
  assert.equal(dialog.type, "dialog");
  assert.equal(dialog.props["aria-label"], props.title);
  assert.equal(dialog.props["aria-busy"], true);
  ref.current = { showModal() { events.push("show-modal"); }, close() { events.push("close-modal"); } };
  const cleanup = effects[0]();
  assert.equal(document.body.style.overflow, "hidden");
  dialog.props.onCancel({ preventDefault() { events.push("prevent-default"); } });
  assert.deepEqual(events, ["show-modal", "prevent-default"]);
  exports.GovernanceDialog({ ...props, pending: false }).props.onCancel({
    preventDefault() { events.push("prevent-default"); },
  });
  assert.equal(events.at(-1), "dismiss");
  cleanup();
  assert.equal(document.body.style.overflow, "auto");
  assert.deepEqual(events.slice(-2), ["close-modal", "focus-trigger"]);
});

for (const [name, spec] of [
  ["profile-editor.tsx", {
    displayName: "Authored profile", domain: "beta-testing", charterTemplate: "Keep human review",
    defaultEnvelope: { tier: 3, authorityCeiling: 2, delegationDepth: 1 },
    roles: [{ name: "writer", systemPrompt: "Write a checklist", skills: ["checked-skill"] }],
  }],
  ["mcp-server-editor.tsx", {
    url: "https://mcp.example.internal", displayName: "Authored MCP", allowedTools: ["read"],
    allowedSandboxes: { matchLabels: { team: "isolated" } }, productionMode: true,
    oauth: { issuer: "https://issuer.example", audience: "mcp" }, scopes: ["read"],
  }],
  ["inference-policy-editor.tsx", {
    displayName: "Authored policy", appliesTo: { sandboxName: "isolated", sandboxMatchLabels: {} },
    tokenBudget: { dailyTokens: 1000, perRequestTokens: 50 },
    contentSafety: { hate: "Low", requirePromptShields: true },
    modelPreference: { primary: { provider: "foundry", deployment: "reviewed-model" } },
  }],
]) {
  test(`${name}: visual edits and raw edits survive both mode transitions`, () => {
    const h = harness();
    const { parseSpec, buildSpec } = h.load(name, true);
    const render = () => h.render(undefined, parseSpec, buildSpec);
    let editor = render();
    editor.setF(parseSpec(spec));
    editor = render();
    const submitted = editor.specJson;
    editor.toggleMode();
    editor = render();
    assert.equal(editor.advanced, true);
    assert.deepEqual(JSON.parse(editor.rawSpec), JSON.parse(submitted));
    editor.setRawSpec(JSON.stringify({ ...spec, displayName: "Changed in JSON" }));
    editor = render();
    editor.toggleMode();
    editor = render();
    assert.equal(editor.advanced, false);
    assert.equal(editor.f.displayName, "Changed in JSON");
    assert.equal(editor.modeError, null);
    assert.equal(JSON.parse(editor.specJson).displayName, "Changed in JSON");
  });

  test(`${name}: unsupported, malformed and wrong-type JSON remains intact`, () => {
    for (const draft of ['{"unfinished":', "null", "[]",
      JSON.stringify({ ...spec, futureAuthority: { permit: false } }),
      JSON.stringify({ ...spec, roles: "not-an-array", allowedTools: "not-an-array" }),
    ]) {
      const h = harness();
      const { parseSpec, buildSpec } = h.load(name, true);
      const render = () => h.render(undefined, parseSpec, buildSpec);
      let editor = render();
      editor.toggleMode();
      editor = render();
      editor.setRawSpec(draft);
      editor = render();
      editor.toggleMode();
      editor = render();
      assert.equal(editor.advanced, true);
      assert.equal(editor.rawSpec, draft);
      assert.equal(editor.specJson, draft);
      assert.ok(editor.modeError);
    }
  });
}

test("existing nested authority settings and managed MCP specifications cannot be silently stripped", () => {
  const h = harness();
  const { parseSpec, buildSpec } = h.load("profile-editor.tsx", true);
  const initial = { domain: "beta", charterTemplate: "Keep constraints",
    defaultEnvelope: { tier: 3, authorityCeiling: 2, delegationDepth: 1, budget: { tokens: 50 } } };
  const editor = h.render(initial, parseSpec, buildSpec);
  assert.equal(editor.advanced, true);
  assert.deepEqual(JSON.parse(editor.specJson), initial);
  const m = harness();
  const mcp = m.load("mcp-server-editor.tsx", true);
  const managed = { managed: { preset: "reviewed-preset" } };
  let locked = m.render(managed, mcp.parseSpec, mcp.buildSpec, true);
  locked.toggleMode();
  locked = m.render(managed, mcp.parseSpec, mcp.buildSpec, true);
  assert.equal(locked.advanced, true);
  assert.equal(locked.modeError, null);
  assert.deepEqual(JSON.parse(locked.specJson), managed);
});

test("profile editor blocks self-amplifying envelopes without discarding the draft", () => {
  const h = harness();
  const { ProfileEditor } = h.load("profile-editor.tsx");
  const props = { toolPolicies: [], skills: [], initialName: "bounded-profile", initialSpec: {
    domain: "docs", charterTemplate: "Review supplied documents",
    defaultEnvelope: { tier: 3, authorityCeiling: 4, delegationDepth: 1 },
    roles: [{ name: "reviewer", systemPrompt: "Review only" }],
  } };
  const render = () => elements(h.component(ProfileEditor, props));
  render().find(node => node.type === "button").props.onClick();
  const nodes = render();
  assert.equal(nodes.find(node => node.type === "button" && node.props.type === "submit").props.disabled, true);
  assert.match(String(nodes.find(node => node.props.role === "alert").props.children), /ceiling.*tier/i);
  assert.equal(JSON.parse(nodes.find(node => node.props.name === "spec").props.value).defaultEnvelope.authorityCeiling, 4);
  assert.ok(nodes.find(node => node.type === "button" && node.props["aria-label"] === "Remove role reviewer"));
});

test("profile editor keeps unnamed roles instead of silently deleting their instructions", () => {
  const h = harness();
  const { buildSpec, parseSpec } = h.load("profile-editor.tsx", true);
  const spec = buildSpec(parseSpec({ roles: [{ name: "", systemPrompt: "Keep these instructions" }] }));
  assert.equal(spec.roles.length, 1);
  assert.equal(spec.roles[0].systemPrompt, "Keep these instructions");
});

test("profile saves enforce controller envelope semantics for visual and raw JSON callers", async () => {
  const calls = [];
  const h = harness({
    "next/cache": { revalidatePath() {} },
    "@/lib/bff": { applyGovernance: async (...args) => {
      calls.push(args); return { kind: "KarsProfile", name: "bounded-profile", note: "applied" };
    } },
  });
  const { applyGovernanceAction } = h.load("governance-actions.ts");
  const valid = { domain: "docs", charterTemplate: "Review supplied documents",
    defaultEnvelope: { tier: 3, authorityCeiling: 2, delegationDepth: 1 },
    futureSetting: { preserved: true },
  };
  const submit = (spec) => applyGovernanceAction({}, new Map([
    ["kind", "KarsProfile"], ["name", "bounded-profile"], ["spec", JSON.stringify(spec)],
  ]));
  for (const spec of [
    { ...valid, domain: "" }, { ...valid, charterTemplate: " " },
    { ...valid, defaultEnvelope: null },
    ...[{ tier: 6 }, { tier: 1.5 }, { authorityCeiling: 0 }, { authorityCeiling: 4 }, { delegationDepth: -1 }, { delegationDepth: 2147483648 }]
      .map(patch => ({ ...valid, defaultEnvelope: { ...valid.defaultEnvelope, ...patch } })),
  ]) {
    const result = await submit(spec);
    assert.ok(result.error);
    assert.equal(result.ok, null);
  }
  assert.equal(calls.length, 0);
  const result = await submit(valid);
  assert.equal(result.error, null);
  assert.match(result.ok, /Saving is not validation/);
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0][1].spec)), valid);
});
