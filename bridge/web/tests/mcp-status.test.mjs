// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import { mcpGuidance, mcpStatus } from "../src/app/console/mcp-status.ts";

const managed = {
  mode: "Managed", spec: { managed: { preset: "playwright" } },
  phase: "Degraded", status_current: true, status_reason: "ManagedMcpUnqualified",
  status_message: "Managed MCP initialize transport failure",
};

test("managed connection failures explain retries without hiding failure or promising recovery", () => {
  for (const status_message of [
    "Managed MCP initialize transport failure",
    "Managed MCP tools/list transport failure",
    "Managed MCP probe response transport failure",
    "Managed MCP probe exceeded its total deadline",
  ]) {
    const server = { ...managed, status_message };
    const guidance = mcpGuidance(server);
    assert.match(guidance.title, /Connection check failed.*automatic retries/);
    assert.match(guidance.detail, /configured to retry failed checks about once a minute/);
    assert.match(guidance.detail, /stays unavailable until verification passes/);
    assert.match(guidance.detail, /do not need to remove and re-add/);
    assert.match(guidance.detail, /If failures continue/);
    assert.equal(mcpStatus(server).tone, "danger");
    assert.equal(mcpStatus(server).ready, false);
  }
});

test("startup and missing credentials have different next steps", () => {
  const pending = { ...managed, phase: "Pending", status_reason: "ManagedMcpPending" };
  assert.match(mcpGuidance({ ...pending, status_message: "Waiting for the exact managed MCP Deployment generation to become available" }).title, /startup/);
  const missing = mcpGuidance({ ...pending, status_message: "The explicitly configured pull Secret is absent from the managed MCP namespace" });
  assert.match(missing.title, /Operator action needed/);
  assert.match(missing.detail, /waiting alone will not supply/);
});

test("configuration and protocol errors do not get described as transient startup failures", () => {
  for (const status_message of [
    "Managed MCP namespace ownership conflicts",
    "Managed MCP initialize negotiated an unsupported protocol",
    "Managed MCP probe HTTP 403",
    "Managed MCP image pull Secret is not a live dockerconfigjson Secret",
  ]) {
    const guidance = mcpGuidance({ ...managed, status_message });
    assert.equal(guidance.title, "Setup needs attention");
    assert.doesNotMatch(guidance.detail, /may still be starting/);
  }
});

test("external, stale and unverified reports never get managed retry guidance", () => {
  for (const override of [
    { mode: "External", spec: { url: "https://example.invalid/mcp" } },
    { mode: "Managed", spec: { url: "https://example.invalid/mcp" } },
    { status_current: false }, { status_current: undefined },
  ]) assert.equal(mcpGuidance({ ...managed, ...override }), null);
});

test("ready guidance explains the limit of tool discovery rather than claiming task acceptance", () => {
  const guidance = mcpGuidance({ ...managed, phase: "Ready" });
  assert.equal(guidance.title, "Connection verified");
  assert.match(guidance.detail, /does not yet verify an agent-driven tool task/);
  assert.doesNotMatch(guidance.detail, /failed|retry/);
});

test("pending installations show the controller's actual blocker", () => {
  const message = "The explicitly configured pull Secret is absent from the managed MCP namespace";
  assert.deepEqual(mcpStatus({
    phase: "Pending", status_current: true,
    status_reason: "ManagedMcpPending", status_message: message,
  }), {
    label: "Pending", tone: "warn", ready: false,
    reason: "ManagedMcpPending", detail: message,
  });
});

test("stale Ready reports and their old messages do not claim readiness", () => {
  const status = mcpStatus({
    phase: "Ready", status_current: false,
    status_reason: "Reconciled", status_message: "Old success",
  });
  assert.equal(status.label, "Reconciling");
  assert.equal(status.ready, false);
  assert.equal(status.reason, null);
  assert.match(status.detail, /Waiting for a current controller/);
  assert.doesNotMatch(status.detail, /Old success/);
});

test("an older BFF without diagnostic fields is explicitly unverified", () => {
  const status = mcpStatus({ phase: "Ready" });
  assert.equal(status.label, "Unverified");
  assert.equal(status.ready, false);
  assert.match(status.detail, /unavailable/);
});

test("current ready and degraded states retain their explanation and severity", () => {
  for (const [phase, tone, ready] of [["Ready", "ok", true], ["Degraded", "danger", false]]) {
    const status = mcpStatus({ phase, status_current: true, status_message: "Controller result" });
    assert.equal(status.label, phase);
    assert.equal(status.tone, tone);
    assert.equal(status.ready, ready);
    assert.equal(status.detail, "Controller result");
  }
});

test("missing phase or explanation does not invent successful installation", () => {
  const status = mcpStatus({ phase: null, status_current: true, status_message: "" });
  assert.equal(status.label, "Pending");
  assert.equal(status.ready, false);
  assert.match(status.detail, /not provided/);
});

test("the actual capabilities page renders blockers as text and hides stale tool proof", async () => {
  const require = createRequire(import.meta.url);
  const ts = require("typescript");
  const { renderToStaticMarkup } = require("react-dom/server");
  const page = readFileSync(new URL("../src/app/console/capabilities/page.tsx", import.meta.url), "utf8");
  const detailsContext = { exports: {}, require(name) {
    if (name === "react/jsx-runtime") return require(name);
    assert.equal(name, "./mcp-status");
    return { mcpStatus, mcpGuidance };
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL("../src/app/console/mcp-status-details.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, detailsContext);
  const server = {
    name: "playwright", namespace: "kars-system", spec: { managed: { preset: "playwright" } },
    phase: "Pending", status_current: true, status_reason: "ManagedMcpPending",
    status_message: "Missing pull Secret <untrusted-markup>",
    workload_ref: "kars-mcp/example", discovered_tools: ["browser_navigate"],
  };
  const empty = () => null;
  let refreshActive;
  let servers = [server];
  let profiles = [];
  const children = ({ children }) => children;
  const imports = {
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/lib/bff": {
      listMcpServers: async () => servers,
      listProfiles: async () => profiles, listSkills: async () => [], getOptions: async () => ({}),
    },
    "@/components/ui": { PageHeader: empty, Section: children, Badge: children },
    "@/components/honest-state": { HonestState: empty },
    "@/components/console-load-error": { ConsoleLoadError: () => assert.fail("Fixture reads must succeed") },
    "../configuration/credential-form": { CredentialForm: empty },
    "../author-resource": { AuthorResource: empty },
    "../profile-editor": { ProfileEditor: empty },
    "../delete-resource": { DeleteResource: empty },
    "../skill-approval": { SkillApproval: empty },
    "@/components/skill-composer": { SkillComposer: empty },
    "@/components/live-refresh": { LiveRefresh: ({ active }) => { refreshActive = active; return null; } },
    "../skill-submit-action": { submitSkillConsoleAction: empty },
    "../mcp-profiles": { McpProfiles: empty },
    "../mcp-catalog": { McpCatalog: empty },
    "../mcp-server-editor": { McpServerEditor: empty },
    "../mcp-status": { mcpStatus },
    "../mcp-status-details": detailsContext.exports,
    "@/components/icon": { Icon: empty },
  };
  const context = {
    exports: {},
    require(name) {
      assert.ok(Object.hasOwn(imports, name), `unexpected page dependency: ${name}`);
      return imports[name];
    },
  };
  runInNewContext(ts.transpileModule(page, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText, context);
  const render = async () => renderToStaticMarkup(await context.exports.default());
  const pending = await render();
  assert.equal(refreshActive, true);
  assert.match(pending, /Service status refreshes automatically while this page is visible/);
  assert.match(pending, /Controller details/);
  assert.match(pending, /ManagedMcpPending:/);
  assert.match(pending, /Missing pull Secret &lt;untrusted-markup&gt;/);
  assert.doesNotMatch(pending, /<untrusted-markup>|1 tools verified/);
  assert.match(pending, /workload reference kars-mcp\/example/);
  server.phase = "Ready";
  server.status_current = false;
  const stale = await render();
  assert.equal(refreshActive, true);
  assert.match(stale, /Reconciling/);
  assert.doesNotMatch(stale, /Missing pull Secret|1 tools verified/);
  server.status_current = true;
  server.status_message = "MCP server reconciled";
  assert.match(await render(), /1 tools verified/);
  assert.equal(refreshActive, true, "Ready services must still reveal outages without a reload");
  server.phase = "Degraded";
  server.status_reason = "ManagedMcpUnqualified";
  server.status_message = "Managed MCP initialize transport failure";
  const degraded = await render();
  assert.match(degraded, /Managed MCP initialize transport failure/);
  assert.match(degraded, /Connection check failed — automatic retries enabled/);
  assert.match(degraded, /role="status"/);
  assert.equal(refreshActive, true, "Transient probe failure must not stop recovery updates");
  server.phase = "Ready";
  server.status_reason = "Reconciled";
  server.status_message = "MCP server reconciled";
  const recovered = await render();
  assert.match(recovered, /Connection verified/);
  assert.doesNotMatch(recovered, /Connection check failed|automatic retries enabled/);
  servers = [];
  await render();
  assert.equal(refreshActive, false, "Stop service polling after deletion");
  profiles = [{ name: "review-profile", roles: [], phase: null, spec: {} }];
  const unknownProfile = await render();
  assert.match(unknownProfile, /Awaiting validation/);
  assert.doesNotMatch(unknownProfile, />ready</i);
  assert.equal(refreshActive, true);
  profiles[0].phase = "Degraded";
  assert.match(await render(), /Controller validation failed/);

  Object.assign(imports["@/lib/bff"], {
    listToolPolicies: async () => [], listInferencePolicies: async () => [], listEgress: async () => [],
  });
  Object.assign(imports, {
    "@/lib/session": { canAdminister: async () => false },
    "../inference-policy-editor": { InferencePolicyEditor: empty },
    "../policy-builder": { PolicyBuilder: empty },
    "@/components/inference-budgets": { InferenceBudgets: empty, InferenceBudgetEdit: empty },
    "@/components/retention-policy": { RetentionPolicyPanel: empty },
  });
  const policyContext = { exports: {}, require: context.require };
  runInNewContext(ts.transpileModule(readFileSync(new URL("../src/app/console/policies/page.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, policyContext);
  const renderPolicies = async () => renderToStaticMarkup(await policyContext.exports.default());
  server.allowed_tools = ["*"];
  server.phase = "Degraded";
  server.status_reason = "ManagedMcpUnqualified";
  server.status_message = "Managed MCP initialize transport failure";
  servers = [server];
  const policyStatus = await renderPolicies();
  assert.match(policyStatus, /Connection check failed — automatic retries enabled/);
  assert.match(policyStatus, /Managed MCP initialize transport failure/);
  assert.match(policyStatus, /Service status refreshes automatically/);
  assert.equal(refreshActive, true, "Policies must actually refresh when offering live status guidance");
  servers = [];
  await renderPolicies();
  assert.equal(refreshActive, false);
});
