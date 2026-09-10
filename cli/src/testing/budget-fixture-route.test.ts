// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
const route = await import(new URL("../../../tests/e2e/budget-fixture-route.mjs", import.meta.url).href);

describe("budget named-provider fixture and private readiness diagnostics", () => {
  it("registers the exact anonymous local endpoint, without credentials or a native-provider fallback", () => {
    const source = route.providerSource("kars-system");
    expect(source.metadata).toEqual({ name: "kars-inference-providers", namespace: "kars-system" });
    expect(source.stringData).toEqual({ KARS_PROVIDER_BUDGET_FIXTURE_ENDPOINT: route.ENDPOINT });
    expect(route.ENDPOINT).toBe("http://provider.budget-provider-fixture.svc.cluster.local:8000/v1");
    expect(route.PROVIDER).toBe("budget-fixture");
    const harness = readFileSync(new URL("../../../tests/e2e/inference-budget-enforcement.mjs", import.meta.url), "utf8");
    expect(harness).toContain("providerId: PROVIDER, endpoint");
    expect(harness).toContain("create(providerSource(namespace))");
    expect(harness).not.toContain('provider: "ollama"');
  });
  it("requires actual generated policy identity to match the registered operator contract", () => {
    expect(() => route.verifyFixturePolicy({ spec: { modelPreference: { primary: {
      provider: route.PROVIDER, deployment: "fixture",
    } } } })).not.toThrow();
    expect(() => route.verifyFixturePolicy({ spec: { modelPreference: { primary: {
      provider: "ollama", deployment: "fixture",
    } } } })).toThrow();
  });
  it("distinguishes HTTP readiness, transport timeout and the private budget gate without body/error output", () => {
    const secret = "DO-NOT-EXPORT-PRIVATE-BODY";
    const unavailable = route.readinessFact("/readyz", { status: 503, value: secret });
    expect(unavailable.httpStatus).toBe(503);
    expect(unavailable.ready).toBe(false);
    expect(JSON.stringify(unavailable)).not.toContain(secret);
    expect(route.readinessFact("/readyz", undefined, { name: "TimeoutError", message: secret }).category).toBe("timeout");
    expect(route.readinessFact("/readyz", { status: 200, value: "ok" }).ready).toBe(false);
    expect(route.readinessFact("/readyz", {
      status: 200, value: "governed inference authority and model contracts available",
    }).ready).toBe(true);
    expect(route.readinessFact("/healthz", { status: 200, value: "ok" }).ready).toBe(true);
  });
  it("exports only fixed production diagnostic stages, numeric facts and counts", () => {
    const secret = "DO-NOT-EXPORT-ENV-ARGV-BODY";
    const record = { target: "kars_controller::inference_budget::auth", level: "WARN",
      fields: { budget_stage: "broker-authorization", source_line: 123, http_status: 403,
        message: secret, env: secret, argv: [secret] } };
    const log = [secret, JSON.stringify(record), JSON.stringify(record),
      JSON.stringify({ ...record, fields: { budget_stage: secret } }),
      JSON.stringify({ ...record, target: "untrusted", fields: record.fields }),
      JSON.stringify({ ...record, target: "__proto__::inference_budget::constructor" }),
      JSON.stringify({ ...record, fields: { ...record.fields, source_line: secret, http_status: secret } }),
    ].join("\n");
    expect(route.budgetStageFacts(log)).toEqual([
      { stage: "broker-authorization", sourceLine: 123, httpStatus: 403, count: 2 },
      { stage: "broker-authorization", count: 1 },
    ]);
    expect(JSON.stringify(route.budgetStageFacts(log))).not.toContain(secret);
    expect(route.budgetStageFacts(JSON.stringify({
      target: "kars_inference_router::inference_budget::client", level: "WARN", fields: {
        budget_stage: "router-contract-match", provider_matches: false, endpoint_matches: false,
        model_matches: true, bounds_valid: true, price_available: secret, provider_id: secret,
      },
    }))).toEqual([{ stage: "router-contract-match", provider_matches: false,
      endpoint_matches: false, model_matches: true, bounds_valid: true, count: 1 }]);
  });
  it("reports actual UID and live-template mismatches as booleans without workload inputs", () => {
    const router = { name: "inference-router", image: "router@sha256:fixture",
      env: [{ name: "KARS_INFERENCE_BUDGET_BINDING", value: "PRIVATE-BINDING" }], args: ["PRIVATE-ARGV"] };
    const pod = { metadata: { ownerReferences: [{ kind: "ReplicaSet", controller: true, uid: "rs" }] },
      spec: { containers: [router] } };
    const replica = { metadata: { uid: "rs",
      ownerReferences: [{ kind: "Deployment", controller: true, uid: "deployment" }] } };
    const deployment = { metadata: { uid: "deployment" },
      spec: { template: { spec: { containers: [structuredClone(router)] } } } };
    expect(route.routerTemplateFacts(pod, replica, deployment).budgetBindingMatches).toBe(true);
    deployment.spec.template.spec.containers[0].env[0].value = "PRIVATE-NEW-BINDING";
    const facts = route.routerTemplateFacts(pod, replica, deployment);
    expect(facts.podOwnerUidMatches).toBe(true);
    expect(facts.replicaOwnerUidMatches).toBe(true);
    expect(facts.budgetBindingMatches).toBe(false);
    expect(facts.environmentMatches).toBe(false);
    expect(JSON.stringify(facts)).not.toContain("PRIVATE");
  });
  it("collects bounded filtered diagnostics before cleanup even when the readiness gate fails", () => {
    const harness = readFileSync(new URL("../../../tests/e2e/inference-budget-enforcement.mjs", import.meta.url), "utf8");
    expect(harness).toMatch(/^function diagnostics\(\)/m);
    expect(harness).toContain('"--tail=256", "--limit-bytes=131072"');
    expect(harness).toContain("facts: budgetStageFacts(log)");
    expect(harness).toMatch(/finally \{\s+try \{\s+diagnostics\(\);\s+\} finally \{/);
    expect(harness.indexOf("diagnostics();")).toBeLessThan(harness.indexOf('process.kill("SIGTERM")'));
  });
});
