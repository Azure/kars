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
const exports = {};
runInNewContext(ts.transpileModule(readFileSync(new URL("../src/components/journey-rail.tsx", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText, { exports, require });

test("paused Team journey describes scheduling rather than suspended execution", () => {
  const current = exports.teamBeat({ paused: true, everRan: true });
  assert.equal(current, "run");
  const html = renderToStaticMarkup(React.createElement(exports.JourneyRail, { current, paused: true }));
  assert.match(html, /Scheduling paused/);
  assert.doesNotMatch(html, /choose Resume first|resumes on Run now|hibernating|suspended/);
  assert.equal(exports.teamBeat({ paused: true, everRan: false }), "review");
});

test("active and blocked journeys retain their separate guidance", () => {
  const active = renderToStaticMarkup(React.createElement(exports.JourneyRail, { current: "run" }));
  assert.match(active, /Live work &amp; steering/);
  assert.doesNotMatch(active, /choose Resume/);
  const blocked = renderToStaticMarkup(React.createElement(exports.JourneyRail, { current: "run", blocked: true, paused: true }));
  assert.match(blocked, /Needs your attention/);
  assert.doesNotMatch(blocked, /choose Resume/);
});
