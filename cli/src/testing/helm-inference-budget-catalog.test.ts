// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";
import { describe, expect, it } from "vitest";

const root = process.env.KARS_TEST_REPO_ROOT ?? fileURLToPath(new URL("../../../", import.meta.url));
const desired = { version: "retained-v1", contracts: [{ id: "retained-model" }], nonInferenceEgressHosts: [] };
const stored = JSON.stringify(desired, null, 2) + "\n";

function fixture() {
  return {
    desired, release: "kars", namespace: "kars-system",
    current: {
      metadata: {
        uid: "catalog-uid",
        labels: { "app.kubernetes.io/managed-by": "Helm" },
        annotations: {
          "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system",
          "kars.azure.com/inference-budget-contracts": "v1",
        },
      },
      data: { "contracts.json": stored },
    },
  };
}

function render(input: unknown, helper: "catalogData" | "tlsChecksum" = "catalogData"): string {
  const directory = mkdtempSync(join(tmpdir(), "kars-catalog-"));
  try {
    mkdirSync(join(directory, "templates"));
    writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: catalog-test\nversion: 0.1.0\n");
    writeFileSync(join(directory, "values.yaml"), JSON.stringify({ input }));
    copyFileSync(join(root, "deploy/helm/kars/templates/_inference-budget-tls.tpl"),
      join(directory, "templates/_budget.tpl"));
    writeFileSync(join(directory, "templates/result.yaml"),
      `apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: result\ndata:\n  catalog: {{ include "kars.inferenceBudget.${helper}" .Values.input | quote }}\n`);
    return parse(execFileSync("helm", ["template", "catalog-test", directory], {
      encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"],
    })).data.catalog;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("retained Helm inference catalog serialization", () => {
  it("renders a fresh catalog without a pre-existing ConfigMap", () => {
    expect(JSON.parse(render({ ...fixture(), current: {} }))).toEqual(desired);
  });

  describe("budget TLS checksum after an operator rotation", () => {
    function input() {
      const metadata = fixture().current.metadata;
      return {
        release: "kars", namespace: "kars-system",
        identity: { certificate: "current certificate", privateKey: "current key" },
        existing: { metadata, data: {
          "tls.crt": Buffer.from("current certificate").toString("base64"),
          "tls.key": Buffer.from("current key").toString("base64"),
        } },
        controller: { metadata: { ...metadata, uid: "controller-uid" },
          spec: { template: { metadata: { annotations: { "checksum/inference-budget-tls": "a".repeat(64) } } } } },
      };
    }

    it("does not restart a sealed controller when the actual TLS identity is unchanged", () => {
      expect(render(input(), "tlsChecksum")).toBe("a".repeat(64));
    });

    it.each(["certificate", "privateKey"])("changes the checksum for an actual %s rotation", field => {
      const value = input();
      if (field === "certificate") value.identity.certificate = "new certificate";
      else value.identity.privateKey = "new key";
      const checksum = render(value, "tlsChecksum");
      expect(checksum).toMatch(/^[a-f0-9]{64}$/);
      expect(checksum).not.toBe("a".repeat(64));
    });

    it("computes the normal checksum on a fresh installation", () => {
      const value = input();
      expect(render({ ...value, existing: {}, controller: {} }, "tlsChecksum"))
        .toMatch(/^[a-f0-9]{64}$/);
    });

    it.each(["existing", "controller"] as const)("refuses a foreign %s", field => {
      const value = structuredClone(input());
      value[field].metadata.annotations["meta.helm.sh/release-name"] = "other";
      expect(() => render(value, "tlsChecksum")).toThrow(/outside this release's live ownership/);
    });
  });

  it("retains exact operator serialization only when the catalog is semantically unchanged", () => {
    expect(render(fixture())).toBe(stored);
    const input = fixture();
    input.desired = { ...desired, version: "reviewed-v2" };
    const output = render(input);
    expect(output).not.toBe(stored);
    expect(JSON.parse(output)).toEqual(input.desired);
  });

  it.each(["release", "namespace", "manager", "marker", "deleting"])("rejects %s ownership drift", fault => {
    const input = fixture();
    if (fault === "release") input.current.metadata.annotations["meta.helm.sh/release-name"] = "other";
    if (fault === "namespace") input.current.metadata.annotations["meta.helm.sh/release-namespace"] = "other";
    if (fault === "manager") input.current.metadata.labels["app.kubernetes.io/managed-by"] = "other";
    if (fault === "marker") input.current.metadata.annotations["kars.azure.com/inference-budget-contracts"] = "other";
    if (fault === "deleting") Object.assign(input.current.metadata, { deletionTimestamp: "2026-10-13T00:00:00Z" });
    expect(() => render(input)).toThrow(/outside this release's ownership/);
  });

  it("surfaces malformed retained JSON instead of silently replacing it", () => {
    const input = fixture();
    input.current.data["contracts.json"] = "{";
    expect(() => render(input)).toThrow();
  });
});
