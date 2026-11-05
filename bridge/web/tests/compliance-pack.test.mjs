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
const source = readFileSync(new URL("../src/components/compliance-pack.tsx", import.meta.url), "utf8");
function load(globals = {}) {
  const imports = {
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/icon": { Icon: () => null },
  };
  const context = { ...globals, exports: {}, require: (name) => {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports.CompliancePackView;
}
const control = (status, advisory = false, id = status) => ({
  control_id: id, framework: "EU AI Act", reference: "Article 9 — Risk management",
  receipt_class: advisory ? "regulatory" : "conformance", status,
  evidence: "Recorded assertion, not an enforcement observation", advisory,
});
const pack = (controls) => ({
  task: "mission", namespace: "kars-system", generated_at: "2026-10-28T08:30:00Z",
  predicate_type: "kars", envelope_digest: "sha256:123456789012345678901234567890",
  signature_scheme: "DSSEv1+ed25519", key_id: "cluster-key", inclusion_seq: 268,
  issued_at: "2026-10-28T08:00:00Z", verify_command: "kars receipt verify mission -n kars-system",
  controls, satisfied: 99, partial: 99, advisory: 99,
});
const Component = load();
const render = (value) => renderToStaticMarkup(React.createElement(Component, { pack: value }));

test("mapped claims never imply satisfied obligations or completed verification", () => {
  const value = pack([control("PASS"), control("PARTIAL"), control("OMITTED", true)]);
  const before = JSON.stringify(value);
  const html = render(value);
  assert.match(html, /1 mapped passing claims/);
  assert.match(html, /1 other mapped claims/);
  assert.match(html, /1 advisory mappings/);
  assert.match(html, /Claim PASS/);
  assert.match(html, /Claim PARTIAL/);
  assert.match(html, /Claim OMITTED/);
  assert.match(html, /not control-enforcement proof or regulatory certification/);
  assert.match(html, /does not run receipt verification/);
  assert.match(html, /not an independent trust anchor/);
  assert.doesNotMatch(html, /99|satisfied<|not a self-assessment|Backed by envelope|ROADMAP|never.*block/);
  assert.equal(JSON.stringify(value), before);
});

test("failed, omitted and unknown claims remain distinct, including advisory statuses", () => {
  const value = pack([control("FAIL"), control("OMITTED"), control("UNKNOWN"), control("PASS", true)]);
  const html = render(value);
  assert.match(html, /0 mapped passing claims/);
  assert.match(html, /3 other mapped claims/);
  for (const status of ["FAIL", "OMITTED", "UNKNOWN", "PASS"]) assert.ok(html.includes(`Claim ${status}`));
  assert.doesNotMatch(html, /every receipt|not a gap|Verified.*verdict/);
});

test("empty mappings and absent inclusion do not imply passing evidence", () => {
  const html = render({ ...pack([]), inclusion_seq: null });
  assert.match(html, /0 mapped passing claims/);
  assert.doesNotMatch(html, /99|advisory mappings|entry #|Claim PASS/);
});

test("claim content is escaped and does not change status semantics", () => {
  const html = render(pack([{ ...control("UNKNOWN"), evidence: '<script>alert("PASS")</script>' }]));
  assert.match(html, /&lt;script&gt;/);
  assert.doesNotMatch(html, /<script\b/i);
  assert.match(html, /Claim UNKNOWN/);
});

test("download retains the received pack with explicit interpretation limits", async () => {
  let blob, clicked = false, appended = false, removed = false, revoke;
  const anchor = { style: {}, click: () => { clicked = true; } };
  const View = load({
    Blob,
    URL: { createObjectURL: (value) => { blob = value; return "blob:pack"; }, revokeObjectURL: (url) => { assert.equal(url, "blob:pack"); } },
    document: {
      createElement: (tag) => { assert.equal(tag, "a"); return anchor; },
      body: { appendChild: () => { appended = true; }, removeChild: () => { removed = true; } },
    },
    setTimeout: (fn) => { revoke = fn; },
  });
  const value = pack([control("PASS"), control("OMITTED", true)]);
  const before = JSON.stringify(value);
  function findButton(node) {
    if (!node || typeof node !== "object") return null;
    if (node.type === "button") return node;
    return React.Children.toArray(node.props?.children).map(findButton).find(Boolean);
  }
  findButton(View({ pack: value })).props.onClick();
  assert.ok(clicked && appended && removed);
  assert.equal(anchor.download, "compliance-evidence-mission.json");
  assert.equal(blob.type, "application/json");
  const { interpretation, ...downloaded } = JSON.parse(await blob.text());
  assert.deepEqual(downloaded, value);
  assert.match(interpretation, /recorded claims/);
  assert.match(interpretation, /satisfied.*PASS/);
  assert.match(interpretation, /not control-enforcement proof or regulatory certification/);
  assert.match(interpretation, /does not run receipt verification/);
  assert.equal(JSON.stringify(value), before);
  revoke();
});
