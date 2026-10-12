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
const source = readFileSync(new URL("../src/app/console/mcp-profiles.tsx", import.meta.url), "utf8");
const actionSource = readFileSync(new URL("../src/app/console/mcp-profile-actions.ts", import.meta.url), "utf8");
const servers = [{ name: "everything", namespace: "kars-system" }];

function compile(source, dependency) {
  const exports = {};
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: dependency });
  return exports;
}

function load({ initial, pending = false, outcome = { error: null, ok: null }, save } = {}) {
  const states = [initial?.servers ?? ["everything"]];
  let cursor = 0;
  const action = save ?? (async () => assert.fail("No mutation expected"));
  const exports = compile(source + "\nexport { ProfileForm };", name => {
    if (name === "react") return { ...React,
      useState(value) {
        const i = cursor++;
        if (!(i in states)) states[i] = value;
        return [states[i], next => { states[i] = typeof next === "function" ? next(states[i]) : next; }];
      },
      useActionState: () => [outcome, action, pending],
    };
    if (name === "./mcp-profile-actions") return { saveMcpProfileAction: action, deleteMcpProfileAction: action };
    if (name === "@/components/icon") return { Icon: () => null };
    return require(name);
  });
  return onClose => {
    cursor = 0;
    return exports.ProfileForm({ initial, servers, onClose });
  };
}

function descendants(node) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap(descendants);
  return [node, ...descendants(node.props?.children)];
}

function submittedForm(tree, enteredName = "research") {
  const form = new FormData();
  for (const node of descendants(tree).filter(n => n.type === "input")) {
    const p = node.props;
    if (!p.name || p.disabled || (p.type === "checkbox" && !p.checked)) continue;
    form.append(p.name, p.type === "hidden" || p.type === "checkbox" ? p.value
      : p.value ?? (p.name === "name" ? enteredName : p.defaultValue ?? ""));
  }
  return form;
}

test("new profile submits one entered name through the existing server action", async () => {
  const writes = [];
  const refreshed = [];
  const actions = compile(actionSource, name => {
    if (name === "next/cache") return { revalidatePath: path => refreshed.push(path) };
    if (name === "@/lib/bff") return {
      BffError: class extends Error {},
      putMcpProfile: async profile => writes.push(JSON.parse(JSON.stringify(profile))),
      deleteMcpProfile: async () => assert.fail("Unexpected deletion"),
    };
    return require(name);
  });
  const render = load();
  const nameInput = descendants(render(() => {})).find(n => n.type === "input" && n.props.name === "name" && n.props.type !== "hidden");
  nameInput.props.onChange?.({ target: { value: "research" } });
  const form = submittedForm(render(() => {}));
  assert.deepEqual(form.getAll("name"), ["research"]);
  const result = await actions.saveMcpProfileAction({ error: null, ok: null }, form);
  assert.equal(result.error, null);
  assert.deepEqual(writes, [{ name: "research", summary: null, servers: ["everything"] }]);
  assert.deepEqual(refreshed, ["/console/configuration", "/console/capabilities"]);
});

test("editing preserves the exact profile name and selected servers", () => {
  const initial = { name: "reviewed-profile", summary: "Reviewed", servers: ["everything"] };
  const form = submittedForm(load({ initial })(() => {}), "must-not-rename");
  assert.deepEqual(form.getAll("name"), ["reviewed-profile"]);
  assert.deepEqual(form.getAll("servers"), ["everything"]);
  assert.equal(form.get("summary"), "Reviewed");
});

test("a saved profile has an explicit completion control instead of an endless refreshing state", () => {
  let closed = false;
  const tree = load({ outcome: { error: null, ok: "Profile saved." } })(() => { closed = true; });
  const html = renderToStaticMarkup(tree);
  assert.match(html, /role="status"/);
  assert.doesNotMatch(html, /refreshing/);
  const done = descendants(tree).find(n => n.type === "button" && n.props.children === "Done");
  assert.ok(done);
  done.props.onClick();
  assert.equal(closed, true);
});

test("pending requests lock inputs and cancellation; server selection remains keyboard accessible", () => {
  const html = renderToStaticMarkup(load({ pending: true })(() => {}));
  assert.match(html, /aria-busy="true"/);
  assert.match(html, /<fieldset disabled=""/);
  assert.match(html, /type="checkbox"[^>]*class="sr-only"/);
  assert.doesNotMatch(html, /type="checkbox"[^>]*class="hidden"/);
  assert.match(html, /<button type="button" disabled=""[^>]*>Cancel/);
});

test("failed submissions keep the editable draft and announce the actual error", () => {
  const initial = { name: "research", summary: "Keep this draft", servers: ["everything"] };
  const tree = load({ initial, outcome: { error: "Server no longer registered.", ok: null } })(() => {});
  const html = renderToStaticMarkup(tree);
  assert.match(html, /role="alert"/);
  assert.match(html, /Server no longer registered/);
  assert.equal(submittedForm(tree).get("summary"), "Keep this draft");
  assert.deepEqual(submittedForm(tree).getAll("servers"), ["everything"]);
});

test("typed create and edit drafts survive a failed action instead of React form reset", () => {
  for (const initial of [undefined, { name: "research", summary: "Original", servers: ["everything"] }]) {
    const outcome = { error: null, ok: null };
    const render = load({ initial, outcome });
    const tree = render(() => {});
    const inputs = descendants(tree).filter(n => n.type === "input");
    const summary = inputs.find(n => n.props.name === "summary");
    assert.equal(typeof summary.props.onChange, "function");
    summary.props.onChange({ target: { value: "Keep the typed draft" } });
    if (!initial) {
      const name = inputs.find(n => n.props.name === "name" && n.props.type !== "hidden");
      assert.equal(typeof name.props.onChange, "function");
      name.props.onChange({ target: { value: "INVALID_NAME" } });
    }
    outcome.error = "Save rejected.";
    const submitted = submittedForm(render(() => {}));
    assert.equal(submitted.get("summary"), "Keep the typed draft");
    assert.equal(submitted.get("name"), initial?.name ?? "INVALID_NAME");
  }
});
