// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";

const root = process.env.KARS_TEST_REPO_ROOT ?? fileURLToPath(new URL("../../../", import.meta.url));
function render(enabled: boolean) {
  const output = execFileSync("helm", ["template", "kars", join(root, "deploy/helm/kars"),
    "--namespace", "kars-system", "--set", `controller.networkPolicy.enabled=${enabled}`],
  { encoding: "utf8", timeout: 20_000, stdio: ["ignore", "pipe", "pipe"] });
  return parseAllDocuments(output).map(document => document.toJSON()).filter(Boolean);
}

describe("private HITL controller network path", () => {
  it("allows only the controller to reach managed sandbox control TLS", () => {
    const objects = render(true);
    const policy = objects.find(object => object.kind === "NetworkPolicy" && object.metadata.name === "kars-system-hitl-control");
    const controller = objects.find(object => object.kind === "Deployment" && object.metadata.name === "kars-controller");
    expect(policy.metadata.namespace).toBe("kars-system");
    expect(policy.spec).toEqual({
      podSelector: { matchLabels: { "app.kubernetes.io/component": "controller" } },
      policyTypes: ["Egress"],
      egress: [{
        to: [{
          namespaceSelector: { matchLabels: { "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "sandbox" } },
          podSelector: { matchLabels: { "kars.azure.com/component": "sandbox" } },
        }],
        ports: [{ protocol: "TCP", port: 9449 }],
      }],
    });
    expect(controller.spec.template.metadata.labels).toMatchObject(policy.spec.podSelector.matchLabels);
    const otherPolicies = objects.filter(object => object.kind === "NetworkPolicy" && object !== policy);
    expect(JSON.stringify(otherPolicies)).not.toContain('"port":9449');
  });

  it("respects the existing network policy installation switch", () => {
    expect(render(false).some(object => object.kind === "NetworkPolicy" && object.metadata.name === "kars-system-hitl-control")).toBe(false);
  });
});
