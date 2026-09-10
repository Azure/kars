// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { X509Certificate } from "node:crypto";

export const PROVIDER = "budget-fixture";
export const ENDPOINT = "http://provider.budget-provider-fixture.svc.cluster.local:8000/v1";
export const TLS_SERVER_EXTENSIONS = [
  "-addext", "basicConstraints=critical,CA:FALSE",
  "-addext", "keyUsage=critical,digitalSignature,keyEncipherment",
  "-addext", "extendedKeyUsage=serverAuth",
];

export function verifyFixtureCertificate(certificate) {
  const leaf = new X509Certificate(certificate);
  assert(!leaf.ca, "Budget fixture TLS server certificate must be an end entity, not a CA");
  assert(leaf.checkHost("kars-inference-budget.kars-system.svc", { subject: "never" }),
    "Budget fixture TLS server SAN must match the private broker hostname");
  assert(leaf.keyUsage?.includes("1.3.6.1.5.5.7.3.1"),
    "Budget fixture TLS certificate must explicitly authorize server authentication");
}

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

const STAGES = {
  kars_inference_router: {
    readiness: ["router-binding", "router-guardrails", "router-provider", "router-candidate",
      "router-target", "router-catalog", "router-credential"],
    client: ["router-budget-client", "router-broker-http", "router-contract-match"],
  },
  kars_controller: {
    auth: ["broker-authorization", "broker-authorization-api"],
    service: ["inference_budget_exhausted", "inference_budget_capacity", "inference_budget_authority",
      "inference_budget_attempt_state", "inference_budget_contract", "inference_budget_closed",
      "inference_budget_frozen", "inference_budget_unavailable"],
  },
};

export function budgetStageFacts(log) {
  const facts = new Map();
  for (const line of log.split("\n")) {
    let record;
    try { record = JSON.parse(line); } catch { continue; }
    const [crate, module, file, ...rest] = String(record?.target).split("::");
    const fields = record?.fields;
    if (module !== "inference_budget" || rest.length || record?.level !== "WARN"
        || !Object.hasOwn(STAGES, crate) || !Object.hasOwn(STAGES[crate], file)
        || !STAGES[crate]?.[file]?.includes(fields?.budget_stage)) continue;
    const fact = { stage: fields.budget_stage };
    if (Number.isInteger(fields.source_line) && fields.source_line > 0 && fields.source_line <= 10_000)
      fact.sourceLine = fields.source_line;
    if (Number.isInteger(fields.http_status) && fields.http_status >= 0 && fields.http_status <= 599)
      fact.httpStatus = fields.http_status;
    if (fact.stage === "router-contract-match") {
      for (const key of ["provider_matches", "endpoint_matches", "model_matches", "bounds_valid", "price_available"])
        if (typeof fields[key] === "boolean") fact[key] = fields[key];
    }
    const key = JSON.stringify(fact);
    facts.set(key, { ...fact, count: (facts.get(key)?.count ?? 0) + 1 });
  }
  return [...facts.values()];
}

export function routerTemplateFacts(pod, replica, deployment) {
  const router = pod?.spec?.containers?.find(c => c.name === "inference-router");
  const current = deployment?.spec?.template?.spec?.containers?.find(c => c.name === "inference-router");
  assert(router && current, "Real router and current Deployment template required");
  const same = key => JSON.stringify(router[key] ?? null) === JSON.stringify(current[key] ?? null);
  const binding = container => container.env?.find(e => e.name === "KARS_INFERENCE_BUDGET_BINDING")?.value;
  return {
    stage: "router-live-template",
    podOwnerUidMatches: !!replica?.metadata?.uid && pod.metadata.ownerReferences?.some(
      owner => owner.controller === true && owner.kind === "ReplicaSet" && owner.uid === replica.metadata.uid),
    replicaOwnerUidMatches: !!deployment?.metadata?.uid && replica.metadata.ownerReferences?.some(
      owner => owner.controller === true && owner.kind === "Deployment" && owner.uid === deployment.metadata.uid),
    imageMatches: same("image"), commandMatches: same("command"), argsMatch: same("args"),
    environmentMatches: same("env"), environmentSourcesMatch: same("envFrom"),
    securityContextMatches: same("securityContext"), volumeMountsMatch: same("volumeMounts"),
    budgetBindingMatches: !!binding(router) && binding(router) === binding(current),
  };
}
