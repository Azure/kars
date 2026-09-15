// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import { mcpStatus } from "../src/app/console/mcp-status.ts";

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
  const server = {
    name: "playwright", namespace: "kars-system", spec: { managed: { preset: "playwright" } },
    phase: "Pending", status_current: true, status_reason: "ManagedMcpPending",
    status_message: "Missing pull Secret <untrusted-markup>",
    workload_ref: "kars-mcp/example", discovered_tools: ["browser_navigate"],
  };
  const empty = () => null;
  const children = ({ children }) => children;
  const imports = {
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/lib/bff": {
      listMcpServers: async () => [server],
      listProfiles: async () => [], listSkills: async () => [], getOptions: async () => ({}),
    },
    "@/components/ui": { PageHeader: empty, Section: children, Badge: children },
    "@/components/honest-state": { HonestState: empty },
    "../configuration/credential-form": { CredentialForm: empty },
    "../author-resource": { AuthorResource: empty },
    "../profile-editor": { ProfileEditor: empty },
    "../delete-resource": { DeleteResource: empty },
    "../skill-approval": { SkillApproval: empty },
    "@/components/skill-composer": { SkillComposer: empty },
    "../skill-submit-action": { submitSkillConsoleAction: empty },
    "../mcp-profiles": { McpProfiles: empty },
    "../mcp-catalog": { McpCatalog: empty },
    "../mcp-server-editor": { McpServerEditor: empty },
    "../mcp-status": { mcpStatus },
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
  assert.match(pending, /ManagedMcpPending:/);
  assert.match(pending, /Missing pull Secret &lt;untrusted-markup&gt;/);
  assert.doesNotMatch(pending, /<untrusted-markup>|1 tools verified/);
  assert.match(pending, /workload reference kars-mcp\/example/);
  server.phase = "Ready";
  server.status_current = false;
  const stale = await render();
  assert.match(stale, /Reconciling/);
  assert.doesNotMatch(stale, /Missing pull Secret|1 tools verified/);
  server.status_current = true;
  server.status_message = "MCP server reconciled";
  assert.match(await render(), /1 tools verified/);
});
