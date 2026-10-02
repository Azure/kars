// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { parse, parseAllDocuments, stringify } from "yaml";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const chart = join(root, "deploy/helm/kars");
const temporaryDirectories: string[] = [];

interface Container {
  name?: string;
  image?: string;
  env?: Array<{
    name: string;
    value?: string;
    valueFrom?: { secretKeyRef: { name: string; key: string; optional: boolean } };
  }>;
  command?: string[];
}

interface Manifest {
  kind: string;
  type?: string;
  data?: Record<string, string>;
  metadata?: { name?: string; namespace?: string; labels?: Record<string, string> };
  spec?: {
    replicas?: number;
    strategy?: { type?: string };
    selector?: Record<string, unknown>;
    template?: {
      metadata?: { labels?: Record<string, string>; annotations?: Record<string, string> };
      spec?: { containers?: Container[]; initContainers?: Container[] };
    };
  };
}

function documents(text: string): Manifest[] {
  return parseAllDocuments(text).map((document) => {
    if (document.errors.length) throw document.errors[0];
    return document.toJSON();
  }).filter(Boolean);
}

function render(path = chart, args: string[] = []): Manifest[] {
  return documents(execFileSync(
    "helm", ["template", "kars", path, "--namespace", "kars-system", ...args],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 30_000 },
  ));
}

function resource(resources: Manifest[], kind: string, name: string): Manifest {
  const found = resources.find((item) => item.kind === kind && item.metadata?.name === name);
  if (!found) throw new Error(`Missing ${kind}/${name}`);
  return found;
}

function reusedValuesChart(enableMesh = false): string {
  const directory = mkdtempSync(join(tmpdir(), "kars-reused-values-"));
  temporaryDirectories.push(directory);
  const copied = join(directory, "chart");
  cpSync(chart, copied, { recursive: true });
  const savedValues = parse(readFileSync(join(chart, "values.yaml"), "utf8"));
  // --reuse-values replaces the new chart defaults with the saved old map.
  // These two fields did not exist before the generic-installation feature.
  delete savedValues.agentMesh;
  delete savedValues.sandbox.nodeSelector;
  savedValues.controller.replicas = 3;
  savedValues.azure.workloadIdentity.clientId = "existing-customer-identity";
  savedValues.sandbox.image.repository = "registry.customer.example/existing-agent";
  if (enableMesh) savedValues.agentMesh = { enabled: true };
  writeFileSync(join(copied, "values.yaml"), stringify(savedValues));
  return copied;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe("Bridge route qualification policy", () => {
  const bridgeChart = join(root, "bridge/deploy/helm/kars-bridge");
  const prefix = "bff.routeQualification";
  const env = (args: string[] = []) => resource(render(bridgeChart, args), "Deployment", "kars-bridge-bff")
    .spec?.template?.spec?.containers?.[0]?.env ?? [];

  it("defaults to required evidence without creating synthetic records", () => {
    expect(env().find((entry) => entry.name === "BRIDGE_ROUTE_QUALIFICATION_MODE")?.value).toBe("required");
    expect(env().some((entry) => entry.name === "BRIDGE_QUALIFICATION_RECORDS_JSON")).toBe(false);
  });

  it("renders explicit unqualified validation mode", () => {
    expect(env([`--set=${prefix}.mode=validation`]).filter((entry) => entry.name === "BRIDGE_ROUTE_QUALIFICATION_MODE"))
      .toEqual([{ name: "BRIDGE_ROUTE_QUALIFICATION_MODE", value: "validation" }]);
  });

  it.each(["", "disabled", "Validation", "validation "])("rejects invalid mode %j", (mode) => {
    expect(() => env([`--set-string=${prefix}.mode=${mode}`])).toThrow(/mode must be required or validation/);
  });

  it.each(["not-json", "null", "{}"])("rejects invalid records %j", (records) => {
    const directory = mkdtempSync(join(tmpdir(), "kars-route-records-"));
    temporaryDirectories.push(directory);
    const file = join(directory, "records.json");
    writeFileSync(file, records);
    expect(() => env([`--set-file=${prefix}.recordsJson=${file}`])).toThrow();
  });

  it("loads record files and rejects duplicate primary sources", () => {
    const directory = mkdtempSync(join(tmpdir(), "kars-route-records-"));
    temporaryDirectories.push(directory);
    const file = join(directory, "records.json");
    writeFileSync(file, "[]");
    expect(env([`--set-file=${prefix}.recordsJson=${file}`])
      .find((entry) => entry.name === "BRIDGE_QUALIFICATION_RECORDS_JSON")?.value).toBe("[]");
    expect(() => env([`--set-file=${prefix}.recordsJson=${file}`,
      "--set=bff.extraEnv[0].name=BRIDGE_QUALIFICATION_RECORDS_JSON",
      "--set-string=bff.extraEnv[0].value=[]"])).toThrow(/conflicts/);
  });

  it("preserves existing primary records in extraEnv", () => {
    expect(env(["--set=bff.extraEnv[0].name=BRIDGE_QUALIFICATION_RECORDS_JSON",
      "--set-string=bff.extraEnv[0].value=[]"]).filter((entry) => entry.name === "BRIDGE_QUALIFICATION_RECORDS_JSON"))
      .toEqual([{ name: "BRIDGE_QUALIFICATION_RECORDS_JSON", value: "[]" }]);
  });

  it("rejects mode shadowing through extraEnv", () => {
    expect(() => env(["--set=bff.extraEnv[0].name=BRIDGE_ROUTE_QUALIFICATION_MODE",
      "--set=bff.extraEnv[0].value=validation"])).toThrow(/not extraEnv/);
  });
});

describe("Helm initial inference credentials", () => {
  const prefix = "inferenceRouter.azure.openai.credentials";
  const endpoint = "--set=foundry.endpoint=https://inference.example.test";
  const controller = (manifests: Manifest[]) => resource(manifests, "Deployment", "kars-controller");
  const keyEnv = (manifests: Manifest[]) => controller(manifests).spec?.template?.spec
    ?.containers?.[0]?.env?.find((entry) => entry.name === "AZURE_OPENAI_API_KEY");

  it.each([{ args: [] }, { args: [`--set=${prefix}=null`] }])("leaves identity authentication unchanged by default: $args", ({ args }) => {
    const manifests = render(chart, args);
    expect(keyEnv(manifests)).toBeUndefined();
    expect(manifests.some((item) => item.metadata?.name === "kars-inference-bootstrap")).toBe(false);
  });

  it("accepts saved values predating credential configuration", () => {
    const copied = reusedValuesChart();
    const path = join(copied, "values.yaml");
    const values = parse(readFileSync(path, "utf8"));
    delete values.inferenceRouter.azure.openai.credentials;
    writeFileSync(path, stringify(values));
    expect(keyEnv(render(copied))).toBeUndefined();
  }, 45_000);

  it("creates an Opaque Secret and a required reference without a literal controller key", () => {
    const manifests = render(chart, [endpoint, `--set-string=${prefix}.apiKey=fixture-not-a-credential`]);
    const secret = resource(manifests, "Secret", "kars-inference-bootstrap");
    expect(secret.metadata?.namespace).toBe("kars-system");
    expect(secret.type).toBe("Opaque");
    expect(secret.data).toEqual({ "api-key": Buffer.from("fixture-not-a-credential").toString("base64") });
    expect(keyEnv(manifests)).toEqual({
      name: "AZURE_OPENAI_API_KEY",
      valueFrom: { secretKeyRef: { name: "kars-inference-bootstrap", key: "api-key", optional: false } },
    });
    expect(JSON.stringify(controller(manifests))).not.toContain("fixture-not-a-credential");
    const changed = render(chart, [endpoint, `--set-string=${prefix}.apiKey=rotated-fixture`]);
    expect(controller(manifests).spec?.template?.metadata?.annotations?.["checksum/inference-credentials"])
      .not.toBe(controller(changed).spec?.template?.metadata?.annotations?.["checksum/inference-credentials"]);
  });

  it("references an operator-managed Secret without creating or adopting it", () => {
    const manifests = render(chart, [endpoint, `--set=${prefix}.existingSecret=operator-key,${prefix}.key=API_KEY`]);
    expect(manifests.some((item) => item.kind === "Secret" &&
      ["operator-key", "kars-inference-bootstrap"].includes(item.metadata?.name ?? ""))).toBe(false);
    expect(keyEnv(manifests)?.valueFrom?.secretKeyRef)
      .toEqual({ name: "operator-key", key: "API_KEY", optional: false });
  });

  it.each([
    [`${prefix}.apiKey=fixture,${prefix}.existingSecret=operator-key`, /choose apiKey or existingSecret/],
    [`${prefix}.apiKey=   `, /apiKey must not be blank/],
    [`${prefix}.existingSecret=bad..name`, /existingSecret must be a Secret name/],
    [`${prefix}.existingSecret=kars-inference-bootstrap`, /cannot use the Helm-managed bootstrap Secret name/],
    [`${prefix}.existingSecret=operator-key,${prefix}.key=bad\/key`, /key must be a Secret data key/],
    [`${prefix}.apiKey=fixture,controller.extraEnv[0].name=AZURE_OPENAI_API_KEY,controller.extraEnv[0].value=duplicate`, /cannot also be set/],
  ])("rejects conflicting or invalid inputs: %s", (setting, message) => {
    expect(() => render(chart, [endpoint, `--set-string=${setting}`])).toThrow(message);
  });

  it("requires an inference endpoint for API-key authentication", () => {
    expect(() => render(chart, [`--set-string=${prefix}.apiKey=fixture`]))
      .toThrow(/API-key inference requires/);
  });
});

describe("existing Helm installation compatibility", () => {
  // Archive copying and a bounded Helm subprocess are integration work, not
  // a five-second unit test, especially with the expanded governance CRDs.
  it("preserves saved customer values and the legacy selector when new maps are absent", () => {
    const manifests = render(reusedValuesChart());
    expect(manifests.some((item) => item.metadata?.name === "agentmesh-registry")).toBe(false);
    const controller = resource(manifests, "Deployment", "kars-controller");
    expect(controller.spec?.replicas).toBe(3);
    const env = controller.spec?.template?.spec?.containers?.[0]?.env;
    expect(env).toContainEqual({ name: "KARS_SANDBOX_NODE_SELECTOR_JSON", value: "{}" });
    expect(env).toContainEqual({ name: "AZURE_WI_CLIENT_ID", value: "existing-customer-identity" });
    expect(env).toContainEqual({
      name: "SANDBOX_IMAGE", value: "registry.customer.example/existing-agent:latest",
    });
  }, 45_000);

  it("can enable the new mesh using reused values without missing nested defaults", () => {
    const manifests = render(reusedValuesChart(true));
    for (const component of ["registry", "relay"]) {
      const deployment = resource(manifests, "Deployment", component);
      expect(deployment.spec?.replicas).toBe(1);
      expect(deployment.spec?.template?.spec?.containers?.[0]?.image)
        .toBe(`ghcr.io/azure/kars-agentmesh-${component}:latest`);
    }
  }, 45_000);

  it("treats removed/null new configuration as the compatible disabled/default state", () => {
    const manifests = render(chart, ["--set", "agentMesh=null,sandbox.nodeSelector=null"]);
    expect(manifests.some((item) => item.metadata?.name === "agentmesh-registry")).toBe(false);
    expect(resource(manifests, "Deployment", "kars-controller").spec?.template?.spec?.containers?.[0]?.env)
      .toContainEqual({ name: "KARS_SANDBOX_NODE_SELECTOR_JSON", value: "{}" });
  });

  it("uses deployment and service selectors compatible with the standalone AGT manifest", () => {
    const legacy = documents(readFileSync(join(root, "deploy/agentmesh-agt.yaml"), "utf8"));
    const manifests = render(chart, ["--set", "agentMesh.enabled=true"]);
    for (const component of ["registry", "relay"]) {
      const deployment = resource(manifests, "Deployment", component);
      expect(deployment.spec?.selector).toEqual(resource(legacy, "Deployment", component).spec?.selector);
      expect(deployment.spec?.template?.metadata?.labels?.app).toBe(`agentmesh-${component}`);
      expect(deployment.spec?.template?.metadata?.labels?.["app.kubernetes.io/name"])
        .toBe(`agentmesh-${component}`);
      expect(deployment.spec?.strategy?.type).toBe("Recreate");
      const service = `agentmesh-${component}`;
      expect(resource(manifests, "Service", service).spec?.selector)
        .toEqual(resource(legacy, "Service", service).spec?.selector);
    }
  });

  it("allows explicit maintenance scale-down without replacing zero with the default", () => {
    const manifests = render(chart, [
      "--set", "agentMesh.enabled=true,agentMesh.registry.replicas=0,agentMesh.relay.replicas=0",
    ]);
    expect(resource(manifests, "Deployment", "registry").spec?.replicas).toBe(0);
    expect(resource(manifests, "Deployment", "relay").spec?.replicas).toBe(0);
  });

  it.each([
    ["agentMesh.namespace=mesh-custom", /agentMesh.namespace must be agentmesh/],
    ["agentMesh.registry.replicas=2", /replica counts must be 0 or 1/],
    ["agentMesh.relay.replicas=2", /replica counts must be 0 or 1/],
    ["agentMesh.registry.replicas=-1", /replica counts must be 0 or 1/],
  ])("rejects unsupported mesh configuration: %s", (setting, message) => {
    expect(() => render(chart, ["--set", `agentMesh.enabled=true,${setting}`])).toThrow(message);
  });

  it.each(["values-generic.yaml", "values-existing-aks.yaml"])(
    "%s keeps the profile needed by existing/default enhanced sandbox CRs",
    (profile) => {
      const manifests = render(chart, ["--values", join(chart, profile)]);
      const installer = resource(manifests, "DaemonSet", "kars-seccomp-installer");
      const command = installer.spec?.template?.spec?.initContainers?.[0]?.command?.join("\n");
      expect(command).toContain("/host/seccomp/profiles/kars-strict.json");
      expect(command).not.toContain("/host/seccomp/profiles/RuntimeDefault.json");
    },
  );
});
