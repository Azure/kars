// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const { workloadCases, policyDenial } = await import(
  new URL("../../../tests/e2e/budget-workload-cases.mjs", import.meta.url).href
);
const policy = JSON.parse(readFileSync(
  new URL("../../../deploy/helm/kars/files/inference-budget-admission.json", import.meta.url), "utf8",
)).items.find((item: { name: string }) => item.name === "kars-inference-budget-workloads");
const namespace = "budget-api-fixture";
const controller = `system:serviceaccount:${namespace}:kars-controller`;
const principal = `system:serviceaccount:${namespace}:untrusted`;

function rejection(name: string, message = policy.spec.validations[0].message) {
  return { status: 422, body: { kind: "Status", status: "Failure", reason: "Invalid",
    details: { name, causes: [{ message:
      `ValidatingAdmissionPolicy '${policy.name}' with binding '${policy.name}' denied request: ${message}` }] } } };
}

class ApiFixture {
  objects = new Map<string, any>();
  calls: Array<{ method: string; path: string; body: any; actor?: string }> = [];
  serial = 0;
  allowTenant = false;
  conflict = false;

  remove(path: string) {
    const uid = this.objects.get(path)?.metadata.uid;
    this.objects.delete(path);
    for (const [child, body] of this.objects) {
      if (body.metadata.ownerReferences?.some((owner: any) => owner.uid === uid)) this.remove(child);
    }
  }

  request = async (method: string, input: string, body?: any, actor?: string) => {
    const url = new URL(input, "http://fixture");
    const path = url.pathname;
    this.calls.push({ method, path: input, body: structuredClone(body), actor });
    if (method === "GET" && path.includes("/serviceaccounts/")) {
      return { status: 200, body: { metadata: { uid: "real-service-account" } } };
    }
    if (method === "GET" && url.searchParams.has("labelSelector")) {
      return { status: 200, body: { items: [...this.objects.entries()]
        .filter(([key, value]) => key.startsWith(path + "/") && value.metadata.labels?.["budget-api-case"] === "actual-chain")
        .map(([, value]) => structuredClone(value)) } };
    }
    if (method === "GET") {
      return this.objects.has(path) ? { status: 200, body: structuredClone(this.objects.get(path)) }
        : { status: 404, body: {} };
    }
    if (path.endsWith("/subjectaccessreviews")) return { status: 201, body: { status: { allowed: true } } };
    if (method === "DELETE") {
      const existing = this.objects.get(path);
      expect(body.preconditions).toEqual({ uid: existing.metadata.uid, resourceVersion: existing.metadata.resourceVersion });
      if (this.conflict) {
        this.conflict = false;
        existing.metadata.resourceVersion = "2";
        return { status: 409, body: {} };
      }
      this.remove(path);
      return { status: 200, body: {} };
    }
    if (url.searchParams.get("dryRun") === "All") {
      if (this.allowTenant && actor === principal) return { status: 201, body };
      return rejection(body.metadata.name);
    }
    if (method === "PATCH") {
      const existing = this.objects.get(path);
      expect(body.metadata.uid).toBe(existing.metadata.uid);
      existing.metadata.labels = body.metadata.labels;
      return { status: 200, body: structuredClone(existing) };
    }
    expect(method).toBe("POST");
    const created = structuredClone(body);
    created.metadata = { ...body.metadata, uid: `native-${++this.serial}`, resourceVersion: "1" };
    this.objects.set(path + "/" + body.metadata.name, created);
    if (body.kind === "Deployment") {
      const ns = body.metadata.namespace;
      const labels = { "budget-api-case": "actual-chain" };
      this.objects.set(`/apis/apps/v1/namespaces/${ns}/replicasets/actual-rs`, {
        kind: "ReplicaSet", metadata: { name: "actual-rs", uid: "controller-rs", labels,
          ownerReferences: [{ controller: true, uid: created.metadata.uid }] },
      });
      this.objects.set(`/api/v1/namespaces/${ns}/pods/actual-pod`, {
        kind: "Pod", metadata: { name: "actual-pod", uid: "controller-pod", labels,
          ownerReferences: [{ controller: true, uid: "controller-rs" }] },
        spec: { automountServiceAccountToken: false },
      });
    }
    return { status: 201, body: structuredClone(created) };
  };
}

const wait = async (check: () => Promise<unknown>) => expect(await check()).toBeTruthy();
const options = { namespace, controller, principal, policy };

describe("native budget workload proof orchestration (not native execution evidence)", () => {
  it("keeps the exact shared primary/ephemeral distinction and controller allowlist", () => {
    const spec = policy.spec;
    expect(spec.validations).toHaveLength(1);
    expect(spec.validations[0].expression).toContain("request.?subResource.orValue('') != 'ephemeralcontainers'");
    expect(spec.validations[0].expression).toContain("request.resource.resource != 'deployments'");
    expect(spec.validations[0].expression).toContain(
      "['system:kube-controller-manager', 'system:serviceaccount:kube-system:deployment-controller', 'system:serviceaccount:kube-system:replicaset-controller']");
    expect(spec.namespaceSelector).toBeUndefined();
    expect(spec.matchConstraints.namespaceSelector).toEqual({ matchLabels: { "kars.azure.com/inference-budget": "v1" } });
    expect(spec.failurePolicy).toBe("Fail");
  });

  it("requires exact native policy denial rather than RBAC, missing-field or other errors", () => {
    expect(policyDenial(rejection("fixture"), policy, "fixture")).toBe(true);
    expect(policyDenial({ status: 403, body: { kind: "Status", reason: "Forbidden" } }, policy, "fixture")).toBe(false);
    expect(policyDenial(rejection("other"), policy, "fixture")).toBe(false);
    expect(policyDenial(rejection("fixture", "evaluation failed: no such key: subResource"), policy, "fixture")).toBe(false);
  });

  it("exercises the native-controller chain, primary actor matrix and exact forbidden paths", async () => {
    const api = new ApiFixture();
    const results: any[] = [];
    await workloadCases(api.request, wait, options, (result: any) => results.push(result));
    expect(results).toHaveLength(19);
    expect(results.every(result => result.matched)).toBe(true);
    expect(results.find(result => result.case === "actual-controller-chain-created").readinessClaimed).toBe(false);
    expect(results.filter(result => result.case.endsWith("-primary"))).toHaveLength(8);
    expect(results.filter(result => result.case.endsWith("-ephemeral-denied"))).toHaveLength(4);
    expect(api.calls.some(call => call.path.endsWith("/status"))).toBe(false);
    expect(api.calls.every(call => !call.body?.status || call.body.kind === "SubjectAccessReview")).toBe(true);
    for (const call of api.calls.filter(call => call.body?.kind === "Pod" && call.method === "POST")) {
      expect(call.body.spec.automountServiceAccountToken).toBe(false);
      expect(call.body.spec.schedulerName).toBe("budget-api-never-schedule");
    }
    expect(api.objects.size).toBe(0);
  });

  it("fails if a forbidden tenant path is admitted, without leaving owned fixtures", async () => {
    const api = new ApiFixture();
    api.allowTenant = true;
    await expect(workloadCases(api.request, wait, options, () => {})).rejects.toThrow("Wrong native denial");
    expect(api.objects.size).toBe(0);
  });

  it("retries cleanup conflicts with fresh UID/RV rather than discarding the fence", async () => {
    const api = new ApiFixture();
    api.conflict = true;
    await workloadCases(api.request, wait, options, () => {});
    expect(api.objects.size).toBe(0);
    expect(api.calls.filter(call => call.method === "DELETE").some(
      call => call.body.preconditions.resourceVersion === "2")).toBe(true);
  });
});
