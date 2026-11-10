// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { execa } from "execa";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parseAllDocuments } from "yaml";

function documents(text: string) {
  const parsed = parseAllDocuments(text);
  expect(parsed.flatMap(document => document.errors)).toEqual([]);
  return parsed.map(document => document.toJSON());
}

describe("SRE report-only authority manifests", () => {
  it("renders a fixed-store fail-closed admission boundary without changing budgets or proposals", async () => {
    const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const { stdout } = await execa("helm", ["template", "kars", chart, "--namespace", "kars-system",
      "--show-only", "templates/sre-report-authority.yaml"], { timeout: 10_000 });
    const [policy, binding] = documents(stdout);
    expect(policy.kind).toBe("ValidatingAdmissionPolicy");
    expect(policy.spec.failurePolicy).toBe("Fail");
    expect(policy.spec.matchConstraints.resourceRules).toEqual([{
      apiGroups: [""], apiVersions: ["v1"], operations: ["CREATE", "UPDATE", "DELETE"], resources: ["configmaps"],
    }]);
    const scope = policy.spec.matchConditions[0].expression;
    expect(scope).toContain("request.namespace == 'kars-sre'");
    expect(scope).toContain("request.name == 'sre-health-report'");
    const predicate = policy.spec.validations[0].expression;
    for (const required of [
      ".name('canonical').check('use').allowed()", "request.operation == 'UPDATE'",
      "request.userInfo.username == 'system:serviceaccount:kars-sre:sre-api-router'",
      "object.metadata.uid == oldObject.metadata.uid", "object.metadata.annotations == oldObject.metadata.annotations",
      "object.metadata.ownerReferences == oldObject.metadata.ownerReferences",
      "object.data.size() == 1", "'report.json' in object.data", "bytes(object.data['report.json']).size() <= 65536",
      "!has(object.binaryData)", "!has(object.metadata.deletionTimestamp)",
    ]) expect(predicate).toContain(required);
    expect(predicate).not.toContain("object.data['report.json'].size()");
    expect(predicate).toContain([
      "(request.operation == 'DELETE' &&",
      " request.userInfo.username == 'system:serviceaccount:kube-system:namespace-controller' &&",
      " namespaceObject != null &&",
      " dyn(namespaceObject.metadata).?deletionTimestamp.orValue(null) != null &&",
      " namespaceObject.status.phase == 'Terminating') ||",
    ].join("\n"));
    expect(predicate.match(/namespace-controller/g)).toHaveLength(1);
    expect(binding.kind).toBe("ValidatingAdmissionPolicyBinding");
    expect(binding.spec).toEqual({ policyName: "kars-sre-report-authority", validationActions: ["Deny", "Audit"] });
    expect(stdout).not.toMatch(/kind: (InferencePolicy|KarsBudgetAccount|KarsSREAction)/);
  });

  it("Bridge receives only exact canonical registration GET in both supported manifests", async () => {
    const chart = fileURLToPath(new URL("../../../bridge/deploy/helm/kars-bridge", import.meta.url));
    const { stdout } = await execa("helm", ["template", "bridge", chart, "--namespace", "kars-system",
      "--show-only", "templates/rbac.yaml"], { timeout: 10_000 });
    const legacy = readFileSync(new URL("../../../bridge/deploy/rbac.yaml", import.meta.url), "utf8");
    for (const text of [stdout, legacy]) {
      const rules = documents(text).filter(document => document?.kind === "ClusterRole")
        .flatMap(document => document.rules)
        .filter(rule => rule.resources?.includes("karssreregistrations"));
      expect(rules).toEqual([{
        apiGroups: ["kars.azure.com"], resources: ["karssreregistrations"], resourceNames: ["canonical"], verbs: ["get"],
      }]);
    }
  });
});
