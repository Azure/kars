// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const { jsx, jsxs } = require("react/jsx-runtime");
const { renderToStaticMarkup } = require("react-dom/server");

function load(path, imports) {
  const context = { exports: {}, URL, require(name) {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}

const common = {
  "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/icon": { Icon: () => null },
};
common["@/components/honest-state"] = load("../src/components/honest-state.tsx", common);
common["@/lib/format"] = load("../src/lib/format.ts", {});
common["@/lib/headlamp"] = load("../src/lib/headlamp.ts", {});

async function render({
  present = false, inventoryFails = false, actionsFail = false, actions = [], fleet = false,
  insightsFail = false, egressFail = false, orchestratorFail = false,
  orchestrator = null, report = { state: "missing", latest: null }, reportFails = false,
  headlamp = null, headlampSso = false,
} = {}) {
  const imports = {
    ...common,
    "@/components/navigation-link": { default: ({ href, children, ...props }) => jsx("a", { href, children, ...props }) },
    "@/lib/config": { headlampUrl: () => headlamp, headlampSsoEnabled: () => headlampSso },
    "@/components/ui": {
      PageHeader: ({ title, lead }) => jsxs("header", { children: [jsx("h1", { children: title }), jsx("p", { children: lead })] }),
      Section: ({ title, children }) => jsxs("section", { children: [jsx("h2", { children: title }), children] }),
      Badge: ({ children }) => children,
    },
    "../sre-action-decision": { SreActionDecision: () => null },
    "@/lib/bff": {
      listSandboxes: async () => {
        if (inventoryFails) throw new Error("private inventory detail");
        return present ? [{ name: "sre", namespace: "kars-system", phase: "Pending" }] : [];
      },
      getSreReport: async () => {
        if (reportFails) throw new Error("private report transport detail");
        return report;
      },
      listSreActions: async () => {
        if (actionsFail) throw new Error("private proposal detail");
        return actions;
      },
      getInsights: async () => {
        if (insightsFail) throw new Error("private insights detail");
        return { amplification_rejections: 0, receipts_issued: 0 };
      },
      listEgress: async () => {
        if (egressFail) throw new Error("private egress detail");
        return [];
      },
      getOrchestrator: async () => {
        if (orchestratorFail) throw new Error("private orchestrator detail");
        return orchestrator;
      },
    },
  };
  const path = fleet ? "../src/app/console/page.tsx" : "../src/app/console/sre-actions/page.tsx";
  return renderToStaticMarkup(await load(path, imports).default());
}

test("Fleet Health uses Headlamp top-level OIDC only when SSO is configured", async () => {
  const headlamp = "https://example.test/headlamp";
  const sso = await render({ fleet: true, headlamp, headlampSso: true });
  assert.ok(sso.includes('href="https://example.test/headlamp/oidc?cluster=main&amp;return=%2F"'));
  const legacy = await render({ fleet: true, headlamp });
  assert.ok(legacy.includes('href="https://example.test/headlamp"'));
  assert.doesNotMatch(legacy, /\/oidc\?/);
});

test("missing SRE is not presented as a healthy idle integration", async () => {
  const html = await render();
  assert.match(html, /SRE agent not installed/);
  assert.doesNotMatch(html, /legitimate idle|not a missing integration/);
});

test("failed inventory remains unknown instead of absent or idle", async () => {
  const html = await render({ inventoryFails: true });
  assert.match(html, /SRE status unavailable/);
  assert.doesNotMatch(html, /SRE agent not installed|legitimate idle|private inventory detail/);
});

test("a present SRE with no proposals reports its actual phase without claiming working diagnosis", async () => {
  const html = await render({ present: true });
  assert.match(html, /No SRE proposals/);
  assert.match(html, /Pending/);
  assert.doesNotMatch(html, /legitimate idle|not a missing integration/);
});

test("failed proposal reads do not become an empty history", async () => {
  const html = await render({ present: true, actionsFail: true });
  assert.match(html, /SRE proposal history unavailable/);
  assert.doesNotMatch(html, /No SRE proposals|private proposal detail/);
});

test("historical proposals remain visible when the SRE sandbox is absent", async () => {
  const html = await render({ actions: [{
    name: "retained-action", namespace: "kars-system", phase: "Applied",
    approval_state: "Approved", actionable: false, action_type: "Scale",
    diagnosis: "saved diagnosis", rationale: "saved rationale",
  }] });
  assert.match(html, /SRE agent not installed/);
  assert.match(html, /saved diagnosis/);
  assert.match(html, /Remediation proposals/);
});

function proposal(params, action_type = "ScaleDeployment") {
  return {
    name: "sre-action-review", namespace: "sre-operator", phase: "Proposed",
    approval_state: "Pending", actionable: true, action_type, params,
    target_namespace: "customer-workloads", target_name: "service-api",
    diagnosis: "Model diagnosis is not the action specification", rationale: "Review the exact parameters",
  };
}

for (const replicas of [0, 1, 50, null]) {
  test(`SRE review exposes persisted replica count ${replicas} without inventing defaults`, async () => {
    const html = await render({ actions: [proposal({ namespace: "customer-workloads", name: "service-api", replicas })] });
    assert.match(html, /sre-operator\/sre-action-review/);
    assert.match(html, /Proposed action parameters/);
    assert.match(html, new RegExp(`&quot;replicas&quot;: ${replicas}`));
  });
}

for (const actionType of ["PatchDeploymentImage", "RolloutRestart", "DeleteResourceQuota", "DeletePod"]) {
  test(`SRE review preserves all ${actionType} parameters separately from model prose`, async () => {
    const params = { namespace: "customer-workloads", name: "service-api" };
    if (actionType === "PatchDeploymentImage") {
      params.container = "api";
      params.image = "registry.example.invalid/service@sha256:" + "a".repeat(64);
    }
    const html = await render({ actions: [proposal(params, actionType)] });
    const escape = (s) => s.replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
    assert.ok(html.includes(escape(JSON.stringify(params, null, 2))));
    assert.match(html, /Model diagnosis is not the action specification/);
  });
}

test("missing SRE parameters remain unavailable rather than fabricated", async () => {
  const html = await render({ actions: [proposal(undefined)] });
  assert.match(html, /Action parameters unavailable/);
  assert.doesNotMatch(html, /&quot;replicas&quot;/);
});

test("historical null SRE parameters remain visible without becoming an empty object", async () => {
  const html = await render({ actions: [{ ...proposal(null), actionable: false, approval_state: "Rejected", phase: "Rejected" }] });
  assert.match(html, /<pre[^>]*>null<\/pre>/);
});

test("SRE action parameters are rendered as escaped text, not HTML", async () => {
  const html = await render({ actions: [proposal({ image: "<script>execute()</script>", replicas: false })] });
  assert.match(html, /&lt;script&gt;execute\(\)&lt;\/script&gt;/);
  assert.match(html, /&quot;replicas&quot;: false/);
  assert.doesNotMatch(html, /<script\b/i);
});

test("SRE review describes short-lived scoped authority without promising one-use credentials", async () => {
  const html = await render();
  assert.match(html, /short-lived, narrowly scoped token/);
  assert.match(html, /single execution attempt/);
  assert.doesNotMatch(html, /one-shot|one-use/);
});

test("Fleet Health distinguishes missing SRE from no new proposals", async () => {
  assert.match(await render({ fleet: true }), /SRE agent not installed/);
});

test("Fleet Health preserves failed SRE history as unavailable", async () => {
  const html = await render({ fleet: true, actionsFail: true });
  assert.match(html, /SRE proposal history unavailable/);
  assert.doesNotMatch(html, /No remediation proposals yet|private proposal detail/);
});

test("Fleet Health does not invent zero audit counters when insights fail", async () => {
  const html = await render({ fleet: true, insightsFail: true });
  assert.match(html, /—<\/p><p[^>]*>Amp\. blocks<\/p>/);
  assert.match(html, /—<\/p><p[^>]*>Receipts issued<\/p>/);
  assert.match(html, /Insights unavailable/);
  assert.doesNotMatch(html, /private insights detail/);
});

test("Fleet Health distinguishes unavailable egress grants from an empty inventory", async () => {
  const html = await render({ fleet: true, egressFail: true });
  assert.match(html, /—<\/p><p[^>]*>Egress grants<\/p>/);
  assert.match(html, /Egress inventory unavailable/);
  assert.doesNotMatch(html, /private egress detail/);
});

test("Fleet Health keeps the orchestrator outage visible", async () => {
  const html = await render({ fleet: true, orchestratorFail: true });
  assert.match(html, /Orchestrator status unavailable/);
  assert.doesNotMatch(html, /private orchestrator detail/);
});

test("Fleet Health renders genuine zero counters after reads recover", async () => {
  const failed = await render({ fleet: true, insightsFail: true, egressFail: true });
  assert.match(failed, /Insights unavailable/);
  assert.match(failed, /Egress inventory unavailable/);
  const recovered = await render({ fleet: true });
  for (const label of ["Amp\\. blocks", "Receipts issued", "Egress grants"]) {
    assert.match(recovered, new RegExp(`>0</p><p[^>]*>${label}</p>`));
  }
  assert.doesNotMatch(recovered, /Insights unavailable|Egress inventory unavailable/);
});

test("Fleet Health does not infer cluster health from a Pending sandbox", async () => {
  const html = await render({ fleet: true, present: true });
  assert.match(html, /Pending/);
  assert.match(html, /No sandboxes currently report Degraded or Failed/);
  assert.doesNotMatch(html, /Cluster is healthy/);
});

test("Fleet Health restores the known orchestrator routing after an outage", async () => {
  assert.match(await render({ fleet: true, orchestratorFail: true }), /Orchestrator status unavailable/);
  const html = await render({ fleet: true, orchestrator: {
    mode: "sandbox", direct_configured: false, sandbox_present: true,
    sandbox_phase: "Running", sandbox_ready: "2/2", sandbox_restarts: 0,
    sandbox_waiting_reason: null, router_candidates: 1, recommend_direct: false,
    note: "Runtime Pod ready; tool execution unverified.",
  } });
  assert.match(html, /Orchestrator \(compose engine\)/);
  assert.match(html, /Sandbox router/);
  assert.match(html, /2\/2/);
  assert.match(html, /1 path/);
  assert.doesNotMatch(html, /Orchestrator status unavailable/);
});

test("orchestrator details expose actual node, uptime and zero restarts without claiming inference", async () => {
  const html = await render({ fleet: true, orchestrator: {
    mode: "sandbox", direct_configured: false, sandbox_present: true,
    sandbox_phase: "Running", sandbox_ready: "2/2", sandbox_restarts: 0,
    sandbox_waiting_reason: null, sandbox_node: "kind-control-plane",
    sandbox_uptime_seconds: 125, router_candidates: 1, recommend_direct: false,
    note: "Pod readiness does not verify successful composition.",
  } });
  assert.match(html, /kind-control-plane/);
  assert.match(html, /Restarts<\/dt><dd[^>]*>0<\/dd>/);
  assert.match(html, /Uptime<\/dt><dd[^>]*>2m<\/dd>/);
  assert.match(html, /Router candidates/);
  assert.match(html, /Configuration &amp; pod status/);
  assert.doesNotMatch(html, /Router fallbacks|means redundancy|healthy orchestrator/);
});

test("older BFF details remain unknown during an upgrade and direct configuration is not green health", async () => {
  const html = await render({ fleet: true, orchestrator: {
    mode: "direct", direct_configured: true, sandbox_present: false,
    sandbox_phase: null, sandbox_ready: null, sandbox_restarts: null,
    sandbox_waiting_reason: null, router_candidates: 0, recommend_direct: false,
    note: "Direct endpoint configured; inference not verified.",
  } });
  assert.match(html, /Configured direct/);
  assert.match(html, /Node<\/dt><dd[^>]*>—<\/dd>/);
  assert.match(html, /Uptime<\/dt><dd[^>]*>—<\/dd>/);
  assert.doesNotMatch(html, /bg-emerald-500\/15 text-emerald-600/);
});

function savedReport(state = "observed", turn_status = "blocked", text = "Read denied; coverage unknown") {
  return { state, latest: {
    accepted_at_ms: 1710000001000,
    report: { turn_status, text, finished_at_ms: 1710000000000, truncated: false },
  } };
}

for (const status of ["blocked", "failed", "timeout"]) {
  test(`automatic ${status} reports preserve incomplete coverage without approving a remedy`, async () => {
    const html = await render({ present: true, report: savedReport("observed", status) });
    assert.match(html, new RegExp(`Turn ${status} — diagnostic coverage incomplete`));
    assert.match(html, /Read denied; coverage unknown/);
    assert.match(html, /Reports do not approve remedies or grant inference capacity/);
    assert.match(html, /No SRE proposals/);
  });
}

test("a finished report is not a healthy cluster or accepted remediation", async () => {
  const html = await render({ report: savedReport("observed", "finished", "Investigate node memory pressure") });
  assert.match(html, /Finished turn — health unverified/);
  assert.match(html, /Investigate node memory pressure/);
  assert.doesNotMatch(html, /Cluster is healthy|Remediation applied/);
});

test("stale report evidence remains explicitly historical and unknown", async () => {
  const html = await render({ report: savedReport("stale") });
  assert.match(html, /SRE report stale — current health unknown/);
  assert.match(html, /Read denied; coverage unknown/);
  assert.match(html, /Persisted/);
});

for (const [state, title] of [
  ["missing", "No persisted SRE report yet"], ["revoked", "SRE report source retired or disabled"],
  ["replaced", "SRE report source identity changed"], ["invalid", "SRE report could not be validated"],
  ["unavailable", "SRE report unavailable"],
]) {
  test(`${state} report does not display untrusted saved text`, async () => {
    const html = await render({ report: savedReport(state, "finished", "untrusted retained text") });
    assert.ok(html.includes(title));
    assert.doesNotMatch(html, /untrusted retained text/);
    assert.match(html, /not an all-clear/);
  });
}

test("report read failure does not leak transport details or hide proposals", async () => {
  const html = await render({ reportFails: true, present: true });
  assert.match(html, /SRE report unavailable/);
  assert.match(html, /No SRE proposals/);
  assert.doesNotMatch(html, /private report transport detail/);
});

test("report text is escaped and truncation is visible", async () => {
  const report = savedReport("observed", "failed", "<script>remediate()</script>");
  report.latest.report.truncated = true;
  const html = await render({ report });
  assert.match(html, /Report excerpt truncated; omitted evidence is not verified/);
  assert.match(html, /&lt;script&gt;remediate\(\)&lt;\/script&gt;/);
  assert.doesNotMatch(html, /<script\b/i);
});

test("shared uptime formatting preserves agent units and unknown observations", () => {
  const { formatUptime } = common["@/lib/format"];
  for (const [seconds, expected] of [
    [null, "—"], [undefined, "—"], [0, "0s"], [59, "59s"], [60, "1m"],
    [3599, "59m"], [3600, "1h 0m"], [3660, "1h 1m"], [86400, "1d"],
  ]) assert.equal(formatUptime(seconds), expected);
});
