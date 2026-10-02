// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => ({ type, props });
function load(path, imports) {
  const context = { exports: {}, require(name) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
function nodes(node) {
  if (Array.isArray(node)) return node.flatMap(nodes);
  if (!node || typeof node !== "object") return [];
  return [node, ...nodes(node.props?.children)];
}
const noToolsClaim = /None — model only|none — model only|No tools — pure reasoning/;

test("Home labels omission as a tool-permitting default without changing selection values", () => {
  const imports = {};
  for (const name of ["@/components/segmented-tier", "@/components/orchestration-cube",
    "@/components/icon", "@/components/journey-rail", "@/lib/format", "@/components/repo-access",
    "../envelope-reveal", "./controls", "./helpers"]) imports[name] = {};
  const { renderReview } = load("app/workspace/new/intake-flow/review.tsx", imports);
  const selections = [];
  for (const toolPolicy of ["", "restricted-policy"]) {
    const tree = renderReview({ budgetTokens: "2000000", tier: 1,
      blueprint: { model: { provider: "azure-foundry", deployment: "gpt-5.4-mini" } },
      delegation: { mode: "single" }, objective: "Brief", model: "azure-foundry::gpt-5.4-mini",
      options: { models: [], runtimes: [], tool_policies: [{ name: "restricted-policy" }],
        mcp_servers: [], mcp_profiles: [], skills: [], memories: [], isolation: [] },
      toolPolicy, setToolPolicy: value => selections.push(value),
      modelFallbacks: [], mcp: [], skills: [], egress: [], recommendedStats: null,
      loopDirective: "", executionPlanDraft: "", executionPlanError: null, state: { error: null } });
    const select = nodes(tree).find(node => node.type === "select" &&
      nodes(node).some(child => child.type === "option" && child.props.value === "restricted-policy"));
    assert.equal(select.props.value, toolPolicy);
    const options = nodes(select).filter(node => node.type === "option");
    assert.equal(options[0].props.value, "");
    assert.match(options[0].props.children, /Default — kars-default.*tools permitted/);
    select.props.onChange({ target: { value: toolPolicy } });
    assert.match(JSON.stringify(tree), /It does not disable tools/);
    assert.doesNotMatch(JSON.stringify(tree), noToolsClaim);
  }
  assert.deepEqual(selections, ["", "restricted-policy"]);
});

test("proposal reveal distinguishes omitted and explicit policies without claiming enforcement", () => {
  const { EnvelopeReveal } = load("app/workspace/new/envelope-reveal.tsx", {
    "@/components/icon": {}, "@/lib/format": {},
  });
  for (const tool_policy of [undefined, null, "", "  ", "restricted-policy"]) {
    const tree = EnvelopeReveal({ blueprint: { tool_policy }, tier: 1, delegation: { mode: "single" } });
    const facet = nodes(tree).find(node => node.type === "li" &&
      nodes(node).some(child => child.props.children === "Tool policy"));
    const output = JSON.stringify(facet);
    assert.match(output, tool_policy?.trim() ? /restricted-policy/ : /kars-default \(default\)/);
    assert.doesNotMatch(output, noToolsClaim);
    assert.doesNotMatch(output, /Bounds every tool/);
  }
});

const { CompositionPanel } = load("app/workspace/missions/[name]/mission-detail-panels.tsx", {
  "next/link": {}, "@/components/deliverable-view": {}, "@/components/provenance-story": {},
  "@/lib/format": {}, "@/components/icon": {},
});
function policyPanel(tool_policy, envelopeToolPolicy, launched) {
  const tree = CompositionPanel({ composition: { tool_policy, mcp_servers: [], egress: [] },
    envelopeToolPolicy, launched });
  return { tree, value: nodes(tree).find(node => node.props.label === "Tool policy").props.value };
}

test("draft uses the stored envelope policy only when composition has no explicit policy", () => {
  for (const value of [undefined, null, "", "  "]) {
    const result = policyPanel(value, "kars-default", false);
    assert.equal(result.value, "kars-default");
    assert.match(JSON.stringify(result.tree), /permits governed tools/);
  }
  assert.equal(policyPanel("restricted-policy", "kars-default", false).value, "restricted-policy");
  assert.equal(policyPanel(null, "customer-policy", false).value, "customer-policy");
});

test("missing draft policy is unknown, never a fabricated default or no-tools boundary", () => {
  for (const value of [null, "", "  "]) {
    const result = policyPanel(value, value, false);
    assert.equal(result.value, "Policy not reported");
    assert.match(JSON.stringify(result.tree), /not evidence that tools are disabled/);
    assert.doesNotMatch(JSON.stringify(result.tree), noToolsClaim);
  }
});

test("launched missions use reported runtime policy, not a planned fallback", () => {
  assert.equal(policyPanel("runtime-policy", "kars-default", true).value, "runtime-policy");
  for (const value of [null, "", "  "]) {
    const result = policyPanel(value, "kars-default", true);
    assert.equal(result.value, "Runtime policy not reported");
    assert.doesNotMatch(JSON.stringify(result.tree), /kars-default/);
    assert.doesNotMatch(JSON.stringify(result.tree), noToolsClaim);
  }
});
