// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import vm from "node:vm";

const ts = createRequire(import.meta.url)("typescript");
function load(path, imports = {}, globals = {}) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, Error, FormData, Headers, ...globals,
    require(name) {
      if (name === "@/lib/budget-review") return load("../src/lib/budget-review.ts");
      if (name === "@/components/role-budget-fields") return { RoleBudgetFields: "RoleBudgetFields" };
      if (name === "react" && !(name in imports)) return { useState: initial => [initial, () => {}] };
      if (name === "@/lib/team-profile") return profileModule;
      if (name === "@/lib/team-roster-plan") return load("../src/lib/team-roster-plan.ts", {}, { TextEncoder });
      if (name === "@/components/team-role-resources") return resourceModule;
      assert.ok(name in imports, `unexpected import ${name}`); return imports[name];
    },
  }, { filename: path });
  return exports;
}
const plain = (value) => JSON.parse(JSON.stringify(value));
const budgetModule = load("../src/lib/team-budget.ts");
const budget = { scope: "GovernedInference", tokens: 2000000, usd_micros: null };
function form(overrides = {}) {
  const result = new FormData();
  for (const [key, value] of Object.entries({
    name: "briefing-team", charter: "Produce recurring internal briefings", tier: "1",
    budget_tokens: "2000000", budget_scope: "GovernedInference", roles: "[]", ...overrides,
  })) if (value !== null) result.set(key, value);
  return result;
}
function actions(bff, principal = async () => ({ namespace: "kars-system" })) {
  return load("../src/app/workspace/teams/new/actions.ts", {
    "next/navigation": { redirect(path) { throw Object.assign(new Error("redirect"), { path }); } },
    "@/lib/bff": { BffError: class extends Error {}, ...bff }, "@/lib/config": { defaultNamespace: () => "kars-system" },
    "@/lib/session": { currentPrincipal: principal }, "@/lib/team-budget": budgetModule,
  });
}
const redirected = (promise) => assert.rejects(promise, (error) => error.path === "/workspace/teams/briefing-team");

test("Team parser preserves finite caps and explicit blank unlimited tokens with governed scope", () => {
  assert.deepEqual(plain(budgetModule.parseTeamBudget(" 2000000 ", "GovernedInference").budget), budget);
  for (const tokens of ["", " "]) {
    assert.deepEqual(plain(budgetModule.parseTeamBudget(tokens, "GovernedInference").budget), { ...budget, tokens: null });
    assert.deepEqual(plain(budgetModule.parseTeamBudget(tokens, "GovernedInference", "7500000").budget), { ...budget, tokens: null, usd_micros: 7500000 });
    assert.equal(budgetModule.parseTeamBudget(tokens, "GovernedInference", "invalid").budget, null);
  }
  for (const tokens of [undefined, null, "0", "-1", "1.5", "NaN", "Infinity", "9007199254740992", 2000000, true, {}, []]) {
    assert.equal(budgetModule.parseTeamBudget(tokens, "GovernedInference").budget, null);
  }
  for (const scope of [undefined, null, "", "Planning", "governedinference"]) {
    assert.equal(budgetModule.parseTeamBudget("2000000", scope).budget, null);
  }
});

test("invalid Team budgets stop before principal lookup or API calls", async () => {
  const fail = () => assert.fail("invalid cap must not reach authentication or create");
  const { createTeamAction } = actions({ createTeam: fail }, fail);
  for (const overrides of [
    { budget_tokens: null }, { budget_tokens: "0" },
    { budget_tokens: "-1" }, { budget_tokens: "0.5" }, { budget_tokens: "9007199254740992" },
    { budget_scope: null }, { budget_scope: "Planning" },
  ]) assert.match((await createTeamAction({}, form(overrides))).error, /limit|scope/);
});

test("Team action preserves explicit unlimited tokens, optional cost limits and paused creation", async () => {
  const calls = [];
  const { createTeamAction } = actions({ createTeam: async (_, body) => { calls.push(plain(body)); } });
  await redirected(createTeamAction({}, form({ budget_tokens: "" })));
  await redirected(createTeamAction({}, form({ budget_tokens: "", budget_usd_micros: "7500000" })));
  assert.deepEqual(calls.map((body) => body.budget), [
    { scope: "GovernedInference", tokens: null, usd_micros: null },
    { scope: "GovernedInference", tokens: null, usd_micros: 7500000 },
  ]);
  assert.ok(calls.every((body) => body.launch === false));
});

test("Team action preserves reviewed cap and defaults to paused creation", async () => {
  const calls = [];
  const { createTeamAction } = actions({ createTeam: async (...args) => { calls.push(args); } });
  await redirected(createTeamAction({}, form()));
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "kars-system");
  assert.deepEqual(plain(calls[0][1].budget), budget);
  assert.equal(calls[0][1].launch, false);
  assert.equal("lifecycle_mode" in calls[0][1], false);
  assert.equal("warm_idle_seconds" in calls[0][1], false);
});

test("stale Team forms cannot silently request unsupported warm runtimes", async () => {
  const fail = () => assert.fail("unsupported runtime must not reach authentication or create");
  const { createTeamAction } = actions({ createTeam: fail }, fail);
  for (const overrides of [
    { lifecycle_mode: "resourceOptimized" }, { lifecycle_mode: "persistent" },
    { lifecycle_mode: "unknown" }, { warm_idle_seconds: "900" },
    { warm_idle_seconds: "-1" }, { warm_idle_seconds: "NaN" },
  ]) assert.match((await createTeamAction({}, form(overrides))).error, /not currently supported/);
  const calls = [];
  const compatible = actions({ createTeam: async (_, body) => calls.push(body) });
  await redirected(compatible.createTeamAction({}, form({
    lifecycle_mode: "ephemeral", warm_idle_seconds: "0",
  })));
  assert.equal("lifecycle_mode" in calls[0], false);
  assert.equal("warm_idle_seconds" in calls[0], false);
});

test("Team action preserves explicit profile authority, including zero delegation, without changing defaults", async () => {
  const calls = [];
  const { createTeamAction } = actions({ createTeam: async (_, request) => calls.push(request) });
  await redirected(createTeamAction({}, form({ tier: "3", authority_ceiling: "2", delegation_depth: "0" })));
  assert.equal(calls[0].authority_ceiling, 2);
  assert.equal(calls[0].delegation_depth, 0);
  await redirected(createTeamAction({}, form()));
  assert.equal(calls[1].authority_ceiling, undefined);
  assert.equal(calls[1].delegation_depth, undefined);
  const invalid = actions({ createTeam: () => assert.fail("invalid authority reached create") },
    () => assert.fail("invalid authority reached authentication")).createTeamAction;
  for (const patch of [
    { tier: "0" }, { tier: "1.2" }, { authority_ceiling: "2" },
    { authority_ceiling: "0" }, { authority_ceiling: "NaN" },
    { delegation_depth: "-1" }, { delegation_depth: "1.5" }, { delegation_depth: "2147483648" },
  ]) assert.ok((await invalid({}, form(patch))).error);
});

test("milestone launch keeps create-paused, setup, then resume ordering", async () => {
  const calls = [];
  const { createTeamAction } = actions({
    createTeam: async (_, request) => calls.push(["create", plain(request)]),
    authenticatedBffFetch: async (path, init) => {
      calls.push([init.method === "POST" ? "milestone" : "resume", path, plain(init)]);
      return { ok: true };
    },
  });
  await redirected(createTeamAction({}, form({ launch: "on", milestones_json: JSON.stringify([
    { id: "brief", title: "Draft first briefing", acceptance_criteria: ["Useful concise output"] },
  ]) })));
  assert.deepEqual(calls.map(([kind]) => kind), ["create", "milestone", "resume"]);
  assert.equal(calls[0][1].launch, false);
  assert.deepEqual(calls[0][1].budget, budget);
  assert.deepEqual(JSON.parse(calls[2][2].body), { paused: false });
  assert.equal(calls[2][2].method, "PATCH");
  assert.equal(calls[1][2].cache, "no-store");
  assert.equal(calls[1][1], "/api/namespaces/kars-system/teams/briefing-team/tasks");
});

test("failed milestone setup cleans up without resuming a partially configured Team", async () => {
  const calls = [];
  const { createTeamAction } = actions({
    createTeam: async () => calls.push("create"),
    authenticatedBffFetch: async (_, init) => {
      if (init.method === "POST") { calls.push("milestone"); throw new Error("setup failed"); }
      assert.equal(init.method, "DELETE", "must clean up, not resume");
      calls.push("cleanup");
      return { ok: true };
    },
  });
  const result = await createTeamAction({}, form({ launch: "on", milestones_json: '[{"title":"Draft first briefing"}]' }));
  assert.match(result.error, /setup failed/);
  assert.deepEqual(calls, ["create", "milestone", "cleanup"]);
});

test("authenticated Team create client serializes reviewed budget without defaults", async () => {
  const calls = [];
  const { createTeam } = load("../src/lib/bff.ts", {
    "./config": { bffBaseUrl: () => "https://bff.example" },
    "next/headers": { cookies: async () => ({ get: () => ({ value: "session" }) }) },
    "./session-token": { verifySession: () => ({ sub: "reviewer", namespace: "kars-system" }), SESSION_COOKIE: "session" },
    "./oidc-config": { ssoConfigured: () => true }, "./credential-review": {},
  }, { fetch: async (url, init) => { calls.push({ url, init }); return { ok: true, status: 200, json: async () => ({ name: "briefing-team" }) }; } });
  await createTeam("kars-system", { name: "briefing-team", charter: "Recurring briefings", budget });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "https://bff.example/api/namespaces/kars-system/teams");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.cache, "no-store");
  assert.equal(calls[0].init.headers.get("x-kars-principal-token"), "session");
  assert.deepEqual(JSON.parse(calls[0].init.body).budget, budget);
});

const jsx = (type, props, key) => ({ type, key, props: props ?? {} });
const jsxRuntime = { jsx, jsxs: jsx, Fragment: "fragment" };
const resourceModule = load("../src/components/team-role-resources.tsx", { "react/jsx-runtime": jsxRuntime });
function nodes(tree) {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  return [tree, ...nodes(tree.props?.children)];
}
function hooks() {
  let index = 0;
  const state = [];
  const react = {
    useState(initial) {
      const slot = index++;
      if (!(slot in state)) state[slot] = typeof initial === "function" ? initial() : initial;
      return [state[slot], (value) => { state[slot] = typeof value === "function" ? value(state[slot]) : value; }];
    },
    useRef(initial) { return react.useState({ current: initial })[0]; },
    useMemo(compute) { return compute(); }, useEffect() {},
    useActionState() { return [{}, () => {}, false]; },
    useTransition() { return [false, (fn) => fn()]; },
  };
  return { react, render(component, props) { index = 0; return component(props); } };
}

test("shared preflight forwards the exact reviewed scope, tokens and dollar cap", async () => {
  const calls = [];
  const runner = hooks();
  const { PreflightCheck } = load("../src/components/preflight-check.tsx", {
    react: runner.react, "react/jsx-runtime": jsxRuntime,
    "./validation-checks": load("../src/components/validation-checks.tsx", {
      "react/jsx-runtime": jsxRuntime,
    }),
    "@/lib/preflight-client": { checkPackage: async (...args) => { calls.push(plain(args)); return { ok: true, checks: [] }; } },
  });
  const button = (props) => nodes(runner.render(PreflightCheck, props)).find((node) => node.type === "button");
  assert.equal(button({ disabled: true }).props.disabled, true);
  button({ blueprint: { runtime: "openclaw" }, tier: 1, workload: "team", budgetTokens: 2000000, budgetScope: "GovernedInference", budgetUsdMicros: 5000000 }).props.onClick();
  await Promise.resolve();
  assert.deepEqual(calls[0], [{ runtime: "openclaw" }, { tier: 1, workload: "team", budget_tokens: 2000000, budget_scope: "GovernedInference", budget_usd_micros: 5000000 }]);
  button({ blueprint: {}, budgetTokens: 2000000 }).props.onClick();
  await Promise.resolve();
  assert.equal(calls[1][1].budget_scope, null, "positive tokens must not invent scope");
  assert.equal("team_roles" in calls[1][1], false, "Missions must keep inherited role routes");
  const teamRoles = [{ name: "reviewer", runtime: "Hermes", model: "foundry::gpt-5.4-mini", skills: [] }];
  button({ blueprint: { model: { provider: "github-copilot", deployment: "gpt-5.4" } },
    workload: "team", teamRoles, budgetTokens: 4800000, budgetScope: "GovernedInference" }).props.onClick();
  await Promise.resolve();
  assert.deepEqual(calls[2][1].team_roles, teamRoles);
  assert.equal(calls[2][0].model.provider, "github-copilot");
});

test("shared preflight server action preserves the lifetime envelope", async () => {
  const calls = [];
  const { validatePackageAction } = load("../src/lib/preflight-actions.ts", {
    "@/lib/config": { defaultNamespace: () => "kars-system" },
    "@/lib/bff": { validatePackage: async (...args) => { calls.push(plain(args)); return { ok: true }; } },
  });
  const envelope = { tier: 1, workload: "team", budget_tokens: 2000000, budget_scope: "GovernedInference", budget_usd_micros: null,
    team_roles: [{ name: "reviewer", runtime: "Hermes", model: "foundry::gpt-5.4-mini" }] };
  await validatePackageAction({ runtime: "openclaw" }, envelope);
  assert.deepEqual(calls, [["kars-system", { runtime: "openclaw" }, envelope]]);
});

for (const [proposedCap, profileCap] of [[null, null], [2400000, null], [2400000, 2500000]]) {
test(`Team composer preserves proposed/profile caps (${proposedCap}/${profileCap}) and invalidates changed preflight`, async () => {
  const runner = hooks();
  const PreflightCheck = () => null;
  const noop = () => null;
  const { TeamComposer } = load("../src/app/workspace/teams/new/team-composer.tsx", {
    react: runner.react, "react/jsx-runtime": jsxRuntime,
    "./actions": { createTeamAction: noop, composeTeamAction: async () => ({ available: true, proposal: {
      tier: 1, budget_tokens: proposedCap, cadence_minutes: 0, roles: [], execution_plan: { tasks: [] }, engineering_enabled: false,
    } }) },
    "@/components/repo-access": { RepoAccess: noop }, "@/components/orchestration-cube": { OrchestrationCube: noop },
    "@/components/validation-checks": { CompositionNote: noop }, "@/components/icon": { Icon: noop }, "@/components/preflight-check": { PreflightCheck },
    "@/components/loop-designer": { LoopDesigner: noop }, "@/lib/team-budget": budgetModule,
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "./team-composer-panels": { renderGovernancePanel: noop, renderOrgPanel: noop },
  });
  const props = { initialCharter: "Write recurring useful internal briefings", options: { models: [], runtimes: [], skills: [], mcp_servers: [], memories: [] },
    ...(profileCap == null ? {} : { profileEnvelope: { tier: 1, authorityCeiling: 1, delegationDepth: 3,
      budget: { scope: "GovernedInference", tokens: profileCap } } }),
  };
  const render = () => nodes(runner.render(TeamComposer, props));
  const text = (node) => JSON.stringify(node.props.children);
  await render().find((node) => node.type === "button" && text(node)?.includes("Compose the org")).props.onClick();
  const field = (name) => render().find((node) => node.type === "input" && node.props.name === name);
  const preflight = () => render().find((node) => node.type === PreflightCheck);
  const submit = () => render().find((node) => node.type === "button" && node.props.type === "submit");
  assert.equal(field("lifecycle_mode"), undefined);
  assert.equal(field("warm_idle_seconds"), undefined);
  assert.equal(render().some((node) =>
    node.type === "option" && ["persistent", "resourceOptimized"].includes(node.props.value)), false);
  assert.ok(render().some((node) =>
    node.type === "p" && text(node)?.includes("Durable Team, on-demand agent pods")));
  const expectedCap = proposedCap;
  assert.equal(field("budget_tokens").props.value, expectedCap?.toString() ?? "");
  assert.equal(field("budget_tokens").props.required, undefined);
  assert.equal(field("budget_tokens").props.placeholder, "Use existing controls");
  assert.equal(preflight().props.disabled, false);
  assert.equal(preflight().props.budgetTokens, expectedCap ?? null);
  assert.equal(submit().props.disabled, true);
  field("name").props.onChange({ target: { value: "briefing-team" } });
  field("budget_tokens").props.onChange({ target: { value: "2000000" } });
  field("launch").props.onChange({ target: { checked: true } });
  assert.equal(preflight().props.budgetTokens, 2000000);
  assert.equal(preflight().props.budgetScope, "GovernedInference");
  assert.equal(preflight().props.disabled, false);
  assert.equal(submit().props.disabled, true);
  preflight().props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, false);
  const validated = preflight();
  field("budget_tokens").props.onChange({ target: { value: "3000000" } });
  assert.notEqual(preflight().key, validated.key);
  assert.equal(submit().props.disabled, true);
  validated.props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, true, "late old-cap result must not authorize new cap");
  preflight().props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, false);
  const finiteValidation = preflight();
  field("budget_tokens").props.onChange({ target: { value: "" } });
  assert.equal(preflight().props.budgetTokens, null);
  assert.equal(preflight().props.budgetScope, "GovernedInference");
  assert.equal(preflight().props.disabled, false);
  assert.notEqual(preflight().key, finiteValidation.key);
  assert.equal(submit().props.disabled, true);
  finiteValidation.props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, true, "a finite-cap check cannot authorize a changed unlimited envelope");
  preflight().props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, false);
  field("budget_tokens").props.onChange({ target: { value: "0" } });
  assert.equal(preflight().props.disabled, true);
  assert.equal(submit().props.disabled, true);
});

}

const profileModule = load("../src/lib/team-profile.ts", { "./team-budget": budgetModule });
const profile = {
  name: "bounded", namespace: "kars-system", phase: "Ready", display_name: "Bounded review",
  tier: 3, charter_template: "Review supplied documents", roles: [{ name: "reviewer", skills: ["review"] }],
  spec: { defaultEnvelope: { tier: 3, authorityCeiling: 2, delegationDepth: 1 } },
};
test("scope-only and cost-only profiles preserve accounting scope and authority without inventing token caps", () => {
  for (const raw of [
    { scope: "GovernedInference" },
    { scope: "GovernedInference", tokens: null },
    { scope: "GovernedInference", usdMicros: 250000 },
  ]) {
    const result = profileModule.readTeamProfile({ ...profile,
      spec: { defaultEnvelope: { ...profile.spec.defaultEnvelope, budget: raw } },
    }, [{ name: "review" }]);
    assert.deepEqual(plain(result), {
      envelope: { ...profile.spec.defaultEnvelope,
        budget: { scope: "GovernedInference", tokens: null, usd_micros: raw.usdMicros ?? null },
      }, error: null,
    });
  }
});

test("profile drafts preserve representable authority and refuse silently dropping constraints or skills", async (t) => {
  assert.deepEqual(plain(profileModule.readTeamProfile(profile, [{ name: "review" }])), {
    envelope: profile.spec.defaultEnvelope, error: null,
  });

  await t.test("profile drafts retain both lifetime caps and the exact available authority policy", () => {
    const envelope = { ...profile.spec.defaultEnvelope,
      budget: { scope: "GovernedInference", tokens: 50000, usdMicros: 250000 },
      toolPolicyRef: { name: "restricted" } };
    const copy = (patch = {}, policies = [{ name: "restricted" }], extra = {}) =>
      profileModule.readTeamProfile({ ...profile, ...extra, spec: { defaultEnvelope: { ...envelope, ...patch } } },
        [{ name: "review" }], policies);
    assert.deepEqual(plain(copy().envelope), {
      ...profile.spec.defaultEnvelope,
      budget: { scope: "GovernedInference", tokens: 50000, usd_micros: 250000 },
      toolPolicy: "restricted",
    });
    assert.match(copy({}, []).error, /toolPolicyRef/);
    assert.match(copy({}, undefined, { tool_policy: "different" }).error, /conflicts/);
    for (const budget of [
      { tokens: 10 }, { scope: "Planning", tokens: 10 }, { scope: "GovernedInference", tokens: 0 },
      { scope: "GovernedInference", tokens: "50" },
      { scope: "GovernedInference", tokens: 10, usdMicros: 0 },
      { scope: "GovernedInference", tokens: 10, usdMicros: "100" },
      { scope: "GovernedInference", tokens: 10, futureCap: 5 },
    ]) assert.match(copy({ budget }).error, /budget/);
    assert.match(copy({ toolPolicyRef: { name: "restricted", namespace: "other" } }).error, /toolPolicyRef/);
  });

  await t.test("Team action preserves optional monetary caps and explicit authority policy pinning", async () => {
    const calls = [];
    const { createTeamAction } = actions({ createTeam: async (_, request) => calls.push(request) });
    await redirected(createTeamAction({}, form({
      budget_usd_micros: "250000", tool_policy: "restricted", pin_tool_policy: "on",
    })));
    assert.equal(calls[0].budget.usd_micros, 250000);
    assert.equal(calls[0].tool_policy, "restricted");
    assert.equal(calls[0].pin_tool_policy, true);
    for (const value of ["0", "-1", "1.1", " ", "NaN", "9007199254740992"]) {
      const result = await createTeamAction({}, form({ budget_usd_micros: value }));
      assert.match(result.error, /cost limit/);
    }
    assert.equal(calls.length, 1);
  });
  for (const phase of [null, "Pending", "Degraded"]) {
    assert.match(profileModule.readTeamProfile({ ...profile, phase }, []).error, /status/);
  }
  for (const [key, value] of [
    ["toolPolicyRef", { name: "restricted" }], ["budget", { tokens: 50 }],
    ["egressAllowlistRef", { name: "restricted" }], ["futureConstraint", true],
  ]) {
    const result = profileModule.readTeamProfile({
      ...profile, spec: { defaultEnvelope: { ...profile.spec.defaultEnvelope, [key]: value } },
    }, [{ name: "review" }]);
    assert.equal(result.envelope, null);
    assert.ok(result.error.includes(key));
  }
  assert.match(profileModule.readTeamProfile(profile, []).error, /skills are unavailable/);
});

test("Team setup does not silently replace missing, cross-namespace, unreadable or degraded profiles", async () => {
  const TeamComposer = () => null;
  const options = { models: [], runtimes: [], tool_policies: [], mcp_servers: [], memories: [],
    skills: [{ name: "review", namespace: "kars-system" }] };
  for (const profiles of [[], [{ ...profile, namespace: "other" }], [{ ...profile, phase: "Degraded" }], null, [profile]]) {
    const { default: Page } = load("../src/app/workspace/teams/new/page.tsx", {
      "react/jsx-runtime": jsxRuntime,
      "@/lib/bff": { getOptions: async () => options, listProfiles: async () => {
        if (profiles === null) throw new Error("inventory unavailable");
        return profiles;
      } },
      "@/lib/config": { defaultNamespace: () => "kars-system" },
      "@/lib/session": { currentPrincipal: async () => ({ name: "Reader", roles: ["viewer"] }) },
      "@/components/honest-state": { HonestState: () => null },
      "@/lib/team-profile": profileModule, "./team-composer": { TeamComposer },
    });
    const rendered = nodes(await Page({ searchParams: Promise.resolve({ profile: "bounded", intent: "Must not auto-compose instead" }) }));
    const composer = rendered.find(node => node.type === TeamComposer);
    if (profiles?.[0] === profile) {
      assert.deepEqual(plain(composer.props.profileEnvelope), profile.spec.defaultEnvelope);
      assert.equal(composer.props.canQualify, false, "an ordinary profile reader cannot designate qualification");
    } else {
      assert.equal(composer, undefined);
      assert.ok(rendered.some(node => node.props.title === "This profile cannot be used yet"));
    }
  }
});

test("the real profile composer submits its ceiling and depth instead of inventing broader limits", () => {
  const runner = hooks();
  const noop = () => null;
  const { TeamComposer } = load("../src/app/workspace/teams/new/team-composer.tsx", {
    react: runner.react, "react/jsx-runtime": jsxRuntime,
    "./actions": { createTeamAction: noop, composeTeamAction: () => assert.fail("profile must not invoke AI composition") },
    "@/components/repo-access": { RepoAccess: noop }, "@/components/orchestration-cube": { OrchestrationCube: noop },
    "@/components/validation-checks": { CompositionNote: noop }, "@/components/icon": { Icon: noop }, "@/components/preflight-check": { PreflightCheck: noop },
    "@/components/loop-designer": { LoopDesigner: noop }, "@/lib/team-budget": budgetModule,
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "./team-composer-panels": { renderGovernancePanel: noop, renderOrgPanel: noop },
  });
  const rendered = nodes(runner.render(TeamComposer, {
    profile, profileEnvelope: profile.spec.defaultEnvelope,
    options: { models: [], runtimes: [], skills: [{ name: "review" }], mcp_servers: [], memories: [], tool_policies: [] },
  }));
  assert.equal(rendered.find(node => node.props.name === "authority_ceiling").props.value, "2");
  assert.equal(rendered.find(node => node.props.name === "delegation_depth").props.value, "1");
  assert.equal(rendered.find(node => node.props.name === "launch").props.checked, false);
  assert.ok(JSON.stringify(rendered).includes("Based on the selected profile"));
  assert.ok(JSON.stringify(rendered).includes("a template is not launch approval"));
});

test("the real profile composer preserves money and policy bounds through preflight and edits", () => {
  const runner = hooks();
  const noop = () => null;
  const PreflightCheck = () => null;
  const { TeamComposer } = load("../src/app/workspace/teams/new/team-composer.tsx", {
    react: runner.react, "react/jsx-runtime": jsxRuntime,
    "./actions": { createTeamAction: noop, composeTeamAction: () => assert.fail("no model call") },
    "@/components/repo-access": { RepoAccess: noop }, "@/components/orchestration-cube": { OrchestrationCube: noop },
    "@/components/validation-checks": { CompositionNote: noop }, "@/components/icon": { Icon: noop }, "@/components/preflight-check": { PreflightCheck },
    "@/components/loop-designer": { LoopDesigner: noop }, "@/lib/team-budget": budgetModule,
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "./team-composer-panels": { renderGovernancePanel: noop, renderOrgPanel: noop },
  });
  const props = { profile, profileEnvelope: { ...profile.spec.defaultEnvelope,
    budget: { scope: "GovernedInference", tokens: 50000, usd_micros: 250000 }, toolPolicy: "restricted" },
    options: { models: [], runtimes: [], skills: [{ name: "review" }], mcp_servers: [], memories: [], tool_policies: [{ name: "restricted" }] } };
  const render = () => nodes(runner.render(TeamComposer, props));
  const field = name => render().find(node => node.props.name === name);
  const preflight = () => render().find(node => node.type === PreflightCheck);
  assert.equal(field("budget_tokens").props.value, "50000");
  assert.equal(field("budget_usd_micros").props.value, "250000");
  assert.equal(field("pin_tool_policy").props.checked, true);
  assert.equal(field("tool_policy").props.value, "restricted");
  assert.equal(preflight().props.budgetUsdMicros, 250000);
  assert.equal(preflight().props.blueprint.tool_policy, "restricted");
  const previousKey = preflight().key;
  field("budget_usd_micros").props.onChange({ target: { value: "240000" } });
  assert.notEqual(preflight().key, previousKey);
  assert.equal(preflight().props.budgetUsdMicros, 240000);
  field("budget_usd_micros").props.onChange({ target: { value: "0" } });
  assert.equal(preflight().props.disabled, true);
  assert.equal(field("launch").props.checked, false);
});

test("profile plans can be authored, cleared and repaired without losing the editor or reusing stale validation", () => {
  const runner = hooks();
  const noop = () => null;
  const PreflightCheck = () => null;
  const { TeamComposer } = load("../src/app/workspace/teams/new/team-composer.tsx", {
    react: runner.react, "react/jsx-runtime": jsxRuntime,
    "./actions": { createTeamAction: noop, composeTeamAction: () => assert.fail("manual profile editing must not call a model") },
    "@/components/repo-access": { RepoAccess: noop }, "@/components/orchestration-cube": { OrchestrationCube: noop },
    "@/components/validation-checks": { CompositionNote: noop }, "@/components/icon": { Icon: noop }, "@/components/preflight-check": { PreflightCheck },
    "@/components/loop-designer": { LoopDesigner: noop }, "@/lib/team-budget": budgetModule,
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "./team-composer-panels": { renderGovernancePanel: noop, renderOrgPanel: props => jsx("org-panel", props) },
  });
  const props = { profile, profileEnvelope: profile.spec.defaultEnvelope,
    options: { models: [], runtimes: [], skills: [{ name: "review" }], mcp_servers: [], memories: [], tool_policies: [] } };
  const render = () => nodes(runner.render(TeamComposer, props));
  const field = name => render().find(node => node.props.name === name);
  const editor = () => render().find(node => node.type === "textarea" && node.props["aria-label"] === "Typed execution plan JSON");
  const preflight = () => render().find(node => node.type === PreflightCheck);
  const submit = () => render().find(node => node.type === "button" && node.props.type === "submit");
  assert.ok(editor(), "empty profile must still expose the existing manual plan editor");
  field("name").props.onChange({ target: { value: "bounded-review" } });
  field("budget_tokens").props.onChange({ target: { value: "2000000" } });
  field("launch").props.onChange({ target: { checked: true } });
  assert.equal(preflight().props.disabled, true);
  const plan = { roles: [{ name: "reviewer", objective: "Review the supplied documents",
    depends_on: [], phases: [{ name: "review", capabilities: [], max_tool_calls: 0 }] }],
    max_parallel: 1, synthesis: { mode: "principal", instructions: "Summarize the review" } };
  editor().props.onChange({ target: { value: JSON.stringify(plan) } });
  assert.deepEqual(JSON.parse(field("execution_plan_json").props.value), plan);
  assert.equal(preflight().props.disabled, false);
  preflight().props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, false);
  const oldValidation = preflight();
  for (const invalid of ["", "{", "null", "[]", "42", '"text"']) {
    editor().props.onChange({ target: { value: invalid } });
    assert.ok(editor());
    assert.equal(editor().props.value, invalid);
    assert.equal(field("execution_plan_json").props.value, "null");
    assert.equal(preflight().props.disabled, true);
    oldValidation.props.onResult({ ok: true, checks: [] });
    assert.equal(submit().props.disabled, true);
  }
  editor().props.onChange({ target: { value: JSON.stringify({ ...plan, max_parallel: 2 }) } });
  assert.equal(preflight().props.disabled, false);
  assert.equal(submit().props.disabled, true, "changed plan needs fresh launch preflight");
  assert.equal(field("authority_ceiling").props.value, "2");
  assert.equal(field("delegation_depth").props.value, "1");
  preflight().props.onResult({ ok: true, checks: [] });
  assert.equal(submit().props.disabled, false);
  const org = render().find(node => node.type === "org-panel");
  assert.equal(org.props.authorityCeiling, "2");
  assert.equal(org.props.delegationDepth, "1");
  org.props.patchRole(org.props.roles[0].id, { runtime: "Hermes" });
  assert.equal(submit().props.disabled, true, "changing a member harness requires fresh preflight");
  const changeBudget = load("../src/lib/budget-review.ts").changeRoleBudget;
  const roleFields = () => render().find(node => node.type === "RoleBudgetFields");
  const beforeRoleEdit = preflight().key;
  roleFields().props.onChange(changeBudget(roleFields().props.draft, 0, "12000"));
  assert.equal(JSON.parse(editor().props.value).roles[0].budget_tokens, 12000);
  assert.equal(preflight().props.blueprint.execution_plan.roles[0].budget_tokens, 12000);
  assert.equal(preflight().props.budgetTokens, 2000000);
  assert.notEqual(preflight().key, beforeRoleEdit);
  assert.equal(submit().props.disabled, true);
  roleFields().props.onChange(changeBudget(roleFields().props.draft, 0, "-1"));
  assert.equal(preflight().props.disabled, true);
  assert.equal(submit().props.disabled, true);
  roleFields().props.onChange(changeBudget(roleFields().props.draft, 0, ""));
  assert.equal(preflight().props.blueprint.execution_plan.roles[0].budget_tokens, null);
  assert.equal(preflight().props.budgetTokens, 2000000);
  assert.equal(preflight().props.disabled, false);
});

function naturalProfileComposer(proposal, initialProfile) {
  const runner = hooks();
  const noop = () => null;
  const PreflightCheck = noop;
  const calls = [];
  let response = { available: true, proposal, rationale: "Use the installed review profile. Launch blocked: MCP qualification missing." };
  const { TeamComposer } = load("../src/app/workspace/teams/new/team-composer.tsx", {
    react: runner.react, "react/jsx-runtime": jsxRuntime,
    "./actions": { createTeamAction: noop, composeTeamAction: async (...args) => { calls.push(plain(args)); return response; } },
    "@/components/repo-access": { RepoAccess: noop }, "@/components/orchestration-cube": { OrchestrationCube: noop },
    "@/components/validation-checks": { CompositionNote: noop }, "@/components/icon": { Icon: noop }, "@/components/preflight-check": { PreflightCheck },
    "@/components/loop-designer": { LoopDesigner: noop }, "@/lib/team-budget": budgetModule,
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "./team-composer-panels": { renderGovernancePanel: noop, renderOrgPanel: noop },
  });
  const props = { initialCharter: "Assess public software continuously using the installed browser MCP", options: {
    models: [], runtimes: [], skills: [{ name: "review" }], tool_policies: [{ name: "restricted" }],
    mcp_servers: [{ name: "browser" }], memories: [],
  } };
  if (initialProfile) {
    props.profile = initialProfile;
    props.profileEnvelope = profileModule.readTeamProfile(initialProfile, props.options.skills, props.options.tool_policies).envelope;
  }
  const render = () => nodes(runner.render(TeamComposer, props));
  const button = text => render().find(node => node.type === "button" && JSON.stringify(node.props.children)?.includes(text));
  return { calls, props, render, button,
    field: name => render().find(node => node.type === "input" && node.props.name === name),
    preflight: () => render().find(node => node.type === PreflightCheck && node.props.blueprint),
    setResponse: value => { response = value; },
  };
}
function naturalProfileProposal() {
  return {
    profile: { ...structuredClone(profile), domain: "software", template_digest: "reviewed-template-v1", knowledge_commons: "reviews",
      tool_policy: "restricted", spec: { defaultEnvelope: {
        ...profile.spec.defaultEnvelope, delegationDepth: 0,
        budget: { scope: "GovernedInference", tokens: 2500000, usdMicros: 500000 },
        toolPolicyRef: { name: "restricted" },
      } } },
    tier: 3, budget_tokens: 2000000, cadence_minutes: 0, engineering_enabled: false,
    mcp_servers: ["browser"], egress: [{ host: "example.com", port: 443 }], egress_mode: "strict",
    roles: [{ name: "reviewer", system_prompt: "Inspect independent evidence", skills: ["review"], runtime: "OpenClaw", model: "" }],
    execution_plan: { schema: "kars.execution-plan/v1", roles: [{ name: "reviewer", phases: [{
      name: "research", execution_mode: "native", capabilities: ["network"], required_tool_calls: [],
    }] }] },
  };
}

test("Team preflight preserves explicit member routes without borrowing a member model for the principal", async () => {
  const proposal = naturalProfileProposal();
  proposal.roles[0].runtime = "Hermes";
  proposal.roles[0].model = "foundry::gpt-5.4-mini";
  const h = naturalProfileComposer(proposal);
  await h.button("Compose the org").props.onClick();
  assert.deepEqual(plain(h.preflight().props.teamRoles), proposal.roles.map(role => ({
    ...role, resources: { mcp_servers: [], egress: [] },
  })));
  assert.equal(h.preflight().props.blueprint.model, null, "principal inherits the controller default, not a member override");
  assert.equal(h.preflight().props.blueprint.runtime, undefined, "unset principal runtime must not inherit the Hermes member override");
});

test("authenticated preflight serializes the reviewed Team roster but leaves Mission payloads unchanged", async () => {
  const calls = [];
  const { validatePackage } = load("../src/lib/bff.ts", {
    "./config": { bffBaseUrl: () => "https://bff.example" },
    "next/headers": { cookies: async () => ({ get: () => ({ value: "session" }) }) },
    "./session-token": { verifySession: () => ({ sub: "reviewer", namespace: "kars-system" }), SESSION_COOKIE: "session" },
    "./oidc-config": { ssoConfigured: () => true }, "./credential-review": {},
  }, { fetch: async (url, init) => { calls.push(JSON.parse(init.body)); return { ok: true, json: async () => ({ ok: true, checks: [] }) }; } });
  const team_roles = [{ name: "reviewer", runtime: "Hermes", model: "foundry::gpt-5.4-mini" }];
  await validatePackage("kars-system", {}, { workload: "team", team_roles });
  await validatePackage("kars-system", {}, { workload: "mission" });
  assert.deepEqual(calls[0].team_roles, team_roles);
  assert.equal("team_roles" in calls[1], false);
});

test("natural intent proposes a visible catalogue profile and preserves its reviewed native package", async () => {
  const proposal = naturalProfileProposal();
  const h = naturalProfileComposer(proposal);
  await h.button("Compose the org").props.onClick();
  assert.deepEqual(h.calls, [[h.props.initialCharter, null, null, { budget_tokens: null, role_tokens: {} }]]);
  assert.equal(h.field("charter").props.value, h.props.initialCharter, "template must not replace the user's task");
  for (const [name, value] of Object.entries({ tier: 3, authority_ceiling: "2", delegation_depth: "0",
    budget_tokens: "2000000", budget_usd_micros: "500000", tool_policy: "restricted", knowledge_commons: "reviews" })) {
    assert.equal(h.field(name).props.value, value, name);
  }
  assert.equal(h.field("pin_tool_policy").props.checked, true);
  assert.equal(h.field("launch").props.checked, false);
  assert.equal(h.preflight().props.blueprint.mcp_servers[0], "browser");
  assert.deepEqual(plain(h.preflight().props.blueprint.skills), ["review"]);
  assert.deepEqual(plain(h.preflight().props.blueprint.execution_plan), proposal.execution_plan);
  assert.deepEqual(plain(h.preflight().props.blueprint.egress), proposal.egress);
  assert.match(JSON.stringify(h.render()), /Team profile proposed for review/);
  assert.match(JSON.stringify(h.render()), /reviewed-template-v1/);
  assert.match(JSON.stringify(h.render()), /Launch blocked: MCP qualification missing/);
  assert.match(JSON.stringify(h.render()), /not profile authority or approval/);
  h.field("name").props.onChange({ target: { value: "review-team" } });
  h.field("launch").props.onChange({ target: { checked: true } });
  h.preflight().props.onResult({ ok: false, checks: [] });
  assert.equal(h.render().find(node => node.props.type === "submit").props.disabled, true);

  h.field("budget_tokens").props.onChange({ target: { value: "1500000" } });
  h.field("budget_usd_micros").props.onChange({ target: { value: "240000" } });
  h.field("authority_ceiling").props.onChange({ target: { value: "1" } });
  const reviewedPlan = structuredClone(proposal.execution_plan);
  reviewedPlan.roles[0].budget_tokens = 900000;
  h.render().find(node => node.props["aria-label"] === "Typed execution plan JSON").props.onChange({ target: { value: JSON.stringify(reviewedPlan) } });
  h.setResponse({ available: true, proposal: { ...proposal, budget_tokens: 1500000, execution_plan: reviewedPlan } });
  h.button("Back to charter").props.onClick();
  await h.button("Compose the org").props.onClick();
  assert.deepEqual(h.calls[1], [h.props.initialCharter, "bounded", null, { budget_tokens: 1500000, role_tokens: { reviewer: 900000 } }]);
  assert.equal(h.field("budget_tokens").props.value, "1500000", "recomposition must not widen an edited cap");
  assert.equal(h.field("budget_usd_micros").props.value, "240000", "recomposition must preserve a tighter reviewed currency cap");
  assert.equal(h.preflight().props.budgetUsdMicros, 240000);
  assert.equal(h.preflight().props.blueprint.execution_plan.roles[0].budget_tokens, 900000);
  assert.equal(h.field("authority_ceiling").props.value, "1");
  assert.equal(h.field("launch").props.checked, false);
});

test("a profile draft prepares a reviewed plan directly without JSON, navigation or launch", async () => {
  const proposal = naturalProfileProposal();
  const h = naturalProfileComposer(proposal, proposal.profile);
  assert.equal(h.calls.length, 0, "opening a template must not consume AI usage");
  assert.equal(h.preflight().props.disabled, true);
  assert.ok(h.button("Prepare work plan"));
  assert.match(JSON.stringify(h.render()), /You do not need to write technical JSON/);
  assert.match(JSON.stringify(h.render()), /Preparation uses AI and records usage/);
  h.field("name").props.onChange({ target: { value: "profile-review" } });
  await h.button("Prepare work plan").props.onClick();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0][1], proposal.profile.name);
  assert.deepEqual(JSON.parse(h.field("execution_plan_json").props.value), proposal.execution_plan);
  assert.equal(h.field("name").props.value, "profile-review");
  assert.equal(h.field("authority_ceiling").props.value, "2");
  assert.equal(h.field("delegation_depth").props.value, "0");
  assert.equal(h.field("launch").props.checked, false);
  assert.equal(h.preflight().props.disabled, false);
  assert.equal(h.button("Prepare work plan"), undefined);
});

test("failed preparation preserves the profile draft and exposes a direct retry without JSON", async () => {
  const proposal = naturalProfileProposal();
  const h = naturalProfileComposer(proposal, proposal.profile);
  h.setResponse({ available: false, reason: "Planning service is unavailable" });
  await h.button("Prepare work plan").props.onClick();
  assert.equal(h.field("execution_plan_json").props.value, "null");
  assert.equal(h.field("authority_ceiling").props.value, "2");
  assert.equal(h.field("delegation_depth").props.value, "0");
  assert.equal(h.preflight().props.disabled, true);
  assert.equal(h.button("Prepare work plan").props.disabled, false);
  assert.match(JSON.stringify(h.render()), /Planning service is unavailable/);
  h.setResponse({ available: true, proposal });
  await h.button("Prepare work plan").props.onClick();
  assert.equal(h.calls.length, 2);
  assert.equal(h.field("launch").props.checked, false);
  assert.deepEqual(JSON.parse(h.field("execution_plan_json").props.value), proposal.execution_plan);
});

test("an explicitly copied profile without funding can receive a finite proposal without losing authority limits", async () => {
  const proposal = naturalProfileProposal();
  delete proposal.profile.spec.defaultEnvelope.budget;
  const h = naturalProfileComposer(proposal, proposal.profile);
  assert.equal(h.field("budget_tokens").props.value, "");
  h.button("Back to charter").props.onClick();
  await h.button("Compose the org").props.onClick();
  assert.equal(h.calls[0][1], "bounded");
  assert.equal(h.field("budget_tokens").props.value, "2000000");
  assert.equal(h.field("authority_ceiling").props.value, "2");
  assert.equal(h.field("delegation_depth").props.value, "0");
  assert.equal(h.field("launch").props.checked, false);
});

test("invalid profile proposals leave the existing draft intact instead of dropping skills or bounds", async () => {
  for (const patch of [
    p => { p.profile.phase = "Pending"; },
    p => { p.profile.spec.defaultEnvelope.futurePermission = true; },
    p => { p.profile.roles[0] = { ...p.profile.roles[0], skills: ["missing"] }; },
    p => { p.roles[0].skills = []; },
    p => { p.roles[0].skills = ["review", "missing"]; },
    p => { p.roles = []; },
    p => { p.tier = 5; },
    p => { p.budget_tokens = 2500001; },
    p => { p.profile.tool_policy = "missing"; },
  ]) {
    const proposal = naturalProfileProposal();
    patch(proposal);
    const h = naturalProfileComposer(proposal);
    await h.button("Compose the org").props.onClick();
    assert.equal(h.field("budget_tokens").props.value, "", "invalid copy must be atomic");
    assert.equal(h.field("authority_ceiling").props.value, "");
    assert.equal(h.field("roles_json").props.value, "[]");
    assert.equal(h.field("launch").props.checked, false);
    assert.doesNotMatch(JSON.stringify(h.render()), /Team profile proposed for review/);
  }
  const h = naturalProfileComposer(naturalProfileProposal());
  await h.button("Compose the org").props.onClick();
  const originalRoles = h.field("roles_json").props.value;
  for (const response of [
    { available: false, reason: "Profile is no longer Ready" },
    { available: true, proposal: { ...naturalProfileProposal(), profile: null } },
  ]) {
    h.setResponse(response);
    h.button("Back to charter").props.onClick();
    await h.button("Compose the org").props.onClick();
    assert.equal(h.field("roles_json").props.value, originalRoles);
    assert.equal(h.field("budget_tokens").props.value, "2000000");
    assert.equal(h.field("launch").props.checked, false);
  }
});

test("failed non-profile recomposition keeps the edited plan, total and role limits", async () => {
  const proposal = naturalProfileProposal();
  delete proposal.profile;
  proposal.budget_tokens = 9000000;
  proposal.execution_plan.roles[0].budget_tokens = 2000000;
  const h = naturalProfileComposer(proposal);
  await h.button("Compose the org").props.onClick();
  const originalPlan = h.render().find(node => node.props["aria-label"] === "Typed execution plan JSON").props.value;
  const originalRoles = h.field("roles_json").props.value;
  h.setResponse({ available: false, reason: "Spending settings could not be confirmed" });
  h.button("Back to charter").props.onClick();
  await h.button("Compose the org").props.onClick();
  assert.deepEqual(h.calls[1][3], { budget_tokens: 9000000, role_tokens: { reviewer: 2000000 } });
  assert.equal(h.field("budget_tokens").props.value, "9000000");
  assert.equal(h.field("roles_json").props.value, originalRoles);
  const editor = h.render().find(node => node.props["aria-label"] === "Typed execution plan JSON");
  assert.equal(editor.props.value, originalPlan);
  editor.props.onChange({ target: { value: "{" } });
  h.button("Back to charter").props.onClick();
  await h.button("Compose the org").props.onClick();
  assert.equal(h.calls.length, 2, "invalid edited JSON must not be discarded by another model request");
});

test("Team composition forwards reviewed spending and profile name, never client profile authority", async () => {
  const spending = { budget_tokens: 9000000, role_tokens: { reviewer: 2000000 } };
  const calls = [];
  await actions({ composeTeam: async (...args) => { calls.push(plain(args)); return { available: true }; } })
    .composeTeamAction("Review public software", "bounded", undefined, spending);
  assert.deepEqual(calls, [["kars-system", "Review public software", "bounded", null, spending]]);
  const wire = [];
  const { composeTeam } = load("../src/lib/bff.ts", {
    "./config": { bffBaseUrl: () => "https://bff.example" },
    "next/headers": { cookies: async () => ({ get: () => ({ value: "session" }) }) },
    "./session-token": { verifySession: () => ({ sub: "reviewer", namespace: "kars-system" }), SESSION_COOKIE: "session" },
    "./oidc-config": { ssoConfigured: () => true }, "./credential-review": {},
  }, { AbortSignal, fetch: async (_, init) => { wire.push(JSON.parse(init.body)); return { ok: true, status: 200, json: async () => ({}) }; } });
  const inherited = { budget_tokens: null, role_tokens: {} };
  await composeTeam("kars-system", "Natural intent", undefined, undefined, inherited);
  await composeTeam("kars-system", "Explicit profile", "bounded", undefined, spending);
  assert.deepEqual(wire, [{ charter: "Natural intent", spending: inherited }, { charter: "Explicit profile", profile: "bounded", spending }]);
  assert.throws(() => composeTeam("kars-system", "Stale editor"), /Copy your draft somewhere safe/);
  assert.equal(wire.length, 2, "missing reviewed settings must never become an uncapped request");
});

test("org chart mirrors controller member defaults and never advertises delegation at depth zero", () => {
  const { renderOrgPanel } = load("../src/app/workspace/teams/new/team-composer-panels.tsx", {
    "react/jsx-runtime": jsxRuntime, "@/components/icon": { Icon: () => null },
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
  });

  const render = overrides => JSON.stringify(renderOrgPanel({
    name: "bounded-review", tier: 3, authorityCeiling: "", delegationDepth: "", roles: [],
    options: { models: [], runtimes: [], skills: [] }, ...overrides,
  }));
  assert.match(render({}), /default member Tier.*2/);
  assert.match(render({ authorityCeiling: "1" }), /default member Tier.*1/);
  assert.match(render({ authorityCeiling: "1", delegationDepth: "0" }), /delegation disabled/);
  assert.doesNotMatch(render({ authorityCeiling: "1", delegationDepth: "0" }), /grants members/);
  for (const overrides of [{ authorityCeiling: "4" }, { authorityCeiling: "NaN" },
    { authorityCeiling: "1.5" }, { delegationDepth: "-1" }]) {
    assert.match(render(overrides), /invalid authority limits/);
  }
});

test("create and edit member model pickers distinguish providers serving the same model", () => {
    const options = {
      models: [
        { provider: "azure-foundry", deployment: "shared-model" },
        { provider: "github-copilot", deployment: "shared-model" },
      ],
      runtimes: [], skills: [],
    };
    const role = { id: 1, name: "reviewer", system_prompt: "", runtime: "", model: "azure-foundry::shared-model", skills: [] };
    const { renderOrgPanel } = load("../src/app/workspace/teams/new/team-composer-panels.tsx", {
      "react/jsx-runtime": jsxRuntime, "@/components/icon": { Icon: () => null },
      "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    });
    const created = renderOrgPanel({
      name: "review-team", tier: 3, authorityCeiling: "", delegationDepth: "",
      roles: [role], options,
    });
    const runner = hooks();
    const { TeamRosterEdit } = load("../src/app/workspace/teams/[name]/team-roster-edit.tsx", {
      react: runner.react, "react/jsx-runtime": jsxRuntime,
      "next/navigation": { useRouter: () => ({ refresh() {} }) },
      "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
      "@/components/viewport-portal": { ViewportPortal: "portal" },
    });
    const props = { ns: "core", name: "review-team", roster: [role], options };
    runner.render(TeamRosterEdit, props).props.onClick();
    const edited = runner.render(TeamRosterEdit, props);
    for (const tree of [created, edited]) {
      const choices = nodes(tree).filter(node => node.type === "option" && node.props.value?.includes("::"));
      assert.deepEqual(choices.map(node => [node.props.value, [].concat(node.props.children).join("")]), [
        ["azure-foundry::shared-model", "shared-model · azure-foundry"],
        ["github-copilot::shared-model", "shared-model · github-copilot"],
      ]);
    }
    const picker = nodes(edited).find(node => node.props["aria-label"] === "reviewer model");
    picker.props.onChange({ target: { value: "github-copilot::shared-model" } });
    assert.equal(nodes(runner.render(TeamRosterEdit, props))
      .find(node => node.props["aria-label"] === "reviewer model").props.value, "github-copilot::shared-model");
});
