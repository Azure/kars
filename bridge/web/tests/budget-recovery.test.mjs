// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => typeof type === "function" ? type(props) : { type, props };
function load(path, imports = {}, globals = {}) {
  const exports = {};
  const result = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics, []);
  runInNewContext(result.outputText, { exports, ...globals, require(name) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } });
  return exports;
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  return tree?.props ? [tree, ...nodes(tree.props.children)] : [];
}
function text(tree) {
  if (Array.isArray(tree)) return tree.map(text).join("");
  return tree?.props ? text(tree.props.children) : tree == null || typeof tree === "boolean" ? "" : String(tree);
}
const budget = load("lib/budget-review.ts");
const plan = { schema: "kars.execution-plan/v1", max_parallel: 1,
  roles: [{ name: "researcher", budget_tokens: 2000000, objective: "PRIVATE objective", phases: [{ capabilities: ["network"], max_tool_calls: 65 }] },
    { name: "verifier", budget_tokens: 1800000 }], synthesis: { objective: "Keep synthesis", max_tool_calls: 45 } };

test("composition keeps reviewed total and role caps without converting estimates into limits", () => {
  const requested = budget.compositionSpending("9000000", JSON.stringify(plan));
  assert.equal(requested.error, null);
  assert.deepEqual(JSON.parse(JSON.stringify(requested.spending)), {
    budget_tokens: 9000000, role_tokens: { researcher: 2000000, verifier: 1800000 },
  });
  assert.deepEqual(JSON.parse(JSON.stringify(budget.compositionSpending("", "").spending)), {
    budget_tokens: null, role_tokens: {},
  });
  const uncapped = { roles: [{ name: "reviewer", budget_tokens: null, expected_tokens: 500000 }] };
  assert.deepEqual(JSON.parse(JSON.stringify(budget.compositionSpending("", JSON.stringify(uncapped)).spending)).role_tokens, {});
});

test("composition refuses malformed plans and ambiguous caps instead of discarding them", () => {
  for (const draft of ["{", "null", "[]", JSON.stringify({ roles: [null] }),
    JSON.stringify({ roles: [{ name: "reviewer" }, { name: "reviewer" }] }),
    JSON.stringify({ roles: [{ name: " " }] }), JSON.stringify({ roles: [{ name: "reviewer", budget_tokens: "bad" }] })]) {
    const result = budget.compositionSpending("9000000", draft);
    assert.equal(result.spending, null, draft);
    assert.ok(result.error, draft);
  }
  for (const cap of ["-1", "0", "NaN", "Infinity", "9007199254740992", "1.5"]) {
    assert.equal(budget.compositionSpending(cap, "").spending, null);
  }
});

for (const value of ["0", "-1", "2.4", "9007199254740992", "Infinity", "2e6", "invalid"]) {
  test(`role budget ${value} remains visible and invalid rather than becoming unbounded`, () => {
    const draft = JSON.stringify(plan);
    const changed = budget.changeRoleBudget(draft, 0, value);
    const parsed = JSON.parse(changed);
    assert.equal(parsed.roles[0].budget_tokens, value);
    assert.match(budget.roleBudgetError(parsed), /researcher.*positive whole number/);
    assert.equal(JSON.stringify(plan), draft);
    assert.deepEqual(parsed.roles[1], plan.roles[1]);
    assert.deepEqual(parsed.synthesis, plan.synthesis);
    assert.deepEqual(parsed.roles[0].phases, plan.roles[0].phases);
  });
}

test("role editing preserves the full plan and uses null only for explicitly blank input", () => {
  for (const [input, expected] of [["1200000", 1200000], ["", null], ["9007199254740991", Number.MAX_SAFE_INTEGER]]) {
    const parsed = JSON.parse(budget.changeRoleBudget(JSON.stringify(plan), 0, input));
    assert.equal(parsed.roles[0].budget_tokens, expected);
    assert.equal(budget.roleBudgetError(parsed), null);
    delete parsed.roles[0].budget_tokens;
    const expectedPlan = structuredClone(plan); delete expectedPlan.roles[0].budget_tokens;
    assert.deepEqual(parsed, expectedPlan);
  }
  assert.throws(() => budget.changeRoleBudget(JSON.stringify(plan), 9, ""), /no longer/);
});

test("real role fields are labeled, synchronized with JSON and show invalid inherited values", () => {
  let draft = JSON.stringify(plan);
  const { RoleBudgetFields } = load("components/role-budget-fields.tsx", { "@/lib/budget-review": budget });
  const render = () => RoleBudgetFields({ draft, idPrefix: "test-role", onChange: value => { draft = value; } });
  const fields = () => nodes(render()).filter(node => node.type === "input");
  assert.equal(fields()[0].props["aria-label"], "researcher token limit");
  assert.match(text(render()), /leaving it blank means it uses the shared limit/);
  fields()[0].props.onChange({ target: { value: "bad" } });
  assert.equal(fields()[0].props["aria-invalid"], true);
  assert.equal(fields()[0].props.value, "bad");
  assert.ok(nodes(render()).some(node => node.props.role === "alert"));
  fields()[0].props.onChange({ target: { value: "" } });
  assert.equal(JSON.parse(draft).roles[0].budget_tokens, null);
  assert.equal(fields()[0].props["aria-invalid"], false);
  assert.equal(fields()[1].props.value, "1800000");
  assert.equal(RoleBudgetFields({ draft: "{", idPrefix: "invalid" }), null);
});

for (const clipboard of ["working", "denied", "missing"]) {
  test(`support recovery with ${clipboard} clipboard does not mutate or transmit the draft`, async () => {
    let state = "idle", reviewed = 0, copied;
    const navigator = clipboard === "missing" ? {} : { clipboard: { writeText: async value => {
      if (clipboard === "denied") throw new Error("Denied");
      copied = value;
    } } };
    const { ValidationChecks } = load("components/validation-checks.tsx", {
      react: { useState: () => [state, value => { state = value; }] }, "@/lib/budget-review": budget,
    }, { navigator });
    const check = { id: "budget_contracts", status: "fail", label: "Operator budget contracts", detail: 'Role "researcher": No current contract for "gpt-6.1-sol".' };
    const props = { checks: [check], budget: { blueprint: { instructions: "PRIVATE credentials", execution_plan: plan }, tokens: 8000000 }, onReviewBudget: () => { reviewed++; } };
    const before = JSON.stringify(props);
    const render = () => ValidationChecks(props);
    const buttons = () => nodes(render()).filter(node => node.type === "button");
    buttons().find(node => text(node) === "Review spending limits").props.onClick();
    assert.equal(reviewed, 1);
    await buttons().find(node => text(node) === "Copy support request").props.onClick();
    const tree = render();
    if (clipboard === "working") {
      assert.equal(state, "copied");
      assert.match(text(tree), /nothing has been sent automatically/);
    } else {
      assert.equal(state, "manual");
      const fallback = nodes(tree).find(node => node.type === "textarea");
      assert.equal(fallback.props.readOnly, true);
      assert.equal(fallback.props["aria-label"], "Support request to copy");
      let selected = false;
      fallback.props.onFocus({ currentTarget: { select: () => { selected = true; } } });
      assert.equal(selected, true);
      copied = fallback.props.value;
    }
    assert.match(copied, /gpt-6.1-sol/);
    assert.match(copied, /8,000,000 tokens/);
    assert.match(copied, /researcher: 2,000,000 tokens/);
    assert.doesNotMatch(copied, /PRIVATE/);
    assert.equal(JSON.stringify(props), before);
    assert.equal(nodes(tree).find(node => node.type === "details").props.open, undefined);
    assert.ok(nodes(tree).some(node => node.props.role === "status"));
  });
}


test("composition spending failures reuse guidance without inventing checked draft limits", () => {
  const { CompositionNote } = load("components/validation-checks.tsx", {
    react: { useState: () => ["idle", () => {}] }, "@/lib/budget-review": budget,
  });
  const detail = 'Role "researcher": No current operator budget contract covers model "gpt-6.1-sol".';
  const tree = CompositionNote({ detail });
  assert.match(text(tree), /Spending setup needs attention/);
  assert.match(text(tree), /person who manages Kars/);
  assert.doesNotMatch(text(tree), /Limits checked/);
  assert.ok(!nodes(tree).some(node => node.type === "button" && text(node) === "Review spending limits"));
  const technical = nodes(tree).find(node => node.type === "details");
  assert.equal(technical.props.open, undefined);
  assert.ok(text(technical).includes(detail));
  assert.equal(text(CompositionNote({ detail: "The connected model is unavailable. Try again later." })),
    "The connected model is unavailable. Try again later.");
});
