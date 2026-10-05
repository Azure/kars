// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(new URL("../../../../../cli/package.json", import.meta.url));
const { parse, parseAllDocuments } = require("yaml");
const chart = fileURLToPath(new URL("../../", import.meta.url));
let counter = 0;

function render(inferenceBudget?: unknown, existingSecret?: unknown) {
  const fixture = join(chart, "tests", `.budget-fixture-${process.pid}-${++counter}`);
  mkdirSync(fixture);
  try {
    cpSync(join(chart, "templates"), join(fixture, "templates"), { recursive: true });
    cpSync(join(chart, "files"), join(fixture, "files"), { recursive: true });
    cpSync(join(chart, "Chart.yaml"), join(fixture, "Chart.yaml"));
    const values = parse(readFileSync(join(chart, "values.yaml"), "utf8"));
    delete values.inferenceBudget;
    if (inferenceBudget !== undefined) values.inferenceBudget = inferenceBudget;
    if (existingSecret !== undefined) {
      const helper = join(fixture, "templates/_inference-budget-tls.tpl");
      const source = readFileSync(helper, "utf8");
      const lookup = 'lookup "v1" "Secret" .Release.Namespace $name';
      expect(source.split(lookup)).toHaveLength(2);
      writeFileSync(helper, source.replace(lookup, "(.Values.testExistingBudgetSecret | default dict)"));
      values.testExistingBudgetSecret = existingSecret;
    }
    values.controller.replicas = 3;
    values.controller.extraEnv = [{ name: "CUSTOMER_SETTING", value: "preserved" }];
    writeFileSync(join(fixture, "values.yaml"), JSON.stringify(values));
    const result = execFileSync("helm", [
      "template", "retained-release", fixture, "--namespace", "customer-system",
    ], { encoding: "utf8", timeout: 20_000, stdio: ["ignore", "pipe", "pipe"] });
    return parseAllDocuments(result).map((document: { toJSON(): unknown; errors: unknown[] }) => {
      if (document.errors.length) throw new Error("Invalid rendered YAML");
      return document.toJSON();
    }).filter(Boolean);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

const configured = {
  enabled: true,
  catalogVersion: "test-v1",
  tlsSecretName: "operator-provided-budget-tls",
  routerImageDigest: `sha256:${"7".repeat(64)}`,
  caBundle: "-----BEGIN CERTIFICATE-----\npublic-test-fixture\n-----END CERTIFICATE-----",
  nonInferenceEgressHosts: ["api.github.com"],
  contracts: [{
    id: "bounded-chat", version: "v1", validUntil: "2030-01-01T00:00:00Z",
    providerId: "private", endpoint: "https://models.example.test", model: "bounded",
    operation: "ChatCompletions", outputField: "MaxTokens",
    maximumInputTokens: 100, maximumOutputTokens: 20, maximumWireBytes: 4096,
    outputBoundIncludesReasoning: true,
    maximumPrice: { kind: "perRequest", maximumMicros: 5 },
  }],
};

describe("governed inference budget Helm contract", () => {
  it.each([undefined, configured])(
    "publishes observational account phase and keyed standard conditions with %s", (value) => {
      const definition = render(value).find((doc: { kind: string; spec?: { names?: { kind?: string } } }) =>
        doc.kind === "CustomResourceDefinition" && doc.spec?.names?.kind === "KarsBudgetAccount");
      const version = definition.spec.versions[0];
      expect(version.additionalPrinterColumns).toContainEqual({
        name: "Phase", type: "string", jsonPath: ".status.phase",
      });
      const status = version.schema.openAPIV3Schema.properties.status.properties;
      expect(status.phase.enum).toEqual([
        "Bootstrap", "Active", "Blocked", "Closing", "Closed", "Frozen", "Corrupt", "Unknown", null,
      ]);
      expect(status.conditions.type).toBe("array");
      expect(status.conditions["x-kubernetes-list-type"]).toBe("map");
      expect(status.conditions["x-kubernetes-list-map-keys"]).toEqual(["type"]);
      expect(status.conditions.items.required).toEqual([
        "lastTransitionTime", "message", "reason", "status", "type",
      ]);
      expect(status.conditions.items.properties.lastTransitionTime.format).toBe("date-time");
      expect(status.conditions.items.properties.observedGeneration.format).toBe("int64");
      expect(status.ledger.properties.phase.enum).toEqual(["Active", "Closing", "Closed", "Frozen"]);
    },
  );

  it("keeps generated Task/Team CEL in sync with the Rust scope contract", () => {
    const source = readFileSync(join(chart, "../../../controller/src/inference_budget/scope.rs"), "utf8");
    const rule = (name: string): string => {
      const value = source.match(new RegExp(`pub const ${name}: &str =\\s*"([^"]+)"`));
      if (!value) throw new Error(`Missing canonical scope rule ${name}`);
      return value[1];
    };
    const documents = render();
    for (const kind of ["KarsTask", "KarsTeam"]) {
      const definition = documents.find((doc: { kind: string; spec?: { names?: { kind?: string } } }) =>
        doc.kind === "CustomResourceDefinition" && doc.spec?.names?.kind === kind);
      const rules = definition.spec.versions[0].schema.openAPIV3Schema.properties.spec["x-kubernetes-validations"]
        .map((validation: { rule: string }) => validation.rule);
      expect(rules).toContain(rule("OPT_IN_RULE"));
      expect(rules).toContain(rule("RETAIN_SCOPE_RULE"));
      if (kind === "KarsTask") expect(rules).toContain(rule("LAUNCH_RULE"));
    }
  });

  it.each([undefined, null, {}, { enabled: false }])(
    "leaves existing customers and replica count intact with %s", (value) => {
      const documents = render(value);
      expect(documents.some((doc: { kind: string; metadata: { name: string } }) =>
        doc.kind === "Service" && doc.metadata.name === "kars-inference-budget")).toBe(false);
      const controller = documents.find((doc: { kind: string; metadata: { name: string } }) =>
        doc.kind === "Deployment" && doc.metadata.name === "kars-controller");
      expect(controller.spec.replicas).toBe(3);
      const env = controller.spec.template.spec.containers[0].env;
      expect(env).toContainEqual({ name: "CUSTOMER_SETTING", value: "preserved" });
      expect(env.some((item: { name: string }) => item.name.startsWith("KARS_INFERENCE_BUDGET"))).toBe(false);
    },
  );

  it("uses the exact runtime-verified admission bundle and custom controller namespace", () => {
    const documents = render(configured);
    const bundle = JSON.parse(readFileSync(join(chart, "files/inference-budget-admission.json"), "utf8")
      .replaceAll("__ACCOUNTING_NAMESPACE__", "customer-system"));
    for (const policy of bundle.items) {
      const actual = documents.find((doc: { kind: string; metadata: { name: string } }) =>
        doc.kind === "ValidatingAdmissionPolicy" && doc.metadata.name === policy.name);
      expect(actual.spec).toEqual(policy.spec);
      const binding = documents.find((doc: { kind: string; metadata: { name: string } }) =>
        doc.kind === "ValidatingAdmissionPolicyBinding" && doc.metadata.name === policy.name);
      expect(binding.spec).toEqual({ policyName: policy.name, validationActions: ["Deny", "Audit"] });
    }
    const catalog = documents.find((doc: { kind: string; metadata: { name: string } }) =>
      doc.kind === "ConfigMap" && doc.metadata.name === "kars-inference-budget-contracts");
    expect(JSON.parse(catalog.data["contracts.json"])).toEqual({
      version: configured.catalogVersion, contracts: configured.contracts,
      nonInferenceEgressHosts: configured.nonInferenceEgressHosts,
    });
    expect(documents.some((doc: { kind: string }) => doc.kind === "Secret")).toBe(false);
  });

  it.each(["catalogVersion", "contracts", "caBundle", "tlsSecretName", "routerImageDigest"])(
    "refuses enabling without operator-supplied %s", (field) => {
      const incomplete: Record<string, unknown> = { ...configured };
      delete incomplete[field];
      expect(() => render(incomplete)).toThrow(/inferenceBudget/);
    },
  );

  const suppliedTls = {
    certificate: "-----BEGIN CERTIFICATE-----\npublic-test-fixture\n-----END CERTIFICATE-----",
    privateKey: "-----BEGIN PRIVATE KEY-----\nprivate-test-fixture\n-----END PRIVATE KEY-----",
  };

  it("installs supplied TLS only as a release-managed private Secret", () => {
    const documents = render({ ...configured, tls: suppliedTls });
    const secrets = documents.filter((doc: { kind: string }) => doc.kind === "Secret");
    expect(secrets).toHaveLength(1);
    const secret = secrets[0];
    expect(secret.metadata.name).toBe(configured.tlsSecretName);
    expect(secret.metadata.namespace).toBe("customer-system");
    expect(secret.metadata.labels["app.kubernetes.io/managed-by"]).toBe("Helm");
    expect(secret.metadata.annotations["kars.azure.com/inference-budget-tls"]).toBe("v1");
    expect(secret.type).toBe("kubernetes.io/tls");
    expect(Buffer.from(secret.data["tls.crt"], "base64").toString()).toBe(suppliedTls.certificate);
    expect(Buffer.from(secret.data["tls.key"], "base64").toString()).toBe(suppliedTls.privateKey);
    const publicDocuments = JSON.stringify(documents.filter((doc: { kind: string }) => doc.kind !== "Secret"));
    expect(publicDocuments).not.toContain("private-test-fixture");
    expect(publicDocuments).not.toContain(secret.data["tls.key"]);
  });

  it("accepts only live, same-release, explicitly owned budget TLS Secrets", () => {
    const owned = {
      type: "kubernetes.io/tls",
      metadata: {
        annotations: {
          "meta.helm.sh/release-name": "retained-release",
          "meta.helm.sh/release-namespace": "customer-system",
          "kars.azure.com/inference-budget-tls": "v1",
        },
        labels: { "app.kubernetes.io/managed-by": "Helm" },
      },
    };
    const budget = { ...configured, tls: suppliedTls };
    expect(() => render(budget, owned)).not.toThrow();
    const invalid = [
      { ...owned, type: "Opaque" },
      { ...owned, metadata: { ...owned.metadata, deletionTimestamp: "2026-01-01T00:00:00Z" } },
      { ...owned, metadata: { ...owned.metadata, annotations: {} } },
      { ...owned, metadata: { ...owned.metadata, labels: {} } },
      ...Object.keys(owned.metadata.annotations).map((key) => ({
        ...owned,
        metadata: { ...owned.metadata, annotations: { ...owned.metadata.annotations, [key]: "foreign" } },
      })),
    ];
    for (const secret of invalid) {
      expect(() => render(budget, secret)).toThrow(/cannot adopt or replace/);
    }
  });

  it("rolls the controller for managed certificate or key changes only", () => {
    const checksum = (budget: unknown) => render(budget).find(
      (doc: { kind: string; metadata: { name: string } }) =>
        doc.kind === "Deployment" && doc.metadata.name === "kars-controller",
    ).spec.template.metadata.annotations?.["checksum/inference-budget-tls"];
    expect(checksum(configured)).toBeUndefined();
    const initial = checksum({ ...configured, tls: suppliedTls });
    expect(initial).toMatch(/^[a-f0-9]{64}$/);
    expect(checksum({ ...configured, tls: suppliedTls })).toBe(initial);
    for (const field of ["certificate", "privateKey"] as const) {
      expect(checksum({ ...configured, tls: { ...suppliedTls, [field]: `${suppliedTls[field]}\n` } }))
        .not.toBe(initial);
    }
  });

  it.each([
    { certificate: suppliedTls.certificate },
    { privateKey: suppliedTls.privateKey },
    { ...suppliedTls, certificate: " " },
    { ...suppliedTls, privateKey: " " },
    { ...suppliedTls, certificate: suppliedTls.privateKey },
    { ...suppliedTls, privateKey: "-----BEGIN ENCRYPTED PRIVATE KEY-----" },
  ])("rejects incomplete or invalid supplied TLS %s", (tls) => {
    expect(() => render({ ...configured, tls })).toThrow(/inferenceBudget.tls/);
  });

  it("does not silently discard a supplied identity when disabled", () => {
    expect(() => render({ ...configured, enabled: false, tls: suppliedTls }))
      .toThrow(/inferenceBudget.tls requires inferenceBudget.enabled/);
    const documents = render({ ...configured, enabled: false, tls: {} });
    expect(documents.some((doc: { kind: string }) => doc.kind === "Secret")).toBe(false);
  });

  it("rejects an invalid managed Secret name", () => {
    expect(() => render({ ...configured, tlsSecretName: "wrong/name", tls: suppliedTls }))
      .toThrow(/inferenceBudget.tlsSecretName must be a Secret name/);
  });

  it("never places private TLS keys in a public CA ConfigMap", () => {
    expect(() => render({ ...configured, caBundle: "-----BEGIN PRIVATE KEY-----" }))
      .toThrow(/public certificates only/);
  });
});
