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
const source = readFileSync(new URL("../src/app/console/audit/audit-receipt-row.tsx", import.meta.url), "utf8");
function load(react = React) {
  const imports = {
    react,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/compliance-pack": { CompliancePackView: () => null },
  };
  const context = { exports: {}, require: (name) => {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const { ReceiptControlEvidence } = load();
const fields = ["taskNamespaceFloorVap", "execBanVap", "postureLockVap", "defaultDenyEgress", "floorEnforced"];
const floor = (value) => Object.fromEntries(fields.map((field) => [field, value]));
const result = (completeness, verified = true) => ({
  verified, checks: [], evidence: { signed_statement: { predicate: { completeness } } },
});
const render = (response) => renderToStaticMarkup(React.createElement(ReceiptControlEvidence, { result: response, id: "controls-run" }));

test("r1259-shaped all-false evidence stays NOT VERIFIED despite successful crypto", () => {
  const response = result(floor(false));
  const before = JSON.stringify(response);
  const html = render(response);
  assert.equal((html.match(/NOT VERIFIED \(recorded false\)/g) ?? []).length, 5);
  for (const field of fields) assert.ok(html.includes(`/predicate/completeness/${field}`));
  assert.equal((html.match(/href="#controls-run-source"/g) ?? []).length, 5);
  assert.match(html, /id="controls-run-source"/);
  assert.match(html, /not that enforcement is absent/);
  assert.match(html, /Signature verification does not prove enforcement/);
  assert.match(html, /Receipt cryptographic checks passed; control assertions are not independently verified/);
  assert.match(html, /not a fifth independent control/);
  assert.doesNotMatch(html, /RECORDED TRUE|Inconsistent recorded rollup/);
  assert.equal(JSON.stringify(response), before);
});

test("each individual signed flag is displayed without recomputing or trusting the rollup", () => {
  for (const field of fields) {
    const html = render(result({ ...floor(false), [field]: true }));
    assert.equal((html.match(/RECORDED TRUE — not independently verified/g) ?? []).length, 1);
    assert.equal((html.match(/NOT VERIFIED \(recorded false\)/g) ?? []).length, 4);
    assert.equal(html.includes("Inconsistent recorded rollup"), field === "floorEnforced");
  }
});

test("historical true assertions are not promoted to independent enforcement proof", () => {
  for (const verified of [true, false]) {
    const html = render(result(floor(true), verified));
    assert.equal((html.match(/RECORDED TRUE — not independently verified/g) ?? []).length, 5);
    assert.doesNotMatch(html, /NOT VERIFIED \(recorded false\)|Inconsistent recorded rollup/);
    if (!verified) assert.match(html, /cryptographic checks did not pass/);
  }
});

test("missing or malformed evidence cannot become false observations or true claims", () => {
  for (const value of [undefined, null, [], "true", {}, 1]) {
    const html = render(result(value));
    assert.equal((html.match(/UNKNOWN — field missing/g) ?? []).length, 5);
    assert.doesNotMatch(html, /RECORDED TRUE|NOT VERIFIED \(recorded false\)/);
  }
  for (const value of [null, "false", "true", 0, 1, [], {}]) {
    const html = render(result(floor(value)));
    assert.equal((html.match(/UNKNOWN — invalid field/g) ?? []).length, 5);
    assert.doesNotMatch(html, /RECORDED TRUE|NOT VERIFIED \(recorded false\)/);
  }
  const html = render({ verified: false, checks: [], evidence: {} });
  assert.match(html, /Completeness evidence is missing/);
  assert.doesNotMatch(html, /href="#controls-run-source"/);
});

test("unloaded evidence does not imply observations", () => {
  const html = render(null);
  assert.match(html, /Use Verify now below/);
  assert.match(html, /No control evidence has been loaded yet/);
  assert.doesNotMatch(html, /recorded false|RECORDED TRUE|UNKNOWN —/);
});

test("completeness claims link to the receipt-scoped evidence panel before and after verification", () => {
  for (const response of [null, result(floor(false))]) {
    for (const ns of ["tenant-a", "tenant-b"]) {
      const receipt = {
        namespace: ns, task: "same-task", claims: [{ class: "completeness", status: "PARTIAL", detail: "Recorded claim" }],
        scheme: "DSSEv1+ed25519", key_id: "key", inclusion_seq: 0,
      };
      const states = [true, receipt, null, null, false, false, null, false, response, null];
      let cursor = 0;
      const { AuditReceiptRow } = load({ ...React, useState: () => [states[cursor++], () => {}] });
      const html = renderToStaticMarkup(React.createElement(AuditReceiptRow, {
        ns, task: "same-task", summaryTask: "same-task", summarySeq: 0, summaryVerdict: "partial",
      }));
      assert.equal(cursor, states.length);
      assert.ok(html.includes(`href="#receipt-controls-${ns}:same-task"`));
      assert.ok(html.includes(`id="receipt-controls-${ns}:same-task"`));
      assert.match(html, /Claims partial/);
      assert.doesNotMatch(html, /Claims pass/);
    }
  }
});
