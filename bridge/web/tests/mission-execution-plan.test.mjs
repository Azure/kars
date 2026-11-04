// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => ({ type, props });
const imports = Object.fromEntries([
  "@/components/segmented-tier", "@/components/orchestration-cube",
  "@/components/icon", "@/components/journey-rail", "@/lib/format",
  "@/components/repo-access", "../envelope-reveal", "./controls",
  "@/components/validation-checks", "./helpers", "./qualification-choice",
].map(name => [name, {}]));
const repoAccess = { exports: {}, require(name) {
  if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
  if (name === "react" || name === "@/lib/config") return {};
  throw new Error(`Unexpected repository-access import: ${name}`);
} };
runInNewContext(ts.transpileModule(readFileSync(new URL(
  "../src/components/repo-access.tsx", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
    jsx: ts.JsxEmit.ReactJSX },
}).outputText, repoAccess);
imports["@/components/repo-access"] = repoAccess.exports;
imports["./controls"].CreateButton = "CreateButton";
const budgetReview = { exports: {} };
runInNewContext(ts.transpileModule(readFileSync(new URL("../src/lib/budget-review.ts", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText, budgetReview);
imports["@/lib/budget-review"] = budgetReview.exports;
imports["@/components/role-budget-fields"] = { RoleBudgetFields: "RoleBudgetFields" };
const context = { exports: {}, require(name) {
  if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
  assert.ok(name in imports, `Unexpected import: ${name}`);
  return imports[name];
} };
runInNewContext(ts.transpileModule(readFileSync(new URL(
  "../src/app/workspace/new/intake-flow/review.tsx", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
    jsx: ts.JsxEmit.ReactJSX },
}).outputText, context);
const { renderReview } = context.exports;

function nodes(node) {
  if (Array.isArray(node)) return node.flatMap(nodes);
  if (!node || typeof node !== "object") return [];
  return [node, ...nodes(node.props?.children)];
}
function fixture() {
  const previous = { schema: "kars.execution-plan/v1", roles: [], max_parallel: 1,
    synthesis: { objective: "Write the handoff", capabilities: ["filesystem-write"],
      max_tool_calls: 2147483647, execution_mode: "native" } };
  const state = { plan: previous, draft: JSON.stringify(previous), error: null,
    delegation: { mode: "principal-specialists", roles: [{ name: "guide-writer" }], max_parallel: 1 } };
  return { state, render(overrides = {}) {
    return renderReview({ budgetTokens: "", tier: 3, objective: "Sign a handoff",
      blueprint: { model: { provider: "azure-openai", deployment: "gpt-5.4-mini" }, execution_plan: state.plan },
      delegation: state.delegation, setDelegation: value => { state.delegation = value; },
      model: "azure-openai::gpt-5.4-mini",
      options: { models: [], runtimes: [], tool_policies: [], mcp_servers: [],
        mcp_profiles: [], skills: [], memories: [], isolation: [] },
      modelFallbacks: [], mcp: [], skills: [], egress: [], recommendedStats: null,
      loopDirective: "", state: { error: null }, executionPlanDraft: state.draft,
      executionPlanError: state.error, setExecutionPlanDraft: value => { state.draft = value; },
      setExecutionPlan: value => { state.plan = value; },
      setExecutionPlanError: value => { state.error = value; },
      ...overrides,
    });
  } };
}
function edit(f, value) {
  nodes(f.render()).find(node => node.type === "textarea" && node.props.spellCheck === false)
    .props.onChange({ target: { value } });
}
test("Mission role budget controls share the JSON editor and block malformed limits", () => {
  const f = fixture();
  edit(f, JSON.stringify({ ...f.state.plan, roles: [{ name: "writer", budget_tokens: 2000000, phases: [] }] }));
  const fields = () => nodes(f.render()).find(node => node.type === "RoleBudgetFields");
  for (const [value, valid] of [["1200000", true], ["0", false], ["9007199254740992", false], ["", true]]) {
    fields().props.onChange(budgetReview.exports.changeRoleBudget(fields().props.draft, 0, value));
    assert.equal(Boolean(validationDisabled(f)), !valid);
    assert.equal(Boolean(submissionDisabled(f)), !valid);
    assert.equal(JSON.parse(f.state.draft).roles[0].budget_tokens, f.state.plan.roles[0].budget_tokens);
    assert.equal(nodes(f.render()).find(node => node.props?.name === "budget_tokens").props.value, "");
  }
  const plan = JSON.parse(f.state.draft); plan.roles[0].budget_tokens = 3000000;
  edit(f, JSON.stringify(plan));
  assert.equal(JSON.parse(fields().props.draft).roles[0].budget_tokens, 3000000);
});

test("repository-bearing Mission drafts require a fresh passing check and wire controlled selection", () => {
  const f = fixture();
  const changed = () => {};
  for (const [selected, fresh, ok, blocked] of [
    [[], false, false, false], [["owner/repo"], false, true, true],
    [["owner/repo"], true, false, true], [["owner/repo"], true, true, false],
  ]) {
    const tree = nodes(f.render({ launch: false, selectedRepos: selected, changeSelectedRepos: changed,
      validationFresh: fresh, validation: { ok, checks: [] } }));
    assert.equal(Boolean(tree.find(node => node.type === "CreateButton").props.disabled), blocked);
    const repo = tree.find(node => node.type === repoAccess.exports.RepoAccess);
    assert.equal(repo.props.selected, selected);
    assert.equal(repo.props.onSelectionChange, changed);
  }
});

function validationDisabled(f) {
  return nodes(f.render()).find(node => node.type === "button" &&
    node.props.children === "Check readiness").props.disabled;
}

function submissionDisabled(f) {
  return nodes(f.render()).find(node => node.type === "CreateButton").props.disabled;
}

for (const empty of ["", " \n "]) {
  test(`clearing a plan to ${JSON.stringify(empty)} blocks submission and remains repairable`, () => {
    const f = fixture();
    const original = f.state.draft;
    edit(f, "{");
    assert.match(f.state.error, /valid JSON/);
    assert.equal(validationDisabled(f), true);
    edit(f, empty);
    assert.equal(f.state.plan, null);
    const submitted = nodes(f.render()).find(node => node.props.name === "delegation_json");
    assert.deepEqual(JSON.parse(submitted.props.value), {
      mode: "single-agent", roles: [], max_parallel: 1,
    });
    assert.equal(f.state.error, null);
    assert.equal(f.state.draft, empty);
    assert.equal(validationDisabled(f), true);
    assert.equal(submissionDisabled(f), true);
    assert.ok(nodes(f.render()).some(node => typeof node.props?.children === "string"
      && node.props.children.includes("Every Mission needs a reviewed capability plan")));
    edit(f, original);
    assert.equal(Boolean(validationDisabled(f)), false);
    assert.equal(Boolean(submissionDisabled(f)), false);
    assert.equal(f.state.plan.roles.length, 0);
  });
}

test("a manual package without a plan exposes an editable plan and blocks submission", () => {
  const f = fixture();
  f.state.plan = null;
  f.state.draft = "";
  assert.equal(validationDisabled(f), true);
  assert.equal(submissionDisabled(f), true);
  assert.ok(nodes(f.render()).some(node => node.type === "textarea"
    && node.props["aria-label"] === "Execution plan"));
});

test("repository access follows the plan currently edited in review", () => {
  const f = fixture();
  const readOnly = () => nodes(f.render()).find(node => node.type === repoAccess.exports.RepoAccess).props.readOnly;
  assert.equal(readOnly(), true);
  edit(f, JSON.stringify({ roles: [{ phases: [{ capabilities: ["github-pr"] }] }], synthesis: { capabilities: [] } }));
  assert.equal(readOnly(), false);
  edit(f, "{}");
  assert.equal(readOnly(), true);
  edit(f, "");
  assert.equal(readOnly(), false);
});

test("malformed nonempty plans remain editable and block validation until repaired", () => {
  const f = fixture();
  const original = JSON.stringify(f.state.plan);
  edit(f, "not JSON");
  assert.equal(validationDisabled(f), true);
  assert.equal(JSON.stringify(f.state.plan), original);
  edit(f, original);
  assert.equal(f.state.error, null);
  assert.equal(JSON.stringify(f.state.plan), original);
  assert.equal(Boolean(validationDisabled(f)), false);
});
