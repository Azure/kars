// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
class Input {
  constructor(value = "", checked = false) { this.value = value; this.checked = checked; }
}
function form() {
  const jsx = (type, props) => ({ type, props });
  const states = ["mission", "RUNBOOK_SIGNING_KEY", "KarsTask", "kars-system", "target-uid"];
  let index = 0;
  const imports = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    react: { useActionState: (_action, initial) => [initial, () => {}, false],
      useState: () => [states[index++], () => {}] },
    "./credential-actions": { putCredentialAction: () => {} },
    "@/lib/credential-review": { credentialReviewMatches: () => false },
  };
  const context = { exports: {}, HTMLInputElement: Input, require(name) {
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(
    "../src/app/console/configuration/credential-form.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports.CredentialForm();
}
function nodes(node) {
  if (Array.isArray(node)) return node.flatMap(nodes);
  if (!node || typeof node !== "object") return [];
  return [node, ...nodes(node.props?.children)];
}

test("action reset preserves reviewed target selection but clears the secret and confirmation", () => {
  const tree = form();
  const value = new Input("private-do-not-retain");
  const confirmed = new Input("on", true);
  const target = new Input("mission");
  const kind = new Input("KarsTask");
  const fields = { value, confirmed, target, kind };
  let cancelled = false;
  tree.props.onReset({ preventDefault() { cancelled = true; },
    currentTarget: { elements: { namedItem: name => fields[name] ?? null } } });
  assert.equal(cancelled, true);
  assert.equal(value.value, "");
  assert.equal(confirmed.checked, false);
  assert.equal(kind.value, "KarsTask");
  assert.equal(target.value, "mission");
  assert.equal(nodes(tree).find(node => node.type === "select").props.value, "KarsTask");
  const password = nodes(tree).find(node => node.props.name === "value");
  assert.equal(password.props.type, "password");
  assert.equal(password.props.value, undefined);
  assert.equal(password.props.defaultValue, undefined);
});

test("initial review reset is safe before a confirmation checkbox exists", () => {
  let cancelled = false;
  form().props.onReset({ preventDefault() { cancelled = true; },
    currentTarget: { elements: { namedItem: () => null } } });
  assert.equal(cancelled, true);
});
