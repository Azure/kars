// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual as equal } from "node:util";
import { BudgetApiError } from "./budget-api-client.mjs";

const reasons = new Set(["BadRequest", "Unauthorized", "Forbidden", "NotFound", "AlreadyExists",
  "Conflict", "Invalid", "Timeout", "ServerTimeout", "TooManyRequests", "InternalError", "ServiceUnavailable"]);
const validName = value => typeof value === "string" && /^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$/.test(value)
  && value.length <= 63;
const requireIdentity = condition => {
  if (!condition) throw new BudgetApiError("cancellation-identity-or-intent");
};

export function cancellationFact(verb, resource, attempt, response, error) {
  requireIdentity(["GET", "PATCH"].includes(verb) && ["namespaces", "karstasks"].includes(resource)
    && Number.isInteger(attempt) && attempt >= 1 && attempt <= 3);
  const status = response?.status ?? (error instanceof BudgetApiError ? error.httpStatus : 0);
  const httpStatus = Number.isInteger(status) && status >= 0 && status <= 599 ? status : 0;
  const body = response?.body;
  const reason = body?.kind === "Status" && reasons.has(body.reason) ? body.reason
    : response?.status === 200 ? "Success" : "Unclassified";
  return { stage: "accepted-work-cancellation", verb, resource, attempt, httpStatus, reason };
}

export async function cancelAcceptedTask({ created, binding, request, report, deadline,
  maxAttempts = 3, now = Date.now, pause = ms => new Promise(resolve => setTimeout(resolve, ms)) }) {
  requireIdentity(Number.isInteger(maxAttempts) && maxAttempts > 0 && maxAttempts <= 3
    && Number.isFinite(deadline));
  const name = created?.metadata?.name, namespace = created?.metadata?.namespace;
  requireIdentity(validName(name) && validName(namespace) && created.metadata.uid
    && Number.isInteger(created.metadata.generation) && created.spec?.execution?.launch === true
    && created.spec.envelope?.budget?.scope === "GovernedInference"
    && binding?.scope === "GovernedInference" && binding.taskUid === created.metadata.uid
    && binding.account?.uid && binding.root?.workspaceUid);
  const expected = structuredClone(created);
  const authority = structuredClone(binding);
  const desired = structuredClone(expected.spec);
  desired.execution.launch = false;
  const path = `/apis/kars.azure.com/v1alpha1/namespaces/${namespace}/karstasks/${name}`;
  let failedVersion;
  const call = async (verb, resource, attempt, path, body) => {
    if (now() >= deadline) throw new BudgetApiError("cancellation-deadline");
    let response;
    try { response = await request(verb, path, body); }
    catch (error) {
      report(cancellationFact(verb, resource, attempt, undefined, error));
      throw error instanceof BudgetApiError ? error : new BudgetApiError("transport");
    }
    report(cancellationFact(verb, resource, attempt, response));
    if (now() >= deadline) throw new BudgetApiError("cancellation-deadline");
    return response;
  };
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const workspace = await call("GET", "namespaces", attempt, `/api/v1/namespaces/${namespace}`);
    if (workspace.status !== 200) throw new BudgetApiError("cancellation-read", workspace.status);
    requireIdentity(workspace.body?.kind === "Namespace" && workspace.body.apiVersion === "v1"
      && workspace.body.metadata?.uid === authority.root.workspaceUid
      && workspace.body.metadata.name === namespace && !workspace.body.metadata.deletionTimestamp);
    const live = await call("GET", "karstasks", attempt, path);
    if (live.status !== 200) throw new BudgetApiError("cancellation-read", live.status);
    const current = live.body;
    requireIdentity(current?.kind === "KarsTask" && current.apiVersion === "kars.azure.com/v1alpha1"
      && current.metadata?.uid === expected.metadata.uid && current.metadata.name === name
      && current.metadata.namespace === namespace && !current.metadata.deletionTimestamp
      && current.metadata.generation === expected.metadata.generation
      && equal(current.spec, expected.spec) && equal(current.status?.inferenceBudget, authority)
      && typeof current.metadata.resourceVersion === "string" && current.metadata.resourceVersion.length > 0);
    if (failedVersion !== undefined && current.metadata.resourceVersion === failedVersion)
      throw new BudgetApiError("cancellation-conflict-without-fresh-version", 409);
    const result = await call("PATCH", "karstasks", attempt, path, {
      metadata: { uid: expected.metadata.uid, resourceVersion: current.metadata.resourceVersion },
      spec: { execution: { launch: false } },
    });
    // Only a genuine API Status Conflict authorizes a fresh, fully fenced retry.
    if (result.status === 409 && result.body?.apiVersion === "v1" && result.body.kind === "Status"
        && result.body.status === "Failure" && result.body.reason === "Conflict" && result.body.code === 409) {
      failedVersion = current.metadata.resourceVersion;
      if (attempt < maxAttempts && now() < deadline) await pause(Math.min(100, deadline - now()));
      continue;
    }
    if (result.status !== 200) throw new BudgetApiError("cancellation-patch", result.status);
    requireIdentity(result.body?.kind === "KarsTask" && result.body.apiVersion === "kars.azure.com/v1alpha1"
      && result.body.metadata?.uid === expected.metadata.uid && result.body.metadata.name === name
      && result.body.metadata.namespace === namespace && !result.body.metadata.deletionTimestamp
      && result.body.metadata.generation === expected.metadata.generation + 1
      && typeof result.body.metadata.resourceVersion === "string"
      && result.body.metadata.resourceVersion !== current.metadata.resourceVersion
      && equal(result.body.spec, desired));
    return result.body;
  }
  throw new BudgetApiError("cancellation-conflict-limit", 409);
}
