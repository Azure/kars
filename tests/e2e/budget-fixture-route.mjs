// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";

export const PROVIDER = "budget-fixture";
export const ENDPOINT = "http://provider.budget-provider-fixture.svc.cluster.local:8000/v1";
export function providerSource(namespace) {
  return { apiVersion: "v1", kind: "Secret", type: "Opaque",
    metadata: { name: "kars-inference-providers", namespace },
    stringData: { KARS_PROVIDER_BUDGET_FIXTURE_ENDPOINT: ENDPOINT } };
}

export function verifyFixturePolicy(policy) {
  assert(policy?.spec?.modelPreference?.primary?.provider === PROVIDER
    && policy.spec.modelPreference.primary.deployment === "fixture"
    && !policy.spec.provider, "Generated fixture policy must select the registered named provider");
}

export function readinessFact(endpoint, response, error) {
  assert(["/healthz", "/readyz"].includes(endpoint));
  const ready = endpoint === "/healthz" ? response?.status === 200
    : response?.status === 200 && response.value === "governed inference authority and model contracts available";
  return { stage: endpoint === "/healthz" ? "router-http" : "governed-readiness",
    endpoint, httpStatus: response?.status ?? 0, ready,
    category: error ? (error.name === "TimeoutError" || error.name === "AbortError" ? "timeout" : "transport")
      : ready ? "ready"
        : response?.value === "not ready \u2014 governed inference authority, provider or contracts unavailable"
          ? "budget-authority-provider-contract" : "unexpected-readiness-response" };
}
