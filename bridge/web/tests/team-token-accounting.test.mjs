// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import vm from "node:vm";

const ts = createRequire(import.meta.url)("typescript");
const source = readFileSync(new URL("../src/app/workspace/teams/[name]/team-outcomes.tsx", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
});
const jsx = (type, props) => ({ type, props: props ?? {} });
const previewFile = new URL("../src/components/deliverable-view.tsx", import.meta.url);
const previewSource = ts.createSourceFile(previewFile.pathname, readFileSync(previewFile, "utf8"),
  ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const previews = {};
vm.runInNewContext(ts.transpileModule(previewSource.statements.filter(node => ts.isFunctionDeclaration(node)
  && ["cleanDeliverable", "toPlainPreview"].includes(node.name?.text)).map(node => node.getText(previewSource)).join("\n"),
{ compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, { exports: previews });
const exports = {};
vm.runInNewContext(outputText, { exports, Intl, Number, require(name) {
  if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
  if (name === "react") return { useState: (value) => [value, () => {}] };
  if (name === "@/components/navigation-link") return { default: "a" };
  if (name === "@/components/deliverable-view") return previews;
  assert.fail(`Unexpected import: ${name}`);
} });
function text(node) {
  if (node == null || typeof node === "boolean") return "";
  if (Array.isArray(node)) return node.map(text).join("");
  if (typeof node !== "object") return String(node);
  if (typeof node.type === "function") return text(node.type(node.props));
  return text(node.props.children);
}
function summary(overrides = {}) {
  return text(exports.TeamValueSummary({
    summary: { change_proposed: 0, no_action_needed: 0, completed: 1, failed: 0 },
    generated: 1, retained: 1, queued: 0, active: 0,
    tokens: 86793, tokenScope: "GovernedInferenceLifetime", ...overrides,
  }));
}

test("Team summary displays lifetime settlement without dividing mismatched outcome windows", () => {
  const result = summary();
  assert.match(result, /Lifetime settled governed-inference tokens: 86\.8K/);
  assert.match(result, /Includes all Team roles and root inference/);
  assert.match(result, /excludes reserved and uncertain charges/);
  assert.match(result, /Lifetime usage is not divided by retained outcomes/);
  assert.match(result, /Monetary cost is not measured/);
  assert.match(result, /unpriced inference does not imply zero cost/);
  assert.doesNotMatch(result, /37\.5K|per resolved retained outcome/);
});

test("assigned queue work is not presented as live execution", () => {
  for (const active of [0, 1, 3]) {
    const result = summary({ active });
    assert.match(result, new RegExp(`Assigned work${active}`));
    assert.match(result, /not currently executing runs/);
    assert.match(result, /assignment can remain active while its run is held/);
    assert.doesNotMatch(result, /Working now/);
  }
});

test("missing, invalid, and version-skewed accounting is unavailable, not zero", () => {
  for (const tokens of [null, undefined, NaN, -1, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
    assert.match(summary({ tokens }), /governed-inference tokens: Unavailable/);
  }
  assert.match(summary({ tokenScope: undefined, tokens: 37488 }), /Token usage: Unavailable/);
  assert.match(summary({ tokens: 0 }), /governed-inference tokens: 0\./);
});

test("digest cards preserve nullable scoped usage at report time", () => {
  const source = readFileSync(new URL("../src/app/workspace/inbox/page.tsx", import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(`${source}\nexport { DigestCard };`, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  const digestModule = {};
  vm.runInNewContext(outputText, { exports: digestModule, Intl, Number, require(name) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    if (name === "@/components/navigation-link") return { default: "a" };
    if (name.startsWith("@/")) return {};
    assert.fail(`Unexpected import: ${name}`);
  } });
  const card = (tokens, scope = "GovernedInferenceLifetime") => text(digestModule.DigestCard({ d: {
    team: "team", health: "Healthy", at: "2026-10-15T00:00:00Z", tokens_spent: tokens, tokens_spent_scope: scope,
  } }));
  assert.match(card(86793), /86,793 lifetime settled governed-inference tokens \(at report time\)/);
  assert.match(card(null), /Unavailable lifetime settled governed-inference tokens/);
  assert.match(card(0), /0 lifetime settled governed-inference tokens/);
  assert.match(card(37488, "RetainedRootSynthesis"), /37,488 retained root synthesis tokens/);
  assert.match(card(37488, null), /Unavailable tokens/);
  assert.match(card(null), /Monetary cost not measured/);
});

test("outcome cards recover broken Markdown titles from retained detail", () => {
  const outcome = { run: "run", disposition: "completed", headline: "[Download repository-metadata-report",
    detail: "[Download repository-metadata-report.md](sandbox:/sandbox/repository-metadata-report.md)",
    duration_seconds: null, artifact_count: 1, pull_requests: [], tokens: null };
  const result = text(exports.RecentTeamOutcomes({ team: "team", outcomes: [outcome] }));
  assert.match(result, /Download repository-metadata-report\.md/);
  assert.doesNotMatch(result, /\[Download|sandbox:/);
  assert.equal(outcome.headline, "[Download repository-metadata-report");
});

const finishedOutcome = {
  run: "run-18", disposition: "completed", headline: "Application files", tokens: null,
  duration_seconds: null, artifact_count: 9, pull_requests: [],
};
function reviewedRows(tasks) {
  return [
    text(exports.RecentTeamOutcomes({ team: "team", outcomes: [finishedOutcome], tasks })),
    text(exports.TeamRunHistory({ team: "team", runs: [finishedOutcome.run],
      outcomes: [finishedOutcome], generated: 18, tasks })),
  ];
}

test("rejected finished runs retain their exact-run review decision in both views", () => {
  for (const result of reviewedRows([{ status: "pending", run: null,
    history: [{ run: "run-18", outcome: "changes_requested" }] }])) {
    assert.match(result, /Run finished/);
    assert.match(result, /Changes requested/);
    assert.doesNotMatch(result, /Completed|Approved/);
  }
  assert.match(summary(), /A finished run is not an accepted result/);
  assert.match(summary(), /Other runs finished/);
});

test("only explicit retained approval or current review status produces review badges", () => {
  for (const result of reviewedRows([{ status: "done", run: null,
    history: [{ run: "run-18", outcome: "approved" }] }])) assert.match(result, /Approved/);
  for (const result of reviewedRows([{ status: "awaiting_review", run: "run-18" }])) {
    assert.match(result, /Awaiting review/);
    assert.doesNotMatch(result, /Approved/);
  }
  for (const tasks of [undefined, [], [{ status: "done", run: "run-18" }],
    [{ status: "done", history: [{ run: "run-18", outcome: "completed" }] }],
    [{ status: "awaiting_review", run: "run-19",
      history: [{ run: "run-17", outcome: "approved" }] }]]) {
    for (const result of reviewedRows(tasks)) {
      assert.doesNotMatch(result, /Approved|Awaiting review|Changes requested/);
    }
  }
});

test("conflicting exact-run review evidence is not presented as approval", () => {
  for (const result of reviewedRows([{ history: [
    { run: "run-18", outcome: "approved" }, { run: "run-18", outcome: "changes_requested" },
  ] }])) {
    assert.match(result, /Review status unavailable/);
    assert.doesNotMatch(result, /Approved/);
  }
});

test("both Team page views receive retained review history", () => {
  const page = readFileSync(new URL("../src/app/workspace/teams/[name]/page.tsx", import.meta.url), "utf8");
  for (const component of ["RecentTeamOutcomes", "TeamRunHistory"]) {
    assert.match(page, new RegExp(`<${component}\\s[^>]*tasks=\\{team\\.tasks\\}`));
  }
});

test("legacy totals and retained run rows explicitly identify root-only synthesis", () => {
  const legacy = summary({ tokens: 37488, tokenScope: "RetainedRootSynthesis" });
  assert.match(legacy, /Retained root synthesis tokens: 37\.5K/);
  assert.match(legacy, /excludes descendant inference and is not total Team usage/);
  for (const tokens of [37488, null]) {
    const result = text(exports.RecentTeamOutcomes({ team: "team", outcomes: [{
      run: "run", disposition: "completed", headline: "Done", tokens,
      duration_seconds: null, artifact_count: 0, pull_requests: [],
    }] }));
    assert.match(result, tokens == null ? /root synthesis tokens unavailable/ : /37\.5K root synthesis tokens/);
    assert.doesNotMatch(result, /cost unavailable/);
  }
});
