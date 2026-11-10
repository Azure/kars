// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(new URL("../../../../../cli/package.json", import.meta.url));
const { parse, parseAllDocuments } = require("yaml");
const chart = fileURLToPath(new URL("../../", import.meta.url));

function render(defaults: unknown, sre: Record<string, unknown> = {}) {
  const fixture = mkdtempSync(join(chart, "tests", ".sre-defaults-"));
  try {
    mkdirSync(join(fixture, "templates"));
    writeFileSync(join(fixture, "Chart.yaml"), JSON.stringify({
      apiVersion: "v2", name: "sre-defaults-test", version: "0.0.0",
    }));
    copyFileSync(join(chart, "templates/_sre-resources.tpl"), join(fixture, "templates/_sre-resources.tpl"));
    writeFileSync(join(fixture, "templates/sre.yaml"), '{{ include "kars.sre.resources" . }}\n');
    const values = parse(readFileSync(join(chart, "values.yaml"), "utf8"));
    values.controller.inferenceDefaults = defaults;
    values.sre = { ...values.sre, enabled: true, ...sre };
    writeFileSync(join(fixture, "values.yaml"), JSON.stringify(values));
    const output = execFileSync("helm", [
      "template", "customer-core", fixture, "--namespace", "customer-system",
    ], { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] });
    return parseAllDocuments(output).map((document: { toJSON(): any }) => document.toJSON())
      .filter(Boolean);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

function policy(documents: any[]) {
  return documents.find(document => document.kind === "InferencePolicy");
}

describe("Core-owned SRE inference defaults", () => {
  for (const provider of ["azure-openai", "foundry", "github-copilot"]) {
    it(`inherits the selected ${provider} route without Bridge configuration`, () => {
      const documents = render({ provider, model: "customer-selected-model" });
      expect(policy(documents).spec.modelPreference.primary).toEqual({
        provider, deployment: "customer-selected-model",
      });
      expect(policy(documents).metadata.namespace).toBe("customer-system");
      expect(documents.find(document => document.kind === "KarsSandbox").metadata.namespace)
        .toBe("customer-system");
      expect(JSON.stringify(documents)).not.toContain("kars-bridge");
    });
  }

  it("preserves an explicitly pinned SRE route when Core defaults differ", () => {
    const documents = render({ provider: "foundry", model: "new-default" }, {
      provider: "azure-openai", model: "retained-sre-model",
    });
    expect(policy(documents).spec.modelPreference.primary).toEqual({
      provider: "azure-openai", deployment: "retained-sre-model",
    });
  });

  it("allows an SRE model override on the Core provider", () => {
    expect(policy(render({ provider: "foundry", model: "default" }, { model: "sre-model" }))
      .spec.modelPreference.primary).toEqual({ provider: "foundry", deployment: "sre-model" });
  });

  it("retains existing explicit SRE configuration without new Core defaults", () => {
    expect(policy(render({}, { provider: "azure-openai", model: "retained" }))
      .spec.modelPreference.primary).toEqual({ provider: "azure-openai", deployment: "retained" });
  });

  it("requires a real selection rather than inventing a model", () => {
    for (const defaults of [{}, { provider: "foundry" }, { model: "selected" },
      { provider: "foundry", model: " " }]) {
      expect(() => render(defaults)).toThrow(/SRE requires provider\/model strings|Configure controller\.inferenceDefaults before enabling SRE/);
    }
  });

  it("does not require inference settings when SRE is disabled", () => {
    expect(render({}, { enabled: false })).toEqual([]);
  });
});
