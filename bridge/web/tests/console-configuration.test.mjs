// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const root = fileURLToPath(new URL("../src/", import.meta.url));
class BffError extends Error {
  constructor(code) {
    super("private upstream credential detail");
    this.code = code;
  }
}
const data = {
  getOptions: { provider: null, models: [], tool_policies: [], skills: [] },
  getFoundry: { connected: false },
  getIntegrations: { sre_present: false, headlamp_deployed: false },
  getGithubApp: { configured: false, storage_ready: false, slug: null, install_url: null },
  getLocalInferenceStatus: { available: false, gpu_node_count: 0, gpu_products: [] },
  getLocalInferenceCatalog: [],
  listLocalModelDeployments: [],
  listAdditionalProviders: [],
  getComponentModels: { cluster_default: null, components: [] },
  listSkills: [],
  listProfiles: [],
  listMcpServers: [],
  listApprovals: [],
  listEgress: [],
  listToolPolicies: [],
  listInferencePolicies: [],
};
let headlampSso = false;
const apiNames = Object.keys(data);
const pages = [
  ["configuration", "Configuration", apiNames.slice(0, 9)],
  ["capabilities", "Agent capabilities", ["getOptions", "listSkills", "listProfiles", "listMcpServers"]],
  ["approvals", "Approvals", ["listEgress", "listApprovals"]],
];

test("credential guidance distinguishes Console write-only access from runtime tool exposure", async () => {
  const Page = load("app/console/capabilities/page.tsx").default;
  const html = renderToStaticMarkup(await Page());
  assert.match(html, /Write-only in the Console/);
  assert.match(html, /Runtime access is not model isolation/);
  assert.match(html, /environment can expose these values in their output/);
  assert.match(html, /Kubernetes Secret access remains governed by cluster permissions/);
  assert.doesNotMatch(html, /Never seen by the model|there is no read path|&lt;name&gt;-credentials/);
});

function load(file, failing, error = new BffError("upstream_error"), calls = []) {
  const exports = {};
  const localRequire = name => {
    if (name === "@/lib/session") return { currentPrincipal: async () => ({ name: "reviewer" }), canAdminister: async () => true };
    if (name === "@/lib/config") return { defaultNamespace: () => "kars-system", headlampSsoEnabled: () => headlampSso };
    if (name === "@/lib/headlamp") return load("lib/headlamp.ts");
    if (name === "next/navigation") return { useRouter: () => ({ refresh: () => assert.fail("No mutation during rendering") }) };
    if (name === "@/app/inbox/approval-actions") return { decide: () => assert.fail("No decision during rendering") };
    if (name === "@/lib/bff") return {
      BffError,
      ...Object.fromEntries(apiNames.map(method => [method, async (params) => {
        calls.push(method);
        if (method === failing || (Array.isArray(failing) && failing.includes(method))
          || (typeof failing === "function" && failing(method, params))) throw error;
        return data[method];
      }])),
    };
    if (name === "@/components/console-load-error") {
      return load("components/console-load-error.tsx", failing, error, calls);
    }
    if (name === "../operator-github-status") {
      const { OperatorGithubStatus } = load("app/console/operator-github-status.tsx");
      return { OperatorGithubStatus: props => React.createElement("div", {
        "data-component": "OperatorGithubStatus",
      }, React.createElement(OperatorGithubStatus, props)) };
    }
    if (name === "./configuration/github-app-actions") return {
      putGithubAppAction: () => assert.fail("No GitHub setup during rendering"),
      startGithubAppRegistrationAction: () => assert.fail("No GitHub registration during rendering"),
      disconnectGithubAppAction: () => assert.fail("No GitHub disconnect during rendering"),
    };
    if (name === "./provider-wizard") return {
      DefaultProviderCard: load("app/console/configuration/provider-wizard.tsx").DefaultProviderCard,
      ProviderWizard: () => React.createElement("div", { "data-component": "ProviderWizard" }),
    };
    if (name === "@/components/ui") return {
      PageHeader: ({ title }) => React.createElement("h1", null, title),
      Section: ({ title, children }) => React.createElement("section", null,
        React.createElement("h2", null, title), children),
      Badge: ({ children, tone }) => React.createElement("span", { "data-tone": tone }, children),
    };
    if (name.startsWith("@/") || name.startsWith(".")) {
      return new Proxy({}, { get: (_, component) => ({ children }) =>
        React.createElement("div", { "data-component": component }, children) });
    }
    return require(name);
  };
  const compiled = ts.transpileModule(readFileSync(`${root}${file}`, "utf8"), {
    fileName: file,
    reportDiagnostics: true,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  assert.equal(compiled.diagnostics?.length ?? 0, 0, JSON.stringify(compiled.diagnostics));
  runInNewContext(compiled.outputText, { exports, require: localRequire, URL });
  return exports;
}

for (const [page, title, reads] of pages) {
  for (const failedRead of reads) {
    test(`${page}: ${failedRead} failure cannot masquerade as missing configuration`, async () => {
      const calls = [];
      const Page = load(`app/console/${page}/page.tsx`, failedRead, undefined, calls).default;
      const html = renderToStaticMarkup(await Page());
      assert.match(html, /role="alert"/);
      assert.ok(html.includes(title));
      assert.match(html, /upstream_error/);
      assert.match(html, page === "approvals"
        ? /does not mean no requests are pending/
        : /does not mean nothing is configured/);
      assert.match(html, /href="\/console\/troubleshooting"/);
      assert.doesNotMatch(html, /private upstream/);
      if (page === "configuration") {
        const wizardAvailable = ["getGithubApp", "getIntegrations", "getLocalInferenceCatalog", "getComponentModels"].includes(failedRead);
        assert.equal(html.includes('data-component="ProviderWizard"'), wizardAvailable);
        assert.equal(html.includes('data-component="OperatorGithubStatus"'), failedRead !== "getGithubApp");
        assert.equal((html.match(/<h1>/g) ?? []).length, 1);
      } else {
        assert.doesNotMatch(html, /data-component="(?:SkillComposer|ProfileEditor|McpServerEditor|CredentialForm)"/);
      }
      assert.deepEqual(calls, page === "configuration" && failedRead === "getOptions"
        ? [...reads, "getOptions"] : reads);
    });
  }
  test(`${page}: a successful retry restores controls`, async () => {
    const Page = load(`app/console/${page}/page.tsx`).default;
    const html = renderToStaticMarkup(await Page());
    assert.doesNotMatch(html, /role="alert"/);
    assert.match(html, page === "configuration"
      ? /data-component="ProviderWizard"/
      : page === "capabilities" ? /data-component="SkillComposer"/ : /Workload decisions/);
  });

  test("Console checkpoints expose written-feedback controls and exclude clarification requests", async () => {
    const saved = data.listApprovals;
    data.listApprovals = [
      { name: "checkpoint", task: "a-long-task", action_kind: "checkpoint", team: "team-a", milestone: "review",
        summary: "Review the retained result", detail: "Exact revision detail", phase: "Pending", actionable: true,
        resource_version: "12", bound_envelope_digest: "sha256:bound" },
      { name: "question", task: "a-long-task", action_kind: "clarification", summary: "Private clarification", actionable: true },
    ];
    try {
      const Page = load("app/console/approvals/page.tsx").default;
      const html = renderToStaticMarkup(await Page());
      assert.match(html, /Review the retained result/);
      assert.match(html, /<summary[^>]*>Request details<\/summary>/);
      assert.doesNotMatch(html, /Private clarification|Those never appear here|only approval/);
      const { ApprovalDecision } = load("components/approval-decision.tsx");
      const controls = renderToStaticMarkup(React.createElement(ApprovalDecision, {
        name: "checkpoint", decider: "reviewer", authWired: true, resourceVersion: "12",
        boundEnvelopeDigest: "sha256:bound", compact: true, requireDenyReason: true,
      }));
      assert.match(controls, /<input/);
      assert.match(controls, /Written feedback is required/);
      assert.match(controls, /<button[^>]*disabled=""[^>]*>Deny<\/button>/);
      assert.doesNotMatch(controls, /<button[^>]*disabled=""[^>]*>Approve<\/button>/);
      assert.match(readFileSync(`${root}app/console/approvals/page.tsx`, "utf8"),
        /requireDenyReason=\{approval\.action_kind === "checkpoint" && !!approval\.team && !!approval\.milestone\}/);
    } finally {
      data.listApprovals = saved;
    }
  });
}

test("policy model-discovery failures retain inventory and budget controls without offering empty route choices", async () => {
  const saved = data.listInferencePolicies;
  data.listInferencePolicies = [{ name: "existing-policy", namespace: "kars-system", spec: {} }];
  try {
    const Page = load("app/console/policies/page.tsx", "getOptions").default;
    const html = renderToStaticMarkup(await Page());
    assert.match(html, /role="alert"/);
    assert.match(html, /Could not load model catalogue \(upstream_error\)/);
    assert.match(html, /does not mean no providers are configured/);
    assert.match(html, /existing-policy/);
    assert.match(html, /data-component="InferenceBudgetEdit"/);
    assert.doesNotMatch(html, /data-component="InferencePolicyEditor"|private upstream/);
    assert.match(html, /href="\/console\/troubleshooting"/);
  } finally {
    data.listInferencePolicies = saved;
  }
});

test("successful policy discovery, including an empty catalogue, restores route editors", async () => {
  const saved = data.getOptions;
  try {
    for (const models of [[], [{ provider: "github-copilot", deployment: "claude-opus-5.5" }]]) {
      data.getOptions = { ...saved, models };
      const Page = load("app/console/policies/page.tsx").default;
      const html = renderToStaticMarkup(await Page());
      assert.doesNotMatch(html, /role="alert"/);
      assert.match(html, /data-component="InferencePolicyEditor"/);
    }
  } finally {
    data.getOptions = saved;
  }
});

test("GitHub setup requires enrolled storage before rendering any credential controls", async () => {
  const saved = data.getGithubApp;
  try {
    for (const storage_ready of [false, undefined, true]) {
      data.getGithubApp = { ...saved, storage_ready };
      const Page = load("app/console/configuration/page.tsx").default;
      const html = renderToStaticMarkup(await Page());
      if (storage_ready) {
        assert.match(html, /name="app_id"/);
        assert.match(html, /type="file"[^>]*name="private_key_file"/);
        assert.doesNotMatch(html, /<textarea|name="private_key"|BEGIN RSA PRIVATE KEY/);
        assert.match(html, /Register a new GitHub App/);
        assert.match(html, /Prepare GitHub registration/);
        assert.match(html, /Only its owner \(private App\)/);
        assert.match(html, /Verify &amp; connect/);
        assert.doesNotMatch(html, /storage requires operator enrollment/);
      } else {
        assert.match(html, /storage requires operator enrollment/);
        assert.match(html, /kars-github-app/);
        assert.match(html, /purpose <code[^>]*>github-app/);
        assert.match(html, /kars credentials grant bootstrap-store/);
        assert.match(html, /workspaceEnrollment.enabled=false/);
        assert.match(html, /preserve all existing grant authority/);
        assert.doesNotMatch(html, /name="(?:app_id|private_key_file)"|Verify &amp; connect|Prepare GitHub registration/);
      }
    }
    data.getGithubApp = { ...saved, storage_ready: true, configured: true, slug: "kars-app" };
    const Page = load("app/console/configuration/page.tsx").default;
    const html = renderToStaticMarkup(await Page());
    assert.match(html, /Shared GitHub App configured/);
    assert.match(html, /kars-app/);
    assert.match(html, /Disconnect/);
    assert.doesNotMatch(html, /name="(?:app_id|private_key_file)"|Verify &amp; connect|Prepare GitHub registration/);
  } finally {
    data.getGithubApp = saved;
  }
});

test("GitHub callback messages are canned and never substitute for current configuration", async () => {
  const saved = data.getGithubApp;
  data.getGithubApp = { ...saved, configured: false, storage_ready: true };
  try {
    const Page = load("app/console/configuration/page.tsx").default;
    const render = githubApp => Page({ searchParams: Promise.resolve({ githubApp }) }).then(renderToStaticMarkup);
    const returned = await render("connected");
    assert.match(returned, /GitHub registration returned/);
    assert.match(returned, /GitHub App not configured/);
    assert.doesNotMatch(returned, /Shared GitHub App configured/);
    assert.match(await render("conversion-failed"), /code may already be consumed/);
    assert.match(await render("configuration-changed"), /did not replace newer state/);
    for (const input of ["private callback payload", "constructor", "__proto__", ["connected", "conversion-failed"]]) {
      const html = await render(input);
      assert.doesNotMatch(html, /private callback payload|GitHub registration returned|code may already be consumed/);
      assert.match(html, /GitHub App not configured/);
    }
    const failedPage = load("app/console/configuration/page.tsx", "getGithubApp").default;
    const failed = renderToStaticMarkup(await failedPage({ searchParams: Promise.resolve({ githubApp: "connected" }) }));
    assert.match(failed, /Could not load github app/);
    assert.doesNotMatch(failed, /Shared GitHub App configured/);
  } finally {
    data.getGithubApp = saved;
  }
});

test("credential enrollment and transport failures stay actionable and redacted", async () => {
  for (const error of [new BffError("credential_authority_unavailable"), new Error("private upstream credential detail")]) {
    const Page = load("app/console/configuration/page.tsx", "getGithubApp", error).default;
    const html = renderToStaticMarkup(await Page());
    assert.doesNotMatch(html, /private upstream credential detail/);
    assert.match(html, error instanceof BffError ? /operator must enroll or repair/ : /connection_failed/);
  }
});

test("a stale default keeps Configuration's catalogue available for replacement without hiding the error", async () => {
  const calls = [];
  const Page = load("app/console/configuration/page.tsx", (method, params) => {
    if (method !== "getOptions") return false;
    return params?.catalogueOnly !== true;
  }, new BffError("invalid_request"), calls).default;
  const html = renderToStaticMarkup(await Page());
  assert.match(html, /role="alert"/);
  assert.match(html, /data-component="ModelCatalogue"/);
  assert.doesNotMatch(html, /data-component="ProviderWizard"/);
  assert.equal(calls.filter(call => call === "getOptions").length, 2);
  assert.doesNotMatch(html, /private upstream/);
});

test("independent credential failures retain the verified default provider and catalogue", async () => {
  const saved = data.getOptions;
  data.getOptions = {
    ...saved,
    provider: { id: "azure-foundry", label: "Azure AI Foundry", note: "Verified cluster provider." },
    models: [{ provider: "azure-foundry", deployment: "gpt-5.4-mini", is_default: true }],
  };
  try {
    const Page = load("app/console/configuration/page.tsx",
      ["getGithubApp", "listAdditionalProviders"], new BffError("credential_authority_unavailable")).default;
    const html = renderToStaticMarkup(await Page());
    assert.match(html, /Azure AI Foundry/);
    assert.match(html, /gpt-5\.4-mini/);
    assert.match(html, /Could not load additional providers/);
    assert.match(html, /Could not load github app/);
    assert.match(html, /data-component="ModelCatalogue"/);
    assert.match(html, /<h2>Integrations<\/h2>/);
    assert.doesNotMatch(html, /Could not load configuration|data-component="(?:ProviderWizard|OperatorGithubStatus)"/);
    assert.doesNotMatch(html, /private upstream|Edit models/);
    assert.equal((html.match(/<h1>/g) ?? []).length, 1);
  } finally {
    data.getOptions = saved;
  }
});

test("Headlamp links include the cluster route and retain explicit cluster and mount prefixes", () => {
  const { headlampLink } = load("lib/headlamp.ts");
  for (const [base, expected] of [
    ["http://localhost:3904", "http://localhost:3904/c/main/kars/sre"],
    ["http://localhost:3904/", "http://localhost:3904/c/main/kars/sre"],
    ["https://example.test/headlamp/", "https://example.test/headlamp/c/main/kars/sre"],
    ["https://example.test/headlamp/c/other/", "https://example.test/headlamp/c/other/kars/sre"],
    ["https://example.test/c/my%20cluster", "https://example.test/c/my%20cluster/kars/sre"],
  ]) assert.equal(headlampLink(base, "/kars/sre"), expected);
  assert.throws(() => headlampLink("javascript:alert(1)", "/kars/sre"), /HTTP or HTTPS/);
});

test("Headlamp SSO enters its own authorization flow with a same-cluster return route", () => {
  const { headlampLink } = load("lib/headlamp.ts");
  for (const [base, route, expected] of [
    ["http://localhost:3905", "/", "http://localhost:3905/oidc?cluster=main&return=%2F"],
    ["https://example.test/headlamp/", "/kars/sre", "https://example.test/headlamp/oidc?cluster=main&return=%2Fkars%2Fsre"],
    ["https://example.test/headlamp/c/other/", "/kars/mesh", "https://example.test/headlamp/oidc?cluster=other&return=%2Fkars%2Fmesh"],
    ["https://example.test/c/my%20cluster?old=1#old", "/", "https://example.test/oidc?cluster=my+cluster&return=%2F"],
  ]) assert.equal(headlampLink(base, route, true), expected);
  for (const route of ["https://evil.test", "//evil.test", "/../auth", "/%2e%2e/auth", "/kars/sre?token=secret", "/kars\\sre", "/kars/sre\n"]) {
    assert.throws(() => headlampLink("https://example.test", route, true), /cluster-relative/);
  }
  assert.throws(() => headlampLink("javascript:alert(1)", "/", true), /HTTP or HTTPS/);
});

test("Configuration SSO links preserve each requested view without sharing Bridge credentials", async () => {
  const saved = data.getIntegrations;
  headlampSso = true;
  data.getIntegrations = {
    sre_present: true, sre_phase: "Ready", sre_ready: "1/1",
    headlamp_deployed: true, headlamp_url: "https://example.test/headlamp",
    headlamp_paths: [{ label: "Agent mesh", path: "/kars/mesh" }],
  };
  try {
    const Page = load("app/console/configuration/page.tsx").default;
    const html = renderToStaticMarkup(await Page());
    assert.ok(html.includes('href="https://example.test/headlamp/oidc?cluster=main&amp;return=%2Fkars%2Fmesh"'));
    assert.ok(html.includes('href="https://example.test/headlamp/oidc?cluster=main&amp;return=%2Fkars%2Fsre"'));
    assert.doesNotMatch(html, /access_token=|id_token=|session_token=/);
  } finally {
    data.getIntegrations = saved;
    headlampSso = false;
  }
});

test("Configuration renders working Headlamp paths for every integration link", async () => {
  const saved = data.getIntegrations;
  data.getIntegrations = {
    sre_present: true, sre_phase: "Ready", sre_ready: "1/1",
    headlamp_deployed: true, headlamp_url: "http://localhost:3904",
    headlamp_paths: [
      { label: "SRE console", path: "/kars/sre" },
      { label: "Sandboxes", path: "/kars/karssandboxes" },
      { label: "Agent mesh", path: "/kars/mesh" },
    ],
  };
  try {
    const Page = load("app/console/configuration/page.tsx").default;
    const html = renderToStaticMarkup(await Page());
    for (const { path } of data.getIntegrations.headlamp_paths) {
      assert.ok(html.includes(`href="http://localhost:3904/c/main${path}"`), path);
    }
    assert.doesNotMatch(html, /href="http:\/\/localhost:3904\/kars\//);
    assert.equal((html.match(/href="http:\/\/localhost:3904\/c\/main\/kars\/sre"/g) ?? []).length, 2);
  } finally {
    data.getIntegrations = saved;
  }
});

test("SRE presence and partial or unknown readiness never become a healthy running claim", async () => {
  const saved = data.getIntegrations;
  try {
    for (const [phase, ready, healthy] of [
      ["Degraded", "2/2", false], ["Migrating", "1/2", false],
      ["Running", "1/2", false], ["Ready", null, false],
      ["Ready", "0/0", false], ["Ready", "2/1", false],
      [null, "2/2", false], ["Ready", "invalid", false],
      ["Ready", "2/2", true], ["Running", "1/1", true],
    ]) {
      data.getIntegrations = { ...saved, sre_present: true, sre_phase: phase, sre_ready: ready };
      const Page = load("app/console/configuration/page.tsx").default;
      const html = renderToStaticMarkup(await Page());
      assert.match(html, healthy ? /SRE sandbox reports ready/ : /SRE sandbox is present but not ready/);
      assert.match(html, new RegExp(`data-tone="${healthy ? "ok" : "warn"}"`));
      assert.match(html, /Readiness alone does not verify a completed diagnosis/);
      assert.doesNotMatch(html, /SRE sandbox is running|it triages cluster health/);
    }
  } finally {
    data.getIntegrations = saved;
  }
});

test("provider connection rejects malformed URLs, credentials and invalid tags", () => {
  const { providerEndpointError, providerTagError } = load("app/console/configuration/provider-wizard.tsx");
  for (const endpoint of ["", "not-a-url", "/relative", "file:///tmp/model", "javascript:alert(1)", "https://user:secret@api.example.com"]) {
    assert.equal(typeof providerEndpointError(endpoint), "string", endpoint);
  }
  for (const endpoint of ["https://api.example.com/v1", "http://model.namespace.svc.cluster.local:8000/v1", " https://api.example.com "]) {
    assert.equal(providerEndpointError(endpoint), null, endpoint);
  }
  for (const tag of ["", "UPPER", "has space", "-prefix", "suffix-", "x".repeat(64)]) {
    assert.equal(typeof providerTagError(tag), "string", tag);
  }
  for (const tag of ["custom", "foundry-eu", "a", "x".repeat(63)]) {
    assert.equal(providerTagError(tag), null, tag);
  }
});
