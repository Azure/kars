// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const plain = value => JSON.parse(JSON.stringify(value));
const jsx = (type, props, key) => ({ type, props: props ?? {}, key });
const jsxRuntime = { jsx, jsxs: jsx, Fragment: "fragment" };
function load(path, imports = {}) {
  const exports = {};
  const result = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics, [], `Syntax diagnostics in ${path}`);
  runInNewContext(result.outputText, { exports, Error, FormData, require(name) {
    if (name === "react/jsx-runtime") return jsxRuntime;
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } });
  return exports;
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree?.props) return [];
  return [tree, ...nodes(tree.props.children)];
}
function hooks() {
  const state = [], transitions = [];
  let cursor = 0;
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in state)) state[index] = typeof initial === "function" ? initial() : initial;
      return [state[index], value => { state[index] = typeof value === "function" ? value(state[index]) : value; }];
    },
    useRef: initial => react.useState({ current: initial })[0],
    useMemo: compute => compute(), useEffect() {},
    useActionState: () => [{ error: null }, () => {}, false],
    useTransition: () => [false, callback => transitions.push(callback())],
  };
  return { react, render(component, props) { cursor = 0; return nodes(component(props)); },
    settle: () => Promise.all(transitions) };
}
const item = { id: "first", title: "Read reference", description: "Use the reviewed MCP natively",
  depends_on: [], acceptance_criteria: ["Deliver source-backed findings"], review_required: true,
  status: "pending", feedback: "Retain provenance", history: [] };
const budgetModule = load("lib/team-budget.ts");
const profileModule = load("lib/team-profile.ts", { "./team-budget": budgetModule });
const redirected = promise => assert.rejects(promise, error => error.path === "/workspace/teams/review-team");
function form(overrides = {}) {
  const result = new FormData();
  for (const [key, value] of Object.entries({ name: "review-team", charter: "Deliver source-backed research", tier: "2",
    budget_tokens: "10000", budget_scope: "GovernedInference", qualification_run: "true",
    runtime: "OpenClaw", model: "github-copilot::principal", mcp_servers: "docs", launch: "on",
    roles_json: JSON.stringify([{ name: "researcher", runtime: "Hermes", model: "foundry::worker", skills: [] }]),
    milestones_json: JSON.stringify([item]), ...overrides })) result.set(key, value);
  return result;
}
class TestBffError extends Error {}
function newActions(roles = ["operator"], failure, composeError) {
  const calls = [];
  const bff = {
    BffError: TestBffError,
    composeTeam: async (...args) => {
      calls.push(["compose", ...args]);
      if (composeError) throw composeError;
      return { available: true };
    },
    createTeam: async (...args) => calls.push(["create", ...args]),
    getTeam: async (...args) => { calls.push(["get", ...args]); return { uid: "team-uid" }; },
    qualifyTeam: async (...args) => { calls.push(["attempt", ...args]); if (failure) throw new Error(failure); },
    authenticatedBffFetch: async (path, init) => {
      calls.push([init.method, path, plain(init)]);
      return { ok: true, json: async () => item };
    },
    putEngineeringSource: () => assert.fail("qualification must not enable continuous intake"),
  };
  return { calls, ...load("app/workspace/teams/new/actions.ts", {
    "next/navigation": { redirect: path => { throw Object.assign(new Error("redirect"), { path }); } },
    "@/lib/config": { defaultNamespace: () => "work" }, "@/lib/team-budget": budgetModule,
    "@/lib/session": { currentPrincipal: async () => ({ name: "Reviewer", roles }) }, "@/lib/bff": bff,
  }) };
}

test("Team composition preserves safe validation errors without exposing upstream details", async () => {
  for (const [status, code] of [[400, "bad_request"], [409, "conflict"], [422, "rejected"]]) {
    const message = "Per-run qualification requires required mode; it cannot use global validation mode";
    const error = Object.assign(new TestBffError(message), { status, code });
    const result = await newActions(["operator"], undefined, error).composeTeamAction("Reviewed charter", undefined, true);
    assert.equal(result.available, false);
    assert.equal(result.reason, message);
    assert.equal(result.proposal, null);
  }
  for (const error of [
    new Error("private transport details"),
    Object.assign(new TestBffError("private upstream details"), { status: 503, code: "upstream" }),
    Object.assign(new TestBffError("private upstream details"), { status: 400, code: "upstream" }),
  ]) {
    const result = await newActions(["operator"], undefined, error).composeTeamAction("Reviewed charter");
    assert.match(result.reason, /orchestrator is unavailable/);
    assert.doesNotMatch(result.reason, /private/);
  }
  const invalid = Object.assign(new TestBffError("private parser details"), { status: 422, code: "unknown" });
  assert.match((await newActions(["operator"], undefined, invalid).composeTeamAction("Reviewed charter")).reason, /invalid fields/);
  const timeout = Object.assign(new Error("private timeout details"), { name: "TimeoutError" });
  assert.match((await newActions(["operator"], undefined, timeout).composeTeamAction("Reviewed charter")).reason, /within five minutes/);
});

test("Team page offers qualification only with role and explicit available mode metadata", async () => {
  for (const roles of [["viewer"], ["operator"], ["admin"]]) {
    for (const available of [true, false, undefined]) {
      const { default: Page } = load("app/workspace/teams/new/page.tsx", {
        "@/lib/bff": { getOptions: async () => ({ per_run_qualification_available: available,
          mcp_servers: [], tool_policies: [], memories: [], skills: [] }) },
        "@/lib/config": { defaultNamespace: () => "work" },
        "@/lib/session": { currentPrincipal: async () => ({ roles }) },
        "@/lib/team-profile": profileModule,
        "@/components/honest-state": { HonestState: "HonestState" },
        "./team-composer": { TeamComposer: "TeamComposer" },
      });
      const tree = await Page({ searchParams: Promise.resolve({ qualification_run: true }) });
      const composer = nodes(tree).find(node => node.type === "TeamComposer");
      assert.equal(composer.props.canQualify, available === true && !roles.includes("viewer"));
    }
  }
});

test("Team designation rejects nonoperators before compose or create; ordinary composition remains unchanged", async () => {
  for (const roles of [[], ["viewer"], ["user"]]) {
    const api = newActions(roles);
    assert.equal((await api.composeTeamAction("Review the docs", undefined, true)).available, false);
    assert.match((await api.createTeamAction({}, form())).error, /operator or admin/);
    assert.equal(api.calls.length, 0);
    await api.composeTeamAction("Ordinary work");
    assert.deepEqual(api.calls[0], ["compose", "work", "Ordinary work", undefined, undefined, undefined]);
  }
});

test("Team designation preserves mixed routes and finite budget, creates paused, then authorizes only the exact returned item", async () => {
  for (const role of ["operator", "admin"]) {
    for (const launch of ["on", "off"]) {
      const api = newActions([role]);
      const spending = { budget_tokens: 10000, role_tokens: { researcher: 4000 } };
      await api.composeTeamAction("Review the docs", "profile", true, spending);
      assert.deepEqual(api.calls.shift(), ["compose", "work", "Review the docs", "profile", true, spending]);
      await redirected(api.createTeamAction({}, form({ launch })));
      assert.deepEqual(api.calls.map(call => call[0]), launch === "on" ? ["create", "POST", "get", "attempt"] : ["create", "POST"]);
      const body = api.calls[0][2];
      assert.equal(body.launch, false);
      assert.equal(body.qualification_run, true);
      assert.equal(body.runtime, "OpenClaw");
      assert.equal(body.model, "github-copilot::principal");
      assert.deepEqual(plain(body.roles), [{ name: "researcher", runtime: "Hermes", model: "foundry::worker", skills: [] }]);
      assert.equal(body.budget.tokens, 10000);
      assert.equal(body.budget.scope, "GovernedInference");
      assert.deepEqual(plain(body.mcp_servers), ["docs"]);
      if (launch === "on") assert.deepEqual(api.calls[3], ["attempt", "work", "review-team", "team-uid", item]);
    }
  }
});

test("Team creation sends Strict by default and rejects unsupported egress before any mutation", async () => {
  for (const mode of [undefined, "strict", " Strict "]) {
    const api = newActions();
    const input = form({ launch: "off", ...(mode === undefined ? {} : { egress_mode: mode }) });
    await redirected(api.createTeamAction({}, input));
    assert.equal(api.calls[0][2].egress_mode, "strict");
  }
  for (const mode of ["learning", "Learn", "", "observe"]) {
    const api = newActions();
    assert.match((await api.createTeamAction({}, form({ egress_mode: mode }))).error, /Standing Teams use Strict egress/);
    assert.equal(api.calls.length, 0);
  }
});

test("Team designation requires independent reviewed work and retains the Team after uncertain attempt submission", async () => {
  for (const milestones of [[], [{ ...item, depends_on: ["prior"] }]]) {
    const api = newActions();
    assert.match((await api.createTeamAction({}, form({ milestones_json: JSON.stringify(milestones) }))).error, /reviewed first milestone/);
    assert.equal(api.calls.length, 0);
  }
  const api = newActions(["operator"], "connection lost after submission");
  assert.match((await api.createTeamAction({}, form())).error, /retained for inspection.*connection lost.*already have been accepted/);
  assert.deepEqual(api.calls.map(call => call[0]), ["create", "POST", "get", "attempt"]);
  const ordinary = newActions();
  await redirected(ordinary.createTeamAction({}, form({ qualification_run: "false" })));
  assert.deepEqual(ordinary.calls.map(call => call[0]), ["create", "POST", "PATCH"]);
  assert.deepEqual(JSON.parse(ordinary.calls[2][2].body), { paused: false });
});

const approveLabel = "Approve one attempt, then pause";
const releaseLabel = "Check qualified evidence and release — remain paused";
const button = (h, label) => h.render().find(node => node.type === "button" && node.props.children === label);
const checkbox = h => h.render().find(node => node.type === "input" && node.props.type === "checkbox");
function control(state = "pending", canQualify = true, error = null) {
  const runner = hooks(), calls = [];
  let refreshes = 0;
  const props = { canQualify, team: { name: "review-team", uid: "team-uid", paused: true, tasks: [plain(item)],
    qualification: { state, note: "Unqualified: one exact attempt only", expires_at: "2026-10-18T18:45:00Z" } } };
  const { QualificationControl } = load("app/workspace/teams/[name]/qualification-control.tsx", {
    react: runner.react, "next/navigation": { useRouter: () => ({ refresh: () => refreshes++ }) },
    "./qualification-actions": { teamQualificationAction: async (...args) => { calls.push(args); return { error }; } },
  });
  return { props, calls, render: () => runner.render(QualificationControl, props), settle: runner.settle,
    get refreshes() { return refreshes; } };
}

test("Team review controls keep unqualified status visible but hide mutations from nonoperators and ordinary Teams", () => {
  for (const state of ["pending", "requested", "running", "blocked", "awaiting_evidence"]) {
    const h = control(state, false);
    assert.ok(h.render().some(node => node.type === "h2"));
    assert.equal(h.render().some(node => node.type === "button" || node.type === "input"), false);
  }
  const h = control(); h.props.team.qualification = null;
  assert.deepEqual(h.render(), []);
});

test("Team attempt control requires fresh consent to the exact Team and next eligible item", async () => {
  for (const mutation of ["uid", "work", "package"]) {
    const h = control();
    assert.equal(button(h, approveLabel).props.disabled, true);
    checkbox(h).props.onChange({ target: { checked: true } });
    assert.equal(button(h, approveLabel).props.disabled, false);
    if (mutation === "uid") h.props.team.uid = "replacement";
    if (mutation === "work") h.props.team.tasks = [{ ...item, feedback: "Changed reviewed intent" }];
    if (mutation === "package") h.props.team.model = "foundry::replacement";
    assert.equal(button(h, approveLabel).props.disabled, true);
    assert.equal(checkbox(h).props.checked, false);
    assert.equal(h.calls.length, 0);
  }
  const h = control();
  checkbox(h).props.onChange({ target: { checked: true } });
  button(h, approveLabel).props.onClick();
  await h.settle();
  assert.deepEqual(h.calls, [["review-team", "team-uid", "attempt", h.props.team.tasks[0]]]);
  assert.equal(h.refreshes, 1);
  assert.equal(checkbox(h).props.checked, false);
  for (const status of ["active", "awaiting_review"]) {
    h.props.team.tasks = [item, { ...item, id: "busy", status }];
    assert.equal(button(h, approveLabel), undefined);
  }
  h.props.team.tasks = [{ ...item, depends_on: ["missing"] }];
  assert.equal(button(h, approveLabel), undefined);
});

test("Team evidence release is separate consent, never resume; failures remain visible and require fresh review", async () => {
  const h = control("awaiting_evidence", true, "Evidence does not match the resource version");
  assert.equal(button(h, approveLabel), undefined);
  assert.equal(button(h, releaseLabel).props.disabled, true);
  checkbox(h).props.onChange({ target: { checked: true } });
  button(h, releaseLabel).props.onClick();
  await h.settle();
  assert.equal(h.calls[0][2], "review");
  assert.equal(button(h, releaseLabel).props.disabled, true);
  assert.ok(h.render().some(node => node.props.role === "alert" && /resource version/.test(node.props.children)));
  assert.equal(h.refreshes, 1);
  h.props.team.qualification.state = "blocked";
  assert.equal(button(h, releaseLabel), undefined);
  assert.equal(button(h, approveLabel), undefined);
});

function manage(roles = ["operator"], status = 200) {
  const calls = [];
  return { calls, ...load("app/workspace/teams/[name]/qualification-actions.ts", {
    "@/lib/session": { currentPrincipal: async () => ({ roles }) },
    "@/lib/config": { defaultNamespace: () => "work/space" },
    "@/lib/bff": {
      qualifyTeam: async (...args) => calls.push(["attempt", ...args]),
      authenticatedBffFetch: async (...args) => { calls.push(["patch", ...args]); return {
        ok: status === 200, status, json: async () => ({ error: { message: "Exact binding changed" } }),
      }; },
    },
  }) };
}

test("Team qualification server actions reject unauthorized or unknown actions and missing reviewed work without writes", async () => {
  for (const action of ["attempt", "pause", "review"]) {
    const api = manage(["viewer"]);
    assert.match((await api.teamQualificationAction("team", "uid", action, item)).error, /operator or admin/);
    assert.equal(api.calls.length, 0);
  }
  const api = manage();
  assert.match((await api.teamQualificationAction("team", "uid", "resume", item)).error, /Unknown/);
  assert.match((await api.teamQualificationAction("team", "uid", "attempt")).error, /eligible pending/);
  assert.equal(api.calls.length, 0);
});

test("Team qualification server actions preserve identity, exact snapshot, no-store and pause-only release semantics", async () => {
  for (const role of ["operator", "admin"]) {
    const api = manage([role]);
    assert.equal((await api.teamQualificationAction("team/name", "uid", "attempt", item)).error, null);
    assert.deepEqual(api.calls.shift(), ["attempt", "work/space", "team/name", "uid", item]);
    for (const action of ["pause", "review"]) {
      assert.equal((await api.teamQualificationAction("team/name", "uid", action)).error, null);
      const [, path, init] = api.calls.shift();
      assert.equal(path, "/api/namespaces/work%2Fspace/teams/team%2Fname");
      assert.equal(init.method, "PATCH"); assert.equal(init.cache, "no-store");
      assert.deepEqual(JSON.parse(init.body), action === "pause" ? { paused: true } : {
        qualification_reviewed: true, qualification_team_uid: "uid",
      });
    }
  }
  const refused = manage(["operator"], 409);
  assert.equal((await refused.teamQualificationAction("team", "uid", "review")).error, "Exact binding changed");
});

function composer(canQualify, proposal = {}) {
  const runner = hooks(), calls = [], noop = () => null;
  let org;
  const PreflightCheck = noop;
  const { TeamComposer } = load("app/workspace/teams/new/team-composer.tsx", {
    react: runner.react, "./actions": { createTeamAction: noop, composeTeamAction: async (...args) => {
      calls.push(args); return { available: true, proposal: { tier: 2, budget_tokens: 10000, roles: [],
        execution_plan: { tasks: [] }, engineering_enabled: true, milestones: [item], ...proposal } };
    } },
    "@/components/repo-access": { RepoAccess: noop }, "@/components/orchestration-cube": { OrchestrationCube: noop },
    "@/components/validation-checks": { CompositionNote: noop },
    "@/components/role-budget-fields": { RoleBudgetFields: noop },
    "@/lib/budget-review": load("lib/budget-review.ts"), "@/components/icon": { Icon: noop }, "@/components/preflight-check": { PreflightCheck },
    "@/components/loop-designer": { LoopDesigner: noop }, "@/lib/team-budget": budgetModule,
    "@/lib/team-profile": profileModule, "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "./team-composer-panels": { renderGovernancePanel: noop, renderOrgPanel: input => { org = input; return null; } },
  });
  const props = { canQualify, initialCharter: "Read the reviewed documentation", options: {
    models: [], runtimes: [], skills: [], mcp_servers: [], memories: [],
  } };
  return { calls, render: () => runner.render(TeamComposer, props), PreflightCheck, org: () => org };
}
const designation = h => h.render().find(node => node.type === "label" && JSON.stringify(node.props.children).includes("Operator qualification:"));
const field = (h, name) => h.render().find(node => node.type === "input" && node.props.name === name);

test("Team selected repositories are controlled, invalidate draft approval, and survive Back/recompose", async () => {
  const h = composer(true, { engineering_enabled: false });
  const compose = async () => h.render().find(node => node.type === "button" && JSON.stringify(node.props.children).includes("Compose the org")).props.onClick();
  await compose();
  field(h, "name").props.onChange({ target: { value: "review-team" } });
  const repos = () => h.render().find(node => "onSelectionChange" in node.props);
  const preflight = () => h.render().find(node => "qualificationRun" in node.props && "selectedRepos" in node.props);
  const submit = () => h.render().find(node => node.type === "button" && node.props.type === "submit");
  repos().props.onSelectionChange(["owner/repo"]);
  const before = preflight();
  assert.deepEqual(plain(before.props.selectedRepos), ["owner/repo"]);
  assert.deepEqual(plain(repos().props.selected), ["owner/repo"]);
  assert.equal(submit().props.disabled, true);
  before.props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, false);
  repos().props.onSelectionChange(["owner/other"]);
  assert.notEqual(preflight().key, before.key);
  before.props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, true);
  h.render().find(node => node.type === "button" && node.props.children === "← Back to charter").props.onClick();
  await compose();
  assert.deepEqual(plain(repos().props.selected), ["owner/other"]);
  assert.deepEqual(plain(preflight().props.selectedRepos), ["owner/other"]);
  assert.equal(submit().props.disabled, true);
  const api = newActions();
  await redirected(api.createTeamAction({}, form({ launch: "off", git_write_repos: "owner/other" })));
  assert.deepEqual(plain(api.calls[0][2].git_write_repos), ["owner/other"]);
});

test("Team proposal resource selections survive review, preflight and submission; edits invalidate approval", async () => {
  const resources = { mcp_servers: ["docs"], egress: [{ host: "docs.example", port: 443 }] };
  const h = composer(true, { mcp_servers: ["docs"], egress: resources.egress,
    roles: [{ name: "reader", runtime: "Hermes", model: "foundry::worker", skills: [], resources }] });
  await h.render().find(node => node.type === "button" && JSON.stringify(node.props.children).includes("Compose the org")).props.onClick();
  assert.deepEqual(JSON.parse(field(h, "roles_json").props.value)[0].resources, resources);
  const preflight = () => h.render().find(node => node.type === h.PreflightCheck && "qualificationRun" in node.props);
  assert.deepEqual(plain(preflight().props.teamRoles[0].resources), resources);
  const before = preflight().key;
  const org = h.org();
  org.patchRole(org.roles[0].id, { resources: { mcp_servers: [], egress: [] } });
  assert.notEqual(preflight().key, before);
  assert.deepEqual(JSON.parse(field(h, "roles_json").props.value)[0].resources, { mcp_servers: [], egress: [] });
  const api = newActions();
  await redirected(api.createTeamAction({}, form({ launch: "off", roles_json: JSON.stringify([
    { name: "reader", runtime: "Hermes", model: "foundry::worker", skills: [], resources },
  ]) })));
  assert.deepEqual(plain(api.calls[0][2].roles[0].resources), resources);
});

test("Team composer designation is operator-only, defaults off, propagates to proposal/preflight and invalidates prior approval", async () => {
  assert.equal(designation(composer(false)), undefined);
  const h = composer(true);
  const toggle = () => nodes(designation(h)).find(node => node.type === "input");
  assert.equal(toggle().props.checked, false);
  toggle().props.onChange({ target: { checked: true } });
  await h.render().find(node => node.type === "button" && JSON.stringify(node.props.children).includes("Compose the org")).props.onClick();
  assert.equal(h.calls[0][2], true);
  assert.equal(field(h, "qualification_run").props.value, "true");
  assert.equal(field(h, "engineering_enabled").props.value, "false");
  assert.equal(field(h, "launch").props.checked, false);
  const preflight = () => h.render().find(node => node.type === h.PreflightCheck && "qualificationRun" in node.props);
  assert.equal(preflight().props.qualificationRun, true);
  assert.equal(preflight().props.budgetTokens, 10000);
  field(h, "name").props.onChange({ target: { value: "review-team" } });
  field(h, "launch").props.onChange({ target: { checked: true } });
  const prior = preflight();
  prior.props.onResult({ ok: true, checks: [] });
  assert.equal(h.render().find(node => node.type === "button" && node.props.type === "submit").props.disabled, false);
  toggle().props.onChange({ target: { checked: false } });
  assert.equal(field(h, "launch").props.checked, false);
  assert.equal(field(h, "qualification_run").props.value, "false");
  assert.notEqual(preflight().key, prior.key);
  assert.equal(preflight().props.qualificationRun, false);
  field(h, "launch").props.onChange({ target: { checked: true } });
  prior.props.onResult({ ok: true, checks: [] });
  assert.equal(h.render().find(node => node.type === "button" && node.props.type === "submit").props.disabled, true);
});
