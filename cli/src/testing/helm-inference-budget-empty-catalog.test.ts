// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { copyFileSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const digest = `sha256:${"a".repeat(64)}`;
function fixture(): any {
  return {
    global: { installationProfile: "kind-beta" },
    sre: { firstInstallFunding: { unboundedTokens: true } },
    inferenceRouter: { image: { tag: `latest@${digest}` } },
    inferenceBudget: {
      enabled: true, calendarPolicy: "record-only", catalogVersion: "fresh-test-catalog",
      contracts: [], caBundle: "public-ca", tlsSecretName: "budget-tls", routerImageDigest: digest,
    },
  };
}
function render(values: any) {
  const directory = resolve(process.env.KARS_TEST_ARTIFACT_DIR ?? ".", `.empty-catalog-${randomUUID()}`);
  mkdirSync(join(directory, "templates"), { recursive: true, mode: 0o700 });
  try {
    writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: budget-test\nversion: 0.1.0\n");
    writeFileSync(join(directory, "values.yaml"), JSON.stringify(values));
    for (const name of ["_inference-budget-tls.tpl", "inference-budget.yaml"]) {
      copyFileSync(join(root, "deploy/helm/kars/templates", name), join(directory, "templates", name));
    }
    return parseAllDocuments(execFileSync("helm", ["template", "kars", directory, "--namespace", "kars-system"], {
      encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"],
    })).map(document => {
      if (document.errors.length) throw document.errors[0];
      return document.toJSON();
    });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("explicit unbounded Kind beta empty catalogue", () => {
  it("renders the real broker and an empty catalogue without prices or fabricated funding records", () => {
    const values = fixture(), before = structuredClone(values);
    const objects = render(values);
    const data = objects.find(object => object.kind === "ConfigMap").data;
    expect(JSON.parse(data["contracts.json"])).toEqual({
      version: "fresh-test-catalog", contracts: [], nonInferenceEgressHosts: [],
    });
    expect(data["calendar-policy"]).toBe("record-only");
    expect(data["first-install-sre-intent"]).toBe("kars-sre-first-install");
    for (const key of ["built-in-funding.json", "token-top-ups.json", "unbounded-token-allowances.json"]) {
      expect(JSON.parse(data[key])).toEqual([]);
    }
    expect(objects.find(object => object.kind === "Service").spec.ports[0].port).toBe(9447);
    expect(objects.some(object => object.kind === "ClusterRoleBinding")).toBe(true);
    expect(objects.some(object => object.kind === "Secret")).toBe(false);
    expect(values).toEqual(before);
  });

  it.each([undefined, null, "", {}, false])("rejects absent or malformed contracts: %j", contracts => {
    const values = fixture();
    values.inferenceBudget.contracts = contracts;
    expect(() => render(values)).toThrow(/contracts must be an explicit list/);
  });

  it.each([undefined, "", "production", "generic"])("does not enable an empty catalogue in profile %j", profile => {
    const values = fixture();
    values.global.installationProfile = profile;
    expect(() => render(values)).toThrow(/empty inferenceBudget.contracts list requires kind-beta/);
  });

  it.each([undefined, "enforce"])("does not disable calendar enforcement implicitly: %j", calendarPolicy => {
    const values = fixture();
    values.inferenceBudget.calendarPolicy = calendarPolicy;
    expect(() => render(values)).toThrow(/empty inferenceBudget.contracts list requires kind-beta/);
  });

  it.each([undefined, {}, { unboundedTokens: false }, { unboundedTokens: "true" },
    { tokens: 100 }, { unboundedTokens: true, tokens: 100 }, { unboundedTokens: true, usdMicros: 100 },
    { unboundedTokens: true, expiresAt: "2099-01-01T00:00:00Z" }])("rejects finite, expiring or malformed funding: %j", funding => {
    const values = fixture();
    values.sre.firstInstallFunding = funding;
    expect(() => render(values)).toThrow(/finite or monetary budgets require genuine provider contracts/);
  });

  it.each(["catalogVersion", "caBundle", "tlsSecretName", "routerImageDigest"])("still requires %s", key => {
    const values = fixture();
    delete values.inferenceBudget[key];
    expect(() => render(values)).toThrow(new RegExp(`inferenceBudget.${key} is required`));
  });

  it("still rejects a mismatched governed router digest", () => {
    const values = fixture();
    values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    expect(() => render(values)).toThrow(/must match the digest pinned/);
  });

  it("preserves explicitly supplied contracts and production enforcement", () => {
    const values = fixture();
    values.global.installationProfile = "production";
    values.sre.firstInstallFunding = { tokens: 100, expiresAt: "2099-01-01T00:00:00Z" };
    values.inferenceBudget.calendarPolicy = "enforce";
    values.inferenceBudget.contracts = [{ id: "explicit-test-contract" }];
    const data = render(values).find(object => object.kind === "ConfigMap").data;
    expect(data["calendar-policy"]).toBe("enforce");
    expect(JSON.parse(data["contracts.json"]).contracts).toEqual(values.inferenceBudget.contracts);
  });
});
