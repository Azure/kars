// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const sourceRoot = fileURLToPath(new URL("../src/", import.meta.url));
const cache = new Map();
const traceCalls = [];
const forbiddenMutation = () => assert.fail("Read-only rendering must not mutate a mission");

// Render real components with React; only browser streaming and server actions
// are replaced. The signed-in browser test separately verifies real hydration.
function load(file, reactOverrides = null, modules = null) {
  const path = resolve(sourceRoot, file);
  if (!reactOverrides && !modules && cache.has(path)) return cache.get(path);
  const exports = {};
  if (!reactOverrides && !modules) cache.set(path, exports);
  const localRequire = (name) => {
    if (modules && Object.hasOwn(modules, name)) return modules[name];
    if (name === "react" && reactOverrides) return { ...React, ...reactOverrides };
    if (name.endsWith("/use-live-trace")) return {
      useLiveTrace: (...args) => {
        traceCalls.push(args);
        return { events: args[3], routerActivity: null };
      },
    };
    if (name === "@/components/live-refresh") return {
      LivePulse: ({ label }) => React.createElement("span", null, label),
      LiveRefresh: ({ active }) => React.createElement("span", { "data-refresh": String(active) }),
    };
    if (name === "next/navigation") return { useRouter: () => ({ refresh: forbiddenMutation }) };
    if (name === "./review-actions") return { submitReview: forbiddenMutation };
    if (name === "./task-actions") return {
      addTeamTask: forbiddenMutation, deleteTeamTask: forbiddenMutation, reviewTeamTask: forbiddenMutation,
    };
    if (name === "@/app/tasks/[name]/launch-actions") return { setLaunch: forbiddenMutation };
    if (!name.startsWith(".") && !name.startsWith("@/")) return require(name);
    const base = name.startsWith("@/") ? resolve(sourceRoot, name.slice(2)) : resolve(dirname(path), name);
    const dependency = [`${base}.ts`, `${base}.tsx`, `${base}/index.ts`, `${base}/index.tsx`].find(existsSync);
    assert.ok(dependency, `Missing source dependency: ${name}`);
    return load(dependency);
  };
  runInNewContext(ts.transpileModule(readFileSync(path, "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText, { exports, require: localRequire, URLSearchParams, Date });
  return exports;
}

const { formatEvidenceTime } = load("lib/format.ts");
const { ExecutionExplorer } = load("components/execution-explorer.tsx");
const { ExecutionLifetime } = load("components/execution-lifetime.tsx");
const { ReviewPanel } = load("app/workspace/missions/[name]/review-panel.tsx");
const { TeamTasks } = load("app/workspace/teams/[name]/team-tasks.tsx");
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));
const { DeployTimeline } = load("app/workspace/missions/[name]/deploy-timeline.tsx");
const { ReadinessPanel } = load("app/workspace/missions/[name]/readiness-panel.tsx");
const { MissionMap } = load("app/workspace/missions/[name]/mission-map.tsx");
const { MissionsList } = load("app/workspace/missions/missions-list.tsx");

test("model selection distinguishes saved settings from the observed default", () => {
  const setDefault = () => {};
  const { ModelCatalogue } = load("app/console/configuration/model-catalogue.tsx", {
    useActionState: (action) => [
      action === setDefault ? { error: null, ok: "Default selection saved; refresh to check." } : { error: null, ok: null },
      forbiddenMutation,
      false,
    ],
  }, {
    "./set-default-model-actions": { setDefaultModelAction: setDefault },
    "./local-inference-actions": { undeployLocalModelAction: forbiddenMutation },
    "./local-model-deploy": { LocalModelDeploy: () => null },
  });
  const props = {
    models: [{ deployment: "gpt-4.1", provider: "azure-foundry", is_default: false }],
    localStatus: null, localCatalog: [], localDeployments: [],
  };
  const pending = render(ModelCatalogue, props);
  assert.match(pending, /role="status"/);
  assert.match(pending, /Default selection saved/);
  assert.match(pending, /Refresh status/);
  assert.match(pending, /Set as default/);
  const applied = render(ModelCatalogue, {
    ...props, models: [{ ...props.models[0], is_default: true }],
  });
  assert.doesNotMatch(applied, /Refresh status|Default selection saved|Set as default/);
  assert.match(applied, />default</);
});

test("Mission creation times render identically across server and browser timezones", () => {
  const tasks = [{ name: "draft", objective: "Draft mission", tier: 3,
    phase: "Pending", execution_phase: "Idle", launched: false,
    created_at: "2026-10-09T08:11:03Z" }];
  const previous = process.env.TZ;
  try {
    const results = ["UTC", "Europe/Budapest", "America/Los_Angeles"].map((zone) => {
      process.env.TZ = zone;
      return render(MissionsList, { tasks });
    });
    assert.equal(results[0], results[1]);
    assert.equal(results[0], results[2]);
    assert.match(results[0], /Created: 2026-10-09 08:11:03 UTC/);
    assert.doesNotMatch(results[0], /Started:/);
  } finally {
    if (previous === undefined) delete process.env.TZ;
    else process.env.TZ = previous;
  }
});

test("access configuration never infers verified access from delivery or controller status", () => {
  const composition = { model: "model", isolation: "standard", tool_policy: "policy",
    mcp_servers: ["service"], egress: [], memory: null };
  for (const state of [
    { launched: false, executionPhase: "Idle", degraded: false },
    { launched: true, executionPhase: "Running", degraded: false },
    { launched: false, executionPhase: "Idle", degraded: false, delivered: true },
    { launched: true, executionPhase: "Running", degraded: false, failed: true },
    { launched: true, executionPhase: "Degraded", degraded: true },
  ]) {
    const html = render(ReadinessPanel, { composition, ...state });
    assert.match(html, /Access configuration/);
    assert.match(html, /not a connectivity test or proof of tool success/);
    assert.doesNotMatch(html, />Verified<|>Ready<|verified the moment|was verified; execution failed/);
  }
});

test("Mission map uses bound reported results without confusing them with a complete audit", () => {
  const task = { current_run_nonce: "current", assignment: null,
    result: { assignment_nonce: "current", status: "ok", source: "durable_agent", total_tokens: 9969,
      output: "Report", reviewable: true, run_evidence: { status: "succeeded", run_nonce: "current" } },
    router_activity: { state: "observed", trace: {
      events: [{ kind: "tool_proposed" }, { kind: "tool_result" }, { kind: "tool_result" }],
    } },
    telemetry: { rounds: 3, tool_calls: null }, activity: [],
    envelope: { budget: { tokens: 300000 }, tier: 3, authority_ceiling: 2, delegation_depth: 0 },
    display_name: "Mission", children: [], sub_agents: [] };
  const html = render(MissionMap, { task });
  assert.match(html, /Reported tool results<\/p><p[^>]*>2<\/p>/);
  assert.match(html, /not a complete execution audit/);
  assert.match(html, /9,969/);
  assert.match(html, /This revision: 3% of 300,000 lifetime token cap/);
  const stale = render(MissionMap, { task: { ...task, current_run_nonce: "new" } });
  assert.doesNotMatch(stale, /Reported tool results|9,969/);
  assert.match(stale, /Usage not recorded/);
});

test("global Deliverables links committed current revisions and searches actual filenames", async () => {
  const current = {
    task: "mission", evidence_key: "canonical-digest", current: true, parent: null,
    team: null, archived: false, display_name: "Current report", objective: "Write a report",
    model: "model", finished_at: "2026-10-09T08:11:50Z", status: "ok",
    review_status: "approved", review_revision: 0, summary: "Report", excerpt: "Report",
    pull_requests: [], deliverable_did: null,
    files: [
      { name: "kars-router-observations.json", size_bytes: 2800, has_content: true },
      { name: "launch-check.md", size_bytes: 186, has_content: true },
    ],
  };
  const historical = { ...current, evidence_key: "prior-digest", current: false,
    display_name: "Historical report", review_status: "none", files: [] };
  const internal = { ...current, task: "ordinary-child", parent: "mission",
    evidence_key: "child-digest", display_name: "Internal phase" };
  const { default: Page } = load("app/workspace/artifacts/page.tsx", null, {
    "@/lib/bff": {
      getArtifacts: async () => ({ missions: [current, historical, internal] }),
      BffError: class extends Error {},
    },
  });
  const html = renderToStaticMarkup(await Page({ searchParams: Promise.resolve({}) }));
  assert.match(html, /href="\/workspace\/missions\/mission"[^>]*>Current report</);
  assert.match(html, /Historical report/);
  assert.match(html, />Historical revision</);
  assert.equal((html.match(/>Approved</g) ?? []).length, 1);
  assert.match(html, /Open current mission/);
  assert.match(html, /launch-check.md/);
  assert.doesNotMatch(html, /Internal phase|kars-router-observations.json/);
  assert.match(html, /aria-label="Search deliverables"/);
  assert.match(html, /flex-col items-start justify-between gap-4 sm:flex-row/);
  assert.match(html, />Document</);
  const searched = renderToStaticMarkup(await Page({
    searchParams: Promise.resolve({ q: "launch-check.md" }),
  }));
  assert.match(searched, /Current report/);
  assert.doesNotMatch(searched, /Historical report|Internal phase/);
  const all = renderToStaticMarkup(await Page({
    searchParams: Promise.resolve({ view: "all" }),
  }));
  assert.match(all, /Internal phase/);
  assert.match(all, /kars-router-observations.json/);
  assert.match(all, />Document</);
  assert.doesNotMatch(all, />Config</);
});

test("deployment status does not invent readiness, access proof, or fresh activity", () => {
  const task = { launched: true, sandbox: "mission", execution_phase: "Running",
    current_run_nonce: "current", result: null, assignment: null,
    agent_identity: { last_seen: "2026-10-08T10:00:00Z" }, activity: [] };
  const html = render(DeployTimeline, { task });
  assert.match(html, /Starting execution/);
  assert.match(html, /does not establish pod readiness/);
  assert.match(html, /not a current reachability check/);
  assert.match(html, /individual access checks are not included/);
  assert.doesNotMatch(html, /every step below is verified|Inference router \+ access verified|>Live</);
  const oldActivity = { kind: "round", runNonce: "old", round: 1, ms: 3,
    ts: "2026-10-08T10:00:01Z", prompt_tokens: 1, completion_tokens: 1,
    total_tokens: 2, tool_calls: 0, finish_reason: "stop" };
  assert.match(render(DeployTimeline, { task: { ...task, activity: [oldActivity] } }), /Starting execution/);
  assert.match(render(DeployTimeline, {
    task: { ...task, activity: [{ ...oldActivity, runNonce: "current" }] },
  }), /Execution observed/);
});

test("terminal deployment presentation uses only the current revision outcome", () => {
  const task = { launched: true, sandbox: "mission", execution_phase: "Running",
    current_run_nonce: "current", assignment: null, activity: [],
    result: { assignment_nonce: "current", reviewable: true, status: "ok", output: "Report" } };
  assert.match(render(DeployTimeline, { task }), /<h2[^>]*>Delivered</);
  const failed = render(DeployTimeline, {
    task: { ...task, result: { ...task.result, status: "failed", reviewable: false } },
  });
  assert.match(failed, /<h2[^>]*>Run failed</);
  assert.doesNotMatch(failed, /Provisioning completed/);
});

test("connected access distinguishes governed MCP from direct external destinations", () => {
  const file = "app/workspace/missions/[name]/page.tsx";
  const page = ts.createSourceFile(file, readFileSync(resolve(sourceRoot, file), "utf8"),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let value;
  const visit = node => {
    if (ts.isJsxSelfClosingElement(node) && node.tagName.getText(page) === "EnvelopeFact") {
      const attrs = node.attributes.properties;
      if (attrs.some(a => a.name?.getText(page) === "label" && a.initializer?.text === "Connected access")) {
        value = attrs.find(a => a.name?.getText(page) === "value")?.initializer?.expression;
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(page);
  assert.ok(value);
  const exports = {};
  runInNewContext(ts.transpileModule(`export function access(task) { return ${value.getText(page)}; }`, {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText, { exports, egressScope: load("lib/format.ts").egressScope });
  assert.equal(exports.access({ composition: null }), "No extra direct egress");
  assert.equal(exports.access({ composition: { mcp_servers: ["server"], egress: [] } }),
    "1 governed MCP service · No extra direct egress");
  assert.equal(exports.access({ composition: { mcp_servers: ["one", "two"], egress: ["api.example.com:443"] } }),
    "2 governed MCP services · 1 direct external destination");
});

const workItem = {
  id: "checklist", title: "Checklist", description: "Prepare useful checks", status: "pending",
  depends_on: [], acceptance_criteria: ["No personal data"], review_required: true,
};

test("Skill approval requires inspected current content and keeps revoke usable after success", () => {
  let result = { error: null, ok: null };
  const { SkillApproval } = load("app/console/skill-approval.tsx", {
    useActionState: () => [result, forbiddenMutation, false],
  }, { "./governance-actions": { reviewSkillAction: forbiddenMutation } });
  const skill = { name: "checklist", uid: "skill-uid", generation: 2,
    validated: true, version_digest: "sha256:current", review: "pending", usable: false };
  let html = render(SkillApproval, { skill });
  assert.match(html, /Review package/);
  assert.match(html, /<button[^>]*disabled=""/);
  html = render(SkillApproval, { skill, inspected: true });
  assert.doesNotMatch(html, /<button[^>]*disabled=""/);
  assert.match(html, /name="uid" value="skill-uid"/);
  assert.match(html, /name="generation" value="2"/);
  assert.match(html, /name="version_digest" value="sha256:current"/);
  assert.match(render(SkillApproval, { skill: { ...skill, validated: false }, inspected: true }),
    /<button[^>]*disabled=""/);
  result = { error: null, ok: "Approved current content" };
  html = render(SkillApproval, { skill: { ...skill, usable: true, review: "approved" }, inspected: true });
  assert.match(html, />Revoke<\/button>/);
  assert.match(html, /role="status"/);
  assert.doesNotMatch(html, /refreshing/);
  result = { error: "Skill changed; inspect the current package", ok: null };
  assert.match(render(SkillApproval, { skill, inspected: true }), /role="alert"/);
  for (const locked_digest of [undefined, "sha256:current"]) {
    html = render(SkillApproval, { skill: { ...skill, review: "approved", locked_digest }, inspected: true });
    assert.match(html, /approval no longer current/);
    assert.doesNotMatch(html, /changed since approval/);
  }
});

test("Access page describes independently enforced signed roles and a scrollable permission matrix", async () => {
  const { default: Page } = load("app/console/access/page.tsx", null, {
    "@/lib/session": { currentPrincipal: async () => ({
      name: "beta", roles: ["operator"], primary: "operator", primaryLabel: "Operator", ssoSignedIn: true,
    }) },
    "@/lib/oidc-config": { ssoConfigured: () => true },
  });
  const html = renderToStaticMarkup(await Page());
  assert.match(html, /API independently/);
  assert.match(html, /separate cluster-permission ceiling/);
  assert.doesNotMatch(html, /SSO only decides|there is no SSO yet/);
  assert.match(html, /overflow-x-auto/);
  assert.match(html, /not on this page\. Sign in again/);
  assert.doesNotMatch(html, /Manage roles &amp; access|manage roles/);
  for (const label of ["Workspace user", "Auditor", "Operator", "Admin"]) {
    assert.ok(html.includes(`aria-label="${label}"`));
  }
  const rows = html.match(/<tr\b[^>]*>[\s\S]*?<\/tr>/g);
  for (const [capability, expected] of [
    ["Cluster &amp; provider configuration", ["not allowed", "not allowed", "allowed", "allowed"]],
    ["Manage inference budgets &amp; retention policy", ["not allowed", "not allowed", "not allowed", "allowed"]],
  ]) {
    const row = rows.find(row => row.includes(capability));
    assert.ok(row, capability);
    assert.deepEqual([...row.matchAll(/aria-label="(allowed|not allowed)"/g)].map(match => match[1]), expected);
  }
});

test("Skill review actions preserve the displayed identity and version, and surface conflicts", async () => {
  const calls = [];
  const refreshed = [];
  let fail = false;
  class BffError extends Error { }
  const actions = load("app/console/governance-actions.ts", null, {
    "next/cache": { revalidatePath: path => refreshed.push(path) },
    "@/lib/bff": {
      BffError,
      approveSkill: async (name, binding) => {
        calls.push({ name, binding });
        if (fail) throw new BffError("Displayed version changed");
        return { name, locked_digest: binding.version_digest };
      },
      revokeSkill: async (name, uid) => { calls.push({ name, uid }); return { name }; },
    },
  });
  const form = new FormData();
  for (const [key, value] of Object.entries({
    name: "checklist", uid: "skill-uid", generation: "2", version_digest: "sha256:current", action: "approve",
  })) form.set(key, value);
  assert.equal((await actions.reviewSkillAction({}, form)).error, null);
  assert.deepEqual(JSON.parse(JSON.stringify(calls.at(-1))), {
    name: "checklist", binding: { uid: "skill-uid", generation: 2, version_digest: "sha256:current" },
  });
  assert.ok(refreshed.includes("/console/capabilities/skills/checklist"));
  fail = true;
  assert.equal((await actions.reviewSkillAction({}, form)).error, "Displayed version changed");
  const count = calls.length;
  form.set("generation", "0");
  assert.match((await actions.reviewSkillAction({}, form)).error, /currently validated/);
  assert.equal(calls.length, count);
  form.set("action", "revoke");
  assert.equal((await actions.reviewSkillAction({}, form)).error, null);
  assert.deepEqual(JSON.parse(JSON.stringify(calls.at(-1))), { name: "checklist", uid: "skill-uid" });
});

test("Workspace Skills refreshes both pending approval and current approval without discarding draft state", async () => {
  let skill = { name: "checklist", validated: true, usable: false, phase: "Ready", review: "pending" };
  let refresh;
  const { default: Page } = load("app/workspace/skills/page.tsx", null, {
    "@/lib/bff": { listUserSkills: async () => [skill], getOptions: async () => ({ tool_policies: [] }) },
    "./skill-upload": { SkillUpload: () => null },
    "@/components/live-refresh": { LiveRefresh: props => { refresh = props; return null; } },
  });
  for (const usable of [false, true]) {
    skill = { ...skill, usable, review: usable ? "approved" : "pending" };
    renderToStaticMarkup(await Page());
    assert.equal(refresh.active, true);
    assert.equal(refresh.intervalMs, 10000);
  }
  skill = { ...skill, validated: false, usable: false, phase: "Pending" };
  renderToStaticMarkup(await Page());
  assert.equal(refresh.active, true);
  assert.equal(refresh.intervalMs, 3500);
});

test("Skill composer preserves every file and failed draft, rejects unnamed files and invents no policy", async () => {
  const files = [{ name: "SKILL.md", content: "Instructions" }, { name: "references/empty.txt", content: "" }];
  const values = [true, "Checklist", "1.0.0", "Review local evidence", "", "read-only", files, null, false];
  let cursor = 0;
  const tasks = [], submissions = [];
  const { SkillComposer } = load("components/skill-composer.tsx", {
    useState: () => {
      const index = cursor++;
      return [values[index], value => { values[index] = typeof value === "function" ? value(values[index]) : value; }];
    },
    useTransition: () => [false, callback => { tasks.push(callback()); }],
  });
  const props = { toolPolicies: [{ name: "read-only" }],
    submit: async input => { submissions.push(input); return { ok: false, error: "Keep the draft for correction" }; } };
  const find = (node, predicate) => {
    if (!React.isValidElement(node)) return null;
    if (predicate(node)) return node;
    for (const child of React.Children.toArray(node.props.children)) {
      const found = find(child, predicate);
      if (found) return found;
    }
    return null;
  };
  const button = tree => find(tree, node => node.type === "button" && node.props.children === "Submit for review");
  button(SkillComposer(props)).props.onClick();
  await tasks.pop();
  assert.deepEqual(JSON.parse(JSON.stringify(submissions[0].files)), files);
  assert.equal(values[7], "Keep the draft for correction");
  assert.equal(values[1], "Checklist");
  assert.equal(values[6], files);
  values[6] = [...files, { name: "", content: "Must not disappear" }];
  cursor = 0;
  button(SkillComposer(props)).props.onClick();
  assert.equal(submissions.length, 1);
  assert.match(values[7], /no files will be silently dropped/);
  cursor = 0;
  const noPolicies = SkillComposer({ ...props, toolPolicies: [] });
  assert.equal(button(noPolicies).props.disabled, true);
  assert.doesNotMatch(renderToStaticMarkup(noPolicies), /<option[^>]*>kars-default/);
  assert.match(renderToStaticMarkup(noPolicies), /No bounding policies available/);
});

test("Team pages distinguish read failures from absent records and never conceal failed approvals", async () => {
  class BffError extends Error {
    constructor(code, status, message = code) { super(message); this.code = code; this.status = status; }
  }
  const failures = {
    conflict: new BffError("conflict", 409, "work checkpoint binding changed; review is unavailable"),
    denied: new BffError("forbidden", 403),
    session: new BffError("unauthorized", 401),
    upstream: new BffError("upstream", 503),
    network: new Error("Connection interrupted: 404 was not an HTTP response"),
  };
  const errorUI = load("app/workspace/teams/read-error.tsx", null, { "@/lib/bff": { BffError } });
  const team = { paused: false, phase: "Active", current_assignment_nonce: "nonce",
    principal_task: "run", generated_tasks: [] };
  for (const file of ["app/workspace/teams/[name]/page.tsx",
    "app/workspace/teams/[name]/runs/[run]/page.tsx"]) {
    const imports = ts.createSourceFile(file, readFileSync(resolve(sourceRoot, file), "utf8"),
      ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX).statements
      .filter(ts.isImportDeclaration);
    // Unrendered page dependencies are irrelevant to these failure paths.
    const modules = Object.fromEntries(imports.map(i => [i.moduleSpecifier.text,
      Object.fromEntries((i.importClause?.namedBindings?.elements ?? []).map(n => [n.name.text, () => null]))]));
    modules[file.includes("/runs/") ? "../../../read-error" : "../read-error"] = errorUI;
    modules["@/lib/config"] = { defaultNamespace: () => "tenant" };
    modules["@/lib/session"] = { currentPrincipal: async () => null };
    modules["next/link"] = { default: ({ href, children }) => React.createElement("a", { href }, children) };
    modules["next/navigation"] = { notFound: () => { throw new Error("ACTUAL_NOT_FOUND"); } };
    const bff = {
      BffError, getTeam: async () => team, getTask: async () => ({}),
      listTaskApprovals: async () => [], getArchivedTeamRun: async () => null,
    };
    modules["@/lib/bff"] = bff;
    const Page = load(file, null, modules).default;
    const args = { params: Promise.resolve({ name: "team", run: "run" }), searchParams: Promise.resolve({}) };
    for (const method of ["getTeam", "getTask", "listTaskApprovals"]) {
      const original = bff[method];
      for (const [kind, error] of Object.entries(failures)) {
        bff[method] = async () => { throw error; };
        const html = renderToStaticMarkup(await Page(args));
        assert.match(html, /role="alert"/, `${file} ${method} ${kind}`);
        assert.doesNotMatch(html, /Run unavailable|No records available|isn&#x27;t reachable/);
        assert.match(html, kind === "conflict" ? /work checkpoint binding changed/
          : kind === "denied" ? /Team access denied/
          : kind === "session" ? /Sign in to view this team/ : /Team data could not be loaded/);
      }
      bff[method] = original;
    }
    const missing = new BffError("not_found", 404);
    if (!file.includes("/runs/")) {
      bff.getTeam = async () => { throw missing; };
      await assert.rejects(Page(args), /ACTUAL_NOT_FOUND/);
    } else {
      bff.getTask = async () => { throw missing; };
      bff.getArchivedTeamRun = async () => { throw failures.upstream; };
      assert.match(renderToStaticMarkup(await Page(args)), /Team data could not be loaded/);
      bff.getArchivedTeamRun = async () => { throw missing; };
      assert.match(renderToStaticMarkup(await Page(args)), /Run unavailable/);
    }
  }
});

test("Team latest-run lookup preserves authoritative chronology instead of sorting opaque IDs", async () => {
  const file = "app/workspace/teams/[name]/page.tsx";
  const imports = ts.createSourceFile(file, readFileSync(resolve(sourceRoot, file), "utf8"),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX).statements.filter(ts.isImportDeclaration);
  const modules = Object.fromEntries(imports.map(i => [i.moduleSpecifier.text,
    Object.fromEntries((i.importClause?.namedBindings?.elements ?? []).map(n => [n.name.text, () => null]))]));
  class BffError extends Error {}
  modules["../read-error"] = load("app/workspace/teams/read-error.tsx", null, { "@/lib/bff": { BffError } });
  modules["@/lib/config"] = { defaultNamespace: () => "work" };
  for (const [binding, expected] of [
    [{ last_generated_task: "run-a-new" }, "run-a-new"],
    [{ last_generated_task: null }, "run-a-new"],
    [{ last_generated_task: "run-a-new", current_assignment_nonce: "current", principal_task: "principal" }, "principal"],
  ]) {
    const calls = [];
    modules["@/lib/bff"] = {
      getTeam: async () => ({ paused: false, phase: "Active",
        generated_tasks: ["run-a-new", "run-f-old"], ...binding }),
      getTask: async (ns, name) => { calls.push([ns, name]); throw new BffError("Stop after real lookup"); },
      listTaskApprovals: async () => [],
    };
    const Page = load(file, null, modules).default;
    await Page({ params: Promise.resolve({ name: "eng" }), searchParams: Promise.resolve({}) });
    assert.deepEqual(calls, [["work", expected]]);
  }
});

test("Run now clears an observed request and keeps polling an unobserved request", () => {
  let index = 0;
  const { RunNowControl } = load("app/workspace/teams/[name]/run-control.tsx", {
    useState: (initial) => [index++ === 1 ? { runKey: "previous-run" } : initial, forbiddenMutation],
    useTransition: () => [false, forbiddenMutation],
  }, { "./run-actions": { runTeamNow: forbiddenMutation } });
  const state = (props) => {
    index = 0;
    return render(RunNowControl, { team: "eng", paused: false, inFlight: false, ...props });
  };
  const waiting = state({ runKey: "previous-run" });
  assert.match(waiting, /Run requested/);
  assert.match(waiting, /data-refresh="true"/);
  const finished = state({ runKey: "new-run" });
  assert.match(finished, />Run now<\/button>/);
  assert.doesNotMatch(finished, /Run requested|data-refresh="true"/);
  assert.match(state({ runKey: "new-run", inFlight: true }), /Run in progress/);
  assert.match(state({ runKey: "previous-run", paused: true }), /Hibernating/);
});

test("A resumed Team cannot be presented as paused while controller status converges", () => {
  const { NowHero } = load("app/workspace/teams/[name]/team-detail-panels.tsx");
  const props = { teamName: "eng", health: "Hibernating", active: false, paused: false,
    runRunning: false, runInFlight: false, everyMinutes: null, commonsEntries: 0,
    nextRunAt: null, lastRunAt: null, delivered: 0, generated: 0, latestRun: null,
    latestOutcome: null, lifecycleMode: "ephemeral", runtimeState: null, idleDeadlineAt: null };
  const resuming = render(NowHero, props);
  assert.match(resuming, /Waiting for active team status/);
  assert.doesNotMatch(resuming, /Paused —|until you resume/);
  const paused = render(NowHero, { ...props, paused: true });
  assert.match(paused, /Paused — no new runs/);
  assert.doesNotMatch(paused, /no sandbox is running/);
});

test("Controller rejection stays visible on paused and enabled Teams", () => {
  const { NowHero } = load("app/workspace/teams/[name]/team-detail-panels.tsx");
  const { TeamsList } = load("app/workspace/teams/teams-list.tsx");
  for (const paused of [true, false]) {
    const team = { name: "rejected", phase: "Degraded", paused, health: null,
      detail: "Delegation depth cannot accommodate this roster.", lifecycle_mode: "ephemeral",
      member_count: 1, generated_task_count: 0, retained_delivered: 0, retained_failed: 0 };
    const hero = render(NowHero, { ...team, teamName: team.name });
    assert.match(hero, /role="alert"/);
    assert.match(hero, /Team blocked by controller validation/);
    assert.match(hero, /Delegation depth cannot accommodate this roster/);
    assert.doesNotMatch(hero, /Hibernating|Waiting for active team status/);
    const list = render(TeamsList, { teams: [team] });
    assert.match(list, /Degraded/);
    assert.match(list, /bg-rose-500/);
    assert.doesNotMatch(list, /Hibernating/);
  }
});

test("Team work exposes review choice and preserves assigned/history-bearing items", () => {
  const pending = render(TeamTasks, { team: "eng", paused: true, tasks: [workItem] });
  assert.match(pending, /aria-label="Task title"/);
  assert.match(pending, /aria-label="Task details"/);
  assert.match(pending, /type="checkbox"/);
  assert.match(pending, /Require human review/);
  assert.match(pending, /Resume the team, then use Run now/);
  assert.match(pending, /<button(?![^>]*disabled="")[^>]*>Remove<\/button>/);
  for (const item of [{ ...workItem, status: "active" }, { ...workItem, history: [{
    run: "old run", run_uid: "uid", assignment_nonce: "nonce", outcome: "changes_requested",
    finished_at: "2026-10-08T22:00:00Z", feedback: "Remove private details",
  }], feedback: "Remove private details" }]) {
    const html = render(TeamTasks, { team: "eng", paused: false, tasks: [item] });
    assert.match(html, /<button[^>]*disabled=""[^>]*>Remove<\/button>/);
    if (item.history) {
      assert.match(html, /Revision feedback: Remove private details/);
      assert.match(html, /Recorded attempts \(1\)/);
      assert.match(html, /2026-10-08 22:00:00 UTC/);
      assert.match(html, /href="\/workspace\/teams\/eng\/runs\/old%20run"/);
    }
  }
});

test("Team review controls require a bound current checkpoint and survive pending-decision reload", () => {
  const task = { ...workItem, status: "awaiting_review", run: "run", run_uid: "uid", assignment_nonce: "nonce" };
  const review = { task_id: task.id, assignment_nonce: "nonce", ready: true, decision_pending: false };
  const props = { team: "eng", paused: false, tasks: [task], reviews: [review] };
  assert.match(render(TeamTasks, props), /<button(?![^>]*disabled="")[^>]*>Approve milestone<\/button>/);
  assert.match(render(TeamTasks, props), /data-refresh="false"/);
  for (const changed of [
    { reviews: [] }, { tasks: [{ ...task, assignment_nonce: null }] },
    { tasks: [{ ...task, run_uid: null }] }, { reviews: [{ ...review, assignment_nonce: "old" }] },
    { reviews: [{ ...review, ready: false }] }, { reviews: [{ ...review, decision_pending: true }] },
  ]) {
    const html = render(TeamTasks, { ...props, ...changed });
    assert.match(html, /<button[^>]*disabled=""[^>]*>Approve milestone<\/button>/);
    assert.match(html, /<button[^>]*disabled=""[^>]*>Request changes<\/button>/);
  }
  const submitted = render(TeamTasks, { ...props, reviews: [{ ...review, decision_pending: true }] });
  assert.match(submitted, /Decision recorded. Waiting for the controller/);
  assert.match(submitted, /data-refresh="true"/);
  assert.match(submitted, /1 awaiting review/);
  assert.doesNotMatch(submitted, /Approved milestone|Work completed/);
  assert.match(render(TeamTasks, { ...props, reviews: [] }), /data-refresh="true"/);
  for (const status of ["pending", "active", "done", "retired"]) {
    assert.match(render(TeamTasks, { ...props, tasks: [{ ...task, status }],
      reviews: [{ ...review, decision_pending: true }] }), /data-refresh="false"/);
  }
});

test("Team source retirement is distinct from delivery and releases only the queue dependency", () => {
  const { TeamTasks: expanded } = load("app/workspace/teams/[name]/team-tasks.tsx", {
    useState: (initial) => [initial === false ? true : initial, forbiddenMutation],
  });
  const retired = { ...workItem, status: "retired", done_at: "2026-10-08T23:00:00Z" };
  const next = { ...workItem, id: "next", title: "Next", depends_on: [retired.id] };
  const html = render(expanded, { team: "eng", paused: false, tasks: [retired, next] });
  assert.match(html, /Source retired/);
  assert.match(html, /not an executed or approved result/);
  assert.match(html, /Source observed: 2026-10-08 23:00:00 UTC/);
  assert.match(html, /1 queued/);
  assert.match(html, /Hide finished work/);
  assert.doesNotMatch(html, /Waiting for:|View result|Recorded attempts|Approve milestone/);
});

test("Team review polls immediately after local submission but not after a new assignment", () => {
  let stateIndex = 0;
  const { TeamTasks: submitted } = load("app/workspace/teams/[name]/team-tasks.tsx", {
    useState: (initial) => [stateIndex++ === 6 ? { checklist: "nonce" } : initial, forbiddenMutation],
  });
  const task = { ...workItem, status: "awaiting_review", run_uid: "uid", assignment_nonce: "nonce" };
  const review = { task_id: task.id, assignment_nonce: "nonce", ready: true, decision_pending: false };
  const props = { team: "eng", paused: false, tasks: [task], reviews: [review] };
  assert.match(render(submitted, props), /data-refresh="true"/);
  stateIndex = 0;
  assert.match(render(submitted, { ...props, tasks: [{ ...task, assignment_nonce: "next" }],
    reviews: [{ ...review, assignment_nonce: "next" }] }), /data-refresh="false"/);
});

test("governance authoring retains controlled drafts on errors and resets action state on close", () => {
  let actionReads = 0;
  let open = true;
  const { AuthorResource } = load("app/console/author-resource.tsx", {
    useState: (initial) => [
      initial === false ? open : initial === '{"old":true}' ? '{"draft":' : initial,
      forbiddenMutation,
    ],
    useActionState: () => {
      actionReads++;
      return [{ error: "Invalid JSON", ok: null }, forbiddenMutation, false];
    },
  }, { "./governance-actions": { applyGovernanceAction: forbiddenMutation } });
  const props = { kind: "ToolPolicy", initialName: "isolated-test", initialSpec: '{"old":true}' };
  const opened = AuthorResource(props);
  assert.equal(typeof opened.type, "function", "action state belongs to the mounted form");
  assert.equal(actionReads, 0);
  const form = opened.type(opened.props);
  const fields = new Map(React.Children.toArray(form.props.children)
    .filter(React.isValidElement).map((child) => [child.props.name, child.props]));
  assert.equal(fields.get("spec").value, '{"draft":');
  assert.equal(fields.get("spec").defaultValue, undefined);
  assert.equal(fields.get("name").value, "isolated-test");
  assert.equal(fields.get("name").readOnly, true);
  const html = renderToStaticMarkup(form);
  assert.match(html, /role="alert"[^>]*>Invalid JSON/);
  assert.match(html, /<textarea[^>]*>\{&quot;draft&quot;:<\/textarea>/);
  open = false;
  assert.equal(AuthorResource(props).type, "button");
  assert.equal(actionReads, 1, "closed editor has no retained action-state hook");
});

test("governance removal waits for observed cleanup and keeps pending cancellation disabled", () => {
  let result = { error: null, ok: null };
  let pending = false;
  const { DeleteResource } = load("app/console/delete-resource.tsx", {
    useState: () => [true, forbiddenMutation],
    useActionState: () => [result, forbiddenMutation, pending],
  }, { "./governance-actions": { deleteGovernanceAction: forbiddenMutation } });
  const props = { kind: "ToolPolicy", name: "isolated-test", label: "tool policy" };
  assert.doesNotMatch(render(DeleteResource, props), /data-refresh/);
  pending = true;
  assert.match(render(DeleteResource, props), /<button[^>]*disabled=""[^>]*>Cancel<\/button>/);
  pending = false;
  result = { error: null, ok: "Deletion accepted" };
  for (const verb of ["Remove", "Revoke"]) {
    const html = render(DeleteResource, { ...props, verb });
    assert.match(html, /data-refresh="true"/);
    assert.match(html, /role="status"/);
    assert.match(html, /requested — waiting for cluster cleanup/);
    assert.doesNotMatch(html, /Removed|Revoked|Yes, remove/);
  }
});

test("operator-cancelled work history cannot claim a completed or approved result", () => {
  const html = render(TeamTasks, { team: "eng", paused: true, tasks: [{
    ...workItem, status: "pending", history: [{
      run: "cancelled-run", run_uid: "uid", assignment_nonce: "nonce",
      outcome: "cancelled", finished_at: "2026-10-09T01:14:13Z",
      feedback: "Stopped after a failed phase",
    }],
  }] });
  assert.match(html, /Cancelled/);
  assert.match(html, /2026-10-09 01:14:13 UTC/);
  assert.match(html, /Stopped after a failed phase/);
  assert.match(html, /href="\/workspace\/teams\/eng\/runs\/cancelled-run"/);
  assert.doesNotMatch(html, />Completed|>Approved|Approve milestone/);
});

test("Team server actions preserve review choice, displayed run binding and API errors", async () => {
  const calls = [];
  const refreshed = [];
  let response = { ok: true, status: 200 };
  const actions = load("app/workspace/teams/[name]/task-actions.ts", null, {
    "next/cache": { revalidatePath: path => refreshed.push(path) },
    "@/lib/config": { defaultNamespace: () => "tenant a" },
    "@/lib/bff": { authenticatedBffFetch: async (path, init) => {
      calls.push({ path, init }); return response;
    } },
  });
  for (const review of [false, true]) {
    assert.equal((await actions.addTeamTask("team a", "Title", "Details", review)).error, null);
    assert.equal(JSON.parse(calls.at(-1).init.body).review_required, review);
  }
  const binding = { assignmentNonce: "displayed-nonce", runUid: "displayed-uid" };
  await actions.reviewTeamTask("team a", "item a", "request_changes", binding, "  Improve it  ");
  assert.equal(calls.at(-1).path, "/api/namespaces/tenant%20a/teams/team%20a/tasks/item%20a/review");
  assert.deepEqual(JSON.parse(calls.at(-1).init.body), {
    decision: "request_changes", feedback: "Improve it", assignment_nonce: "displayed-nonce", run_uid: "displayed-uid",
  });
  response = { ok: false, status: 409, json: async () => ({ error: { message: "Revision changed" } }) };
  const count = refreshed.length;
  assert.equal((await actions.reviewTeamTask("team a", "item a", "approve", binding)).error, "Revision changed");
  assert.equal(refreshed.length, count);
});
const base = {
  running: false, activity: [], telemetry: { rounds: 2, tool_calls: null },
  assignmentEvents: [], approvals: [], ns: "kars-system", name: "briefing",
  runNonce: "rev-2", agentLabel: "Briefing agent", agentPhase: "succeeded",
};
const lifecycle = {
  event_id: "assigned", at: "2026-10-03T06:47:40Z", event_type: "assignment_accepted",
  state: "Running", stage: "executing", child_role: "principal", worker_did: "did:mesh:producer",
  message: "Accepted this revision", outcome: null,
};
const approval = {
  name: "release", phase: "Denied", requested_at: "2026-10-03T06:47:41Z",
  decided_at: "2026-10-03T06:47:42Z", summary: "Release denied", detail: "Needs review", decider: "Reviewer",
};
const round = {
  kind: "round", round: 0, seq: 1, ts: "2026-10-03T06:47:43Z", runNonce: "rev-2",
  ms: 300, prompt_tokens: 3211, completion_tokens: 471, total_tokens: 3682,
  tool_calls: 0, finish_reason: "stop", agent_name: "briefing",
};

test("evidence dates and review history render identically across timezones", () => {
  const previous = process.env.TZ;
  const rendered = [];
  try {
    for (const timezone of ["UTC", "Europe/Budapest", "America/Los_Angeles"]) {
      process.env.TZ = timezone;
      assert.equal(formatEvidenceTime("2026-10-03T06:47:38.353Z"), "2026-10-03 06:47:38 UTC");
      assert.equal(formatEvidenceTime("2026-10-03T06:47:38.353035009+00:00"), "2026-10-03 06:47:38 UTC");
      assert.equal(formatEvidenceTime("2026-10-03T08:47:38+02:00"), "2026-10-03 06:47:38 UTC");
      rendered.push(render(ReviewPanel, {
        task: "briefing", assignmentNonce: "rev-2", kind: "docs", eligible: true,
        initial: { status: "none", revision: 2, history: [{ revision: 1, decision: "request_changes", decided_at: "2026-10-03T06:47:38Z", reviewer: "Reviewer" }] },
      }));
    }
    assert.equal(new Set(rendered).size, 1);
    assert.match(rendered[0], /2026-10-03 06:47:38 UTC/);
  } finally {
    if (previous === undefined) delete process.env.TZ;
    else process.env.TZ = previous;
  }
});

test("missing, malformed and timezone-less dates stay unavailable", () => {
  for (const value of [null, undefined, "", "badZ", "2026-10-03T06:47:38", "2026-10-03", "2026-13-03T06:47:38Z"]) {
    assert.equal(formatEvidenceTime(value), "Time unavailable");
  }
});

test("waiting graph stays visible before detailed records arrive without claiming zero work", () => {
  for (const running of [false, true]) {
    traceCalls.length = 0;
    const html = render(ExecutionExplorer, { ...base, running });
    assert.match(html, /Drill-down/);
    assert.match(html, running ? /Waiting for revision-bound tool records/ : /This does not mean no work occurred/);
    assert.doesNotMatch(html, /Launch the execution|Awaiting first action|Complete execution lifetime|0\/0|0 tool calls/);
    assert.equal(traceCalls[0][2], running);
    assert.equal(traceCalls[0][4], "rev-2");
    assert.ok(traceCalls.slice(1).every((args) => args[2] === false), "Child must not open a duplicate live stream");
  }
  const { ActivityStream } = load("components/activity-stream.tsx");
  const totals = render(ActivityStream, base);
  assert.match(totals, /Detailed trace unavailable/);
  assert.match(totals, /2 model rounds/);
  assert.doesNotMatch(totals, /0 tool calls/);
  assert.doesNotMatch(render(ActivityStream, { ...base, telemetry: null }), /Recorded run totals:/);
});

test("missing trace retains genuine assignment and timestamped approval records", () => {
  const html = render(ExecutionExplorer, { ...base, assignmentEvents: [lifecycle], approvals: [approval] });
  assert.match(html, /Drill-down/);
  assert.match(html, /Recorded execution timeline/);
  assert.match(html, /Accepted this revision/);
  assert.match(html, /Release denied/);
  assert.doesNotMatch(html, /Complete execution lifetime|Launch the execution/);
});

test("partial round records retain graph and timeline without claiming no work", () => {
  const html = render(ExecutionExplorer, { ...base, activity: [round] });
  assert.match(html, /Drill-down/);
  assert.match(html, /Model round 1/);
  assert.match(html, /3,682 tokens/);
  const { AgentGraph } = load("components/agent-graph.tsx");
  assert.match(render(AgentGraph, base), /No tool actions are retained here/);
  assert.doesNotMatch(html, /Detailed trace unavailable|Launch the execution|completed without retained tool actions/);
});

test("timeline distinguishes unavailable records from an unmatched search", () => {
  const empty = render(ExecutionLifetime, base);
  assert.match(empty, /No timeline records are available here/);
  assert.doesNotMatch(empty, /No retained event matches|0\/0/);
  const filtered = render(ExecutionLifetime, { ...base, assignmentEvents: [lifecycle], focusQuery: "not-present" });
  assert.match(filtered, /No retained event matches this search/);
  assert.doesNotMatch(filtered, /No timeline records are available/);
});

test("rendered approval controls fail closed for unavailable, failed and pending revisions", () => {
  const props = { task: "briefing", assignmentNonce: "rev-2", kind: "docs", initial: null };
  for (const extra of [{}, { eligible: false }, { eligible: true, assignmentNonce: "" }]) {
    assert.match(render(ReviewPanel, { ...props, ...extra }), /<button[^>]*disabled=""[^>]*>Approve deliverable<\/button>/);
  }
  assert.match(render(ReviewPanel, { ...props, eligible: true }), /<button(?![^>]*disabled="")[^>]*>Approve deliverable<\/button>/);
  const pending = render(ReviewPanel, { ...props, eligible: true, initial: { redrive_pending: true } });
  assert.doesNotMatch(pending, />Approve deliverable<\/button>/);
  assert.match(pending, /Revision requested/);
});

test("stopped review allows feedback but keeps revision execution behind explicit Overview launch", () => {
  const props = { task: "mission/a", assignmentNonce: "rev-2", kind: "docs", eligible: true, initial: null };
  const stopped = render(ReviewPanel, props);
  assert.match(stopped, /runtime is stopped/);
  assert.match(stopped, /href="\/workspace\/missions\/mission%2Fa\?tab=overview"/);
  assert.match(stopped, /A requested revision waits until/);
  assert.match(stopped, /<button(?![^>]*disabled="")[^>]*>Request changes<\/button>/);
  assert.match(stopped, /<button(?![^>]*disabled="")[^>]*>Approve deliverable<\/button>/);
  const approved = render(ReviewPanel, { ...props, initial: { status: "approved" } });
  assert.match(approved, /<button(?![^>]*disabled="")[^>]*>Request changes to iterate<\/button>/);
  const running = render(ReviewPanel, { ...props, launched: true });
  assert.doesNotMatch(running, /runtime is stopped/);
  assert.match(running, /<button(?![^>]*disabled="")[^>]*>Request changes<\/button>/);
  const pending = render(ReviewPanel, { ...props, initial: { redrive_pending: true } });
  assert.match(pending, /runtime remains stopped/);
  assert.match(pending, /Launch the revision from Overview/);
  assert.doesNotMatch(pending, />Request changes<\/button>/);
  for (const launched of [false, true]) {
    assert.match(render(ReviewPanel, { ...props, launched, assignmentNonce: "" }),
      /<button[^>]*disabled=""[^>]*>Request changes<\/button>/);
  }
});

test("stopped Mission feedback submits the displayed run without implicitly launching compute", async () => {
  const feedback = "Read both handoffs and schedule both breaks.";
  const values = [feedback, null, true, false];
  const tasks = [], submissions = [];
  let cursor = 0, refreshes = 0;
  const { ReviewPanel: InteractiveReview } = load("app/workspace/missions/[name]/review-panel.tsx", {
    useState: () => {
      const index = cursor++;
      return [values[index], value => { values[index] = value; }];
    },
    useTransition: () => [false, callback => { tasks.push(callback()); }],
  }, {
    "next/navigation": { useRouter: () => ({ refresh: () => { refreshes++; } }) },
    "./review-actions": { submitReview: async (...args) => { submissions.push(args); return { error: null }; } },
    "@/app/tasks/[name]/launch-actions": { setLaunch: forbiddenMutation },
  });
  const find = node => {
    if (!React.isValidElement(node)) return null;
    if (node.type === "button" && node.props.children === "Request revision") return node;
    for (const child of React.Children.toArray(node.props.children)) {
      const found = find(child);
      if (found) return found;
    }
    return null;
  };
  const props = { task: "briefing", assignmentNonce: "run-1", kind: "docs", initial: null, launched: false };
  const button = find(InteractiveReview(props));
  assert.equal(button.props.disabled, false);
  button.props.onClick();
  await tasks.pop();
  assert.deepEqual(submissions, [["briefing", "run-1", "request_changes", feedback]]);
  assert.equal(refreshes, 1);
  assert.equal(values[0], "");
  assert.equal(values[2], false);
});

test("revision retry is exposed only for a stopped pending revision with verified failure", () => {
  const props = { task: "briefing", assignmentNonce: "", kind: "docs", launched: false,
    initial: { redrive_pending: true, retryable_revision_nonce: "failed-revision", revision: 5, history: [] } };
  const html = render(ReviewPanel, props);
  assert.match(html, /Retry failed revision/);
  assert.match(html, /same complete documents and feedback/);
  assert.match(html, /lifetime budget does not reset/);
  assert.match(html, /request remains stopped/);
  assert.doesNotMatch(html, /Launch the revision from Overview|Approve deliverable/);
  for (const changed of [
    { ...props, launched: true },
    { ...props, initial: { ...props.initial, redrive_pending: false } },
    { ...props, initial: { ...props.initial, retryable_revision_nonce: null } },
  ]) {
    assert.doesNotMatch(render(ReviewPanel, changed), /Retry failed revision/);
  }
});

test("explicit retry uses the displayed failed nonce, never launches, and surfaces ambiguous errors", async () => {
  for (const outcome of [{ error: null }, { error: "Request outcome unconfirmed; refresh before retrying." }]) {
    const values = ["", null, false, false], tasks = [], submissions = [];
    let cursor = 0, refreshes = 0;
    const { ReviewPanel: InteractiveReview } = load("app/workspace/missions/[name]/review-panel.tsx", {
      useState: () => {
        const index = cursor++;
        return [values[index], value => { values[index] = value; }];
      },
      useTransition: () => [false, callback => { tasks.push(callback()); }],
    }, {
      "next/navigation": { useRouter: () => ({ refresh: () => { refreshes++; } }) },
      "./review-actions": { submitReview: async (...args) => { submissions.push(args); return outcome; } },
      "@/app/tasks/[name]/launch-actions": { setLaunch: forbiddenMutation },
    });
    const find = node => {
      if (!React.isValidElement(node)) return null;
      if (node.type === "button" && node.props.children === "Retry failed revision") return node;
      for (const child of React.Children.toArray(node.props.children)) {
        const found = find(child);
        if (found) return found;
      }
      return null;
    };
    const props = { task: "briefing", assignmentNonce: "", kind: "docs", launched: false,
      initial: { redrive_pending: true, retryable_revision_nonce: "failed-revision", history: [] } };
    find(InteractiveReview(props)).props.onClick();
    await tasks.pop();
    assert.deepEqual(submissions, [["briefing", "failed-revision", "retry_revision"]]);
    assert.equal(refreshes, outcome.error ? 0 : 1);
    if (outcome.error) {
      cursor = 0;
      const html = renderToStaticMarkup(InteractiveReview(props));
      assert.match(html, /role="alert"/);
      assert.match(html, /Request outcome unconfirmed/);
    }
  }
});

test("Mission outcome and files remain informative after feedback replaces the current nonce", () => {
  const file = "app/workspace/missions/[name]/page.tsx";
  const page = ts.createSourceFile(file, readFileSync(resolve(sourceRoot, file), "utf8"),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const panels = new Map();
  const visit = node => {
    if (ts.isObjectLiteralExpression(node)) {
      const id = node.properties.find(p => p.name?.getText(page) === "id")?.initializer?.text;
      if (id === "deliverable" || id === "artifacts") {
        panels.set(id, node.properties.find(p => p.name?.getText(page) === "node")?.initializer);
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(page);
  assert.equal(panels.size, 2);
  const review = { redrive_pending: true, requested_revision_nonce: "rev-new", history: [] };
  for (const [id, expression] of panels) {
    const exports = {};
    const code = `export function Panel({task, review, currentRunNonce}) {
      const name = "mission/a", currentResult = null, realDelivery = false;
      return (${expression.getText(page)});
    }`;
    runInNewContext(ts.transpileModule(code, { compilerOptions: {
      module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText, {
      exports, require, ReviewPanel, reviewKind: () => "docs",
      HonestState: load("components/honest-state.tsx").HonestState,
    });
    const props = { task: { launched: false, artifacts: [], pull_requests: [] },
      review, currentRunNonce: "rev-new" };
    const pending = render(exports.Panel, props);
    assert.match(pending, id === "deliverable" ? /Launch the revision from Overview/ : /No files for this revision yet/);
    assert.doesNotMatch(pending, />Approve deliverable<\/button>|>Request changes<\/button>/);
    const stale = render(exports.Panel, { ...props, currentRunNonce: "different-run" });
    assert.match(stale, id === "deliverable" ? /No deliverable for this revision yet/ : /No files for this revision yet/);
    assert.doesNotMatch(stale, /Changes requested|Launch the revision from Overview/);
  }
});

const { RouterActivityPanel } = load("components/router-activity-panel.tsx");
const { missionRunState, reviewKind } = load("lib/mission-run-evidence.ts");
const { ROUTER_OBSERVATIONS_ARTIFACT: systemFile } = load("lib/router-activity.ts");
const observed = {
  version: 1, source: "runtime-forwarded-router-observations", coverage: "returned-responses-only",
  responses: 3, scope_id: "scope-a", rounds: [1, 2], state: "observed",
  trace: {
    scope_id: "scope-a", rounds: [1, 2], coverage: "returned-responses-only", durable: false,
    missing_rounds: [2], dropped_events: 4, truncated: true,
    events: [{
      kind: "round", round: 1, seq: 1, scope_id: "scope-a", source: "router-upstream",
      provider: "foundry", model: "gpt-5.4-mini", http_status: 200, accepted: true, outcome: "complete",
      usage: { prompt_tokens: 0, completion_tokens: null, total_tokens: null }, usage_state: "partial",
      finish_reason: "stop", partial_observation: true, ms: 23, tool_calls_observed: 1,
    }],
  },
};
const producer = {
  task_uid: "task-uid", run_nonce: "rev-2", assignment_id: "assignment-2",
  agent_name: "Briefing writer", agent_did: "did:mesh:writer", dispatcher_did: "did:mesh:dispatcher",
  sandbox_uid: "sandbox-uid", pod_uid: "pod-uid", runtime_boot_id: "boot-id",
};
const binding = { producer, observations: observed, provisional: false };
const routerProps = { binding, ns: "kars-system", name: "briefing" };
const panelWithObservations = observations => ({ ...routerProps, binding: { ...binding, observations } });
const sharedObservations = JSON.parse(readFileSync(new URL("../../../inference-router/tests/fixtures/router-observations-v1.json", import.meta.url), "utf8"));

test("ordinary Mission activity passes only current reviewed phases into the shared explorer", () => {
  const file = "app/workspace/missions/[name]/page.tsx";
  const page = ts.createSourceFile(file, readFileSync(resolve(sourceRoot, file), "utf8"),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let phaseVariable, explorer;
  const visit = node => {
    if (ts.isVariableDeclaration(node) && node.name.getText(page) === "phases") phaseVariable = node;
    if (ts.isJsxSelfClosingElement(node) && node.tagName.getText(page) === "ExecutionExplorer") explorer = node;
    ts.forEachChild(node, visit);
  };
  visit(page);
  assert.equal(phaseVariable?.initializer?.getText(page), "reviewedPhases(task) ?? []");
  const prop = explorer?.attributes.properties.find(p => p.name?.getText(page) === "phases");
  assert.equal(prop?.initializer?.expression?.getText(page), "phases");
  const { reviewedPhases, reviewedPhaseRouterActivity } = load("lib/team-run-evidence.ts");
  const phaseProducer = { ...producer, task_uid: "phase-uid", run_nonce: "phase-run", status: "succeeded" };
  const phase = { role: "checklist-reviewer", phase: "quality-check", state: "completed",
    task_name: "reviewed-phase", task_uid: "phase-uid", run_nonce: "phase-run",
    result: { assignment_nonce: "phase-run", reviewable: true, status: "ok", source: "durable_agent", output: "Reviewed checklist",
      run_evidence: phaseProducer }, router_activity: sharedObservations };
  const task = { current_run_nonce: base.runNonce,
    reviewed_execution: { run_nonce: base.runNonce, phases: [phase], issue: null } };
  assert.ok(reviewedPhaseRouterActivity(reviewedPhases(task)[0]));
  const html = render(ExecutionExplorer, { ...base, phases: reviewedPhases(task) ?? [] });
  assert.match(html, /checklist-reviewer/);
  assert.match(html, /quality-check/);
  assert.match(html, /Run: phase-run\nAssignment: assignment-2/);
  assert.match(html, /Harness reported: fetch/);
  for (const changed of [
    { ...task, current_run_nonce: "replacement-run" },
    { ...task, reviewed_execution: { ...task.reviewed_execution, issue: "Source custody changed" } },
    { ...task, reviewed_execution: { ...task.reviewed_execution,
      phases: [{ ...phase, task_uid: "replacement-phase" }] } },
  ]) {
    const unavailable = render(ExecutionExplorer, { ...base, phases: reviewedPhases(changed) ?? [] });
    assert.doesNotMatch(unavailable, /Run: phase-run|Harness reported: fetch/);
  }
});

test("deployment timeline recognizes bound reviewed-phase work before principal dispatch", () => {
  const phase = { role: "reviewer", phase: "check", state: "completed",
    task_name: "phase-task", task_uid: "phase-uid", run_nonce: "phase-run",
    result: { assignment_nonce: "phase-run", reviewable: true, status: "ok",
      source: "durable_agent", output: "Reviewed facts",
      run_evidence: { ...producer, task_uid: "phase-uid", run_nonce: "phase-run", status: "succeeded" } },
    router_activity: sharedObservations };
  const task = { launched: true, sandbox: null, execution_phase: "Launching",
    current_run_nonce: base.runNonce, result: null, assignment: null, activity: [],
    reviewed_execution: { run_nonce: base.runNonce, phases: [phase], issue: null } };
  const html = render(DeployTimeline, { task });
  assert.match(html, /Execution observed/);
  assert.match(html, /retained for 1 reviewed phase/);
  assert.match(html, /principal sandbox can start after their handoffs/);
  assert.match(html, /does not establish principal readiness/);
  assert.doesNotMatch(html, /Waiting for the first revision-bound|<h2[^>]*>Delivered</);
  for (const changed of [
    { ...task, current_run_nonce: "replacement-run" },
    { ...task, reviewed_execution: { ...task.reviewed_execution, issue: "Custody changed" } },
    { ...task, reviewed_execution: { ...task.reviewed_execution,
      phases: [{ ...phase, task_uid: "replacement-phase" }] } },
    { ...task, reviewed_execution: { ...task.reviewed_execution,
      phases: [{ ...phase, result: { ...phase.result, assignment_nonce: "old" } }] } },
    { ...task, reviewed_execution: { ...task.reviewed_execution,
      phases: [{ ...phase, router_activity: null }] } },
    { ...task, reviewed_execution: { ...task.reviewed_execution,
      phases: [{ ...phase, router_activity: { ...sharedObservations,
        trace: { ...sharedObservations.trace, events: [] } } }] } },
  ]) {
    const unavailable = render(DeployTimeline, { task: changed });
    assert.match(unavailable, /Starting execution/);
    assert.doesNotMatch(unavailable, /Execution observed|retained for 1 reviewed phase/);
  }
});

test("revoking router activity clears its drill-down focus without hiding retained legacy records", () => {
  const renderSelected = (routerActivity, query) => {
    const states = ["tools", { nonce: base.runNonce, query }];
    const { ExecutionExplorer: SelectedExplorer } = load("components/execution-explorer.tsx", {
      useState: () => [states.shift(), forbiddenMutation],
    });
    return render(SelectedExplorer, { ...base, activity: [round], routerActivity });
  };
  for (const query of [rawAgent(sharedObservations)[0].traceQuery]) {
    assert.match(renderSelected(binding, query), /Focused from graph:/);
    const revoked = renderSelected(null, query);
    assert.doesNotMatch(revoked, /Focused from graph:|Selected router observations|All observations for selected producer/);
    assert.match(revoked, /Rounds and tool records/);
    assert.match(revoked, /3,682/);
  }
  assert.doesNotMatch(renderSelected(binding, "observations:ids=%5B%5D"), /Focused from graph:/);
  assert.match(renderSelected(null, "agent:briefing"), /Focused from graph:/);
});

test("default timeline unfolds cumulative router records without duplicating legacy model rows", () => {
  const first = { ...sharedObservations, responses: 1, rounds: [1], trace: {
    ...sharedObservations.trace, rounds: [1], events: sharedObservations.trace.events.slice(0, 2),
  } };
  const timelines = [first, sharedObservations].map(observations => {
    const html = render(ExecutionExplorer, { ...base, running: true, activity: [round],
      routerActivity: { ...binding, observations, provisional: true },
      assignmentEvents: [lifecycle], approvals: [approval],
    });
    return html.slice(html.indexOf("Recorded execution timeline"));
  });
  for (const [index, html] of timelines.entries()) {
    assert.equal((html.match(/<li>/g) ?? []).length, index === 0 ? 4 : 6);
    assert.equal((html.match(/Timestamp unavailable/g) ?? []).length, index === 0 ? 2 : 4);
    assert.match(html, /Live provisional observations/);
    assert.match(html, /Run: rev-2 · Assignment: assignment-2 · Producer: did:mesh:writer/);
    assert.match(html, /Router response · round 1/);
    assert.match(html, /Model proposed: fetch/);
    assert.match(html, /Proposal only; execution not established/);
    assert.match(html, /06:47:40.000 UTC/);
    assert.match(html, /06:47:42.000 UTC/);
    assert.doesNotMatch(html, /06:47:43|3,682 tokens|Model round 1|href=/);
    assert.ok(html.indexOf("Accepted this revision") < html.indexOf("Release denied"));
    assert.ok(html.indexOf("Release denied") < html.indexOf("Sequence 1"));
  }
  assert.doesNotMatch(timelines[0], /Harness reported|Sequence 3|round 2/);
  assert.match(timelines[1], /Harness reported: fetch/);
  assert.match(timelines[1], /Reported in router round 2; outcome failure/);
  assert.match(timelines[1], /Router response · round 2/);
  assert.doesNotMatch(timelines[1], /round 0|round 3/);
});

test("router-only timeline preserves source order, unavailable timestamps and nullable outcomes", () => {
  for (const ok of [false, null, true]) {
    const observations = structuredClone(sharedObservations);
    observations.trace.events[2].ok = ok;
    const html = render(ExecutionLifetime, { ...base, routerActivity: { ...binding, observations } });
    assert.equal((html.match(/<li>/g) ?? []).length, 4);
    assert.doesNotMatch(html, / UTC|0 tokens|Model round 0/);
    assert.match(html, /tokens: unavailable/);
    assert.match(html, /no ordering between producers is inferred/);
    assert.ok(html.indexOf("Sequence 2") < html.indexOf("Sequence 3"));
    assert.ok(html.indexOf("Sequence 3") < html.indexOf("Sequence 4"));
    assert.ok(html.includes(`outcome ${ok === null ? "unavailable" : ok ? "success" : "failure"}`));
    assert.match(html, /Retained revision observations/);
    assert.match(html, /Source: model-proposed/);
    assert.match(html, /Source: harness-reported/);
  }
});

test("timeline searches router identity, sources, calls and outcomes", () => {
  for (const [focusQuery, count] of [
    ["Briefing writer", 4], ["did:mesh:writer", 4], ["assignment-2", 4],
    ["model-proposed", 1], ["fetch", 2], ["failure", 1], ["not-present", 0],
  ]) {
    const html = render(ExecutionLifetime, { ...base, focusQuery,
      routerActivity: { ...binding, observations: sharedObservations },
    });
    assert.equal((html.match(/<li>/g) ?? []).length, count, focusQuery);
    if (count === 0) assert.match(html, /No retained event matches this search/);
  }
});

test("timeline makes unavailable and incomplete router coverage explicit while retaining lifecycle", () => {
  const incomplete = render(ExecutionLifetime, { ...base, routerActivity: binding });
  for (const text of ["missing rounds: 2", "Recorder-wide dropped events: 4", "Selection truncated: yes"])
    assert.ok(incomplete.includes(text), text);
  const unavailable = render(ExecutionLifetime, { ...base, activity: [round], assignmentEvents: [lifecycle],
    routerActivity: { ...binding, observations: { ...sharedObservations, state: "unavailable", reason: "Router read failed" } },
  });
  assert.match(unavailable, /Router observations unavailable: Router read failed/);
  assert.match(unavailable, /Accepted this revision/);
  assert.doesNotMatch(unavailable, /Model round 1|Router response|Sequence 1|Selection truncated/);
});

test("router observations name the producer and revision without inventing missing usage", () => {
  const html = render(RouterActivityPanel, routerProps);
  for (const text of ["Returned with this completed revision by Briefing writer.", "Run: rev-2", "Assignment: assignment-2", "Producer: did:mesh:writer", "Router round 1", "gpt-5.4-mini", "Provider: foundry", "HTTP: 200",
    "input: 0", "output: Unavailable", "total: Unavailable", "usage: partial", "Partial observation",
    "Responses received: 3", "Correlated router rounds: 2", "missing rounds: 2", "Recorder-wide dropped events: 4", "Selection truncated: yes",
    "not independent proof of tool execution", "Hidden retries and responses without IDs are not covered"]) {
    assert.ok(html.includes(text), text);
  }
  assert.match(html, /href="\/api\/namespaces\/kars-system\/tasks\/briefing\/artifact\/kars-router-observations.json\?run_nonce=rev-2"/);
  const unpinned = render(RouterActivityPanel, { ...routerProps, binding: { ...binding, producer: { ...producer, run_nonce: "" } } });
  assert.doesNotMatch(unpinned, /href=/);
});

test("router proposals and harness outcomes remain distinct and nullable", () => {
  const proposal = { kind: "tool_proposed", scope_id: "scope-a", round: 1, seq: 2, source: "model-proposed", call_id: "call-a", name: "file_write", ok: null };
  const html = render(RouterActivityPanel, panelWithObservations({ ...observed, trace: { ...observed.trace, events: [proposal,
    ...[true, false, null].map((ok, index) => ({ ...proposal, kind: "tool_result", seq: index + 3, source: "harness-reported", ok, reported_in_round: 2 })),
  ] } }));
  for (const text of ["Model proposed: file_write", "Proposal only; execution not established", "Harness reported: file_write",
    "Proposed in router round 1", "Reported in router round 2", "Reported outcome: Success", "Reported outcome: Failure", "Reported outcome: Unavailable"]) {
    assert.ok(html.includes(text), text);
  }
});

test("router tools nest under the proposing round while preserving the reporting round", () => {
  const html = render(RouterActivityPanel, panelWithObservations(sharedObservations));
  const tools = html.match(/<ol aria-label="Tools proposed in router round 1"[^>]*>([\s\S]*?)<\/ol>/)?.[1];
  assert.ok(tools, "round one has a nested tool list");
  assert.equal((tools.match(/<details /g) ?? []).length, 2);
  assert.match(tools, /Model proposed: fetch/);
  assert.match(tools, /Harness reported: fetch/);
  assert.match(tools, /Reported in router round 2 · Reported outcome: Failure/);
  assert.doesNotMatch(tools, /Router round [0-9]|Provider:/);
  assert.ok(html.indexOf("Router round 1") < html.indexOf("Tools proposed in router round 1"));
  assert.ok(html.indexOf("Tools proposed in router round 1") < html.indexOf("Router round 2"));
  assert.doesNotMatch(html, /Tools proposed in router round 2/);
  assert.equal((html.match(/<details /g) ?? []).length, 4);
});

test("router hierarchy preserves nonconsecutive raw round IDs and missing round evidence", () => {
  const observations = structuredClone(sharedObservations);
  observations.rounds = observations.trace.rounds = [3, 8];
  for (const event of observations.trace.events) {
    event.round = event.round === 1 ? 3 : 8;
    if (event.kind === "tool_result") event.reported_in_round = 8;
  }
  const html = render(RouterActivityPanel, panelWithObservations(observations));
  assert.match(html, /Router round 3/);
  assert.match(html, /Router round 8/);
  assert.match(html, /Tools proposed in router round 3/);
  assert.match(html, /Reported in router round 8/);
  assert.doesNotMatch(html, /Router round [12] ·|Tools proposed in router round [128]/);
  observations.trace.events = observations.trace.events.filter(event => event.kind !== "round");
  const toolsOnly = render(RouterActivityPanel, panelWithObservations(observations));
  assert.match(toolsOnly, /Proposing round 3 · round record unavailable/);
  assert.doesNotMatch(toolsOnly, /Router round [0-9]|Provider:|Tokens —/);
});

test("empty and unavailable router selections do not claim zero work", () => {
  const empty = render(RouterActivityPanel, panelWithObservations({ ...observed, trace: { ...observed.trace, events: [] } }));
  assert.match(empty, /No selected records remain available. This does not establish that no work occurred/);
  const unavailable = render(RouterActivityPanel, panelWithObservations({
    ...observed, state: "unavailable", reason: "Response correlation changed or was invalid",
  }));
  assert.match(unavailable, /Router observations unavailable: Response correlation changed or was invalid/);
  assert.doesNotMatch(unavailable, /Router round 1|Coverage gaps|0 tool calls/);
});

test("raw graph labels identify observations, not principal authority or executed proposals", () => {
  const html = render(ExecutionExplorer, { ...base, routerActivity: { ...binding, observations: sharedObservations } });
  assert.match(html, /Recorded producer/);
  assert.match(html, /Model proposed: fetch/);
  assert.match(html, /Harness reported: fetch/);
  assert.match(html, />model-proposed</);
  assert.match(html, />harness-reported</);
  assert.match(html, /Available timestamps identify router observations/);
  assert.doesNotMatch(html, /timestamps and destinations are not recorded/);
  assert.doesNotMatch(html, />core<|>\s*root\s*<|>recorded action</);
  const legacy = render(ExecutionExplorer, { ...base, activity: [round] });
  assert.match(legacy, />core</);
  assert.match(legacy, />\s*root\s*</);
  assert.match(legacy, />recorded action</);
});

const { buildAgentExecutions } = load("components/agent-graph/execution.ts");
const { actionRound, exactActionQuery, exactActionsQuery } = load("components/agent-graph/activity.ts");
const { routerProducerKey } = load("lib/router-activity.ts");
const { SelectionInspector } = load("components/agent-graph/inspectors.tsx");
const rawAgent = observations => buildAgentExecutions({
  agentLabel: "Unrelated current principal", agentModel: "unrelated-model", agentRuntime: "unrelated-runtime",
  agentPhase: "succeeded", name: "briefing", running: false, events: [round],
  subAgents: [{ name: "Unrelated specialist", role: "Unrelated role" }],
  routerActivity: { ...binding, observations },
});

test("raw graph retains exact producer, positive rounds and nullable evidence without inheriting current roster", () => {
  const agents = rawAgent(sharedObservations);
  assert.equal(agents.length, 1);
  const [agent] = agents;
  assert.equal(agent.displayName, producer.agent_name);
  assert.equal(agent.observedAgentInstance, producer.pod_uid);
  assert.equal(agent.runtime, null);
  assert.equal(agent.lastActivity, null);
  assert.equal(agent.destinations.length, 0);
  assert.equal(agent.rounds, 2);
  assert.equal(agent.toolCalls, 1);
  assert.equal(agent.failures, 1);
  assert.doesNotMatch(JSON.stringify(agent), /Unrelated|unrelated-model|unrelated-runtime/);
  assert.deepEqual(Array.from(agent.actions, actionRound), [1, 1, 1, 2]);
  assert.ok(agent.actions.every(action => action.ts === null));
  const proposal = render(SelectionInspector, { selection: { kind: "action", agent, action: agent.actions[1] } });
  for (const text of ["Proposal only", "Duration unavailable", "Timestamp unavailable", "Router round 1", "Raw arguments excluded from telemetry"])
    assert.ok(proposal.includes(text), text);
  assert.doesNotMatch(proposal, /Succeeded|Success reported|Updated a file/);
  const result = render(SelectionInspector, { selection: { kind: "action", agent, action: agent.actions[2] } });
  assert.match(result, /Failure reported/);
  assert.match(result, /Reported in router round 2/);
  const unknown = structuredClone(sharedObservations);
  unknown.trace.events[2].ok = null;
  const [unknownAgent] = rawAgent(unknown);
  const unknownResult = render(SelectionInspector, { selection: { kind: "action", agent: unknownAgent, action: unknownAgent.actions[2] } });
  assert.match(unknownResult, /Outcome unavailable/);
  assert.doesNotMatch(unknownResult, /Failure reported|Success reported/);
});

test("new router timestamps and safe write details survive live validation, graph, tools and timeline", () => {
  const { liveRouterActivity, advancesRouterActivity } = load("lib/router-activity.ts");
  const observations = structuredClone(sharedObservations);
  observations.trace.events.forEach((event, index) => {
    event.observed_at = `2026-10-08T20:00:00.00${index}Z`;
    if (event.kind !== "round") event.name = "file_write";
  });
  const event = observations.trace.events[2];
  event.ok = true;
  event.ms = 17;
  event.result_summary = { kind: "file_write", bytes: 1245, artifact_name: "checklist.md" };
  const live = { ...producer, attempt_uid: "attempt-1", progress_sequence: 1, observations };
  assert.ok(liveRouterActivity(live, producer.run_nonce));
  const [agent] = rawAgent(observations);
  assert.equal(agent.lastActivity, "2026-10-08T20:00:00.003Z");
  assert.equal(agent.actions[2].ms, 17);
  const panels = [
    render(RouterActivityPanel, panelWithObservations(observations)),
    render(SelectionInspector, { selection: { kind: "action", agent, action: agent.actions[2] } }),
    render(ExecutionLifetime, { ...base, routerActivity: { ...binding, observations } }),
  ];
  for (const html of panels) {
    assert.match(html, /Wrote 1245 bytes; exported checklist.md/);
    assert.doesNotMatch(html, /Timestamp unavailable|Timestamp not retained|outcome unavailable|Result detail was not reported/);
  }
  assert.match(panels[0], /Router observed at/);
  for (const change of [
    { observed_at: "2026-02-30T20:00:00.000Z" }, { observed_at: "2026-10-08T20:00:00" },
    { observed_at: 123 }, { ms: -1 }, { ms: 3_600_001 }, { ok: false },
    { result_summary: { kind: "file_write", bytes: 1245, content: "secret-marker" } },
    { result_summary: { kind: "file_write", bytes: 1245, artifact_name: "../secret" } },
    { result_summary: { kind: "file_read", bytes: 5, returned_bytes: 6, truncated: false } },
  ]) {
    const bad = structuredClone(live);
    Object.assign(bad.observations.trace.events[2], change);
    assert.equal(liveRouterActivity(bad, producer.run_nonce), null, JSON.stringify(change));
  }
  const changedHistory = structuredClone(live);
  changedHistory.progress_sequence++;
  changedHistory.observations.trace.events[2].ms++;
  assert.equal(advancesRouterActivity(live, changedHistory), false);
});

test("raw drill-down selects exact producer observation IDs and rejects malformed or foreign selections", () => {
  const [agent] = rawAgent(sharedObservations);
  const selected = render(RouterActivityPanel, { ...panelWithObservations(sharedObservations), focusQuery: exactActionQuery(agent, agent.actions[2]) });
  assert.match(selected, /Harness reported: fetch/);
  assert.match(selected, /Proposing round 1 · round record not selected/);
  assert.match(selected, /Reported in router round 2/);
  assert.doesNotMatch(selected, /Model proposed: fetch|Router round [0-9]|Provider:|Tokens —|usage:|Finish:/);
  const foreign = exactActionQuery(agent, { ...agent.actions[2], id: `${agent.actions[2].id}-foreign` });
  for (const focusQuery of [foreign, "observations:ids=not-json", "observations:ids=%7B%7D", "observations:ids=%5B1%5D"])
    assert.match(render(RouterActivityPanel, { ...panelWithObservations(sharedObservations), focusQuery }), /No selected records remain available/);
  const grouped = render(RouterActivityPanel, { ...panelWithObservations(sharedObservations),
    focusQuery: exactActionsQuery(agent, agent.actions.slice(0, 2)),
  });
  assert.match(grouped, /Router round 1/);
  assert.match(grouped, /Model proposed: fetch/);
  assert.doesNotMatch(grouped, /Harness reported: fetch|Router round 2/);
});

test("producer drill-down follows cumulative observations while exact actions remain pinned", () => {
  const first = { ...sharedObservations, responses: 1, rounds: [1], trace: {
    ...sharedObservations.trace, rounds: [1], events: sharedObservations.trace.events.slice(0, 2),
  } };
  const [agent] = rawAgent(first);
  const focusQuery = agent.traceQuery;
  const early = render(RouterActivityPanel, { ...panelWithObservations(first), focusQuery });
  assert.match(early, /Model proposed: fetch/);
  assert.doesNotMatch(early, /Harness reported: fetch|Router round 2/);
  const grown = render(RouterActivityPanel, { ...panelWithObservations(sharedObservations), focusQuery });
  assert.match(grown, /Harness reported: fetch/);
  assert.match(grown, /Router round 2/);
  assert.equal(rawAgent(sharedObservations)[0].traceQuery, focusQuery);
  const exact = render(RouterActivityPanel, { ...panelWithObservations(sharedObservations),
    focusQuery: exactActionQuery(agent, agent.actions[1]),
  });
  assert.match(exact, /Model proposed: fetch/);
  assert.doesNotMatch(exact, /Harness reported: fetch|Router round 2/);
});

test("producer focus rejects changed identity, missing keys and ambiguous selectors", () => {
  const focusQuery = rawAgent(sharedObservations)[0].traceQuery;
  for (const field of Object.keys(producer)) {
    const changed = { ...binding, observations: sharedObservations, producer: { ...producer, [field]: `${producer[field]}-changed` } };
    assert.match(render(RouterActivityPanel, { ...routerProps, binding: changed, focusQuery }), /No selected records remain available/, field);
  }
  const encoded = new URLSearchParams({ producer: routerProducerKey(producer) }).toString();
  for (const query of ["observations:", "observations:producer=", "observations:producer=not-json",
    `observations:${encoded}&${encoded}`, `observations:${encoded}&ids=%5B%5D`, `observations:${encoded}&unexpected=true`]) {
    assert.match(render(RouterActivityPanel, { ...panelWithObservations(sharedObservations), focusQuery: query }), /No selected records remain available/, query);
  }
});

test("empty and unavailable producer focus follows only its bound producer, never legacy principal", () => {
  const empty = { ...sharedObservations, trace: { ...sharedObservations.trace, events: [] } };
  for (const observations of [empty, { ...sharedObservations, state: "unavailable", reason: "Router read failed" }]) {
    const [agent] = rawAgent(observations);
    assert.match(agent.traceQuery, /^observations:/);
    assert.equal(new URLSearchParams(agent.traceQuery.slice("observations:".length)).get("producer"), routerProducerKey(producer));
    const html = render(RouterActivityPanel, { ...panelWithObservations(observations), focusQuery: agent.traceQuery });
    assert.match(html, /No selected records remain available|Router observations unavailable/);
  }
});

test("live observations cannot offer a completed download and stale terminal bindings cannot enter the graph", () => {
  const live = render(RouterActivityPanel, { ...routerProps, binding: { ...binding, provisional: true } });
  assert.match(live, /Live, provisional observations from Briefing writer/);
  assert.match(live, /Not a completion record or a downloadable deliverable/);
  assert.doesNotMatch(live, /href=|Download system evidence|Returned with this completed revision/);
  for (const runNonce of [undefined, "", "rev-other"]) {
    const html = render(ExecutionExplorer, { ...base, runNonce, routerActivity: binding });
    assert.doesNotMatch(html, /Briefing writer|Recorded producer|Router response/);
  }
});

test("system evidence never changes the useful deliverable review category", () => {
  const artifacts = (...names) => names.map(name => ({ name }));
  for (const [names, expected] of [
    [["briefing.md", systemFile], "docs"], [[systemFile], "output"], [[], "output"],
    [["real.json", systemFile], "code"], [["data.csv", "briefing.md", systemFile], "data"],
    [["main.rs", "data.csv", systemFile], "code"], [["report.MD", systemFile], "docs"],
  ]) assert.equal(reviewKind(artifacts(...names)), expected);
  assert.equal(reviewKind(undefined), "output");
});

test("router projection is shown only for the current successful durable terminal", () => {
  const result = {
    assignment_nonce: "rev-2", status: "ok", source: "durable_agent", reviewable: true, output: "Attached briefing.md",
    blocked: null, run_evidence: { status: "succeeded", run_nonce: "rev-2" },
  };
  const task = { current_run_nonce: "rev-2", result, assignment: null, router_activity: observed };
  assert.equal(missionRunState(task).routerActivity, observed);
  assert.equal(missionRunState(task).realDelivery, true);
  for (const changed of [
    { current_run_nonce: "rev-3" }, { current_run_nonce: null }, { result: null },
    ...[{ assignment_nonce: "rev-1" }, { status: "failed" }, { source: "single_turn" }, { run_evidence: null },
      { run_evidence: { status: "failed", run_nonce: "rev-2" } },
      { run_evidence: { status: "succeeded", run_nonce: "rev-1" } }].map(patch => ({ result: { ...result, ...patch } })),
    { result: { ...result, status: null }, assignment: { task_id: "rev-2", state: "Running", completed_at: null } },
    { router_activity: null }, { router_activity: undefined },
  ]) assert.equal(missionRunState({ ...task, ...changed }).routerActivity, null, JSON.stringify(changed));
});

test("bound router observations use the original graph and retain timelines and streaming pins", () => {
  traceCalls.length = 0;
  const html = render(ExecutionExplorer, { ...base, routerActivity: binding, assignmentEvents: [lifecycle], approvals: [approval] });
  assert.doesNotMatch(html, /Detailed trace unavailable/);
  assert.match(html, /Briefing writer/);
  assert.match(html, /Recorded producer/);
  assert.match(html, /Accepted this revision/);
  assert.match(html, /Release denied/);
  assert.equal(traceCalls[0][4], "rev-2");
  assert.equal(traceCalls[0][2], false);
  assert.ok(traceCalls.slice(1).every(args => args[2] === false));
  assert.match(render(ExecutionExplorer, { ...base, routerActivity: binding, activity: [round] }), /Drill-down/);
});

test("artifact list separates downloadable system evidence from output files and preserves legacy absence", () => {
  const { ArtifactsPanel } = load("app/workspace/missions/[name]/mission-detail-panels.tsx");
  const props = {
    ns: "kars-system", task: "briefing", runNonce: "rev-2", pullRequests: [], activity: [], egress: [], tokens: 22,
    artifacts: [{ name: "briefing.md", content: "# Useful briefing", size_bytes: 17 }, { name: systemFile, content: "{}", size_bytes: 2 }],
  };
  const html = render(ArtifactsPanel, { ...props, routerActivityState: "observed" });
  assert.match(html, /1 output file · 1 system evidence file/);
  assert.match(html, /System evidence · not a deliverable/);
  assert.match(html, /Partial router observations are shown in Activity/);
  assert.match(html, /<h1[^>]*>Useful briefing<\/h1>/);
  assert.match(html, /kars-router-observations.json\?run_nonce=rev-2/);
  assert.doesNotMatch(html, /Detailed revision-bound trace unavailable/);
  assert.match(render(ArtifactsPanel, { ...props, routerActivityState: "unavailable" }), /unavailable trace; see Activity for the reason/);
  assert.match(render(ArtifactsPanel, props), /Detailed revision-bound trace unavailable/);
  const unpinned = render(ArtifactsPanel, { ...props, runNonce: null });
  assert.match(unpinned, /Downloads unavailable: no current revision/);
  assert.doesNotMatch(unpinned, /href=/);
  const reversed = { ...props, artifacts: [...props.artifacts].reverse() };
  const reordered = render(ArtifactsPanel, reversed);
  assert.ok(reordered.indexOf("briefing.md") < reordered.indexOf(systemFile));
  const details = [...reordered.matchAll(/<details\b([^>]*)>([\s\S]*?)<\/details>/g)];
  assert.match(details[0][1], /open=""/);
  assert.match(details[0][2], /briefing.md/);
  assert.doesNotMatch(details[1][1], /open=/);
  assert.equal(reversed.artifacts[0].name, systemFile, "Rendering must not reorder input evidence");
  assert.doesNotMatch(render(ArtifactsPanel, { ...props, artifacts: [props.artifacts[1]] }), /<details[^>]*open=/);
});

test("retained halt history cannot hide the halt control or mislabel a resumed or delivered Mission", () => {
  const file = "app/workspace/missions/[name]/page.tsx";
  const page = ts.createSourceFile(file, readFileSync(resolve(sourceRoot, file), "utf8"),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let control, banner;
  const visit = node => {
    if (ts.isJsxExpression(node) && node.expression) {
      const text = node.expression.getText(page);
      if (text.includes("<HaltButton ")) control = node;
      if (text.startsWith("task.halted &&")) banner = node;
    }
    ts.forEachChild(node, visit);
  };
  visit(page);
  assert.ok(control && banner);
  const exports = {};
  const code = `export function Header({task, live, realDelivery}) {
    const ns = "kars-system", name = "mission";
    return <>${control.getText(page)}${banner.getText(page)}</>;
  }`;
  runInNewContext(ts.transpileModule(code, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText, {
    exports, require, HaltButton: () => React.createElement("button", null, "Halt agent"),
  });
  for (const launched of [true, false]) {
    for (const realDelivery of [true, false]) {
      const live = launched && !realDelivery;
      const html = render(exports.Header, { task: { launched, halted: "Prior interruption." }, live, realDelivery });
      assert.equal(html.includes("Halt agent"), live);
      assert.match(html, /Prior interruption\./);
      assert.match(html, launched || realDelivery ? /Previous halt/ : /Mission halted/);
      assert.match(html, /current execution status is shown above/);
    }
  }
  const fresh = render(exports.Header, { task: { launched: true, halted: null }, live: true, realDelivery: false });
  assert.match(fresh, /Halt agent/);
  assert.doesNotMatch(fresh, /Previous halt|Mission halted/);
});

test("Team run files distinguish absent attribution from recorded and inferred role ownership", () => {
  const { ArtifactSummary } = load("app/workspace/teams/[name]/runs/[run]/artifact-summary.tsx");
  const artifact = { name: "checklist.md", source_agent: null };
  assert.match(render(ArtifactSummary, { artifact }), /captured for this run · individual producer not recorded/);
  assert.doesNotMatch(render(ArtifactSummary, { artifact }), /from principal|producer unknown/);
  const role = { name: "writer", system_prompt: "Write" };
  assert.match(render(ArtifactSummary, { artifact: { ...artifact, source_agent: "writer" }, role }),
    /from writer · recorded producer/);
  assert.match(render(ArtifactSummary, { artifact: { ...artifact, name: "writer-checklist.md" }, role }),
    /possibly writer · inferred attribution/);
});
