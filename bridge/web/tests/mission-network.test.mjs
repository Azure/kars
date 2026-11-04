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
const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const source = read("../src/app/workspace/missions/[name]/network-mode.tsx");

function render(mode, data = null, loading = true) {
  const exports = {};
  const states = [data, loading];
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports,
    require: (name) => name === "react" ? {
      ...React, useEffect: () => {}, useState: () => [states.shift(), () => {}],
    } : name === "@/components/icon" ? { Icon: () => null } : require(name),
  });
  return renderToStaticMarkup(React.createElement(exports.NetworkMode, { ns: "work", task: "review", mode }));
}

test("Mission intake submits Strict and keeps reviewed hosts editable", () => {
  const intake = read("../src/app/workspace/new/intake-flow.tsx");
  const review = read("../src/app/workspace/new/intake-flow/review.tsx");
  const types = read("../src/app/workspace/new/intake-flow/review-types.ts");
  assert.match(intake, /bp\.egress_mode = "strict"/);
  assert.match(intake, /if \(egress\.length\) bp\.egress = egress/);
  assert.match(review, /<EgressEditor egress=\{egress\} onChange=\{setEgress\}/);
  assert.match(review, /Datapath observations do not grant access/);
  for (const text of [intake, review, types]) assert.doesNotMatch(text, /setEgressMode|egressMode/);
});

test("Mission network panel reports Strict without offering a fictitious mode switch", () => {
  const html = render("Strict");
  assert.match(html, /Reported: Strict/);
  assert.match(html, /New destinations require approved access/);
  assert.match(html, /Reading network evidence/);
  assert.doesNotMatch(html, /<button|<input|Back to learning/);
  assert.doesNotMatch(source, /egress-mode|method: "POST"|function flip/);
});

test("unknown and unexpected modes are not presented as Learning or verified enforcement", () => {
  assert.match(render(null), /Mode unavailable/);
  assert.match(render("Learn"), /Unexpected mode: Learn/);
  for (const mode of [null, "Learn", "Unknown"]) {
    const html = render(mode);
    assert.doesNotMatch(html, /Reported: Strict|can reach the network|nothing is blocked/);
  }
});

test("an empty reported baseline remains Strict, not Learning", () => {
  const html = render("Strict", { available: true, enforced: [], domains: [] }, false);
  assert.match(html, /An empty baseline does not enable Learning/);
  assert.match(html, /excludes temporary grants/);
  assert.match(html, /does not confirm the router has installed a change/);
});

test("missing evidence and observed domains do not establish successful enforcement", () => {
  const failed = render("Strict", { available: false }, false);
  assert.match(failed, /Network evidence unavailable/);
  assert.doesNotMatch(failed, /No baseline destinations reported/);
  const partial = render("Strict", { available: false, enforced: ["known.example.com:443"] }, false);
  assert.match(partial, /Network evidence unavailable/);
  assert.match(partial, /known.example.com:443/);
  const observed = render("Strict", { available: true, enforced: ["docs.example.com:443"], domains: ["unreviewed.example.com"] }, false);
  assert.match(observed, /docs.example.com:443/);
  assert.match(observed, /unreviewed.example.com/);
  assert.match(observed, /do not establish whether a connection was allowed or denied/);
  assert.match(source, /if \(!r\.ok\) throw new Error/);
});
