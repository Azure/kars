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
});
