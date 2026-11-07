// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import { roleBudgetError } from "../src/lib/budget-review.ts";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => ({ type, props });
const context = { exports: {}, require(name) {
  if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
  if (name === "./helpers") return { modelKey: (provider, model) => `${provider}::${model}` };
  if (name === "@/components/repo-access") return { missionGithubWriteEnabled: () => false };
  if (name === "@/lib/budget-review") return { roleBudgetError };
  return {};
} };
runInNewContext(ts.transpileModule(readFileSync(new URL(
  "../src/app/workspace/new/intake-flow/review.tsx", import.meta.url,
), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS,
  target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText, context);
function nodes(node) {
  if (Array.isArray(node)) return node.flatMap(nodes);
  if (!node || typeof node !== "object") return [];
  return [node, ...nodes(node.props?.children)];
}
function text(node) {
  if (Array.isArray(node)) return node.map(text).join("");
  if (!node || typeof node === "boolean") return "";
  return typeof node === "object" ? text(node.props?.children) : String(node);
}

test("Mission review distinguishes a selected additional provider from the cluster default", () => {
  const models = [
    { provider: "azure-foundry", deployment: "shared-model", is_default: true },
    { provider: "github-copilot", deployment: "shared-model", is_default: false },
  ];
  for (const provider of [null, { id: "azure-foundry", label: "Azure AI Foundry", note: "Cluster provider" }]) {
    const changes = [];
    let fallbacks = ["azure-foundry::shared-model", "github-copilot::shared-model"];
    const tree = context.exports.renderReview({ budgetTokens: "", tier: 3,
      blueprint: { model: { provider: "github-copilot", deployment: "shared-model" } },
      delegation: { mode: "single" }, objective: "Write a checklist",
      model: "github-copilot::shared-model", setModel: value => changes.push(value),
      setModelFallbacks: update => { fallbacks = update(fallbacks); },
      options: { provider, models, runtimes: [], tool_policies: [], mcp_servers: [],
        mcp_profiles: [], skills: [], memories: [], isolation: [] },
      toolPolicy: "", modelFallbacks: [], mcp: [], skills: [], egress: [],
      recommendedStats: null, loopDirective: "", executionPlanDraft: "",
      executionPlanError: null, state: { error: null } });
    const select = nodes(tree).find(node => node.type === "select" && !node.props.multiple);
    assert.equal(select.props.value, "github-copilot::shared-model");
    const choices = nodes(select).filter(node => node.type === "option");
    assert.deepEqual(choices.map(node => [node.props.value, text(node)]), [
      ["azure-foundry::shared-model", "shared-model · azure-foundry (default)"],
      ["github-copilot::shared-model", "shared-model · github-copilot"],
    ]);
    select.props.onChange({ target: { value: "github-copilot::shared-model" } });
    assert.deepEqual(changes, ["github-copilot::shared-model"]);
    assert.deepEqual(fallbacks, ["azure-foundry::shared-model"]);
    assert.match(text(tree), /connected providers without changing the cluster default/);
    assert.doesNotMatch(text(tree), /This cluster serves models via|switch to any model Azure/);
    assert.equal(text(tree).includes("Cluster default provider:"), provider !== null);
  }
});
