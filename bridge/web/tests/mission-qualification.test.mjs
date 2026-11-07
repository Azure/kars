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
function load(path, imports = {}, globals = {}) {
  const exports = {};
  const result = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics, [], `Syntax diagnostics in ${path}`);
  runInNewContext(result.outputText, { exports, ...globals, require(name) {
    if (name === "@/lib/budget-review") return load("lib/budget-review.ts");
    if (name === "@/components/role-budget-fields") return { RoleBudgetFields: "RoleBudgetFields" };
    if (name === "@/components/validation-checks" && !(name in imports)) return { CompositionNote: "CompositionNote" };
    if (name in imports) return imports[name];
    if (name === "react" || name === "react/jsx-runtime") return require(name);
    assert.fail(`Unexpected import: ${name}`);
  } });
  return exports;
}
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));
const record = { approved_by: "Operator Example", approved_by_sub: "oidc-subject-1",
  approved_at: "2026-10-16T06:00:00Z", expires_at: "2026-10-16T06:45:00Z", package_sha256: "ab".repeat(32) };
const qualificationComponents = load("components/mission-qualification.tsx", {
  "@/components/navigation-link": { default: props => React.createElement("a", props) },
});
const choiceModule = load("app/workspace/new/intake-flow/qualification-choice.tsx");

test("qualification checkbox is absent for nonoperators, explicit and unchecked by default for operators", () => {
  assert.equal(render(choiceModule.QualificationChoice, { canQualify: false, checked: true }), "");
  const html = render(choiceModule.QualificationChoice, { canQualify: true, checked: false, onChange() {} });
  assert.match(html, /type="checkbox"/);
  assert.doesNotMatch(html, /checked=""/);
  assert.match(html, /Qualification run/);
  assert.match(html, /lack prior execution evidence/);
  assert.match(html, /resource readiness, security/);
  assert.match(html, /finite budgets and content-bound skill approval still apply/);
  assert.match(html, /does not apply to standing Teams/);
  assert.match(html, /current resource identities/);
  assert.match(html, /not a runtime cancellation deadline/);
});

test("saved qualification evidence remains explicit and complete, even after the admission deadline", () => {
  const html = render(qualificationComponents.MissionQualificationRecord, { record });
  for (const value of Object.values(record)) assert.ok(html.includes(value), value);
  assert.match(html, /not proof of qualification/);
  assert.match(html, /expiry does not cancel admitted work/);
  assert.match(html, /cannot be replicated, revised or run again/);
  assert.match(html, /href="\/workspace\/new"/);
  assert.doesNotMatch(html, />Qualified</);
});

test("qualification resource fingerprints are displayed without claiming qualification", () => {
  const bound = { ...record, resource_bindings: { "McpServer/docs": "sha256:resource-version" } };
  const html = render(qualificationComponents.MissionQualificationRecord, { record: bound });
  assert.match(html, /Reviewed resource bindings/);
  assert.match(html, /McpServer\/docs/);
  assert.match(html, /sha256:resource-version/);
  assert.match(html, /rechecked before admission/);
  assert.doesNotMatch(html, />Qualified</);
  const legacy = render(qualificationComponents.MissionQualificationRecord, { record });
  assert.doesNotMatch(legacy, /Reviewed resource bindings/);
});

test("qualification output approvals remain actionable, while revision and retry controls are absent", () => {
  const { ReviewPanel } = load("app/workspace/missions/[name]/review-panel.tsx", {
    "next/navigation": { useRouter: () => ({ refresh() {} }) },
    "@/components/navigation-link": { default: props => React.createElement("a", props) },
    "@/components/mission-qualification": qualificationComponents,
    "@/lib/format": { formatEvidenceTime: value => value },
    "./review-actions": { submitReview: () => assert.fail("render must not mutate") },
    "@/app/tasks/[name]/launch-actions": { setLaunch: () => assert.fail("render must not mutate") },
  });
  const props = { task: "qualification", assignmentNonce: "run-1", eligible: true,
    kind: "docs", initial: null, launched: false, qualificationRun: true };
  const html = render(ReviewPanel, props);
  assert.match(html, /<button\b[^>]*>Approve deliverable<\/button>/);
  assert.doesNotMatch(html, />Request changes|>Request revision|Launch the revision/);
  assert.match(html, /approval does not certify/);
  const retry = render(ReviewPanel, { ...props, initial: { redrive_pending: true, retryable_revision_nonce: "retry" } });
  assert.doesNotMatch(retry, />Retry failed revision|Launch the revision/);
  const normal = render(ReviewPanel, { ...props, qualificationRun: false });
  assert.match(normal, />Request changes</);
});

test("qualification approval submits the existing revision-pinned output review action", async () => {
  const transitions = [], submissions = [];
  let refreshes = 0;
  const { ReviewPanel } = load("app/workspace/missions/[name]/review-panel.tsx", {
    react: { useState: value => [value, () => {}],
      useTransition: () => [false, callback => { transitions.push(callback()); }] },
    "next/navigation": { useRouter: () => ({ refresh() { refreshes++; } }) },
    "@/components/navigation-link": { default: props => React.createElement("a", props) },
    "@/components/mission-qualification": qualificationComponents,
    "@/lib/format": { formatEvidenceTime: value => value },
    "./review-actions": { submitReview: async (...args) => { submissions.push(args); return { error: null }; } },
    "@/app/tasks/[name]/launch-actions": { setLaunch: () => assert.fail("approval must not alter admission") },
  });
  const tree = ReviewPanel({ task: "qualification", assignmentNonce: "run-1", eligible: true,
    kind: "docs", initial: null, launched: false, qualificationRun: true });
  const approve = descendants(tree).find(node => node.type === "button" && node.props.children === "Approve deliverable");
  assert.ok(approve);
  assert.equal(approve.props.disabled, false);
  approve.props.onClick();
  await Promise.all(transitions);
  assert.deepEqual(submissions, [["qualification", "run-1", "approve", undefined]]);
  assert.equal(refreshes, 1);
});

class TestBffError extends Error {}
function actions(roles, composeError) {
  const calls = { compose: [], validate: [], create: [] };
  const api = load("app/workspace/new/actions.ts", {
    "next/navigation": { redirect: path => { throw Object.assign(new Error("redirect"), { path }); } },
    "@/lib/config": { defaultNamespace: () => "workspace" },
    "@/lib/session": { currentPrincipal: async () => ({ name: "Reviewer", roles }) },
    "@/lib/bff": { BffError: TestBffError,
      composePackage: async (...args) => {
        calls.compose.push(args);
        if (composeError) throw composeError;
        return { available: false, proposal: null };
      },
      validatePackage: async (...args) => { calls.validate.push(args); return { ok: true, checks: [] }; },
      createTask: async (...args) => { calls.create.push(args); },
    },
  });
  return { ...api, calls };
}
function form(qualification) {
  const data = new FormData();
  for (const [key, value] of Object.entries({ objective: "Onboard configured provider", tier: "1",
    budget_tokens: "20000", budget_scope: "GovernedInference", launch: "off",
    blueprint_json: JSON.stringify({ runtime: "Hermes", model: { provider: "configured", deployment: "model" } }),
    ...(qualification === undefined ? {} : { qualification_run: qualification }),
  })) data.set(key, value);
  return data;
}

test("Mission validation and creation receive the same reviewed repository selection", async () => {
  const api = actions(["user"]);
  const selected = ["Owner/Repo", "owner/other"];
  await api.validateMissionAction({ runtime: "Hermes" }, { git_write_repos: selected });
  const data = form(); data.set("git_write_repos", selected.join(","));
  await assert.rejects(api.createMissionAction({}, data), error => !!error.path);
  assert.deepEqual(JSON.parse(JSON.stringify(api.calls.create[0][1].git_write_repos)), selected);
  assert.deepEqual(api.calls.validate[0][2].git_write_repos, selected);
});

test("composition reports package rejections without mislabelling them as orchestrator outages", async () => {
  for (const [status, code] of [[400, "bad_request"], [409, "conflict"], [422, "rejected"]]) {
    const message = "Per-run qualification requires required mode; it cannot use global validation mode";
    const error = Object.assign(new TestBffError(message), { status, code });
    const result = await actions(["operator"], error).composeMissionAction("Onboard", true);
    assert.equal(result.available, false);
    assert.equal(result.reason, message);
    assert.equal(result.proposal, null);
  }
  const upstream = Object.assign(new TestBffError("private upstream details"), { status: 503, code: "upstream" });
  assert.equal((await actions(["operator"], upstream).composeMissionAction("Onboard")).reason,
    "The orchestrator is unavailable — compose the package manually below.");
});

test("operators and admins propagate explicit designation without changing the blueprint or budgets", async () => {
  for (const role of ["operator", "admin"]) {
    for (const flag of [true, false]) {
      const api = actions([role]);
      const spending = { budget_tokens: 20000, role_tokens: {} };
      await api.composeMissionAction("Onboard", flag, spending);
      assert.deepEqual(api.calls.compose[0], ["workspace", "Onboard", flag, spending]);
      await api.validateMissionAction({ runtime: "Hermes" }, { qualification_run: flag, budget_tokens: 20000 });
      assert.equal(api.calls.validate[0][2].qualification_run, flag);
      await assert.rejects(api.createMissionAction({}, form(flag ? "on" : "off")), error => !!error.path);
      const body = api.calls.create[0][1];
      assert.equal(body.qualification_run, flag);
      assert.equal(body.blueprint.qualification_run, undefined);
      assert.equal(body.blueprint.runtime, "Hermes");
      assert.equal(body.envelope.budget.tokens, 20000);
      assert.equal(body.envelope.budget.scope, "GovernedInference");
      assert.equal(body.launch, false);
    }
  }
});

test("nonoperators cannot forge any designation, while ordinary missions remain unchanged", async () => {
  for (const roles of [[], ["user"], ["auditor"]]) {
    for (const flag of [true, false, "true"]) {
      const api = actions(roles);
      assert.equal((await api.composeMissionAction("Onboard", flag)).available, false);
      assert.equal((await api.validateMissionAction({}, { qualification_run: flag })).ok, false);
      assert.match((await api.createMissionAction({}, form(flag === false ? "off" : "on"))).error, /operator or admin/);
      assert.deepEqual(api.calls, { compose: [], validate: [], create: [] });
    }
    const api = actions(roles);
    await api.composeMissionAction("Ordinary");
    await api.validateMissionAction({});
    await assert.rejects(api.createMissionAction({}, form()), error => !!error.path);
    assert.equal(api.calls.create[0][1].qualification_run, undefined);
    assert.equal(api.calls.compose.length, 1);
    assert.equal(api.calls.validate.length, 1);
  }
});

test("malformed qualification fields and invalid finite budgets are never silently discarded", async () => {
  const api = actions(["operator"]);
  for (const value of ["true", "false", "", "1"]) {
    assert.match((await api.createMissionAction({}, form(value))).error, /Review the qualification run designation/);
  }
  for (const value of [null, "true", 1, {}]) {
    assert.equal((await api.composeMissionAction("Onboard", value)).available, false);
    assert.equal((await api.validateMissionAction({}, { qualification_run: value })).ok, false);
  }
  const invalidBudget = form("on"); invalidBudget.set("budget_tokens", "-1");
  assert.match((await api.createMissionAction({}, invalidBudget)).error, /Budget limits must be positive/);
  assert.deepEqual(api.calls, { compose: [], validate: [], create: [] });
});

test("BFF client sends optional qualification boolean at top level with authenticated POSTs only", async () => {
  const requests = [];
  const api = load("lib/bff.ts", {
    "./config": { bffBaseUrl: () => "http://bff" },
    "next/headers": { cookies: async () => ({ get: () => ({ value: "signed-session" }) }) },
    "./session-token": { SESSION_COOKIE: "session", verifySession: async () => true },
    "./oidc-config": { ssoConfigured: () => true }, "./credential-review": {},
  }, { Headers, AbortSignal, fetch: async (...args) => {
    requests.push(args); return { ok: true, json: async () => ({}) };
  } });
  for (const flag of [undefined, false, true]) {
    await api.composePackage("workspace/a", "Onboard", flag, { budget_tokens: null, role_tokens: {} });
    await api.validatePackage("workspace/a", { runtime: "Hermes" }, { qualification_run: flag, workload: "mission" });
    await api.createTask("workspace/a", { name: "draft", blueprint: { runtime: "Hermes" }, qualification_run: flag });
    for (const [url, init] of requests.slice(-3)) {
      assert.match(url, /workspace%2Fa\/(compose|validate|tasks)$/);
      assert.equal(init.method, "POST");
      assert.equal(init.cache, "no-store");
      assert.equal(init.headers.get("x-kars-principal-token"), "signed-session");
      const body = JSON.parse(init.body);
      assert.equal(body.qualification_run, flag);
      assert.equal(Object.hasOwn(body, "qualification_run"), flag !== undefined);
      assert.equal(body.blueprint?.qualification_run, undefined);
    }
  }
  await api.validatePackage("workspace", {}, { workload: "team" });
  assert.equal(Object.hasOwn(JSON.parse(requests.at(-1)[1].body), "qualification_run"), false);
});

function descendants(node) {
  if (Array.isArray(node)) return node.flatMap(descendants);
  if (!node || typeof node !== "object") return [];
  return [node, ...descendants(node.props?.children)];
}
function flow({ canQualify = true, composeAvailable = true, initialObjective } = {}) {
  const slots = [], composeCalls = [], validationCalls = [];
  let cursor = 0, tree;
  const jsx = (type, props) => typeof type === "function" ? type(props) : { type, props };
  const jsxRuntime = { jsx, jsxs: jsx, Fragment: "fragment" };
  const react = {
    useState(value) { const index = cursor++; if (!(index in slots)) slots[index] = value;
      return [slots[index], next => { slots[index] = typeof next === "function" ? next(slots[index]) : next; }]; },
    useRef(value) { const index = cursor++; return slots[index] ??= { current: value }; },
    useEffect() {}, useMemo: fn => fn(), useActionState: () => [{ error: null }, () => {}],
  };
  const { IntakeFlow } = load("app/workspace/new/intake-flow.tsx", {
    react, "react/jsx-runtime": jsxRuntime,
    "@/components/orchestration-cube": { OrchestrationCube: () => null },
    "@/components/icon": { Icon: () => null }, "@/components/journey-rail": { JourneyRail: () => null },
    "@/components/loop-designer": { LoopDesigner: props => ({ type: "loop-designer", props }) },
    "@/lib/efficiency": load("lib/efficiency.ts"),
    "./intake-flow/helpers": load("app/workspace/new/intake-flow/helpers.ts"),
    "./intake-flow/qualification-choice": load("app/workspace/new/intake-flow/qualification-choice.tsx", { "react/jsx-runtime": jsxRuntime }),
    "./intake-flow/review": { renderReview: "review" },
    "./actions": {
      composeMissionAction: async (...args) => { composeCalls.push(args); return composeAvailable
        ? { available: true, proposal: { runtime: "Hermes", budget_tokens: 20000 }, rationale: null, source: "orchestrator" }
        : { available: false, reason: "Compose unavailable", proposal: null }; },
      createMissionAction() {}, proposeLoopAction() {},
    },
    "@/lib/preflight-client": { checkPackage: (...args) => new Promise(resolve => validationCalls.push({ args, resolve })) },
  });
  const options = { models: [], default_model: null, provider: null,
    runtimes: [{ kind: "Hermes", label: "Hermes", wired: true, status: "ready", note: "Configured harness" }],
    isolation: [], tool_policies: [], mcp_servers: [], mcp_profiles: [], skills: [], memories: [] };
  return {
    composeCalls, validationCalls,
    render() { cursor = 0; tree = IntakeFlow({ options, canQualify, initialObjective }); return tree; },
    nodes() { return descendants(tree); },
    review() { assert.equal(tree.type, "review"); return tree.props; },
    async compose() {
      descendants(tree).find(node => node.type === "textarea").props.onChange({ target: { value: "Onboard configured provider", style: {}, scrollHeight: 80 } });
      this.render();
      await descendants(tree).find(node => node.type === "button" && node.props.children === "Compose the package →").props.onClick();
      this.render();
    },
  };
}

test("Mission repository selection reaches validation, survives Back/recompose, and discards in-flight approval even after reverting", async () => {
  const ui = flow(); ui.render(); await ui.compose();
  ui.review().changeSelectedRepos(["owner/repo"]); ui.render();
  const pending = ui.review().runValidation();
  assert.deepEqual(ui.validationCalls[0].args[1].git_write_repos, ["owner/repo"]);
  ui.review().changeSelectedRepos(["owner/other"]); ui.render();
  ui.review().changeSelectedRepos(["owner/repo"]); ui.render();
  ui.validationCalls[0].resolve({ ok: true, checks: [] });
  await pending; ui.render();
  assert.equal(ui.review().validationFresh, false);
  assert.equal(ui.review().validation, null);
  const fresh = ui.review().runValidation();
  ui.validationCalls[1].resolve({ ok: true, checks: [] });
  await fresh; ui.render();
  assert.equal(ui.review().validationFresh, true);
  ui.review().goBack(); ui.render(); await ui.compose();
  assert.deepEqual(ui.review().selectedRepos, ["owner/repo"]);
  assert.equal(ui.review().validationFresh, false);
  ui.review().changeSelectedRepos([]); ui.render();
  const empty = ui.review().runValidation();
  assert.deepEqual(ui.validationCalls[2].args[1].git_write_repos, []);
  ui.validationCalls[2].resolve({ ok: true, checks: [] }); await empty;
});

test("reviewed loop reaches composition immediately, survives Back, and never replaces Mission identity", async () => {
  const intent = "Compare operator recovery approaches";
  const ui = flow({ initialObjective: intent }); ui.render();
  const parts = { goal: "Compare recovery reliability", patternId: "debate", criteria: "Real live challenges and replies", context: "Unbounded non-expiring token allowance" };
  const directive = Object.values(parts).join("\n");
  ui.nodes().find(node => node.type === "loop-designer").props.onApply(directive, parts);
  await new Promise(setImmediate); ui.render();
  assert.equal(ui.composeCalls[0][0], `${intent}\n\n${directive}`);
  assert.equal(ui.review().objective, intent);
  assert.equal(ui.review().blueprint.objective, undefined);
  assert.ok(ui.review().blueprint.instructions.includes(directive));
  ui.review().goBackToLoopReview(); ui.render();
  const designer = ui.nodes().find(node => node.type === "loop-designer");
  assert.equal(designer.props.initialGoal, parts.goal);
  assert.equal(designer.props.initialCriteria, parts.criteria);
  assert.equal(designer.props.initialContext, parts.context);
  const revised = { ...parts, context: "Revised context without expiry" };
  const updated = Object.values(revised).join("\n");
  designer.props.onApply(updated, revised);
  await new Promise(setImmediate); ui.render();
  assert.equal(ui.composeCalls[1][0], `${intent}\n\n${updated}`);
  assert.ok(!ui.composeCalls[1][0].includes(parts.context));
  ui.review().goBack(); ui.render();
  ui.nodes().find(node => node.type === "button" && node.props.children === "Skip — compose from my plain intent instead").props.onClick();
  await new Promise(setImmediate); ui.render();
  assert.equal(ui.composeCalls[2][0], intent);
  assert.equal(ui.review().objective, intent);
  assert.ok(!ui.review().blueprint.instructions?.includes(updated));
});

test("failed composition retries keep the reviewed directive", async () => {
  const intent = "Compare recovery approaches";
  const ui = flow({ initialObjective: intent, composeAvailable: false }); ui.render();
  const directive = "Success requires actual replies; funding must not expire";
  ui.nodes().find(node => node.type === "loop-designer").props.onApply(directive);
  await new Promise(setImmediate); ui.render();
  await ui.nodes().find(node => node.type === "button" && node.props.children === "Compose the package →").props.onClick();
  assert.equal(ui.composeCalls.length, 2);
  for (const call of ui.composeCalls) assert.equal(call[0], `${intent}\n\n${directive}`);
});

test("LoopDesigner restores context and includes it in both text and structured review", () => {
  const jsx = (type, props) => typeof type === "function" ? type(props) : { type, props };
  const patterns = load("lib/loop-patterns.ts");
  const { LoopDesigner } = load("components/loop-designer.tsx", {
    react: { useState: value => [typeof value === "function" ? value() : value, () => {}], useMemo: fn => fn() },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/components/icon": { Icon: () => null },
    "@/lib/loop-patterns": patterns,
  });
  let applied;
  const context = "Keep real accounting with non-expiring allowances";
  const tree = LoopDesigner({ surface: "mission", defaultOpen: true, initialGoal: "Compare approaches",
    initialPatternId: patterns.patternsFor("mission")[0].id, initialCriteria: "Ground the recommendation",
    initialContext: context, onApply: (text, parts) => { applied = { text, parts }; } });
  const nodes = descendants(tree);
  assert.ok(nodes.some(node => node.type === "textarea" && node.props.value === context));
  nodes.find(node => node.type === "button" && node.props.children === "Use this loop").props.onClick();
  assert.equal(applied.parts.context, context);
  assert.ok(applied.text.includes(context));
});

test("Mission sends reviewed total and role limits and runtime changes never increase them", async () => {
  const ui = flow(); ui.render(); await ui.compose();
  ui.review().setBudgetTokens("100000");
  ui.review().setMcp(["github"]);
  ui.review().setEgress([{ host: "api.osv.dev", port: 443 }]);
  ui.review().setExecutionPlanDraft(JSON.stringify({ roles: [{ name: "reviewer", budget_tokens: 50000 }] }));
  ui.render();
  ui.review().changeRuntime("Hermes"); ui.render();
  ui.review().goBack(); ui.render(); await ui.compose();
  assert.deepEqual(JSON.parse(JSON.stringify(ui.composeCalls[1][2])), { budget_tokens: 100000, role_tokens: { reviewer: 50000 } });
  ui.review().setExecutionPlanDraft("{"); ui.render();
  ui.review().goBack(); ui.render(); await ui.compose();
  assert.equal(ui.composeCalls.length, 2);
  assert.equal(ui.review().executionPlanDraft, "{", "the invalid draft remains editable");
  assert.match(ui.review().composeNote, /plan/i);
});

test("manual-review fallback retains designation; nonoperator UI defaults to ordinary composition", async () => {
  const ui = flow({ composeAvailable: false }); ui.render();
  const checkbox = ui.nodes().find(node => node.type === "input" && node.props.type === "checkbox");
  assert.equal(checkbox.props.checked, false);
  checkbox.props.onChange({ target: { checked: true } }); ui.render();
  await ui.compose();
  assert.deepEqual(JSON.parse(JSON.stringify(ui.composeCalls[0])), ["Onboard configured provider", true, { budget_tokens: null, role_tokens: {} }]);
  ui.nodes().find(node => node.type === "button" && node.props.children === "Review a manual package instead").props.onClick();
  ui.render();
  assert.equal(ui.review().qualificationRun, true);
  const ordinary = flow({ canQualify: false }); ordinary.render();
  assert.equal(ordinary.nodes().some(node => node.props?.type === "checkbox"), false);
  await ordinary.compose();
  assert.equal(ordinary.composeCalls[0][1], undefined);
  assert.equal(ordinary.review().qualificationRun, false);
});

test("review serializes the authorized checkbox outside the blueprint and omits it for ordinary users", async () => {
  const ui = flow(); ui.render(); await ui.compose();
  const jsx = (type, props) => ({ type, props });
  const imports = { "react/jsx-runtime": { jsx, jsxs: jsx }, "./qualification-choice": choiceModule };
  for (const name of ["@/components/segmented-tier", "@/components/orchestration-cube",
    "@/components/validation-checks", "@/components/icon", "@/components/journey-rail", "@/lib/format",
    "@/components/repo-access", "../envelope-reveal", "./controls", "./helpers"]) imports[name] = {};
  imports["@/components/repo-access"].missionGithubWriteEnabled = () => false;
  const { renderReview } = load("app/workspace/new/intake-flow/review.tsx", imports);
  for (const canQualify of [false, true]) {
    for (const qualificationRun of [false, true]) {
      const nodes = descendants(renderReview({ ...ui.review(), canQualify, qualificationRun }));
      const field = nodes.find(node => node.props?.name === "qualification_run");
      assert.equal(field?.props.value, canQualify ? (qualificationRun ? "on" : "off") : undefined);
      assert.equal(JSON.parse(nodes.find(node => node.props?.name === "blueprint_json").props.value).qualification_run, undefined);
      const choice = nodes.find(node => node.type === choiceModule.QualificationChoice);
      assert.equal(choice.props.canQualify, canQualify);
      assert.equal(choice.props.checked, qualificationRun);
    }
  }
});

test("Mission page requires operator capability and available mode metadata, never query parameters", async () => {
  for (const allowed of [false, true]) {
    for (const available of [true, false, undefined, "unreachable"]) {
      const jsx = (type, props) => ({ type, props });
      const { default: Page } = load("app/workspace/new/page.tsx", {
        "react/jsx-runtime": { jsx, jsxs: jsx },
        "./intake-flow": { IntakeFlow: "IntakeFlow" },
        "@/lib/bff": { getOptions: async () => {
          if (available === "unreachable") throw new Error("unavailable");
          return { per_run_qualification_available: available };
        }, getEfficiency: async () => null },
        "@/lib/session": { hasRole: async role => { assert.equal(role, "operator"); return allowed; } },
      });
      const tree = await Page({ searchParams: Promise.resolve({ intent: "Onboard", qualification_run: true }) });
      assert.equal(descendants(tree).find(node => node.type === "IntakeFlow").props.canQualify, allowed && available === true);
    }
  }
});

test("persisted Mission pages include designation in header, Overview and Activity and exclude replication", () => {
  const path = "app/workspace/missions/[name]/page.tsx";
  const source = ts.createSourceFile(path, readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const guards = [], reviews = [];
  let evidenceCount = 0, replicationCount = 0;
  const visit = node => {
    if (ts.isJsxExpression(node) && node.expression && ts.isBinaryExpression(node.expression)) {
      const right = node.expression.right;
      if (ts.isJsxSelfClosingElement(right)) {
        const tag = right.tagName.getText(source);
        if (tag === "MissionQualificationRecord" || tag === "ReliabilityRunner") {
          guards.push(node.getText(source));
          if (tag === "MissionQualificationRecord") evidenceCount++; else replicationCount++;
        }
      }
    }
    if (ts.isJsxSelfClosingElement(node) && node.tagName.getText(source) === "ReviewPanel") reviews.push(node);
    ts.forEachChild(node, visit);
  };
  visit(source);
  assert.equal(evidenceCount, 3);
  assert.equal(replicationCount, 1);
  assert.equal(reviews.length, 2);
  for (const review of reviews) assert.equal(review.attributes.properties.find(prop => prop.name?.getText(source) === "qualificationRun")
    ?.initializer?.expression?.getText(source), "!!task.qualification_run");
  const exports = {};
  runInNewContext(ts.transpileModule(`export function Evidence({task}) {
    const currentResult = {}, ns = "workspace", name = "draft";
    return <>${guards.join("")}</>;
  }`, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
  { exports, require, ...qualificationComponents, ReliabilityRunner: () => React.createElement("button", null, "Replicate") });
  const saved = render(exports.Evidence, { task: { qualification_run: record } });
  assert.equal((saved.match(/aria-label="Qualification run"/g) ?? []).length, 3);
  assert.doesNotMatch(saved, />Replicate</);
  const ordinary = render(exports.Evidence, { task: { qualification_run: null } });
  assert.match(ordinary, />Replicate</);
  assert.doesNotMatch(ordinary, /Qualification run/);
});

test("qualification toggles invalidate successful and in-flight preflight, including toggling back", async () => {
  const ui = flow(); ui.render(); await ui.compose();
  const validation = ui.review().runValidation();
  ui.validationCalls[0].resolve({ ok: true, checks: [] }); await validation; ui.render();
  assert.equal(ui.review().validationFresh, true);
  ui.review().setLaunch(true); ui.render();
  ui.review().changeQualification(true); ui.render();
  assert.equal(ui.review().validationFresh, false);
  assert.equal(ui.review().launch, false);
  const pending = ui.review().runValidation();
  assert.equal(ui.validationCalls[1].args[1].qualification_run, true);
  assert.equal(ui.validationCalls[1].args[0].qualification_run, undefined);
  ui.review().changeQualification(false); ui.render();
  ui.review().changeQualification(true); ui.render();
  ui.validationCalls[1].resolve({ ok: true, checks: [] }); await pending; ui.render();
  assert.equal(ui.review().validationFresh, false);
  assert.equal(ui.review().validation, null);
  assert.equal(ui.review().qualificationRun, true);
});
