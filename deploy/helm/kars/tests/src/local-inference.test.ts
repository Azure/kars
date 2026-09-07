// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Reuse the CLI's existing Vitest configuration and YAML dependency, without
// adding a second runner or changing any CLI installation/upgrade code.
const require = createRequire(new URL("../../../../../cli/package.json", import.meta.url));
const { parse, parseAllDocuments } = require("yaml");
const chart = fileURLToPath(new URL("../../", import.meta.url));
let fixtureNumber = 0;

function renderLegacy(localInference?: unknown) {
  const fixture = join(chart, "tests", `.render-fixture-${process.pid}-${++fixtureNumber}`);
  mkdirSync(join(fixture, "templates"), { recursive: true });
  try {
    copyFileSync(join(chart, "Chart.yaml"), join(fixture, "Chart.yaml"));
    copyFileSync(
      join(chart, "templates/controller-deployment.yaml"),
      join(fixture, "templates/controller-deployment.yaml"),
    );
    // No new chart defaults may fill the missing map: this models Helm's
    // --reuse-values path, not a normal old-values/new-defaults coalescing.
    writeFileSync(join(fixture, "values.yaml"), "{}\n");
    const legacy = parse(readFileSync(join(chart, "values.yaml"), "utf8"));
    delete legacy.localInference;
    if (localInference !== undefined) legacy.localInference = localInference;
    legacy.controller.replicas = 3;
    legacy.controller.image.repository = "registry.customer.test/custom-controller";
    legacy.controller.extraEnv = [{ name: "CUSTOMER_SETTING", value: "retained" }];
    legacy.sandbox.nodeSelector = { "customer.example/pool": "isolated" };
    writeFileSync(join(fixture, "legacy-values.json"), JSON.stringify(legacy));
    const rendered = execFileSync("helm", [
      "template", "customer-release", fixture, "--namespace", "customer-system",
      "--values", join(fixture, "legacy-values.json"),
    ], { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] });
    const deployment = parseAllDocuments(rendered).map((doc: { toJSON(): unknown }) => doc.toJSON())
      .find((doc: { kind?: string } | null) => doc?.kind === "Deployment");
    return deployment;
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

describe("local inference Helm upgrade compatibility", () => {
  for (const [name, value] of [
    ["missing section", undefined],
    ["null section", null],
    ["empty section", {}],
    ["missing targets", { namespaces: [] }],
    ["null lists", { namespaces: null, targets: null }],
  ]) {
    it(`keeps egress disabled and customer settings intact with ${name}`, () => {
      const deployment = renderLegacy(value);
      const container = deployment.spec.template.spec.containers[0];
      const env = Object.fromEntries(container.env.map((entry: { name: string; value?: string }) =>
        [entry.name, entry.value]));
      expect(env.LOCAL_INFERENCE_NAMESPACES).toBe("");
      expect(env.LOCAL_INFERENCE_TARGETS_JSON).toBe("[]");
      expect(env.CUSTOMER_SETTING).toBe("retained");
      expect(JSON.parse(env.KARS_SANDBOX_NODE_SELECTOR_JSON)).toEqual({ "customer.example/pool": "isolated" });
      expect(deployment.spec.replicas).toBe(3);
      expect(deployment.metadata.namespace).toBe("customer-system");
      expect(container.image).toBe("registry.customer.test/custom-controller:latest");
    });
  }

  it("preserves an explicitly configured local namespace and target", () => {
    const target = {
      namespace: "models", matchLabels: { app: "private-model" }, ports: [8000],
    };
    const deployment = renderLegacy({ namespaces: ["models"], targets: [target] });
    const env = Object.fromEntries(deployment.spec.template.spec.containers[0].env.map(
      (entry: { name: string; value?: string }) => [entry.name, entry.value],
    ));
    expect(env.LOCAL_INFERENCE_NAMESPACES).toBe("models");
    expect(JSON.parse(env.LOCAL_INFERENCE_TARGETS_JSON)).toEqual([target]);
  });
});
